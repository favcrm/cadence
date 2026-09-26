
import fcntl, json, os, socket, sys, time

locks = sys.argv[1]
sid = sys.argv[sys.argv.index("-r") + 1] if "-r" in sys.argv else \
    "mock-session-%d" % os.getpid()
os.makedirs(locks, exist_ok=True)
def awrite(path, text):
    # Atomic write — a capture-pane reader sees whole content or none.
    tmp = path + ".tmp"
    with open(tmp, "w") as f: f.write(text)
    os.rename(tmp, path)
def aappend(path, text):
    try: cur = open(path).read()
    except OSError: cur = ""
    awrite(path, cur + text)
pane = os.environ["FAKE_PANE"]
# An untrusted folder parks the TUI on its directory-trust select
# before any session exists — `<pane>.trust` models it; deleting the
# file is the answered prompt, and the TUI redraws and continues.
if os.path.exists(pane + ".trust"):
    awrite(pane + ".screen",
        "Mock Devin TUI\n"
        "Do you trust the files in this folder?\n\n"
        "❭ 1 Yes, trust this folder\n"
        "· 2 No, do not trust\n\n"
        "↓↑ to select · ↵ confirm · esc cancel\n")
    while os.path.exists(pane + ".trust"):
        time.sleep(0.1)
    awrite(pane + ".screen", "")
lf = open(os.path.join(locks, sid + ".lock"), "a")
try:
    fcntl.flock(lf, fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError:
    print("session_locked: %s" % sid); sys.exit(1)
open(os.environ["FAKE_PANE"] + ".sid", "w").write(sid)
# Record the launch argv — tests assert flags are replayed on resume.
# Temp + rename so a reader never sees the file torn mid-write.
_argv_tmp = os.environ["FAKE_PANE"] + ".argv.tmp"
open(_argv_tmp, "w").write("\n".join(sys.argv))
os.rename(_argv_tmp, os.environ["FAKE_PANE"] + ".argv")
# Record the pane env the adapter exported via tmux -e.
_env_tmp = os.environ["FAKE_PANE"] + ".env.tmp"
open(_env_tmp, "w").write(
    "CADENCE_ALIAS=%s\nCADENCE_STATE_DIR=%s\n" % (
        os.environ.get("CADENCE_ALIAS", ""),
        os.environ.get("CADENCE_STATE_DIR", "")))
os.rename(_env_tmp, os.environ["FAKE_PANE"] + ".env")
def memory_rpc():
    # Test-only bridge: the lockholding provider process opens the real
    # daemon socket, so SO_PEERCRED and /proc ancestry see this pane rather
    # than the integration-test process.  The request/response files are
    # opt-in and scoped to this mock pane; production providers have no such
    # bridge.
    req_path = os.environ["FAKE_PANE"] + ".memory-rpc"
    try:
        raw = open(req_path).read()
    except FileNotFoundError:
        return
    try:
        request = json.loads(raw)
        sock_path = os.path.join(os.environ["CADENCE_STATE_DIR"], "cadence.sock")
        chunks = []
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(15)
            sock.connect(sock_path)
            sock.sendall((json.dumps(request, separators=(",", ":")) + "\n").encode())
            while True:
                chunk = sock.recv(65536)
                if not chunk:
                    break
                chunks.append(chunk)
                if b"\n" in chunk:
                    break
        response = json.loads(b"".join(chunks).split(b"\n", 1)[0].decode())
    except Exception as exc:
        response = {"ok": False, "error": {"kind": "internal", "message": str(exc)}}
    try:
        os.unlink(req_path)
    except FileNotFoundError:
        pass
    awrite(os.environ["FAKE_PANE"] + ".memory-rpc.response", json.dumps(response))
aappend(os.environ["FAKE_PANE"] + ".screen",
        "Mock Devin TUI [%s]\n" % sid +
        # An idle input line — the same shape the real TUI shows so the
        # screen probe recognizes an empty prompt.
        "❭ Ask Devin to build features, fix bugs, or work on your code\n")
while True:
    memory_rpc()
    inp = os.environ["FAKE_PANE"] + ".input"
    try:
        data = open(inp).read()
    except FileNotFoundError:
        data = ""
    if "<ENTER>" in data:
        text, rest = data.split("<ENTER>", 1)
        queued = os.environ["FAKE_PANE"] + ".queue"
        busy_box = os.path.exists(os.environ["FAKE_PANE"] + ".inputbox")
        if os.path.exists(os.environ["FAKE_PANE"] + ".hold-enter"):
            # Enter swallowed: the marker is consumed but the draft
            # stays staged in the input line, unsubmitted.
            awrite(inp, text + rest)
        elif os.path.exists(os.environ["FAKE_PANE"] + ".noecho"):
            # The TUI consumed the draft into a running turn — the busy
            # status row and the guide-box watermark show work — but the
            # submitted line never echoes into the transcript
            # (CAD-520 F28: a render miss that is not a dropped delivery).
            awrite(inp, rest)
            if text.strip():
                awrite(os.environ["FAKE_PANE"] + ".status",
                       "⠸ Thinking · 1s (esc twice to interrupt)\n")
                awrite(os.environ["FAKE_PANE"] + ".inputbox",
                       "Guide Devin while it works\n")
        elif busy_box and text.strip():
            # The busy guide box: Enter only *queues* the draft — the
            # TUI then invites one more Enter to send it into the
            # running turn (`Press Enter to send queued messages`).
            awrite(inp, rest)
            awrite(queued, text.strip())
            awrite(os.environ["FAKE_PANE"] + ".status",
                   "Press Enter to send queued messages\n")
        elif busy_box and os.path.exists(queued):
            # The flush Enter: queued text joins the running turn and
            # echoes into the transcript like a normal submit.
            awrite(inp, rest)
            flushed = open(queued).read()
            os.unlink(queued)
            awrite(os.environ["FAKE_PANE"] + ".status",
                   "⠸ Thinking · 1s (esc twice to interrupt)\n")
            aappend(os.environ["FAKE_PANE"] + ".screen",
                    "> %s\nMOCK_REPLY: %s\n" % (flushed, flushed))
        else:
            awrite(inp, rest)
            if text.strip():
                aappend(os.environ["FAKE_PANE"] + ".screen",
                        "> %s\nMOCK_REPLY: %s\n" % (text.strip(), text.strip()))
    # C-c, or Esc (the Claude/Devin interrupt key, CAD-323): the turn
    # stops and the staged input clears.
    if "<KEY:C-c>" in data or "<KEY:Escape>" in data:
        awrite(inp, "")
        aappend(os.environ["FAKE_PANE"] + ".screen", "^C interrupt\n")
    time.sleep(0.05)
