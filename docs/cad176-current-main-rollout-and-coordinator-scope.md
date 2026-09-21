# CAD-176 current-main rollout and coordinator scope

This note binds the follow-up to the merged tree used for the isolated
rehearsal. It is evidence and an operator packet; it does not activate a
daemon, migrate a live store, restart a board, or configure outbound delivery.

## Current tree and isolated evidence

- Baseline source used for the recorded v6-to-v8 rehearsal:
  `f1dd5a300bae3897d34a70f550feead5d12a6b35` (PR98 UI plus PR99 durable
  handoff fix).
- Review candidate: this worktree rebased onto current `origin/main`
  `9d2414f`, carrying the separate coordinator v8-to-v9 migration. The
  final full candidate head is supplied with the review handoff after the
  documentation commit.
- Isolated binary: `target/debug/cadence`, built with
  `CARGO_BUILD_JOBS=4 cargo build --locked` from that exact tree.
- Binary identity: `cadence 0.1.0+f1dd5a300bae3897d34a70f550feead5d12a6b35`.
- Fixture: a copy of
  `~/.local/state/cadence/backups/cadence-live-20260921T072606Z.sqlite3`.
  The live state directory was never opened by this binary.
- Before: schema 6, 21 agents, 2079 messages, integrity `ok`.
- After starting and stopping the isolated daemon: schema 8, monitor tables
  `monitors`, `monitor_tasks`, and `monitor_alerts`, `agents.effort` present
  with 0 non-null rows, 21 agents, 2079 messages, integrity `ok`, and
  `monitor list` returned an empty registration set.

The installed `/home/ubuntu/.local/bin/cadence` still identifies as the older
`6279117b` build. It is not evidence for the merged tree and must not be used
to describe a current-main rollout.

The evidence above is the baseline v6-to-v8 rehearsal from the merged tree.
This worktree adds a separate v8-to-v9 migration for the coordinator's
`auto_dispatch_enabled` column. That candidate migration needs its own
isolated rehearsal and reviewed build; the baseline evidence does not prove
that v9 candidate or authorize a live schema change.

## Operator-only live sequence

1. Preserve both deleted-inode daemon and board executables, then take a fresh
   SQLite `.backup` immediately before any restart. Record the pre-restart
   schema and row counts and verify `PRAGMA integrity_check` is `ok`.
2. Verify no in-flight turns, install the reviewed current-main binary at a
   versioned path, and restart the daemon through the normal `--when-idle`
   gate. This is the v6-to-v8 migration boundary.
3. Verify build commit, schema 8 for the baseline monitor rollout (or schema
   9 only after the separate coordinator candidate is reviewed), monitor
   tables, `agents` and `messages`
   counts, integrity, and the monitor RPC before restarting the board.
4. Restart the board against the same reviewed build and verify its health and
   Overview response. The UI is local visibility; it is not push delivery or
   proof that a human saw an alert.

Rollback restores the fresh pre-restart backup and the preserved old binaries;
rows written after that backup are lost. Reverting code while retaining the
new store is unsupported. No live step is included in this change.

## Bounded coordinator increment

The existing `dispatch_enabled` registration bit remains a manual-dispatch
permission. It is not reinterpreted as consent for a background scheduler.
The follow-up adds a separate persisted `auto_dispatch_enabled` opt-in. A
registration must explicitly request both flags; without the new flag the
watcher only observes and records alerts as before.

For an opted-in registration the daemon periodically reconciles its explicit
task coverage. For each due `draft` or `revising` task it calls the same guarded
dispatch checks used by `monitor dispatch`: open job and project binding,
explicit acceptance and assignee, live actor identity, readiness, approval
waits, queued work, and unfinished-task capacity. No admin merge, approval,
role, project, or external-agent inference is added. A `dispatched` or
`running` task reuses its durable kickoff only when the existing idempotent
branch proves it is live; it is never minted twice.

Guard failures are recorded as one deduplicated durable `dispatch_blocked`
alert per task, with the latest reason and an operator action. The retry unit
is one monitor interval; there is no tight retry loop or queued-message spam.
Quota telemetry that is absent remains `unknown` and is surfaced in the block
reason. Successful task completion continues through the existing result,
independent-review, author-revision, and Ops routing path.

Passive inboxes are a separate transport boundary. An actor queue can wake its
owned provider and has a bounded fallback wait. An inbox endpoint has no actor:
the coordinator may observe queued count and oldest age and emit a durable
backlog/future-work signal, but it must not call `inbox_read` and claim semantic
completion. A mailbox receipt is delivery evidence only. External
collaboration identities such as `fable-cc` are not Cadence aliases and are
not addressed by this scheduler.

Acceptance for this slice is focused and reviewable:

- auto dispatch requires the new explicit opt-in and preserves the manual RPC;
- a guarded busy/queued/approval/unknown-quota route produces one durable,
  actionable block record and retries only on later reconciliation ticks;
- a successful dispatch has one kickoff across repeated checks and daemon
  restart, and the normal completion path reaches the existing independent
  review route;
- the auto opt-in and block alert survive store reopen;
- passive inbox rows remain queued with age/backlog evidence and are never
  treated as semantic task completion by the coordinator.

The existing monitor deployment, current-main build, or this follow-up alone
does not establish full autonomous operation. The end-to-end acceptance
exercise still needs an operator-admitted fixture with a real worker result,
independent review, restart mid-handoff, and the separate live rollout gate.
