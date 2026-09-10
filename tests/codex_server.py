"""Exercise the SSH bootstrap against a real, disposable fake Codex process."""
import importlib.util
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[1] / "src/commands/cloud_agent/codex.py"
spec = importlib.util.spec_from_file_location("codex_bootstrap", SOURCE)
bootstrap = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bootstrap)

FAKE = r'''
import base64, hashlib, http.server, os, pathlib, sys
if "--help" in sys.argv:
    print("--listen" if os.environ.get("FAKE_OLD") else "--listen --ws-auth --ws-token-file")
    sys.exit()
if "--version" in sys.argv:
    print("codex-cli 0.153.4")
    sys.exit()
assert sys.argv[1] == "app-server"
assert sys.argv[sys.argv.index("--ws-auth") + 1] == "capability-token"
assert 'approval_policy="never"' in sys.argv
assert 'sandbox_mode="danger-full-access"' in sys.argv
token = pathlib.Path(sys.argv[sys.argv.index("--ws-token-file") + 1]).read_text()
port = int(sys.argv[sys.argv.index("--listen") + 1].rsplit(":", 1)[1])
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_GET(self):
        if self.path == "/readyz":
            self.send_response(200)
            self.end_headers()
            return
        if self.headers.get("Authorization") != "Bearer " + token and not os.environ.get("FAKE_NO_AUTH"):
            self.send_response(401)
            self.end_headers()
            return
        self.send_response(101)
        self.send_header("Connection", "Upgrade")
        self.send_header("Upgrade", "websocket")
        key = self.headers["Sec-WebSocket-Key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
        self.send_header("Sec-WebSocket-Accept", base64.b64encode(hashlib.sha1(key.encode()).digest()).decode())
        self.end_headers()
http.server.HTTPServer.allow_reuse_address = True
http.server.HTTPServer(("127.0.0.1", port), Handler).serve_forever()
'''


class BootstrapTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name).resolve()
        self.directory = self.home / "project with spaces"
        self.directory.mkdir()
        binary = self.home / ".local/bin/codex"
        binary.parent.mkdir(parents=True)
        binary.write_text(f"#!{sys.executable}\n" + FAKE)
        binary.chmod(0o700)
        npm = binary.with_name("npm")
        npm.write_text(f"#!{sys.executable}\n" + r'''
import os, pathlib, sys
home = pathlib.Path(os.environ['HOME'])
with (home / 'npm-calls').open('a') as log:
    log.write(' '.join(sys.argv[1:]) + '\n')
if os.environ.get('FAKE_UPDATE_FAIL'):
    print('registry unavailable', file=sys.stderr)
    sys.exit(1)
if sys.argv[1] == 'view':
    print(os.environ.get('FAKE_LATEST', '0.153.4'))
else:
    prefix = pathlib.Path(sys.argv[sys.argv.index('--prefix') + 1])
    binary = prefix / 'node_modules/.bin/codex'
    binary.parent.mkdir(parents=True, exist_ok=True)
    binary.write_text((home / '.local/bin/codex').read_text().replace('0.153.4', prefix.name))
    binary.chmod(0o700)
    print('installation output stays in update.log')
''')
        npm.chmod(0o700)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            self.port = sock.getsockname()[1]
        self.root = self.home / ".railway/codex"
        self.token = "a" * 43
        env = patch.dict(os.environ, {"RAILWAY_PUBLIC_DOMAIN_8080": "test.example.com"})
        env.start()
        self.addCleanup(env.stop)
        self.children = []
        popen = subprocess.Popen

        def launch(*args, **kwargs):
            child = popen(*args, **kwargs)
            if kwargs.get("start_new_session"):
                self.children.append(child)
            return child

        mock = patch.object(bootstrap.subprocess, "Popen", side_effect=launch)
        mock.start()
        self.addCleanup(mock.stop)
        # Linux uses /proc identity. macOS still exercises real subprocesses,
        # with the owned-child handle standing in for /proc's start timestamp.
        if sys.platform != "linux":
            mock = patch.object(bootstrap, "process_start", side_effect=lambda pid:
                str(pid) if any(c.pid == pid and c.poll() is None for c in self.children) else None)
            mock.start()
            self.addCleanup(mock.stop)
        self.addCleanup(self.stop_children)

    def stop_children(self):
        for child in self.children:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGTERM)
            child.wait(timeout=5)

    def start(self, **extra):
        return bootstrap.setup(dict(directory=str(self.directory), token=self.token, **extra), self.home, self.port)

    def test_start_reuse_inspect_and_restart_preserve_identity_and_credentials(self):
        first = self.start()
        self.assertFalse(first["reused"])
        self.assertEqual(first["url"], "wss://test.example.com:443")
        self.assertEqual(first["directory"], str(self.directory))
        state = json.loads((self.root / "server.json").read_text())
        reused = bootstrap.setup(
            {"directory": str(self.directory), "token": "b" * 43}, self.home, self.port)
        self.assertTrue(reused["reused"])
        self.assertEqual(reused["token"], first["token"])
        self.assertEqual(len(self.children), 1)
        info = bootstrap.setup({"action": "inspect"}, self.home, self.port)
        self.assertEqual(set(info), {"directory", "version"})
        self.assertEqual(bootstrap.probe(self.port, "wrong", True), 401)
        for path in [self.root / "server.json", self.root / "server-token"]:
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.root.stat().st_mode & 0o777, 0o700)
        self.stop_children()
        restarted = bootstrap.setup({"action": "connect"}, self.home, self.port)
        self.assertFalse(restarted["reused"])
        self.assertEqual(restarted["token"], first["token"])
        self.assertNotEqual(json.loads((self.root / "server.json").read_text())["pid"], state["pid"])

    def test_unknown_discovery_is_read_only_and_connect_does_not_create_a_server(self):
        self.assertIsNone(bootstrap.setup({"action": "inspect"}, self.home, self.port))
        with self.assertRaisesRegex(bootstrap.SetupError, "no managed Codex"):
            bootstrap.setup({"action": "connect"}, self.home, self.port)
        self.assertFalse(self.root.exists())

    def test_reuse_cannot_silently_change_project(self):
        self.start()
        with self.assertRaisesRegex(bootstrap.SetupError, "already serving"):
            bootstrap.setup({"directory": str(self.home), "token": self.token}, self.home, self.port)
        self.assertEqual(len(self.children), 1)

    def test_start_upgrades_running_instance_and_local_connection_version(self):
        first = self.start()
        old_pid = self.children[0].pid
        with patch.dict(os.environ, {"FAKE_LATEST": "0.154.0"}):
            updated = self.start()
            self.assertEqual(updated["version"], "0.154.0")
            self.assertFalse(updated["reused"])
            self.assertEqual(updated["token"], first["token"])
            self.assertEqual(updated["directory"], first["directory"])
            self.assertIsNotNone(self.children[0].poll())
            self.assertNotEqual(self.children[-1].pid, old_pid)
            self.assertTrue(self.start()["reused"])
        calls = (self.home / 'npm-calls').read_text()
        self.assertEqual(calls.count('install --prefix'), 2)
        self.assertIn('@openai/codex@0.154.0', calls)

    def test_connect_and_inspect_do_not_update_a_running_instance(self):
        first = self.start()
        calls = (self.home / 'npm-calls').read_text()
        with patch.dict(os.environ, {"FAKE_UPDATE_FAIL": "1"}):
            connected = self.start(action="connect")
            self.assertTrue(connected["reused"])
            self.assertEqual(connected["version"], first["version"])
            self.assertIsNotNone(self.start(action="inspect"))
        self.assertEqual((self.home / 'npm-calls').read_text(), calls)

    def test_failed_update_keeps_the_existing_server_available(self):
        first = self.start()
        with patch.dict(os.environ, {"FAKE_UPDATE_FAIL": "1"}):
            with self.assertRaisesRegex(bootstrap.SetupError, 'Could not update Codex'):
                self.start()
        self.assertTrue(bootstrap.healthy(self.port, first['token']))
        self.assertIn('registry unavailable', (self.root / 'update.log').read_text())
        self.assertEqual(len(self.children), 1)

    def test_invalid_registry_version_never_installs_or_stops_the_server(self):
        first = self.start()
        with patch.dict(os.environ, {"FAKE_LATEST": "../../malicious"}):
            with self.assertRaisesRegex(bootstrap.SetupError, 'latest official Codex release'):
                self.start()
        self.assertTrue(bootstrap.healthy(self.port, first['token']))
        self.assertEqual((self.home / 'npm-calls').read_text().count('install --prefix'), 1)

    def test_busy_port_is_not_taken_over(self):
        with socket.socket() as sock:
            sock.bind(("0.0.0.0", self.port))
            sock.listen()
            with self.assertRaisesRegex(bootstrap.SetupError, "occupied by another process"):
                self.start()
        self.assertEqual(self.children, [])

    def test_server_that_accepts_unauthenticated_upgrades_is_stopped(self):
        with patch.dict(os.environ, {"FAKE_NO_AUTH": "1"}):
            with self.assertRaisesRegex(bootstrap.SetupError, "authentication; stopped"):
                self.start()
        self.children[0].wait(timeout=5)

    def test_old_binary_is_rejected_before_server_start(self):
        with patch.dict(os.environ, {"FAKE_OLD": "1"}):
            with self.assertRaisesRegex(bootstrap.SetupError, "Update Codex"):
                self.start()
        self.assertEqual(self.children, [])

    def test_stale_pid_cannot_claim_an_existing_listener(self):
        self.start()
        state_path = self.root / "server.json"
        state = json.loads(state_path.read_text())
        state["start"] = "not-the-process-start-time"
        bootstrap.save(state_path, state)
        with self.assertRaisesRegex(bootstrap.SetupError, "occupied by another process"):
            self.start()
        self.assertIsNone(self.children[0].poll())

    def test_concurrent_setup_is_rejected(self):
        self.root.mkdir(parents=True)
        with (self.root / "setup.lock").open("w") as lock:
            bootstrap.fcntl.flock(lock, bootstrap.fcntl.LOCK_EX | bootstrap.fcntl.LOCK_NB)
            with self.assertRaisesRegex(bootstrap.SetupError, "Another Codex setup"):
                self.start()
        self.assertEqual(self.children, [])


if __name__ == "__main__":
    unittest.main()
