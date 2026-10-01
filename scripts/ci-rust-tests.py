#!/usr/bin/env python3
"""Execute the recorded scope with pinned tools and fixed feature shapes."""
import argparse
import hashlib
import json
import math
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
    """One shard of the suite (`M/N`, 1-based) as (m, n), or None.

    Only the `tests` phase applies a partition — inventory must always
    list every test. Deliberately an explicit argument, never ambient
    env: exact-command contract tests broke twice on env leaking across
    steps.
    """
    if not raw or not raw.strip():
        return None
    text = raw.strip()
    match = re.fullmatch(r"([1-9][0-9]*)/([1-9][0-9]*)", text)
    if not match:
        raise ValueError(f"invalid test partition {raw!r}, want M/N")
    if int(match.group(1)) > int(match.group(2)):
        raise ValueError(f"invalid test partition {raw!r}, shard exceeds total")
    return int(match.group(1)), int(match.group(2))


def load_weights(path):
    """tests/shard-weights.json → {id: seconds}; malformed fails loud."""
    doc = json.loads(Path(path).read_text())
    if not isinstance(doc, dict) or doc.get("schema") != 1:
        raise ValueError(f"{path}: weights schema must be 1")
    weights = doc.get("weights")
    if not isinstance(weights, dict):
        raise ValueError(f"{path}: weights must be an object")
    out = {}
    for key, seconds in weights.items():
        if not isinstance(key, str) or key.count(" ") != 1:
            raise ValueError(f"{path}: weight key must be '<binary-id> <name>': {key!r}")
        if not isinstance(seconds, (int, float)) or isinstance(seconds, bool):
            raise ValueError(f"{path}: non-numeric weight for {key!r}")
        if not math.isfinite(seconds) or seconds < 0:
            raise ValueError(f"{path}: weight must be finite and >= 0 for {key!r}")
        out[key] = float(seconds)
    return out


def assign(tests, weights, n):
    """Deterministic LPT: heaviest first onto the least-loaded shard.

    `tests` is a list of (binary_id, name) pairs. Missing weights get
    the 0.1s default — weights only affect balance, never coverage.
    A non-finite or negative weight poisons the least-loaded scan, so
    it is refused here too, not only in `load_weights`.
    Returns n lists of ids, each sorted.
    """
    for key, seconds in weights.items():
        if not math.isfinite(seconds) or seconds < 0:
            raise ValueError(f"weight must be finite and >= 0 for {key!r}")
    by_weight = sorted(
        tests,
        key=lambda t: (-weights.get(f"{t[0]} {t[1]}", 0.1), f"{t[0]} {t[1]}"),
    )
    shards = [[] for _ in range(n)]
    loads = [0.0] * n
    for binary_id, name in by_weight:
        lightest = min(range(n), key=lambda i: (loads[i], i))
        shards[lightest].append(f"{binary_id} {name}")
        loads[lightest] += weights.get(f"{binary_id} {name}", 0.1)
    return [sorted(shard) for shard in shards]


def filterset(ids):
    """A nextest -E expression selecting exactly `ids` (`<bid> <name>`).

    `binary_id()` rejects quoted strings and a quoted `test(="...")`
    inside a conjunction silently selects nothing on pinned nextest
    0.9.145, so both travel unquoted — allowed characters verified
    against the DSL's bare-argument charset; anything else fails here,
    loudly, and the -E self-check is the second fence.
    """
    clauses = []
    for ident in ids:
        binary_id, name = ident.split(" ", 1)
        for label, part in (("binary id", binary_id), ("test name", name)):
            if not re.fullmatch(r"[A-Za-z0-9_:\-./]+", part):
                raise ValueError(f"{label} needs quoting the DSL refuses: {part!r}")
        clauses.append(f"(binary_id(={binary_id}) & test(={name}))")
    return " | ".join(clauses)


def list_tests(root, args, extra=None):
    """(binary_id, name) of every non-ignored test the scope selects."""
    command = [
        str(root / "scripts/cadence-nextest"),
        "list",
        *args,
        "--locked",
        "--features",
        "test-seam",
        "--message-format",
        "json",
    ]
    if extra:
        command += extra
    out = subprocess.run(command, cwd=root, check=True, capture_output=True, text=True)
    doc = json.loads(out.stdout)
    tests = []
    for binary_id, suite in doc["rust-suites"].items():
        for name, case in suite["testcases"].items():
            # A -E mismatch still lists the testcase, with status
            # "mismatch" — the self-check would silently see extra ids.
            if (
                case["kind"] == "test"
                and not case["ignored"]
                and case["filter-match"]["status"] == "matches"
            ):
                tests.append((binary_id, name))
    return sorted(tests)


def write_assignment(path, shard, total, mode, inventory, tests, weights_sha256):
    ids = sorted(inventory)
    digest = hashlib.sha256(("\n".join(ids)).encode()).hexdigest()
    doc = {
        "schema": 1,
        "shard": shard,
        "total": total,
        "mode": mode,
        "inventory_sha256": digest,
        "weights_sha256": weights_sha256,
        "inventory": ids,
        "tests": sorted(tests),
    }
    Path(path).write_text(json.dumps(doc, indent=2) + "\n")


def run(root, plan, phase, partition="", weights=None, assignment_out=None, dry_run=False):
    args = scope_args(plan)
    shard = partition_args(partition)
    if plan["mode"] == "docs":
        if assignment_out is not None and shard is not None:
            write_assignment(
                assignment_out, shard[0], shard[1], plan["mode"], [], [], ""
            )
        print("Rust execution omitted: " + plan["reason"])
        return
    if phase == "inventory":
        if shard is not None:
            raise ValueError("inventory must never be partitioned")
        command = [str(root / "scripts/nextest-inventory")]
        command += (["all-targets"] if plan["mode"] == "full" else ["selected", *args])
        command += ["--features", "test-seam"]
        subprocess.run(command, cwd=root, check=True)
        return

    (root / "target/nextest/cadence/junit.xml").unlink(missing_ok=True)
    command = [str(root / "scripts/cadence-nextest"), *args, "--locked", "--features", "test-seam"]
    if shard is not None:
        weights_doc = {}
        weights_sha256 = ""
        if weights is not None:
            weights_sha256 = hashlib.sha256(Path(weights).read_bytes()).hexdigest()
            weights_doc = load_weights(weights)
        inventory = list_tests(root, args)
        inventory_ids = [f"{b} {n}" for b, n in inventory]
        shards = assign(inventory, weights_doc, shard[1])
        mine = shards[shard[0] - 1]
        if assignment_out is not None:
            write_assignment(
                assignment_out, shard[0], shard[1], plan["mode"],
                inventory_ids, mine, weights_sha256,
            )
        if not mine:
            print(f"shard {shard[0]}/{shard[1]}: no tests assigned; nextest skipped")
            return
        expr = filterset(mine)
        # Self-check: the filterset must select exactly this shard's
        # assignment — a quoting or escaping bug fails here, never
        # silently drops tests.
        listed = {f"{b} {n}" for b, n in list_tests(root, args, ["-E", expr])}
        if listed != set(mine):
            raise ValueError(
                "filterset self-check mismatch — "
                f"missing: {sorted(set(mine) - listed)}; "
                f"extra: {sorted(listed - set(mine))}"
            )
        command += ["-E", expr]
    if dry_run:
        print("dry run:", *command)
        return
    subprocess.run(command, cwd=root, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--phase", choices=["inventory", "tests"], required=True)
    parser.add_argument("--partition", default="", help="test shard as M/N (tests phase only)")
    parser.add_argument("--weights", default=None, help="shard weights JSON (tests phase only)")
    parser.add_argument("--assignment-out", default=None, help="write this shard's assignment JSON")
    parser.add_argument("--dry-run", action="store_true", help="assign and self-check, don't run")
    args = parser.parse_args()
    run(
        args.root.resolve(),
        json.loads(args.plan.read_text()),
        args.phase,
        args.partition,
        args.weights,
        args.assignment_out,
        args.dry_run,
    )


if __name__ == "__main__":
    main()
