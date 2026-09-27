#!/usr/bin/env python3
import json, os, re, signal, subprocess, sys, time

# Per-install config: the state dir sits beside this script, and the
# test's knobs (`mock_knob`) live in its `mock-env` file — overlaid on
# this call's env, so a pane spawned below inherits them. Never the
# process env, which every test in the binary shares.
here = os.path.dirname(os.path.abspath(__file__))
try:
    os.environ.update(json.load(open(os.path.join(here, "mock-env"))))
except FileNotFoundError:
    pass
args = sys.argv[1:]
if args[0] == "-L":
    sock = args[1]; args = args[2:]
state = os.path.join(here, "tmux-state", sock)
os.makedirs(state, exist_ok=True)

def sess_path(name, ext):
    return os.path.join(state, name + "." + ext)

def sess_pid(name):
    try:
        pid = int(open(sess_path(name, "pid")).read().strip())
        os.kill(pid, 0)
        return pid
    except Exception:
        return None

def die(msg, code=1):
    sys.stderr.write(msg + "\n"); sys.exit(code)

def awrite(path, text):
    # Atomic write — a capture-pane reader sees whole content or none.
    tmp = path + ".tmp"
    with open(tmp, "w") as f: f.write(text)
    os.rename(tmp, path)

def aappend(path, text):
    try: cur = open(path).read()
    except OSError: cur = ""
    awrite(path, cur + text)

cmd, rest = args[0], args[1:]
# Every invocation lands in calls.log — tests that must count a probe
# (e.g. `cadence status` probing exactly once per pty agent) read it.
try:
    with open(os.path.join(state, "calls.log"), "a") as f:
        f.write(cmd + " " + " ".join(rest) + "\n")
except Exception:
    pass
# Deterministic latency injection: MOCK_TMUX_HOLD=<secs> delays the
# command named by MOCK_TMUX_HOLD_CMD (default display-message) — the
# harness's way to make an adapter probe straggle past the daemon's
# stop grace without any sleep in test code. MOCK_TMUX_HOLD_FMT narrows
# the hold to calls whose args contain it (e.g. only #{pane_dead}), so
# latency lands on the probe under test instead of every probe.
hold = float(os.environ.get("MOCK_TMUX_HOLD", "0"))
hold_fmt = os.environ.get("MOCK_TMUX_HOLD_FMT", "")
if hold and cmd == os.environ.get("MOCK_TMUX_HOLD_CMD", "display-message") \
        and (not hold_fmt or hold_fmt in rest):
    time.sleep(hold)
# MOCK_TMUX_FAIL=<cmd> makes that subcommand die — deterministic
# failure injection, e.g. a transient capture-pane outage while a
# gate probe runs.
if cmd and cmd == os.environ.get("MOCK_TMUX_FAIL", ""):
    die("mock injected failure")
if cmd == "new-session":
    name = rest[rest.index("-s") + 1]
    cwd = rest[rest.index("-c") + 1] if "-c" in rest else os.getcwd()
    pane_cmd = rest[-1]
    pane = os.path.join(state, name)
    env = dict(os.environ, FAKE_PANE=pane)
    # tmux -e VAR=value exports into the pane process env.
    for i, a in enumerate(rest[:-1]):
        if a == "-e" and "=" in rest[i + 1]:
            k, v = rest[i + 1].split("=", 1)
            env[k] = v
    # Detach the pane's stdio to a file — the adapter's Command::output
    # would otherwise wait on pipes the long-lived pane inherited.
    log = open(sess_path(name, "log"), "ab")
    proc = subprocess.Popen(["bash", "-c", pane_cmd], cwd=cwd, env=env,
                            stdin=subprocess.DEVNULL, stdout=log,
                            stderr=log, start_new_session=True)
    open(sess_path(name, "pid"), "w").write(str(proc.pid))
    open(sess_path(name, "screen"), "a").close()
    sys.exit(0)
if cmd == "has-session":
    name = rest[rest.index("-t") + 1]
    sys.exit(0 if sess_pid(name) else 1)
if cmd == "display-message":
    name = rest[rest.index("-t") + 1]
    fmt = rest[-1]
    pid = sess_pid(name)
    if fmt == "#{pane_pid}":
        # A dead pane keeps its pid (tmux keeps dead panes); emulate.
        try: print(int(open(sess_path(name, "pid")).read().strip()))
        except Exception: die("no such session")
    elif fmt == "#{pane_dead}":
        print("0" if pid else "1")
    elif fmt == "#{pane_in_mode}":
        try: print(open(sess_path(name, "mode")).read().strip() or "0")
        except FileNotFoundError: print("0")
    elif fmt == "#{cursor_x},#{cursor_y}":
        # The cursor sits on the TUI's input line: column 2 (right
        # after the prompt glyph + space) when empty, or after the
        # staged draft. Row = the rendered input row — the staged line
        # capture-pane appends when text is staged, else the last
        # prompt-glyph row of the screen.
        try: staged = open(sess_path(name, "input")).read()
        except FileNotFoundError: staged = ""
        try: rows = open(sess_path(name, "screen")).read().splitlines()
        except FileNotFoundError: rows = []
        if staged:
            print("%d,%d" % (2 + len(staged), len(rows)))
        else:
            y = max((i for i, l in enumerate(rows)
                     if l.strip().startswith(("❯", "❭", "»"))), default=0)
            print("2,%d" % y)
    else: die("unknown format " + fmt)
    sys.exit(0)
if cmd == "capture-pane":
    name = rest[rest.index("-t") + 1]
    # Count captures so tests can prove the stall ticker only samples
    # panes while a turn is running.
    try:
        with open(sess_path(name, "captures"), "a") as f: f.write("c")
    except OSError: pass
    out = ""
    try: out += open(sess_path(name, "screen")).read()
    except FileNotFoundError: die("no such session")
    # The TUI chrome row directly above the input box — the busy
    # spinner or the `Press Enter to send queued messages` invitation.
    # A `.status` file holds it verbatim (written by the test or by the
    # pane's own TUI when it queues a mid-turn send).
    try: out += open(sess_path(name, "status")).read()
    except FileNotFoundError: pass
    # The input line renders like the TUI's own: the pane's `.glyph`
    # file (written by its TUI; `❭` is the Devin default) + staged
    # draft. With no staged draft, a `.inputbox` file is the box's own
    # placeholder text — the busy `Guide Devin` box keeps an editable
    # input row on screen while the idle box's placeholder lives in
    # the screen file itself.
    try: staged = open(sess_path(name, "input")).read()
    except FileNotFoundError: staged = ""
    if not staged:
        try: staged = open(sess_path(name, "inputbox")).read()
        except FileNotFoundError: pass
    if staged:
        try: glyph = open(sess_path(name, "glyph")).read().strip() or "❭"
        except FileNotFoundError: glyph = "❭"
        out += glyph + " " + staged + "\n"
    # Test-controlled extra screen content — a file the test writes to
    # make the pane look busy, approval-blocked, etc. A `tui-once` file
    # replaces it for exactly one capture: the rename claims it
    # atomically, so no second capture can see it however the test's
    # writes interleave with this read.
    once = sess_path(name, "tui-once")
    claimed = "%s.%d" % (once, os.getpid())
    try:
        os.rename(once, claimed)
        out += open(claimed).read()
        os.unlink(claimed)
    except FileNotFoundError:
        try: out += open(sess_path(name, "tui-state")).read()
        except FileNotFoundError: pass
    # Real tmux only prints the pane with `-p` — without it the capture
    # lands in the paste buffer and stdout stays empty. Emulate that so
    # a dropped `-p` fails loudly here the way it does on a real pane.
    if "-p" not in rest:
        sys.exit(0)
    # Real tmux keeps SGR attributes (and OSC 8 links) only with `-e`;
    # a plain capture drops them. Screen files may carry a styled frame.
    if "-e" not in rest:
        out = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)",
                     "", out)
    sys.stdout.write(out); sys.exit(0)
if cmd == "load-buffer":
    open(os.path.join(state, "buffer"), "w").write(open(rest[-1]).read())
    sys.exit(0)
if cmd == "paste-buffer":
    name = rest[rest.index("-t") + 1]
    # A `.swallow` file models a busy TUI dropping the bracketed paste:
    # the write path "works" but the text never reaches the screen.
    if not os.path.exists(sess_path(name, "swallow")):
        aappend(sess_path(name, "input"),
                open(os.path.join(state, "buffer")).read())
    sys.exit(0)
if cmd == "send-keys":
    name = rest[rest.index("-t") + 1]
    for key in rest[rest.index("-t") + 2:]:
        if key == "--":  # ends tmux option parsing — not a key
            continue
        aappend(sess_path(name, "input"),
                "<ENTER>" if key == "Enter" else "<KEY:" + key + ">")
    sys.exit(0)
if cmd == "set-option":
    # Record option writes so tests can assert pane defaults.
    with open(os.path.join(state, "setopt.log"), "a") as f:
        f.write(" ".join(rest) + "\n")
    sys.exit(0)
if cmd == "kill-session":
    name = rest[rest.index("-t") + 1]
    pid = sess_pid(name)
    if pid:
        try: os.killpg(pid, signal.SIGKILL)
        except ProcessLookupError: pass
    sys.exit(0)
if cmd == "list-clients":
    # Attached terminal clients: one tty per line of `<session>.clients`
    # (absent = none attached). An unknown session fails like tmux.
    name = rest[rest.index("-t") + 1].lstrip("=")
    if not sess_pid(name):
        die("can't find session: " + name)
    try: sys.stdout.write(open(sess_path(name, "clients")).read())
    except OSError: pass
    sys.exit(0)
die("unhandled tmux cmd " + cmd)
