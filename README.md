<p align="center"><img src="ui/public/icon.svg" width="112" height="112" alt="Cadence icon"></p>

# Cadence

Coordinate coding agents across terminals, from planning through verified delivery.

Cadence is an early Rust implementation of a local development-agent controller.
It is designed for a Codex or Claude PM coordinating Codex, Claude, Cursor and
Devin workers on one host, including terminals accessed over SSH.

**Development stage:** implementation in progress. Provider support is an
acceptance-tested capability, not a promise of universal session attachment.

New to the project: [start here](docs/START-HERE.md) for scope, architecture, code layout, design and role-specific context.

Running a session as PM: [docs/SESSION.md](docs/SESSION.md) — roles, lanes, dispatch, review, verdict, merge, restart.

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
                                #  --worktree <name> isolates it like join's;
                                #  --model <id> --effort <level> persist and
                                #  are validated against Codex model/list on
                                #  every fresh launch/resume)
cadence claude                  # headless managed Claude (stream-json) — each
                                #  message is one turn; the result IS the report
                                #  (--model/--effort/--permission-mode/--allow/
                                #  --bypass are stored and replayed on resume; no
                                #  attach — `cadence events --follow` observes)
                                #  e.g. `cadence claude --model opus --effort low`;
                                #  unset = the CLI's own settings default, shown
                                #  as `model_source: "provider default"` beside
                                #  `model_reported` in `agent show`/`list`;
                                #  `agent set <a> model=… effort=… --next-launch`
                                #  changes them for the next stop + resume
                                #  --turn-idle-secs <n> bounds silence before a
                                #  turn fences unknown (default 900; liveness is
                                #  activity-based) — --turn-max-secs <n> adds an
                                #  absolute cap
                                #  --broker-approvals routes permission prompts
                                #  to `agent requests`/`agent respond` via the
                                #  mcp-permission server (--permission-timeout-secs
                                #  bounds the wait, default 900; refused with
                                #  --bypass/--tui)
cadence claude --tui            # interactive Claude in an owned tmux pane —
                                #  pty rules: ready-gated literal paste, explicit
                                #  `message result` reporting, -r <session-id>
                                #  resumes a native session
cadence cursor                  # Cursor Agent TUI in an owned tmux pane —
                                #  same pty rules; the chat id is minted via
                                #  `cursor-agent create-chat`, --resume <chat-id>
                                #  resumes one (--model, --permission-mode
                                #  auto-review|force / --bypass are stored
                                #  and replayed on every launch). Launch also
                                #  merges Shell(cadence) into
                                #  ~/.cursor/cli-config.json permissions.allow
                                #  so the worker's own cadence calls never
                                #  hit an approval prompt (.bak kept, a
                                #  malformed config refuses the launch)

cadence join <pm-slug> devin    # spawn a worker wired to a group: results route to the PM
                                #  (providers: devin, codex, claude, cursor, fake)
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
cadence agent set <slug> auto_stop=off  # opt out of idle auto-stop (the daemon
                                #  stops non-PM agents idle 60m, resumably;
                                #  pm.yaml [host] auto_stop_idle_secs)

cadence agent remove <slug>     # delete a dead agent + its history
cadence agent gc --older-than 1d  # sweep dead agent records — no disk or
                                  #  memory remedy; removed agents cannot
                                  #  resume. Daemon timer: opt-in via pm.yaml
                                  #  [host] agent_gc_older_than_secs
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
cadence message cancel <id> [--reason "why"]
                                # operator exit from `queued`: never delivered;
                                #  reply_to gets one notice, task-bound refused
cadence agent ready <slug>      # operator claim: pane inspected, idle, empty input
cadence agent probe <slug>      # analyze the pane without claiming (pty)
cadence agent set <slug> k=v    # merge an allowlisted param (auto_ready)
                                #  into a live agent — auto_ready=verified opts
                                #  the pane into daemon-verified claims
cadence agent attach <slug>     # print the tmux attach command (--run to exec)

cadence agent register obs --provider inbox   # durable mailbox, no process
cadence inbox obs [--wait 30]   # drain it: one JSON object per message,
                                #  each completed via=inbox_read (empty = silent)

cadence status                  # one screen: every agent's state, running
                                #  message age+head, queued/unknown counts,
                                #  pane verdict, tracker issues, unread inboxes
                                #  (--group <pm> scopes to a group, --json,
                                #   --watch <secs> redraws; one probe per
                                #   pty pane, none for the rest)
cadence events <slug>           # newest 50 events (oldest first) + cursor;
                                #  --after <n> pages forward, --follow streams
                                #  from the tail; --job <job> same shape
cadence daemon restart          # stop (waits for exit), start, wait for live
                                #  agents, then a before/after table — a pty
                                #  pane whose pid changed fails the run
                                #  (--when-idle waits for idle panes first,
                                #   --timeout <secs>, --ui bounces the UI)
cadence daemon stop             # shutdown + wait for the process to release
                                #  the state-dir lock — `stop && start` is safe

cadence review <PR>             # the reviewer's mechanical routine: detached
                                #  checkout (merge result when the base moved),
                                #  gates from cadence-review.toml as committed
                                #  on the base head (a PR that changes it is
                                #  flagged and never suggested `pass`), new-test
                                #  stress, one full suite, equal-conditions
                                #  compare — a Markdown+JSON report under the
                                #  state dir (--no-full, --stress N, --keep,
                                #  --json; never posts, merges or pushes)

cadence doctor --host           # read-only host watchdog: disk free, provider
                                #  store/WAL growth, per-user pipe pressure,
                                #  memory commitment (Committed_AS vs
                                #  CommitLimit — the fork EAGAIN mode) and a
                                #  process-group census, orphaned processes
                                #  from deleted worktrees, leaked temp dirs,
                                #  stale worktrees — each finding carries its
                                #  remedy; exit 0/1/2
                                #  (--json for machines; pm.yaml [host] tunes)
```

issue acceptance <ID> --from <FILE> replaces the unique level-two Acceptance
checklist in that issue, or inserts it when the section is absent. The input
file must contain at least one nonempty - [ ] or - [x] item; replacement keeps
the checklist order and checked state while preserving the rest of the body.
Duplicate Acceptance headings, malformed or empty input, missing issues and
missing files are refused before any tracker write. This command only authors
and reads criteria. Dispatch enforcement for empty acceptance remains the
later CAD-159 increment; this slice does not change existing issues or
running jobs.

The **board** tracks issues as folders of Markdown files under `~/pm`
(`CADENCE_PM_DIR` overrides) — a private git repo outside every project
repo. There is exactly one writer implementation (`src/issue/write.rs`);
the `cadence issue` CLI and the board's HTTP write API both drive it, so
every write is one git commit (`operator (ui)` for API writes). The HTTP
write path carries no auth — loopback + `Host` allowlist + four
cross-site guards (exact content type, `X-Cadence-Board` marker,
Origin/Sec-Fetch-Site) are the whole boundary; artifacts always serve
sandboxed, and html/svg download rather than render. Job state drives
card status when an issue is bound (`status_source: job`), agent
binding joins `agent.tasks` × `jobs.issue_id` exactly, `GET /api/stream`
pushes live `issues`/`jobs`/`agents` events, and an Agents screen ranks
fenced agents first with their recovery commands. See
[docs/BOARD.md](docs/BOARD.md).

```bash
cadence issue init                 # first run creates ~/pm and installs its
                                   # lint/push git hooks (idempotent, keeps
                                   # foreign hooks)
cadence issue doctor               # tracker health: hooks, lint, remote, push lag
cadence issue new "title"          # project resolves from the cwd repo
cadence issue ls --ready           # leaves with no unfinished blockers
cadence issue show CAD-16
cadence issue acceptance CAD-16 --from acceptance.md
cadence issue lint                 # dangling/cyclic links, depth, sizes —
                                   # the safety net; writes validate first
cadence issue sync                 # multi-host: fetch, rebase, lint, push —
                                   # conflict/lint aborts leave the tree
                                   # untouched; --dry-run, --no-push,
                                   # --resolve ours|theirs
cadence issue log CAD-16           # the audit trail from git alone:
cadence issue diff CAD-16 --to HEAD   # log/diff/blame, `ls --at <rev>`
cadence issue blame CAD-16            # for the board at a revision —
                                      # all read-only; every tracker
                                      # commit carries Issue:/Actor:
                                      # trailers history reads for `by`
cadence issue trailer CAD-16       # prints `Issue: CAD-16` — tag code
                                      # commits so issue detail lists
                                      # them (`commits` on show/API)
cadence issue start CAD-16         # mints the project worktree+branch,
                                      # records refs, moves to doing
cadence issue finish CAD-16        # safe cleanup: refuses while the owner
                                      # is busy, the tree dirty, or the
                                      # branch unmerged+unpushed
cadence dispatch CAD-16 --to w1 --note kick.md
                                   # one-step hand-off: issue start +
                                      # one templated kickoff + comment;
                                      # --job --spec f dispatches the job
cadence ui run                     # 127.0.0.1:3010 — SPA + read/write API
cadence secret scan [--file f]     # credential scan: JSON findings
                                   # {rule,line,column,redacted,severity,
                                   # fingerprint}, never the value; exit 1
                                   # on a blocking finding (stdin without
                                   # --file)
```

**Secret scan (CAD-109).** `issue comment`, `report`, `memory propose`
and the `intake` GitHub relay scan their text before they write. A
blocking finding refuses the write with the rule id, line and a redacted
prefix (never the value), and nothing is written; a relay report stays
`blocked` locally until its text is clean. The rules are the vendored,
version-pinned gitleaks pack (`src/secret/gitleaks.toml`, MIT) plus
cadence's bare-token rules (`figd_`, `sk-ant-`, `sk-proj-`,
`github_pat_`, `glpat-`, `npm_`, `dvn_`, `AIza`) and the CAD-108 argv
rule. Every rule blocks except `generic-api-key`, which only warns for
now; warnings come back as `secret_warnings` in the write's JSON. There
is no bypass flag. The operator allowlist is `secret-allowlist.toml` in
the state dir (`$CADENCE_STATE_DIR`, else `$XDG_STATE_HOME/cadence`, else
`~/.local/state/cadence`; the directory that holds `intake-relay.yaml`),
edited by hand:

```toml
[[allow]]
rule = "generic-api-key"          # every finding of this rule
reason = "why"

[[allow]]
rule = "cadence-argv-secret"
fingerprint = "0123456789abcdef"  # one value, from `secret scan` output
reason = "why"
```

A malformed allowlist refuses guarded writes until it is fixed. CI also
runs pinned gitleaks 8.30.1 over each PR's commits (`secrets` job).

A **job** is a PM-scoped unit of work: `job new` binds an existing PM +
spec file, `job dispatch` sends a task's kickoff to a group worker, and
`job verdict` binds QA to the exact reported commit. Messages stay the
delivery axis — the job layer tracks the work axis on top. See
[docs/JOBS.md](docs/JOBS.md).
On GitHub the reviewer binds the same verdict to the PR head with `scripts/qa-verdict.sh` ([verdict gate](docs/DOGFOOD.md#reviewer-verdict-gate)).
`cadence audit` reconstructs every merge's provenance from that stored
evidence and flags self-merges and verdict-less heads — see
[docs/AUDIT.md](docs/AUDIT.md).

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
remove/gc/bootstrap/unfence/requests/respond, send/ask/ack/result/
reconcile, start/run/status/stop/restart).
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

Every launch writes a briefing under the daemon's state dir —
`$CADENCE_STATE_DIR/briefings/<group-root>/BRIEFING-<alias>.md`
(identity, protocol quickref, roster); `agent show` prints the path and
the bootstrap message names it. Nothing is written into the agent's cwd
repo by default. `--agents-md` opts the launch into an idempotent
`<!-- cadence:* -->` block in the repo's `AGENTS.md` (persisted in
params, replayed on resume); joins also enqueue a `bootstrap-<alias>`
message. `--bootstrap` adds the message to standalone launches;
`--no-bootstrap` skips all of it.

Owned panes set `mouse on` + `set-clipboard on` (OSC52) on the private
tmux server. When a provider TUI captures the mouse, **Shift+drag**
still selects terminal-natively; `Ctrl-b [` enters copy mode as a
fallback. OSC52 reaches the system clipboard only on terminals that
support it.

### Sandbox

A disposable Cadence beside the production one, for trying a build or
driving a feature by hand:

```bash
cadence sandbox up smoke          # own state dir, tracker and board port
                                  #  (3110-3199, --port to pick; 3010 refused)
eval "$(cadence sandbox env smoke)"  # point this shell at it
cadence sandbox ls                # every sandbox, daemon/board running?
cadence sandbox down smoke        # stop its board and daemon, keep files
cadence sandbox reset smoke       # stop, then delete its root
```

Roots live under `$CADENCE_SANDBOX_ROOT` (default
`$XDG_STATE_HOME/cadence-sandbox/<name>`) as `.cadence-sandbox` (the
marker `reset` requires), `state/`, `pm/` and `sandbox.env`. `up` runs
the daemon and board from the binary that invoked it, and refuses a root
that overlaps production's state dir, socket or `~/pm`, or the
`CADENCE_STATE_DIR`/`CADENCE_PM_DIR` the shell exports. Under
`CADENCE_PROFILE=sandbox:<name>` the daemon skips the skill sync into
`$HOME`, `ui tailscale` is refused, the provider WAL watcher only
records intent, and the Cursor `cli-config.json` merge needs
`CADENCE_SANDBOX_ALLOW_GLOBAL=1`. Provider CLIs still share `$HOME`, so
agents launched in a sandbox use the host's provider logins and stores.

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
