
import json, os, sys, time

sessions = sys.argv[1]
if "--resume" in sys.argv:
    sid = sys.argv[sys.argv.index("--resume") + 1]
elif "--session-id" in sys.argv:
    sid = sys.argv[sys.argv.index("--session-id") + 1]
else:
    sid = "mock-claude-%d" % os.getpid()
os.makedirs(sessions, exist_ok=True)
# Record the launch argv so tests can assert the profile's flags —
# temp + rename so a reader never sees the file torn mid-write.
_argv_tmp = os.environ["FAKE_PANE"] + ".argv.tmp"
open(_argv_tmp, "w").write(" ".join(sys.argv[1:]))
os.rename(_argv_tmp, os.environ["FAKE_PANE"] + ".argv")
pid = os.getpid()
# /proc/self/stat field 22 — what the real registry's procStart is.
try:
    stat = open("/proc/self/stat").read()
    proc_start = stat[stat.rindex(")") + 1:].split()[19]
except Exception:
    proc_start = None
entry = {"pid": pid, "sessionId": sid, "cwd": os.getcwd(),
         "procStart": proc_start, "kind": "interactive"}
if os.environ.get("MOCK_CLAUDE_SWAP"):
    entry["sessionId"] = "swapped-" + sid
# MOCK_CLAUDE_NO_REGISTRY keeps the pane alive but never publishes the
# session — the adapter's open wait then runs to its deadline, the
# transient-proof-timeout shape a Claude resume must survive.
if not os.environ.get("MOCK_CLAUDE_NO_REGISTRY"):
    open(os.path.join(sessions, "%d.json" % pid), "w").write(json.dumps(entry))
# Record the pane env the adapter exported via tmux -e.
_env_tmp = os.environ["FAKE_PANE"] + ".env.tmp"
open(_env_tmp, "w").write(
    "CADENCE_ALIAS=%s\nCADENCE_STATE_DIR=%s\n" % (
        os.environ.get("CADENCE_ALIAS", ""),
        os.environ.get("CADENCE_STATE_DIR", "")))
os.rename(_env_tmp, os.environ["FAKE_PANE"] + ".env")
# The pane's own input-line glyph + a boxed empty prompt — the shape
# the real TUI shows so the screen probe recognizes idle.
open(os.environ["FAKE_PANE"] + ".glyph", "w").write("❯")
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
        "Mock Claude TUI [%s]\n" % sid +
        "  [Opus] mock-mode on\n" +
        "─" * 40 + "\n❯ \n" + "─" * 40 + "\n")
while True:
    inp = os.environ["FAKE_PANE"] + ".input"
    try:
        data = open(inp).read()
    except FileNotFoundError:
        data = ""
    if "<ENTER>" in data:
        text, rest = data.split("<ENTER>", 1)
        awrite(inp, rest)
        if text.strip():
            # The submitted line echoes into the transcript and the
            # box re-renders empty below it.
            aappend(os.environ["FAKE_PANE"] + ".screen",
                    "> %s\nMOCK_REPLY: %s\n" % (text.strip(), text.strip()) +
                    "─" * 40 + "\n❯ \n" + "─" * 40 + "\n")
    # C-c, or Esc (the Claude/Devin interrupt key, CAD-323): the turn
    # stops and the staged input clears.
    if "<KEY:C-c>" in data or "<KEY:Escape>" in data:
        awrite(inp, "")
        aappend(os.environ["FAKE_PANE"] + ".screen", "^C interrupt\n")
    time.sleep(0.05)
