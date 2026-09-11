"""Hermetic tests of the actual remote bootstrap, including detached children."""
import importlib.util
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time
import unittest

BOOTSTRAP = Path(__file__).resolve().parents[1] / 'src/commands/cloud_agent/opencode.py'
# Production uses /proc PID start times. macOS CI exercises the same lifecycle
# using ps, keeping the production bootstrap Linux-specific.
WRAPPER = '''
import importlib.util,json,sys,subprocess
from pathlib import Path
spec = importlib.util.spec_from_file_location('bootstrap', sys.argv[1])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
m.LEGACY_PORT = int(sys.argv[3])
m.CODE_PORT = int(sys.argv[4])
if sys.platform == 'darwin':
    def process_start(pid):
        result = subprocess.run(['ps', '-p', str(pid), '-o', 'stat=', '-o', 'lstart='], capture_output=True, text=True)
        fields = result.stdout.strip().split(None, 1)
        # Process status changes between probes; only start time identifies it.
        return fields[1] if result.returncode == 0 and len(fields) == 2 and not fields[0].startswith('Z') else None
    m.process_start = process_start
try:
    print(json.dumps(m.setup(json.load(sys.stdin), Path(sys.argv[2]))))
except Exception as error:
    print(str(error), file=sys.stderr)
    sys.exit(1)
'''
FAKE = '''
import base64,json,os,socket,sys
from pathlib import Path
from urllib.parse import urlparse
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
# HTTPServer.server_bind does reverse DNS, which can stall on macOS CI.
# The fake server must use loopback only, including hostname resolution.
socket.getfqdn = lambda host: 'localhost'
class Handler(BaseHTTPRequestHandler):
    def authenticated(self):
        expected = 'Basic ' + base64.b64encode((os.environ['OPENCODE_SERVER_USERNAME'] + ':' + os.environ['OPENCODE_SERVER_PASSWORD']).encode()).decode()
        if not os.environ.get('TEST_DISABLE_AUTH') and self.headers.get('Authorization') != expected:
            self.send_response(401)
            self.end_headers()
            return False
        return True
    def config_path(self):
        return Path(os.environ['HOME']) / '.config/opencode/opencode.json'
    def do_GET(self):
        if not self.authenticated(): return
        self.send_response(200)
        self.end_headers()
        if urlparse(self.path).path in ('/config', '/global/config'):
            path = self.config_path()
            self.wfile.write(path.read_bytes() if path.exists() else b'{}')
        else:
            self.wfile.write(json.dumps({'healthy': True, 'directory': os.getcwd(), **({'version': 'v0.0.0-beta-test'} if 'opencode2' in sys.argv[0] else {})}).encode())
    def do_PATCH(self):
        if not self.authenticated(): return
        path = self.config_path()
        config = json.loads(path.read_text()) if path.exists() else {}
        updates = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        def merge(original, updates):
            for key, value in updates.items():
                if isinstance(value, dict) and isinstance(original.get(key), dict):
                    merge(original[key], value)
                else:
                    original[key] = value
        if not os.environ.get('TEST_IGNORE_PERMISSIONS'):
            merge(config, updates)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(json.dumps(config))
        self.send_response(200)
        self.end_headers()
        self.wfile.write(json.dumps(config).encode())
    def log_message(self,*args): pass
port = int(sys.argv[sys.argv.index('--port') + 1])
ThreadingHTTPServer(('0.0.0.0', port), Handler).serve_forever()
'''


class BootstrapTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home = Path(self.tmp.name)
        self.directory = self.home / "project ' with $(touch INJECTED)"
        self.directory.mkdir()
        binary = self.home / '.opencode/bin/opencode'
        binary.parent.mkdir(parents=True)
        binary.write_text('#!' + sys.executable + '\n' + FAKE)
        binary.chmod(0o700)
        beta = self.home / '.local/bin/opencode2'
        beta.parent.mkdir(parents=True)
        beta.write_text(binary.read_text())
        beta.chmod(0o700)
        self.harness = 'opencode'
        with socket.socket() as sock, socket.socket() as code, socket.socket() as custom:
            sock.bind(('127.0.0.1', 0))
            code.bind(('127.0.0.1', 0))
            custom.bind(('127.0.0.1', 0))
            self.port = sock.getsockname()[1]
            self.code_port = code.getsockname()[1]
            self.custom_port = custom.getsockname()[1]
        self.env = {key: value for key, value in os.environ.items() if not key.startswith(('OPENCODE_SERVER_', 'RAILWAY_PUBLIC_DOMAIN', 'RAILWAY_CODE_PORT'))}
        self.env['RAILWAY_FACTORY_VM_ID'] = 'vm-source'
        self.env[f'RAILWAY_PUBLIC_DOMAIN_{self.port}'] = 'app-test.up.railway.app'
        self.state = self.home / '.railway/desktop/opencode/server.json'

    def run_bootstrap(self, request=None, check=True):
        request = dict(request or {'directory': str(self.directory), 'password': 'test-password'})
        request.setdefault('harness', self.harness)
        result = subprocess.run(
            [sys.executable, '-c', WRAPPER, str(BOOTSTRAP), str(self.home), str(self.port), str(self.code_port)],
            input=json.dumps(request),
            capture_output=True, text=True, timeout=15, env=self.env,
        )
        if check:
            self.assertEqual(result.returncode, 0, result.stderr)
            return json.loads(result.stdout)
        return result

    def tearDown(self):
        try:
            self.run_bootstrap({'action': 'stop'})
        finally:
            self.tmp.cleanup()

    def test_detaches_reuses_credentials_and_restarts(self):
        first = self.run_bootstrap()
        self.assertFalse(first['reused'])
        self.assertEqual(first['url'], 'https://app-test.up.railway.app')
        self.assertEqual(first['directory'], str(self.directory.resolve()))
        initial_state = json.loads(self.state.read_text())
        # The subprocess capturing stdout has exited: an inherited SSH output
        # descriptor would have kept communicate() above blocked until timeout.
        again = self.run_bootstrap({'directory': str(self.directory), 'password': 'different'})
        self.assertTrue(again['reused'])
        self.assertEqual(again['password'], first['password'])
        self.assertEqual(json.loads(self.state.read_text())['pid'], initial_state['pid'])
        self.assertEqual(self.state.stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.state.parent.stat().st_mode & 0o777, 0o700)
        self.assertFalse((self.home / 'INJECTED').exists())
        self.run_bootstrap({'action': 'stop'})
        restarted = self.run_bootstrap({'directory': str(self.directory), 'password': 'different'})
        self.assertFalse(restarted['reused'])
        self.assertEqual(restarted['password'], first['password'])

    def test_beta_starts_its_own_binary_and_rejects_cross_edition_reuse_or_stop(self):
        self.harness = 'opencode2'
        first = self.run_bootstrap()
        self.assertFalse(first['reused'])
        self.assertTrue(self.run_bootstrap()['reused'])
        self.assertEqual(json.loads(self.state.read_text())['harness'], 'opencode2')
        for request in [
            {'harness': 'opencode', 'action': 'stop'},
            {'harness': 'opencode', 'directory': str(self.directory), 'password': 'other'},
        ]:
            result = self.run_bootstrap(request, check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('another OpenCode edition', result.stderr)
        self.assertTrue(self.run_bootstrap()['reused'])

    def test_discovery_is_read_only_filters_editions_and_returns_no_password(self):
        self.assertIsNone(self.run_bootstrap({'action': 'inspect'}))
        self.assertFalse(self.state.parent.exists())
        self.run_bootstrap()
        before = self.state.read_bytes()
        result = self.run_bootstrap({'action': 'inspect'})
        self.assertEqual(result, {'directory': str(self.directory.resolve())})
        self.assertEqual(before, self.state.read_bytes())
        self.assertIsNone(self.run_bootstrap({'action': 'inspect', 'harness': 'opencode2'}))
        self.run_bootstrap({'action': 'stop'})
        self.assertIsNone(self.run_bootstrap({'action': 'inspect'}))

    def test_connect_reuses_or_restarts_only_a_saved_server(self):
        missing = self.run_bootstrap({'action': 'connect'}, check=False)
        self.assertNotEqual(missing.returncode, 0)
        self.assertFalse(self.state.parent.exists())
        first = self.run_bootstrap()
        again = self.run_bootstrap({'action': 'connect'})
        self.assertTrue(again['reused'])
        self.assertEqual(again['password'], first['password'])
        self.assertEqual(again['directory'], first['directory'])
        self.run_bootstrap({'action': 'stop'})
        wrong = self.run_bootstrap({'action': 'connect', 'harness': 'opencode2'}, check=False)
        self.assertNotEqual(wrong.returncode, 0)
        restarted = self.run_bootstrap({'action': 'connect'})
        self.assertFalse(restarted['reused'])
        self.assertEqual(restarted['password'], first['password'])
        self.assertEqual(restarted['directory'], first['directory'])

    def test_boot_credentials_take_precedence_over_candidate(self):
        self.env['OPENCODE_SERVER_USERNAME'] = 'boot-user'
        self.env['OPENCODE_SERVER_PASSWORD'] = 'boot-password'
        result = self.run_bootstrap()
        self.assertEqual(result['username'], 'boot-user')
        self.assertEqual(result['password'], 'boot-password')

    def test_setup_and_reconnect_enable_permissions_without_restarting_or_losing_config(self):
        path = self.home / '.config/opencode/opencode.json'
        path.parent.mkdir(parents=True)
        original = {'model': 'openai/test', 'mcp': {'test': {'type': 'remote', 'url': 'https://example.com'}},
                    'permission': {'bash': {'*': 'ask', 'rm *': 'deny'}, 'edit': 'ask'},
                    'agent': {'custom': {'description': 'Keep me', 'permission': {'bash': 'ask', 'edit': 'deny'}}}}
        path.write_text(json.dumps(original))
        self.run_bootstrap()
        configured = json.loads(path.read_text())
        self.assertEqual(configured['permission'], {'read': 'allow', 'external_directory': 'allow', 'doom_loop': 'allow', 'bash': {'*': 'allow', 'rm *': 'deny'}, 'edit': 'allow'})
        self.assertEqual(configured['agent']['custom'], {'description': 'Keep me', 'permission': {'bash': 'allow', 'edit': 'deny'}})
        self.assertEqual(configured['mcp'], original['mcp'])
        self.assertEqual(configured['model'], original['model'])
        pid = json.loads(self.state.read_text())['pid']
        path.write_text(json.dumps(original))
        self.assertTrue(self.run_bootstrap({'action': 'connect'})['reused'])
        self.assertEqual(json.loads(self.state.read_text())['pid'], pid)
        self.assertEqual(json.loads(path.read_text()), configured)

    def test_permission_update_failure_is_reported_and_leaves_server_running(self):
        self.env['TEST_IGNORE_PERMISSIONS'] = '1'
        result = self.run_bootstrap(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('did not apply automatic permissions', result.stderr)
        self.assertIsNotNone(self.run_bootstrap({'action': 'inspect'}))

    def test_custom_code_endpoint_coexists_with_app_and_survives_reconnect_and_restart(self):
        self.env['RAILWAY_CODE_PORT'] = str(self.custom_port)
        self.env[f'RAILWAY_PUBLIC_DOMAIN_{self.custom_port}'] = 'code-test.up.railway.app'
        self.env['RAILWAY_PUBLIC_DOMAIN'] = 'app-test.up.railway.app'
        with socket.socket() as app:
            app.bind(('0.0.0.0', self.port))
            app.listen()
            for harness in ('opencode', 'opencode2'):
                with self.subTest(harness=harness):
                    self.harness = harness
                    first = self.run_bootstrap()
                    self.assertEqual(first['url'], 'https://code-test.up.railway.app')
                    self.assertEqual(json.loads(self.state.read_text())['port'], self.custom_port)
                    self.assertIsNotNone(self.run_bootstrap({'action': 'inspect'}))
                    self.assertTrue(self.run_bootstrap({'action': 'connect'})['reused'])
                    self.run_bootstrap({'action': 'stop'})
                    restarted = self.run_bootstrap({'action': 'connect'})
                    self.assertFalse(restarted['reused'])
                    self.assertEqual(restarted['url'], first['url'])
                    self.assertEqual(restarted['password'], first['password'])
                    self.run_bootstrap({'action': 'stop'})
                    self.state.unlink()

    def test_legacy_state_without_port_keeps_its_endpoint_when_code_domain_exists(self):
        first = self.run_bootstrap()
        state = json.loads(self.state.read_text())
        state.pop('port')
        self.state.write_text(json.dumps(state))
        self.env[f'RAILWAY_PUBLIC_DOMAIN_{self.code_port}'] = 'code-test.up.railway.app'
        self.assertIsNotNone(self.run_bootstrap({'action': 'inspect'}))
        reused = self.run_bootstrap({'action': 'connect'})
        self.assertTrue(reused['reused'])
        self.assertEqual(reused['url'], first['url'])
        self.run_bootstrap({'action': 'stop'})
        restarted = self.run_bootstrap({'action': 'connect'})
        self.assertEqual(restarted['url'], first['url'])
        self.assertEqual(restarted['password'], first['password'])
        self.assertEqual(json.loads(self.state.read_text())['port'], self.port)

    def test_saved_code_endpoint_never_falls_back_to_app_domain(self):
        self.env[f'RAILWAY_PUBLIC_DOMAIN_{self.code_port}'] = 'code-test.up.railway.app'
        self.env['RAILWAY_PUBLIC_DOMAIN'] = 'app-test.up.railway.app'
        self.run_bootstrap()
        self.env.pop(f'RAILWAY_PUBLIC_DOMAIN_{self.code_port}')
        result = self.run_bootstrap({'action': 'connect'}, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f'no public address for port {self.code_port}', result.stderr)

    def test_checkpoint_restore_adopts_new_port_including_pre_port_state(self):
        for legacy in (False, True):
            with self.subTest(legacy=legacy):
                self.env['RAILWAY_FACTORY_VM_ID'] = 'vm-source'
                self.env.pop('RAILWAY_CODE_PORT', None)
                first = self.run_bootstrap()
                saved = json.loads(self.state.read_text())
                self.run_bootstrap({'action': 'stop'})
                if legacy:
                    saved.pop('port')
                    saved.pop('vm_id')
                self.state.write_text(json.dumps(saved))
                self.env['RAILWAY_FACTORY_VM_ID'] = 'vm-restored'
                self.env['RAILWAY_CODE_PORT'] = str(self.custom_port)
                self.env[f'RAILWAY_PUBLIC_DOMAIN_{self.custom_port}'] = 'code-restored.up.railway.app'
                with socket.socket() as app:
                    app.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                    app.bind(('0.0.0.0', self.port))
                    app.listen()
                    restored = self.run_bootstrap({'action': 'connect'})
                    self.assertEqual(restored['url'], 'https://code-restored.up.railway.app')
                    self.assertEqual(restored['password'], first['password'])
                    self.assertEqual(restored['directory'], first['directory'])
                    self.assertFalse(restored['reused'])
                    state = json.loads(self.state.read_text())
                    self.assertEqual((state['port'], state['vm_id']), (self.custom_port, 'vm-restored'))
                    self.run_bootstrap({'action': 'stop'})
                self.state.unlink()

    def test_restoring_without_endpoint_does_not_keep_source_code_port(self):
        self.env['RAILWAY_CODE_PORT'] = str(self.custom_port)
        self.env[f'RAILWAY_PUBLIC_DOMAIN_{self.custom_port}'] = 'code-source.up.railway.app'
        self.run_bootstrap()
        self.run_bootstrap({'action': 'stop'})
        self.env['RAILWAY_FACTORY_VM_ID'] = 'vm-restored'
        self.env.pop('RAILWAY_CODE_PORT')
        self.env.pop(f'RAILWAY_PUBLIC_DOMAIN_{self.custom_port}')
        self.assertEqual(self.run_bootstrap({'action': 'connect'})['url'], 'https://app-test.up.railway.app')

    def test_configured_port_requires_its_own_domain_and_rejects_invalid_ports(self):
        self.env['RAILWAY_CODE_PORT'] = str(self.custom_port)
        self.env[f'RAILWAY_PUBLIC_DOMAIN_{self.code_port}'] = 'code-other.up.railway.app'
        result = self.run_bootstrap(check=False)
        self.assertIn(f'no public address for port {self.custom_port}', result.stderr)
        for port in ('0', '1023', '8080', '8790', '65536', 'nope'):
            # LEGACY_PORT is remapped by the test wrapper.
            self.env['RAILWAY_CODE_PORT'] = str(self.port) if port == '8080' else port
            result = self.run_bootstrap(check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('RAILWAY_CODE_PORT', result.stderr)
        self.assertFalse(self.state.exists())

    def test_restored_pid_is_never_adopted_or_stopped(self):
        self.run_bootstrap()
        saved = self.state.read_text()
        self.env['RAILWAY_FACTORY_VM_ID'] = 'vm-restored'
        try:
            self.assertIsNone(self.run_bootstrap({'action': 'inspect'}))
            self.run_bootstrap({'action': 'stop'})
        finally:
            self.env['RAILWAY_FACTORY_VM_ID'] = 'vm-source'
            self.state.write_text(saved)
        self.assertTrue(self.run_bootstrap()['reused'])

    def test_legacy_agent_can_use_the_unqualified_domain(self):
        self.env.pop(f'RAILWAY_PUBLIC_DOMAIN_{self.port}')
        self.env['RAILWAY_PUBLIC_DOMAIN'] = 'legacy.up.railway.app'
        self.assertEqual(self.run_bootstrap()['url'], 'https://legacy.up.railway.app')

    def test_occupied_port_is_not_replaced(self):
        with socket.socket() as other:
            other.bind(('0.0.0.0', self.port))
            other.listen()
            result = self.run_bootstrap(check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('occupied', result.stderr)
            self.assertFalse(self.state.exists())

    def test_busy_code_port_does_not_fall_back_to_the_free_app_port(self):
        self.env[f'RAILWAY_PUBLIC_DOMAIN_{self.code_port}'] = 'code-test.up.railway.app'
        with socket.socket() as other:
            other.bind(('0.0.0.0', self.code_port))
            other.listen()
            result = self.run_bootstrap(check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(f'Port {self.code_port} is occupied', result.stderr)
            self.assertFalse(self.state.exists())

    def test_missing_domain_does_not_launch(self):
        self.env.pop(f'RAILWAY_PUBLIC_DOMAIN_{self.port}')
        result = self.run_bootstrap(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('no public address', result.stderr)
        self.assertFalse(self.state.exists())

    def test_unprotected_server_is_stopped(self):
        self.env['TEST_DISABLE_AUTH'] = '1'
        result = self.run_bootstrap(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('not enforcing password', result.stderr)

    def test_stop_does_not_signal_reused_pid(self):
        self.run_bootstrap()
        real_state = json.loads(self.state.read_text())
        altered = dict(real_state, pid=os.getpid(), start='wrong-start-time')
        self.state.write_text(json.dumps(altered))
        self.run_bootstrap({'action': 'stop'})
        self.state.write_text(json.dumps(real_state))
        # Reaching this assertion proves stop did not signal our process group.
        self.assertTrue(self.run_bootstrap()['reused'])


if __name__ == '__main__':
    unittest.main()
