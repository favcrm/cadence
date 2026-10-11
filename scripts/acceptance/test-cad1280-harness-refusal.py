#!/usr/bin/env python3
"""Independent black-box acceptance for CAD-1280's agent-UID refusal guard.

Exercise the real measurement-harness entry points with each confinement
marker in a disposable state directory. A recorder stands in for the Cadence
CLI; a refusal must happen before it, the daemon socket, or any provider can
be reached. No daemon or provider is started by this check.
"""
from pathlib import Path
import subprocess
import sys
import tempfile


REPO = Path(__file__).resolve().parents[2]
HARNESS = REPO / "scripts/measurements/agent-list-bench.py"
MARKERS = ("agent-uid.json", "agent-uid-mode.json")
COMMANDS = ("prepare", "preflight", "measure")


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def main():
    with tempfile.TemporaryDirectory(prefix="c1280-") as temp:
        base = Path(temp)
        recorder = base / "cadence-recorder"
        invocations = base / "cadence-invoked"
        recorder.write_text(
            "#!/bin/sh\n"
            f"printf '%s\\n' \"$*\" >> {str(invocations)!r}\n"
            "exit 99\n",
            encoding="utf-8",
        )
        recorder.chmod(0o755)

        cases = 0
        for marker_index, marker in enumerate(MARKERS):
            for command_index, command in enumerate(COMMANDS):
                root = base / f"r{marker_index}{command_index}"
                state = root / "state"
                state.mkdir(parents=True)
                (state / marker).write_text("{}\n", encoding="utf-8")
                args = [sys.executable, str(HARNESS), command,
                        "--cadence", str(recorder), "--root", str(root)]
                if command == "measure":
                    args.extend(("--label", "acceptance"))

                result = subprocess.run(args, capture_output=True, text=True, timeout=10)
                output = result.stdout + result.stderr
                require(result.returncode != 0,
                        f"{command} accepted {marker}; stdout={result.stdout!r} stderr={result.stderr!r}")
                require(f"refusing: {marker} is in the state dir" in output,
                        f"{command} did not report the {marker} refusal: {output!r}")
                require("agent-uid confinement bypasses the PATH stubs" in output,
                        f"{command} refusal omitted the confinement rationale: {output!r}")
                require(not invocations.exists(),
                        f"{command} invoked the Cadence CLI before refusing {marker}")
                require(not (state / "cadence.sock").exists(),
                        f"{command} created a daemon socket before refusing {marker}")
                cases += 1

        print(f"PASS: {cases} real-entrypoint refusal cases; both markers refused "
              "by prepare/preflight/measure before Cadence invocation or daemon socket creation")


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        sys.exit(1)
