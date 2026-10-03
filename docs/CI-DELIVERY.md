# Integration and production candidates

## Current state: reduced-gate window

CAD-1073 / PR #738 retired the legacy integration suite. The required `test`
check compiles and runs four `safety_floor` tests; green means that floor
passed, **not** full behavior coverage. `fmt`, `clippy`, `build` and `ui`
remain required on PRs and merge groups. The committed `.github/reduced-gates`
marker and `scripts/require-full-gates` refuse release artifacts, candidate
staging and promotion from a reduced-gate source SHA. Do not remove them as a
CI-speed optimization.

The shard/inventory benchmarks and old fixture journeys described below are
historical full-gate design, not current measurements or restored coverage.
See [development-loop simplification](DEV-CYCLE.md) for the #738 reflection,
current local check recipe and approval handoff.

## Full-gate delivery design (historical while reduced gates apply)

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

## Shared sccache on Cloudflare R2 (CAD-904)

Rust jobs (`test-shard`, `test-once`, `build`, `ui`) can share
compiled artifacts through an sccache backend on the R2 bucket
`cadence-ci-sccache` (30-day object expiry). GitHub caches are scoped per
branch, so merge-queue refs could not read each other's rust-cache; R2 is
not. rust-cache stays: it caches the registry, git dependencies and the
compiled dependency artifacts in `target/`, and is not changed here (its
save scope is a separate ticket). Expect a modest gain: the `cadence`
crate and the test binaries depend on the changed library, so they always
miss on a PR. The win is dependency crates when rust-cache misses, mainly
`merge_group` runs. The real number comes from the post-merge measurement
(the ticket's acceptance), not from this change. `clippy` is left out: its
calls go through clippy-driver and are all non-cacheable.

The feature is a strict no-op without credentials. With no secrets (fork
PRs, or before setup) `scripts/ci-sccache enable` exits 0 without touching
the job, `RUSTC_WRAPPER` stays unset and the job compiles exactly as
before. Any failure after credentials resolve (download, checksum mismatch,
unreachable bucket, bad token) also fails open to the same state.
`release-artifact` (the attested build) never uses the cache.

Who may write (CI-SEC-2): **only the `cache-warm` job, on `push` to
`refs/heads/main`, holds the read-write key.** A queued PR's build scripts
and tests run in the merge_group gate jobs, so a read-write key there would
let that code poison the cache or exfiltrate the key. Therefore:

- `cache-warm` has `if: push && ref == refs/heads/main`, the static
  `environment: sccache-writer`, `continue-on-error: true`, and nothing
  `needs` it. It builds the profiles the gates use (the feature-on test
  profile and the release profile) so main fills the
  cache. It skips the builds when sccache did not enable.
- Every other job, including all gate jobs on `pull_request` and
  `merge_group` and on main pushes, gets only the read-only pair and no
  `environment:`.
- The guard evaluator in `tests/scripts/test_ci_sccache.py` (retired under CAD-1073) compares
  strings case-insensitively like GitHub and supports `==` and `!=`; any
  other expression form (functions, `!`) is rejected. The test also rejects
  `secrets[...]`, `toJSON(secrets)` and any computed `environment:`.
- `tests/scripts/test_ci_sccache.py` (retired under CAD-1073) (run in `fmt`) fails if an RW secret
  appears anywhere but `cache-warm`, if its `if` admits any event but push
  to main, if the writer environment is attached to another job, or if
  any job waits on it.

How it works:

- sccache 0.18.0 is pinned by version and archive SHA-256
  (`.config/sccache.sha256`, same shape as `.config/cargo-nextest.sha256`).
  It installs into `$CARGO_HOME/bin`, which the Landlock worker confinement
  test already grants.
- Read-only is enforced twice: a read-only bucket-scoped token, and
  `SCCACHE_S3_RW_MODE=READ_ONLY` (sccache `docs/S3.md`, v0.18.0).
  `SCCACHE_S3_NO_CREDENTIALS` means anonymous public access, not read-only,
  so it is not used. In read-only mode sccache's stats still count every
  miss as a "cache write error"; that is the local refusal to write, not
  a failed upload.
- Settings: `SCCACHE_BUCKET=cadence-ci-sccache`, `SCCACHE_REGION=auto`,
  `SCCACHE_ENDPOINT` from the repo variable `SCCACHE_R2_ENDPOINT`,
  `SCCACHE_S3_KEY_PREFIX=v1/rustc-<version>`, `CARGO_INCREMENTAL=0`.
  Each Rust job prints `sccache --show-stats` (and to the step summary),
  plus the tail of `SCCACHE_ERROR_LOG` when it is non-empty.
- `enable` runs before the first compiling step of every job (a contract
  test checks the order): in `test-shard` the inventory build is the shard's
  only compile.
- Credentials are exported to later steps of the job as `AWS_ACCESS_KEY_ID`
  and `AWS_SECRET_ACCESS_KEY` through `GITHUB_ENV`. That is the one way a
  restarted sccache server can still authenticate, but it means test
  processes inherit those names, and cadence treats `AWS_*` as provider
  credentials. The cadence tests plant their own values and pass; remember
  it when debugging an env-leak test.
- Live read-only proof: `test-once` runs `scripts/ci-sccache verify-ro`, one
  SigV4 `PutObject` of a throwaway key with the read-only credentials. R2
  must answer 403; a 2xx fails the job, an inconclusive answer only warns.
  It is a no-op without credentials or in read-write mode.
- Residual risk: PR jobs run PR code with the read-only token in their
  environment; the token is scoped to this one bucket and cannot write.

### Operator setup (once; nothing here is done by the agent)

1. Create two R2 API tokens scoped to the bucket `cadence-ci-sccache`
   (Cloudflare dashboard, R2, Manage API tokens, "Create API token", specify
   bucket): one **Object Read & Write**, one **Object Read only**.
2. Derive each S3 key pair from its token. The dashboard shows both values
   when you create a token. Otherwise: Access Key ID is the token's `id`
   (`GET /user/tokens/verify` or the create response), and the Secret Access
   Key is the lowercase hex SHA-256 of the token value. In a shell (the token
   stays out of argv):
   `read -rs TOKEN; printf %s "$TOKEN" | sha256sum | cut -d' ' -f1`.
3. Settings, Environments, New environment `sccache-writer`. Under
   Deployment branches and tags choose "Selected branches and tags" and add
   **one** pattern: `main`. No `gh-readonly-queue/*` rule, no tag rule, and
   no required reviewers (they would stall every main run).
4. Add the environment secrets `SCCACHE_R2_RW_ACCESS_KEY_ID` and
   `SCCACHE_R2_RW_SECRET_ACCESS_KEY` to `sccache-writer`.
5. Add the repository secrets `SCCACHE_R2_RO_ACCESS_KEY_ID` and
   `SCCACHE_R2_RO_SECRET_ACCESS_KEY` (Settings, Secrets and variables,
   Actions, Secrets). Fork PRs never receive them and fall back cleanly.
6. Add the repository variable `SCCACHE_R2_ENDPOINT` =
   `https://<account-id>.r2.cloudflarestorage.com`.
7. Verify: a PR's `sccache` step summary shows mode READ_ONLY. After the
   first main push, `cache-warm` shows mode READ_WRITE and later PR and
   queue runs show cache hits. To turn the feature off, delete the five
   values (four secrets and the variable); CI returns to today's behaviour with no workflow change.

`cache-warm` is the only job that references the environment, and only on
main pushes, so the environment never affects which jobs run on which event.
Because the job is skipped (not failed) on every other event, a missing or
misconfigured environment cannot block a PR or the merge queue.

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

## Local pre-push recipe (CAD-905, CAD-922)

`scripts/pre-push` is the one command to run before pushing. It stops at
the first failing step, judges every step by its exit code alone and prints
one line per step (`[ OK ]`, `[FAIL] name: exit N`, `[SKIP] name: why`):

1. `cargo fmt --all -- --check` (always);
2. `scripts/split-doctor-host --check` (live source inventory); legacy
   `scripts/split-map-sync --check` only when its inventory exists;
3. `cargo clippy --all-targets --locked -- -D warnings`, then the same with
   `--features test-seam`, when Rust or Cargo files changed;
4. `pnpm --dir ui run typecheck` when `ui/` changed;
5. the active Python test commands that CI's `fmt` job runs, including
   review-recipe and release-freeze regressions, when `scripts/`, `.github/`
   or `tests/scripts/` changed;
6. with `--tests`, `cargo test --locked --test safety_floor -- --test-threads 2`
   regardless of whether the floor's source appears in the diff.

"Changed" is the union of commits since `origin/main`, working-tree edits
and untracked files; use `--base REF` to override and `--list` to print the
plan without running it. It sets `CARGO_BUILD_JOBS=4` unless already set and
does not run the full suite. During CAD-1073 the merge queue runs the same
reduced floor, not a hidden full integration suite.

The legacy `tests/split-map*.toml` and `split_map_inventory` contract were
retired. Do not run `split-map-sync` against missing inputs; see
[SPLIT-MANIFESTS.md](SPLIT-MANIFESTS.md) for the retained doctor source guard.

## Shared CI contracts

The required `fmt` job runs the shared scope/runner, shard-coverage,
nextest-cost, delivery/staging/review-observation contracts and doctor/host
split-map check once. Their failures still block the required gate and
release evidence. They no longer repeat in every Rust test shard.
`tests/scripts/test_ci_shared_checks.py` (retired under CAD-1073) checks that each command remains
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

## Rust toolchain pin (CAD-927)

`rust-toolchain.toml` pins an exact Rust release (`channel = "x.y.z"`, never
`stable`). It is the only place the version is written. Every workflow
installs Rust through `scripts/ci-rust-toolchain`, which reads the channel
from that file and runs `rustup toolchain install <channel>` with the
profile and components the job passes. `--export` also sets
`RUSTUP_TOOLCHAIN` for jobs that build another checkout (staging's
`candidate-source`). Local `cargo` follows the file too, so a local
`cargo clippy` matches CI. `tests/scripts/test_ci_toolchain_pin.py` (retired under CAD-1073) fails
if a workflow installs a literal or floating channel.

A new stable release can no longer turn the queue red: lints and
`-D warnings` change only when the pin changes. Because the cache keys
(`Swatinem/rust-cache` `shared-key: gate-*`, and sccache) hash the rustc
version, the first main run after a bump re-warms them; the key prefixes
do not change.

To bump the pin, in one PR:

1. Edit `channel` in `rust-toolchain.toml` and nothing else for the
   version. `rustup toolchain install` the new version locally.
2. Run `cargo clippy --all-targets --locked -- -D warnings` (and again
   with `--features test-seam`) plus `cargo fmt --all -- --check`, and fix
   every new lint or format change in the same PR.
3. Run `scripts/pre-push`. The PR touches `rust-toolchain.toml`, so expect
   it to be reviewed like any other CI change.
