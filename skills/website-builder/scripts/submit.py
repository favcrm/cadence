"""Record one step's owner-confirmed data and advance exactly one step.

Usage: python3 scripts/submit.py --run DIR --data FILE

FILE is {"step": "<name>", "data": {...}}; "step" may be omitted to mean the
current step. Resubmitting a recorded step with the same data is a no-op.
"""

import argparse
import json
import os

from common import (
    DONE,
    ORDER,
    STEPS,
    check_record,
    digest,
    emit,
    load,
    now,
    refuse,
    run,
    save,
    view,
)

# check.py --record owns these keys for verify; owner data may not set them.
EVIDENCE_KEYS = ("url", "status", "bytes_match", "csp", "nosniff", "form", "checked_at")


def read_payload(path):
    try:
        with open(path, encoding="utf-8") as handle:
            payload = json.load(handle)
    except ValueError as error:
        refuse(f"--data is not JSON: {error}")
    if not isinstance(payload, dict):
        refuse("--data must be a JSON object")
    unknown = sorted(set(payload) - {"step", "data"})
    if unknown:
        refuse(f"unknown top-level keys in --data: {', '.join(unknown)}")
    if not isinstance(payload.get("data"), dict):
        refuse("--data.data must be an object")
    return payload.get("step"), payload["data"]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--run", required=True, metavar="DIR")
    parser.add_argument("--data", required=True, metavar="FILE")
    args = parser.parse_args()

    state = load(args.run)
    step, data = read_payload(args.data)
    if step is None:
        step = state["step"]
        if step == DONE:
            refuse("the run is done; there is no step left to submit")
    if step not in STEPS:
        refuse(f"unknown step {step!r}; steps are {', '.join(STEPS)}")
    if step == "verify" and any(key in data for key in EVIDENCE_KEYS):
        refuse("verify: evidence keys are written by check.py --record, not submitted")

    position = ORDER.index(step)
    current = ORDER.index(state["step"])
    if position < current:
        recorded = state[step]
        if all(recorded.get(key) == value for key, value in data.items()):
            emit({"step": step, "recorded": True, "idempotent": True, "next": view(state)})
            return 0
        refuse(f"{step} is already recorded with different data; the run does not overwrite it")
    if position > current:
        refuse(f"{step} is out of order; the next step is {state['step']}")

    record = dict(data)
    check_record(step, record, args.run, state)
    stamp = now()
    if step == "verify":
        # check.py --record owns the evidence keys; the owner adds acceptance.
        state["verify"] = {**(state["verify"] or {}), **record}
        record = state["verify"]
    else:
        state[step] = record
    state["step"] = ORDER[position + 1]
    state["status"] = DONE if state["step"] == DONE else "open"
    state["history"].append(
        {"at": stamp, "step": step, "action": "submitted", "digest": digest(record)}
    )
    save(args.run, state, stamp)
    emit({"step": step, "recorded": True, "idempotent": False, "next": view(state)})
    return 0


if __name__ == "__main__":
    raise SystemExit(run(main))
