# Technical proposal — install, identity, chat, platforms, sandbox

Date: 2026-09-23. Status: proposal; readable source of the Details tab in
[index.html](index.html). Team and learning mechanics live in the architecture
records [agent filesystem](../../docs/design/AGENT-FILESYSTEM.md) and
[learning loop](../../docs/design/LEARNING-LOOP.md).

## Where Cadence stands (origin/main, 2026-09-23)

- Strong: durable messages and jobs, adapters for Claude, Codex, Devin, Cursor,
  git tracker, SHA-pinned verdicts, monitor alerts, secret scan, approval broker
  for headless Claude, tmux socket per state dir.
- Missing: release workflow, install script, `setup`; UI write endpoints beyond
  issues (and BOARD.md forbids dispatch endpoints without auth); chat-grade
  transcripts (Claude keeps tool names and final text only); Pi adapter; positive
  operator identity (CAD-280); macOS attribution (`/proc`).
- Measured on the live host (357 issues, 78 agents): `/api/issues` 16.4 s,
  `/api/overview` 29 s, `/api/agents` 5.8 s — the UI refetches all of them on every
  SSE event.
- Users start empty: a lost provider session restarts blank; only `~/pm` is
  portable; `rollout backup` records a receipt, not a copy.

## Install and setup (CAD-311, CAD-312, CAD-315, CAD-317)

- `curl -fsSL https://raw.githubusercontent.com/favcrm/cadence/main/install.sh | sh`
  → OS/arch detect, tarball + sha256, `~/.local/share/cadence/releases/<ver>/`,
  link `~/.local/bin/cadence`, run `cadence setup`. `--prefix` for sandboxes.
- `docs/INSTALL-AGENT.md`: a paste-in prompt for Claude Code, Codex or Cursor that
  runs the installer and `cadence setup --json --no-open` and reports each failed
  check with its fix.
- `cadence setup` is idempotent: state dir → tracker → skill → daemon → UI →
  single-use operator login link. `--json` emits `{check, status, detail, fix}`.
- Wizard start options: fresh · restore a backup · connect my tracker (git URL) ·
  import repos and GitHub issues.

## Identity and trust (CAD-313, CAD-381)

| Identity | Proven by | Can | Cannot |
|---|---|---|---|
| Operator | 0600 token → single-use link → HttpOnly SameSite=Strict cookie | Approve plans, merges, effects; curate memory; connect platforms | — |
| Agent | The daemon's own launch record (CAD-230 enrollment) for pty and managed endpoints alike | CLI within its role; platform tools via the proxy | Operator endpoints; credentials; its own `AGENT.md`; accepting its own work |
| Platform account | Platform credential exchange (AgenticOS: AOS-49) | Back the proxy with scoped access | Enter agent env/argv, the vault or backups |

## Chat with the master (CAD-318 children)

- Adapters emit payload events (`assistant_text`, `tool_call` redacted,
  `tool_result`, `turn_result`) into a durable thread; a provider session is
  disposable, the thread is not.
- API: `/api/threads/:id`, `/messages`, `/stream` (SSE with payloads), `/interrupt`.
- The master is a structured provider (Claude headless, Codex app-server, Pi rpc).
  Terminal agents (Devin, Cursor) are workers with a live read-only terminal view.
  PTY scraping is not the master channel (busy-probes-idle, ghost text, approval
  menus, no typed events).
- Continuity pack on a new, lost or compacted session: summary, last N turns,
  plan state, operator preferences (`USER.md`).

## Data store and continuity (CAD-314, CAD-316, CAD-319, CAD-324, CAD-325)

| Layer | Holds | Where |
|---|---|---|
| Decisions and work | Projects, plans, issues, verdict refs | `~/pm` git (unchanged) |
| Company brain | Agents, teams, lessons, decisions, FAQ, project artifacts | `~/pm` — Markdown in git |
| Runtime | Agents, messages, jobs, monitors | `cadence.sqlite3` |
| Threads | Conversations and payload events | store tables |
| Read model | Issue index, overview aggregates | store, rebuilt from git |

`cadence backup` (online SQLite backup + manifest, nightly, keep 7, taken before
every update), `cadence export --bundle` (no credentials, secret-scanned),
`cadence restore` (repo paths remapped by remote URL).

## Plans and delegation (CAD-329, CAD-330)

- `cadence project new`; `cadence plan propose --file PLAN.md` (Markdown with
  frontmatter: issues, acceptance, dependencies, roles, risk class); plan card in
  the thread; approval is a tracker commit; the daemon refuses dispatch outside an
  approved plan with a named reason (CAD-360).
- Sessions started from agent files (CAD-361), write leases per code area (CAD-378),
  fresh-context reviewer routing (CAD-362), merges and effects in Needs-you
  (CAD-363), terminal view (CAD-364).

## Connected platforms (CAD-331, AOS-49)

- Contract: credential exchange started by the operator; MCP tools that declare
  an effect `read | draft | send`; a pending-effect API; optional usage and files.
- The platform proxy holds credentials and enforces per-agent scopes; every
  `send` (message, publish, deploy, spend) waits for the operator's press.
- AgenticOS v2 today: cookie-only auth (agent credential is AOS-14/AOS-49),
  threads and files merged, connectors on branches, no deploy service (Container
  gadgets, AOS-32). Cloudflare own-account deploy ships first.

## Sandbox testing mode (CAD-310, PR #179)

The production daemon runs on this host; testing must never touch it, restart
it, or repoint `~/.local/bin/cadence`.

| Resource | Handling |
|---|---|
| State dir, socket, SQLite | `<root>/state` (`CADENCE_STATE_DIR`) |
| Tracker | `<root>/pm` (`CADENCE_PM_DIR`) |
| tmux panes | socket derived from the state dir |
| UI port | 3110+; 3010 refused; tailnet refused |
| Skill sync, Cursor global config, provider WAL checkpoints | off under `CADENCE_PROFILE=sandbox:<name>` |

Tiers: S1 same-user profile (`cadence sandbox up|env|down|reset|ls`), S2 a
dedicated Linux user for true first runs, S3 clean-machine CI (ubuntu + macOS).
Review round 1 found that Claude agents drop `CADENCE_PM_DIR`/`CADENCE_PROFILE`
and that the profile must be anchored to the state dir; round 2 is in progress.

## UI structure (CAD-326)

| Today | Becomes |
|---|---|
| Overview | Home rail: Needs you, active work, since you left, learning |
| Board | Projects → list / board / plans / context |
| Plan (static docs) | Project context |
| Agents | Agents → definitions (SOUL, AGENT, MEMORY), sessions, reports |
| Memory | Company vault → company, teams & projects, playbooks, lessons, decisions, FAQ, verification queue, context packs |
| Settings | Models → agent files; Providers, Autonomy, Platforms, Data, Access, Sandbox |

Foundation: router with real history, query cache per resource, SSE diffs, feature
folders, light and dark tokens, 390 px layout.
