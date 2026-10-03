#!/usr/bin/env python3
"""CAD-1103: ci-fast-linker must never install packages off a hosted runner.

Fakes only: PATH holds a fake `sudo` that logs and fails, and no mold/lld, so
nothing is ever installed and no real linker is probed.
"""
import os
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "ci-fast-linker"


def run(extra):
    with tempfile.TemporaryDirectory(prefix="clk") as root:
        root = Path(root)
        bindir = root / "bin"
        bindir.mkdir()
        log = root / "sudo.log"
        fake = bindir / "sudo"
        fake.write_text(f'#!/bin/sh\nprintf "%s\\n" "$*" >> "{log}"\necho fake-apt-error >&2\nexit 1\n')
        fake.chmod(fake.stat().st_mode | stat.S_IXUSR)
        env = {"PATH": str(bindir), "GITHUB_ENV": str(root / "env")}
        env.update(extra)
        (root / "env").write_text("")
        run = subprocess.run(["/bin/sh", str(SCRIPT), "enable"], env=env,
                             capture_output=True, text=True)
        return run, log.read_text() if log.exists() else "", (root / "env").read_text()


class FastLinkerTests(unittest.TestCase):
    def test_refuses_to_install_off_a_hosted_runner(self):
        for extra in ({}, {"GITHUB_ACTIONS": "true"},
                      {"RUNNER_ENVIRONMENT": "github-hosted"},
                      {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "self-hosted"}):
            with self.subTest(extra=extra):
                result, sudo_calls, env = run(extra)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(sudo_calls, "", "sudo must not be called")
                self.assertEqual(env, "")

    def test_hosted_runner_installs_with_noninteractive_sudo_and_fails_open(self):
        result, sudo_calls, env = run({"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted"})
        self.assertEqual(result.returncode, 0)
        self.assertTrue(sudo_calls.startswith("-n apt-get install"), sudo_calls)
        self.assertIn("fake-apt-error", result.stderr)
        self.assertEqual(env, "")


if __name__ == "__main__":
    unittest.main()
