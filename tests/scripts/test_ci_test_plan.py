#!/usr/bin/env python3
"""Exercise selection fallbacks and the pinned runner without compiling Rust."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


policy = load("policy", "ci-test-plan.py")
runner = load("runner", "ci-rust-tests.py")


class Selection(unittest.TestCase):
    targets = {"tests/foo.rs": "foo", "tests/split_map_inventory.rs": "split_map_inventory"}

    def test_queue_main_tags_always_full_even_for_docs(self):
        for event in ("merge_group", "push", "workflow_dispatch", ""):
            self.assertEqual(policy.select(event, [("M", "docs/CI-DELIVERY.md")], self.targets)["mode"], "full")

    def test_isolated_test_retains_cross_file_inventory(self):
        result = policy.select("pull_request", [("M", "tests/foo.rs")], self.targets)
        self.assertEqual(result["targets"], ["foo", "split_map_inventory"])
        self.assertEqual(runner.scope_args(result), ["--lib", "--bins", "--test", "foo", "--test", "split_map_inventory"])

    def test_documentation_and_unknown_changes(self):
        self.assertEqual(policy.select("pull_request", [("M", "docs/CI-DELIVERY.md")], {})["mode"], "docs")
        for path in ("src/lib.rs", "tests/common/mod.rs", "Cargo.lock", ".github/workflows/ci.yml", "scripts/ci-test-plan.py", "AGENTS.md", "docs/AUDIT.md", "skills/cadence/SKILL.md", "tests/unknown.rs"):
            self.assertEqual(policy.select("pull_request", [("M", path)], self.targets)["mode"], "full", path)
        for status in ("D", "R100", "T", "?"):
            self.assertEqual(policy.select("pull_request", [(status, "docs/CI-DELIVERY.md")], {})["mode"], "full")
        self.assertEqual(policy.select("pull_request", [], {})["mode"], "full")

    def test_nul_diff_preserves_paths_and_rejects_truncation(self):
        self.assertEqual(policy.parse_changes(b"M\0docs/a b.md\0"), [("M", "docs/a b.md")])
        for raw in (b"M\0docs/a.md", b"M\0", b"\xff\0"):
            with self.assertRaises(ValueError):
                policy.parse_changes(raw)

    def test_cross_target_source_reference_forces_full(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "tests").mkdir()
            for text in ('include!("foo.rs");', 'mod foo;', 'pub mod foo;'):
                with self.subTest(text=text):
                    (root / "tests/consumer.rs").write_text(text)
                    with patch.object(policy.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, b"tests/foo.rs\0tests/consumer.rs\0")):
                        self.assertIn("consumer.rs references tests/foo.rs", policy.referenced_changes(root, [("M", "tests/foo.rs")]))

    def test_bad_event_or_missing_tool_falls_back_in_real_cli(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            event = root / "event.json"
            out = root / "plan.json"
            for payload in ({}, {"pull_request": {"base": {"sha": "a" * 40}, "head": {"sha": "b" * 40}}}):
                event.write_text(json.dumps(payload))
                result = subprocess.run(["python3", str(ROOT / "scripts/ci-test-plan.py"), "--root", temp, "--event-name", "pull_request", "--event-file", str(event), "--out", str(out)], capture_output=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(json.loads(out.read_text())["mode"], "full")


class Execution(unittest.TestCase):
    def test_invalid_scope_never_executes(self):
        for plan in ({}, {"schema": 1, "mode": "selected", "targets": []}, {"schema": 1, "mode": "selected", "targets": ["--all-targets"]}, {"schema": 1, "mode": "docs", "targets": ["foo"]}):
            with patch.object(runner.subprocess, "run") as run:
                with self.assertRaises(ValueError):
                    runner.run(ROOT, plan, "tests")
                run.assert_not_called()

    def test_docs_does_not_invoke_rust(self):
        with patch.object(runner.subprocess, "run") as run:
            runner.run(ROOT, {"schema": 1, "mode": "docs", "targets": [], "reason": "docs"}, "tests")
            run.assert_not_called()

    def test_inventory_and_execution_share_scope_and_propagate_failure(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            report = root / "target/nextest/cadence/junit.xml"
            report.parent.mkdir(parents=True)
            report.write_text("stale")
            plan = {"schema": 1, "mode": "selected", "targets": ["foo"]}
            with patch.object(runner.subprocess, "run") as run:
                runner.run(root, plan, "inventory")
                self.assertEqual(run.call_args.args[0][1:], ["selected", "--lib", "--bins", "--test", "foo", "--features", "test-seam"])
                run.side_effect = subprocess.CalledProcessError(100, "nextest")
                with self.assertRaises(subprocess.CalledProcessError):
                    runner.run(root, plan, "tests")
                self.assertEqual(run.call_args.args[0][1:], ["--lib", "--bins", "--test", "foo", "--locked", "--features", "test-seam"])
                self.assertFalse(report.exists())


if __name__ == "__main__":
    unittest.main()
