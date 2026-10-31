#!/usr/bin/env python3
"""Validate nextest JUnit coverage for scripts/run-result-tests and the CI
shard aggregate.

Strict mode (``--inventory``) is the real guard. Each FILE is a nextest
``list --message-format json`` inventory of exactly the selection its
report executed, so one inventory per shard in the aggregate job. Every
(binary id, test name) pair the inventories marked selected must appear
exactly once, green and unskipped, across the reports; every pair they
excluded must appear skipped; nothing else may appear at all. That closes
the four holes a target-level guard cannot see: a case skipped in every
shard, executed twice, missing from one report, or dropped from every
report. With several inventories each pair must be selected in exactly
one of them, so a partition gap and an overlap both fail — but only under
``--complete-partition``, which the aggregate passes. It is the one holder
of every shard's inventory; a single shard legitimately leaves the other
slices to its siblings.

``--ignored-manifest`` pins the ``#[ignore]``d cases to a reviewed list.
Every ignored pair in the inventory must be declared there, and every
declaration whose binary the inventory lists must still be ignored, so
adding or removing an ignore is a diff in that file instead of a silent
way to stop running a test. A binary absent from the inventory — a
narrowed pull-request selection — does not make its declarations stale.

Target mode (``--targets-file``) stays as the independent, cheaper guard
on the target selection itself: every integration target named in FILE,
the lib suite and the bins must each show at least one runnable testcase.

Sanity mode (neither flag) only requires that each report parses and one
carries a runnable testcase.

Every mode fails on a testcase carrying <failure>, <error>, or a
rerun/flaky status: shard artifacts merge into one verdict, so a red or
retried test can never hide inside a green coverage check.
"""
import argparse
import json
import sys
import xml.etree.ElementTree as ET

CASE_CHILDREN = ("skipped", "system-out", "system-err", "properties")
SUITE_CHILDREN = ("testcase", "properties", "system-out", "system-err")
EXCLUSION_REASONS = ("ignored", "partition")


def refuse(message):
    print(f"::error::{message}", file=sys.stderr)
    raise SystemExit(1)


def load_inventory(path):
    """Return {(binary id, test name): {"ignored", "selected"}} for one list."""
    try:
        with open(path, "rb") as handle:
            data = json.load(handle)
    except (OSError, ValueError) as error:
        refuse(f"could not read nextest inventory {path}: {error}")
    if not isinstance(data, dict):
        refuse(f"nextest inventory {path} is not a JSON object")
    suites = data.get("rust-suites")
    if not isinstance(suites, dict) or not suites:
        refuse(f"nextest inventory {path} lists no rust-suites")
    cases = {}
    for binary, suite in suites.items():
        if not isinstance(suite, dict) or suite.get("binary-id") != binary:
            refuse(f"inventory {path} does not list {binary!r} under its own binary id")
        if suite.get("status") != "listed":
            refuse(f"inventory {path} suite {binary!r} is not listed")
        testcases = suite.get("testcases")
        if not isinstance(testcases, dict):
            refuse(f"inventory {path} suite {binary!r} has no testcases map")
        # An empty suite is legal — a binary may declare no test — and the
        # target-level guard below is what refuses an empty lib or bins
        # selection. Strict mode only judges what the suite does list.
        for name, case in testcases.items():
            if not isinstance(name, str) or not name:
                refuse(f"inventory {path} suite {binary!r} has a nameless testcase")
            if not isinstance(case, dict):
                refuse(f"inventory {path} case {binary}::{name} is not an object")
            ignored = case.get("ignored")
            if type(ignored) is not bool:
                refuse(f"inventory {path} case {binary}::{name} has no ignored flag")
            match = case.get("filter-match")
            if not isinstance(match, dict):
                refuse(f"inventory {path} case {binary}::{name} has no filter-match")
            selected = match.get("status") == "matches"
            if not selected:
                # An exclusion must be explained; an unexplained mismatch is a
                # filterset we never asked for and would silently drop a test.
                if match.get("status") != "mismatch":
                    refuse(f"inventory {path} case {binary}::{name} has no match status")
                if match.get("reason") not in EXCLUSION_REASONS:
                    refuse(
                        f"inventory {path} case {binary}::{name} is excluded "
                        f"for {match.get('reason')!r}, not ignored or partitioned"
                    )
                if match.get("reason") == "ignored" and not ignored:
                    refuse(f"inventory {path} case {binary}::{name} claims to be ignored")
            if selected and ignored:
                refuse(f"inventory {path} selected the ignored case {binary}::{name}")
            cases[(binary, name)] = {"ignored": ignored, "selected": selected}
    count = data.get("test-count")
    if type(count) is not int or count != len(cases):
        refuse(
            f"inventory {path} test-count {count!r} disagrees with "
            f"{len(cases)} listed testcases"
        )
    return cases


def load_manifest(path):
    try:
        with open(path, encoding="utf-8") as handle:
            lines = handle.read().splitlines()
    except OSError as error:
        refuse(f"could not read ignored-test manifest {path}: {error}")
    declared = set()
    for number, line in enumerate(lines, 1):
        # A test name is Rust path segments, so `#` can only start a comment.
        entry = line.split("#", 1)[0].strip()
        if not entry:
            continue
        binary, _, name = entry.partition(" ")
        name = name.strip()
        if not binary or not name or " " in name:
            refuse(f"{path}:{number} is not '<binary id> <test name>': {entry!r}")
        if (binary, name) in declared:
            refuse(f"{path}:{number} repeats {binary}::{name}")
        declared.add((binary, name))
    if not declared:
        refuse(f"ignored-test manifest {path} declares no test")
    return declared


def load_reports(paths):
    """Return (executed, skipped) maps of (binary id, test name) -> reports.

    nextest reports every discovered case in every shard, so the same
    identity legitimately appears once per report: executed in the shard
    that owns it and skipped in the rest. A case therefore may be skipped
    in many reports but executed at most once, and never twice inside one.
    Coverage is judged against the inventory, not against agreement between
    reports, so a report that is short still cannot hide a hole.
    """
    executed, skipped = {}, {}
    for path in paths:
        try:
            root = ET.parse(path).getroot()
        except (OSError, ET.ParseError) as error:
            refuse(f"could not read nextest JUnit report {path}: {error}")
        if root.tag not in ("testsuite", "testsuites"):
            refuse(f"{path} is not a JUnit report")
        try:
            scan_suite(root, path, executed, skipped, set())
        except ET.ParseError as error:
            refuse(f"could not read nextest JUnit report {path}: {error}")
    return executed, skipped


def scan_suite(node, path, executed, skipped, seen):
    for child in node:
        if child.tag in ("testsuite", "testsuites"):
            scan_suite(child, path, executed, skipped, seen)
            continue
        if child.tag not in SUITE_CHILDREN:
            refuse(f"{path} has an unknown <{child.tag}> element under a suite")
        if child.tag != "testcase":
            continue
        binary = node.get("name")
        name = child.get("name")
        if not binary or not name:
            refuse(f"{path} has a testcase without a binary or a name")
        classname = child.get("classname")
        if classname is not None and classname != binary:
            refuse(f"{path} testcase {binary}::{name} names classname {classname!r}")
        identity = (binary, name)
        if identity in seen:
            refuse(f"{path} repeats {binary}::{name}; one report carries it once")
        seen.add(identity)
        for element in child:
            if element.tag not in CASE_CHILDREN:
                refuse(
                    f"{path} testcase {binary}::{name} carries a red or retried "
                    f"<{element.tag}> status"
                )
        marks = child.findall("skipped")
        if len(marks) > 1:
            refuse(f"{path} testcase {binary}::{name} carries two <skipped> marks")
        if marks:
            skipped.setdefault(identity, set()).add(path)
            continue
        if identity in executed:
            refuse(
                f"{binary}::{name} executed by both {executed[identity]} and "
                f"{path}; a selected test runs exactly once"
            )
        executed[identity] = path
    require_counts(node, path)


def require_counts(node, path):
    if node.tag == "testsuites":
        # nextest's root restates the totals for every nested suite.
        cases = list(node.iter("testcase"))
    else:
        cases = [child for child in node if child.tag == "testcase"]
    skipped = sum(1 for case in cases if case.find("skipped") is not None)
    actual = {
        "tests": len(cases),
        "failures": 0,
        "errors": 0,
        "skipped": skipped,
        "disabled": 0,
    }
    for attribute, expected in actual.items():
        supplied = node.get(attribute)
        if supplied is None:
            continue
        if not supplied.isdigit() or int(supplied) != expected:
            refuse(
                f"{path} suite {node.get('name')!r} reports {attribute}={supplied} "
                f"but carries {expected}"
            )


def check_strict(inventories, executed, skipped, complete):
    merged = {}
    for path in inventories:
        for identity, case in load_inventory(path).items():
            known = merged.get(identity)
            if known is None:
                merged[identity] = {"ignored": case["ignored"], "selected": 0}
                known = merged[identity]
            elif known["ignored"] != case["ignored"]:
                refuse(
                    f"inventories disagree on whether {identity[0]}::{identity[1]} "
                    "is ignored"
                )
            if case["selected"]:
                known["selected"] += 1
    selected = {key for key, case in merged.items() if case["selected"]}
    ignored = {key for key, case in merged.items() if case["ignored"]}
    if complete:
        # Only the aggregate holds every shard's inventory, so only it can
        # ask whether the partitions tile the discovered set. A single shard
        # legitimately leaves most tests to its siblings.
        stranded = sorted(
            key for key, case in merged.items()
            if not case["ignored"] and not case["selected"]
        )
        if stranded:
            binary, name = stranded[0]
            refuse(
                f"{binary}::{name} is excluded by every inventory's partition and "
                "runs nowhere"
            )
        doubled = sorted(
            key for key, case in merged.items() if case["selected"] > 1
        )
        if doubled:
            binary, name = doubled[0]
            refuse(
                f"{binary}::{name} is selected by "
                f"{merged[(binary, name)]['selected']} shard inventories; "
                "one test has one owner"
            )
    missing = selected - executed.keys()
    extra = executed.keys() - selected
    if missing:
        for binary, name in sorted(missing)[:10]:
            refuse(f"selected test {binary}::{name} did not execute")
        refuse(f"{len(missing)} selected tests did not execute")
    if ignored & executed.keys():
        identity = sorted(ignored & executed.keys())[0]
        refuse(f"ignored test {identity[0]}::{identity[1]} executed")
    if extra:
        for binary, name in sorted(extra)[:10]:
            refuse(f"unlisted test {binary}::{name} executed")
        refuse(f"{len(extra)} unlisted tests executed")
    lost = ignored - skipped.keys()
    if lost:
        identity = sorted(lost)[0]
        refuse(f"ignored test {identity[0]}::{identity[1]} is missing from the reports")
    stray = skipped.keys() - merged.keys()
    if stray:
        identity = sorted(stray)[0]
        refuse(f"skipped test {identity[0]}::{identity[1]} is not in the inventory")
    return merged, ignored, selected


def check_manifest(manifest, merged, ignored):
    undeclared = ignored - manifest
    if undeclared:
        for binary, name in sorted(undeclared)[:10]:
            refuse(
                f"ignored test {binary}::{name} is not declared in the manifest"
            )
        refuse(f"{len(undeclared)} ignored tests are undeclared")
    present = {binary for binary, _ in merged}
    stale = [entry for entry in manifest if entry[0] in present and entry not in ignored]
    if stale:
        for binary, name in sorted(stale)[:10]:
            refuse(
                f"manifest declares {binary}::{name} ignored but the inventory "
                "selected it"
            )
        refuse(f"{len(stale)} manifest entries no longer hold")


def has_runnable(suite):
    return any(case.find("skipped") is None for case in suite.findall("testcase"))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", required=True)
    parser.add_argument("--targets-file")
    parser.add_argument("--inventory", action="append", default=[])
    parser.add_argument("--ignored-manifest")
    parser.add_argument("--complete-partition", action="store_true")
    parser.add_argument("junit", nargs="+")
    args = parser.parse_args()

    executed, skipped = load_reports(args.junit)
    if not executed and not skipped:
        refuse("the junit reports carry no testcase")

    if args.inventory:
        if not args.ignored_manifest:
            refuse("--inventory requires --ignored-manifest")
        merged, ignored, selected = check_strict(
            args.inventory, executed, skipped, args.complete_partition
        )
        check_manifest(
            load_manifest(args.ignored_manifest), merged, ignored
        )
        print(
            f"strict coverage: {len(selected)} selected tests executed once, "
            f"{len(ignored)} ignored and declared, "
            f"{len(merged)} discovered"
        )

    suites = []
    for path in args.junit:
        suites.extend(ET.parse(path).getroot().iter("testsuite"))
    if args.targets_file is not None:
        with open(args.targets_file) as handle:
            expected = [line.strip() for line in handle if line.strip()]
        failed = False
        for target in expected:
            name = f"{args.package}::{target}"
            matches = [suite for suite in suites if suite.get("name") == name]
            if not matches or not any(has_runnable(suite) for suite in matches):
                print(
                    f"::error::integration target {target} has no runnable tests",
                    file=sys.stderr,
                )
                failed = True
        if not any(
            suite.get("name") == args.package and has_runnable(suite)
            for suite in suites
        ):
            print("::error::lib selection has no runnable tests", file=sys.stderr)
            failed = True
        if not any(
            suite.get("name", "").startswith(f"{args.package}::bin/")
            and has_runnable(suite)
            for suite in suites
        ):
            print("::error::bins selection has no runnable tests", file=sys.stderr)
            failed = True
        if failed:
            raise SystemExit(1)
        print(
            f"validated {len(expected)} integration targets "
            "and runnable lib/bin suites"
        )
        return

    if not args.inventory:
        if not any(has_runnable(suite) for suite in suites):
            print(
                "::error::partition junit has no runnable tests", file=sys.stderr
            )
            raise SystemExit(1)
        print("partition junit carries runnable tests")


if __name__ == "__main__":
    main()
