---
title: CAD-173 structured nextest activation follow-up
status: proposed
---

# Scope

This follow-up is stacked on the reviewed pinned-runner boundary from PR92.
It makes the review path consume a structured JUnit report and moves the full
suite and isolated test command together to the same checksum-verified
nextest backend. The configuration change remains human-class and must stay
behind independent review and operator approval; this branch does not run a
live review, restart a daemon, or install globally.

# Requirements

- **ACT-001**: full_suite and test_command use the same nextest wrapper; the
  isolated command passes the test name after -- with --exact.
- **ACT-002**: nextest writes JUnit to the profile-resolved
  target/nextest/cadence/junit.xml; the review removes the old file before
  every run and records per-test names, outcomes, counts, and durations.
- **ACT-003**: a missing, malformed, or zero-test JUnit report is invalid;
  isolated results become unknown and a successful full-suite process with
  invalid evidence becomes a blocking failure.
- **ACT-004**: JUnit failure testcase names feed the existing equal-conditions
  gated-tree/base-tree comparison and unchanged flake ledger.
- **ACT-005**: retries remain zero in the wrapper and profile; callers cannot
  override the profile, retry count, or zero-test policy.
- **ACT-006**: the pinned installer verifies the reviewed archive and
  extracted-binary checksums and writes only to an absolute task-local
  directory (default /tmp/cadence-nextest-0.9.145).

# Acceptance

- cargo fmt --all -- --check.
- Focused review unit tests cover JUnit failure/count/duration parsing,
  missing versus zero-test evidence, nextest config backend matching, and
  exact isolated command requirements.
- scripts/test-cadence-nextest passes checksum, retries, lock, and zero-test
  override checks.
- scripts/test-cadence-nextest-activation passes on a temporary crate and
  proves the generated JUnit file is at the expected path, names a failure,
  and refuses a zero-test filter.
- Current-head benchmark evidence is recorded separately from this PR. It
  must name the exact main SHA, inventory count/equality, tool version,
  lock ownership, and whether build/test caches were warm or cold.

# Safety boundary

The activation config is a reviewed artifact, not permission to merge or
change a running gate. CI configuration, timeout reductions, release builds,
production state directories, and live restarts remain outside this slice.
