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

    def test_disposable_runner_opt_in_refreshes_apt_then_installs(self):
        # CAD-1967: a workflow that declares its runner disposable may install
        # mold; a per-job InstaBox machine ships without apt lists, so the
        # install is preceded by an update. Still fails open.
        result, sudo_calls, env = run({
            "GITHUB_ACTIONS": "true",
            "RUNNER_ENVIRONMENT": "self-hosted",
            "CADENCE_CI_LINKER_INSTALL": "1",
        })
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = sudo_calls.splitlines()
        self.assertEqual(len(calls), 2, sudo_calls)
        self.assertTrue(calls[0].startswith("-n apt-get update"), sudo_calls)
        self.assertTrue(calls[1].startswith("-n apt-get install"), sudo_calls)
        self.assertEqual(env, "")

    def test_opt_in_without_actions_still_refuses(self):
        result, sudo_calls, env = run({"CADENCE_CI_LINKER_INSTALL": "1"})
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
