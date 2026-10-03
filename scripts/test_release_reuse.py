#!/usr/bin/env python3
"""CAD-1131: a different tree (or commit, digest, tamper) never reuses a release artifact."""
import contextlib
import importlib.util
import io
import json
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("release_reuse", ROOT / "scripts/release-reuse.py")
rr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rr)


def git(repo, *args):
    return subprocess.check_output(
        ["git", "-c", "user.name=t", "-c", "user.email=t@t", "-C", str(repo), *args], text=True).strip()


class ReuseTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="rr-")
        self.addCleanup(self.tmp.cleanup)
        self.repo = Path(self.tmp.name) / "repo"
        self.repo.mkdir()
        git(self.repo, "init", "-q")
        (self.repo / "f").write_text("one")
        git(self.repo, "add", "f")
        git(self.repo, "commit", "-q", "-m", "one")
        self.sha = git(self.repo, "rev-parse", "HEAD")
        self.tree = rr.git_tree(self.repo, self.sha)
        self.bin = Path(self.tmp.name) / "built"
        self.bin.write_bytes(b"\x7fELF cadence 1.0+" + self.sha.encode() + b"\0")
        self.dest = Path(self.tmp.name) / "dist"
        self.pack()

    def pack(self, sha=None):
        args = type("A", (), dict(binary=str(self.bin), sha=sha or self.sha, dest=str(self.dest),
                                  rustc="r", cargo="c", run_id=1))
        import os
        cwd = os.getcwd()
        os.chdir(self.repo)
        try:
            with contextlib.redirect_stdout(io.StringIO()):
                rr.pack(args)
        finally:
            os.chdir(cwd)

    def edit(self, **changes):
        path = self.dest / "manifest.json"
        manifest = json.loads(path.read_text())
        manifest.update(changes)
        path.write_text(json.dumps(manifest))

    def test_same_tree_same_commit_is_reused(self):
        digest = rr.verify_candidate(self.dest, self.sha, self.tree)
        self.assertEqual(digest, rr.sha256_of(self.dest / "cadence"))

    def test_different_tree_never_reuses_an_artifact(self):
        (self.repo / "f").write_text("two")
        git(self.repo, "commit", "-aq", "-m", "two")
        sha2 = git(self.repo, "rev-parse", "HEAD")
        tree2 = rr.git_tree(self.repo, sha2)
        self.assertNotEqual(tree2, self.tree)
        # Even an artifact that lies about its commit cannot cross trees.
        # A forged artifact: old tree's manifest, rewritten to claim sha2,
        # carrying sha2 in the binary with a matching digest. Only the tree
        # check can refuse it.
        binary = self.dest / "cadence"
        binary.write_bytes(binary.read_bytes() + sha2.encode())
        digest = rr.sha256_of(binary)
        (self.dest / "cadence.sha256").write_text(f"{digest}  cadence\n")
        self.edit(source_sha=sha2, sha256=digest)
        with self.assertRaisesRegex(ValueError, "different tree"):
            rr.verify_candidate(self.dest, sha2, tree2)

    def test_same_tree_other_commit_is_rebuilt(self):
        git(self.repo, "commit", "-q", "--allow-empty", "-m", "same tree")
        sha2 = git(self.repo, "rev-parse", "HEAD")
        self.assertEqual(rr.git_tree(self.repo, sha2), self.tree)
        with self.assertRaisesRegex(ValueError, "different commit"):
            rr.verify_candidate(self.dest, sha2, self.tree)

    def test_tampered_binary_or_extra_file_is_refused(self):
        (self.dest / "cadence").write_bytes((self.dest / "cadence").read_bytes() + b"x")
        with self.assertRaisesRegex(ValueError, "digest"):
            rr.verify_candidate(self.dest, self.sha, self.tree)
        (self.dest / "extra").write_text("x")
        with self.assertRaisesRegex(ValueError, "exactly"):
            rr.verify_candidate(self.dest, self.sha, self.tree)

    def test_wrong_build_shape_is_refused(self):
        self.edit(features=[])
        with self.assertRaisesRegex(ValueError, "release x86_64-linux ui"):
            rr.verify_candidate(self.dest, self.sha, self.tree)

    def test_checkout_must_be_the_source_sha(self):
        with self.assertRaisesRegex(ValueError, "does not match"):
            rr.git_tree(self.repo, "0" * 40)


if __name__ == "__main__":
    unittest.main()
