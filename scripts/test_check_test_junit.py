#!/usr/bin/env python3
"""Contract test for scripts/check-test-junit.py.

CAD-1970: the coverage guard must refuse a hole, not merely count targets.
Each case builds a small nextest-shaped inventory and JUnit report set in a
temporary directory and drives the real CLI in a subprocess, so nothing here
imports the implementation or asserts on its source text.

The fixture mirrors what nextest 0.9.150 actually writes: `junit.xml` under
`testsuites` with one `testsuite` per binary id, every discovered case
present in every report, the partition's owner executed and the rest marked
`<skipped>`. Ignored cases are skipped in every report.
"""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

SCRIPTS = Path(__file__).resolve().parent
CHECKER = SCRIPTS / "check-test-junit.py"
PACKAGE = "fixture-agent"
LIB = PACKAGE
SUITE = f"{PACKAGE}::fixture_case"
SHARDS = 2

# (binary, name) -> owner shard (1-based), or "ignored" for a permanent skip.
DEFAULT = {
    (LIB, "adapter::tests::parses"): 1,
    (LIB, "adapter::tests::refuses"): 1,
    (LIB, "wiki::tests::child_entry"): "ignored",
    (SUITE, "cad0001_acceptance::runs"): 2,
    (SUITE, "cad0002_acceptance::refuses"): 2,
}

MANIFEST = f"{LIB} wiki::tests::child_entry\n"


def inventory_doc(entries):
    """nextest `list --message-format json` for one shard."""
    suites = {}
    for (binary, name), state in entries.items():
        if state == "selected":
            match = {"status": "matches"}
        elif state == "ignored":
            match = {"status": "mismatch", "reason": "ignored"}
        else:
            match = {"status": "mismatch", "reason": "partition"}
        suite = suites.setdefault(
            binary,
            {"binary-id": binary, "status": "listed", "testcases": {}},
        )
        suite["testcases"][name] = {
            "ignored": state == "ignored",
            "filter-match": match,
        }
    return {"rust-suites": suites, "test-count": len(entries)}


def junit_doc(entries):
    """nextest `junit.xml` with `report-skipped = "all"`."""
    root = ET.Element("testsuites", name="nextest-run")
    by_binary = {}
    for (binary, name), skipped in entries.items():
        by_binary.setdefault(binary, []).append((name, skipped))
    for binary, cases in by_binary.items():
        suite = ET.SubElement(root, "testsuite", name=binary)
        for name, skipped in cases:
            case = ET.SubElement(
                suite, "testcase", name=name, classname=binary, time="0.001"
            )
            if skipped:
                ET.SubElement(
                    case,
                    "skipped",
                    message="Skipped: test is in a different partition",
                )
    recount(root)
    return root


def recount(root):
    """Refresh the count attributes nextest writes, after a mutation."""
    for suite in root.iter("testsuite"):
        cases = suite.findall("testcase")
        suite.set("tests", str(len(cases)))
        suite.set("failures", str(sum(len(c.findall("failure")) for c in cases)))
        suite.set("errors", str(sum(len(c.findall("error")) for c in cases)))
        suite.set("skipped", str(sum(1 for c in cases if c.find("skipped") is not None)))
    cases = list(root.iter("testcase"))
    root.set("tests", str(len(cases)))
    root.set("failures", str(sum(len(c.findall("failure")) for c in cases)))
    root.set("errors", str(sum(len(c.findall("error")) for c in cases)))
    root.set("skipped", str(sum(1 for c in cases if c.find("skipped") is not None)))


def shard_view(entries, shard):
    """What shard `shard` of SHARDS sees: its owners execute, the rest skip."""
    return {
        identity: owner == "ignored" or owner != shard
        for identity, owner in entries.items()
    }


class Scenario:
    def __init__(self, root, entries):
        self.root = root
        self.entries = dict(entries)
        self.manifest = root / "ignored.txt"
        self.manifest.write_text(MANIFEST)
        self.inventories = []
        self.reports = []
        for shard in range(1, SHARDS + 1):
            inventory = root / f"inventory-{shard}.json"
            inventory.write_text(json.dumps(inventory_doc(self.states(shard))))
            self.inventories.append(inventory)
            report = root / f"junit-{shard}.xml"
            ET.ElementTree(junit_doc(shard_view(self.entries, shard))).write(report)
            self.reports.append(report)

    def states(self, shard):
        return {
            identity: (
                "ignored" if owner == "ignored"
                else "selected" if owner == shard
                else "partition"
            )
            for identity, owner in self.entries.items()
        }

    def mutate(self, shard, change):
        """Apply `change(root)` to one report and refresh its counts."""
        root = ET.parse(self.reports[shard - 1]).getroot()
        change(root)
        recount(root)
        ET.ElementTree(root).write(self.reports[shard - 1])

    def write_inventory(self, shard, states):
        self.inventories[shard - 1].write_text(json.dumps(inventory_doc(states)))

    def run(self, complete=True, shards=None):
        shards = shards or list(range(1, SHARDS + 1))
        argv = [sys.executable, str(CHECKER), "--package", PACKAGE]
        if complete:
            argv.append("--complete-partition")
        for shard in shards:
            argv += ["--inventory", str(self.inventories[shard - 1])]
        argv += ["--ignored-manifest", str(self.manifest)]
        argv += [str(self.reports[shard - 1]) for shard in shards]
        return subprocess.run(argv, capture_output=True, text=True)


def cases(root, name):
    for suite in root.iter("testsuite"):
        for case in suite.findall("testcase"):
            if case.get("name") == name:
                yield suite, case


def drop(root, name):
    for suite, case in list(cases(root, name)):
        suite.remove(case)


def unskip(root, name):
    for _, case in cases(root, name):
        for mark in case.findall("skipped"):
            case.remove(mark)


def skip(root, name):
    for _, case in cases(root, name):
        if case.find("skipped") is None:
            ET.SubElement(case, "skipped", message="Skipped")


class CoverageGuard(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.scenario = Scenario(Path(self.tmp.name), DEFAULT)

    def assertRefused(self, result, needle):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(needle, result.stderr)

    # -- the cases the target-level guard could not see --------------------

    def test_complete_partitioned_run_passes(self):
        result = self.scenario.run()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("4 selected tests executed once", result.stdout)
        self.assertIn("1 ignored and declared", result.stdout)

    def test_case_dropped_from_every_report_is_refused(self):
        """The old hole: agreement among reports is not proof of coverage."""
        for shard in (1, 2):
            self.scenario.mutate(shard, lambda root: drop(root, "cad0001_acceptance::runs"))
        self.assertRefused(self.scenario.run(), "did not execute")

    def test_skip_elsewhere_cannot_stand_in_for_the_owner_execution(self):
        self.scenario.mutate(1, lambda root: drop(root, "adapter::tests::parses"))
        self.assertRefused(self.scenario.run(), "did not execute")

    def test_duplicate_execution_is_refused(self):
        self.scenario.mutate(2, lambda root: unskip(root, "adapter::tests::parses"))
        self.assertRefused(self.scenario.run(), "executed by both")

    def test_repeated_case_inside_one_report_is_refused(self):
        def duplicate(root):
            suite = next(iter(root.iter("testsuite")))
            suite.append(ET.fromstring(ET.tostring(suite.find("testcase"))))

        self.scenario.mutate(1, duplicate)
        self.assertRefused(self.scenario.run(), "repeats")

    def test_selected_case_skipped_in_its_own_shard_is_refused(self):
        self.scenario.mutate(1, lambda root: skip(root, "adapter::tests::parses"))
        self.assertRefused(self.scenario.run(), "did not execute")

    def test_partition_gap_is_refused(self):
        for shard in (1, 2):
            states = self.scenario.states(shard)
            states[(SUITE, "cad0001_acceptance::runs")] = "partition"
            self.scenario.write_inventory(shard, states)
        self.assertRefused(self.scenario.run(), "runs nowhere")

    def test_single_shard_is_not_failed_for_its_siblings_tests(self):
        """A shard owns a slice; the other slice is another shard's work."""
        result = self.scenario.run(complete=False, shards=[1])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("2 selected tests executed once", result.stdout)

    def test_partition_overlap_is_refused(self):
        states = self.scenario.states(1)
        for identity, state in states.items():
            if state == "partition":
                states[identity] = "selected"
        self.scenario.write_inventory(1, states)
        self.assertRefused(self.scenario.run(), "one owner")

    # -- red, retried and malformed evidence ------------------------------

    def test_failure_is_refused(self):
        def boom(root):
            for _, case in cases(root, "adapter::tests::parses"):
                ET.SubElement(case, "failure", message="boom")

        self.scenario.mutate(1, boom)
        self.assertRefused(self.scenario.run(), "red or retried")

    def test_rerun_status_is_refused(self):
        def flaky(root):
            for _, case in cases(root, "cad0002_acceptance::refuses"):
                ET.SubElement(case, "rerunFailure", message="flaky")

        self.scenario.mutate(2, flaky)
        self.assertRefused(self.scenario.run(), "red or retried")

    def test_contrary_counts_are_refused(self):
        def lie(root):
            next(iter(root.iter("testsuite"))).set("tests", "99")

        root = ET.parse(self.scenario.reports[0]).getroot()
        lie(root)
        ET.ElementTree(root).write(self.scenario.reports[0])
        self.assertRefused(self.scenario.run(), "but carries")

    def test_malformed_report_is_refused(self):
        self.scenario.reports[0].write_text("<testsuite")
        self.assertRefused(self.scenario.run(), "could not read nextest JUnit report")

    def test_report_with_no_cases_is_refused(self):
        for shard in (1, 2):
            self.scenario.reports[shard - 1].write_text(
                "<testsuites name='nextest-run'/>"
            )
        self.assertRefused(self.scenario.run(), "carry no testcase")

    # -- the ignored inventory is declared, never assumed -----------------

    def test_manifest_without_entries_is_refused(self):
        self.scenario.manifest.write_text("# nothing declared\n")
        self.assertRefused(self.scenario.run(), "declares no test")

    def test_ignored_case_missing_from_manifest_is_refused(self):
        self.scenario.manifest.write_text(f"{SUITE} cad0002_acceptance::refuses\n")
        self.assertRefused(self.scenario.run(), "not declared in the manifest")

    def test_manifest_entry_that_now_runs_is_refused(self):
        self.scenario.manifest.write_text(
            MANIFEST + f"{SUITE} cad0002_acceptance::refuses\n"
        )
        self.assertRefused(self.scenario.run(), "selected it")

    def test_ignored_case_that_executes_is_refused(self):
        self.scenario.mutate(1, lambda root: unskip(root, "wiki::tests::child_entry"))
        self.assertRefused(self.scenario.run(), "ignored test")

    def test_ignored_case_absent_from_every_report_is_refused(self):
        for shard in (1, 2):
            self.scenario.mutate(shard, lambda root: drop(root, "wiki::tests::child_entry"))
        self.assertRefused(self.scenario.run(), "missing from the reports")

    # -- inventory shape ---------------------------------------------------

    def test_inventory_without_manifest_is_refused(self):
        argv = [sys.executable, str(CHECKER), "--package", PACKAGE]
        for inventory in self.scenario.inventories:
            argv += ["--inventory", str(inventory)]
        argv += [str(report) for report in self.scenario.reports]
        result = subprocess.run(argv, capture_output=True, text=True)
        self.assertRefused(result, "requires --ignored-manifest")

    def test_unexplained_exclusion_is_refused(self):
        document = json.loads(self.scenario.inventories[0].read_text())
        document["rust-suites"][LIB]["testcases"]["adapter::tests::refuses"][
            "filter-match"
        ] = {"status": "mismatch", "reason": "filtered"}
        self.scenario.inventories[0].write_text(json.dumps(document))
        self.assertRefused(self.scenario.run(), "not ignored or partitioned")

    def test_contradictory_test_count_is_refused(self):
        document = json.loads(self.scenario.inventories[0].read_text())
        document["test-count"] = 1
        self.scenario.inventories[0].write_text(json.dumps(document))
        self.assertRefused(self.scenario.run(), "test-count")

    def test_malformed_inventory_is_refused(self):
        self.scenario.inventories[0].write_text("[]")
        self.assertRefused(self.scenario.run(), "is not a JSON object")

    def test_empty_inventory_is_refused(self):
        document = {"rust-suites": {}, "test-count": 0}
        self.scenario.inventories[0].write_text(json.dumps(document))
        self.assertRefused(self.scenario.run(), "lists no rust-suites")


if __name__ == "__main__":
    unittest.main()
