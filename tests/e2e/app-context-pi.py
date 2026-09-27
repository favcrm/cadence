#!/usr/bin/env python3
"""Deterministic CAD690 context worker using the real Pi transport and daemon socket.

Reuse the existing protocol fixture rather than emulate daemon completion.
No caller assertion or turn token is invented, and no token is journalled.
"""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import time

spec = importlib.util.spec_from_file_location(
    "cad631_pi_protocol", Path(__file__).with_name("fake-pi.py")
)
pi = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pi)

STATE = Path(os.environ["CADENCE_STATE_DIR"])
ALIAS = os.environ["CADENCE_ALIAS"]



def frame(method, params, detached=False):
    wire = json.dumps({"method": method, "params": params})
    if detached:
        # start_new_session performs real setsid; this remains the enrolled
        # provider's child and carries the genuine current turn in stdin only.
        code = "import socket,sys;s=socket.socket(socket.AF_UNIX);s.settimeout(5);s.connect(sys.argv[1]);s.sendall(sys.stdin.buffer.read()+b'\\n');print(s.makefile().readline())"
        child = subprocess.run([sys.executable, "-c", code, str(STATE / "cadence.sock")],
                               input=wire, text=True, capture_output=True,
                               start_new_session=True, timeout=10)
        if child.returncode != 0:
            raise RuntimeError("detached native probe failed")
        return json.loads(child.stdout)
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(5)
        connection.connect(str(STATE / "cadence.sock"))
        connection.sendall((wire + "\n").encode())
        return json.loads(connection.makefile("rb").readline())


def rpc(method, params):
    answer = frame(method, params)
    if answer.get("ok") is not True:
        raise RuntimeError("CAD690 native RPC refused: " + str(answer.get("error")))
    return answer["result"]


def hold(kickoff, phase):
    prefix = STATE / ("context-" + phase + "-" + kickoff["run_id"])
    prefix.with_suffix(".held").write_text("held")
    release = prefix.with_suffix(".release")
    deadline = time.monotonic() + 40
    while not release.exists():
        if time.monotonic() >= deadline:
            raise RuntimeError("context provider turn was not released")
        time.sleep(0.02)


def source_text(kickoff):
    values = [line[len("CONTEXT_SOURCE="):] for line in kickoff["instruction"].splitlines()
              if line.startswith("CONTEXT_SOURCE=")]
    if len(values) != 1:
        raise RuntimeError("exactly one actual source canary required")
    return "Context draft: " + values[0]


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
    path = STATE / "agents" / ALIAS / "app-context-receipts.jsonl"
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
        draft = source_text(kickoff)
        if (STATE / ("context-hold-writer-" + kickoff["run_id"])).exists():
            hold(kickoff, "writer")
        result = dict(
            common,
            outcome="succeeded",
            artifacts=[{"media_type": "text/markdown", "text": draft}],
        )
        receipt["artifact_sha256"] = "sha256:" + hashlib.sha256(draft.encode()).hexdigest()
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
        ):
            raise RuntimeError("actual fetched artifact does not match its pinned receipt")
        probe_path = STATE / ("context-probe-" + kickoff["run_id"] + ".json")
        if probe_path.exists():
            probe = json.loads(probe_path.read_text())
            cases = []
            for detached in [False, True]:
                own = frame("app_run_artifact", {"artifact_id": artifact["id"],
                    "message": turn["id"], "token": turn["turn_id"]}, detached)
                if own.get("ok") is not True or own["result"]["digest"] != actual_digest:
                    raise RuntimeError("genuine B dependency positive failed")
                calls = [
                    ("app_run_artifact", {"artifact_id": probe["artifact_id"],
                        "message": turn["id"], "token": turn["turn_id"]}),
                    ("app_run_artifact", {"artifact_id": probe["artifact_id"],
                        "message": turn["id"], "token": turn["turn_id"],
                        "context_id": probe["context_id"], "operator": True}),
                    ("app_context_show", {"install_id": probe["install_id"],
                        "context_id": probe["context_id"]}),
                    ("app_run_show", {"run_id": probe["run_id"]}),
                ]
                for method, params in calls:
                    answer = frame(method, params, detached)
                    cases.append({"detached": detached, "method": method,
                                  "ok": answer.get("ok"),
                                  "error_kind": answer.get("error", {}).get("kind")})
                    if answer.get("ok") is not False or answer.get("error", {}).get("kind") != "rejected" or "result" in answer:
                        raise RuntimeError("cross-context native read was not rejected")
            receipt["native_scope_cases"] = cases
            receipt["own_dependency_positive"] = True
        result = dict(
            common,
            producer_step_id=dependency["producer_step_id"],
            producer_revision=dependency["revision"],
            artifact_sha256=actual_digest,
            decision="approve",
            rationale="The fetched context draft retains its exact source canary.",
        )
        receipt.update(artifact_sha256=actual_digest, dependency_fetched=True)
        if (STATE / ("context-hold-reviewer-" + kickoff["run_id"])).exists():
            hold(kickoff, "reviewer")
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
    (STATE / ("context-result-emitted-" + kickoff["run_id"] + "-" + kickoff["step_id"])).write_text("emitted")


pi.run_prompt = run_prompt
pi.main()
