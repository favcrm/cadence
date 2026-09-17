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
| `agent_list` | — | `{agents:[Agent]}` |
| `agent_show` | `alias` | `{agent, messages, event_cursor, queued, unknown}` — `unknown` counts unreconciled unknowns fencing the agent |
| `agent_send` | `alias, text, message?, reply_to?, source?` | `{message,state,duplicate}` |
| `agent_ask` | `alias, text, message?, reply_to?, wait?` | the `Message` row; state may be non-terminal if `wait` expired |
| `agent_events` | `alias, after, wait(<=30)` | `{events:[Event], cursor}` |
| `agent_requests` | `alias` | `{requests:[{request,method,params}]}` |
| `agent_respond` | `alias, request, decision?|answers?` | `{state:"answered"}` |
| `agent_ready` | `alias, by?` | `{state:"ready-claimed"}` — single-use readiness claim for `pty`; `by` records the claimer |
| `agent_capture` | `alias` | `{capture}` — current pane contents (pty) |
| `agent_probe` | `alias` | `{probe:{idle,reason,...}}` — analyzed pane state without claiming (pty) |
| `agent_set` | `alias, patch` | merges an allowlisted param into the live agent — today only `auto_ready` (`"verified"` or null-removal, pty only); `{state:"updated"}` |
| `agent_inbox` | `alias, after?, wait?` | drains queued inbox messages, completing each `via=inbox_read`; `{messages, cursor}` |
| `message_report` | `message, token, kind: ack|result, text?` | `{state:"reported"}` — explicit PTY ack/result |
| `message_reconcile` | `message, status: interrupted|completed|failed, note?, by?` | `{state:"reconciled", message}` — operator-only exit from `unknown`; no turn token. `completed`/`failed` route `reply_to`; `interrupted` routes nothing |
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
| `managed` | owned provider process (JSON-RPC stdio) | implemented: provider `codex` |
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

**Briefings.** Every launch path (`devin`, `codex`, `join`) writes
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
member fenced by an unreconciled `unknown` is listed under `fenced`
with the `agent unfence` + `agent resume` commands and never attempted,
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

## pty endpoints (provider `devin`)

Cadence launches `devin [-r <session>]` inside a detached tmux session
on a private socket (`cadence-<state-hash>`), so every pane it can kill
is one it spawned. The agent record keeps the fields separate: `alias`,
`thread_id` = the native Devin session id, `endpoint` =
`tmux://<socket>/<session>`, `pid` = pane process, `generation` = a uuid
minted per `open`.

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
interpretation.

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
`reply_to` exactly like a normal finish (same deterministic delivery id
— exactly once); `interrupted` routes nothing — nothing was ever
reported. An `unknown` finish itself routes nothing either: the
replier hears the operator's verdict, not a fabricated result. When the
agent's last `unknown` reconciles, the fence lifts `attention →
stopped` — never auto-started; `agent resume` is the next move.
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
result_routed, ready, ready_claimed, claim_used, gate_wait, submitted,
acknowledged, paste_not_rendered, delivery_parked, inbox_read,
params_updated, reconciled, relaunch_skipped, attention,
stop_requested`. `wait>0` long-polls up to 30s.

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
Enabled agents relaunch *unless* fenced — state `attention` or an
unreconciled `unknown`. A fenced agent is skipped before any actor or
provider spawn: its state is restored to `attention` with the reconcile
hint, a `relaunch_skipped` event records the reason, and the sweep
continues with healthy agents. Recovery is `cadence agent unfence
<alias> --status interrupted` then `cadence agent resume <alias>`; a
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
