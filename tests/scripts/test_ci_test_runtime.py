#!/usr/bin/env python3
"""Runtime probe CLI refusal contracts; no package installs or Rust builds."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
CLI = ROOT / 'scripts/ci-test-runtime.py'
IMAGE = 'docker.io/library/rust@sha256:' + 'f' * 64
PIN = ("CI_TEST_IMAGE='" + IMAGE + "'\nCI_TEST_SNAPSHOT='20260901T000000Z'\n"
       "CI_TEST_OS_ID='debian'\nCI_TEST_OS_VERSION='13'\nCI_TEST_TOOLCHAIN='1.98.1'\n")


def load():
    import importlib.util
    spec = importlib.util.spec_from_file_location('ci_test_runtime', CLI)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


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
            self.assertEqual(result.stdout, 'image=' + IMAGE + '\nsnapshot=20260901T000000Z\nsuite=trixie\npackages=python3 zstd procps jq\nuid=1001\n')
            self.assertEqual(sorted(path.name for path in root.iterdir()), ['.config', 'scripts'])
            rejected = subprocess.run(['bash', str(bootstrap)],
                                      env=dict(os.environ, GITHUB_ACTIONS='false'), capture_output=True, text=True)
            self.assertNotEqual(rejected.returncode, 0)
            self.assertIn('pinned root CI job container', rejected.stderr)

    def test_repair_ownership_refuses_outside_the_pinned_container(self):
        # --repair-ownership chowns as root; outside the guarded CI job
        # container it must refuse before touching anything. CI-only env
        # makes the refusal testable without GITHUB_ACTIONS — the shell
        # gate rejects any non-container caller either way.
        with tempfile.TemporaryDirectory(prefix='crt-repair.') as temporary:
            root = Path(temporary)
            (root / '.config').mkdir()
            (root / 'scripts').mkdir()
            (root / '.config/ci-test-runtime.env').write_text(PIN)
            bootstrap = root / 'scripts/bootstrap'
            bootstrap.write_bytes((ROOT / 'scripts/ci-test-runtime-bootstrap').read_bytes())
            for env_extra in ({}, {'GITHUB_ACTIONS': 'true'},
                              {'GITHUB_ACTIONS': 'true',
                               'CADENCE_TEST_CONTAINER_IMAGE': IMAGE}):
                result = subprocess.run(
                    ['bash', str(bootstrap), '--repair-ownership'],
                    env={**os.environ, **env_extra},
                    capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0, env_extra)
                self.assertIn('pinned root CI job container', result.stderr)

    def test_repair_ownership_rejects_extra_args_and_unknown_modes(self):
        with tempfile.TemporaryDirectory(prefix='crt-mode.') as temporary:
            root = Path(temporary)
            (root / '.config').mkdir()
            (root / 'scripts').mkdir()
            (root / '.config/ci-test-runtime.env').write_text(PIN)
            bootstrap = root / 'scripts/bootstrap'
            bootstrap.write_bytes((ROOT / 'scripts/ci-test-runtime-bootstrap').read_bytes())
            for argv in (['--repair-ownership', 'extra'], ['--repar-ownership'],
                         ['--chown'], ['repair-ownership']):
                result = subprocess.run(['bash', str(bootstrap), *argv],
                                        env=dict(os.environ, GITHUB_ACTIONS='false'),
                                        capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0, argv)

    def test_pin_file_cannot_execute_shell(self):
        result = self.invoke(PIN + 'echo should-not-run\n', IMAGE)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, '')


class CapabilityProbes(unittest.TestCase):
    """Preflight probes run against the real installed tools, synthetic
    scratch state only — the merge-tree probe makes a throwaway git repo,
    the orphan probe a short-lived double-fork. Neither needs Docker or
    the suite's fixtures, so they can be exercised here directly."""

    def test_git_merge_base_probe_runs_real_merge_tree(self):
        module = load()
        # The probe must invoke a real merge-tree --merge-base on the
        # host git — not a version-string comparison — so a git lacking
        # the option fails here the way tests/audit_review.rs would.
        module.git_merge_base_capability(dict(os.environ))

    def test_orphan_reap_probe_detects_a_reaper(self):
        module = load()
        # On this dev host PID 1 reaps; the probe must pass quickly.
        module.orphan_reap_capability()

    def test_orphan_disappearance_during_proc_read_is_success(self):
        # Linux procfs can report ESRCH after open when the task is
        # reaped during read, rather than ENOENT before open. Both mean
        # the known exited child has disappeared; other I/O errors fail.
        module = load()
        real_read = Path.read_text

        def gone(path, *args, **kwargs):
            if str(path).startswith('/proc/') and str(path).endswith('/stat'):
                raise ProcessLookupError(3, 'No such process')
            return real_read(path, *args, **kwargs)

        with patch.object(Path, 'read_text', gone):
            module.orphan_reap_capability()

    def test_orphan_reap_probe_waits_for_the_orphan_to_leave(self):
        # A lingering zombie must fail the probe — without --init in the
        # job container the suite's detached mock panes stay zombies.
        module = load()
        real_read = Path.read_text

        def fake_read(self, *a, **kw):
            if str(self).endswith('/stat'):
                return '1 (orphan) Z 1'
            return real_read(self, *a, **kw)

        with patch.object(Path, 'read_text', fake_read):
            with self.assertRaises(ValueError):
                module.orphan_reap_capability()


if __name__ == '__main__':
    unittest.main()
