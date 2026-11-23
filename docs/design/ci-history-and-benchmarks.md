# CI history and benchmarks (shard-era)

Historical measurements and design notes moved out of
[CI-DELIVERY.md](../CI-DELIVERY.md). The suite no longer runs on pull
requests and there are no test shards in `ci.yml`; none of this describes
current CI. Read it for the reasoning and the numbers, and re-measure before
reviving any of these designs.

## PR test selection (before the suite left the pull-request path)

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

CAD-1727 collapsed those targets into one `suite` binary and set
`autotests = false`, so the trusted-base classifier refuses target
narrowing for that shape and PRs run the full Rust scope. That is
intentional: the six shards partition the one binary's tests, and a full
build is now a single link.
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


## Shared CI contracts (shard era)

The required `fmt` job runs the shared scope/runner, shard-coverage,
nextest-cost, delivery/staging/review-observation contracts and doctor/host
split-map check once. Their failures still block the required gate and
release evidence. They no longer repeat in every Rust test shard.
`tests/scripts/test_ci_shared_checks.py` (retired under CAD-1073) checked
that each command remained once-only and blocking, including early failures
in multi-command steps.

The enqueue consumer checks run through
`python3 scripts/test_enqueue_requirements.py` in `fmt`. Its
`RequirementsConsumer` inherits all 18 evidence-posting checks and adds
10 requirements cases, so one command runs both sets without duplicating
the inherited checks. Local pre-push discovers that same command from the
workflow. These checks drive the real enqueue consumer with synthetic
external transports; they do not establish daemon identity or live
GitHub/merge-queue acceptance.

Each `test-shard` job selects the scope from the base policy, proves
Cargo/nextest inventory parity, compiles on a rust-cache hit, builds its
SPA, executes its assigned tests and uploads assignment/cost artifacts.
The `test` aggregate (historical: removed from `ci.yml` by CAD-1970) verified complete, disjoint coverage from the
assignment receipts (eight automatically; four or eight on a manual
benchmark). Default-feature refusal proofs and doctests
remain in `test-once`. Exact-SHA main queue-evidence reuse is unchanged.

Uploads are named `test-shard-<n>-attempt-<run_attempt>` (CAD-1745). A
failed-job-only rerun copies an untouched green partition into the new
attempt without re-running or re-uploading it, so the aggregate resolves
each copied row to the unique earlier execution it aliases (identical
start/completion timestamps and runner, later `created_at`). It then takes
every partition's latest *actual* execution and downloads exactly those
six artifact IDs. Earlier reports remain on the run as evidence, and a
non-green latest execution, missing or ambiguous provenance, or a missing,
expired or foreign-run artifact fails the aggregate instead of falling
back to an older success.

An indexed `queued`/`in_progress` shard row is reconciled once by immutable
job ID (CAD-1758): a successful matrix dependency can precede publication
of its terminal state in the jobs index. The current record must match
the row's ID, run, attempt, head and name before the same provenance and
latest-execution checks apply. Terminal indexed failures are never
refreshed into success. An unreadable, malformed, foreign or still
nonterminal record refuses the aggregate; there is no sleep, retry loop
or fallback to older evidence.

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

CAD-1727 changed that cost shape: 83 of the per-file test binaries are now
modules of one `suite` target, so a full test profile is 4.9 GiB (27
executables) instead of 16.8 GiB (109), and a shard links the suite once
plus the 15 explicitly declared targets. The CAD-869 measurement stands
for the tree it was taken on; an archive producer's size and transfer cost
are now different, so re-measure before reviving that flow.


## CI throughput benchmark (CAD-840)

The suite width is fixed: six `test-shards` partitions (`hash:k/6`,
`--jobs 4`). CAD-1073 retired the shard-suite benchmark, and the old
`workflow_dispatch` input `test_shards` (4 or 8) did not change that
matrix, so it is gone. A manual `ci.yml` dispatch runs the same suite as
a pull request. It does not publish a release or reuse a main push's
queue evidence. Cross-build runs only on pull_request and merge_group,
and the secrets scan only on pull_request, so a dispatch skips both.
Dispatch concurrency is its own group, so it does not cancel a pull
request or a main push. Dispatching still consumes a full CI run.

```sh
gh workflow run ci.yml -R favcrm/cadence --ref <frozen-ref>
```

The workflow must be on the default branch before GitHub accepts a
manual dispatch. Freeze the ref, then verify the returned run's
`head_sha`. A moving main is not a controlled comparison.

Read timing evidence (requires read-only Actions access via `gh`):

```sh
python3 scripts/ci-throughput.py --run-id <run-id>
python3 scripts/ci-throughput.py --run-id <run-id> \
  --compare-run-id <other-run-id> --json > throughput-comparison.json
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

Do not claim a saving from one noisy run. There is no width switch left
to choose: the gate is six shards. Build-once nextest distribution and a
separate merge-queue runner pool remain follow-ups; neither is deployed.

## Remaining delivery work

CAD-479 tracks measurement of affected-test selection; the queue retains
the full suite. Timing artifacts from CAD-638 provide evidence for balanced
shards. Neither optimization is a reason to reduce required coverage.
Fresh-session review remains required by AGENTS.md; a required GitHub
review bridge must report both PR-head and merge-group checks before
activating it in branch protection. CAD-120 tracks reviewer identity
integration. Real-provider acceptance remains CAD-434; the fake-provider
MVP journey cannot prove provider compatibility or real fleet continuity.

