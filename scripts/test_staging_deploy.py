#!/usr/bin/env python3
"""Unit tests for staging-deploy.py — runner and HTTP fully mocked."""
import hashlib
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
REL_A = f"{SHA_A}-4242-1"
REL_B = f"{SHA_B}-1-1"


class FakeRunner:
    """Records (argv, env, cwd) and answers by argv shape."""

    def __init__(self, base, sha=SHA_A, run_id=4242, attempt=1):
        self.base = Path(base)
        self.sha = sha
        self.run_id = run_id
        self.attempt = attempt
        self.calls = []
        self.tailscale_rc = 0
        # What `prepare` stamps when it differs from the gh answer —
        # a rerun resolving a newer attempt mid-tick.
        self.prepare_attempt = None
        # Substring of the cadence binary path whose `sandbox up` fails.
        self.fail_up_for = None
        self.seed_rc = 0
        self.reset_rc = 0
        self.pm_dir = self.base / "sandbox" / "staging" / "pm"
        self.state_dir = self.base / "sandbox" / "staging" / "state"

    def __call__(self, argv, env=None, cwd=None):
        argv = [str(a) for a in argv]
        self.calls.append((argv, dict(env or {}), cwd))
        joined = " ".join(argv)
        if argv[:2] == ["gh", "run"]:
            return 0, json.dumps(
                [{"databaseId": self.run_id, "headSha": self.sha,
                  "attempt": self.attempt}]
            ), ""
        if argv[:2] == ["gh", "api"]:
            return 0, json.dumps(
                {"head_sha": self.sha, "run_attempt": self.attempt}
            ), ""
        if "delivery-candidate.py" in joined:
            dest = Path(argv[argv.index("--dest") + 1])
            dest.mkdir(parents=True)
            blob = b"binary-" + dest.name.encode()
            (dest / "cadence").write_bytes(blob)
            (dest / "candidate.json").write_text(json.dumps({
                "source_sha": self.sha,
                "ci_run_id": self.run_id,
                "ci_run_attempt": self.prepare_attempt or self.attempt,
                "sha256": hashlib.sha256(blob).hexdigest(),
            }))
            return 0, "{}", ""
        if "sandbox reset" in joined:
            if self.reset_rc:
                return self.reset_rc, "", "reset: marker unreadable"
            return 0, "{}", ""
        if "sandbox down" in joined:
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
            if self.seed_rc:
                return self.seed_rc, "", "seed: failed mid-way"
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
            return 200, json.dumps({"ok": True, "daemon": "reachable"})
        if url.endswith("/api/meta"):
            return 200, json.dumps({"build_commit": sha})
        raise AssertionError(url)
    return get


def http_down(url, host):
    raise OSError("connection refused")


def make_release(base, rel, sha, run_id, attempt, tamper=False):
    d = Path(base) / "releases" / rel
    d.mkdir(parents=True)
    blob = b"binary-" + rel.encode()
    (d / "cadence").write_bytes(blob)
    (d / "candidate.json").write_text(json.dumps({
        "source_sha": sha, "ci_run_id": run_id,
        "ci_run_attempt": attempt,
        "sha256": hashlib.sha256(blob).hexdigest(),
    }))
    if tamper:
        (d / "cadence").write_bytes(b"tampered")
    return d


class TickTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.base = Path(self.tmp.name)
        self._env = os.environ.copy()
        os.environ["CADENCE_ALIAS"] = "worker-1"
        os.environ["CADENCE_PROFILE"] = "sandbox:other"
        os.environ["CADENCE_PM_DIR"] = "/some/where"
        os.environ["CADENCE_SOCKET"] = "/prod/state/cadence.sock"

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
        self.assertEqual(status["deployed_release"], REL_A)
        self.assertEqual(status["deployed_sha"], SHA_A)
        self.assertEqual(status["ci_run_id"], 4242)
        self.assertEqual(status["ci_run_attempt"], 1)
        self.assertIsNone(status["previous_release"])
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
        cadence = str(self.base / "releases" / REL_A / "cadence")
        self.assertTrue(r.argvs(f"{cadence} sandbox up staging --port 3020"))

    def test_noop_when_release_matches_and_healthy(self):
        (self.base / "status.json").write_text(json.dumps(
            {"deployed_release": REL_A, "deployed_sha": SHA_A}))
        r = FakeRunner(self.base)
        rc = self.deploy(r, http_ok(SHA_A))
        self.assertEqual(rc, 0)
        self.assertFalse(r.argvs("sandbox up"))
        self.assertFalse(r.argvs("delivery-candidate.py"))

    def test_dead_daemon_is_not_a_noop(self):
        # /api/health stays 200 with the daemon gone — the tick must
        # redeploy, not sit on the healthy path forever.
        (self.base / "status.json").write_text(json.dumps(
            {"deployed_release": REL_A, "deployed_sha": SHA_A}))
        make_release(self.base, REL_A, SHA_A, 4242, 1)

        r = FakeRunner(self.base)

        def http_dead_daemon(url, host):
            # The daemon comes back once the sandbox is restarted.
            alive = bool(r.argvs("sandbox up"))
            if url.endswith("/api/health"):
                return 200, json.dumps({
                    "ok": alive,
                    "daemon": "reachable" if alive else "unreachable",
                })
            if url.endswith("/api/meta"):
                return 200, json.dumps({"build_commit": SHA_A})
            raise AssertionError(url)

        rc = self.deploy(r, http_dead_daemon)
        self.assertEqual(rc, 0)
        cadence = str(self.base / "releases" / REL_A / "cadence")
        self.assertTrue(r.argvs(f"{cadence} sandbox down"))
        self.assertTrue(r.argvs(f"{cadence} sandbox up staging --port 3020"))

    def test_noop_retries_a_refused_tailnet(self):
        # Staging live but publication previously refused — the next
        # tick retries `ui tailscale start` without redeploying.
        (self.base / "status.json").write_text(json.dumps(
            {"deployed_release": REL_A, "deployed_sha": SHA_A,
             "tailnet": "refused: opt-in missing"}))
        make_release(self.base, REL_A, SHA_A, 4242, 1)
        r = FakeRunner(self.base)  # tailscale_rc=0 → sharing
        rc = self.deploy(r, http_ok(SHA_A))
        self.assertEqual(rc, 0)
        self.assertFalse(r.argvs("sandbox up"), "no redeploy on a no-op tick")
        self.assertTrue(r.argvs("ui tailscale start"))
        status = json.loads((self.base / "status.json").read_text())
        self.assertTrue(status["tailnet"].startswith("sharing"))

    def test_prepare_returning_a_newer_attempt_is_refused(self):
        # A rerun between selection and prepare must not be deployed
        # under the old attempt's release id.
        r = FakeRunner(self.base)  # gh reports attempt 1
        r.prepare_attempt = 2    # but prepare resolves attempt 2
        with self.assertRaises(sd.Refused):
            self.deploy(r, http_ok(SHA_A))
        self.assertFalse(r.argvs("sandbox up"))
        self.assertFalse(r.argvs("sandbox down"))

    def test_rerun_same_sha_new_attempt_is_a_new_release(self):
        # First deploy at attempt 1.
        r = FakeRunner(self.base, attempt=1)
        self.assertEqual(self.deploy(r, http_ok(SHA_A)), 0)
        # CI reruns the same sha: attempt 2 → a fresh prepare into a
        # new release dir; the old one survives for rollback.
        r2 = FakeRunner(self.base, attempt=2)
        rc = self.deploy(r2, http_ok(SHA_A))
        self.assertEqual(rc, 0)
        rel2 = f"{SHA_A}-4242-2"
        self.assertTrue((self.base / "releases" / REL_A).is_dir())
        self.assertTrue((self.base / "releases" / rel2).is_dir())
        status = json.loads((self.base / "status.json").read_text())
        self.assertEqual(status["deployed_release"], rel2)
        self.assertEqual(status["previous_release"], REL_A)

    def test_tampered_cached_release_is_refused(self):
        rel = REL_A
        make_release(self.base, rel, SHA_A, 4242, 1, tamper=True)
        r = FakeRunner(self.base)
        with self.assertRaises(sd.Refused):
            self.deploy(r, http_down)
        # The tampered binary was never executed.
        for argv, _, _ in r.calls:
            self.assertNotEqual(argv[0], str(self.base / "releases" / rel / "cadence"))
        self.assertFalse(r.argvs("sandbox up"))

    def test_repairing_live_release_keeps_distinct_fallback(self):
        # rel_id == deployed_release but unhealthy: the redeploy's
        # rollback target must stay the recorded older release, not the
        # same candidate it just failed on.
        (self.base / "status.json").write_text(json.dumps({
            "deployed_release": REL_A, "deployed_sha": SHA_A,
            "previous_release": REL_B, "previous_sha": SHA_B}))
        make_release(self.base, REL_A, SHA_A, 4242, 1)
        make_release(self.base, REL_B, SHA_B, 1, 1)
        r = FakeRunner(self.base)
        r.fail_up_for = REL_A
        rc = self.deploy(r, http_down)
        self.assertEqual(rc, 1)
        prev_bin = str(self.base / "releases" / REL_B / "cadence")
        self.assertTrue(
            r.argvs(f"{prev_bin} sandbox up staging --port 3020"),
            "rollback must target the older release")

    def test_run_id_override_pins_the_run(self):
        r = FakeRunner(self.base, sha=SHA_B, run_id=999, attempt=3)
        rc = self.deploy(r, http_ok(SHA_B), run_id=999)
        self.assertEqual(rc, 0)
        self.assertFalse(r.argvs("gh run list"))
        self.assertTrue(r.argvs("gh api"))
        self.assertTrue(r.argvs("--run-id 999"))
        status = json.loads((self.base / "status.json").read_text())
        self.assertEqual(status["deployed_release"], f"{SHA_B}-999-3")
        self.assertEqual(status["ci_run_attempt"], 3)

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
                # An inherited socket override would route these calls at
                # the production daemon.
                self.assertNotIn("CADENCE_SOCKET", env)
                # The sandbox root follows the tick's base — never the
                # permanent live-staging root.
                self.assertEqual(
                    env["CADENCE_SANDBOX_ROOT"], str(self.base / "sandbox"))
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

    def test_seed_failure_resets_the_sandbox_once(self):
        r = FakeRunner(self.base)
        r.seed_rc = 1
        rc = self.deploy(r, http_ok(SHA_A))
        self.assertEqual(rc, 1)
        self.assertTrue(r.argvs("sandbox reset staging"))
        status = json.loads((self.base / "status.json").read_text())
        self.assertIn("staging sandbox reset", status["last_error"])
        self.assertEqual(status["failed_release"], REL_A)
        self.assertIsNone(status["deployed_sha"])

    def test_seed_failure_reset_failure_needs_manual_repair(self):
        r = FakeRunner(self.base)
        r.seed_rc = 1
        r.reset_rc = 2
        rc = self.deploy(r, http_ok(SHA_A))
        self.assertEqual(rc, 1)
        self.assertTrue(r.argvs("sandbox reset staging"))
        status = json.loads((self.base / "status.json").read_text())
        self.assertIn("sandbox reset ALSO failed (2)", status["last_error"])
        self.assertIn("manual repair needed", status["last_error"])
        self.assertEqual(status["failed_release"], REL_A)

    def test_no_reset_once_seeded(self):
        (self.base / "seeded").touch()
        r = FakeRunner(self.base)
        self.assertEqual(self.deploy(r, http_ok(SHA_A)), 0)
        self.assertFalse(r.argvs("sandbox reset"))
        self.assertFalse(r.argvs("seed-pm.sh"))

    def test_tailnet_refusal_is_nonfatal(self):
        r = FakeRunner(self.base)
        r.tailscale_rc = 1
        rc = self.deploy(r, http_ok(SHA_A))
        self.assertEqual(rc, 0)
        status = json.loads((self.base / "status.json").read_text())
        self.assertIn("refused", status["tailnet"])
        self.assertIsNone(status["tailnet_health"])
        self.assertEqual(status["deployed_sha"], SHA_A)

    def test_tailnet_health_recorded_when_sharing(self):
        r = FakeRunner(self.base)  # tailscale_rc=0 → sharing
        rc = self.deploy(r, http_ok(SHA_A))
        self.assertEqual(rc, 0)
        status = json.loads((self.base / "status.json").read_text())
        self.assertEqual(status["tailnet_health"], "ok")

    def test_failed_health_rolls_back_to_previous(self):
        (self.base / "status.json").write_text(json.dumps({
            "deployed_release": REL_B, "deployed_sha": SHA_B,
            "ci_run_id": 1}))
        make_release(self.base, REL_B, SHA_B, 1, 1)
        r = FakeRunner(self.base)
        rc = self.deploy(r, http_down)  # health never answers
        self.assertEqual(rc, 1)
        status = json.loads((self.base / "status.json").read_text())
        self.assertIn("health check failed", status["last_error"])
        self.assertEqual(status["failed_release"], REL_A)
        prev = str(self.base / "releases" / REL_B / "cadence")
        self.assertTrue(r.argvs(f"{prev} sandbox up staging --port 3020"))
        # Rolled back: deployed still names the live release.
        self.assertEqual(status["deployed_release"], REL_B)
        # Never `sandbox reset` on its own.
        self.assertFalse(r.argvs("sandbox reset"))

    def test_failed_up_rolls_back(self):
        (self.base / "status.json").write_text(json.dumps({
            "deployed_release": REL_B, "deployed_sha": SHA_B,
            "ci_run_id": 1}))
        make_release(self.base, REL_B, SHA_B, 1, 1)
        r = FakeRunner(self.base)
        r.fail_up_for = REL_A
        rc = self.deploy(r, http_down)
        self.assertEqual(rc, 1)
        status = json.loads((self.base / "status.json").read_text())
        self.assertIn("daemon start refused", status["last_error"])
        self.assertEqual(status["failed_release"], REL_A)
        prev = str(self.base / "releases" / REL_B / "cadence")
        self.assertTrue(r.argvs(f"{prev} sandbox up staging --port 3020"))
        self.assertEqual(status["deployed_release"], REL_B)

    def test_known_bad_skip_only_while_live(self):
        (self.base / "status.json").write_text(json.dumps({
            "deployed_release": REL_B, "deployed_sha": SHA_B,
            "failed_release": REL_A}))
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
        self.assertNotIn("failed_release", status)
        self.assertEqual(status["deployed_sha"], SHA_A)

    def test_known_bad_recovers_previous_when_board_dead(self):
        (self.base / "status.json").write_text(json.dumps({
            "deployed_release": REL_A, "deployed_sha": SHA_A,
            "failed_release": REL_A,
            "previous_release": REL_B, "previous_sha": SHA_B}))
        make_release(self.base, REL_B, SHA_B, 1, 1)
        r = FakeRunner(self.base)
        rc = self.deploy(r, http_ok(SHA_B))  # board runs SHA_B after recovery
        self.assertEqual(rc, 0)
        prev = str(self.base / "releases" / REL_B / "cadence")
        self.assertTrue(r.argvs(f"{prev} sandbox up staging --port 3020"))
        status = json.loads((self.base / "status.json").read_text())
        self.assertEqual(status["deployed_release"], REL_B)
        self.assertEqual(status["deployed_sha"], SHA_B)

    def test_known_bad_retries_when_nothing_to_recover(self):
        (self.base / "status.json").write_text(json.dumps({
            "deployed_release": None, "deployed_sha": None,
            "failed_release": REL_A}))
        r = FakeRunner(self.base)
        rc = self.deploy(r, http_ok(SHA_A))
        self.assertEqual(rc, 0)
        self.assertTrue(r.argvs("sandbox up staging --port 3020"))
        status = json.loads((self.base / "status.json").read_text())
        self.assertEqual(status["deployed_release"], REL_A)

    def test_prune_keeps_current_and_previous(self):
        (self.base / "status.json").write_text(json.dumps(
            {"deployed_release": REL_B, "deployed_sha": SHA_B}))
        for i in range(8):
            d = self.base / "releases" / f"{i:040d}-{i}-1"
            d.mkdir(parents=True)
            (d / "candidate.json").write_text("{}")
            os.utime(d, (i, i))
        make_release(self.base, REL_B, SHA_B, 1, 1)
        r = FakeRunner(self.base)
        self.assertEqual(self.deploy(r, http_ok(SHA_A)), 0)
        names = {p.name for p in (self.base / "releases").iterdir()}
        self.assertIn(REL_A, names)
        self.assertIn(REL_B, names)
        self.assertLessEqual(len(names), sd.KEEP)

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
