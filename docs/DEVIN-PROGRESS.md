# Devin progress — feat/rust-core

M0/M1 foundation for Cadence, ported from the Python managed-agent
prototype (`agent-harness-test/managed.py`, `service.py`,
`codex_adapter.py`).

## Done

- `docs/PROTOCOL.md` — wire API v1, state machines, error taxonomy,
  endpoint capability matrix, recovery rules.
- Crate `cadence-agent` / binary `cadence`: clap CLI, Unix-socket daemon
  (same-UID peer check, 0700 state dir), SQLite store (WAL,
  `BEGIN IMMEDIATE`, schema v1), agent registry, durable FIFO queue,
  idempotent send with content-conflict check, transactional
  result+outbox routing (`reply_to` → deterministic uuid5 delivery id,
  no reply loops), per-agent actor threads, approval brokering
  (`agent requests`/`agent respond`, never auto-accept), unknown-outcome
  fencing (`submitting/running → unknown` on restart; fenced actors
  land in `attention`, not relaunched).
- Managed Codex adapter: `codex app-server --listen stdio://`, env
  scrub, own process group, `turn/start` with `clientUserMessageId`,
  `turn/completed` wait, final-answer item assembly, `turn/interrupt`,
  request brokering. Provider stderr to `agents/<alias>.provider.log`.
- `fake` endpoint: in-process test double for queue/approval/failure
  lifecycle without model calls.
- CLI: `doctor`, `daemon run|start|status|stop`,
  `agent register|list|show|requests|respond|stop|resume`,
  `message send|ask`, `events [--follow]`. Unimplemented verbs absent
  rather than stubbed.
- Hardening (independent-review fixes): singleton `cadence.lock` flock
  held for the daemon lifetime — a second `serve` fails before touching
  store/socket; per-alias actor ownership through termination (map entry
  removed only by the actor itself) plus a `stopping` reservation held
  through the stop's final write so a resume cannot start a new actor
  generation in the gap; bounded stop/shutdown (interrupt → ~3s grace →
  force-close → in-flight attempt becomes `unknown`, fence preserved —
  stop on an already-fenced agent keeps `attention`+reason); transport
  EOF wakes turn-completion waits; ambiguous post-submission outcomes
  (uncorrelatable `turn/start`, unclassifiable status) persist `unknown`;
  adapter published before `open` and guarded so init stops are bounded
  and failed initialization leaves no provider process behind.
- M2a — `managed-ws` endpoint: owned `codex app-server --listen
  ws://127.0.0.1:<ephemeral>` (loopback only, own process group), the
  same app-server JSON-RPC protocol over a tungstenite WebSocket with a
  single I/O owner — one thread owns the `WebSocket` and drains an
  outbound channel between bounded reads, so requests, pongs and close
  replies never interleave on the socket. Shared request/response
  correlation extracted to `adapter/link.rs` so stdio and WS transports
  cannot diverge. Connect/handshake/write/close are all bounded, with
  the upgrade running in a helper thread joined by absolute deadline —
  a drip-feeding or silent peer cannot stall startup. The child is
  published before connecting so `stop` kills a provider stuck
  mid-handshake; every post-spawn error path cleans it up. Fresh
  threads get one minimal seed turn at open — Codex persists a
  thread's rollout only after its first turn, which is what
  `codex resume --remote` needs to attach. Agent record carries
  `endpoint`; cleared on actor exit. `agent attach` prints (or `--run`
  executes) `codex resume --remote <endpoint> <thread>`; rejected for
  non-WS kinds, dead agents, or missing endpoint/thread. External
  approval resolution: `serverRequest/resolved` drops the matching
  pending handle; `agent_respond` claims handles atomically (validate
  under lock, then send) so a late/concurrent respond is `rejected`
  and `waiting_input` never regresses from a resolved/finished turn.
  Schema v1→v2 migration is a single transaction with a column-exists
  check, so an interrupted upgrade converges instead of wedging.
  Live smoke verified on the earlier transport revision: official TUI
  attached to the exact native thread showed an externally submitted
  prompt and its separate actual reply (`CADENCE_WS_SMOKE_42`).
- M2b — `pty` endpoint (provider `devin`): owned tmux session on a
  private socket (`cadence-<state-hash>`) runs the official `devin`
  TUI. Native session identity is proven via
  `~/.local/share/devin/cli/session_locks/<id>.lock` — a `/proc` walk
  requires a lock holder to descend from the pane pid at open, at every
  send, and on reconnect; a foreign lock refuses takeover, a changed
  owner fails closed. Restart reattaches a live pane owning the same
  session or relaunches `devin -r <stored>` on a dead one. Submission
  is gated: pane alive/unblocked/lock-owning plus a single-use,
  short-TTL operator claim (`agent ready`); refuse → `queued` retry,
  never blind paste. Delivery is literal (`load-buffer` +
  `paste-buffer -p` + `Enter`, 1–4000 chars, no control characters, no
  shell). A paste marks the message `running` with a
  `pty-<generation>-<uuid>` token (`submitted` event); only explicit
  `message ack` / `message result` reports complete it — wrong or
  stale-generation tokens and conflicting duplicates are `rejected`.
  `agent_respond` is `rejected` on pty (approvals stay in-terminal);
  `agent capture` returns the pane; `agent attach` prints the tmux
  attach command. Schema v2→v3 adds `agents.params` and
  `agents.generation` atomically. `stop` kills the owned pane; daemon
  shutdown detaches instead so the operator's terminal survives.

## Deferred (documented, not claimed)

- Devin ACP adapter, Claude native inbox, Cursor —
  `endpoint_kind`s declared; `doctor` reports them not implemented.
- Automatic TUI draft/permission-prompt detection — the `agent ready`
  operator claim is the authoritative gate; screen scraping is not
  claimed as reliable.
- Registration of a pre-existing foreign pane (no kill authority) —
  only launched, owned sessions are supported in M2b.
- Job/task lifecycle, worktrees, revision-bound QA verdicts (M3).
- Cooperative job API (legacy prototype path) — intentionally not ported.
- systemd user unit, SSH-disconnect lifecycle testing.
- Cross-host, multi-user authz, budgets.

## Verify

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```
