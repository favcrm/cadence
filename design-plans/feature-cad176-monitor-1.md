---
goal: "CAD-176 persistent local supervision and guarded task dispatch"
version: "1.0"
date_created: "2026-09-20"
last_updated: "2026-09-20"
owner: "luna-watchdog"
status: "Ready for review"
tags: [feature, supervision, cadence, jobs]
---

# Introduction

![Status: Ready for review](https://img.shields.io/badge/status-Ready%20for%20review-blue)

Implement the smallest daemon-owned supervision slice that is absent from PR #71 and PR #82. A monitor is a durable, project-scoped registration with explicit task coverage. The daemon periodically records its own heartbeat, consumes already-persisted task events from a durable cursor, and writes deduplicated local alerts for observed stall or attention events. An explicit operator command may pass a covered, demonstrably eligible task to the existing job-dispatch transaction. Healthy state is never inferred from an idle label, a queued message, an open issue, or a missing alert.

## Acceptance criteria

- **AC-001**: A registration persists its owner, project, interval,
  lifecycle state, heartbeat/check timestamps, durable event cursor,
  delivery state, and exact task coverage; a project name never adds
  implicit tasks.
- **AC-002**: A due monitor pass runs locally, records successful-check
  evidence, restores after daemon restart, and exposes `degraded` with an
  error when a pass fails. `active` describes observer execution only.
- **AC-003**: A concrete task-scoped alert is durable, acknowledged
  explicitly, and is created at most once per monitor/event fingerprint
  across repeated checks and restart. Healthy or receipt-only rows do not
  create alerts.
- **AC-004**: Delivery remains explicitly `unconfigured`; no provider,
  GitHub, approval, or production notification is activated.
- **AC-005**: `monitor dispatch` requires explicit opt-in, covered task
  scope, matching open project binding, acceptance text, a live idle
  actor, no approval/queue/competing-work conflict, and readiness where
  gated. Retries reuse an existing live kickoff through the current
  idempotent dispatch transaction. This is an explicit caller handoff, not
  autonomous backlog selection; the periodic observer never dispatches.
- **AC-006**: Focused integration tests cover both migration orders (the
  monitor v8 bridge from a pre-PR80 v6 store and monitor v8 after a PR80 v7
  store), persistent coverage/heartbeat/alert dedupe/restart,
  acknowledgement, and guarded dispatch. Ops owns the full suite and
  rollout review.

## 1. Requirements & Constraints

- **REQ-001**: Persist monitor identity, project scope, owner, explicit covered task IDs, check interval, heartbeat, last successful check, next check, event cursor, lifecycle state, and delivery state in the Cadence SQLite store.
- **REQ-002**: Restore active monitor registrations after daemon restart without a new model turn or an LLM-owned polling loop.
- **REQ-003**: Consume the existing event log by durable cursor and create at most one alert for each `(monitor, observed event)` fingerprint. A restart or repeated check must not duplicate an alert.
- **REQ-004**: Surface monitoring lifecycle (`active|degraded|off`) separately from alert delivery (`unconfigured` in this slice). Active means the daemon completed a check within the recorded interval; it does not mean that a worker is healthy or productive.
- **REQ-005**: Expose monitor registration, status, heartbeat/check evidence, coverage, alert listing, and alert acknowledgement through the daemon RPC and CLI.
- **REQ-006**: Keep task coverage explicit. Registration requires one or more task IDs; the observer never expands coverage from a project name, open issue, job membership, queue row, or worker idle state.
- **REQ-007**: Generate alerts from concrete persisted `turn_stalled`, `attention`, `paste_not_rendered`, `delivery_parked`, or future silent/approval event rows attached to a covered task. Healthy or receipt-only events do not generate alerts.
- **REQ-008**: The explicit dispatch command may call the existing `Store::dispatch_task` path only after checking project binding, task acceptance, worker endpoint/state, approval gate policy, unfinished assignments, and enabled monitor dispatch. The observer thread never dispatches or answers provider prompts.
- **SEC-001**: Do not add blanket permissions, automatic approval responses, raw terminal writes, external notifications, GitHub writes, daemon activation, or production configuration.
- **CON-001**: Base the worktree on `origin/main` at the dispatch-time SHA. Do not cherry-pick or depend on unmerged PR #71 or PR #82 code.
- **CON-002**: Use one focused Cargo command at a time with `CARGO_BUILD_JOBS=4`; do not run release builds or the full integration suite in the developer lane.
- **CON-003**: Preserve existing job-dispatch idempotency, approval claims, generation fences, cancellation, and unknown-outcome recovery semantics.
- **GUD-001**: Keep external notification delivery explicitly unconfigured; the durable local alert inbox is the only delivery surface in this increment.
- **PAT-001**: Follow the existing SQLite migration, `BEGIN IMMEDIATE` single-writer, Unix-socket RPC, `Notify`, and event-stream patterns.

## 2. Implementation Steps

### Implementation Phase 1

- GOAL-001: Add durable monitor and alert records with explicit coverage and restart-safe cursors.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-001 | Extend `src/store.rs` with monitor/coverage/alert rows, schema migration, row conversion, JSON views, and single-writer methods for register, list/show, heartbeat, observe, alerts, ack, and status. | yes | 2026-09-20 |
| TASK-002 | Enforce monitor identifiers, non-empty unique task coverage, interval bounds, project binding, deduplicated `(monitor, fingerprint)` alerts, and atomic cursor-plus-alert advancement. | yes | 2026-09-20 |

### Implementation Phase 2

- GOAL-002: Run persisted monitors from the daemon and expose their evidence through RPC/CLI.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-003 | Add a daemon monitor watch loop that schedules due active monitors, records heartbeat/check/next-check evidence, scans only covered task events, and marks check failures as degraded. | yes | 2026-09-20 |
| TASK-004 | Add monitor RPC methods and `cadence monitor register|list|show|heartbeat|alerts|ack` commands. Return coverage, lifecycle status, delivery status, last check, next check, cursor, and alert counts without claiming worker health. | yes | 2026-09-20 |

### Implementation Phase 3

- GOAL-003: Provide one explicit, guarded handoff into existing job dispatch.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-005 | Add `monitor dispatch` as an explicit action for a covered task. Require monitor dispatch enablement, an open job with matching project binding, non-empty acceptance, an enabled live worker, no competing unfinished assignment or queued turn, and a non-gated or verified-auto-ready endpoint; then call `Store::dispatch_task` and preserve its duplicate result. | yes | 2026-09-20 |
| TASK-006 | Record successful explicit handoffs as durable monitor events while leaving automatic scheduling, quotas, review-capacity policy, fairness, and lease recovery for a later issue. | yes | 2026-09-20 |

### Implementation Phase 4

- GOAL-004: Prove the public seams with focused tests and document the bounded contract.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-007 | Add integration coverage for registration/explicit coverage, heartbeat/status axes, event alert dedupe, acknowledgement, daemon restart restoration, degraded check reporting, and no alert for healthy/receipt-only events. | yes | 2026-09-20 |
| TASK-008 | Add integration coverage for safe explicit dispatch, rejection of missing acceptance/project binding/busy/fenced/non-auto-ready workers, and existing dispatch idempotency. | yes | 2026-09-20 |
| TASK-009 | Update `docs/PROTOCOL.md` with monitor RPC and CLI semantics, including the distinction between observer active/degraded/off and delivery unconfigured. | yes | 2026-09-20 |

## 3. Alternatives

- **ALT-001**: Reuse PR #71's unmerged pane/silent-end changes. Rejected because this lane must remain based on `main` and the pending recovery implementation has an independent owner/reviewer.
- **ALT-002**: Reimplement PR #82's GitHub intake/report relay. Rejected because external report transport, receipts, and GitHub dispatch are a separate pending capability.
- **ALT-003**: Run a model-driven polling agent or infer health from `idle`/`queued`/issue state. Rejected because it would not survive the overseer turn and would mislabel absence of evidence as health.
- **ALT-004**: Add an autonomous scheduler with quota, fairness, reviewer reservation, and crash leases in this increment. Deferred because the current job store has no authoritative resource/quota/reviewer-capacity signals; this slice offers one explicit guarded dispatch handoff instead.

## 4. Dependencies

- **DEP-001**: Existing `agents`, `messages`, `events`, `jobs`, and `tasks` store contracts on `origin/main`.
- **DEP-002**: Existing daemon `Notify`, event cursor, task dispatch, agent lifecycle, approval-gate, and generation-fence behavior.
- **DEP-003**: Independent QA must review the exact PR head; Ops owns merge/build admission and any later activation.

## 5. Files

- **FILE-001**: `src/store.rs` — monitor schema, durable state, coverage, alerts, and guarded dispatch predicates.
- **FILE-002**: `src/daemon.rs` — monitor watch loop, RPC handlers, status and dispatch checks.
- **FILE-003**: `src/main.rs` — monitor CLI commands and JSON output.
- **FILE-004**: `src/lib.rs` — module exports only if a new public module is required.
- **FILE-005**: `docs/PROTOCOL.md` — public monitor contract and safety boundary.
- **FILE-006**: `tests/integration.rs` — public-surface persistence, dedupe, restart, failure, and dispatch tests.

## 6. Testing

- **TEST-001**: Register a monitor with two explicit tasks, verify status reports the same coverage and delivery remains `unconfigured`.
- **TEST-002**: Run a due check twice over one concrete stalled event and verify one durable alert and one cursor advancement.
- **TEST-003**: Stop/restart the daemon with an active monitor and verify the registration/cursor restore without a duplicate alert.
- **TEST-004**: Force an invalid covered task/check failure and verify `degraded`, `last_error`, and no false healthy claim.
- **TEST-005**: Acknowledge one alert and verify it remains durable with an explicit acknowledgement event.
- **TEST-006**: Dispatch a covered eligible task once, repeat the request, and verify the existing kickoff ID/deduplication behavior is preserved.
- **TEST-007**: Verify explicit dispatch rejects absent acceptance, missing project binding, fenced/dead/native-busy/approval-gated workers, and competing unfinished work without enqueueing a message.
- **TEST-008**: Exercise both v6→v8 and v7→v8 upgrade orders, then run
  `cargo fmt --all -- --check`, focused `cargo clippy --all-targets
  --all-features -- -D warnings`, and the focused integration test filter
  under the admitted build constraints.

## 7. Risks & Assumptions

- **RISK-001**: The new SQLite migration is a data-schema change and requires independent QA/Ops review; a later main migration may require a rebase before merge.
- **RISK-002**: Existing event rows before registration are intentionally outside coverage because registration establishes a new durable cursor; re-registration is required to change scope.
- **RISK-003**: Local alerts prove daemon observation only. External user notification and acknowledgement delivery are not configured by this PR.
- **ASSUMPTION-001**: Jobs eligible for explicit dispatch carry a non-empty `jobs.repo` equal to the monitor project key; tasks without that binding are rejected rather than inferred.
- **ASSUMPTION-002**: Existing `Store::dispatch_task` remains the sole message/task transition path, so its transaction and deterministic kickoff ID remain authoritative.

## 8. Related Specifications / Further Reading

- `docs/TEAM.md` — role ownership and review routing.
- `docs/JOBS.md` — task lifecycle and dispatch invariants.
- `docs/PROTOCOL.md` — existing daemon wire contract.
- `/home/ubuntu/pm/cadence/CAD-176/artifacts/cad-176-watchdog-gap-map.md` — capability/gap map for PR #71 and the pending report relay.
- `/home/ubuntu/pm/cadence/CAD-176/artifacts/aos-cadence-lessons-20260920.md` — observed supervision failure and acceptance scenario.
