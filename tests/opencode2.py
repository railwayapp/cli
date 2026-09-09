"""Exercise the shipped installer with small, synthetic official-style packages."""
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

SHIM = Path(__file__).resolve().parents[1] / 'src/commands/cloud_agent/opencode2.py'
spec = importlib.util.spec_from_file_location('shim', SHIM)
shim = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shim)


def package(binary, symlink=False):
    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode='w:xz') as archive:
        unrelated = tarfile.TarInfo('../../unrelated')
        unrelated.size = 6
        archive.addfile(unrelated, io.BytesIO(b'ignore'))
        info = tarfile.TarInfo('./opt/OpenCode Beta/resources/opencode-cli')
        if symlink:
            info.type = tarfile.SYMTYPE
            info.linkname = '/etc/passwd'
            archive.addfile(info)
        else:
            info.size = len(binary)
            archive.addfile(info, io.BytesIO(binary))
    data = data.getvalue()
    header = f'{"data.tar.xz/":<16}{0:<12}{0:<6}{0:<6}{"100644":<8}{len(data):<10}`\n'.encode()
    return b'!<arch>\n' + header + data


def release(data, tag='v0.0.0-beta-1', arch='amd64'):
    name = f'opencode-desktop-linux-{arch}.deb'
    return {'tag_name': tag, 'assets': [{'name': name,
        'browser_download_url': f'{shim.DOWNLOADS}{tag}/{name}',
        'digest': 'sha256:' + hashlib.sha256(data).hexdigest()}]}


class ShimTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.data = package(b'#!/bin/sh\necho beta\n')
        self.downloads = 0

    def tearDown(self):
        self.tmp.cleanup()

    def download(self, asset, path):
        self.downloads += 1
        path.write_bytes(self.data)

    def install(self, metadata=None):
        return shim.install(self.root, metadata or release(self.data), 'x86_64', self.download)

    def test_cached_version_is_reused_and_new_version_replaces_it(self):
        binary = self.install()
        self.assertEqual(binary.stat().st_mode & 0o777, 0o700)
        self.install()
        self.assertEqual(self.downloads, 1)
        self.data = package(b'new beta executable')
        self.install(release(self.data, 'v0.0.0-beta-2'))
        self.assertEqual(self.downloads, 2)
        self.assertEqual(binary.read_bytes(), b'new beta executable')
        self.assertFalse((self.root.parent / 'unrelated').exists())

    def test_checksum_failure_preserves_the_installed_runtime(self):
        binary = self.install()
        before = binary.read_bytes()
        metadata = release(b'expected different bytes', 'v0.0.0-beta-2')
        with self.assertRaisesRegex(shim.InstallError, 'checksum'):
            self.install(metadata)
        self.assertEqual(binary.read_bytes(), before)
        self.assertEqual(json.loads((self.root / 'release.json').read_text())['tag'], 'v0.0.0-beta-1')

    def test_corrupt_cache_is_repaired(self):
        binary = self.install()
        binary.write_bytes(b'corrupt')
        self.install()
        self.assertEqual(self.downloads, 2)
        (self.root / 'release.json').write_text('invalid json')
        self.install()
        self.assertEqual(self.downloads, 3)

    def test_architecture_origin_and_checksum_are_validated(self):
        self.assertEqual(shim.release_asset(release(self.data, arch='arm64'), 'aarch64')['name'], 'opencode-desktop-linux-arm64.deb')
        for machine in ['riscv64', 'aarch64']:
            with self.assertRaises(shim.InstallError):
                shim.release_asset(release(self.data), machine)
        for field, value in [('browser_download_url', 'https://example.com/package.deb'), ('digest', '')]:
            metadata = release(self.data)
            metadata['assets'][0][field] = value
            with self.assertRaises(shim.InstallError):
                shim.release_asset(metadata, 'x86_64')

    def test_symlink_is_not_installed_as_executable(self):
        self.data = package(b'', symlink=True)
        with self.assertRaisesRegex(shim.InstallError, 'no standalone'):
            self.install()
        self.assertFalse((self.root / 'opencode2').exists())

    def test_exec_preserves_arguments_stdin_and_exit_status(self):
        binary = self.root / 'runtime'
        binary.write_text('#!' + sys.executable + '\nimport json,sys\nprint(json.dumps([sys.argv[1:],sys.stdin.read()]))\nsys.exit(23)\n')
        binary.chmod(0o700)
        # Substitute only the network installer; exercise the actual main/exec.
        source = SHIM.read_text().replace('binary = ensure_runtime()', 'binary = Path(' + repr(str(binary)) + ')')
        shim_path = self.root / 'shim.py'
        shim_path.write_text(source)
        result = subprocess.run([sys.executable, str(shim_path), '--prompt', 'hello $(literal)'], input='stdin prompt', text=True, capture_output=True)
        self.assertEqual(result.returncode, 23, result.stderr)
        self.assertEqual(json.loads(result.stdout), [['--prompt', 'hello $(literal)'], 'stdin prompt'])


if __name__ == '__main__':
    unittest.main()
