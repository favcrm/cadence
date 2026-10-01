#!/usr/bin/env python3
"""CAD-926: only a push to main saves the gate jobs' rust-cache.

Stdlib only, like test_ci_sccache.py. It inspects the checked-in workflow.

Why: queue-evidence skips the gate jobs on a main push, so main never saved
their caches, while every gh-readonly-queue ref saved ~1.4 GB that no other
ref can read (12.7 GB of caches against the 10 GB limit, merge_group hit rate
25%). Now `cache-warm` (push to main) is the only saver, under the shared keys
the gates restore, and every pull_request / merge_group job passes
`save-if: false`.
"""
from pathlib import Path
import os
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = Path(os.environ.get("CADENCE_CI_WORKFLOW") or ROOT / ".github/workflows/ci.yml")
ACTION = "Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6"
WARM = "cache-warm"
# Gate job -> the shared key it restores (and cache-warm's matrix leg saves).
GATE_KEYS = {
    "clippy": "gate-clippy",
    "test-shard": "gate-test",
    "test-once": "gate-test",
    "build": "gate-release",
    "ui": "gate-release",
}
# Jobs that run on pull_request / merge_group but are not gates: restore only.
OTHER_RESTORE_ONLY = ("cross-build",)
# Release jobs never run on pull_request / merge_group (main push / v* tag),
# keep their own default-keyed cache and are deliberately untouched.
UNTOUCHED = ("release-artifact", "release-build")


def job_names(workflow):
    jobs = workflow.split("\njobs:\n", 1)[1]
    return re.findall(r"(?m)^  ([a-z][a-z-]*):$", jobs)


def job_body(workflow, name):
    body = workflow.split(f"\n  {name}:\n", 1)[1]
    return re.split(r"\n  [a-z][a-z-]*:\n", body, maxsplit=1)[0]


def cache_steps(body):
    """(step text) for every rust-cache step in a job body, in order."""
    steps = re.split(r"(?m)^      - ", body)[1:]
    return [st for st in steps if "uses: Swatinem/rust-cache@" in st]


def step_index(body, marker):
    steps = re.split(r"(?m)^      - ", body)[1:]
    return [i for i, st in enumerate(steps) if marker in st]


def field(step, name):
    m = re.search(rf"(?m)^          {name}: (.+)$", step)
    return m.group(1).strip() if m else None


def warm_keys(workflow):
    """The shared keys cache-warm saves, with the matrix expanded."""
    body = job_body(workflow, WARM)
    legs = re.search(r"(?m)^        profile: \[(.+)\]$", body)[1].split(",")
    keys = set()
    for st in cache_steps(body):
        key = field(st, "shared-key")
        assert key, "cache-warm rust-cache needs a shared-key"
        for leg in (l.strip() for l in legs):
            keys.add(key.replace("${{ matrix.profile }}", leg))
    return keys


def assert_cache_scope(case, workflow):
    # One pinned action everywhere: a different pin has a different cache
    # format/key and would silently never hit.
    uses = set(re.findall(r"Swatinem/rust-cache@\S+", workflow))
    case.assertEqual(uses, {ACTION}, uses)

    # Every gate and every other pull_request/merge_group job: restore-only.
    for job in (*GATE_KEYS, *OTHER_RESTORE_ONLY):
        steps = cache_steps(job_body(workflow, job))
        case.assertEqual(len(steps), 1, job)
        case.assertEqual(field(steps[0], "save-if"), "false", f"{job} must not save")
    # Nothing else may save either, unless it is cache-warm or an untouched
    # release job that cannot run on pull_request/merge_group.
    for job in job_names(workflow):
        if job in (WARM, *UNTOUCHED, *GATE_KEYS, *OTHER_RESTORE_ONLY):
            continue
        case.assertEqual(cache_steps(job_body(workflow, job)), [], f"{job} unexpectedly uses rust-cache")
    for job in UNTOUCHED:
        steps = cache_steps(job_body(workflow, job))
        case.assertEqual(len(steps), 1, job)
        case.assertNotIn("save-if", steps[0], job)
        case.assertNotIn("shared-key", steps[0], job)
        # Still unreachable from a PR or queue run.
        body = job_body(workflow, job)
        case.assertTrue(
            re.search(r"(?m)^    if:.*(?:\n      .*)*?event_name == 'push'", body)
            or re.search(r"(?m)^    needs: \[release-gate\]$", body), job)

    # The only saver is cache-warm, and only on push to main.
    warm = job_body(workflow, WARM)
    case.assertIn("if: ${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}", warm)
    for st in cache_steps(warm):
        case.assertIsNone(field(st, "save-if"), "cache-warm must use the default (save)")
    saved = warm_keys(workflow)
    case.assertEqual(saved, set(GATE_KEYS.values()))

    # Key agreement: what a gate restores is what cache-warm saves, and the
    # gate sharing the key builds the same profile.
    for job, key in GATE_KEYS.items():
        case.assertEqual(field(cache_steps(job_body(workflow, job))[0], "shared-key"), key, job)
        case.assertIn(key, saved, job)

    # The key hashes RUST*/CARGO* env: the rust-cache step must precede
    # `ci-sccache enable` (which exports RUSTC_WRAPPER) in every job that
    # restores or saves, or the keys diverge and the hit rate is 0%.
    for job in (*GATE_KEYS, WARM):
        body = job_body(workflow, job)
        enable = step_index(body, "scripts/ci-sccache enable")
        cache = step_index(body, "uses: Swatinem/rust-cache@")
        case.assertEqual(len(cache), 1, job)
        case.assertLessEqual(len(enable), 1, job)  # clippy never enables sccache
        for i in enable:
            case.assertLess(cache[0], i, job)


class CacheScope(unittest.TestCase):
    def setUp(self):
        self.workflow = WORKFLOW.read_text()

    def test_cache_scope_contract_holds(self):
        assert_cache_scope(self, self.workflow)

    def test_a_gate_job_that_saves_is_rejected(self):
        for job in (*GATE_KEYS, *OTHER_RESTORE_ONLY):
            body = job_body(self.workflow, job)
            mutated_body = body.replace("          save-if: false\n", "", 1)
            self.assertNotEqual(body, mutated_body, job)
            with self.subTest(job=job), self.assertRaises(AssertionError):
                assert_cache_scope(self, self.workflow.replace(body, mutated_body, 1))

    def test_save_if_that_only_excludes_merge_group_is_rejected(self):
        body = job_body(self.workflow, "build")
        mutated = body.replace("save-if: false", "save-if: ${{ github.event_name != 'merge_group' }}", 1)
        with self.assertRaises(AssertionError):
            assert_cache_scope(self, self.workflow.replace(body, mutated, 1))

    def test_a_key_mismatch_between_gate_and_warm_is_rejected(self):
        body = job_body(self.workflow, "ui")
        mutated = body.replace("shared-key: gate-release", "shared-key: gate-ui", 1)
        self.assertNotEqual(body, mutated)
        with self.assertRaises(AssertionError):
            assert_cache_scope(self, self.workflow.replace(body, mutated, 1))

    def test_a_warm_leg_dropped_is_rejected(self):
        mutated = self.workflow.replace("profile: [test, release, clippy]", "profile: [test, release]", 1)
        self.assertNotEqual(mutated, self.workflow)
        with self.assertRaises(AssertionError):
            assert_cache_scope(self, mutated)

    def test_a_warm_saver_that_does_not_save_is_rejected(self):
        body = job_body(self.workflow, WARM)
        mutated = body.replace("          shared-key: gate-${{ matrix.profile }}\n",
                               "          shared-key: gate-${{ matrix.profile }}\n          save-if: false\n", 1)
        with self.assertRaises(AssertionError):
            assert_cache_scope(self, self.workflow.replace(body, mutated, 1))

    def test_a_new_job_with_a_saving_cache_is_rejected(self):
        mutated = self.workflow.replace("  secrets:\n", f"  extra:\n    steps:\n      - uses: {ACTION} # v2.9.2\n\n  secrets:\n", 1)
        self.assertNotEqual(mutated, self.workflow)
        with self.assertRaises(AssertionError):
            assert_cache_scope(self, mutated)

    def test_a_different_action_pin_is_rejected(self):
        mutated = self.workflow.replace(ACTION, "Swatinem/rust-cache@" + "0" * 40, 1)
        with self.assertRaises(AssertionError):
            assert_cache_scope(self, mutated)

    def test_enabling_sccache_before_the_cache_is_rejected(self):
        body = job_body(self.workflow, WARM)
        # swap the rust-cache step and the sccache enable step
        parts = re.split(r"(?m)^      - ", body)
        i = next(i for i, st in enumerate(parts) if "uses: Swatinem/rust-cache@" in st)
        j = next(i for i, st in enumerate(parts) if "ci-sccache enable" in st)
        parts[i], parts[j] = parts[j], parts[i]
        mutated = "      - ".join(parts)
        self.assertNotEqual(body, mutated)
        with self.assertRaises(AssertionError):
            assert_cache_scope(self, self.workflow.replace(body, mutated, 1))

    def test_release_jobs_changed_is_rejected(self):
        body = job_body(self.workflow, "release-artifact")
        mutated = body.replace(f"{ACTION} # v2.9.2\n", f"{ACTION} # v2.9.2\n        with:\n          save-if: false\n", 1)
        self.assertNotEqual(body, mutated)
        with self.assertRaises(AssertionError):
            assert_cache_scope(self, self.workflow.replace(body, mutated, 1))


if __name__ == "__main__":
    unittest.main()
