#!/usr/bin/env python3
"""Unit tests for staging-deploy.py — runner and HTTP fully mocked."""
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest

import importlib.util

_spec = importlib.util.spec_from_file_location(
    "staging_deploy", Path(__file__).resolve().parent / "staging-deploy.py"
)
sd = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(sd)

SHA_A = "a" * 40
SHA_B = "b" * 40


class FakeRunner:
    """Records (argv, env) and answers by argv shape."""

    def __init__(self, base, sha=SHA_A, run_id=4242):
        self.base = Path(base)
        self.sha = sha
        self.run_id = run_id
        self.calls = []
        self.tailscale_rc = 0
        # Substring of the cadence binary path whose `sandbox up` fails.
        self.fail_up_for = None
        self.pm_dir = self.base / "sandbox" / "staging" / "pm"
        self.state_dir = self.base / "sandbox" / "staging" / "state"

    def __call__(self, argv, env=None, cwd=None):
        argv = [str(a) for a in argv]
        self.calls.append((argv, dict(env or {}), cwd))
        joined = " ".join(argv)
        if argv[:2] == ["gh", "run"]:
            return 0, json.dumps(
                [{"databaseId": self.run_id, "headSha": self.sha}]
            ), ""
        if argv[:2] == ["gh", "api"]:
            return 0, json.dumps({"head_sha": self.sha}), ""
        if "delivery-candidate.py" in joined:
            dest = Path(argv[argv.index("--dest") + 1])
            dest.mkdir(parents=True)
            (dest / "candidate.json").write_text(json.dumps({"source_sha": self.sha}))
            (dest / "cadence").write_text("binary")
            return 0, "{}", ""
        if argv[-3:-1] == ["sandbox", "down"] or joined.endswith("sandbox down staging"):
            if not (self.state_dir).exists():
                return 1, "", f"no sandbox '{sd.NAME}' at {self.base}/sandbox/{sd.NAME}"
            return 0, "{}", ""
        if "sandbox up" in joined:
            if self.fail_up_for and self.fail_up_for in argv[0]:
                return 1, "", "daemon start refused"
            self.state_dir.mkdir(parents=True, exist_ok=True)
            return 0, '{"port": 3020}', ""
        if "sandbox env" in joined:
            return 0, (
                f"unset CADENCE_ALIAS CADENCE_ROLLOUT_AS\n"
                f"export CADENCE_STATE_DIR='{self.state_dir}'\n"
                f"export CADENCE_PM_DIR='{self.pm_dir}'\n"
                f"export CADENCE_PROFILE='sandbox:staging'\n"
            ), ""
        if "seed-pm.sh" in joined:
            pm = Path(argv[-1])
            pm.mkdir(parents=True, exist_ok=True)
            (pm / "pm.yaml").write_text("seeded")
            return 0, "", ""
        if " agent register " in joined:
            return 0, "{}", ""
        if "ui tailscale status" in joined:
            return 0, "tailscale sharing: off\n", ""
        if "ui tailscale start" in joined:
            if self.tailscale_rc:
                return self.tailscale_rc, "", (
                    "`ui tailscale start` is global to this host — refused under "
                    "CADENCE_PROFILE=sandbox:staging; to allow it, restart the "
                    "sandbox with the opt-in"
                )
            return 0, '{"state": "sharing"}', ""
        if argv[:2] == ["git", "init"]:
            (Path(argv[2]) / ".git").mkdir(parents=True, exist_ok=True)
            return 0, "", ""
        if argv[:2] == ["git", "-C"]:
            return 0, "", ""
        return 0, "{}", ""

    def argvs(self, needle):
        return [a for a, _, _ in self.calls if needle in " ".join(a)]


def http_ok(sha):
    def get(url, host):
        if url.endswith("/api/health"):
            return 200, "{}"
        if url.endswith("/api/meta"):
            return 200, json.dumps({"build_commit": sha})
        raise AssertionError(url)
    return get


def http_down(url, host):
    raise OSError("connection refused")


class TickTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.base = Path(self.tmp.name)
        self._env = os.environ.copy()
        os.environ["CADENCE_ALIAS"] = "worker-1"
        os.environ["CADENCE_PROFILE"] = "sandbox:other"
        os.environ["CADENCE_PM_DIR"] = "/some/where"

    def tearDown(self):
        os.environ.clear()
        os.environ.update(self._env)
        self.tmp.cleanup()

    def deploy(self, runner, http=None, run_id=None):
        return sd.tick(runner, http or http_down, base=self.base, run_id=run_id)

    def test_full_first_deploy_seeds_and_writes_status(self):
        r = FakeRunner(self.base)
        rc = self.deploy(r, http_ok(SHA_A))
        self.assertEqual(rc, 0)
        status = json.loads((self.base / "status.json").read_text())
        self.assertEqual(status["deployed_sha"], SHA_A)
        self.assertEqual(status["ci_run_id"], 4242)
        self.assertIsNone(status["previous_sha"])
        self.assertIsNone(status["last_error"])
        self.assertEqual(status["url_loopback"], "http://cadence-3020.localhost:3020")
        self.assertIn("9460", status["url_tailnet"])
        self.assertIn("sharing", status["tailnet"])
        # Seeded once: repos init'd, seed script + two inbox agents.
        self.assertTrue((self.base / "seeded").exists())
        self.assertEqual(len(r.argvs("seed-pm.sh")), 1)
        seed_call = next(c for c in r.calls if "seed-pm.sh" in " ".join(c[0]))
        self.assertEqual(Path(seed_call[2]), self.base / "repos" / "cadence")
        self.assertEqual(seed_call[1]["SEED_ALLOW_INITIALIZED"], "1")
        self.assertEqual(seed_call[1]["SEED_REPO_ROOT"], str(self.base / "repos"))
        self.assertEqual(len(r.argvs("agent register")), 2)
        # The verified release binary, never anything else, is executed.
        cadence = str(self.base / "releases" / SHA_A / "cadence")
        self.assertTrue(r.argvs(f"{cadence} sandbox up staging --port 3020"))

    def test_noop_when_sha_matches_and_healthy(self):
        (self.base / "status.json").write_text(json.dumps({"deployed_sha": SHA_A}))
        r = FakeRunner(self.base)
        rc = self.deploy(r, http_ok(SHA_A))
        self.assertEqual(rc, 0)
        self.assertFalse(r.argvs("sandbox up"))
        self.assertFalse(r.argvs("delivery-candidate.py"))

    def test_run_id_override_pins_the_run(self):
        r = FakeRunner(self.base, sha=SHA_B, run_id=999)
        rc = self.deploy(r, http_ok(SHA_B), run_id=999)
        self.assertEqual(rc, 0)
        self.assertFalse(r.argvs("gh run list"))
        self.assertTrue(r.argvs("gh api"))
        self.assertTrue(r.argvs("--run-id 999"))

    def test_child_env_is_scrubbed_and_sandboxed(self):
        r = FakeRunner(self.base)
        self.deploy(r, http_ok(SHA_A))
        # Commands run under the caller env must carry neither the
        # caller's profile nor pm dir, and must carry the staging root.
        for argv, env, _ in r.calls:
            joined = " ".join(argv)
            if "sandbox up" in joined or "sandbox down" in joined:
                self.assertNotIn("CADENCE_ALIAS", env)
                self.assertNotIn("CADENCE_PROFILE", env)
                self.assertNotIn("CADENCE_PM_DIR", env)
                self.assertEqual(env["CADENCE_SANDBOX_ROOT"], str(sd.BASE / "sandbox"))
                self.assertEqual(env["CADENCE_SANDBOX_ALLOW_GLOBAL"], "1")
        # The sandbox env itself provides profile/pm — not the caller's.
        for argv, env, _ in r.calls:
            if "agent register" in " ".join(argv):
                self.assertEqual(env["CADENCE_PROFILE"], "sandbox:staging")
                self.assertTrue(env["CADENCE_PM_DIR"].startswith(str(self.base)))

    def test_seed_pm_dir_assertion_blocks_escape(self):
        r = FakeRunner(self.base)
        r.pm_dir = Path("/home/ubuntu/pm")  # forged — must be refused
        rc = self.deploy(r, http_ok(SHA_A))
        self.assertEqual(rc, 1)
        self.assertFalse((self.base / "seeded").exists())
        status = json.loads((self.base / "status.json").read_text())
        self.assertIn("escapes", status["last_error"])

    def test_tailnet_refusal_is_nonfatal(self):
        r = FakeRunner(self.base)
        r.tailscale_rc = 1
        rc = self.deploy(r, http_ok(SHA_A))
        self.assertEqual(rc, 0)
        status = json.loads((self.base / "status.json").read_text())
        self.assertIn("refused", status["tailnet"])
        self.assertEqual(status["deployed_sha"], SHA_A)

    def test_failed_health_rolls_back_to_previous(self):
        (self.base / "status.json").write_text(
            json.dumps({"deployed_sha": SHA_B, "ci_run_id": 1})
        )
        old = self.base / "releases" / SHA_B
        old.mkdir(parents=True)
        (old / "cadence").write_text("old")
        (old / "candidate.json").write_text("{}")
        r = FakeRunner(self.base)
        rc = self.deploy(r, http_down)  # health never answers
        self.assertEqual(rc, 1)
        status = json.loads((self.base / "status.json").read_text())
        self.assertIn("health check failed", status["last_error"])
        prev = str(old / "cadence")
        self.assertTrue(r.argvs(f"{prev} sandbox up staging --port 3020"))
        # Never `sandbox reset` on its own.
        self.assertFalse(r.argvs("sandbox reset"))

    def test_prune_keeps_current_and_previous(self):
        (self.base / "status.json").write_text(
            json.dumps({"deployed_sha": SHA_B})
        )
        for i in range(8):
            d = self.base / "releases" / (f"{i:040d}")
            d.mkdir(parents=True)
            (d / "candidate.json").write_text("{}")
            os.utime(d, (i, i))
        (self.base / "releases" / SHA_B).mkdir(exist_ok=True)
        (self.base / "releases" / SHA_B / "candidate.json").write_text("{}")
        r = FakeRunner(self.base)
        self.assertEqual(self.deploy(r, http_ok(SHA_A)), 0)
        names = {p.name for p in (self.base / "releases").iterdir()}
        self.assertIn(SHA_A, names)
        self.assertIn(SHA_B, names)
        self.assertLessEqual(len(names), sd.KEEP)

    def test_failed_up_rolls_back(self):
        (self.base / "status.json").write_text(
            json.dumps({"deployed_sha": SHA_B, "ci_run_id": 1})
        )
        old = self.base / "releases" / SHA_B
        old.mkdir(parents=True)
        (old / "cadence").write_text("old")
        (old / "candidate.json").write_text("{}")
        r = FakeRunner(self.base)
        r.fail_up_for = SHA_A
        rc = self.deploy(r, http_down)
        self.assertEqual(rc, 1)
        status = json.loads((self.base / "status.json").read_text())
        self.assertIn("daemon start refused", status["last_error"])
        self.assertEqual(status["failed_sha"], SHA_A)
        # Rolled back onto the previous release binary — the sandbox is
        # not left down.
        prev = str(old / "cadence")
        self.assertTrue(r.argvs(f"{prev} sandbox up staging --port 3020"))

    def test_known_bad_sha_is_skipped_but_pin_bypasses(self):
        (self.base / "status.json").write_text(
            json.dumps({"deployed_sha": SHA_B, "failed_sha": SHA_A})
        )
        r = FakeRunner(self.base)
        rc = self.deploy(r, http_ok(SHA_B))
        self.assertEqual(rc, 0)
        self.assertFalse(r.argvs("sandbox down"))
        self.assertFalse(r.argvs("delivery-candidate.py"))
        # --run-id is the operator's override: it deploys anyway.
        r2 = FakeRunner(self.base, run_id=777)
        rc = self.deploy(r2, http_ok(SHA_A), run_id=777)
        self.assertEqual(rc, 0)
        self.assertTrue(r2.argvs("sandbox up"))
        status = json.loads((self.base / "status.json").read_text())
        self.assertNotIn("failed_sha", status)
        self.assertEqual(status["deployed_sha"], SHA_A)

    def test_port_3010_is_refused(self):
        old = sd.PORT
        sd.PORT = 3010
        try:
            with self.assertRaises(sd.Refused):
                self.deploy(FakeRunner(self.base))
        finally:
            sd.PORT = old

    def test_lock_contention_exits_cleanly(self):
        import fcntl
        self.base.mkdir(parents=True, exist_ok=True)
        held = open(self.base / "deploy.lock", "w")
        fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
        r = FakeRunner(self.base)
        self.assertEqual(self.deploy(r), 0)
        self.assertEqual(r.calls, [])
        held.close()


if __name__ == "__main__":
    unittest.main()
