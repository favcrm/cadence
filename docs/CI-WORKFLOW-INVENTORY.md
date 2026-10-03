# Workflow inventory after #738 (CAD-1088)

This inventory was taken during the reduced-gate window; CAD-1102 ended that
window and made the current gate permanent. Required branch checks observed at the start of CAD-1088 are **fmt,
clippy, test, build, ui**; neither `test-shard` nor `test-once` is required.
No branch/ruleset settings are changed by this cleanup.

| Workflow | Triggers | Current role |
| --- | --- | --- |
| `ci.yml` | PR, merge group, main push, v tags, dispatch | Five required gates; exact-SHA queue reuse; isolated cache writer; guarded release chain |
| `staging.yml` | Filtered PR, dispatch, 15-minute schedule, main CI completion | PR migration rehearsal active; select/stage/promote refuse reduced source trees |
| `e2e.yml` | Nightly, v tags, dispatch | MVP journey retired; cargo-only seam-exclusion probe, not E2E coverage |
| `clean-install.yml` | Filtered PR, dispatch | Installer fixtures retired; native installation dispatch scaffolding remains |
| `handover.yml` | PR, main push | Shell syntax/shellcheck active; fixture echoes are retirement notices |
| `obsolete-merge-groups.yml` | Queue ref deletion, filtered PR | Active stale-run janitor; old contract fixture retired |
| `stress.yml` | Dispatch | Dormant legacy suite consumer; restore its inputs before use |
| `mutation.yml` | Dispatch | Legacy integration-target mutation scaffold; not proof for a deleted target |
| `test-feedback.yml` | Dispatch | Dormant legacy feedback/replay consumer |
| `ci-selection-benchmark.yml` | Dispatch | Dormant full-versus-selected integration benchmark |
| `nextest-shard-benchmark.yml` | Dispatch | Dormant shard/inventory benchmark |

Dormant dispatch files/scripts are retained as rebuild scaffolding in this
increment, not deleted or advertised as working checks. Do not dispatch them
as evidence that the retired suite passes. A separate decision should select
which to rebuild or archive rather than leaving five misleading manual tools
indefinitely. `cadence-nextest` itself is still live in the review recipe.

## Bounded simplification implemented here

- Add trusted-base PR feedback selection. Isolated docs: no Rust or UI
  compilation, including no cross-builds. Isolated UI source: UI validation
  and embedded build; unrelated default Rust clippy/floor/build commands are
  not applicable. All shared/config/workflow/script/mixed/unknown changes:
  full validation. Classifier missing or failing: full validation. Required
  contexts remain and say when commands were not run; merge-group, main,
  tags and dispatch always keep real full gates. This PR adds the policy
  for later PRs: its base lacks the classifier, so bootstrap is full.
- Fold the echo-only shard runner and separate doctest runner into required
  `test`. Run doctests and the four-test floor as separate blocking steps
  with one checkout/toolchain/cache. Saves two runner startups and removes
  predecessor wait; all real test commands remain. This may reduce runner
  minutes without shortening whole-PR latency if UI remains the longest leg.
- Restore the live `split-doctor-host --check` unconditionally. Its manifest
  is in `src/doctor/host`, not the retired integration manifests.
- Run offline local-plan/workflow regressions from `fmt` and local pre-push.
- Remove Node/pnpm setup and obsolete fixture artifact paths from the
  cargo-only nightly probe. Require the actual seam compile-error message;
  a network/toolchain error must not look like a passing refusal proof.
- Remove obsolete staging PR filters and fix the candidate UI cache path.
  Preserve every release guard and its ordering. Stage frontend setup remains
  as restoration scaffolding in this increment; it was behind the freeze, which CAD-1102 lifted.
- Align local/documented commands with the reduced-window state.

## Deliberately not changed

`build` and `ui` release compilation partly overlap, but cover different
feature shapes. Do not delete either job or feature coverage to claim a
speedup. Consolidation needs measured warm/cold comparisons and a plan for
stable required names. `cache-warm` alone owns write credentials; no shortcut
may give them to PR or queue code.

During the CAD-1073 window, main release-artifact attempts were deliberately
**red** (now ended by CAD-1102, so they pass require-full-gates). If a future
window re-adds the marker, keep them red: `continue-on-error` would conceal an
enforced refusal. A future eligibility/reporting
step could explicitly mark a frozen release as ineligible *before* requesting
a release, with fail-closed exact-source-tree tests; keep the guard as defense
in depth and distinguish eligibility from successful release evidence.
Staging eligibility, release provenance, attestation/publish permission split,
required checks, queue evidence reuse and operator authority are unchanged.

## Measurement

Baseline PR run 37103055285: UI 225s, build 201s, test 141s, clippy 91s, fmt
18s. Compare this cleanup's PR and queue run after landing with similar cache
and runner conditions; report elapsed time and summed runner minutes
separately. A removed startup is a structural saving, not a measured claimed
one-minute improvement. Four hours of #738 coordination is addressed by the
[development loop](DEV-CYCLE.md), not by weakening these four-minute checks.
