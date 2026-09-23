"""CLI-owned OpenCode launcher, executed over SSH with a JSON request on stdin.

Runs the image's official OpenCode V2 `opencode` as a password-protected
server. Credentials stay in the VM's private state directory and in the SSH
response. The child has its own session and closed SSH descriptors; no tunnel
is needed.
"""
import base64
import fcntl
import json
import os
import re
import shutil
from pathlib import Path
import signal
import socket
import sqlite3
import subprocess
import sys
import time
import urllib.error
import urllib.request

CODE_PORT = 4096
# Agents created before the code endpoint existed serve on the app port.
LEGACY_PORT = 8080
OLD_IMAGE = ("This cloud agent is on an older image whose OpenCode is not V2. "
             "Create a new agent with railway code --opencode --new.")
V1_SERVER = ("This agent's saved OpenCode server was started as OpenCode 1, which this CLI no longer supports. "
             "Create a new agent with railway code --opencode --new.")


class SetupError(Exception):
    pass


def save(path, value):
    temporary = path.with_suffix(".tmp")
    with temporary.open("w") as stream:
        os.chmod(temporary, 0o600)
        json.dump(value, stream)
    temporary.replace(path)


def process_start(pid):
    try:
        # comm can contain spaces and parentheses; fields after its final ')'
        # start at field 3, and starttime is field 22.
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return None if fields[0] == "Z" else fields[19]
    except (OSError, IndexError):
        return None


def restored_state(state):
    current = os.environ.get("RAILWAY_FACTORY_VM_ID")
    return bool(current and state.get("vm_id") and state["vm_id"] != current)


def owned_process(state):
    if restored_state(state):
        return False
    pid = state.get("pid")
    start = state.get("start")
    return isinstance(pid, int) and pid > 1 and start is not None and process_start(pid) == start


def probe(port, credentials, path):
    request = urllib.request.Request(f"http://127.0.0.1:{port}/{path}")
    if credentials:
        token = base64.b64encode(
            f"{credentials['username']}:{credentials['password']}".encode()
        ).decode()
        request.add_header("Authorization", f"Basic {token}")
    try:
        # Loopback probes must never pass credentials to an HTTP_PROXY.
        with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request, timeout=1) as response:
            body = json.loads(response.read(65536))
            return response.status, body
    except urllib.error.HTTPError as error:
        return error.code, {}
    except (OSError, ValueError):
        return None, {}


def server_status(port, credentials=None):
    # Current V2 exposes server identity at /api/info. Older V2 endpoints
    # are only fallbacks for discovering an existing server.
    for path in ("api/info", "api/status", "api/health"):
        status, body = probe(port, credentials, path)
        if status != 404:
            return status, body
    return status, body


def health(port, credentials=None):
    status, body = server_status(port, credentials)
    ready = isinstance(body.get("version"), str) and (
        body.get("healthy") is True or (type(body.get("pid")) is int and isinstance(body.get("urls"), list))
    )
    return status, ready


def stop_owned(state):
    if not owned_process(state):
        return
    try:
        os.killpg(state["pid"], signal.SIGTERM)
    except ProcessLookupError:
        return
    deadline = time.monotonic() + 5
    while owned_process(state) and time.monotonic() < deadline:
        time.sleep(0.1)
    if owned_process(state):
        os.killpg(state["pid"], signal.SIGKILL)
        deadline = time.monotonic() + 5
        while owned_process(state) and time.monotonic() < deadline:
            time.sleep(0.1)
        if owned_process(state):
            raise SetupError("The OpenCode server did not stop; retry.")


def release_tuple(version):
    return tuple(map(int, version.split("."))) if re.fullmatch(r"\d+\.\d+\.\d+", version or "") else None


def v2_runtime(home, minimum=None):
    """The image's official OpenCode V2 executable.

    `minimum` is the release a saved server last ran: V2 storage is not
    reopened by an older release. The CLI never installs or upgrades OpenCode
    on a VM; an image without V2 is recreated.
    """
    search = f"{home}/.opencode/bin:{home}/.local/bin:" + os.environ.get("PATH", "/usr/local/bin:/usr/bin:/bin")
    for binary in filter(None, [str(home / ".opencode/bin/opencode"), shutil.which("opencode", path=search)]):
        try:
            version = executable_version(binary)
        except (OSError, subprocess.SubprocessError, SetupError):
            continue
        floor = release_tuple(minimum)
        if version.startswith("2.") and (floor is None or release_tuple(version) >= floor):
            return binary, version
    raise SetupError(OLD_IMAGE)


def executable_version(binary):
    result = subprocess.run([str(binary), "--version"], stdin=subprocess.DEVNULL,
                            capture_output=True, text=True, timeout=10)
    version = result.stdout.strip().removeprefix("opencode v")
    if result.returncode or not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise SetupError("The OpenCode executable has an unsupported version.")
    return version


def saved_v2(state):
    """Whether a saved server record is a V2 server this CLI can manage.

    Records written before `protocol` existed named V2 servers opencode2;
    an `opencode` record without a protocol was OpenCode 1.
    """
    if not state:
        return True
    protocol = state.get("protocol")
    if protocol is None:
        return state.get("harness") == "opencode2"
    if protocol not in ("v1", "v2"):
        raise SetupError("The saved OpenCode protocol is unsupported; update the Railway CLI.")
    return protocol == "v2"


def reject_downgrade(current, requested):
    if current and requested and current != requested and not current.startswith("0.0.0-beta-"):
        if tuple(map(int, current.split("."))) > tuple(map(int, requested.split("."))):
            raise SetupError(f"The saved server last ran OpenCode {current}, newer than this image's {requested}; its storage is not reopened by an older release. Create a new agent with railway code --opencode --new.")


def port_available(port):
    try:
        with socket.socket() as sock:
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            sock.bind(("0.0.0.0", port))
        return True
    except OSError:
        return False


def server_port(state):
    configured = os.environ.get("RAILWAY_CODE_PORT")
    if configured:
        if not configured.isascii() or not configured.isdecimal():
            raise SetupError("RAILWAY_CODE_PORT must be a valid code endpoint port.")
        configured = int(configured)
        if not 1024 <= configured <= 65535 or configured in (LEGACY_PORT, 8790):
            raise SetupError("RAILWAY_CODE_PORT must be 1024-65535, excluding the app and gateway ports.")
    # A live legacy server retains its endpoint. On a restored disk there is
    # no owned process: adopt the new VM's explicit code port, even when the
    # checkpoint contains pre-port launcher state. The API owns this setting.
    if owned_process(state):
        port = state.get("port", LEGACY_PORT)
    elif configured:
        port = configured
    elif state and not restored_state(state):
        port = state.get("port", LEGACY_PORT)
    else:
        port = CODE_PORT if os.environ.get(f"RAILWAY_PUBLIC_DOMAIN_{CODE_PORT}") else LEGACY_PORT
    if type(port) is not int or not 1024 <= port <= 65535 or port == 8790:
        raise SetupError("The saved OpenCode server has an unsupported port.")
    return port


def setup(request, home):
    home = Path(home)
    if request.get("harness", "opencode") not in ("opencode", "opencode2"):
        raise SetupError("Unsupported OpenCode harness.")
    if request.get("protocol", "v2") != "v2":
        raise SetupError("Unsupported OpenCode protocol.")
    os.umask(0o077)
    root = Path(home) / ".railway" / "desktop" / "opencode"
    state_path = root / "server.json"
    if request.get("action") == "inspect":
        # Discovery is read-only, returns no credentials, and must not seed
        # directories or start a server on an unrelated cloud agent.
        state = json.loads(state_path.read_text()) if state_path.exists() else {}
        if not saved_v2(state):
            return None
        port = server_port(state)
        if (owned_process(state)
                and health(port, state) == (200, True)
                and health(port)[0] == 401):
            return {"directory": state.get("directory") or str(Path(f"/proc/{state['pid']}/cwd").resolve())}
        return None
    if request.get("action") == "connect" and not state_path.exists():
        raise SetupError("This agent has no managed OpenCode server. Set one up with railway code first.")
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(root, 0o700)
    with (root / "setup.lock").open("w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise SetupError("Another OpenCode setup is running on this agent. Retry when it finishes.")
        state = json.loads(state_path.read_text()) if state_path.exists() else {}
        if request.get("action") == "stop":
            # A V1 server is still ours to stop; nothing else touches it.
            stop_owned(state)
            state.pop("pid", None)
            state.pop("start", None)
            if state:
                save(state_path, state)
            return {"stopped": True}
        if not saved_v2(state):
            raise SetupError(V1_SERVER)

        port = server_port(state)
        if request.get("action") == "connect":
            if not state.get("password") or not state.get("username"):
                raise SetupError("This agent has no server for the selected OpenCode edition.")
            directory = state.get("directory")
            if not directory and owned_process(state):
                directory = str(Path(f"/proc/{state['pid']}/cwd").resolve())
            if not directory:
                raise SetupError("The saved server has no project directory. Rerun railway code with --agent and --dir to reconnect.")
            request = dict(request, directory=directory, password=state["password"])
        directory = str(Path(request["directory"]).resolve(strict=True))
        if not Path(directory).is_dir():
            raise SetupError("OpenCode's working directory must be a directory.")
        domain = os.environ.get(f"RAILWAY_PUBLIC_DOMAIN_{port}")
        if not domain and port == LEGACY_PORT:
            domain = os.environ.get("RAILWAY_PUBLIC_DOMAIN")
        if not domain:
            raise SetupError(f"This agent has no public address for port {port}.")
        credentials = {
            "username": state.get("username") or os.environ.get("OPENCODE_SERVER_USERNAME") or "opencode",
            "password": state.get("password") or os.environ.get("OPENCODE_SERVER_PASSWORD") or request["password"],
        }
        # V2's explicit server uses the fixed Basic-auth user opencode. Keep
        # historical credentials on reuse; normalize only new starts.
        if not state:
            credentials["username"] = "opencode"
        if not all(isinstance(value, str) and value for value in credentials.values()):
            raise SetupError("OpenCode requires a nonempty username and password.")

        runtime = None
        if not port_available(port) and not owned_process(state):
            raise SetupError(f"Port {port} is occupied by another process. Stop it or use --new for a fresh agent.")
        current = server_status(port, credentials)[1].get("version") if owned_process(state) else None
        if not current:
            # The image's opencode is the runtime; a saved release only sets
            # the floor so V2 storage is never reopened by an older one.
            previous = state.get("version")
            runtime = v2_runtime(home, previous)
            reject_downgrade(previous, runtime[1])
        reused = False
        if not port_available(port):
            if owned_process(state) and health(port, credentials) == (200, True) and health(port)[0] == 401:
                reused = True
            else:
                raise SetupError(f"Port {port} is occupied by another process. Stop it or use --new for a fresh agent.")
        if not reused:
            if owned_process(state):
                raise SetupError("The managed OpenCode process is running but unhealthy. Run --remove, then reconnect.")
            environment = dict(os.environ)
            environment.update({
                "HOME": str(home),
                "OPENCODE_SERVER_USERNAME": credentials["username"],
                "OPENCODE_SERVER_PASSWORD": credentials["password"],
            })
            environment["PATH"] = f"{home}/.opencode/bin:{home}/.local/bin:" + environment.get("PATH", "/usr/local/bin:/usr/bin:/bin")
            # `protocol` distinguishes this record from OpenCode 1 servers saved
            # by older CLIs; those said harness=opencode without a protocol.
            state = dict(credentials, harness="opencode", protocol="v2", directory=directory, port=port,
                         vm_id=os.environ.get("RAILWAY_FACTORY_VM_ID"), version=runtime[1], binary=str(runtime[0]))
            save(state_path, state)
            with (root / "server.log").open("w") as log:
                child = subprocess.Popen(
                    [str(runtime[0]), "serve", "--hostname", "0.0.0.0", "--port", str(port)],
                    cwd=directory, env=environment, stdin=subprocess.DEVNULL,
                    stdout=log, stderr=subprocess.STDOUT, close_fds=True,
                    start_new_session=True,
                )
            state.update({"pid": child.pid, "start": process_start(child.pid)})
            save(state_path, state)
            deadline = time.monotonic() + 600
            while time.monotonic() < deadline:
                if child.poll() is not None:
                    raise SetupError(f"OpenCode exited during startup. Check {root / 'server.log'} on the agent.")
                if health(port, credentials) == (200, True):
                    if health(port)[0] != 401:
                        os.killpg(child.pid, signal.SIGTERM)
                        raise SetupError("OpenCode is not enforcing password authentication; stopped it.")
                    break
                time.sleep(0.25)
            else:
                os.killpg(child.pid, signal.SIGTERM)
                raise SetupError(f"OpenCode did not become healthy. Check {root / 'server.log'} on the agent.")
        if reused and not state.get("directory"):
            cwd = Path(f"/proc/{state['pid']}/cwd")
            if cwd.exists():
                state["directory"] = str(cwd.resolve())
                save(state_path, state)
        if reused and not state.get("vm_id") and os.environ.get("RAILWAY_FACTORY_VM_ID"):
            state["vm_id"] = os.environ["RAILWAY_FACTORY_VM_ID"]
            save(state_path, state)
        version = server_status(port, credentials)[1].get("version") or state.get("version")
        state.update(protocol="v2", version=version)
        save(state_path, state)
        return dict(credentials, url=f"https://{domain}", directory=directory, reused=reused, version=version)


if __name__ == "__main__":
    try:
        result = setup(json.load(sys.stdin), Path.home())
        print("RAILWAY_OPENCODE_CONNECTION=" + json.dumps(result), flush=True)
    except (SetupError, OSError, ValueError, KeyError) as error:
        print(f"OpenCode setup failed: {error}", file=sys.stderr)
        sys.exit(1)
