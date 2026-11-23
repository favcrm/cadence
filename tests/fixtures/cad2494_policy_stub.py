#!/usr/bin/env python3
"""Test double for `gh` and `cadence` that adds an approved custom policy to the
CAD-2100 stub: it answers the policy's check runs and patches the requirements
answer, and hands every other call to the base stub unchanged.

CAD2494_BASE_DIR holds the base stub installed as `gh` and `cadence`. The
scenario (CAD2100_SCENARIO) adds `req_patch` (fields set on the requirements
answer) and `policy_check_runs` ({check name: [[app slug, status, conclusion,
started_at], ...]}).
"""
import json
import os
import subprocess
import sys
from urllib.parse import parse_qs, urlparse

S = json.load(open(os.environ["CAD2100_SCENARIO"]))
NAME = os.path.basename(sys.argv[0])
ARGS = sys.argv[1:]
BASE = os.path.join(os.environ["CAD2494_BASE_DIR"], NAME)


def base():
    done = subprocess.run([sys.executable, BASE, *ARGS], stdin=sys.stdin, capture_output=True,
                          text=True)
    return done


def relay(done):
    sys.stdout.write(done.stdout)
    sys.stderr.write(done.stderr)
    sys.exit(done.returncode)


if NAME == "gh" and ARGS[:1] == ["api"]:
    path = next((a for a in ARGS[1:] if a.startswith("repos/")), "")
    if "/check-runs?" in path:
        name = parse_qs(urlparse(path).query).get("check_name", [""])[0]
        rows = (S.get("policy_check_runs") or {}).get(name)
        if rows is not None:
            runs = [{"id": 100 + i, "name": name, "head_sha": S["head"], "status": status,
                     "conclusion": conclusion, "started_at": started,
                     "app": {"id": 500 + i, "slug": app}, "check_suite": {"id": 900 + i}}
                    for i, (app, status, conclusion, started) in enumerate(rows)]
            print(json.dumps({"total_count": len(runs), "check_runs": runs}))
            sys.exit(0)
if NAME == "cadence" and ARGS[:2] == ["delivery", "requirements"]:
    done = base()
    if done.returncode != 0:
        relay(done)
    doc = json.loads(done.stdout)
    doc["requirements"].update(S.get("req_patch") or {})
    print(json.dumps(doc))
    sys.exit(0)
relay(base())
