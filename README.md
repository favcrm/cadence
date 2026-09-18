# Cadence

Coordinate coding agents across terminals, from planning through verified delivery.

Cadence is an early Rust implementation of a local development-agent controller.
It is designed for a Codex or Claude PM coordinating Codex, Claude, Cursor and
Devin workers on one host, including terminals accessed over SSH.

**Development stage:** implementation in progress. Provider support is an
acceptance-tested capability, not a promise of universal session attachment.

See [the implementation plan](docs/IMPLEMENTATION-PLAN.md) and the
[dogfooding retrospective](docs/DOGFOOD.md) for what running Cadence on
itself taught us.

Keep task checkouts under `.worktrees/`; see [workspace setup](docs/WORKSPACES.md)
for creation, handover and cleanup instructions.

## Quick start

```bash
cadence daemon start            # detached controller (CADENCE_STATE_DIR sets the state dir)
cadence skill install           # install the `cadence` agent skill into
                                #  ~/.agents/skills + .claude/.cursor/.copilot
                                #  symlinks (daemon start re-syncs stale copies)

cadence devin                   # fresh Devin TUI in an owned tmux pane, then attach
cadence devin -r cookie-cesium  # resume an existing Devin session, like `devin -r`
                                # (--permission-mode auto|accept-edits|smart|dangerous
                                #  or --bypass for dangerous — stored and replayed
                                #  on every launch/resume, so the pane never stalls
                                #  on its first approval menu)
cadence codex                   # Codex managed-ws endpoint, then `codex resume --remote`
                                # (--detach opts out; non-TTY or inside tmux prints
                                #  the attach command instead of exec'ing it;
                                #  --worktree <name> isolates it like join's)
cadence claude                  # headless managed Claude (stream-json) — each
                                #  message is one turn; the result IS the report
                                #  (--model/--permission-mode/--allow/--bypass are
                                #  stored and replayed on resume; no attach —
                                #  `cadence events --follow` observes)
                                #  --turn-idle-secs <n> bounds silence before a
                                #  turn fences unknown (default 900; liveness is
                                #  activity-based) — --turn-max-secs <n> adds an
                                #  absolute cap

cadence join <pm-slug> devin    # spawn a worker wired to a group: results route to the PM
                                #  (providers: devin, codex, claude, fake)
                                #  (--worktree <name> isolates it in .cadence/wt/<name>
                                #   on branch cadence/<name>; --no-bootstrap skips the
                                #   briefing file + kickoff message — note a bootstrap
                                #   on provider claude spends one real model turn;
                                #   --permission-mode/--bypass also apply to devin)
cadence attach [name]           # attach this terminal (alias, native id, or unambiguous
                                #  provider name); no name lists live attachable agents
                                #  grouped by PM; non-TTY/in-tmux prints the command
cadence send <slug> --text "t"  # durable message (verb form of `message send`;
                                #  --ready fuses the operator's gate claim for pty)

cadence resume <group>          # PM-first group resume: the PM, then every
                                #  member whose upstream is the PM (live ones
                                #  skipped, per-member status), then attach
cadence resume --all            # sweep every registered agent with a resumable
                                #  thread/session and no live endpoint
cadence daemon start --resume   # run the same sweep once the daemon is up
cadence stop <group>            # stop PM + members (registered + resumable still)

cadence agent remove <slug>     # delete a dead agent + its history
cadence agent gc --older-than 1d  # sweep dead agents (never automatic)
cadence agent bootstrap <slug>  # write + enqueue the briefing for an
                                #  already-live agent (launched pre-briefing)

cadence agent resume <slug>     # reopen a stopped agent, then attach
                                #  (waits for the endpoint, ~30s bound;
                                #   --detach opts out, non-TTY prints)
cadence agent unfence <slug>    # reconcile every unknown fencing the agent,
                                #  then resume (--no-resume leaves it stopped;
                                #  --status completed|failed records a verdict)
cadence message reconcile <id> --status interrupted [--note "why"]
                                # operator exit from `unknown`: completed/failed
                                #  route reply_to; interrupted routes a notice
cadence agent ready <slug>      # operator claim: pane inspected, idle, empty input
cadence agent probe <slug>      # analyze the pane without claiming (pty)
cadence agent set <slug> k=v    # merge an allowlisted param (auto_ready)
                                #  into a live agent — auto_ready=verified opts
                                #  the pane into daemon-verified claims
cadence agent attach <slug>     # print the tmux attach command (--run to exec)

cadence agent register obs --provider inbox   # durable mailbox, no process
cadence inbox obs [--wait 30]   # drain it: one JSON object per message,
                                #  each completed via=inbox_read (empty = silent)
```

The **board** tracks issues as folders of Markdown files under `~/pm`
(`CADENCE_PM_DIR` overrides) — a private git repo outside every project
repo. There is exactly one writer implementation (`src/issue/write.rs`);
the `cadence issue` CLI and the board's HTTP write API both drive it, so
every write is one git commit (`operator (ui)` for API writes). The HTTP
write path carries no auth — loopback + `Host` allowlist + four
cross-site guards (exact content type, `X-Cadence-Board` marker,
Origin/Sec-Fetch-Site) are the whole boundary; artifacts always serve
sandboxed, and html/svg download rather than render. See
[docs/BOARD.md](docs/BOARD.md).

```bash
cadence issue init                 # first run creates ~/pm
cadence issue new "title"          # project resolves from the cwd repo
cadence issue ls --ready           # leaves with no unfinished blockers
cadence issue show CAD-16
cadence issue lint                 # dangling/cyclic links, depth, sizes
cadence ui run                     # 127.0.0.1:3010 — SPA + read/write API
```

A **job** is a PM-scoped unit of work: `job new` binds an existing PM +
spec file, `job dispatch` sends a task's kickoff to a group worker, and
`job verdict` binds QA to the exact reported commit. Messages stay the
delivery axis — the job layer tracks the work axis on top. See
[docs/JOBS.md](docs/JOBS.md).

```bash
cadence job new --pm pm --spec spec.md --issue CAD-26
                                # open job + default <job>-t1 draft task
cadence job task add j1 --task j1-fix --assignee w1 \
        --worktree .cadence/wt/fix --accept "tests pass"
cadence job dispatch j1-fix     # kickoff → w1; revision 1
cadence job show j1             # tasks + live kickoff state + drift flags
cadence job verdict j1-fix --sha <40-hex> --revise --reviewer rev
                                # sha must equal the reported head_sha;
                                #  revise at the cap blocks instead
cadence job dispatch j1-fix     # revision 2 — fresh kickoff id
cadence job verdict j1-fix --sha <40-hex> --pass --reviewer rev
cadence job accept j1-fix --merged-sha <sha>
                                # verified → done; PM notified via job_event
cadence job cancel j1           # cancels non-terminal tasks; queued
                                #  kickoffs cancel, running ones finish —
                                #  agents are never stopped by a job
```

Workers on pty report with `message result <id> --token <t> --sha "$(git
rev-parse HEAD)"`; managed endpoints (codex/claude) never call
`message result` — their kickoff asks for a last line `SHA: <40-hex>`
instead, and `job task sha` repairs a missing one. A verdict on a NULL
SHA is rejected. Inside a cadence pane the reviewer is the pane alias;
outside, `--reviewer` is required (`operator` is the human's id), and
the reviewer is never the assignee.

The hot path is verb-first — `devin`, `codex`, `join`, `attach`, `send`,
`resume`, `stop`, `inbox` — while `agent`, `message` and `daemon` hold
the admin subcommands (register/list/show/ready/capture/probe/set/
remove/gc/bootstrap/unfence, send/ask/ack/result/reconcile,
start/run/status/stop).
Everywhere a command takes an agent name, an alias or a provider-native
session id resolves the same way.

An **inbox** agent is a durable mailbox endpoint: registerable as a
group root or a `reply_to` target, queueing messages in SQLite until
`cadence inbox` drains them over the daemon socket — no actor, no
polling loop, and the backlog survives restarts.

A capability registry (`src/adapter/registry.rs`) holds one descriptor
per `(provider, endpoint_kind)` pair — what the endpoint can do (actor,
attach surface, ready gate, reporting style, params). `agent show` /
`agent list` expose it as `capabilities`, and `health` + `cadence doctor`
generate their capability lists from it instead of string checks.

A **group** is a PM agent plus its workers; the PM's slug is the group
handle. Workers join with `params.upstream` set to the PM's alias, which
makes their result reports route back to the PM's queue by default.
Inside a cadence pane `cadence agent list` shows just the caller's group
(the root row carries `"group_root": true`); `--all` shows every agent.

Every launch writes `.cadence/<group-root>/BRIEFING-<alias>.md` (identity,
protocol quickref, roster) plus an idempotent `<!-- cadence:* -->` block
in the repo's `AGENTS.md`; joins also enqueue a `bootstrap-<alias>`
message. `--bootstrap` adds the message to standalone launches;
`--no-bootstrap` skips all of it.

Owned panes set `mouse on` + `set-clipboard on` (OSC52) on the private
tmux server. When a provider TUI captures the mouse, **Shift+drag**
still selects terminal-natively; `Ctrl-b [` enters copy mode as a
fallback. OSC52 reaches the system clipboard only on terminals that
support it.

## Principles

- Preserve native session identity and visible terminal conversations.
- Separate terminal submission, explicit acknowledgement and verified work.
- Persist messages and events; stop on uncertain execution instead of replaying edits.
- Keep provider permissions intact and source state outside the repository.
- Review the exact worker revision before calling a task complete.

The initial release targets Linux. Provider CLIs and their authentication remain
external dependencies; native terminal integration may require tmux.

## License

MIT. This project is not affiliated with provider vendors or other projects named Cadence.
