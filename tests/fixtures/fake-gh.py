#!/usr/bin/env python3
import json, os, subprocess, sys
d = os.path.dirname(os.path.abspath(__file__))
state_p = os.path.join(d, "gh-state.json")
with open(os.path.join(d, "gh.log"), "a") as f:
    f.write(" ".join(sys.argv[1:]) + "\n")
st = json.load(open(state_p))
a = sys.argv[1:]


def observed_head():
    """The PR's head: `head_from` asks the real git repo for the tip of
    the ticket's branch (the E2E, where the worker's commit IS the PR
    head — a stale state file must never be observed); else the stored
    `head` (the integration test, which moves it by hand)."""
    src = st.get("head_from")
    if src:
        r = subprocess.run(
            ["git", "-C", src["repo"], "for-each-ref", "--format=%(objectname)",
             "refs/heads/" + src["glob"]],
            capture_output=True, text=True)
        heads = r.stdout.split()
        if heads:
            return heads[0]
    return st["head"]


if a[:2] == ["pr", "view"]:
    run = {"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS"}
    if not st["green"]:
        run = {"__typename": "CheckRun", "name": "ci", "status": "IN_PROGRESS", "conclusion": None}
    print(json.dumps({"headRefOid": observed_head(), "state": st["state"],
                      "statusCheckRollup": [run], "additions": 12, "deletions": 3,
                      "changedFiles": 2,
                      "autoMergeRequest": {"enabledAt": "x"} if st["auto"] else None}))
elif a[:2] == ["pr", "merge"]:
    if "--disable-auto" in a:
        st["auto"] = False
    else:
        sha = a[a.index("--match-head-commit") + 1]
        if sha != observed_head():
            sys.stderr.write("head moved\n")
            sys.exit(1)
        st["auto"] = True
        # Auto-merge with checks already green lands the PR at once.
        if st["green"]:
            st["state"] = "MERGED"
    json.dump(st, open(state_p, "w"))
else:
    sys.exit(2)
