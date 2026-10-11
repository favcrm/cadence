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

    MANIFEST = '[package]\nname = "scope-fixture"\nversion = "0.1.0"\n'

    def base_with_tests(self, manifest=MANIFEST, extra=()):
        """Commit a base holding a clean autodiscovery manifest + targets."""
        self.git("reset", "--hard", self.base)
        self.git("clean", "-fdq")
        (self.repo / "Cargo.toml").write_text(manifest)
        for path in ("src/lib.rs", "tests/alpha.rs", "tests/beta.rs") + extra:
            file = self.repo / path
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_text("base-content")  # so `change` edits stay visible
        self.base = self.commit()

    def test_isolated_tests_feedback_selects_owned_targets(self):
        self.base_with_tests()
        result = self.scope(self.change(["tests/alpha.rs"]))
        self.assertEqual(result["scope"], "tests", result)
        self.assertEqual(result["rust"], "true")
        self.assertEqual(result["ui"], "false")
        self.assertEqual(result["tests"], "true")
        self.assertEqual(result["targets"], ["alpha"])
        result = self.scope(self.change(["tests/alpha.rs", "tests/beta.rs"]))
        self.assertEqual(result["scope"], "tests", result)
        self.assertEqual(result["targets"], ["alpha", "beta"])

    def test_tests_scope_refuses_shared_new_deleted_and_mixed(self):
        self.base_with_tests()
        # An added test file is not a pre-existing target.
        self.expect(self.scope(self.change(["tests/alpha.rs", "tests/gamma.rs"])), "full")
        # Mixed tests + anything else is full.
        self.expect(self.scope(self.change(["tests/alpha.rs", "src/lib.rs"])), "full")
        self.expect(self.scope(self.change(["tests/alpha.rs", "docs/GUIDE.md"])), "full")
        # A shared basename: another base .rs source references it.
        self.base_with_tests(extra=("src/consumer.rs",))
        (self.repo / "src/consumer.rs").write_text("// see tests/alpha.rs\n")
        self.git("add", "src/consumer.rs")
        self.git("commit", "-qm", "share")
        self.base = self.git("rev-parse", "HEAD")
        # Editing the shared target alone already forces full...
        self.expect(self.scope(self.change(["tests/alpha.rs"])), "full")
        # ...while the independent sibling still narrows.
        result = self.scope(self.change(["tests/beta.rs"]))
        self.assertEqual(result["scope"], "tests", result)
        self.assertEqual(result["targets"], ["beta"])

    def test_tests_scope_refuses_ambiguous_base_manifest(self):
        # An explicit [[test]] table or autotests = false makes target name
        # to file mapping ambiguous; nested tests/**/main.rs is a target too.
        for extra in ('\n[[test]]\nname = "alpha"\npath = "tests/alpha.rs"\n',
                      'autotests = false\n'):
            with self.subTest(manifest=extra):
                self.base_with_tests(manifest=self.MANIFEST + extra)
                self.expect(self.scope(self.change(["tests/alpha.rs"])), "full")
        self.base_with_tests(extra=("tests/nested/main.rs",))
        self.expect(self.scope(self.change(["tests/alpha.rs"])), "full")

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
