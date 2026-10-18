#!/usr/bin/env python3
"""CAD-1540 acceptance: the cross-build required-check gate must refuse a matrix
that failed or was cancelled, and pass only when it passed or was legitimately
skipped (docs-only changes).

Written from the ticket, not from the implementation: it drives the real gate
script the workflow runs with each of the four matrix outcomes, and checks the
workflow wires it to a job that always runs — which is the property that makes
the check requireable at all.

CAD-1541: every wiring assertion is scoped to the gate job's own block, so a
future job adding `always()` for its own reasons cannot stand in for
`cross-build-required` losing it.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys

import yaml
REPO = pathlib.Path(__file__).resolve().parents[1]
GATE = REPO / "scripts" / "cross-build-gate.sh"
WORKFLOW = REPO / ".github" / "workflows" / "ci.yml"

GATE_JOB = "cross-build-required"
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
    """Assert on the gate job itself, never on the file as a whole: a whole-file
    match would be satisfied by any other job's `always()`, which is exactly how
    the gate could lose its own condition without this check noticing."""
    try:
        doc = yaml.safe_load(workflow) or {}
    except yaml.YAMLError as exc:
        return [f"ci.yml is not parseable YAML: {exc}"]

    job = (doc.get("jobs") or {}).get(GATE_JOB)
    if not isinstance(job, dict):
        return [f"ci.yml has no `{GATE_JOB}` job"]

    failures = []

    needs = job.get("needs")
    if needs not in (["cross-build"], "cross-build"):
        failures.append(
            f"`{GATE_JOB}` needs {needs!r}; it must depend on the cross-build matrix"
        )

    condition = str(job.get("if", ""))
    if "always()" not in condition:
        failures.append(
            f"`{GATE_JOB}` does not use always() in its own `if`, so a failed "
            "matrix would skip it and the required check would never report"
        )
    if "github.event_name" not in condition:
        failures.append(f"`{GATE_JOB}` does not gate on the event name")

    steps = job.get("steps") or []
    if not any(
        "scripts/cross-build-gate.sh" in str(step.get("run", ""))
        for step in steps
        if isinstance(step, dict)
    ):
        failures.append(f"`{GATE_JOB}` does not run scripts/cross-build-gate.sh")
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
        "and its own job runs under always() with the matrix as a dependency"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
