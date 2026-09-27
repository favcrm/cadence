
import os, sys, time, uuid

chats = sys.argv[1]
if "create-chat" in sys.argv:
    print(uuid.uuid4()); sys.exit(0)
sid = sys.argv[sys.argv.index("--resume") + 1] if "--resume" in sys.argv \
    else "missing-resume"
if os.environ.get("MOCK_CURSOR_SWAP"):
    sid = "swapped-" + sid
# A chat named by MOCK_CURSOR_DIE_ON is unresumable — the real TUI
# exits on a deleted/foreign chat, so the mock does too.
if sid == os.environ.get("MOCK_CURSOR_DIE_ON"):
    sys.exit(1)
chat_dir = os.path.join(chats, "mockhash", sid)
os.makedirs(chat_dir, exist_ok=True)
# The real TUI holds an fd on the chat's store.db for its whole life —
# the profile's ownership proof scans /proc fds for exactly this.
db = open(os.path.join(chat_dir, "store.db"), "a")
# Record the launch argv so tests can assert the profile's flags —
# temp + rename so a reader never sees the file torn mid-write.
_argv_tmp = os.environ["FAKE_PANE"] + ".argv.tmp"
open(_argv_tmp, "w").write(" ".join(sys.argv[1:]))
os.rename(_argv_tmp, os.environ["FAKE_PANE"] + ".argv")
# Record the pane env the adapter exported via tmux -e.
_env_tmp = os.environ["FAKE_PANE"] + ".env.tmp"
open(_env_tmp, "w").write(
    "CADENCE_ALIAS=%s\nCADENCE_STATE_DIR=%s\n" % (
        os.environ.get("CADENCE_ALIAS", ""),
        os.environ.get("CADENCE_STATE_DIR", "")))
os.rename(_env_tmp, os.environ["FAKE_PANE"] + ".env")
# The pane's own input-line glyph + the idle frame — the shape the real
# TUI shows so the screen probe recognizes idle.
open(os.environ["FAKE_PANE"] + ".glyph", "w").write("→")
with open(os.environ["FAKE_PANE"] + ".screen", "a") as f:
    f.write("Mock Cursor TUI [%s]\n" % sid)
    f.write("  → Plan, search, build anything\n")
    f.write("  Cursor Grok 4.6 High\n  /mock · main\n")
def awrite(path, text):
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        f.write(text)
    os.rename(tmp, path)
while True:
    inp = os.environ["FAKE_PANE"] + ".input"
    try:
        data = open(inp).read()
    except FileNotFoundError:
        data = ""
    if "<ENTER>" in data:
        text, rest = data.split("<ENTER>", 1)
        pane = os.environ["FAKE_PANE"]
        if os.path.exists(pane + ".noecho"):
            # The TUI consumed the draft into a running turn. The body
            # never echoes. The frame is the live 2026-09-26 shape: a
            # braille "Working" row, the follow-up watermark, a
            # right-aligned interrupt hint, and pane-width padding
            # after the hint (CAD-612).
            awrite(inp, rest)
            if text.strip():
                line = (
                    "  → Add a follow-up"
                    + (" " * 80)
                    + "ctrl+c to stop"
                    + (" " * 24)
                )
                awrite(
                    pane + ".screen",
                    "Mock Cursor TUI\n"
                    "  \u2820 Working\n"
                    "    Tip: Use /run-everything to skip all approvals.\n"
                    "\n"
                    + line
                    + "\n"
                    "\n"
                    "  1 task\n"
                    "  Grok 4.7 256K High\n"
                    "  /mock · main\n",
                )
        else:
            awrite(inp, rest)
            if text.strip():
                with open(pane + ".screen", "a") as f:
                    f.write("  %s\nMOCK_REPLY: %s\n" % (text.strip(), text.strip()))
                    # The submitted line echoes into the transcript and the
                    # input's watermark flips to the follow-up form.
                    f.write("  → Add a follow-up\n")
                    f.write("  Cursor Grok 4.6 High\n  /mock · main\n")
    if "<KEY:C-c>" in data:
        open(inp, "w").write("")
        with open(os.environ["FAKE_PANE"] + ".screen", "a") as f:
            f.write("^C interrupt\n")
    time.sleep(0.05)
