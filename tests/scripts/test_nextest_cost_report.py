"""Exercise the diagnostic's CLI against representative nextest JUnit reports."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[2] / "scripts" / "nextest-cost-report"


class NextestCostReportTest(unittest.TestCase):
    def run_report(self, xml, *args):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "junit.xml"
            path.write_text(xml)
            return subprocess.run([sys.executable, str(SCRIPT), str(path), *args],
                                  capture_output=True, text=True)

    def test_multi_binary_counts_and_timings_do_not_double_count_suite_totals(self):
        result = self.run_report('''<testsuites tests="99" time="500">
          <testsuite name="daemon" time="400">
            <testcase name="quick" time="1"/>
            <testcase name="failed" time="3"><failure>secret log text</failure></testcase>
            <testcase name="ignored" time="999"><skipped/></testcase>
          </testsuite>
          <testsuite name="board">
            <testcase name="slow" time="10"/>
            <testcase name="unknown"/>
            <testcase name="error" time="2"><error/></testcase>
          </testsuite>
        </testsuites>''', "--json")
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        self.assertEqual((report["testcases"], report["executed"], report["passed"], report["failed"], report["skipped"]), (6, 5, 3, 2, 1))
        self.assertEqual(report["cumulative_test_seconds"], 16)
        self.assertEqual((report["p50_test_seconds"], report["p95_test_seconds"]), (2, 10))
        self.assertEqual(report["missing_executed_timings"], 1)
        self.assertEqual([case["name"] for case in report["slowest"]], ["slow", "failed", "error", "quick"])
        self.assertNotIn("secret log text", result.stdout)

    def test_markdown_limits_rows_and_escapes_names(self):
        result = self.run_report('''<testsuite name="binary|&lt;script&gt;">
          <testcase name="slow`case|line&#10;next" time="5"/>
          <testcase name="quick" time="1"/>
        </testsuite>''', "--top", "1")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("binary&#124;&lt;script&gt;", result.stdout)
        self.assertIn("slow&#96;case&#124;line next", result.stdout)
        self.assertNotIn("| quick |", result.stdout)
        self.assertIn("not wall time", result.stdout)

    def test_nested_suite_names_belong_to_the_correct_tests(self):
        result = self.run_report('''<testsuites><testsuite name="outer">
          <testsuite name="inner"><testcase name="test" time="0"/></testsuite>
        </testsuite></testsuites>''', "--json")
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        self.assertEqual(report["testcases"], 1)
        self.assertEqual(report["slowest"][0]["suite"], "inner")
        self.assertEqual(report["p95_test_seconds"], 0)

    def test_skipped_only_has_no_executed_timing_distribution(self):
        result = self.run_report('<testsuites><testsuite><testcase name="ignored" time="20"><skipped/></testcase></testsuite></testsuites>', "--json")
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        self.assertEqual(report["executed"], 0)
        self.assertEqual(report["passed"], 0)
        self.assertEqual(report["slowest"], [])
        self.assertIsNone(report["p95_test_seconds"])

    def test_malformed_empty_and_invalid_reports_fail_without_a_summary(self):
        for xml in ["", "<testsuites>", "<testsuites/>", "<other/>",
                    '<testsuite><testcase name=""/></testsuite>']:
            with self.subTest(xml=xml):
                result = self.run_report(xml)
                self.assertEqual(result.returncode, 1)
                self.assertEqual(result.stdout, "")
                self.assertIn("nextest-cost-report:", result.stderr)

    def test_invalid_duration_values_are_not_reported_as_timings(self):
        for value in ["NaN", "inf", "-1", "bad"]:
            with self.subTest(value=value):
                result = self.run_report(f'<testsuite><testcase name="test" time="{value}"/></testsuite>', "--json")
                self.assertEqual(result.returncode, 1)
                self.assertEqual(result.stdout, "")

    def test_missing_report_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run([sys.executable, str(SCRIPT), str(Path(directory) / "missing.xml")], capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")

    def test_invalid_top_count_fails(self):
        result = self.run_report('<testsuite><testcase name="test" time="1"/></testsuite>', "--top", "0")
        self.assertEqual(result.returncode, 2)


if __name__ == "__main__":
    unittest.main()
