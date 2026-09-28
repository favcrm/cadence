#!/usr/bin/env python3
"""Exercise the Codex sandbox preflight without a model or host namespaces."""

import os
import subprocess
import sys

mode, *args = sys.argv[1:]
assert args[0] == "sandbox", args
assert "-c" in args, args
assert any(a in args for a in ('sandbox_mode="workspace-write"', 'sandbox_mode="read-only"')), args
assert "-C" not in args and "--" in args, args
assert "danger-full-access" not in " ".join(args), args
cwd = os.getcwd()
command = args[args.index("--") + 1:]
assert cwd and command, args

if mode == "fail":
    print("bwrap: loopback: Failed RTM_NEWADDR: Operation not permitted", file=sys.stderr)
    sys.exit(1)
if mode == "hang":
    subprocess.run(["sleep", "30"], check=True)
    sys.exit(0)
assert mode == "ok", mode
sys.exit(subprocess.run(command, cwd=cwd).returncode)
