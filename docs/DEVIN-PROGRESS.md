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

## Deferred (documented, not claimed)

- Devin ACP adapter, PTY endpoint, Claude native inbox, Cursor —
  `endpoint_kind`s declared; `doctor` reports them not implemented.
- Job/task lifecycle, worktrees, revision-bound QA verdicts (M3).
- Cooperative job API (legacy prototype path) — intentionally not ported.
- systemd user unit, SSH-disconnect lifecycle testing.
- Cross-host, multi-user authz, budgets.

## Verify

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```
