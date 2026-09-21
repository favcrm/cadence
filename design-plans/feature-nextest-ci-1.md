---
title: CAD-173 CI nextest all-targets follow-up
status: proposed
issue: CAD-173
---

# Scope

This is a CI-only follow-up based on current `main`. It has an explicit
dependency on draft PR93 (`cadence/cad-173-luna-activation`) for the
task-local checksum-verified installer and the pinned nextest wrapper. The
dependency is carried as a visible merge in the implementation worktree; no
PR93 commit is silently copied and PR93's branch remains frozen for review.

The CI test job replaces the serial `cargo test --all-targets` command with
the same pinned wrapper after a complete Cargo/nextest inventory comparison.
The inventory covers the library, binary, `board`, and `integration` test
targets together. The wrapper supplies retries zero and `--no-tests fail`,
while the job passes `--locked` and owns a temporary `CADENCE_SUITE_LOCK`
before the process-per-test runner starts.

Nextest does not run Rust doctests. The job therefore follows the all-targets
run with `cargo test --doc --locked`; this keeps doctest coverage explicit and
does not count doctests in the four-target parity manifest.

# Workflow safety

The workflow keeps the existing SHA-pinned actions and read-only permissions.
Every job receives a bounded timeout. `cargo clippy`, the nextest run, and
doctests require the checked-in lockfile. Concurrency cancels only an older
`pull_request` run for the same PR number; main pushes and different PRs have
distinct non-canceling groups. No cache, toolchain pin, release behavior, or
review authority is changed in this slice.

# Acceptance

- `scripts/nextest-inventory all-targets` rejects empty Cargo or nextest
  manifests and fails on any name mismatch.
- The task-local installer verifies the reviewed archive and extracted binary
  before the wrapper is invoked; it never writes PATH, Cargo home, the repo,
  or a production state directory.
- A nextest failure or zero-test selection fails the CI job, with retries
  forced to zero.
- Doctest execution remains a separate locked Cargo step.
- The workflow's concurrency expression protects main and distinct PRs while
  canceling obsolete same-PR pull-request runs.

No local full suite, service restart, live state-dir access, merge, or deploy
is part of this implementation. Current GitHub timing must be measured from
the new workflow and not inferred from local or historical nextest runs.
