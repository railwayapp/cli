"""VM-side conversation metadata without transcript bodies or agent launches.

Claude's versioned SDK owns transcript parsing; Grok's summary.json is its index.
The SDK is installed lazily into an isolated cache only when Claude history exists.
"""

import datetime
import importlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import sqlite3
import urllib.parse
import urllib.request
import base64

SDK_VERSION = "0.2.152"
RESULT_PREFIX = "RAILWAY-THREADS:"


def read_json(path):
    try:
        with path.open("rb") as file:
            data = file.read(1024 * 1024 + 1)
        if len(data) <= 1024 * 1024:
            return json.loads(data)
    except (OSError, ValueError):
        pass
    return None


def text(value):
    if not isinstance(value, str):
        return ""
    return " ".join("".join(c if c.isprintable() else " " for c in value).split())[:512]


def timestamp(milliseconds):
    if not isinstance(milliseconds, (int, float)):
        return None
    try:
        return datetime.datetime.fromtimestamp(
            milliseconds / 1000, datetime.timezone.utc
        ).isoformat()
    except (ValueError, OverflowError, OSError):
        return None


def iso_timestamp(value):
    if not isinstance(value, str):
        return None
    try:
        date = datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))
        if date.tzinfo is not None:
            return date.astimezone(datetime.timezone.utc).isoformat()
    except ValueError:
        pass
    return None


def process_environment(pid):
    try:
        if not isinstance(pid, int) or pid <= 0:
            return {}
        raw = Path(f"/proc/{pid}/environ").read_bytes()
        return dict(
            entry.decode(errors="replace").split("=", 1)
            for entry in raw.split(b"\0") if b"=" in entry
        )
    except (OSError, ValueError):
        return {}


def alive(pid):
    if not isinstance(pid, int) or pid <= 0:
        return False
    try:
        os.kill(pid, 0)
        return True
    except PermissionError:
        return True
    except OSError:
        return False


def live_identity(pid):
    if not alive(pid):
        return {}
    env = process_environment(pid)
    return {
        "active": True,
        "pane_id": env.get("RAILWAY_THREAD_PANE_ID"),
        "console_name": env.get("RAILWAY_DURABLE_SESSION_NAME"),
    }


def config_roots():
    roots = {
        "claude": {os.environ.get("CLAUDE_CONFIG_DIR", str(Path.home() / ".claude"))},
        "grok": {os.environ.get("GROK_HOME", str(Path.home() / ".grok"))},
    }
    # Sessions launched with per-process config overrides still belong to this VM.
    for process in Path("/proc").glob("[0-9]*"):
        try:
            if process.stat().st_uid != os.getuid():
                continue
            names = [Path(p.decode(errors="replace")).name
                     for p in (process / "cmdline").read_bytes().split(b"\0")[:2]]
            env = process_environment(int(process.name))
            for harness, key in (("claude", "CLAUDE_CONFIG_DIR"), ("grok", "GROK_HOME")):
                if harness in names and env.get(key):
                    roots[harness].add(env[key])
        except OSError:
            continue
    return {h: sorted(Path(p).expanduser() for p in paths) for h, paths in roots.items()}


def claude_sdk():
    cache = Path.home() / ".cache" / "railway" / f"claude-sessions-{SDK_VERSION}"
    if not (cache / "claude_agent_sdk").is_dir():
        import fcntl
        cache.parent.mkdir(parents=True, exist_ok=True)
        with (cache.parent / "claude-sessions.lock").open("w") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            if not (cache / "claude_agent_sdk").is_dir():
                staging = Path(tempfile.mkdtemp(prefix="claude-sessions-", dir=cache.parent))
                try:
                    if shutil.which("uv"):
                        command = ["uv", "pip", "install", "--python", sys.executable]
                    else:
                        command = [sys.executable, "-m", "pip", "install", "--disable-pip-version-check"]
                    subprocess.run(
                        command + ["--target", str(staging), f"claude-agent-sdk=={SDK_VERSION}"],
                        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                        timeout=90, check=True,
                    )
                    staging.rename(cache)
                finally:
                    if staging.exists():
                        shutil.rmtree(staging)
    sys.path.insert(0, str(cache))
    return importlib.import_module("claude_agent_sdk")


def claude_threads(root, sdk):
    previous = os.environ.get("CLAUDE_CONFIG_DIR")
    os.environ["CLAUDE_CONFIG_DIR"] = str(root)
    try:
        sessions = sdk.list_sessions()
        live = {}
        if shutil.which("claude"):
            try:
                result = subprocess.run(
                    ["claude", "agents", "--json", "--all"], stdin=subprocess.DEVNULL,
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5, check=True,
                )
                for row in json.loads(result.stdout):
                    if isinstance(row, dict) and row.get("sessionId"):
                        old = live.get(row["sessionId"], {})
                        if not old or alive(row.get("pid")):
                            live[row["sessionId"]] = row
            except (OSError, ValueError, subprocess.SubprocessError):
                pass  # Older Claude versions have history but no agents --json.
        rows = []
        for session in sessions:
            if not session.cwd:
                continue  # Resume must run in the saved directory, never an arbitrary /app.
            current = live.get(session.session_id, {})
            state = {"busy": "working", "waiting": "waiting", "idle": "idle"}.get(
                current.get("status"), {"blocked": "waiting", "working": "working",
                                        "failed": "failed", "done": "done"}.get(current.get("state"), "idle")
            )
            rows.append({
                "harness": "claude", "config_dir": str(root),
                "thread": {"id": session.session_id,
                           "title": text(session.custom_title or session.summary or session.first_prompt) or "New Thread",
                           "directory": session.cwd, "created_at": timestamp(session.created_at),
                           "updated_at": timestamp(session.last_modified) or "", "state": state},
                "background_id": current.get("id") if current.get("kind") == "background" else None,
                **live_identity(current.get("pid")),
            })
        return rows
    finally:
        if previous is None:
            os.environ.pop("CLAUDE_CONFIG_DIR", None)
        else:
            os.environ["CLAUDE_CONFIG_DIR"] = previous


def grok_threads(root):
    active = {}
    registry = read_json(root / "active_sessions.json")
    for row in registry if isinstance(registry, list) else []:
        if isinstance(row, dict) and row.get("session_id") and alive(row.get("pid")):
            active[row["session_id"]] = live_identity(row["pid"])
    rows = []
    for path in (root / "sessions").glob("*/*/summary.json"):
        row = read_json(path)
        if not isinstance(row, dict):
            continue
        info = row.get("info") or {}
        if not isinstance(info, dict) or not isinstance(info.get("id"), str) or not isinstance(info.get("cwd"), str):
            continue
        if not info["id"] or not info["cwd"].startswith("/"):
            continue
        kind = text(row.get("session_kind"))
        hidden = row.get("hidden")
        if hidden is True or (hidden is None and kind.startswith("subagent")):
            continue
        title = text(row.get("generated_title")) or text(row.get("session_summary"))
        if not title and not row.get("num_messages") and not (
            kind == "fork" or row.get("parent_session_id") or row.get("forked_at")
        ):
            continue
        rows.append({
            "harness": "grok", "config_dir": str(root),
            "thread": {"id": info["id"], "directory": info["cwd"],
                       "title": title or "New Thread", "created_at": iso_timestamp(row.get("created_at")),
                       "updated_at": iso_timestamp(row.get("last_active_at")) or iso_timestamp(row.get("updated_at")) or "",
                       "state": "idle"},
            **active.get(info["id"], {}),
        })
    return rows


def codex_threads():
    root = Path(os.environ.get("CODEX_HOME", str(Path.home() / ".codex")))
    paths = sorted((p for p in root.glob("state_*.sqlite") if p.stem[6:].isdigit()),
                   key=lambda p: int(p.stem[6:]), reverse=True)
    if not paths:
        return []
    # Codex maintains a metadata index independently of App Server's lifetime.
    with sqlite3.connect(paths[0].absolute().as_uri() + "?mode=ro", uri=True, timeout=2) as db:
        db.row_factory = sqlite3.Row
        columns = {row[1] for row in db.execute("PRAGMA table_info(threads)")}
        required = {"id", "cwd", "title", "created_at", "updated_at", "archived", "source"}
        if not required.issubset(columns):
            raise ValueError("Unsupported Codex metadata schema")
        fields = sorted(required | ({"name", "preview"} & columns))
        rows = db.execute("SELECT " + ",".join(fields) + " FROM threads WHERE archived=0 ORDER BY updated_at DESC")
        return [{"harness": "codex", "config_dir": str(root), "thread": {
            "id": row["id"], "directory": row["cwd"],
            "title": text((row["name"] if "name" in columns else None) or row["title"] or
                          (row["preview"] if "preview" in columns else None)) or "New Thread",
            "created_at": timestamp(row["created_at"] * 1000),
            "updated_at": timestamp(row["updated_at"] * 1000) or "", "state": "idle",
        }} for row in rows if row["source"] in ("cli", "vscode", "appServer", "exec")]


def opencode_threads():
    data = Path(os.environ.get("XDG_DATA_HOME", str(Path.home() / ".local" / "share"))) / "opencode"
    override = os.environ.get("OPENCODE_DB")
    paths = [Path(override) if Path(override).is_absolute() else data / override] if override and override != ":memory:" else sorted(data.glob("*.db"))
    rows = []
    for path in paths:
        if not path.is_file():
            continue
        with sqlite3.connect(path.absolute().as_uri() + "?mode=ro", uri=True, timeout=2) as db:
            db.row_factory = sqlite3.Row
            for table, harness in (("session", "opencode"), ("session_v2", "opencode2")):
                columns = {row[1] for row in db.execute(f"PRAGMA table_info({table})")}
                if not columns:
                    continue
                if not {"id", "title", "directory", "parent_id", "time_created", "time_updated", "time_archived"}.issubset(columns):
                    raise ValueError("Unsupported OpenCode metadata schema")
                for row in db.execute(f"SELECT id,title,directory,time_created,time_updated FROM {table} WHERE parent_id IS NULL AND time_archived IS NULL ORDER BY time_updated DESC"):
                    title = text(row["title"])
                    rows.append({"harness": harness, "config_dir": str(Path.home() / ".config" / "opencode"),
                        "database": str(path), "thread": {
                            "id": row["id"], "directory": row["directory"],
                            "title": "New Thread" if not title or title.startswith("New session - ") else title,
                            "created_at": timestamp(row["time_created"]), "updated_at": timestamp(row["time_updated"]) or "", "state": "idle",
                        }})
    return rows if paths else opencode_server_threads()


def opencode_server_threads():
    root = Path.home() / ".railway" / "desktop" / "opencode"
    state = read_json(root / "server.json")
    if not isinstance(state, dict):
        return []
    harness = state.get("harness", "opencode")
    if harness not in ("opencode", "opencode2"):
        return []
    beta = harness == "opencode2"
    credentials = base64.b64encode((state["username"] + ":" + state["password"]).encode()).decode()
    rows, cursor, seen = [], None, set()
    while True:
        query = {"limit": 100, "order": "desc"} if beta else {"directory": state["directory"]}
        if cursor:
            query["cursor"] = cursor
        request = urllib.request.Request("http://127.0.0.1:8080/" + ("api/" if beta else "") +
                                         "session?" + urllib.parse.urlencode(query),
                                         headers={"Authorization": "Basic " + credentials})
        with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request, timeout=5) as response:
            page = json.load(response)
        for row in page["data"] if beta else page:
            if row.get("parentID") or (row.get("time") or {}).get("archived"):
                continue
            title = text(row.get("title"))
            rows.append({"harness": harness, "config_dir": str(root), "thread": {
                "id": row["id"], "title": "New Thread" if not title or title.startswith("New session - ") else title,
                "directory": (row.get("location") or {}).get("directory") or row.get("directory") or state["directory"],
                "created_at": timestamp(row["time"]["created"]),
                "updated_at": timestamp(row["time"]["updated"]) or "", "state": "idle",
            }})
        cursor = (page.get("cursor") or {}).get("next") if beta else None
        if not cursor:
            return rows
        if cursor in seen:
            raise ValueError("Repeated conversation cursor")
        seen.add(cursor)


def discover():
    roots = config_roots()
    rows, warnings, failed = [], [], []
    for harness in ("grok", "claude"):
        try:
            for root in roots[harness]:
                if harness == "grok":
                    rows.extend(grok_threads(root))
                elif (root / "projects").is_dir():
                    rows.extend(claude_threads(root, claude_sdk()))
        except Exception as error:
            failed.append(harness)
            warnings.append(f"{harness} history unavailable ({type(error).__name__})")
    for harnesses, read in ((["codex"], codex_threads), (["opencode", "opencode2"], opencode_threads)):
        try:
            rows.extend(read())
        except Exception as error:
            failed.extend(harnesses)
            warnings.append(f"{'/'.join(harnesses)} history unavailable ({type(error).__name__})")
    # Relocations and restored histories can leave duplicate IDs in the tree.
    newest = {}
    for row in sorted(rows, key=lambda r: r["thread"]["updated_at"], reverse=True):
        newest.setdefault((row["harness"], row["thread"]["id"]), row)
    return {"threads": list(newest.values()), "warnings": warnings, "failed": failed}


if __name__ == "__main__":
    print(RESULT_PREFIX + json.dumps(discover(), ensure_ascii=True))
