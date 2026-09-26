#!/usr/bin/env python3
"""A scripted stand-in for `pi --mode rpc` (CAD-322 slice-1 tests).

The adapter launches it exactly like the real CLI through
`CADENCE_PI_COMMAND` (`python3 tests/e2e/fake-pi.py [mode]`) and speaks
Pi's line protocol: commands in as `{"id","type",...}`, responses out as
`{"id","type":"response","command",...,"success":...}`, and asynchronous
events as bare `{"type":...}` lines — deliberately NOT JSON-RPC.

Modes (argv[1]):

- (default) a `prompt` turn emits agent_start, turn_start,
  message_start, text_delta updates, assistant message_end (echoes the
  prompt tail), turn_end, agent_end{willRetry:false}, agent_settled.
  A prompt containing "run tool" adds a tool_execution_start /
  tool_execution_update / tool_execution_end cycle for
  `bash cadence status` before the text tail.
- `no-credits`: prompt responses are `success:false` +
  "No API key found ... /login", like real Pi without credentials.
- `crash`: exits(2) right after accepting a prompt — the adapter must
  observe a dead process, not hang.
- `hang`: accepts the prompt, streams deltas, then never settles — the
  caller must abort (the turn then ends with stopReason "aborted").
- `linger`: normal turns, but on stdin EOF the process forks — the
  parent exits while a grandchild keeps the stdout pipe open ~1.5 s,
  so the reader's EOF lands well after `close()` returned (the I4
  reopen race).
- `dialog`: emits a blocking `confirm` extension_ui_request at startup;
  the adapter must auto-cancel it (extension_ui_response, cancelled).
- `wrong-model`: accepts `--model` but get_state reports a DIFFERENT
  model — the silent-fallback shape CAD-559 exists to catch, so the
  adapter must refuse the launch.

When CADENCE_ALIAS is `master` the fake also records what the launch
actually delivered — `pi-argv.json` (sys.argv tail, i.e. every flag the
adapter put on the provider) and `pi-env.json` (sorted environment
NAMES only, never values) — written into its cwd, which for a master is
`<state>/master/cwd` (writable under confinement). For any other alias
(a CAD-544 worker) the same record lands in
`<CADENCE_STATE_DIR>/agents/pi-record-<alias>.json` (argv + env NAMES).

`--session <path>` (workers): if the file exists its `sessionId` and
prompt count are resumed — the same id returns from `get_state` and
each `prompt` appends a line and bumps the count. Otherwise the fake
mints a session id and writes the header immediately (real Pi writes
sessions lazily; eager is fine for a fake — the file then exists for
the next open to resume).

`abort` ends a live turn with the aborted tail then responds — it is
idempotent when idle, like real Pi. `get_state` reports
sessionId/model/thinkingLevel; `set_thinking_level` honours only real
levels (bogus silently falls back to "off", like real Pi) so the
adapter's `get_state` verification is exercised.

`--model <provider/id>`: the reported model echoes the request, split
the way real Pi reports it — `{"provider": <first segment>, "id":
<rest>}` — so `provider/id` round-trips through get_state. No `--model`
means the fake's own `fake/model-1`, like Pi's silent default.
"""

import json
import os
import sys
import time

LEVELS = ["off", "minimal", "low", "medium", "high", "xhigh", "max"]
MODE = sys.argv[1] if len(sys.argv) > 1 and not sys.argv[1].startswith("-") else "normal"
ALIAS = os.environ.get("CADENCE_ALIAS", "")

# The launch record: the argv tail and the env NAMES the child actually
# received (values are never written — some are credentials). The
# master's record lands in its cwd; every other alias's lands beside
# the provider logs under `<state>/agents/` (a worker's cwd is a repo
# checkout the test does not own). A Landlock-confined worker (CAD-556)
# cannot write that sibling-of-its-own-dir path — the record then lands
# inside its granted dir, `<state>/agents/<alias>/pi-record.json`.
def record_launch():
    # Raw argv tail — the mode word sits first when one was given, same
    # shape the pi_master tests already assert on.
    argv_tail = sys.argv[1:]
    if ALIAS == "master":
        with open(os.path.join(os.getcwd(), "pi-argv.json"), "w") as f:
            json.dump(argv_tail, f)
        with open(os.path.join(os.getcwd(), "pi-env.json"), "w") as f:
            json.dump(sorted(os.environ), f)
    elif ALIAS and os.environ.get("CADENCE_STATE_DIR"):
        payload = json.dumps({"argv": argv_tail, "env": sorted(os.environ)})
        agents = os.path.join(os.environ["CADENCE_STATE_DIR"], "agents")
        try:
            with open(os.path.join(agents, "pi-record-%s.json" % ALIAS), "w") as f:
                f.write(payload)
        except OSError:
            # Confined: only `agents/<alias>` is writable.
            with open(os.path.join(agents, ALIAS, "pi-record.json"), "w") as f:
                f.write(payload)


# `--session <path>`: an existing file resumes its stored session id
# and prompt count; a missing file mints a fresh session and writes the
# header now, so a later open resumes it.
def load_session():
    args = sys.argv[1:]
    if "--session" not in args:
        return None, 0
    path = args[args.index("--session") + 1]
    try:
        with open(path) as f:
            lines = [json.loads(line) for line in f if line.strip()]
        session = next(
            (row.get("id") for row in lines if row.get("type") == "session"), None
        )
        prompts = sum(1 for row in lines if row.get("type") == "prompt")
        return session, prompts
    except (OSError, ValueError):
        pass
    return None, 0


record_launch()
SESSION_FILE, PROMPTS_SEEN, _sid = None, 0, None
_args = sys.argv[1:]
if "--session" in _args:
    SESSION_FILE = _args[_args.index("--session") + 1]
    _sid, PROMPTS_SEEN = load_session()

# The reported model echoes `--model p/rest` the way real Pi reports it
# (provider = first segment, id = the rest, inner slashes kept). In
# `wrong-model` mode the report deliberately disagrees — the silent
# fallback CAD-559 makes loud.
def reported_model():
    if MODE == "wrong-model":
        return {"id": "not-the-asked-1", "name": "Wrong Model", "provider": "fake"}
    if "--model" in _args:
        want = _args[_args.index("--model") + 1]
        provider, _, mid = want.partition("/")
        if mid:
            return {"id": mid, "name": want, "provider": provider}
        return {"id": want, "name": want, "provider": "fake"}
    return {"id": "fake/model-1", "name": "Fake Model", "provider": "fake"}


state = {
    "sessionId": _sid or "fakepi-session-" + str(os.getpid()),
    "model": reported_model(),
    "thinkingLevel": "medium",
    "isStreaming": False,
    "pendingMessageCount": 0,
    "sessionFile": SESSION_FILE,
    "promptsSeen": PROMPTS_SEEN,
}
live_turn = False
pending_dialog = None


def append_session(row):
    if not SESSION_FILE:
        return
    with open(SESSION_FILE, "a") as f:
        f.write(json.dumps(row) + "\n")


if SESSION_FILE and not os.path.exists(SESSION_FILE):
    append_session({
        "type": "session",
        "version": 3,
        "id": state["sessionId"],
        "cwd": os.getcwd(),
    })


def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def respond(req_id, command, success, data=None, error=None):
    out = {"type": "response", "command": command, "success": success}
    if req_id is not None:
        out["id"] = req_id
    if data is not None:
        out["data"] = data
    if error is not None:
        out["error"] = error
    emit(out)


def assistant_message(text, stop="stop"):
    return {
        "role": "assistant",
        "content": [{"type": "text", "text": text}],
        "stopReason": stop,
        "usage": {"input": 1, "output": 1, "totalTokens": 2},
    }


def finish_turn(stop="stop", text=""):
    emit({"type": "message_end", "message": assistant_message(text, stop)})
    emit({"type": "turn_end", "message": assistant_message(text, stop), "toolResults": []})
    emit({"type": "agent_end", "messages": [], "willRetry": False})
    emit({"type": "agent_settled"})


def run_prompt(message):
    global live_turn
    live_turn = True
    emit({"type": "agent_start"})
    emit({"type": "turn_start"})
    emit({"type": "message_start", "message": {"role": "assistant"}})
    reply = "fake-pi reply: " + message.strip().splitlines()[-1][:80]
    if "run tool" in message:
        emit({
            "type": "tool_execution_start",
            "toolCallId": "call_1",
            "toolName": "bash",
            "args": {"command": "cadence status"},
        })
        emit({
            "type": "tool_execution_update",
            "toolCallId": "call_1",
            "toolName": "bash",
            "args": {"command": "cadence status"},
            "partialResult": {"content": [{"type": "text", "text": "partial"}]},
        })
        emit({
            "type": "tool_execution_end",
            "toolCallId": "call_1",
            "toolName": "bash",
            "result": {"content": [{"type": "text", "text": "ok: 1 agent"}]},
            "isError": False,
        })
    for chunk in [reply[: len(reply) // 2], reply[len(reply) // 2 :]]:
        emit({
            "type": "message_update",
            "usage": {},
            "assistantMessageEvent": {"type": "text_delta", "contentIndex": 0, "delta": chunk},
        })
    if MODE == "hang":
        return  # stays live but never settles — the caller must abort
    live_turn = False
    finish_turn("stop", reply)


def main():
    global pending_dialog
    if MODE == "dialog":
        pending_dialog = "dlg-1"
        emit({
            "type": "extension_ui_request",
            "id": "dlg-1",
            "method": "confirm",
            "title": "Proceed?",
        })
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except ValueError:
            continue
        rid = req.get("id")
        rtype = req.get("type")
        if rtype == "extension_ui_response":
            if pending_dialog and req.get("id") == pending_dialog:
                pending_dialog = None
            continue
        if rtype == "get_state":
            respond(rid, "get_state", True, data=dict(state))
        elif rtype == "set_thinking_level":
            level = req.get("level")
            if level in LEVELS:
                state["thinkingLevel"] = level
            else:
                state["thinkingLevel"] = "off"  # real Pi silently falls back
            respond(rid, "set_thinking_level", True)
        elif rtype == "get_available_thinking_levels":
            respond(rid, "get_available_thinking_levels", True,
                    data={"levels": list(LEVELS)})
        elif rtype == "set_model":
            respond(rid, "set_model", True,
                    data={"id": req.get("modelId", "fake/model-1")})
        elif rtype == "prompt":
            if MODE == "no-credits":
                respond(rid, "prompt", False,
                        error="No API key found for the selected model. "
                              "Use /login to sign in.")
                continue
            respond(rid, "prompt", True)
            if MODE == "crash":
                os._exit(2)
            state["promptsSeen"] += 1
            append_session({"type": "prompt", "message": req.get("message", "")})
            run_prompt(req.get("message", ""))
        elif rtype == "abort":
            if live_turn:
                finish_turn("aborted")
            respond(rid, "abort", True)
        else:
            respond(rid, rtype or "unknown", True)
    # stdin hit EOF (the adapter's close_stdin). `linger` leaves a
    # grandchild holding stdout open ~1.5 s after the parent exits —
    # the reader's EOF then lands inside a reopened generation's
    # lifetime (I4); other modes die at once. The grandchild writes a
    # marker file the instant before it lets the pipe go, so the test
    # can wait for the stale EOF deterministically instead of racing
    # a fixed sleep.
    if MODE == "linger" and os.fork() == 0:
        time.sleep(1.5)
        try:
            open(os.path.join(os.environ["CADENCE_STATE_DIR"], "linger-eof"), "w").close()
        except OSError:
            pass
    os._exit(0)


if __name__ == "__main__":
    main()
