#!/usr/bin/env python3
"""CAD-1333 reviewer-written refusal check for scripts/lib/operator-preflight.sh.

Written by the reviewer from the ticket, not by the implementer. Each case runs
the guide's own call pattern under `set -euo pipefail`:

    source operator-preflight.sh; <check> || exit 1; echo MUTATE

A refusal must exit non-zero before MUTATE, print exactly one plain
`preflight:` line on stderr, and never echo the planted secret. Every function
also has a positive control, so a blanket refusal cannot pass.

Fakes only: gh, the Cloudflare read-auth probe and the tenant probe are shell
stubs in a temp bin dir; HOME is a temp dir; no network, no real state.
"""
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

LIB = Path(__file__).resolve().parent / "lib" / "operator-preflight.sh"
BASH = shutil.which("bash") or "/bin/bash"
# Built at runtime: a token-shaped fake with no high-entropy source literal.
SECRET = "ghp_" + "PLANTED" + "x" * 29
SHA_A = "a" * 40
SHA_B = "b" * 40


class PreflightRefusals(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="pf"))
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)
        self.home = self.root / "home"
        self.home.mkdir()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.stub("leaky-fail", f'echo "{SECRET}"; echo "{SECRET}" >&2; exit 1')
        self.stub("leaky-ok", f'echo "{SECRET}"; echo "{SECRET}" >&2; exit 0')

    def stub(self, name, body):
        path = self.bin / name
        path.write_text(f"#!/bin/sh\n{body}\n")
        path.chmod(0o755)

    def run_check(self, call, path=None):
        script = (
            "set -euo pipefail\n"
            f'source "{LIB}"\n'
            f"PATH={path or str(self.bin) + ':/usr/bin:/bin'}\n"
            f"{call} || exit 1\n"
            "echo MUTATE\n"
        )
        env = {"HOME": str(self.home), "PATH": os.environ.get("PATH", "/usr/bin:/bin")}
        return subprocess.run([BASH, "-c", script], env=env, capture_output=True,
                              text=True, timeout=30)

    def assert_refused(self, call, reason, path=None):
        r = self.run_check(call, path)
        self.assertNotEqual(r.returncode, 0, f"{call!r} was not refused: {r.stderr}")
        self.assertNotIn("MUTATE", r.stdout, f"{call!r} let the script reach its mutation")
        lines = r.stderr.splitlines()
        self.assertEqual(len(lines), 1, f"{call!r} must give one plain reason, got {lines}")
        self.assertTrue(lines[0].startswith("preflight: "), lines)
        self.assertIn(reason, lines[0])
        self.assertNotIn(SECRET, r.stdout + r.stderr, f"{call!r} leaked the planted secret")

    def assert_allowed(self, call, path=None):
        r = self.run_check(call, path)
        self.assertEqual(r.returncode, 0, f"{call!r} was refused: {r.stderr}")
        self.assertIn("MUTATE", r.stdout)
        self.assertEqual(r.stderr, "")
        self.assertNotIn(SECRET, r.stdout + r.stderr)

    def test_sourcing_has_no_side_effects(self):
        r = subprocess.run([BASH, "-c", f'set -euo pipefail; PATH={self.bin}; source "{LIB}"'],
                           env={"HOME": str(self.home)}, capture_output=True, text=True)
        self.assertEqual((r.returncode, r.stdout, r.stderr), (0, "", ""))
        self.assertEqual(sorted(p.name for p in self.home.iterdir()), [])

    def test_gh_auth(self):
        self.stub("gh", f'echo "token: {SECRET}"; echo "{SECRET}" >&2; exit 1')
        self.assert_refused("preflight_gh_auth", "GitHub")  # expired login
        (self.bin / "gh").unlink()
        self.assert_refused("preflight_gh_auth", "GitHub", path=self.bin)  # gh missing
        self.stub("gh", f'echo "token: {SECRET}"; exit 0')
        self.assert_allowed("preflight_gh_auth")

    def test_cf_auth(self):
        self.assert_refused("preflight_cf_auth", "Cloudflare")  # not configured
        self.assert_refused("preflight_cf_auth no-such-cf-cli whoami", "Cloudflare")
        self.assert_refused("preflight_cf_auth leaky-fail whoami", "Cloudflare")
        self.assert_allowed("preflight_cf_auth leaky-ok whoami")

    def test_sha(self):
        for call in (
            f"preflight_sha {SHA_A} {SHA_B}",  # wrong SHA
            f"preflight_sha {SHA_A[:7]} {SHA_A[:7]}",  # short SHAs, even equal
            f"preflight_sha {SHA_A} {SHA_A[:7]}",
            "preflight_sha nothex nothex",
            "preflight_sha " + "g" * 40 + " " + "g" * 40,
            "preflight_sha '' ''",
            "preflight_sha",
            f"preflight_sha $'{SHA_A}\\n' $'{SHA_A}\\n'",
            f"preflight_sha '{SHA_A} ' '{SHA_A} '",
        ):
            with self.subTest(call=call):
                self.assert_refused(call, "SHA")
        self.assert_allowed(f"preflight_sha {SHA_A} {SHA_A}")
        self.assert_allowed(f"preflight_sha {SHA_A.upper()} {SHA_A}")

    def test_digest(self):
        for call in ("preflight_digest_set ''", "preflight_digest_set", "preflight_digest_set '   '"):
            with self.subTest(call=call):
                self.assert_refused(call, "digest")
        self.assert_allowed("preflight_digest_set sha256:" + "c" * 64)

    def test_state_dir(self):
        prod = self.home / ".local/state/cadence"
        prod.mkdir(parents=True)
        link = self.root / "prod-link"
        link.symlink_to(prod, target_is_directory=True)
        for value in ("''", "relative/state", "./state", f"'{prod}'", f"'{prod}/'",
                      f"'{self.home}/.local/state//cadence'",
                      f"'{self.home}/.local/state/./cadence'", f"'{link}'"):
            with self.subTest(state_dir=value):
                self.assert_refused(f"preflight_state_dir {value}", "state directory")
        self.assert_refused("preflight_state_dir", "state directory")
        sandbox = self.root / "sandbox-state"
        self.assert_allowed(f"preflight_state_dir '{sandbox}'")

    def test_probe(self):
        self.assert_refused("preflight_probe", "probe")  # no probe configured
        self.assert_refused("preflight_probe leaky-fail status", "tenant")  # no answer
        self.assert_refused("preflight_probe no-such-tenant-probe", "tenant")
        self.assert_allowed("preflight_probe leaky-ok status")


if __name__ == "__main__":
    unittest.main()
