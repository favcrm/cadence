# Workflow inventory after #738 (CAD-1088)

This inventory was taken during the reduced-gate window; CAD-1102 ended that
window and made the current gate permanent. Required branch checks observed at the start of CAD-1088 are **fmt,
clippy, test, build, ui**; neither `test-shard` nor `test-once` is required.
No branch/ruleset settings are changed by this cleanup. The last three rows
were added when those workflows landed (CAD-1455, CAD-1506, CAD-1475); the
rest of the table records the CAD-1088 snapshot.

| Workflow | Triggers | Current role |
| --- | --- | --- |
| `ci.yml` | PR, merge group, main push, v tags, dispatch (`native_fixture`) | Five required gates; exact-SHA queue reuse; isolated cache writer; guarded release chain; the C1 proof is opt-in per dispatch (CAD-1970) |
| `staging.yml` | Filtered PR, dispatch, 15-minute schedule, main CI completion | PR migration rehearsal active; select/stage/promote refuse reduced source trees |
| `e2e.yml` | Nightly, v tags, dispatch | MVP journey retired; cargo-only seam-exclusion probe, not E2E coverage |
| `clean-install.yml` | Filtered PR, dispatch | Installer fixtures retired; native installation dispatch scaffolding remains |
| `handover.yml` | PR, main push | Shell syntax/shellcheck active; fixture echoes are retirement notices |
| `obsolete-merge-groups.yml` | Queue ref deletion, filtered PR | Active stale-run janitor; old contract fixture retired |
| `stress.yml` | Dispatch | Manual flake loop over a nextest filterset; runs against the live suite |
| `mutation.yml` | Dispatch | Manual guard-removal proof; takes a live cargo test target and test name |
| `ci-selection-benchmark.yml` | Dispatch | Manual full-versus-selected integration benchmark |
| `nextest-shard-benchmark.yml` | Dispatch | Manual shard/inventory benchmark |
| `instabox-measure.yml` | Dispatch, filtered PR | Manual compile-time benchmark; since CAD-1970 also times one full unpartitioned suite on `instabox-large` |
| `promotion-completion.yml` | `staging` run completion on main | Binds a promoted candidate's signed receipt to the finished upstream run and attests a small completion receipt; it never approves, rebuilds or deploys (CAD-1455) |
| `release-mirror.yml` | Dispatch | Manual retry that copies an already published release into the owned release store; no build, retag or byte replacement (CAD-1506) |
| `release-visibility.yml` | Dispatch on main | Operator-owned private→public flip of one published catalogue record after the `production` environment approval; never writes artifacts, deletes or withdraws (CAD-1475) |

Dormant dispatch files/scripts are retained as rebuild scaffolding in this
increment, not deleted or advertised as working checks. Do not dispatch them
as evidence that the retired suite passes. A separate decision should select
which to rebuild or archive rather than leaving misleading manual tools
indefinitely. `cadence-nextest` itself is still live in the review recipe.

## Archived in the deep test audit

The dormant `test-feedback` workflow and its `scripts/test-feedback.py` were
archived: both allowlisted cases named cargo test targets (`backup_rollout`,
`threads`) that CAD-1073 deleted, so the workflow could not have run. The
`scripts/split-integration-tests` and `scripts/split-board-tests` generators
were archived for the same reason — their source suites (`tests/integration.rs`,
`tests/board.rs`) are gone, so neither could regenerate anything. The
`e2e` Cargo feature and `scripts/e2e/mvp.sh` went with the MVP journey they
existed for (`tests/e2e_mvp.rs`, `tests/e2e/`). `scripts/split-map-sync` and
`scripts/shard-weights` stay: they are inert until a reviewed inventory
restoration supplies their inputs, and `AGENTS.md` still names the split
manifests as retired rather than deleted.

## Bounded simplification implemented here

- Add trusted-base PR feedback selection. Isolated docs: no Rust or UI
  compilation, including no cross-builds. Isolated UI source: UI validation
  and embedded build; unrelated default Rust clippy/floor/build commands are
  not applicable. Edits confined to pre-existing independent top-level
  `tests/<name>.rs` targets narrow the `test-shards` matrix's integration
  selection only (lib/bins suites and every zero-target guard unchanged).
  The required `test` check is the aggregate: it merges every shard's
  junit and re-runs the coverage guard (CAD-1480). All
  shared/config/workflow/script/mixed/unknown changes: full validation.
  Classifier missing or failing: full validation. Required contexts remain
  and say when commands were not run; merge-group, main, tags and dispatch
  always keep real full gates. This PR adds the policy
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

## CAD-1970: the coverage contract, and what is deliberately not asserted

The shard matrix is unchanged. What changed is what a green shard *means*.

- **Coverage is judged against the binaries, not against the reports.** Each
  shard writes `target/nextest/cadence/inventory.json` — its own nextest
  `list --message-format json` for the same selection and features — and
  `scripts/check-test-junit.py --inventory` requires every selected
  `(binary id, test name)` to appear exactly once, green and unskipped. The
  required `test` aggregate passes `--complete-partition` over all six
  inventories and refuses a test no shard owns (a partition gap) or one two
  shards own (an overlap). A case dropped from all six reports therefore
  fails; before CAD-1970 it passed, because the guard only asked whether each
  target still had one runnable case. Agreement among shards was never
  coverage.
- **An `#[ignore]` is a reviewed line, not a silent exclusion.**
  `.config/nextest-ignored.txt` declares the fourteen ignored cases. The guard
  refuses an ignored case that is not declared, and a declaration whose case
  now runs. A binary absent from a narrowed selection does not make its lines
  stale.
- **`scripts/test_check_test_junit.py`** drives the real CLI over synthetic
  report/inventory sets for each refusal above; it runs from `fmt` like the
  other gate contracts. That is the author's change detection, not the
  independent negative acceptance the ticket requires, which still has to
  come from a reviewer.
- **The C1 native-fixture proof is a dispatch input.** `native-fixture-release-exclusion`
  now runs only with `workflow_dispatch` + `native_fixture=true`. Its script,
  its isolated runner and its assertions are untouched; what changed is that a
  plain manual dispatch on main no longer fails on an artifact from another
  lane.
- **`ci-selection-benchmark.yml` lost a step that could not run.** It invoked
  `--test test_seam`, a target CAD-1727 removed with `autotests = false`. The
  seam guard is `scripts/check-release-test-seam` in `build`; the benchmark was
  not that guard.
- **`instabox-measure.yml` gained a `full-suite` leg** that times one
  unpartitioned `scripts/run-result-tests` on `instabox-large`. It is
  measurement, not policy: it changes no required check and no runner label.

Deliberately not asserted: that one runner is faster. The six shards exist
because CAD-1480 measured the suite past a single runner's wall-time budget on
the runner available then. The first same-content comparison is now in:

| `f7915b475` (PR #1469), both cold | six shards | one `instabox-large` |
| --- | --- | --- |
| workers | 6 × `--jobs 4` | 1 × `--jobs 8` (8 vCPU, 16 GB) |
| summed runner-seconds | 2696 | 568 |
| created → last shard | 717 s | — |
| created → required `test` green | ≈735 s | 568 s |
| summed queue delay | 843 s | ≈7 s |

One run each, so no distribution. The single runner was 149 s faster on the
critical path and 4.75× cheaper in runner-seconds, and the shards spent 843 s
queued. That is still not the whole decision: both legs were cold, and the
unmeasured dimension is **throughput**, not latency — six shard jobs let six
runs share a pool, one large job per run shares a pool observed at ≥2
concurrent slots. Changing the matrix is its own PR with a warm-cache run and
a two-PR overlap measurement. Until then the matrix stays as it is.

Also deliberately unchanged: `test-shards` still compiles the workspace once
per shard. Reusing one archive across shards was measured only as cold
compilation (`instabox-measure` on `a9951a20`), which is not the end-to-end
question, and a `--archive-file` consumer needs the same absolute
workspace/target layout the producer used. That is a separate measured change.

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
