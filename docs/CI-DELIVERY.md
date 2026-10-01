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

## Remaining delivery work

CAD-479 tracks measurement of affected-test selection; the queue retains
the full suite. Timing artifacts from CAD-638 provide evidence for balanced
shards. Neither optimization is a reason to reduce required coverage.
Independent review remains required by AGENTS.md; a required GitHub
review bridge must report both PR-head and merge-group checks before
activating it in branch protection. CAD-120 tracks reviewer identity
integration. Real-provider acceptance remains CAD-434; the fake-provider
MVP journey cannot prove provider compatibility or real fleet continuity.

## Live staging on the host

A `cadence sandbox` named `staging` runs on this host as the live
staging instance, redeployed from every green `ci.yml` run on main:

- **Loopback:** `http://cadence-3020.localhost:3020` (board on
  `127.0.0.1:3020`, its own daemon, tracker, seed data and two inbox
  agents `staging-a`/`staging-b`).
- **Tailnet:** `https://ip-172-31-1-32.tail9fcf30.ts.net:9460` once the
  operator has published the mapping (below).

`scripts/staging-deploy.py` is one idempotent tick, driven by the
`cadence-staging` systemd user timer every five minutes: pick the newest
successful ci.yml push run on main (`--run-id N` pins a manual deploy or
rollback), verify the artifact through `delivery-candidate.py prepare`
— attestation, manifest and digest, so a binary that never passed
prepare is never executed — then `sandbox down`/`sandbox up` the verified
release on port 3020. Health is checked on loopback with the board's own
Host header and `/api/meta`'s `build_commit` must equal the candidate
SHA; a failed check rolls back to the previous release. `status.json`,
`deploy.log`, the deploy flock and the last five verified releases live
under `~/.local/share/cadence-staging/`; children run with
`CADENCE_SANDBOX_ROOT` pointed there so the staging sandbox never
collides with agents' sandboxes. `deploy.lock` makes overlapping ticks a
clean no-op, and the timer's `ExecStartPre` detaches its dedicated clone
(`%h/.local/share/cadence-staging/src`) onto `origin/main` first — the
deploy logic is always main's reviewed code, never a lane's.

Flow: green main build → staging live on the next timer tick → human
review on the board → the existing `staging.yml` approval → rollout.
The tick polls *latest* green main, not every green build: commits that
land faster than the five-minute interval coalesce into one deploy of
the newest run, intermediate builds are never installed, and the
interval is a poll cadence, not an end-to-end SLA (a slow CI run, a
held `deploy.lock` or a failed health check defers the deploy).

**Tailnet publish.** The board's live updates are Server-Sent Events
(`ui/src/lib/sse.ts`, served by `src/ui/threads.rs`), which
`tailscale serve` streams — no WebSocket is involved. Publishing is the
operator's one-time act, not the timer's:

```bash
sudo tailscale serve --bg --https=9460 http://127.0.0.1:3020
```

The sandbox may only touch the tailnet with the explicit opt-in
`CADENCE_SANDBOX_ALLOW_GLOBAL=1` (the deploy sets it); without it every
route onto the tailnet is refused, and a serve port that already targets
production's board is never overwritten. No nginx or other relay: the
board refuses operator sessions arriving through relays by design (the
peer check in `src/ui/operator.rs`), so the tailscale HTTPS proxy — for
which the board has a dedicated proof chain — is the only remote path.

**One-time operator setup:**

```bash
# dedicated clean clone the timer checks out onto origin/main
git clone https://github.com/favcrm/cadence.git \
  ~/.local/share/cadence-staging/src
# install and enable the user units
install -Dm644 scripts/staging/cadence-staging.{service,timer} \
  -t ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now cadence-staging.timer
# publish the tailnet mapping (above)
```
