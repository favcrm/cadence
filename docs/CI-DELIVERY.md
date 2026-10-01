# Integration and production candidates

PRs continue to target main. The merge queue keeps the full fmt, clippy,
test, build and UI gates. Main CI reuses exact-SHA queue evidence and
builds an attested artifact. A successful main build is available for
staging; it is not automatically a production candidate.

## Stage a selected batch

After this workflow lands on main, select the successful main CI run
that holds the desired artifact and dispatch:

```sh
gh workflow run staging.yml -R favcrm/cadence --ref main \
  -f ci_run_id=<main-ci-run-id> -f baseline_ci_run_id=<previous-production-ci-run-id>
```

The staging job verifies the run's workflow, repository, main ancestry,
SHA, attempt, manifest, digest and build provenance. It downloads the
embedded-UI release binary and runs the existing MVP journey against
those exact bytes, with the candidate revision's fixtures. Providers
and GitHub are faked; the daemon, board and browser are real and isolated.
It uses a temporary HOME/state/tracker and board port 3186 on a hosted
runner. No production state, credentials or daemon are accessed.

The previous release is independently downloaded and attested. A second
fixture starts its daemon, registers an inbox agent, records a rollout
lease and consistent backup, starts the candidate on that historical
schema, verifies integrity and agent identity preservation, then restores
the backup and starts the old binary. A migration/recovery failure blocks
promotion. This synthetic fixture exercises startup/schema compatibility;
it does not claim a rehearsal of real production data or live turns.

Review the staging evidence and approve the production environment job.
That job downloads and verifies the artifact again after the approval
wait, refuses a changed attempt or digest, then publishes a
`production-candidate` receipt. Approval records readiness; it does not
install or restart production. Only a successful workflow whose `stage`
and `promote` jobs both passed can supply the default updater's candidate.

`cadence update --check`, `cadence update`, and
`cadence upgrade --latest-main` now select that approved artifact. The
resolver pins its CI run, attempt and digest, and does not fall back to
an unapproved green build. A rerun that changes the artifact requires
new staging and approval. Existing attestation, ancestry, backward-move,
operator, backup, drain, health and rollback checks remain in force.

Bootstrap requires staging and approving the first candidate before
using the new default updater. Older installed updaters still select
green main until the rollout owner installs this change. Explicit
`cadence upgrade --sha <sha>` remains an operator recovery path using its
existing attestation/CI checks; it does not claim production approval.

## GitHub environment setup

Before enabling production promotion, configure `staging` to admit main
only, and `production` to admit main only with the existing release
operator as a required reviewer. Disable admin bypass for production.
The workflow uses read-only repository/Actions permissions and holds no
production deployment credentials. The rollout owner remains the sole
installer. A separate staging branch is unnecessary.

## Mutation experiments

Feature-branch pushes no longer trigger the entire ordinary CI suite.
Deliverable PRs and merge groups still do. Dispatch a deliberate guard
removal separately:

```sh
gh workflow run mutation.yml -R favcrm/cadence --ref main \
  -f revision=<mutation-sha> -f target=<integration-test-target> \
  -f test=<exact-test-name>
```

The trusted main workflow installs the pinned nextest binary and runs
only the selected adversarial test. A killed mutation needs nextest's
test-failure exit code and a fresh JUnit report containing exactly that
one failed test. Compilation/setup failure, zero tests, a skipped test,
an error or a different failed test makes the experiment fail. Ordinary
CI must still prove the original implementation passes the test.

## PR test selection and review coverage

The required `test` job reads its selection policy from the PR base.
A missing base policy or any uncertainty runs the full suite. Documentation
edits, even alongside isolated test edits, also run the full Rust scope:
Rust can read Markdown contents through a directory walk or constructed
path without a literal filename reference. Benchmark run 36298074797
measured 9 seconds for docs-selected versus 797 seconds for docs-full in
this test job; this change gives up that fast path, not a measured whole-PR
wall-clock saving. Fmt, clippy, build and UI remain required.

An isolated top-level integration test edit runs its Cargo target plus
all lib/bin tests and the split-map inventory contract. A reference from
another tracked source makes the file shared. Production source, shared
fixtures, manifests, workflows, tools and unknown paths run all targets.
The selection is deliberately conservative, not a Rust dependency graph.
Non-PR events retain all targets; exact-SHA main queue-evidence reuse is
unchanged. Inventory parity, pinned nextest, zero retries, default-feature
refusal proofs and doctests remain for every Rust selection. Each run
keeps its plan beside its JUnit timing artifact.

`cadence review` runs the configured lib/bin/board baseline once, plus
the existing gates, new-test stress and isolated failure comparisons.
Reports label this `review-baseline`; it is not full CI coverage. The
legacy `full_suite` key and `--no-full` option remain compatible; the
option now skips that configured baseline. Older project recipes default
to `full`. Recipes still come from the base revision.

The manually dispatched `CI selection benchmark` compares full and
selected jobs against identical source for two supplied existing paths
(an eligible isolated integration test and ordinary documentation).
Its artifacts explicitly label these as replay scenarios, not live PR
before/after measurements. Job start/end timestamps measure wall time;
sum job durations for runner minutes. Add unchanged required-job costs
and a full merge-group run when estimating a landing. Do not describe
replay timings as measurements of a production-code PR: those changes
currently receive the full fallback.

## Shared CI contracts

The required `fmt` job runs the shared scope/runner, shard-coverage,
nextest-cost, delivery/staging/review-observation contracts and doctor/host
split-map check once. Their failures still block the required gate and
release evidence. They no longer repeat in every Rust test shard.
`tests/scripts/test_ci_shared_checks.py` checks that each command remains
once-only and blocking, including early failures in multi-command steps.

Each `test-shard` job selects the scope from the base policy, proves
Cargo/nextest inventory parity, compiles on a rust-cache hit, builds its
SPA, executes its assigned tests and uploads assignment/cost artifacts.
The required `test` aggregate verifies complete, disjoint coverage from the
assignment receipts (eight automatically; four or eight on a manual
benchmark). Default-feature refusal proofs and doctests
remain in `test-once`. Exact-SHA main queue-evidence reuse is unchanged.

## Why shards compile themselves (CAD-869)

CAD-858 (#603) tried a serial `test-build` producer that archived the
compiled tests once and handed a 2.5 GB nextest archive to eight consumers.
Measured, it doubled the critical path: shards could not start until the
~5 min producer finished (run 36795228728: `test-build` 00:15:23-00:20:14Z,
shards 00:20:16-00:24:47Z, 10 min total), and each shard still spent 3.5-4.3
min on archive download, verification, extraction and the tests themselves.
Before it, shards started at t=0 and compiled against a warm rust-cache for
a 5-6 min run (merge-group run 36794979053, ~5.5 min). Compile-once saves
little when the dependency cache already makes each shard's compile cheap,
and the serial producer plus transfer costs more than it saves.

CAD-869 removed the producer, the archive/bundle scripts, the pinned
container runtime and their contract tests. Shards compile in parallel as
before; the CAD-854 move of the shared contracts into `fmt` is kept. Test
sets are unchanged: the same LPT weights, filter equality, eight disjoint
assignment receipts and zero retries, enforced by `scripts/ci-shard-check.py`
in the `test` aggregate. Revisit build-once distribution only with a
producer that is not serial on the critical path and a measured win over a
cache-hit shard.

## CI throughput benchmark (CAD-840)

Automatic PR, merge-group, main and tag CI stays eight-way. A manual
`ci.yml` dispatch can run the **same full suite** four-way or eight-way:

```sh
# Use one reviewed, frozen branch/ref for both dispatches.
gh workflow run ci.yml -R favcrm/cadence --ref <frozen-ref> -f test_shards=4
gh workflow run ci.yml -R favcrm/cadence --ref <frozen-ref> -f test_shards=8
```

The matrix, partition denominator and assignment aggregate use the same
width. Each width must prove its complete inventory union, with no missing,
duplicate or out-of-range shard. Default-feature refusal proofs, doctests,
fmt, both clippy shapes, build and UI checks remain. Cross-build runs only
on pull_request and merge_group events, and the secrets scan only on
pull_request, so manual runs at either width skip both equally. Manual
runs do not publish a release or reuse a main push's queue evidence.
Benchmark dispatches have their own concurrency groups, so they cannot
hold up an unrelated run sharing the main ref.

Do not dispatch both trials simultaneously merely to compare them: they
would compete with each other. Alternate widths across multiple trials,
with similar background PR load and cache/toolchain state. Dispatching a
benchmark consumes a full CI run; no scheduled extra runs are added here.
The workflow must be available on the default branch before GitHub accepts
manual dispatches. Freeze the ref, then verify the returned runs have the
same `head_sha`; a moving main is not a controlled comparison.

Read timing evidence (requires read-only Actions access via `gh`):

```sh
python3 scripts/ci-throughput.py --run-id <run-id>
python3 scripts/ci-throughput.py --run-id <four-run-id> \
  --compare-run-id <eight-run-id> --json > throughput-comparison.json
```

The report separates **dispatch-to-start delay** from job execution.
Dispatch-to-start includes dependency wait and scheduling; it is not pure
runner queue time. Job `created_at` can change on reruns, so subtracting
it from `started_at` may produce a negative, false queue duration.
Workflow elapsed uses the run's completion observation (`updated_at`);
gates elapsed ends at the latest required gate. Summed runner-minutes are
executed job wall time, not CPU time or GitHub billed minutes. Skipped jobs
add no time. Inventory/build and test step times expose repeated compilation.

Single-run reports can describe failed/incomplete evidence, clearly marked
as partial. Comparisons refuse incomplete/red runs, missing or duplicate
gates, malformed timing, unknown shard layouts, reruns, different SHAs,
non-manual runs or a same-width pair. The reporter uses the exact attempt's
paginated job inventory; it never mixes attempts, posts statuses or changes
runner/repository settings. It is observational evidence, not merge approval
or an independent replacement for the assignment coverage gate.

For offline review, provide raw API responses with `--run run.json --jobs
jobs.json`; compare a second pair using `--compare-run other-run.json
--compare-jobs other-jobs.json`. The jobs file has the shape
`{"jobs": [...]}`. Keep the raw responses with the report. Every job must
carry the run's `run_id`, `head_sha` and `run_attempt`, and a run cannot be
compared with itself. Offline reports are labelled `source:
offline-unverified`: the files are only as trustworthy as their provenance.

Choose a default only after repeated same-SHA trials show better gate
latency **and** acceptable runner-minutes under realistic load. Do not
claim a saving from one noisy run. Build-once nextest distribution and a
separate merge-queue runner pool remain follow-ups; neither is deployed
by this benchmark increment.

## Remaining delivery work

CAD-479 tracks measurement of affected-test selection; the queue retains
the full suite. Timing artifacts from CAD-638 provide evidence for balanced
shards. Neither optimization is a reason to reduce required coverage.
Independent review remains required by AGENTS.md; a required GitHub
review bridge must report both PR-head and merge-group checks before
activating it in branch protection. CAD-120 tracks reviewer identity
integration. Real-provider acceptance remains CAD-434; the fake-provider
MVP journey cannot prove provider compatibility or real fleet continuity.
