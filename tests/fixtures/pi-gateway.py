#!/usr/bin/env python3
"""CAD-1076: the smallest `pi --mode rpc` stand-in for a master whose
history the gateway refuses. Every request type lands in
`pi-rpc.jsonl` in its cwd; a prompt is journaled with its marker.

A prompt carrying REFUSED gets the gateway's 400 until `new_session`;
TOOL-REFUSED runs one tool first; ALWAYS-REFUSED is refused every time.
Any other prompt is answered.
"""
import json
import sys

GATEWAY_400 = ('400 {"message":"Unsupported or malformed text request.",'
               '"type":"invalid_request_error","code":"invalid_request"}')
MARKERS = ("ALWAYS-REFUSED", "TOOL-REFUSED", "REFUSED")
args = sys.argv[1:]
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
                         "model": {"provider": provider, "id": model_id}}
    elif kind == "get_available_models":
        reply["data"] = {"models": [{"provider": provider, "id": model_id}]}
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
