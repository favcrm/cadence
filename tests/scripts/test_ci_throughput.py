#!/usr/bin/env python3
"""CAD-840: honest timing evidence; manual shard widths never weaken CI."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import textwrap
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/ci-throughput.py"
WORKFLOW = ROOT / ".github/workflows/ci.yml"


def load():
    spec = importlib.util.spec_from_file_location("throughput", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def fixture(width=8, run_id=10):
    run = {"id": run_id, "head_sha": "a" * 40, "event": "workflow_dispatch",
           "path": ".github/workflows/ci.yml", "run_attempt": 1,
           "status": "completed", "conclusion": "success",
           "created_at": "2026-09-29T12:00:00Z", "updated_at": "2026-09-29T12:10:00Z"}
    names = ["fmt", "clippy", "test", "build", "ui", "test-once"]
    names += [f"test-shard ({i})" for i in range(1, width + 1)]
    jobs = [{"name": name, "status": "completed", "conclusion": "success",
             "run_id": run_id, "head_sha": "a" * 40, "run_attempt": 1,
             "started_at": "2026-09-29T12:02:00Z", "completed_at": "2026-09-29T12:06:00Z",
             # GitHub can reset created_at after started_at on a rerun.
             "created_at": "2026-09-29T12:03:00Z",
             "steps": [{"name": "Verify selected inventory parity", "status": "completed",
                        "conclusion": "success", "started_at": "2026-09-29T12:02:00Z",
                        "completed_at": "2026-09-29T12:03:00Z"},
                       {"name": "Run recorded Rust scope with pinned nextest", "status": "completed",
                        "conclusion": "success", "started_at": "2026-09-29T12:03:00Z",
                        "completed_at": "2026-09-29T12:05:00Z"}]} for name in names]
    return run, jobs


class TimingEvidence(unittest.TestCase):
    def setUp(self):
        self.mod = load()

    def test_dispatch_delay_is_not_job_created_at_or_pure_queue(self):
        run, jobs = fixture()
        report = self.mod.summarize(run, jobs)
        self.assertEqual(report["workflow_elapsed_seconds"], 600)
        self.assertEqual(report["runner_seconds"], len(jobs) * 240)
        self.assertEqual(report["jobs"][0]["dispatch_to_start_seconds"], 120)
        self.assertEqual(report["jobs"][0]["execution_seconds"], 240)
        self.assertEqual(report["jobs"][0]["steps"][0]["execution_seconds"], 60)
        self.assertEqual(report["gate_elapsed_seconds"], 360)
        self.assertTrue(report["comparable"])
        self.assertEqual(report["shards"], 8)
        self.assertNotIn("queue_seconds", json.dumps(report))

    def test_four_and_eight_compare_same_sha_without_claiming_causality(self):
        left = self.mod.summarize(*fixture(4, 10))
        right = self.mod.summarize(*fixture(8, 11))
        comparison = self.mod.compare(left, right)
        self.assertEqual(comparison["source_sha"], "a" * 40)
        self.assertEqual(comparison["runner_seconds_delta"], 4 * 240)
        self.assertEqual(comparison["gate_elapsed_seconds_delta"], 0)
        self.assertIn("dispatch-to-start", self.mod.markdown(right))
        self.assertIn("not pure runner queue", self.mod.markdown(right))

    def test_incomplete_failed_or_cancelled_runs_are_not_clean_baselines(self):
        for status, conclusion in [("in_progress", None), ("queued", None),
                                   ("completed", "failure"), ("completed", "cancelled")]:
            run, jobs = fixture()
            run.update(status=status, conclusion=conclusion)
            if status != "completed":
                run["updated_at"] = run["created_at"]
            report = self.mod.summarize(run, jobs)
            self.assertFalse(report["comparable"])
            self.assertTrue(report["comparison_blockers"])
            with self.assertRaises(ValueError):
                self.mod.compare(report, self.mod.summarize(*fixture(4, 20)))

    def test_missing_failed_duplicate_or_extra_shards_refuse_comparison(self):
        for mutate in [lambda js: js.pop(),
                       lambda js: js.append(copy.deepcopy(js[-1])),
                       lambda js: js[-1].update(conclusion="failure"),
                       lambda js: js[-1].update(status="in_progress", completed_at=None),
                       lambda js: js[-1].update(name="test-shard (9)")]:
            run, jobs = fixture()
            mutate(jobs)
            self.assertFalse(self.mod.summarize(run, jobs)["comparable"])

    def test_required_gates_and_once_proofs_cannot_be_missing_or_skipped(self):
        for name in ["fmt", "clippy", "test", "build", "ui", "test-once"]:
            for conclusion in ["failure", "skipped", "cancelled", None]:
                run, jobs = fixture()
                next(j for j in jobs if j["name"] == name)["conclusion"] = conclusion
                self.assertFalse(self.mod.summarize(run, jobs)["comparable"])
            run, jobs = fixture()
            self.assertFalse(self.mod.summarize(run, [j for j in jobs if j["name"] != name])["comparable"])

    def test_skipped_optional_jobs_add_no_runner_time(self):
        run, jobs = fixture()
        jobs.append({"name": "release-artifact", "status": "completed", "conclusion": "skipped",
                     "run_id": 10, "head_sha": "a" * 40, "run_attempt": 1,
                     "started_at": run["created_at"], "completed_at": run["created_at"], "steps": []})
        report = self.mod.summarize(run, jobs)
        self.assertTrue(report["comparable"])
        self.assertEqual(report["runner_seconds"], 14 * 240)

    def test_missing_or_inverted_times_cannot_fake_a_fast_job(self):
        for start, end in [(None, None), ("2026-09-29T12:06:00Z", "2026-09-29T12:02:00Z"),
                           ("not a date", "2026-09-29T12:06:00Z"),
                           ("2026-09-29T11:00:00Z", "2026-09-29T12:06:00Z"),
                           ("2026-09-29T12:02:00", "2026-09-29T12:06:00Z")]:
            run, jobs = fixture()
            jobs[-1].update(started_at=start, completed_at=end)
            self.assertFalse(self.mod.summarize(run, jobs)["comparable"])
        run, jobs = fixture()
        run["updated_at"] = "not a timestamp"
        with self.assertRaises(ValueError):
            self.mod.summarize(run, jobs)

    def test_different_sha_event_width_or_rerun_is_not_a_controlled_pair(self):
        base = self.mod.summarize(*fixture(4))
        for field, value in [("head_sha", "b" * 40), ("event", "pull_request"),
                             ("run_attempt", 2), ("path", ".github/workflows/other.yml")]:
            run, jobs = fixture(8, 11)
            run[field] = value
            with self.assertRaises(ValueError):
                self.mod.compare(base, self.mod.summarize(run, jobs))
        with self.assertRaises(ValueError):
            self.mod.compare(base, self.mod.summarize(*fixture(4, 11)))

    def test_online_fetch_is_paginated_attempt_pinned_and_read_only(self):
        run, jobs = fixture()
        calls = []
        def api(command, **kwargs):
            calls.append(command)
            self.assertEqual(command[:2], ["gh", "api"])
            self.assertTrue(kwargs["check"])
            self.assertEqual(kwargs["timeout"], 120)
            body = run if len(calls) == 1 else [{"jobs": jobs[:3]}, {"jobs": jobs[3:]}]
            return subprocess.CompletedProcess(command, 0, stdout=json.dumps(body))
        with patch.object(self.mod.subprocess, "run", side_effect=api):
            report = self.mod.fetch("favcrm/cadence", 10)
        self.assertTrue(report["comparable"])
        self.assertIn("/attempts/1/jobs?per_page=100", calls[1][2])
        self.assertEqual(calls[1][-2:], ["--paginate", "--slurp"])
        for repo, run_id in [("--write", 10), ("favcrm/cadence", 0)]:
            with self.assertRaises(ValueError):
                self.mod.fetch(repo, run_id)
        with patch.object(self.mod.subprocess, "run", side_effect=subprocess.CalledProcessError(1, "gh")):
            with self.assertRaises(subprocess.CalledProcessError):
                self.mod.fetch("favcrm/cadence", 10)

    def test_jobs_must_belong_to_the_run(self):
        for field, value in [("run_id", 999), ("head_sha", "b" * 40), ("run_attempt", 3),
                             ("run_id", None), ("head_sha", None), ("run_attempt", None)]:
            run, jobs = fixture()
            jobs[3][field] = value
            report = self.mod.summarize(run, jobs)
            self.assertFalse(report["comparable"], (field, value))
            self.assertTrue(any(field in b for b in report["comparison_blockers"]), (field, value))
            run, jobs = fixture()
            del jobs[3][field]
            self.assertFalse(self.mod.summarize(run, jobs)["comparable"], field)

    def test_offline_reports_are_marked_unverified_and_api_reports_are_not(self):
        self.assertEqual(self.mod.summarize(*fixture(), source="offline-unverified")["source"],
                         "offline-unverified")
        self.assertEqual(self.mod.summarize(*fixture())["source"], "api")

    def test_same_run_id_cannot_be_compared_with_itself(self):
        with self.assertRaises(ValueError):
            self.mod.compare(self.mod.summarize(*fixture(4, 10)), self.mod.summarize(*fixture(8, 10)))

    def test_shard_must_have_both_compile_and_test_steps(self):
        for step_name in ["Verify selected inventory parity", "Run recorded Rust scope with pinned nextest"]:
            for mutate in [lambda steps, n: steps.remove(next(x for x in steps if x["name"] == n)),
                           lambda steps, n: next(x for x in steps if x["name"] == n).update(conclusion="failure"),
                           lambda steps, n: steps.append(copy.deepcopy(next(x for x in steps if x["name"] == n)))]:
                run, jobs = fixture()
                shard = next(j for j in jobs if j["name"] == "test-shard (3)")
                mutate(shard["steps"], step_name)
                report = self.mod.summarize(run, jobs)
                self.assertFalse(report["comparable"], step_name)
                self.assertTrue(any("test-shard (3)" in b and step_name in b
                                    for b in report["comparison_blockers"]))

    def test_malformed_steps_are_a_clean_blocker_not_a_traceback(self):
        for bad in [["text"], [None], None, "x", [{"name": "x"}, 3]]:
            run, jobs = fixture()
            jobs[-1]["steps"] = bad
            report = self.mod.summarize(run, jobs)
            self.assertFalse(report["comparable"])
            self.assertTrue(any("malformed steps" in b for b in report["comparison_blockers"]))

    def test_cli_offline_report_says_unverified_and_survives_malformed_steps(self):
        with tempfile.TemporaryDirectory(prefix="cad840-") as directory:
            root = Path(directory)
            run, jobs = fixture()
            jobs[-1]["steps"] = ["junk"]
            (root / "run.json").write_text(json.dumps(run))
            (root / "jobs.json").write_text(json.dumps({"jobs": jobs}))
            out = subprocess.run(["python3", str(SCRIPT), "--run", str(root / "run.json"),
                                  "--jobs", str(root / "jobs.json"), "--json"],
                                 capture_output=True, text=True)
            self.assertEqual(out.returncode, 0, out.stderr)
            doc = json.loads(out.stdout)
            self.assertEqual(doc["source"], "offline-unverified")
            self.assertFalse(doc["comparable"])
            self.assertNotIn("Traceback", out.stderr)

    def test_cli_cross_run_offline_pair_is_refused(self):
        with tempfile.TemporaryDirectory(prefix="cad840-") as directory:
            root = Path(directory)
            run4, jobs4 = fixture(4, 10)
            run8, jobs8 = fixture(8, 11)
            for name, doc in [("r4", run4), ("j4", {"jobs": jobs8}), ("r8", run8), ("j8", {"jobs": jobs8})]:
                (root / f"{name}.json").write_text(json.dumps(doc))
            out = subprocess.run(["python3", str(SCRIPT), "--run", str(root / "r4.json"),
                                  "--jobs", str(root / "j4.json"), "--compare-run", str(root / "r8.json"),
                                  "--compare-jobs", str(root / "j8.json"), "--json"],
                                 capture_output=True, text=True)
            self.assertNotEqual(out.returncode, 0)

    def test_cli_offline_fixture_and_failure_exit_codes(self):
        with tempfile.TemporaryDirectory(prefix="cad840-") as directory:
            root = Path(directory)
            run, jobs = fixture()
            (root / "run.json").write_text(json.dumps(run))
            (root / "jobs.json").write_text(json.dumps({"jobs": jobs}))
            cmd = ["python3", str(SCRIPT), "--run", str(root / "run.json"),
                   "--jobs", str(root / "jobs.json"), "--json"]
            out = subprocess.run(cmd, capture_output=True, text=True)
            self.assertEqual(out.returncode, 0, out.stderr)
            self.assertEqual(json.loads(out.stdout)["shards"], 8)
            (root / "run.json").write_text("{}")
            self.assertNotEqual(subprocess.run(cmd, capture_output=True).returncode, 0)


class WorkflowContract(unittest.TestCase):
    def setUp(self):
        self.text = WORKFLOW.read_text()

    def test_reporter_step_names_exist_in_the_workflow(self):
        mod = load()
        for step in (mod.COMPILE_STEP, mod.TEST_STEP):
            self.assertIn(f"- name: {step}\n", self.text)

    def test_manual_only_width_expression_and_choice(self):
        self.assertIn("  workflow_dispatch:\n", self.text)
        self.assertRegex(self.text, r"test_shards:\n(?:.*\n)*?\s+type: choice")
        self.assertIn('options: ["4", "8"]', self.text)
        self.assertIn('default: "8"', self.text)
        self.assertIn("github.event_name == 'workflow_dispatch' && inputs.test_shards == '4'", self.text)
        self.assertIn("fromJSON(github.event_name == 'workflow_dispatch' && inputs.test_shards == '4'", self.text)
        self.assertIn("  CADENCE_CI_SHARDS: ${{ github.event_name == 'workflow_dispatch' && inputs.test_shards == '4' && '4' || '8' }}", self.text)
        self.assertIn("shard: ${{ fromJSON(github.event_name == 'workflow_dispatch' && inputs.test_shards == '4' && '[1,2,3,4]' || '[1,2,3,4,5,6,7,8]') }}", self.text)
        self.assertIn("--partition \"${{ matrix.shard }}/$CADENCE_CI_SHARDS\"", self.text)
        self.assertIn('--total "$CADENCE_CI_SHARDS"', self.text)
        self.assertIn("benchmark-{0}", self.text)

    def test_invalid_dispatch_choice_fails_actual_shell(self):
        block = self.text.split("- name: Validate manual benchmark width\n", 1)[1]
        block = re.split(r"\n      - ", block, maxsplit=1)[0]
        shell = block.split("        run: |\n", 1)[1]
        for event, width, accepted in [("workflow_dispatch", "4", True),
                                       ("workflow_dispatch", "8", True),
                                       ("workflow_dispatch", "5", False),
                                       ("workflow_dispatch", "4;exit 0", False),
                                       ("pull_request", "", True)]:
            env = {**os.environ, "GITHUB_EVENT_NAME": event, "BENCHMARK_SHARDS": width}
            out = subprocess.run(["bash", "-eo", "pipefail", "-c", textwrap.dedent(shell)], env=env,
                                 capture_output=True)
            self.assertEqual(out.returncode == 0, accepted)

    def test_report_contract_runs_in_fast_fmt_gate(self):
        fmt = self.text.split("  fmt:\n", 1)[1].split("\n  clippy:", 1)[0]
        self.assertIn("python3 tests/scripts/test_ci_throughput.py", fmt)
        self.assertIn("cargo fmt --all -- --check", fmt)


if __name__ == "__main__":
    unittest.main()
