#!/usr/bin/env python3
"""CAD-1540 acceptance: the cross-build required-check gate must refuse a matrix
that failed or was cancelled, and pass only when it passed or was legitimately
skipped (docs-only changes).

Written from the ticket, not from the implementation: it drives the real gate
script the workflow runs with each of the four matrix outcomes, and checks the
workflow wires it to a job that always runs — which is the property that makes
the check requireable at all.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys

REPO = pathlib.Path(__file__).resolve().parents[1]
GATE = REPO / "scripts" / "cross-build-gate.sh"
WORKFLOW = REPO / ".github" / "workflows" / "ci.yml"

# matrix result -> may the gate pass?
OUTCOMES = {"success": True, "skipped": True, "failure": False, "cancelled": False}


def run_gate(result: str) -> subprocess.CompletedProcess:
    env = dict(os.environ, CROSS_BUILD_RESULT=result)
    return subprocess.run(
        [str(GATE)], env=env, capture_output=True, text=True, cwd=REPO
    )


def check_outcomes() -> list[str]:
    failures = []
    for result, may_pass in OUTCOMES.items():
        proc = run_gate(result)
        if (proc.returncode == 0) != may_pass:
            failures.append(
                f"gate on {result!r}: exit {proc.returncode}, expected "
                f"{'pass' if may_pass else 'refusal'} — "
                f"{(proc.stdout + proc.stderr).strip()}"
            )
    return failures


def check_wiring(workflow: str) -> list[str]:
    failures = []
    if "scripts/cross-build-gate.sh" not in workflow:
        failures.append("ci.yml does not run scripts/cross-build-gate.sh")
    if "needs: [cross-build]" not in workflow:
        failures.append("the gate job does not depend on the cross-build matrix")
    if "always()" not in workflow:
        failures.append(
            "the gate job does not use always(), so a failed matrix would skip it "
            "and the required check would never report"
        )
    return failures


def main() -> int:
    failures = check_outcomes() + check_wiring(
        WORKFLOW.read_text(encoding="utf-8")
    )
    if failures:
        print("FAIL:")
        for line in failures:
            print(f"  - {line}")
        return 1
    print(
        "ok: gate passes success/skipped, refuses failure/cancelled, "
        "and runs under always() with the matrix as a dependency"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
