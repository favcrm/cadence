"""Candidate metadata is authority only after GitHub run identity is checked."""
import importlib.util
from pathlib import Path
import unittest
import xml.etree.ElementTree as ET

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

    def test_authentic_nextest_filter_skips_are_not_executed_cases(self):
        # Run 36304100801: trusted control 3b0e6a9b, mutant b8dc1df8.
        report = (Path(__file__).parent / "fixtures/mutation-filtered.xml").read_text()
        self.module.validate_mutation(
            100, report,
            "cad667_migration_delivery_failure_keeps_reads_and_installs_closed_until_explicit_retry",
        )
        expected = "cad667_migration_delivery_failure_keeps_reads_and_installs_closed_until_explicit_retry"
        for code, name in [(0, expected), (1, expected), (101, expected), (100, "other")]:
            with self.subTest(code=code, name=name):
                with self.assertRaises(ValueError):
                    self.module.validate_mutation(code, report, name)

    def test_filter_skips_cannot_hide_mixed_status_or_other_executed_cases(self):
        failed = '<testcase name="guard"><failure/></testcase>'
        for additional in [
            '<testcase name="other"/>',
            '<testcase name="other"><failure/></testcase>',
            '<testcase name="other"><error/></testcase>',
            '<testcase name="other"><skipped/><failure/></testcase>',
            '<testcase name="other"><skipped/><error/></testcase>',
            '<testcase name="other"><skipped/><skipped/></testcase>',
            '<testcase name="other"><skipped><failure/></skipped></testcase>',
        ]:
            with self.subTest(additional=additional):
                with self.assertRaises(ValueError):
                    self.module.validate_mutation(100, f'<testsuite>{failed}{additional}</testsuite>', "guard")

    def test_authentic_failure_cannot_hide_unrelated_skips_or_duplicate_identities(self):
        report = (Path(__file__).parent / "fixtures/mutation-filtered.xml").read_text()
        name = "cad667_migration_delivery_failure_keeps_reads_and_installs_closed_until_explicit_retry"
        for variant in ("ignored", "missing_reason", "skipped_target", "duplicate_skip", "duplicate_target"):
            with self.subTest(variant=variant):
                root = ET.fromstring(report)
                suite = root.find("testsuite")
                skipped = suite.find("testcase")
                if variant == "ignored":
                    skipped.find("skipped").set("message", "ignored by user")
                elif variant == "missing_reason":
                    skipped.find("skipped").attrib.pop("message")
                elif variant == "skipped_target":
                    # Even a different class cannot label the requested target skipped.
                    skipped.set("name", name)
                    skipped.set("classname", "other")
                elif variant == "duplicate_skip":
                    suite.append(ET.fromstring(ET.tostring(skipped)))
                else:
                    duplicate = ET.fromstring(ET.tostring(skipped))
                    duplicate.set("name", name)
                    suite.append(duplicate)
                with self.assertRaises(ValueError):
                    self.module.validate_mutation(100, ET.tostring(root), name)

    def test_malformed_and_conflicting_failure_reports_are_rejected(self):
        for report in [
            '<testsuite>',
            '<not-junit><testcase name="guard"><failure/></testcase></not-junit>',
            '<testsuite><testcase name="guard"><failure/><failure/></testcase></testsuite>',
            '<testsuite><testcase name="guard"><failure/><skipped/></testcase></testsuite>',
            '<testsuite><testcase name="guard"><failure/><error/></testcase></testsuite>',
        ]:
            with self.subTest(report=report):
                with self.assertRaises(ValueError):
                    self.module.validate_mutation(100, report, "guard")


if __name__ == "__main__":
    unittest.main()
