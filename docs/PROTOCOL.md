# Cadence wire protocol — v1

The control API is newline-delimited JSON over a Unix-domain socket at
`<state_dir>/cadence.sock`. Each line is one request; the daemon answers
with one line per request on the same connection. Multiple requests may
share a connection.

State directory: `$CADENCE_STATE_DIR`, else `$XDG_STATE_HOME/cadence`,
else `~/.local/state/cadence` (mode 0700). The daemon accepts only
same-UID peers (`SO_PEERCRED`). This establishes same-user access; it is
not a hostile same-user isolation boundary.

One daemon owns a state directory: `serve` takes an exclusive `flock` on
`<state_dir>/cadence.lock` before touching the store or socket and holds
it for the process lifetime. A second start fails `rejected` without
running recovery; `daemon start` reports `already_running` with the
existing daemon's health.

## Frames

Request: `{"method": "<name>", "params": {...}}`

Response: `{"ok": true, "result": {...}}` or
`{"ok": false, "error": {"kind": "rejected|provider|unknown|internal|conflict", "message": "...", "code"?: "...", "revision"?: n}}`

Error kinds:

- `rejected` — invalid or disallowed request; nothing was attempted. Structured rejections also carry `code`.
- `conflict` — a revision check failed. `code` is `revision_conflict` and `revision` is the current stored revision. Nothing was written.
- `provider` — the provider explicitly rejected the request.
- `unknown` — transport failed after the request may have reached the
  provider. The attempt is preserved for review; never retried blindly.
- `internal` — local runtime failure (I/O, storage, protocol).

## Methods

| Method | Params | Result |
|---|---|---|
| `health` | — | `{state:"ready", protocol:1, capabilities:[...], agent_gc_timer:{enabled, older_than_secs, ...}, agent_auto_stop:{enabled, idle_secs, by_provider, last_stopped, last_kept, ...}, child_subreaper}` — `child_subreaper` is true when the daemon process is the child subreaper of what it launches (`daemon run`, CAD-308); `adopted_live` counts live children the daemon did not spawn (adopted orphans still running), `adopted_oldest` lists the oldest five as `{pid, comm, age_secs}`, `adopted_reaped_total` counts adopted children reaped since start — all zero when the reaper is not enabled |
| `shutdown` | — | `{state:"stopping"}`; daemon stops actors (bounded), writes the clean-stop marker, then exits. Caller rule (CAD-384): the proven operator, or the agent holding the live rollout lease under a live operator grant (`rollout_grant`; the rollout owner's `daemon restart` from its own pane); in a sandbox, also a caller tied to none of its agents. Every other agent, and a detached child of one, is refused. The rule prevents mistaken or misattributed stops; it is not a security boundary against a hostile same-uid agent, which can write the state database or kill the daemon directly (CAD-280). A `rollout claim --as <identity>` must itself pass operator proof |
| `rollout_grant` | `agent, until_secs?` | `{granted, by, granted_at, expires_at}` — CAD-384: lets `agent` claim the rollout lease (an agent's `rollout claim` is refused without one) and, while it holds the lease, stop the daemon from its own pane. Supersedes the agent's live grant; records a `rollout_grant` event. Operator only (`operator_connection`). CLI `cadence rollout grant <alias> [--until <ttl>]` |
| `rollout_revoke` | `agent` | `{revoked, by, at}` — ends `agent`'s live grant (refused when it holds none); records `rollout_revoke`. Operator only. CLI `cadence rollout revoke <alias>` |
| `agent_register` | `alias, provider, cwd?, endpoint_kind?, role?, sandbox?, instructions?, params?, team_role?, model_policy?` | `{alias,state:"starting"|"idle",provider}`. `team_role` is model-lookup metadata (`ops` normalizes to `devops`); it does not change runtime `role`. `model_policy` is `inherit` (default) or `provider_default`. Caller rule (CAD-149): a connection attributed to an agent (a registered pane or enrolled endpoint on its ancestry) may register only its own member — `params.upstream` naming the caller — and only when the caller is a group root (no upstream); anything else from an agent is refused, naming the rule (so a worker can mint no agent, and no one becomes a PM by naming itself). A connection attributed to no agent registers only on positive operator proof (`peer::operator_proof`, CAD-431): a worker's detached (`setsid` + fork) child derives no agent identity and is refused, so it cannot mint a root agent outside its group — one the review loop would take as an independent reviewer. The operator's own shell passes (see the residual under `agent_set`). An existing alias gets the duplicate refusal before any caller check |
| `agent_list` | — | `{agents:[Agent+tasks+capabilities]}` — `tasks` names the alias's non-terminal task assignments; `capabilities` is the registry descriptor |
| `agent_show` | `alias` | `{agent, messages, event_cursor, queued, unknown, inbox?}` — `unknown` counts unreconciled unknowns fencing the agent; `agent.awaiting_report` (also on `agent_list` rows) is `{message, turn_id, task_id, since_secs, acked, report_timeout_secs, remaining_secs, count, queued_behind}` while a delivered pty turn awaits its report (null otherwise; `remaining_secs` null when the bound is disabled; `turn_id`, here and on message rows, is `null` unless the connection is that agent's own pane or endpoint — CAD-375), and that message's row carries `awaiting_report: true`; passive `inbox` evidence includes queued count, oldest age, last receipt/progress, and `semantic_completion:"external_consumer_required"`; it is never a drain or completion claim. `agent.capabilities` is the registry descriptor; `agent.model_reported` is the model the provider reports running (claude: the stream's `system/init` model) beside `model_configured`, `model_effective`, `model_source` (`configured` or `provider default`), `effort_configured`, `effort_reported`, `effort_effective` and legacy `effort` — also on every `agent_list` row. `team_role`, `model_lookup_role`, and `model_selection` (`source`, `lookup_role`, `revision`, `model`) record how a launch model was chosen. Unsupported endpoints leave `model_selection` null. Existing rows without stored provenance are labeled `legacy_configured` or `legacy_provider_default` at read time |
| `model_defaults_get` | — | `{revision, config, providers, roles}` — daemon-wide provider baselines and team-role overrides. Suggestions are previously observed model ids, not a catalog. Does not start provider processes |
| `model_defaults_set` | `document` (raw JSON string `{expected_revision, config}`) | the same snapshot as get, after an atomic revision bump. Mismatched `expected_revision` is `kind:"conflict"`, `code:"revision_conflict"`, with `revision` set to the current value and no write. **Operator only** (CAD-337), the connection-bound gate `slot_reconcile` and `approval_record` use: a pane or managed endpoint, or any caller not provably the operator (`peer::operator_proof`), is refused naming the rule; identity-shaped fields (`attribution`, `by`, `actor`, …) are refused, not read. The `model_defaults_updated` event records the verified caller: attribution `operator`, transport `operator-connection` |
| `agent_send` | `alias, text, message?, reply_to?, source?, task?, nudge?` | `{message,state,duplicate,warning?}` — `task` attaches the delivery to a task for indexing; `nudge: true` (CLI `send --nudge` / `message send --nudge`, pty only, no `reply_to`, not with `--ready`) records a turnless `source: "nudge"` delivery — see the CAD-250 section; `warning` names a stale inbox target (queued anyway — see inbox endpoints) |
| `agent_ask` | `alias, text, message?, reply_to?, wait?` | the `Message` row; state may be non-terminal if `wait` expired |
| `agent_events` | `alias, after?, wait(<=30), tail?` | `{events:[Event], cursor, has_older}` — `tail:true` returns the newest page (50) in ascending order instead of paging forward from `after` |
| `thread_read` | `alias, after?, limit?(1-500, default 100), wait?(<=30)` — or backwards: `tail?:true` / `before?(seq>=1)`, `limit?` (CAD-328) | `{alias, thread:{id,alias,created,updated}\|null, entries:[{seq,thread,role,kind,text,payload,message,created}], cursor}` — the agent's durable chat (CAD-319), oldest first after `after`. `tail` reads the newest `limit` entries and `before` the `limit` entries below a seq, still oldest first, plus `more_before` (older entries remain); a backward read never waits and refuses `after`/`wait`. Removing the agent archives its thread (rows kept by thread id, a `system` entry marks the removal) and a new agent under the reused alias starts a fresh one (CAD-304 S4); archived threads are not readable by alias in the MVP. Reads are unscoped in the MVP — any local caller can read any alias's thread, the same as `agent_show`/`agent_events`. Roles `operator\|agent\|system`; kinds `message\|assistant_text\|tool_call\|tool_result\|turn_result`. Text and payload strings are secret-redacted before they are stored; tool calls and tool results are a one-line redacted summary (≤160 chars; a result adds `payload.is_error`), never the raw input or output. `assistant_text` is intermediate prose only (managed Claude text blocks, Codex commentary items, `payload.phase: "commentary"`); the final answer is stored once, as the `turn_result` (CAD-320). Codex `final_answer` and unphased items are held until the message finishes and kept as `assistant_text` only when the result does not carry them — an `unknown` or failed turn loses none of its text. A live turn token quoted in prose is not scrubbed on write — the secret scan does not know it — and is redacted at `export`; a read by anyone but the owning agent's connection masks it (CAD-375) |
| `thread_send` | `alias, text, message?` | `agent_send`'s receipt plus `thread` — starts the alias's thread on first use, inside the enqueue transaction (a refused message starts none), and queues the text as an `operator` entry. Refused for a connection the daemon attributes to an agent (pane or enrolled managed endpoint), for an underivable caller, and for one tied to no agent that is not provably the operator (`peer::operator_proof`, CAD-384) — a detached child of an agent. Any other field is refused. Once a thread exists, every message queued to the alias is recorded too: `operator` when `agent_send`'s connection is tied to no agent — which then must be provably the operator, or the send is refused (CAD-384) — `system` (payload `from`/`source`) otherwise |
| `agent_requests` | `alias` | `{requests:[{request,method,params}]}` |
| `agent_respond` | `alias, request, decision?|answers?, reason?` | `{state:"answered"}` — `reason` rides a brokered decline as the provider's denial message. Caller rule (CAD-370): the proven operator or the requester's own PM; the requesting agent never answers its own request, and a refused answer leaves it pending (see Caller rules) |
| `request_open` | `alias, kind?, tool, input_summary?, input?, request?` | `{request,state:"waiting_input",existing?}` — registers a brokered request (caller-named `request` dedupes retries); `rejected` unless the agent's params carry `broker_approvals`. **Caller rule (CAD-376), from the connection only** (the nearest registered pane or strictly verified enrolled endpoint on the peer's ancestry — never `alias`, never `CADENCE_ALIAS`): only the owning agent's own connection — in practice its `mcp-permission` server, a child of the brokered provider — may call it; another agent, a connection with no agent identity (the operator included: it answers with `agent_respond`) and a request carrying `by`/`as`/`actor`/`caller`/`operator`/`reviewer`/`pane`/`lane`/`pid` are refused naming the rule, before anything is recorded — a refused open parks nothing and notifies no one |
| `request_wait` | `request, wait?(<=120)` | `{state:"waiting"}` on slice expiry, `{state:"answered",answer}` after `agent_respond`, `{state:"closed",reason}` once the handle is gone (actor exit, daemon restart, `request_close`). Waiting consumes the parked answer, so the same caller rule applies: another agent's wait is refused and the answer stays parked for the owner |
| `request_close` | `request` | `{state:"closed"}` or `{state:"answered",answer}` — retires the handle a waiter abandoned (local deadline); a boundary-parked answer still lands. Same caller rule: another agent's close is refused and the handle stays pending (or its answer parked) |
| `agent_ready` | `alias, by?, force?` | `{state:"ready-claimed"}` — single-use readiness claim for `pty`; probes the pane first and refuses a visibly busy one unless `force`; `by` records the claimer |
| `agent_capture` | `alias` | `{capture}` — current pane contents (pty) |
| `agent_probe` | `alias` | `{probe:{idle,reason,...}}` — analyzed pane state without claiming (pty) |
| `agent_answer` | `alias, choice, by?, note?` | `{state:"answered"}` — sends one menu-choice keystroke to a `pty` pane probing `approval_menu` (CLI: `cadence agent answer <alias> <choice> [--reason <text>]`); re-probes and refuses any other detected pane state. Detection uses terminal text: CAD-220 tracks the residual ambiguity of a quoted menu directly adjoining the busy frame, so this is not an authoritative provider approval signal. `choice` is the option's index in the whole printed option block (top to bottom, independent of the highlighted row); the profile's keymap turns it into tmux keys — numbered menus take the digit, hotkeyed options their suffix, unnumbered selects arrows + Enter relative to the highlight. The answerer is derived from the socket peer's pid — `/proc` ancestry into the pane roots plus the pane `CADENCE_ALIAS` env and the pane's pty fds (a `setsid` detach keeps both) — a caller inside or tied to the target's own pane is refused, inside another agent's pane stamps `by_kind:"agent"`, an ancestry walk that cannot complete refuses while the target pane is alive (never a derivation-failure `operator`), and a caller that matches no pane is `operator` only when it holds a terminal no pane owns — a fully detached caller is honestly `unknown`; a supplied `by` that disagrees is kept only as `claimed_by`. Records `approval_answered` with `by`/`by_kind`/`caller_pid`/`choice`/`line`/`note` and wakes the agent's delivery loop |
| `agent_set` | `alias, patch, next_launch?` | merges an allowlisted param into the live agent — `auto_ready` (`"verified"` or null-removal, pty only), `stall_secs`, `silent_end_secs` (pty only), `report_timeout_secs` (pty only); `{state:"updated"}`. With `next_launch: true` it instead stores launch params `model`/`effort` (claude; Codex model/effort are checked against `model/list`; null clears to the provider default) or `approval_policy` (codex; `never|on-request|on-failure|untrusted`, null clears) for the next open without touching the live process; `{state:"updated", applies:"next launch"}` Caller rule (CAD-149), from the connection only (the nearest registered pane or enrolled endpoint on the peer's ancestry, else `peer::operator_proof`): the operator and the target's own PM (`params.upstream`) may set any allowed key; the agent itself only `next_launch` model/effort (`registry::ParamClass`); anyone else is refused, naming the rule. A request carrying `by`/`as`/`actor`/`caller`/`operator`/`reviewer`/`pane`/`lane`/`pid` is refused. Every accepted change records `params_updated` with `by`, `by_kind`, `target`, `next_launch` and `changes:[{key,old,new}]` **Residual (F1, CAD-280):** "operator" means only that `peer::operator_proof` passed, and a same-uid process that detaches from every pane (`setsid -f env -i … </dev/null >/dev/null`) passes it — any worker can do that. So the rule stops an agent acting from its own pane, not a hostile one, and an audit `by:"operator"` is not proof the operator acted; CAD-280 (operator by positive proof) tightens this. PM authority is bound to the PM's registration: the `upstream` alias counts only while its current registration predates the target, so a removed PM's alias registered again governs none of the old members |
| `agent_inbox` | `alias, after?, wait?` | drains queued inbox messages, completing each `via=inbox_read`; `{messages, cursor}` |
| `message_report` | `message, token, kind: ack|result, text?, sha?` | `{state:"reported"}` — explicit ack/result; `sha` names the produced commit for task-attached kickoffs. The token must equal the message's `turn_id` and be current for the agent's live generation under its endpoint's own scheme (CAD-162: pty `pty-<gen>-…`, managed Claude `claude-<gen>-…` — `ack` only, its turn result completes the message; codex, devin cloud, fake and inbox have no checkable scheme and refuse every report) |
| `message_reconcile` | `message, status: interrupted|completed|failed, note?, sha?` | `{state:"reconciled", message}` — operator-only exit from `unknown`, by the connection (CAD-374): an agent is refused and `by` is refused, the record says `operator`; no turn token. `completed`/`failed` route `reply_to` as a result; `interrupted` routes an informational notice. A `sha` on `completed` binds like a worker `--sha` |
| `message_cancel` | `message, by?, reason?` | `{state:"cancelled", message}` — terminal exit from `queued`; never delivered. Refused for any other state (the error names it) and for task-bound deliveries (`task cancel` owns those). Caller rule (CAD-384): the proven operator or the recipient's own PM; `by` is the caller |
| `interrupt` | `alias, wait?` | `{alias, interrupted, message, turn_id?, state, reason?}` — CAD-323: stops the running turn with the provider's own interrupt, never a kill: managed Claude gets the stream-json `control_request` `{subtype:"interrupt"}` on stdin, written non-blocking (SIGINT when stdin is gone, full or busy), aimed only at the adapter's active turn, Codex `turn/interrupt` on exactly the running turn id (sent only while it is the active turn), a pty pane its profile's interrupt keys (Claude `Escape`, Devin `Escape Escape`, others `C-c`); Devin cloud and fake endpoints refuse. A managed turn's own result then finishes the message `interrupted` (held text flushed, partial `tool_result`s recorded — Codex tool items included); a pane has no result wire, so under the pane's paste lock the daemon's guarded `interrupted` finish runs first and the keys go only if it won and the turn is still the pane's latest turn-holding paste — a turn that already ended is never stopped in its successor's place, and a report that lands first wins. The agent stays up and idle; nothing is requeued or replayed. `wait` (default 30, max 120 s) bounds the wait for the message to settle; `state` is its state then. No running turn, or one that ended before the interrupt reached it, is a no-op (`interrupted:false`, `reason`). Every call that passes the caller rule records `interrupt_requested` `{outcome: delivered|noop|refused, message, turn_id?, by, by_kind, error?}`. Caller rule: `agent_set`'s policy with a trust-bearing change (`peer::may_mutate_agent`, `Controlled`) — the operator or the target's own PM (its dispatcher); the master (CAD-339) only for a running turn the daemon recorded as its own real dispatch (`master_dispatched`, never written for a duplicate) that also routes to it (`reply_to: master`); a peer, another group's PM and the agent itself are refused, as are identity-shaped request fields. `cadence interrupt <alias> [--wait N]` |
| `approval_record` | `id?, source, head, repo, pr, action?` | `{state:"recorded", duplicate, approval_id, source, action, head_sha, scope:{repo,pr}, recorded_via}` — operator approval evidence for `cadence audit` (CAD-217); grants nothing. Refused from any connection that descends from a registered pane or enrolled endpoint or is not provably the operator (the `slot_reconcile` rule, `peer::operator_proof`) and when the request carries `by`/`operator`/`actor`/`alias`/`lane`/`pid`/`pane`/`recorded_via`. `head` must be the full SHA; an identical retry of a live approval answers `duplicate:true`; the same `id` naming other evidence, or a revoked `id`, is refused; with no `id` the daemon picks `<action>-pr<N>-<head[..12]>`, counting up (`-2`, …) past revoked or different records. `cadence audit approve`; see docs/AUDIT.md |
| `approval_revoke` | `id, source, reason` | `{state:"revoked", duplicate, approval_id, source, reason, recorded_via}` — same operator rule; must name a recorded approval. Cancelling a message never revokes. `cadence audit revoke` |
| `project_new` | `key, repo, prefix?, goal?, agents?: ["pm=1", …], issue?` | `{project, prefix, path, repo, remote, manifest, agents, seeded, changed:true, committed:true, actor}`, or `{…, changed:false, committed:false}` when the key already names this repo and PROJECT.md exists — `cadence project new` (CAD-358): registers the repo in `<pm>/<key>/project.yaml` (existing keys only) and seeds `PROJECT.md` (goal, `agents:`, default stages, `milestones: []`) in one tracker commit (`Issue:` when `issue` is given, `Actor:`). `repo` is an absolute path in a git checkout. Refused with nothing written: a different repo for the key, a repo another project owns, the tracker or the daemon state dir (a repo inside either, one containing either, or a symlink to either), the key `agents`, an invalid key/prefix, a prefix in use, a non-git path. **The operator or the master** (see the authority table): any other pane or managed endpoint, a detached child of any agent, and identity-shaped fields are refused |
| `job_new` | `pm, spec, spec_sha256, job?, title?, issue?, repo?, base_ref?, max_revisions?, task_title?` | `{job, duplicate}` — bookkeeping only; creates the `open` job + default `<job>-t1` draft task |
| `job_list` | `state?, all?` | `{jobs:[Job+task counts]}` |
| `job_show` | `job` | `{job:{...,tasks:[Task+kickoff+attention+latest_verdict]}}` — lazily flags drift |
| `job_events` | `job, after?, limit?, tail?` | `{events:[Event], cursor, has_older}` — the `job_id`-scoped view; `tail` matches `agent_events` |
| `task_new` | `job, task?, title?, assignee?, spec?, acceptance?, worktree?, branch?, base_sha?` | `{task}` — draft task in an open job |
| `task_show` | `task` | `{task:{...,messages,verdicts}}` |
| `task_dispatch` | `task, to?, message?, by?` | `{task, message, duplicate, queued_behind_dead}` — enqueues the kickoff at a new revision, or returns the live kickoff (`duplicate:true`) |
| `task_verdict` | `task, sha, verdict: pass|revise|blocked, evidence?, message?, revision?` | `{task, verdict}` — binds `sha == head_sha` on `state=review`. The reviewer is the verified caller (CAD-372): an agent's alias or `operator`; `reviewer`/`pane` are refused |
| `task_accept` | `task, merged_sha?, by?` | `{task}` — `verified → done` |
| `task_sha` | `task, sha, by?` | `{task}` — repairs a NULL `head_sha` on a `review` task |
| `task_fail` | `task, reason, by?` | `{task}` — mark unrecoverable |
| `task_reopen` | `task` | `{task}` — `blocked|verified|failed → draft`, `revision` resets. The proven operator or the job's own PM, by the connection (CAD-373); the assignee and other agents are refused |
| `task_cancel` | `task, by?` | `{task}` — cancels the task; a `queued`/`submitting` kickoff cancels in the same tx, a `running` one completes on its own |
| `memory_propose` | `project, kind, scope?, source?, confidence?, text? or from?, id?` | `{project,slug,status:"proposed",digest,quorum}` — proposer identity is derived from the Unix peer's one live agent endpoint (owned pty pane or enrolled managed endpoint, CAD-381); request aliases and operator fallbacks are refused |
| `memory_review` | `project?, slug, operation: accept|verify, verdict: pass|revise, evidence, digest` | `{project,slug,operation,cycle,digest,quorum}` — only a distinct non-author native PM or worker endpoint may submit one receipt for the exact digest; the operation/cycle and registration incarnation are durable |
| `memory_finalize` | `project?, slug, operation: accept|verify|reject` plus `digest` for accept/verify; `old,new` for supersede | `accept/verify → {project,slug,status,operation,cycle,digest,quorum,finalized,committed}`; `reject → {project,slug,status:"rejected",digest,committed}`. All lifecycle decisions require a native PM endpoint; accept/verify persist a finalization receipt and consume that cycle, so retrieval waits for PM finalization and later verify starts a new cycle. Supersede refuses until crash-atomic pair recovery exists; no body edit is accepted |
| `monitor_register` | `monitor, project, tasks[], interval_secs?, owner?, dispatch_enabled?, auto_dispatch_enabled?` | `{monitor, duplicate}` — registers an explicit task coverage set; the first check is `degraded`, then `active`; automatic dispatch requires both explicit bits and preserves all dispatch guards; delivery stays separately `unconfigured` |
| `monitor_list` | — | `{monitors:[Monitor]}` — durable observer registrations and explicit coverage |
| `monitor_show` | `monitor` | `{monitor:Monitor}` — heartbeat, last successful check, durable event cursor, coverage and alert counts |
| `monitor_heartbeat` | `monitor` | `{monitor:Monitor}` — records the caller's monitor heartbeat; it is not a worker-health claim |
| `monitor_alerts` | `monitor, after?, open?, limit?` | `{monitor, alerts:[MonitorAlert], cursor}` — reads local durable alerts |
| `monitor_alert_ack` | `monitor, alert, by?` | `{alert:MonitorAlert}` — acknowledges one local alert; history remains durable |
| `monitor_stop` | `monitor` | `{monitor:Monitor}` — turns one registration off without deleting coverage or alert history. Operator only, by the connection (CAD-373) |
| `monitor_dispatch` | `monitor, task` | `{monitor, task, message, duplicate, queued_behind_dead}` — explicit operator handoff (by the connection, CAD-373) through the existing guarded job-dispatch transaction; automatic reconciliation calls the same internal guard only for a registration with `auto_dispatch_enabled` |
| `job_cancel` | `job, by?` | `{job}` — cancels the job + every non-terminal task |
| `job_close` | `job, by?` | `{job}` — legal only when every task is `done` |
| `agent_unfence` | `alias, status?, note?, resume?` | operator only, by the connection (CAD-374) — a PM cannot unfence its worker and escalates to the operator; reconciles every `unknown` on the agent (default `interrupted`); `{alias, reconciled:[id], resumed, pane?, state, error?}` — `resume:true` also starts the actor and waits (bounded ~30s) for its open; `pane` is pty-only: `adopted`/`respawned`/`none` |
| `agent_stop` | `alias` | `{alias,state:"stopped"|"attention"}`. Caller rule (CAD-384): the proven operator, the agent's own PM, or the agent itself |
| `agent_resume` | `alias` | `{alias,state:"starting"|"attention"}`. Same caller rule as `agent_stop` |
| `agent_remove` | `alias, force?` | deletes the agent + the history no job references; refuses live endpoints and open work unless `force` (queued/submitting cancelled, running finished `interrupted`, each through the normal finish path so `reply_to` is notified; `agent_remove_forced` names them and who was `notified` — only recipients a notice actually reached); non-terminal tasks assigned to the alias are unassigned in the same transaction (state, revision and history kept; `task_unassigned` job event and a `job_event` to the job's PM naming `job dispatch <task> --to <worker>`; listed as `unassigned`), so a later agent under the alias inherits no task; an `unknown` message refuses even `force`. Only the operator or the agent's own PM (the `agent_set` caller rule, CAD-304); records `agent_removed` with `by`/`by_kind` on the `daemon` stream |
| `agent_gc` | `older_than?` | sweeps dead agents; `{removed:[alias], not_permitted?:[alias]}` — each candidate passes the `agent_remove` caller rule (the operator sweeps all, a PM its own members) |
| `agent_gc_plan` | `older_than?` | read-only: `{candidates:[alias], not_permitted:[alias]}` — what `agent_gc` would sweep for this caller under the caller rule and what it may not; `session end --dry-run` renders it |
| `slot_acquire` | `kind: build|test|suite, request_id, lane?, pid?, probe?, exec?` | `{granted:true,token,kind,wait_secs}` or `{granted:false,position,wait_secs?,held,capacity}` — non-blocking; callers poll with a stable `request_id` (queue identity only — the daemon mints the `slot-*` token on grant). Caller identity is connection-derived (below): `lane` is advisory, `pid` must be the socket peer or its ancestor. A re-poll adopts a hold only on an exact `(request_id, pid, lane, kind)` match — for a strict hold also the exact recorded holder `(pid, starttime, uid)`; any other caller sharing the id queues. `probe:true` answers without joining the queue (the CLI's `--wait-secs 0` path). `pid` is the holder whose death frees the slot. `exec:true` (`build-slot run`, CAD-230b): `pid` must be the socket peer itself — the process that execs into the command — and a strict caller's hold is *exec-bound* |
| `slot_release` | `token, lane?, pid?` | `{released:true,token,kind}` — the release must name the holding `(lane, pid)`, both derived from the connection (`lane` advisory, `pid` must be the peer or its ancestor); a foreign token is a named refusal, a never-held token a named rejection, and a token just reaped this call answers `{released:false, reason}` to its own lane (a `trap`-style cleanup never hard-fails) — foreign lanes get the same never-held rejection, so a token's existence is never probed across lanes |
| `slot_status` | `lane?` | `{pools:{build,suite}:{capacity,held[]}, waiting[], config, enrollments[], strict:{available,reason?,reconcile_required?,state_generation}}` — also a reap pass: dead holders/waiters drop on the read. A hold's `token` shows only to the connection whose derived lane owns the hold and whose ancestry includes the hold's pid; everyone else sees identity only. Each hold names its `binding` (`legacy`/`strict`); a strict hold adds `enrollment_id, owner_generation, auth_state, liveness, accounting, reconcile_required` and, when something must be done that reconcile cannot do, a `remedy` (see Managed endpoints below) |
| `slot_launch` | `recipe, project, worktree?, wait_secs?` | `{runner_id,state:"queued",project,recipe,kind,digest,head_sha,log_path,lane,requester}` — CAD-230b: the daemon runs one of the project's `build.recipes` as a *runner* under an exec-bound strict slot (see Daemon-launched runners below). Only these four fields are accepted — `argv`, `cmd`, `env`, `cwd`, `lane`, `alias`, `pid` or anything else is refused by name. The requester must derive a pane (legacy), an active enrolled managed endpoint, or pass operator proof; the runner's process tree never launches — attached, or detached and env-scrubbed: the daemon is its child subreaper, so a detached descendant stays a daemon descendant and fails operator proof (CAD-308). Answers at once; poll `slot_runner`. `cadence build-slot launch <recipe> [--project] [--worktree] [--wait-secs] [--detach]` |
| `slot_runner` | `runner_id` | the runner's receipt `{runner_id,project,recipe,kind,digest,head_sha,dirty,worktree,cwd,argv,env,requester,state,complete,last_state?,pid,starttime,enrollment_id,created,started,ended,exit_code,signal,reason,log_path}` — readable by any connection with a slot identity or operator proof; carries no token. `cadence build-slot runner <id>` |
| `slot_reconcile` | `enrollment_id, token, evidence:{owner_generation,pid,starttime,uid,observed_at,process_read,command_outcome,side_effect_review}` | `{reconciled:true,token,kind,observed}` — the one operator path over a strict hold. Operator authority needs positive proof (CAD-276): refused from any connection that derives a slot identity (a pane or an enrolled endpoint is an agent), and from one that is not provably the operator — the peer must run as the daemon's uid with a fully readable ancestry on which no hop is a registered pane, an enrolled or tombstoned root, a descendant of the daemon (the daemon is the child subreaper of everything it launches, so a `setsid -f`/double-fork orphan of one of its trees stays its descendant — CAD-308), or a same-uid process carrying `CADENCE_ALIAS` or `CADENCE_RUNNER_ID` (a daemon-launched runner's tree, CAD-230b), hold no pane pty, and have its session leader on that ancestry (a `setsid` + double-fork orphan does not); refused too when the request carries `by`/`operator`/`actor`/`alias`/`lane`/`pid`. The evidence must name the recorded hold exactly; the daemon then reads `/proc` itself and frees only on proven death — a live or unknown holder is refused whatever the evidence says. `cadence build-slot reconcile <enrollment_id> <token> --evidence <json>` |
| `report_verdict` | `issue, text` | the stored report (`{id, report, path, kind:"verdict", agent, committed, duplicate}`) plus `delivery` (the ticket's loop record). CAD-431: the only way a `verdict` report is filed — `text` is the report Markdown (`verdict: pass\|revise`, `sha:`, findings as the body). See "Worker loop" for who may file it |
| `delivery_list` | `issue?` | `{records:[Record]}` — the worker loop's records (CAD-431), oldest dispatch first. A read, open to anyone (the master included) |
| `delivery_observe` | `issue, head, pr_state (OPEN\|MERGED\|CLOSED), ci_green?, auto_merge?, additions?, deletions?, files?` | `{issue, state, was, disable_auto, merge_ready}` — what the operator's process read from GitHub. **Operator only** |
| `delivery_merge` | `issue, phase (authorize\|check\|enqueued), sha?` | `authorize`/`check`: `{issue, sha, pr}`; `enqueued`: the record, now `enqueued`. **Operator only** |
| `delivery_decline` | `issue, reason` | the record, now `declined`, `note` = the reason. **Operator only** |

`alias`, `provider`, `message` ids: `^[a-z0-9][a-z0-9-]{0,63}$`.
`text`: 1–48000 chars. `reply_to` may not equal `alias`.

Wherever a method takes `alias`, a provider-native id — `thread_id` or
`session_id`, e.g. a Devin session slug — resolves to the canonical
alias. An exact alias match always wins; a native id matching more than
one agent is rejected as ambiguous. `agent_register` is the exception:
its `alias` is a new name, never resolved.

`agent_stop` is bounded: it interrupts the provider, waits a short grace
(~3s), then force-closes the transport and joins the actor — the bound
also covers provider initialization, since the adapter is published
before `open`. A turn that was still in flight becomes `unknown` and the
agent stays `attention` — a stop never masks a fence: on an agent already
in `attention` it only disables and preserves the state and reason. While
a stop is in flight the alias stays reserved: `agent_resume` is
`rejected` until the stop's final write is done, and a second
`agent_stop` is `rejected` (`already stopping`) before any mutation —
a stale stop cannot write over a new actor generation. A sequential
stop after completion is idempotent. Resume on an unknown-fenced agent
is `rejected`. The rejection says to inspect the uncertain message and
side effects first: a missed render observation does not prove the
delivery did not happen, and reconciliation is an explicit operator
decision rather than an automatic interrupted or completed result.
CLI `agent unfence` resumes by default, so the rejection does not ask
for a second resume; `--no-resume` reconciles without resuming. The
bare `agent_unfence` RPC stays reconcile-only unless `resume` is true.
The fence is lifted only by an explicit operator reconcile.

### Caller rules (CAD-370, CAD-372–CAD-375, CAD-384)

Authority comes from **who the connection is**, never from a request
field. The daemon derives the caller from the socket peer's
`SO_PEERCRED` pid: the nearest registered pane or enrolled managed
endpoint on its `/proc` ancestry IS that agent; deriving none, the peer
is the operator only on positive proof (`peer::operator_proof`,
CAD-276). `CADENCE_ALIAS`, `pane`, `by`, `reviewer`, `owner` and the
other identity-shaped fields never decide who is asking. On the verbs
below they are refused, naming the field, and nothing is written.
Every daemon method carries one rule in `daemon::caller_rule::RULES`
(read, bearer token, handler-bound, agent-targeted, attributed,
shutdown, or a named unguarded gap); a test parses the method table
from `Shared::dispatch_method`, so a method added without a rule fails
it. The table's rules run before the method, on the same derivation
(`agent_caller`) the handler-bound verbs use.

| Verb | Who may call | Refused |
|---|---|---|
| `task_verdict` | any verified agent except the task's assignee and the agent whose kickoff reported the judged revision; or the proven operator. The reviewer recorded is the caller's alias, or `operator` | the assignee/author (`… the task's assignee …`); an unprovable caller; `reviewer`/`pane`/`by`/… in the request |
| `task_reopen` | the proven operator, or the job's own PM (the caller's derived alias equals the job's `pm`; skill/cadence/SKILL.md "Job work"). The record names the caller | the task's assignee, a peer, another group's PM |
| `monitor_stop`, `monitor_dispatch` | the proven operator | every agent |
| `message_reconcile`, `agent_unfence` | the proven operator; the record says `by:"operator"` | every agent: the fenced one, a peer and its own PM ("reconciliation is an explicit operator decision"). A PM cannot unfence or reconcile its worker; it escalates to the operator |
| `agent_respond` | the proven operator, or the requester's own PM (`params.upstream`, bound to the PM's registration as in `agent_set`) | the requesting agent itself, a peer, another group's PM |
| `plan_approve`, `plan_reject`, `approval_record`, `approval_revoke`, `model_defaults_set`, `slot_reconcile` | the proven operator (`operator_connection`) | every agent |
| `report_verdict` | the ticket's assigned reviewer, by its verified connection (a pane or enrolled managed endpoint) | the ticket's worker (`… never judges its own work`), any other agent (`… review is assigned to r1, not r2`), the master (not on its allowlist), the operator (`… decides at the merge`), a frontmatter `agent` naming anyone but the caller, and a verdict whose `sha` is not the head under review (`stale verdict`) |
| `delivery_observe`, `delivery_merge`, `delivery_decline` | the proven operator (`operator_connection`) | every agent, and a detached child of any agent |
| `project_new` | the proven operator (`operator_connection`) or the master by its verified connection (`caller_is_master`, CAD-339); identity-shaped fields are refused for both first; the commit's `Actor:` is `operator` or `master` | every other agent, and a detached child of any agent (the master's included) |
| `agent_stop`, `agent_resume` (CAD-384) | the proven operator, the target's own PM, or the target itself (`peer::may_mutate_agent`, self-service class) | a peer, another group's PM, a detached child of an agent |
| `message_cancel` (CAD-384) | the proven operator or the recipient's own PM (`peer::may_mutate_agent`, controlled class) | the recipient itself, a peer, another group's PM, a detached child |
| `job_cancel`, `job_close`, `task_fail`, `task_cancel`, `task_dispatch`, `task_accept`, `task_sha`, `monitor_alert_ack` (`by`), `monitor_register` (`owner`), `agent_ready` (`by`) (CAD-384) | any agent, attributed to itself; the proven operator, whose own `by`/`owner` is kept (default `operator`). No handler defaults `by` to the operator | a detached child of an agent; an agent whose `by`/`owner`/`pane`/… names anyone else, the operator included |
| `shutdown` (CAD-384) | the proven operator; the agent holding the live rollout lease under a live operator grant; in a sandbox, a caller tied to none of its agents | every other agent (an ungranted or revoked holder included), and a detached child of one. This guards against mistaken or misattributed stops, not a hostile same-uid agent, which can write the state database or kill the daemon (CAD-280) — it is not a security boundary |
| `rollout_grant`, `rollout_revoke` (CAD-384) | the proven operator (`operator_connection`) | every agent |
| `agent_stop`, `agent_resume`, `message_cancel` in a sandbox (CAD-384) | also a caller tied to none of the sandbox's agents — `sandbox down` run from a production pane; recorded `by:"operator (sandbox)"` | a caller tied to a sandbox agent by ancestry, pty, env alias, or as a sandbox-daemon descendant |
| `thread_send`, and an `agent_send` that would land in a thread as the operator's (CAD-384) | the proven operator | every agent (for `thread_send`); a detached child of an agent |
| running turn tokens (any answer) | only the connection that derives the agent owning the turn | everyone else, the operator and the board included, reads `null` for that `turn_id` (and `[turn token withheld]` where prose quotes it), whether or not the token is current |

Refusals name the verb and the rule, e.g. `monitor stop is an
operator action — this connection is agent 'lead'`, `job task reopen
refused: agent 'w9' is not job 'j1''s PM ('pm')`, `agent respond
refused: agent 'wr' cannot answer its own request … (caller rule,
CAD-370)`, `job verdict: caller identity is connection-bound; request
field 'reviewer' is not accepted`. Turn tokens are withheld on every
read path at once (`agent_show`, `agent_list`, events, job and task
views, threads, `agent_capture`): the daemon filters each answer for
the token of EVERY running message and masks the ones the caller does
not own, current or not — a hot restart clears the generation while
adopted turns keep running, then restores it. Only a finished turn's
token (the message is no longer `running`) passes. The owner is derived
from registered panes and enrollments; while those are not yet
published (the restart window) the owner is withheld from too. Residual (CAD-280): "operator" means only that `operator_proof`
passed, which a same-uid process that detaches from every pane and
scrubs its env and stdio still does.

## Agents

`endpoint_kind` selects the delivery mechanism; reachable message states
depend on it:

| endpoint_kind | delivery | status |
|---|---|---|
| `managed` | owned provider process — JSON-RPC stdio (`codex`) or newline-delimited stream-json (`claude`) | implemented: providers `codex`, `claude` |
| `managed-ws` | owned `codex app-server --listen ws://127.0.0.1:*`; official TUI attachable | implemented: provider `codex` |
| `pty` | owned tmux pane running the official TUI; literal paste + explicit reports | implemented: providers `devin`, `claude` |
| `inbox` | durable mailbox — no actor; messages queue until `agent_inbox` drains them | implemented: provider `inbox` |
| `fake` | in-process test double | test fixture only |

`agent_register` accepts `params` (JSON object) for endpoint options:
pty uses `{"session": "<native-id>"}` to resume an existing Devin or
Claude session instead of starting a fresh one, and `{"auto_ready":
"verified"}` opts into daemon-side verified claims (see the pty
section). Provider `inbox` requires endpoint kind `inbox` and vice
versa — mixed pairs are rejected — and its `cwd` may be omitted (a
mailbox runs no process; the state dir is recorded instead).

`params` also carries wiring metadata: `{"upstream": "<pm-alias>"}`
marks the agent as a worker joined to a group (`cadence join` sets it).
When a message is sent to such an agent without an explicit `reply_to`,
`agent_send`/`agent_ask` default `reply_to` to the upstream alias, so
the worker's result lands on the PM's queue. An explicit `reply_to`
always wins; `reply_to` may still not equal the sender alias and must
name a registered agent (both enforced by `enqueue`).

For provider `claude`, `params` holds the launch options the adapter
replays verbatim on every resume: `{"model": "<cli model>",
"effort": "low|medium|high|xhigh|max"` (the CLI's `--effort`; any other
level is refused at register and by the verbs),
`"permission_mode": "<claude mode>"}` (default `manual`;
`--bypass` stores `bypassPermissions`), `{"allowed_tools": ["<pat>",
…]}` — appended to the `Bash(cadence *)` baseline the CLI tool needs
to self-report — and, on the managed endpoint only, the turn-liveness
knobs `{"turn_idle_secs": N}` (default 900) plus `{"turn_max_secs": N}`
(optional absolute cap; pty liveness is the pane itself). On the pty
endpoint the same `permission_mode`/`allowed_tools`/`model`/`effort` params map
to the TUI's own launch flags and `{"session": "<claude-session-id>"}`
resumes a native session (`cadence claude --tui -r <id>`).

For provider `devin`, `{"permission_mode": "<mode>"}` sets Devin's own
approval policy — `auto`, `accept-edits`, `smart` or `dangerous`
(`cadence devin --bypass` stores `dangerous`). The pane profile replays
it as `--permission-mode <mode>` on every open, fresh launch and `-r`
resume alike, so an unattended pane never stalls on its first approval
menu. `agent_register` validates the four values and rejects anything
else; `agent set` cannot patch it live (the mode is launch-time only —
relaunch or rejoin to change it). When the key is absent the flag is
omitted entirely and Devin's own default applies.

For provider `codex`, `{"model": "<model>", "effort": "<level>"}` selects
the thread model and reasoning effort. Cadence queries the provider's
`model/list` metadata whenever either is configured, rejects an unavailable
model or unsupported pair, and labels metadata failures as `availability
unknown`; it never silently inherits a global model. The settings are sent
as `model` and `config.model_reasoning_effort` on both `thread/start` and
`thread/resume`, so a stopped agent preserves them. `{"approval_policy":
"<policy>"}` selects the
`approvalPolicy` sent on `thread/start`/`thread/resume` — `never`,
`on-request`, `on-failure` or `untrusted` — replayed verbatim on every
open like the other launch params (`cadence codex --approval-policy`,
`cadence join --approval-policy`; the join flag is refused for other
providers). `agent_register` and
`agent set --next-launch` reject anything else with an error naming all
four, and the adapter validates once more before the wire. When the key
is absent a cadence-launched worker sends `never`: no approval
round-trips to stall an unattended turn on. `agent show` reports the
effective `approval_policy` of a codex agent with
`approval_policy_source` — `configured` or `cadence default` (both null
for other providers). The worker's filesystem
posture is `sandbox` — `read-only` or `workspace-write`
(`agent register --sandbox`; `cadence join --sandbox`; `cadence codex
--sandbox`). The adapter re-checks the stored sandbox before it
launches the app-server, so a hand-edited value fences the agent with
an error naming it and nothing is spawned. A joined codex worker defaults to `workspace-write`
scoped to its worktree cwd, `read-only` only when explicitly asked —
paired with `approval_policy=never` that is the same trust posture the
other providers already run under, not a new one. Codex turn liveness
is activity-based like managed claude's: every app-server message
resets the clock, `{"turn_idle_secs": N}` (default 900; `cadence codex
--turn-idle-secs`, `join --turn-idle-secs`) bounds silence and the
optional `{"turn_max_secs": N}` is an absolute cap. The old fixed 600 s
wall-clock deadline fenced healthy long turns (CAD-227).

**Briefings.** Every launch path (`devin`, `codex`, `claude`, `join`) writes
`$CADENCE_STATE_DIR/briefings/<root>/BRIEFING-<alias>.md` — under the
daemon's state dir, keyed by group root, never in the agent's cwd repo.
`cadence agent show <alias>` prints the absolute path while the file
exists; a missing file reads `"briefing": null` plus
`"briefing_missing": "<path>"`, never a live path. Agents are told
to read the path printed in their bootstrap message. The file carries
identity (alias, native session id, upstream), any `--instructions-file`
content under a role-instructions section, a protocol quickref, and
the group roster at write time — a snapshot; `cadence self`/`agent list`
stay live truth. The briefing is the only channel role instructions have
on every provider but codex (which also takes them natively as developer
instructions), so `--instructions-file` with `--no-bootstrap` is refused
there before anything is registered. Briefings are written only after the endpoint reports
open, and regenerated on resume when missing. Nothing else lands in the
cwd repo unless the operator opts in: `--agents-md` (persisted in
launch params, replayed on resume) adds an idempotent
`<!-- cadence:begin -->`/`<!-- cadence:end -->` block to
`<repo>/AGENTS.md` (created or appended, never touching outside
content). `join` additionally enqueues the deterministic
`bootstrap-<worker>` message (`source="bootstrap"`); standalone
launches stay silent unless `--bootstrap` is passed; `--no-bootstrap`
skips file, block and message. `cadence agent bootstrap <alias>`
retrofits a live agent — file and the durable message — and refuses
unknown aliases.

**Isolated worktrees.** `--worktree <name>` on `devin`, `codex`, and
`join` runs the worker in `<repo>/.cadence/wt/<name>` on branch
`cadence/<name>` via `git worktree add -b`. It requires a git repo,
rejects invalid names, existing target dirs, branch collisions, and
applying it to an already-registered agent. Because the worktree lives
inside the repo, `.cadence/` is appended to `.gitignore` when absent —
only after `git worktree add` succeeds.

**Group scoping.** `agent_list` (the RPC) always returns every agent.
Every row carries `"group"`: the agent's own `params.upstream` when
wired, else its own alias — the group's root alias either way. Root
rows are also marked `"group_root": true`, so consumers can render
workers nested under their PM without re-deriving the relation. The
`cadence agent list` CLI scopes by default when `CADENCE_ALIAS` is set
and resolves via `agent_show`: the caller's group root is its
`params.upstream` if set, else its own alias, and the output keeps the
root plus agents whose upstream names it (one level — no transitive
walk). Outside a pane, an unresolvable `CADENCE_ALIAS`, or `--all` all
produce the untouched global list. The bare `cadence attach` listing
sorts roots before their members and exposes the same `group` /
`group_root` fields.

**Group lifecycle.** `cadence resume <group>` resolves `<group>` like
`join` (alias or native id → the PM agent), resumes the PM first, then
every member whose `params.upstream` names the PM — members that are
already live (endpoint set or an actor-alive state) are skipped, and
each member gets a bounded ~15s endpoint wait rather than hanging on a
broken session. Per-member status lines go to stderr and the summary
JSON reports `resumed` / `skipped` / `fenced` / `failed` separately; a
fenced member — an unreconciled `unknown` or any `attention` state —
is listed under `fenced` with the reconcile or remove-and-rejoin hint
and never attempted,
and a member whose provider opened a different native session is
reported `unrecoverable` with an explicit `agent remove` + `join -r`
hint. Once
the group is processed, `resume` attaches to the PM by default under
the same rules as a launch (`--detach` opts out; non-TTY or nested tmux
prints the attach command). `cadence stop <group>` is the symmetric
teardown — members first, then the PM; agents stay registered and
resumable, and the summary lists what was stopped. `agent resume` /
`agent stop` stay strictly single-agent. `cadence resume --all` sweeps
every registered agent that has a resumable thread/session and no live
endpoint, printing the same resumed/skipped/fenced/failed summary;
`cadence daemon start
--resume` runs the same sweep once the daemon answers (default off).
Resuming an already-live agent is rejected with a `cadence attach
<alias>` hint.

**Dead-agent hygiene.** `agent list` and `agent show` report `dead`
per endpoint kind, not "no endpoint string": attachable kinds (`pty`,
`managed-ws`) are dead when registered, not operator-stopped, and
holding no live endpoint; `managed` agents are dead when fenced
(`attention`) or enabled but unattended; `inbox` and `fake` never die.
A second flag
`resumable` answers the question `dead` was being asked — the agent is
stopped-or-dead, still holds a saved native thread/session, and has no
unreconciled `unknown` fencing it. `agent remove <alias>` deletes the
row and its message/event history, refusing while an endpoint is live
or a lifecycle actor owns the alias. `agent gc [--older-than <dur>]`
sweeps dead agents on command; each candidate is independent so one
in-transition alias doesn't fail the sweep. Only the operator or the
agent's own PM may remove an agent (the `agent_set` caller rule). Job
history kept under a removed alias is not listed by `agent show` for a
later agent registered under the same alias: a terminal row older than
the current registration belongs to the previous agent. Both remove **records**
(the row plus its message/event history): they free no disk and are no
memory remedy — the only process either touches is a fenced pty
agent's surviving pane, which they kill so no orphan outlives its row —
and a removed agent can no longer be resumed.

**Agent-gc timer (opt-in, CAD-199).** The daemon runs the same sweep on
its own only when pm.yaml sets `[host] agent_gc_older_than_secs` —
unset, it never removes anything. A configured age below 7 days is
raised to 7 days with a warning. From the stall-watch tick it re-reads
the setting every minute and sweeps at most once an hour, never on an
actor loop. On top of the manual rule (endpoint NULL, `attention` or
`stopped`, idle longer than the age) it keeps any enabled agent, any
alias a lifecycle actor owns, any pty agent whose pane is still up, and
any agent with a message in a state other than completed, failed,
interrupted or cancelled — queued, running and `unknown` all keep the
row. It kills nothing: unlike `agent gc` it never touches a pane. Each
removal records one `agent_gc_removed` event on the `daemon` stream
(`cadence events daemon`) with the alias, `reason`, `age_secs`,
`older_than_secs` and the saved `thread_id`/`session_id`, in the same
transaction that deletes the row. `health` (`cadence daemon status`)
reports the effective setting as `agent_gc_timer` — `enabled`,
`older_than_secs`, `configured_secs`, `warning`, and the last check and
sweep.

**Idle auto-stop (default ON, CAD-96).** The daemon stops — resumably,
through the normal `agent stop` path, so pane-session reaping (CAD-201)
and build-slot enrollment revocation apply — any agent whose actor has
had nothing to do for the idle bound: default 3600s for every provider;
`[host] auto_stop_idle_secs` (0 = off) and
`auto_stop_idle_secs_by_provider` (`{claude: 7200, codex: 0}`) override
it, and per agent `agent set <alias> auto_stop=off` opts out while
`auto_stop_idle_secs=<n>` sets that agent's own bound (0 = off). A
bound below 600s is raised to 600s with a warning. "Idle" means the
agent is `idle` with no message in any state but completed, failed,
interrupted or cancelled (queued, submitting, running, awaiting a
report and `unknown` all keep it), and the newest durable activity —
any message's created/started/completed stamp, or any event on its
stream other than bookkeeping kinds (`quota_updated`, `params_updated`,
`stop_requested`, `pane_tree_*`, …) — is older than the bound. Every
endpoint open writes `ready`, so a resume or daemon restart starts the
clock afresh. Exempt: group roots (role `pm`, no `upstream`, or named
as another agent's upstream), inboxes, agents with no saved native
thread (they could not resume), pty agents with a tmux client attached
to their pane (`cadence attach`) or whose pane probe is not idle. A
managed-ws codex TUI client is not detectable and does not exempt its
agent. The check runs from the stall-watch tick at most once a minute,
never on an actor loop, and re-reads the durable facts right before
each stop. Each stop records `agent_auto_stopped` on the agent's stream
(`idle_secs`, `idle_since`, `bound_secs`, `bound_source`, `reason`,
`resume`); `agent_show`/`agent_list` then carry `auto_stopped` and
`state_label` (`stopped (auto, idle 72m)`) until a manual stop or a
resume supersedes it, and `cadence status` shows that label.
`cadence agent resume <alias>` (or `cadence resume <group>`) brings the
agent back. `health` reports `agent_auto_stop`: the effective bound,
per-provider overrides, `warning`, the last check, `last_stopped`,
`stopped_total`, and `last_kept` — why each live agent was kept.

**Auto-resume on queued work (CAD-413).** An auto-stop parks an agent;
it does not dismiss it. The agent's durable stop reason is the newest
of its `agent_auto_stopped`, `stop_requested`, `ready`,
`agent_auto_resumed` and `agent_auto_resume_failed` events, so it
survives a daemon restart. On every stall-watch tick the daemon looks
for `stopped` agents with a queued message (nudges excluded); one whose
stop reason is still `agent_auto_stopped` records `agent_auto_resumed`
(`message` — the oldest waiting, `queued`, `auto_stopped_at`, `reason`)
and is resumed through the `agent resume` path, and its actor delivers
the queue as usual (the pty ready gate still applies). Whatever path
queued the message — a send, a routed result, a job dispatch — and
whether it was queued before a restart, the result is the same. An
operator or PM `agent stop` (before or after an auto-stop) writes
`stop_requested` and supersedes the marker: that agent stays stopped
and the message waits. The daemon re-checks the marker, records the
resume and starts the agent under the lifecycle lock that `agent stop`
takes before it writes `stop_requested`. A stop racing the sweep
therefore always wins: in flight or finished, the resume declines
quietly, with no event and no needs-me row. A resume that was recorded
but never started (the daemon died in between) is reported as failed on
the next sweep. A resume that is refused at start or whose open
never reaches `ready` records `agent_auto_resume_failed` (`message`,
`queued`, `reason`, `resume`) and is not retried; `agent_show` and
`agent_list` then carry `auto_resume_failed`, and the Overview raises
an `auto_resume_failed` needs-me row naming the agent and the waiting
message, with `cadence agent resume <alias>` as the command. It
replaces the generic `fenced` row. An operator resume or stop clears it.

A fenced
agent with no endpoint prints the `devin -r <session>` resume hint from
its launch summary.

## Endpoint capabilities

Every `(provider, endpoint_kind)` pair resolves to one descriptor in
`src/adapter/registry.rs` (`SPECS`) — the single table daemon, CLI,
`doctor` and the board consult instead of comparing provider/kind
strings. `registry::spec(provider, kind)` validates the pair and
rejects unknown combinations with the supported list; `spec_opt` is
the non-failing form, and kind-scoped helpers (`has_actor`,
`attachable`, `ready_gate`, `screen_probe`, `reports_turn_result`,
`report_hint`, `respond_rejection`) answer the recurring questions.

`agent_show.agent.capabilities` and each `agent_list.agents[]` row carry
the spec rendered as JSON (`null` when the registered pair has no
spec):

| field | meaning |
|---|---|
| `provider`, `endpoint_kind` | the registered pair |
| `display` | human-readable name |
| `has_actor` | `false` for the `inbox` mailbox — no actor, no process |
| `attach` | `none` \| `headless` \| `tmux` \| `provider_tui` — the attach surface |
| `ready_gate` | operator ready-claim gate exists (pty) |
| `screen_probe` | pane capture/probe exists (pty) |
| `reports` | `explicit` (`message result`) or `turn_result` (the adapter's turn result completes the message) |
| `brokers_requests` | provider-initiated requests reach `agent_respond` |
| `resumable` / `resume` | whether resume applies and what it means |
| `live_settable_params` | keys `agent_set` may patch live (`auto_ready` on pty) |
| `launch_params` | params the provider's launch verb accepts |
| `session_id_label` | label for the native session id |

`health.capabilities` and `doctor.capabilities` are generated from the
same table plus daemon-level features (`agent_registry`,
`durable_queue`, `operator_reconcile`, `job_lifecycle`,
`revision_bound_verdicts`, `approval_brokering`, `result_routing`,
`model_defaults`) —
never hand-listed per provider. Adding a provider or kind means one
`SPECS` entry; every check, capability list and doctor probe follows.
A board that sees a reachable daemon without `model_defaults` reports
the settings route as unsupported instead of guessing from an error string.

## pty endpoints (providers `devin`, `claude`, `cursor`)

Cadence launches `devin [--permission-mode <mode>] [-r <session>]`,
`claude [--session-id <id> | --resume <id>]` or `cursor-agent --trust
[--model <m>] [--force|--auto-review] --resume <chat>` inside a detached tmux
session on a private socket (`cadence-<state-hash>`), so every pane it
can kill is one it spawned. The agent record keeps the fields separate:
`alias`, `thread_id` = the native provider session id, `endpoint` =
`tmux://<socket>/<session>`, `pid` = pane process, `generation` = a uuid
minted per `open`.

**Per-TUI profiles.** The adapter is generic owned-pane machinery —
private socket, pane lifecycle, readiness claims, literal paste, the
differential render check — with every provider-specific fact behind a
`TuiProfile` (`src/adapter/pty/profile.rs`): launch argv, native
session-ownership proof, the screen analyzer that produces the probe
verdict, message wording, the open deadline, and the forbidden-prefix
list below. `devin`, `claude` and `cursor` are the real profiles; `tui-stub` is
a test-double profile the integration harness registers to prove the
mechanics are profile-driven. A new TUI is a new profile module, not
a copy of the adapter.

**Ownership is proven, not assumed.** Devin flock's
`~/.local/share/devin/cli/session_locks/<session>.lock`; Cadence walks
`/proc` to require that a lock holder is a descendant of the pane pid —
at open, at every send, and on reconnect. If the lock for a requested
session is held by any other process, registration refuses (no
takeover). On restart a live pane that still owns the recorded session
is reattached; a dead pane is relaunched with `devin -r <stored>`. A
pane owning a *different* session fails closed (`attention`).

Claude's proof is its own per-process registry:
`~/.claude/sessions/<pid>.json` records `{pid, sessionId, cwd,
procStart}` for every interactive process. An entry counts as owned
when its pid is alive — `procStart` matched against `/proc/<pid>/stat`
field 22 so a recycled pid cannot impersonate it — and descends from
the pane pid. The same three rules follow: a live foreign pid claiming
the wanted session refuses takeover, a pane owning a different session
fails closed, and a dead pane relaunches with `claude --resume
<stored>`.

Cursor has no lock file or session registry — its proof is the chat
store the TUI itself holds open: a running `cursor-agent` keeps an fd
on `~/.cursor/chats/<project-hash>/<chat-id>/store.db` for the whole
session, and its argv carries `--resume <chat-id>` from exec. A live
attachment counts when the holding process (or the argv carrier)
descends from the pane pid — `/proc` walked the same way, and any
descendant holder proves it (`/proc` order is not pane-first). A fresh
launch mints its chat id once (`cursor-agent create-chat`, uuid-shaped
output anchored exactly) and records it via `cadence/session_minted`
→ `params.session` *before* the pane exists, so a respawn after a
failed open resumes the same id rather than abandoning a chat per
retry. The same three rules follow: a live foreign attachment to the
wanted chat refuses takeover, a pane owning a different chat fails
closed, and a dead pane relaunches with `cursor-agent --resume
<stored>`; a pane holding a chat nobody recorded is refused, never
adopted — the next open mints instead. Cursor chats are disposable
ids, and only for cursor does a resume that provably cannot complete
unwedge the alias: when the pane exits on the stored chat or stays up
but never acquires it (a mismatch never counts — the pane's chat could
be a foreign one), the adapter emits `cadence/session_resume_failed`
and the daemon clears `params.session` and `thread_id` in one write —
both feed `desired_session`, so dropping only params would resume the
dead id through the thread fallback — so the next open mints a fresh
chat. Other pty profiles opt out (`session_disposable`), and the
daemon refuses the clear for them independently: an operator-supplied
Claude/Devin session survives a transient proof timeout. A lost
`set_params` on either arm records `session_persist_failed`.

Cursor also has no per-launch allow flag for the worker's own
`cadence` calls — where claude's argv carries
`--allowedTools 'Bash(cadence *)'`, cursor's allowlist lives in the
CLI's own `~/.cursor/cli-config.json` (`permissions.allow`, entries
like `Shell(ls)`). At every launch the profile merges
`Shell(cadence)` into that array idempotently: the file is parsed
first, rewritten only when the entry is absent, a `.bak` of the
original bytes is kept on modification, and an unparseable or
non-array config refuses the launch rather than clobbering it. The
rewrite is atomic — a same-directory `.cadence-tmp`, fsync, rename —
and both the config and its `.bak` are created with the source
file's mode, never born world-readable. In every permission mode a
worker's `cadence self`/`message result` then runs without an
approval menu.

**Process tree (CAD-201) — what `agent stop` promises for pane
children.** tmux starts each pane's command as a session leader, so
the pane root's pid is the session id every descendant inherits —
including MCP servers that move to their own process group and so
outlive a `kill-session`. At every open (and hot-restart adoption)
the adapter records the root's identity as a `pane_root` event:
`{pid, start_time, sid, session_leader, generation}`, with
`start_time` from `/proc/<pid>/stat` field 22. `agent show` reports it
as `pane_root` (`current: true` when it belongs to the live
generation). On `agent stop`, after the pane is killed, a background
thread (never the actor loop or the RPC) reaps what is left of that
session:

1. The identity is read while the agent row still names its
   generation; a record from another generation is refused.
2. Members are the live, non-zombie processes whose session id equals
   the recorded `sid` and that started no earlier than the root. If
   the root pid now belongs to a different process (start time
   differs — pid reuse), nothing is signalled.
3. `pane_tree_reap_intent` records the members; each gets SIGTERM
   only after its pid + start time + session are re-verified (pinned
   through a pidfd, so the check and the signal hit the same process).
4. A bounded drain — 60s by default, the time ops measured MCP
   children take to exit after stdin EOF (`CADENCE_PTY_DRAIN_SECS`
   overrides it) — re-samples every 250ms.
5. Survivors whose identity still matches are SIGKILLed, after the
   generation/reopen check runs again. `pane_tree_reaped` records
   `terminated`, `exited`, `killed` and `residue` (anything still in
   the session at the final sample, e.g. a process forked during the
   drain, which is never signalled). A refusal is
   `pane_tree_reap_refused` with its reason.

Limits: a process that calls `setsid` itself leaves the pane's
session and is not found — cadence does not promise its cleanup.
Nothing is ever signalled by pid alone, nothing outside the recorded
session is touched, and a daemon running inside the pane's own
session refuses to reap. An agent whose pane was opened before this
record existed (or whose root was unreadable at open) gets
`pane_tree_unowned` on stop and no signal beyond the pane's own
`kill-session`. A repeated stop reaps nothing: the newest record is
already the reap result. `agent remove`/`agent gc` kill a fenced pane
but do not reap its session — `agent stop` it first. Daemon shutdown
detaches panes and reaps nothing.

**Working directory (CAD-202).** While the endpoint is live, `agent
show` reports `pane_cwd` `{path, deleted, pid}` — read from
`/proc/<pid>/cwd` of the terminal's foreground group leader when it is
in the pane's session (tmux's `pane_current_path`), else of the pane
root — and `cwd_deleted`; `cadence status` carries `cwd_deleted` and
flags the row. A pane whose cwd was deleted (its worktree removed
under it) refuses every delivery at the gate with `cwd_deleted: …`:
the message stays `queued` (a `gate_wait` event, like any gate
refusal), never failed. `cadence dispatch` and `issue start --job
--assignee` refuse a pty worker whose live pane cwd is deleted
(`cwd_deleted`, not overridable) or outside every declared repo of
the issue's project (`cwd_outside_project`; a linked worktree under
the repo counts as inside). `--force` overrides the project check and
records the override as an issue comment (`Dispatch override
(--force, cwd_outside_project): …`) and as `cwd_override` in the
output. Plain `cadence send` is not project-checked, nor is a worker
with no live pane (it opens in its registered cwd); `cadence job
dispatch` of an existing task is not project-checked either. The
delivery gate's deleted-cwd refusal applies to every path.

**Submission gates.** `run_turn` requires all of: pane alive,
`pane_dead=0`, `pane_in_mode=0`, native ownership still held, a screen
probe showing no approval menu, and a fresh unconsumed claim — either
an operator claim from `agent ready` (60s TTL) or, under
`auto_ready=verified`, a daemon-minted claim. The probe runs *before*
any claim is consumed, so an open menu refuses every paste — claimed
or self-claimed — and the refusal eats nothing: the claim survives to
deliver once the menu closes. Claims are single-use (consumed
atomically by exactly one send), FIFO, and capped; every consumption
emits a `claim_used` event recording the message id and the claimer
(`agent ready <alias>` records `CADENCE_ALIAS` when set, else
`"operator"`; daemon-minted claims record `"daemon"` on the
`ready_claimed` event itself). A probe failure inside the gate is
itself a gate refusal — `capture-pane` flaking mid-check retries
like a busy pane rather than failing the message. The remedy for a
menu is `cadence agent answer <alias> <choice>` (`agent_answer`
RPC): the adapter re-probes, requires `approval_menu`, validates
`choice` against the option rows on screen, and sends the profile's
keymap — never a paste. `choice` indexes the whole printed option
block top-to-bottom (the highlighted row may not be first), bounded
by the visible count, and an unparseable or single-row block refuses
rather than guessing; navigation moves `Up`/`Down` from the
highlighted row on unnumbered selects, and a multi-digit index on a
numbered menu arrows instead of typing — `send-keys "10"` would
press `1` then `0` and select the wrong option. The answerer's
identity is derived, never claimed: the socket peer's pid walks its
`/proc` ancestry into the recorded pane roots, so a CLI run inside an
agent's pane IS that agent — a caller inside the target's own pane is
refused (a pane must never approve its own prompt) and one inside
another agent's pane records that alias with `by_kind:"agent"`. The
target's pane is checked first, then others in sorted order — a
self-refusal never loses to map iteration order. Pane membership is
three signals, not one: ancestry, the `CADENCE_ALIAS` env the peer
still carries (a `setsid` detach keeps it), and a shared controlling
pty via fd targets (detach keeps stdio) — so `setsid cadence agent
answer <self>` cannot launder a self-approval into `operator`. A
caller whose ancestry cannot be fully walked is never stamped
`operator`: while the target's pane is alive the ambiguity is a
refusal, after it is gone the stamp is `by:"unknown"` with
`caller_pid` still recorded. (The documented residual is a maximal
detach — `setsid` plus a scrubbed env plus redirected stdio; the same
actor could `tmux send-keys` its own pane, so the guard targets
accidents and the common detach, not a determined in-pane actor.)
Anything else is `by:"operator"`; a supplied `by` that disagrees is
kept as `claimed_by`, not an attribution. `approval_answered` records
`by`, `by_kind`, `caller_pid`, `choice`, the menu line and the probe,
then wakes the agent's delivery loop —
the menu may be exactly what a queued send waits behind. The probe,
key selection and `send-keys` run inside the adapter's paste lock,
serialized against the gate's own probe+paste so a menu closing
mid-answer can never strand a key in the input line.

With `auto_ready=verified` the daemon mints a claim only after a pane
probe verifies idle: the screen must show the `❭` prompt with an empty
input line, and none of the observed busy signatures or an approval
menu — an approval screen's `❭` option marker can mimic a prompt, so
menu detection wins over prompt shape. Menu evidence is matched in a
wider window than busy (the ~24 lines ending at the last non-blank
row — `capture-pane` pads short content with blank rows, so the
region is not the pane's literal bottom): a numbered approval menu
can stay open *above* a still-visible busy input box, which pushes
its option rows and selection footer well above the busy anchor.
Menu-exclusive anchors (the `↑↓ select · ↵ confirm · esc cancel`
legend — structural glyphs no transcript speaks) decide alone on the
trimmed row's leading glyph. Natural-language anchors (the permission
prompt's title, `Run this command?`, the trust-dialog wordings) only
decide beside real menu structure — a parsed option row, a qualified
option block or the anchored legend — because an indented transcript
row leading with the same words is identical in shape; numbered rows
must additionally form a real menu run (an *indented* `❯`-led row
inside — a column-0 `❯` is the input box or a transcript echo — or
the legend right after) so a quoted markdown list is never a menu's
options. A highlighted `❯`/`→` row only counts as menu structure when
it sits inside a real option block — at least one sibling option —
so transcript echoes of submitted prompts are never mistaken for a
menu's highlight. Option labels and lone legend fragments count only
as a cluster beside that structure — transcript text quoting one
stays inert. Cursor's option rows parse their trailing `(hint)`
through the hotkey allowlist — `foo(bar)` is transcript text, not an
option — and sibling rows beside a `→` highlight must carry a key,
since a menu highlights exactly one row. A `↓ more below`/`↑ more
above` marker beside the block refuses the
answer rather than picking a row whose index does not match the
printed list. When
a menu is open the probe's reason is the menu line itself — the
command being approved — and busy is anchored tighter still:
the `Guide Devin while it works` input watermark, or the status row
directly above the input box — the spinner label (`Thinking`,
`Typing`, `Running tools`) or an `esc to interrupt`/`Cancel agent`
hint, plus the queued-message footer. Blank rows and the box's rules
(they can carry embedded text like `(bypass permissions on)`) sit
between the two and are skipped; a `Did you know` tip banner in the
region is neutral — it quotes the same hints as documentation, never
as a status row, so a tip never stalls verified auto-ready.
Claude's analyzer reads the same
verdict from its own shapes: the input box is the last `❯`-leading
line under a `─` border (a menu's `❯` option marker is never boxed),
busy is the spinner's `esc to interrupt` hint or a `Waiting…` tool
marker, and menus are the permission prompt and directory-trust
dialog. One Claude quirk needs the pane cursor: an idle box shows a
dim *ghost suggestion* (`❯  ls -l …`) that plain capture cannot tell
from a staged draft — the suggestion never moves the cursor off the
prompt start, so the adapter fetches `#{cursor_x},#{cursor_y}`
alongside the capture and the analyzer only counts visible text as
input when the cursor has moved (an unreadable cursor treats it as
real text — conservative).
Cursor's analyzer keys on its own shapes, all captured from live
panes (2026.09.15/2026.09.18): the input line is the last `→`-leading
row *inside the bottom status region* — and never the frame's last
row, since the model/cwd bar always renders below it (a `→` higher up
or ending the frame is transcript text, a scrolled-out input row, or
a menu cursor). An empty input shows a watermark (`Plan, search,
build anything` idle, `Add a follow-up` after a turn — either is
*empty*, never a draft), busy is the `ctrl+c to stop` interrupt hint
on the input row itself or the status row directly above it (the
braille spinner, a spinner word with its token counter, or the staged
`follow-ups` box), and approval requires a strong anchor (`Run this
command?`, `Not in allowlist`, `Waiting for approval`) or a cluster
of menu hints — navigation chrome like `more below` or `Esc to close`
alone never decides, since transcript text can quote it. Everything
matches only inside the status region.
`agent probe <alias>`
runs the same analyzer on demand (`{idle, reason, prompt_visible,
input_nonempty, busy_marker, approval_menu}`) without claiming. An
operator claim is not blind either: `agent ready` runs the same probe
first and refuses with the reason when the pane is visibly busy —
`agent ready --force` (or `send --ready --force`) claims anyway, and
the `ready_claimed` event records `"forced": true` alongside the probe
verdict it overrode. A refused send returns the message to `queued`
(event `gate_wait`) and retries after a back-off (5 → 10 → 20 → 30s; a
claim or inbox arrival wakes it early); it is never pasted blind and
never dropped. `CADENCE_PTY_RETRY_SECS` sets the 5s base for both waits
and scales the whole schedule, the 30s cap included (cap = 6 × base). It
takes seconds in [0.1, 3600]; any other value (0, negative, NaN, inf,
not a number) is refused with a stderr warning and the 5s default
applies. Tests shrink it; production leaves it unset. Message text is a
single line of 1–4000 chars with no control characters (`agent_send` to a pty agent refuses a body with
control characters up front instead of answering `queued`), delivered
literally via `load-buffer` + `paste-buffer -p` + `Enter` — no shell
interpretation. A body whose first non-space character is in the
profile's forbidden-prefix list is rejected `PreWrite` *before* the
gate (so no claim is consumed, nothing reaches the pane, the agent is
not fenced): TUIs commonly treat a leading character as a command or
mode switch, so a verbatim paste of one is an injection path. Devin's
list was fixed by live observation in a scratch pane: `/` opens the
command menu, `!` switches to bash mode, `@` opens the file picker —
all forbidden; `#` stays a literal draft character. Claude's list was
observed the same way (2.1.275): `/` opens the command menu, `!`
switches to shell mode — a command that runs *outside* the permission
system — `@` opens the agent/file autocomplete; `#` again stays
literal. Cursor's list was fixed live the same way
(2026.09.15-d2fe57e): `/` opens the command menu, `!` enters
shell-command mode, `@` opens the file picker — all forbidden; `#`
stays a literal draft.

**Durable submission vs. receipt.** Paste alone is not proof: the
render check is *differential* — the pane is captured before the paste,
and afterwards the occurrence count of the body's normalized tail slice
(whitespace-stripped on both sides, so TUI line wrapping cannot hide a
match) must *increase*. An identical earlier notification already on
screen therefore cannot pass for a dropped re-delivery. And rendered is
not submitted: the input line must also be empty again after `Enter` —
a staged draft left in the input means the keystroke was swallowed.
Missing the deadline is `NotRendered` — *evidence* of a dropped or
unsubmitted paste, not proof, since a saturated host can render late.
The `paste_not_rendered` event carries that evidence: the normalized
screen tail before the paste and the tail after the deadline (12 rows
each), plus the probe verdict that admitted the send.
On that evidence a routed `worker_result` notification is requeued
(retried 5s later, bounded, then the delivery is `failed` with
`via=pty_render_miss` and a `delivery_parked` event — a notification
must never fence the recipient or kill its pane; the worker's result
stays durable on the worker's own message). Any other message goes
`unknown` and fences the actor — a possibly-pasted task is never replayed blind. Once the check
passes the message is `running` with `turn_id =
pty-<generation>-<uuid>` and completes only through an explicit
`message_report` (`message ack` keeps it `running`; `message result`
finishes it `completed` and routes `reply_to`). The token must equal
the recorded `turn_id` and belong to the agent's current generation — a
report against a previous pane life, or carrying another endpoint
kind's token, is `rejected` as stale (CAD-162; see JOBS.md "Turn tokens
per endpoint"); a
conflicting result for a completed message is `rejected`; an identical
retry is idempotent. Reporting identifies the caller by possession of
the token — self-asserted, not authenticated. A real `claude` launch
additionally wires a `Stop` hook through `--settings` that runs
`cadence self` and `message ack`s any running message when a turn ends
— an acknowledgement of turn-end only; the agent's own
`message result` remains the completing report.

**One report-owing turn per actor (CAD-250).** A pty pane serializes
turns, and the store says so: while an agent holds a `running` message
that is not a routed notification, its actor claims only routed
notifications (`worker_result`, `worker_notice`, `job_event` —
fire-and-forget, complete at paste). Every other delivery — a second
task, a `--task` follow-up, a `send --ready` message — is accepted
`queued` and stays there, never refused and never pasted, until the
held turn is reported, reconciled or bounded to `unknown`; a `message
result` wakes the actor so the next one is claimed at once.

**Nudges — mid-turn steering.** `cadence send <alias> --nudge --text
…` (or `message send --nudge`, RPC `agent_send nudge: true`) is the
one way to reach a pane that holds a turn. A nudge is a durable
message with `source: "nudge"` (`nudge: true` on its row) that owns no
turn: like a routed notification it passes the hold and needs no
ready claim — though, like a routed notice, it consumes a stacked
operator `agent ready` claim if one is waiting — and it still waits for
the screen probe to read the pane idle and menu-free. It is accepted
only for an agent with a live pane (an actor and an endpoint, state
`idle`/`busy`); otherwise `send` refuses with `agent <a> has no live
pane`. It never becomes `running` or `awaiting_report`,
owes no report, takes no `reply_to` (and no upstream default), and
completes at its confirmed paste (`result.via: "pty_nudge"`). An
unconfirmed paste ends `unknown` with a `nudge_unconfirmed` event —
never retried, and an unknown nudge fences nothing (it is excluded
from the agent's `unknown` count and from `agent unfence`; reconcile
it with `message reconcile` if you want it closed). Delivered at most
once and never replayed: whenever the agent's actor ends — stop,
fence or daemon shutdown — its queued nudges are `cancelled`
(`result.via: "stop_cancelled"` / `"fence_cancelled"` /
`"shutdown_cancelled"`); after a crash the next start does the same
(`"restart_cancelled"`), and one caught mid-paste goes non-fencing
`unknown`. A nudge still queued 15 minutes after it was sent (a pane
that stayed busy) is cancelled too (`"ttl_cancelled"`). Every one of
these emits a `nudge_cancelled` event `{message, was, state, reason}`;
nudges are never recorded for hot-restart adoption. pty endpoints only — a managed, inbox or cloud agent refuses
`--nudge` naming its provider/kind; `--nudge` with `--ready` is a CLI
error, and so is `--nudge` with `--task` (steering, not task work);
the body is at most 500 characters and follows the pty rules (one
line, no control characters, no forbidden leading character). A nudge caller needs no
authority a plain `send` lacks. The held
turn is **`awaiting_report`** — derived, never stored: `running` with
the `submitted` marker (an ack keeps it). `agent_show`/`agent_list`
carry the `awaiting_report` block (wait, bound, remaining, queued
behind it), the message row `awaiting_report: true`, `status` flags
the agent `awaiting_report`, and the overview adds a needs-me row
(`kind: awaiting_report`, remedy `cadence agent show <alias>`) once
work is queued behind it — a healthy turn in progress with nothing
waiting is not a row.

**Report bound.** A delivered turn waits at most `report_timeout_secs`
(agent param, launch or live `agent set <alias> report_timeout_secs=<n>`;
default 7200 = 2h; `0` disables — and with it the WAL watch's bound
too: a stale unreported row on a live actor then keeps deferring that
provider's checkpoints) for its result. The bound covers every row that
holds the turn — `running` and not a routed notice or nudge, exactly
what the hold matches — including one adopted before its `submitted`
marker landed, so no row can hold the queue unbounded. The clock starts
at delivery and restarts on each valid ack — an ack is the worker's own
report that it holds the turn. When it runs out the actor moves the
turn to `unknown` with `result.via: "report_timeout"` and one
`report_timeout` event `{message, turn_id, waited_secs,
report_timeout_secs}`, then fences like any other uncertain outcome
(`attention`, pane detached, never relaunched): the `unknown` finish
routes exactly one `worker_notice` to `reply_to` (which `send` defaults
to the agent's upstream; a kickoff's is the job PM, whose scoped
`turn_unknown` also raises the monitor alert) — never a result, never
a completion, never a replay. Both writes are guarded: the expiry and
a `message result` each check `running` inside the transaction that
writes, so whichever commits first wins and the other is judged
against the row as it now stands (the report is refused `not awaiting
a report (state unknown)`) — the PM never receives both a notice and a
result for one turn. Other unreported turns on the same actor — only
rows that accumulated before this rule — go `unknown` with it, since
the pane they ran on is detached. The operator exits the fence the
usual way (`message reconcile` / `agent unfence`); what queued behind
the turn then delivers in order.

A pane that dies after a possible paste leaves submitted messages
`unknown` (fence, never replay); a pane that dies before the paste
fails the message. `agent_respond` is `rejected` for pty — provider
permission prompts are answered in the terminal, and a visible prompt
is one of the things the ready claim asserts absent. A fence — like
daemon shutdown — *detaches* the pane rather than killing it. The TUI
process can still be alive, but `agent capture` and `agent probe`
require the live actor adapter and answer that the agent has no live
endpoint while the fence holds. That is not a read-only capture of the
detached session. `agent_unfence` accepts `resume: true` to reconcile
and start the actor in one call — wait (bounded ~30s) for its open —
and reports `resumed` plus `pane`: `adopted` when the surviving pane
was re-attached (same pid, same native session), `respawned` when a
new pane was launched on the recorded session (new pid, same native
session), `none` when the resume was not requested or did not land
(with `error` when the start was rejected). The CLI's `cadence agent
unfence` resumes by default (`--no-resume` reconciles only); do not
follow that default with a second `agent resume`. The bare RPC
defaults to reconcile-only. Adoption is attachment, not readiness — a
visibly busy adopted pane still gates sends behind the screen probe
(or `agent ready --force`). The kill is reserved for explicit verbs:
`agent_stop` kills the owned pane (a live one through its actor, a
fenced survivor directly), and `agent_remove`/`agent_gc` kill any
surviving pane before dropping the row — no orphan sessions on the
private socket.

**Worker-side conveniences.** The pane is spawned with
`CADENCE_ALIAS` and `CADENCE_STATE_DIR` in its environment (`tmux
new-session -e`), so a worker inside it can run `cadence self` to get
`{alias, running: [{id, turn_id}]}` — its report token without asking
the operator. `message send --ready` fuses the operator claim with the
send: the flag runs the same idle probe as `agent ready` (a busy pane
refuses; `--force` overrides), applied only on pty endpoints and
skipped silently elsewhere. A `worker_result` routed *to* a pty PM is
fire-and-forget: once the paste succeeds the delivery completes with
`{"status":"completed","via":"pty_deliver"}` — the PM is not expected
to report on a notification. Post-paste disconnect still fences
`unknown` as usual.

**Pane defaults.** After every `open` (fresh spawn or reattach) the
adapter applies best-effort `set-option` calls on the *private* tmux
server only: `mouse on`, `set-clipboard on` (OSC52),
`status-left-length 40`, `pane-border-status top`,
`pane-border-format " #{session_name} "`. Failures are ignored —
cosmetics never fence an endpoint.

**Devin command approvals.** The Devin CLI persists command grants in
the user-global `~/.config/devin/config.json` under
`permissions.allow` as `Exec(<argv prefix>)` entries — the TUI's own
"always allow" flow writes there. `Exec(cadence)` pre-allows every
cadence subcommand. A project-scoped `.devin/config.json` accepts the
same schema keys and can carry the grant with the repo, but the user
file is the store the CLI is observed to write; both are listed here
so the choice is deliberate rather than rediscovered.

Trust boundary: the tmux socket lives under the private state dir name
scheme but tmux sockets are reachable by the same user; the report
route is token-possession only. Peer result text is recorded data,
not authorization for anything.

`managed-ws` runs the same app-server protocol as `managed`, over a
loopback WebSocket instead of stdio. The agent record exposes `endpoint`
(`ws://127.0.0.1:<port>`) while the actor is alive; an official Codex
TUI attaches to the same native thread with:

```
codex resume --remote <endpoint> <thread_id>
```

## managed claude endpoints (provider `claude`)

`cadence claude` and `cadence join <pm> claude` run one long-lived
headless process per agent:

```
claude -p --input-format stream-json --output-format stream-json --verbose
         (--session-id <uuid> | --resume <uuid>) [--model <m>] [--effort <level>]
         --permission-mode <mode> --allowedTools <pat> …
```

The wire is newline-delimited typed events, not JSON-RPC — there are no
request ids. Each durable message is written as one
`{"type":"user","message":…}` line; the next `result` event completes
it. Turns are serialized by the actor, so correlation is "the first
result after the send". The result's `result` field is the report text
— a managed claude turn completes the message itself, no
`cadence message result` call exists. Turn ids mint as
`claude-<generation>-<uuid4>`.

Result mapping: `subtype:"success"` + `is_error:false` → `completed`
(`stop_reason` preserved); `is_error:true` or `subtype:"error_*"` →
`failed` (a definitive answer — the `errors[]` text is kept);
`subtype:"interrupted"` → `interrupted`; a process exit or EOF before
any `result` → `unknown` + fence, recoverable via `agent unfence` +
`agent resume` which relaunches with `--resume <stored session>`.
`permission_denials` on a result do NOT fail the turn — they are
recorded as a `permission_denied` event and the agent is expected to
work around them.

Turn liveness is **activity-based, never wall-clock**: every parsed
stdout event (`assistant`, `user`, `system`, `stream_event`, `result`)
resets the clock. A turn is `unknown` only after `turn_idle_secs` of
silence (default 900 — a healthy multi-hour turn is fine) or after the
optional `turn_max_secs` absolute cap, which fences even a chatty turn.
The fence records the provider's own reason (`No provider event for
900s`, `Turn exceeded turn_max_secs`, EOF, …) so reconcile knows why.
Each `assistant` tool_use block also lands as a compact `tool_use`
event — the tool name and a redacted one-line input summary — so
`events --follow` shows progress on a long turn. Each `tool_result`
block in a `user` event lands as a `tool_result` event with a redacted
summary of at most 160 chars and `is_error`, never the output itself.
For a threaded agent both become thread entries, and so does each
assistant text block (`assistant_text`, never an event). The adapter
holds the latest text block until the next event and drops the one
the `result` repeats, so the final answer appears once, as the
`turn_result` (CAD-320). A turn that ends without a result (death, idle
fence, cap, interrupt grace) flushes the held block before its
`turn_result`.

The child's environment is scrubbed **by rule**: `CLAUDECODE` and every
inherited `CLAUDE_*`, `CODEX_*`, `CADENCE_*` name is removed — a name
list would keep missing new leak variables (`CLAUDE_CODE_SUBAGENT_MODEL`,
`CLAUDE_EFFORT`, `CLAUDE_PID`, …). The keep-list is operator-set
configuration: `CLAUDE_CONFIG_DIR` and `CLAUDE_CODE_OAUTH_TOKEN`.
`ANTHROPIC_*` auth/proxy variables are never touched; `CADENCE_ALIAS`
and `CADENCE_STATE_DIR` are re-injected per agent.

Session identity: a fresh open mints `--session-id <uuid>`; reopening
uses `--resume <thread_id>`. Every `system/init` event is checked — a
reported `session_id` different from the opened one means another
Claude process owns the session and the agent fences `attention` with
session-mismatch wording (remove + rejoin mints fresh).

Interrupt is SIGINT to the provider's own process group; the adapter
waits a bounded 60s grace for the interrupted `result`, then fails
closed `unknown`. `close` ends stdin first (clean EOF exit) before the
TERM→KILL fallback. There is no attachable surface — `agent attach`
prints an explanation naming `cadence events --follow` and manual
`claude --resume` after `agent stop`. Provider stderr lands in
`providers/<alias>.provider.log` and each `result` emits a
`claude_result` event carrying `total_cost_usd`/`num_turns`/`session_id`
for audit.

**Brokered approvals.** `--broker-approvals` (launch flag; `join …
claude` too) adds `--permission-prompt-tool mcp__cadence__approve` plus
a generated `--mcp-config <state>/agents/<alias>.mcp.json` and
`--strict-mcp-config` to the launch line — replayed on resume like
every stored param. The config names the hidden `cadence
mcp-permission` stdio server (newline-delimited JSON-RPC 2.0:
`initialize` → `notifications/initialized` → `tools/list` →
`tools/call`, verified against claude 2.1.277) with `CADENCE_ALIAS` /
`CADENCE_STATE_DIR` / `CADENCE_PERMISSION_TIMEOUT_SECS` in its env.
Every prompt the permission mode can't pre-decide arrives as a
`tools/call` on the single `approve` tool; the server `request_open`s
it (method `cadence/approval`, deduped on a caller-named handle) and
blocks in `request_wait` for the operator's `agent_respond`: `accept`
→ `{"behavior":"allow","updatedInput":<input>}`, `decline` →
`{"behavior":"deny","message":<reason>}` (`--reason` sets the message).
While a request is open the agent holds `waiting_input`, a
`request_opened` event lands, one `worker_notice` reaches the upstream
PM naming the exact respond command, and the open wait stamps provider
activity so the human's thinking time is never an idle fence. The
server denies on its own `permission_timeout_secs` deadline (default
900 — `request_close` then retires the handle so the agent isn't stuck)
and denies cleanly `closed` when the daemon restarts mid-wait — the
pending map is in-memory, never replayed. Without the flag nothing is
brokered: `agent respond` is `rejected` naming the real opt-ups —
relaunch or rejoin with `--permission-mode <mode>` / `--allow "<pat>"`
/ `--bypass`, and watch `permission_denied` events. Broker mode refuses
`--bypass`/`bypassPermissions` (prompts are moot) and `--tui` (the pane
answers its own).

## inbox endpoints (provider `inbox`)

An inbox is a durable mailbox, not a process: `agent_register` with
`provider=inbox, endpoint_kind=inbox` creates an `idle` agent with the
pseudo-endpoint `inbox://<alias>` and no actor. Registration writes no
briefing and `cwd` may be omitted. Everything else about the agent
model still applies — it can be a group root (`agent list` renders
workers under it) or a routed `reply_to` target.

`agent_send`/`agent_ask` enqueue into the mailbox exactly like any
agent; the messages stay `queued` in SQLite — they survive daemon
restarts and accrue while nothing reads them. `agent_inbox` drains:
every `queued` message with `seq > after` is completed in one
transaction with result `{"status":"completed","via":"inbox_read"}`
and emits `inbox_read`. The `inbox_read` value is a local receipt, so it
is never routed through `reply_to` or used to wake a reviewer; the
consumed row and any genuine `worker_result` body remain durable and
available to the mailbox consumer. `wait>0`
long-polls on the daemon's change signal up to 30s, so a consumer
blocks on the socket instead of polling — `cadence inbox <alias>
[--after N] [--wait S]` prints one JSON object per consumed message
and nothing on an empty drain. `agent_show` reports the backlog as
`queued`; `cadence self` on an inbox answers that count rather than a
running turn.

Lifecycle: a mailbox is always `idle`, so `agent_resume` is rejected
(there is nothing to resume), `agent_stop` is rejected (there is
nothing to interrupt), and `agent_remove` deletes the mailbox and its
history outright — there is no live endpoint to refuse on. Daemon
startup and `resume --all` skip inbox rows.

Unconsumed inboxes (CAD-251). An inbox is **stale** when its unread
count is above `inbox_warn_unread` (default 50) and no `inbox_read`
completed within `inbox_warn_idle_secs` (default 86400; idle time runs
from the last read, else from the oldest unread message). Both are
endpoint params, live-settable with `cadence agent set <inbox>
inbox_warn_unread=N inbox_warn_idle_secs=S` (a bare key restores the
default). A stale inbox warns and never refuses: `agent_send` into it
still queues and adds `warning` to the receipt (`cadence send` also
prints it on stderr); routed deliveries (`reply_to`/upstream results)
have no caller, so the daemon's sweep (on the stall-watch screen-sample
cadence) records one `inbox_unconsumed` event on the inbox — at most
once per idle window, and only after new arrivals. `agent_list` rows for
an inbox carry `inbox_health` (`unread`, `oldest_unread_age_secs`,
`last_read_at`, `idle_secs`, `threshold`, `stale`, `owner`, `warning`);
`cadence status` lists stale inboxes in its footer and `cadence
overview` raises an `inbox_stale` row. Retention and ownership: nothing
is ever dropped or expired automatically — a message leaves the queue
only through a drain, a cancel or `agent_remove`. The inbox's **owner**
is the root of its `params.upstream` chain, or `operator` when the inbox
is its own root; the stale-inbox warning, event and overview row name
that owner as the one who must drain it or retire the inbox.

(`cadence agent attach <alias>` prints this command; `--run` executes it
in the current terminal. `cadence agent resume <alias>` gets the same
post-open treatment as a provider launch: it waits for the endpoint —
bounded ~30s with a clear timeout error — then attaches this terminal by
default; `--detach` or a non-TTY/nested-tmux context prints the attach
command instead, and the summary JSON keeps the `starting` state plus a
`next.attach` hint. Endpoint kinds with nothing attachable (managed
stdio, fake) return the resume receipt immediately. The top-level
`cadence attach [name]` is
client-side sugar over `agent_show` + `agent_list`: it resolves an alias
or native id, then a provider name when exactly one live agent of that
provider exists — ambiguous or absent names list candidates rather than
guess. A resolved attach execs only where this terminal can (stdin a
TTY, not inside tmux — the same rule launches and `resume` follow);
otherwise it prints the command, as does `--print`. The no-name listing
orders each group root before its members and exposes `group` /
`group_root` per row.) A fresh `managed-ws` thread is seeded with one
minimal turn at open — Codex only persists a thread's rollout after its
first turn, and `resume --remote` fails on an unseeded thread. The
endpoint is cleared when the actor exits, so a printed command never
points at a dead address; attaching to a `stopped`/`offline` or non-WS
agent is `rejected`. Terminal echo of a submitted prompt is visibility,
not receipt — message state remains authoritative.

Trust boundary, stated plainly: the endpoint is an unauthenticated
loopback port reachable by ANY local user — `SO_PEERCRED` does not
apply to TCP, and the port is discoverable via `ss`. The URL lives only
in the private 0700 state dir, but treat every local process as able to
connect. Do not expose `managed-ws` on multi-user hosts you distrust.

The wire is tungstenite with a single I/O owner: only one thread ever
touches the `WebSocket`, and outbound payloads (requests, pongs, close
replies) travel over a channel it drains between bounded reads — no two
writers ever share the socket. Connect, handshake, writes, and close
are bounded: the upgrade runs against an absolute deadline (a
drip-feeding or silent peer fails startup within the connect deadline)
and the owned child is killed — `stop` can interrupt setup because the
child is published before the transport connects.

Approval requests remain brokered through `agent_respond`; an attached
TUI may also see and answer them. The provider then emits
`serverRequest/resolved`; Cadence drops the matching pending handle,
emits `input_resolved`, keeps any other pending requests, and a late
`agent_respond` on the consumed handle is `rejected`. Nothing is ever
auto-accepted by Cadence.

Agent states: `starting → idle ⇄ busy → waiting_input →` and terminal-ish
`attention | stopping → stopped | offline`. `attention` means an uncertain
provider outcome needs human review; the actor will not relaunch itself.

## Messages

States: `queued → submitting → running → completed | failed | interrupted
| unknown | cancelled`. A `source: "nudge"` delivery skips `running`:
`queued → submitting → completed | unknown | failed | cancelled`, and
its `unknown` fences nothing (CAD-250). `awaiting_report` is a derived phase of
`running` — a delivered pty turn whose result report is still owed —
visible in the views and bounded by `report_timeout_secs` (see the pty
section); it is never a stored state. `unknown` is durable and fences its actor. Any ambiguous
post-submission outcome lands there — transport loss mid-turn, a turn
deadline, an acknowledged `turn/start` that cannot be correlated to a
turn id, an unclassifiable completion status, or a forced close while a
turn was in flight. `interrupted` is terminal for "the outcome was
never learned and the operator moved on" — written by a reconcile, or
by a provider that reports an interrupted turn; it is never replayed.
`cancelled` is terminal for "dequeued before delivery" — written by
`message_cancel` or a `task cancel` sweeping its queued kickoff; the row
keeps its history and is never claimed, pasted or replayed.

**Fencing and reconcile.** An `unknown` message fences its agent
(`attention`, no relaunch, queued work waits). The only exit that keeps
history is `message_reconcile` — an operator statement requiring no
turn token (the token is stale by definition when a message is
`unknown`), refused for every other current state with an error naming
it. One transaction moves the message to the chosen terminal state with
result `{status, via:"operator_reconcile", note}` and emits a
`reconciled` event carrying the message id, status, note and caller
(`"operator"` — the only caller the gate accepts, CAD-374). `completed`/`failed` route
`reply_to` exactly like a normal finish (same deterministic
`cadence-result:` delivery id — exactly once); `interrupted` routes a
`worker_notice` instead — the replier learns the operator closed the
turn, but nothing is reported as worker output. An `unknown` finish
routes no result either — the outcome was never learned — but the
replier does hear about the fence: one `worker_notice` ("outcome
unknown, worker fenced, operator reconcile pending") under its own
`cadence-notice:` id, disjoint from the result slot a reconcile may
still fill. When the agent's last `unknown` reconciles, the fence lifts
`attention → stopped` with `enabled=0` — the same condition as an
operator stop, so a later restart leaves it stopped rather than
relaunching it; `agent resume` re-enables it.
`agent_unfence` is the bulk form: every `unknown` on the agent in one
call, printing each id; `cadence agent unfence <alias>` then resumes
unless `--no-resume`.

**Cancel.** `message_cancel` is the only exit from `queued`: one
state-guarded UPDATE moves the message to `cancelled` with result
`{status:"cancelled", via:"message_cancel", by, reason}` and emits a
`cancelled` event carrying the id, caller and reason. The guard makes
the cancel atomic against an actor's `take_queued` claim — a claim that
already moved the message to `submitting` wins, and the cancel is
refused naming the current state. Terminal states refuse the same way.
A `reply_to` gets one `worker_notice` (`"cancelled before delivery —
nothing ran"`) so a waiter is never left hanging; the notice rides the
`cadence-notice:` namespace like every other. A task-bound delivery is
refused with a pointer to `cadence task cancel` — the task lifecycle
owns its kickoff. Cancel never touches a running turn: interruption
happens at the provider, then reconcile settles the record.

Idempotency: a client-supplied `message` id makes retries of the *same
envelope* (alias+body+reply_to+source) return `duplicate:true`. The same
id with different content is a `rejected` conflict.

Result routing: when a message has `reply_to`, finishing it enqueues a
`worker_result` message to that agent in the SAME transaction. The routed
id is `uuid5("cadence-result:" + message_id)` (deterministic; resend is a
no-op) and carries no `reply_to`, so routing cannot loop. A routed
`worker_result` delivered to a pty endpoint is fire-and-forget: once the
paste is submitted the delivery itself completes — the receiving PM is
not expected to report a result on a notification.

Routed bodies to a pty endpoint are bounded: the pty paste gate refuses
an oversized body outright, which would fail the delivery whole. The
record itself is never truncated — `messages.result` keeps every byte —
but for a pty recipient an overlong `text`/`note`/`reason` field in the
routed payload is clipped to a preview and the prompt gains a pointer to
`cadence agent show <worker>`. Non-pty recipients (inbox, managed) get
the full body — nothing else in the path bounds text length.

Notices share the same mechanism with a distinct namespace and source:
`worker_notice`, `uuid5("cadence-notice:<kind>:" + message_id)`, no
`reply_to`, fire-and-forget. The body is plainly worded as an
informational notice — never a result — so the recipient cannot mistake
it for worker output. Exactly one is routed when a turn goes `unknown`
(worker fenced, reconcile pending) and one when the operator reconciles
`interrupted`.

Job notifications are the third routed source: `job_event`,
`uuid5("cadence-job:<task>:r<rev>:<state>:<dedupe>")`, `reply_to` NULL,
`task_id` set for indexing, fire-and-forget — the same bounded
render-miss retry and park-instead-of-fence behaviour, so a job
notification can never fence a PM pane. One is enqueued per verdict
transition (`verified`/`revising`/`blocked`) and on `task_done`; a
removed PM simply gets no copy. `Message::is_routed()` is exactly
`worker_result | worker_notice | job_event` — those sources cannot be
forged through `agent_send` (the `identifier` charset has no `_`).

CLI surface: `cadence send` is the verb form of `message send`
(identical path, same `--text/--file/--message/--reply-to/--ready`).
`--ready` on `send` and `ask` is the operator's explicit gate claim for
pty targets — it calls `agent_ready` first and is a silent no-op on
endpoint kinds without a readiness gate. `message ask` accepts
`--reply-to` like `send` (the RPC delegates to the same enqueue).

## Events

`agent_events` pages the durable log: `{seq, alias, kind, payload, at}`.
Kinds: `registered, queued, submitting, turn_started, turn_finished,
provider_event, input_required, input_answered, input_resolved,
request_opened, request_closed,
result_routed, notice_routed, ready, ready_claimed, claim_used,
gate_wait, submitted, session_minted, session_resume_failed,
session_persist_failed,
acknowledged, paste_not_rendered, delivery_parked, inbox_read,
inbox_unconsumed,
params_updated, reconciled, relaunch_skipped, attention,
approval_menu, approval_answered, turn_silent_end,
turn_stalled, turn_resumed, monitor_registered, monitor_alert,
monitor_alert_ack, monitor_degraded, monitor_dispatch,
monitor_dispatch_blocked, monitor_dispatch_resolved, monitor_off,
stop_requested, pane_root, pane_root_unrecorded, pane_tree_reap_intent,
pane_tree_reaped, pane_tree_reap_refused, pane_tree_unowned`. `wait>0` long-polls
up to 30s.

Two page shapes. Forward paging sends `after` — rows above the cursor,
oldest first, up to the page limit. `tail:true` (what `cadence events`
sends when no `--after` is given) returns the newest 50 rows instead —
still oldest first inside the page — plus `has_older` marking whether
history sits below it; `cursor` continues forward in both shapes, so
`--follow` anchors at the tail and streams from there.

Job operations emit the same rows with `job_id`/`task_id` set —
`job_created, task_created, task_dispatched, task_running,
task_reported, verdict_recorded, task_revising, task_blocked,
task_reopened, task_failed, task_cancelled, task_sha_recorded,
task_done, job_closed, job_cancelled` — and `job_events` pages them
across aliases with the same `after`/`tail` contract
(`{job, after?, tail?}` → `{events, cursor, has_older}`).

## Stall detection

A daemon-side watch measures how long each `running` message has gone
without proof of life and reports crossings — it never interrupts,
fences, cancels, or replays anything it observes.

- **Activity clocks.** Managed adapters stamp every incoming provider
  transport message (`activity_at`); the daemon also folds in every
  adapter event, the turn start, and every valid `message_report`
  token. Pty endpoints have no transport clock: the watch samples the
  pane on a bounded interval (≤ one `capture-pane` per running pty
  agent per minute) and hashes the normalized tail — whitespace
  collapses, control characters drop, spinner/bullet glyphs and
  elapsed-time counters (`· 2m 15s`, `83%`, `12:34`) are ignored, so a
  ticking status line never reads as activity while real transcript
  motion does. Screen activity is debounced: a new screen hash counts
  as activity only when confirmed — either the next sample shows the
  same new hash, or the screen has differed from both of the last two
  settled hashes for two consecutive samples (so a scrolling pane,
  whose every sample differs, still confirms). A hash seen once and
  reverted — a capture taken mid-repaint — neither resumes a stalled
  turn nor resets the silence clock. Confirmation costs one extra
  sample interval of latency before `turn_resumed`; the capture bound
  is unchanged (≤ one per interval per agent). An open brokered
  approval request counts as activity for the whole wait.
- **`turn_stalled`.** When silence crosses the resolved budget the
  watch emits the event once per episode — payload `{message,
  silent_secs, last_activity, task?}` — and sends one notice: a
  `job_event` to the job's PM for a `job_dispatch` kickoff, else a
  `worker_notice` to the message's `reply_to`, else nothing. The event
  carries `job_id`/`task_id` scope so `job_events` pages it. The
  message stays `running`; the watch never touches the turn.
- **`turn_resumed`.** The first activity after a stall emits the
  closing event `{message, silent_secs, task?}` and a matching notice
  to the same recipient, then re-arms — a later silence raises a new
  `turn_stalled` episode with its own notice. A turn that ends while
  stalled just ends; no recovery event is owed.
- **Probe verdicts ride the samples (pty).** Every landed screen sample
  carries the analyzer's verdict beside the hash, so the watch sees
  pane states a bare hash cannot name. A menu frame is never activity —
  the wait is a human's, not the provider's: it neither confirms a
  screen change nor feeds the idle streak. An idle frame extends the
  silent-end streak; anything else resets it.
- **`approval_menu`.** The first sampled frame probing
  `approval_menu` while a message runs emits `{message, line}` once —
  the rising edge, never per sample — carrying the menu line (the
  command being approved) so the row names what's asked, and only
  once per open menu: a bounded history of fired lines survives
  message transitions so alternating subjects do not re-fire, but the
  history clears when the menu closes — the same subject re-requested
  is a new wait and fires again. When no turn is running the oldest
  `queued`/`submitting` head is tracked instead — a menu that blocks
  its delivery emits the same event marked `queued: true` — and a
  menu on a pane with nothing tracked at all emits it marked
  `idle: true` with no `message` (the wait belongs to no turn).
  `agent_list`/`agent_show` expose it as `pane_menu` while it stays
  open (queued-head and idle-pane menus included); the `status` PANE
  column renders `approval: <line>`; the overview needs-me row
  (`kind: approval_menu`) gives the remedy `cadence agent answer
  <alias> <choice>`. A needs-me row is a suggestion to *look*, never
  proof a real menu exists — menu detection reads text shapes a
  transcript can quote, so the remedy command is only safe because
  `agent answer` re-probes the live screen and refuses anything that
  is not a corroborated menu.
- **`turn_silent_end`.** A still-`running` message whose pane probes
  idle for `silent_end_secs` over at least three consecutive samples —
  never one capture, never while a menu or a brokered request explains
  the wait — emits `{message, age_secs, last_activity, probe}` once
  per message: the provider ended without reporting. The flag is
  evidence, not resolution — the message stays `running` until an
  explicit report, reconcile, or the report bound. `agent_list`/`agent_show` expose
  `silent_ended` + `ended_secs` (the idle streak's age), `status`
  renders `ended?: <age>` beside an idle pane on a running message,
  and the overview needs-me row (`kind: silent_end`) gives the remedy
  `cadence send <alias> --nudge --text "finish and report …"` — a
  turnless nudge that pastes past the unreported turn (a plain
  follow-up `send` would queue behind it, one report-owing turn per
  actor, CAD-250); `cadence agent attach <alias>` is the manual
  alternative. Left alone, the turn goes `unknown` when
  `report_timeout_secs` runs out.
- **Budget resolution.** `jobs.stall_secs` (set at `job new
  --stall-secs`) wins for task-attached deliveries; otherwise the
  agent's `params.stall_secs` (launch param or live `agent set alias
  stall_secs=<n>`); otherwise the daemon default of 1800s. `0`
  disables firing — silence is still measured. Values accept an
  unsigned integer or digit string; negatives are rejected at
  `job_new` and both `agent` param validators. `silent_end_secs`
  resolves the same way minus the job layer — the agent's launch or
  live-set param, else the 600s default; `0` disables silent-end
  detection (idle-pane silence is then only ever a stall
  observation).
- **Views.** `agent_list`/`agent_show` add `silent_secs` and `stalled`
  while a turn runs — plus `pane_menu`, `ended_secs` and
  `silent_ended` when the sampled pane verdict warrants them; `job
  show`/`task show` add the same set to a task row whose kickoff is
  running. An agent with no turn still samples on the same throttle —
  a menu with nothing in flight surfaces as `pane_menu` and an
  `idle: true` event — but carries the menu line alone: stall and
  silent-end bookkeeping need a started turn.
- **Restart.** Watch state is in memory only: after a daemon restart
  the silence and idle-streak clocks for a still-`running` message
  start from the restart — no stall or silent end survives across
  it, and no episode replays.

## Jobs and tasks

Full semantics live in `docs/JOBS.md`. The wire contract in brief:

- `messages.task_id` attaches a delivery to a task (kickoff, `--task`
  follow-up, or `job_event` notification); `events.job_id`/`task_id`
  scope the job event view. Old rows read NULL — unattached.
- Task states: `draft → dispatched → running → review →
  verified|revising|blocked → done`, plus `failed`/`cancelled`;
  `job task reopen` returns `blocked|verified|failed` to `draft`.
- Only `source='job_dispatch'` messages drive task state — a dispatch's
  kickoff completing moves the task to `review` with `head_sha` from
  `result.sha`, else the last `SHA: <40-hex>` line of the result text,
  else NULL. Attachments for indexing never move state.
- `task_verdict` requires `state='review'`, `sha == head_sha` (NULL
  head_sha rejects naming `job task sha`), optional `--revision` equal
  to the current one, and a reviewer that is neither the assignee nor
  the revision's author. The reviewer is the verified caller (CAD-372):
  the agent the connection descends from, or `operator` on operator
  proof; `reviewer`/`pane` request fields are refused.
- `task_dispatch` legal from `draft|revising`, from
  `dispatched|running` once the live kickoff is terminal (new revision;
  a reconcile to `completed` takes the normal completion edge), and
  from `blocked` only with a `--to` reassign. A live kickoff returns
  `duplicate:true` with the live message id.

## Persistent monitoring

`monitor_register` creates a daemon-owned observer whose scope is the
listed task ids. A project name never expands coverage implicitly: every
task must belong to a job whose explicit `repo` equals `project`, and a
registration with the same id is idempotent only when all settings and
coverage match. Monitor rows, coverage, alert state, the event cursor, and
the separate `auto_dispatch_enabled` opt-in are stored in SQLite and survive
a daemon restart. The older `dispatch_enabled` bit still permits an
explicit `monitor_dispatch` call; it is not treated as background consent.

The monitor watch runs local SQLite checks on a bounded cadence. A
successful pass records `heartbeat_at`, `last_check_at`,
`last_success_at`, `next_check_at`, and the durable event cursor and moves
the observer from `degraded` to `active`. A failed pass records
`degraded` and the error. These fields describe observer execution only;
the daemon does not infer that a worker or provider is healthy from a
missing event, an idle row, or a successful database read.

Only concrete, task-scoped events from the monitor's explicit coverage
can create alerts. `(monitor, event fingerprint)` is unique, so a crash
or restart can repeat a read without creating a duplicate alert. Alerts
are local records with `open`, `acknowledged`, or `resolved` state. A
dispatch-blocked alert moves to `resolved` when the covered task acquires a
durable kickoff; operator acknowledgement remains a separate action. Delivery is reported
separately as `{configured:false,state:"unconfigured"}` in this bounded
increment; no provider, GitHub, or production notification is activated.

`monitor_dispatch` is a caller-requested safety gate. It requires an
active monitor, explicit dispatch opt-in, covered draft/revising task,
acceptance text, matching open project, explicit assignee, live idle
actor, no pending approval or queued work, and (where the endpoint has a
ready gate) an explicit verified readiness claim. It then delegates to
the existing `task_dispatch` transaction, including its group,
revision, lease and idempotency checks. When the separate automatic opt-in
is true, the periodic observer applies this same guard to the monitor's
fixed coverage set for draft/revising tasks. A refusal becomes one durable
`dispatch_blocked` alert per task, updated on later monitor intervals rather
than retried in a tight loop. The automatic path also refuses when quota
telemetry is absent, stale, or unbound. The allowance check uses a fresh
provider-owned sample bound to the assignee; only its explicit `available`
state is accepted, and missing/non-available provider facts fail closed. It is
admission evidence, not a standing permission grant. Neither path interrupts,
resumes, accepts
approvals, or changes worker state, and the observer never selects work
outside the explicit coverage set. Fairness, reviewer capacity, lease
recovery, and the independent-review handoff remain outside this slice.

## Approvals

Provider-initiated requests (e.g. `item/commandExecution/requestApproval`,
`item/tool/requestUserInput`, `session/request_permission`) pause the
agent at `waiting_input` and appear in `agent_requests`. `agent_respond`
answers them per type; nothing is auto-accepted. A request may also be
resolved outside Cadence — an attached TUI answering the approval makes
the provider emit `serverRequest/resolved`, which drops the pending
handle (`input_resolved`); a late `agent_respond` is then `rejected`.
The agent stays `waiting_input` while other requests remain pending.

Brokered requests share the same model through a second entry point:
`request_open` registers one (method `cadence/<kind>`), `request_wait`
blocks the caller until the answer is parked or the handle is gone, and
`request_close` retires a handle the caller abandoned — all three only
from the requesting agent's own connection (CAD-376). `agent_respond`
branches on the method — `cadence/*` requests accept `--decision
accept|decline` (with an optional `--reason` on decline) and park the
answer for the waiter instead of calling `adapter.respond`; provider
methods keep the per-type responses above. Pending entries are
in-memory: an actor exit or daemon restart reads as `closed` to any
waiter. Today the only producer is the `mcp-permission` server backing
brokered claude approvals (see the managed claude section).

## Build slots (CAD-113)

One bounded, fair, observable scheduler for cargo build/test work on a
host, so lanes queue instead of thrashing. The simplest consumer is
`cadence build-slot run test -- cargo test --lib` — `run` acquires
then *execs* the command, so the slot's holder is the real build
process and its exit frees the slot. A manual wrap works too:
`token=$(cadence build-slot acquire build --pid $$ --wait-secs 600);
cargo build; cadence build-slot release $token` — `acquire` requires
`--pid` (the hold binds the process that actually lives for the work;
`$$` inside a shell wrapper) while `release` defaults it to the
calling shell — the daemon owns the queue, the CLI only polls.

Caller identity is bound to the connection, never to the request. On
every `slot_*` RPC the daemon takes the peer's pid from `SO_PEERCRED`
and walks `/proc` ancestry up to init; the caller's lane is the alias
of the *nearest* registered pty pane on that chain (its own pane beats
any outer one, so resolution never depends on map order) — or, when an
enrolled managed endpoint is nearer, that endpoint's strict binding
(see *Managed endpoints* below). A request's
`lane` field is advisory — it is never consulted for authority — and a
`pid` field must name the socket peer itself or one of its ancestors
(`acquire --pid $$` legitimately claims the invoking shell); anything
else refuses rather than rebinds. The walk is fail-closed: an
unreadable `/proc`, an incomplete chain, or no pane match refuses the
call outright. There is no `operator` fallback — a caller detached
from every registered pane holds no lane at all, so `setsid` or a
detached helper cannot borrow an identity it was never given.
`slot_status` applies the same rule to visibility: a hold's `token`
appears only to the connection whose derived lane matches the hold
*and* whose ancestry includes the hold's pid.

A registered pid is a process only together with its start time
(CAD-385). Every agent row records `pid_start` — `/proc/<pid>/stat`
field 22 of the pid it records (the pane pid for `pty`, the provider
pid for managed endpoints) — on each open, re-attach and hot-restart
adoption, and every pid → alias mapping (slot identity, the memory
caller verifier, `agent answer`, operator proof, board attribution,
`rollout release --force`) compares it with the live process first.
A row whose pid now names another process, or none, is stale: it
matches nothing, and the caller is placed exactly as an unregistered
process. A row with no recorded start (written before schema v14, or
with `/proc` unreadable) cannot be told from a reused pid, so it fails
closed: a caller descending from it is refused, and operator proof
still counts it as a pane. `cadence doctor --host` (check
`pane-identity`) lists such rows; the remedy is `cadence daemon
restart`, whose recovery clears every recorded pid before each live
pane is adopted again with its start recorded.

### Managed endpoints — strict enrollment (CAD-230 phase a)

A managed Claude or Codex worker (`managed`/`managed-ws` endpoint) has
no tmux pane, so the pane rule above can never admit it. Instead, when
the daemon opens such an endpoint it mints an **enrollment** from the
pid the adapter recorded — never from anything a caller says. The
enrollment binds the owner actor (the alias), the owner generation
(the owner row's registration instant, endpoint generation and
provider pid), and the provider process as root = worker, each as an
exact `(pid, /proc starttime, uid)` identity, with `issued_epoch` and a
daemon-capped `expires_epoch` (24h). The daemon never enrolls itself,
init, or a process of another uid; the same owner generation and root
identity renew an existing enrollment (resume), anything else
supersedes it.

On every slot call the chain walk above looks for the *nearest*
identity node: a registered pane keeps the legacy binding unchanged; an
enrolled root makes the call **strict**. A strict caller must be the
enrolled root itself or reach it through a complete ancestry read at
call time, every hop running as the daemon's uid (real and effective),
no parent younger than its child (a pid recycled mid-walk fails), and
the root exactly the recorded process (starttime). Only the peer and
its verified *non-root* ancestors may be claimed as a holder (CAD-276):
the enrolled root outlives the work, so `acquire --pid <root>` from a
tool subprocess is refused (the root may hold only as its own peer),
while `build-slot run` binds its own pid and `acquire --pid $$` the
invoking shell. The holder's identity is recorded with the hold. The owner row is revalidated
before every slot call: a missing row, a closed endpoint or a changed
owner generation revokes the enrollment. A failed strict verification
refuses — it never falls through to an outer pane, and an alias, lane,
pid, argv or `CADENCE_ALIAS` in the request or environment never
grants ownership. A revoked or expired enrollment stays as a
**tombstone** for as long as any hold names it or its exact root
process (pid + starttime) is alive or unknown: the tombstone keeps
winning the nearest-identity check, so the provider and its tool tree
are refused new work instead of falling through to a pane registered
above them. It is pruned only once nothing holds under it and its root
is proven dead.

Several enrollments may share one root — a same-root supersession (the
same provider process re-enrolled under a new owner generation) keeps
the old one as a tombstone while a hold names it. A caller is matched
against them in one deterministic order (CAD-276): live first
(`active`, then `expired`, then revoked), then the most recently
issued, then the enrollment id. A release runs as the root's
enrollment whose id matches the hold, so the old holder can still give
back a hold taken before the supersession; the exact-holder rule is
unchanged. A shared root never makes restart reject the state file,
but every enrollment on the same root must share an
owner — across owners the pair is invalid (strict blocked, file kept
byte-identical as evidence), and the daemon never enrolls one exact
process for a second owner.

Residual: an orphan of a dead provider re-parented under a pane — only
possible with a subreaper below that pane — has no enrolled root left
on its chain, so it falls to the pane's legacy binding. (Orphans of
trees the daemon launched re-parent to the daemon itself, its child
subreaper — CAD-308 — never to a pane.)

### Exec-bound holds (CAD-230 phase b1)

`build-slot run` sends `exec:true`: the daemon then requires the
claimed `pid` to be the socket peer itself (an ancestor is refused) —
the `cadence build-slot run` process, which execs into the command
once granted. `exec` keeps the pid and the `/proc` starttime, so for a
strict caller the recorded holder `(pid, starttime, uid)` *is* the
running build; `slot_status` flags the hold `exec_bound`. A pane
caller's `run` keeps its legacy hold (the pid check is the only
difference — `run` always claimed its own pid).

A strict hold ends with its holder without anyone's cooperation: the
daemon re-reads every strict holder once a second and frees a hold
whose holder is proven dead — gone, a recycled pid (new starttime), or
a zombie its parent has not reaped yet whose thread group is down to
itself (`holder exited`; a zombie leader with live threads is
`unknown`) — through the
same fail-closed writer (an unwritable file keeps it accounted).
`unknown` still never frees. A holder's `release` of a hold the watcher
already freed answers `released:false` with the reason, as a same-call
reap does. Only the holder or a verified descendant can release it
(unchanged); a recycled pid is a different process — it reads as the
holder's death, and its own request gets a fresh hold, never the old
token. `exec_bound` describes how the hold was taken (the daemon
verified the claimed pid was the peer); it grants nothing by itself.
A process the command leaves running in the background is not the
holder — the hold ends with the exec'd process. Legacy holds keep the
reap-on-call rule above.

### Daemon-launched runners (CAD-230 phase b2)

A caller with no pane and no managed endpoint (a subagent, an external
worker, a CI-like run) has no slot identity, so it cannot queue. It can
ask the daemon to run one of its project's **recipes** instead, declared
only in the tracker's `project.yaml`:

```yaml
build:
  recipes:
    check:
      argv: [cargo, clippy, --all-targets, --locked, --, -D, warnings]
      cwd: .                  # repo-relative (default: checkout root)
      env: [PATH, HOME, CARGO_BUILD_JOBS]  # names passed from the daemon's env
      kind: build             # slot pool: build (default) | test | suite
```

`cadence build-slot launch check [--project p] [--worktree <path>]`
names a recipe, a project and optionally a checkout; nothing about the
command. The daemon:

1. authorizes the requester from the connection — a pane agent, an
   *active* enrolled managed endpoint, or operator proof (the same
   positive proof `slot_reconcile` needs); otherwise it refuses naming
   the rule. A runner's attached process tree is refused (a recipe must
   not nest `build-slot`), and so is a detached descendant that keeps
   the runner's environment; a revoked or expired endpoint is refused
   too. A detached descendant that scrubs that environment is the known
   residual below (CAD-308);
2. resolves the launch **intent** from config: the recipe (unknown →
   refused, naming the recipes the project defines; `argv` non-empty,
   `env` POSIX names and never `CADENCE_*`, `cwd` repo-relative with no
   `..` and still inside the checkout once resolved), the checkout (the
   `--worktree` path, or the cwd when the CLI resolved the project from
   it, else the project's first repo) — whose git common-dir root must
   be one of the project's registered repo paths — and its `HEAD`. The
   **digest** is sha256 over project, recipe, kind, argv, checkout, cwd,
   env names and `HEAD` at resolution — an *intent record* of what was
   asked for, not an attestation of the working tree's content; a tree
   with uncommitted changes (tracked edits
   or untracked, non-ignored files) is recorded as `dirty` — read at
   launch and again at gate-open — and is not part of the digest. The
   daemon runs git (`rev-parse`, `status`) inside the requester-chosen
   checkout; those reads run with `core.fsmonitor=false`,
   `GIT_OPTIONAL_LOCKS=0` and only `PATH`/`HOME`, so a checkout's config
   cannot make the daemon run a program;
3. writes the receipt `pending`, digest bound to a fresh `run-<hex>` id,
   **before** anything is spawned;
4. spawns `/bin/sh` running a gate in its own process group, in the
   recipe's cwd, with ONLY the allowlisted names (values from the
   daemon's environment) plus `CADENCE_RUNNER_ID`/`CADENCE_RUNNER_DIGEST`,
   stdout and stderr appended to `<state>/runners/<id>.log`. The gate
   blocks on its stdin;
5. enrolls that exact process as root = worker (`runner_id`, owner
   generation `runner:<id>:<digest>`, accounted to the requester's
   lane — never revalidated against an agent row, never superseded by
   the requester's endpoint) and queues it for the recipe's pool as a
   strict, exec-bound request;
6. on grant re-reads the checkout's `HEAD` (moved → refused, the gate
   never opens), records `running`, then writes `go <runner_id>`: the
   gate `exec`s the recipe with stdin from `/dev/null`, keeping the
   enrolled pid and starttime. A refusal, a queue timeout
   (`wait_secs`, default 600) or a closing daemon closes the gate
   instead — it exits 125 and the recipe never starts;
7. waits for the exit (the daemon is the parent). Once the recipe's
   process has exited — observed before it is reaped, so its pid (the
   group id) cannot be reused — whatever it left in its process group is
   SIGKILLed: nothing keeps building outside the slot. It then records
   `exited` with `exit_code`/`signal`, frees the hold on proof of death and revokes
   the enrollment (pruned once nothing holds under it), and emits
   `runner_finished` on the requester's lane.

The CLI streams the log and exits with the recipe's code (128+signal
when killed); a runner that never ran exits with the refusal. Receipt
states: `pending`, `queued`, `running` (in flight), then `exited`,
`timed_out`, `refused`, `cancelled` or `unknown`. **Restart:** a runner
in flight when its daemon stopped is `unknown` with `complete:false`
and its `last_state` at the next boot (`runner_unknown` event) — never
relaunched (resumption is CAD-236's). Its enrollment is revoked; its
hold stays accounted under the tri-state rule until the process is
proven dead. A closing daemon opens no gate and writes no further
runner state. An operator-launched runner is accounted to the lane
`(operator)`, which no agent alias can take.

Not yet provided: a cancel verb (stop the runner's process to end it)
and a run-time cap — a hung recipe holds its slot until it exits, like
any strict hold; and receipts and logs under `<state>/runners/` are
kept without a size cap or expiry (a recipe's output lands in the state
dir's filesystem).

Threat model: same-uid processes are not a hostile boundary (as above).
What a runner authorizes is "this admitted requester may run this
project's operator-declared recipe against a checkout of one of its
registered repos" — the checkout's content (build scripts, tests) is
the requester's code, as with any build. The daemon never takes argv,
cwd, env, a pid, a lane or an alias from a request, and the slot holder
is always the process the daemon itself spawned and verified. A
runner's tree carries `CADENCE_RUNNER_ID`, so a descendant that detaches
from it (`setsid -f`) but keeps that environment is still refused as
the operator.

A descendant that detaches AND scrubs the runner's environment — `env
-u CADENCE_RUNNER_ID -u CADENCE_RUNNER_DIGEST setsid -f …` with stdio
redirected — carries no marker, but it no longer re-parents to init:
the daemon is its child subreaper (below), so it stays a daemon
descendant and operator proof refuses it
(`build_slot_launch_detached_scrubbed_descendant_is_refused`; before
CAD-308 it launched runners as `(operator)`).

### Daemon child subreaper (CAD-308)

`daemon run` marks its process `PR_SET_CHILD_SUBREAPER` before it
spawns anything (`daemon.log`: one `subreaper:` line; `health` answers
`child_subreaper: true`). A process in any tree the daemon launched —
a runner recipe, a managed provider and its tools, the private tmux
server it starts and that server's panes — that detaches (`setsid -f`,
a double fork, `daemon(3)`) re-parents to the daemon instead of init
when its parent exits. It stays a daemon descendant, so operator
proof's descendant rule refuses it whatever its env, session or stdio
(`slot_reconcile`, `slot_launch` as `(operator)`, `approval_record`,
`approval_revoke`).

The daemon reaps those adopted orphans, and only those. Every child
cadence spawns goes through `cadence_agent::reaper::spawn` (clippy's
`disallowed-methods` refuses `Command::{spawn, output, status}`
elsewhere), which records the child's pid and `/proc` start time while
holding a gate the reaper takes exclusively — so a pass never sees a
spawned child before it is registered. Once a second the reaper prunes
registrations whose process is provably gone or replaced, peeks the
next exited child with `waitid(P_ALL, WNOWAIT)`, and reaps by pid only
a child that is not registered; when an owned zombie heads the queue it
sweeps the other children from `/proc/self/task/*/children`. Adapters,
runner threads and `git` calls keep collecting their own children's
statuses. In-process daemons (a test binary's `daemon::serve_with`)
never enable any of this, and a process that never enabled it (every
CLI, `cadence ui`) registers nothing — there is no reaper to prune its
registry.

Limits of the reaper: the sweep reads `/proc/<pid>/task/*/children`,
which needs `CONFIG_PROC_CHILDREN` (Ubuntu and common distribution
kernels have it). Without it, adopted zombies queued behind an owned
zombie wait until its owner collects it — the reaper still never takes
an owned status; it only reaps later. A `Child` its owner drops without
waiting stays registered and its zombie stays unreaped (as before
CAD-308), and while it heads the queue each pass sweeps `/proc` once a
second.

**Behaviour change — orphans no longer see `getppid() == 1`.** An
orphan of a daemon-launched tree has the daemon as its parent, so a
helper that polls `getppid() == 1` to exit once orphaned (some Node MCP
or language servers under a managed provider) no longer notices and
keeps running as a live daemon child. The daemon never kills adopted
processes; `health.adopted_live` / `adopted_oldest` (shown by `cadence
daemon status`) make accumulation visible. Helpers that watch stdin EOF
or `PR_SET_PDEATHSIG` are unaffected.

**Residual (CAD-280):** the subreaper covers trees the *daemon
instance* launched. A process that asks a long-lived process OUTSIDE
them to start it — a tmux server this daemon did not start (one that
outlived a daemon restart, the operator's own, an agent-started one),
`systemd-run --user`, cron/at, `ssh localhost` — is not a daemon
descendant, and with env and stdio scrubbed it still passes operator
proof; so does a setsid detach from a pane whose tmux server is such a
process (`slot_reconcile_refuses_a_managed_tools_setsid_detach` pins
the pane case). Operator-by-positive-proof (CAD-280) closes that
class.

*Deviation from design v3:* v3 admits only the exact root/worker
process. A managed provider's builds run in its tools' subprocesses
(the provider's shell running `cadence build-slot run … cargo …`), so
the strict rule also admits the enrolled root's **descendants — only
through the complete, verified ancestry chain above**, never by a
detached relay (a `setsid` + double fork leaves the chain and is
refused). Same-uid processes remain outside a hostile boundary.

Authorization and accounting stay separate. Revocation (endpoint
closed, owner drift, supersession) and expiry refuse new acquires and
drop the enrollment's waiters (`slot_wait_dropped`, before any
ranking), but never free a hold: a strict hold is freed only by its
exact holder's release (the same enrollment, the recorded pid *and*
starttime) or by proven death. Its liveness is tri-state — `alive`
retains, `dead` (gone, or the pid recycled) frees, `unknown`
(unreadable or inconsistent `/proc`) stays accounted. Past
`max_hold_secs` a strict hold is `expired_pending_reconcile`, still
occupying its slot. `slot_status` exposes `auth_state`, `liveness`,
`accounting`, `owner_generation` and `reconcile_required` per hold and
the enrollments themselves; `slot_reconcile` is the only operator
path and cannot free a live or unknown hold. So `reconcile_required`
is set only where reconcile can act — a holder proven dead whose free
has not landed while the strict writer is available — and any other
hold that needs a hand names its `remedy` instead: `holder alive —
release from the holder or stop it` (past `max_hold_secs`), `liveness
unknown — restart the daemon after verifying`, or a dead holder whose
free could not be written (restart once `slots.json` is writable).
`cadence build-slot status` prints both inside the hold's `[strict …]`
flags.

Two pools share one queue: `build`/`test` requests draw on
`build_slots` (default 3), `suite` requests on `suite_slots` (default
1) — independent, so a queued full suite never starves ordinary
builds. Grant order is FIFO with two modifiers: `test`/`suite`
requests from a configured *priority lane* (`[host] priority_lanes` —
the reviewer lane) outrank ordinary requests, and a `(lane, kind)`
waiting continuously longer than `starve_secs` (default 900) jumps to
the front. Seniority belongs to an *unserved* wait and is carried by
exactly one waiter — the lane's eldest for that kind: a caller that
re-queues under a new request id keeps the lane's accumulated wait
for up to one waiter TTL after its last poll, but later arrivals of
a burst stamp their own arrival and queue behind it, so one lane can
never multiply an old anchor into N front-running requests. Every
grant for that `(lane, kind)` restarts the anchor — a lane can never
keep an old anchor alive by always having one more request queued —
and an inherited stamp is clamped to `starve_secs` at enqueue time,
so rank 0 stays true FIFO: a two-hour waiter still beats a
901-second one, and any request is granted within the bound once it
reaches the front.

A slot is a daemon-minted `slot-*` token bound to (lane, pid,
pid-starttime): `release` must name the holding lane and pid — and
because both come from the connection's own identity (above), one
caller can never free another's hold. A re-poll whose `request_id`
matches a hold adopts it only on an exact `(request_id, pid, lane,
kind)` match — a second process sharing a natural request id queues
like everyone else. A holder whose process dies or whose pid is
recycled is reaped on the next acquire/status (a strict hold also by
the daemon's own once-a-second watcher, CAD-230b) — a killed agent
frees its slot, nothing is ever killed for one — and a hold past
`max_hold_secs` (default 7200) is reaped as `hold expired` so a
forgotten hold cannot wedge a pool. Waiting is client-side:
`slot_acquire` answers instantly with granted-or-position, and a
polling caller keeps its place by refreshing `last_poll`; a request
that goes silent past the waiter TTL (30s) or whose pid dies drops
out of the queue. `probe:true` is the non-mutating read — it grants
or reports position without ever joining the queue. All slot ages
ride a monotonic clock: an NTP step or suspend cannot age a waiter
or expire a hold.

Holds persist to `<state>/slots.json` (atomic + fsynced, mode 0600)
— the record is `(token, request_id, kind, lane, pid, pid-starttime,
acquired_epoch)`. The first strict record upgrades the file to the
version-2 envelope `{"format":"cadence-slots","version":2,
"state_generation", "enrollments", "holds" (strict, each naming its
enrollment and holder identity), "legacy_holds" (the v1 rows)}`; a v1
file keeps its shape until then, and its rows only ever migrate to
`legacy_holds` — never to strict authorization. Every write, legacy
included, serializes the whole state, so legacy traffic never drops a
strict record. Strict writes are fail-closed: the next state is
written before it is applied, so a failed write grants and frees
nothing and makes strict admission unavailable. An unreadable or
unparsable file, an unknown version, anything that is not a
well-formed envelope (`{}`, `null`, `[]`, a v1 file whose `holds` is
not a list), a malformed or duplicate record or a strict hold naming
no enrollment rejects the file at boot: it is
kept untouched as evidence, strict admission is unavailable
(`strict.available:false`), and legacy callers carry on in memory.
Boot validates in order — the envelope, then enrollments (expiry; a
dead or unreadable root revokes), then strict holds (tri-state), then
legacy holds (below). On daemon boot each persisted hold is revalidated:
a hold survives restart only while its recorded process is still the
same live process (pid + starttime), and the dead are dropped with a
`slot_released` event (`holder died` / `pid recycled`) rather than
silently re-granted. Waiters do not persist, and neither does
seniority — it measures an unserved wait and no waiter survives a
restart, so every caller re-polls into a fresh anchor. One deadlock
guard applies: a *process* may never *queue* for one pool while
holding a slot in the other (the guard keys on `(lane, pid)` — two
shells sharing a lane name never block each other) — a grant that
never waits is always allowed, so the safe order is simply "wait
only while holding nothing". The queue is bounded twice over: 128
waiters total, 32 per lane. `CADENCE_SUITE_LOCK` keeps working
underneath as the test-process suite slot — the daemon queue is the
observable layer above it.

Configuration rides the `[host]` table in `pm.yaml` (all optional):

```yaml
host:
  build_slots: 3        # concurrent build+test grants
  suite_slots: 1        # concurrent full-suite grants
  jobs_per_lane: 4      # CARGO_BUILD_JOBS `issue start`/`dispatch` injects
  starve_secs: 900      # never-starve bound on (lane, kind) seniority
  max_hold_secs: 7200   # a forgotten hold is reaped past this
  priority_lanes: [qa-1]  # test/suite requests outrank ordinary ones
  # load_warn_ratio — doctor --host load warn = ratio x cpus (fail
  # 2x). Unset: derived from the slot plan — the farm is meant to run
  # (build_slots + suite_slots) x jobs_per_lane deep, so the default
  # warns above 1.25x that plan (floor 1.0), never below it.
  io_stall_warn_pct: 30 # doctor --host io stall warn %
  io_stall_fail_pct: 60 # doctor --host io stall fail %
  # agent_gc_older_than_secs — unset (default) keeps the daemon's
  # agent-gc timer OFF. Set, it removes dead agent registry rows idle
  # longer than this (7-day floor), at most hourly. Records only:
  # frees no memory and no disk; removed agents cannot be resumed.
  # auto_stop_idle_secs — the daemon stops (resumably) an agent idle
  # this long with nothing queued/running/awaiting report/unknown.
  # Unset = 3600 (ON); 0 = off; below 600 is raised to 600.
  auto_stop_idle_secs: 3600
  auto_stop_idle_secs_by_provider: # per provider; 0 = off for it
    claude: 7200
```

`cadence issue start`/`dispatch` write `<worktree>/.env` atomically
(mode 0600, existing modes and foreign lines preserved, symlinks
refused) with `CARGO_BUILD_JOBS=<jobs_per_lane>` and
`CADENCE_BUILD_SLOT=<cadence binary>` so a worker never has to
remember flags. `build-slot run` exports `CADENCE_BUILD_SLOT_TOKEN` /
`CADENCE_BUILD_SLOT_PID` / `CADENCE_BUILD_SLOT_LANE` to the command so
a nested script can release its own hold early. Observability:
`slot_acquired` / `slot_waited` / `slot_released` events land on the
requesting lane's event stream — and no event ever carries a token:
a token returns only in the `slot_acquire` RPC reply and in the
holding connection's own `slot_status`, because lane streams are
readable by any local caller and token+lane+pid are the whole release
credential. `cadence status` carries a `slots:` footer line,
`cadence build-slot status [--json]` shows holders and waiters (your
own connection's holds show tokens; others' show identity only), and
`doctor --host`'s `load` check reports load, io stall and the queue.
A caller with no derivable pane identity — the operator's own shell
included — sees the slot calls refused; the footers then simply omit
the slot line rather than fail.
Nothing here kills a process or cancels anyone's work.

## Worker loop (CAD-431)

A ticket the master dispatches (`master_dispatch`) enters the loop: the
daemon keeps one record per ticket in `<state>/delivery.json`, and only
the daemon writes it. Report files never move a ticket through the loop.

1. **Done → review.** The report router (the same pass that routes
   reports to the master) takes the worker's newest `done` report filed
   after the dispatch. It needs `sha:` (the head) and `pr:`
   (`https://github.com/<owner>/<repo>/pull/<n>`); without them the
   worker is told to file again. The PR must be in one of the ticket's
   project repos (`repos[].remote`, compared with `normalize_remote`,
   case-insensitive) and held by no other live ticket; otherwise the
   worker is told why and the record does not change. Every surface
   shows the PR as `owner/repo#n` (`pr_ref`). The daemon picks the
   reviewer: never the worker or anyone in its group line (its upstream
   chain, and every agent whose upstream chain reaches the worker — so a
   worker cannot staff its own reviewer), never the master, never a
   fenced or disabled agent or an inbox, never an agent in the record's
   `excluded` list. The previous round's reviewer keeps the ticket while
   it qualifies; otherwise an agent of a different provider from the
   worker's wins, else another session of the same provider. With
   nobody eligible the record is `unstaffed` (a Needs-you row) and the
   router retries every pass. The kickoff is composed by the daemon on
   one line: the PR, the head, the acceptance criteria (or the ticket
   path when they do not fit the endpoint's kickoff ceiling), and the
   pinning rules.
2. **Verdict.** `cadence report file --task <ID> --kind verdict --file
   <f>` sends the report to `report_verdict`. The daemon derives the
   caller from the connection and files it only for the assigned
   reviewer, while the ticket is `reviewing`, for exactly the head under
   review. REVISE goes back to the worker as a message pinned to the
   ticket (the record returns to `working`); the worker fixes, pushes
   and files a new `done`. The second REVISE does not go back: the
   record is `escalated`, a Needs-you row for the operator. PASS makes
   the record `passed`.
3. **Merge decision.** GitHub facts come only from the operator's own
   process: `cadence delivery sync` reads each PR with the operator's
   `gh` (head, CI rollup, diff stats, auto-merge) and hands them to
   `delivery_observe`. A PASS whose head GitHub shows open and green is
   one `merge_decision` row in Needs-you: owner, age, PR link, the verdict's
   first line, diff stats. `cadence delivery merge <ID>` checks with
   the daemon, re-reads the PR, then runs `gh pr merge <n> -R
   <owner/repo> --auto --squash --match-head-commit <reviewed sha>` and
   records `enqueued`. `cadence delivery decline <ID> --reason …`
   records the reason. Both are refused for any agent before `gh` runs.
4. **Head moves.** A new `done` sha, or an observed head that differs
   from the reviewed one while `reviewing`, `passed` or `enqueued`,
   re-enters review at the new head, and the old PASS is stale. A head
   that moved without a `done` from the worker may have been pushed by
   the reviewer on duty, so that reviewer joins `excluded` and the new
   head goes to someone else.
   Whenever auto-merge is on for a head that is not the enqueued,
   reviewed one, `delivery_observe` answers `disable_auto: true` and
   `sync` runs `gh pr merge <n> --disable-auto`; until an observation
   shows it off, Needs-you carries an `auto_merge_on` row. A `MERGED`
   or `CLOSED` observation ends the loop.

Pinning: `gh pr merge --match-head-commit <sha>` sends the SHA as
GraphQL `expectedHeadOid` on every path — `mergePullRequest` for a
direct merge, and the auto-merge / merge-queue mutation when `--auto`
is given or the base branch requires a queue (cli/cli
`pkg/cmd/pr/merge`). GitHub checks it when auto-merge is enabled or the
PR is enqueued, not again afterwards: a later push is not re-checked,
which is why the loop disables auto-merge on every moved head. `--auto`
is kept for repos without a queue too — there the same mutation pins
the head and waits for required checks instead of merging an unfinished
run.

A `delivery.json` that exists but does not parse is never read as
empty: `delivery_list` (so `delivery ls` and `delivery sync`) fails,
Needs-you shows a `delivery_unreadable` row, and `master_dispatch`
refuses before dispatching anything. Writes go through a synced
temporary file, a rename and a synced directory, under the lock.
Verdicts reach the master only from `report_verdict`; the report
router never routes a `verdict` file it finds under `reports/`.

The daemon never runs `gh`, and no agent environment needs GitHub
credentials for the loop. `cadence delivery sync --watch <secs>` keeps
the observations current. The board's `POST /api/delivery/<ID>/merge`
(`{}`) and `POST /api/delivery/<ID>/decline` (`{"reason"}`) keep the
operator rule of the chat-first Home (CAD-328): an agent-attributed
request gets 403 and anything short of positive operator proof on the
HTTP peer gets 403 `operator_proof`, before `gh` runs. Merge runs in
the board process, which the operator started, with the operator's
`gh`.

## Recovery

On daemon start: messages in `submitting`/`running` become `unknown`.
`attention` fences survive the restart intact — recovery clears the
dead pid/endpoint/generation but keeps the state and its recorded
error verbatim, so the serve loop still sees the fence. Enabled agents
relaunch *unless* fenced — state `attention` or an unreconciled
`unknown`. A fenced agent is skipped before any actor or provider
spawn: a `relaunch_skipped` event records the reason and the sweep
continues with healthy agents. An unknown fence keeps the provider's
own reason on `agent.error`. A missed render observation does not prove
the delivery did not happen. Inspect that message and side effects
before reconciling; reconciliation is an explicit operator decision,
not an automatic interrupted or completed result. CLI `agent unfence`
resumes by default — do not follow it with a second resume.
`--no-resume` reconciles without resuming. The bare RPC defaults to
reconcile-only. A reconciled agent lands `stopped` and disabled — the
same condition as an operator stop — so the next restart leaves it
stopped rather than relaunching it. A pty pane that survived the
restart is re-adopted by `open` when resume actually runs — the
reattach path verifies the pane still owns the stored native session
lock, so resume converges on the same Devin session rather than a fresh
one. `agent capture` still needs a live actor; it does not read a
detached pane. `agent stop` kills a surviving pane. A pane that adopted
a *different* session fails closed and the hint is `agent remove` +
`join -r` — retrying resume mints a new provider session each time.

**Hot restart.** The one exception to fence-on-start is a stop the
next daemon can *recognize* as clean. A graceful shutdown —
`shutdown`, `daemon stop`/`restart`, or a handled SIGTERM/SIGINT —
finishes actor cleanup and then, as its last act, writes
`shutdown.json` into the state dir: the daemon's instance id
(recorded at start in `daemon-instance`), the stop time, and every
pty message still `running` **or** `submitting` with its message id,
turn id, endpoint generation, pane pid and native session.
`submitting` rows are recorded so the restart can name why they
fenced — an unproven paste is never adopted, it simply fails the
`running` check. A pty pane is never interrupted during the drain —
a Ctrl-C could kill the very turn being preserved — while a
`submitting` paste finishes its render check or times out inside the
bounded wait, so a row that reaches `running` before the marker is
written is adopted like any other proven turn. On the next start the
marker is consumed exactly once, and only when it matches the
immediately preceding recorded run and is younger than its bound
(~15 minutes): missing, mismatched, stale or unreadable markers all
take the crash path above.

"Provably clean" is a statement about ordering, not authenticity:
the instance stamp is an unauthenticated file with exactly the trust
level of the sqlite file beside it. It proves *this* daemon finished
its own drain before writing the marker — nothing more — and the
pane checks below are what make adoption safe even if the marker
itself were doctored.

For each recorded entry the store-level checks run first — the
message must still be `running` with the same `turn_id`, the token
must embed the recorded generation, the agent still enabled and
unfenced — then the actor re-validates the pane itself: alive, the
same pid, still holding the recorded native-session lock. Until that
proof completes the agent's `generation` stays cleared, so a report
landing in the window is refused as stale rather than finishing a
turn that may be gone; a `running` row whose token predates the
snapshot generation is refused at marker-write time, never recorded.
When every check passes the messages stay `running` — all of the
agent's recorded turns, not one — the agent is never fenced, the
endpoint and the recorded generation are republished (so the
original tokens still validate `message_report`), `turn_adopted` is
emitted per turn, and the stall watch arms from the new daemon's
start. Any failed check falls back for that agent alone — message
`unknown`, agent `attention`, and `turn_adopt_refused` naming the
check — alongside healthy agents that adopt or relaunch normally.

The safety argument: an uncertain outcome is a paste that never
proved it rendered, or a provider process whose survival cannot be
shown. A turn whose paste rendered in a pane that verifiably
survived a provably clean stop — same pid, same native-session lock,
same generation — is neither, so re-adopting it risks no replay and
no double submission. Managed endpoints are never adopted: the
provider process dies with the daemon either way, so a managed
in-flight turn stays `unknown` across any restart. `daemon restart`'s
before/after table reports the outcome per agent under `TURN`:
`kept`, `fenced`, or `-` — a fenced turn exits non-zero.

**Stale running rows (CAD-250 reconcile path).** Stores written before
one-turn-per-actor can hold many `running` pty rows per alias (the
2026-09-22 audit measured 91 on one Devin PM), none ever reported.
Nothing migrates or deletes them. On the next daemon start they take
the ordinary paths above: a crash start turns them `unknown`; a hot
restart adopts them (they are proven turns on a surviving pane), they
read `awaiting_report`, and the actor's first pass retires every one
whose `report_timeout_secs` has run out to `unknown` — one
`report_timeout` event and one `turn_finished` each, one notice where a
`reply_to` exists — and fences the agent. Every transition is an event
on the agent's stream; the rows and their history stay until an
operator reconciles them (`agent unfence`).

## Agent skill

The binary vendors `skill/cadence/SKILL.md` (`include_str!`) — the
protocol primer a cadence-managed agent reads. `cadence skill install`
writes it to `$HOME/.agents/skills/cadence/SKILL.md` and links `cadence`
→ that dir inside `~/.claude/skills`, `~/.cursor/skills` and
`~/.copilot/skills` (`.agents` has no XDG equivalent — all under
`$HOME`). A missing or wrong-target symlink is created/replaced; a real
dir or file named `cadence` is left alone and reported under `skipped`.
`cadence skill status` reports installed/content-match/per-dir link
state. Every `daemon run` re-syncs stale or missing copies and links —
a rebuilt binary propagates skill changes without a manual install;
the refresh logs one line to `daemon.log`, never to stdout.
