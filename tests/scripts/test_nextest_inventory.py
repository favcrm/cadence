"""Exercise the real inventory script with fake build/list commands; no Rust build."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[2] / "scripts" / "nextest-inventory"


class InventoryTimingTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="cad652-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        scripts = self.root / "scripts"
        scripts.mkdir()
        shutil.copyfile(SCRIPT, scripts / "nextest-inventory")
        self.source = self.root / "target/cargo-timings/cargo-timing.html"
        self.source.parent.mkdir(parents=True)
        self.retained = self.root / "retained/inventory.html"
        self.retained.parent.mkdir()
        self.source.write_text("stale source")
        self.retained.write_text("stale retained")
        self.fake("cargo", """import json, os, pathlib, sys
assert not pathlib.Path('cargo-args.json').exists(), 'unexpected additional Cargo invocation'
pathlib.Path('cargo-args.json').write_text(json.dumps(sys.argv[1:]))
if os.environ.get('MAKE_REPORT', 'yes') == 'yes':
    pathlib.Path('target/cargo-timings/cargo-timing.html').write_text('first build')
for name in json.loads(os.environ.get('CARGO_CASES', '["case_one"]')):
    print(name + ': test')
sys.exit(int(os.environ.get('CARGO_EXIT', '0')))
""")
        self.fake("scripts/cadence-nextest", """import json, os, pathlib, sys
pathlib.Path('nextest-args.json').write_text(json.dumps(sys.argv[1:]))
pathlib.Path('target/cargo-timings/cargo-timing.html').write_text('later build')
name = os.environ.get('NEXTEST_CASE', 'case_one')
suites = json.loads(os.environ.get('NEXTEST_SUITES', json.dumps({'suite': [name]})))
print(json.dumps({'rust-suites': {
    suite: {'testcases': {case: {} for case in cases}}
    for suite, cases in suites.items()
}}))
""")

    def fake(self, relative, body):
        path = self.root / relative
        path.write_text("#!/usr/bin/env python3\n" + body)
        path.chmod(0o755)

    def run_inventory(self, selectors=None, **extra):
        env = os.environ.copy()
        env.update(extra)
        env["PATH"] = str(self.root) + os.pathsep + env["PATH"]
        env["CADENCE_INVENTORY_TIMINGS_REPORT"] = str(self.retained)
        env["GITHUB_OUTPUT"] = str(self.root / "outputs")
        return subprocess.run(
            ["sh", "scripts/nextest-inventory", *(selectors or ["all-targets"]), "--features", "test-seam"],
            cwd=self.root, env=env, capture_output=True, text=True,
        )

    def test_first_report_survives_later_build_with_selection_preserved(self):
        result = self.run_inventory()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("equal=yes", result.stdout)
        self.assertEqual(self.retained.read_text(), "first build")
        self.assertEqual((self.root / "outputs").read_text(), "timings_available=false\ntimings_available=true\n")
        self.assertEqual(self.source.read_text(), "later build")
        self.assertEqual(json.loads((self.root / "cargo-args.json").read_text()), [
            "test", "--manifest-path", str(self.root / "Cargo.toml"),
            "--all-targets", "--locked", "--timings", "--features", "test-seam", "--", "--list",
        ])
        self.assertEqual(json.loads((self.root / "nextest-args.json").read_text()), [
            "list", "--all-targets", "--locked", "--features", "test-seam", "--message-format", "json",
        ])

    def test_failed_cargo_report_is_retained_without_running_nextest(self):
        result = self.run_inventory(CARGO_EXIT="101")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cargo inventory for all-targets failed", result.stderr)
        self.assertEqual(self.retained.read_text(), "first build")
        self.assertFalse((self.root / "nextest-args.json").exists())

    def test_missing_failed_report_never_reuses_stale_source_or_destination(self):
        result = self.run_inventory(CARGO_EXIT="101", MAKE_REPORT="no")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("timings unavailable", result.stderr)
        self.assertIn("cargo inventory for all-targets failed", result.stderr)
        self.assertFalse(self.source.exists())
        self.assertFalse(self.retained.exists())

    def test_missing_successful_report_does_not_capture_later_build(self):
        result = self.run_inventory(MAKE_REPORT="no")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.retained.exists())
        self.assertEqual(self.source.read_text(), "later build")
        self.assertEqual((self.root / "outputs").read_text(), "timings_available=false\n")

    def test_partial_preservation_is_unavailable_without_failing_inventory(self):
        self.fake("cp", "import pathlib, sys\npathlib.Path(sys.argv[2]).write_text('partial')\nsys.exit(1)\n")
        result = self.run_inventory()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("cannot retain complete report", result.stderr)
        self.assertFalse(self.retained.exists())
        self.assertFalse(Path(str(self.retained) + ".tmp").exists())
        self.assertEqual((self.root / "outputs").read_text(), "timings_available=false\n")

    def test_capture_setup_failure_excludes_stale_report_without_failing_inventory(self):
        self.fake("mkdir", "import sys\nsys.exit(1)\n")
        result = self.run_inventory()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("cannot prepare capture paths", result.stderr)
        self.assertEqual(self.retained.read_text(), "stale retained")
        self.assertEqual((self.root / "outputs").read_text(), "timings_available=false\n")

    def test_inventory_mismatch_still_fails_with_first_report_retained(self):
        result = self.run_inventory(NEXTEST_CASE="different_case")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("inventories differ", result.stderr)
        self.assertEqual(self.retained.read_text(), "first build")

    def test_selected_scope_reaches_both_backends_without_all_targets(self):
        selectors = ["selected", "--lib", "--bins", "--test", "board_context"]
        result = self.run_inventory(selectors=selectors)
        self.assertEqual(result.returncode, 0, result.stderr)
        cargo = json.loads((self.root / "cargo-args.json").read_text())
        nextest = json.loads((self.root / "nextest-args.json").read_text())
        self.assertEqual(cargo, ["test", "--manifest-path", str(self.root / "Cargo.toml"), "--locked", "--timings", *selectors[1:], "--features", "test-seam", "--", "--list"])
        self.assertEqual(nextest, ["list", "--locked", *selectors[1:], "--features", "test-seam", "--message-format", "json"])
        self.assertNotIn("--all-targets", cargo + nextest)

    def check_case_occurrences(self, cargo_cases, nextest_suites, expected_count=None):
        for selectors in (["all-targets"], ["selected", "--lib", "--bins", "--test", "board_context"]):
            with self.subTest(selectors=selectors):
                # Each scope is a separate invocation; the fake Cargo still
                # refuses an unexpected second build within that invocation.
                for filename in ("cargo-args.json", "nextest-args.json"):
                    (self.root / filename).unlink(missing_ok=True)
                result = self.run_inventory(
                    selectors=selectors,
                    CARGO_CASES=json.dumps(cargo_cases),
                    NEXTEST_SUITES=json.dumps(nextest_suites),
                )
                if expected_count is None:
                    self.assertNotEqual(result.returncode, 0, result.stdout)
                    self.assertIn("inventories differ", result.stderr)
                else:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(result.stdout,
                        f"inventory: target={selectors[0]} tests={expected_count} "
                        "runner=cargo-nextest retries=0 equal=yes\n")
                self.assertEqual(self.retained.read_text(), "first build")
                self.assertEqual(self.source.read_text(), "later build")

    def test_missing_same_named_case_in_another_binary_fails(self):
        self.check_case_occurrences(["shared", "shared"], {"one": ["shared"]})

    def test_extra_same_named_case_in_another_binary_fails(self):
        self.check_case_occurrences(["shared"], {"one": ["shared"], "two": ["shared"]})

    def test_matching_same_named_cases_count_each_occurrence(self):
        self.check_case_occurrences(
            ["shared", "shared"], {"one": ["shared"], "two": ["shared"]}, expected_count=2,
        )

    def test_unordered_cases_across_binaries_compare_deterministically(self):
        self.check_case_occurrences(
            ["zeta", "alpha", "middle"], {"one": ["zeta", "alpha"], "two": ["middle"]},
            expected_count=3,
        )


if __name__ == "__main__":
    unittest.main()
