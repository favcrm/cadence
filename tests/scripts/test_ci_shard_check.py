#!/usr/bin/env python3
"""Exercise the shard-assignment coverage gate the `test` job runs."""
import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CHECK = ROOT / "scripts" / "ci-shard-check.py"


def assignment(shard, total, inventory, tests, weights_sha256="w", mode="full"):
    ids = sorted(inventory)
    return {
        "schema": 1,
        "shard": shard,
        "total": total,
        "mode": mode,
        "inventory_sha256": hashlib.sha256("\n".join(ids).encode()).hexdigest(),
        "weights_sha256": weights_sha256,
        "inventory": ids,
        "tests": sorted(tests),
    }


class ShardCheck(unittest.TestCase):
    def check(self, docs, total=None):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for doc in docs:
                (root / f"shard-assignment-1-shard-{doc['shard']}.json").write_text(
                    json.dumps(doc)
                )
            return subprocess.run(
                [sys.executable, str(CHECK), "--dir", str(root),
                 "--total", str(total if total is not None else len(docs))],
                capture_output=True,
                text=True,
            )

    def docs2(self, inventory=None):
        inventory = inventory if inventory is not None else ["b t1", "b t2"]
        return [
            assignment(1, 2, inventory, inventory[:1]),
            assignment(2, 2, inventory, inventory[1:]),
        ]

    def test_happy_path(self):
        self.assertEqual(self.check(self.docs2()).returncode, 0)

    def test_missing_shard(self):
        r = self.check(self.docs2()[:1], total=2)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("missing shard", r.stderr)

    def test_duplicate_shard(self):
        docs = self.docs2() + [self.docs2()[0]]
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for i, doc in enumerate(docs):
                (root / f"shard-assignment-1-shard-{doc['shard']}-{i}.json").write_text(
                    json.dumps(doc)
                )
            r = subprocess.run(
                [sys.executable, str(CHECK), "--dir", str(root), "--total", "2"],
                capture_output=True,
                text=True,
            )
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("twice", r.stderr)

    def test_digest_mismatch(self):
        docs = self.docs2()
        docs[1]["inventory_sha256"] = "0" * 64
        self.assertNotEqual(self.check(docs).returncode, 0)

    def test_inventory_mismatch(self):
        docs = self.docs2()
        docs[1] = assignment(2, 2, ["b t1", "b t2", "b t3"], ["b t2"])
        self.assertNotEqual(self.check(docs).returncode, 0)

    def test_overlap(self):
        docs = self.docs2()
        docs[1] = assignment(2, 2, ["b t1", "b t2"], ["b t1", "b t2"])
        r = self.check(docs)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("overlap", r.stderr)

    def test_missing_test(self):
        docs = self.docs2()
        docs[1] = assignment(2, 2, ["b t1", "b t2"], [])
        r = self.check(docs)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("missing", r.stderr)

    def test_extra_test(self):
        docs = self.docs2()
        docs[1] = assignment(2, 2, ["b t1", "b t2"], ["b t2", "b tX"])
        r = self.check(docs)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("extra", r.stderr)

    def test_tampered_inventory(self):
        docs = self.docs2()
        docs[0]["inventory"] = ["b t1", "b t2", "b t3"]  # sha untouched
        r = self.check(docs)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("inventory_sha256", r.stderr)




class DocsEndToEnd(unittest.TestCase):
    """The real runner + real checker on a docs-mode plan: eight empty
    assignments, coverage verified, no nextest needed (CAD-809)."""

    def test_docs_shards_produce_a_verifiable_assignment(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            plan = root / "plan.json"
            plan.write_text(
                json.dumps({"schema": 1, "mode": "docs", "targets": [], "reason": "docs only"})
            )
            # PATH without any cadence-nextest: an invocation would fail.
            env = {k: v for k, v in __import__("os").environ.items() if k != "CADENCE_NEXTTEST_BIN"}
            for shard in range(1, 9):
                out = root / f"shard-assignment-1-shard-{shard}.json"
                r = subprocess.run(
                    [
                        sys.executable,
                        str(ROOT / "scripts" / "ci-rust-tests.py"),
                        "--root", str(ROOT),
                        "--plan", str(plan),
                        "--phase", "tests",
                        "--partition", f"{shard}/8",
                        "--weights", str(ROOT / "tests" / "shard-weights.json"),
                        "--assignment-out", str(out),
                    ],
                    env=env, capture_output=True, text=True,
                )
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertIn("omitted", r.stdout)
            r = subprocess.run(
                [sys.executable, str(CHECK), "--dir", str(root), "--total", "8"],
                capture_output=True, text=True,
            )
            self.assertEqual(r.returncode, 0, r.stderr)

    def test_docs_check_fails_when_a_shard_is_missing(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            plan = root / "plan.json"
            plan.write_text(
                json.dumps({"schema": 1, "mode": "docs", "targets": [], "reason": "docs only"})
            )
            for shard in range(1, 8):
                out = root / f"shard-assignment-1-shard-{shard}.json"
                subprocess.run(
                    [
                        sys.executable,
                        str(ROOT / "scripts" / "ci-rust-tests.py"),
                        "--root", str(ROOT),
                        "--plan", str(plan),
                        "--phase", "tests",
                        "--partition", f"{shard}/8",
                        "--assignment-out", str(out),
                    ],
                    capture_output=True, text=True, check=True,
                )
            r = subprocess.run(
                [sys.executable, str(CHECK), "--dir", str(root), "--total", "8"],
                capture_output=True, text=True,
            )
            self.assertNotEqual(r.returncode, 0)
            self.assertIn("missing shard", r.stderr)


if __name__ == "__main__":
    unittest.main()
