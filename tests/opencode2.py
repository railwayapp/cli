"""Exercise V2 installation with small official-style npm packages; no network."""
import base64
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SHIM = Path(__file__).resolve().parents[1] / 'src/commands/cloud_agent/opencode2.py'
spec = importlib.util.spec_from_file_location('shim', SHIM)
shim = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shim)


def package(binary, symlink=False):
    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode='w:gz') as archive:
        unrelated = tarfile.TarInfo('../../unrelated')
        unrelated.size = 6
        archive.addfile(unrelated, io.BytesIO(b'ignore'))
        info = tarfile.TarInfo('package/bin/opencode')
        if symlink:
            info.type = tarfile.SYMTYPE
            info.linkname = '/etc/passwd'
            archive.addfile(info)
        else:
            info.size = len(binary)
            archive.addfile(info, io.BytesIO(binary))
    return data.getvalue()


def metadata(data, version='2.0.5', arch='x64-baseline'):
    name = f'@opencode/cli-linux-{arch}'
    return {'name': name, 'version': version, 'dist': {
        'tarball': f'{shim.REGISTRY}{name}/-/cli-linux-{arch}-{version}.tgz',
        'integrity': 'sha512-' + base64.b64encode(hashlib.sha512(data).digest()).decode()}}


class ShimTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.data = package(b'#!/bin/sh\necho "opencode v2.0.5"\n')
        self.downloads = 0

    def tearDown(self):
        self.tmp.cleanup()

    def download(self, asset, path):
        self.downloads += 1
        path.write_bytes(self.data)

    def install(self, version='2.0.5', release=None):
        with patch.object(shim, 'request', return_value=io.BytesIO(json.dumps(release or metadata(self.data, version)).encode())):
            return shim.install(self.root, version, 'x86_64', self.download)

    def test_cached_version_is_reused_and_upgrade_preserves_old_executable(self):
        binary = self.install()
        before = binary.read_bytes()
        self.assertEqual(binary.stat().st_mode & 0o777, 0o700)
        self.install()
        self.assertEqual(self.downloads, 1)
        self.data = package(b'#!/bin/sh\necho "opencode v2.0.6"\n')
        newer = self.install('2.0.6')
        self.assertEqual(self.downloads, 2)
        self.assertNotEqual(binary, newer)
        self.assertEqual(binary.read_bytes(), before)
        self.assertFalse((self.root.parent / 'unrelated').exists())

    def test_checksum_failure_preserves_the_installed_runtime(self):
        binary = self.install()
        before = binary.read_bytes()
        with self.assertRaisesRegex(shim.InstallError, 'checksum'):
            self.install('2.0.6', metadata(b'expected different bytes', '2.0.6'))
        self.assertEqual(binary.read_bytes(), before)
        self.assertFalse((self.root / '2.0.6/opencode2').exists())

    def test_corrupt_cache_is_repaired(self):
        binary = self.install()
        binary.write_bytes(b'corrupt')
        self.install()
        self.assertEqual(self.downloads, 2)
        (binary.parent / 'release.json').write_text('invalid json')
        self.install()
        self.assertEqual(self.downloads, 3)

    def test_architecture_origin_identity_and_integrity_are_validated(self):
        good = metadata(self.data, arch='arm64')
        with patch.object(shim, 'request', return_value=io.BytesIO(json.dumps(good).encode())):
            self.assertIn('linux-arm64', shim.release_asset('2.0.5', 'aarch64')['url'])
        with self.assertRaises(shim.InstallError):
            shim.release_asset('2.0.5', 'riscv64')
        for field, value in [('tarball', 'https://example.com/package.tgz'), ('integrity', '')]:
            bad = metadata(self.data)
            bad['dist'][field] = value
            with self.assertRaises(shim.InstallError):
                self.install(release=bad)
        bad = metadata(self.data)
        bad['version'] = '2.0.4'
        with self.assertRaises(shim.InstallError):
            self.install(release=bad)
        for version in ['../other', '2.0.5/../../other', '0.0.0-beta-19425']:
            with self.assertRaises(shim.InstallError):
                self.install(version)

    def test_downloaded_binary_must_report_requested_version(self):
        self.data = package(b'#!/bin/sh\necho "opencode v2.0.4"\n')
        with self.assertRaisesRegex(shim.InstallError, 'requested version'):
            self.install()
        self.assertFalse((self.root / '2.0.5/opencode2').exists())

    def test_symlink_is_not_installed_as_executable(self):
        self.data = package(b'', symlink=True)
        with self.assertRaisesRegex(shim.InstallError, 'no standalone'):
            self.install()
        self.assertFalse((self.root / '2.0.5/opencode2').exists())

    def test_exec_preserves_arguments_stdin_and_exit_status(self):
        binary = self.root / 'runtime'
        binary.write_text('#!' + sys.executable + '\nimport json,sys\nprint(json.dumps([sys.argv[1:],sys.stdin.read()]))\nsys.exit(23)\n')
        binary.chmod(0o700)
        source = SHIM.read_text().replace('binary = ensure_runtime()', 'binary = Path(' + repr(str(binary)) + ')')
        shim_path = self.root / 'shim.py'
        shim_path.write_text(source)
        result = subprocess.run([sys.executable, str(shim_path), '--prompt', 'hello $(literal)'], input='stdin prompt', text=True, capture_output=True)
        self.assertEqual(result.returncode, 23, result.stderr)
        self.assertEqual(json.loads(result.stdout), [['--prompt', 'hello $(literal)'], 'stdin prompt'])


if __name__ == '__main__':
    unittest.main()
