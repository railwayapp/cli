"""Opt-in POSIX TUI regression: python3 tests/codex_terminal_smoke.py <railway> <agent>.

Requires an already configured backend and a compatible local Codex installation.
Exercises real terminal input, idle status, cancellation, and polluted local
settings without running a model turn or stopping the backend.
"""
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time


QUERIES = [
    (b"\x1b[6n", b"\x1b[1;1R"),
    (b"\x1b[c", b"\x1b[?1;2c"),
    (b"\x1b[0c", b"\x1b[?1;2c"),
    (b"\x1b[?u", b"\x1b[?0u"),
    (b"\x1b[>c", b"\x1b[>0;276;0c"),
    (b"\x1b]10;?", b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"),
    (b"\x1b]11;?", b"\x1b]11;rgb:0000/0000/0000\x1b\\"),
]


def attach(binary, agent, inherited_home, directory, key, wait_for_idle):
    pid, fd = pty.fork()
    if pid == 0:
        os.environ.update(TERM="xterm-256color", CODEX_HOME=str(inherited_home))
        os.execv(binary, [binary, "code", "--codex", "connect", agent])
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
    output = bytearray()
    status = None
    launched = False
    started = None
    quit_at = None
    title = None
    idle_at_quit = False
    deadline = time.monotonic() + 90
    try:
        while time.monotonic() < deadline:
            if select.select([fd], [], [], 0.1)[0]:
                try:
                    chunk = os.read(fd, 65536)
                except OSError as error:
                    if error.errno == errno.EIO:
                        break
                    raise
                if not chunk:
                    break
                output.extend(chunk)
                for query, reply in QUERIES:
                    if query in chunk:
                        os.write(fd, reply)
                text = output.decode(errors="replace")
                plain = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", text)
                assert "Install Codex" not in plain, "Install a matching local Codex first"
                assert "Update now (runs" not in plain, "Codex's update prompt stole terminal input"
                if not launched and "Launch your local Codex client now?" in plain:
                    os.write(fd, b"\r")
                    launched = True
                if "Launching local Codex" in plain:
                    launched = True
                if started is None and "OpenAI Codex" in plain:
                    started = time.monotonic()
                titles = re.findall(r"\x1b\]0;([^\x07]*)\x07", text)
                if titles:
                    title = titles[-1]
            if started is not None and quit_at is None:
                elapsed = time.monotonic() - started
                remote_visible = title == Path(directory).name or (
                    title is not None and title.endswith(" " + Path(directory).name))
                if remote_visible and elapsed > (8 if wait_for_idle else 1):
                    idle_at_quit = title == Path(directory).name
                    os.write(fd, key)
                    quit_at = time.monotonic()
            if quit_at is not None and time.monotonic() - quit_at > 6:
                break
            done, raw = os.waitpid(pid, os.WNOHANG)
            if done:
                status = raw
                break
    finally:
        if status is None:
            done, raw = os.waitpid(pid, os.WNOHANG)
            if done:
                status = raw
        if status is None:
            os.killpg(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
        os.close(fd)
    # Never print the captured terminal: the results panel contains credentials.
    assert launched and started is not None, "Native Codex did not launch"
    assert quit_at is not None, "Did not reach cancellation check"
    if status is None:
        with tempfile.NamedTemporaryFile(prefix="codex-terminal-failure-", suffix=".log", delete=False) as log:
            log.write(output)
            evidence = log.name
        raise AssertionError(f"Codex did not detach: idle={idle_at_quit}, title={title!r}; private evidence: {evidence}")
    assert os.waitstatus_to_exitcode(status) == 0, "Railway did not exit successfully"
    assert not wait_for_idle or idle_at_quit, "Codex remained busy after MCP startup"
    assert b"Codex App Server Configuration:" in output, "Results panel was not restored"
    print(json.dumps({"quit_key": "Ctrl+C" if key == b"\x03" else "Ctrl+D",
                      "idle_checked": wait_for_idle, "exit_code": 0}))


def main():
    binary = str(Path(sys.argv[1]).resolve())
    agent = sys.argv[2]
    saved = json.loads(subprocess.check_output(
        [binary, "code", "get-config", agent, "--json"], text=True))
    with tempfile.TemporaryDirectory(prefix="railway-codex-terminal-") as home:
        # A local-only MCP name must not keep a remote TUI's startup pending.
        path = Path(home) / "config.toml"
        config = '[mcp_servers.railway_local_only]\ncommand = "not-a-remote-mcp-server"\n'
        path.write_text(config)
        for key, idle in [(b"\x04", False), (b"\x03", True), (b"\x03", True)]:
            attach(binary, agent, home, saved["codex"]["connection"]["directory"], key, idle)
            assert path.read_text() == config, "Inherited local configuration was modified"


if __name__ == "__main__":
    main()
