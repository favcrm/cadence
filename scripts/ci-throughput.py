#!/usr/bin/env python3
"""Read-only CI timing evidence. Compare manual 4/8 runs on the same SHA.

Dispatch-to-start includes dependencies and scheduling, not pure runner queue.
Runner seconds sum executed job wall time, not CPU time or billed minutes.
This report is observational; it does not approve a merge or change any gate.
"""
import argparse
from datetime import datetime
import json
from pathlib import Path
import re
import subprocess

GATES = ("fmt", "clippy", "test", "build", "ui", "test-once")
SHARD = re.compile(r"test-shard \(([1-9][0-9]*)\)")
COMPILE_STEP = "Verify selected inventory parity"
TEST_STEP = "Run recorded Rust scope with pinned nextest"


def timestamp(value):
    if not isinstance(value, str):
        raise ValueError("missing timestamp")
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("timestamp must include timezone")
    return parsed


def interval(start, end):
    seconds = (timestamp(end) - timestamp(start)).total_seconds()
    if seconds < 0:
        raise ValueError("inverted timestamps")
    return seconds


def successful(job):
    return job.get("status") == "completed" and job.get("conclusion") == "success"


def summarize(run, jobs):
    created = run["created_at"]
    elapsed = interval(created, run["updated_at"])
    if not isinstance(jobs, list) or not all(isinstance(j, dict) for j in jobs):
        raise ValueError("jobs must be an array of objects")
    blockers = []
    if not successful(run):
        blockers.append("workflow is not completed successfully")
    if run.get("path") != ".github/workflows/ci.yml":
        blockers.append("not the CI workflow")
    sha = run.get("head_sha", "")
    if not isinstance(sha, str) or not re.fullmatch(r"[0-9a-f]{40}", sha):
        blockers.append("source SHA is not a full commit")
    if type(run.get("run_attempt")) is not int or run["run_attempt"] != 1:
        blockers.append("reruns are not controlled first-attempt baselines")
    if type(run.get("id")) is not int or run["id"] <= 0:
        raise ValueError("run id must be a positive integer")

    by_name = {}
    rows = []
    runner_seconds = 0
    for job in jobs:
        name = job.get("name")
        if not isinstance(name, str):
            raise ValueError("job name must be a string")
        by_name.setdefault(name, []).append(job)
        row = {"name": name, "status": job.get("status"), "conclusion": job.get("conclusion"),
               "dispatch_to_start_seconds": None, "execution_seconds": None, "steps": []}
        # Skipped jobs can have inverted synthetic timestamps. They ran no work.
        if job.get("conclusion") != "skipped":
            try:
                row["dispatch_to_start_seconds"] = interval(created, job.get("started_at"))
                if job.get("status") == "completed":
                    row["execution_seconds"] = interval(job.get("started_at"), job.get("completed_at"))
                    if timestamp(job["completed_at"]) > timestamp(run["updated_at"]):
                        raise ValueError("job ends after run observation")
                    runner_seconds += row["execution_seconds"]
            except (ValueError, TypeError, KeyError) as error:
                blockers.append(f"{name}: invalid job timing ({error})")
                row["execution_seconds"] = None
            for step in job.get("steps", []):
                item = {"name": step.get("name"), "execution_seconds": None}
                if step.get("conclusion") != "skipped" and step.get("status") == "completed":
                    try:
                        item["execution_seconds"] = interval(step.get("started_at"), step.get("completed_at"))
                    except (ValueError, TypeError) as error:
                        blockers.append(f"{name}: invalid step timing ({error})")
                row["steps"].append(item)
        rows.append(row)

    shard_names = [name for name in by_name if name.startswith("test-shard")]
    indices = sorted(int(SHARD.fullmatch(name)[1]) for name in shard_names if SHARD.fullmatch(name))
    width = len(indices)
    if width not in (4, 8) or indices != list(range(1, width + 1)) or len(indices) != len(shard_names):
        blockers.append("shards are not exactly 1..4 or 1..8")
        width = None
    for name in (*GATES, *shard_names):
        entries = by_name.get(name, [])
        if len(entries) != 1 or not successful(entries[0]):
            blockers.append(f"{name}: missing, duplicate or non-successful job")
    for name in shard_names:
        for step_name in (COMPILE_STEP, TEST_STEP):
            steps = [s for j in by_name[name] for s in j.get("steps", []) if s.get("name") == step_name]
            if len(steps) != 1 or not successful(steps[0]):
                blockers.append(f"{name}: missing or non-successful {step_name}")

    gate_end = None
    required = (*GATES, *shard_names)
    if all(len(by_name.get(n, [])) == 1 and successful(by_name[n][0]) for n in required):
        try:
            gate_end = max(interval(created, by_name[n][0].get("completed_at")) for n in required)
        except (ValueError, TypeError):
            pass  # Timing errors are already recorded above.
    return {"schema": 1, "run_id": run["id"], "source_sha": sha, "event": run.get("event"),
            "attempt": run.get("run_attempt"), "shards": width,
            "workflow_elapsed_seconds": elapsed if run.get("status") == "completed" else None,
            "gate_elapsed_seconds": gate_end, "runner_seconds": runner_seconds,
            "comparable": not blockers, "comparison_blockers": blockers, "jobs": rows}


def compare(left, right):
    if not left["comparable"] or not right["comparable"]:
        raise ValueError("comparison requires complete successful first-attempt CI evidence")
    if left["source_sha"] != right["source_sha"]:
        raise ValueError("comparison requires the same exact source SHA")
    if left["event"] != "workflow_dispatch" or right["event"] != "workflow_dispatch":
        raise ValueError("comparison requires two manual full-suite benchmark runs")
    if {left["shards"], right["shards"]} != {4, 8}:
        raise ValueError("comparison requires one 4-shard and one 8-shard run")
    four, eight = (left, right) if left["shards"] == 4 else (right, left)
    return {"source_sha": left["source_sha"], "four_run_id": four["run_id"],
            "eight_run_id": eight["run_id"],
            "runner_seconds_delta": eight["runner_seconds"] - four["runner_seconds"],
            "gate_elapsed_seconds_delta": eight["gate_elapsed_seconds"] - four["gate_elapsed_seconds"],
            "workflow_elapsed_seconds_delta": eight["workflow_elapsed_seconds"] - four["workflow_elapsed_seconds"],
            "note": "8 minus 4; observed difference, not causal proof. Repeat with comparable load/cache/toolchain."}


def minutes(seconds):
    return "unknown" if seconds is None else f"{seconds / 60:.2f}"


def markdown(report):
    lines = [f"# CI timing: run {report['run_id']}", "",
             f"SHA: `{report['source_sha']}`; event: {report['event']}; attempt: {report['attempt']}; shards: {report['shards']}",
             f"Workflow elapsed: {minutes(report['workflow_elapsed_seconds'])} min; gates elapsed: {minutes(report['gate_elapsed_seconds'])} min; summed runner-minutes: {minutes(report['runner_seconds'])}.",
             "", "Job dispatch-to-start includes dependency wait and scheduling; it is not pure runner queue time.",
             "Runner-minutes sum executed job wall time, not CPU time or GitHub billing. Incomplete evidence is partial.",
             "", "| Job | Result | dispatch-to-start min | Execution min | Inventory/build min | Tests min |",
             "|---|---|---:|---:|---:|---:|"]
    for job in report["jobs"]:
        steps = {s["name"]: s["execution_seconds"] for s in job["steps"]}
        # Names come from the workflow; prevent table delimiters/newlines corrupting a report.
        name = job["name"].replace("|", "\\|").replace("\n", " ").replace("\r", " ")
        lines.append(f"| {name} | {job['conclusion'] or job['status']} | {minutes(job['dispatch_to_start_seconds'])} | {minutes(job['execution_seconds'])} | {minutes(steps.get(COMPILE_STEP))} | {minutes(steps.get(TEST_STEP))} |")
    if report["comparison_blockers"]:
        lines += ["", "Not a clean comparison baseline:", *[f"- {b}" for b in report["comparison_blockers"]]]
    return "\n".join(lines) + "\n"


def fetch(repo, run_id):
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo) or run_id <= 0:
        raise ValueError("provide owner/repo and a positive run id")
    endpoint = f"repos/{repo}/actions/runs/{run_id}"
    def api(path, *flags):
        out = subprocess.run(["gh", "api", path, *flags], check=True, capture_output=True, text=True, timeout=120)
        return json.loads(out.stdout)
    run = api(endpoint)
    if run["id"] != run_id:
        raise ValueError("API returned a different run")
    # Never mingle jobs from different attempts; reruns are not clean baselines.
    pages = api(f"{endpoint}/attempts/{run['run_attempt']}/jobs?per_page=100", "--paginate", "--slurp")
    return summarize(run, [j for page in pages for j in page["jobs"]])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default="favcrm/cadence")
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--run-id", type=int)
    source.add_argument("--run", type=Path, help="offline workflow run JSON")
    parser.add_argument("--jobs", type=Path, help="offline {jobs: [...]} JSON")
    parser.add_argument("--compare-run-id", type=int)
    parser.add_argument("--compare-run", type=Path)
    parser.add_argument("--compare-jobs", type=Path)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    try:
        if args.run_id is not None:
            if args.jobs or args.compare_run or args.compare_jobs:
                raise ValueError("online and offline inputs cannot be mixed")
            report = fetch(args.repo, args.run_id)
            other = fetch(args.repo, args.compare_run_id) if args.compare_run_id is not None else None
        else:
            if args.jobs is None or args.compare_run_id is not None or bool(args.compare_run) != bool(args.compare_jobs):
                raise ValueError("offline mode requires --jobs and a paired --compare-run/--compare-jobs")
            def offline(run, jobs):
                return summarize(json.loads(run.read_text()), json.loads(jobs.read_text())["jobs"])
            report = offline(args.run, args.jobs)
            other = offline(args.compare_run, args.compare_jobs) if args.compare_run else None
        comparison = compare(report, other) if other else None
        result = {"runs": [report, other], "comparison": comparison} if other else report
        if args.json:
            print(json.dumps(result, indent=2))
        else:
            print(markdown(report), end="")
            if other:
                print(markdown(other), end="")
                print("\nComparison (8 minus 4):")
                print(json.dumps(comparison, indent=2))
    except (ValueError, TypeError, KeyError, OSError, subprocess.SubprocessError) as error:
        parser.error(str(error))


if __name__ == "__main__":
    main()
