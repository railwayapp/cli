#!/usr/bin/env python3
"""CLI-seeded OpenCode 2 shim: install the latest official Beta, then exec it.

The Beta repository publishes Desktop packages rather than a current CLI npm
tag. Its Linux deb contains a standalone resources/opencode-cli executable.
Only that regular file is extracted; Desktop itself is never installed.
"""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import sqlite3
import subprocess
import time
import sys
import tarfile
import tempfile
import urllib.request

RELEASES = "https://api.github.com/repos/anomalyco/opencode-beta/releases?per_page=5"
DOWNLOADS = "https://github.com/anomalyco/opencode-beta/releases/download/"


class InstallError(Exception):
    pass


def request(url):
    return urllib.request.urlopen(urllib.request.Request(url, headers={
        "User-Agent": "railway-opencode2", "Accept": "application/vnd.github+json",
    }), timeout=30)


def latest_release():
    with request(RELEASES) as response:
        releases = json.loads(response.read(2 * 1024 * 1024))
    for release in releases:
        if not release.get("draft") and release.get("tag_name", "").startswith("v0.0.0-beta-"):
            return release
    raise InstallError("No published OpenCode 2 Beta release was found.")


def release_asset(release, machine):
    arch = {"x86_64": "amd64", "amd64": "amd64", "aarch64": "arm64", "arm64": "arm64"}.get(machine)
    if not arch:
        raise InstallError(f"OpenCode 2 Beta does not provide a Linux package for {machine}.")
    name = f"opencode-desktop-linux-{arch}.deb"
    asset = next((asset for asset in release.get("assets", []) if asset.get("name") == name), None)
    if not asset:
        raise InstallError(f"The latest OpenCode 2 Beta release has no {name} package.")
    url = asset.get("browser_download_url", "")
    if not url.startswith(DOWNLOADS) or url != f"{DOWNLOADS}{release['tag_name']}/{name}":
        raise InstallError("OpenCode 2 Beta returned an unexpected download address.")
    digest = asset.get("digest", "")
    if not digest.startswith("sha256:") or len(digest) != 71 or any(c not in "0123456789abcdef" for c in digest[7:]):
        raise InstallError("OpenCode 2 Beta's package has no valid SHA-256 digest.")
    return asset


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def download(asset, path):
    with request(asset["browser_download_url"]) as response, path.open("wb") as output:
        shutil.copyfileobj(response, output, 1024 * 1024)


def extract_binary(package, output):
    # Debian's ar container may use GNU trailing '/' member names. Stream its
    # compressed tar directly, without extracting any paths from the archive.
    with package.open("rb") as stream:
        if stream.read(8) != b"!<arch>\n":
            raise InstallError("The OpenCode 2 Beta download is not a Debian package.")
        while True:
            header = stream.read(60)
            if not header:
                break
            if len(header) != 60 or header[58:] != b"`\n":
                raise InstallError("Invalid Debian archive header.")
            name = header[:16].decode("ascii").strip().rstrip("/")
            size = int(header[48:58])
            position = stream.tell()
            if name.startswith("data.tar"):
                with tarfile.open(fileobj=stream, mode="r|*") as archive:
                    for member in archive:
                        if member.name.endswith("/resources/opencode-cli") and member.isfile():
                            with archive.extractfile(member) as source, output.open("wb") as target:
                                shutil.copyfileobj(source, target, 1024 * 1024)
                                target.flush()
                                os.fsync(target.fileno())
                            output.chmod(0o700)
                            return
                break
            stream.seek(position + size + size % 2)
    raise InstallError("The OpenCode 2 Beta package contains no standalone CLI executable.")


def install(root, release, machine, download_package=download):
    asset = release_asset(release, machine)
    binary = root / "opencode2"
    record = root / "release.json"
    try:
        cached = json.loads(record.read_text()) if record.exists() else {}
    except (ValueError, OSError):
        cached = {}
    if not isinstance(cached, dict):
        cached = {}
    if (cached.get("tag") == release["tag_name"] and cached.get("package_digest") == asset["digest"]
            and binary.is_file() and cached.get("binary_digest") == sha256(binary)):
        return binary
    print(f"Downloading OpenCode2 [Beta] {release['tag_name']}...", file=sys.stderr, flush=True)
    with tempfile.TemporaryDirectory(prefix="download-", dir=root) as temporary:
        temporary = Path(temporary)
        package = temporary / "package.deb"
        download_package(asset, package)
        if f"sha256:{sha256(package)}" != asset["digest"]:
            raise InstallError("OpenCode 2 Beta package checksum did not match; the current runtime was preserved.")
        staged = temporary / "opencode2"
        extract_binary(package, staged)
        state = {"tag": release["tag_name"], "package_digest": asset["digest"], "binary_digest": sha256(staged)}
        # Rename keeps existing sessions running on their original executable.
        staged.replace(binary)
        staged_record = temporary / "release.json"
        staged_record.write_text(json.dumps(state))
        staged_record.replace(record)
    print(f"OpenCode2 [Beta] {release['tag_name']} is ready.", file=sys.stderr, flush=True)
    return binary


def ensure_runtime():
    if platform.system() != "Linux":
        raise InstallError("The OpenCode 2 shim runs on Linux cloud agents.")
    os.umask(0o077)
    root = Path.home() / ".railway/runtimes/opencode2"
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (root / "install.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        return install(root, latest_release(), platform.machine())


def credential_database():
    data = Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local/share") / "opencode"
    database = os.environ.get("OPENCODE_DB", "opencode.db")
    if database == ":memory:":
        raise InstallError("OpenCode2 provider credentials require a persistent database.")
    return data / database


def initialize_credentials(binary, database):
    if database.is_file():
        with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as db:
            if db.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='credential'").fetchone():
                return
    # Let the installed Beta perform its own migrations. A private server exits
    # with the API client; it never joins an existing background service.
    environment = dict(os.environ, OPENCODE_DISABLE_MODELS_FETCH="1")
    process = subprocess.Popen(
        [str(binary), "api", "--standalone", "GET", "/api/health"],
        env=environment, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, start_new_session=True,
    )
    try:
        process.communicate(timeout=30)
        if process.returncode:
            raise InstallError("Could not initialize OpenCode2 provider storage; credentials were not imported.")
    except subprocess.TimeoutExpired:
        raise InstallError("Initializing OpenCode2 provider storage timed out; retry setup.") from None
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
        raise InstallError("Unsupported OpenCode2 provider credential payload.")
    seen = set()
    for item in payload["credentials"]:
        if not isinstance(item, dict):
            raise InstallError("Invalid OpenCode2 provider credential.")
        provider, value = item.get("integrationID"), item.get("value")
        if (not isinstance(provider, str) or not provider or provider.startswith("mcp_")
                or provider in seen or not isinstance(item.get("id"), str)
                or not item["id"].startswith("cred_") or not isinstance(item.get("label"), str)
                or not isinstance(value, dict)):
            raise InstallError("Invalid OpenCode2 provider credential.")
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
            raise InstallError("Unsupported OpenCode2 provider credential format.")
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
            raise InstallError("Invalid OpenCode2 provider credential payload.") from None
        initialize_credentials(binary, database)
        for path in (database, Path(str(database) + "-wal"), Path(str(database) + "-shm")):
            if path.exists():
                path.chmod(0o600)
        try:
            with sqlite3.connect(database.as_uri() + "?mode=rw", uri=True, timeout=5) as db:
                columns = {row[1] for row in db.execute("PRAGMA table_info(credential)")}
                required = {"id", "integration_id", "label", "value", "time_created", "time_updated"}
                if not required <= columns:
                    raise InstallError("Unsupported OpenCode2 provider database; credentials were not imported.")
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
            raise InstallError("Could not save OpenCode2 provider credentials; retry setup.") from None
        # Remove the transferred copy only after the transaction commits.
        pending.unlink()


if __name__ == "__main__":
    try:
        import_only = sys.argv[1:] == ["--railway-import-auth"]
        pending = Path.home() / ".railway/runtimes/opencode2/credentials.json"
        if import_only and not pending.exists():
            sys.exit(0)
        binary = ensure_runtime()
        if import_only:
            import_credentials(binary, pending)
            sys.exit(0)
        # Do not read stdin: terminal input and piped prompts belong to OpenCode.
        os.execv(str(binary), [str(binary), *sys.argv[1:]])
    except (InstallError, OSError, ValueError, KeyError, sqlite3.Error, tarfile.TarError) as error:
        print(f"OpenCode2 [Beta] could not start: {error}", file=sys.stderr)
        sys.exit(1)
