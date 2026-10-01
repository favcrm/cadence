#!/usr/bin/env python3
# CAD-918: the daemon's boot-fixed `gh` for delegated-approval tests.
# Serves `pr view` and `pr diff` from delegated-gh.json beside it.
import json, os, sys
d = os.path.dirname(os.path.abspath(__file__))
st = json.load(open(os.path.join(d, "delegated-gh.json")))
a = sys.argv[1:]
if a[:2] == ["pr", "view"]:
    print(json.dumps(st["view"]))
elif a[:2] == ["pr", "diff"]:
    sys.stdout.write(st["diff"])
else:
    sys.exit(2)
