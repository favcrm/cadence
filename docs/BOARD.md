# Cadence board

An internal project tracker: one folder per issue in a private directory,
one writer implementation (`src/issue/write.rs`) shared by the
`cadence issue` CLI and the board's HTTP write API, and `cadence ui`
serving a board + JSON API on loopback.

- **Tracker dir:** `~/pm` (or `CADENCE_PM_DIR`). A private git repo —
  never pushed anywhere public.
- **Notes dir:** `/var/www/agent-notes` (`notes_dir` in `pm.yaml`) —
  kickoff/QA/verdict chains, read-only.
- **Runtime:** the cadence daemon socket, read-only. The board and CLI
  work fine with the daemon down; the runtime strip just shows
  `daemon unreachable`.
- **Board:** `http://cadence.localhost:18000` through the dev gateway
  (nginx → `127.0.0.1:3010`).

## Issue folder

```
~/pm/
├── pm.yaml                 # schema, statuses, link types, artifact cap, notes_dir
├── README.md               # the rules, for agents and humans
├── cadence/
│   ├── project.yaml        # key, prefix: CAD, repos, components, default_owner
│   │                       #   (dispatch hint only — never stamped on new issues)
│   ├── CAD-16/
│   │   ├── issue.md        # YAML frontmatter + body; ## Acceptance checkboxes
│   │                       #   (a bare `- [ ]` is a stub and does not count)
│   │   ├── comments/
│   │   │   └── 20260917T172400Z-fable-cc.md   # one file per comment, create-only
│   │   └── artifacts/      # files under artifact_max_bytes (1 MiB)
│   └── CAD-17/issue.md
├── sportslog/project.yaml  # prefix: SPL
└── ops/project.yaml        # prefix: OPS, work with no repo
```

`issue.md` frontmatter:

```yaml
---
id: CAD-16                  # must equal the folder name
title: claude provider: managed stream-json endpoint
status: backlog             # file status; notes/rollup can override it
priority: P1                # P0..P3
owner: cookie-cesium        # optional
component: adapter          # optional, must be declared by the project
parent: CAD-30              # optional, one level of sub-issues
blocked_by: [CAD-12, CAD-14]
relates: [CAD-18]
duplicate_of: CAD-7         # optional
refs:                       # pointers, not files
  - {kind: pr, url: https://github.com/favcrm/cadence/pull/16, label: "PR #16"}
  - {kind: note, path: /var/www/agent-notes/20260917-…-kickoff.md}
created: 2026-09-17T16:01:23Z
---
```

## Derived, never stored

| Field | Rule |
|---|---|
| `status` | A container's status rolls up from its children; else a bound job's task states (`dispatched`/`running`/`revising` → `doing`, `review` → `review`, all-terminal → `done`, `blocked` only flags `blocked_reason`); else the latest note carrying `Issue: <id>`; else the file. `status_source` says which: `rollup` \| `job` \| `notes` \| `file`. Draft tasks are unstarted templates and never drive the board. |
| `blocks` / `duplicates` | Inverses of `blocked_by` / `duplicate_of`, computed at read time. |
| `ready` | Leaf issue, status `ready`, nothing unfinished in `blocked_by`. |
| `blocked` | Any `blocked_by` target not `done`. |
| counts, activity, sessions | Comments/artifacts/refs lengths; merged notes+comments+git log; agents bound through `agent.tasks` × `jobs.issue_id` — exact, never a text scan. |

Statuses: `backlog ready doing review done dropped`. Issues are never
deleted — `dropped` is the end state. `dropped` issues don't render on
the board (`issue ls` still lists them).

## `cadence issue` — the only writer

```bash
cadence issue init                          # create pm.yaml + README + git init and
                                            # install the git hooks: pre-commit runs
                                            # `issue lint`, post-commit background-
                                            # pushes `origin`. Idempotent — ours are
                                            # refreshed, a foreign hook is kept and
                                            # reported, never overwritten
cadence issue doctor                        # read-only health: root, git repo,
                                            # remote, each hook (present/executable/
                                            # ours), lint, push lag, the
                                            # push-failures.log tail; exit !=0 on a
                                            # failing check
cadence issue project add cadence --prefix CAD --repo ~/Project/cadence \
    --component adapter --owner cookie-cesium
cadence issue new "title"                   # --project wins, else CADENCE_PROJECT,
                                            # else the cwd repo's remote/path; no
                                            # match fails closed. --parent, --priority,
                                            # --blocked-by, --owner, --component, --id
cadence issue ls [--project p] [--status s] [--ready] [--json]
cadence issue ls --at <rev> [--project p] [--json]
                                            # the board as it was at <rev> —
                                            # cards report status_source: file
cadence issue show CAD-16 [--json]
cadence issue log CAD-16 [--limit 50]       # parsed git log for the issue
                                            # folder: sha, at, by, kind,
                                            # summary, fields for `set`
cadence issue diff CAD-16 [<rev>] [--to <rev>]
                                            # field-level from/to, body line
                                            # counts, comment/artifact files
cadence issue blame CAD-16                  # per field: the entry that last
                                            # changed it (value, sha, at, by)
cadence issue trailer CAD-16                # prints `Issue: CAD-16` — the
                                            # trailer line for code commits
cadence issue set CAD-16 status=doing owner=fable-cc
cadence issue link CAD-16 blocked_by CAD-12 # also relates|parent|duplicate_of
cadence issue unlink CAD-16 blocked_by CAD-12
cadence issue ref CAD-16 pr https://… --label "PR #16"
cadence issue comment CAD-16 -m "text" --author me
cadence issue attach CAD-16 ./shot.png      # copies into artifacts/, 1 MiB cap
cadence issue lint                          # schema, links, depth, sizes, symlinks
                                            # → exit !=0 on errors; warnings
                                            #   (e.g. ready/doing/review with an
                                            #   open blocked_by) never fail it
cadence issue sync [--no-push] [--dry-run] [--resolve ours|theirs]
cadence issue set CAD-16 owner=             # empty value clears the field
```

Every write is exactly one git commit in the PM repo, made under a lock
file (`~/pm/.lock`) with atomic `issue.md` replacement. Field values are
validated inside `issue::write` itself — on `new`, `set` and the HTTP
PATCH alike — so a bad value can never be committed and lint stays the
safety net: `status` is one of the six, `priority` is P0–P3, `component`
must be declared by the issue's project (`""` clears it), and every
link target must exist — on `link`/`unlink` and on the `--blocked-by`/
`--parent` create fields alike. Writes also validate what lint would
catch: `blocked_by`/parent cycles, depth > 2, oversize artifacts.
Symlinks inside the tracker are never followed — a linked folder or
`issue.md` is invisible to reads, an error in lint, and refused by
writes.

## Multi-host trackers — `issue sync`

The post-commit hook pushes `origin` opportunistically after every
write, but when two hosts both write before either pushes, the losing
push is rejected non-fast-forward and the tracker's branch diverges.
`cadence issue sync` is the recovery: it fetches `origin`, rebases the
local branch onto `origin/<branch>`, lints the merged tree, then pushes
the result. The branch must be clean, no rebase or merge may be in
progress, and `origin` must exist — each refusal names the offending
paths.

A same-file conflict aborts the rebase and restores the tree exactly as
found (same HEAD, clean status), reporting each conflicted path with
both the local and remote commit subjects — exit 1, nothing pushed.
`--resolve ours|theirs` instead takes the named side whole for every
conflict and continues; a replayed commit that becomes empty is
skipped. A rebase that merges cleanly but fails `issue lint` aborts the
same way — the tree is restored and the push never happens.

`--dry-run` fetches and reports `ahead`/`behind` plus `would_conflict`
paths without touching the tree or the remote; `--no-push` rebases and
lints but leaves the push for a later sync. `issue doctor` reports the
same ahead/behind counts against `origin/<branch>` and points at
`issue sync` whenever the local side is behind.

## History — from git alone

Every write is one commit whose subject carries the verb
(`CAD-16: set status=review (operator (ui))`), so the tracker's own
log is the audit trail. Each commit also carries trailers after a
blank line: `Issue: <ID>` once per issue the write touches (link and
unlink record both ends) and `Actor: <who>` — resolved from an
explicit `--author`/`--by`, the API actor string, `CADENCE_ALIAS`,
else `operator`. Lint never requires trailers on historical commits;
`issue doctor` reports the trailer share of the last 50 commits,
informational only. The history verbs are strictly read-only — no
lock, no commit, no fetch — and work on a tracker with no remote. A
PM dir that is not a git repository refuses cleanly.

- `issue log <ID> [--limit N]` walks `git log` for the issue folder
  (`issue.md`, `comments/`, `artifacts/` — `--follow` stays off so
  identical templates never leak a sibling's commits) and parses each
  entry: `sha` (short), `at` (RFC 3339 UTC), `by` (the `Actor:`
  trailer, else the ` (actor)` suffix, else `comment by <name>`, else
  the commit author), `kind`
  (`created|set|link|unlink|ref|comment|attach|other`), `summary`
  (subject minus the id prefix and actor), and a `fields` map for
  `set` entries. Commits that are not cadence-shaped — hand edits,
  reverts, sync replays with foreign subjects — appear as `other`
  with the raw subject; nothing in the walk can fail on them.
- `issue diff <ID> [<rev>] [--to <rev>]` diffs two snapshots of
  `issue.md` field by field — `{field, from, to}` over the fixed
  frontmatter keys — plus `body_changed`, added/removed body line
  counts, and comment/artifact files added or removed. Bare `diff`
  compares the issue's newest change with its parent; `<rev>` is a
  from-revision (`diff <ID> <first-sha> --to HEAD` shows everything
  since creation). Revs must resolve to commits reachable from `HEAD`
  — unknown or unrelated revs are refused by name.
- `issue blame <ID>` reports, for every frontmatter field currently
  set, the history entry that last changed it (`value`, `sha`, `at`,
  `by`) — derived by comparing parsed frontmatter at each commit with
  its parent, not by reading raw `git blame` line output.
- `issue ls --at <rev> [--project P]` exports the tree at `<rev>`
  (`git archive | tar`) into a temp dir, loads it through the normal
  loader, and discards the export. Job and note derivation is a
  property of *now*, so every card reports `status_source: "file"`
  (containers still `rollup` — that is the tree's own truth). The
  response carries `at: {sha, time}`.
- `issue trailer <ID>` prints the exact `Issue: <ID>` trailer line so
  agents and hooks can tag code commits without guessing the format.
  The issue's detail (`issue show --json`, `GET /api/issues/<ID>`,
  the drawer's Commits section) lists matching commits from the
  project's `project.yaml` repos: the newest ≤20 on any ref whose
  message carries an `Issue: <ID>` trailer or names the id as a
  whole word in the subject (the `(CAD-47)` squash-merge convention)
  as `{repo, sha, at, author, subject}` — read-only `git log --all`
  bounded to 2000 commits and 5s per repo; a missing or non-git repo
  path lands in `commits_skipped`, never an error.

The board API serves the same entries:
`GET /api/issues/<ID>/history?limit=N` (default 50) — same shape as
`issue log`. The drawer's History section lists them newest-first,
10 at a time with a "show more" that refetches a larger limit.

## `cadence ui` — the reader

```bash
cadence ui run                  # foreground, 127.0.0.1:3010
cadence ui start [--port 3010]  # detached; pid + ui.log under state dir
cadence ui status               # pid + /api/health probe
cadence ui stop
```

`run`/`start` take `--dist <dir>` to serve an unpacked SPA. Built with
`--features ui`, the binary embeds `ui/dist` (index.html,
assets/index.js, assets/index.css, and the latin woff2 files — Vite
emits fixed names) so `--dist` is unnecessary.

### API — reads

| Route | Returns |
|---|---|
| `GET /api/health` | `ok`, `pm_dir`, `pm_present`, counts, `daemon`, `embedded` |
| `GET /api/projects` | folders, prefixes, components, repos, issue counts |
| `GET /api/issues?project=` | card views: derived status, readiness, counts, `rev` |
| `GET /api/issues/:id` | the drawer payload: frontmatter, body, links both ways, refs, files, comments, notes chain, merged activity |
| `GET /api/issues/:id/file` | raw `issue.md`, `text/markdown` |
| `GET /api/issues/:id/activity` | the merged activity stream only |
| `GET /api/issues/:id/history?limit=N` | parsed git history for the issue — same entries as `issue log` (default limit 50, `400` on a bad limit) |
| `GET /api/issues/:id/artifacts/:name` | one artifact file — inline for a small safe list (`text/plain` for md/txt/logs/code, images), **`Content-Disposition: attachment` for everything else, always for html/svg/xml/js/pdf**. Every artifact response carries `Content-Security-Policy: sandbox; default-src 'none'` and `Cache-Control: no-store`; names must satisfy the write grammar (no `/`, no leading dot); symlinks → `404` |
| `GET /api/agents` | worker agent rows enriched with the exact task/issue binding (`agent.tasks` × `jobs.issue_id`) + running/queued/fenced/parked totals and `by_issue` for card strips; `endpoint_kind: inbox` mailboxes are counted separately under `inboxes`; `daemon:"unreachable"` when the socket is down |
| `GET /api/agents/:alias` | the agent drawer: `agent_show` + last 20 events + `tasks`/`on` bindings + `recovery`/`resume` commands; `404` on unknown alias, `400` on alias grammar |
| `GET /api/stream` | server-sent events — `event: issues` on tracker change, `event: jobs`/`event: agents` on daemon state change; `: ping` immediately and every 15 s of silence; `405` on HEAD; deltas only (baselines at connect) |

### API — writes

Every write goes through `issue::write` — the same functions the CLI
runs — so the API is a second front door, not a second writer. Each
successful call is exactly one git commit whose subject carries the
actor: `CAD-16: set status=review (operator (ui))`.

| Route | Body | Returns |
|---|---|---|
| `POST /api/issues` | `{project, title, priority?, owner?, component?, parent?, blocked_by?}` | `201` |
| `PATCH /api/issues/:id` | `{status?, priority?, owner?, component?, title?, body?, if_rev?}` — `""` clears owner/component; `body` replaces the markdown only | `200` |
| `POST /api/issues/:id/links` | `{type: blocked_by\|relates\|parent\|duplicate_of, target, if_rev?}` | `200` |
| `DELETE /api/issues/:id/links` | same shape | `200` |
| `POST /api/issues/:id/refs` | `{kind, url\|path, label?, if_rev?}` — exactly one of url/path | `200` |
| `POST /api/issues/:id/comments` | `{body, if_rev?}` — author `operator`, kind `ui`, markdown stored verbatim | `200` |
| `POST /api/issues/:id/artifacts?name=<base>` | raw bytes, create-only | `200` |

Success bodies are `{issue, card, warnings}` — the fresh payloads, so
the UI needs no second fetch. `warnings` notes a `ready`/`doing`/`review`
status that still has open blockers (usable, just flagged). Conflicts
are `409` with `conflict: if_rev | status_derived | exists` plus the
current card; a `job`-, `notes`- or `rollup`-derived status refuses
`status` writes with the reason. `if_rev` is the `rev` field — a hash of
`issue.md` — for optimistic concurrency; a stale one returns `409` and
the current rev. Unknown JSON fields are rejected
(`deny_unknown_fields`); JSON bodies cap at 256 KiB, artifact uploads at
`artifact_max_bytes` (1 MiB) enforced while reading — the body is never
fully buffered first.

### Security posture

No auth — **containment is the whole defence**, so the write path adds
four cross-site guards, each checked in order before any body is read
or any writer runs, and each refusal names its check in `403` JSON:

1. **Route + method.** Writes are `POST`/`PATCH`/`DELETE` on known
   shapes only; a known shape with the wrong method is `405`, anything
   else `404`.
2. **Exact content type.** JSON routes require exactly
   `application/json`; the artifact route requires exactly
   `application/octet-stream`. A cross-site HTML form can only send
   "simple" types (`text/plain`, `x-www-form-urlencoded`, `multipart`)
   — all refused here.
3. **`X-Cadence-Board: 1`.** A custom header a cross-site request
   cannot send without a CORS preflight — and this server never
   answers a preflight: `OPTIONS` is `405` and no response ever carries
   `Access-Control-*`.
4. **Origin / fetch metadata.** If `Origin` is present it must be one
   of the allowlisted board origins (the `Host` allowlist over http);
   if `Sec-Fetch-Site` is present it must be `same-origin`.

Then the I1 containment still holds:

- loopback bind only (`127.0.0.1`); `Host` allowlist → `421`
- issue ids must match `<PREFIX>-<n>` before any filesystem use → `400`;
  artifact names match the same `[A-Za-z0-9._-]` basename grammar
- static paths canonicalize inside `--dist` → traversal `400`
- symlinks are never followed: a linked project/issue folder, `issue.md`,
  comment, artifact file or `artifacts/` dir is invisible to reads, an
  error in `lint`, and refused by writes
- every response carries `X-Content-Type-Options: nosniff` and
  `Referrer-Policy: no-referrer`; HTML gets `default-src 'self'` CSP;
  artifact bodies get `sandbox; default-src 'none'` and only the safe
  allowlist renders inline — html/svg/xml/js/pdf always download
- artifact reads serve regular files only, never follow `..`, and a
  symlinked artifact is `404`
- no arbitrary file read, no command execution, no git/PR/dispatch
  endpoints — that list must not grow without auth

The honest limit: anyone who can open `http://127.0.0.1:3010` from this
machine — a local process, or a browser tab on an allowed origin — can
write the tracker. That is the threat model: a private repo on a
single-operator host, loopback plus the guards above. Auth is deferred
to I3+.

## Frontend

`ui/`: Vite + React 19 + TypeScript + Tailwind 4, pnpm. Design tokens
per `~/Project/agentic-alpha/DESIGN.md` — graphite ink ramp, IBM Plex
Sans/Mono, teal `#2dd4bf` on interactive elements only.

```bash
cd ui
pnpm install
pnpm dev         # vite dev server, proxies /api → 127.0.0.1:3010
pnpm typecheck   # tsc --noEmit
pnpm build       # → ui/dist (committed; --features ui embeds it)
```

The original mock is `ui/design/board-mock-v4.html`. In I2 cards drag
between columns (derived/container cards don't — the reason shows on
hover), the drawer edits fields/body/links/refs, comments and attaches
artifacts, and backlog has quick-add. In I3 the board is live: the SPA
opens an `EventSource` on `/api/stream` and each `issues`/`jobs`/`agents`
frame triggers the normal refresh — EventSource reconnects on its own
and the 30 s/focus poll stays as the fallback. The Agents screen ranks
fenced agents first, shows the daemon's recovery text verbatim, and
opens a drawer with identity, params, capabilities, tasks, bound
issues, running messages, and the event tail. A fence banner on the
board links straight to it.

## Seeding

`scripts/seed-pm.sh [pm-dir]` runs the whole dataset through the CLI —
one commit per write, ending with `issue lint`. It refuses to run over
an existing `pm.yaml`; point `CADENCE_PM_DIR` at a fresh dir for a test
seed.
