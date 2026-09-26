
import fcntl, os, sys, time

locks = sys.argv[1]
sid = sys.argv[sys.argv.index("-r") + 1] if "-r" in sys.argv else \
    "stub-session-%d" % os.getpid()
os.makedirs(locks, exist_ok=True)
lf = open(os.path.join(locks, sid + ".lock"), "a")
try:
    fcntl.flock(lf, fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError:
    print("session_locked: %s" % sid); sys.exit(1)
open(os.environ["FAKE_PANE"] + ".sid", "w").write(sid)
# The pane's own input-line glyph — the mock tmux renders staged text
# with it, so a staged draft reads as this TUI's prompt line.
open(os.environ["FAKE_PANE"] + ".glyph", "w").write("»")
def awrite(path, text):
    # Atomic write — a capture-pane reader sees whole content or none.
    tmp = path + ".tmp"
    with open(tmp, "w") as f: f.write(text)
    os.rename(tmp, path)
def aappend(path, text):
    try: cur = open(path).read()
    except OSError: cur = ""
    awrite(path, cur + text)
aappend(os.environ["FAKE_PANE"] + ".screen",
        "Mock Stub TUI [%s]\n" % sid +
        # The stub profile's empty-prompt signature.
        "» stub ready\n")
while True:
    inp = os.environ["FAKE_PANE"] + ".input"
    try:
        data = open(inp).read()
    except FileNotFoundError:
        data = ""
    if "<ENTER>" in data:
        text, rest = data.split("<ENTER>", 1)
        if os.path.exists(os.environ["FAKE_PANE"] + ".hold-enter"):
            # Enter swallowed: the marker is consumed but the draft
            # stays staged in the input line, unsubmitted.
            awrite(inp, text + rest)
        else:
            awrite(inp, rest)
            if text.strip():
                aappend(os.environ["FAKE_PANE"] + ".screen",
                        "> %s\nSTUB_REPLY: %s\n" % (text.strip(), text.strip()))
    time.sleep(0.05)
