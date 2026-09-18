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
`{"ok": false, "error": {"kind": "rejected|provider|unknown|internal", "message": "..."}}`

Error kinds:

- `rejected` — invalid or disallowed request; nothing was attempted.
- `provider` — the provider explicitly rejected the request.
- `unknown` — transport failed after the request may have reached the
  provider. The attempt is preserved for review; never retried blindly.
- `internal` — local runtime failure (I/O, storage, protocol).

## Methods

| Method | Params | Result |
|---|---|---|
| `health` | — | `{state:"ready", protocol:1, capabilities:[...]}` |
| `shutdown` | — | `{state:"stopping"}`; daemon stops actors (bounded) then exits |
| `agent_register` | `alias, provider, cwd?, endpoint_kind?, role?, sandbox?, instructions?, params?` | `{alias,state:"starting"|"idle",provider}` |
| `agent_list` | — | `{agents:[Agent+tasks+capabilities]}` — `tasks` names the alias's non-terminal task assignments; `capabilities` is the registry descriptor |
| `agent_show` | `alias` | `{agent, messages, event_cursor, queued, unknown}` — `unknown` counts unreconciled unknowns fencing the agent; `agent.capabilities` is the registry descriptor |
| `agent_send` | `alias, text, message?, reply_to?, source?, task?` | `{message,state,duplicate}` — `task` attaches the delivery to a task for indexing |
| `agent_ask` | `alias, text, message?, reply_to?, wait?` | the `Message` row; state may be non-terminal if `wait` expired |
| `agent_events` | `alias, after, wait(<=30)` | `{events:[Event], cursor}` |
| `agent_requests` | `alias` | `{requests:[{request,method,params}]}` |
| `agent_respond` | `alias, request, decision?|answers?` | `{state:"answered"}` |
| `agent_ready` | `alias, by?` | `{state:"ready-claimed"}` — single-use readiness claim for `pty`; `by` records the claimer |
| `agent_capture` | `alias` | `{capture}` — current pane contents (pty) |
| `agent_probe` | `alias` | `{probe:{idle,reason,...}}` — analyzed pane state without claiming (pty) |
| `agent_set` | `alias, patch` | merges an allowlisted param into the live agent — today only `auto_ready` (`"verified"` or null-removal, pty only); `{state:"updated"}` |
| `agent_inbox` | `alias, after?, wait?` | drains queued inbox messages, completing each `via=inbox_read`; `{messages, cursor}` |
| `message_report` | `message, token, kind: ack|result, text?, sha?` | `{state:"reported"}` — explicit PTY ack/result; `sha` names the produced commit for task-attached kickoffs |
| `message_reconcile` | `message, status: interrupted|completed|failed, note?, by?, sha?` | `{state:"reconciled", message}` — operator-only exit from `unknown`; no turn token. `completed`/`failed` route `reply_to` as a result; `interrupted` routes an informational notice. A `sha` on `completed` binds like a worker `--sha` |
| `job_new` | `pm, spec, spec_sha256, job?, title?, issue?, repo?, base_ref?, max_revisions?, task_title?` | `{job, duplicate}` — bookkeeping only; creates the `open` job + default `<job>-t1` draft task |
| `job_list` | `state?, all?` | `{jobs:[Job+task counts]}` |
| `job_show` | `job` | `{job:{...,tasks:[Task+kickoff+attention+latest_verdict]}}` — lazily flags drift |
| `job_events` | `job, after?, limit?` | `{events:[Event], cursor}` — the `job_id`-scoped view |
| `task_new` | `job, task?, title?, assignee?, spec?, acceptance?, worktree?, branch?, base_sha?` | `{task}` — draft task in an open job |
| `task_show` | `task` | `{task:{...,messages,verdicts}}` |
| `task_dispatch` | `task, to?, message?, by?` | `{task, message, duplicate, queued_behind_dead}` — enqueues the kickoff at a new revision, or returns the live kickoff (`duplicate:true`) |
| `task_verdict` | `task, sha, verdict: pass|revise|blocked, reviewer?, pane?, evidence?, message?, revision?` | `{task, verdict}` — binds `sha == head_sha` on `state=review` |
| `task_accept` | `task, merged_sha?, by?` | `{task}` — `verified → done` |
| `task_sha` | `task, sha, by?` | `{task}` — repairs a NULL `head_sha` on a `review` task |
| `task_fail` | `task, reason, by?` | `{task}` — mark unrecoverable |
| `task_reopen` | `task, pane?` | `{task}` — `blocked|verified|failed → draft`, `revision` resets |
| `task_cancel` | `task, by?` | `{task}` — cancels the task; a `queued`/`submitting` kickoff cancels in the same tx, a `running` one completes on its own |
| `job_cancel` | `job, by?` | `{job}` — cancels the job + every non-terminal task |
| `job_close` | `job, by?` | `{job}` — legal only when every task is `done` |
| `agent_unfence` | `alias, status?, note?, by?` | reconciles every `unknown` on the agent (default `interrupted`); `{alias, reconciled:[id], state}` |
| `agent_stop` | `alias` | `{alias,state:"stopped"|"attention"}` |
| `agent_resume` | `alias` | `{alias,state:"starting"|"attention"}` |
| `agent_remove` | `alias` | deletes the agent + its history; refuses live endpoints |
| `agent_gc` | `older_than?` | sweeps dead agents; `{removed:[alias]}` |

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
is `rejected`, naming `cadence agent unfence <alias> --status
interrupted` then `cadence agent resume <alias>` — the fence is only
lifted by an explicit operator reconcile.

## Agents

`endpoint_kind` selects the delivery mechanism; reachable message states
depend on it:

| endpoint_kind | delivery | status |
|---|---|---|
| `managed` | owned provider process — JSON-RPC stdio (`codex`) or newline-delimited stream-json (`claude`) | implemented: providers `codex`, `claude` |
| `managed-ws` | owned `codex app-server --listen ws://127.0.0.1:*`; official TUI attachable | implemented: provider `codex` |
| `pty` | owned tmux pane running the official TUI; literal paste + explicit reports | implemented: provider `devin` |
| `inbox` | durable mailbox — no actor; messages queue until `agent_inbox` drains them | implemented: provider `inbox` |
| `fake` | in-process test double | test fixture only |

`agent_register` accepts `params` (JSON object) for endpoint options:
pty uses `{"session": "<native-id>"}` to resume an existing Devin
session instead of starting a fresh one, and `{"auto_ready":
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
"permission_mode": "<claude mode>"}` (default `manual`;
`--bypass` stores `bypassPermissions`), `{"allowed_tools": ["<pat>",
…]}` — appended to the `Bash(cadence *)` baseline the CLI tool needs
to self-report — and the turn-liveness knobs `{"turn_idle_secs": N}`
(default 900) plus `{"turn_max_secs": N}` (optional absolute cap).

For provider `devin`, `{"permission_mode": "<mode>"}` sets Devin's own
approval policy — `auto`, `accept-edits`, `smart` or `dangerous`
(`cadence devin --bypass` stores `dangerous`). The pane profile replays
it as `--permission-mode <mode>` on every open, fresh launch and `-r`
resume alike, so an unattended pane never stalls on its first approval
menu. `agent_register` validates the four values and rejects anything
else; `agent set` cannot patch it live (the mode is launch-time only —
relaunch or rejoin to change it). When the key is absent the flag is
omitted entirely and Devin's own default applies.

**Briefings.** Every launch path (`devin`, `codex`, `claude`, `join`) writes
`.cadence/<root>/BRIEFING-<alias>.md` — the group root's `.cadence/`
dir in the root's cwd (the PM's repo for a joined worker, the agent's
own for a standalone launch). The file carries identity (alias, native
session id, upstream), a protocol quickref, and the group roster at
write time — a snapshot; `cadence self`/`agent list` stay live truth.
Where the agent's cwd sits in a git repo, `<repo>/AGENTS.md` gains an
idempotent `<!-- cadence:begin -->`/`<!-- cadence:end -->` block (created
or appended, never touching outside content). `join` additionally
enqueues the deterministic `bootstrap-<worker>` message
(`source="bootstrap"`); standalone launches stay silent unless
`--bootstrap` is passed; `--no-bootstrap` skips file, block and message.
`cadence agent bootstrap <alias>` retrofits a live agent — file, block
and the durable message — and refuses unknown aliases.

**Isolated worktrees.** `--worktree <name>` on `devin`, `codex`, and
`join` runs the worker in `<repo>/.cadence/wt/<name>` on branch
`cadence/<name>` via `git worktree add -b`. It requires a git repo,
rejects invalid names, existing target dirs, branch collisions, and
applying it to an already-registered agent, and appends `.cadence/` to
`.gitignore` when absent.

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

**Dead-agent hygiene.** `agent list` marks attention/stopped agents with
no live endpoint as `dead`. `agent remove <alias>` deletes the row and
its message/event history, refusing while an endpoint is live or a
lifecycle actor owns the alias. `agent gc [--older-than <dur>]` sweeps
dead agents (manual only, never automatic); each candidate is
independent so one in-transition alias doesn't fail the sweep. A fenced
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
`revision_bound_verdicts`, `approval_brokering`, `result_routing`) —
never hand-listed per provider. Adding a provider or kind means one
`SPECS` entry; every check, capability list and doctor probe follows.

## pty endpoints (provider `devin`)

Cadence launches `devin [--permission-mode <mode>] [-r <session>]`
inside a detached tmux session
on a private socket (`cadence-<state-hash>`), so every pane it can kill
is one it spawned. The agent record keeps the fields separate: `alias`,
`thread_id` = the native Devin session id, `endpoint` =
`tmux://<socket>/<session>`, `pid` = pane process, `generation` = a uuid
minted per `open`.

**Per-TUI profiles.** The adapter is generic owned-pane machinery —
private socket, pane lifecycle, readiness claims, literal paste, the
differential render check — with every provider-specific fact behind a
`TuiProfile` (`src/adapter/pty/profile.rs`): launch argv, native
session-ownership proof, the screen analyzer that produces the probe
verdict, message wording, the open deadline, and the forbidden-prefix
list below. `devin` is the first profile; `tui-stub` is a test-double
profile the integration harness registers to prove the mechanics are
profile-driven. A second real TUI is a new profile module, not a copy
of the adapter.

**Ownership is proven, not assumed.** Devin flock's
`~/.local/share/devin/cli/session_locks/<session>.lock`; Cadence walks
`/proc` to require that a lock holder is a descendant of the pane pid —
at open, at every send, and on reconnect. If the lock for a requested
session is held by any other process, registration refuses (no
takeover). On restart a live pane that still owns the recorded session
is reattached; a dead pane is relaunched with `devin -r <stored>`. A
pane owning a *different* session fails closed (`attention`).

**Submission gates.** `run_turn` requires all of: pane alive,
`pane_dead=0`, `pane_in_mode=0`, lock still owned, and a fresh
unconsumed claim — either an operator claim from `agent ready` (60s
TTL) or, under `auto_ready=verified`, a daemon-minted claim. Claims
are single-use (consumed atomically by exactly one send), FIFO, and
capped; every consumption emits a `claim_used` event recording the
message id and the claimer (`agent ready <alias>` records
`CADENCE_ALIAS` when set, else `"operator"`; daemon-minted claims
record `"daemon"` on the `ready_claimed` event itself).

With `auto_ready=verified` the daemon mints a claim only after a pane
probe verifies idle: the screen must show the `❭` prompt with an empty
input line, and none of the observed busy signatures (`esc to
interrupt` hints, the guide/steer bar, queued-message footers) or an
approval menu — an approval screen's `❭` option marker can mimic a
prompt, so menu detection wins over prompt shape. Busy and menu
markers are matched only in the status region (the ~14 lines ending at
the last non-blank row — `capture-pane` pads short content with blank
rows, so the region is not the pane's literal bottom) — the transcript
above can legitimately print the same strings without the pane being
busy, and the `Guide Devin while it works` input watermark is itself a
busy signal even outside the region. `agent probe <alias>`
runs the same analyzer on demand (`{idle, reason, prompt_visible,
input_nonempty, busy_marker, approval_menu}`) without claiming. A
refused send returns the message to `queued` (event `gate_wait`) and
retries; it is never pasted blind and never dropped. Message text is a
single line of 1–4000 chars with no control characters, delivered
literally via `load-buffer` + `paste-buffer -p` + `Enter` — no shell
interpretation. A body whose first non-space character is in the
profile's forbidden-prefix list is rejected `PreWrite` *before* the
gate (so no claim is consumed, nothing reaches the pane, the agent is
not fenced): TUIs commonly treat a leading character as a command or
mode switch, so a verbatim paste of one is an injection path. Devin's
list was fixed by live observation in a scratch pane: `/` opens the
command menu, `!` switches to bash mode, `@` opens the file picker —
all forbidden; `#` stays a literal draft character.

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
On that evidence a routed `worker_result` notification is requeued
(bounded, then the delivery is `failed` with `via=pty_render_miss` and
a `delivery_parked` event — a notification must never fence the
recipient or kill its pane; the worker's result stays durable on the
worker's own message). Any other message goes `unknown` and fences the
actor — a possibly-pasted task is never replayed blind. Once the check
passes the message is `running` with `turn_id =
pty-<generation>-<uuid>` and completes only through an explicit
`message_report` (`message ack` keeps it `running`; `message result`
finishes it `completed` and routes `reply_to`). The token must equal
the recorded `turn_id` and belong to the agent's current generation — a
report against a previous pane life is `rejected` as stale; a
conflicting result for a completed message is `rejected`; an identical
retry is idempotent. Reporting identifies the caller by possession of
the token — self-asserted, not authenticated.

A pane that dies after a possible paste leaves submitted messages
`unknown` (fence, never replay); a pane that dies before the paste
fails the message. `agent_respond` is `rejected` for pty — Devin
permission prompts are answered in the terminal, and a visible prompt
is one of the things the ready claim asserts absent. `agent_stop`
kills the owned pane; daemon shutdown detaches instead, so a restart
reattaches rather than destroying a terminal the operator may be using.

**Worker-side conveniences.** The pane is spawned with
`CADENCE_ALIAS` and `CADENCE_STATE_DIR` in its environment (`tmux
new-session -e`), so a worker inside it can run `cadence self` to get
`{alias, running: [{id, turn_id}]}` — its report token without asking
the operator. `message send --ready` fuses the operator claim with the
send: the flag *is* the explicit claim (idle, empty input, no prompt —
verified by the human typing it), applied only on pty endpoints and
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
         (--session-id <uuid> | --resume <uuid>) [--model <m>]
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
event — the tool name only — so `events --follow` shows progress on a
long turn.

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
`claude --resume` after `agent stop`. Phase A brokers no provider
requests: `agent respond` is `rejected` naming the real opt-ups —
relaunch or rejoin with `--permission-mode <mode>` / `--allow "<pat>"`
/ `--bypass`, and watch `permission_denied` events. Provider stderr
lands in `providers/<alias>.provider.log` and
each `result` emits a `claude_result` event carrying
`total_cost_usd`/`num_turns`/`session_id` for audit.

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
transaction with result `{"status":"completed","via":"inbox_read"}`,
emits `inbox_read`, and any `reply_to` on a consumed message routes
its result in the same transaction (a drained `reply_to` therefore
lands on the replier's queue and wakes its actor). `wait>0`
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
| unknown`. `unknown` is durable and fences its actor. Any ambiguous
post-submission outcome lands there — transport loss mid-turn, a turn
deadline, an acknowledged `turn/start` that cannot be correlated to a
turn id, an unclassifiable completion status, or a forced close while a
turn was in flight. `interrupted` is terminal for "the outcome was
never learned and the operator moved on" — written by a reconcile, or
by a provider that reports an interrupted turn; it is never replayed.

**Fencing and reconcile.** An `unknown` message fences its agent
(`attention`, no relaunch, queued work waits). The only exit that keeps
history is `message_reconcile` — an operator statement requiring no
turn token (the token is stale by definition when a message is
`unknown`), refused for every other current state with an error naming
it. One transaction moves the message to the chosen terminal state with
result `{status, via:"operator_reconcile", note}` and emits a
`reconciled` event carrying the message id, status, note and caller
(`CADENCE_ALIAS`, else `"operator"`). `completed`/`failed` route
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
result_routed, notice_routed, ready, ready_claimed, claim_used,
gate_wait, submitted,
acknowledged, paste_not_rendered, delivery_parked, inbox_read,
params_updated, reconciled, relaunch_skipped, attention,
stop_requested`. `wait>0` long-polls up to 30s.

Job operations emit the same rows with `job_id`/`task_id` set —
`job_created, task_created, task_dispatched, task_running,
task_reported, verdict_recorded, task_revising, task_blocked,
task_reopened, task_failed, task_cancelled, task_sha_recorded,
task_done, job_closed, job_cancelled` — and `job_events` pages them
across aliases (`{job, after?, limit?}` → `{events, cursor}`).

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
  to the current one, `reviewer != assignee`, and pane rules: inside a
  cadence pane the reviewer is the pane alias (`--reviewer` and
  `operator` refused); outside, `--reviewer` is required.
- `task_dispatch` legal from `draft|revising`, from
  `dispatched|running` once the live kickoff is terminal (new revision;
  a reconcile to `completed` takes the normal completion edge), and
  from `blocked` only with a `--to` reassign. A live kickoff returns
  `duplicate:true` with the live message id.

## Approvals

Provider-initiated requests (e.g. `item/commandExecution/requestApproval`,
`item/tool/requestUserInput`, `session/request_permission`) pause the
agent at `waiting_input` and appear in `agent_requests`. `agent_respond`
answers them per type; nothing is auto-accepted. A request may also be
resolved outside Cadence — an attached TUI answering the approval makes
the provider emit `serverRequest/resolved`, which drops the pending
handle (`input_resolved`); a late `agent_respond` is then `rejected`.
The agent stays `waiting_input` while other requests remain pending.

## Recovery

On daemon start: messages in `submitting`/`running` become `unknown`.
`attention` fences survive the restart intact — recovery clears the
dead pid/endpoint/generation but keeps the state and its recorded
error verbatim, so the serve loop still sees the fence. Enabled agents
relaunch *unless* fenced — state `attention` or an unreconciled
`unknown`. A fenced agent is skipped before any actor or provider
spawn: a `relaunch_skipped` event records the reason and the sweep
continues with healthy agents. Recovery is `cadence agent unfence
<alias> --status interrupted` then `cadence agent resume <alias>`; a
reconciled agent lands `stopped` and disabled — the same condition as
an operator stop — so the next restart leaves it stopped rather than
relaunching it. A
pty pane that survived the restart is re-adopted by `open` — the
reattach path verifies the pane still owns the stored native session
lock, so resume converges on the same Devin session rather than a fresh
one. A pane that adopted a *different* session fails closed and the
hint is `agent remove` + `join -r` — retrying resume mints a new
provider session each time.

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
