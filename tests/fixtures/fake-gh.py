#!/usr/bin/env python3
import json, os, sys
d = os.path.dirname(os.path.abspath(__file__))
state_p = os.path.join(d, "gh-state.json")
with open(os.path.join(d, "gh.log"), "a") as f:
    f.write(" ".join(sys.argv[1:]) + "\n")
st = json.load(open(state_p))
a = sys.argv[1:]
if a[:2] == ["pr", "view"]:
    run = {"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS"}
    if not st["green"]:
        run = {"__typename": "CheckRun", "name": "ci", "status": "IN_PROGRESS", "conclusion": None}
    print(json.dumps({"headRefOid": st["head"], "state": st["state"],
                      "statusCheckRollup": [run], "additions": 12, "deletions": 3,
                      "changedFiles": 2,
                      "autoMergeRequest": {"enabledAt": "x"} if st["auto"] else None}))
elif a[:2] == ["pr", "merge"]:
    if "--disable-auto" in a:
        st["auto"] = False
    else:
        sha = a[a.index("--match-head-commit") + 1]
        if sha != st["head"]:
            sys.stderr.write("head moved\n")
            sys.exit(1)
        st["auto"] = True
    json.dump(st, open(state_p, "w"))
else:
    sys.exit(2)
