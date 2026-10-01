#!/usr/bin/env python3
"""CAD-905: scripts/split-map-sync derives split-map test lists from sources."""
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/split-map-sync"

SRC = """use common::*;

#[test]
fn alpha() {}

#[test]
#[ignore = "slow"]
fn beta() {}

fn helper_not_a_test() {}
"""
MAP = """# header

[common]
helpers = [
    "helper_not_a_test",
]

[binaries.foo]
tests = [
    "alpha",
    "beta",
]

[binaries.bar]
tests = [
    "gamma",
]
"""
BAR = "#[test]\nfn gamma() {}\n"


def run(root, *args):
    return subprocess.run([sys.executable, str(SCRIPT), "--root", str(root), *args],
                          capture_output=True, text=True)


class SplitMapSync(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory(prefix="sms-")
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)
        (self.root / "tests").mkdir()
        (self.root / "tests/foo.rs").write_text(SRC)
        (self.root / "tests/bar.rs").write_text(BAR)
        (self.root / "tests/split-map.toml").write_text(MAP)
        (self.root / "tests/split-map-board.toml").write_text("[common]\nhelpers = []\n")

    def manifest(self):
        return tomllib.loads((self.root / "tests/split-map.toml").read_text())

    def test_in_sync_passes(self):
        r = run(self.root, "--check")
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_unregistered_test_fails_naming_file_section_and_test(self):
        (self.root / "tests/foo.rs").write_text(SRC + "\n#[test]\nfn delta() {}\n")
        r = run(self.root, "--check")
        self.assertEqual(r.returncode, 1)
        self.assertIn("tests/split-map.toml", r.stderr)
        self.assertIn("[binaries.foo]", r.stderr)
        self.assertIn('"delta"', r.stderr)
        self.assertIn("scripts/split-map-sync", r.stderr)
        # --check never writes.
        self.assertEqual((self.root / "tests/split-map.toml").read_text(), MAP)

    def test_sync_writes_missing_entries_and_check_then_passes(self):
        (self.root / "tests/foo.rs").write_text(SRC + "\n#[test]\nfn delta() {}\n")
        r = run(self.root)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.manifest()["binaries"]["foo"]["tests"],
                         ["alpha", "beta", "delta"])
        self.assertEqual(self.manifest()["binaries"]["bar"]["tests"], ["gamma"])
        self.assertEqual(self.manifest()["common"]["helpers"], ["helper_not_a_test"])
        self.assertEqual(run(self.root, "--check").returncode, 0)

    def test_deleted_test_is_stale_and_removed(self):
        (self.root / "tests/foo.rs").write_text(SRC.replace(
            "#[test]\n#[ignore = \"slow\"]\nfn beta() {}\n", ""))
        r = run(self.root, "--check")
        self.assertEqual(r.returncode, 1)
        self.assertIn('remove "beta"', r.stderr)
        self.assertEqual(run(self.root).returncode, 0)
        self.assertEqual(self.manifest()["binaries"]["foo"]["tests"], ["alpha"])

    def test_moved_test_stays_registered_exactly_once(self):
        (self.root / "tests/foo.rs").write_text(SRC.replace("fn beta() {}", "fn beta() {}\n#[test]\nfn gamma() {}"))
        (self.root / "tests/bar.rs").write_text("")
        self.assertEqual(run(self.root).returncode, 0)
        m = self.manifest()["binaries"]
        self.assertEqual(m["foo"]["tests"], ["alpha", "beta", "gamma"])
        self.assertEqual(m["bar"]["tests"], [])

    def test_section_without_source_file_fails(self):
        (self.root / "tests/bar.rs").unlink()
        r = run(self.root, "--check")
        self.assertEqual(r.returncode, 1)
        self.assertIn("[binaries.bar]", r.stderr)

    def test_board_manifest_is_covered_too(self):
        (self.root / "tests/board_x.rs").write_text("#[test]\nfn b1() {}\n")
        (self.root / "tests/split-map-board.toml").write_text(
            "[binaries.board_x]\ntests = [\n]\n")
        r = run(self.root, "--check")
        self.assertEqual(r.returncode, 1)
        self.assertIn("tests/split-map-board.toml", r.stderr)
        self.assertIn("[binaries.board_x]", r.stderr)

    def test_repo_manifests_are_in_sync(self):
        r = run(ROOT, "--check")
        self.assertEqual(r.returncode, 0, r.stderr)


if __name__ == "__main__":
    unittest.main()
