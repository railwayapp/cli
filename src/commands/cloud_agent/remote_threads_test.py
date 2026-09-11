import json
import os
from pathlib import Path
import tempfile
import sqlite3
from contextlib import closing
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import remote_threads as threads


class DiscoveryTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    def grok(self, session_id, **fields):
        row = {"info": {"id": session_id, "cwd": "/app/project with spaces"},
               "session_summary": "Original title", "created_at": "2026-09-10T10:00:00Z",
               "updated_at": "2026-09-10T11:00:00Z", "num_messages": 4, **fields}
        path = self.root / "sessions" / "encoded-cwd" / session_id / "summary.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(row))
        return path

    def test_grok_history_includes_exited_threads_and_visible_forks(self):
        self.grok("renamed", generated_title="My renamed thread", title_is_manual=True)
        self.grok("hidden-child", session_kind="subagent_fork")
        self.grok("visible-child", session_kind="subagent", hidden=False)
        self.grok("empty", session_summary="", num_messages=0)
        self.grok("fork", session_summary="", num_messages=0, parent_session_id="renamed")
        self.grok("corrupt").write_text("{truncated")
        with patch.object(threads.subprocess, "run") as run:
            rows = {r["thread"]["id"]: r for r in threads.grok_threads(self.root)}
        self.assertEqual(set(rows), {"renamed", "visible-child", "fork"})
        self.assertEqual(rows["renamed"]["thread"]["title"], "My renamed thread")
        self.assertEqual(rows["renamed"]["thread"]["directory"], "/app/project with spaces")
        self.assertFalse(rows["renamed"].get("active", False))
        run.assert_not_called()

    def test_grok_stale_pids_do_not_attach_and_exact_pane_ids_are_preserved(self):
        self.grok("live")
        self.grok("exited")
        (self.root / "active_sessions.json").write_text(json.dumps([
            {"session_id": "live", "pid": 123}, {"session_id": "exited", "pid": 456}]))
        with patch.object(threads, "alive", side_effect=lambda pid: pid == 123), patch.object(
            threads, "process_environment", return_value={
                "RAILWAY_THREAD_PANE_ID": "pane-123", "RAILWAY_DURABLE_SESSION_NAME": "relay-123"}
        ):
            rows = {r["thread"]["id"]: r for r in threads.grok_threads(self.root)}
        self.assertEqual(rows["live"]["pane_id"], "pane-123")
        self.assertEqual(rows["live"]["console_name"], "relay-123")
        self.assertNotIn("console_name", rows["exited"])

    def test_claude_sdk_history_merges_status_by_uuid_not_background_id(self):
        saved = SimpleNamespace(session_id="full-uuid", custom_title="My title", summary="Auto title",
                                first_prompt="Prompt", cwd="/app/other", created_at=1000, last_modified=2000)
        def list_sessions():
            self.assertEqual(os.environ["CLAUDE_CONFIG_DIR"], str(self.root))
            return [saved]
        sdk = SimpleNamespace(list_sessions=list_sessions)
        status = [{"sessionId": "full-uuid", "id": "short-id", "kind": "background", "state": "blocked",
                   "status": "waiting", "pid": 123}]
        with patch.object(threads.shutil, "which", return_value="/bin/claude"), patch.object(
            threads.subprocess, "run", return_value=SimpleNamespace(stdout=json.dumps(status))
        ) as run, patch.object(threads, "live_identity", return_value={"active": True, "pane_id": "exact"}):
            rows = threads.claude_threads(self.root, sdk)
        self.assertEqual(run.call_args.args[0], ["claude", "agents", "--json", "--all"])
        self.assertEqual(rows[0]["thread"]["id"], "full-uuid")
        self.assertEqual(rows[0]["background_id"], "short-id")
        self.assertEqual(rows[0]["thread"]["title"], "My title")
        self.assertEqual(rows[0]["thread"]["state"], "waiting")
        self.assertEqual(rows[0]["thread"]["created_at"], "1970-01-01T00:00:01+00:00")

    def test_provider_failure_preserves_other_history(self):
        self.grok("grok-thread")
        (self.root / "projects").mkdir()
        with patch.object(threads, "codex_threads", return_value=[]), patch.object(threads, "opencode_threads", return_value=[]), patch.object(threads, "config_roots", return_value={"claude": [self.root], "grok": [self.root]}), patch.object(
            threads, "claude_sdk", side_effect=RuntimeError("SDK unavailable")
        ):
            result = threads.discover()
        self.assertEqual(result["failed"], ["claude"])
        self.assertEqual(result["threads"][0]["thread"]["id"], "grok-thread")

    def test_codex_metadata_works_without_a_running_backend_or_local_snapshot(self):
        with closing(sqlite3.connect(self.root / "state_5.sqlite")) as db, db:
            db.execute("CREATE TABLE threads (id TEXT, cwd TEXT, title TEXT, name TEXT, created_at INT, updated_at INT, archived INT, source TEXT)")
            db.executemany("INSERT INTO threads VALUES (?,?,?,?,?,?,?,?)", [
                ("real-id", "/app/other", "Original question", "Generated title", 100, 200, 0, "cli"),
                ("archived", "/app", "Hidden", None, 100, 200, 1, "cli"),
                ("child", "/app", "Subagent", None, 100, 200, 0, "subagent"),
            ])
        with patch.dict(os.environ, {"CODEX_HOME": str(self.root)}):
            rows = threads.codex_threads()
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["thread"]["id"], "real-id")
        self.assertEqual(rows[0]["thread"]["title"], "Generated title")

    def test_opencode_versions_are_read_only_and_keep_native_titles_and_directories(self):
        data = self.root / "opencode"
        data.mkdir()
        database = data / "opencode.db"
        with closing(sqlite3.connect(database)) as db, db:
            for table in ("session", "session_v2"):
                db.execute(f"CREATE TABLE {table} (id TEXT, title TEXT, directory TEXT, parent_id TEXT, time_created INT, time_updated INT, time_archived INT)")
                db.executemany(f"INSERT INTO {table} VALUES (?,?,?,?,?,?,?)", [
                    ("saved", "Sacramento weather", "/app/weather", None, 1000, 2000, None),
                    ("draft", "New session - 2026-09-10", "/app", None, 1000, 1000, None),
                    ("child", "Subagent", "/app", "saved", 1000, 2000, None),
                    ("archived", "Hidden", "/app", None, 1000, 2000, 3000),
                ])
        before = database.read_bytes()
        with patch.dict(os.environ, {"XDG_DATA_HOME": str(self.root), "OPENCODE_DB": ""}):
            rows = threads.opencode_threads()
        self.assertEqual(database.read_bytes(), before)
        self.assertEqual(len(rows), 4)
        self.assertEqual({row["harness"] for row in rows}, {"opencode", "opencode2"})
        for row in rows:
            self.assertEqual(row["database"], str(database))
            if row["thread"]["id"] == "saved":
                self.assertEqual(row["thread"]["title"], "Sacramento weather")
                self.assertEqual(row["thread"]["directory"], "/app/weather")
            else:
                self.assertEqual(row["thread"]["title"], "New Thread")


if __name__ == "__main__":
    unittest.main()
