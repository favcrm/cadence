#!/usr/bin/env python3
"""CAD-1076: the smallest `pi --mode rpc` stand-in for a master whose
history the gateway refuses. Every request type lands in
`pi-rpc.jsonl` in its cwd; a prompt is journaled with its marker.

A prompt carrying REFUSED gets the gateway's 400 until `new_session`;
TOOL-REFUSED runs one tool first; ALWAYS-REFUSED is refused every time.
Any other prompt is answered.

It also answers `get_available_models` with a catalog, the way real Pi
answers `success(id, "get_available_models", {models})` — the CAD-601
open guard cross-checks the reported model against that list. An
optional leading argv word before `--mode` selects the catalog reply:
`ok` (default) lists the requested model exactly as `get_state`
reports it; `forged` lists it but the get_state `thinkingLevelMap`
differs; `absent` lists a different id; `other_provider` lists it
under another provider; `empty` returns `models: []`; `missing`
returns no `models` key; `fail` answers `success: false`; `notlist`
returns a non-array `models`.
"""
import json
import sys

GATEWAY_400 = ('400 {"message":"Unsupported or malformed text request.",'
               '"type":"invalid_request_error","code":"invalid_request"}')
MARKERS = ("ALWAYS-REFUSED", "TOOL-REFUSED", "REFUSED")
MODES = ("ok", "forged", "absent", "other_provider", "empty", "missing", "fail", "notlist")
args = sys.argv[1:]
MODE = args.pop(0) if args and args[0] in MODES else "ok"
model = args[args.index("--model") + 1] if "--model" in args else "fake/model-1"
provider, _, model_id = model.partition("/")
session = 0


def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def journal(line):
    with open("pi-rpc.jsonl", "a") as f:
        f.write(json.dumps(line) + "\n")


def settle(text, error=None):
    message = {"role": "assistant", "content": [{"type": "text", "text": text}],
               "stopReason": "error" if error else "stop"}
    if error:
        message["errorMessage"] = error
    emit({"type": "message_end", "message": message})
    emit({"type": "agent_settled"})


for line in sys.stdin:
    req = json.loads(line)
    kind, rid = req.get("type"), req.get("id")
    reply = {"type": "response", "id": rid, "command": kind, "success": True}
    if kind == "get_state":
        reply["data"] = {"sessionId": "s%d" % session, "thinkingLevel": "medium",
                         "model": dict({"provider": provider, "id": model_id, "api": "devin-local", "baseUrl": "u",
                         "reasoning": True, "thinkingLevelMap": {"high": "swe-2-high"}},
                         **({"thinkingLevelMap": {"high": "claude-opus-5-max"}} if MODE == "forged" else {}))}
    elif kind == "get_available_models":
        good = {"provider": provider, "id": model_id, "api": "devin-local", "baseUrl": "u",
                "reasoning": True, "thinkingLevelMap": {"high": "swe-2-high"}}
        if MODE == "ok" or MODE == "forged":
            reply["data"] = {"models": [good]}
        elif MODE == "absent":
            reply["data"] = {"models": [dict(good, id="model-2")]}
        elif MODE == "other_provider":
            reply["data"] = {"models": [dict(good, provider="other")]}
        elif MODE == "empty":
            reply["data"] = {"models": []}
        elif MODE == "missing":
            reply["data"] = {}
        elif MODE == "fail":
            reply = {"type": "response", "id": rid, "command": kind, "success": False, "error": "boom"}
        elif MODE == "notlist":
            reply["data"] = {"models": {"x": good}}
    elif kind == "new_session":
        session += 1
        reply["data"] = {"sessionId": "s%d" % session}
    if kind != "prompt":
        journal({"rpc": kind})
        emit(reply)
        continue
    text = req.get("message", "")
    marker = next((m for m in MARKERS if m in text), None)
    journal({"rpc": "prompt", "marker": marker, "session": session})
    emit(reply)
    if marker == "TOOL-REFUSED":
        emit({"type": "tool_execution_start", "toolCallId": "t1",
              "toolName": "bash", "args": {"command": "true"}})
        journal({"rpc": "tool", "session": session})
    if marker == "ALWAYS-REFUSED" or (marker and session == 0):
        settle("", GATEWAY_400)
    else:
        settle("master answer in session %d" % session)
