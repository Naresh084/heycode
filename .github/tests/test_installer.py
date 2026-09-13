#!/usr/bin/env python3
"""Exercise install.sh offline; native release installation is a separate gate."""
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

INSTALLER = Path(__file__).resolve().parents[2] / 'install.sh'
BINARY = b'#!/bin/sh\nprintf "HeyCode 0.1.0\\n"\n'

class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='heycode-installer-test-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.tools = self.root / 'tools'
        self.tools.mkdir()
        self.install = self.root / 'directory with spaces'
        (self.root / 'binary').write_bytes(BINARY)
        (self.root / 'checksums').write_text(hashlib.sha256(BINARY).hexdigest() + '  heycode-linux-x86_64\n')
        for name, contents in {
            'uname': '#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo x86_64;; esac\n',
            'curl': '''#!/bin/sh
set -eu
source=
output=
while [ "$#" -gt 0 ]; do
 case "$1" in
  */SHA256SUMS) source="$FIXTURES/checksums";;
  */heycode-linux-x86_64) source="$FIXTURES/binary";;
  -o) shift; output=$1;;
 esac
 shift
done
test -n "$source" && test -n "$output"
cp "$source" "$output"
''',
        }.items():
            path = self.tools / name
            path.write_text(contents)
            path.chmod(0o755)
    def run_installer(self):
        env = dict(os.environ, PATH=str(self.tools) + os.pathsep + os.environ['PATH'],
                   FIXTURES=str(self.root), HEYCODE_VERSION='0.1.0', HEYCODE_INSTALL_DIR=str(self.install))
        return subprocess.run(['sh', str(INSTALLER)], env=env, capture_output=True, text=True)
    def test_install_replaces_only_after_verification_and_retains_previous(self):
        self.install.mkdir()
        target = self.install / 'heycode'
        target.write_bytes(b'previous executable')
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(target.read_bytes(), BINARY)
        self.assertTrue(os.access(target, os.X_OK))
        self.assertEqual((self.install / 'heycode.previous').read_bytes(), b'previous executable')
        self.assertTrue((self.install / '.heycode-install').is_file())
    def test_corrupt_download_preserves_installed_executable(self):
        self.install.mkdir()
        target = self.install / 'heycode'
        target.write_bytes(b'previous executable')
        (self.root / 'binary').write_bytes(b'corrupted download')
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('SHA-256 mismatch', result.stderr)
        self.assertEqual(target.read_bytes(), b'previous executable')
        self.assertFalse((self.install / '.heycode-install').exists())
    def test_symlink_target_does_not_overwrite_another_program(self):
        self.install.mkdir()
        other = self.root / 'another-program'
        other.write_bytes(b'untouched')
        (self.install / 'heycode').symlink_to(other)
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(other.read_bytes(), b'untouched')

if __name__ == '__main__':
    unittest.main()
