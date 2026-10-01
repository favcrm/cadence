#!/usr/bin/env python3
# CAD-918: the daemon's boot-fixed `gh` for delegated-approval tests.
# Serves delegated-gh.json beside it and asserts every argument: the
# repo, the PR number and the field list. Each call is logged.
import json, os, sys
d = os.path.dirname(os.path.abspath(__file__))
st = json.load(open(os.path.join(d, "delegated-gh.json")))
a = sys.argv[1:]
with open(os.path.join(d, "gh.log"), "a") as f:
    f.write(" ".join(a) + "\n")
REPO, PR, HEAD = "acme/app", st["pr"], st["view"]["headRefOid"]
VIEW = ("headRefOid,headRefName,baseRefName,state,title,author,"
        "statusCheckRollup,files,changedFiles")


def need(ok):
    if not ok:
        sys.stderr.write("fake gh: unexpected arguments %r\n" % a)
        sys.exit(3)


if a[:2] == ["pr", "view"] and a[-1] == "state,mergeCommit":
    need(a == ["pr", "view", PR, "-R", REPO, "--json", "state,mergeCommit"])
    print(json.dumps(st.get("merge", {"state": "OPEN", "mergeCommit": None})))
elif a[:2] == ["pr", "view"]:
    need(a == ["pr", "view", PR, "-R", REPO, "--json", VIEW])
    print(json.dumps(st["view"]))
elif a[:2] == ["pr", "diff"]:
    need(a == ["pr", "diff", PR, "-R", REPO])
    sys.stdout.write(st["diff"])
elif a[:1] == ["api"] and a[1] == "repos/%s/branches/main" % REPO:
    need(len(a) == 2)
    print(json.dumps(st["branch"]))
elif a[:1] == ["api"] and a[1] == "repos/%s/commits/%s/check-runs?per_page=100" % (REPO, HEAD):
    need(len(a) == 2)
    print(json.dumps(st["runs"]))
else:
    need(False)
