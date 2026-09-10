"""Detached Codex App Server bootstrap; requests and credentials travel over SSH."""
import base64
import fcntl
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import sys
import time


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
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return None if fields[0] == "Z" else fields[19]
    except (OSError, IndexError):
        return None


def owned_process(state):
    pid, start = state.get("pid"), state.get("start")
    return isinstance(pid, int) and pid > 1 and start is not None and process_start(pid) == start


def probe(port, token=None, websocket=False):
    # Direct loopback probes bypass HTTP_PROXY. Health endpoints are intentionally
    # public; only a WebSocket upgrade tests the server's authentication boundary.
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=1)
    headers = {}
    if websocket:
        key = base64.b64encode(os.urandom(16)).decode()
        headers = {"Connection": "Upgrade", "Upgrade": "websocket",
                   "Sec-WebSocket-Key": key, "Sec-WebSocket-Version": "13"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    try:
        connection.request("GET", "/" if websocket else "/readyz", headers=headers)
        response = connection.getresponse()
        if websocket and response.status == 101:
            expected = base64.b64encode(hashlib.sha1(
                (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()
            ).digest()).decode()
            if response.getheader("Sec-WebSocket-Accept") != expected:
                return None
        return response.status
    except (OSError, http.client.HTTPException):
        return None
    finally:
        connection.close()


def healthy(port, token):
    return (probe(port) == 200 and probe(port, token, True) == 101
            and probe(port, websocket=True) == 401)


def port_available(port):
    try:
        # Some platforms let a reusable wildcard bind coexist with a listener
        # on a specific interface. Check loopback explicitly as well.
        for address in ("0.0.0.0", "127.0.0.1"):
            with socket.socket() as sock:
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                sock.bind((address, port))
                sock.listen(1)
        return True
    except OSError:
        return False


def runtime(environment, binary="codex"):
    try:
        help_result = subprocess.run([str(binary), "app-server", "--help"], env=environment,
                                     capture_output=True, text=True, timeout=15, check=True)
        if not all(flag in help_result.stdout for flag in ("--ws-auth", "--ws-token-file", "--listen")):
            raise SetupError("Update Codex on this agent: authenticated App Server WebSockets are required.")
        version = subprocess.run([str(binary), "--version"], env=environment,
                                 capture_output=True, text=True, timeout=15, check=True).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        raise SetupError("Could not run Codex on this agent. Install a current @openai/codex release and retry.") from None
    match = re.fullmatch(r"codex-cli ([0-9]+\.[0-9]+\.[0-9]+(?:-[a-zA-Z0-9.-]+)?)", version)
    if not match:
        raise SetupError("Could not identify the remote Codex version.")
    return match[1]


def update_runtime(home, root, environment):
    # Install beside the image's runtime. Validate the new binary before
    # touching a running server, and keep npm output out of the SSH response.
    log_path = root / "update.log"
    try:
        with log_path.open("w") as log:
            result = subprocess.run(["npm", "view", "@openai/codex@latest", "version"],
                                    env=environment, stdin=subprocess.DEVNULL,
                                    stdout=subprocess.PIPE, stderr=log, text=True,
                                    timeout=60, check=True)
            version = result.stdout.strip()
            if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[a-zA-Z0-9.-]+)?", version):
                raise SetupError("Could not identify the latest official Codex release.")
            prefix = Path(home) / ".railway/runtimes/codex-server" / version
            binary = prefix / "node_modules/.bin/codex"
            if not binary.is_file():
                subprocess.run(["npm", "install", "--prefix", str(prefix), "--no-audit",
                                "--no-fund", f"@openai/codex@{version}"],
                               env=environment, stdin=subprocess.DEVNULL,
                               stdout=log, stderr=log, timeout=180, check=True)
            if runtime(environment, binary) != version:
                raise SetupError("The installed Codex server does not match the requested release.")
            return str(binary), version
    except (OSError, subprocess.SubprocessError):
        raise SetupError(f"Could not update Codex. Check {log_path} on the agent and retry.") from None


def stop_owned(state):
    if owned_process(state):
        os.killpg(state["pid"], signal.SIGTERM)
    deadline = time.monotonic() + 15
    while owned_process(state):
        if time.monotonic() >= deadline:
            raise SetupError("The previous Codex server did not stop for its update. Retry once it exits.")
        time.sleep(0.1)


def setup(request, home, port=8080):
    os.umask(0o077)
    action = request.get("action", "start")
    if action not in ("start", "connect", "inspect"):
        raise SetupError("Unsupported Codex server action.")
    root = Path(home) / ".railway" / "codex"
    state_path = root / "server.json"
    if action == "inspect":
        state = json.loads(state_path.read_text()) if state_path.exists() else {}
        if (owned_process(state) and state.get("token") and state.get("directory")
                and healthy(port, state["token"])):
            return {"directory": state["directory"], "version": state.get("version")}
        return None
    if action == "connect" and not state_path.exists():
        raise SetupError("This agent has no managed Codex server. Run railway code --codex --agent <name> first.")
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(root, 0o700)
    with (root / "setup.lock").open("w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise SetupError("Another Codex setup is running on this agent. Retry when it finishes.") from None
        state = json.loads(state_path.read_text()) if state_path.exists() else {}
        if action == "connect":
            if not state.get("directory") or not state.get("token"):
                raise SetupError("The saved Codex connection is incomplete. Rerun railway code --codex --agent <name>.")
            request = dict(request, directory=state["directory"], token=state["token"])
        directory = str(Path(request["directory"]).resolve(strict=True))
        if not Path(directory).is_dir():
            raise SetupError("Codex's working directory must be a directory.")
        domain = os.environ.get("RAILWAY_PUBLIC_DOMAIN_8080") or os.environ.get("RAILWAY_PUBLIC_DOMAIN")
        if not domain or not re.fullmatch(r"[a-zA-Z0-9.-]+", domain):
            raise SetupError("This agent has no valid public address for port 8080.")
        token = state.get("token") or request.get("token")
        if not isinstance(token, str) or not re.fullmatch(r"[a-zA-Z0-9_-]{43,}", token):
            raise SetupError("Codex requires a high-entropy URL-safe connection token.")

        reused = owned_process(state)
        if reused:
            if not healthy(port, token):
                raise SetupError(f"The managed Codex server is unhealthy. Check {root / 'server.log'} on the agent.")
            if state.get("directory") != directory:
                raise SetupError(f"Codex is already serving {state.get('directory')}. Use connect or --new for a different directory.")
        environment = dict(os.environ, HOME=str(home))
        environment["PATH"] = f"{home}/.local/bin:" + environment.get("PATH", "/usr/local/bin:/usr/bin:/bin")
        if not reused and not port_available(port):
            raise SetupError(f"Port {port} is occupied by another process. Use --new for a fresh agent.")
        if action == "start" or not reused:
            binary, version = update_runtime(home, root, environment)
            if reused and state.get("version") != version:
                stop_owned(state)
                reused = False
        if not reused:
            if not port_available(port):
                raise SetupError(f"Port {port} is occupied by another process. Use --new for a fresh agent.")
            token_path = root / "server-token"
            token_path.write_text(token)
            os.chmod(token_path, 0o600)
            state = dict(token=token, directory=directory, version=version)
            save(state_path, state)
            with (root / "server.log").open("w") as log:
                child = subprocess.Popen(
                    [binary, "app-server", "--listen", f"ws://0.0.0.0:{port}",
                     "--ws-auth", "capability-token", "--ws-token-file", str(token_path),
                     "-c", 'approval_policy="never"', "-c", 'sandbox_mode="danger-full-access"'],
                    cwd=directory, env=environment, stdin=subprocess.DEVNULL,
                    stdout=log, stderr=subprocess.STDOUT, close_fds=True, start_new_session=True,
                )
            state.update(pid=child.pid, start=process_start(child.pid))
            save(state_path, state)
            deadline = time.monotonic() + 45
            try:
                while time.monotonic() < deadline:
                    if child.poll() is not None:
                        raise SetupError(f"Codex exited during startup. Check {root / 'server.log'} on the agent.")
                    if probe(port) == 200:
                        if not healthy(port, token):
                            raise SetupError("Codex did not enforce WebSocket authentication; stopped it.")
                        break
                    time.sleep(0.25)
                else:
                    raise SetupError(f"Codex startup timed out. Check {root / 'server.log'} on the agent.")
            except BaseException:
                if child.poll() is None:
                    os.killpg(child.pid, signal.SIGTERM)
                raise
        # Codex's native --remote parser requires an explicit port, including
        # the default TLS port. Keep it in the wire value (URL serializers omit it).
        return dict(url=f"wss://{domain}:443", token=token, directory=directory,
                    version=state["version"], reused=reused)


if __name__ == "__main__":
    try:
        result = setup(json.load(sys.stdin), Path.home())
        print("RAILWAY_CODEX_CONNECTION=" + json.dumps(result), flush=True)
    except (SetupError, OSError, ValueError, KeyError) as error:
        print(f"Codex setup failed: {error}", file=sys.stderr)
        sys.exit(1)
