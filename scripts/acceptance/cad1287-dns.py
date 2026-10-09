#!/usr/bin/env python3
"""Manual CAD-1287 real-host acceptance for the master's Landlock policy.

Run only on a Linux systemd-resolved host with working DNS and readable
/run/systemd/journal. Uses the production CLI's pure `master confinement`
output and its actual `confine` command; it does not start a master/daemon.
"""

import json
import os
from pathlib import Path
import subprocess
import sys


RESOLVER = "/run/systemd/resolve"
OUTSIDE = "/run/systemd/journal"
GETENT = "/usr/bin/getent"


def refuse(message):
    print(f"CAD-1287 manual acceptance: BLOCKED: {message}", file=sys.stderr)
    raise SystemExit(2)


def main():
    if len(sys.argv) != 3:
        refuse("usage: cad1287-dns.py <built-cadence-binary> <private-root>")
    cadence = str(Path(sys.argv[1]).resolve(strict=True))
    root = Path(sys.argv[2]).resolve(strict=True)
    if sys.platform != "linux":
        refuse("Linux required")
    if os.environ.get("CADENCE_MASTER_CONFINE_READ") is not None:
        refuse("CADENCE_MASTER_CONFINE_READ must be absent")
    if not Path("/etc/resolv.conf").is_symlink():
        refuse("/etc/resolv.conf is not a symlink")
    if Path("/etc/resolv.conf").resolve() != Path("/run/systemd/resolve/stub-resolv.conf"):
        refuse("/etc/resolv.conf does not resolve to the systemd stub")
    if not Path(RESOLVER).is_dir():
        refuse("systemd resolver directory is unavailable")
    try:
        with os.scandir(OUTSIDE) as entries:
            next(entries, None)
    except OSError as error:
        refuse(f"outside-grant baseline is not readable ({error.__class__.__name__})")

    env = os.environ.copy()
    env.update({
        "HOME": str(root / "home"),
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "XDG_DATA_HOME": str(root / "data"),
        "TMPDIR": str(root / "tmp"),
    })
    for name in ("home", "config", "cache", "data", "tmp", "state"):
        (root / name).mkdir(mode=0o700, parents=True, exist_ok=True)

    def run(argv):
        return subprocess.run(argv, env=env, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, check=False)

    policy_result = run([cadence, "--state-dir", str(root / "state"),
                         "master", "confinement"])
    if policy_result.returncode != 0:
        refuse(f"native master confinement CLI exited {policy_result.returncode}")
    try:
        policies = json.loads(policy_result.stdout)
    except (UnicodeDecodeError, json.JSONDecodeError):
        refuse("native master confinement CLI did not return JSON")

    checked = 0
    for name in ("claude", "pi"):
        policy = policies.get(name)
        if not isinstance(policy, dict):
            refuse(f"native CLI omitted the {name} policy")
        reads = policy.get("read", [])
        writes = policy.get("write", [])
        if sum(Path(path) == Path(RESOLVER) for path in reads) != 1:
            refuse(f"{name}: resolver grant is not present exactly once")
        if any(Path(path) == Path(RESOLVER) for path in writes):
            refuse(f"{name}: resolver grant is writable")
        # Feed every read and write grant emitted by the actual native CLI to
        # cadence confine; no selected, broadened, or reconstructed policy.
        args = [cadence, "confine"]
        for path in reads:
            args.extend(["--read", str(path)])
        for path in writes:
            args.extend(["--write", str(path)])
        confined_prefix = args + ["--"]
        dns = run(confined_prefix + [GETENT, "hosts", "api.anthropic.com"])
        if dns.returncode != 0 or not dns.stdout.strip():
            refuse(f"{name}: confined getent failed (exit {dns.returncode})")
        denied = run(confined_prefix + ["/bin/ls", "-1", OUTSIDE])
        if denied.returncode == 0 or b"Permission denied" not in denied.stderr:
            refuse(f"{name}: outside-grant ls did not return kernel EACCES")
        checked += 1

    print(f"CAD-1287 manual acceptance: PASS; actual native Claude+Pi policies; "
          f"resolver exact read-only grant; getent success; ordinary outside "
          f"readable; confined outside access EACCES; policies checked={checked}")


if __name__ == "__main__":
    main()
