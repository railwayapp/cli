"""CLI-owned OpenCode launcher, executed over SSH with a JSON request on stdin.

Credentials stay in the VM's private state directory and in the SSH response.
The child has its own session and closed SSH descriptors; no tunnel is needed.
"""
import base64
import fcntl
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request


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


def owned_process(state):
    pid = state.get("pid")
    start = state.get("start")
    return isinstance(pid, int) and pid > 1 and start is not None and process_start(pid) == start


def health(port, credentials=None):
    request = urllib.request.Request(f"http://127.0.0.1:{port}/global/health")
    if credentials:
        token = base64.b64encode(
            f"{credentials['username']}:{credentials['password']}".encode()
        ).decode()
        request.add_header("Authorization", f"Basic {token}")
    try:
        # Loopback probes must never pass credentials to an HTTP_PROXY.
        with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request, timeout=1) as response:
            body = json.loads(response.read(65536))
            return response.status, body.get("healthy") is True
    except urllib.error.HTTPError as error:
        return error.code, False
    except (OSError, ValueError):
        return None, False


def port_available(port):
    try:
        with socket.socket() as sock:
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            sock.bind(("0.0.0.0", port))
        return True
    except OSError:
        return False


def setup(request, home, port=8080):
    os.umask(0o077)
    root = Path(home) / ".railway" / "desktop" / "opencode"
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(root, 0o700)
    state_path = root / "server.json"
    with (root / "setup.lock").open("w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise SetupError("Another OpenCode setup is running on this agent. Retry when it finishes.")
        state = json.loads(state_path.read_text()) if state_path.exists() else {}
        if request.get("action") == "stop":
            if owned_process(state):
                try:
                    os.killpg(state["pid"], signal.SIGTERM)
                except ProcessLookupError:
                    pass
                deadline = time.monotonic() + 5
                while owned_process(state) and time.monotonic() < deadline:
                    time.sleep(0.1)
                if owned_process(state):
                    try:
                        os.killpg(state["pid"], signal.SIGKILL)
                    except ProcessLookupError:
                        pass
            state.pop("pid", None)
            state.pop("start", None)
            if state:
                save(state_path, state)
            return {"stopped": True}

        directory = str(Path(request["directory"]).resolve(strict=True))
        if not Path(directory).is_dir():
            raise SetupError("OpenCode's working directory must be a directory.")
        domain = os.environ.get("RAILWAY_PUBLIC_DOMAIN_8080") or os.environ.get("RAILWAY_PUBLIC_DOMAIN")
        if not domain:
            raise SetupError("This agent has no public address for port 8080.")
        credentials = {
            "username": state.get("username") or os.environ.get("OPENCODE_SERVER_USERNAME") or "opencode",
            "password": state.get("password") or os.environ.get("OPENCODE_SERVER_PASSWORD") or request["password"],
        }
        if not all(isinstance(value, str) and value for value in credentials.values()):
            raise SetupError("OpenCode requires a nonempty username and password.")

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
            state = dict(credentials)
            save(state_path, state)
            with (root / "server.log").open("w") as log:
                child = subprocess.Popen(
                    ["opencode", "serve", "--hostname", "0.0.0.0", "--port", str(port)],
                    cwd=directory, env=environment, stdin=subprocess.DEVNULL,
                    stdout=log, stderr=subprocess.STDOUT, close_fds=True,
                    start_new_session=True,
                )
            state.update({"pid": child.pid, "start": process_start(child.pid)})
            save(state_path, state)
            deadline = time.monotonic() + 60
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
        return dict(credentials, url=f"https://{domain}", directory=directory, reused=reused)


if __name__ == "__main__":
    try:
        result = setup(json.load(sys.stdin), Path.home())
        print("RAILWAY_OPENCODE_CONNECTION=" + json.dumps(result), flush=True)
    except (SetupError, OSError, ValueError, KeyError) as error:
        print(f"OpenCode setup failed: {error}", file=sys.stderr)
        sys.exit(1)
