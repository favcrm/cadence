---
goal: Add a pinned nextest runner and fail-closed suite-lock boundary
version: '1.0'
date_created: 2026-09-20
last_updated: 2026-09-20
owner: luna-watchdog
status: 'In progress'
tags: [feature, testing, nextest, locks]
---

# Introduction

![Status: In progress](https://img.shields.io/badge/status-In%20progress-yellow)

This plan adds a reviewable, opt-in nextest runner for cadence's integration suite. The runner pins the measured nextest version and reviewed executable digest, hard-codes zero retries even when nextest retry environment variables are present, compares a complete cargo/nextest test inventory, and takes the canonical host suite lock outside the process-per-test harness. The existing review configuration remains on cargo until the human-class runner, supply-chain, structured-result, and lock approvals recorded in CAD-173 are granted.

## 1. Requirements & Constraints

- **REQ-001**: Provide one checked-in runner that verifies `cargo-nextest` is exactly version `0.9.145` and matches the checked-in trusted executable SHA-256 before executing a test command.
- **REQ-002**: Force the nextest profile to `retries = 0` and reject caller arguments that attempt to raise or replace retries.
- **REQ-007**: Clear `NEXTEST_RETRIES`/`NEXTEST_PROFILE` and pass CLI `--retries 0` on every run path so a caller environment cannot override the reviewed profile.
- **REQ-003**: Hold `CADENCE_SUITE_LOCK` with an outer `flock` for direct nextest invocation; when `cadence review` already holds that lock, run the child without nested flock and with an explicit held marker.
- **REQ-004**: Make the integration harness refuse direct nextest execution when the outer lock contract is absent, while preserving ordinary cargo filtered tests and the review child's explicit empty-path contract.
- **REQ-005**: Provide a non-empty, deterministic inventory check proving cargo and nextest expose the same complete integration test set; preserve unit, binary, board, and integration coverage in the review gates.
- **REQ-006**: Record cold versus warm runner timings as evidence when the operator/Ops lane is admitted; historical CAD-173 measurements must remain labelled historical rather than current proof.
- **SEC-001**: Keep nextest installation, pin activation, gate configuration, CI changes, and timeout changes behind the human approvals named by CAD-173; this PR must not install a binary or activate the runner.
- **SEC-002**: Do not let a process-per-test filtered child silently bypass the host-wide suite lock or deadlock by flocking a lock already held by `src/review.rs`.
- **SEC-003**: Do not trust an arbitrary `CADENCE_NEXTTEST_BIN` path because it prints the pinned version; require the exact digest from the checked-in artifact manifest.
- **CON-001**: Base the isolated branch on `origin/main` and do not touch active PR82/PR87 worktrees or borrow their commits.
- **CON-002**: Do not delete or rewrite the test pyramid, alter test bodies, run a release build, or run the full integration suite in the developer lane.
- **CON-003**: Use one focused Cargo command at a time with `CARGO_BUILD_JOBS=4`; Ops owns any current-head full-suite timing under the host slot.
- **GUD-001**: Keep `cadence-review.toml` on cargo and make the nextest wrapper available for independent review; activation is a later, explicit configuration decision.
- **PAT-001**: Preserve existing `cadence review` outer `Flock` ownership and the child `CADENCE_SUITE_LOCK` blanking contract.

## 2. Implementation Steps

### Implementation Phase 1

- GOAL-001: Add the pinned, zero-retry nextest command and complete inventory evidence without activating the review gate.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-001 | Add `.config/nextest.toml` with the named cadence profile and `retries = 0`; add the trusted artifact manifest and `scripts/cadence-nextest` to verify the pinned binary digest/version, clear retry/profile environment overrides, reject retry arguments, acquire the outer lock for direct calls, and bypass only with the review-owned held marker. | yes | 2026-09-21 |
| TASK-002 | Add `scripts/nextest-inventory` that refuses empty manifests and compares sorted cargo integration names with nextest names; document that this check is fixture/evidence tooling until the pinned binary is installed by approval. | yes | 2026-09-20 |
| TASK-003 | Keep `cadence-review.toml` unchanged for activation and document the candidate commands, pin, approval boundary, and cold/warm timing protocol. | yes | 2026-09-20 |

### Implementation Phase 2

- GOAL-002: Make lock ownership fail closed across cargo, nextest, and cadence review.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-004 | Update `tests/integration.rs` nextest detection so any nextest process without an explicit review-held empty-path marker refuses before tests run; ordinary cargo filtered runs retain their current behavior. | yes | 2026-09-20 |
| TASK-005 | Update `src/review.rs` to export the outer-lock-held marker to the child and report lock ownership as outer/review-held, without adding a nested `flock` to `full_suite`. | yes | 2026-09-20 |

### Implementation Phase 3

- GOAL-003: Verify the runner contract with focused tests and a reviewable evidence pack.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-006 | Add focused unit/integration assertions for pinned digest/version rejection, retries-zero enforcement including `NEXTEST_RETRIES`, direct-lock refusal, review-held no-nested-lock behavior, and complete inventory non-empty guards. | yes | 2026-09-21 |
| TASK-007 | Run formatting, focused review/lock tests, and current-head inventory/build checks in the Ops-admitted lane; report cold/warm full-suite timing only if the pinned tool and host slot are actually available. | | |
| TASK-008 | Publish the exact branch head and evidence to an independent reviewer; request Ops gate review after independent pass, with no activation or merge performed by this lane. | | |

## Acceptance

- **AC-001**: A direct runner call executes only when the installed executable matches the checked-in SHA-256 and reports `cargo-nextest 0.9.145`, the checked-in cadence profile is selected, caller retry/profile/config overrides are rejected, and `NEXTEST_RETRIES` cannot raise retries above zero.
- **AC-002**: A direct runner owns `CADENCE_SUITE_LOCK` in one outer `flock`; a review-owned child runs with an empty child path and explicit held marker, while `--no-suite-lock` does not emit that marker and cannot activate an unowned nextest suite.
- **AC-003**: The integration harness rejects nextest without the external lock contract, retains ordinary cargo filtered behavior, and the inventory command reports success only when both non-empty manifests are exactly equal.
- **AC-004**: The current review gate remains cargo-configured, no nextest binary is installed, no production or CI gate is activated, and all timing claims identify whether they are historical, fixture, or Ops-admitted current-head evidence.
- **AC-005**: Focused syntax, wrapper-contract, formatting, and Rust guard checks pass on the exact branch head; any unavailable pinned binary or host suite slot is reported as a dependency rather than inferred healthy.

## 3. Alternatives

- **ALT-001**: Put `flock` directly in `cadence-review.toml`'s `full_suite`. Rejected because `src/review.rs` already owns the outer lock and blanks the child path; a nested flock would deadlock or lock the wrong file.
- **ALT-002**: Leave `tests/integration.rs`'s filtered-run bypass unchanged. Rejected because nextest runs every test in a filtered child and would silently run beside another host suite.
- **ALT-003**: Switch `cadence-review.toml` to nextest in this PR. Rejected because CAD-173 requires reviewed human-class approvals for installation, gate activation, structured result handling, and the trust-boundary lock change.
- **ALT-004**: Delete or rewrite integration tests to meet the historical test-count target. Rejected because the measurement says process isolation supplies the speed-up and pane/tmux/git coverage must remain.

## 4. Dependencies

- **DEP-001**: Human approval for the pinned cargo-nextest installation and supply-chain checksum.
- **DEP-002**: Independent review of structured result/failure attribution before any full-suite runner activation.
- **DEP-003**: Ops admission to the one host suite slot for current-head cold/warm timing; active PR82/PR87 gates remain undisturbed.
- **DEP-004**: A current cargo/nextest inventory run with both manifests non-empty and exactly equal.

## 5. Files

- **FILE-001**: `.config/nextest.toml` — named zero-retry profile.
- **FILE-009**: `.config/cargo-nextest.sha256` — reviewed extracted executable digest and source archive digest.
- **FILE-002**: `scripts/cadence-nextest` — pinned binary check and canonical outer-lock wrapper.
- **FILE-003**: `scripts/nextest-inventory` — complete non-empty cargo/nextest name comparison.
- **FILE-004**: `tests/integration.rs` — fail-closed nextest lock detection and focused assertions.
- **FILE-005**: `src/review.rs` — review-owned lock marker and report evidence.
- **FILE-006**: `docs/SESSION.md` and `docs/TEAM.md` — activation boundary and timing protocol.
- **FILE-007**: `design-plans/feature-pinned-nextest-runner-1.md` — executable plan and acceptance mapping.
- **FILE-008**: `scripts/test-cadence-nextest` — local fake-runner contract and inventory fixtures.

## 6. Testing

- **TEST-001**: `scripts/test-cadence-nextest` plus shell syntax checks prove the checked-in executable digest is required; a missing, wrong, or version-only binary refuses.
- **TEST-002**: The same local fixture proves retries cannot be supplied by caller, `NEXTEST_RETRIES` is neutralized with CLI `--retries 0`, and direct invocation requires `CADENCE_SUITE_LOCK`.
- **TEST-003**: The same local fixture proves `CADENCE_REVIEW_SUITE_LOCK_HELD=1` with an empty child lock path runs without a second flock.
- **TEST-004**: Focused Rust test proves nextest detection rejects filtered execution without the review-held marker and ordinary cargo filtering remains allowed.
- **TEST-005**: Inventory command proves both manifests are non-empty and equal before reporting success; missing nextest reports an honest dependency failure.
- **TEST-006**: Current-head `cargo test --test integration -- --list`, `cargo test --lib --bins --test board --no-run`, and equivalent inventory/build checks preserve unit/bin/board/integration coverage without a full-suite run in this lane.
- **TEST-007**: Ops-admitted timing evidence, when available, records separate cold and warm runs, host slot ownership, exact head, binary version, retries, and test count; no historical measurement is relabelled as current.

## 7. Risks & Assumptions

- **RISK-001**: The pinned binary is absent from the normal toolchain/PATH; a checksum-verified copy may exist only in the task-scoped `/tmp` validation directory, and no nextest pass or timing claim may be fabricated from its presence alone.
- **RISK-002**: Nextest's machine-readable failure output is not adopted in this bounded slice; activation remains blocked until the structured-result parser protects equal-conditions failure attribution and zero-test fail-closed behavior.
- **RISK-003**: A direct caller can lie about the review-held marker under the same-user trust model; the wrapper and harness require the marker plus an empty lock path and keep authority outside the runner.
- **ASSUMPTION-001**: `src/review.rs` remains the sole owner of the outer suite flock for `cadence review`.
- **ASSUMPTION-002**: The current integration test inventory is the coverage contract and no test is filtered, partitioned, quarantined, or deleted by this PR.

## 8. Related Specifications / Further Reading

- `CAD-173` tracker artifacts `cad-173-suite-measurement.md` and `cad-173-nextest-adoption-spec.md`
- `cadence-review.toml`
- `docs/SESSION.md`
- `docs/TEAM.md`
- `tests/integration.rs` suite-slot guard
- `src/review.rs` outer review lock
