#!/usr/bin/env python3
"""Execute the recorded scope with pinned tools and fixed feature shapes."""
import argparse
import json
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
