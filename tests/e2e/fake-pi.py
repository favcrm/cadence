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
- `model-drift`: `set_model` acks success but the next get_state
  reports `fake/fell-back` — the mid-session silent-fallback shape the
  /model gate's verification exists to catch (CAD-551's set_model is
  held to open()'s rule).

- `gateway-cap` (CAD-1076): mirrors the AgenticOS gateway's request
  schema, whose `messages` array is capped (100 there, GATEWAY_CAP
  here). The fake counts what Pi would send — the system prompt, each
  prompt, each assistant message and each tool result — and a model
  call over the cap ends the turn with the gateway's exact 400, before
  any work, until `new_session` resets the history. A prompt with
  "run empty tool" runs a tool whose result has no content (the Demo
  "(no output)" shape). `gateway-always` refuses every model call;
  `gateway-stuck` also refuses `new_session`.
  In any mode, a prompt containing `fake-fail <key>` ends with the
  provider error FAIL_SHAPES[key] (a 429, a 5xx, a credential error, the
  exact gateway 400) — the shapes a session reset must tell apart.

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
adapter's `get_state` verification is exercised. The CAD-551 session
verbs are here too: `get_available_models`, `get_session_stats`
(contextUsage), `compact`, `new_session` (a fresh sessionId), and a
`set_model` that actually changes what `get_state` reports.

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
        # Only a boolean is recorded: prove the private config was selected
        # without logging inherited environment values or credentials.
        expected = os.path.join(os.environ.get("CADENCE_STATE_DIR", ""), "master", "pi")
        with open(os.path.join(os.getcwd(), "pi-private-config.json"), "w") as f:
            json.dump(os.environ.get("PI_CODING_AGENT_DIR") == expected, f)
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


# The pi-devin catalog cache (CAD-570): real pi-devin keeps its model
# catalog at ${XDG_CACHE_HOME:-~/.cache}/pi-devin/models.json. Writing
# it exactly there exercises the agent's private XDG_CACHE_HOME — a
# confined agent whose var is unset or pointed at the operator's
# ~/.cache gets EACCES, which lands on stderr (the provider log).
# `catalog-cache` mode turns the EACCES into a hard exit(3) so a
# missing private dir fails the open rather than passing unasserted.
def write_catalog_cache():
    cache = os.environ.get("XDG_CACHE_HOME") or os.path.join(
        os.environ.get("HOME", "/"), ".cache"
    )
    path = os.path.join(cache, "pi-devin")
    try:
        os.makedirs(path, exist_ok=True)
        with open(os.path.join(path, "models.json"), "w") as f:
            json.dump({"models": [], "fetched": True}, f)
    except OSError as e:
        sys.stderr.write(
            "Devin: failed to read model catalog cache: %s %s/pi-devin/models.json\n"
            % (e, cache)
        )
        sys.stderr.flush()
        if MODE == "catalog-cache":
            sys.exit(3)


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


# The request journal: one `{"rpc": <type>}` line per request that
# actually crossed the wire — the CAD-559 /model gate tests assert a
# refused verb never reached the provider. Same dir contract as
# record_launch: masters journal beside pi-argv.json in cwd, every
# other alias under `<CADENCE_STATE_DIR>/agents/`.
def record_rpc(rtype):
    if not rtype:
        return
    line = json.dumps({"rpc": rtype}) + "\n"
    if ALIAS == "master":
        try:
            with open(os.path.join(os.getcwd(), "pi-rpc.jsonl"), "a") as f:
                f.write(line)
        except OSError:
            pass
    elif ALIAS and os.environ.get("CADENCE_STATE_DIR"):
        agents = os.path.join(os.environ["CADENCE_STATE_DIR"], "agents")
        try:
            os.makedirs(agents, exist_ok=True)
            with open(os.path.join(agents, "pi-rpc-%s.jsonl" % ALIAS), "a") as f:
                f.write(line)
        except OSError:
            # Confined: only `agents/<alias>` is writable.
            try:
                with open(os.path.join(agents, ALIAS, "pi-rpc.jsonl"), "a") as f:
                    f.write(line)
            except OSError:
                pass


record_launch()
write_catalog_cache()
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
    if MODE.startswith("cursor-effort-"):
        return {"id": "grok-4.7-high", "provider": "cursor"}
    if MODE == "bare-open-missing-provider":
        return {"id": "model-1"}
    if MODE == "bare-open-empty-provider":
        return {"id": "model-1", "provider": ""}
    if MODE == "bare-open-missing-id":
        return {"name": "model-1", "provider": "fake"}
    if MODE == "bare-open-empty-id":
        return {"id": "", "name": "model-1", "provider": "fake"}
    if MODE == "cursor-bare":
        return {"id": "grok-4.7-high", "name": "Cursor", "provider": "cursor"}
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
MODELS = [
    {"id": "model-1", "name": "Fake Model", "provider": "fake",
     "contextWindow": 200000},
    {"id": "model-2", "name": "Fake Model Small", "provider": "fake",
     "contextWindow": 64000},
    {"id": "claude-sonnet-4", "name": "Claude Sonnet 4",
     "provider": "anthropic", "contextWindow": 200000},
    # On the suite's [pi].models.allow — the allowed `/model` target in
    # the CAD-559 gate tests.
    {"id": "demo-1", "name": "Acme Demo", "provider": "acme",
     "contextWindow": 200000},
]
usage = {"tokens": 42000, "contextWindow": 200000}
GATEWAY_CAP = {"gateway-cap": 12, "gateway-always": 0, "gateway-stuck": 0}.get(MODE)
GATEWAY_400 = ('400 {"message":"Unsupported or malformed text request.",'
               '"type":"invalid_request_error","code":"invalid_request"}')
history = 1  # the system prompt
FAIL_SHAPES = {
    "429": '429 {"error":{"message":"Rate limited","type":"rate_limit_error","code":"rate_limited"}}',
    "502": '502 {"error":{"message":"The model provider could not complete the call.","type":"server_error","code":"upstream_unavailable"}}',
    "auth": "No API key found for agenticos. Use /login to sign in.",
    "gateway400": GATEWAY_400,
    "gateway400colon": GATEWAY_400.replace("400 ", "400: ", 1),
    # Both substrings, neither the gateway's status nor its code.
    "loose400": '400 {"message":"invalid_request in tool schema","code":"invalid_tools"}',
    "invalid500": '500 {"error":{"message":"x","code":"invalid_request"}}',
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


def assistant_message(text, stop="stop", error=None):
    message = {
        "role": "assistant",
        "content": [{"type": "text", "text": text}],
        "stopReason": stop,
        "usage": {"input": 1, "output": 1, "totalTokens": 2},
    }
    if error is not None:
        message["errorMessage"] = error
    return message


def finish_turn(stop="stop", text="", error=None):
    message = assistant_message(text, stop, error)
    emit({"type": "message_end", "message": message})
    emit({"type": "turn_end", "message": message, "toolResults": []})
    emit({"type": "agent_end", "messages": [], "willRetry": False})
    emit({"type": "agent_settled"})


def gateway_refused():
    """One model call: refused with the gateway's 400 when the history
    it would carry is over the cap (CAD-1076)."""
    global live_turn
    if GATEWAY_CAP is None or history <= GATEWAY_CAP:
        return False
    live_turn = False
    finish_turn("error", "", GATEWAY_400)
    return True


def run_prompt(message):
    global live_turn, history
    live_turn = True
    history += 1
    emit({"type": "agent_start"})
    emit({"type": "turn_start"})
    emit({"type": "message_start", "message": {"role": "assistant"}})
    reply = "fake-pi reply: " + message.strip().splitlines()[-1][:80]
    if MODE == "echo-all":
        # CAD-1009: the whole provider prompt, so a test can read what
        # the daemon actually delivered (envelope lines included).
        reply = "fake-pi prompt: " + message
    slow = MODE == "slow"  # CAD-551: a visible turn for the working row
    # Exact token after "fake-fail ": no key may shadow another.
    words = message.split()
    for i, word in enumerate(words[:-1]):
        if word == "fake-fail" and words[i + 1] in FAIL_SHAPES:
            live_turn = False
            finish_turn("error", "", FAIL_SHAPES[words[i + 1]])
            return
    if gateway_refused():
        return
    if "run empty tool" in message:
        emit({"type": "tool_execution_start", "toolCallId": "call_e",
              "toolName": "bash", "args": {"command": "cadence status"}})
        emit({"type": "tool_execution_end", "toolCallId": "call_e",
              "toolName": "bash", "result": {"content": []}, "isError": False})
        history += 2
        if gateway_refused():
            return
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
        if slow:
            time.sleep(1.6)
        emit({
            "type": "tool_execution_end",
            "toolCallId": "call_1",
            "toolName": "bash",
            "result": {"content": [{"type": "text", "text": "ok: 1 agent"}]},
            "isError": False,
        })
        history += 2
        if gateway_refused():
            return
    for chunk in [reply[: len(reply) // 2], reply[len(reply) // 2 :]]:
        emit({
            "type": "message_update",
            "usage": {},
            "assistantMessageEvent": {"type": "text_delta", "contentIndex": 0, "delta": chunk},
        })
    if MODE == "hang":
        return  # stays live but never settles — the caller must abort
    if slow:
        time.sleep(1.6)
    live_turn = False
    history += 1
    finish_turn("stop", reply)


def main():
    global pending_dialog, history
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
        record_rpc(rtype)
        if rtype == "extension_ui_response":
            if pending_dialog and req.get("id") == pending_dialog:
                pending_dialog = None
            continue
        if rtype == "get_state":
            if MODE == "cursor-effort-state-error":
                respond(rid, "get_state", False, error="synthetic state failure")
                continue
            respond(rid, "get_state", True, data=dict(state))
        elif rtype == "set_thinking_level":
            if MODE == "cursor-effort-refusal":
                respond(rid, "set_thinking_level", False, error="synthetic effort refusal")
                continue
            if MODE == "cursor-effort-mismatch":
                respond(rid, "set_thinking_level", True)
                continue
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
            provider = req.get("provider", "fake")
            model_id = req.get("modelId", "model-1")
            if MODE == "cursor-switch-drift":
                state["model"] = {"id": "grok-4.7-high",
                                  "provider": "cursor",
                                  "name": "Unsafe Cursor fallback"}
            elif MODE == "model-switch-unverified":
                # No actual namespace is available to classify.
                state["model"] = {"id": provider + "/" + model_id}
            elif MODE == "model-switch-empty-provider":
                state["model"] = {"id": provider + "/" + model_id,
                                  "provider": ""}
            elif MODE == "model-switch-missing-id":
                state["model"] = {"name": provider + "/" + model_id,
                                  "provider": provider}
            elif MODE == "model-switch-empty-id":
                state["model"] = {"id": "", "provider": provider}
            elif MODE == "model-switch-full-id":
                state["model"] = {"id": provider + "/" + model_id,
                                  "provider": "fake"}
            elif MODE == "model-drift":
                # Acks then reports something else — Pi's silent
                # fallback, mid-session.
                state["model"] = {"id": "fell-back",
                                  "provider": "fake",
                                  "name": "Fell Back"}
            else:
                known = next(
                    (m for m in MODELS
                     if m["provider"] == provider and m["id"] == model_id),
                    None)
                state["model"] = {
                    "id": model_id,
                    "provider": provider,
                    "name": (known or {}).get("name", model_id),
                }
            respond(rid, "set_model", True, data=dict(state["model"]))
        elif rtype == "get_available_models":
            respond(rid, "get_available_models", True,
                    data={"models": [dict(m) for m in MODELS]})
        elif rtype == "get_session_stats":
            respond(rid, "get_session_stats", True, data={
                "sessionId": state["sessionId"],
                "contextUsage": {
                    "tokens": usage["tokens"],
                    "contextWindow": usage["contextWindow"],
                    "percent": round(
                        usage["tokens"] / usage["contextWindow"] * 100, 1),
                },
            })
        elif rtype == "compact":
            usage["tokens"] = 8000
            respond(rid, "compact", True,
                    data={"compacted": True, "tokensAfter": usage["tokens"]})
        elif rtype == "new_session":
            if MODE == "gateway-stuck":
                respond(rid, "new_session", False, error="synthetic new_session failure")
                continue
            history = 1
            state["sessionId"] = "fakepi-session-{}-{}".format(
                os.getpid(), int(time.time()))
            respond(rid, "new_session", True,
                    data={"sessionId": state["sessionId"]})
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
