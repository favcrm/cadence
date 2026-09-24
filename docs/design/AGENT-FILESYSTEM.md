# Agent filesystem: agents, sessions and projects as Markdown

Date: 2026-09-23. Status: **proposed** design record. Operator direction
(2026-09-23): the system is a filesystem and all information is Markdown (as in
AgenticOS v2's docs); each agent is a folder `agents/<slug>/` with `SOUL.md`,
`AGENT.md` and `MEMORY.md`; there is **no separate role layer** — an agent is the
definition, and **an agent can run many sessions**; **no teams for now** —
staffing lives in each project's `PROJECT.md`. Amends
[ADR-0001](../adr/0001-role-profiles.md): Markdown files replace `team.yaml`; the
runtime registry stays the truth for sessions, and drift from the files is
reported, never silently reconciled. Tickets: CAD-338 (this record → ADR),
CAD-343, CAD-346, CAD-351; answers the CAD-116 question.

## Model

| Concept | What it is | Where |
|---|---|---|
| **Agent** | A durable definition: who it is, what it does, what it knows. Company-wide; one folder. | `agents/<slug>/` |
| **Session** | One running instance of an agent on one provider, for one task at a time. Disposable. Many can run at once. | runtime registry (SQLite), id `<slug>#<n>` |
| **Project** | Repos, context, which agents work on it and how many sessions of each, autonomy, tickets and artifacts. | `projects/<slug>/PROJECT.md` and folders |

Examples: `agents/master`, `agents/pm`, `agents/dev`, `agents/qa`, `agents/devops`,
`agents/research`, `agents/curator`. When a
project needs a genuinely different agent — another mandate, soul or memory — it
gets its own slug (e.g. `agents/dev-mobile`), not an override file. There is nothing to inherit from.

## Why files

- **Durable and portable.** Definitions and memory live in git (`~/pm`), so they
  survive restarts, provider switches, reinstalls and new machines, and every
  change has an author and a diff.
- **Readable by humans and agents**, in any editor or Markdown vault app.
- **Provider-neutral.** One definition compiles to each vendor's native format at
  every session launch (Claude subagent frontmatter, Codex agent TOML, Pi flags,
  pane briefing).

## Layout

```text
~/pm/                                   # the system filesystem (git)
  company/  HANDBOOK.md  STANDARDS.md  POLICIES.md  GLOSSARY.md  USER.md
  agents/<slug>/
    SOUL.md                             # who it is: voice, values, working norms
    AGENT.md                            # what it does: contract, provider/model/effort, permissions
    MEMORY.md                           # index of what it knows (≤ 200 lines)
    memory/<id>.md                      # verified items scoped to this agent
    memory/inbox/<id>.md                # proposals from its sessions, awaiting verification
    journal/INDEX.md                    # links to reports its sessions filed (in tickets)
  projects/<slug>/                      # see "Project filesystem"
  lessons/  decisions/  faq/  inbox/    # company-scope knowledge
```

## The three agent files

### `SOUL.md` — who the agent is

Short (≤ 4,000 characters), operator-owned, changes rarely; name and one-line
description as frontmatter (absorbs OpenClaw's `IDENTITY.md`). Voice, values and
working norms that matter for the work: *ask before guessing*, *evidence over
claims*, *say what you did not check*. Persona prompts did not improve accuracy
(Zheng et al. 2023; Wharton 2025) and irrelevant persona details cost up to ~30
points (Araujo et al. 2025) — keep it to tone, boundaries and norms.

### `AGENT.md` — what the agent does

```markdown
---
name: qa                          # required by Claude Code, Codex, Copilot
description: Fresh-context QA reviewer; verdicts pinned to the exact SHA.
preferred: {provider: claude, model: opus, effort: high}
fallbacks: [{provider: codex, model: gpt-5.5, effort: high}]
constraints: [fresh_context, vendor_differs_from: author]
permissions: read-only            # compiled per vendor
skills: [cadence-review, secret-scan]
memory_scopes: [company, project, agent]
sessions: {max_concurrent: 3}
budget: {turns: 60}
escalate_on: [ambiguity, missing_access, scope_change, destructive_op, two_failed_attempts]
reports: [done, question, blocked]
---
# QA reviewer
Review the exact commit against the contract, run the gates, record a verdict
pinned to the SHA.
## May
## Never
```

`AGENT.md` is not the repository's `AGENTS.md`: that file stays the project's
shared rules for anyone working in the repo; `AGENT.md` defines one agent.

### `MEMORY.md` — what the agent knows

An index, not a dump (≤ 200 lines / 25 KB, what Claude Code preloads): one line
per item with id, claim, scope and "verified at SHA", linking to `memory/<id>.md`
or to shared items. Written by the vault writer from verified items; sessions
propose into `memory/inbox/`. See [LEARNING-LOOP.md](LEARNING-LOOP.md).

## Sessions

- A session is started from an agent: `cadence session start qa --project reminders
  --task RMD-1` (or by the PM's dispatch). Its registry row records the agent slug,
  session number, provider/model/effort actually used, worktree and task.
- **All sessions of an agent share its `SOUL.md`, `AGENT.md` and `MEMORY.md`.**
  What one session learns reaches the others only after verification.
- Each session picks its provider from `preferred`/`fallbacks` by availability,
  quota and the cross-vendor rule — two concurrent `qa` sessions may run on
  different providers.
- Sessions are bounded by `sessions.max_concurrent` in `AGENT.md`, the project's
  counts in `PROJECT.md` and the host ceiling; write leases keep one writer per code
  area across all sessions.
- **Separation of duties is by agent, not session:** two sessions of the same
  agent never review, merge or accept each other's work or lessons.
- Identity: the daemon's launch record (CAD-381) maps a caller to its session and
  agent; agents never self-assert who they are.
- Reports are filed in the ticket (`projects/<slug>/tickets/<ID>/reports/`) with
  the session id; the agent's `journal/INDEX.md` links them.

## `PROJECT.md` (replaces `team.yaml`; no teams for now)

```markdown
---
project: reminders
repos: [{remote: github.com/harbor-bakery/reminders, path: ~/code/reminders}]
agents: {pm: 1, dev: 4, qa: 1, devops: 1}      # max concurrent sessions per agent
autonomy: approve-plans
write_leases: per-issue planned paths
---
# Reminders
Goal, non-goals, context links (architecture, ADRs, runbooks).
```

`cadence project new <key> --repo <path>` (CAD-358, the operator or the master) registers the
repo in `project.yaml` and seeds this file: the goal, an `agents:` map (default
`{pm: 1, dev: 1, qa: 1}`), the default stages and `milestones: []`. The repo list
stays in `project.yaml`, the one place cwd resolution reads; `<pm>/agents/` holds
the agent files, so `agents` is not a project key.

Teams can come back later as a grouping of projects if several projects share
staffing; nothing in this layout depends on them.

## Project filesystem

```text
projects/<slug>/
  PROJECT.md                     # goal, repos, agents × sessions, autonomy, links
  shared/                        # long-lived, cross-ticket artifacts
    plans/ designs/ research/ reports/ decisions/ runbooks/ brand/
    INDEX.md                     # generated: file, kind, owner, date, source ticket
  memory/                        # verified project lessons (exists today)
  tickets/<ID>/
    issue.md  comments/          # exist today
    artifacts/                   # images, previews, verdicts, evidence
    reports/                     # session reports (done / question / blocked)
    plan.md                      # when the ticket is an epic or has a plan
    ARTIFACTS.md                 # generated index with provenance
```

Rules: an artifact lives with its narrowest owner (the ticket; an epic's plan in
the epic); `shared/` holds only cross-ticket or long-lived material, promoted by
move + link, never copy; every file has an index entry (author, date, sha, kind,
source task); binaries over ~1 MB go to a content-addressed blob store
(`.blobs/sha256/…`, syncable to R2 or AgenticOS files) with only the entry in git;
all writes go through one writer (`cadence issue attach`, `cadence report`);
code-coupled docs (ADRs, API specs) stay in the product repo and are linked.
Produced HTML (previews, mockups, reports) is an artifact like any other: the
source file lives in the project folder (`shared/designs/` or the ticket's
`artifacts/`, or the product repo's `design-plans/` for design work) with its
Markdown source beside it; a preview host holds only an expiring copy.
Migration of today's `~/pm/<project>/<ID>/` to `projects/<slug>/tickets/<ID>/` is
recommended while there are six projects (pending operator choice).

## Size caps

| File | Cap | Reason |
|---|---|---|
| `SOUL.md` | 4,000 characters | Injected every session; OpenClaw caps `USER.md` the same way |
| `AGENT.md` | 20,000 characters | OpenClaw per-file cap; Copilot agents max 30,000 |
| `MEMORY.md` | 200 lines / 25 KB | Claude Code preloads exactly this much |
| Whole static prefix | 32 KiB | Codex caps combined `AGENTS.md` content at 32 KiB |

The vault lint (CAD-346) refuses files over their cap.

## Who may write what

| Path | Operator | Master | PM | Curator | The agent's own sessions |
|---|---|---|---|---|---|
| `company/` | write | propose | — | propose | — |
| `agents/<slug>/SOUL.md`, `AGENT.md` | write | propose (new agents) | propose | — | **never** |
| `agents/<slug>/MEMORY.md`, `memory/` | review | — | — | write (verified) | — |
| `agents/<slug>/memory/inbox/` | — | — | — | read | write |
| `projects/<slug>/tickets/<ID>/` | write | write | write | read | attach / report |
| `lessons/`, `decisions/`, `faq/` | write | propose | propose | write | propose |

All writes go through one writer (the pattern of `src/issue/write.rs`): one commit
per change with an `Actor:` trailer, refusal outside the caller's column. A session
can never edit its agent's contract, permissions or soul — privilege escalation by
self-edit — and prompt-injected text can at most land in the inbox, where logic
checks and the curator see it. (OpenClaw's templates invite agents to rewrite
`SOUL.md`; the Cloud Security Alliance's OpenClaw hardening guide, 2026-03, says the
agent "should not be able to write to it at runtime".)

## Compatibility

| Ecosystem | Their file | How ours maps |
|---|---|---|
| Claude Code | `.claude/agents/<name>.md` (name, description, model, effort, permissionMode, skills, memory; body = prompt) | `AGENT.md` frontmatter compiles 1:1; `SOUL.md` + body become the prompt |
| Codex | `.codex/agents/<name>.toml` (name, description, developer_instructions, model, model_reasoning_effort, sandbox_mode) | Generated TOML per session |
| GitHub Copilot | `.github/agents/*.agent.md` | Same frontmatter subset |
| Agent Skills | `<name>/SKILL.md` | Playbooks keep this format |
| OpenClaw | workspace `AGENTS.md`, `SOUL.md`, `IDENTITY.md`, `USER.md`, `MEMORY.md`, `memory/YYYY-MM-DD.md` | Same names except `AGENT.md` (singular, distinct from repo `AGENTS.md`); `IDENTITY.md` folded into `SOUL.md`; dated notes are ticket reports |

Generated provider files are written to the session's launch area in the state
dir, never into the product repository, and regenerated on every launch.

## Launch

1. Resolve the agent's `AGENT.md`; pick provider by preference, availability,
   quota and the cross-vendor rule; compile to the provider's native form.
2. Context pack: `SOUL.md` and the `AGENT.md` body first (byte-stable, so prompts
   cache), then the `MEMORY.md` index and the task's verified items.
3. Drift between `AGENT.md` and a running session (model, effort, permissions) is
   reported as "restart to apply", never switched mid-task.

## First implementation: the master (CAD-339, MVP)

Until CAD-338 lands, only the master is an agent folder, and it lives where
this record puts the system filesystem: **`<pm>/agents/master/`** — the
tracker dir (`~/pm`, or `CADENCE_PM_DIR`), which is already git with one
writer. `agents/` carries no `project.yaml`, so the tracker never reads it as
a project; the project key `agents` is refused by `issue project add` and
flagged by lint. The repo ships the default `agents/master/SOUL.md` and
`AGENT.md`; `cadence master start` (operator only) installs whichever is
missing in one tracker commit, then has the daemon launch alias `master` as a
managed **Claude** session and queue its briefing — SOUL.md then AGENT.md,
verbatim. The master is Claude-only for now: a Codex master needs a read-only
sandbox with its writes going through daemon verbs (follow-up). Its working
directory is an empty folder under the state dir (`<state>/master/cwd`), never
the tracker or a repo, where any agent could plant a `CLAUDE.md`, hooks or
settings; the tracker is passed as `CADENCE_PM_DIR`. The master is started
explicitly, never at daemon start (auto-start belongs to the setup wizard,
CAD-327).

What is enforced, and how:

| Rule | Enforced by | Gap (process guard, not a security boundary) |
|---|---|---|
| The master reaches only its own verbs | The daemon's master policy is an **allowlist** (`MASTER_ALLOWED`): reads, `plan_propose`, `master_dispatch`, `question_escalate`, `master_summary`, `message_report` on its own messages. Every other method — present or added later — is refused before it runs; a test walks the whole method table | A process that escapes the master's tree (setsid + double fork) is not recognised as the master (CAD-276's residual; CAD-384 for all agents) |
| Its tools are those verbs only | Claude launch: `--restricted` (no user/project/local settings files), `--strict-mcp-config` (no MCP servers), `--tools Bash`, `--permission-mode dontAsk`, and `--allowedTools` listing the exact `cadence` subcommands — never `cadence *` (`build-slot run -- <argv>` would exec anything); edits, `gh`, `git push/merge/commit` also disallowed. Fixed by alias, never by stored params | Claude's own prefix matching is the boundary for shell tricks inside an allowed command |
| It dispatches only approved plan tickets, once, by the rules | `master_dispatch`: the daemon checks the ticket is in an approved plan, `ready`, its `blocked_by` done or dropped, and sends to the ticket's own agent (`--to` only when none is named) the standard kickoff composed from the ticket. `agent_send`/`agent_ask`/jobs are refused to the master | As above |
| Only the operator writes `SOUL.md`/`AGENT.md` | `cadence master edit` → daemon `agent_file_write`, proven operator only; every write and install records a per-file sha256 in the state dir, and `master start` refuses a file whose digest changed (before it writes anything) | A same-uid process can edit the file and the digest record; the edit is caught only at the next start |
| An escalation is the master's (or operator's) | `cadence master escalate` → daemon `question_escalate`, bound to the connection; the record is in the state dir and is the only source of Needs-you question rows. There is no `escalate` report kind — a forged one fails `report file` and lint | Same-uid write to the state dir |
| No merge, push or platform effect | Launch: no forge/platform tokens in env, `GH_CONFIG_DIR` empty, `GIT_TERMINAL_PROMPT=0`, plus the tool posture above | Credentials in files are covered by the read confinement below, for the master's own tree only |
| It reads only its views (CAD-439) | Claude Code auto-allows read-only Bash commands (`id`, `ps`, `echo <glob>`, `cat` in its working dirs) in every mode, `dontAsk` included, and `--file` verbs read any path, so the daemon launches the provider under `cadence confine` (Linux Landlock): system trees, the Claude CLI's install (read-only), the programs it runs (a `cadence` release: its whole releases root, so an upgrade mid-run keeps working), the tracker, and the master's own cwd, temp dir, briefing and Claude config dir (`<state>/master/claude`, its `CLAUDE_CONFIG_DIR`, 0700). The master has its own, separate login by default: `master start` copies nothing, reports `login: none` and prints `CLAUDE_CONFIG_DIR=<state>/master/claude claude auth login`, and Needs-you shows that command while the dir holds no login; `master start --copy-login` is the operator's opt-in copy of the `claudeAiOauth` entry only, 0600, recorded as `master_login_copied`. Nothing of the operator's `~/.claude`/`~/.claude.json`, the rest of `$HOME`, the state dir or `/proc`; no pty; inherited by every descendant, `setsid` included. On Landlock ABI 6+ it also cannot signal processes outside it or reach abstract unix sockets. Without Landlock `master start` refuses; `--unconfined` starts it unwrapped, with a warning, a `master_started_unconfined` event and a Needs-you info row | Landlock confines the filesystem, not what the process asks others to do: arbitrary exec escapes through unix-socket services (`systemd-run --user`, tmux, ssh-agent) — the Bash allowlist is the barrier against arbitrary exec, Landlock the backstop for reads. The master's tree reads its own login copy. The tracker's `.git/hooks` and `.git/config` sit inside its write set (Landlock cannot exclude a subtree; no write primitive found). With `--copy-login` both hold one refresh token: if the provider rotates refresh tokens, a refresh on one side can sign the other out — why a separate login is the default. Where Landlock works the master is confined whatever its stored params say |

`thread_send` (the operator's chat) needs a provably-operator connection
(CAD-276), so a detached child of any agent cannot post as the operator; the
board relays browser writes from its own process (CAD-313's gap).

## Migration

- `docs/roles/*.md` and `~/.local/state/cadence/roles/` become
  `agents/<slug>/AGENT.md` (pm, dev, qa, devops, research, architect, curator).
- Settings model defaults per provider and team role become the `preferred`
  fields of those agents; the Settings screen edits the files.
- Today's registry aliases (e.g. `qa-1`, `ops-1`) become sessions of agents
  (`qa#1`, `devops#1`); `--team-role` maps to the agent slug; `--role pm|worker`
  authority derives from the agent (CAD-345 unifies the vocabulary).

## Open questions

- One `USER.md` or one per operator (team mode, later)?
- Should provider-specific quirks live in agent memory with a `provider:` scope?
