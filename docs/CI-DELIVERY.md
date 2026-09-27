# Integration and production candidates

PRs continue to target main. The merge queue keeps the full fmt, clippy,
test, build and UI gates. Main CI reuses exact-SHA queue evidence and
builds an attested artifact. A successful main build is available for
staging; it is not automatically a production candidate.

## Stage a selected batch

After this workflow lands on main, select the successful main CI run
that holds the desired artifact and dispatch:

```sh
gh workflow run staging.yml -R favcrm/cadence --ref main -f ci_run_id=<main-ci-run-id>
```

The staging job verifies the run's workflow, repository, main ancestry,
SHA, attempt, manifest, digest and build provenance. It downloads the
embedded-UI release binary and runs the existing MVP journey against
those exact bytes, with the candidate revision's fixtures. Providers
and GitHub are faked; the daemon, board and browser are real and isolated.
It uses a temporary HOME/state/tracker and board port 3186 on a hosted
runner. No production state, credentials or daemon are accessed.

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

## Remaining delivery work

CAD-479 owns affected-test selection for PRs; the queue retains the full
suite. Timing artifacts from CAD-638 provide evidence for balanced
shards. Neither optimization is a reason to reduce required coverage.
Independent review remains required by AGENTS.md; a required GitHub
review bridge must report both PR-head and merge-group checks before
activating it in branch protection. CAD-120 tracks reviewer identity
integration. Real-provider acceptance remains CAD-434; the fake-provider
MVP journey cannot prove provider compatibility or real fleet continuity.
