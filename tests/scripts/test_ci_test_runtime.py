#!/usr/bin/env python3
"""Runtime probe CLI refusal contracts; no package installs or Rust builds."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
CLI = ROOT / 'scripts/ci-test-runtime.py'
IMAGE = 'docker.io/library/rust@sha256:' + 'f' * 64
PIN = ("CI_TEST_IMAGE='" + IMAGE + "'\nCI_TEST_SNAPSHOT='20260901T000000Z'\n"
       "CI_TEST_OS_ID='debian'\nCI_TEST_OS_VERSION='12'\nCI_TEST_TOOLCHAIN='1.98.1'\n")


class RuntimeCLI(unittest.TestCase):
    def invoke(self, pin, image):
        with tempfile.TemporaryDirectory(prefix='crt.') as temporary:
            root = Path(temporary)
            (root / '.config').mkdir()
            (root / '.config/ci-test-runtime.env').write_text(pin)
            return subprocess.run([sys.executable, str(CLI), '--root', str(root)],
                                  env=dict(os.environ, CADENCE_TEST_CONTAINER_IMAGE=image),
                                  capture_output=True, text=True)

    def test_caller_image_does_not_override_source_pin(self):
        result = self.invoke(PIN, 'docker.io/library/rust@sha256:' + 'e' * 64)
        self.assertEqual(result.returncode, 1)
        self.assertIn('container selection', result.stderr)
        self.assertEqual(result.stdout, '')

    def test_duplicate_unknown_or_non_digest_source_pins_refuse(self):
        for pin in (PIN + "CI_TEST_IMAGE='" + IMAGE + "'\n",
                    PIN + "ALLOW_FALLBACK='yes'\n", PIN.replace(IMAGE, 'rust:latest'),
                    PIN.replace("CI_TEST_TOOLCHAIN='1.98.1'\n", '')):
            with self.subTest(pin=pin):
                result = self.invoke(pin, IMAGE)
                self.assertEqual(result.returncode, 1)
                self.assertEqual(result.stdout, '')

    def test_bootstrap_plan_is_read_only_and_keeps_snapshot_and_nonroot_identity(self):
        with tempfile.TemporaryDirectory(prefix='crt-plan.') as temporary:
            root = Path(temporary)
            (root / '.config').mkdir()
            (root / 'scripts').mkdir()
            (root / '.config/ci-test-runtime.env').write_text(PIN)
            bootstrap = root / 'scripts/bootstrap'
            bootstrap.write_bytes((ROOT / 'scripts/ci-test-runtime-bootstrap').read_bytes())
            result = subprocess.run(['bash', str(bootstrap), '--plan'], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, 'image=' + IMAGE + '\nsnapshot=20260901T000000Z\nsuite=bookworm\npackages=python3 zstd procps jq\nuid=1001\n')
            self.assertEqual(sorted(path.name for path in root.iterdir()), ['.config', 'scripts'])
            rejected = subprocess.run(['bash', str(bootstrap)],
                                      env=dict(os.environ, GITHUB_ACTIONS='false'), capture_output=True, text=True)
            self.assertNotEqual(rejected.returncode, 0)
            self.assertIn('pinned root CI job container', rejected.stderr)

    def test_pin_file_cannot_execute_shell(self):
        result = self.invoke(PIN + 'echo should-not-run\n', IMAGE)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, '')


if __name__ == '__main__':
    unittest.main()
