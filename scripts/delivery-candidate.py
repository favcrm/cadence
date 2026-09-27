#!/usr/bin/env python3
"""Resolve a main artifact, or prove an exact test killed a mutation.

GitHub calls use argv, bounded waits and a read-only token. Never execute
the artifact before its provenance and digest have been verified.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import xml.etree.ElementTree as ET


def validate_ci(run, repo, run_id):
    expected = {
        "id": run_id, "path": ".github/workflows/ci.yml", "head_branch": "main",
        "event": "push", "status": "completed", "conclusion": "success",
    }
    if any(run.get(k) != v for k, v in expected.items()):
        raise ValueError("candidate must be a successful ci.yml push run on main")
    if any(run.get(k, {}).get("full_name") != repo for k in ("repository", "head_repository")):
        raise ValueError("candidate belongs to another repository or fork")
    if not re.fullmatch(r"[0-9a-f]{40}", run.get("head_sha", "")):
        raise ValueError("candidate has no full source SHA")
    if type(run.get("run_attempt")) is not int or run["run_attempt"] < 1:
        raise ValueError("candidate has no run attempt")


def validate_mutation(code, xml, name):
    # 100 is nextest's TEST_RUN_FAILED; compilation/setup failures have
    # different exit codes. The report must name exactly one executed test.
    try:
        report = ET.fromstring(xml)
    except ET.ParseError as error:
        raise ValueError("malformed mutation JUnit report") from error
    if report.tag not in ("testsuite", "testsuites"):
        raise ValueError("mutation report is not JUnit")
    executed = []
    for case in report.iter("testcase"):
        statuses = [child.tag for child in case
                    if child.tag in ("failure", "error", "skipped")]
        nested_statuses = [child.tag for child in case.iter()
                           if child.tag in ("failure", "error", "skipped")]
        if len(statuses) > 1 or nested_statuses != statuses:
            raise ValueError("mutation testcase has conflicting statuses")
        # nextest report-skipped=all includes filtered cases. Only a pure
        # skip is non-executed; never discard a skipped failure or error.
        if statuses != ["skipped"]:
            executed.append(case)
    if (code != 100 or len(executed) != 1 or executed[0].get("name") != name
            or executed[0].find("failure") is None
            or executed[0].find("error") is not None
            or executed[0].find("skipped") is not None):
        raise ValueError("mutation did not fail exactly the requested test")


def gh(*args):
    return subprocess.check_output(["gh", *map(str, args)], timeout=600)


def prepare(repo, run_id, dest):
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo) or run_id < 1:
        raise ValueError("invalid repository or run id")
    run = json.loads(gh("api", f"repos/{repo}/actions/runs/{run_id}"))
    validate_ci(run, repo, run_id)
    sha = run["head_sha"]
    comparison = json.loads(gh("api", f"repos/{repo}/compare/{sha}...main"))
    if comparison.get("status") not in ("identical", "ahead"):
        raise ValueError("candidate is not on main")
    dest.mkdir(parents=True, exist_ok=False)
    gh("run", "download", run_id, "--repo", repo, "--name", f"cadence-{sha}-x86_64-linux", "--dir", dest)
    manifest = json.loads((dest / "manifest.json").read_text())
    digest = hashlib.sha256((dest / "cadence").read_bytes()).hexdigest()
    if (manifest.get("source_sha") != sha or manifest.get("sha256") != digest
            or manifest.get("run_id") != run_id or manifest.get("run_attempt") != run["run_attempt"]
            or manifest.get("target") != "x86_64-linux" or manifest.get("features") != ["ui"]
            or (dest / "cadence.sha256").read_text().split() != [digest, "cadence"]):
        raise ValueError("candidate artifact does not match its source, run, attempt or digest")
    gh("attestation", "verify", dest / "cadence", "--repo", repo,
       "--signer-workflow", f"{repo}/.github/workflows/ci.yml",
       "--source-ref", "refs/heads/main", "--source-digest", sha, "--deny-self-hosted-runners")
    # No execution is needed here. The isolated journey executes it later.
    (dest / "cadence").chmod(0o755)
    return {"schema": 1, "source_sha": sha, "ci_run_id": run_id,
            "ci_run_attempt": run["run_attempt"], "sha256": digest}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    candidate = commands.add_parser("prepare")
    candidate.add_argument("--repo", required=True)
    candidate.add_argument("--run-id", required=True, type=int)
    candidate.add_argument("--dest", required=True, type=Path)
    mutation = commands.add_parser("mutation")
    mutation.add_argument("--target", required=True)
    mutation.add_argument("--test", required=True)
    args = parser.parse_args()
    if args.command == "prepare":
        receipt = prepare(args.repo, args.run_id, args.dest)
        (args.dest / "candidate.json").write_text(json.dumps(receipt) + "\n")
        if os.environ.get("GITHUB_OUTPUT"):
            with open(os.environ["GITHUB_OUTPUT"], "a") as output:
                for key, value in receipt.items():
                    output.write(f"{key}={value}\n")
        print(json.dumps(receipt))
    else:
        if not re.fullmatch(r"[a-zA-Z0-9_]+", args.target) or not re.fullmatch(r"[a-zA-Z0-9_:]+", args.test):
            raise ValueError("target and exact test name must be identifiers")
        report = Path("target/nextest/cadence/junit.xml")
        report.unlink(missing_ok=True)
        runner = os.environ["CADENCE_NEXTTEST_BIN"]
        profile = Path(__file__).resolve().parents[1] / ".config/nextest.toml"
        # The mutant cannot replace the runner/wrapper/profile to fake a
        # killed test. The trusted installer verifies the runner's bytes.
        env = dict(os.environ, CADENCE_SUITE_LOCK="", CADENCE_REVIEW_SUITE_LOCK_HELD="1",
                   CADENCE_REVIEW_PR="mutation", CADENCE_REVIEW_HEAD="mutation")
        env.pop("NEXTEST_PROFILE", None)
        env.pop("NEXTEST_RETRIES", None)
        result = subprocess.run([
            runner, "nextest", "run", "--config-file", str(profile), "--profile", "cadence",
            "--retries", "0", "--no-tests", "fail", "--locked", "--features", "test-seam",
            "--test", args.target, "--", args.test, "--exact",
        ], timeout=1200, env=env)
        validate_mutation(result.returncode, report.read_text(), args.test)
        print(f"Mutation killed by exact test {args.target}::{args.test}")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError, ET.ParseError) as error:
        print(f"delivery-candidate: {error}", file=sys.stderr)
        sys.exit(1)
