#!/usr/bin/env python3
"""CAD-958: stress.yml is manual-dispatch only, read-only and never writes a cache.

Stdlib plus PyYAML (the strict loader from test_workflows_strict.py). The
contract runs on the checked-in workflow, and each rule is proven by mutating
the workflow text and requiring the check to fail.

Why: the stress workflow loops a test N times on ephemeral runners. It must
not become a PR/queue/push gate, must not hold the RW sccache key or the
`sccache-writer` environment, and must not save the shared rust-cache.
"""
from pathlib import Path
import os
import re
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_workflows_strict import load_strict  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = Path(os.environ.get("CADENCE_STRESS_WORKFLOW") or ROOT / ".github/workflows/stress.yml")
CI = ROOT / ".github/workflows/ci.yml"
SHA_PIN = re.compile(r"^[^@\s]+@[0-9a-f]{40}$")
INPUTS = {"ref", "filter", "features", "count", "copies", "load", "stop_on_fail"}


def triggers(doc):
    # PyYAML (YAML 1.1) loads the bare key `on` as boolean True.
    return doc.get("on", doc.get(True))


def steps_of(doc):
    return [s for job in doc["jobs"].values() for s in job.get("steps", [])]


def check(text):
    """Return the list of contract violations in a stress workflow text."""
    bad = []
    doc = load_strict(text)
    on = triggers(doc)
    if not isinstance(on, dict) or set(on) != {"workflow_dispatch"}:
        bad.append(f"triggers must be exactly workflow_dispatch, got {on!r}")
    else:
        got = set(on["workflow_dispatch"]["inputs"])
        if got != INPUTS:
            bad.append(f"inputs differ: {sorted(got ^ INPUTS)}")
    if doc.get("permissions") != {"contents": "read"}:
        bad.append(f"top-level permissions must be exactly contents: read, got {doc.get('permissions')!r}")
    for name, job in doc["jobs"].items():
        if "permissions" in job and job["permissions"] != {"contents": "read"}:
            bad.append(f"job {name} widens permissions")
        if "environment" in job:
            bad.append(f"job {name} uses an environment")
        if not isinstance(job.get("timeout-minutes"), int) or job["timeout-minutes"] > 360:
            bad.append(f"job {name} needs an integer timeout-minutes <= 360")
    if re.search(r"secrets\.SCCACHE_R2_RW_|SCCACHE_CI_RW_|sccache-writer", text):
        bad.append("RW sccache credentials or the sccache-writer environment are referenced")
    if "secrets.SCCACHE_R2_RO_ACCESS_KEY_ID" not in text:
        bad.append("read-only sccache key is not wired")
    for secret in set(re.findall(r"secrets\.([A-Za-z0-9_]+)", text)):
        if not secret.startswith("SCCACHE_R2_RO_"):
            bad.append(f"secret {secret} is not the read-only sccache key")
    for step in steps_of(doc):
        uses = step.get("uses")
        if uses and not SHA_PIN.match(uses):
            bad.append(f"action not SHA-pinned: {uses}")
        if uses and uses.startswith("Swatinem/rust-cache@"):
            if step.get("with", {}).get("save-if") is not False:
                bad.append("rust-cache must set save-if: false")
        if uses and uses.startswith("actions/cache"):
            bad.append("actions/cache can save; use rust-cache with save-if: false")
        # Untrusted inputs may not be interpolated into a shell script.
        if "run" in step and re.search(r"\$\{\{\s*inputs\.", step["run"]):
            bad.append("an input is interpolated into a run script; pass it through env")
    if "cargo-nextest" not in text or "scripts/cadence-nextest" not in text:
        bad.append("must run the pinned scripts/cadence-nextest wrapper")
    if re.search(r"--retries|NEXTEST_RETRIES|NEXTEST_PROFILE|(?<!stable )--profile(?! minimal)", text):
        bad.append("retries and profile are pinned by the wrapper; do not override")
    if "if: ${{ always() && inputs.load }}" not in text:
        bad.append("the CPU burner needs an always() kill step")
    if "pull_request_target" in text:
        bad.append("pull_request_target is referenced")
    return bad


class StressWorkflow(unittest.TestCase):
    def setUp(self):
        self.text = WORKFLOW.read_text()

    def assertRejects(self, mutated, needle):
        self.assertNotEqual(self.text, mutated, "mutation did not change the workflow")
        bad = check(mutated)
        self.assertTrue(any(needle in b for b in bad), f"{needle!r} not reported: {bad}")

    def test_checked_in_workflow_satisfies_the_contract(self):
        self.assertEqual(check(self.text), [])

    def test_count_default_and_copies_validated(self):
        doc = load_strict(self.text)
        inputs = triggers(doc)["workflow_dispatch"]["inputs"]
        self.assertEqual(inputs["count"]["default"], "30")
        self.assertEqual(inputs["features"]["default"], "test-seam")
        self.assertIn("1-200", inputs["count"]["description"])
        self.assertIn("1-8", inputs["copies"]["description"])
        self.assertRegex(self.text, r'-gt 200')
        self.assertRegex(self.text, r'-gt 8')

    def test_copies_is_a_matrix_and_fail_fast_is_off(self):
        strat = load_strict(self.text)["jobs"]["stress"]["strategy"]
        self.assertIs(strat["fail-fast"], False)
        self.assertIn("matrix.copy", self.text)

    def test_gate_test_cache_is_restored_without_saving(self):
        self.assertIn("shared-key: gate-test", self.text)
        self.assertIn("save-if: false", self.text)

    def test_ci_runs_this_contract(self):
        self.assertIn("python3 tests/scripts/test_ci_stress.py", CI.read_text())

    # Mutations: each forbidden change must be caught.
    def test_mutation_pull_request_trigger(self):
        self.assertRejects(self.text.replace("on:\n  workflow_dispatch:", "on:\n  pull_request:\n  workflow_dispatch:", 1), "triggers")

    def test_mutation_push_trigger(self):
        self.assertRejects(self.text.replace("on:\n  workflow_dispatch:", "on:\n  push:\n    branches: [main]\n  workflow_dispatch:", 1), "triggers")

    def test_mutation_merge_group_trigger(self):
        self.assertRejects(self.text.replace("on:\n  workflow_dispatch:", "on:\n  merge_group:\n  workflow_dispatch:", 1), "triggers")

    def test_mutation_schedule_trigger(self):
        self.assertRejects(self.text.replace("on:\n  workflow_dispatch:", "on:\n  schedule:\n    - cron: '0 * * * *'\n  workflow_dispatch:", 1), "triggers")

    def test_mutation_rw_secret(self):
        self.assertRejects(self.text.replace("SCCACHE_R2_RO_ACCESS_KEY_ID", "SCCACHE_R2_RW_ACCESS_KEY_ID", 1), "RW sccache")

    def test_mutation_sccache_writer_environment(self):
        self.assertRejects(self.text.replace("  stress:\n    needs: [plan]\n", "  stress:\n    needs: [plan]\n    environment: sccache-writer\n", 1), "environment")

    def test_mutation_any_environment(self):
        self.assertRejects(self.text.replace("  plan:\n", "  plan:\n    environment: staging\n", 1), "uses an environment")

    def test_mutation_cache_saves(self):
        self.assertRejects(self.text.replace("save-if: false", "save-if: true", 1), "save-if")

    def test_mutation_cache_save_if_dropped(self):
        self.assertRejects(self.text.replace("          save-if: false\n", "", 1), "save-if")

    def test_mutation_actions_cache(self):
        mutated = self.text.replace("      - run: rustup toolchain install stable --profile minimal\n", "      - uses: actions/cache@0000000000000000000000000000000000000000\n        with:\n          path: target\n          key: k\n      - run: rustup toolchain install stable --profile minimal\n", 1)
        self.assertRejects(mutated, "actions/cache")

    def test_mutation_contents_read_dropped(self):
        self.assertRejects(self.text.replace("permissions:\n  contents: read\n", "", 1), "permissions")

    def test_mutation_contents_write(self):
        self.assertRejects(self.text.replace("contents: read", "contents: write", 1), "permissions")

    def test_mutation_job_widens_permissions(self):
        self.assertRejects(self.text.replace("  plan:\n", "  plan:\n    permissions:\n      contents: write\n", 1), "widens")

    def test_mutation_timeout_dropped(self):
        self.assertRejects(self.text.replace("    timeout-minutes: 5\n", "", 1), "timeout")

    def test_mutation_unpinned_action(self):
        mutated = re.sub(r"actions/checkout@[0-9a-f]{40}", "actions/checkout@v4", self.text, count=1)
        self.assertRejects(mutated, "SHA-pinned")

    def test_mutation_input_interpolated_into_script(self):
        mutated = self.text.replace('          set -eu\n          case "$COUNT"', '          set -eu\n          echo ${{ inputs.filter }}\n          case "$COUNT"', 1)
        self.assertRejects(mutated, "interpolated")

    def test_mutation_other_secret(self):
        self.assertRejects(self.text.replace("secrets.SCCACHE_R2_RO_SECRET_ACCESS_KEY", "secrets.GITHUB_PAT", 1), "not the read-only")

    def test_mutation_retries_override(self):
        self.assertRejects(self.text.replace('-E "$FILTER" > "$log"', '-E "$FILTER" --retries 3 > "$log"', 1), "retries")

    def test_mutation_burner_kill_not_always(self):
        self.assertRejects(self.text.replace("if: ${{ always() && inputs.load }}", "if: ${{ inputs.load }}", 1), "always()")

    def test_mutation_duplicate_key_is_rejected_by_strict_loader(self):
        import yaml
        mutated = self.text.replace("permissions:\n  contents: read\n", "permissions:\n  contents: read\npermissions:\n  contents: read\n", 1)
        with self.assertRaises(yaml.YAMLError):
            check(mutated)


if __name__ == "__main__":
    unittest.main()
