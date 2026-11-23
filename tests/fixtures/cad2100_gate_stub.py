#!/usr/bin/env python3
"""Test double for `gh` and `cadence`, backed by a REAL git repository.

CAD-2100 acceptance support. The suite installs this file as `gh` (for both
`scripts/enqueue-reviewed` and the daemon's classifier) and as `cadence` (for
the script only), and points CAD2100_SCENARIO at a JSON document. Everything a
GitHub answer says about commits (compare, trees, file contents) is computed
from the repository at `repo_dir`, so a stale base, a merge of main and a
restored file are real git history, not canned lists. The knobs below damage
one answer at a time. It answers only what the callers ask and fails loudly on
anything else.
"""
import base64
import json
import os
import re
import subprocess
import sys

S = json.load(open(os.environ["CAD2100_SCENARIO"]))
NAME = os.path.basename(sys.argv[0])
ARGS = sys.argv[1:]
REPO = S["repo"]
RUN_URL = f"https://github.com/{REPO}/actions/runs/101"
RAW = "Accept: application/vnd.github.raw"


def out(doc):
    print(json.dumps(doc))
    sys.exit(0)


def fail(msg, rc=1):
    print(msg, file=sys.stderr)
    sys.exit(rc)


def git(*args):
    done = subprocess.run(["git", *args], cwd=S["repo_dir"], capture_output=True,
                          env={"PATH": "/usr/bin:/bin", "HOME": "/nonexistent",
                               "GIT_CONFIG_NOSYSTEM": "1"})
    if done.returncode != 0:
        fail("Not Found (HTTP 404)")
    return done.stdout


def compare(base, head):
    merge_base = git("merge-base", base, head).decode().strip()
    raw = git("diff", "--raw", "-z", "-M", "--no-abbrev", merge_base, head).split(b"\0")
    names = {"A": "added", "M": "modified", "D": "removed", "R": "renamed", "T": "changed",
             "C": "copied"}
    files, i = [], 0
    while i < len(raw) and raw[i]:
        letter = raw[i].decode().split()[-1][0]
        i += 1
        old = new = raw[i].decode()
        i += 1
        if letter in "RC":
            new = raw[i].decode()
            i += 1
        added = deleted = 0
        if letter not in "RC":
            for row in git("diff", "--numstat", "--no-renames", merge_base, head, "--",
                           new).decode().splitlines():
                a, d = row.split("\t")[:2]
                added += int(a) if a.isdigit() else 0
                deleted += int(d) if d.isdigit() else 0
        entry = {"filename": new, "status": names[letter], "additions": added,
                 "deletions": deleted}
        if letter == "R" and not S.get("compare_strip_previous"):
            entry["previous_filename"] = old
        files.append(entry)
    drop = set(S.get("compare_drop", []))
    files = [f for f in files if f["filename"] not in drop] + S.get("compare_extra", [])
    while len(files) < S.get("compare_pad", 0):
        files.append({"filename": f"pad/{len(files)}.txt", "status": "added",
                      "additions": 1, "deletions": 0})
    return {"merge_base_commit": {"sha": merge_base}, "files": files}


def trees(sha):
    entries = []
    for row in git("ls-tree", "-r", "-t", "-z", "--full-tree", sha).split(b"\0"):
        if not row:
            continue
        meta, path = row.decode().split("\t", 1)
        mode, kind, obj = meta.split()
        entries.append({"path": path, "mode": mode, "type": kind, "sha": obj})
    return {"sha": sha, "truncated": bool(S.get("trees_truncated")), "tree": entries}


def contents(path, ref):
    override = (S.get("ref_overrides") or {}).get(ref, {})
    if path in override:
        text = override[path]
        if text is None:
            fail("Not Found (HTTP 404)")
        data = text.encode()
    else:
        data = git("show", f"{ref}:{path}")
    if RAW in ARGS:
        sys.stdout.write(data.decode())
        sys.exit(0)
    out({"encoding": "base64", "content": base64.b64encode(data).decode()})


def pr_view():
    out({"number": S["pr"], "state": "OPEN", "title": S["title"], "headRefName": "feature",
         "headRefOid": S["head"], "baseRefName": S["base"], "baseRefOid": S["base_oid"],
         "changedFiles": S["changed_files"], "mergeStateStatus": "CLEAN",
         "autoMergeRequest": None, "statusCheckRollup": [],
         "author": {"login": S.get("author", "authorlogin")}})


def gh():
    if ARGS[:2] == ["pr", "view"]:
        pr_view()
    if ARGS[:2] == ["repo", "view"]:
        out({"defaultBranchRef": {"name": S.get("default_branch", "main")}})
    if ARGS[:1] != ["api"]:
        fail(f"stub gh: unexpected {ARGS}", 3)
    path = next(a for a in ARGS[1:] if a.startswith("repos/"))
    if path == f"repos/{REPO}/git/ref/heads/main":
        if S.get("tip_fails"):
            fail("Not Found (HTTP 404)")
        tip = S["tip_sha"] if "tip_sha" in S else S["tip"]
        if "--jq" in ARGS:
            print(tip)
            sys.exit(0)
        out({"ref": "refs/heads/main", "object": {"sha": tip, "type": "commit"}})
    m = re.fullmatch(rf"repos/{re.escape(REPO)}/compare/([^.]+)\.\.\.(.+)", path)
    if m:
        out(compare(m.group(1), m.group(2)))
    m = re.fullmatch(rf"repos/{re.escape(REPO)}/git/trees/([0-9a-f]+)\?recursive=1", path)
    if m:
        out(trees(m.group(1)))
    m = re.fullmatch(rf"repos/{re.escape(REPO)}/contents/(.+)\?ref=([^&]+)", path)
    if m:
        contents(m.group(1), m.group(2))
    if path.startswith(f"repos/{REPO}/rules/branches/"):
        out([{"type": "required_status_checks", "parameters": {"required_status_checks": [
            {"context": "ci", "integration_id": 15368}]}}])
    if path.endswith("/protection"):
        fail("Branch not protected (HTTP 404)")
    if path.startswith(f"repos/{REPO}/actions/runs?head_sha={S['head']}"):
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
    fail(f"stub gh: unexpected api {path}", 3)


def cadence():
    if ARGS[:2] == ["issue", "comment"]:
        # The evidence comment the script would post: keep it for the test to read.
        with open(S["comment_out"], "w") as handle:
            handle.write(sys.stdin.read())
        out({})
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
        comments = [{"body": body}]
        if S.get("comment_out") and os.path.exists(S["comment_out"]):
            comments.append({"body": open(S["comment_out"]).read()})
        out({"id": "CAD-1", "project": "cadence", "owner": None, "claim": None,
             "comments": comments})
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
        readiness = None
        if kind == "routine":
            readiness = {"repo": REPO, "pr": S["pr"], "head": S["head"],
                         "base": S["base_oid"], "base_ref": S["base"], "policy_digest": digest,
                         "ci_run_id": 101, "ci_run_url": RUN_URL,
                         "outcome_report": "CAD-1/reports/x"}
        out({"issue": "CAD-1", "project": "cadence", "repo": REPO, "pr": S["pr"],
             "head": S["head"], "base": S["base_oid"], "base_ref": S["base"],
             "merge_base": S["base_oid"], "policy_digest": digest, "origin": "hosted",
             "requirements": req, "readiness": readiness})
    if ARGS[:2] == ["audit", "verdicts"]:
        if ARGS[ARGS.index("--project") + 1] != "cadence":
            fail("stub cadence: verdicts read without the ticket's project", 3)
        out({"origin": "hosted", "project": "cadence", "verdicts": S["verdicts"], "skipped": []})
    if ARGS[:2] == ["audit", "identities"]:
        out({"identities": S["identities"]})
    if ARGS[:2] == ["audit", "approval"]:
        if S.get("approved"):
            out({"state": "in-force", "approval_id": "ap-1", "recorded_via": "operator-connection",
                 "source": "operator", "action": "merge"})
        print(json.dumps({"state": "missing", "reason": "no approval recorded"}))
        sys.exit(1)
    fail(f"stub cadence: unexpected {ARGS}", 3)


{"gh": gh, "cadence": cadence}[NAME]()
