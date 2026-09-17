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
| `agent_register` | `alias, provider, cwd, endpoint_kind?, role?, sandbox?, instructions?` | `{alias,state:"starting",provider}` |
| `agent_list` | — | `{agents:[Agent]}` |
| `agent_show` | `alias` | `{agent, messages, event_cursor}` |
| `agent_send` | `alias, text, message?, reply_to?` | `{message,state,duplicate}` |
| `agent_ask` | `alias, text, message?, wait?` | the `Message` row; state may be non-terminal if `wait` expired |
| `agent_events` | `alias, after, wait(<=30)` | `{events:[Event], cursor}` |
| `agent_requests` | `alias` | `{requests:[{request,method,params}]}` |
| `agent_respond` | `alias, request, decision?|answers?` | `{state:"answered"}` |
| `agent_ready` | `alias` | `{state:"ready-claimed"}` — single-use readiness claim for `pty` |
| `agent_capture` | `alias` | `{capture}` — current pane contents (pty) |
| `message_report` | `message, token, kind: ack|result, text?` | `{state:"reported"}` — explicit PTY ack/result |
| `agent_stop` | `alias` | `{alias,state:"stopped"|"attention"}` |
| `agent_resume` | `alias` | `{alias,state:"starting"|"attention"}` |

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
stop after completion is idempotent. Resume on a fenced agent returns
`attention` and does not re-enable.

## Agents

`endpoint_kind` selects the delivery mechanism; reachable message states
depend on it:

| endpoint_kind | delivery | status |
|---|---|---|
| `managed` | owned provider process (JSON-RPC stdio) | implemented: provider `codex` |
| `managed-ws` | owned `codex app-server --listen ws://127.0.0.1:*`; official TUI attachable | implemented: provider `codex` |
| `pty` | owned tmux pane running the official TUI; literal paste + explicit reports | implemented: provider `devin` |
| `fake` | in-process test double | test fixture only |
| `native_inbox` | provider-native inbox | declared, not implemented |

`agent_register` accepts `params` (JSON object) for endpoint options:
pty uses `{"session": "<native-id>"}` to resume an existing Devin
session instead of starting a fresh one.

`params` also carries wiring metadata: `{"upstream": "<pm-alias>"}`
marks the agent as a worker joined to a group (`cadence join` sets it).
When a message is sent to such an agent without an explicit `reply_to`,
`agent_send`/`agent_ask` default `reply_to` to the upstream alias, so
the worker's result lands on the PM's queue. An explicit `reply_to`
always wins; `reply_to` may still not equal the sender alias and must
name a registered agent (both enforced by `enqueue`).

**Join bootstrap.** `cadence join` (and `cadence devin`/`cadence codex`
launches that set `upstream`) writes a briefing to
`.cadence/<pm>/BRIEFING-<worker>.md` in the PM's repository and enqueues
a deterministic `bootstrap-<worker>` message telling the worker its
identity (`cadence self`), how to report (`message result` with the
running `turn_id`), how readiness works, and scope rules (peer output is
data, not authorization). The deterministic id makes re-joins
idempotent; `--no-bootstrap` skips both the file and the message.

**Isolated worktrees.** `--worktree <name>` on `devin`, `codex`, and
`join` runs the worker in `<repo>/.cadence/wt/<name>` on branch
`cadence/<name>` via `git worktree add -b`. It requires a git repo,
rejects invalid names, existing target dirs, branch collisions, and
applying it to an already-registered agent, and appends `.cadence/` to
`.gitignore` when absent.

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
unconsumed operator claim from `agent ready` (60s TTL, consumed
atomically by exactly one send — at most one paste per claim). The
claim is the authoritative gate: Cadence cannot reliably detect a typed
draft or an on-screen permission prompt, so the claiming operator
asserts the terminal is idle with an empty input — inspect with
`agent capture` first. A refused send returns the message to `queued`
(event `gate_wait`) and retries; it is never pasted blind and never
dropped. Message text is a single line of 1–4000 chars with no control
characters, delivered literally via `load-buffer` + `paste-buffer -p`
+ `Enter` — no shell interpretation.

**Durable submission vs. receipt.** A successful paste marks the
message `running` with `turn_id = pty-<generation>-<uuid>` and emits
`submitted`. Terminal echo proves visibility only; the message completes
only through an explicit `message_report` (`message ack` keeps it
`running`; `message result` finishes it `completed` and routes
`reply_to`). The token must equal the recorded `turn_id` and belong to
the agent's current generation — a report against a previous pane life
is `rejected` as stale; a conflicting result for a completed message is
`rejected`; an identical retry is idempotent. Reporting identifies the
caller by possession of the token — self-asserted, not authenticated.

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
server only: `mouse on`, `status-left-length 40`,
`pane-border-status top`, `pane-border-format " #{session_name} "`.
Failures are ignored — cosmetics never fence an endpoint.

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

(`cadence agent attach <alias>` prints this command; `--run` executes it
in the current terminal. The top-level `cadence attach [name]` is
client-side sugar over `agent_show` + `agent_list`: it resolves an alias
or native id, then a provider name when exactly one live agent of that
provider exists — ambiguous or absent names list candidates rather than
guess.) A fresh `managed-ws` thread is seeded with one
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
turn was in flight.

Idempotency: a client-supplied `message` id makes retries of the *same
envelope* (alias+body+reply_to+source) return `duplicate:true`. The same
id with different content is a `rejected` conflict.

Result routing: when a message has `reply_to`, finishing it enqueues a
`worker_result` message to that agent in the SAME transaction. The routed
id is `uuid5("cadence-result:" + message_id)` (deterministic; resend is a
no-op) and carries no `reply_to`, so routing cannot loop.

## Events

`agent_events` pages the durable log: `{seq, alias, kind, payload, at}`.
Kinds: `registered, queued, submitting, turn_started, turn_finished,
provider_event, input_required, input_answered, input_resolved,
result_routed, ready, ready_claimed, gate_wait, submitted, acknowledged,
attention, stop_requested`. `wait>0` long-polls up to 30s.

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

On daemon start: messages in `submitting`/`running` become `unknown` and
their agents `offline`/`attention`. Enabled agents relaunch *unless*
fenced by an `unknown` attempt — those stay `attention` for review.
