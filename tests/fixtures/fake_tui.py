#!/usr/bin/env python3
"""Live stub TUI used only by the CAD-1204 fake tmux fixture."""
import fcntl
import os
from pathlib import Path
import sys
import time

locks = Path(os.environ["CADENCE_STUB_LOCKS"])
session = sys.argv[-1] if len(sys.argv) > 1 else "cad1204-native"
lock = open(locks / f"{session}.lock", "a+")
fcntl.flock(lock, fcntl.LOCK_EX)
print("stub ready\n» stub ready", flush=True)
for line in sys.stdin:
    if line.strip() in ("exit", "quit"):
        break
    # Keep input and the profile's idle signature visible to capture-pane.
    print(line.rstrip(), flush=True)
    print("» stub ready", flush=True)
    time.sleep(.01)
