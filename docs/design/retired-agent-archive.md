# CAD-1226 — Retired-agent archive: reversible design and eligibility

Decision spike. This note changes no code, schema, timer, GC path, UI or
production state, and it archives nothing. Every claim cites source at
`origin/main` 8522672b (the base this lane was cut from). Line numbers are
for that revision and drift as the tree moves.

Related: CAD-1221 (fleet read cost), CAD-804 (GC predicate parity, keep
marker, floor, auditable plan), CAD-199 (record-only GC timer), CAD-96
(idle auto-stop), CAD-149/CAD-304 (caller rule on agent remove/gc), CAD-284
(job history survives agent removal), CAD-319 (thread archival).

## 0. Recommendation

- **Reuse of an existing state is rejected.** No existing column means
  "archived for the fleet view, reversible, no behavior change" (section 3).
- **Add a new marker** on the `agents` row: `archived_at`, `archived_by`,
  `archive_reason`. Rows are never deleted, so alias reservation, messages,
  events, task links and resume metadata stay where they are.
- **Operator-only** preview, apply and restore, behind
  `operator_connection`. Agent panes, PMs and detached children are refused.
- **Eligibility is re-proved inside the write transaction**, under the same
  lifecycle lock that `agent gc` and the timer hold. A preview is never
  authority.
- **Ordinary reads filter archived rows in SQL before enrichment**, not in
  the CLI or the UI.
- **CAD-804 owns the keep marker and GC predicate parity.** This design
  consumes `gc_keep` and adds no second keep column and no GC change.
- **Nothing has been archived or cleaned up.** This document is evidence
  of a design, not of operational cleanup.

## 1. Source map (current behavior)

### 1.1 Removal

| Path | Current behavior |
|---|---|
| `agent_remove` handler, `src/daemon.rs:3069` | Derives the caller via `authorize_agent_mutation` (`Controlled`, `src/daemon/identity.rs:651`). Refuses a live endpoint, then an owned alias (`lifecycle.owned`, `src/daemon.rs:322`). Calls `Store::remove_agent`, then kills a pty pane. |
| `Store::remove_agent`, `src/store/agents.rs:983` | One `write_tx`. Open message = state NOT IN (completed, failed, interrupted, cancelled). Open task = assignee is the alias and state NOT IN (verified, done, cancelled, failed). Without `force`, open work refuses. With `force`, queued/submitting are cancelled, running is interrupted, tasks are unassigned, and `unknown` refuses even with force. Then `prune_agent_history`, `DELETE FROM agents`, and an `agent_removed` event on the daemon stream. |
| `prune_agent_history`, `src/store/agents.rs:1167` | Deletes the alias's messages and events, except task-linked messages, `dispatch_message`/verdict messages and job-scoped events (CAD-284). Calls `thread_detach_in`. |
| `thread_detach_in`, `src/store/threads.rs:503` | Sets `threads.alias` NULL, keeps `archived_alias`, and appends a system entry "agent was removed; this thread is archived". |

Removal is destructive: it deletes the row and the alias's history. It cannot
implement a reversible archive, as the ticket states.

### 1.2 GC plan, manual sweep, timer, session end

| Path | Current behavior |
|---|---|
| `Store::gc_candidates`, `src/store/agents.rs:1190` | `endpoint IS NULL AND state IN ('attention','stopped') AND updated < cutoff`. No `enabled` check, no open-work check, no keep, no task check. |
| `gc_partition`, `src/daemon/identity.rs:771` | Applies the caller rule (`may_mutate_agent`, `src/peer.rs:397`) to each candidate. The operator is permitted every candidate, including inbox rows. |
| `agent_gc`, `src/daemon.rs:3135` | `older_than` is taken verbatim. No 7-day floor exists here or in the CLI (`src/cli/agent.rs:703-706` only parses). Each candidate goes through `remove_agent(force=false)`, so open work refuses. A pty pane is killed. |
| `agent_gc_plan`, `src/daemon.rs:3179` | Read-only. Returns bare alias lists (CAD-804 item 4). |
| `session end`, `src/session.rs:2127-2170` | Dry run calls `agent_gc_plan` with `older_than: 3600`. A real run calls `agent_gc` with `older_than: 3600`. This is a fleet-wide sweep with no floor. |
| Timer tick and sweep, `src/daemon/timers.rs:74` and `:133` | Off unless `[host] agent_gc_older_than_secs` is set (`src/daemon.rs:4214-4249`). Floor 7 days (`timers.rs:7`). The sweep skips `enabled` rows, live pty panes and lifecycle-owned aliases. |
| `timer_gc_remove`, `src/store/agents.rs:1227` | Re-checks endpoint NULL, state attention/stopped, `!enabled`, age, and zero open messages (lines 1240-1247). **It does not check non-terminal tasks.** |
| Auto-stop exemption, `src/daemon/timers.rs:176-177`, `:356` | Exempts group roots (role pm, no upstream, or named as an upstream), inboxes, attached terminals and `auto_stop=off`. GC has no equivalent exemption. |

### 1.3 enabled, state and resume

- Operator stop sets `enabled=0` (`src/daemon/agents_rpc.rs:840`).
  Resume sets `enabled=1` (`src/daemon.rs:871`).
- An unknown reconciled out of a fence sets `state='stopped', enabled=0`
  (`src/store/delivery.rs:945`). The comment at `delivery.rs:858-866` says
  `enabled=0` is what keeps a restarted daemon from relaunching the agent.
- Resume: `try_resume` (`src/daemon/agents_rpc.rs:597`) takes the lifecycle
  lock, refuses an unreconciled unknown, and calls `start_actor_locked`.
- `resumable` (`agents_rpc.rs:701-714`) is `(stopped or dead)` AND
  `thread_id` non-empty AND no unknown. It is derived, not stored.

### 1.4 Saved session metadata and identity

- Columns: `thread_id`, `session_id`, `model`, `effort`, `pid`, `pid_start`,
  `endpoint`, `generation`, `params` (JSON), `cwd`, `role`, `provider`,
  `endpoint_kind` (base table `src/store/schema.rs:247-254`; later additive
  columns, e.g. `pid_start` at `schema.rs:600`).
- Written on open at `src/store/agents.rs:578-598`.
- Duplicate alias: `register_agent` refuses at `agents.rs:297`, `:314`,
  `:328`, with `duplicate_alias` at `:377`.
- `resolve_alias` (`agents_rpc.rs:1095-1103`) falls back to
  `Store::agent_by_native` (`agents.rs:230-243`), which matches `thread_id`
  OR `session_id` with **no archive filter**.
- **Unverified:** the comment at `agents.rs:376` says a launch reuses a
  saved agent when the error contains `UNIQUE`. This spike did not find the
  consumer. It must be found and checked before any implementation.

### 1.5 Threads, jobs, tasks, messages

- `threads` (`src/store/threads.rs:238-243`): `alias UNIQUE`,
  `archived_alias`. App conversations also have an `archived` flag
  (`threads.rs:277`, `:528-535`).
- `thread_in` (`threads.rs:545`) looks up by alias only, so a thread moved to
  `archived_alias` is not returned by alias.
- `jobs` (`src/store/schema.rs:329-341`): `pm_alias`, `issue_id`, `state`.
  `tasks` (`schema.rs:342` onward): `job_id`, `assignee`, `state`,
  `dispatch_message`. `tasks_for_assignee` (`src/store/plans.rs:343`) returns
  non-terminal tasks.
- Messages: `active_messages` (`src/store/messages.rs:1698`) is queued,
  submitting, running, unknown. `FENCING_UNKNOWN_SQL` (`messages.rs:301`).

### 1.6 Read paths

- `agent_list`, `src/daemon.rs:2733`. Line 2743 reads
  `last_events_of_all` over **all** rows. Line 2747 is `for agent in
  self.store.agents()?`, a full-table read. The state/provider/kind filters
  run first (good). Enrichment follows the filter: `tasks_for_assignee`,
  capabilities, adapter lookup, `agent_liveness` (which runs
  `has_unknown`), inbox status and health, stall view, awaiting report and
  board view. There is no archive or recent-activity filter.
- `agent_show`, `src/daemon.rs:2827`: one agent, with an optional message
  window (`limit`, `since`, `active_only`).
- The CLI `agent list` scoping to a pane's group is **client-side**: it
  calls the RPC for all rows, then `retain`s (`src/cli/mod.rs:1636-1643`).
  It is not a server-side scope and must not be cited as one.

### 1.7 Caller rule and precedent

- `src/daemon/caller_rule.rs:303-311`: `agent_remove` is a handler under
  `authorize_agent_mutation`; `agent_gc` is a handler under `agent_caller` +
  `gc_partition`; `agent_gc_plan` is `Read`. A new verb must be listed here.
- `src/peer.rs:370-398`: the operator may do anything. The target's PM may do
  anything to it. An agent may self-serve only. Everyone else is refused.
- `operator_connection` (`identity.rs:809`) refuses identity-shaped request
  fields and any caller under an agent's ancestry. `operator_connection_on_agent`
  (`identity.rs:834`) is the target-alias variant.
- Precedent for the archive shape: `app_contexts.state` with a
  `CHECK(state IN ('active','archived'))` (`src/store/app_contexts.rs:17`)
  and `app_context_archive` with an expected-revision compare-and-set
  (`app_contexts.rs:216`, the update at `:251`).
- Additive nullable columns are the migration pattern (`schema.rs` `ALTER
  TABLE ... ADD COLUMN`).

## 2. Overlap with CAD-804

| Concern | CAD-804 | CAD-1226 proposal | Action |
|---|---|---|---|
| Single GC predicate (`gc_candidates`, timer, manual) | Owns | Does not touch any GC path | None here |
| Keep marker `agents.gc_keep` | Defines | Consumes it as a keep reason (E9) | One column, owned by CAD-804. CAD-1226 implementation depends on it. |
| `agent gc --older-than` floor | Owns | No age parameter at all | None here |
| Auditable plan rows (`reason`, `age`, `enabled`, `keep`) | Owns | Same field vocabulary in the archive preview | Share one reasons vocabulary if CAD-804 lands first |
| Timer ignores non-terminal tasks (`agents.rs:1240-1247`) | **Not in CAD-804** | Excluded (E6) | Recommend adding to CAD-804 scope. Timer deletes a row and leaves `tasks.assignee` naming a missing alias. `remove_agent` unassigns tasks instead (`agents.rs:1071-1087`). |
| Manual sweep has no `enabled` check; timer requires `enabled=0` | **Not in CAD-804** | Requires `enabled=0` (E4) | Recommend adding to CAD-804 scope. The two paths are opposite on this condition. |
| Inbox and standing rows reachable by manual sweep (operator is permitted all) | Observed (fav-inbox-*, standing PMs) | Excluded (E7, E8) | Confirm in CAD-804 |
| `session end` real sweep at `older_than: 3600` (`session.rs:2166`) | **Not in CAD-804 as a stop item** | Unchanged | Flag to the PM. Until CAD-804 lands, this is the largest destructive path. This spike does not run it and does not change it. |

## 3. Reuse versus a new marker

| Candidate | Verdict | Reason |
|---|---|---|
| `agents.enabled` | Rejected | Overloaded: operator stop, resume, fence lift and the timer's keep signal all write it. Archive must not change relaunch behavior. |
| `agents.state='stopped'` | Rejected | Lifecycle state and resume target. Visible in ordinary reads. Mixes with attention and offline. |
| `threads.archived` / `archived_alias` | Pattern reused, row not reused | Per-thread, app-conversation scope. Remove moves the alias, which is a routing change. The idea (archived alias does not inherit) carries over. |
| `app_contexts.state` | Shape reused | `CHECK` on state plus revision compare-and-set. |
| **New: `agents.archived_at REAL NULL`, `archived_by TEXT NULL`, `archive_reason TEXT NULL`** | **Chosen** | Additive, nullable, no backfill (NULL = not archived). The row stays, so alias reservation holds and history stays where it is. No index in v1; add one only when measured. |

Alias reservation: the row stays, so `register_agent` already refuses a
duplicate (`agents.rs:297/314/328`). The implementation should replace the
generic duplicate error with an explicit one for an archived alias: "archived;
`cadence agent restore <alias>` or choose another alias".

## 4. Eligibility

An archive is legal only if **every** rule holds. These are re-checked in the
apply transaction (section 6). Any failure refuses the whole batch.

| Rule | Condition | Source basis |
|---|---|---|
| E1 | Row exists and `archived_at IS NULL` | New column |
| E2 | `state = 'stopped'`. Refuses `attention` (pending outcome reconciliation) and `offline` (v1; operator decision D-c) | Ticket; `agent_liveness` treats attention as dead, not as retired |
| E3 | `endpoint IS NULL`; no live pty pane (`adapter::pty::pane_alive`); `lifecycle.owned` false | `timers.rs:139-160` (sweep skips enabled, live pane, owned), `daemon.rs:322` |
| E4 | `enabled = 0` | Operator-stop intent (`agents_rpc.rs:840`). A restart relaunches `enabled=1` rows (`delivery.rs:858-866`). |
| E5 | No message with state NOT IN (completed, failed, interrupted, cancelled). This covers queued, submitting, running (held), unknown and any later state. | `remove_agent` open set (`agents.rs:983+`), `FENCING_UNKNOWN_SQL` |
| E6 | No task with `assignee = alias` and state NOT IN (verified, done, cancelled, failed) | `tasks_for_assignee` (`plans.rs:343`) |
| E7 | Not `master` (`master::is_master`, `src/master.rs:393`); not an inbox (`registry::is_inbox_kind` / `is_inbox_provider`, `registry.rs:865-870`); not role `pm` | Ticket |
| E8 | Not a standing group root: not named as `params.upstream` by any row. Childless app-added workers (`plans_rpc.rs:782`) are **not** standing by this rule (D-a). | Ticket; `agent_upstream` (`daemon.rs:4634`) |
| E9 | `gc_keep = 0`. Column owned by CAD-804. | Ticket; CAD-804 |
| E10 | Preview token still matches: `plan_digest` equals the digest recomputed now, and `updated` is unchanged since preview (compare-and-set) | `app_context_archive` pattern |

Reason vocabulary (preview and refusal): `not_stopped:<state>`,
`has_endpoint`, `pane_alive`, `lifecycle_owned`, `enabled`,
`open_message:<id>:<state>`, `open_task:<id>`, `master`, `inbox`,
`standing_pm`, `group_root`, `gc_keep`, `already_archived`, `stale_preview`.

Atomicity: E1–E10 and the `UPDATE` run in one `write_tx` while holding the
lifecycle lock. The lock is required because `lifecycle.owned` lives in daemon
memory (`daemon.rs:322`) and a resume takes that lock in `try_resume`
(`agents_rpc.rs:605`). A store-only check cannot see a resume that has started
but not yet written.

## 5. Preview (read-only)

`agent_archive_plan {aliases: [...]}`, `operator_connection`, read only.

- Explicit list only. No pattern, no `all`, no age. Duplicates are refused.
  A maximum batch size applies (D-f; the design suggests 50).
- Per alias: `{alias, eligible, reasons[], state, enabled, endpoint_present,
  open_messages: {state: count}, open_tasks: [ids], keep: {master, inbox,
  standing_pm, group_root, gc_keep}, updated, thread_id_present,
  session_id_present, job_ids}`. `params`, `instructions`, `quota` and
  `model_selection` are excluded from the response.
- `plan_digest` is SHA-256 over canonical JSON of `(alias, updated, state,
  enabled, endpoint presence, open counts)`.
- The response says "preview only; apply re-checks". `eligible: true` is not
  an action.

## 6. Apply and restore

`agent_archive {aliases, plan_digest, reason}`, `operator_connection`.
Identity-shaped fields are refused (`reject_identity_fields`). `by` and
`by_kind` come from the connection through `caller_audit` (`daemon.rs:4629`),
never from a request field.

Apply, in one `write_tx` under the lifecycle lock:
1. Recompute E1–E10 for every alias.
2. If any alias fails, refuse the whole batch. Nothing is partial.
3. `UPDATE agents SET archived_at, archived_by, archive_reason WHERE alias=?`
   with `archived_at IS NULL` in the same statement.
4. One `agent_archived` event on the daemon stream:
   `{aliases, plan_digest, by, by_kind, reason}`.

Apply does **not** kill a process or pane (E3 guarantees none is live),
prune messages or events, detach threads, wake actors, or change `enabled`,
`state`, `thread_id` or `session_id`.

Replay: a second apply fails E1 (`already_archived`), refuses the batch, and
writes no second event.

`agent_restore {aliases, reason}`, `operator_connection`.
- Requires `archived_at IS NOT NULL`. Still requires E2–E4 (stopped, no
  endpoint, `enabled=0`), so an inconsistent row is refused, not repaired.
- Clears the three marker columns. Writes one `agent_restored` event.
- Does **not** set `enabled`, change `state`, or start a process. A restored
  agent remains stopped. Resume is a separate operator action through the
  existing gates (`try_resume`).

Honest rollback: restore reverts the marker only. Nothing was deleted, so
there is nothing to recover. Whether a restored agent can resume depends on
the provider keeping its session (`session_id`, `thread_id`). That is
provider-owned and was not verified in this spike. `resumable` may read true
after the provider has expired the session.

## 7. Views and retention

| View | Selection | Enrichment | Notes |
|---|---|---|---|
| Operational (default `agent_list`) | `WHERE archived_at IS NULL` in SQL | Only for listed rows | Archived rows incur no `tasks_for_assignee`, liveness, inbox, stall, awaiting or board read. The `last_events_of_all` read is restricted to listed aliases. Live count unchanged. |
| Recent activity | Operational rows whose `updated` is inside an operator-supplied `since` | As operational | No default window is proposed (D-e). A stopped worker's recent completion stays visible here while it is not archived. |
| Archived (searchable) | `WHERE archived_at IS NOT NULL`, operator-only | Lightweight projection: alias, role, provider, endpoint kind, state, `archived_at`, `archived_by`, `updated`, thread/session presence, job ids | Excludes `params`, `instructions`, `quota`, `model_selection`, `error`. Paged with limit and cursor. |
| Explicit alias (`agent_show <alias>`) | Returns the archived row with `archived: true` | As today | History stays readable. Mutations refuse with an archived message. |
| Job and task views | Unchanged | Unchanged | Messages and events are not touched, so job history reads as before. |

Native-id fallback: `agent_by_native` (`agents.rs:230`) and `resolve_alias`
(`agents_rpc.rs:1095`) must exclude archived rows, or refuse them explicitly,
so a session id cannot reach an archived alias for a mutation.

Mutation refusal: a send or resume to an archived alias refuses with an
explicit message. The send path was not located in this spike. The
implementation must find every enqueue and resume entry point.

Retention: archived rows are kept indefinitely in v1. No purge is designed.
A purge is destructive and needs its own design under CAD-804 controls
and risk trigger 2.

Performance: the SQL filter is server-side. The CLI group scoping is not,
and it must not be cited as an archive gain. Any performance claim needs
the CAD-1221 harness (about 400 agents, with a stated archived fraction),
measured before and after.

## 8. Preservation, credentials and alias safety

- Writes: the three marker columns on one `agents` row, plus one daemon-stream
  event. Nothing else.
- Untouched: messages, events (including job-scoped), tasks, jobs, verdicts,
  threads and thread entries, briefings (`client::briefing_path`), cwd and
  worktrees, pane sockets, provider sessions, and `params`.
- `params` secret handling was **not audited** in this spike. Archived
  `params` are treated as sensitive as live ones: never exported, never in a
  lightweight projection.
- No alias inheritance by construction: the row stays, so a new agent under
  the same alias cannot exist without restore. This holds only if the
  unverified `UNIQUE` reuse path (section 1.4) does not exist or is refused.
- Restore never copies, clears or rewrites `session_id` or `thread_id`.

## 9. Failure modes

| Mode | Threatens | Guard |
|---|---|---|
| Caller is an agent pane, PM, detached child or forged field | I1 | `operator_connection` plus identity-field refusal; `by` from the connection |
| Crash between steps | I3 | Single `write_tx`: all or nothing |
| Preview is stale (resume, message or task after preview) | I2 | E1–E10 re-checked under the lifecycle lock; `plan_digest` and `updated` compare-and-set |
| Concurrent resume of the same alias | I2 | Lifecycle lock; E2–E4 re-checked after it is taken |
| Replay of the same apply | I3, I6 | `already_archived` (E1); no second event |
| Loaded host, slow apply | I2 | Lock held for the write only; no wait on a provider |
| Session id reaching an archived row | I5 | Native fallback excludes archived rows |
| Re-register of an archived alias | I5 | Duplicate refusal (explicit message) |
| Clock edges | — | `archived_at` is informational. No age policy exists, so no clock decision depends on it. |
| Board or HTTP relay of the same verbs | I1 | None in v1. If added, must be at least as strict as the daemon RPC (AGENTS.md). |

Invariants:
- I1: only an operator connection may preview, archive or restore. An agent
  pane, endpoint, PM or detached child is refused.
- I2: eligibility is proven inside the write transaction under the lifecycle
  lock. A preview never authorizes.
- I3: a batch applies all-or-nothing.
- I4: archive deletes and prunes nothing. Restore starts nothing.
- I5: an archived alias cannot be re-registered or resolved for mutation.
- I6: each archive or restore writes exactly one audit event, in the same
  transaction as its state change.

## 10. Acceptance (required before implementation)

One acceptance check per enforced rule, written by someone other than the
implementer. The implementer may not edit it. The reviewer confirms it runs
the real guard.

| Check | Proves | Real guard | Wrong outcome without the guard |
|---|---|---|---|
| `archive_refuses_agent_caller` | I1 | `operator_connection` on the archive verbs | A pane agent or a PM archives a worker it does not own, or forges `by` |
| `archive_recheck_under_lock` | I2 | E1–E10 inside the apply `write_tx` under the lifecycle lock | A preview taken before a message is queued, or before a resume, archives a live agent |
| `archive_replay_no_second_event` | I3, I6 | E1 (`already_archived`) | Two archive events for one state change, or a second write |
| `archived_alias_not_reusable` | I5 | Duplicate refusal and native-fallback exclusion | `join` or a session id reattaches an archived alias's session |
| `keep_rows_never_archivable` | E7–E9 | Master, inbox, standing PM and `gc_keep` checks | An inbox or standing PM is archived with every other rule satisfied |

Recommended primary check for the first implementation ticket:
`archive_recheck_under_lock`. It exercises the state race that ordinary
tests would miss. The check runs against the real daemon store, not a mock
of it. The fixture must be an isolated temp state dir.

## 11. Risk classification (for independent review, not self-approved)

- This spike is documentation only. The change touches `docs/design/**`. That
  path appears in the trigger 7 path list in `docs/roles/risk-classes.md`.
  An independent reviewer must classify it. If it is `human`, the operator
  approves before enqueue. This author does not classify it.
- The ticket's "human (1, 4)" for the later implementation needs correcting.
  Per `risk-classes.md`, schema migrations and any change that rewrites user
  or registry data are **trigger 2** (line 7). Trigger 4 is supply chain and
  CI. The implementation is trigger 1 (`src/daemon/caller_rule.rs`,
  `src/daemon/identity.rs`, `src/daemon.rs`, `src/peer.rs`) and trigger 2
  (lifecycle rows), plus the `schema` path (`src/store/schema.rs`).
- Expected review path for the implementation: two reviews, because it touches
  trigger 1 paths. Operator approval for the head.

## 12. Operator-owned decisions (not invented here)

- D-a. The standing-service set. Does a childless group root (role worker,
  no upstream, not named by any row) count as standing? This design says no.
  The named standing roles (`fav-inbox-*`, `cs-*`, `qa-devin`, `devin-c/d`)
  are a list the operator must confirm.
- D-b. Whether an attention agent may be archived after outcome
  reconciliation, and what reconciliation outcome qualifies.
- D-c. Whether `offline` is archivable. This design says no in v1.
- D-d. Whether a PM may archive its own workers. This design says no in v1.
  A later PM-scoped rule needs its own bad-case check.
- D-e. The default window for recent activity. None is proposed.
- D-f. The batch maximum. The design suggests 50.
- D-g. Whether a stopped agent with an unacknowledged completion report is
  archivable. This design does not block on it. The operator decides.
- D-h. Retention and any purge. Out of scope.
- D-i. Whether `session end`'s fleet-wide `agent_gc` at `older_than: 3600`
  should be disabled until CAD-804 lands. An operational decision, not made here.

## 13. Recommended next increments

1. **CAD-804 first.** Its keep marker, floor, parity and plan output, plus
   the two scope additions in section 2 (timer and task check; manual versus
   timer `enabled`). Its own ticket and approvals.
2. **CAD-1226 implementation (later ticket).** Schema column (trigger 2),
   store methods (`agents_listed` with archive filter, `archive`, `restore`,
   `plan`), caller-rule entries, explicit-alias CLI verbs (`agent archive
   plan|apply|restore`), refusal paths for register, native fallback, send
   and resume, one event per mutation, and the five checks in section 10.
   Measurement against the CAD-1221 harness before any performance claim.
3. **Operational use only after** the implementation is merged and
   separately approved. The operator runs the preview on named aliases and
   applies them. Nothing in this spike does that.

## 14. Verification performed and limitations

Performed in this spike:
- Static source reads and `rg` searches at `origin/main` 8522672b.
- `git fetch origin`; HEAD equals `origin/main`.
- Duplicate check: no PR for CAD-1226 or "retired agent archive" in
  `gh pr list` (open PR #807 covers CAD-1153, unrelated).

Not performed:
- No build, test or daemon run. No isolated-state reproduction.
  The eligibility claims are read from source, not executed.
- The `UNIQUE` reuse path (`agents.rs:376`) consumer was not located.
- `params` secret content was not audited.
- Provider session retention was not verified.
- The send and resume entry points for archived-alias refusal were not
  enumerated.
- No measurement of any read-path cost. CAD-1221 owns that measurement.

## Out of scope

Schema, code, GC predicate changes, timer enablement, CLI or UI changes,
production or staging actions, archive or cleanup of any agent, and any
push or PR. Tracked by: CAD-804 (GC), CAD-1221 (read cost), the
implementation ticket still to be created.
