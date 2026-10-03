#!/usr/bin/env python3
"""Offline regressions for the reduced-window local check plan (CAD-1088)."""
import importlib.machinery
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
loader = importlib.machinery.SourceFileLoader("pre_push_plan", str(ROOT / "scripts/pre-push"))
spec = importlib.util.spec_from_loader(loader.name, loader)
pre_push = importlib.util.module_from_spec(spec)
loader.exec_module(pre_push)


class PlanTests(unittest.TestCase):
    def test_explicit_tests_runs_every_target_even_without_test_source_changes(self):
        for changed in ({"src/review.rs"}, {"docs/START-HERE.md"}, set(), None):
            with self.subTest(changed=changed):
                steps = pre_push.plan(str(ROOT), changed, True)
                floor = [argv for _, argv, skip in steps if argv and not skip
                         and argv == ["scripts/run-result-tests"]]
                self.assertEqual(len(floor), 1)

    def test_active_script_contracts_are_in_local_plan(self):
        steps = pre_push.plan(str(ROOT), {".github/workflows/ci.yml"}, False)
        commands = [argv for _, argv, skip in steps if argv and not skip]
        self.assertIn(["python3", "scripts/test-review-recipe"], commands)
        self.assertIn(["python3", "-m", "unittest", "scripts/test_reduced_gate_release.py"], commands)
        self.assertIn(["python3", "-m", "unittest", "discover", "-s", "scripts",
                       "-p", "test_check_doc_links.py"], commands)

    def test_new_contract_discovery_is_present(self):
        # Local and CI commands must remain aligned, not merely scan the
        # retired tests/scripts directory and claim contracts are retired.
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / ".github/workflows/ci.yml"
            path.parent.mkdir(parents=True)
            path.write_text("jobs:\n  fmt:\n    steps:\n"
                            "      - run: python3 scripts/test-review-recipe\n"
                            "      - run: python3 -m unittest scripts/test_reduced_gate_release.py\n"
                            "  clippy:\n    steps: []\n")
            self.assertTrue(pre_push.contract_scripts(root))

    def test_missing_named_contract_is_not_silently_filtered_out(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / ".github/workflows/ci.yml"
            path.parent.mkdir(parents=True)
            path.write_text("jobs:\n  fmt:\n    steps:\n"
                            "      - run: python3 scripts/test-missing\n"
                            "  clippy:\n    steps: []\n")
            steps = pre_push.plan(root, {"scripts/pre-push"}, False)
            self.assertTrue(any(argv == ["python3", "scripts/test-missing"] and not skip
                                for _, argv, skip in steps))

    def test_no_tests_without_explicit_flag(self):
        steps = pre_push.plan(str(ROOT), {"tests/safety_floor.rs"}, False)
        self.assertFalse(any(argv and "safety_floor" in argv and not skip
                             for _, argv, skip in steps))

    def test_doctor_split_check_remains_active(self):
        steps = pre_push.plan(str(ROOT), {"src/doctor/host/mod.rs"}, False)
        self.assertTrue(any(argv and "scripts/split-doctor-host" in argv and not skip
                            for _, argv, skip in steps))


if __name__ == "__main__":
    unittest.main()
