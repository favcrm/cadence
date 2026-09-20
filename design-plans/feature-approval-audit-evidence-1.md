---
goal: Add durable, provenance-bounded approval evidence to cadence audit
version: '1.0'
date_created: 2026-09-20
last_updated: 2026-09-20
owner: luna-approval
status: 'In progress'
tags: [feature, audit, approval, provenance]
---

# Introduction

![Status: In progress](https://img.shields.io/badge/status-In%20progress-yellow)

This plan adds a small evidence-only record to the existing daemon event log. An explicit operator record is bound to an action, a full landed head SHA, and a scope; an explicit revocation is a separate event. The audit consumes those events read-only and reports approved, revoked, missing, or unknown evidence. Message delivery state, worker output, and an arbitrary daemon message source never grant or revoke authorization.

## 1. Requirements & Constraints

- **REQ-001**: Persist an explicit approval record containing a stable record id, source, action, full 40-character hexadecimal head SHA, scope, and event timestamp.
- **REQ-002**: Persist explicit revocation as a distinct event referencing the approval record; message cancellation must not create or imply revocation.
- **REQ-003**: Make duplicate identical writes idempotent and reject a conflicting reuse of an approval id.
- **REQ-004**: Make audit output bind merge evidence by exact full landed head and action `merge`, preserving the recorded source, scope, id, and timestamps.
- **REQ-005**: Report evidence state as `approved`, `revoked`, `missing`, or `unknown`; do not infer approval from a queued/cancelled message, a worker result, or `source=user`.
- **SEC-001**: Expose recording only as an explicit operator evidence action; reject calls made from a cadence worker pane and reject ambiguous sources `user` and `daemon`.
- **SEC-002**: Keep evidence recording outside dispatch, merge, task acceptance, and permission policy; the new records are audit facts and never grant execution authority.
- **CON-001**: Use the existing `events` table and daemon stream; do not add a schema migration, live migration, restart, deploy, or production activation in this slice.
- **CON-002**: Work from `origin/main` in the isolated CAD-217 worktree and run one focused Cargo test lane with `CARGO_BUILD_JOBS=4`; do not run the full suite or release builds.
- **GUD-001**: Follow the existing audit convention that unavailable sources produce an explained unknown and do not produce an accusation flag.
- **GUD-002**: Keep the JSON schema additive and preserve existing audit fields and exit behavior for non-human rows.
- **PAT-001**: Use the daemon event transaction/idempotency patterns already used by `Store::event_scoped` and durable message operations.

## 2. Implementation Steps

### Implementation Phase 1

- GOAL-001: Define and persist evidence-only approval and revocation events without changing authority or dispatch behavior.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-001 | Add validated `Store::record_approval` and `Store::revoke_approval` methods in `src/store.rs`; write `approval_recorded` and `approval_revoked` events to `Store::DAEMON_STREAM`, require explicit non-ambiguous sources, require a full SHA, dedupe identical ids, and reject conflicting or orphan revocations. | | |
| TASK-002 | Add operator-only `cadence approval record` and `cadence approval revoke` CLI/RPC plumbing in `src/main.rs` and `src/daemon.rs`; pass no inferred source, reject `CADENCE_ALIAS` callers, and return the durable event result without invoking dispatch or merge code. | | |
| TASK-003 | Add `docs/AUDIT.md` documentation for event fields, trust boundaries, cancellation/revocation separation, and truthful unknown behavior. | | |

### Implementation Phase 2

- GOAL-002: Consume explicit evidence in the read-only audit and expose reviewable status.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-004 | Extend `src/audit.rs` store evidence loading to parse only the two explicit event kinds, reject malformed/untrusted payloads as unknown, bind `merge` records to the exact full landed head, and evaluate revocation independently of message state and worker output. | | |
| TASK-005 | Add additive `approval` JSON/text output and human-row flags for missing/revoked evidence while leaving unavailable evidence unflagged; retain source/action/head/scope/id and timing details. | | |

### Implementation Phase 3

- GOAL-003: Verify lifecycle, provenance, deduplication, and audit truthfulness with focused tests.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-006 | Add focused store and CLI tests covering full-head validation, source boundary, duplicate/conflicting ids, explicit revoke, orphan revoke refusal, pane refusal, and cancellation independence. | | |
| TASK-007 | Add focused audit tests covering approved, revoked, missing, unavailable/unknown, post-hoc, non-merge action, exact-head mismatch, malformed `source=user`, and additive JSON/text rendering. | | |
| TASK-008 | Run `CARGO_BUILD_JOBS=4 cargo test --lib audit approval` or the smallest equivalent focused filters in one lane, inspect the exact diff/head, and publish an independent review request with the resulting commit SHA. | | |

## 3. Alternatives

- **ALT-001**: Add an `approval_records` SQLite table and schema migration. Rejected for this slice because the existing daemon event log already provides durable, ordered storage and the task forbids live migration/activation.
- **ALT-002**: Treat queued messages, their `source` field, or cancellation result as approval evidence. Rejected because delivery provenance is not human authorization and cancellation is not revocation.
- **ALT-003**: Make dispatch or merge consume the new records immediately. Rejected because this increment is additive audit evidence and must not redesign existing authority or approval policy.

## 4. Dependencies

- **DEP-001**: The existing `events` table and `Store::DAEMON_STREAM` from schema v1 must remain readable on all supported stores.
- **DEP-002**: The audit's existing full-head merge and evidence-gap handling must remain the binding and unknown-reporting model.
- **DEP-003**: Focused Cargo tests require the shared build admission coordinated with Ops and `CARGO_BUILD_JOBS=4`.

## 5. Files

- **FILE-001**: `src/store.rs` — validated idempotent durable approval/revocation event writers.
- **FILE-002**: `src/daemon.rs` — operator-only RPC methods with no dispatch/merge authority.
- **FILE-003**: `src/main.rs` — explicit `approval record`/`approval revoke` commands and client routing.
- **FILE-004**: `src/audit.rs` — read-only event parsing, exact-head binding, state rendering, and flags.
- **FILE-005**: `docs/AUDIT.md` — evidence schema and trust-boundary documentation.
- **FILE-006**: `tests/integration.rs` — focused lifecycle and audit regression coverage.

## 6. Testing

- **TEST-001**: Store-level test proves a valid record persists, an identical retry dedupes, and conflicting reuse is rejected.
- **TEST-002**: Store/daemon test proves `source=user`, `source=daemon`, worker-pane callers, orphan revocations, and non-full heads cannot create approval evidence.
- **TEST-003**: Lifecycle test proves message cancellation leaves approval state unchanged and only `approval_revoked` changes it.
- **TEST-004**: Audit fixture test proves exact full-head merge records render source/action/scope and `approved`; a short or different head is not bound.
- **TEST-005**: Audit fixture test proves explicit revocation renders `revoked` and flags only the human row; missing evidence is actionable, unavailable evidence is `unknown` and unflagged.
- **TEST-006**: Audit fixture test proves post-hoc or malformed/untrusted records do not become merge-time approval and worker output is ignored.
- **TEST-007**: CLI parser/RPC tests prove the evidence command is explicit, source is not inferred, and existing dispatch/merge paths are unchanged.

## 7. Risks & Assumptions

- **RISK-001**: Same-user local callers can claim an operator source; audit will expose the claimed source/provenance and will not present queue or worker metadata as proof. Strong human identity attestation is outside this bounded slice.
- **RISK-002**: Existing event retention/pruning can remove old evidence; a missing readable record is reported as `missing`, while an unreadable or unavailable store is `unknown` with a reason.
- **RISK-003**: Existing human merges without a record will acquire an audit flag once the audit can read a healthy store; this is reporting only and does not block dispatch or merge.
- **ASSUMPTION-001**: The daemon event stream remains the durable source for this first additive slice and is not treated as a policy/grant ledger.
- **ASSUMPTION-002**: A merge approval is identified by action `merge` plus the exact full landed head; scope is retained and displayed rather than inferred or widened.

## 8. Related Specifications / Further Reading

- `docs/AUDIT.md`
- `docs/SESSION.md`
- `docs/PROTOCOL.md`
- `docs/adr/0001-role-profiles.md`
- `CAD-217` and `CAD-207` tracker history
