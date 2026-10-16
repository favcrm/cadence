#!/usr/bin/env python3
"""Validate nextest JUnit coverage for scripts/run-result-tests and the CI
shard aggregate.

With --targets-file: every integration target named in FILE (one
identifier per line, exactly the inventory run-result-tests selected),
the lib suite and the bins must each show at least one runnable testcase
across the given junit files — one file for a full run, N shard artifacts
for a partitioned run.

Without --targets-file: partition sanity only — every report parses and
the shard ran at least one runnable testcase.

Both modes fail on any testcase carrying <failure> or <error>: shard
artifacts merge into one verdict, so a red test can never hide inside a
green coverage check.
"""
import argparse
import sys
import xml.etree.ElementTree as ET


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", required=True)
    parser.add_argument("--targets-file")
    parser.add_argument("junit", nargs="+")
    args = parser.parse_args()

    suites = []
    for path in args.junit:
        try:
            suites.extend(ET.parse(path).getroot().findall(".//testsuite"))
        except (OSError, ET.ParseError) as error:
            print(
                f"::error::could not read nextest JUnit report {path}: {error}",
                file=sys.stderr,
            )
            raise SystemExit(1)

    red = [
        (suite.get("name"), case.get("name"))
        for suite in suites
        for case in suite.findall("testcase")
        if case.find("failure") is not None or case.find("error") is not None
    ]
    if red:
        for suite, case in red[:10]:
            print(
                f"::error::failing testcase in merged junit: {suite}::{case}",
                file=sys.stderr,
            )
        raise SystemExit(1)

    def has_runnable(suite):
        return any(
            case.find("skipped") is None for case in suite.findall("testcase")
        )

    if args.targets_file is None:
        if not any(has_runnable(suite) for suite in suites):
            print(
                "::error::partition junit has no runnable tests", file=sys.stderr
            )
            raise SystemExit(1)
        print("partition junit carries runnable tests")
        return

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


if __name__ == "__main__":
    main()
