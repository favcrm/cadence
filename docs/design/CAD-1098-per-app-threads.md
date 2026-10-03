# Design contract: CAD-1098 per-app assistant conversations

Status: draft (rev 3: spec REVISE 826a80da applied) for
Spec/security review. No product code in this PR. Human trigger 1 (message
delivery, scoped-turn context, token delivery).

## Goals and non-goals
Goals: each app's chat shows only that app's conversations; app turns never
leak content, context or tokens into another conversation; the board home
master thread is unchanged; app conversations hold no setup/admin power.
Non-goals: a second master agent, new scoped verbs, changes to who may redeem
them, customer/segment subjects, parallel turns.

## Data model
- A **conversation** is scoped by `(app installation, optional subject)`.
  v1 subject is a campaign: `campaign:<id>`. Home (NULL scope) is the existing
  master thread. Company context (page, record, revision) stays a **per-turn
  hint**, re-proved at delivery (`message_app`), never part of the key.
- Schema v32: `threads` gains `install_id`, `subject`, `is_general`,
  `archived`, `title` (all NULL/0 for home). The inline `alias UNIQUE` becomes
  partial unique indexes: home `(alias) WHERE install_id IS NULL`; General
  `(alias, install_id) WHERE is_general=1`; subject `(alias, install_id,
  subject) WHERE subject IS NOT NULL`. "New conversation" adds further
  conversations with neither flag nor subject. Table
  rebuild in one transaction; entries already key by `thread_id` and stay
  append-only. Nothing is ever deleted or moved.
- **One master agent, one queue, many conversations.** Rejected: an agent per
  install (49 `is_master` sites, confinement copies); one shared session
  (model context would span apps, I2 unprovable).
- **Each conversation is its own provider session.** When the next delivered
  message's conversation differs from the live session's, the actor ends the
  session and opens a new one on the new-session path
  (`continuity::Reason::New`). The pack is built from that conversation only
  (`continuity_entries` by thread id) plus USER.md; no plan state in app
  packs. Cost: one bounded pack (`PACK_MAX`) per switch. CAD-1076's per-session
  message count now grows per conversation.
- Subject verification: a campaign subject is accepted only if the campaign
  exists **in the same install and the proven context** (`app_context_proof`
  plus a lookup of the campaign row); the conversation row stores the
  install, subject and context id it was proven under.

## Routing and APIs
- `thread_send` with a verified `app` binding lands in the conversation the
  **server resolves**: the conversation id named by the send is accepted only
  as a selector and checked: it must exist, be unarchived, have
  `install_id == app.install_id`, and (for a campaign) its subject must verify
  against the same install and context as above. No client field decides
  scope, subject or install. Without a binding the send lands in home.
- New daemon RPCs, all operator-proven like `thread_send`, mirrored by board
  routes under the same RouteClass: `conversation_list(install)`,
  `conversation_create(install, subject?)` (idempotent per `(install,
  subject)` for subjects; always new for "New conversation"),
  `conversation_select` is client state only (no daemon write).
  `thread_read`/stream take `conversation` (validated, 404 creates nothing).
- The thread id is fixed in the enqueue transaction with the entry; every
  later entry for that message (assistant text, tools, turn result, pack note)
  goes to the message's conversation, not "the alias's thread".
- Daemon-originated messages to master (wakes, routed results, setup chatter)
  carry no binding and stay in home.
- Conversation `/new` and `/clear` and the "New conversation" button are the
  same call: `conversation_create(install)`. The old one stays listed.

## Invariants
- I1: only a connection provably the operator sends into or creates any
  conversation; an agent pane, endpoint or detached child is refused.
- I2: a message's conversation equals the one the server resolved from its
  verified binding (home if none). No path writes a message, entry, hint,
  token or pack for conversation A while the message or session is B's.
- I3: the scoped turn token and message id are bound to that message in its own
  conversation. Redeem needs `stored.alias == caller`, running state, the
  current token, **and** the message's conversation `install_id` equal to the
  requested install. A token never appears in an entry, pack or other
  conversation.
- I4: delivery re-checks `conversation.install_id == hint.install_id`; on a
  mismatch the hint and token slot are dropped (message still delivers
  unscoped).
- I5 (subject binding): a campaign conversation serves only its own campaign
  in its own install and context. Its hint's context must verify against the
  conversation's stored context; a scoped verb in it naming another campaign,
  install or company is refused.
- I6 (scoped powers, two gates). A master turn whose running message is in an
  app conversation has only scoped powers. `master_policy` alone cannot give
  that: it sees daemon RPCs from the master's process tree, not (a) approved
  commands run by the daemon with a grant token as operator evidence, (b) local
  tracker writes (`cadence issue new`, `cadence report file` write the PM dir
  directly), (c) local home reads (`issue ls/show/log`, epic/project, `plan
  ls/show`), or (d) the static Pi guard rules and Claude `--allowedTools`.
  So:
  - **Gate 1, per-session tool profile (primary).** The session is already
    per conversation (I7). The adapter chooses its rule set at session open
    from the conversation scope: an **app profile** generates the Claude
    `--allowedTools` and the Pi guard RULES from only the nine scoped `cadence
    app ...` verbs (`scoped_verb_stems()`) plus `cadence daemon health`; no
    `issue`, `plan`, `report`, `wiki`, `agent`, `master`, `overview`, `status`,
    `thread show` or `project` verb. The app session's confinement also drops
    read access to the PM dir and wiki where Landlock is available. The profile
    is fixed at launch from the daemon-held conversation row, never from the
    prompt or a param; a Home session keeps today's rules.
  - **Gate 2, `master_policy` (daemon).** After `caller_is_master`, resolve the
    master's running message from the store (never a param) and its
    conversation; an app-scoped one may call only `MASTER_APP_ALLOWED`,
    enumerated: the nine scoped RPCs (`app_record_csv_assistant_import`,
    `app_record_csv_assistant_preview`, `app_segment_assistant_save`,
    `app_segment_assistant_list`, `app_segment_assistant_show`,
    `app_segment_assistant_preview`, `app_content_assistant_draft`,
    `app_content_assistant_proposals`, `app_content_assistant_proposal_show`),
    `health`, `message_report` (own running message only, needed to finish a
    turn) and `thread_read` **only for the running message's own conversation**
    (a different or absent conversation is refused). Everything else is
    refused, in particular `master_ask_permission`, `master_peek_grant`,
    `master_permission_use` (so `run_approved` never starts from an app
    conversation, standing `always` rules included), `agent_*`, `job_*`,
    `task_show`, `monitor_*`, `delivery_list`, `wiki_*` (incl. `wiki_write`),
    `project_new`, `plan_propose`, `master_dispatch`, `question_escalate`,
    `interrupt`, `answer_route`. An unresolvable running message fails closed
    to this set.
  - Setup/admin work (registering or joining agents) is not a master RPC
    (`agent_register` is not in `MASTER_ALLOWED`); it is reachable only through
    an approved command (a grant). Gate 1 and Gate 2 together keep grants out
    of app conversations; a grant already issued in Home is not usable there
    because `master_permission_use` is refused and the app profile lacks the
    CLI verb.
  - CLI belt: the session sets `CADENCE_CONVERSATION=app` and the CLI-side
    master checks (`issue new`, `report file`, local reads) refuse when it is
    set. This is defense in depth only; the control is Gate 1.
- I11 (no spill across conversations): the master's tmp dir is per
  conversation (`master/tmp/<conversation id>`), or cleared on a conversation
  switch, so spilled tool output and the Pi read tool cannot reach another
  conversation's files.
- I12 (no persistent leak): app turns cannot write the wiki (including
  `agents/master/knowledge/`); USER.md reaches app packs read-only and holds no
  home or plan content; the app profile has no tool that writes it.
- I7: a session serves one conversation; a pack contains only that
  conversation's delivered entries (so `/new` carries no prior content).
- I8: operator-only actions (approvals, `audit approve`, effect
  confirmations) do not depend on the conversation; moving nothing creates
  authority.
- I9: board GET, stream and POST are at least as strict as the daemon RPC.
- I10: pre-migration history is never rewritten, copied or moved.

## Failure modes
- [ ] Crash between steps: conversation row, entry and queue row commit in one
  transaction (I2). Session switch is an in-memory decision re-made on
  the next open; a pack is consumed with its thread note, so a crash re-sends
  it (I7).
- [ ] Loaded host: a late redeem after the session switched fails the running
  and current-token checks (I3). Queued messages show the visible
  "Assistant is finishing another task - your message is queued" state.
- [ ] Wrong caller: agents, detached `setsid` children and unproven connections
  hit the existing caller rule and endpoint-session bind (I1, I3).
- [ ] Concurrent callers: two first sends to one campaign (or two first-time
  General creates) race the partial unique indexes plus `INSERT OR IGNORE`,
  yielding one row; sends to different conversations queue FIFO (I2).
- [ ] Forked or detached child: inherits no conversation authority (I3).
- [ ] Relay paths: board routes call the same RPCs; a path or query
  conversation id is only a selector validated as above (I9).
- [ ] Forged field: client `thread`, `scope`, `subject`, an unverified
  `app.install_id`, a conversation of another install, or a campaign not in
  this context are refused and leave no row; retry of one message id with a
  different conversation is refused (I2, I5).
- [ ] Partial write: the v32 rebuild is one transaction and converges on
  reopen; no event without its entry (I10).
- [ ] Clock/TTL edges: token scheme unchanged; no new TTL.
- [ ] Migration of shared history: every existing entry stays in home, linked
  from app panels ("Earlier history is in Home"). Not moved (I10).
- [ ] Rollback: a v31 binary refuses a v32 store (`rollout` crossing gate), so
  downgrade is restore-from-backup and loses app conversations. Forward-fix is
  the supported path; the migration goes through the rollout gate and backup.
- [ ] Retention: archived conversations are kept indefinitely, read-only
  (sends refused). Uninstall archives them.

## UI
- Chat header picker: `General v`, the install's campaign conversations, and
  `+ New`. Selection is per app (`chat-conv:<install>` plus collapse state
  `chat-collapsed:<install>`, per app).
- The campaign page auto-selects its campaign conversation (switchable to
  General). The new-campaign "assistant draft" calls
  `conversation_create(install, campaign:<id>)`, selects it, and sends the
  brief there.
- Composer `/new` and `/clear` = "New conversation": start fresh, old stays
  listed. Waiting dot counts only the selected app's selected conversation.
- A queued-behind-another-task notice when the master has a running message
  elsewhere. Desktop and narrow browser QA required.

## Adversarial tests (written first; each fails without its guard)
| Test name | Proves | Guard | Fails without the guard because |
|---|---|---|---|
| `scoped_send_lands_only_in_its_conversation` | I2 | server-resolved conversation | entry lands in home as today |
| `thread_send_refuses_client_scope_subject_fields` | I2 | allowlist | field accepted or ignored |
| `forged_app_binding_creates_no_conversation` | I2,I5 | proof before create | empty row appears |
| `conversation_of_other_install_refused` | I2 | `install_id == app.install_id` | CRM sends into a Social conversation |
| `campaign_conversation_not_usable_for_other_campaign_or_company` | I5 | subject/context verification | campaign A's conversation serves B or another company |
| `campaign_subject_must_exist_in_context` | I5 | campaign lookup in proven context | conversation made for a nonexistent or foreign campaign |
| `turn_output_follows_the_messages_conversation` | I2 | entry thread from message | output files under the wrong conversation |
| `new_clear_pack_has_no_prior_conversation_content` | I7 | per-conversation pack | marker string from the old conversation is in the new pack |
| `conversation_switch_opens_new_provider_session` | I7 | switch rule | session reused, sees both |
| `redeem_refuses_token_for_other_install_conversation` | I3 | install check | Social token redeems against CRM |
| `delivery_drops_hint_and_slot_on_scope_mismatch` | I4 | delivery recheck | slot delivered for a mismatch |
| `admin_rpcs_refused_in_app_conversation` | I6 | Gate 2 narrowed allowlist | `plan_propose`, `project_new`, `master_dispatch` succeed from CRM chat |
| `permission_use_and_standing_rule_refused_in_app_conversation` | I6 | Gate 2 excludes `master_ask_permission`/`peek_grant`/`permission_use` | a standing `always` rule runs an approved command (e.g. `agent register`) with operator authority from an app turn |
| `issue_new_and_report_file_refused_in_app_conversation` | I6 | Gate 1 app profile (Claude allowedTools and Pi guard) | the turn writes the tracker via `issue new` / `report file` |
| `tracker_and_plan_reads_refused_in_app_conversation` | I6 | Gate 1 app profile; PM dir unreadable | `issue show` / `plan show` pulls home plan content into the app turn |
| `app_profile_generated_per_session_from_conversation_row` | I6 | profile chosen at open from the stored scope | an app session launches with the Home rule set, or a prompt/param picks the profile |
| `thread_read_limited_to_own_conversation` | I6 | Gate 2 conversation check | an app turn reads home or another app's conversation |
| `unresolvable_running_message_fails_closed_to_app_allowlist` | I6 | fail-closed | admin call passes with no resolvable conversation |
| `master_allowlist_exhaustive_per_scope` | I6 | table test over every daemon method, pins `MASTER_APP_ALLOWED` | a new method is silently open to app turns |
| `wiki_write_refused_in_app_conversation` | I12 | Gate 1 and 2 exclude `wiki_*` | customer data persists into master knowledge read by home turns |
| `master_tmp_not_shared_across_conversations` | I11 | per-conversation tmp / clear on switch | a CRM turn reads a home turn's spilled `issue show` output |
| `detached_child_cannot_redeem_in_app_conversation` | I1,I3 | endpoint-session bind | setsid child redeems |
| `agent_cannot_send_or_create_conversation` | I1 | `rpc_thread_send` and create proof | agent writes into an operator thread |
| `concurrent_first_send_to_campaign_makes_one_conversation` | I2 | partial unique index | two rows or a lost send |
| `retry_same_message_other_conversation_refused` | I2 | retry comparison | message duplicated across conversations |
| `board_conversation_routes_match_rpc_strictness` | I9 | shared proof; 404 unknown | peer or forged id accepted |
| `v32_migration_keeps_history_home_and_is_refused_by_v31` | I10 | rebuild plus rollout gate | history moved or old binary opens it |
| `archived_conversation_is_read_only` | I2 | archived check | send lands in archived thread |
| `crm_panel_shows_only_selected_conversation` (UI) | UI | per-conversation resource | Social turns visible in CRM; collapse shared |

## Out of scope
- Customer and segment subjects; archive and rename UI; parallel turns across
  conversations (head-of-line blocking is accepted); warm per-conversation
  sessions to avoid pack re-sends.
- Moving or backfilling old history.
- Operator-approval and caller-identity rework (CAD-411, CAD-814).
