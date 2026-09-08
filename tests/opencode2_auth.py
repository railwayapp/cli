#!/usr/bin/env python3
"""Provider credential import regression checks; no real credentials or network."""
import importlib.util
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest

SHIM = Path(__file__).resolve().parents[1] / 'src/commands/cloud_agent/opencode2.py'
spec = importlib.util.spec_from_file_location('opencode2_auth', SHIM)
shim = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shim)


def credential(provider='openai', secret='local-access', identifier='cred_local'):
    return {'id': identifier, 'integrationID': provider, 'label': 'account',
            'value': {'type': 'oauth', 'methodID': 'chatgpt-browser', 'access': secret,
                      'refresh': 'local-refresh', 'expires': 4102444800000,
                      'metadata': {'accountID': 'account'}}}


class CredentialTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.database = self.root / 'opencode.db'
        self.pending = self.root / 'credentials.json'
        with sqlite3.connect(self.database) as db:
            db.executescript('''CREATE TABLE credential (id TEXT PRIMARY KEY, integration_id TEXT,
              label TEXT NOT NULL, value TEXT NOT NULL, active INTEGER, time_created INTEGER NOT NULL,
              time_updated INTEGER NOT NULL);
              CREATE TABLE session (value TEXT); INSERT INTO session VALUES ('private remote session');''')

    def tearDown(self):
        self.tmp.cleanup()

    def stage(self, *records):
        self.pending.write_text(json.dumps({'version': 1, 'credentials': list(records)}))
        self.pending.chmod(0o600)

    def apply(self):
        shim.import_credentials(Path('unused-runtime'), self.pending, self.database)

    def test_imports_oauth_metadata_and_keys_then_removes_transfer_file(self):
        oauth = credential()
        key = credential('opencode-go', identifier='cred_go')
        key['value'] = {'type': 'key', 'key': 'local-key'}
        self.stage(oauth, key)
        self.apply()
        with sqlite3.connect(self.database) as db:
            rows = db.execute('SELECT integration_id,value,active FROM credential ORDER BY integration_id').fetchall()
            self.assertEqual(len(rows), 2)
            self.assertEqual(json.loads(rows[0][1]), oauth['value'])
            self.assertEqual(rows[0][2], 1)
            self.assertEqual(db.execute('SELECT value FROM session').fetchone()[0], 'private remote session')
        self.assertFalse(self.pending.exists())

    def test_repeated_setup_preserves_refreshed_remote_credentials(self):
        self.stage(credential())
        self.apply()
        remote = credential(secret='refreshed-remotely')['value']
        with sqlite3.connect(self.database) as db:
            db.execute('UPDATE credential SET value=?', (json.dumps(remote),))
        self.stage(credential(secret='older-local-token'))
        self.apply()
        with sqlite3.connect(self.database) as db:
            rows = db.execute('SELECT value FROM credential').fetchall()
            self.assertEqual([json.loads(row[0]) for row in rows], [remote])

    def test_transaction_rolls_back_on_id_conflict_and_retains_transfer(self):
        self.stage(credential('existing'))
        self.apply()
        self.stage(credential('new', identifier='cred_new'), credential('conflict'))
        with self.assertRaisesRegex(shim.InstallError, 'Could not save'):
            self.apply()
        with sqlite3.connect(self.database) as db:
            self.assertEqual(db.execute('SELECT integration_id FROM credential').fetchall(), [('existing',)])
        self.assertTrue(self.pending.exists())

    def test_invalid_credentials_are_rejected_before_any_database_write(self):
        invalid = credential()
        invalid['value'] = {'type': 'oauth', 'refresh': 'NEVER-PRINT'}
        self.stage(credential('valid'), invalid)
        with self.assertRaises(shim.InstallError) as error:
            self.apply()
        self.assertNotIn('NEVER-PRINT', str(error.exception))
        with sqlite3.connect(self.database) as db:
            self.assertEqual(db.execute('SELECT count(*) FROM credential').fetchone()[0], 0)
        self.assertTrue(self.pending.exists())

    def test_unknown_database_schema_is_preserved(self):
        with sqlite3.connect(self.database) as db:
            db.executescript('DROP TABLE credential; CREATE TABLE credential (value TEXT); INSERT INTO credential VALUES ("keep");')
        self.stage(credential())
        with self.assertRaisesRegex(shim.InstallError, 'Unsupported'):
            self.apply()
        with sqlite3.connect(self.database) as db:
            self.assertEqual(db.execute('SELECT value FROM credential').fetchone()[0], 'keep')

    def test_empty_pending_and_mcp_records_are_not_imported(self):
        self.apply()
        self.stage(credential('mcp_example'))
        with self.assertRaises(shim.InstallError):
            self.apply()


if __name__ == '__main__':
    unittest.main()
