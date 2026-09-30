#!/usr/bin/env python3
"""Deterministic CAD692 release worker using the real Pi transport and daemon socket.

Reuse the existing protocol fixture rather than emulate daemon completion.
No caller assertion or turn token is invented, and no token is journalled.
"""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shlex
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



def frame(method, params, detached=False, timeout=5):
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
        connection.settimeout(timeout)
        connection.connect(str(STATE / "cadence.sock"))
        connection.sendall((wire + "\n").encode())
        return json.loads(connection.makefile("rb").readline())


def pi_bash_frame(method, params, nested_detach=False):
    """Exercise Pi's real Linux bash-tool topology, not a direct provider RPC.

    Pi spawns /bin/bash as a direct child with detached=true. Its shell is a
    session leader; the cadence CLI (represented by this socket peer) remains
    in that shell session. An extra setsid inside the tool must still refuse.
    """
    wire = json.dumps({"method": method, "params": params})
    code = ("import socket,sys; s=socket.socket(socket.AF_UNIX);"
            "s.settimeout(5); s.connect(sys.argv[1]);"
            "s.sendall(sys.stdin.buffer.read()+b'\\n');"
            "print(s.makefile().readline())")
    command = " ".join(["python3", "-c", shlex.quote(code),
                        shlex.quote(str(STATE / "cadence.sock"))])
    if nested_detach:
        command = "setsid " + command
    # The trailing ':' keeps bash alive as the verified session leader until
    # the socket caller exits; bash otherwise may exec its final command.
    child = subprocess.run(["/bin/bash", "-c", command + "; :"],
                           input=wire, text=True, capture_output=True,
                           start_new_session=True, timeout=10)
    if child.returncode != 0:
        raise RuntimeError("Pi bash-tool native probe failed: " + child.stderr)
    return json.loads(child.stdout)


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
    source = values[0]
    # Generate large artifacts through a real producer turn from short valid
    # workflow inputs. Do not widen input bounds to reach the release preview.
    if source == "REPEAT_UTF8_E_5000":
        source = "é" * 5000
    elif source == "REPEAT_ASCII_X_17408":
        source = "x" * 17408
    return "Context draft: " + source


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
    path = STATE / "agents" / ALIAS / "app-release-receipts.jsonl"
    with path.open("a") as output:
        output.write(json.dumps(row, sort_keys=True) + "\n")


def run_prompt(prompt):
    pi.live_turn = True
    pi.emit({"type": "agent_start"})
    pi.emit({"type": "turn_start"})
    pi.emit({"type": "message_start", "message": {"role": "assistant"}})
    kickoff = kickoff_from_prompt(prompt)
    turn = active_turn(kickoff)
    social_probe_path = STATE / ("social-image-probe-" + kickoff["run_id"] + ".json")
    social_probe = json.loads(social_probe_path.read_text()) if social_probe_path.exists() else None
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
        capability_refused = False
        if social_probe is None:
            draft = source_text(kickoff)
        else:
            slot = social_probe["slot"]
            request = {"message": turn["id"], "token": turn["turn_id"],
                       "slot": slot, "request_id": "social-" + slot + "-once", "input": {}}
            first = frame("app_run_capability_call", request)
            if first.get("ok") is not True:
                capability_refused = True
                receipt["capability_refused"] = True
                draft = ""
            else:
                replay = frame("app_run_capability_call", request)
                if replay != first:
                    raise RuntimeError("social capability stable-key replay changed receipt")
                receipt = first["result"]
                (STATE / ("app-capability-result-" + kickoff["run_id"] + ".json")).write_text(
                    json.dumps(receipt))
                draft = ("我哋幫你跟進客戶，清晰記錄每一步。 #客戶關係" if slot == "image"
                         else "Source receipt " + receipt["id"])
        cap_probe = STATE / ("app-capability-probe-" + kickoff["run_id"] + ".json")
        if cap_probe.exists():
            from concurrent.futures import ThreadPoolExecutor
            probe = json.loads(cap_probe.read_text())
            request = {"message": turn["id"], "token": turn["turn_id"],
                       "slot": "source", "request_id": "source-once",
                       "input": {"source": "CONTEXT_SOURCE=" + probe["source"]}}
            if probe.get("slow_single_call"):
                # Real enrolled provider PID and active assigned turn, with no
                # client timeout masking the PM's 15-second contention window.
                replies = [frame("app_run_capability_call", request, timeout=40)]
                if replies[0].get("ok") is True:
                    # Replay only after completion, still requiring the exact
                    # same durable receipt and one adapter invocation.
                    replies.append(frame("app_run_capability_call", request))
            else:
                with ThreadPoolExecutor(max_workers=2) as pool:
                    replies = list(pool.map(lambda _: pi_bash_frame("app_run_capability_call", request), range(2)))
            if any(reply.get("ok") is not True for reply in replies):
                raise RuntimeError("valid concurrent capability calls failed: " + str(replies))
            if replies[0]["result"] != replies[1]["result"]:
                raise RuntimeError("concurrent calls did not return one durable receipt")
            receipt = replies[0]["result"]
            cases = []
            second_charge = frame("app_run_capability_call", dict(request, request_id="second-charge"))
            if second_charge.get("ok") is not False:
                raise RuntimeError("one approved slot allowed a second paid provider call")
            cases.append({"kind":"second-charge","refused":True})
            for changed in [
                dict(request, token="forged"),
                dict(request, context_id=probe["context_id"]),
                dict(request, run_id=kickoff["run_id"]),
                dict(request, slot="publication"),
                dict(request, input={"source": "OTHER_CLIENT"}),
            ]:
                answer = frame("app_run_capability_call", changed)
                if answer.get("ok") is not False:
                    raise RuntimeError("forged capability call was accepted")
                cases.append({"kind":"forged","refused":True})
            detached = frame("app_run_capability_call", request, True)
            if detached.get("ok") is not False:
                raise RuntimeError("detached child invoked a run capability")
            cases.append({"kind":"setsid","refused":True})
            nested_detached = pi_bash_frame("app_run_capability_call", request, True)
            if nested_detached.get("ok") is not False:
                raise RuntimeError("setsid child escaped Pi's bash-tool session")
            cases.append({"kind":"bash-tool-setsid","refused":True})
            quote_request = {"install_id":probe["install_id"],
                             "context_id":probe["context_id"],"slot":"source"}
            for detached in [False, True]:
                answer = frame("app_binding_quote", quote_request, detached)
                if answer.get("ok") is not False:
                    raise RuntimeError("agent or detached child fetched operator price quote")
                cases.append({"kind":"quote-operator-only","refused":True})
            if probe.get("other_receipt_id"):
                cross = frame("app_run_capability_result",
                    {"receipt_id":probe["other_receipt_id"],
                     "message":turn["id"],"token":turn["turn_id"]})
                if cross.get("ok") is not False:
                    raise RuntimeError("shared worker fetched another context receipt")
                cases.append({"kind":"cross-context","refused":True})
            receipt["probes"] = cases
            (STATE / ("app-capability-result-" + kickoff["run_id"] + ".json")).write_text(
                json.dumps(receipt))
        if (STATE / ("context-hold-writer-" + kickoff["run_id"])).exists():
            hold(kickoff, "writer")
        if capability_refused:
            result = dict(common, outcome="failed", artifacts=[])
        else:
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
        cap_result_path = STATE / ("app-capability-result-" + kickoff["run_id"] + ".json")
        if cap_result_path.exists():
            cap_receipt = json.loads(cap_result_path.read_text())
            exact = frame("app_run_capability_result", {
                "receipt_id":cap_receipt["id"], "message":turn["id"],
                "token":turn["turn_id"]})
            asset = frame("app_run_capability_asset", {
                "receipt_id":cap_receipt["id"], "message":turn["id"],
                "token":turn["turn_id"]})
            if exact.get("ok") is not True or exact["result"] != {
                key: value for key, value in cap_receipt.items() if key != "probes"}:
                raise RuntimeError("reviewer could not fetch exact dependency capability receipt")
            if asset.get("ok") is not True:
                raise RuntimeError("reviewer could not fetch exact dependency asset")
            import base64
            decoded = base64.b64decode(asset["result"]["base64"], validate=True)
            if ("sha256:" + hashlib.sha256(decoded).hexdigest()) != asset["result"]["digest"]:
                raise RuntimeError("reviewer asset bytes differ from durable digest")
            if social_probe and social_probe["slot"] == "image":
                import struct
                if (asset["result"]["media_type"] != "image/png"
                        or not decoded.startswith(b"\x89PNG\r\n\x1a\n")
                        or struct.unpack(">II", decoded[16:24]) != (1, 1)):
                    raise RuntimeError("reviewer image fixture is not the retained square PNG")
            if frame("app_run_capability_asset", {
                "receipt_id":cap_receipt["id"], "message":turn["id"],
                "token":turn["turn_id"]}, True).get("ok") is not False:
                raise RuntimeError("detached reviewer fetched capability asset")
        probe_path = STATE / ("app-release-probe-" + kickoff["run_id"] + ".json")
        if probe_path.exists():
            probe = json.loads(probe_path.read_text())
            cases = []
            for detached in [False, True]:
                for method, params, allowed in [
                    ("app_effect_show", {"effect_id": probe["effect_id"]}, False),
                    ("app_effect_list", {"install_id": probe["install_id"]}, False),
                    ("app_binding_show", {"install_id": probe["install_id"], "binding_id": probe["binding_id"]}, False),
                    ("app_binding_list", {"install_id": probe["install_id"]}, False),
                    ("platform_effects", {}, True),
                    ("agent_events", {"alias": ALIAS, "tail": True}, True),
                    ("agent_capture", {"alias": ALIAS}, False),
                ]:
                    answer = frame(method, params, detached)
                    if any(canary in json.dumps(answer) for canary in probe["canaries"]):
                        raise RuntimeError("current B reviewer inspected another context release material")
                    if allowed:
                        if answer.get("ok") is not True:
                            raise RuntimeError("actual reviewer generic read control failed")
                        if method == "agent_events" and not answer["result"]["events"]:
                            raise RuntimeError("actual shared reviewer history is not populated")
                    elif answer.get("ok") is not False or answer.get("error", {}).get("kind") != "rejected":
                        raise RuntimeError("actual B reviewer app release path not refused")
                    cases.append({"method": method, "detached": detached, "ok": answer.get("ok"), "redacted": True})
            receipt["native_release_probes"] = cases
        result = dict(
            common,
            producer_step_id=dependency["producer_step_id"],
            producer_revision=dependency["revision"],
            artifact_sha256=actual_digest,
            decision="approve",
            rationale=("The fetched caption and retained square PNG match the selected source and both digests."
                       if social_probe else "The fetched context draft retains its exact source canary."),
        )
        if cap_result_path.exists():
            result["asset_receipt_id"] = cap_receipt["id"]
            result["asset_sha256"] = asset["result"]["digest"]
        override_path = STATE / ("app-review-asset-override-" + kickoff["run_id"] + ".json")
        if override_path.exists():
            result.update(json.loads(override_path.read_text()))
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
