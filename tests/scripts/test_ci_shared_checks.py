#!/usr/bin/env python3
"""Shared script contracts run once, and still fail a required CI gate.

Like test_ci_gate_evidence.py, inspect and execute the checked-in workflow
blocks with stdlib only; CI needs no new YAML or tool dependency.
"""
from pathlib import Path
import re
import subprocess
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/ci.yml"
COMMANDS = (
    "python3 tests/scripts/test_ci_test_plan.py",
    "python3 tests/scripts/test_ci_rust_tests.py",
    "python3 tests/scripts/test_ci_shard_check.py",
    "python3 tests/scripts/test_nextest_cost_report.py",
    "python3 tests/scripts/test_delivery_candidate.py",
    "python3 tests/scripts/test_auto_stage.py",
    "python3 tests/scripts/test_native_review_observation.py",
    "scripts/split-doctor-host --check",
)


def job_body(workflow, name):
    body = workflow.split(f"  {name}:\n", 1)[1]
    return re.split(r"\n  \S[^:\n]*:\n", body, maxsplit=1)[0]


def run_blocks(job):
    """Return each run step and its shell body in this workflow's layout."""
    for step in re.split(r"(?m)^      - ", job)[1:]:
        match = re.search(r"(?m)^        run: (.+)$", step)
        if not match:
            # A bare `- run: ...` step has its run key on the first line.
            match = re.match(r"run: (.+)", step)
        if not match:
            continue
        if match[1] == "|":
            tail = step[match.end():].lstrip("\n")
            body = re.split(r"\n {0,9}\S", tail, maxsplit=1)[0]
            script = textwrap.dedent(body)
        else:
            script = match[1]
        yield step, script


def assert_shared_checks(case, workflow):
    fmt = job_body(workflow, "fmt")
    case.assertNotRegex(fmt, r"(?m)^    (strategy|continue-on-error):")
    case.assertNotRegex(fmt, r"(?m)^        shell:")
    case.assertIn("needs: [queue-evidence]", fmt)
    case.assertIn("if: ${{ !cancelled() && needs.queue-evidence.outputs.tested != 'true' }}", fmt)
    blocks = list(run_blocks(fmt))
    for command in COMMANDS:
        line = rf"(?m)^\s*(?:(?:- )?run: )?{re.escape(command)}\s*$"
        case.assertEqual(len(re.findall(line, workflow)), 1, command)
        matches = [(step, script) for step, script in blocks
                   if command in script.splitlines()]
        case.assertEqual(len(matches), 1, f"{command} must execute in fmt")
        step, _ = matches[0]
        case.assertNotRegex(step, r"(?m)^        (if|continue-on-error|shell):")
    # The moved checks still participate in both release evidence paths.
    for job in ("release-artifact", "release-gate"):
        body = job_body(workflow, job)
        case.assertIn("fmt", re.search(r"needs: \[([^\]]+)\]", body)[1])
    case.assertIn('"fmt"', job_body(workflow, "queue-evidence"))


class SharedChecks(unittest.TestCase):
    def setUp(self):
        self.workflow = WORKFLOW.read_text()

    def test_shared_checks_execute_once_in_required_fmt(self):
        assert_shared_checks(self, self.workflow)

    def test_self_check_is_required(self):
        blocks = list(run_blocks(job_body(self.workflow, "fmt")))
        command = "python3 tests/scripts/test_ci_shared_checks.py"
        matches = [(step, script) for step, script in blocks if command in script.splitlines()]
        self.assertEqual(len(matches), 1)
        self.assertNotRegex(matches[0][0], r"(?m)^        (if|continue-on-error|shell):")

    def test_dropped_command_is_rejected(self):
        for command in COMMANDS:
            with self.subTest(command=command), self.assertRaises(AssertionError):
                assert_shared_checks(self, self.workflow.replace(command, "true", 1))

    def test_duplicate_in_shard_is_rejected(self):
        for command in COMMANDS:
            mutated = self.workflow.replace("  test-shard:\n", f"  test-shard:\n    steps:\n      - run: {command}\n", 1)
            with self.subTest(command=command), self.assertRaises(AssertionError):
                assert_shared_checks(self, mutated)

    def test_conditional_or_nonblocking_steps_are_rejected(self):
        marker = "      - name: Test scope selection and execution contracts\n"
        self.assertIn(marker, self.workflow)
        for option in ("if: false", "continue-on-error: true", "shell: bash {0}"):
            mutated = self.workflow.replace(marker, marker + f"        {option}\n", 1)
            with self.subTest(option=option), self.assertRaises(AssertionError):
                assert_shared_checks(self, mutated)

    def test_nonblocking_or_matrix_job_is_rejected(self):
        for option in ("continue-on-error: true", "strategy:\n      matrix:\n        repeat: [1, 2]"):
            mutated = self.workflow.replace("  fmt:\n", f"  fmt:\n    {option}\n", 1)
            with self.subTest(option=option), self.assertRaises(AssertionError):
                assert_shared_checks(self, mutated)

    def test_shell_stops_on_every_shared_command_failure(self):
        # Execute the actual relocated blocks using Actions' default fail-fast
        # bash. No real build or provider is invoked. Even an early failure
        # inside the three-command delivery block must fail the step.
        blocks = list(run_blocks(job_body(self.workflow, "fmt")))
        scripts = [script for _, script in blocks if any(c in script.splitlines() for c in COMMANDS)]
        self.assertEqual(len(scripts), 6)
        for script in scripts:
            commands = script.splitlines()
            for failed in commands:
                probe = "\n".join("false" if c == failed else "true" for c in commands)
                with self.subTest(failed=failed):
                    result = subprocess.run(["bash", "-eo", "pipefail", "-c", probe], capture_output=True)
                    self.assertNotEqual(result.returncode, 0)
            result = subprocess.run(["bash", "-eo", "pipefail", "-c", "\n".join("true" for _ in commands)], capture_output=True)
            self.assertEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
