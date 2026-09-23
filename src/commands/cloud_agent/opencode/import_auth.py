#!/usr/bin/env python3
"""Import provider sign-ins staged by the Railway CLI into OpenCode's store.

Runs once per provision on the VM, after the credential seed has written
~/.railway/runtimes/opencode/credentials.json. OpenCode V2 keeps provider
credentials in the `credential` table of $XDG_DATA_HOME/opencode/opencode.db
(default ~/.local/share/opencode/opencode.db); the runtime is asked to create
and migrate that store itself before rows are inserted.
"""
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import signal
import sqlite3
import subprocess
import sys
import time

STATE = Path.home() / ".railway/runtimes/opencode"


class InstallError(Exception):
    pass


def executable_version(binary):
    output = subprocess.run([str(binary), "--version"], stdin=subprocess.DEVNULL,
                            capture_output=True, text=True, timeout=10)
    version = output.stdout.strip().removeprefix("opencode v")
    if output.returncode or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise InstallError("The OpenCode executable has an unsupported version.")
    return version


def resolve_binary():
    """The image's official OpenCode V2 executable, never a Railway shim."""
    candidates = [Path.home() / ".opencode/bin/opencode", shutil.which("opencode")]
    for candidate in filter(None, candidates):
        try:
            if executable_version(candidate).startswith("2."):
                return Path(candidate)
        except (OSError, subprocess.SubprocessError, InstallError):
            continue
    raise InstallError("OpenCode V2 is not installed on this agent. Create a new agent with "
                       "railway code --opencode --new, or run: curl -fsSL https://opencode.ai/v2/install | bash")


def credential_database():
    data = Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local/share") / "opencode"
    database = os.environ.get("OPENCODE_DB", "opencode.db")
    if database == ":memory:":
        raise InstallError("OpenCode provider credentials require a persistent database.")
    return data / database


def backup_v1_storage(database):
    """OpenCode 1 storage is migrated in place the first time V2 opens it.
    Keep a copy so a legacy server's history can be restored if needed."""
    if not database.is_file():
        return None
    with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as db:
        tables = {row[0] for row in db.execute("SELECT name FROM sqlite_master WHERE type='table'")}
    if "session" not in tables or "credential" in tables:
        return None
    STATE.mkdir(parents=True, exist_ok=True, mode=0o700)
    backup = STATE / f"opencode-v1-before-upgrade-{time.time_ns()}.db"
    with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as source, sqlite3.connect(backup) as target:
        source.backup(target)
    backup.chmod(0o600)
    return backup


def initialize_credentials(binary, database):
    if database.is_file():
        with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as db:
            if db.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='credential'").fetchone():
                return
    backup_v1_storage(database)
    # Let the installed V2 perform its own migrations. A private server exits
    # with the API client; it never joins an existing background service.
    environment = dict(os.environ, OPENCODE_DISABLE_MODELS_FETCH="1")
    process = subprocess.Popen(
        [str(binary), "api", "--standalone", "GET", "/api/info"],
        env=environment, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, start_new_session=True,
    )
    try:
        process.communicate(timeout=60)
        if process.returncode:
            raise InstallError("Could not initialize OpenCode provider storage; credentials were not imported.")
    except subprocess.TimeoutExpired:
        raise InstallError("Initializing OpenCode provider storage timed out; retry setup.") from None
    finally:
        # Also clean up a private child server if its client failed or timed out.
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()


def validate_credentials(payload):
    if not isinstance(payload, dict) or payload.get("version") != 1 or not isinstance(payload.get("credentials"), list):
        raise InstallError("Unsupported OpenCode provider credential payload.")
    seen = set()
    for item in payload["credentials"]:
        if not isinstance(item, dict):
            raise InstallError("Invalid OpenCode provider credential.")
        provider, value = item.get("integrationID"), item.get("value")
        if (not isinstance(provider, str) or not provider or provider.startswith("mcp_")
                or provider in seen or not isinstance(item.get("id"), str)
                or not item["id"].startswith("cred_") or not isinstance(item.get("label"), str)
                or not isinstance(value, dict)):
            raise InstallError("Invalid OpenCode provider credential.")
        seen.add(provider)
        metadata = value.get("metadata", {})
        valid = isinstance(metadata, dict) and all(isinstance(v, str) for v in metadata.values())
        if value.get("type") == "key":
            valid = valid and isinstance(value.get("key"), str)
        elif value.get("type") == "oauth":
            valid = (valid and all(isinstance(value.get(k), str) for k in ("methodID", "access", "refresh"))
                     and type(value.get("expires")) is int and value["expires"] >= 0)
        else:
            valid = False
        if not valid:
            raise InstallError("Unsupported OpenCode provider credential format.")
    return payload["credentials"]


def import_credentials(binary, pending, database=None):
    database = (database or credential_database()).resolve()
    # Serialize repeated setup attempts; never print the credential payload or
    # errors from OpenCode's private server, which may contain credentials.
    with pending.with_suffix(".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if not pending.exists():
            return
        try:
            credentials = validate_credentials(json.loads(pending.read_text()))
        except (ValueError, TypeError):
            raise InstallError("Invalid OpenCode provider credential payload.") from None
        initialize_credentials(binary, database)
        for path in (database, Path(str(database) + "-wal"), Path(str(database) + "-shm")):
            if path.exists():
                path.chmod(0o600)
        try:
            with sqlite3.connect(database.as_uri() + "?mode=rw", uri=True, timeout=5) as db:
                columns = {row[1] for row in db.execute("PRAGMA table_info(credential)")}
                required = {"id", "integration_id", "label", "value", "time_created", "time_updated"}
                if not required <= columns:
                    raise InstallError("Unsupported OpenCode provider database; credentials were not imported.")
                db.execute("BEGIN IMMEDIATE")
                for item in credentials:
                    # Preserve provider accounts already configured remotely,
                    # including refreshed OAuth tokens. Repeating setup is safe.
                    if db.execute("SELECT 1 FROM credential WHERE integration_id=?", (item["integrationID"],)).fetchone():
                        continue
                    now = int(time.time() * 1000)
                    fields = ["id", "integration_id", "label", "value", "time_created", "time_updated"]
                    values = [item["id"], item["integrationID"], item["label"], json.dumps(item["value"]), now, now]
                    if "active" in columns:
                        fields.append("active")
                        values.append(1)
                    db.execute(f"INSERT INTO credential ({','.join(fields)}) VALUES ({','.join('?' for _ in fields)})", values)
                db.commit()
        except sqlite3.Error:
            raise InstallError("Could not save OpenCode provider credentials; retry setup.") from None
        # Remove the transferred copy only after the transaction commits.
        pending.unlink()


if __name__ == "__main__":
    try:
        os.umask(0o077)
        pending = STATE / "credentials.json"
        if not pending.exists():
            sys.exit(0)
        import_credentials(resolve_binary(), pending)
    except (InstallError, OSError, ValueError, KeyError, sqlite3.Error) as error:
        print(f"OpenCode credentials were not imported: {error}", file=sys.stderr)
        sys.exit(1)
