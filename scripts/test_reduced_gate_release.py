#!/usr/bin/env python3
"""CAD-1073: reduced-gate commits cannot publish or stage release bytes."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
GUARD = ROOT / "scripts/require-full-gates"
MARKER = ".github/reduced-gates"


class ReducedGateReleaseTest(unittest.TestCase):
    def test_guard_checks_committed_marker_and_exact_checkout(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            def git(*args):
                return subprocess.check_output(["git", "-C", str(repo), *args], text=True).strip()
            git("init", "-q")
            git("config", "user.name", "Test")
            git("config", "user.email", "test@example.invalid")
            (repo / ".github").mkdir()
            (repo / MARKER).write_text("reduced gates active\n")
            git("add", ".")
            git("commit", "-qm", "reduced")
            reduced = git("rev-parse", "HEAD")
            env = dict(os.environ)
            def check(sha):
                return subprocess.run([str(GUARD), str(repo), sha], env=env,
                                      text=True, capture_output=True)
            self.assertNotEqual(check(reduced).returncode, 0)
            (repo / MARKER).unlink()  # working-tree deletion cannot defeat the source-tree marker
            self.assertNotEqual(check(reduced).returncode, 0)
            git("rm", "-q", MARKER)
            git("commit", "-qm", "full gates restored")
            full = git("rev-parse", "HEAD")
            self.assertEqual(check(full).returncode, 0)
            self.assertNotEqual(check(reduced).returncode, 0)  # stale SHA cannot authorize
            self.assertNotEqual(check("invalid").returncode, 0)
            self.assertNotEqual(check("0" * 40).returncode, 0)

    def test_release_and_staging_guard_before_any_candidate_or_build(self):
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        staging = (ROOT / ".github/workflows/staging.yml").read_text()
        def job(source, name, end):
            return source.split(f"  {name}:\n", 1)[1].split(f"  {end}:\n", 1)[0]
        artifact = job(ci, "release-artifact", "release-attest")
        tag = job(ci, "release-gate", "release-build")
        self.assertIn("github.ref == 'refs/heads/main'", artifact)
        self.assertIn("startsWith(github.ref, 'refs/tags/v')", tag)
        for body, before in ((artifact, "pnpm/action-setup"), (tag, "scripts/ci-rust-toolchain")):
            self.assertRegex(body, r"scripts/require-full-gates")
            self.assertLess(body.index("scripts/require-full-gates"), body.index(before))
            self.assertRegex(body, r"SOURCE_SHA: \$\{\{ github.sha \}\}")
        for name, end, before in (("select", "stage", "scripts/auto-stage.py select"),
                                  ("stage", "promote", "delivery-candidate.py prepare")):
            body = job(staging, name, end)
            self.assertIn("scripts/require-full-gates", body)
            self.assertLess(body.index("scripts/require-full-gates"), body.index(before))
            self.assertIn("SOURCE_SHA: ${{ github.sha }}", body)
        stage = job(staging, "stage", "promote")
        candidate_checkout = stage.index("ref: ${{ steps.candidate.outputs.source_sha }}")
        candidate_guard = stage.index('control/scripts/require-full-gates "$GITHUB_WORKSPACE/candidate-source"')
        self.assertLess(candidate_checkout, candidate_guard)
        self.assertLess(candidate_guard, stage.index("pnpm/action-setup"))
        self.assertIn("SOURCE_SHA: ${{ steps.candidate.outputs.source_sha }}", stage)
        promote = staging.split("  promote:\n", 1)[1]
        self.assertIn("scripts/require-full-gates", promote)
        self.assertLess(promote.index("scripts/require-full-gates"),
                        promote.index("delivery-candidate.py prepare"))
        self.assertIn("SOURCE_SHA: ${{ github.sha }}", promote)
        # CAD-1102 ended the CAD-1073 window, so the marker is absent on main.
        # The guard stays wired above; the first test proves a committed
        # marker still refuses release when a future window adds one.


if __name__ == "__main__":
    unittest.main()
