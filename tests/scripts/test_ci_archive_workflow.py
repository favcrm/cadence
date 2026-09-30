#!/usr/bin/env python3
"""Compile-once workflow DAG contracts for .github/workflows/ci.yml.

The suite partitions as producer (test-build) -> eight consumers (test-shard)
-> required aggregate (test). These tests pin the wiring, never a real
archive, Rust compile or Actions run: job needs/conditions, immutable
artifact transfer, producer-output authority, fresh consumer target and the
preserved queue/release/test-once behavior.
"""
import json
import os
import re
import subprocess
import textwrap
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/ci.yml"
TEXT = WORKFLOW.read_text()

PINNED_DOWNLOAD = "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c"
PINNED_UPLOAD = "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a"

PRODUCER_OUTPUT_FIELDS = {
    "source_sha", "run_id", "producer_attempt", "mode",
    "archive_sha256", "plan_sha256", "inventory_sha256",
    "manifest_sha256", "artifact_id",
}


def job_block(name):
    """Return the YAML text of one job, delimited by two-space job keys."""
    match = re.search(rf"^  {re.escape(name)}:\n", TEXT, re.M)
    if not match:
        raise AssertionError(f"workflow has no job named {name}")
    start = match.end()
    nxt = re.search(r"^  [a-zA-Z][a-zA-Z0-9_-]*:\n", TEXT[start:], re.M)
    return TEXT[start:start + nxt.start()] if nxt else TEXT[start:]


def step_blocks(block):
    """Split a job's steps into per-step text, preserving order."""
    starts = [m.start() for m in re.finditer(r"^      - ", block, re.M)]
    return [block[s:starts[i + 1] if i + 1 < len(starts) else len(block)]
            for i, s in enumerate(starts)]


def needs_list(block):
    match = re.search(r"^    needs: \[([^\]]*)\]", block, re.M)
    return [item.strip() for item in match.group(1).split(",") if item.strip()] if match else []


def job_if(block):
    match = re.search(r"^    if: (.*)$", block, re.M)
    return match.group(1).strip() if match else ""


def step_named(block, fragment):
    for step in step_blocks(block):
        if fragment in step:
            return step
    raise AssertionError(f"no step containing {fragment!r}")


def step_with_all(block, *fragments):
    for step in step_blocks(block):
        if all(fragment in step for fragment in fragments):
            return step
    raise AssertionError(f"no step containing all of {fragments!r}")


class PinnedRuntime(unittest.TestCase):
    def test_producer_and_shards_use_source_digest_and_nonroot_shell(self):
        pin = (ROOT / '.config/ci-test-runtime.env').read_text()
        image = re.search(r"^CI_TEST_IMAGE='([^']+)'$", pin, re.M)[1]
        self.assertRegex(image, r'^docker\.io/library/rust@sha256:[0-9a-f]{64}$')
        for name in ('test-build', 'test-shard'):
            block = job_block(name)
            with self.subTest(job=name):
                self.assertIn('image: ' + image, block)
                self.assertIn('CADENCE_TEST_CONTAINER_IMAGE: ' + image, block)
                self.assertIn('shell: /usr/bin/setpriv --reuid=1001 --regid=1001 --clear-groups /bin/bash -e -o pipefail {0}', block)
                self.assertIn('bash scripts/ci-test-runtime-bootstrap', block)
                self.assertNotIn('rustup toolchain install stable', block)
                self.assertLess(block.index('bash scripts/ci-test-runtime-bootstrap'), block.index('python3 '))
        for name in ('test-once', 'build', 'ui', 'clippy'):
            self.assertNotIn('ci-test-runtime-bootstrap', job_block(name))


    def test_container_artifact_paths_use_measured_temp_not_host_expression(self):
        for name in ('test-build', 'test-shard'):
            block = job_block(name)
            with self.subTest(job=name):
                self.assertIn('id: runtime', block)
                self.assertIn('temp=%s', block)
                self.assertNotIn('runner.temp', block)
                self.assertIn('steps.runtime.outputs.temp', block)


class ProducerJob(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.producer = job_block("test-build")

    def test_producer_needs_queue_evidence_with_same_gate(self):
        self.assertEqual(needs_list(self.producer), ["queue-evidence"])
        self.assertIn("!cancelled()", job_if(self.producer))
        self.assertIn("needs.queue-evidence.outputs.tested != 'true'", job_if(self.producer))

    def test_producer_outputs_complete_immutable_contract(self):
        outputs = dict(
            (name, (step, out))
            for name, step, out in re.findall(
                r"^      ([a-z_0-9]+): \$\{\{ steps\.(prepare|upload)\.outputs\.([a-zA-Z_0-9-]+) \}\}",
                self.producer, re.M))
        self.assertEqual(set(outputs), PRODUCER_OUTPUT_FIELDS)
        self.assertEqual(outputs["artifact_id"], ("upload", "artifact-id"))
        for field in PRODUCER_OUTPUT_FIELDS - {"artifact_id"}:
            self.assertEqual(outputs[field], ("prepare", field), field)

    def test_scope_selection_inventory_and_timings_moved_to_producer(self):
        self.assertIn("Select PR test scope from the base policy", self.producer)
        self.assertIn("git show \"$BASE_SHA:scripts/ci-test-plan.py\"", self.producer)
        self.assertIn("base policy unavailable; full fallback", self.producer)
        # Inventory parity now runs inside the helper's prepare phase.
        self.assertIn("ci-nextest-bundle.py prepare", self.producer)
        self.assertIn("CADENCE_INVENTORY_TIMINGS_REPORT", self.producer)
        self.assertIn("Record inventory build context", self.producer)
        self.assertIn("Keep inventory compiler timings", self.producer)
        self.assertIn("steps.prepare.outputs.timings_available", self.producer)

    def test_prepare_invocation_uses_trusted_runner_and_bundle_dir(self):
        step = step_named(self.producer, "ci-nextest-bundle.py prepare")
        self.assertIn('scripts/ci-nextest-bundle.py prepare', step)
        self.assertIn('--root "$GITHUB_WORKSPACE"', step)
        self.assertIn('--plan "$RUNNER_TEMP/ci-test-plan.json"', step)
        # The trusted (possibly base-revision) runner is handed to prepare;
        # a runner without the ARCHIVE_PROTOCOL = 1 literal forces a recorded
        # full upgrade inside prepare, never a per-shard compile fallback.
        self.assertIn('--inventory-runner "$RUNNER_TEMP/ci-rust-tests.py"', step)
        self.assertIn('--directory "$RUNNER_TEMP/nextest-bundle"', step)

    def test_upload_is_unique_immutable_and_uncompressed(self):
        step = step_with_all(self.producer, PINNED_UPLOAD, "nextest-bundle")
        name = re.search(r"^          name: (.+)$", step, re.M)[1].strip()
        for token in ("github.run_id", "github.run_attempt", "github.sha"):
            self.assertIn(token, name)
        self.assertIn("compression-level: 0", step)
        self.assertIn("if-no-files-found: error", step)
        self.assertIn("retention-days: 14", step)
        self.assertNotIn("overwrite: true", step)
        for filename in ("bundle.json", "nextest.tar.zst", "inventory.json", "ci-test-plan.json"):
            self.assertIn(filename, step)

    def test_producer_keeps_rust_cache_for_its_single_compile(self):
        self.assertIn("Swatinem/rust-cache", self.producer)
        self.assertIn("Install pinned cargo-nextest", self.producer)


class ShardConsumer(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.shard = job_block("test-shard")

    def test_shard_requires_producer_success(self):
        self.assertEqual(needs_list(self.shard), ["queue-evidence", "test-build"])
        condition = job_if(self.shard)
        self.assertIn("!cancelled()", condition)
        self.assertIn("needs.queue-evidence.outputs.tested != 'true'", condition)
        self.assertIn("needs.test-build.result == 'success'", condition)

    def test_eight_lpt_shards_and_fail_fast_preserved(self):
        self.assertIn("fail-fast: false", self.shard)
        self.assertIn("shard: [1, 2, 3, 4, 5, 6, 7, 8]", self.shard)

    def test_fresh_target_no_rust_cache_no_target_deletion(self):
        self.assertNotIn("Swatinem/rust-cache", self.shard)
        self.assertNotIn("uses: Swatinem", self.shard)
        self.assertIsNone(re.search(r"rm\s+-[a-zA-Z]*r", self.shard))
        self.assertIsNone(re.search(r"rm\s+[^\n]*target", self.shard))

    def test_scope_inventory_and_timings_left_the_shard(self):
        for moved in ("Select PR test scope from the base policy",
                      "Verify selected inventory parity",
                      "Record inventory build context",
                      "Keep inventory compiler timings",
                      "ci-test-plan.py",
                      "CADENCE_INVENTORY_TIMINGS_REPORT"):
            self.assertNotIn(moved, self.shard)

    def test_producer_json_passes_through_env_never_interpolated(self):
        step = step_named(self.shard, "producer.json")
        self.assertIn("PRODUCER_JSON: ${{ toJSON(needs.test-build) }}", step)
        self.assertIn('"$PRODUCER_JSON"', step)
        run = re.search(r"run: (.+)$", step, re.M)[1]
        self.assertNotIn("${{", run)
        self.assertNotIn("toJSON", run)

    def test_select_downloads_exact_immutable_artifact_id(self):
        select = step_named(self.shard, "ci-nextest-bundle.py select")
        self.assertIn("id: select", select)
        self.assertIn('--producer "$RUNNER_TEMP/producer.json"', select)
        self.assertIn('--out "$RUNNER_TEMP/bundle-reference.json"', select)
        download = step_named(self.shard, PINNED_DOWNLOAD)
        self.assertIn("artifact-ids: ${{ steps.select.outputs.artifact_id }}", download)
        self.assertIn("path: ${{ steps.runtime.outputs.temp }}/nextest-bundle", download)

    def test_expected_context_is_independent_of_bundle_bytes(self):
        step = step_named(self.shard, "ci-nextest-bundle.py expect")
        self.assertIn('--reference "$RUNNER_TEMP/bundle-reference.json"', step)
        self.assertIn('--out "$RUNNER_TEMP/bundle-expected.json"', step)
        self.assertNotIn("bundle.json", step)

    def test_run_uses_head_runner_with_bundle_expected_and_partition(self):
        step = step_named(self.shard, "--phase tests")
        self.assertIn("scripts/ci-rust-tests.py --root \"$GITHUB_WORKSPACE\"", step)
        self.assertNotIn("$RUNNER_TEMP/ci-rust-tests.py", step)
        self.assertIn('--plan "$RUNNER_TEMP/nextest-bundle/ci-test-plan.json"', step)
        self.assertIn('--bundle "$RUNNER_TEMP/nextest-bundle"', step)
        self.assertIn('--expected "$RUNNER_TEMP/bundle-expected.json"', step)
        self.assertIn('--partition "${{ matrix.shard }}/8"', step)
        self.assertIn('--weights "$GITHUB_WORKSPACE/tests/shard-weights.json"', step)
        self.assertIn(
            '--assignment-out "$RUNNER_TEMP/shard-assignment-${{ github.run_id }}-shard-${{ matrix.shard }}.json"',
            step)

    def test_failed_shard_assignment_receipt_preserved(self):
        step = step_with_all(self.shard, PINNED_UPLOAD, "shard-assignment-")
        self.assertIn("name: shard-assignment-${{ github.run_id }}-shard-${{ matrix.shard }}", step)
        self.assertNotIn("run_attempt", step)
        self.assertIn("overwrite: true", step)
        self.assertIn("if-no-files-found: error", step)

    def test_docs_gating_follows_producer_mode(self):
        self.assertIn("needs.test-build.outputs.mode != 'docs'", self.shard)
        self.assertNotIn("steps.scope.outputs.mode", self.shard)

    def test_cost_artifact_reads_the_bundle_plan(self):
        step = step_named(self.shard, "nextest-costs-")
        self.assertIn("${{ steps.runtime.outputs.temp }}/nextest-bundle/ci-test-plan.json", step)
        self.assertNotIn("${{ runner.temp }}/ci-test-plan.json", step)

    def test_shared_contract_checks_remain_required_without_pinning_cad854_placement(self):
        # CAD854 moves these to fmt; do not reserve their old placement
        # and create a semantic clash when the first-priority PR lands.
        required = self.shard + job_block('fmt')
        for script in ("test_ci_test_plan.py", "test_ci_rust_tests.py",
                       "test_ci_shard_check.py", "test_nextest_cost_report.py"):
            self.assertIn(f"tests/scripts/{script}", required)
        self.assertIn("scripts/split-doctor-host --check", required)
        self.assertIn("pnpm build", self.shard)
        self.assertIn("Install pinned cargo-nextest", self.shard)
        self.assertIn("bash scripts/ci-test-runtime-bootstrap", self.shard)


class AggregateAndNeighbors(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.test = job_block("test")
        cls.test_once = job_block("test-once")
        cls.queue = job_block("queue-evidence")
        cls.fmt = job_block("fmt")
        cls.release_artifact = job_block("release-artifact")
        cls.release_gate = job_block("release-gate")

    def test_aggregate_needs_and_explicit_producer_success(self):
        self.assertEqual(needs_list(self.test),
                         ["queue-evidence", "test-build", "test-shard", "test-once"])
        self.assertIn('[ "${{ needs.test-build.result }}" = success ]', self.test)
        self.assertIn('[ "${{ needs.test-shard.result }}" = success ]', self.test)
        self.assertIn('[ "${{ needs.test-once.result }}" = success ]', self.test)

    def test_producer_gate_accepts_only_literal_success(self):
        # The producer's gate is its own run block; replay it through real
        # bash the way Actions would so a skipped/failed/missing producer
        # result fails closed (source-context or rerun misuse included).
        step = step_named(self.test, "Require a successful bundle producer")
        script = textwrap.dedent(re.search(r"run: \|\n(.*)", step, re.S)[1])
        for result in ("success", "failure", "cancelled", "skipped", "Success", "", "success "):
            substituted = script.replace("${{ needs.test-build.result }}", result)
            run = subprocess.run(["bash", "-eo", "pipefail", "-c", substituted],
                                 capture_output=True, text=True)
            if result == "success":
                self.assertEqual(run.returncode, 0, result)
            else:
                self.assertNotEqual(run.returncode, 0, result)

    def test_producer_outputs_gate_replays_through_jq(self):
        # Malformed or missing fields in needs.test-build.outputs must fail
        # the jq predicate, not pass through to coverage proof.
        step = step_named(self.test, "Require complete producer evidence")
        script = textwrap.dedent(re.search(r"run: \|\n(.*)", step, re.S)[1])
        good = {field: "1" for field in PRODUCER_OUTPUT_FIELDS}
        good.update(source_sha="a" * 40, mode="selected",
                    archive_sha256="b" * 64, plan_sha256="c" * 64,
                    inventory_sha256="d" * 64, manifest_sha256="e" * 64)
        cases = [good]
        for field in PRODUCER_OUTPUT_FIELDS:
            cases.append({k: v for k, v in good.items() if k != field})
        cases += [dict(good, mode="bogus"), dict(good, artifact_id="latest"),
                  dict(good, archive_sha256="z" * 64), dict(good, run_id="0"),
                  dict(good, producer_attempt="0"), dict(good, source_sha="main")]
        for i, outputs in enumerate(cases):
            run = subprocess.run(
                ["bash", "-eo", "pipefail", "-c", script],
                env={**os.environ, "PRODUCER_OUTPUTS": json.dumps(outputs)},
                capture_output=True, text=True)
            self.assertEqual(run.returncode == 0, i == 0, (i, run.stderr))

    def test_aggregate_refuses_missing_producer_outputs(self):
        self.assertIn("needs.test-build.outputs", self.test)
        for field in ("source_sha", "run_id", "producer_attempt", "mode",
                      "archive_sha256", "plan_sha256", "inventory_sha256",
                      "manifest_sha256", "artifact_id"):
            self.assertIn(field, self.test)

    def test_shard_coverage_gate_unchanged(self):
        self.assertIn("pattern: shard-assignment-${{ github.run_id }}-shard-*", self.test)
        self.assertIn("scripts/ci-shard-check.py --dir", self.test)
        self.assertIn("--total 8", self.test)

    def test_test_once_stays_independent_of_the_producer(self):
        self.assertEqual(needs_list(self.test_once), ["queue-evidence"])
        self.assertIn("needs.queue-evidence.outputs.tested != 'true'", job_if(self.test_once))
        self.assertIn("cadence-nextest --test test_seam --locked", self.test_once)
        self.assertIn("cargo test --doc --locked", self.test_once)
        self.assertNotIn("test-build", self.test_once)
        self.assertNotIn("nextest-bundle", self.test_once)

    def test_queue_and_release_wiring_unchanged(self):
        self.assertIn('["fmt", "clippy", "test", "build", "ui"]', self.queue)
        self.assertEqual(needs_list(self.release_artifact),
                         ["queue-evidence", "fmt", "clippy", "test", "build", "ui"])
        self.assertEqual(needs_list(self.release_gate), ["fmt", "clippy", "test", "build", "ui"])

    def test_fmt_runs_the_new_contract_suites_once(self):
        self.assertIn("tests/scripts/test_ci_nextest_bundle.py", self.fmt)
        self.assertIn("tests/scripts/test_ci_archive_workflow.py", self.fmt)
        self.assertIn("tests/scripts/test_ci_bundle_producer.py", self.fmt)
        self.assertIn("tests/scripts/test_ci_archive_safety.py", self.fmt)
        self.assertIn("tests/scripts/test_ci_test_runtime.py", self.fmt)

    def test_workflow_env_never_sets_rejected_identity_variables(self):
        # collect_identity refuses nonempty RUSTC/RUSTC_WRAPPER/
        # CARGO_ENCODED_RUSTFLAGS/CARGO_BUILD_TARGET and any repo/global
        # Cargo config; the workflow must not smuggle them in.
        for variable in ("RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "RUSTC",
                         "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_TARGET",
                         "CARGO_CONFIG", "CARGO_HOME"):
            self.assertIsNone(
                re.search(rf"^\s*{variable}\s*[:=]", TEXT, re.M), variable)


if __name__ == "__main__":
    unittest.main()
