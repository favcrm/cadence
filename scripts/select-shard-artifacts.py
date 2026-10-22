#!/usr/bin/env python3
"""Select the artifact of each partition's authoritative shard execution.

A failed-job-only rerun keeps green jobs as new rows carrying the rerun's
`run_attempt` but their original execution timestamps and runner; those
carried rows never uploaded again. Resolve each carried row to the unique
earlier execution it aliases, take every partition's latest *actual*
execution, and print the artifacts uploaded by exactly those attempts.

Read-only: `gh api` lists this run's jobs and artifacts. Every completed
row must carry created_at, started_at, completed_at and runner_id, and a
real execution must order them created <= started <= completed. Any doubt
below — missing or ambiguous provenance, a latest execution that did not
succeed, an expired artifact or one from another run or head — exits
nonzero without printing `artifact_ids`, so the caller never falls back to
older evidence.
"""
import argparse
from datetime import datetime
import json
import re
import subprocess
import sys

SHARDS = range(1, 7)
JOB_NAME = re.compile(r"test-shards \(([1-6])\)")
FULL_SHA = re.compile(r"[0-9a-f]{40}")
REPOSITORY = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+")


def positive(value, label):
    if type(value) is not int or value < 1:
        raise ValueError(f"invalid {label}")
    return value


def timestamp(value, label):
    if not isinstance(value, str):
        raise ValueError(f"missing {label}")
    try:
        result = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        raise ValueError(f"invalid {label}") from None
    if result.tzinfo is None:
        raise ValueError(f"{label} has no timezone")
    return result


def is_carried(job):
    """Whether this row reports an execution that happened before it existed.

    GitHub's carried rows keep the original execution's timestamps but take
    the rerun's `created_at`; a real execution is always created before it
    starts. Rows without that evidence are treated as executions and still
    have to bind their own attempt's artifact.
    """
    if job.get("status") != "completed" or job.get("created_at") is None:
        return False
    created = timestamp(job["created_at"], "job creation")
    for field in ("started_at", "completed_at"):
        value = job.get(field)
        if value is not None and created > timestamp(value, field):
            return True
    return False


def execution(job):
    """The identity of the execution a row reports, independent of the row."""
    runner = job.get("runner_id")
    if type(runner) is not int or runner < 0:
        raise ValueError("shard execution has no runner identity")
    return (
        timestamp(job.get("started_at"), "job start"),
        timestamp(job.get("completed_at"), "job completion"),
        runner,
        job.get("status"),
        job.get("conclusion"),
    )


def execution_provenance(job, shard):
    """Parse a completed row's native execution identity.

    A completed row without all of created_at, started_at, completed_at and
    runner_id is ambiguous: it cannot be told apart from a carried row, so
    it is refused instead of trusted as an actual execution.
    """
    created = timestamp(job.get("created_at"), "job creation")
    started = timestamp(job.get("started_at"), "job start")
    completed = timestamp(job.get("completed_at"), "job completion")
    runner = job.get("runner_id")
    if type(runner) is not int or runner < 0:
        raise ValueError(f"shard {shard} completed job has no runner identity")
    return created, started, completed


def select(jobs, artifacts, run_id, sha, attempt):
    grouped = {shard: [] for shard in SHARDS}
    seen = set()
    for job in jobs:
        if not isinstance(job, dict) or not isinstance(job.get("name"), str):
            raise ValueError("malformed job metadata")
        name = job["name"]
        match = JOB_NAME.fullmatch(name)
        if match is None:
            if name.startswith("test-shards"):
                raise ValueError(f"unrecognized shard job {name!r}")
            continue
        shard = int(match[1])
        job_id = positive(job.get("id"), "shard job id")
        if job_id in seen:
            raise ValueError(f"shard {shard} repeats job id {job_id}")
        seen.add(job_id)
        job_attempt = positive(job.get("run_attempt"), "shard job attempt")
        if (type(job.get("run_id")) is not int
                or job["run_id"] != run_id
                or job.get("head_sha") != sha):
            raise ValueError(f"shard {shard} job {job_id} is not this run and head")
        if job_attempt > attempt:
            raise ValueError(f"shard {shard} job {job_id} is from a later attempt")
        grouped[shard].append(job)

    selected = []
    for shard, rows in grouped.items():
        actual, carried = [], []
        for job in rows:
            if job.get("status") != "completed":
                actual.append(job)
                continue
            created, started, completed = execution_provenance(job, shard)
            if is_carried(job):
                carried.append(job)
            elif created <= started <= completed:
                actual.append(job)
            else:
                raise ValueError(
                    f"shard {shard} execution timestamps are out of order"
                )
        for alias in carried:
            origins = [
                job for job in actual
                if job.get("status") == "completed"
                and job["run_attempt"] < alias["run_attempt"]
                and execution(job) == execution(alias)
            ]
            if len(origins) != 1:
                raise ValueError(
                    f"shard {shard} carried row has missing or ambiguous execution"
                )
        if not actual:
            raise ValueError(f"shard {shard} has no execution")
        latest = max(job["run_attempt"] for job in actual)
        current = [job for job in actual if job["run_attempt"] == latest]
        if len(current) != 1:
            raise ValueError(f"shard {shard} has ambiguous attempt {latest} execution")
        job = current[0]
        if job.get("status") != "completed" or job.get("conclusion") != "success":
            raise ValueError(
                f"shard {shard} attempt {latest} did not succeed "
                f"({job.get('status')}/{job.get('conclusion')})"
            )

        name = f"test-shard-{shard}-attempt-{latest}"
        candidates = [
            artifact for artifact in artifacts
            if isinstance(artifact, dict) and artifact.get("name") == name
        ]
        if len(candidates) != 1:
            raise ValueError(f"shard {shard} has missing or ambiguous artifact {name}")
        artifact = candidates[0]
        provenance = artifact.get("workflow_run")
        if (
            artifact.get("expired") is not False
            or not isinstance(provenance, dict)
            or type(provenance.get("id")) is not int
            or provenance["id"] != run_id
            or provenance.get("head_sha") != sha
        ):
            raise ValueError(
                f"shard {shard} artifact {name} is expired or from another run or head"
            )
        selected.append(positive(artifact.get("id"), f"shard {shard} artifact id"))

    if len(set(selected)) != len(SHARDS):
        raise ValueError("one artifact is selected for multiple shards")
    return selected


def gh_pages(endpoint, key):
    try:
        result = subprocess.run(
            ["gh", "api", "--paginate", "--slurp", endpoint],
            check=True, capture_output=True, text=True, timeout=120,
        )
    except subprocess.CalledProcessError as error:
        detail = (error.stderr or "").strip().splitlines()
        raise ValueError(
            "GitHub API listing failed: " + (detail[0] if detail else str(error.returncode))
        ) from None
    pages = json.loads(result.stdout)
    if not isinstance(pages, list):
        raise ValueError("malformed paginated API response")
    rows = []
    for page in pages:
        if not isinstance(page, dict) or not isinstance(page.get(key), list):
            raise ValueError("malformed API page")
        rows.extend(page[key])
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--run-id", required=True, type=int)
    parser.add_argument("--sha", required=True)
    parser.add_argument("--attempt", required=True, type=int)
    args = parser.parse_args()
    if not REPOSITORY.fullmatch(args.repo):
        raise ValueError("invalid repository")
    run_id = positive(args.run_id, "workflow run id")
    attempt = positive(args.attempt, "workflow attempt")
    if not FULL_SHA.fullmatch(args.sha):
        raise ValueError("invalid workflow head sha")
    endpoint = f"repos/{args.repo}/actions/runs/{run_id}"
    jobs = gh_pages(f"{endpoint}/jobs?filter=all&per_page=100", "jobs")
    artifacts = gh_pages(f"{endpoint}/artifacts?per_page=100", "artifacts")
    ids = select(jobs, artifacts, run_id, args.sha, attempt)
    print("artifact_ids=" + ",".join(str(value) for value in ids))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(f"select-shard-artifacts: {error}", file=sys.stderr)
        raise SystemExit(1)
