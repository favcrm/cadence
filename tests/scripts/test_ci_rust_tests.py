#!/usr/bin/env python3
"""Exercise test-suite sharding: partition args and phase gating.

The partition travels as an explicit CLI argument, never ambient env:
exact-command contract tests broke twice on env leaking across steps.
"""
import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


runner = load("runner", "ci-rust-tests.py")

FULL = {"schema": 1, "mode": "full", "targets": [], "reason": "t"}


class PartitionArgs(unittest.TestCase):
    def test_empty_means_whole_suite(self):
        self.assertEqual(runner.partition_args(""), [])
        self.assertEqual(runner.partition_args("   "), [])

    def test_valid_shard(self):
        self.assertEqual(runner.partition_args("2/8"), ["--partition", "hash:2/8"])
        self.assertEqual(runner.partition_args(" 1/4 "), ["--partition", "hash:1/4"])

    def test_invalid_shapes_refused(self):
        for raw in ("0/8", "9/8", "2/0", "a/b", "1-8", "1/2/3", "--partition", "hash:1/8"):
            with self.assertRaises(ValueError, msg=raw):
                runner.partition_args(raw)


class PhaseGating(unittest.TestCase):
    def run_phase(self, phase, partition=""):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with patch.object(runner.subprocess, "run") as run:
                runner.run(root, FULL, phase, partition)
        return run.call_args.args[0]

    def test_tests_phase_partitions(self):
        command = self.run_phase("tests", "3/8")
        self.assertIn("--partition", command)
        self.assertIn("hash:3/8", command)

    def test_tests_phase_without_partition(self):
        command = self.run_phase("tests")
        self.assertNotIn("--partition", command)

    def test_inventory_rejects_partition(self):
        with tempfile.TemporaryDirectory() as tmp:
            with patch.object(runner.subprocess, "run") as run:
                with self.assertRaises(ValueError):
                    runner.run(Path(tmp), FULL, "inventory", "3/8")
        run.assert_not_called()

    def test_bad_partition_fails_before_running(self):
        with tempfile.TemporaryDirectory() as tmp:
            with patch.object(runner.subprocess, "run") as run:
                with self.assertRaises(ValueError):
                    runner.run(Path(tmp), FULL, "tests", "bogus")
        run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
