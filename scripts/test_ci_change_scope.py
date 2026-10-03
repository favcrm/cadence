#!/usr/bin/env python3
"""Observable CI-selection contracts using temporary Git repos, no network."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parent / "ci-change-scope.py"


class ScopeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="scope-")
        self.repo = Path(self.tmp.name)
        self.git("init", "-q")
        self.git("config", "user.name", "scope-test")
        self.git("config", "user.email", "scope@example.invalid")
        (self.repo / "seed").write_text("seed")
        self.base = self.commit()

    def tearDown(self):
        self.tmp.cleanup()

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.repo), *args],
                                       stderr=subprocess.PIPE, text=True).strip()

    def commit(self):
        self.git("add", "-A")
        self.git("commit", "-qm", "fixture")
        return self.git("rev-parse", "HEAD")

    def change(self, paths):
        self.git("reset", "--hard", self.base)
        self.git("clean", "-fdq")
        for path in paths:
            file = self.repo / path
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_text("fixture")
        return self.commit()

    def scope(self, head, event="pull_request", base=None, env=None, repo=None):
        run = subprocess.run([sys.executable, "-I", str(SCRIPT), "--repo", str(repo or self.repo),
                              "--base", base or self.base, "--head", head, "--event", event],
                             capture_output=True, text=True, env=env)
        self.assertEqual(run.returncode, 0, run.stderr)
        return json.loads(run.stdout)

    def expect(self, result, scope):
        self.assertEqual(result["scope"], scope, result)
        self.assertEqual(result["rust"], "true" if scope == "full" else "false")
        self.assertEqual(result["ui"], "false" if scope == "docs" else "true")

    def test_isolated_docs_and_ui_feedback(self):
        for paths, expected in ((["docs/GUIDE.md", "README.md"], "docs"),
                                (["ui/src/main.ts", "ui/src/theme.css", "ui/public/icon.svg"], "ui")):
            with self.subTest(paths=paths):
                self.expect(self.scope(self.change(paths)), expected)

    def test_shared_unknown_and_mixed_inputs_require_full(self):
        cases = (["src/lib.rs"], [".github/workflows/ci.yml"], ["scripts/task.py"],
                 ["Cargo.toml"], ["AGENTS.md"], ["docs/roles/risk.md"],
                 ["docs/design/spec.md"], ["docs/security.md"], ["docs/TEAM.md"],
                 ["ui/package.json"], ["ui/pnpm-lock.yaml"], ["ui/vite.config.ts"],
                 ["ui/tsconfig.json"], ["ui/src/unknown.wasm"],
                 ["docs/GUIDE.md", "ui/src/main.ts"], ["docs/GUIDE.md", "src/lib.rs"])
        for paths in cases:
            with self.subTest(paths=paths):
                self.expect(self.scope(self.change(paths)), "full")

    def test_merge_queue_and_non_pr_events_always_validate_full(self):
        head = self.change(["docs/GUIDE.md"])
        for event in ("merge_group", "push", "workflow_dispatch", "pull_request_target", "unknown"):
            with self.subTest(event=event):
                self.expect(self.scope(head, event), "full")

    def test_invalid_missing_and_empty_inputs_cannot_select_fast_path(self):
        head = self.change(["docs/GUIDE.md"])
        for sha in ("invalid", "0" * 40, "HEAD", "$(touch forged)", self.git("rev-parse", "HEAD^{tree}")):
            with self.subTest(sha=sha):
                self.expect(self.scope(head, base=sha), "full")
        self.expect(self.scope(self.base), "full")
        self.expect(self.scope(head, repo=self.repo / "absent"), "full")
        self.expect(self.scope(head, env=dict(os.environ, PATH="/absent")), "full")
        self.assertFalse((self.repo / "forged").exists())
        malformed = subprocess.run([sys.executable, "-I", str(SCRIPT), "--scope", "docs"],
                                   capture_output=True, text=True)
        self.assertNotEqual(malformed.returncode, 0)

    def test_deletion_rename_symlink_and_executable_changes_require_full(self):
        self.change(["docs/old.md"])
        base = self.git("rev-parse", "HEAD")
        old = self.repo / "docs/old.md"
        for action in ("delete", "rename", "symlink", "executable"):
            with self.subTest(action=action):
                self.git("reset", "--hard", base)
                self.git("clean", "-fdq")
                if action == "delete":
                    old.unlink()
                elif action == "rename":
                    old.rename(self.repo / "docs/new.md")
                elif action == "symlink":
                    old.unlink()
                    old.symlink_to("../seed")
                else:
                    old.chmod(0o755)
                self.expect(self.scope(self.commit(), base=base), "full")


if __name__ == "__main__":
    unittest.main()
