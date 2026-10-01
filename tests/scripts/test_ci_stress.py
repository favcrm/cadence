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
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_workflows_strict import load_strict  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/stress.yml"
CI = ROOT / ".github/workflows/ci.yml"
SHA_PIN = re.compile(r"^[^@\s]+@[0-9a-f]{40}$")
INPUTS = {"filter", "features", "count", "copies", "load", "stop_on_fail"}
# Allowlist: every ${{ ... }} expression in the file must match one of these
# (anchored) and a `run:` body may hold none. Anything else, such as
# inputs['x'], format(), github['token'], toJSON(secrets) or fromJSON(inputs),
# is rejected.
ALLOWED_EXPR = tuple(re.compile(p) for p in (
    r"always\(\)",
    r"always\(\) && inputs\.load",
    r"inputs\.(count|copies|features|filter|load|stop_on_fail)",
    r"matrix\.copy",
    r"fromJSON\(needs\.plan\.outputs\.copies\)",
    r"steps\.validate\.outputs\.copies",
    r"github\.(run_id|run_attempt)",
    r"runner\.temp",
    r"secrets\.SCCACHE_R2_RO_(ACCESS_KEY_ID|SECRET_ACCESS_KEY)",
    r"vars\.SCCACHE_R2_ENDPOINT",
))
EXPR = re.compile(r"\$\{\{(.*?)\}\}", re.S)


def triggers(doc):
    # PyYAML (YAML 1.1) loads the bare key `on` as boolean True.
    return doc.get("on", doc.get(True))


def steps_of(doc):
    return [s for job in doc["jobs"].values() for s in job.get("steps", [])]


def step_named(doc, prefix):
    found = [s for s in steps_of(doc) if s.get("name", "").startswith(prefix)]
    return found[0] if found else {}


def cargo_env(env):
    return {k: v for k, v in (env or {}).items() if k.startswith(("CARGO", "RUST"))}


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
    ci_env = cargo_env(load_strict(CI.read_text()).get("env"))
    ci_env.pop("CADENCE_CI_SHARDS", None)
    if cargo_env(doc.get("env")) != ci_env:
        bad.append(f"CARGO*/RUST* env must equal ci.yml's {ci_env}, got {cargo_env(doc.get('env'))}")
    for name, job in doc["jobs"].items():
        if "permissions" in job and job["permissions"] != {"contents": "read"}:
            bad.append(f"job {name} widens permissions")
        if "environment" in job:
            bad.append(f"job {name} uses an environment")
        if "uses" in job or "secrets" in job:
            bad.append(f"job {name} calls a reusable workflow or passes secrets")
        if not isinstance(job.get("timeout-minutes"), int) or job["timeout-minutes"] > 120:
            bad.append(f"job {name} needs an integer timeout-minutes <= 120")
    if re.search(r"secrets\.SCCACHE_R2_RW_|SCCACHE_CI_RW_|sccache-writer", text):
        bad.append("RW sccache credentials or the sccache-writer environment are referenced")
    if "secrets.SCCACHE_R2_RO_ACCESS_KEY_ID" not in text:
        bad.append("read-only sccache key is not wired")
    for secret in set(re.findall(r"secrets\.([A-Za-z0-9_]+)", text)):
        if not secret.startswith("SCCACHE_R2_RO_"):
            bad.append(f"secret {secret} is not the read-only sccache key")
    for expr in EXPR.findall(text):
        if not any(a.fullmatch(expr.strip()) for a in ALLOWED_EXPR):
            bad.append(f"expression not on the allowlist: {expr.strip()}")
    if re.search(r"secrets:\s*inherit", text):
        bad.append("secrets: inherit is not allowed")
    for step in steps_of(doc):
        uses = step.get("uses")
        if uses and not SHA_PIN.match(uses):
            bad.append(f"action not SHA-pinned: {uses}")
        if uses and uses.startswith("Swatinem/rust-cache@"):
            if step.get("with", {}).get("save-if") is not False:
                bad.append("rust-cache must set save-if: false")
        if uses and uses.startswith("actions/cache"):
            bad.append("actions/cache can save; use rust-cache with save-if: false")
        if uses and uses.startswith("actions/setup-node@"):
            if {"cache", "cache-dependency-path"} & set(step.get("with", {})):
                bad.append("setup-node must not use a cache input (it saves from untrusted code)")
        if uses and uses.startswith("actions/checkout@"):
            if "ref" in step.get("with", {}):
                bad.append("checkout must use the dispatched ref (github.sha), not a ref input")
            if step.get("with", {}).get("persist-credentials") is not False:
                bad.append("checkout must set persist-credentials: false")
        # Nothing is interpolated into a shell script; values arrive via env.
        if "run" in step and "${{" in step["run"]:
            bad.append("a run script interpolates an expression; pass it through env")
    if "scripts/cadence-nextest" not in step_named(doc, "Run the filter in a loop").get("run", ""):
        bad.append("the loop step must run the pinned scripts/cadence-nextest wrapper")
    if "scripts/cadence-nextest" not in step_named(doc, "Build test binaries once").get("run", ""):
        bad.append("the build step must run the pinned scripts/cadence-nextest wrapper")
    if re.search(r"--retries|NEXTEST_RETRIES|NEXTEST_PROFILE|(?<!stable )--profile(?! minimal)", text):
        bad.append("retries and profile are pinned by the wrapper; do not override")
    if "rm -f target/nextest/cadence/junit.xml" not in step_named(doc, "Run the filter in a loop").get("run", ""):
        bad.append("the loop must remove the previous iteration's junit report")
    if step_named(doc, "Summarize this copy").get("if") != "${{ always() }}":
        bad.append("the per-copy summary must run under always()")
    if step_named(doc, "Stop the CPU burner").get("if") != "${{ always() && inputs.load }}":
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

    def validate(self, **env):
        run = step_named(load_strict(self.text), "Validate inputs")["run"]
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            base = {"PATH": os.environ["PATH"], "COUNT": "30", "COPIES": "1",
                    "FEATURES": "test-seam", "FILTER": "test(=x)", "GITHUB_OUTPUT": str(out)}
            r = subprocess.run(["bash", "-c", run], env={**base, **env}, capture_output=True, text=True)
            return r.returncode, out.read_text() if out.exists() else ""

    def test_checked_in_workflow_satisfies_the_contract(self):
        self.assertEqual(check(self.text), [])

    def test_validation_step_bounds_count_and_copies(self):
        self.assertEqual(self.validate(COUNT="200", COPIES="8"), (0, "copies=[1,2,3,4,5,6,7,8]\n"))
        self.assertEqual(self.validate(COUNT="1", COPIES="1")[0], 0)
        for bad in ({"COUNT": "201"}, {"COUNT": "0"}, {"COUNT": ""}, {"COUNT": "1e2"},
                    {"COUNT": "-1"}, {"COUNT": "1 "}, {"COPIES": "0"}, {"COPIES": "9"},
                    {"COPIES": ""}, {"COPIES": "1;id"}, {"FILTER": ""},
                    {"FEATURES": "a;b"}, {"FEATURES": "$(id)"}):
            with self.subTest(bad=bad):
                self.assertNotEqual(self.validate(**bad)[0], 0)

    def test_empty_features_is_accepted(self):
        self.assertEqual(self.validate(FEATURES="")[0], 0)

    def test_defaults(self):
        inputs = triggers(load_strict(self.text))["workflow_dispatch"]["inputs"]
        self.assertEqual(inputs["count"]["default"], "30")
        self.assertEqual(inputs["features"]["default"], "test-seam")

    def test_copies_is_a_matrix_and_fail_fast_is_off(self):
        strat = load_strict(self.text)["jobs"]["stress"]["strategy"]
        self.assertIs(strat["fail-fast"], False)
        self.assertIn("matrix.copy", self.text)

    def test_gate_test_cache_is_restored_without_saving(self):
        self.assertIn("shared-key: gate-test", self.text)
        self.assertIn("save-if: false", self.text)

    def test_cargo_env_matches_ci(self):
        self.assertEqual(cargo_env(load_strict(self.text)["env"]),
                         {"CARGO_TERM_COLOR": "always", "RUSTFLAGS": "-D warnings"})

    def test_ci_runs_this_contract(self):
        self.assertIn("python3 tests/scripts/test_ci_stress.py", CI.read_text())

    # Mutations: each forbidden change must be caught.
    def test_mutation_extra_triggers(self):
        for trig in ("pull_request:", "push:\n    branches: [main]", "merge_group:",
                     "schedule:\n    - cron: '0 * * * *'", "workflow_run:\n    workflows: [ci]"):
            with self.subTest(trigger=trig):
                self.assertRejects(self.text.replace("on:\n  workflow_dispatch:", f"on:\n  {trig}\n  workflow_dispatch:", 1), "triggers")

    def test_mutation_rw_secret(self):
        self.assertRejects(self.text.replace("SCCACHE_R2_RO_ACCESS_KEY_ID", "SCCACHE_R2_RW_ACCESS_KEY_ID", 1), "RW sccache")

    def test_mutation_environments(self):
        self.assertRejects(self.text.replace("  stress:\n    needs: [plan]\n", "  stress:\n    needs: [plan]\n    environment: sccache-writer\n", 1), "environment")
        self.assertRejects(self.text.replace("  plan:\n", "  plan:\n    environment: staging\n", 1), "uses an environment")

    def test_mutation_cache_saves(self):
        self.assertRejects(self.text.replace("save-if: false", "save-if: true", 1), "save-if")
        self.assertRejects(self.text.replace("          save-if: false\n", "", 1), "save-if")
        mutated = self.text.replace("      - run: rustup toolchain install stable --profile minimal\n", "      - uses: actions/cache@0000000000000000000000000000000000000000\n        with:\n          path: target\n          key: k\n      - run: rustup toolchain install stable --profile minimal\n", 1)
        self.assertRejects(mutated, "actions/cache")

    def test_mutation_setup_node_cache(self):
        node = "          node-version: 22\n"
        for extra in ("          cache: pnpm\n", "          cache-dependency-path: ui/pnpm-lock.yaml\n"):
            with self.subTest(extra=extra):
                self.assertRejects(self.text.replace(node, node + extra, 1), "setup-node")

    def test_mutation_permissions(self):
        self.assertRejects(self.text.replace("permissions:\n  contents: read\n", "", 1), "permissions")
        self.assertRejects(self.text.replace("contents: read", "contents: write", 1), "permissions")
        self.assertRejects(self.text.replace("contents: read\n", "contents: read\n  id-token: write\n", 1), "permissions")
        self.assertRejects(self.text.replace("  plan:\n", "  plan:\n    permissions:\n      contents: write\n", 1), "widens")

    def test_mutation_timeout(self):
        self.assertRejects(self.text.replace("    timeout-minutes: 5\n", "", 1), "timeout")
        self.assertRejects(self.text.replace("timeout-minutes: 120", "timeout-minutes: 300", 1), "timeout")

    def test_mutation_unpinned_action(self):
        self.assertRejects(re.sub(r"actions/checkout@[0-9a-f]{40}", "actions/checkout@v4", self.text, count=1), "SHA-pinned")

    def test_mutation_expression_in_run_body(self):
        marker = '          set -eu\n          case "$COUNT"'
        for expr in ("${{ inputs.filter }}", "${{ github.event.inputs.filter }}",
                     "${{ inputs['filter'] }}", "${{ format('{0}', inputs.filter) }}",
                     "${{ github['event']['inputs']['filter'] }}",
                     "${{ fromJSON(toJSON(inputs)).filter }}", "${{ matrix.copy }}",
                     "${{ secrets.SCCACHE_R2_RO_ACCESS_KEY_ID }}", "${{ github.token }}"):
            with self.subTest(expr=expr):
                mutated = self.text.replace(marker, f"          set -eu\n          echo {expr}\n          case \"$COUNT\"", 1)
                self.assertRejects(mutated, "interpolates an expression")

    def test_mutation_expression_not_on_the_allowlist_anywhere(self):
        anchor = "          SCCACHE_CI_ENDPOINT: ${{ vars.SCCACHE_R2_ENDPOINT }}\n"
        for expr in ("github.token", "github['token']", "secrets['X']", "toJSON(secrets)",
                     "secrets.GITHUB_PAT", "secrets.SCCACHE_R2_RW_ACCESS_KEY_ID",
                     "inputs['filter']", "format('{0}', inputs.filter)",
                     "github.event.inputs.filter", "vars.OTHER"):
            with self.subTest(expr=expr):
                self.assertRejects(self.text.replace(anchor, anchor + f"          T: ${{{{ {expr} }}}}\n", 1), "allowlist")

    def test_mutation_secret_reach(self):
        self.assertRejects(self.text.replace("secrets.SCCACHE_R2_RO_SECRET_ACCESS_KEY", "secrets.GITHUB_PAT", 1), "not the read-only")

    def test_mutation_job_level_uses_or_secrets(self):
        self.assertRejects(self.text.replace("  plan:\n", "  plan:\n    uses: ./.github/workflows/ci.yml\n    secrets: inherit\n", 1), "reusable workflow")
        self.assertRejects(self.text.replace("  plan:\n", "  plan:\n    secrets: inherit\n", 1), "reusable workflow")

    def test_mutation_loop_does_not_use_the_wrapper(self):
        mutated = self.text.replace('scripts/cadence-nextest --locked ${FEATURES', 'cargo nextest run --locked ${FEATURES', 1)
        self.assertRejects(mutated, "loop step must run")

    def test_mutation_retries_override(self):
        self.assertRejects(self.text.replace('-E "$FILTER" > "$log"', '-E "$FILTER" --retries 3 > "$log"', 1), "retries")

    def test_mutation_stale_junit_not_removed(self):
        self.assertRejects(self.text.replace("            rm -f target/nextest/cadence/junit.xml\n", "", 1), "junit")

    def test_mutation_summary_not_always(self):
        self.assertRejects(self.text.replace("name: Summarize this copy\n        if: ${{ always() }}", "name: Summarize this copy\n        if: ${{ success() }}", 1), "summary")

    def test_mutation_burner_kill_not_always(self):
        self.assertRejects(self.text.replace("if: ${{ always() && inputs.load }}", "if: ${{ inputs.load }}", 1), "always()")

    def test_mutation_cargo_env_differs(self):
        self.assertRejects(self.text.replace("  RUSTFLAGS: -D warnings\n", "", 1), "env must equal")
        self.assertRejects(self.text.replace("CARGO_TERM_COLOR: always", "CARGO_TERM_COLOR: never", 1), "env must equal")

    def test_mutation_checkout_ref_input(self):
        mutated = self.text.replace("        with:\n          persist-credentials: false", "        with:\n          ref: ${{ github.event.inputs.x }}\n          persist-credentials: false", 1)
        self.assertRejects(mutated, "checkout must use")

    def test_mutation_ref_input_returns(self):
        mutated = self.text.replace("    inputs:\n", "    inputs:\n      ref:\n        type: string\n", 1)
        self.assertRejects(mutated, "inputs differ")


if __name__ == "__main__":
    unittest.main()
