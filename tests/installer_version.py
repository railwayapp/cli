"""Run the installer with fake HTTP responses and a tiny release archive."""
import io
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest


INSTALLER = Path(__file__).resolve().parents[1] / 'install.sh'
RELEASES = 'https://github.com/railwayapp/cli/releases'
SHELLS = [shutil.which(name) for name in ('sh', 'bash', 'dash', 'ash')]
SHELLS = list(dict.fromkeys(shell for shell in SHELLS if shell))

CURL = r'''#!/bin/sh
printf '%s\n' "$*" >> "$TEST_CURL_LOG"
output=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output) output="$2"; shift ;;
    https://*) url="$1" ;;
  esac
  shift
done
case "$url" in
  https://api.github.com/*)
    printf '%s\n' '{"message":"API rate limit exceeded"}'
    exit 22
    ;;
  https://github.com/railwayapp/cli/releases/latest)
    printf '%s' "$TEST_LATEST_URL"
    if [ "$TEST_CURL_STATUS" != 0 ]; then
      printf '%s\n' "curl: ($TEST_CURL_STATUS) simulated request failure" >&2
    fi
    exit "$TEST_CURL_STATUS"
    ;;
  https://github.com/railwayapp/cli/releases/download/*)
    cp "$TEST_ARCHIVE" "$output"
    ;;
  *) exit 99 ;;
esac
'''


class InstallerVersionTests(unittest.TestCase):
    def run_installer(self, shell, args=('-y',), version=None,
                      latest_url=RELEASES + '/tag/v5.61.0', curl_status=0):
        with tempfile.TemporaryDirectory(prefix='railway-installer-test-') as tmp:
            root = Path(tmp)
            mock_bin = root / 'mock-bin'
            mock_bin.mkdir()
            curl = mock_bin / 'curl'
            curl.write_text(CURL)
            curl.chmod(0o755)
            bin_dir = root / 'railway' / 'bin'
            bin_dir.mkdir(parents=True)
            binary = b'#!/bin/sh\nprintf "railway 5.61.0\\n"\n'
            archive = root / 'release.tar.gz'
            with tarfile.open(archive, 'w:gz') as tar:
                member = tarfile.TarInfo('railway')
                member.size = len(binary)
                member.mode = 0o755
                tar.addfile(member, io.BytesIO(binary))
            if '--remove' in args:
                (bin_dir / 'railway').write_bytes(binary)
                (bin_dir / 'railway').chmod(0o755)
                # The uninstaller also removes an old global scratch binary.
                # Keep that cleanup away from the developer's /tmp/railway.
                rm = mock_bin / 'rm'
                rm.write_text('#!/bin/sh\n'
                              '[ "$*" = "-f /tmp/railway" ] && exit 0\n'
                              'exec /bin/rm "$@"\n')
                rm.chmod(0o755)

            env = {key: value for key, value in os.environ.items()
                   if not key.startswith('RAILWAY_')}
            env.update({
                'PATH': f'{mock_bin}{os.pathsep}{bin_dir}{os.pathsep}' + os.defpath,
                'RAILWAY_HOME': str(root / 'railway'),
                # Prevent shell startup edits outside this temporary directory.
                'SHELL': '/bin/false',
                'TERM': 'dumb',
                'TMPDIR': str(root),
                'TEST_CURL_LOG': str(root / 'curl.log'),
                'TEST_ARCHIVE': str(archive),
                'TEST_LATEST_URL': latest_url,
                'TEST_CURL_STATUS': str(curl_status),
            })
            if version is not None:
                env['RAILWAY_VERSION'] = version
            result = subprocess.run(
                [shell, str(INSTALLER), *args], env=env, cwd=root,
                stdin=subprocess.DEVNULL, capture_output=True, text=True,
                timeout=15,
            )
            log = root / 'curl.log'
            requests = log.read_text().splitlines() if log.exists() else []
            installed = (bin_dir / 'railway').exists()
            return result, requests, installed

    def test_install_succeeds_when_unauthenticated_api_is_rate_limited(self):
        for shell in SHELLS:
            with self.subTest(shell=shell):
                result, requests, installed = self.run_installer(shell)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertTrue(installed)
                self.assertEqual(len(requests), 2)
                self.assertNotIn('api.github.com', '\n'.join(requests))
                self.assertIn('/download/v5.61.0/railway-v5.61.0-', requests[1])

    def test_pinned_version_skips_lookup(self):
        for shell in SHELLS:
            with self.subTest(shell=shell):
                result, requests, installed = self.run_installer(
                    shell, version='5.60.0', curl_status=28)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertTrue(installed)
                self.assertEqual(len(requests), 1)
                self.assertIn('/download/v5.60.0/railway-v5.60.0-', requests[0])

    def test_failed_request_cannot_supply_a_version(self):
        for shell in SHELLS:
            for status in (22, 28, 60):
                with self.subTest(shell=shell, status=status):
                    result, requests, installed = self.run_installer(
                        shell, curl_status=status)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(installed)
                    self.assertEqual(len(requests), 1)
                    self.assertIn('Could not determine', result.stderr)
                    self.assertIn(f'curl: ({status})', result.stderr)

    def test_unexpected_redirects_do_not_trigger_a_download(self):
        for shell in SHELLS:
            for url in ('', RELEASES + '/latest', 'https://github.com/login',
                        RELEASES.replace('railwayapp', 'other') + '/tag/v5.61.0',
                        RELEASES + '/tag/v5.61.0/extra',
                        RELEASES + '/tag/vnot-a-version'):
                with self.subTest(shell=shell, url=url):
                    result, requests, installed = self.run_installer(shell, latest_url=url)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(installed)
                    self.assertEqual(len(requests), 1)
                    self.assertIn('Could not determine', result.stderr)

    def test_help_does_not_use_the_network(self):
        for shell in SHELLS:
            with self.subTest(shell=shell):
                result, requests, _ = self.run_installer(shell, args=('--help',))
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(requests, [])

    def test_remove_does_not_use_the_network(self):
        for shell in SHELLS:
            with self.subTest(shell=shell):
                result, requests, installed = self.run_installer(shell, args=('--remove',))
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertFalse(installed)
                self.assertEqual(requests, [])


if __name__ == '__main__':
    unittest.main()
