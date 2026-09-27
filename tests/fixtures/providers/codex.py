
import json, os, sys, threading, time
pidfile, mode = sys.argv[1], sys.argv[2]
turn_count = 0
# interrupt-text / interrupt-tool (CAD-323): the turn in flight, waiting
# for `turn/interrupt` — every interrupt lands in <pidfile>.interrupts.
# hold: turn t-<n> completes once <pidfile>.release-t-<n> exists.
in_flight = None
with open(pidfile, "w") as f:
    f.write(str(os.getpid()))
emit_lock = threading.Lock()
def emit(msg):
    with emit_lock:
        sys.stdout.write(json.dumps(msg) + "\n"); sys.stdout.flush()
for line in sys.stdin:
    try: msg = json.loads(line)
    except Exception: continue
    mid, method = msg.get("id"), msg.get("method")
    if mid is None: continue
    if method == "initialize":
        if mode == "slow-init": time.sleep(30)
        emit({"id": mid, "result": {"serverInfo": {"name": "mock", "version": "0"}}})
    elif method == "model/list":
        # Metadata-only response: configured Codex tests can exercise the
        # pair validator without making a paid model turn.
        if mode == "bad-model-list":
            emit({"id": mid, "result": {}})
        else:
            emit({"id": mid, "result": {"data": [
                {"id": "gpt-5.6-luna", "model": "gpt-5.6-luna",
                 "isDefault": False,
                 "supportedReasoningEfforts": [
                     {"reasoningEffort": "low"},
                     {"reasoningEffort": "medium"},
                     {"reasoningEffort": "high"},
                     {"reasoningEffort": "xhigh"},
                     {"reasoningEffort": "max"}]},
                {"id": "mock-model", "model": "mock-model",
                 "isDefault": True,
                 "supportedReasoningEfforts": [{"reasoningEffort": "medium"}]}
            ], "nextCursor": None}})
    elif method in ("thread/start", "thread/resume"):
        # Record the launch payload before answering so tests can read
        # exactly what reached the wire (<pidfile>.requests).
        with open(pidfile + ".requests", "a") as rf:
            rf.write(json.dumps({"method": method,
                                 "params": msg.get("params", {})}) + "\n")
        if mode == "bad-thread":
            emit({"id": mid, "result": {"thread": {}}})
        else:
            launch = msg.get("params", {})
            effort = launch.get("config", {}).get("model_reasoning_effort", "medium")
            model = launch.get("model", "mock-model")
            emit({"id": mid, "result": {"thread": {
                "id": "th-1", "sessionId": "s-1", "model": model,
                "reasoningEffort": effort},
                "model": model, "reasoningEffort": effort}})
    elif method == "account/rateLimits/read":
        if mode in ("no-quota", "quota-recover"):
            emit({"id": mid, "error": {"code": -32601,
                 "message": "rate limits unavailable in this auth mode"}})
        else:
            emit({"id": mid, "result": {
                "accountId": "acct-codex-test",
                "rateLimits": {
                    "primary": {"usedPercent": 23,
                                 "windowDurationMins": 60,
                                 "resetsAt": 1900000000},
                    "secondary": None},
                "rateLimitsByLimitId": {
                    "codex": {"usedPercent": 7,
                              "windowDurationMins": 10080,
                              "resetsAt": 1900100000}},
                "planType": "mock-pro"}})
    elif method == "turn/start":
        turn_count += 1
        if mode == "bad-turn":
            emit({"id": mid, "result": {"turn": {}}})
        elif mode == "die-after-start":
            emit({"id": mid, "result": {"turn": {"id": "t-1"}}}); os._exit(0)
        elif mode == "silent":
            emit({"id": mid, "result": {"turn": {"id": "t-1"}}})
        elif mode == "hold":
            tid = "t-%d" % turn_count
            emit({"id": mid, "result": {"turn": {"id": tid}}})
            in_flight = tid
            def finish(tid=tid):
                while not os.path.exists(pidfile + ".release-" + tid):
                    time.sleep(0.05)
                emit({"method": "turn/completed", "params": {"turn": {
                    "id": tid, "status": "completed", "items": [
                        {"id": "f-" + tid, "type": "agentMessage",
                         "text": "MOCK_OK", "phase": "final_answer"}]}}})
            threading.Thread(target=finish, daemon=True).start()
        elif mode in ("interrupt-text", "interrupt-tool") and turn_count == 1:
            # The first turn streams, then waits for turn/interrupt;
            # later turns complete like "ok".
            tid = "t-%d" % turn_count
            emit({"id": mid, "result": {"turn": {"id": tid}}})
            emit({"method": "item/completed", "params": {"turnId": tid, "item": {
                "id": "i0", "type": "agentMessage", "text": "working",
                "phase": "commentary"}}})
            if mode == "interrupt-tool":
                emit({"method": "item/started", "params": {"turnId": tid, "item": {
                    "id": "cmd-1", "type": "commandExecution",
                    "command": "sleep 300", "status": "inProgress"}}})
            in_flight = tid
        else:
            emit({"id": mid, "result": {"turn": {"id": "t-1"}}})
            def item(iid, text, phase=None):
                it = {"id": iid, "type": "agentMessage", "text": text}
                if phase:
                    it["phase"] = phase
                emit({"method": "item/completed",
                      "params": {"turnId": "t-1", "item": it}})
                return it
            if mode in ("items", "items-mixed"):
                # A commentary agentMessage persisted mid-turn (CAD-319).
                item("i0", "looking", "commentary")
            if mode == "items-mixed":
                # Unphased beside a final: Codex's result is the final
                # only, so the thread keeps this one (CAD-320).
                item("i0b", "an aside")
            if mode in ("items", "items-mixed"):
                # The final answer persists as an item too; the turn
                # result carries it, so the thread must not repeat it
                # (CAD-320).
                item("i1", "MOCK_OK", "final_answer")
            if mode == "items-unphased":
                # An older Codex: no phases, the result joins them all.
                done = [item("i0", "first"), item("i1", "second")]
                emit({"method": "turn/completed", "params": {"turn": {
                    "id": "t-1", "status": "completed", "items": done}}})
                continue
            if mode == "items-die":
                # Items persist, then the process dies: the turn is
                # unknown and its result empty — the thread keeps both.
                item("i0", "partial")
                item("i1", "almost there", "final_answer")
                os._exit(0)
            if mode == "heartbeat":
                # ~3.6s of streamed activity, never silent for long.
                for _ in range(12):
                    time.sleep(0.3)
                    emit({"method": "item/agentMessage/delta",
                          "params": {"turnId": "t-1", "delta": "."}})
            if mode in ("quota-update", "quota-recover"):
                if mode == "quota-recover":
                    update = {"accountId": "acct-recovered",
                              "rateLimits": {"primary": {"usedPercent": 42}}}
                elif turn_count == 1:
                    # First update explicitly clears nullable window fields;
                    # the account id is tested separately as a conservative
                    # identity field and must survive its explicit null.
                    update = {"accountId": None,
                              "rateLimits": {"primary": {
                                  "usedPercent": 42,
                                  "windowDurationMins": None,
                                  "resetsAt": None}}}
                else:
                    # The second update omits the nullable fields entirely.
                    # Omission must preserve their already-cleared state.
                    update = {"rateLimits": {"primary": {"usedPercent": 44}}}
                emit({"method": "account/rateLimits/updated", "params": update})
            emit({"method": "turn/completed", "params": {"turn": {
                "id": "t-1", "status": "completed", "items": [
                    {"id": "i1", "type": "agentMessage",
                     "text": "MOCK_OK", "phase": "final_answer"}]}}})
    elif method == "turn/interrupt":
        p = msg.get("params", {})
        with open(pidfile + ".interrupts", "a") as f:
            f.write(json.dumps(p) + "\n")
        if in_flight is None or p.get("turnId") != in_flight:
            emit({"id": mid, "error": {"code": -32600,
                  "message": "no active turn to interrupt"}})
            continue
        emit({"id": mid, "result": {}})
        items = []
        if mode == "interrupt-tool":
            # The killed command completes with its partial output.
            cmd = {"id": "cmd-1", "type": "commandExecution",
                   "command": "sleep 300", "status": "failed",
                   "aggregatedOutput": "partial line 1\n", "exitCode": None}
            emit({"method": "item/completed",
                  "params": {"turnId": in_flight, "item": cmd}})
            items.append(cmd)
        emit({"method": "turn/completed", "params": {"turn": {
            "id": in_flight, "status": "interrupted", "items": items,
            "error": None}}})
        in_flight = None
