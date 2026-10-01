#!/usr/bin/env python3
"""CAD-922: scripts/pre-push runs the right steps in order, fails fast, and
judges every step by exit code alone. cargo/pnpm/nextest are PATH stubs that
log their argv, so no build runs."""
from pathlib import Path
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
GIT = ["git", "-c", "user.name=t", "-c", "user.email=t@t"]
STUB = """#!/bin/sh
echo "$(basename "$0") $* [jobs=${CARGO_BUILD_JOBS:-}]" >> "$STUB_LOG"
case " $* " in *" $STUB_FAIL "*) [ -n "$STUB_FAIL" ] && { echo clean; exit "${STUB_RC:-1}"; };; esac
exit 0
"""
CI = """jobs:
  fmt:
    steps:
      - run: python3 tests/scripts/test_fake_contract.py
  clippy:
    steps: []
"""
MAP = "[binaries.foo]\ntests = [\n    \"alpha\",\n]\n"


class PrePush(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory(prefix="pp-")
        self.addCleanup(self._tmp.cleanup)
        t = Path(self._tmp.name)
        self.repo, self.bin, self.log = t / "repo", t / "bin", t / "log"
        self.bin.mkdir()
        for tool in ("cargo", "pnpm"):
            p = self.bin / tool
            p.write_text(STUB)
            p.chmod(0o755)
        (self.repo / "scripts").mkdir(parents=True)
        (self.repo / "tests/scripts").mkdir(parents=True)
        (self.repo / ".github/workflows").mkdir(parents=True)
        (self.repo / "docs").mkdir()
        (self.repo / "ui").mkdir()
        shutil.copy(ROOT / "scripts/split-map-sync", self.repo / "scripts/split-map-sync")
        nx = self.repo / "scripts/cadence-nextest"
        nx.write_text(STUB)
        nx.chmod(0o755)
        (self.repo / ".github/workflows/ci.yml").write_text(CI)
        (self.repo / "tests/scripts/test_fake_contract.py").write_text("print('contract ran')\n")
        (self.repo / "tests/split-map.toml").write_text(MAP)
        (self.repo / "tests/split-map-board.toml").write_text("")
        (self.repo / "tests/foo.rs").write_text("#[test]\nfn alpha() {}\n")
        self.git("init", "-q", "-b", "main")
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "base")

    def git(self, *a):
        subprocess.run([*GIT, "-C", str(self.repo), *a], check=True)

    def edit(self, rel, text):
        p = self.repo / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)

    def pp(self, *args, fail="", rc="1", path=None):
        env = dict(os.environ, STUB_LOG=str(self.log), STUB_FAIL=fail, STUB_RC=rc,
                   PATH=path or f"{self.bin}:{os.environ['PATH']}")
        env.pop("CARGO_BUILD_JOBS", None)
        return subprocess.run([sys.executable, str(ROOT / "scripts/pre-push"),
                               "--root", str(self.repo), "--base", "main", *args],
                              capture_output=True, text=True, env=env)

    def calls(self):
        return self.log.read_text().splitlines() if self.log.exists() else []

    def test_rust_change_runs_fmt_splitmap_both_clippies_in_order_with_jobs_4(self):
        self.edit("src/lib.rs", "pub fn x() {}\n")
        r = self.pp()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(self.calls(), [
            "cargo fmt --all -- --check [jobs=4]",
            "cargo clippy --all-targets --locked -- -D warnings [jobs=4]",
            "cargo clippy --all-targets --locked --features test-seam -- -D warnings [jobs=4]",
        ])
        self.assertIn("[ OK ] split-map", r.stdout)
        self.assertIn("pre-push: all ok", r.stdout)
        self.assertIn("[SKIP] ui typecheck", r.stdout)
        self.assertIn("[SKIP] contracts:", r.stdout)

    def test_docs_only_change_skips_clippy_ui_and_contracts(self):
        self.edit("docs/x.md", "hi\n")
        r = self.pp()
        self.assertEqual(r.returncode, 0, r.stdout)
        self.assertEqual(self.calls(), ["cargo fmt --all -- --check [jobs=4]"])
        self.assertIn("[SKIP] clippy: no Rust changes", r.stdout)

    def test_fmt_failure_is_fail_fast(self):
        self.edit("src/lib.rs", "pub fn x() {}\n")
        r = self.pp(fail="fmt", rc="3")
        self.assertEqual(r.returncode, 1)
        self.assertIn("[FAIL] fmt: exit 3", r.stdout)
        self.assertEqual(len(self.calls()), 1, "nothing runs after the first failure")
        self.assertNotIn("split-map", r.stdout)

    def test_unregistered_test_fails_before_any_clippy_naming_the_section(self):
        self.edit("tests/foo.rs", "#[test]\nfn alpha() {}\n#[test]\nfn beta() {}\n")
        r = self.pp()
        self.assertEqual(r.returncode, 1)
        self.assertIn("[FAIL] split-map", r.stdout)
        self.assertIn("tests/split-map.toml [binaries.foo]", r.stdout)
        self.assertEqual(self.calls(), ["cargo fmt --all -- --check [jobs=4]"])

    def test_verdict_is_the_exit_code_not_the_text(self):
        # The stub prints "clean" and exits 1: still a failure.
        self.edit("src/lib.rs", "pub fn x() {}\n")
        r = self.pp(fail="--features", rc="2")
        self.assertEqual(r.returncode, 1)
        self.assertIn("exit 2", r.stdout)
        self.assertIn("[ OK ] split-map", r.stdout)

    def test_missing_tool_is_a_failure_not_a_skip(self):
        self.edit("src/lib.rs", "pub fn x() {}\n")
        only_git = Path(self._tmp.name) / "onlygit"
        only_git.mkdir()
        (only_git / "git").symlink_to(shutil.which("git"))
        r = self.pp(path=str(only_git))
        self.assertEqual(r.returncode, 1)
        self.assertIn("[FAIL] fmt: exit 127", r.stdout)

    def test_ui_change_runs_typecheck(self):
        self.edit("ui/src/a.ts", "export {}\n")
        r = self.pp()
        self.assertEqual(r.returncode, 0, r.stdout)
        self.assertIn("pnpm --dir ui run typecheck [jobs=4]", self.calls())

    def test_scripts_change_runs_ci_fmt_job_contracts(self):
        self.edit("scripts/foo.py", "x = 1\n")
        r = self.pp()
        self.assertEqual(r.returncode, 0, r.stdout)
        self.assertIn("[ OK ] contract test_fake_contract.py", r.stdout)

    def test_failing_contract_fails_the_run(self):
        self.edit("scripts/foo.py", "x = 1\n")
        self.edit("tests/scripts/test_fake_contract.py", "raise SystemExit(5)\n")
        r = self.pp()
        self.assertEqual(r.returncode, 1)
        self.assertIn("[FAIL] contract test_fake_contract.py: exit 5", r.stdout)

    def test_tests_flag_runs_changed_binary_only_with_two_threads(self):
        self.edit("tests/foo.rs", "#[test]\nfn alpha() {}\n// touched\n")
        r = self.pp("--tests")
        self.assertEqual(r.returncode, 0, r.stdout)
        self.assertIn("cadence-nextest --test foo --locked --features test-seam "
                      "--test-threads 2 [jobs=4]", self.calls())
        # Without the flag the binary is not run.
        self.log.unlink()
        r = self.pp()
        self.assertFalse(any("cadence-nextest" in c for c in self.calls()))
        self.assertIn("pass --tests", r.stdout)

    def test_tests_flag_skips_feature_gated_deleted_and_unknown(self):
        self.edit("Cargo.toml", '[[test]]\nname = "gated"\npath = "tests/gated.rs"\n'
                  'required-features = ["e2e"]\n')
        self.edit("tests/gated.rs", "#[test]\nfn g() {}\n")
        (self.repo / "tests/foo.rs").unlink()
        self.edit("tests/split-map.toml", "[common]\nhelpers = []\n")
        r = self.pp("--tests")
        self.assertEqual(r.returncode, 0, r.stdout)
        self.assertIn("[SKIP] tests gated: needs features CI builds separately: e2e", r.stdout)
        self.assertIn("[SKIP] tests foo: file deleted or renamed", r.stdout)
        self.assertFalse(any("cadence-nextest" in c for c in self.calls()))

    def test_tests_flag_with_unknown_changes_is_not_no_tests_changed(self):
        r = subprocess.run([sys.executable, str(ROOT / "scripts/pre-push"), "--root",
                            str(self.repo), "--base", "nope", "--tests", "--list"],
                           capture_output=True, text=True)
        self.assertIn("[SKIP] tests: unknown changes", r.stdout)
        self.assertNotIn("no tests/*.rs changed", r.stdout)

    def test_contract_listed_in_ci_but_missing_fails(self):
        self.edit("scripts/foo.py", "x = 1\n")
        (self.repo / "tests/scripts/test_fake_contract.py").unlink()
        r = self.pp()
        self.assertEqual(r.returncode, 1)
        self.assertIn("[FAIL] contract test_fake_contract.py", r.stdout)

    def test_nextest_wrapper_change_runs_its_contract_test(self):
        # CAD-968: reverting `flock -o` must be caught before CI.
        self.edit("scripts/test-cadence-nextest", STUB)
        (self.repo / "scripts/test-cadence-nextest").chmod(0o755)
        self.edit("scripts/cadence-nextest", STUB + "# touched\n")
        r = self.pp()
        self.assertEqual(r.returncode, 0, r.stdout)
        self.assertIn("test-cadence-nextest  [jobs=4]", self.calls())
        self.assertIn("[ OK ] nextest wrapper", r.stdout)

    def test_failing_nextest_wrapper_test_fails_the_run(self):
        self.edit("scripts/test-cadence-nextest", "#!/bin/sh\necho clean\nexit 4\n")
        (self.repo / "scripts/test-cadence-nextest").chmod(0o755)
        r = self.pp()
        self.assertEqual(r.returncode, 1)
        self.assertIn("[FAIL] nextest wrapper: exit 4", r.stdout)

    def test_unrelated_change_skips_the_nextest_wrapper_test(self):
        self.edit("src/lib.rs", "pub fn x() {}\n")
        r = self.pp()
        self.assertIn("[SKIP] nextest wrapper", r.stdout)
        self.assertFalse(any("test-cadence-nextest" in c for c in self.calls()))

    def test_list_runs_nothing(self):
        self.edit("src/lib.rs", "pub fn x() {}\n")
        r = self.pp("--list")
        self.assertEqual(r.returncode, 0)
        self.assertEqual(self.calls(), [])
        self.assertIn("[PLAN] clippy:", r.stdout)


if __name__ == "__main__":
    unittest.main()
