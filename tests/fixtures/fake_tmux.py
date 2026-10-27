#!/usr/bin/env python3
"""Test-only minimal tmux provider for CAD-1204.

State is private to a test root (CADENCE_FAKE_TMUX_STATE). Pane processes are
real children and survive daemon restarts. The live child owns the stub
profile's flock; tmux commands only report facts observed from that process.
"""
import fcntl
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


def state_path():
    return Path(os.environ["CADENCE_FAKE_TMUX_STATE"])


def load():
    try:
        return json.loads(state_path().read_text())
    except (OSError, ValueError):
        return {}


def save(state):
    p = state_path()
    p.parent.mkdir(parents=True, exist_ok=True)
    tmp = p.with_suffix(".tmp")
    tmp.write_text(json.dumps(state))
    tmp.replace(p)


def barrier():
    entered = os.environ.get("CADENCE_FAKE_TMUX_BARRIER_ENTERED")
    release = os.environ.get("CADENCE_FAKE_TMUX_BARRIER_RELEASE")
    armed = os.environ.get("CADENCE_FAKE_TMUX_BARRIER_ARMED")
    if not entered or not release or not armed or not Path(armed).exists():
        return
    Path(entered).write_text("entered\n")
    deadline = time.monotonic() + float(os.environ.get("CADENCE_FAKE_TMUX_BARRIER_MAX_SECS", "20"))
    while not Path(release).exists() and time.monotonic() < deadline:
        time.sleep(.01)


def main(argv):
    if len(argv) < 2 or argv[0] != "-L":
        return 2
    args = argv[2:]
    if not args:
        return 2
    op, rest = args[0], args[1:]
    trace = Path(str(state_path()) + ".commands")
    trace.parent.mkdir(parents=True, exist_ok=True)
    with trace.open("a") as stream:
        stream.write(json.dumps(args) + "\\n")
    s = load()
    session = s.get("session")
    if op == "new-session":
        name = rest[rest.index("-s") + 1]
        cmd = rest[-1]
        base = state_path().parent
        base.mkdir(parents=True, exist_ok=True)
        fifo, output = base / "pane.in", base / "pane.out"
        fifo.unlink(missing_ok=True)
        os.mkfifo(fifo)
        output.touch()
        # RDWR avoids a FIFO open rendezvous and gives the child a stable stdin.
        fd = os.open(fifo, os.O_RDWR)
        pane = subprocess.Popen(["/bin/sh", "-c", cmd], stdin=fd,
                                stdout=output.open("ab", buffering=0),
                                stderr=subprocess.STDOUT, close_fds=True,
                                start_new_session=True)
        os.close(fd)
        save({"session": name, "pid": pane.pid, "fifo": str(fifo), "output": str(output)})
        return 0
    if op == "has-session":
        barrier()
        if not session or not s.get("pid"):
            return 1
        try:
            os.kill(int(s["pid"]), 0)
            return 0
        except (ProcessLookupError, PermissionError):
            return 1
    if op == "display-message":
        fmt = rest[-1]
        if not session:
            return 1
        if fmt == "#{pane_pid}":
            print(s["pid"])
        elif fmt == "#{pane_dead}":
            try:
                os.kill(int(s["pid"]), 0)
                print("0")
            except ProcessLookupError:
                print("1")
        elif fmt == "#{pane_in_mode}":
            print("0")
        elif fmt == "#{cursor_x},#{cursor_y}":
            print("0,0")
        else:
            print("")
        return 0
    if op == "load-buffer":
        if len(rest) < 2:
            return 1
        Path(str(state_path()) + ".buffer").write_text(Path(rest[-1]).read_text())
        return 0
    if op == "paste-buffer":
        if not session:
            return 1
        payload = Path(str(state_path()) + ".buffer").read_text()
        with open(s["fifo"], "w") as stream:
            stream.write(payload)
        return 0
    if op == "send-keys":
        if not session:
            return 1
        if rest[-1] == "Enter":
            with open(s["fifo"], "w") as stream:
                stream.write("\n")
        return 0
    if op == "capture-pane":
        if not session:
            return 1
        if "#{pane_pid}" in rest:
            print(s["pid"])
        else:
            try:
                print(Path(s["output"]).read_text(errors="replace"), end="")
            except OSError:
                pass
        return 0
    if op in ("set-option", "list-clients", "list-sessions"):
        if op == "list-sessions" and session:
            print(session)
        return 0
    if op in ("kill-session", "kill-server"):
        if session:
            try:
                os.killpg(int(s["pid"]), signal.SIGKILL)
            except ProcessLookupError:
                pass
            save({})
        return 0
    return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
