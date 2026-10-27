#!/usr/bin/env python3
"""CAD-1745: bind reports to actual executions, not carried job aliases.

CAD-1758: a queued/in_progress shard index row may be reconciled with its
current job detail. The fixture transport serves that one read as raw REST
only; reconciliation and artifact selection stay in the real CLI.

Independent acceptance from the ticket and observed GitHub API contract.
Only gh's read-only transport is simulated. The real selector CLI and
existing JUnit validator execute in subprocesses. No implementation is
imported, and no workflow/source-text assertions are used.
"""
import copy
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

SCRIPTS = Path(__file__).resolve().parent
SELECTOR = SCRIPTS / "select-shard-artifacts.py"
CHECKER = SCRIPTS / "check-test-junit.py"
REPO = "acceptance-fixture/shard-reports"
RUN = 73190427
SHA = "1e" * 20
OTHER_SHA = "2f" * 20
PACKAGE = "acceptance-fixture"
ORIGINAL_IDS = (640901, 640902, 640903, 640904, 640905, 640906)
MIXED_IDS = (640901, 640902, 640903, 640904, 640905, 420017)

# Raw REST transport only: no job reconciliation or artifact selection.
FAKE_GH = r'''
import json
import os
from pathlib import Path
import sys
from urllib.parse import parse_qs, urlsplit

root = Path(os.environ["SHARD_API_FIXTURE"])
fixture = json.loads((root / "api.json").read_text())


def unexpected(message):
    (root / "transport-error").write_text(message)
    print(message, file=sys.stderr)
    raise SystemExit(97)


args = sys.argv[1:]
if not args or args.pop(0) != "api":
    unexpected("Only gh api is available in this acceptance fixture")
endpoint = None
params = {}
method = "GET"
paginate = False
slurp = False
i = 0
while i < len(args):
    arg = args[i]
    if arg == "--paginate":
        paginate = True
    elif arg == "--slurp":
        slurp = True
    elif arg in ("-X", "--method", "-H", "--header", "-f", "--raw-field",
                 "-F", "--field", "--hostname"):
        i += 1
        if i >= len(args):
            unexpected("Missing gh option value")
        value = args[i]
        if arg in ("-X", "--method"):
            method = value.upper()
        elif arg in ("-f", "--raw-field", "-F", "--field"):
            key, separator, val = value.partition("=")
            if not separator:
                unexpected("Malformed field")
            params[key] = val
        elif arg == "--hostname" and value != "github.com":
            unexpected("Unexpected API host")
    elif arg.startswith("-"):
        unexpected("Unsupported gh option: " + arg)
    elif endpoint is None:
        endpoint = arg
    else:
        unexpected("Unexpected positional argument")
    i += 1

if method != "GET" or endpoint is None:
    unexpected("Only read-only API listing requests are allowed")
url = urlsplit(endpoint)
if url.netloc and url.netloc != "api.github.com":
    unexpected("Unexpected endpoint host")
query = {key: values[-1] for key, values in parse_qs(url.query).items()}
query.update(params)
prefix = "/repos/{}/actions/runs/{}/".format(fixture["repo"], fixture["run"])
path = "/" + url.path.lstrip("/")
detail_prefix = "/repos/{}/actions/jobs/".format(fixture["repo"])
detail_id = None
if path.startswith(detail_prefix) and path[len(detail_prefix):].isdigit():
    detail_id = int(path[len(detail_prefix):])
if detail_id is not None:
    # Native single-job lookup: one JSON object, no pagination/slurp wrapper.
    if detail_id in fixture.get("fail_details", []):
        print("synthetic job detail read failure", file=sys.stderr)
        raise SystemExit(1)
    record = fixture.get("job_details", {}).get(str(detail_id))
    if record is None:
        print("synthetic job detail unavailable", file=sys.stderr)
        raise SystemExit(1)
    print(json.dumps(record))
    raise SystemExit(0)
if path == prefix + "jobs":
    key = "jobs"
    rows = fixture[key]
    mode = query.get("filter", "latest")
    if mode == "latest":
        latest = max((row["run_attempt"] for row in rows), default=0)
        rows = [row for row in rows if row["run_attempt"] == latest]
    elif mode != "all":
        unexpected("Unsupported job filter")
elif path == prefix + "artifacts":
    key = "artifacts"
    rows = fixture[key]
else:
    unexpected("Unexpected API endpoint: " + path)

try:
    page = int(query.get("page", 1))
    size = int(query.get("per_page", 30))
except ValueError:
    unexpected("Non-numeric pagination")
if page < 1 or not 1 <= size <= 100:
    unexpected("Invalid pagination bounds")
pages = []
while True:
    if fixture.get("fail_listing") == key and page >= fixture.get("fail_page", 1):
        print("synthetic API read failure", file=sys.stderr)
        raise SystemExit(1)
    start = (page - 1) * size
    batch = rows[start:start + size]
    pages.append({"total_count": len(rows), key: batch})
    if not paginate or start + size >= len(rows):
        break
    page += 1
if slurp:
    print(json.dumps(pages))
else:
    for payload in pages:
        print(json.dumps(payload))
'''


def timestamp(attempt, minute):
    return f"2031-05-17T12:{10 * (attempt - 1) + minute:02d}:00Z"


def job(partition, attempt=1, job_id=None, **changes):
    result = {
        "id": job_id if job_id is not None else 880100 + partition,
        "name": f"test-shards ({partition})",
        "run_id": RUN,
        "run_attempt": attempt,
        "head_sha": SHA,
        "status": "completed",
        "conclusion": "success",
        "created_at": timestamp(attempt, 0),
        "started_at": timestamp(attempt, 1),
        "completed_at": timestamp(attempt, 3),
        "runner_id": 10000 + 100 * attempt + partition,
    }
    result.update(changes)
    if result["status"] != "completed":
        result["completed_at"] = None
    if result["status"] == "queued":
        result["started_at"] = None
        result["runner_id"] = None
    return result


def carried_job(partition, attempt=2, **changes):
    # A later API row, not a later execution: retain original runtime
    # fields/outcome, but change row identity, attempt and creation time.
    result = job(partition)
    result.update({
        "id": 610000 + 100 * attempt + partition,
        "run_attempt": attempt,
        "created_at": timestamp(attempt, 0),
    })
    result.update(changes)
    return result


def artifact(partition, attempt=1, artifact_id=None, **changes):
    result = {
        "id": artifact_id if artifact_id is not None else ORIGINAL_IDS[partition - 1],
        "name": f"test-shard-{partition}-attempt-{attempt}",
        "expired": False,
        "workflow_run": {"id": RUN, "head_sha": SHA},
    }
    result.update(changes)
    return result


class ShardArtifactAcceptance(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="shard-acceptance-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.calls = 0
        self.jobs = [job(p) for p in range(1, 7)]
        self.artifacts = [artifact(p) for p in range(1, 7)]
        self.job_details = {}
        self.fail_details = set()
        self.api_options = {}
        self.red_ids = set()
        self.empty_ids = set()

    def add_carries(self, partitions=range(1, 7), attempt=2):
        self.jobs.extend(carried_job(p, attempt) for p in partitions)

    def add_rerun(self, attempt=2, conclusion="success", status="completed"):
        self.add_carries(range(1, 6), attempt)
        # Numeric IDs and API list order deliberately do not encode recency.
        self.jobs.insert(0, job(6, attempt, 770601, status=status,
                                conclusion=conclusion))
        self.artifacts.insert(0, artifact(6, attempt, 420017))

    def stale_index_row(self, partition=6, attempt=2, status="queued"):
        """Return the authoritative row, leaving a nonterminal index copy."""
        index = next(i for i, row in enumerate(self.jobs)
                     if row["name"] == f"test-shards ({partition})"
                     and row["run_attempt"] == attempt)
        actual = copy.deepcopy(self.jobs[index])
        self.jobs[index] = job(partition, attempt, actual["id"],
                               status=status, conclusion=None,
                               created_at=actual["created_at"])
        return actual

    def offer_detail(self, actual, **changes):
        detail = copy.deepcopy(actual)
        detail.update(changes)
        self.job_details[str(actual["id"])] = detail
        return detail

    def bind_fixture_to_run(self, run_id):
        for row in self.jobs:
            row["run_id"] = run_id
        for row in self.artifacts:
            row["workflow_run"]["id"] = run_id

    def reports(self, root):
        evidence = root / "evidence"
        evidence.mkdir()
        inventory = "".join(f"probe_{p}\n" for p in range(1, 7))
        for row in self.artifacts:
            report_dir = evidence / str(row["id"])
            report_dir.mkdir(exist_ok=True)
            suites = ET.Element("testsuites")
            if row["id"] not in self.empty_ids:
                match = re.fullmatch(r"test-shard-([1-6])(?:-attempt-[0-9]+)?", row["name"])
                if match:
                    p = int(match.group(1))
                    suite = ET.SubElement(suites, "testsuite", name=f"{PACKAGE}::probe_{p}")
                    case = ET.SubElement(suite, "testcase", name="synthetic_case")
                    if row["id"] in self.red_ids:
                        ET.SubElement(case, "failure", message="retained failing evidence")
                    if p == 1:
                        for name in (PACKAGE, f"{PACKAGE}::bin/fixture"):
                            suite = ET.SubElement(suites, "testsuite", name=name)
                            ET.SubElement(suite, "testcase", name="synthetic_case")
            ET.ElementTree(suites).write(report_dir / "junit.xml", encoding="utf-8")
            (report_dir / "expected-targets.txt").write_text(inventory)
        return evidence

    def snapshot(self, evidence):
        return {str(p.relative_to(evidence)): p.read_bytes()
                for p in evidence.rglob("*") if p.is_file()}

    def select(self, *, attempt=2, run_id=RUN):
        self.calls += 1
        root = self.root / str(self.calls)
        root.mkdir()
        bindir = root / "bin"
        bindir.mkdir()
        fake = bindir / "gh"
        fake.write_text(f"#!{sys.executable}\n" + FAKE_GH)
        fake.chmod(0o700)
        payload = {"repo": REPO, "run": run_id, "jobs": self.jobs,
                   "artifacts": self.artifacts,
                   "job_details": self.job_details,
                   "fail_details": sorted(self.fail_details),
                   **self.api_options}
        (root / "api.json").write_text(json.dumps(payload))
        evidence = self.reports(root)
        before = self.snapshot(evidence)
        for directory in ("home", "config", "cache", "tmp"):
            (root / directory).mkdir()
        # No inherited credentials, gh config, provider env or global PATH.
        env = {
            "PATH": str(bindir),
            "HOME": str(root / "home"),
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_CACHE_HOME": str(root / "cache"),
            "TMPDIR": str(root / "tmp"),
            "GH_CONFIG_DIR": str(root / "config" / "gh"),
            "GH_TOKEN": "synthetic-noncredential",
            "GH_HOST": "github.com",
            "GH_PROMPT_DISABLED": "1",
            "SHARD_API_FIXTURE": str(root),
            "LC_ALL": "C",
        }
        result = subprocess.run(
            [sys.executable, "-I", str(SELECTOR), "--repo", REPO,
             "--run-id", str(run_id), "--sha", SHA, "--attempt", str(attempt)],
            cwd=root, env=env, capture_output=True, text=True, timeout=15,
        )
        error = root / "transport-error"
        self.assertFalse(error.exists(), error.read_text() if error.exists() else "")
        self.assertEqual(self.snapshot(evidence), before,
                         "Selection must preserve all earlier report evidence")
        return result, evidence

    def selected(self, *, expected, attempt=2, run_id=RUN):
        result, evidence = self.select(attempt=attempt, run_id=run_id)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        lines = [line for line in result.stdout.splitlines()
                 if line.startswith("artifact_ids=")]
        self.assertEqual(len(lines), 1, result.stdout)
        match = re.fullmatch(r"artifact_ids=([0-9]+(?:,[0-9]+){5})", lines[0])
        self.assertIsNotNone(match, result.stdout)
        ids = tuple(int(value) for value in match.group(1).split(","))
        self.assertEqual(len(set(ids)), 6)
        # The contract specifies the six IDs, not their output ordering.
        self.assertEqual(set(ids), set(expected), result.stdout)
        return ids, evidence

    def refused(self, *, attempt=2, run_id=RUN):
        result, _ = self.select(attempt=attempt, run_id=run_id)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIsNone(re.search(r"artifact_ids=\s*[0-9]", result.stdout),
                          "Refusal must not publish even partial selectable IDs")

    def validate(self, evidence, ids):
        before = self.snapshot(evidence)
        inventories = [(evidence / str(value) / "expected-targets.txt").read_bytes()
                       for value in ids]
        self.assertEqual(len(inventories), 6)
        self.assertTrue(all(value == inventories[0] for value in inventories))
        result = subprocess.run(
            [sys.executable, "-I", str(CHECKER), "--package", PACKAGE,
             "--targets-file", str(evidence / str(ids[0]) / "expected-targets.txt"),
             *(str(evidence / str(value) / "junit.xml") for value in ids)],
            cwd=evidence, capture_output=True, text=True, timeout=15,
        )
        self.assertEqual(self.snapshot(evidence), before)
        return result

    def test_first_attempt_selects_each_actual_producer(self):
        self.selected(expected=ORIGINAL_IDS, attempt=1)

    def test_all_successes_copied_into_new_attempt_keep_original_reports(self):
        self.add_carries()
        self.selected(expected=ORIGINAL_IDS)

    def test_mixed_attempts_replace_old_failure_and_reconcile_carried_successes(self):
        self.jobs[-1]["conclusion"] = "failure"
        self.red_ids.add(640906)
        self.add_rerun()
        ids, evidence = self.selected(expected=MIXED_IDS)
        self.assertNotEqual(self.validate(evidence, ORIGINAL_IDS).returncode, 0)
        current_result = self.validate(evidence, ids)
        self.assertEqual(current_result.returncode, 0, current_result.stderr)
        self.assertTrue((evidence / "640906" / "junit.xml").exists())

    def test_metadata_order_and_id_magnitude_do_not_choose_execution(self):
        self.add_rerun()
        self.selected(expected=MIXED_IDS)
        self.jobs.reverse()
        self.artifacts.reverse()
        self.selected(expected=MIXED_IDS)

    def test_copies_in_several_attempts_resolve_to_one_actual_execution(self):
        self.add_carries(attempt=2)
        self.add_carries(attempt=3)
        self.selected(expected=ORIGINAL_IDS, attempt=3)

    def test_latest_success_can_replace_an_intermediate_actual_failure(self):
        self.jobs.append(job(6, 2, 770602, conclusion="failure"))
        self.artifacts.append(artifact(6, 2, 420018))
        self.red_ids.add(420018)
        self.add_rerun(attempt=3)
        self.selected(expected=MIXED_IDS, attempt=3)

    def test_current_non_green_actual_execution_cannot_fall_back(self):
        original_jobs = copy.deepcopy(self.jobs)
        original_artifacts = copy.deepcopy(self.artifacts)
        for status, conclusion in (
            ("completed", "failure"), ("completed", "cancelled"),
            ("completed", "skipped"), ("completed", "timed_out"),
            ("completed", "neutral"), ("in_progress", None), ("queued", None),
        ):
            with self.subTest(status=status, conclusion=conclusion):
                self.jobs = copy.deepcopy(original_jobs)
                self.artifacts = copy.deepcopy(original_artifacts)
                self.add_rerun(status=status, conclusion=conclusion)
                self.refused()

    def test_current_actual_success_requires_complete_execution_provenance(self):
        self.add_rerun()
        actual = copy.deepcopy(self.jobs[0])
        for field in ("created_at", "started_at", "completed_at", "runner_id"):
            with self.subTest(field=field, missing=True):
                self.jobs[0] = copy.deepcopy(actual)
                del self.jobs[0][field]
                self.refused()
            with self.subTest(field=field, null=True):
                self.jobs[0] = dict(actual, **{field: None})
                self.refused()

    def test_current_actual_success_requires_valid_execution_order(self):
        self.add_rerun()
        actual = copy.deepcopy(self.jobs[0])
        # Actual attempt 2 normally has created=12:10, start=12:11,
        # completion=12:13. Neither creation below is an alias timestamp:
        # it is after start but not after completion, and must still refuse.
        for created in (timestamp(2, 2), timestamp(2, 3)):
            with self.subTest(created_at=created):
                self.jobs[0] = dict(actual, created_at=created)
                self.refused()
        with self.subTest(start_after_completion=True):
            self.jobs[0] = dict(actual, started_at=timestamp(2, 4))
            self.refused()

    def test_boolean_job_run_id_cannot_equal_integer_expected_run_one(self):
        self.add_rerun()
        self.bind_fixture_to_run(1)
        self.selected(expected=MIXED_IDS, run_id=1)
        self.jobs[0]["run_id"] = True
        self.refused(run_id=1)

    def test_boolean_artifact_run_id_cannot_equal_integer_expected_run_one(self):
        self.add_rerun()
        self.bind_fixture_to_run(1)
        self.selected(expected=MIXED_IDS, run_id=1)
        self.artifacts[0]["workflow_run"]["id"] = True
        self.refused(run_id=1)

    def test_carried_failure_is_not_a_green_actual_execution(self):
        self.jobs[-1]["conclusion"] = "failure"
        self.add_carries(range(1, 6))
        self.jobs.append(carried_job(6, conclusion="failure"))
        self.refused()

    def test_copy_without_its_original_execution_refuses(self):
        self.add_carries()
        self.jobs = [row for row in self.jobs
                     if not (row["name"] == "test-shards (5)" and row["run_attempt"] == 1)]
        self.refused()

    def test_copy_matching_two_original_rows_is_ambiguous(self):
        self.add_carries()
        self.jobs.append(job(5, job_id=880999))
        self.refused()

    def test_copy_provenance_must_match_all_execution_fields_and_pins(self):
        self.add_carries()
        index = next(i for i, row in enumerate(self.jobs)
                     if row["name"] == "test-shards (5)" and row["run_attempt"] == 2)
        original_copy = copy.deepcopy(self.jobs[index])
        mismatches = (
            ("started_at", "2031-05-17T12:01:01Z"),
            ("completed_at", "2031-05-17T12:03:01Z"),
            ("runner_id", 999999),
            ("status", "in_progress"),
            ("conclusion", "failure"),
            ("head_sha", OTHER_SHA),
            ("run_id", RUN + 1),
        )
        for field, value in mismatches:
            with self.subTest(field=field):
                self.jobs[index] = dict(original_copy, **{field: value})
                self.refused()

    def test_copy_with_incomplete_or_unreadable_provenance_refuses(self):
        self.add_carries()
        index = next(i for i, row in enumerate(self.jobs)
                     if row["name"] == "test-shards (5)" and row["run_attempt"] == 2)
        original_copy = copy.deepcopy(self.jobs[index])
        for field in ("created_at", "started_at", "completed_at", "runner_id",
                      "status", "conclusion"):
            with self.subTest(missing=field):
                self.jobs[index] = copy.deepcopy(original_copy)
                del self.jobs[index][field]
                self.refused()
        for field in ("created_at", "started_at", "completed_at"):
            with self.subTest(invalid_timestamp=field):
                self.jobs[index] = dict(original_copy, **{field: "not-a-timestamp"})
                self.refused()

    def test_copy_cannot_use_an_original_with_foreign_run_or_sha(self):
        self.add_carries()
        index = next(i for i, row in enumerate(self.jobs)
                     if row["name"] == "test-shards (5)" and row["run_attempt"] == 1)
        original = copy.deepcopy(self.jobs[index])
        for field, value in (("head_sha", OTHER_SHA), ("run_id", RUN + 1)):
            with self.subTest(field=field):
                self.jobs[index] = dict(original, **{field: value})
                self.refused()

    def test_copy_cannot_use_same_attempt_execution_as_earlier_provenance(self):
        self.add_carries()
        # Identical runtime fields do not establish a strictly earlier
        # producer when the original row is relabelled to the same attempt.
        self.jobs[4]["run_attempt"] = 2
        self.artifacts[4]["name"] = "test-shard-5-attempt-2"
        self.refused()

    def test_current_red_report_still_fails_the_existing_consumer(self):
        self.add_rerun()
        self.red_ids.add(420017)
        ids, evidence = self.selected(expected=MIXED_IDS)
        self.assertNotEqual(self.validate(evidence, ids).returncode, 0)

    def test_selected_empty_report_cannot_hide_a_coverage_hole(self):
        self.add_rerun()
        self.empty_ids.add(420017)
        ids, evidence = self.selected(expected=MIXED_IDS)
        self.assertNotEqual(self.validate(evidence, ids).returncode, 0)

    def test_newest_actual_job_with_wrong_sha_or_run_cannot_restore_old_success(self):
        self.add_rerun()
        authoritative = copy.deepcopy(self.jobs[0])
        for field, value in (("head_sha", OTHER_SHA), ("run_id", RUN + 1)):
            with self.subTest(field=field):
                self.jobs[0] = dict(authoritative, **{field: value})
                self.refused()

    def test_selected_artifact_must_match_exact_sha_and_run(self):
        self.add_rerun()
        original = copy.deepcopy(self.artifacts[0])
        for metadata in ({"id": RUN + 1, "head_sha": SHA},
                         {"id": RUN, "head_sha": OTHER_SHA}, None):
            with self.subTest(workflow_run=metadata):
                self.artifacts[0] = dict(original, workflow_run=metadata)
                self.refused()

    def test_missing_partition_job_refuses_even_with_all_six_artifacts(self):
        self.jobs.pop()
        self.refused(attempt=1)

    def test_ambiguous_authoritative_actual_jobs_refuse(self):
        self.add_rerun()
        self.jobs.append(job(6, 2, 770699))
        self.refused()

    def test_missing_artifact_for_exact_producer_attempt_cannot_fall_back(self):
        self.add_rerun()
        self.artifacts.pop(0)
        self.refused()

    def test_expired_authoritative_artifact_cannot_fall_back(self):
        self.add_rerun()
        self.artifacts[0]["expired"] = True
        self.refused()

    def test_two_artifacts_for_authoritative_attempt_are_ambiguous(self):
        self.add_rerun()
        self.artifacts.append(artifact(6, 2, 420099))
        self.refused()

    def test_duplicate_legacy_names_cannot_overwrite_authoritative_reports(self):
        self.add_rerun()
        self.artifacts.extend([
            artifact(6, artifact_id=940001, name="test-shard-6"),
            artifact(6, artifact_id=940002, name="test-shard-6"),
        ])
        self.red_ids.update((940001, 940002))
        ids, evidence = self.selected(expected=MIXED_IDS)
        self.assertEqual(self.validate(evidence, ids).returncode, 0)
        self.assertTrue((evidence / "940001" / "junit.xml").exists())
        self.assertTrue((evidence / "940002" / "junit.xml").exists())
        self.artifacts.reverse()
        self.selected(expected=MIXED_IDS)

    def test_legacy_name_alone_is_not_an_attempt_binding(self):
        self.artifacts[-1]["name"] = "test-shard-6"
        self.refused(attempt=1)

    def test_expired_stale_evidence_does_not_disqualify_current_report(self):
        self.artifacts[-1]["expired"] = True
        self.add_rerun()
        self.selected(expected=MIXED_IDS)

    def unrelated_jobs(self):
        return [job(1, job_id=990000 + n, name=f"unrelated ({n})")
                for n in range(205)]

    def unrelated_artifacts(self):
        return [artifact(1, artifact_id=950000 + n, name=f"unrelated-{n}")
                for n in range(205)]

    def test_all_job_and_artifact_pages_contribute_to_selection(self):
        self.add_rerun()
        self.jobs = self.unrelated_jobs() + self.jobs
        self.artifacts = self.unrelated_artifacts() + self.artifacts
        self.selected(expected=MIXED_IDS)

    def test_original_execution_and_copies_can_be_on_different_pages(self):
        self.jobs += self.unrelated_jobs()
        self.add_carries()
        self.selected(expected=ORIGINAL_IDS)

    def test_late_page_unmatched_copy_cannot_be_silently_ignored(self):
        self.jobs += self.unrelated_jobs()
        self.add_carries()
        self.jobs[-1]["runner_id"] = 999999
        self.refused()

    def test_late_page_current_actual_failure_prevents_early_page_fallback(self):
        self.jobs += self.unrelated_jobs()
        self.add_carries(range(1, 6))
        self.jobs.append(job(6, 2, 770601, conclusion="failure"))
        self.artifacts.append(artifact(6, 2, 420017))
        self.refused()

    def test_late_page_duplicate_artifact_still_refuses(self):
        self.add_rerun()
        self.artifacts += self.unrelated_artifacts()
        self.artifacts.append(artifact(6, 2, 420099))
        self.refused()

    def test_late_page_duplicate_actual_job_still_refuses(self):
        self.add_rerun()
        self.jobs += self.unrelated_jobs()
        self.jobs.append(job(6, 2, 770699))
        self.refused()

    def test_api_read_failures_publish_no_partial_selection(self):
        self.add_rerun()
        for listing in ("jobs", "artifacts"):
            with self.subTest(listing=listing, page=1):
                self.api_options = {"fail_listing": listing, "fail_page": 1}
                self.refused()
        self.jobs += self.unrelated_jobs()
        self.artifacts += self.unrelated_artifacts()
        for listing in ("jobs", "artifacts"):
            with self.subTest(listing=listing, page=2):
                self.api_options = {"fail_listing": listing, "fail_page": 2}
                self.refused()

    def test_stale_nonterminal_index_resolves_same_id_successful_detail(self):
        # Reproduces the CAD-1758 observation: the jobs index still says the
        # shard row is queued/in_progress while its immutable detail already
        # reports the completed, successful attempt that uploaded the artifact.
        for scenario, expected in (("first", ORIGINAL_IDS), ("rerun", MIXED_IDS)):
            for status in ("queued", "in_progress"):
                with self.subTest(scenario=scenario, index_status=status):
                    self.jobs = [job(p) for p in range(1, 7)]
                    self.artifacts = [artifact(p) for p in range(1, 7)]
                    self.job_details = {}
                    self.fail_details = set()
                    if scenario == "first":
                        attempt, partition = 1, 1
                    else:
                        self.add_rerun()
                        attempt, partition = 2, 6
                    actual = self.stale_index_row(partition, attempt, status)
                    self.offer_detail(actual)
                    self.selected(expected=expected, attempt=attempt)

    def test_still_nonterminal_detail_refuses(self):
        self.add_rerun()
        jobs = copy.deepcopy(self.jobs)
        artifacts = copy.deepcopy(self.artifacts)
        for status in ("queued", "in_progress"):
            with self.subTest(detail_status=status):
                self.jobs = copy.deepcopy(jobs)
                self.artifacts = copy.deepcopy(artifacts)
                self.job_details = {}
                actual = self.stale_index_row()
                self.offer_detail(
                    actual, status=status, conclusion=None, completed_at=None,
                    started_at=None if status == "queued" else actual["started_at"],
                    runner_id=None if status == "queued" else actual["runner_id"],
                )
                self.refused()

    def test_failed_or_cancelled_detail_refuses(self):
        self.add_rerun()
        jobs = copy.deepcopy(self.jobs)
        artifacts = copy.deepcopy(self.artifacts)
        for conclusion in ("failure", "cancelled"):
            with self.subTest(detail_conclusion=conclusion):
                self.jobs = copy.deepcopy(jobs)
                self.artifacts = copy.deepcopy(artifacts)
                self.job_details = {}
                actual = self.stale_index_row()
                self.offer_detail(actual, conclusion=conclusion)
                self.refused()

    def test_unavailable_or_malformed_detail_refuses(self):
        self.add_rerun()
        jobs = copy.deepcopy(self.jobs)
        artifacts = copy.deepcopy(self.artifacts)
        for mode in ("transport-failure", "missing", "not-an-object", "missing-name",
                     "missing-completed_at"):
            with self.subTest(detail=mode):
                self.jobs = copy.deepcopy(jobs)
                self.artifacts = copy.deepcopy(artifacts)
                self.job_details = {}
                self.fail_details = set()
                actual = self.stale_index_row()
                if mode == "transport-failure":
                    self.fail_details.add(actual["id"])
                elif mode == "not-an-object":
                    self.job_details[str(actual["id"])] = [actual]
                elif mode == "missing-name":
                    self.job_details[str(actual["id"])] = {
                        key: value for key, value in actual.items() if key != "name"
                    }
                elif mode == "missing-completed_at":
                    self.job_details[str(actual["id"])] = {
                        key: value for key, value in actual.items()
                        if key != "completed_at"
                    }
                self.refused()

    def test_detail_identity_swap_refuses(self):
        self.add_rerun()
        jobs = copy.deepcopy(self.jobs)
        artifacts = copy.deepcopy(self.artifacts)
        swaps = (
            ("id", lambda detail: detail["id"] + 1),
            ("run_id", lambda detail: RUN + 1),
            ("run_attempt", lambda detail: detail["run_attempt"] - 1),
            ("head_sha", lambda detail: OTHER_SHA),
            ("name", lambda detail: "test-shards (5)"),
        )
        for field, change in swaps:
            with self.subTest(field=field):
                self.jobs = copy.deepcopy(jobs)
                self.artifacts = copy.deepcopy(artifacts)
                self.job_details = {}
                actual = self.stale_index_row()
                self.offer_detail(actual, **{field: change(actual)})
                self.refused()

    def test_indexed_terminal_failure_cannot_be_refreshed_by_successful_detail(self):
        # A detail read may only reconcile a nonterminal index row. Offering
        # the same ID's successful record must not rewrite a terminal row.
        self.add_rerun()
        jobs = copy.deepcopy(self.jobs)
        artifacts = copy.deepcopy(self.artifacts)
        for conclusion in ("failure", "cancelled"):
            with self.subTest(index_conclusion=conclusion):
                self.jobs = copy.deepcopy(jobs)
                self.artifacts = copy.deepcopy(artifacts)
                self.job_details = {}
                index = next(i for i, row in enumerate(self.jobs)
                             if row["name"] == "test-shards (6)"
                             and row["run_attempt"] == 2)
                successful = copy.deepcopy(self.jobs[index])
                self.jobs[index] = dict(successful, conclusion=conclusion)
                self.offer_detail(successful)
                self.refused()


if __name__ == "__main__":
    unittest.main()
