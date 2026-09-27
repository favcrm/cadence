#!/usr/bin/env python3
"""Cancel only proven obsolete ci merge groups after a queue-ref deletion.

Run from trusted default-branch code. API failures retain the run; this is an
optimization, never a required verification gate. No persistent watcher.
Deletion after merging is common: preserve ancestor/equal main evidence.
Cancellation is an ordinary POST after two full identity/ref/queue/ancestry
proofs. GitHub has no atomic compare-and-cancel; a change after the final read
cannot be ruled out atomically. Real cancellation/savings require an owned
controlled stale queue run after this trusted default-branch workflow lands.
"""
import argparse
import json
import os
from pathlib import Path
import re
import time
from urllib.error import HTTPError
from urllib.parse import quote, urlencode
from urllib.request import Request, build_opener, HTTPRedirectHandler

REPO = "favcrm/cadence"
REPO_ID = 1372936414
WORKFLOW_ID = 359597313
WORKFLOW_PATH = ".github/workflows/ci.yml"
SHA = re.compile(r"[0-9a-f]{40}\Z")
QUEUE_REF = re.compile(r"gh-readonly-queue/main/pr-[1-9][0-9]*-[0-9a-f]{40}\Z")
LIVE = {"queued", "in_progress"}


class Unsafe(Exception):
    """A required proof is unavailable or ambiguous: retain the run."""


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise Unsafe("API redirect refused")


def repository_matches(value):
    return isinstance(value, dict) and value.get("id") == REPO_ID and value.get("full_name") == REPO


class GitHub:
    def __init__(self, token):
        self.token = token
        self.opener = build_opener(NoRedirect())
        self.requests = 0

    def request(self, path, *, body=None, absent_ok=False):
        self.requests += 1
        req = Request("https://api.github.com/" + path,
                      data=json.dumps(body).encode() if body is not None else None,
                      headers={"Authorization": "Bearer " + self.token,
                               "Accept": "application/vnd.github+json",
                               "X-GitHub-Api-Version": "2022-11-28",
                               "Content-Type": "application/json"})
        try:
            with self.opener.open(req, timeout=15) as response:
                data = response.read()
                return json.loads(data) if data else None
        except HTTPError as error:
            # Only an exact-ref 404 is evidence of absence. Every other lookup
            # must succeed, including repository visibility and queue access.
            if error.code == 404 and absent_ok:
                return None
            raise Unsafe(f"API HTTP {error.code}") from error

    def repository(self):
        return self.request(f"repos/{REPO}")

    def branch_sha(self, branch):
        value = self.request(f"repos/{REPO}/git/ref/heads/{quote(branch, safe='/')}", absent_ok=True)
        if value is None:
            if branch == "main":
                raise Unsafe("main ref unavailable")
            # A 404 alone may hide a permission error. Require a successful
            # matching-refs response confirming no ref with this prefix too.
            matches = self.request(f"repos/{REPO}/git/matching-refs/heads/{quote(branch, safe='/')}")
            if matches != []:
                raise Unsafe("ref absence not positively confirmed")
            return None
        if value.get("ref") != "refs/heads/" + branch or value.get("object", {}).get("type") != "commit":
            raise Unsafe("ref identity mismatch")
        sha = value["object"].get("sha", "")
        if not SHA.fullmatch(sha):
            raise Unsafe("invalid ref SHA")
        return sha

    def runs_for_branch(self, branch):
        runs = []
        expected = None
        for page in range(1, 11):
            query = urlencode({"event": "merge_group", "branch": branch, "per_page": 100, "page": page})
            value = self.request(f"repos/{REPO}/actions/workflows/{WORKFLOW_ID}/runs?{query}")
            total = value.get("total_count")
            if type(total) is not int or total < 0 or total > 1000 or (expected is not None and total != expected):
                raise Unsafe("run pagination changed or exceeds API limit")
            expected = total
            batch = value.get("workflow_runs")
            if not isinstance(batch, list) or len(batch) > 100:
                raise Unsafe("invalid run page")
            runs.extend(batch)
            if len(batch) < 100 or len(runs) == expected:
                ids = [r.get("id") for r in runs]
                if len(runs) != expected or len(set(ids)) != len(ids):
                    raise Unsafe("incomplete or duplicate run pagination")
                return runs
        raise Unsafe("run pagination incomplete")

    def queue_heads(self):
        query = '''query($cursor:String) { repository(owner:"favcrm",name:"cadence") {
          databaseId mergeQueue(branch:"main") { entries(first:100,after:$cursor) {
            totalCount nodes { id headCommit { oid } } pageInfo { hasNextPage endCursor }
          } } } }'''
        cursor = None
        heads = set()
        ids = set()
        expected = None
        for _ in range(10):
            value = self.request("graphql", body={"query": query, "variables": {"cursor": cursor}})
            if value.get("errors"):
                raise Unsafe("queue GraphQL error")
            repo = value.get("data", {}).get("repository")
            if not repo or repo.get("databaseId") != REPO_ID or not repo.get("mergeQueue"):
                raise Unsafe("queue unavailable or foreign repository")
            entries = repo["mergeQueue"]["entries"]
            total = entries.get("totalCount")
            if type(total) is not int or total < 0 or total > 1000 or (expected is not None and total != expected):
                raise Unsafe("queue size changed or exceeds pagination limit")
            expected = total
            if not isinstance(entries.get("nodes"), list) or len(entries["nodes"]) > 100:
                raise Unsafe("invalid queue page")
            for entry in entries["nodes"]:
                sha = (entry.get("headCommit") or {}).get("oid", "")
                if not SHA.fullmatch(sha) or not entry.get("id") or entry["id"] in ids:
                    raise Unsafe("ambiguous queue head or duplicate page")
                ids.add(entry["id"])
                heads.add(sha)
            info = entries["pageInfo"]
            if info["hasNextPage"] is False:
                if len(ids) != expected:
                    raise Unsafe("incomplete queue pagination")
                return heads
            if info["hasNextPage"] is not True or not info.get("endCursor") or info["endCursor"] == cursor:
                raise Unsafe("invalid queue pagination")
            cursor = info["endCursor"]
        raise Unsafe("queue pagination incomplete")

    def current_run(self, run_id):
        return self.request(f"repos/{REPO}/actions/runs/{run_id}")

    def comparison(self, sha, main):
        return self.request(f"repos/{REPO}/compare/{sha}...{main}")

    def cancel(self, run_id):
        # Ordinary cancel only: a completed run's 409 is retained as a race,
        # never followed by force-cancel or retry against another identity.
        self.request(f"repos/{REPO}/actions/runs/{run_id}/cancel", body={})


def run_identity(run, branch):
    if (not repository_matches(run.get("repository")) or
            not repository_matches(run.get("head_repository")) or
            run.get("event") != "merge_group" or run.get("head_branch") != branch or
            run.get("workflow_id") != WORKFLOW_ID or run.get("path") != WORKFLOW_PATH or
            type(run.get("id")) is not int or run["id"] <= 0 or
            type(run.get("run_attempt")) is not int or run["run_attempt"] <= 0 or
            not SHA.fullmatch(run.get("head_sha", ""))):
        raise Unsafe("run identity outside allowlist")
    return (run["id"], run["head_sha"], run["run_attempt"])


def prove_obsolete(api, candidate, branch):
    current = api.current_run(candidate["id"])
    if run_identity(current, branch) != run_identity(candidate, branch) or current.get("status") not in LIVE:
        raise Unsafe("run changed attempt, identity or completed")
    sha = current["head_sha"]
    if api.branch_sha(branch) is not None:
        raise Unsafe("queue ref exists (including moved or reused ref)")
    if sha in api.queue_heads():
        raise Unsafe("head is still in the active queue")
    main = api.branch_sha("main")
    comparison = api.comparison(sha, main)
    # compare(candidate...main): ahead/identical means main contains candidate.
    # Only behind/diverged prove it is not reusable merged-main evidence.
    if comparison.get("base_commit", {}).get("sha") != sha or comparison.get("status") not in {"behind", "diverged"}:
        raise Unsafe("main evidence or ambiguous ancestry")
    if api.branch_sha("main") != main:
        raise Unsafe("main moved during ancestry proof")
    if api.branch_sha(branch) is not None:
        raise Unsafe("queue ref recreated during proof")


def clean_deleted_group(api, event, *, apply=False):
    report = {"cancelled": [], "eligible": [], "retained": []}
    branch = event.get("ref", "")
    try:
        if (event.get("ref_type") != "branch" or not QUEUE_REF.fullmatch(branch) or
                not repository_matches(event.get("repository")) or not repository_matches(api.repository())):
            raise Unsafe("delete event outside allowlist")
        runs = api.runs_for_branch(branch)
        # A reused ref with another SHA has no unambiguous deleted identity.
        identities = [run_identity(run, branch) for run in runs]
        if len({identity[1] for identity in identities}) > 1:
            raise Unsafe("ref name has been reused for different heads")
        for candidate in runs:
            if candidate.get("status") not in LIVE:
                report["retained"].append({"id": candidate["id"], "reason": "not queued or in progress"})
                continue
            try:
                prove_obsolete(api, candidate, branch)
                report["eligible"].append(candidate["id"])
                if apply:
                    # A fresh full proof immediately precedes the POST. There
                    # is no API atomic compare-and-cancel; retain any change.
                    prove_obsolete(api, candidate, branch)
                    api.cancel(candidate["id"])
                    report["cancelled"].append(candidate["id"])
            except (Unsafe, OSError, ValueError, KeyError, TypeError) as error:
                report["retained"].append({"id": candidate["id"], "reason": str(error)})
    except (Unsafe, OSError, ValueError, KeyError, TypeError) as error:
        report["retained"].append({"reason": str(error)})
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--event-file", type=Path, default=os.environ.get("GITHUB_EVENT_PATH"))
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    api = GitHub(os.environ["GH_TOKEN"])
    started = time.monotonic()
    report = clean_deleted_group(api, json.loads(args.event_file.read_text()), apply=args.apply)
    report.update(api_requests=api.requests, seconds=round(time.monotonic() - started, 3))
    output = json.dumps(report, indent=2)
    print(output)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as summary:
            summary.write("Obsolete merge-group cleanup (retention on uncertainty):\n```json\n" + output + "\n```\n")


if __name__ == "__main__":
    main()
