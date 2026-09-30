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

## Compile-once Rust test archives (CAD-858)

The `test-build` job selects the scope using the PR base policy, proves the
existing Cargo/nextest inventory parity once, and creates a pinned nextest
0.9.145 archive. A trusted base runner without `ARCHIVE_PROTOCOL = 1` gets
an explicitly recorded full-scope archive fallback, not eight independent
compiles. Actual documentation edits still receive the full scope; existing
explicit `docs` plans retain verified empty markers and receipts.

Eight `test-shard` consumers download the exact immutable artifact ID from
successful producer job outputs. Source SHA, run, original producer attempt,
archive/plan/inventory/manifest digests, compiler/tool/config identity, flags,
container/runtime identity and absolute workspace/target layout must match
independently collected consumer context. The artifact cannot supply its own expected
context. Failed-job reruns retain the original successful producer ID and
attempt; missing/expired artifacts or incompatible contexts fail explicitly.
Rerun the producer and its dependent consumers when new producer bytes are
needed, rather than guessing a latest artifact or overwriting its name.

Producer and shards use the identical source-pinned official Rust 1.98.1
Trixie Linux/amd64 image manifest, with signed Debian snapshot repositories
for Python/zstd/procps/jq bootstrap. `.config/ci-test-runtime.env` records the
image, snapshot, distro and compiler; refreshed pins require review and fresh
runtime acceptance. Both jobs independently measure os-release, installed
package versions and x86_64 ELF bytes for libc/libstdc++/the loader. Missing,
malformed, changed or unproven runtime identity fails closed. Normal shell
steps run as non-root uid 1001; only fresh-container bootstrap and producer
cache and CI-directory ownership restoration use root. The latter validates
the observed container paths `/github/home`, `/usr/local/cargo` and
`/__w/_temp` before changing ownership after root Actions. Both containers
use Docker `--init`; preflight exercises the installed Git
`merge-tree --merge-base` capability and checks that a known exited orphan
is reaped. Consumers still have no target cache.
Hosted `ImageOS`/`ImageVersion` remain bounded immutable observations in the
sealed producer manifest, not userspace compatibility authority. Run 36682010782
attempts 1/2 proved the hosted label can mix image versions, even on failed-only
reruns; dropping a label check without a pinned and independently measured
runtime is not this contract. Container jobs share the same container workspace
path with each other, not the historical host path. Containers do not freeze
the host kernel; Linux/X64 remains required, kernel details are logged and real
fixture/syscall/permission behavior still needs full runtime CI acceptance.

Consumers require an absent `target/`, never delete or overwrite a restored
cache, and preflight GNU tar.zst members before one archive-backed list extracts
into the identical producer workspace. Preflight refuses non-target paths,
links/devices, duplicates, corrupt decoding, missing metadata and resource
limits: 20,000 members, 32 GiB cumulative logical file sizes, bounded metadata
headers, and bounded decompressed input. These are explicit refusal thresholds,
not sampled coverage; exceeding one requires investigation and reviewed policy
changes. GNU long names and sparse files are supported. `zstd` is required.
The archive-backed list must reproduce both testcase identities and run
eligibility; the restored CLI must report the exact source SHA. Later filtered
lists and runs use extracted metadata only, with no Cargo build selectors,
features or `--locked`. Existing LPT weights, filter equality, eight disjoint
assignment receipts, zero retries and the required `test` aggregate remain.
Producer failures/skips/missing evidence cannot green the aggregate.

`test-once` stays independent for default-feature refusal proofs and doctests.
Each consumer still installs/builds the SPA and needs real `tsc`; Rust fixture
Cargo subprocesses remain real. After archive inventory/source verification,
the consumer performs one `cargo fetch --locked` to populate the registry
needed by fixture calls such as `cargo tree --locked --offline`; docs-only
plans skip it. This fetch does not build the suite. This is not elimination of every Rust build or
sharing of release/UI-feature binaries. No production deployment or approval
policy is changed.

The historical two-partition archive was 1,523,895,478 bytes. Eight transfers
would total about 12.2 GB before wrapper overhead; this is arithmetic, not a
current measurement. The first current bootstrap run uploaded a 2,493,340,836-byte
full bundle (~19.95 GB arithmetic for eight downloads); the pinned container's
compressed image layers total 562,530,953 bytes, before bootstrap/network costs.
Run 36740682630 subsequently uploaded a 2,517,835,247-byte bundle; its
eight downloads took 39–332 seconds each. Archive verification and exact
assignment of all 3,615 tests passed, but seven shards failed actual test
bodies: Bookworm Git lacked `--merge-base`, an offline Cargo call lacked
registry data, and permission/provider lifecycle tests failed. The runtime
correction targets those causes; full CI must verify the ownership and
reaping hypotheses before accepting this revision.
No measured speedup is implied. Producer scheduling, upload/download, preflight/extraction,
UI setup and other required builds can outweigh saved compilation. Require
current-head full/selected archive execution and eight-shard coverage evidence;
contract tests and the historical run are not a measured speedup or current
runtime acceptance. Measure critical-path feedback and runner minutes before
claiming delivery improvement.

## Remaining delivery work

CAD-479 tracks measurement of affected-test selection; the queue retains
the full suite. Timing artifacts from CAD-638 provide evidence for balanced
shards. Neither optimization is a reason to reduce required coverage.
Independent review remains required by AGENTS.md; a required GitHub
review bridge must report both PR-head and merge-group checks before
activating it in branch protection. CAD-120 tracks reviewer identity
integration. Real-provider acceptance remains CAD-434; the fake-provider
MVP journey cannot prove provider compatibility or real fleet continuity.
