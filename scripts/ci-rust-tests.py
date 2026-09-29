#!/usr/bin/env python3
"""Execute the recorded scope with pinned tools and fixed feature shapes."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess


def scope_args(plan):
    mode = plan.get("mode")
    targets = plan.get("targets")
    if plan.get("schema") != 1 or not isinstance(targets, list):
        raise ValueError("invalid test plan")
    if mode in {"docs", "full"}:
        if targets:
            raise ValueError("docs/full plan cannot carry selected targets")
        return [] if mode == "docs" else ["--all-targets"]
    if mode != "selected" or not targets:
        raise ValueError("unknown or empty selected scope")
    args = ["--lib", "--bins"]
    for target in targets:
        if not isinstance(target, str) or not re.fullmatch(r"[A-Za-z][A-Za-z0-9_]*", target):
            raise ValueError("invalid Cargo test target")
        args.extend(["--test", target])
    return args


def partition_args(raw):
    """Nextest args for one shard of the suite (`M/N`, 1-based).

    Empty/blank means the whole suite. Only the `tests` phase applies a
    partition — inventory must always list every test, and the wrapper
    passes `--partition` through untouched (it is not a pinned knob).
    """
    if not raw or not raw.strip():
        return []
    text = raw.strip()
    match = re.fullmatch(r"([1-9][0-9]*)/([1-9][0-9]*)", text)
    if not match:
        raise ValueError(f"invalid test partition {raw!r}, want M/N")
    if int(match.group(1)) > int(match.group(2)):
        raise ValueError(f"invalid test partition {raw!r}, shard exceeds total")
    return ["--partition", f"hash:{text}"]


def run(root, plan, phase):
    args = scope_args(plan)
    if plan["mode"] == "docs":
        print("Rust execution omitted: " + plan["reason"])
        return
    if phase == "inventory":
        command = [str(root / "scripts/nextest-inventory")]
        command += (["all-targets"] if plan["mode"] == "full" else ["selected", *args])
        command += ["--features", "test-seam"]
    else:
        (root / "target/nextest/cadence/junit.xml").unlink(missing_ok=True)
        command = [str(root / "scripts/cadence-nextest"), *args, "--locked", "--features", "test-seam"]
        command += partition_args(os.environ.get("CADENCE_TEST_PARTITION", ""))
    subprocess.run(command, cwd=root, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--phase", choices=["inventory", "tests"], required=True)
    args = parser.parse_args()
    run(args.root.resolve(), json.loads(args.plan.read_text()), args.phase)


if __name__ == "__main__":
    main()
