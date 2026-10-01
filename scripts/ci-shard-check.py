#!/usr/bin/env python3
"""Verify the shard assignment artifacts cover the inventory exactly.

Usage: scripts/ci-shard-check.py --dir DIR --total N

Reads DIR/shard-assignment-<run>-shard-<m>.json files written by each
test-shard job's --assignment-out. Fails unless shards 1..N each appear
exactly once, every file agrees on total/mode/inventory_sha256/
weights_sha256/inventory, each file's inventory_sha256 matches its own
inventory, the shard test lists are pairwise disjoint, and their union
equals the inventory. Docs mode means all-empty lists, consistently.
"""
import argparse
import hashlib
import json
from pathlib import Path
import sys


def fail(msg):
    print(f"shard-check: {msg}", file=sys.stderr)
    raise SystemExit(1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dir", type=Path, required=True)
    parser.add_argument("--total", type=int, required=True)
    args = parser.parse_args()
    if args.total < 1:
        fail("total must be positive")

    shards = {}
    for path in sorted(args.dir.rglob("shard-assignment-*-shard-*.json")):
        doc = json.loads(path.read_text())
        if doc.get("schema") != 1:
            fail(f"{path}: schema must be 1")
        shard = doc.get("shard")
        if type(shard) is not int or not 1 <= shard <= args.total:
            fail(f"{path}: bad shard {shard!r}; expected 1..{args.total}")
        if shard in shards:
            fail(f"shard {shard} reported twice")
        shards[shard] = (path, doc)

    missing = [m for m in range(1, args.total + 1) if m not in shards]
    if missing:
        fail(f"missing shard assignments: {missing} (have {sorted(shards)})")

    reference = None
    union = []
    seen = set()
    for shard in sorted(shards):
        path, doc = shards[shard]
        if doc.get("total") != args.total:
            fail(f"{path}: total {doc.get('total')!r} != {args.total}")
        if reference is None:
            reference = {k: doc.get(k) for k in ("mode", "inventory_sha256", "weights_sha256", "inventory")}
        else:
            for key, want in reference.items():
                if doc.get(key) != want:
                    fail(f"{path}: {key} differs across shards")
        inventory = doc.get("inventory") or []
        digest = hashlib.sha256("\n".join(inventory).encode()).hexdigest()
        if digest != doc.get("inventory_sha256"):
            fail(f"{path}: inventory_sha256 does not match its inventory")
        overlap = seen.intersection(doc.get("tests") or [])
        if overlap:
            fail(f"shard {shard} overlaps earlier shards: {sorted(overlap)[:3]}")
        seen.update(doc.get("tests") or [])
        union.extend(doc.get("tests") or [])

    if sorted(union) != sorted(reference["inventory"]):
        missing_tests = sorted(set(reference["inventory"]) - set(union))
        extra_tests = sorted(set(union) - set(reference["inventory"]))
        fail(f"shard union != inventory; missing {missing_tests[:3]}, extra {extra_tests[:3]}")

    print(f"shard-check: {args.total} shards cover {len(union)} tests exactly")


if __name__ == "__main__":
    main()
