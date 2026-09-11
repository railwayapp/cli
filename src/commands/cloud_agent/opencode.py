"""CLI-owned OpenCode launcher, executed over SSH with a JSON request on stdin.

Credentials stay in the VM's private state directory and in the SSH response.
The child has its own session and closed SSH descriptors; no tunnel is needed.
"""
import base64
import fcntl
import http.client
import json
import os
from pathlib import Path
import signal
import socket
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


def health(port, credentials=None, harness="opencode"):
    path = "api/health" if harness == "opencode2" else "global/health"
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
            return response.status, body.get("healthy") is True and (harness != "opencode2" or isinstance(body.get("version"), str))
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
    harness = request.get("harness", "opencode")
    if harness not in ("opencode", "opencode2"):
        raise SetupError("Unsupported OpenCode harness.")
    os.umask(0o077)
    root = Path(home) / ".railway" / "desktop" / "opencode"
    state_path = root / "server.json"
    if request.get("action") == "inspect":
        # Discovery is read-only, returns no credentials, and must not seed
        # directories or start a server on an unrelated cloud agent.
        state = json.loads(state_path.read_text()) if state_path.exists() else {}
        port = server_port(state)
        if (state.get("harness", "opencode") == harness and owned_process(state)
                and health(port, state, harness) == (200, True)
                and health(port, harness=harness)[0] == 401):
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
        if owned_process(state) and state.get("harness", "opencode") != harness:
            raise SetupError("This agent is running another OpenCode edition. Use --new, or --remove with that edition's flag first.")
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

        port = server_port(state)
        if request.get("action") == "connect":
            if state.get("harness", "opencode") != harness or not state.get("password") or not state.get("username"):
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
        if not all(isinstance(value, str) and value for value in credentials.values()):
            raise SetupError("OpenCode requires a nonempty username and password.")

        reused = False
        if not port_available(port):
            if owned_process(state) and health(port, credentials, harness) == (200, True) and health(port, harness=harness)[0] == 401:
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
            state = dict(credentials, harness=harness, directory=directory, port=port,
                         vm_id=os.environ.get("RAILWAY_FACTORY_VM_ID"))
            save(state_path, state)
            with (root / "server.log").open("w") as log:
                child = subprocess.Popen(
                    [harness, "serve", "--hostname", "0.0.0.0", "--port", str(port)],
                    cwd=directory, env=environment, stdin=subprocess.DEVNULL,
                    stdout=log, stderr=subprocess.STDOUT, close_fds=True,
                    start_new_session=True,
                )
            state.update({"pid": child.pid, "start": process_start(child.pid)})
            save(state_path, state)
            deadline = time.monotonic() + (600 if harness == "opencode2" else 60)
            while time.monotonic() < deadline:
                if child.poll() is not None:
                    raise SetupError(f"OpenCode exited during startup. Check {root / 'server.log'} on the agent.")
                if health(port, credentials, harness) == (200, True):
                    if health(port, harness=harness)[0] != 401:
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
        if harness == "opencode":
            configure_permissions(port, credentials, directory)
        if reused and not state.get("vm_id") and os.environ.get("RAILWAY_FACTORY_VM_ID"):
            state["vm_id"] = os.environ["RAILWAY_FACTORY_VM_ID"]
            save(state_path, state)
        return dict(credentials, url=f"https://{domain}", directory=directory, reused=reused)


if __name__ == "__main__":
    try:
        result = setup(json.load(sys.stdin), Path.home())
        print("RAILWAY_OPENCODE_CONNECTION=" + json.dumps(result), flush=True)
    except (SetupError, OSError, ValueError, KeyError) as error:
        print(f"OpenCode setup failed: {error}", file=sys.stderr)
        sys.exit(1)
