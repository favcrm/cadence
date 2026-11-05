#!/usr/bin/env python3
"""Test double for `gh` and `cadence`, as seen by scripts/enqueue-reviewed.

CAD-2081 acceptance support: the suite installs this file twice, as `gh` and as
`cadence`, and points CAD2081_SCENARIO at a JSON document describing the PR, the
org's verdict rows and the files each ref carries. It answers only what the gate
asks and nothing else, so an unexpected question fails loudly.
"""
import json
import os
import re
import sys

S = json.load(open(os.environ["CAD2081_SCENARIO"]))
NAME = os.path.basename(sys.argv[0])
ARGS = sys.argv[1:]
RUN_URL = f"https://github.com/{S['repo']}/actions/runs/101"


def out(doc):
    print(json.dumps(doc))
    sys.exit(0)


def fail(msg, rc=1):
    print(msg, file=sys.stderr)
    sys.exit(rc)


def gh():
    if ARGS[:2] == ["pr", "view"]:
        out({"number": S["pr"], "state": "OPEN", "title": S["title"], "headRefName": "feature",
             "headRefOid": S["head"], "baseRefName": S["base"], "baseRefOid": S["base_oid"],
             "mergeStateStatus": "CLEAN", "autoMergeRequest": None, "statusCheckRollup": [],
             "author": {"login": S["author"]}})
    if ARGS[:1] != ["api"]:
        fail(f"stub gh: unexpected {ARGS}", 3)
    path = next(a for a in ARGS[1:] if a.startswith("repos/"))
    repo = S["repo"]
    if path == f"repos/{repo}/git/ref/heads/main":
        if S.get("tip_fails"):
            fail("Not Found (HTTP 404)")
        print(S.get("tip_sha") or S["tip"])
        sys.exit(0)
    if path.startswith(f"repos/{repo}/rules/branches/"):
        out([{"type": "required_status_checks", "parameters": {"required_status_checks": [
            {"context": "ci", "integration_id": 15368}]}}])
    if path.endswith("/protection"):
        fail("Branch not protected (HTTP 404)")
    if path.startswith(f"repos/{repo}/actions/runs?head_sha={S['head']}"):
        out({"total_count": 1, "workflow_runs": [{
            "id": 101, "run_number": 5, "run_attempt": 1, "workflow_id": 77,
            "path": ".github/workflows/ci.yml", "check_suite_id": 55, "head_sha": S["head"],
            "event": "pull_request", "status": "completed", "conclusion": "success",
            "run_started_at": "2026-11-03T10:00:00Z", "html_url": RUN_URL}]})
    if "/check-runs?" in path:
        out({"total_count": 1, "check_runs": [{
            "id": 9, "name": "ci", "head_sha": S["head"], "status": "completed",
            "conclusion": "success", "started_at": "2026-11-03T10:00:01Z",
            "app": {"id": 15368, "slug": "github-actions"}, "check_suite": {"id": 55}}]})
    m = re.fullmatch(rf"repos/{re.escape(repo)}/issues/comments/([0-9]+)", path)
    if m:
        comment = (S.get("qa_comments") or {}).get(m.group(1))
        if comment is None:
            fail("Not Found (HTTP 404)")
        out(comment)
    m = re.fullmatch(rf"repos/{re.escape(repo)}/contents/(.+)\?ref=([^&]+)", path)
    if m:
        # The ref matters: a file read at the head is not a file read at the base.
        text = S["files_by_ref"].get(m.group(2), {}).get(m.group(1))
        if text is None:
            fail("Not Found (HTTP 404)")
        print(text, end="")
        sys.exit(0)
    fail(f"stub gh: unexpected api {path}", 3)


def cadence():
    if ARGS[:3] == ["issue", "show", "CAD-1"]:
        names = " ".join(v["name"] for v in S["verdicts"])
        body = f"{S['head']}\nBase branch: {S['base']}\nGreen CI run: {RUN_URL}\n{names}\n"
        if S.get("approved"):
            body += "Operator approval id: ap-1\n"
        if S.get("class") == "routine":
            body += ("Delivery class: routine\nPolicy digest: sha256:" + "0" * 64
                     + "\nOutcome report: CAD-1/reports/x\nNative CI run: 101 " + RUN_URL + "\n")
        if S.get("class") == "consequential":
            body += "Delivery class: consequential\nPolicy digest: sha256:" + "0" * 64 + "\n"
        for comment_id in () if S.get("no_qa_evidence") else (S.get("qa_comments") or {}):
            body += (f"Browser QA comment: https://github.com/{S['repo']}/pull/{S['pr']}"
                     f"#issuecomment-{comment_id}\n")
        if S.get("qa_evidence_url"):
            body += f"Browser QA comment: {S['qa_evidence_url']}\n"
        out({"id": "CAD-1", "project": "cadence", "owner": None, "claim": None,
             "comments": [{"body": body}]})
    if ARGS[:2] == ["delivery", "requirements"]:
        digest = "sha256:" + "0" * 64
        kind = S.get("class", "strict")
        if kind == "routine":
            req = {"class": "routine", "activation": "active", "reviews": 0,
                   "review_kind": "none", "security_capable": False,
                   "operator_approval": False, "browser_qa": False, "full_checks": False}
        elif kind == "consequential":
            req = {"class": "consequential", "activation": "active", "reviews": 1,
                   "review_kind": "combined", "security_capable": False,
                   "operator_approval": False, "browser_qa": False, "full_checks": True}
        elif kind == "strict1":
            req = {"class": "strict", "activation": "no_profile", "reviews": 1,
                   "review_kind": "combined", "security_capable": False,
                   "operator_approval": False, "browser_qa": False, "full_checks": True}
        else:
            req = {"class": "strict", "activation": "no_profile", "reviews": 2,
                   "review_kind": "standards_and_spec", "security_capable": False,
                   "operator_approval": False, "browser_qa": False, "full_checks": True}
        req.update({"policy_digest": digest, "reasons": []})
        if S.get("board_asks_qa"):
            req["browser_qa"] = True
        readiness = None
        if kind == "routine" and not S.get("no_readiness"):
            readiness = {"repo": S["repo"].lower(), "pr": S["pr"], "head": S["head"],
                         "base": S["base_oid"], "base_ref": S["base"], "policy_digest": digest,
                         "ci_run_id": 101, "ci_run_url": RUN_URL,
                         "outcome_report": "CAD-1/reports/x"}
        doc = {"issue": "CAD-1", "project": "cadence", "repo": S["repo"].lower(), "pr": S["pr"],
               "head": S["head"], "base": S["base_oid"], "base_ref": S["base"],
               "merge_base": S["base_oid"], "policy_digest": digest, "origin": "hosted",
               "requirements": req, "readiness": readiness}
        if S.get("native"):
            del doc["origin"]  # the daemon's own answer carries no marker
        out(doc)
    if ARGS[:2] == ["audit", "verdicts"]:
        # A row with `at_head` was filed at that head: the board answers for the head asked about.
        asked = ARGS[ARGS.index("--head") + 1]
        rows = [v for v in S["verdicts"] if v.get("at_head", asked) == asked]
        if S.get("native"):
            out({"origin": "local", "verdicts": rows, "skipped": []})
        if ARGS[ARGS.index("--project") + 1] != "cadence":
            fail("stub cadence: verdicts read without the ticket's project", 3)
        out({"origin": "hosted", "project": "cadence", "verdicts": rows, "skipped": []})
    if ARGS[:2] == ["audit", "identities"]:
        if S["identities"] is None:
            fail("the workspace identity map is not readable by this caller")
        out({"identities": S["identities"]})
    if ARGS[:2] == ["audit", "approval"]:
        # A set `approved_head` is the one head the operator approved.
        if S.get("approved") and S.get("approved_head") in (None, ARGS[ARGS.index("--head") + 1]):
            out({"state": "in-force", "approval_id": "ap-1", "recorded_via": "operator-connection",
                 "source": "operator", "action": "merge"})
        print(json.dumps({"state": "missing", "reason": "no approval recorded"}))
        sys.exit(1)
    fail(f"stub cadence: unexpected {ARGS}", 3)


{"gh": gh, "cadence": cadence}[NAME]()
