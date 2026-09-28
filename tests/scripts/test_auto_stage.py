"""Bounded automatic staging selector: eligibility, baseline, dedup and receipts."""
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "scripts/auto-stage.py"


def load():
    spec = importlib.util.spec_from_file_location("autostage", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


mod = load()
NOW = mod.utcnow()
GOOD_SHA = "a" * 40


def run(run_id=42, attempt=1, sha=GOOD_SHA, conclusion="success", status="completed",
        created="2026-09-28T19:52:04Z", repo="favcrm/cadence", **extra):
    base = {"id": run_id, "run_attempt": attempt, "path": ".github/workflows/ci.yml",
            "head_sha": sha, "head_branch": "main", "event": "push",
            "status": status, "conclusion": conclusion, "created_at": created,
            "repository": {"full_name": repo}, "head_repository": {"full_name": repo}}
    base.update(extra)
    return base


def baseline(run_id=7, sha="b" * 40):
    return {"schema": 1, "ci_run_id": run_id, "source_sha": sha,
            "sha256": "c" * 64, "verified_by": "operator:staging",
            "verified_at": "2026-09-28T00:00:00Z", "evidence": "staging run 1"}


class SelectTests(unittest.TestCase):
    def test_newest_success_picked_failed_and_fork_runs_refused(self):
        runs = [run(44, conclusion="failure"),
                run(43, repo="fork/cadence"),
                run(42)]
        selected, refused = mod.select_candidate(runs, "favcrm/cadence", NOW, 24)
        self.assertEqual(selected["id"], 42)
        self.assertEqual([r[0] for r in refused], [44, 43])

    def test_stale_runs_outside_train_window_refused(self):
        runs = [run(42, created="2026-09-20T00:00:00Z")]
        with self.assertRaises(mod.NoCandidateError):
            mod.select_candidate(runs, "favcrm/cadence", NOW, 24)

    def test_empty_window_raises_no_candidate(self):
        with self.assertRaises(mod.NoCandidateError):
            mod.select_candidate([], "favcrm/cadence", NOW, 24)

    def test_candidate_off_main_refused(self):
        with self.assertRaises(mod.NoCandidateError):
            mod.check_on_main("diverged")
        mod.check_on_main("identical")
        mod.check_on_main("ahead")


class BaselineTests(unittest.TestCase):
    def write(self, directory, data):
        path = Path(directory) / "baseline.json"
        path.write_text(data if isinstance(data, str) else json.dumps(data))
        return path

    def test_missing_baseline_refuses(self):
        with self.assertRaisesRegex(mod.BaselineError, "missing-baseline"):
            mod.load_baseline("/nonexistent/baseline.json")

    def test_ambiguous_and_unverified_baselines_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            multi = self.write(directory, {"baselines": [baseline(), baseline()]})
            with self.assertRaisesRegex(mod.BaselineError, "ambiguous-baseline"):
                mod.load_baseline(multi)
            bogus = baseline()
            bogus.pop("sha256")
            missing = self.write(directory, bogus)
            with self.assertRaisesRegex(mod.BaselineError, "unverified-baseline"):
                mod.load_baseline(missing)

    def test_baseline_sha_mismatch_and_failed_run_refused(self):
        entry = baseline()
        with self.assertRaisesRegex(mod.BaselineError, "not match"):
            mod.verify_baseline_run(entry, run(7, sha="d" * 40))
        with self.assertRaisesRegex(mod.BaselineError, "identity checks"):
            mod.verify_baseline_run(entry, run(7, sha="b" * 40, conclusion="failure"))
        with self.assertRaisesRegex(mod.BaselineError, "not found"):
            mod.verify_baseline_run(entry, None)
        self.assertTrue(mod.verify_baseline_run(entry, run(7, sha="b" * 40)))


class ClassifyTests(unittest.TestCase):
    def candidate(self, run_id=42, attempt=1):
        return {"ci_run_id": run_id, "ci_run_attempt": attempt}

    def test_duplicate_selection_deduplicated(self):
        state = {"inflight": [], "receipts": [
            {"decision": "staged", "staging_run_id": 9,
             "candidate": self.candidate()}]}
        decision, reason, supersedes, deferred = mod.classify(
            self.candidate(), baseline(), state, 10)
        self.assertEqual(decision, "duplicate")
        self.assertIn("9", reason)
        self.assertIsNone(deferred)

    def test_inflight_staging_run_coalesces_to_next_tick(self):
        state = {"inflight": [11], "receipts": []}
        decision, reason, supersedes, deferred = mod.classify(
            self.candidate(), baseline(), state, 10)
        self.assertEqual(decision, "skip")
        self.assertEqual(deferred, 11)

    def test_current_run_never_blocks_itself(self):
        state = {"inflight": [10], "receipts": []}
        decision, _, _, deferred = mod.classify(self.candidate(), baseline(), state, 10)
        self.assertEqual(decision, "stage")
        self.assertIsNone(deferred)

    def test_superseded_candidates_recorded_not_promoted(self):
        state = {"inflight": [], "receipts": [
            {"decision": "failed", "staging_run_id": 8,
             "candidate": self.candidate(run_id=41, attempt=2)}]}
        decision, _, supersedes, _ = mod.classify(self.candidate(), baseline(), state, 10)
        self.assertEqual(decision, "stage")
        self.assertEqual(supersedes[0]["ci_run_id"], 41)

    def test_moving_main_cannot_change_pinned_candidate(self):
        selected = run(42, sha=GOOD_SHA)
        receipt = mod.selection_receipt(
            "stage", "ok", candidate=mod.candidate_identity(selected, "d" * 64),
            baseline=baseline(), main_head_sha="e" * 40, repo="favcrm/cadence",
            window_hours=24)
        self.assertEqual(receipt["candidate"]["source_sha"], GOOD_SHA)
        self.assertEqual(receipt["main_head_sha"], "e" * 40)
        self.assertEqual(receipt["idempotency_key"], "candidate:42:1:dddddddddddd")

    def test_wrong_digest_or_attempt_breaks_the_pin(self):
        with self.assertRaises(ValueError):
            mod.check_digest_pin({"sha256": "d" * 64}, "e" * 64)
        with self.assertRaises(ValueError):
            mod.check_digest_pin({}, "e" * 64)
        mod.check_digest_pin({"sha256": "d" * 64}, "d" * 64)


class ReceiptTests(unittest.TestCase):
    def test_failing_gate_blocks_staging_with_reason(self):
        receipt = mod.staging_receipt(
            "schedule", 10, 1,
            {"ci_run_id": 42, "ci_run_attempt": 1, "source_sha": GOOD_SHA, "sha256": "d" * 64},
            baseline(),
            {"mvp_journey": "pass", "migration_rehearsal": "fail", "digest_recheck": "pass"})
        self.assertEqual(receipt["decision"], "failed")
        self.assertIn("migration_rehearsal", receipt["reason"])
        self.assertEqual(receipt["idempotency_key"], "candidate:42:1:dddddddddddd")

    def test_all_gates_pass_stages(self):
        receipt = mod.staging_receipt(
            "workflow_dispatch", 10, 1,
            {"ci_run_id": 42, "ci_run_attempt": 1, "source_sha": GOOD_SHA, "sha256": "d" * 64},
            baseline(),
            {"mvp_journey": "pass", "migration_rehearsal": "pass", "digest_recheck": "pass"})
        self.assertEqual((receipt["decision"], receipt["reason"]),
                         ("staged", "all staging gates passed on the pinned bytes"))

    def test_staging_receipt_cli_maps_gh_outcomes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            candidate = root / "candidate.json"
            candidate.write_text(json.dumps({"ci_run_id": 42, "ci_run_attempt": 1,
                                             "source_sha": GOOD_SHA, "sha256": "d" * 64}))
            out = root / "receipt.json"
            argv = ["staging-receipt", "--trigger", "schedule", "--staging-run-id", "10",
                    "--staging-run-attempt", "1", "--candidate-json", str(candidate),
                    "--baseline-json", str(root / "absent.json"),
                    "--mvp", "pass", "--rehearsal", "skip", "--digest-recheck", "pass",
                    "--expected-digest", "d" * 64, "--out", str(out)]
            mod.main(argv)
            receipt = json.loads(out.read_text())
            self.assertEqual(receipt["decision"], "failed")
            self.assertIn("migration_rehearsal", receipt["reason"])
            self.assertTrue(receipt["baseline"]["unverified"])

    def test_staging_receipt_cli_rejects_digest_swap(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            candidate = root / "candidate.json"
            candidate.write_text(json.dumps({"ci_run_id": 42, "ci_run_attempt": 1,
                                             "source_sha": GOOD_SHA, "sha256": "e" * 64}))
            out = root / "receipt.json"
            mod.main(["staging-receipt", "--trigger", "schedule", "--staging-run-id", "10",
                      "--staging-run-attempt", "1", "--candidate-json", str(candidate),
                      "--baseline-json", str(root / "absent.json"),
                      "--mvp", "pass", "--rehearsal", "pass", "--digest-recheck", "pass",
                      "--expected-digest", "d" * 64, "--out", str(out)])
            receipt = json.loads(out.read_text())
            self.assertEqual(receipt["decision"], "failed")
            self.assertEqual(receipt["gates"]["digest_recheck"], "fail")


if __name__ == "__main__":
    unittest.main()
