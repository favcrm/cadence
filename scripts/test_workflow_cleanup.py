#!/usr/bin/env python3
"""Executable workflow boundary checks plus one required-gate wiring smoke test."""
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
    def test_required_gates_still_have_real_checks_and_scope_wiring(self):
        ci = workflow("ci.yml")
        self.assertIn("  merge_group:", ci)
        for name in ("fmt", "clippy", "test", "build", "ui"):
            body = job(ci, name)
            self.assertIn("change-scope", body)
            self.assertIn("needs.queue-evidence.outputs.tested", body)
            commands = [step for step in re.split(r"(?=^      - )", body, flags=re.M)
                        if re.search(r"(?:run:|uses:).*(?:cargo |pnpm |rust-cache@|ci-rust-toolchain)", step)]
            self.assertTrue(commands, name)
            for step in commands:
                category = "ui" if name == "ui" else "rust"
                self.assertIn(f"needs.change-scope.outputs.{category} != 'false'", step)
        test = job(ci, "test")
        self.assertIn("cargo test --doc --locked", test)
        self.assertIn("cargo test --locked --test safety_floor", test)
        self.assertIn("scripts/split-doctor-host --check", job(ci, "fmt"))

    def test_scope_shell_defaults_full_on_missing_or_forged_policy(self):
        scope = job(workflow("ci.yml"), "change-scope")
        block = scope.split("        run: |\n", 1)[1]
        script = textwrap.dedent(block)
        with tempfile.TemporaryDirectory() as root:
            attacker = Path(root) / "scripts/ci-change-scope.py"
            attacker.parent.mkdir()
            attacker.write_text("from pathlib import Path\nPath('forged-policy-executed').touch()\n"
                                "print('{\"scope\":\"ui\",\"rust\":\"false\",\"ui\":\"true\"}')\n")
            git = Path(root) / "git"
            git.write_text("#!/bin/sh\n[ \"$1\" = show ] || exit 1\n"
                           "[ \"$2\" = \"$BASE:scripts/ci-change-scope.py\" ] || exit 1\n"
                           "[ \"$TEST_MISSING\" = yes ] && exit 1\n"
                           "printf '%s\\n' 'import os' 'print(os.environ[\"TEST_RESULT\"])'\n")
            git.chmod(0o755)
            for event, missing, result, expected in (
                ("pull_request", "yes", '{}', "full"),
                ("pull_request", "no", '{"scope":"docs","rust":"true","ui":"false"}', "full"),
                ("pull_request", "no", '{"scope":"docs","rust":"false","ui":"false"}', "docs"),
                ("merge_group", "no", '{"scope":"docs","rust":"false","ui":"false"}', "full"),
            ):
                with self.subTest(event=event, missing=missing, result=result):
                    output = Path(root) / "output"
                    output.write_text("")
                    env = dict(os.environ, EVENT=event, BASE="0" * 40, HEAD="1" * 40,
                               RUNNER_TEMP=root, GITHUB_WORKSPACE=root,
                               GITHUB_OUTPUT=str(output), GITHUB_STEP_SUMMARY=str(Path(root) / "summary"),
                               TEST_MISSING=missing, TEST_RESULT=result,
                               PATH=root + os.pathsep + os.environ["PATH"])
                    run = subprocess.run(["bash", "-e", "-o", "pipefail", "-c", script],
                                         env=env, cwd=root, capture_output=True, text=True)
                    self.assertEqual(run.returncode, 0, run.stderr)
                    self.assertIn(f"scope={expected}\n", output.read_text())
                    self.assertFalse((Path(root) / 'forged-policy-executed').exists())

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



if __name__ == "__main__":
    unittest.main()
