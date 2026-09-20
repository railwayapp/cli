#!/usr/bin/env python3
"""Resolve an image-provided or pinned official OpenCode V2 server.

A saved remote version selects the same server release on restart. Only the
regular CLI binary is extracted, after SHA-512 verification.
"""
import base64
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import sqlite3
import subprocess
import time
import sys
import tarfile
import tempfile
import urllib.request

REGISTRY = "https://registry.npmjs.org/"
TESTED_VERSION = "2.0.8"


class InstallError(Exception):
    pass


def request(url):
    return urllib.request.urlopen(urllib.request.Request(url, headers={
        "User-Agent": "railway-opencode2", "Accept": "application/json",
    }), timeout=30)


def validate_version(version):
    if (not isinstance(version, str) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version)
            or int(version.split(".")[0]) != 2):
        raise InstallError("Unsupported OpenCode 2 release version.")
    return version


def release_asset(version, machine):
    validate_version(version)
    # Baseline works on x86_64 VMs without AVX2 as well as newer CPUs.
    arch = {"x86_64": "x64-baseline", "amd64": "x64-baseline", "aarch64": "arm64", "arm64": "arm64"}.get(machine)
    if not arch:
        raise InstallError(f"OpenCode 2 does not provide a Linux package for {machine}.")
    name = f"@opencode/cli-linux-{arch}"
    with request(REGISTRY + name.replace("/", "%2f") + "/" + version) as response:
        package = json.loads(response.read(2 * 1024 * 1024))
    url = f"{REGISTRY}{name}/-/cli-linux-{arch}-{version}.tgz"
    dist = package.get("dist", {})
    if package.get("name") != name or package.get("version") != version or dist.get("tarball") != url:
        raise InstallError("OpenCode 2 returned an unexpected package identity or download address.")
    integrity = dist.get("integrity", "")
    try:
        algorithm, digest = integrity.split("-", 1)
        valid = algorithm == "sha512" and len(base64.b64decode(digest, validate=True)) == 64
    except (ValueError, TypeError):
        valid = False
    if not valid:
        raise InstallError("OpenCode 2's package has no valid SHA-512 integrity digest.")
    return {"url": url, "integrity": integrity}


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def download(asset, path):
    with request(asset["url"]) as response, path.open("wb") as output:
        shutil.copyfileobj(response, output, 1024 * 1024)


def extract_binary(package, output):
    with tarfile.open(package, mode="r:gz") as archive:
        for member in archive:
            if member.name == "package/bin/opencode" and member.isfile():
                with archive.extractfile(member) as source, output.open("wb") as target:
                    shutil.copyfileobj(source, target, 1024 * 1024)
                    target.flush()
                    os.fsync(target.fileno())
                output.chmod(0o700)
                return
    raise InstallError("The OpenCode 2 package contains no standalone CLI executable.")


def install(root, version, machine, download_package=download):
    version = validate_version(version)
    # Keep releases separately: download/verification failure cannot replace the
    # running server, and an existing process retains its original executable.
    root = root / version
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    binary = root / "opencode2"
    record = root / "release.json"
    try:
        cached = json.loads(record.read_text()) if record.exists() else {}
    except (ValueError, OSError):
        cached = {}
    if (isinstance(cached, dict) and cached.get("version") == version and binary.is_file()
            and cached.get("binary_digest") == sha256(binary)):
        return binary
    asset = release_asset(version, machine)
    print(f"Downloading OpenCode 2 {version}...", file=sys.stderr, flush=True)
    with tempfile.TemporaryDirectory(prefix="download-", dir=root) as temporary:
        temporary = Path(temporary)
        package = temporary / "package.tgz"
        download_package(asset, package)
        digest = hashlib.sha512()
        with package.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
        if "sha512-" + base64.b64encode(digest.digest()).decode() != asset["integrity"]:
            raise InstallError("OpenCode 2 package checksum did not match; the current runtime was preserved.")
        staged = temporary / "opencode2"
        extract_binary(package, staged)
        checked = subprocess.run([str(staged), "--version"], capture_output=True, text=True, timeout=10)
        if checked.returncode or checked.stdout.strip() != f"opencode v{version}":
            raise InstallError("The downloaded OpenCode 2 runtime does not match the requested version.")
        state = {"version": version, "package_digest": asset["integrity"], "binary_digest": sha256(staged)}
        staged.replace(binary)
        staged_record = temporary / "release.json"
        staged_record.write_text(json.dumps(state))
        staged_record.replace(record)
    return binary


def ensure_runtime(version=None):
    if platform.system() != "Linux":
        raise InstallError("The OpenCode 2 shim runs on Linux cloud agents.")
    os.umask(0o077)
    root = Path.home() / ".railway/runtimes/opencode2"
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    version = version or os.environ.get("RAILWAY_OPENCODE_VERSION")
    # Images provide the official CLI. Inspect the actual executable, never the
    # opencode2 alias (old Railway aliases may invoke this installer recursively).
    candidates = [Path.home() / ".opencode/bin/opencode", shutil.which("opencode")]
    for candidate in filter(None, candidates):
        try:
            output = subprocess.run([str(candidate), "--version"], stdin=subprocess.DEVNULL,
                                    capture_output=True, text=True, timeout=10)
            installed = validate_version(output.stdout.strip().removeprefix("opencode v"))
            if output.returncode == 0 and (version is None or installed == version):
                return Path(candidate)
        except (OSError, subprocess.SubprocessError, InstallError):
            continue
    version = version or TESTED_VERSION
    with (root / "install.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        return install(root, version, platform.machine())


def credential_database():
    data = Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local/share") / "opencode"
    database = os.environ.get("OPENCODE_DB", "opencode.db")
    if database == ":memory:":
        raise InstallError("OpenCode provider credentials require a persistent database.")
    return data / database


def require_v2_storage(database):
    if database.is_file():
        with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as db:
            tables = {row[0] for row in db.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        if "session" in tables and "credential" not in tables:
            raise InstallError("This VM contains OpenCode 1 data. Use railway code --opencode upgrade <agent> before starting V2.")


def initialize_credentials(binary, database):
    if database.is_file():
        with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as db:
            if db.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='credential'").fetchone():
                return
            if db.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='session'").fetchone():
                raise InstallError("This VM contains OpenCode 1 data. Use railway code --opencode upgrade <agent> before importing V2 credentials.")
    # Let the installed V2 perform its own migrations. A private server exits
    # with the API client; it never joins an existing background service.
    environment = dict(os.environ, OPENCODE_DISABLE_MODELS_FETCH="1")
    process = subprocess.Popen(
        [str(binary), "api", "--standalone", "GET", "/api/info"],
        env=environment, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, start_new_session=True,
    )
    try:
        process.communicate(timeout=30)
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
    # errors from Beta's private server, which may contain credentials.
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
        import_only = sys.argv[1:] == ["--railway-import-auth"]
        pending = Path.home() / ".railway/runtimes/opencode2/credentials.json"
        if import_only and not pending.exists():
            sys.exit(0)
        if sys.argv[1:] != ["--version"]:
            require_v2_storage(credential_database())
        binary = ensure_runtime()
        if import_only:
            import_credentials(binary, pending)
            sys.exit(0)
        # Do not read stdin: terminal input and piped prompts belong to OpenCode.
        os.execv(str(binary), [str(binary), *sys.argv[1:]])
    except (InstallError, OSError, ValueError, KeyError, sqlite3.Error, tarfile.TarError) as error:
        print(f"OpenCode could not start: {error}", file=sys.stderr)
        sys.exit(1)
