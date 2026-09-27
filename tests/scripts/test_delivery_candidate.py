"""Candidate metadata is authority only after GitHub run identity is checked."""
import importlib.util
from pathlib import Path
import unittest

SCRIPT = Path(__file__).resolve().parents[2] / "scripts/delivery-candidate.py"


class CandidateTests(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location("candidate", SCRIPT)
        self.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.module)
        self.run = {
            "id": 42, "run_attempt": 1, "path": ".github/workflows/ci.yml",
            "head_sha": "a" * 40, "head_branch": "main", "event": "push",
            "status": "completed", "conclusion": "success",
            "repository": {"full_name": "favcrm/cadence"},
            "head_repository": {"full_name": "favcrm/cadence"},
        }

    def test_completed_main_build_is_accepted(self):
        self.module.validate_ci(self.run, "favcrm/cadence", 42)

    def test_forged_failed_fork_and_wrong_workflow_are_refused(self):
        for field, value in [
            ("id", 43), ("run_attempt", 0), ("head_sha", "main"),
            ("path", ".github/workflows/mutation.yml"),
            ("head_branch", "feat/mutation"), ("event", "pull_request"),
            ("status", "in_progress"), ("conclusion", "failure"),
            ("conclusion", "cancelled"),
            ("repository", {"full_name": "foreign/repo"}),
            ("head_repository", {"full_name": "fork/cadence"}),
        ]:
            with self.subTest(field=field, value=value):
                run = dict(self.run, **{field: value})
                with self.assertRaises(ValueError):
                    self.module.validate_ci(run, "favcrm/cadence", 42)

    def test_compile_failure_empty_or_wrong_test_is_not_a_killed_mutation(self):
        for code, xml in [
            (101, '<testsuite><testcase name="guard"><failure/></testcase></testsuite>'),
            (100, '<testsuites/>'),
            (100, '<testsuite><testcase name="other"><failure/></testcase></testsuite>'),
            (100, '<testsuite><testcase name="guard"/></testsuite>'),
            (100, '<testsuite><testcase name="guard"><skipped/></testcase></testsuite>'),
            (100, '<testsuite><testcase name="guard"><error/></testcase></testsuite>'),
        ]:
            with self.subTest(code=code, xml=xml):
                with self.assertRaises(ValueError):
                    self.module.validate_mutation(code, xml, "guard")

    def test_only_exact_assertion_failure_is_a_killed_mutation(self):
        self.module.validate_mutation(
            100, '<testsuite><testcase name="guard"><failure type="test failure"/></testcase></testsuite>', "guard"
        )


if __name__ == "__main__":
    unittest.main()
