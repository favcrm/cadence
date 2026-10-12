#!/usr/bin/env python3
"""CAD-1400 refusal acceptance for tag release installation and publication."""
import importlib.util
from pathlib import Path
import re
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[2]
CI = ROOT / ".github/workflows/ci.yml"
CLEAN_INSTALL = ROOT / "scripts/clean-install.py"


def job_block(workflow, name):
    """Extract one top-level Actions job, ending at the next top-level job."""
    match = re.search(
        rf"(?ms)^  {re.escape(name)}:\n(.*?)(?=^  [A-Za-z0-9_-]+:\n|\Z)",
        workflow,
    )
    if not match:
        raise AssertionError(f"workflow job {name!r} is missing")
    return match.group(1)


def needs_of(job):
    match = re.search(r"(?m)^    needs:\s*(?:\[([^\]]*)\]|([A-Za-z0-9_-]+))\s*$", job)
    if not match:
        raise AssertionError("job has no parseable needs dependency")
    value = match.group(1) or match.group(2)
    return {item.strip().strip("'\"") for item in value.split(",")}


def load_clean_install():
    spec = importlib.util.spec_from_file_location("clean_install", CLEAN_INSTALL)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class Cad1400ReleaseInstallGate(unittest.TestCase):
    def test_publisher_dependency_refuses_missing_or_failed_native_install(self):
        ci = CI.read_text()
        publish = job_block(ci, "release-publish")
        dependencies = needs_of(publish)

        # GitHub Actions' default job condition is success(): every declared
        # need must succeed. No `if: always()` / `if: success() || failure()`
        # escape may override that publication gate. Requiring the producer
        # and native-install jobs here means absent producer jobs fail this
        # acceptance, while failed native matrix legs make release-install
        # fail and therefore keep release-publish from running.
        self.assertIn("release-build", dependencies,
                      "publisher no longer depends on the artifact producer")
        self.assertIn("release-install", dependencies,
                      "publisher does not wait for native install success")
        self.assertNotRegex(publish, r"(?m)^    if:.*(?:always\(\)|failure\(\))")

        install = job_block(ci, "release-install")
        self.assertIn("release-build", needs_of(install),
                      "native install must consume this run's release-build artifacts")
        self.assertIn("${{ github.ref_name }}", install)
        self.assertIn("${{ github.sha }}", install)
        self.assertIn("${{ github.run_id }}", install)
        self.assertIn("${{ github.run_attempt }}", install)
        self.assertIn("scripts/clean-install.py", install)
        self.assertIn("harness", install)
        for target, runner in (
            ("x86_64-linux", "ubuntu-22.04"),
            ("aarch64-linux", "ubuntu-22.04-arm"),
            ("aarch64-macos", "macos-14"),
        ):
            self.assertIn(f"target: {target}", install)
            self.assertIn(f"runner: {runner}", install)

    def test_real_zip_inventory_guard_rejects_unexpected_entry(self):
        verifier = load_clean_install()
        expected = ["cadence-v0.1.0-x86_64-linux.tar.gz",
                    "cadence-v0.1.0-x86_64-linux.tar.gz.sha256"]
        with tempfile.TemporaryDirectory() as tmp:
            archive = Path(tmp) / "artifact.zip"
            with zipfile.ZipFile(archive, "w") as zf:
                for name in expected:
                    zf.writestr(name, b"candidate")
                zf.writestr("unexpected.txt", b"must not be accepted")
            with self.assertRaisesRegex(ValueError, "unexpected ZIP entries"):
                verifier.zip_members(archive, expected)


if __name__ == "__main__":
    unittest.main()
