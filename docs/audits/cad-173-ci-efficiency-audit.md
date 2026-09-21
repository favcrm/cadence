# CAD-173 CI efficiency audit

**Date:** 2026-09-21 (UTC)  
**Audit branch:** `cadence/cad-173-efficiency-audit`  
**Baseline:** PR94 run [35558415443](https://github.com/favcrm/cadence/actions/runs/35558415443), head `8ee49303bebc0d0225e6717e345304cf349fd1bb`  
**Base tree used for this document:** `6279117b9378ba823ed4a0a6ef8ce3e6b052c156`

This is a read-only efficiency audit of the PR94 CI shape. It does not
change PR93 or PR94, activate a test runner, alter a required check, or claim
that a local benchmark represents GitHub. The measurement is one successful
GitHub run, so recommendations that need repeated-run evidence remain
proposals with validation criteria.

## Findings

The PR94 run used **678 elapsed runner-seconds (11.30 runner-minutes)** when
the five CI job intervals are added. The separate Handover run for the same
head added about **10 seconds (0.17 runner-minutes)**, so the observed
CI-plus-Handover total was **688 seconds (11.47 runner-minutes)**. These are
elapsed job intervals; GitHub's billing-minute rounding is not exposed by the
run API and is not inferred here. The workflow's critical path was about
5m28s, with the test job lasting 5m25s.

| job | job interval | material steps | observed runner seconds |
| --- | ---: | --- | ---: |
| build | 85s | release Cargo build 80s | 85 |
| fmt | 7s | format check 4s | 7 |
| test | 325s | inventory 37s; nextest 280s; doctests about 1s | 325 |
| ui | 219s | UI release Cargo build 197s; pnpm setup/build/typecheck | 219 |
| clippy | 42s | clippy 36s | 42 |
| **CI total** | — | — | **678s / 11.30m** |

The test job reported **662 passed and 0 skipped**, with one test reported as
slow. Its nextest step's summary duration was 279.150s. The all-target
inventory and the runner are separate steps: the 37 seconds includes Cargo
and nextest listing, dependency/setup work, and the parity check, while the
279.150 seconds is the execution step. The log does not expose a compile-only
split for the test job, so the 37 seconds must not be presented as pure
compilation or the 279.150 seconds as pure test-body CPU time.

The per-test duration sum was 1,113.905 seconds across the 279.150 second
runner step, an effective parallelism of about 3.99 for this run. That is a
timing observation, not proof of a fixed worker count or of host contention.
The target breakdown from the nextest result was:

| target | tests | sum of reported test durations |
| --- | ---: | ---: |
| `cadence-agent` (library) | 263 | 6.633s |
| `cadence-agent::bin/cadence` | 26 | 0.237s |
| `cadence-agent::board` | 83 | 55.620s |
| `cadence-agent::integration` | 290 | 1,051.415s |
| **total** | **662** | **1,113.905s** |

The slowest reported tests were all integration tests. They are useful
investigation targets, not evidence that their timeout budgets can be
shortened safely:

| seconds | test |
| ---: | --- |
| 68.272 | `job_event_parks_on_unrendered_pty_pm` |
| 40.495 | `pty_unrendered_worker_result_requeues_then_parks` |
| 36.232 | `pty_stall_transient_sample_neither_resumes_nor_resets` |
| 30.629 | `pty_claude_resume_timeout_keeps_session` |
| 23.993 | `pty_shutdown_straggler_detaches_pane` |
| 22.328 | `pty_stall_resume_rearms_and_spinner_is_not_activity` |
| 21.680 | `fenced_agent_resume_hint` |
| 21.614 | `pty_dead_pane_fences_submitted_and_stops_actor` |
| 21.403 | `pty_hot_restart_submitting_never_records_running` |
| 18.509 | `pty_claude_busy_and_approval_gate_sends` |
| 18.054 | `pty_cursor_busy_and_approval_gate_sends` |
| 17.010 | `pty_unfence_resume_busy_adopted_pane_stays_gated` |

The source confirms why these are expensive. For example, the first two
exercise bounded message requeues and wait windows, the stall tests wait for
real sampling transitions, and `pty_claude_resume_timeout_keeps_session`
intentionally waits for a profile deadline. Several tests also use fixed
multi-second sleeps to model a stall timer. Replacing those waits with polls
or an injected clock may help, but shortening them without a failure-boundary
proof would weaken the tests. The first safe follow-up is instrumentation of
the wait/retry phases so wall time can be split into useful work, expected
timer simulation, and retry backoff.

## Cost and duplication observations

PR94's workflow has no Rust registry, git dependency, target, or compiler
cache. The UI job enables pnpm caching, but that does not help the Rust jobs.
The baseline log shows fresh dependency downloads in both the release build
and inventory paths. This supports investigating caching, but it does not
provide a cache-hit comparison or a measured download duration.

The workflow installs the floating `stable` toolchain independently in each
Rust job. No `rust-toolchain.toml` pins the compiler in the audited tree.
The install steps were short in this run, but reproducibility and cache-key
identity are still separate concerns from their current duration.

Two release compiles are visible and materially expensive: the ordinary
release build took 80 seconds and the UI-feature release build took 197
seconds. The UI feature changes the artifact, so reusing the ordinary binary
would be unsafe. Sharing dependency/target state or passing verified build
artifacts could reduce this cost, but either needs feature-aware keys and
validation that the artifact corresponds to the checked-out commit.

The CI workflow listens to `pull_request` and to pushes on `main` and
`feat/**`. PR94's concurrency group uses the PR number for pull requests and
the ref for pushes; only pull-request groups cancel in-flight work. Therefore
an open PR whose branch matches `feat/**` can have a pull-request CI run and
a push CI run in different groups. The audited PR94 branch was
`cadence/cad-173-ci-nextest`, so the exact head had only one CI run and one
Handover run; no duplicate was observed for the baseline. This is a workflow
risk with a useful baseline cost: at the observed job mix, each extra full CI
run represents 11.30 elapsed runner-minutes before billing rounding. A future
cache or workflow change may alter that number. Handover does not listen to
`feat/**` pushes, so it is not included in that duplicate-CI estimate.

The test job sets `CARGO_BUILD_JOBS=4` for inventory, nextest, and doctests.
The build, UI, and clippy jobs do not set it. The approximately four-way
effective test parallelism is measured from the result durations; no change
to worker count is justified by this one run. Increasing it could reduce
wall time while increasing CPU contention, and the jobs already run on
separate hosted runners, so this needs a paired benchmark rather than a
workflow guess.

## Ranked follow-ups

The expected savings below are estimates or bounds, clearly separated from
the measured baseline.

1. **Deduplicate `feat/**` push and pull-request CI events.**
   This has the clearest cost ceiling: one avoided duplicate saves roughly
   11.30 elapsed runner-minutes at the observed job mix. Choose either an
   event policy (remove the redundant branch push trigger) or a concurrency
   expression that deliberately shares the PR and push group. Before editing,
   enumerate required status contexts for an open `feat/**` PR and verify
   that a push from a PR branch cannot cancel a newer PR commit. The baseline
   supplies no duplicate instance, so this should be a separate workflow PR,
   not an unreviewed change in the audit.

2. **Add a safe Cargo dependency cache, then measure exact and restored runs.**
   Start with registry and git sources only, keyed by runner OS, the pinned
   Rust toolchain identity, and `Cargo.lock`; this has a small correctness
   surface and cannot make an old target binary executable. A target cache is
   a separate step: its key must include the compiler identity, lockfile,
   feature set, profile, and relevant build flags. A broad restore key may
   make the cache look warm while still forcing recompilation, so report hit
   status and compile steps separately. Expected savings are currently
   **unknown**; only the presence of fresh downloads is measured.

3. **Measure feature-aware target/artifact reuse for the two release jobs.**
   The measured upper bound is 277 seconds of release-build wall time per
   CI run (80s + 197s), not a promise of 277 seconds saved. A candidate must
   prove that the ordinary and UI-feature outputs are not confused, that
   artifacts are tied to the exact commit and toolchain, and that a cold
   cache has no worse required-check behavior. Keep the ordinary and UI
   builds separate until that proof exists.

4. **Instrument the ten slowest pty tests before changing their waits.**
   Record retry count, poll count, timer budget, and the phase that completes
   for each test. Then optimize only waits shown to be scheduler slack or
   redundant polling. Preserve retry-zero, failure visibility, and all
   target coverage. The current run gives a 68.272s maximum and 279.150s
   test-step wall time; it does not give a safe savings estimate.

5. **Revisit inventory cost only after preserving parity evidence.**
   The 37s inventory step is a required non-empty Cargo/nextest name-set
   comparison for all targets. It must not be removed or replaced with a
   filtered run. A future optimization may reuse a verified compile/list
   artifact within the same job, but must retain the exact count and name-set
   comparison and separately run doctests.

6. **Review timeout ceilings after repeated telemetry.**
   PR94's 20-minute limits are several times the observed build/test job
   durations. Lowering them could reduce hung-run cost, but one successful
   run cannot establish a tail bound. Collect at least two cold and two warm
   runs per relevant job before changing a timeout, and keep a timeout failure
   distinct from a test failure.

## Concrete follow-up tickets

These are intentionally separate from PR93/94 and can be dispatched without
changing either branch:

| ticket proposal | scope | acceptance evidence |
| --- | --- | --- |
| CAD-173-E1 | CI event matrix and `feat/**` duplicate suppression | event matrix for PR, branch push, and main push; required-check proof; one fixture or GitHub run showing obsolete work cancels safely |
| CAD-173-E2 | Cargo registry/git cache with pinned toolchain key | cold and restored runs on one SHA; cache hit/miss recorded; all jobs still use `--locked`; no target artifact trust change |
| CAD-173-E3 | Feature-aware target/artifact reuse study | ordinary/UI outputs remain distinct; exact SHA/toolchain/feature proof; paired runner-minute result; rollback path |
| CAD-173-E4 | pty wait-phase telemetry and bounded optimization | per-test phase timings for the listed slow tests; focused regressions; no timeout or retry weakening |
| CAD-173-E5 | Inventory/list cost reduction study | non-empty exact name parity remains enforced for all targets and doctests remain explicit |

## Validation plan and limits

An Ops-admitted CI slot is required for any new full inventory or suite
benchmark on the shared host. The next measurement should use one pinned
commit and record, separately for cold and warm trees:

- workflow run, commit, runner image, Rust toolchain, and nextest version;
- every job interval and every compile, inventory, execution, and doctest
  step;
- total runner-minutes across all jobs, not only the critical path;
- test count, passed/failed/skipped/no-execution status, and the same
  per-test timing extract;
- cache hit/miss state and key components;
- host load for local runs, with local evidence labelled separately from
  GitHub evidence.

Do not use the historical CAD-173 measurement (`c76effa`, 272 tests) as a
current CI guarantee. Do not claim a cache saving, duplicate-run saving, or
wait optimization until the corresponding paired evidence exists. This audit
adds no executable or workflow change because the single baseline run proves
where runner time is spent but does not prove which low-risk mutation will
save it without changing required-check or coverage behavior.
