# CAD-1226 — Retired-agent archive: reversible design and eligibility

Decision spike, revision 2. Revision 2 answers the independent QA verdict
`20261008-085249-3c1a2-retired-agent-archive-sol-verdict.md` (REVISE on
7b7c6038). It changes no code, schema, timer, GC path, caller rule, UI or
production state, and it archives nothing. Source line numbers are at
`origin/main` 8522672b. The cited files are unchanged at the rebased base `fedd7964`. Re-check function names before citing. Revision 3 adds C1 and C2 from the Sol re-QA on `349eea99`.

Related: CAD-1221 (fleet read cost), CAD-804 (GC predicate, keep marker,
floor, plan output), CAD-199 (record-only timer), CAD-96 (idle auto-stop),
CAD-149/304 (caller rule), CAD-284 (job history), CAD-316 (delivery rollup),
CAD-319 (thread archive).

## 0. Status and recommendation

- An archived row is **not** protected by being stopped, disabled, idle or
  `gc_keep=0`. Today every deletion path accepts such a row (section 1.2).
  Retention needs explicit deletion guards (section 4). Those guards are a
  **release prerequisite**: no code path may write an archive marker before
  they are merged and independently accepted (section 9).
- Reuse of an existing state is rejected. Add nullable `archived_at`,
  `archived_by`, `archive_reason` and a monotonic `lifecycle_rev` to the
  `agents` row. The row is kept; its alias stays reserved.
- Preview, apply and restore are operator-only (`operator_connection`).
  Apply and restore are compare-and-set on `lifecycle_rev` inside one
  `BEGIN IMMEDIATE` transaction. A stale or replayed request writes nothing.
- No age policy, purge, timer, or automatic selection is designed. Standing
  services and rows of unknown retention intent are refused until the
  operator decides (section 11).
- Nothing has been archived or cleaned up. This note is design evidence
  only. It is not implementation, merge, installation or operational
  acceptance.

## 1. Source map

### 1.1 Deletion and candidate paths (verified)

| Path | Behavior at 8522672b |
|---|---|
| `Store::remove_agent`, `src/store/agents.rs:983` | Refuses a live endpoint and open work (state NOT IN completed/failed/interrupted/cancelled; task not in verified/done/cancelled/failed). `force` cancels or interrupts messages and unassigns tasks (`agents.rs:1071-1087`). Then `prune_agent_history` (`:1167`) deletes the alias's messages and events, except task-linked, dispatch/verdict messages and job-scoped events. Then `DELETE FROM agents` (`:1150`). |
| `Store::timer_gc_remove`, `src/store/agents.rs:1227` | Checks endpoint NULL, state attention/stopped, `!enabled`, age, and zero open messages (`:1240-1247`). **No non-terminal task check.** Then prune and `DELETE FROM agents` (`:1252`). |
| `Store::gc_candidates`, `agents.rs:1190` | Endpoint NULL and state attention/stopped and `updated < cutoff`. No archive, enabled, keep or task filter. |
| Callers of `remove_agent` | `agent_remove` (`src/daemon.rs:3113`); `agent_gc` (`daemon.rs:3155`, per candidate with `force=false`); `master_rpc.rs:813` (master, force). |
| Callers of `timer_gc_remove` | Timer sweep (`src/daemon/timers.rs:157`). |
| `agent_gc` and `agent_gc_plan`, `daemon.rs:3135` and `:3179` | `older_than` taken verbatim, no 7-day floor. Plan returns bare aliases. |
| `session end`, `src/session.rs:2127-2170` | Dry run plans and real run sweeps via `agent_gc` at `older_than: 3600`. A `--project` run skips the sweep. |
| `roll_up_delivery_events`, `src/store/events.rs:348` (CAD-316) | Folds `submitting`/`submitted` rows of **every** alias older than the cutoff into one summary row. It rewrites history for any alias, archived ones included. |

Result: a row with `state='stopped'`, `enabled=0`, endpoint NULL, no open
work and `gc_keep=0` is deletable through manual GC, session end, the
timer, direct `agent_remove`, and the master path. Archive alone gives no
retention.

### 1.2 Actor start, enqueue, assignment and identity (verified)

| Path | Behavior |
|---|---|
| `start_actor_locked`, `src/daemon.rs:841` | The single actor-start choke point. Called by `launch_actor` (`daemon.rs:826`), `try_resume` (`src/daemon/agents_rpc.rs:597`) and `auto_resume` (`src/daemon/timers.rs:697`, start at `:730`). Sets `enabled=1` only when `enable` is true, and always writes state `starting`. |
| `auto_resume_tick`, `timers.rs:632` | Selects `queued_for_stopped` (`src/store/messages.rs:1938`: messages `queued` on `stopped` rows). A stop marker that is still newest triggers `auto_resume`. An archived row with a queued message would be restarted. |
| `enqueue_tx_as`, `src/store/messages.rs:1045` | The transactional enqueue for every caller message, task kickoff and daemon notice. It calls `agent_in` (any row, archived or not). |
| `send_with`, `src/daemon/messages_rpc.rs:80-82` | Resolves the target and enqueues. A handler precheck alone loses to a racing archive. |
| `dispatch_task`, `src/store/plans.rs:606` | Assigns a task inside a transaction after `agent_in` (around `:630`). |
| `route_notice`, `src/store/delivery.rs:595`; `recipient_binding`, `src/store/messages.rs:1215` | Routes terminal reports to `reply_to`. Reasons today: `recipient_missing`, `recipient_identity_unavailable`, `recipient_identity_changed`. A report that cannot route is recorded as `handoff_unresolved` (`messages.rs:1262`, a daemon-stream event). |
| `resolve_alias`, `agents_rpc.rs:1095` | Exact alias first (`agent_opt`), then `agent_by_native` (`src/store/agents.rs:230`; `thread_id` OR `session_id`, no archive filter). |
| `Agent::to_json`, `src/store/agents.rs` (around `:200`) | Includes `params`, `quota`, `model_selection` (presented), `error`; not `instructions`. `agent_show` returns this JSON unchanged. |
| Registration reservation | `register_agent` refuses duplicates (`agents.rs:297`, `:314`, `:328`, `duplicate_alias` at `:377`). |
| Launch UNIQUE branch, `src/cli/launch.rs:462-470` | On an error whose text contains `UNIQUE`, it calls `agent_show`, then `agent_resume` for `stopped` or `offline` rows. Reservation alone therefore resumes a reserved row. |

### 1.3 Other facts

- `write_tx` runs every mutation in `BEGIN IMMEDIATE` (`src/store/mod.rs`
  module doc, lines 1-6). The `lifecycle` map in daemon memory is the
  ownership record (`src/daemon.rs:322`) and needs its own lock.
- `enabled` is written by stop (`agents_rpc.rs:840`, `=0`), resume (`daemon.rs:871`,
  `=1`), and unknown reconciliation (`src/store/delivery.rs:945`, `=0`). The
  comment at `delivery.rs:858-866` says `enabled=0` keeps a restarted daemon
  from relaunching the agent.
- `resumable` is derived (`agents_rpc.rs:701-714`): stopped or dead, a
  `thread_id`, and no unknown. It is never stored.
- Auto-stop exempts group roots, inboxes and attached terminals
  (`timers.rs:176-177`, `:356`). GC has no equivalent exemption.
- Precedent for the marker shape: `app_contexts.state` with a `CHECK`
  (`src/store/app_contexts.rs:17`) and an expected-revision compare-and-set
  (`app_context_archive`, `:216`; the update at `:251`).

### 1.4 Saved metadata, threads and resumability (C2)

- Open writes the saved metadata in one statement: `thread_id`, `session_id`,
  `model`, `effort`, `pid`, `pid_start`, `endpoint`, `generation`, `quota`,
  `state='idle'`, `updated` (`src/store/agents.rs:578`), plus a `ready` event.
  `params` and `cwd`, `role`, `provider`, `endpoint_kind` are set at
  registration (`register_agent`). `params` changes through `agent_set`
  (`src/daemon/agents_rpc.rs:337`).
- `resumable` is derived, never stored (`agents_rpc.rs:701-714`): stopped or
  dead, a non-empty `thread_id`, and no unknown. An archived stopped row with a
  `thread_id` therefore reads `resumable: true`. That is local metadata, not
  provider availability.
- Removal detaches threads (`src/store/threads.rs:503`, `thread_detach_in`).
  The home thread gets a system entry, and its `alias` becomes NULL with
  `archived_alias` set. `conversations_detach_in` (`threads.rs:531`) does the
  same for app conversations (`install_id IS NOT NULL`), sets `archived=1`, and
  is also called on the home-thread path.
- Lookup: `thread(alias)` (`threads.rs:541`, `thread_in` at `:545`) matches the
  live alias only. `thread_by_id` (`threads.rs:562`) matches id, and
  `rpc_thread_read` (`src/daemon/threads_rpc.rs:9-20`) then requires
  `t.alias == alias`. After a removal detach the alias is NULL, so no current
  route returns that thread by id. Archived-thread history is therefore not
  reachable through existing reads. No route is invented here.
- Archive (this design) leaves `thread_id`, `session_id`, `params`, `cwd` and
  the thread `alias` untouched: no detach and no move. Thread reads keep their
  current rules. Restore clears only the marker. It does not start, rebind or
  verify a provider session.
- Unverified, not audited here: provider lifetime of a stored `session_id` or
  `thread_id`; whether `params` contain secrets (`agent_show` returns them);
  credential and provider availability.

## 2. Overlap with CAD-804

| Concern | Owner | This design |
|---|---|---|
| Single predicate for `gc_candidates`, timer and manual sweep, including `enabled` and open-message parity | CAD-804 item 1 (already owns it) | Relies on it. No change proposed here. |
| `gc_keep` column and keep parity | CAD-804 item 2 | Consumes it as E9. Archive and restore never write it. |
| `agent gc --older-than` floor | CAD-804 item 3 | None here. The archive has no age parameter. |
| Auditable plan rows | CAD-804 item 4 | Archived rows must appear there as kept with reason `archived` (section 4). |
| Timer ignores non-terminal tasks | **Proposed addition**, not in CAD-804 | Recommended scope addition. Protection in section 4 also depends on it. Not adopted. |
| Archived-row deletion guards | **Not in CAD-804** | Section 4. Owner is a PM decision (D-j). It must land before any archive writer. |

## 3. Reuse versus a new marker

| Candidate | Verdict | Reason |
|---|---|---|
| `enabled` | Rejected | Stop, resume, unknown fence and the timer's keep signal all write it. Archive must not change relaunch behavior. |
| `state='stopped'` | Rejected | Lifecycle state and resume target. Shows in ordinary reads. |
| `threads.archived` / `archived_alias` | Idea reused, row not | Per-thread, app conversation. Removal moves the alias, which is a routing change. |
| `app_contexts.state` | Shape reused | `CHECK` on state plus revision compare-and-set. |
| **New: `archived_at`, `archived_by`, `archive_reason`, `lifecycle_rev`** | **Chosen** | Additive: `archived_*` are nullable (NULL = not archived). `lifecycle_rev` is `NOT NULL DEFAULT 0` and monotonic (section 6). The row is kept, so alias reservation holds. |

## 4. Retention and deletion protection (B1)

### 4.1 What "retained" means

For an archived alias, retained means all of these survive until an explicit
future decision: the `agents` row (with its marker and resume metadata),
its messages, its events, its task and job bindings, its thread history,
and its verdicts. The only accepted change to message and event history is
the CAD-316 delivery rollup, which is excluded for archived aliases (P-DEL-d).
Nothing in this design purges or ages out archived history.

`gc_keep=0` is an eligibility condition for archiving. It provides **no**
retention after archive. Retention comes only from the guards below.

### 4.2 Required deletion guards (P-DEL)

These are specified here as dependencies. No guard is implemented by this
document.

- **P-DEL-a, transactional refusal in both delete functions.** Inside the
  existing `write_tx`, before any prune or `DELETE`:
  - `remove_agent` returns a rejection with reason `archived` when
    `archived_at IS NOT NULL`. This check runs first, so `force` cannot
    bypass it.
  - `timer_gc_remove` returns `Ok(None)` for the same condition, with no
    event.
  The check must run first. A SQLite `RAISE(ABORT)` aborts only the failing
  statement. On the normal error path the enclosing `write_tx` rolls back the
  whole callback when the error propagates (`src/store/mod.rs:308-318`,
  `src/store/seal.rs:835`), so earlier prune statements are undone. A swallowed
  error that lets the callback return success would commit them. A pre-prune
  refusal therefore stays the primary guard; the trigger is only a backstop.
- **P-DEL-b, truthful candidates.** `gc_candidates` adds
  `AND archived_at IS NULL`. Manual `agent gc`, `agent_gc_plan`, the session
  end dry run and the timer all then exclude archived rows. The plan output
  (CAD-804 item 4) lists archived rows as kept with reason `archived`, so an
  operator's dry run never reports them as sweepable and never hides them
  silently.
- **P-DEL-c, backstop for any other row delete.** A `BEFORE DELETE ON agents
  WHEN OLD.archived_at IS NOT NULL` trigger raises `RAISE(ABORT)`. It covers
  raw SQL and future callers. It is a backstop only; P-DEL-a is the primary
  guard, because the trigger does not stop earlier history deletes in the
  same transaction.
- **P-DEL-d, history rewrite excluded.** `roll_up_delivery_events` (`events.rs:348`)
  skips aliases with `archived_at IS NOT NULL`.

Coverage: the delete sites are `agents.rs:1150` (`remove_agent`) and
`agents.rs:1252` (`timer_gc_remove`). The callers are listed in section
1.1. The master path (`master_rpc.rs:813`) is covered by P-DEL-a.

### 4.3 Restore and gc_keep

- Archive writes only `archived_at`, `archived_by`, `archive_reason` and
  `lifecycle_rev`. It never writes `gc_keep` (CAD-804) or `enabled`.
- Restore clears the three marker columns and bumps `lifecycle_rev`. It
  does not write `gc_keep`, so the pre-archive keep policy stays exactly as
  it was.
- Restore is local metadata only. It does not guarantee the provider still
  holds the session named by `session_id` or `thread_id`. That is
  provider-owned and unverified here. Restore does not start, rebind or move
  threads (section 1.4).

### 4.4 Ownership and ordering

The deletion guards share the predicate and callers that CAD-804 already
owns. The PM decides whether they land inside CAD-804 or in a sibling ticket
that merges first (D-j). Either way, the archive writers in section 9 depend
on them.

## 5. Eligibility

Archive apply re-proves every rule inside its transaction. Any failure
refuses the whole batch.

| Rule | Condition | Basis |
|---|---|---|
| E1 | Row exists; `archived_at IS NULL` | New column |
| E2 | `state='stopped'`. `attention` (unreconciled outcome) and `offline` refused in v1 | Ticket. D-c covers offline. |
| E3 | Endpoint NULL; no live pty pane; not lifecycle-owned | `timers.rs:139-160`, `daemon.rs:322` |
| E4 | `enabled=0` | Stop intent; `delivery.rs:858-866` |
| E5 | No message with state NOT IN (completed, failed, interrupted, cancelled) | `remove_agent` open set; `messages.rs:301` |
| E6 | No task with `assignee=alias` and state NOT IN (verified, done, cancelled, failed) | `plans.rs:343` |
| E7 | Not master (`src/master.rs:393`), not inbox (`registry.rs:865-870`), not role `pm` | Ticket |
| E8 | Not a standing root: not named as `params.upstream` by any row | `daemon.rs:4634`; D-a |
| E9 | `gc_keep=0` (CAD-804 column) | Ticket |
| E10 | Every supplied `expected` digest matches the recomputed digest (section 6) | Section 6 |

Refusal reasons: `not_stopped:<state>`, `has_endpoint`, `pane_alive`,
`lifecycle_owned`, `enabled`, `open_message:<id>:<state>`,
`open_task:<id>`, `master`, `inbox`, `standing_pm`, `group_root`, `gc_keep`,
`already_archived`, `stale_preview`, `archived` (for delete and enqueue
refusals).

## 6. Revision and preview contract (B2)

### 6.1 `lifecycle_rev`

- `agents.lifecycle_rev INTEGER NOT NULL DEFAULT 0` (new row starts at 0).
- A trigger increments it on every `UPDATE OF state, enabled, endpoint,
  archived_at, gc_keep`. Every existing writer of those columns is covered
  without editing each one. The trigger is chosen over explicit increments,
  which are easy to miss: a grep finds 13 `UPDATE agents SET` matches for these columns, including tests and migrations, and the grep may miss multi-column forms.
- Nothing decrements it, so it is monotonic. Archive bumps it, and restore
  bumps it again.
- It does not depend on wall-clock `updated`, which can repeat or stay the
  same across a write.

### 6.2 Digest

`digest = SHA-256(canonical JSON {alias, lifecycle_rev, state, enabled,
endpoint_present, gc_keep, archived, open_messages sorted by id with state,
open_task_ids sorted})`. It excludes `updated`.

### 6.3 Preview, apply, restore

- Preview (`agent_archive_plan`, operator-only, read-only): explicit aliases
  only, no pattern, `all` or age. Duplicates refused. A batch maximum applies
  (D-f). Returns per alias: `eligible`, `reasons`, `lifecycle_rev`, `digest`,
  and the fields in section 5. Preview states "apply re-checks". It is never
  authority.
- Apply (`agent_archive`, operator-only): request carries
  `expected: [{alias, digest}]` and `reason`. Inside one `BEGIN IMMEDIATE`
  transaction under the lifecycle lock:
  1. Recompute the digest and eligibility for each alias.
  2. The supplied set must equal the target set. Any missing or extra alias,
     any digest mismatch, or any eligibility failure refuses the whole batch.
  3. Only if all pass: set `archived_at`, `archived_by`, `archive_reason`
     (the trigger bumps `lifecycle_rev`), and write one `agent_archived` event
     on the daemon stream.
- Restore (`agent_restore`, operator-only): request carries
  `expected: [{alias, lifecycle_rev}]`. Requires `archived_at IS NOT NULL`
  and the same CAS, all-or-nothing. Still requires E2–E4 and E9. On success it
  clears the three marker columns (bumps `lifecycle_rev`) and writes one
  `agent_restored` event. It does not set `enabled`, change `state`, or start
  an actor.

Replay table (each row is one request; the state column is the row before the
request):

| Sequence | Second request result | Writes |
|---|---|---|
| Preview P at rev r → apply P → rev r+1 archived | P's digest no longer matches at r+1; `already_archived` | none |
| Archive → restore (rev r+2) → replay P | P digest mismatch (`stale_preview`); row not archived; `lifecycle_rev` was r+2, not r | none |
| Archive → replay archive | `already_archived`; no second event | none |
| Mixed batch: fresh alias A and stale alias B | Whole batch refused with per-alias reasons; A is not archived | none |

A refusal writes no row and no event. An implementation that wants refusal
diagnostics must not use the audit event stream for them.

## 7. Actor, enqueue, assignment and launch paths (B2 related)

Reads: `agent_show <alias>` (exact alias) returns an archived row with
`archived: true`. The operational list does not return archived rows (section
8). The races below are about **writes** that can make an archived row active.

| Path | Required refusal (reason `archived`) | Basis |
|---|---|---|
| `start_actor_locked` (`daemon.rs:841`) | Refuse before `set_enabled`/`starting`. This one check covers `try_resume`, `launch_actor` and `auto_resume`. | Shared choke point |
| `queued_for_stopped` (`messages.rs:1938`) | Exclude archived rows in SQL, so auto-resume never selects them. Defence in depth. | `timers.rs:632` |
| `enqueue_tx_as` (`messages.rs:1045`) | Refuse an archived target inside the transaction. Covers send, dispatch kickoff, daemon notices and routed reports. | Single enqueue point |
| `dispatch_task` (`plans.rs:606`) | Refuse an archived assignee inside the transaction. | `plans.rs:606-630` |
| `recipient_binding` (`messages.rs:1215`) | New reason `recipient_archived`, recorded through the existing `handoff_unresolved` path. | `messages.rs:1262` |
| `rpc_set`, `rpc_stop`, unfence (`agents_rpc.rs:337`, `:800`) | Refuse on an archived row. | Mutating RPCs |
| Native fallback (`agents.rs:230`; `resolve_alias`) | Exclude archived rows for mutation resolution. | Session id must not reach an archived alias |
| `register_agent` duplicate | Archived-specific error that does **not** contain the text `UNIQUE`. | `cli/launch.rs:462` |

Races, with the guard in place:
- **Archive vs enqueue.** Both run in `BEGIN IMMEDIATE` on one connection,
  so they serialize. If the enqueue commits first, E5 sees the open message
  and archive refuses. If archive commits first, the enqueue refuses the
  archived target.
- **Archive vs resume.** The lifecycle lock covers the daemon-side owned
  check. `start_actor_locked` re-checks archive state after the lock is held,
  so a resume that began before archive refuses instead of starting.
- **Archive vs task assignment.** The validator (section 7.1) runs inside each
  assignment transaction. E6 checks open tasks at archive time. Both orders
  refuse or serialize, as with enqueue.

**7.1 Task assignment (C1).** Four write paths put an assignee on a task or
move a terminal task back to nonterminal. The proposed shared validator
`archived_assignee_refusal(tx, alias)` is a design name only. It refuses an
alias whose row is archived, and it runs inside the same `BEGIN IMMEDIATE`
transaction as the write, after the existing `agent_in` lookup.

- `create_job` (`src/store/plans.rs:384`; assignee check at `:473-476`). The
  job row and `job_created` event are written earlier in the callback. The
  refusal propagates, so the write transaction rolls them back. No partial job,
  task or event remains. Reached by `rpc_job_new` (`src/daemon/jobs_rpc.rs:34`).
- `create_task` (`plans.rs:512`; check at `:540-542`), reached by `rpc_task_new`
  (`jobs_rpc.rs:280`). Same rule and rollback.
- `dispatch_task` (`plans.rs:606`; check at `:630-631`). The existing guard in
  section 7 is the same check. The shared validator must replace it, not sit
  beside it.
- `reopen_task` (`plans.rs:968`), reached by `rpc_task_reopen`
  (`jobs_rpc.rs:458`). It sets `blocked`, `verified` or `failed` to `draft`,
  nonterminal, and keeps the assignee. Refuse when the assignee is archived. The
  refusal changes nothing: no state write, no assignee cleared, and the terminal
  historical link is kept.

Other writers of task state or assignee are not validated by this design:
`plans.rs:720, 867, 939, 981, 1010, 1054`; `agents.rs:1076` (force-unassign);
`delivery.rs:1130, 1187`; `monitors.rs:593`; and seven `app_runs.rs` writers.
Inventorying them is an activation prerequisite.

Launch UNIQUE. `cli/launch.rs:462-470` takes the UNIQUE branch, calls
`agent_show`, and then `agent_resume` for `stopped` or `offline`. The
archived-specific register error must not contain `UNIQUE`, so launch does
not enter that branch. Even if it did, `start_actor_locked` refuses the resume.
Reservation alone is not sufficient protection.

Routed terminal reports. A terminal result stays on the sender's message row
and in its events. A report routed to an archived `reply_to` is not delivered.
It is recorded as `handoff_unresolved` (`recipient_archived`) on the daemon
stream, and the sender's result is unchanged. No automatic re-delivery is
designed. Any later re-route requires an explicit operator action, which is
not specified here.

## 8. Views

| View | Selection | Notes |
|---|---|---|
| Operational (default list) | `WHERE archived_at IS NULL` in SQL, before enrichment | Archived rows incur no per-row enrichment (tasks, liveness, inbox, stall, board). `last_events_of_all` is restricted to listed aliases. This is a server-side claim. The CLI group scope (`src/cli/mod.rs:1636-1643`) is client-side and is not counted. |
| Recent activity | Operational rows (archived rows excluded in SQL, as above) whose `updated` falls inside an operator-supplied `since` | No default window (D-e). |
| Archived (searchable) | `WHERE archived_at IS NOT NULL`, operator-only, lightweight projection | Projection excludes `params`, `instructions`, `quota`, `model_selection`, `error`. This is not an absolute "never exported" claim: `agent_show` still returns full rows, and that behavior is unchanged. |
| Explicit alias | Full row with `archived: true` | Unchanged except for the flag. |

Performance claims need the CAD-1221 harness: about 400 agents, a stated
archived fraction, measured before and after.

## 9. Release ordering and activation

Activation is blocked until every earlier release is merged and independently
accepted. No step introduces a runtime enable flag, which would be an invented
policy.

R1 to R3 are logical dependencies inside one eventual archive feature PR. They
are not separate PRs and not a stack. AGENTS.md asks for one end-to-end PR unless
a genuinely permitted split exists, and that PR gets the reviews and approvals
its own diff requires. A future owner allocation given by the PM is not
operator approval. It does not assume CAD-804 owns P-DEL.

- **R1 (protection, schema).** Adds `archived_at`, `archived_by`,
  `archive_reason`, `lifecycle_rev` and the trigger. Adds P-DEL-a, b, c and d,
  the reason `archived` in plan output, and the timer task check if D-j
  adopts it. No writer sets `archived_at`, so R1 is inert.
- **R2 (writer guards).** Adds the section 7 refusals: `start_actor_locked`,
  `queued_for_stopped`, `enqueue_tx_as`, `dispatch_task`, `recipient_binding`,
  mutating RPCs, native fallback, and the register error. Still no writer of
  `archived_at`.
- **R3 (verbs).** Adds preview, apply and restore, registers them in
  `src/daemon/caller_rule.rs` (currently none), and adds the explicit-alias CLI.
  R3 may merge only after R1 and R2 acceptance checks pass and D-a, D-g and
  D-j are decided by the operator.
- **Activation** means R3 merged and operator-approved for the head. Nothing
  here asserts that approval.

## 10. Invariants and future acceptance

Invariants:
- I1: only an operator connection may preview, archive or restore. Agent
  panes, endpoints, PMs and detached children are refused. `by` comes from
  the connection, never a request field.
- I2: eligibility and the digest are re-proved inside the write transaction,
  under the lifecycle lock. A preview is never authority.
- I3: a batch applies all-or-nothing. A refusal changes no row and writes no
  event.
- I4: archive and restore delete nothing and start nothing. Every delete path
  refuses an archived row (P-DEL-a to c).
- I5: an archived alias cannot be enqueued to, assigned to, started, resumed,
  mutated, or re-registered.
- I6: each successful archive or restore writes exactly one audit event, in
  the same transaction as its state change.
- I7: `lifecycle_rev` never decreases. A request carrying a non-current revision
  is refused.

Future acceptance. One check per enforced rule, written by someone other than
the implementer. The implementer may not edit it. The reviewer confirms each
runs the real guard, not a mock or a CLI-only filter. Fixtures use an isolated
temp state dir.

| Check | Proves | Real guard | Wrong outcome without the guard |
|---|---|---|---|
| `archive_refuses_non_operator` | I1 | `operator_connection` on the verbs | A pane agent or PM archives a worker, or forges `by` |
| `archive_recheck_under_lock` | I2, E1–E6 | Eligibility inside the apply transaction | A message queued after preview is archived |
| `archive_rev_aba_replay` | I3, I6, I7 | `lifecycle_rev` trigger and digest comparison | Preview → archive → restore → replay re-archives the restored row and emits a second event |
| `archive_mixed_batch_all_or_nothing` | I3 | Batch-set equality plus per-alias digest | Fresh alias archived while stale alias is refused |
| `archived_row_survives_deletion` | I4, P-DEL-a to c | `remove_agent`, `timer_gc_remove`, the BEFORE DELETE trigger | Manual GC, session end at 3600, timer, direct remove (force and not), master path or raw SQL deletes the row or its history |
| `archived_row_not_started_or_enqueued` | I5 | `start_actor_locked`, `enqueue_tx_as`, `dispatch_task`, `queued_for_stopped` | Send, dispatch, resume or auto-resume (with an earlier auto-stop marker) starts or queues the archived row |
| `archived_assignee_refused_on_create_and_reopen` | I5, C1 | `archived_assignee_refusal` in `create_job`, `create_task`, `dispatch_task`, `reopen_task` | An archived worker gets a nonterminal assignment through create or reopen. Refusal must leave no job, task, event or state change, and a reopen race with archive must refuse. |
| `routed_report_to_archived_recipient` | Section 7 | `recipient_binding` reason | Report delivered into an archived row, or dropped without a record |
| `archived_register_not_unique_recovery` | I5 | Register error text plus start guard | `launch` takes the UNIQUE branch and resumes the archived alias |
| `rollup_skips_archived_alias` | Section 4.1 | P-DEL-d | Archived alias's delivery events folded |
| `restore_preserves_gc_keep` | Section 4.3 | Restore writes no `gc_keep` | Restore clears or sets the pre-archive keep policy |
| `archived_rows_excluded_before_enrichment` | Section 8 | SQL filter before the per-row loop | Enrichment runs for archived rows, or the filter is UI-only |

Recommended primary check for R1: `archived_row_survives_deletion`. It is the
gap the QA verdict named. For R3: `archive_rev_aba_replay`.

## 11. Risk classification and operator-owned decisions

Risk classification is a recommendation for independent review. This author
does not classify and does not approve.

- **Current document (this revision):** `human (7)`, per the QA verdict and
  `docs/roles/risk-classes.md`, which lists `docs/design/**` under trigger 7.
- **Ticket's "human (1, 4)"** should be corrected for the future work. Schema
  and data deletion are trigger 2 (`risk-classes.md` line 7). Trigger 4 is
  supply chain and CI, which this work does not touch.
- **R1 (protection, schema):** at least trigger 2. `src/store/schema.rs` is the
  schema path. The delete guards are in `src/store/agents.rs`, a deletion
  change under trigger 2.
- **R2 and R3 (writers, verbs):** at least trigger 1. Paths include
  `src/daemon.rs`, `src/peer.rs`, `src/daemon/identity.rs`,
  `src/daemon/caller_rule.rs`. Trigger 7 applies to `src/store/events.rs` if
  the audit wiring lives there. Trigger 3 applies only if credential, redaction
  or session paths change, and that must be shown from the diff, not assumed.
- Per the QA verdict, excluded identity and store paths need two independent
  review identities, plus operator approval for the head. Neither is recorded
  here.

Operator-owned decisions (fail closed until decided):
- D-a. Standing-service set. Until decided, rows named as standing, plus
  role `pm`, inboxes and master, are refused. No standing list is invented.
- D-b. Attention archivability after outcome reconciliation. Refused in v1.
- D-c. Offline archivability. Refused in v1.
- D-d. PM self-archive. Refused in v1. Any later PM-scoped rule needs its own
  check.
- D-e. Recent-activity window. None proposed.
- D-f. Batch maximum. Suggested 50.
- D-g. Archivability of a stopped agent with an unacknowledged completion
  report. Unresolved. Until decided, apply refuses any alias whose history has
  an unacknowledged routed report. Retention intent for a row is unknown until
  the operator says otherwise, so unknown intent is refused.
- D-h. Retention, purge and age policy for archived history. Out of scope.
  Nothing here purges or ages out.
- D-i. Whether `session end`'s fleet-wide sweep at 3600 should be disabled
  before CAD-804 and P-DEL land. Operational, not decided here.
- D-j. Owner of P-DEL and the timer task check, CAD-804 or a sibling ticket.

## 12. Verification performed and limitations

Performed (static, no execution):
- Source reads at 8522672b for every reference above, with search for every
  `DELETE FROM agents` and `remove_agent`/`timer_gc_remove` caller.
- `git fetch origin`. origin/main is at fedd7964, one later commit (CAD-1220),
  which touches none of the cited source paths.

Not performed, and unverified:
- No build, test, daemon run or isolated-state reproduction. Every behavior
  claim is read from source, not executed.
- `params` secret content is not audited. Export of `params` through full
  `agent_show` is current behavior and is not changed by this design.
- Provider session retention is not verified. Restore is local metadata only.
- Whether any path other than the ones listed writes `state`, `enabled` or
  `endpoint` (the trigger covers them all, but the implementation must confirm
  coverage by test).
- `handoff_unresolved` is not confirmed to appear in any operator UI.
- No measurement of any read-path cost. CAD-1221 owns that.

## Out of scope

Schema, code, GC predicate changes, timer enablement, caller rule, CLI or UI
changes, production or staging actions, archive or cleanup of any agent, any
push, PR or merge, and any approval. Tracked by CAD-804 (GC and P-DEL owner,
pending D-j), CAD-1221 (read cost), and the implementation ticket still to be
created after R1 and R2 are scheduled.
