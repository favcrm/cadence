#!/usr/bin/env python3
"""CAD-1104: with one reviewer by default, the operator's approval is the second
check, so every one-review path on a risk-paths schema/4/6/7 list must be human."""
import importlib.machinery
import importlib.util
import subprocess
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
loader = importlib.machinery.SourceFileLoader("enq", str(ROOT / "scripts/enqueue-reviewed"))
spec = importlib.util.spec_from_loader("enq", loader)
enq = importlib.util.module_from_spec(spec)
loader.exec_module(enq)

ONE = (ROOT / "docs/roles/one-review-paths.toml").read_text()
RISK_TEXT = (ROOT / "docs/roles/risk-paths.toml").read_text()
RISK = enq.risk_globs(RISK_TEXT)


def one(path):
    return enq.one_review_qualifies([("M", "100644", "100644", path)], ONE)


def examples(glob):
    return [glob.replace("**", "x").replace("*", "x"), glob.replace("/**", "/a/b.rs")]


class HumanPaths(unittest.TestCase):
    def test_every_one_review_risk_path_is_human(self):
        doc = tomllib.loads(RISK_TEXT)
        tracked = subprocess.run(["git", "ls-files"], cwd=ROOT, capture_output=True,
                                 text=True, check=True).stdout.split()
        paths = set(tracked)
        for t in enq.RISK_HUMAN_TABLES:
            for g in doc[t]["paths"]:
                paths.update(examples(g))
        gaps = [p for p in sorted(paths)
                if one(p) and any(enq.glob_match(g, p) for g in RISK)
                and not enq.is_human_path(p, RISK)]
        self.assertEqual(gaps, [])

    def test_trigger_1_and_3_keep_two_reviews(self):
        doc = tomllib.loads(RISK_TEXT)
        for t in ("trigger1", "trigger3"):
            for g in doc[t]["paths"]:
                for p in examples(g):
                    self.assertFalse(one(p), p)

    def test_unreadable_risk_list_fails_closed(self):
        self.assertIsNone(enq.risk_globs(None))
        self.assertIsNone(enq.risk_globs("not = [toml"))
        self.assertIsNone(enq.risk_globs("[schema]\npaths = []\n"))

    def test_examples(self):
        for p in ["src/cli/audit.rs", "src/remote_result_outbox.rs", "docs/design/x.md",
                  "config/production-baseline.json", "src/rollout.rs",
                  "ui/sub/package-lock.json", "apps/x/.github/w.yml"]:
            self.assertTrue(enq.is_human_path(p, RISK), p)
        self.assertFalse(enq.is_human_path("src/issue/reclaim.rs", RISK))

    def test_every_human_path_maps_to_a_trigger(self):
        """CAD-1100: a scope approval needs every human-class path in a
        risk-paths schema/4/6/7 list, so is_human_path must imply a trigger
        glob for the tracked tree AND the classifier's own synthetic space
        (a nested .github/.cargo segment, a human basename anywhere)."""
        doc = tomllib.loads(RISK_TEXT)
        trigger_globs = [g for t in enq.PATH_TRIGGER_TABLES for g in doc[t[0]]["paths"]]
        tracked = subprocess.run(["git", "ls-files"], cwd=ROOT, capture_output=True,
                                 text=True, check=True).stdout.split()
        candidates = set(tracked)
        # The classifier's synthetic space: what it flags even when git
        # tracks no such file today.
        candidates.update([
            "apps/x/package-lock.json", "apps/x/.github/workflows/w.yml",
            "apps/x/.cargo/config.toml", "ui/sub/yarn.lock", "ui/sub/.npmrc",
            "x/bun.lockb", "x/package.json", "x/pnpm-lock.yaml",
            "x/build.rs", "x/rust-toolchain.toml", "config/new.json",
            ".config/new.toml", ".github/new.yml", "docs/roles/new.md",
            "scripts/new", "src/audit/x.rs", "src/delegation/x.rs",
            "tests/common/x.rs", "tests/safety_floor.rs",
            "Cargo.toml", "Cargo.lock", "ui/package.json",
            "cadence-review.toml", "src/review.rs", "docs/CHARTER.md",
            "AGENTS.md", "build.rs", "rust-toolchain.toml", "clippy.toml",
            "src/cli/daemon.rs", "src/audit.rs", "src/delegation.rs",
            "src/issue/delivery_policy.rs", "docs/AUDIT.md",
        ])
        unmapped = sorted(p for p in candidates
                          if enq.is_human_path(p, RISK)
                          and not any(enq.glob_match(g, p) for g in trigger_globs))
        self.assertEqual(unmapped, [])


if __name__ == "__main__":
    unittest.main()
