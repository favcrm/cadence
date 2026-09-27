#!/usr/bin/env python3
"""One ordinary hosted test result; assertion failure remains a failed job."""

import argparse
import json
import os
from pathlib import Path
import re
import selectors
import shutil
import signal
import subprocess
import tempfile
import time
import xml.etree.ElementTree as ET


CASES = {
    "cad663-stale-turn": (
        "backup_rollout", "when_idle_refuses_stale_turns_on_a_stopped_agent"
    ),
    "cad650-master-interrupt": (
        "threads", "cad323_master_interrupts_only_a_turn_it_dispatched"
    ),
}
LOG_LIMIT = 64 * 1024


def validate_inputs(revision, case, control_sha):
    for value in (revision, control_sha):
        if re.fullmatch(r"[0-9a-f]{40}", value) is None:
            raise ValueError("revision and control SHA must be full lowercase commit SHAs")
    if case not in CASES:
        raise ValueError("case is not allowlisted")


def source_sha(root):
    result = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "--verify", "HEAD^{commit}"],
        check=True, capture_output=True, text=True, timeout=10,
    )
    sha = result.stdout.strip()
    if re.fullmatch(r"[0-9a-f]{40}", sha) is None:
        raise ValueError("checkout did not resolve to a full commit SHA")
    return sha


def run_once(argv, cwd, env, log, timeout=1200):
    """Keep a bounded combined log, and stop only the child group we created."""
    tail = bytearray()
    timed_out = False
    process = subprocess.Popen(
        argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True,
    )
    deadline = time.monotonic() + timeout
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ)
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                timed_out = True
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                break
            for key, _ in selector.select(min(remaining, 1)):
                chunk = os.read(key.fd, 8192)
                if not chunk:
                    selector.unregister(key.fileobj)
                else:
                    tail.extend(chunk)
                    del tail[:-LOG_LIMIT]
        try:
            code = process.wait(timeout=max(0.01, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            timed_out = True
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            code = process.wait(timeout=5)
    process.stdout.close()
    log.write_bytes(tail)
    return code, timed_out


def classify(report, code, target, name, started_ns):
    if not report.is_file() or report.is_symlink():
        raise ValueError("JUnit report is missing or is a symlink")
    if report.stat().st_mtime_ns < started_ns:
        raise ValueError("JUnit report is stale")
    root = ET.parse(report).getroot()
    if root.tag not in ("testsuites", "testsuite"):
        raise ValueError("unexpected JUnit root")
    cases = root.findall(".//testcase")
    expected_suite = f"cadence-agent::{target}"
    suites = list(root.iter("testsuite"))
    if any(suite.get("name") != expected_suite for suite in suites):
        raise ValueError("JUnit contains a foreign target suite")
    names = [case.get("name") for case in cases]
    if any(not case_name for case_name in names) or len(names) != len(set(names)):
        raise ValueError("JUnit testcase identities are empty or duplicated")
    skipped = []
    executed = []
    for case in cases:
        if not any(case in list(suite) for suite in suites) or case.get("classname", expected_suite) != expected_suite:
            raise ValueError("JUnit testcase does not belong to the allowlisted target")
        skips = case.findall("skipped")
        if skips:
            if len(skips) != 1 or case.get("name") == name or case.findall("failure"):
                raise ValueError("JUnit requested testcase is skipped or skip entry is malformed")
            skipped.append(case.get("name"))
        else:
            executed.append(case)
    if len(executed) != 1 or executed[0].get("name") != name:
        raise ValueError("JUnit must contain exactly one executed matching testcase")
    case = executed[0]
    if any(root.findall(f".//{tag}") for tag in ("error", "disabled")):
        raise ValueError("JUnit contains an unexecuted testcase or error")
    if len(root.findall(".//skipped")) != len(skipped):
        raise ValueError("JUnit skip entries are malformed")
    for suite in root.iter():
        if suite.tag in ("testsuites", "testsuite"):
            for count, tag in (
                ("tests", "testcase"), ("failures", "failure"),
                ("skipped", "skipped"), ("errors", "error"), ("disabled", "disabled"),
            ):
                value = suite.get(count)
                if value is not None and (
                    re.fullmatch(r"[0-9]+", value) is None
                    or int(value) != len(suite.findall(f".//{tag}"))
                ):
                    raise ValueError(f"JUnit summary {count} contradicts its contents")
    failures = root.findall(".//failure")
    classification = {
        "skipped_cases": skipped,
        "junit_counts": {
            "tests": len(cases), "executed": 1, "failures": len(failures),
            "skipped": len(skipped), "errors": 0,
        },
    }
    if code == 0 and not failures:
        classification["outcome"] = "pass"
    elif code == 100 and len(failures) == 1 and failures[0] in list(case):
        classification["outcome"] = "test_failure"
    else:
        raise ValueError("runner exit and JUnit do not describe an ordinary test result")
    return classification


def execute(args, control, timeout=1200):
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    for name in ("junit.xml", "runner.log"):
        (output / name).unlink(missing_ok=True)
    receipt = {
        "outcome": "invalid_feedback", "reason": "execution not reached",
        "requested_sha": args.revision, "dispatch_control_sha": args.control_sha,
        "control_sha": None, "candidate_sha": None, "returncode": None,
        "argv": [], "case": args.case,
        "log_tail_limit_bytes": LOG_LIMIT,
    }
    report = None
    try:
        validate_inputs(args.revision, args.case, args.control_sha)
        receipt["control_sha"] = source_sha(control)
        if receipt["control_sha"] != args.control_sha:
            raise ValueError("control checkout differs from dispatch SHA")
        if args.command == "validate":
            receipt["reason"] = "inputs validated; test execution not reached"
            return 0
        candidate = Path(args.candidate).resolve()
        receipt["candidate_sha"] = source_sha(candidate)
        if receipt["candidate_sha"] != args.revision:
            raise ValueError("candidate checkout differs from requested SHA")
        target, name = CASES[args.case]
        receipt["argv"] = [
            str(control / "scripts/cadence-nextest"),
            "--manifest-path", str(candidate / "Cargo.toml"),
            "--locked", "--features", "test-seam", "--test", target,
            "--", name, "--exact",
        ]
        candidate_report = candidate / "target/nextest/cadence/junit.xml"
        # Refuse linked report directories rather than following them out of target.
        for parent in (candidate / "target", candidate_report.parent.parent, candidate_report.parent):
            if parent.is_symlink():
                raise ValueError("JUnit directory is a symlink")
        if candidate_report.is_symlink():
            raise ValueError("JUnit report is a symlink")
        report = candidate_report
        report.unlink(missing_ok=True)
        receipt["rustc_version"] = subprocess.run(
            ["rustc", "--version"], check=True, capture_output=True,
            text=True, timeout=10,
        ).stdout.strip()
        with tempfile.TemporaryDirectory(prefix="cfb-", dir="/tmp") as temporary:
            env = os.environ.copy()
            original_home = env.get("HOME", str(Path.home()))
            env.setdefault("CARGO_HOME", str(Path(original_home) / ".cargo"))
            env.setdefault("RUSTUP_HOME", str(Path(original_home) / ".rustup"))
            for key, leaf in (
                ("HOME", "home"), ("XDG_CONFIG_HOME", "config"),
                ("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state"),
                ("XDG_CACHE_HOME", "cache"), ("TMPDIR", "tmp"),
            ):
                child = Path(temporary) / leaf
                child.mkdir()
                env[key] = str(child)
            # Allocate an owned outer lock; the trusted wrapper owns child markers.
            for key in ("CADENCE_REVIEW_SUITE_LOCK_HELD", "CADENCE_REVIEW_PR", "CADENCE_REVIEW_HEAD"):
                env.pop(key, None)
            env["CADENCE_SUITE_LOCK"] = str(Path(temporary) / "suite.lock")
            env["CARGO_TARGET_DIR"] = str(candidate / "target")
            env["CARGO_BUILD_JOBS"] = "4"
            receipt["build_context"] = {
                "cwd": str(control), "cargo_target_dir": env["CARGO_TARGET_DIR"],
                "cargo_build_jobs": env["CARGO_BUILD_JOBS"],
                "rustflags": env.get("RUSTFLAGS"),
                "nextest_config": str(control / ".config/nextest.toml"),
                "nextest_profile": "cadence", "nextest_retries": 0,
            }
            started_ns = time.time_ns()
            code, timed_out = run_once(
                receipt["argv"], control, env, output / "runner.log", timeout,
            )
            receipt["returncode"] = code
            receipt["timed_out"] = timed_out
            if timed_out:
                raise ValueError("runner exceeded the bounded timeout")
            receipt.update(classify(report, code, target, name, started_ns))
            receipt["reason"] = "one matching executed testcase; nonselected skips retained"
    except (ValueError, OSError, ET.ParseError, subprocess.SubprocessError) as error:
        receipt["outcome"] = "invalid_feedback"
        receipt["reason"] = str(error)
    finally:
        try:
            if report is not None and report.is_file() and not report.is_symlink():
                if any(parent.is_symlink() for parent in (report.parent, report.parent.parent, report.parent.parent.parent)):
                    raise ValueError("JUnit directory became a symlink")
                shutil.copyfile(report, output / "junit.xml")
        except (ValueError, OSError) as error:
            receipt["outcome"] = "invalid_feedback"
            receipt["reason"] = f"could not preserve JUnit: {error}"
        (output / "outcome.json").write_text(json.dumps(receipt, indent=2) + "\n")
    return 0 if receipt["outcome"] == "pass" else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("validate", "run"))
    parser.add_argument("--revision", required=True)
    parser.add_argument("--case", required=True)
    parser.add_argument("--control-sha", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--candidate")
    args = parser.parse_args()
    if args.command == "run" and not args.candidate:
        parser.error("run requires --candidate")
    return execute(args, Path(__file__).resolve().parents[1])


if __name__ == "__main__":
    raise SystemExit(main())
