#!/usr/bin/env python3
"""Offline workflow-shape regressions for CAD-1088 (no external packages)."""
from pathlib import Path
import os
import re
import subprocess
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parent.parent


def workflow(name):
    return (ROOT / ".github/workflows" / name).read_text()


def job(text, name):
    match = re.search(rf"^  {re.escape(name)}:\n(.*?)(?=^  [\w-]+:\n|\Z)",
                      text, re.M | re.S)
    if not match:
        raise AssertionError(f"missing job {name}")
    return match.group(1)


class CleanupTests(unittest.TestCase):
    def test_single_required_test_runner_preserves_doctests_and_floor(self):
        ci = workflow("ci.yml")
        test = job(ci, "test")
        self.assertIn("needs: [queue-evidence]", test)
        self.assertIn("cargo test --doc --locked", test)
        self.assertIn("cargo test --locked --test safety_floor", test)
        self.assertNotRegex(ci, r"(?m)^  test-(shard|once):")
        self.assertNotIn("needs.test-shard", ci)
        self.assertNotIn("needs.test-once", ci)
        self.assertNotIn("continue-on-error", test)

    def test_required_gates_and_merge_group_remain(self):
        ci = workflow("ci.yml")
        self.assertIn("  merge_group:", ci)
        for name in ("fmt", "clippy", "test", "build", "ui"):
            self.assertIn("needs.queue-evidence.outputs.tested != 'true'", job(ci, name))

    def test_live_doctor_manifest_cannot_be_skipped(self):
        fmt = job(workflow("ci.yml"), "fmt")
        self.assertIn("run: scripts/split-doctor-host --check", fmt)
        self.assertNotIn("tests/split-map-doctor.toml", fmt)
        self.assertNotIn("tests/split-map-host.toml", fmt)
        self.assertIn("python3 scripts/test_workflow_cleanup.py", fmt)
        self.assertIn("python3 scripts/test_pre_push_plan.py", fmt)

    def test_retired_journey_is_cargo_only_and_checks_guard_message(self):
        e2e = workflow("e2e.yml")
        self.assertNotIn("pnpm/action-setup", e2e)
        self.assertNotIn("actions/setup-node", e2e)
        self.assertNotIn("tests/e2e/pnpm-lock.yaml", e2e)
        self.assertIn("cargo check --release --locked --features test-seam", e2e)
        self.assertIn("grep -q 'must never be compiled into a release build'", e2e)
        self.assertNotIn("continue-on-error", e2e)

    def test_seam_probe_rejects_success_and_unrelated_build_failure(self):
        e2e = workflow("e2e.yml")
        block = re.search(r"      - name: MVP journey retired[^\n]*\n        run: \|\n(.*?)(?=      - if:)",
                          e2e, re.S).group(1)
        script = textwrap.dedent(block)
        with tempfile.TemporaryDirectory() as root:
            cargo = Path(root) / "cargo"
            for code, message, expected in (
                (0, "", 1),
                (1, "toolchain unavailable", 1),
                (1, "must never be compiled into a release build", 0),
            ):
                with self.subTest(code=code, message=message):
                    cargo.write_text(f"#!/bin/sh\necho '{message}' >&2\nexit {code}\n")
                    cargo.chmod(0o755)
                    env = dict(os.environ, RUNNER_TEMP=root,
                               PATH=root + os.pathsep + os.environ["PATH"])
                    result = subprocess.run(["bash", "-e", "-o", "pipefail", "-c", script],
                                            env=env, capture_output=True, text=True)
                    self.assertEqual(result.returncode, expected, result.stdout + result.stderr)

    def test_release_guards_are_still_hard_refusals(self):
        ci, staging = workflow("ci.yml"), workflow("staging.yml")
        for name in ("release-artifact", "release-gate"):
            self.assertIn("scripts/require-full-gates", job(ci, name))
        for name in ("select", "stage", "promote"):
            self.assertIn("scripts/require-full-gates", job(staging, name))
        self.assertTrue((ROOT / ".github/reduced-gates").is_file())


if __name__ == "__main__":
    unittest.main()
