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
        with socket.socket() as sock, socket.socket() as code:
            sock.bind(("127.0.0.1", 0))
            code.bind(("127.0.0.1", 0))
            self.port = sock.getsockname()[1]
            self.code_port = code.getsockname()[1]
        self.root = self.home / ".railway/codex"
        self.token = "a" * 43
        environment = {key: value for key, value in os.environ.items()
                       if not key.startswith("RAILWAY_PUBLIC_DOMAIN")}
        environment[f"RAILWAY_PUBLIC_DOMAIN_{self.port}"] = "test.example.com"
        env = patch.dict(os.environ, environment, clear=True)
        env.start()
        self.addCleanup(env.stop)
        ports = patch.multiple(bootstrap, LEGACY_PORT=self.port, CODE_PORT=self.code_port)
        ports.start()
        self.addCleanup(ports.stop)
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
        return bootstrap.setup(dict(directory=str(self.directory), token=self.token, **extra), self.home)

    def test_start_reuse_inspect_and_restart_preserve_identity_and_credentials(self):
        first = self.start()
        self.assertFalse(first["reused"])
        self.assertEqual(first["url"], "wss://test.example.com:443")
        self.assertEqual(first["directory"], str(self.directory))
        state = json.loads((self.root / "server.json").read_text())
        reused = bootstrap.setup(
            {"directory": str(self.directory), "token": "b" * 43}, self.home)
        self.assertTrue(reused["reused"])
        self.assertEqual(reused["token"], first["token"])
        self.assertEqual(len(self.children), 1)
        info = bootstrap.setup({"action": "inspect"}, self.home)
        self.assertEqual(set(info), {"directory", "version"})
        self.assertEqual(bootstrap.probe(self.port, "wrong", True), 401)
        for path in [self.root / "server.json", self.root / "server-token"]:
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.root.stat().st_mode & 0o777, 0o700)
        self.stop_children()
        restarted = bootstrap.setup({"action": "connect"}, self.home)
        self.assertFalse(restarted["reused"])
        self.assertEqual(restarted["token"], first["token"])
        self.assertNotEqual(json.loads((self.root / "server.json").read_text())["pid"], state["pid"])

    def test_unknown_discovery_is_read_only_and_connect_does_not_create_a_server(self):
        self.assertIsNone(bootstrap.setup({"action": "inspect"}, self.home))
        with self.assertRaisesRegex(bootstrap.SetupError, "no managed Codex"):
            bootstrap.setup({"action": "connect"}, self.home)
        self.assertFalse(self.root.exists())

    def test_reuse_cannot_silently_change_project(self):
        self.start()
        with self.assertRaisesRegex(bootstrap.SetupError, "already serving"):
            bootstrap.setup({"directory": str(self.home), "token": self.token}, self.home)
        self.assertEqual(len(self.children), 1)

    def test_busy_port_is_not_taken_over(self):
        with socket.socket() as sock:
            sock.bind(("0.0.0.0", self.port))
            sock.listen()
            with self.assertRaisesRegex(bootstrap.SetupError, "occupied by another process"):
                self.start()
        self.assertEqual(self.children, [])

    def test_code_endpoint_coexists_with_app_and_survives_reconnect_and_restart(self):
        os.environ[f"RAILWAY_PUBLIC_DOMAIN_{self.code_port}"] = "code.example.com"
        os.environ["RAILWAY_PUBLIC_DOMAIN"] = "app.example.com"
        with socket.socket() as app:
            app.bind(("0.0.0.0", self.port))
            app.listen()
            first = self.start()
            self.assertEqual(first["url"], "wss://code.example.com:443")
            self.assertEqual(json.loads((self.root / "server.json").read_text())["port"], self.code_port)
            self.assertIsNotNone(bootstrap.setup({"action": "inspect"}, self.home))
            self.assertTrue(bootstrap.setup({"action": "connect"}, self.home)["reused"])
            self.stop_children()
            restarted = bootstrap.setup({"action": "connect"}, self.home)
            self.assertFalse(restarted["reused"])
            self.assertEqual(restarted["url"], first["url"])
            self.assertEqual(restarted["token"], first["token"])

    def test_legacy_state_without_port_keeps_its_endpoint_when_code_domain_exists(self):
        first = self.start()
        path = self.root / "server.json"
        state = json.loads(path.read_text())
        state.pop("port")
        bootstrap.save(path, state)
        os.environ[f"RAILWAY_PUBLIC_DOMAIN_{self.code_port}"] = "code.example.com"
        self.assertIsNotNone(bootstrap.setup({"action": "inspect"}, self.home))
        reused = bootstrap.setup({"action": "connect"}, self.home)
        self.assertTrue(reused["reused"])
        self.assertEqual(reused["url"], first["url"])
        self.stop_children()
        restarted = bootstrap.setup({"action": "connect"}, self.home)
        self.assertEqual(restarted["url"], first["url"])
        self.assertEqual(restarted["token"], first["token"])
        self.assertEqual(json.loads(path.read_text())["port"], self.port)

    def test_saved_code_endpoint_never_falls_back_to_app_domain(self):
        os.environ[f"RAILWAY_PUBLIC_DOMAIN_{self.code_port}"] = "code.example.com"
        os.environ["RAILWAY_PUBLIC_DOMAIN"] = "app.example.com"
        self.start()
        del os.environ[f"RAILWAY_PUBLIC_DOMAIN_{self.code_port}"]
        with self.assertRaisesRegex(bootstrap.SetupError, f"no valid public address for port {self.code_port}"):
            bootstrap.setup({"action": "connect"}, self.home)

    def test_legacy_agent_can_use_the_unqualified_domain(self):
        del os.environ[f"RAILWAY_PUBLIC_DOMAIN_{self.port}"]
        os.environ["RAILWAY_PUBLIC_DOMAIN"] = "legacy.example.com"
        self.assertEqual(self.start()["url"], "wss://legacy.example.com:443")

    def test_busy_code_port_does_not_fall_back_to_the_free_app_port(self):
        os.environ[f"RAILWAY_PUBLIC_DOMAIN_{self.code_port}"] = "code.example.com"
        with socket.socket() as sock:
            sock.bind(("0.0.0.0", self.code_port))
            sock.listen()
            with self.assertRaisesRegex(bootstrap.SetupError, f"Port {self.code_port} is occupied"):
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
