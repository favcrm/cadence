
import json, os, signal, subprocess, sys, time

pidfile = sys.argv[1]
# Mode travels in the pidfile basename — the daemon scrubs CADENCE_*
# from the child env, so an env var would never arrive.
mode = os.path.basename(pidfile).removeprefix("claude-").removesuffix(".pid")
argv = sys.argv[2:]
sid = ""
for i, a in enumerate(argv):
    if a in ("--session-id", "--resume") and i + 1 < len(argv):
        sid = argv[i + 1]
with open(pidfile, "w") as f:
    f.write(str(os.getpid()))
# Atomic dump — a reader between truncate and write must never see a
# torn file; the same temp+rename shape .env uses below.
argv_tmp = pidfile + ".argv.tmp"
with open(argv_tmp, "w") as f:
    f.write("\n".join(sys.argv))
os.rename(argv_tmp, pidfile + ".argv")
env_tmp = pidfile + ".env.tmp"
with open(env_tmp, "w") as f:
    for k in sorted(os.environ):
        f.write("%s=%s\n" % (k, os.environ[k]))
os.rename(env_tmp, pidfile + ".env")

count = [0]

def emit(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()

def result(**kw):
    count[0] += 1
    base = {"type": "result", "session_id": sid, "num_turns": 1,
            "total_cost_usd": 0.001, "result_index": count[0] - 1}
    base.update(kw)
    emit(base)

def init():
    # bad-session reports a session the process was NOT opened with.
    reported = "00000000-foreign-session" if current_mode() == "bad-session" else sid
    emit({"type": "system", "subtype": "init", "session_id": reported,
          "model": "mock-claude", "tools": []})

def on_sigint(signum, frame):
    with open(pidfile + ".sigint", "a") as f:
        f.write("SIGINT\n")
    in_flight[0] = None
    init()
    result(subtype="interrupted", is_error=False, result="INTERRUPTED",
           stop_reason="interrupted")

signal.signal(signal.SIGINT, on_sigint)

# The fixture path rides a sidecar like `.mode` — never the env, which
# every concurrent test's mock child would inherit.
fixture = open(pidfile + ".fixture").read().strip() if os.path.exists(pidfile + ".fixture") else None
fixture_lines = open(fixture).read().splitlines() if fixture else []

def current_mode():
    # <pidfile>.mode overrides the env mode per message — lets a test
    # switch a resumed provider from "die" to "ok".
    try:
        return open(pidfile + ".mode").read().strip()
    except FileNotFoundError:
        return mode

# ---- brokered permission flow (permit mode) ----
mcp_proc = None
mcp_next_id = [0]

def mcp_server():
    """Spawn the `--mcp-config` server once, like the real CLI: env
    from the config overlays ours, then initialize/initialized."""
    global mcp_proc
    if mcp_proc is not None:
        return mcp_proc
    cfg_path = None
    for i, a in enumerate(argv):
        if a == "--mcp-config" and i + 1 < len(argv):
            cfg_path = argv[i + 1]
    if cfg_path is None:
        return None
    srv = json.load(open(cfg_path))["mcpServers"]["cadence"]
    env = dict(os.environ)
    env.update(srv.get("env", {}))
    proc = subprocess.Popen([srv["command"]] + srv["args"],
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            env=env, text=True, bufsize=1)
    mcp_proc = proc
    mcp_rpc(proc, "initialize",
            {"protocolVersion": "2025-11-25", "capabilities": {},
             "clientInfo": {"name": "mock-claude", "version": "0"}})
    proc.stdin.write(json.dumps(
        {"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
    proc.stdin.flush()
    mcp_rpc(proc, "tools/list", {})
    return proc

def mcp_rpc(proc, method, params):
    mcp_next_id[0] += 1
    proc.stdin.write(json.dumps(
        {"jsonrpc": "2.0", "id": mcp_next_id[0],
         "method": method, "params": params}) + "\n")
    proc.stdin.flush()
    line = proc.stdout.readline()
    if not line:
        raise RuntimeError("mcp server exited")
    return json.loads(line)

def record_verdict(verdict):
    tmp = pidfile + ".verdict.tmp"
    with open(tmp, "w") as f:
        f.write(json.dumps(verdict))
    os.rename(tmp, pidfile + ".verdict")

def ask_permission(command_text):
    """One approve call — returns the verdict object the server put
    inside the text content block (allow/deny), or a local deny when
    the broker is unreachable."""
    proc = mcp_server()
    if proc is None:
        verdict = {"behavior": "deny",
                   "message": "no --mcp-config on argv"}
        record_verdict(verdict)
        return verdict
    try:
        resp = mcp_rpc(proc, "tools/call",
                       {"name": "approve",
                        "arguments": {"tool_name": "Bash",
                                      "input": {"command": command_text},
                                      "tool_use_id": "tu_permit_%d"
                                      % mcp_next_id[0]}})
        text = resp["result"]["content"][0]["text"]
        verdict = json.loads(text)
    except Exception as e:
        verdict = {"behavior": "deny", "message": "mcp call failed: %s" % e}
    record_verdict(verdict)
    return verdict

# The turn left waiting for an interrupt ("text" or "tool"), if any.
in_flight = [None]

def on_control(msg):
    with open(pidfile + ".controls", "a") as f:
        f.write(json.dumps(msg) + "\n")
    req = msg.get("request", {})
    emit({"type": "control_response",
          "response": {"subtype": "success",
                       "request_id": msg.get("request_id"), "response": {}}})
    if req.get("subtype") != "interrupt" or in_flight[0] is None:
        return  # an idle CLI acknowledges and does nothing
    kind, in_flight[0] = in_flight[0], None
    if kind == "tool":
        # The aborted tool's partial output, as the CLI records it.
        emit({"type": "user", "session_id": sid,
              "message": {"role": "user", "content": [
                  {"type": "tool_result", "tool_use_id": "tu_int",
                   "is_error": True,
                   "content": "partial line 1\n[Request interrupted by user for tool use]"}]}})
    result(subtype="error_during_execution", is_error=True,
           errors=["[Request interrupted by user]"], stop_reason=None,
           terminal_reason="aborted_tools" if kind == "tool" else "aborted_streaming")

for line in sys.stdin:
    try:
        msg = json.loads(line)
    except Exception:
        continue
    if msg.get("type") == "control_request":
        on_control(msg)
        continue
    if msg.get("type") != "user":
        continue
    mode_now = current_mode()
    content = msg["message"]["content"]
    text = content if isinstance(content, str) else \
        " ".join(b.get("text", "") for b in content)
    if mode_now == "die":
        os._exit(0)
    if mode_now == "replay":
        for raw in fixture_lines:
            try:
                ev = json.loads(raw)
            except Exception:
                continue
            if "session_id" in ev:
                ev["session_id"] = sid
            emit(ev)
        continue
    init()
    emit({"type": "assistant",
          "message": {"role": "assistant",
                      "content": [{"type": "text", "text": "working"}]},
          "session_id": sid})
    if mode_now == "text-die":
        os._exit(0)  # dies mid-turn, after a text block (CAD-320)
    if mode_now == "await-interrupt":
        in_flight[0] = "text"
        continue  # the interrupt (control request or SIGINT) ends it
    if mode_now == "interrupt-tool":
        emit({"type": "assistant", "session_id": sid,
              "message": {"role": "assistant", "content": [
                  {"type": "tool_use", "id": "tu_int", "name": "Bash",
                   "input": {"command": "sleep 300"}}]}})
        in_flight[0] = "tool"
        continue
    if mode_now == "hold":
        # The turn stays open until the test drops `<pidfile>.release`,
        # then completes like "ok" (CAD-162: ack mid-turn).
        while not os.path.exists(pidfile + ".release"):
            time.sleep(0.05)
    if mode_now == "silent":
        while True:
            time.sleep(5)  # alive but eventless — the idle fence path
    if mode_now == "chatty":
        while True:
            emit({"type": "assistant",
                  "message": {"role": "assistant",
                              "content": [{"type": "text", "text": "."}]},
                  "session_id": sid})
            time.sleep(0.3)
    if mode_now == "heartbeat":
        for _ in range(12):
            emit({"type": "assistant",
                  "message": {"role": "assistant",
                              "content": [{"type": "text", "text": "."}]},
                  "session_id": sid})
            time.sleep(0.3)
    if mode_now == "tooluse":
        emit({"type": "assistant",
              "message": {"role": "assistant",
                          "content": [{"type": "tool_use", "name": "Bash",
                                       "input": {"command": "true"}}]},
              "session_id": sid})
    if mode_now == "fail":
        result(subtype="error_during_execution", is_error=True,
               errors=["mock exploded"], stop_reason="error")
        continue
    if mode_now == "permit":
        # The real CLI blocks on the permission-prompt tool here — one
        # approve call per tool use; the verdict decides the outcome.
        verdict = ask_permission(text)
        if verdict.get("behavior") == "allow":
            result(subtype="success", is_error=False,
                   result="MOCK_OK:" + text, stop_reason="end_turn",
                   permission_denials=[])
        else:
            message = verdict.get("message", "denied")
            denials = [{"tool_name": "Bash",
                        "tool_use_id": "tu_permit",
                        "tool_input": {"command": text,
                                       "description":
                                           "CANARY-DENIED-INPUT-4e7f"},
                        "message": message}]
            result(subtype="success", is_error=False,
                   result="DENIED:" + message, stop_reason="end_turn",
                   permission_denials=denials)
        continue
    denials = []
    if mode_now == "deny":
        denials = [{"tool_name": "Bash", "tool_use_id": "tu_1",
                    "tool_input": {"command": "touch /tmp/x"}}]
    result(subtype="success", is_error=False, result="MOCK_OK:" + text,
           stop_reason="end_turn", permission_denials=denials)
