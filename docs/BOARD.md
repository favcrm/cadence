# Cadence board

An internal project tracker: one folder per issue in a private directory,
a `cadence issue` CLI as the only writer, and a read-only `cadence ui`
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
│   ├── CAD-16/
│   │   ├── issue.md        # YAML frontmatter + body + ## Acceptance checkboxes
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
| `status` | Latest note whose header carries `Issue: <id>` beats the file; a container's status rolls up from its children. `status_source` says which: `file` \| `notes` \| `rollup`. |
| `blocks` / `duplicates` | Inverses of `blocked_by` / `duplicate_of`, computed at read time. |
| `ready` | Leaf issue, status `ready`, nothing unfinished in `blocked_by`. |
| `blocked` | Any `blocked_by` target not `done`. |
| counts, activity, sessions | Comments/artifacts/refs lengths; merged notes+comments+git log; agents whose running turn names the id. |

Statuses: `backlog ready doing review done dropped`. Issues are never
deleted — `dropped` is the end state. `dropped` issues don't render on
the board (`issue ls` still lists them).

## `cadence issue` — the only writer

```bash
cadence issue init                          # create pm.yaml + README + git init
cadence issue project add cadence --prefix CAD --repo ~/Project/cadence \
    --component adapter --owner cookie-cesium
cadence issue new "title"                   # --project wins, else CADENCE_PROJECT,
                                            # else the cwd repo's remote/path; no
                                            # match fails closed. --parent, --priority,
                                            # --blocked-by, --owner, --component, --id
cadence issue ls [--project p] [--status s] [--ready] [--json]
cadence issue show CAD-16 [--json]
cadence issue set CAD-16 status=doing owner=fable-cc
cadence issue link CAD-16 blocked_by CAD-12 # also relates|parent|duplicate_of
cadence issue unlink CAD-16 blocked_by CAD-12
cadence issue ref CAD-16 pr https://… --label "PR #16"
cadence issue comment CAD-16 -m "text" --author me
cadence issue attach CAD-16 ./shot.png      # copies into artifacts/, 1 MiB cap
cadence issue lint                          # schema, links, depth, sizes → exit !=0
```

Every write is exactly one git commit in the PM repo, made under a lock
file (`~/pm/.lock`) with atomic `issue.md` replacement. Writes validate
what lint would catch: dangling links, `blocked_by`/parent cycles,
depth > 2, oversize artifacts, unknown components/statuses/priorities.

## `cadence ui` — the reader

```bash
cadence ui run                  # foreground, 127.0.0.1:3010
cadence ui start [--port 3010]  # detached; pid + ui.log under state dir
cadence ui status               # pid + /api/health probe
cadence ui stop
```

`run`/`start` take `--dist <dir>` to serve an unpacked SPA. Built with
`--features ui`, the binary embeds `ui/dist` (three files: index.html,
assets/index.js, assets/index.css — Vite emits fixed names and inlines
fonts) so `--dist` is unnecessary.

### API (GET/HEAD only)

| Route | Returns |
|---|---|
| `GET /api/health` | `ok`, `pm_dir`, `pm_present`, counts, `daemon`, `embedded` |
| `GET /api/projects` | folders, prefixes, components, repos, issue counts |
| `GET /api/issues?project=` | card views: derived status, readiness, counts |
| `GET /api/issues/:id` | the drawer payload: frontmatter, body, links both ways, refs, files, comments, notes chain, merged activity |
| `GET /api/issues/:id/file` | raw `issue.md`, `text/markdown` |
| `GET /api/issues/:id/activity` | the merged activity stream only |
| `GET /api/agents` | agent rows + running/queued/fenced/parked totals; `daemon:"unreachable"` when the socket is down |

### Security posture

No auth in I1 — containment is the defence:

- loopback bind only (`127.0.0.1`), no CORS headers
- `Host` allowlist: `cadence.localhost[:18000]`, the bind address forms,
  plus `--allow-host` extras → `421` otherwise
- `POST`/`PUT`/`DELETE`/… → `405`; `HEAD` allowed
- issue ids must match `<PREFIX>-<n>` before any filesystem use → `400`
- static paths canonicalize inside `--dist` → traversal `400`
- no arbitrary file read, no command execution, no git/PR/dispatch
  endpoints — that list must not grow without auth

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

The original mock is `ui/design/board-mock-v4.html`. Cards are not
draggable in I1 — the write path is I2.

## Seeding

`scripts/seed-pm.sh [pm-dir]` runs the whole dataset through the CLI —
one commit per write, ending with `issue lint`. It refuses to run over
an existing `pm.yaml`; point `CADENCE_PM_DIR` at a fresh dir for a test
seed.
