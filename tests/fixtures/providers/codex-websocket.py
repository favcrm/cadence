
import base64, hashlib, json, os, socket, struct, sys, threading, time

pidfile, mode = sys.argv[1], sys.argv[2]
url = sys.argv[sys.argv.index("--listen") + 1]
host, port = url.split("://", 1)[1].split(":")
srv = socket.socket()
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind((host, int(port)))
srv.listen(4)
with open(pidfile, "w") as f:
    f.write(str(os.getpid()))
GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
SEED = "Cadence endpoint initialization"

def recv_exact(conn, n):
    data = b""
    while len(data) < n:
        chunk = conn.recv(n - len(data))
        if not chunk:
            return None
        data += chunk
    return data

def read_frame(conn):
    hdr = recv_exact(conn, 2)
    if hdr is None:
        return None, None
    opcode, flags = hdr[0] & 0x0F, hdr[1]
    length = flags & 0x7F
    if length == 126:
        length = struct.unpack(">H", recv_exact(conn, 2))[0]
    elif length == 127:
        length = struct.unpack(">Q", recv_exact(conn, 8))[0]
    mask = recv_exact(conn, 4) if flags & 0x80 else b""
    payload = recv_exact(conn, length) if length else b""
    if payload is None:
        return None, None
    if mask:
        payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
    return opcode, payload

def send_frag(conn, fin, opcode, payload):
    n = len(payload)
    if n < 126:
        hdr = bytes([(0x80 if fin else 0) | opcode, n])
    elif n < 65536:
        hdr = bytes([(0x80 if fin else 0) | opcode, 126]) + struct.pack(">H", n)
    else:
        hdr = bytes([(0x80 if fin else 0) | opcode, 127]) + struct.pack(">Q", n)
    conn.sendall(hdr + payload)

def send_frame(conn, opcode, payload):
    send_frag(conn, True, opcode, payload)

def send_json(conn, msg):
    send_frame(conn, 1, json.dumps(msg).encode())

def send_fragmented_complete(conn, turn, text):
    # One message split across two continuations with an interleaved
    # ping: the client must reassemble it and answer the control frame.
    body = json.dumps({"method": "turn/completed", "params": {"turn": {
        "id": turn, "status": "completed", "items": [
            {"id": "i1", "type": "agentMessage",
             "text": text, "phase": "final_answer"}]}}}).encode()
    half = len(body) // 2
    send_frag(conn, False, 0x1, body[:half])
    send_frame(conn, 0x9, b"mid-frag")
    send_frag(conn, True, 0x0, body[half:])

def complete(conn, turn, text):
    send_json(conn, {"method": "turn/completed", "params": {"turn": {
        "id": turn, "status": "completed", "items": [
            {"id": "i1", "type": "agentMessage",
             "text": text, "phase": "final_answer"}]}}})

def handshake(conn):
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = conn.recv(4096)
        if not chunk:
            return False
        data += chunk
    key = ""
    for line in data.decode().split("\r\n"):
        if line.lower().startswith("sec-websocket-key:"):
            key = line.split(":", 1)[1].strip()
    # RFC 6455: the client nonce must decode to exactly 16 bytes.
    # Refuse anything else, as a strict standards-compliant server does.
    try:
        valid = len(base64.b64decode(key)) == 16
    except Exception:
        valid = False
    if not valid:
        conn.sendall(b"HTTP/1.1 400 Bad Request\r\n\r\n")
        return False
    accept = base64.b64encode(
        hashlib.sha1((key + GUID).encode()).digest()).decode()
    conn.sendall((
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Accept: {accept}\r\n\r\n").encode())
    return True

def ping_check(conn):
    # Ping, then expect a pong before completing: proves the client
    # serializes control replies on the same write path as requests.
    send_frame(conn, 0x9, b"ping-check")
    conn.settimeout(5)
    pong = False
    try:
        while True:
            op, _ = read_frame(conn)
            if op is None:
                break
            if op == 0xA:
                pong = True
                break
    except Exception:
        pass
    conn.settimeout(None)
    with open(pidfile + ".pong", "w") as f:
        f.write("yes" if pong else "no")

def external_resolve(conn):
    # An attached TUI answered the approval: once the test drops the
    # trigger file, resolve the pending request outside the client and
    # let the turn finish without a client response.
    deadline = time.time() + 30
    while not os.path.exists(pidfile + ".resolve"):
        if time.time() > deadline:
            break
        time.sleep(0.05)
    send_json(conn, {"method": "serverRequest/resolved",
        "params": {"requestId": "srv-1", "threadId": "th-1"}})
    complete(conn, "t-1", "MOCK_OK")

def read_request(conn):
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = conn.recv(4096)
        if not chunk:
            return None
        data += chunk
    key = ""
    for line in data.decode().split("\r\n"):
        if line.lower().startswith("sec-websocket-key:"):
            key = line.split(":", 1)[1].strip()
    return key

def handle(conn):
    if mode == "no-upgrade":
        # Accept TCP, never answer the handshake. The client's bounded
        # handshake must give up and clean up the owned process.
        time.sleep(3600)
        return
    if mode == "drip":
        # Answer with a valid 101 one byte per second: only an absolute
        # deadline bounds this — per-read timeouts never fire.
        key = read_request(conn)
        if key is None:
            return
        accept = base64.b64encode(
            hashlib.sha1((key + GUID).encode()).digest()).decode()
        response = ("HTTP/1.1 101 Switching Protocols\r\n"
            "Upgrade: websocket\r\nConnection: Upgrade\r\n"
            f"Sec-WebSocket-Accept: {accept}\r\n\r\n").encode()
        for byte in response:
            conn.sendall(bytes([byte]))
            time.sleep(1)
        time.sleep(3600)
        return
    if mode == "bad-upgrade":
        # Refuse the upgrade outright: HTTP 200, not 101.
        if read_request(conn) is None:
            return
        conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
        time.sleep(3600)
        return
    if not handshake(conn):
        return
    approvals = set()
    while True:
        op, payload = read_frame(conn)
        if op is None or op == 8:
            break
        if op == 9:
            send_frame(conn, 0xA, payload)
            continue
        if op != 1:
            continue
        try:
            msg = json.loads(payload)
        except Exception:
            continue
        mid, method = msg.get("id"), msg.get("method")
        if method is None:
            if mid in approvals:
                approvals.discard(mid)
                if not approvals:
                    complete(conn, "t-1", "MOCK_OK")
            continue
        if method == "initialize":
            if mode == "slow-init":
                # Marks the handshake done and initialize in flight.
                open(pidfile + ".init", "w").close()
                time.sleep(30)
            send_json(conn, {"id": mid, "result": {
                "serverInfo": {"name": "mock-ws", "version": "0"}}})
        elif method == "model/list":
            send_json(conn, {"id": mid, "result": {"data": [
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
            # Record the launch payload before answering (<pidfile>.requests).
            with open(pidfile + ".requests", "a") as rf:
                rf.write(json.dumps({"method": method,
                                     "params": msg.get("params", {})}) + "\n")
            launch = msg.get("params", {})
            effort = launch.get("config", {}).get("model_reasoning_effort", "medium")
            model = launch.get("model", "mock-model")
            send_json(conn, {"id": mid, "result": {"thread": {
                "id": "th-1", "sessionId": "s-1", "model": model,
                "reasoningEffort": effort},
                "model": model, "reasoningEffort": effort}})
        elif method == "account/rateLimits/read":
            if mode == "no-quota":
                send_json(conn, {"id": mid, "error": {"code": -32601,
                    "message": "rate limits unavailable in this auth mode"}})
            else:
                send_json(conn, {"id": mid, "result": {
                    "accountId": "acct-codex-test",
                    "rateLimits": {"primary": {"usedPercent": 23,
                        "windowDurationMins": 60, "resetsAt": 1900000000}},
                    "rateLimitsByLimitId": {}, "planType": "mock-pro"}})
        elif method == "turn/start":
            text = ""
            try:
                text = msg["params"]["input"][0]["text"]
            except Exception:
                pass
            if mode == "ping-first" and not text.startswith(SEED):
                ping_check(conn)
            send_json(conn, {"id": mid, "result": {"turn": {"id": "t-1"}}})
            if text.startswith("DIE2"):
                send_frame(conn, 8, b"")
                return
            if text.startswith("DIE"):
                conn.close()
                return
            if text.startswith("FRAG"):
                send_fragmented_complete(conn, "t-1", "MOCK_OK")
                continue
            if text.startswith(SEED):
                complete(conn, "t-1", "READY")
            elif mode == "silent":
                pass
            elif text.startswith("NEED_INPUT_EXT"):
                approvals.add("srv-1")
                send_json(conn, {"id": "srv-1",
                    "method": "item/commandExecution/requestApproval",
                    "params": {"command": "x"}})
                external_resolve(conn)
                return
            elif text.startswith("NEED_INPUT2"):
                # Two outstanding approvals: answering one must leave
                # the agent waiting_input until both are answered.
                approvals.update(("srv-1", "srv-2"))
                for rid in ("srv-1", "srv-2"):
                    send_json(conn, {"id": rid,
                        "method": "item/commandExecution/requestApproval",
                        "params": {"command": "x"}})
            elif text.startswith("NEED_INPUT"):
                approvals.add("srv-1")
                send_json(conn, {"id": "srv-1",
                    "method": "item/commandExecution/requestApproval",
                    "params": {"command": "x"}})
            else:
                complete(conn, "t-1", "MOCK_OK")
        elif method == "turn/interrupt":
            send_json(conn, {"id": mid, "result": {}})
    conn.close()

while True:
    conn, _ = srv.accept()
    threading.Thread(target=handle, args=(conn,), daemon=True).start()
