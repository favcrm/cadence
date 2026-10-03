# Design contract: CAD-1098 per-app-installation assistant threads

Status: draft for Spec/security review. No product code in this PR.
Operator decision (CAD-1098, 2026-10-03): each app installation (CRM, Social
Content, ...) gets its own conversation thread with the master instead of
sharing one. Human trigger 1 (message delivery, scoped-turn context, token
delivery).

## Goals and non-goals
Goals: an app's chat panel shows only that installation's thread; app
turns never leak content, context or tokens into another thread; the board
home master thread stays what it is. Non-goals: per-context threads, new
master powers, changes to who may redeem scoped verbs, a second master agent.

## Data model (recommendation)
- **Thread per installation, not per (install, context).** Context is a
  per-turn hint (`app_hint_envelope`) that changes as the operator moves
  between pages and records; per-context threads would shred one
  conversation into dozens and multiply sessions. Context stays stamped on
  each message and is still re-proved at delivery.
- Schema v32: `threads` gains `scope TEXT NULL` (NULL = home, else the
  install id) and the inline `alias UNIQUE` becomes a unique index on
  `(alias, COALESCE(scope,''))` (table rebuild in one transaction). Entries
  already key by `thread_id`; they stay append-only.
- **One master agent, one queue, many threads.** No new agent rows: the 49
  `is_master` sites, confinement, reserved alias and allowlists stay as they
  are. Rejected: one agent alias per install (N processes, N copies of master
  confinement); one shared session (model context spans every app, so I2 is
  unprovable).
- **A provider session is bound to one thread.** When the next delivered
  message's thread differs from the live session's, the actor ends the
  session and opens a new one on the existing new-session path
  (`continuity::Reason::New`). The pack is built from the target thread
  only (`continuity_entries` by thread id) plus USER.md; an app-thread pack
  carries no plan state. Cost: one bounded pack (`PACK_MAX`) per switch.
  One operator rarely alternates quickly. Side effect: the CAD-1076 per-session
  message count now grows per thread, not across all apps.
- Delivery stays FIFO per alias, so a long Social turn delays a CRM turn
  (accepted; see Out of scope).

## Routing
- `thread_send` with a verified `app` binding lands in `(master, app.install_id)`;
  without one it lands in home. The thread is **derived from the proven
  binding**, never a field. `thread`, `scope`, `install` stay refused by the
  existing allowlist.
- The thread id is fixed in the enqueue transaction with the entry. Every
  later entry for that message (assistant text, tool calls, turn result,
  pack note) goes to the message's thread, not "the alias's thread".
- Daemon-originated messages to master (wakes, routed results, review
  verdicts, setup chatter such as agent joins and permission requests) have no
  binding and stay in home. Board home shows home only, unchanged.
- Reads: `thread_read` / board GET and stream take an optional `install`,
  validated against installed apps; unknown install is a 404 that creates
  nothing. Uninstall archives the install thread read-only (entries kept).

## Invariants
- I1: only a connection provably the operator can send into any master thread;
  an agent pane, endpoint, or detached child is refused exactly as today.
- I2: a message's thread equals the thread of its verified App binding (home if
  none). No path writes a message, entry, hint, token or pack for thread A
  while the session or message belongs to thread B.
- I3: the scoped turn token and message id are bound to that message in its own
  thread. Redeem needs `stored.alias == caller`, running state, the current
  token, **and** the message's thread scope equal to the requested install.
  A turn token never appears in an entry, a pack or another thread.
- I4: delivery re-checks `thread.scope == hint.install_id`; a mismatch drops
  the hint and the token slot (the message still delivers, with no scoped
  authority). Fail closed on authority, not on the message.
- I5: a session serves one thread. A continuity pack for thread T contains
  only T's delivered entries (no other thread, no plans for app threads).
- I6: operator-only actions (approvals, `audit approve`, effect confirmations)
  are decided by the operator connection and do not depend on the thread.
  Moving a message between threads never creates authority.
- I7: the board GET, stream and POST are at least as strict as the daemon RPC:
  same operator proof, same refusal of agent peers and forged fields.
- I8: pre-migration history is never rewritten or duplicated.

## Failure modes
- [ ] Crash between steps: thread row, entry and queue row commit in one
  transaction (I2). The session switch is an in-memory decision re-made on
  the next open; a pack is consumed only with its thread note, so a crash
  re-sends it (I5).
- [ ] Loaded host: a late redeem after the session switched fails the running
  and current-token checks (I3).
- [ ] Wrong caller: agents, detached `setsid` children and unproven connections
  hit the existing caller rule and endpoint-session bind (I1, I3).
- [ ] Concurrent callers: two first sends for one install race the unique
  index and `INSERT OR IGNORE`, so one thread; sends to different threads
  queue FIFO, each persisted in its own thread (I2).
- [ ] Forked or detached child: inherits no thread authority; redeem still
  needs the endpoint session plus a live token (I3).
- [ ] Relay paths: board POST derives scope from the proven binding with the
  same RPC; a path or query `install` is read-only and validated (I7).
- [ ] Forged field: `thread`, `scope`, an unverified `app.install_id`, or a
  retried message id with a different install are refused and leave no thread
  (I2). Retry comparison includes the thread.
- [ ] Partial write: the v32 rebuild runs in one transaction and converges on
  reopen (`IF NOT EXISTS`); an event without its entry cannot occur (I8).
- [ ] Clock/TTL edges: tokens keep today's per-turn scheme; no new TTL.
- [ ] Migration of shared history: every existing entry stays in the home
  thread. App panels start empty and show one line linking to Home history.
  Rejected: moving entries by their `app` stamp (rewrites append-only
  history and the model's continuity) (I8).
- [ ] Rollback: a v31 binary refuses a v32 store (`rollout` crossing gate), so
  downgrade is restore-from-backup and loses app-thread turns. Forward-fix is
  the supported path. The migration goes through the rollout gate with its
  backup.

## UI
- `ChatPane` reads `resources.appThread(installId)` (home keeps
  `masterThread`) and streams `/api/threads/master/stream?install=<id>`.
  Optimistic pending rows write to the same resource.
- Collapse state is per app (`chat-collapsed:<install>`), and the waiting dot
  counts only that thread. Header names the app. Desktop and narrow QA required.

## Adversarial tests (written first; each fails without its guard)
| Test name | Proves | Guard | Fails without the guard because |
|---|---|---|---|
| `scoped_send_lands_only_in_install_thread` | I2 | thread derived from proven binding | entry lands in home as today |
| `thread_send_refuses_thread_scope_install_fields` | I2 | allowlist | field accepted or silently ignored |
| `forged_app_binding_creates_no_thread` | I2 | `thread_app` proof before thread create | empty install thread appears |
| `turn_output_follows_the_messages_thread` | I2 | entry thread from message, not alias | output for A files under B while B is queued |
| `pack_for_thread_contains_no_other_thread` | I5 | session bound to thread, per-thread pack | Social marker string present in CRM pack |
| `thread_switch_opens_new_provider_session` | I5 | switch rule | session reused and sees both threads |
| `redeem_refuses_token_for_other_install_thread` | I3 | thread scope == install | Social token redeems against CRM |
| `delivery_drops_hint_and_slot_on_scope_mismatch` | I4 | delivery recheck | token slot delivered for a mismatched thread |
| `detached_child_cannot_redeem_in_app_thread` | I1,I3 | endpoint-session bind | setsid child redeems |
| `agent_cannot_thread_send_into_app_thread` | I1 | `rpc_thread_send` | agent writes into an operator thread |
| `concurrent_first_sends_make_one_thread` | I2 | unique index | two thread rows or one send lost |
| `retry_same_message_other_install_refused` | I2 | retry comparison incl. thread | message duplicated across threads |
| `board_thread_routes_match_rpc_strictness` | I7 | shared proof; 404 on unknown install | peer or forged install accepted |
| `v32_migration_keeps_history_home_and_is_refused_by_v31` | I8 | rebuild plus rollout gate | history moved, or old binary opens it |
| `crm_panel_shows_only_its_thread` (UI) | UI | per-install resource | Social turns visible in CRM; collapse shared |

## Out of scope
- Per-context threads; warm per-thread sessions to avoid pack re-sends;
  parallel turns across threads (head-of-line blocking stays).
- Narrowing master powers inside app threads (open question).
- Backfilling or moving old history; retention policy for archived threads.
- Operator-approval and caller-identity rework (CAD-411, CAD-814).

## Open questions for the operator
1. Should an app thread's turns keep full master powers (setup: agent
   register, permission requests), or only the scoped verbs? Recommended
   now: unchanged; narrowing is a follow-up.
2. Is head-of-line blocking across apps acceptable until parallel turns exist?
3. Accept "old history stays in Home, app panels start empty"?
