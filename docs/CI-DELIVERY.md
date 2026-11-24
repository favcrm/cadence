# Integration and production candidates

## Current state

Branch protection requires `fmt`, `clippy`, `build`, `ui`, `delivery-gate` and
`changed-tests`;
`enqueue-reviewed` reads the live set. The CI jobs prove the code compiles, lints
and builds the release seam, **not** behaviour coverage. The suite runs in
`tests.yml` on a schedule and on demand and is not a required check;
`changed-tests.yml` runs only the tests a PR adds or changes. A classifier loaded
from the base commit selects PR feedback: isolated docs use no compilation;
isolated UI code runs UI validation but not unrelated default Rust gates; edits
confined to pre-existing independent top-level `tests/<name>.rs` targets run the
Rust gates. Required contexts report not-applicable commands rather than claiming
to build. Governance, workflow, scripts, dependency manifests, mixed or unknown
changes run all gates. Merge groups always run the full current validation; the
narrowed `tests` scope is pull_request feedback only, and the runner refuses it on
any other event.

Fast PR feedback is not evidence for skipping integration validation.
`scripts/require-full-gates` refuses release artifacts, candidate staging and
promotion from any source SHA that commits `.github/reduced-gates`; an
operator-approved reduced-gate window may add that marker (`docs/DEV-CYCLE.md`,
"Emergency clean-slate transition"). Never remove a marker as a CI-speed
optimisation.

See [the development loop](DEV-CYCLE.md) for the local check recipe and the
operator handoff, and [CI wall-time](guides/ci-performance.md) for diagnosing slow
jobs and the rust-cache/sccache split. Shard-era benchmarks and design notes are in
[design/ci-history-and-benchmarks.md](design/ci-history-and-benchmarks.md).

Consolidating the required build and UI checks, or changing branch protection,
needs baseline measurements and an operator decision. Do not rename a check in
YAML before the required-check policy is migrated.

## Stage a selected batch

Select the successful main CI run that holds the desired artifact and dispatch:

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

`cadence update --check` and `cadence update` are the operator path;
`cadence upgrade` is recovery only. `cadence update` with no `--to` (and
`cadence upgrade --latest-main`) select that approved artifact.
`cadence update --to <sha>` pins a commit and runs the `upgrade --sha`
checks, not the production-candidate receipt. The
resolver pins its CI run, attempt and digest, and does not fall back to
an unapproved green build. A rerun that changes the artifact requires
new staging and approval. Existing attestation, ancestry, backward-move,
operator, backup, drain, health and rollback checks remain in force.

## GitHub environment setup

Before enabling production promotion, configure `staging` to admit main
only, and `production` to admit main only with the existing release
operator as a required reviewer. Disable admin bypass for production.
The workflow uses read-only repository/Actions permissions and holds no
production deployment credentials. The rollout owner remains the sole
installer. A separate staging branch is unnecessary.

## Shared sccache on Cloudflare R2

Rust jobs (`build`, `ui`) can share compiled artifacts through an sccache backend
on the R2 bucket `cadence-ci-sccache` (30-day object expiry). GitHub caches are
scoped per branch, so merge-queue refs could not read each other's rust-cache; R2
is not. rust-cache stays: it caches the registry, git dependencies and the
compiled dependency artifacts in `target/`. Expect a modest gain: the `cadence`
crate depends on the changed library, so it always misses on a PR. The win is
dependency crates when rust-cache misses, mainly `merge_group` runs. `clippy` is
left out: its calls go through clippy-driver and are all non-cacheable.

The feature is a strict no-op without credentials. With no secrets (fork PRs, or
before setup) `scripts/ci-sccache enable` exits 0 without touching the job,
`RUSTC_WRAPPER` stays unset and the job compiles exactly as before. Any failure
after credentials resolve (download, checksum mismatch, unreachable bucket, bad
token) also fails open to the same state. `release-artifact` (the attested build)
never uses the cache.

The [CI cache warming workflow](../.github/workflows/ci-cache-warm.yml) runs
independently on the same main push as `ci.yml`. Automatic staging still waits
for a completed, successful `ci.yml` run and verifies the exact release
artifact. Cache completion or failure is no longer part of that wait. In
seven of eight sampled main runs, warming finished 7–88s after attestation
(for example, [run 44444184776](https://github.com/favcrm/cadence/actions/runs/44444184776)).
That is the historical wait, not a post-change measurement.

Who may write: **only the `cache-warm` job, on `push` to `refs/heads/main`, holds
the read-write key.** A queued PR's build scripts and tests run in the
merge_group gate jobs, so a read-write key there would let that code poison the
cache or exfiltrate the key. Therefore:

- `cache-warm` checks for a main push, uses the static `sccache-writer`
  environment, and warms the same profiles with the same cache keys. A hit
  skips work unless the `release` leg has shared sccache enabled.
- `cache-warm-cross` is the same pattern for the cross-build gates: `push` to
  `refs/heads/main` only, one leg per cross target, writing only the
  `cross-dual2-<target>` rust-cache keys the cross-build matrix restores with
  `save-if: false`. It holds no secrets and no environment.
- `test-seed-pilot` (CAD-2749) is manual only. Dispatch CI cache warming on
  `main` with `expected_sha` set to the exact current main commit. The
  job skips other refs and rejects a mismatched SHA before checkout. On an
  exact cache hit it skips the build; on a miss it compiles the test-seam
  library and suite without running tests, then saves only dependencies
  to `gate-test-ld2` on success. It has no sccache credentials or environment.
  `changed-tests` still restores read-only, and no main push runs the pilot.
- Every other job, including all gate jobs on `pull_request` and `merge_group`
  and on main pushes, gets only the read-only pair and no `environment:`.

How it works:

- sccache 0.18.0 is pinned by version and archive SHA-256 (`.config/sccache.sha256`).
  It installs into `$CARGO_HOME/bin`, which the Landlock worker confinement test
  already grants.
- Read-only is enforced twice: a read-only bucket-scoped token, and
  `SCCACHE_S3_RW_MODE=READ_ONLY`. `SCCACHE_S3_NO_CREDENTIALS` means anonymous public
  access, not read-only, so it is not used. In read-only mode sccache's stats still
  count every miss as a "cache write error"; that is the local refusal to write,
  not a failed upload.
- Settings: `SCCACHE_BUCKET=cadence-ci-sccache`, `SCCACHE_REGION=auto`,
  `SCCACHE_ENDPOINT` from the repo variable `SCCACHE_R2_ENDPOINT`,
  `SCCACHE_S3_KEY_PREFIX=v1/rustc-<version>`, `CARGO_INCREMENTAL=0`. Each Rust job
  prints `sccache --show-stats` (and to the step summary), plus the tail of
  `SCCACHE_ERROR_LOG` when it is non-empty.
- `enable` runs before the first compiling step of every job.
- Credentials are exported to later steps of the job as `AWS_ACCESS_KEY_ID` and
  `AWS_SECRET_ACCESS_KEY` through `GITHUB_ENV`, so test processes inherit those
  names, and cadence treats `AWS_*` as provider credentials. Remember it when
  debugging an env-leak test.
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

`cache-warm` is the only job that references the environment. Its workflow
does not run for pull requests, merge groups, tags or manual dispatch.


## Mutation experiments

Dispatch a deliberate guard removal separately from ordinary CI:

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


## Local pre-push recipe

`scripts/pre-push` is the one command to run before pushing. It stops at
the first failing step, judges every step by its exit code alone and prints
one line per step (`[ OK ]`, `[FAIL] name: exit N`, `[SKIP] name: why`):

1. `cargo fmt --all -- --check` (always);
2. `scripts/split-doctor-host --check` (live source inventory);
3. `cargo clippy --all-targets --locked -- -D warnings`, then the same with
   `--features test-seam`, when Rust or Cargo files changed;
4. `bun run --cwd ui typecheck` when `ui/` changed;
5. the active Python test commands that CI's `fmt` job runs, when `scripts/`,
   `.github/` or `tests/scripts/` changed;
6. with `--tests`, `scripts/run-result-tests` (every test target, then the
   lib and binary-target unit tests).

"Changed" is the union of commits since `origin/main`, working-tree edits and
untracked files; use `--base REF` to override and `--list` to print the plan
without running it. It sets `CARGO_BUILD_JOBS=4` unless already set and does not
run the full suite. See [SPLIT-MANIFESTS.md](SPLIT-MANIFESTS.md) for the doctor
source guard.

## Rust toolchain pin

`rust-toolchain.toml` pins an exact Rust release (`channel = "x.y.z"`, never
`stable`). It is the only place the version is written. Every workflow
installs Rust through `scripts/ci-rust-toolchain`, which reads the channel
from that file and runs `rustup toolchain install <channel>` with the
profile and components the job passes. `--export` also sets
`RUSTUP_TOOLCHAIN` for jobs that build another checkout (staging's
`candidate-source`). Local `cargo` follows the file too, so a local
`cargo clippy` matches CI.

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


## PR-gate `ci` profile and fast linker

The `build` and `ui` jobs compile with `cargo build --profile ci` (inherits
`release`; opt-level 1, 256 codegen units, no debuginfo, no incremental, no
LTO) and link with mold, or lld, installed by `scripts/ci-fast-linker enable`
(it falls back to the default linker when neither works). These jobs prove the
tree compiles, links, carries no test seam and passes the migration rehearsal;
none of that depends on optimisation. `release-artifact`, `release-build`,
staging and promotion keep the full `--release` profile, `require-full-gates`
and the `cargo check --release --features test-seam` refusal probe. The
`cache-warm` release leg warms the same `ci` profile with the same linker flag
(the flag is part of cargo's fingerprint). Not merged: `build` and `ui` stay two
compiles (default features vs `--features ui`, different path scopes), and the
two clippy passes stay (default vs `test-seam` lint different cfg code).

## Build once per tree

A queue entry's `merge_group` run builds the shipping binary
(`--release --features ui`) in `release-queue-build`, in parallel with the
gates, and uploads `release-tree-<tree hash>` (binary, sha256, manifest with
the tree). It is not a required check. On the main push for that sha,
`release-reuse` downloads it from the merge_group run that `queue-evidence`
found and `scripts/release-reuse.py` verifies, for this checkout's own tree:
the artifact name and manifest tree, the source commit, the binary digest,
the build shape (release, x86_64-linux, `ui`) and the commit baked into the
binary. Any doubt (no artifact, build unfinished after 15 minutes, any
mismatch) leaves `reused` unset and `release-artifact` rebuilds. The binary
is never executed during reuse. `release-attest` then attests the verified
bytes on main exactly as before (source ref `refs/heads/main`, digest
= the main sha), so `cadence update`, staging and promotion verify the same
attestation. The commit is baked into the binary (`build.rs`), so a tree
reached by a different commit is rebuilt rather than misreporting its
revision. `staging.yml` already downloads the attested artifact of the
selected main CI run (`delivery-candidate.py prepare`: run, SHA, attempt,
digest, attestation) and never rebuilds it; only its PR-only `rehearsal-check`
compiles PR bytes, and those are never promoted. The `release-full` leg of
`cache-warm` warms the release profile that `release-queue-build` restores.
Residual: the artifact crosses jobs of the queue run, which also
runs the entry's tests; the upload never overwrites, but the main attestation
vouches for bytes the queue run built.

## Live staging candidate

The staging deploy candidate polls the **latest successful** `ci.yml` push run
on main every five minutes, verifies its attested release with
`scripts/delivery-candidate.py prepare`, then starts the verified binary in the
`staging` sandbox. It coalesces intermediate green builds; the timer interval
is a polling cadence, not an end-to-end SLA. `--run-id` pins a run for an
operator-directed deploy or recovery. The loopback health and `/api/meta`
`build_commit` must match the selected source SHA. Failed deploys retain and
record the proven fallback; candidate and fallback executions are re-hashed
against their receipts. The first successful start seeds a separate fixture
tracker and registers two inbox agents once. The timer uses a dedicated clone
checked out at `origin/main`; its state and release cache are under
`~/.local/share/cadence-staging`.

The timer is not installed or enabled by source changes; installing the unit or
the timer, a runtime deploy and a state-directory migration each need their own
operator authorization. The board listens on loopback port 3150 (inside the
permitted 3110-3199 range) with tailnet HTTPS on `:9460` (`PORT` and `TS_PORT` in
`scripts/staging-deploy.py`). Tailnet publishing is through `cadence ui tailscale`
under the sandbox opt-in, not through sudo or nginx. The deploy code revalidates
an absent, foreign or live mapping on ordinary and known-bad healthy ticks,
refuses to overwrite foreign routes, and keeps a healthy loopback board available
if a tailnet publish restarts it. Tailnet reachability is observed separately
from loopback health.
