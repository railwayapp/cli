"""VM-side conversation metadata. No prompts, transcript bodies, or agent launches.

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
                           "title": text(session.custom_title or session.summary or session.first_prompt) or "Claude conversation",
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
                       "title": title or "Grok conversation", "created_at": iso_timestamp(row.get("created_at")),
                       "updated_at": iso_timestamp(row.get("last_active_at")) or iso_timestamp(row.get("updated_at")) or "",
                       "state": "idle"},
            **active.get(info["id"], {}),
        })
    return rows


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
    # Relocations and restored histories can leave duplicate IDs in the tree.
    newest = {}
    for row in sorted(rows, key=lambda r: r["thread"]["updated_at"], reverse=True):
        newest.setdefault((row["harness"], row["thread"]["id"]), row)
    return {"threads": list(newest.values()), "warnings": warnings, "failed": failed}


if __name__ == "__main__":
    print(RESULT_PREFIX + json.dumps(discover(), ensure_ascii=True))
