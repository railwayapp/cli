"""CLI-owned OpenCode launcher, executed over SSH with a JSON request on stdin.

Credentials stay in the VM's private state directory and in the SSH response.
The child has its own session and closed SSH descriptors; no tunnel is needed.
"""
import base64
import fcntl
import http.client
import importlib.machinery
import importlib.util
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
import urllib.parse
import urllib.request

CODE_PORT = 4096
LEGACY_PORT = 8080


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


def server_status(port, credentials=None, protocol="v1"):
    if protocol not in ("v2", "opencode2"):
        return probe(port, credentials, "global/health")
    # Current V2 exposes server identity at /api/info. Older V2/Beta
    # endpoints are only fallbacks for discovering an existing server.
    for path in ("api/info", "api/status", "api/health"):
        status, body = probe(port, credentials, path)
        if status != 404:
            return status, body
    return status, body


def health(port, credentials=None, harness="v1"):
    status, body = server_status(port, credentials, harness)
    ready = body.get("healthy") is True
    if harness in ("v2", "opencode2"):
        ready = isinstance(body.get("version"), str) and (
            ready or (type(body.get("pid")) is int and isinstance(body.get("urls"), list))
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
            raise SetupError("The old OpenCode server did not stop; retry the upgrade.")


def prepare_v2(request, home):
    source = request.get("runtime_shim")
    if not source:
        return None
    path = home / ".railway/runtimes/opencode2/launcher.py"
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(".tmp")
    temporary.write_text(source)
    temporary.chmod(0o700)
    temporary.replace(path)
    spec = importlib.util.spec_from_file_location(
        "railway_opencode2", path,
        loader=importlib.machinery.SourceFileLoader("railway_opencode2", str(path)),
    )
    runtime = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(runtime)
    # Download and verify before stopping a working older server.
    binary = runtime.ensure_runtime(request.get("version"))
    return binary, executable_version(binary)


def executable_version(binary):
    result = subprocess.run([str(binary), "--version"], stdin=subprocess.DEVNULL,
                            capture_output=True, text=True, timeout=10)
    version = result.stdout.strip().removeprefix("opencode v")
    if result.returncode or not re.fullmatch(r"[12]\.\d+\.\d+", version):
        raise SetupError("The OpenCode executable has an unsupported version.")
    return version


def state_protocol(state):
    protocol = state.get("protocol")
    if protocol is None:
        protocol = "v2" if state.get("harness") == "opencode2" else "v1"
    if protocol not in ("v1", "v2"):
        raise SetupError("The saved OpenCode protocol is unsupported; update the Railway CLI.")
    return protocol


def legacy_database(home):
    data = Path(os.environ.get("XDG_DATA_HOME") or home / ".local/share") / "opencode"
    database = data / os.environ.get("OPENCODE_DB", "opencode.db")
    if not database.is_file():
        return False
    with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as db:
        tables = {row[0] for row in db.execute("SELECT name FROM sqlite_master WHERE type='table'")}
    return "session" in tables and "credential" not in tables


def migrated_database(home):
    data = Path(os.environ.get("XDG_DATA_HOME") or home / ".local/share") / "opencode"
    database = data / os.environ.get("OPENCODE_DB", "opencode.db")
    if not database.is_file():
        return False
    with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as db:
        return bool(db.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='credential'").fetchone())


def legacy_runtime(home, state):
    # Never run an upgraded `opencode` against a V1 store based on its name.
    candidates = [state.get("binary"), str(home / ".opencode/bin/opencode"), shutil.which("opencode")]
    for binary in filter(None, candidates):
        try:
            version = executable_version(binary)
            if version.startswith("1.") and (not state.get("version") or version == state["version"]):
                return binary, version
        except (OSError, subprocess.SubprocessError, SetupError):
            continue
    raise SetupError("This connection uses OpenCode 1 — legacy, but no V1 executable matching its saved release is installed. Restore its V1 runtime or explicitly upgrade with railway code --opencode upgrade <agent>.")


def reject_downgrade(current, requested):
    if current and requested and current != requested and not current.startswith("0.0.0-beta-"):
        if tuple(map(int, current.split("."))) > tuple(map(int, requested.split("."))):
            raise SetupError(f"The cloud server is newer ({current}) than the requested release ({requested}); refusing to downgrade its storage.")


def backup_database(home, root):
    data = Path(os.environ.get("XDG_DATA_HOME") or home / ".local/share") / "opencode"
    database = data / os.environ.get("OPENCODE_DB", "opencode.db")
    prefix = root / f"opencode-before-upgrade-{time.time_ns()}"
    if database.is_file():
        backup = prefix.with_suffix(".db")
        with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as source, sqlite3.connect(backup) as target:
            source.backup(target)
    if (root / "server.json").exists():
        shutil.copy2(root / "server.json", prefix.with_suffix(".server.json"))
    if (data / "auth.json").exists():
        shutil.copy2(data / "auth.json", prefix.with_suffix(".auth.json"))
    config = Path(os.environ.get("XDG_CONFIG_HOME") or home / ".config") / "opencode"
    if config.is_dir():
        shutil.copytree(config, prefix.with_suffix(".config"), symlinks=True)


def port_available(port):
    try:
        with socket.socket() as sock:
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            sock.bind(("0.0.0.0", port))
        return True
    except OSError:
        return False


def automatic_permissions(config):
    # Match auto mode: turn approval requests into allows, preserving explicit
    # denials and agent restrictions (for example, the read-only plan agent).
    def allow_asks(value):
        if isinstance(value, dict):
            return {key: allow_asks(item) for key, item in value.items()}
        return "allow" if value == "ask" else value

    current = config.get("permission", {})
    policy = allow_asks(current)
    if isinstance(policy, dict) and "*" not in policy:
        # Global PATCH preserves key order. Appending a new wildcard after
        # existing rules would override their explicit denials. Override only
        # OpenCode's asking defaults; the other built-in defaults already allow.
        policy = {"read": "allow", "external_directory": "allow", "doom_loop": "allow", **policy}
    updates = {} if current == policy else {"permission": policy}
    agents = {}
    for name, agent in config.get("agent", {}).items():
        if isinstance(agent, dict) and "permission" in agent:
            policy = allow_asks(agent["permission"])
            if policy != agent["permission"]:
                agents[name] = {"permission": policy}
    if agents:
        updates["agent"] = agents
    return updates


def configure_permissions(port, credentials, directory):
    # Standard OpenCode's attach client has no --auto flag. Apply the policy
    # through the running server so reconnecting to an older server also works.
    # The global API preserves JSONC and reloads active locations. The project
    # PATCH endpoint writes config.json, which some versions do not load.
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=15)
    token = base64.b64encode(f"{credentials['username']}:{credentials['password']}".encode()).decode()
    project_path = "/config?" + urllib.parse.urlencode({"directory": directory})

    def request(method, path="/global/config", payload=None):
        connection.request(method, path, body=json.dumps(payload) if payload is not None else None,
                           headers={"Authorization": f"Basic {token}", "Content-Type": "application/json"})
        response = connection.getresponse()
        body = response.read()
        if response.status != 200:
            raise SetupError(f"Could not configure OpenCode automatic permissions (HTTP {response.status}).")
        config = json.loads(body)
        if not isinstance(config, dict):
            raise SetupError("OpenCode returned an invalid permissions configuration.")
        return config

    try:
        updates = automatic_permissions(request("GET"))
        if updates:
            request("PATCH", payload=updates)
            if automatic_permissions(request("GET")):
                raise SetupError("OpenCode did not apply automatic permissions. Rerun setup to retry.")
        if automatic_permissions(request("GET", project_path)):
            raise SetupError("OpenCode's project configuration overrides automatic permissions. Update its permission rules and reconnect.")
    except (OSError, ValueError, http.client.HTTPException) as error:
        raise SetupError("Could not configure OpenCode automatic permissions. Rerun setup to retry.") from error
    finally:
        connection.close()


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
    requested_protocol = request.get("protocol")
    if requested_protocol is not None and requested_protocol not in ("v1", "v2"):
        raise SetupError("Unsupported OpenCode protocol.")
    os.umask(0o077)
    root = Path(home) / ".railway" / "desktop" / "opencode"
    state_path = root / "server.json"
    if request.get("action") == "inspect":
        # Discovery is read-only, returns no credentials, and must not seed
        # directories or start a server on an unrelated cloud agent.
        state = json.loads(state_path.read_text()) if state_path.exists() else {}
        protocol = state_protocol(state)
        port = server_port(state)
        if (owned_process(state)
                and health(port, state, protocol) == (200, True)
                and health(port, harness=protocol)[0] == 401):
            return {"directory": state.get("directory") or str(Path(f"/proc/{state['pid']}/cwd").resolve()), "protocol": protocol}
        return None
    if request.get("action") in ("connect", "upgrade") and not state_path.exists():
        raise SetupError("This agent has no managed OpenCode server. Set one up with railway code first.")
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(root, 0o700)
    with (root / "setup.lock").open("w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise SetupError("Another OpenCode setup is running on this agent. Retry when it finishes.")
        state = json.loads(state_path.read_text()) if state_path.exists() else {}
        protocol = state_protocol(state) if state else (requested_protocol or state_protocol(request))
        upgrading = request.get("action") == "upgrade"
        if state and requested_protocol and requested_protocol != protocol and not upgrading:
            raise SetupError("This agent has an OpenCode 1 — legacy connection. Use railway code --opencode connect <agent> to retain V1, or railway code --opencode upgrade <agent> to migrate it.")
        if request.get("action") == "stop":
            stop_owned(state)
            state.pop("pid", None)
            state.pop("start", None)
            if state:
                save(state_path, state)
            return {"stopped": True}

        port = server_port(state)
        if request.get("action") in ("connect", "upgrade"):
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
        # Stable V2's explicit server uses the fixed Basic-auth user opencode.
        # Keep historical credentials on reuse; normalize only new starts or an
        # explicit upgrade after probing the existing server with its old user.
        if protocol == "v2" and not state:
            credentials["username"] = "opencode"
        if not all(isinstance(value, str) and value for value in credentials.values()):
            raise SetupError("OpenCode requires a nonempty username and password.")

        runtime = None
        if protocol == "v2" or upgrading:
            if not port_available(port) and not owned_process(state):
                raise SetupError(f"Port {port} is occupied by another process. Stop it or use --new for a fresh agent.")
            current = server_status(port, credentials, protocol)[1].get("version") if owned_process(state) else None
            previous = current or state.get("version")
            if not upgrading and legacy_database(home):
                raise SetupError("This VM contains OpenCode 1 data. Reconnect to its V1 server or explicitly upgrade with railway code --opencode upgrade <agent>.")
            if not current or upgrading:
                runtime_request = dict(request)
                if not upgrading and previous:
                    runtime_request["version"] = previous
                runtime = prepare_v2(runtime_request, home)
                if runtime is None:
                    binary = shutil.which("opencode2", path=f"{home}/.local/bin:{home}/.opencode/bin")
                    runtime = (binary, executable_version(binary))
                reject_downgrade(previous, runtime[1])
                if not runtime[1].startswith("2."):
                    raise SetupError("The selected runtime does not support OpenCode V2.")
                if upgrading:
                    stop_owned(state)
                    backup_database(home, root)
                    protocol = "v2"
                    credentials["username"] = "opencode"
            # Record the storage protocol before a V2 process can migrate it.
            # A failed upgrade must never cause a later V1 restart.
            marker = root / "storage-protocol"
            marker.write_text("v2\n")
        elif not owned_process(state):
            if (root / "storage-protocol").exists() or migrated_database(home):
                raise SetupError("OpenCode storage has been migrated to V2; V1 cannot reopen it. Restore the pre-upgrade backup to use V1.")
            runtime = legacy_runtime(home, state)
        reused = False
        if not port_available(port):
            if owned_process(state) and health(port, credentials, protocol) == (200, True) and health(port, harness=protocol)[0] == 401:
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
            if runtime:
                environment["RAILWAY_OPENCODE_VERSION"] = runtime[1]
            state = dict(credentials, harness="opencode2" if protocol == "v2" else "opencode", protocol=protocol, directory=directory, port=port,
                         vm_id=os.environ.get("RAILWAY_FACTORY_VM_ID"))
            if runtime:
                state["version"] = runtime[1]
                state["binary"] = str(runtime[0])
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
            deadline = time.monotonic() + (600 if protocol == "v2" else 60)
            while time.monotonic() < deadline:
                if child.poll() is not None:
                    raise SetupError(f"OpenCode exited during startup. Check {root / 'server.log'} on the agent.")
                if health(port, credentials, protocol) == (200, True):
                    if health(port, harness=protocol)[0] != 401:
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
        if protocol == "v1":
            configure_permissions(port, credentials, directory)
        if reused and not state.get("vm_id") and os.environ.get("RAILWAY_FACTORY_VM_ID"):
            state["vm_id"] = os.environ["RAILWAY_FACTORY_VM_ID"]
            save(state_path, state)
        version = server_status(port, credentials, protocol)[1].get("version") or state.get("version")
        state.update(protocol=protocol, version=version)
        save(state_path, state)
        return dict(credentials, url=f"https://{domain}", directory=directory, reused=reused, protocol=protocol, version=version)


if __name__ == "__main__":
    try:
        result = setup(json.load(sys.stdin), Path.home())
        print("RAILWAY_OPENCODE_CONNECTION=" + json.dumps(result), flush=True)
    except (SetupError, OSError, ValueError, KeyError) as error:
        print(f"OpenCode setup failed: {error}", file=sys.stderr)
        sys.exit(1)
