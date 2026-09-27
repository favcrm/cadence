#!/usr/bin/env python3
"""Deterministic CAD631 worker using the real Pi transport and daemon socket.

Reuse the existing protocol fixture rather than emulate daemon completion.
No caller assertion or turn token is invented, and no token is journalled.
"""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import socket
import time

spec = importlib.util.spec_from_file_location(
    "cad631_pi_protocol", Path(__file__).with_name("fake-pi.py")
)
pi = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pi)

STATE = Path(os.environ["CADENCE_STATE_DIR"])
ALIAS = os.environ["CADENCE_ALIAS"]
DRAFT = "Lunch is served from noon to 3pm."


def rpc(method, params):
    # Native peer attribution applies. In particular there is no test caller
    # frame and no operator identity in this request.
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(5)
        connection.connect(str(STATE / "cadence.sock"))
        connection.sendall(
            (json.dumps({"method": method, "params": params}) + "\n").encode()
        )
        answer = json.loads(connection.makefile("rb").readline())
    if answer.get("ok") is not True:
        raise RuntimeError("CAD631 native RPC refused: " + str(answer.get("error")))
    return answer["result"]


def kickoff_from_prompt(prompt):
    decoder = json.JSONDecoder()
    matches = []
    for position, char in enumerate(prompt):
        if char != "{":
            continue
        try:
            candidate, _ = decoder.raw_decode(prompt[position:])
        except ValueError:
            continue
        if (
            isinstance(candidate, dict)
            and candidate.get("schema") == 1
            and candidate.get("kind") in ("produce_text", "review_text")
            and isinstance(candidate.get("run_id"), str)
            and isinstance(candidate.get("step_id"), str)
        ):
            matches.append(candidate)
    if len(matches) != 1:
        raise RuntimeError("expected exactly one app kickoff in provider prompt")
    return matches[0]


def active_turn(kickoff):
    deadline = time.monotonic() + 10
    task = kickoff["run_id"] + "-" + kickoff["step_id"]
    while time.monotonic() < deadline:
        show = rpc("agent_show", {"alias": ALIAS})
        matches = [
            message
            for message in show["messages"]
            if message.get("task_id") == task
            and message.get("source") == "app_run_dispatch"
            and message.get("state") == "running"
            and isinstance(message.get("turn_id"), str)
        ]
        if len(matches) == 1:
            return matches[0]
        time.sleep(0.02)
    raise RuntimeError("provider prompt never obtained its actual active turn")


def journal(row):
    path = STATE / "agents" / ALIAS / "app-run-receipts.jsonl"
    with path.open("a") as output:
        output.write(json.dumps(row, sort_keys=True) + "\n")


def run_prompt(prompt):
    pi.live_turn = True
    pi.emit({"type": "agent_start"})
    pi.emit({"type": "turn_start"})
    pi.emit({"type": "message_start", "message": {"role": "assistant"}})
    kickoff = kickoff_from_prompt(prompt)
    turn = active_turn(kickoff)
    common = {
        "schema": 1,
        "kind": kickoff["kind"],
        "run_id": kickoff["run_id"],
        "step_id": kickoff["step_id"],
        "revision": kickoff["revision"],
    }
    receipt = {
        "run_id": kickoff["run_id"],
        "step_id": kickoff["step_id"],
        "kind": kickoff["kind"],
        "message_id": turn["id"],
        "native_turn_observed": True,
    }
    if kickoff["kind"] == "produce_text":
        (STATE / "app-run-writer-held").write_text(kickoff["run_id"])
        # Hold this actual running turn until concurrent operator dispatch
        # calls finish. The flag belongs only to this test's state dir.
        deadline = time.monotonic() + 15
        while not (STATE / "app-run-release-writer").exists():
            if time.monotonic() >= deadline:
                raise RuntimeError("writer release flag not supplied")
            time.sleep(0.02)
        result = dict(
            common,
            outcome="succeeded",
            artifacts=[{"media_type": "text/markdown", "text": DRAFT}],
        )
        receipt["artifact_sha256"] = "sha256:" + hashlib.sha256(DRAFT.encode()).hexdigest()
    else:
        dependencies = kickoff["dependencies"]
        if len(dependencies) != 1:
            raise RuntimeError("review requires one actual artifact reference")
        dependency = dependencies[0]
        artifact = rpc(
            "app_run_artifact",
            {
                "artifact_id": dependency["artifact_id"],
                "message": turn["id"],
                "token": turn["turn_id"],
            },
        )
        actual_digest = "sha256:" + hashlib.sha256(artifact["text"].encode()).hexdigest()
        if (
            artifact["id"] != dependency["artifact_id"]
            or artifact["digest"] != actual_digest
            or dependency["sha256"] != actual_digest
            or artifact["size"] != len(artifact["text"].encode())
            or artifact["text"] != DRAFT
        ):
            raise RuntimeError("actual fetched artifact does not match its pinned receipt")
        result = dict(
            common,
            producer_step_id=dependency["producer_step_id"],
            producer_revision=dependency["revision"],
            artifact_sha256=actual_digest,
            decision="approve",
            rationale="The fetched draft retains the supplied lunch hours.",
        )
        receipt.update(artifact_sha256=actual_digest, dependency_fetched=True)
        if (STATE / "app-run-hold-reviewer").exists():
            # An opt-in partial-restart test pauses only its own actual
            # review turn, after proving authenticated dependency access.
            (STATE / "app-run-reviewer-held").write_text(kickoff["run_id"])
            deadline = time.monotonic() + 15
            while not (STATE / "app-run-release-reviewer").exists():
                if time.monotonic() >= deadline:
                    raise RuntimeError("held reviewer was interrupted before material result")
                time.sleep(0.02)
    journal(receipt)
    text = json.dumps(result, sort_keys=True)
    pi.emit(
        {
            "type": "message_update",
            "assistantMessageEvent": {
                "type": "text_delta",
                "contentIndex": 0,
                "delta": text,
            },
        }
    )
    pi.live_turn = False
    pi.finish_turn("stop", text)


pi.run_prompt = run_prompt
pi.main()
