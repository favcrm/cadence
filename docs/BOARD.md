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
│   ├── project.yaml        # key, prefix: CAD, repos, components, tags, default_owner
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
tags: [claude, provider]    # optional; [a-z0-9][a-z0-9-]{0,31}, ≤12, stored
                            # sorted + de-duplicated; from project.yaml's
                            # `tags:` list when the project declares one
parent: CAD-30              # optional, one level of sub-issues — an issue
                            # with children is an epic
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
                                            # --tag t (repeatable) declares the
                                            # project's tag vocabulary
cadence issue new "title"                   # --project wins, else CADENCE_PROJECT,
                                            # else the cwd repo's remote/path; no
                                            # match fails closed. --parent, --priority,
                                            # --blocked-by, --owner, --component, --id,
                                            # --tag t (repeatable), --epic <ID>
                                            # (= --parent)
cadence issue ls [--project p] [--ready] [--json]
    [--tag t]... [--status s]... [--epic <ID>]  # filters combine (AND): every --tag
    [--owner o] [--component c]             # must be present, any --status may
    [--priority P1] [--open]                # match (derived status), --epic lists
                                            # its children, --open = not done or
                                            # dropped; an aligned table without --json
cadence issue epic ls [--project p] [--json]
                                            # epics = issues with children: total,
                                            # counts per status, done_ratio (done ÷
                                            # total − dropped), blocked, owners
cadence issue epic show CAD-38 [--json]     # the epic's row + its children:
                                            # status, owner, priority, tags
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
cadence issue retro CAD-16 [--json]         # read-only retrospective: rounds,
                                            # caught defects, flakes, timings,
                                            # proposed lessons + explicit
                                            # unknowns from tracker/notes/
                                            # store evidence — nothing written
cadence issue trailer CAD-16                # prints `Issue: CAD-16` — the
                                            # trailer line for code commits
cadence issue start CAD-16                  # mints .cadence/wt/cad-16-<slug> on
    [--repo <path>] [--name <slug>]         # cadence/cad-16-<slug> in the project
    [--base <ref>] [--owner <who>]          # repo, records branch+worktree refs,
    [--job --pm <alias> --spec <file>       # moves backlog|ready to doing, prints
     [--assignee <alias>]]                  # the trailer; --job also opens the M3
                                            # job + scoped task (daemon required)
cadence issue set CAD-16 status=doing owner=fable-cc
cadence issue set CAD-16 tags=claude,provider
                                            # replaces the tag list; `tags=` clears
cadence issue set CAD-16 CAD-17 CAD-18 status=ready priority=P1
                                            # bulk: ids first, then pairs — one
                                            # commit, nothing written unless every
                                            # id and value is valid
cadence issue tag CAD-16 CAD-17 add ui      # add|rm tags on one issue or several;
cadence issue tag CAD-16 rm ui api          # same one-commit, all-or-nothing batch.
                                            # Issues the edit leaves unchanged stay
                                            # out of the commit; a batch that
                                            # changes nothing is refused
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
must be declared by the issue's project (`""` clears it), `tags` are
well-formed, at most 12, stored sorted and de-duplicated — and drawn
from the project's `tags:` list when `project.yaml` declares one (no
list accepts any well-formed tag; an absent `tags` means none) — and
every link target must exist — on `link`/`unlink` and on the `--blocked-by`/
`--parent` create fields alike. Writes also validate what lint would
catch: `blocked_by`/parent cycles, depth > 2, oversize artifacts.
A bulk `set`/`tag` is still one commit: its subject names every id
(`CAD-16, CAD-17: set status=ready`) and carries one `Issue:` trailer
per id, so each issue's `log` and `blame` read it as their own.
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
  (`created|set|tag|link|unlink|ref|comment|attach|other`), `summary`
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

## Issue worktrees — `issue start`

`issue start <ID>` is the one command a worker runs to begin: it mints
`.cadence/wt/<id-lower>-<slug>` on branch `cadence/<id-lower>-<slug>`
inside the project repo, records both as refs on the issue, moves
`backlog`/`ready` to `doing` (other statuses are left alone), sets
`owner` when empty (`--owner`, else the resolved actor), and prints
everything the worker needs — worktree, branch, base `{ref, sha}` and
the `Issue: <ID>` trailer — as JSON.

- **Repo** resolves `--repo`, then the cwd's repo when it is one of
  the project's `project.yaml` repos, then the project's only repo —
  else it refuses naming the candidates. An explicit `--repo` must be
  one of the declared repos (code-commit discovery only walks those);
  an undeclared path is refused with the declared list and a pointer
  to `repos` in `project.yaml`. **Base** resolves `--base`, then the
  repo's `origin/HEAD` target, then the current branch; no fetch ever
  runs.
- The worktree is minted through the same helper as
  `cadence devin --worktree` (shared `src/worktree.rs`), including the
  `.gitignore` `.cadence/` rule — added only when missing.
- The worktree's cargo builds share one dependency cache — only for
  checkouts that are cargo packages: `issue start` links the
  hashed-content subdirs of the lane's `target/debug/` — `deps`,
  `.fingerprint`, `build`, `incremental` plus cargo's three lock
  files (`.cargo-lock`, `.cargo-build-lock`, `.cargo-artifact-lock`)
  — into `<repo>/.cadence/target/shared/debug`, so dependency
  artifacts compile once per host. `examples` is deliberately not
  linked: cargo uplifts example binaries to unhashed
  `debug/examples/<name>` paths, so sharing would hand one lane
  another lane's example — the same hole as sharing `debug/` whole.
  The lane's `debug/` itself stays a real dir, so uplifted binaries
  like `debug/cadence` are per-lane files — one lane's `cargo test`
  can never exec another lane's binary. Because the lock files are
  shared, concurrent lanes serialise *whole builds* on cargo's own
  locking: the second lane prints `Blocking waiting for file lock on
  build directory` until the first finishes — dedup in exchange for
  queueing, never parallel compiles into one dir. Sharing covers the
  debug host target only: `--release` and `--target <triple>` outputs
  stay per-lane, and a `RUSTFLAGS` change or `cargo clippy` run
  rewrites shared fingerprints — lanes with differing flags will
  thrash each other's cache entries (correct, but rebuild-y). A
  `build: {target_dir: per-worktree}` section in `project.yaml`
  keeps the lane fully private (a previously linked farm is
  unlinked). A checkout with no `Cargo.toml` gets no farm at all —
  no `target/` is created and a `target/debug/build` it already owns
  is never moved. A `build.target-dir` anywhere in cargo's config
  chain — the worktree's own `.cargo/config.toml`, an ancestor's, or
  `$CARGO_HOME/config.toml` — overrides the whole mechanism: the
  farm is not planted and the recorded `cargo_target` is the
  operator's dir; nothing under `.cargo/` is ever written by
  cadence, so a tracked config survives byte-for-byte. A
  `CARGO_TARGET_DIR` env overrides the same way it always has. The
  effective dir is recorded as `cargo_target` on the `worktree` ref
  and printed as `target_dir`; a stale recorded value is corrected
  on re-start with its own commit. If a pre-existing cargo lock file
  is held by a running build, `issue start` refuses rather than
  leaving the lane half-shared — the lock check runs before any of
  the lane's artifacts move, so a live build loses nothing; retry
  when the lane is idle. Pre-existing artifacts merge into the
  shared dirs; a name already present stays the lane's copy under a
  `<name>.local` sibling, and a path that is a symlink to somewhere
  else — an operator's own link — is refused, not silently
  half-shared.
- One tracker commit records a `branch` ref (label = repo basename)
  and a `worktree` ref (absolute path), the status/owner updates and
  the CAD-42 `Issue:`/`Actor:` trailers under subject
  `<ID>: start <branch>`.
- Idempotent: when the worktree and branch already exist *for this
  issue* (matching refs recorded), the command returns them with
  `created: false` and makes no commit. It refuses — naming both —
  when the branch exists but points somewhere unrelated to a recorded
  worktree.
- `--job --pm <alias> --spec <file> [--assignee <alias>]` opens the
  M3 job through `job_new`: the job carries `--issue`, `--repo` and
  `--base-ref <base sha>`, and the `task_worktree`/`task_branch`/
  `task_base_sha`/`task_assignee` params scope the default `<job>-t1`
  task — exactly one task, already bound to the worktree. The daemon,
  the PM alias and any assignee (the PM itself or a member of its
  group) are probed *before* anything is created, so a daemon-down,
  unknown-PM or bad-assignee run leaves no worktree, branch or
  commit.

Ref kinds `branch` and `worktree` are written by `issue start` (the
drawer renders them as `repo: branch` and `wt/<name>` chips); adding
them by hand via `issue ref` works but is unusual — and a tracker
whose pre-commit hook runs a pre-CAD-43 `cadence` will refuse these
refs at lint time, so upgrade the installed binary first.

## Dispatch and finish — `dispatch`, `issue finish`

`dispatch <ISSUE> --to <worker> --note <kickoff>` is the PM's one-step
hand-off: it runs `issue start --owner <worker>` (idempotent — an
already-started issue is reused), then enqueues exactly one
single-line kickoff built from a fixed template:

```
read <note> — <ISSUE>: <summary or issue title>. Your worktree exists:
<worktree> (branch <branch>, base <sha7>). Commit trailer:
Issue: <ISSUE>. PR to main; reply to <reply-to>.
```

then adds a tracker comment `Dispatched to <worker>: <note>` and a
`message` ref carrying the queued message id. `--reply-to` defaults to
`CADENCE_ALIAS` and is required when that is unset. `--job --spec
<file>` instead opens the M3 job through `issue start --job --pm
<reply-to> --assignee <worker>` and sends the kickoff through
`job dispatch`, so the task and the message are bound.

Everything is checked before anything is created: the note is
readable, the daemon is reachable, the worker exists and is not
fenced (`attention`), for `--job` it is the PM or a member of its
group, and the composed body passes the pty single-line and
forbidden-prefix rules. A second identical run reuses the worktree but
refuses to queue a duplicate while the first kickoff is still queued
or running — the output says so. Delivery itself stays the daemon's
business: no `--ready`, no forced claims; the output prints the queued
message id and the worker's probe verdict so the operator knows
whether it lands now or when the pane idles.

`issue finish <ID>` is the other end — safe cleanup of the recorded
worktree+branch pair. It refuses, naming what it found, while:

- the issue's `owner` agent has a queued/running message or a pty pane
  that probes busy (the daemon must be reachable — it refuses rather
  than guesses; an `inbox` owner is a durable mailbox, never busy),
- the worktree has uncommitted changes — ignored paths like the
  `ui/node_modules` build symlink don't count (the refusal lists what
  does), or
- the branch's work survives nowhere: not merged into the repo's
  default branch (`origin/HEAD`, else the checkout's current branch)
  and not pushed to a remote-tracking ref. "Merged" recognizes
  however the work landed — ancestry, `git cherry` patch-equivalence
  (rebase/cherry-pick), the branch's combined diff reverse-applying
  onto the default tree (a squash merge, checked in a temporary index
  so no worktree is touched), or a merged GitHub PR naming the head
  branch. The output's `merged_by` reports which rule matched.

Then it runs `git worktree remove`, deletes the local branch (with
`--remote` the remote one too), and lands one tracker commit
`<ID>: finish <branch>` that marks both refs `closed: true` — kept as
history, so a later `issue log` still shows where the work lived.
`git worktree remove` takes the worktree dir and nothing else — the
removal unlinks the lane's cache symlinks without following them, so
the shared dep cache is never deleted here. When a `cargo_target`
was recorded, the output reports it with `cargo_target_exists`, a
literal check on the path after the removal (a target inside the
worktree reports `false`; the field is omitted when nothing was
recorded). The issue's status is not touched: status follows the job
or the PM.
`--force` overrides each refusal and is recorded — the `overrode`
list in the output and a `Forced: true` trailer on the commit.
`--keep-branch` removes only the worktree.

## Project memory — `cadence memory`

One reviewed fact per file at `<pm>/<project>/memory/<slug>.md`:
YAML frontmatter (`id`, `type`, `status`, `confidence`, `scope`,
`author`, `source`, `created`, `verified_at`, optional `supersedes`)
plus a body contract — a fact, then `**Why:**` and `**How to apply:**`
sections. Types are `rule | gotcha | decision | recipe`; statuses are
`proposed | accepted | rejected | superseded`.

```bash
cadence memory propose --project demo --type gotcha \
    --scope-path 'src/adapter/**' -m "$(cat <<'EOF'
Retrying a wedged adapter needs daemon restart first.

**Why:** the adapter holds the socket.

**How to apply:** `cadence daemon stop` before the retry.
EOF
)"
cadence memory accept pipe-drain [--project demo]   # curator only
cadence memory reject <slug>    cadence memory supersede <old> <new>
cadence memory verify <slug>    # re-stamp verified_at — curator only
cadence memory ls [--project k] [--status s] [--type t] [--component c]
                  [--path f] [--stale [--days 30]] [--json]
cadence memory show <slug> [--json]
cadence memory match --issue <ID> [--provider p] [--json]
cadence memory lint [--project k]
```

Proposals may come from anyone (the author's `CADENCE_ALIAS` is
stamped); accept/reject/supersede/verify are curator actions — a
cadence worker pane is refused unless the daemon proves it is the PM
or the group root; a missing daemon fails closed. `verify` is gated
because `verified_at` is the curator's re-check attestation and feeds
ranking + staleness. Every write is one tracker commit carrying
`Memory: <slug>` and `Actor:` trailers — never `Issue:`. `issue lint`
validates memory files with everything else (the pre-commit hook
covers them); `memory lint` runs the same checks alone. Lint bounds a
fact block to 5 lines and 512 bytes and a path glob to 200 chars and
two `**` segments; a file that cannot be parsed at all is a warning —
every read path already reports it, and an error there would let one
stray file brick every tracker commit.

Matching is a union: a memory applies when ANY scope axis intersects
the dispatch context — `--scope-project` (every dispatch in the
project), `--scope-component`, `--scope-path` (repo-relative globs
matched against the issue's recorded-commit paths), `--scope-tag`
(issue `tags:`), `--scope-provider` (the target worker's provider).
`match --issue` prints the ranked list: `rule` > `gotcha` > `recipe` >
`decision`, then confidence, then newest `verified_at`.

Injection happens in two places:

- `dispatch` renders the matched accepted set to
  `<state>/dispatch/<message-id>-lessons.md` (≤ 12 entries, ≤ 4 KiB),
  appends `Lessons: <file>.` to the kickoff body, records the slugs in
  the dispatch comment (`Lessons injected: …`) and returns `lessons`
  and `lessons_file` in the JSON. `--no-lessons` skips it; `--job`
  kickoffs are daemon-templated and never carry the file. Memory
  failures degrade, never sink the dispatch (the worktree already
  exists): a memory file that fails to load is excluded and named
  while valid lessons still inject, and an unwritable lessons file or
  a suffix that would push the kickoff body over the pty cap drops the
  file — every case carries a `lessons_error` string naming the
  reason. The lessons file is written via tmp + rename, so a failed
  write never leaves a partial artifact.
- `agent bootstrap`/`join` briefings gain a
  `## Project memory — accepted rules (<project>)` section listing the
  project's accepted `rule`s for the worker's cwd — ≤ 8 entries and
  ≤ 4 KiB, same bound as the dispatch lessons file. An over-budget
  rule is skipped, not a stop; omitted rules are counted.

Memory readers skip files that fail to load and report them: `ls`,
`ls --stale` and `match` warn once on stderr and include `load_errors`
in `--json`; the API returns `memory_errors`. An absent `memory/` dir
is a valid empty store; a directory that exists but cannot be
enumerated is a load error. Write-time glob bounds are re-applied at
load — a hand-edited over-complex path scope is quarantined with an
error before it can reach matching.

Staleness: `ls --stale` flags an accepted memory not verified within
the `--days` window (30 default), or whose path globs match files
changed in a project repo after `verified_at` — informational only;
`verify` is the refresh.

## `cadence ui` — the reader

```bash
cadence ui run                  # foreground, 127.0.0.1:3010
cadence ui start [--port 3010]  # detached; pid + ui.log under state dir
cadence ui status               # pid, /api/health probe, effective options
cadence ui stop [--tailscale-off]

cadence ui tailscale start [--port 9450] [--read-only]
cadence ui tailscale stop
cadence ui tailscale status     # sharing state, tailnet URL, identity probe, QR
```

`run`/`start` take `--dist <dir>` to serve an unpacked SPA. Built with
`--features ui`, the binary embeds `ui/dist` (index.html,
assets/index.js, assets/index.css, and the latin woff2 files — Vite
emits fixed names) so `--dist` is unnecessary. `start` merges flags
over the persisted `ui.json` in the state dir and saves the result —
a later plain `ui start` reuses it, `--reset` forgets it, and
`ui status` prints the effective options. Extra flags:
`--allow-host <name>` / `--allow-origin <origin>` (repeatable — extend
the Host/Origin allowlists), `--read-only` (every write answers `403`,
the SPA hides its edit controls; `--no-read-only` clears a persisted
one).

## Remote access — `ui tailscale`

`cadence ui tailscale start` publishes the board on the tailnet through
`tailscale serve` — the loopback bind never changes, Tailscale's proxy
terminates TLS and forwards to `127.0.0.1:<ui port>`:

```
phone/laptop ── https:<dns>:9450 (tailnet) ──▶ tailscaled
                                                 │ http://127.0.0.1:3010
                                                 ▼
                                            cadence ui
```

`start` checks `tailscale status --json` first and refuses plainly when
tailscale is missing, logged out, not `Running`, or the tailnet has no
HTTPS certs enabled (admin console → DNS → HTTPS Certificates). It then
ensures the `https:<port> → http://127.0.0.1:<ui port>` mapping
idempotently — an identical existing mapping is reused, a *different*
one on the same port is a hard refusal, never overwritten. The tailnet
DNS name (with and without the port) joins the Host allowlist and
`https://<dns>[:port]` the Origin allowlist. `tailscale start` works
whether the board is running or not: it persists the options and
restarts the detached server (brief outage, announced) so the new
allowlists take effect. `ui start --tailscale[=<port>]` is the same
flow for scripts.

`ui tailscale stop` removes only the mapping cadence recorded — checked
against the live serve config, so a foreign mapping on the port is left
alone and named — drops the tailnet options from `ui.json`, and
restarts the board local-only if it was running. Plain `ui stop`
leaves the mapping in place; `ui stop --tailscale-off` is the same
removal without the restart semantics. `funnel` is never invoked —
tailnet-only, no public exposure.

Writers are attributed per request: when tailscale sharing is armed
**and** the TCP peer is loopback **and** the request's `Host` is the
tailnet name, `Tailscale-User-Login` resolves the actor to
`<login> (tailscale)` — the tracker commit's `Actor:` trailer names the
person who wrote. The same headers on a direct loopback request
(non-tailnet `Host`) are ignored; the default actor stays
`operator (ui)`. `GET /api/meta` reports `{read_only, actor,
tailnet_url, version, build_commit, build_time, daemon}` so the SPA
renders the right controls and the serving binary's build identity.

Threat model, unchanged in four lines:

1. **Tailnet-only** — `tailscale serve`, never `funnel`; nothing is
   exposed outside your tailnet.
2. **Loopback bind** — the board still binds `127.0.0.1`; only
   tailscaled (same host) can reach it.
3. **Host + Origin allowlists** — the tailnet name is the only new
   allowed Host; `https://<dns>:<port>` the only new write Origin.
   Everything else is `421`/`403` exactly as before.
4. **Header trust rule** — `Tailscale-User-*` identity headers count
   only on the tailnet `Host` from a loopback peer; forged headers on
   direct loopback are ignored.

`--read-only` on `tailscale start` is the browse-only share: every
write route answers `403` with `check: "read_only"` and the SPA hides
quick-add, drag, edit, link/ref, attach and comment controls.

### API — reads

| Route | Returns |
|---|---|
| `GET /api/health` | `ok`, `pm_dir`, `pm_present`, counts, `daemon`, `embedded` |
| `GET /api/meta` | `read_only`, `actor` (the request's resolved write identity), `tailnet_url`, the serving binary's `version`/`build_commit`/`build_time`, plus the daemon's `daemon_info` when reachable |
| `GET /api/projects` | folders, prefixes, components, declared tags, repos, issue counts |
| `GET /api/issues?project=` | card views: derived status, readiness, `tags`, counts, `rev`. The `issue ls` filters, combinable: `tag=` (repeat or comma-join — all of), `status=` (repeat or comma-join — any of), `epic=<ID>`, `owner=`, `component=`, `priority=`, `open=1`; `400` on a value that could never match (unknown status/priority, bad tag or id grammar) |
| `GET /api/epics?project=` | issues with children — the `issue epic ls --json` payload: `total`, `counts` per status, `done_ratio`, `blocked`, `owners`, `children` |
| `GET /api/issues/:id` | the drawer payload: frontmatter, body, links both ways, refs, files, comments, notes chain, merged activity |
| `GET /api/issues/:id/file` | raw `issue.md`, `text/markdown` |
| `GET /api/issues/:id/activity` | the merged activity stream only |
| `GET /api/issues/:id/history?limit=N` | parsed git history for the issue — same entries as `issue log` (default limit 50, `400` on a bad limit) |
| `GET /api/issues/:id/artifacts/:name` | one artifact file — inline for a small safe list (`text/plain` for md/txt/logs/code, images), **`Content-Disposition: attachment` for everything else, always for html/svg/xml/js/pdf**. Every artifact response carries `Content-Security-Policy: sandbox; default-src 'none'` and `Cache-Control: no-store`; names must satisfy the write grammar (no `/`, no leading dot); symlinks → `404` |
| `GET /api/agents` | worker agent rows enriched with the exact task/issue binding (`agent.tasks` × `jobs.issue_id`) + running/queued/fenced/parked totals and `by_issue` for card strips; `endpoint_kind: inbox` mailboxes are counted separately under `inboxes`; `daemon:"unreachable"` when the socket is down |
| `GET /api/agents/:alias` | the agent drawer: `agent_show` + last 20 events + `tasks`/`on` bindings + `recovery`/`resume` commands; `404` on unknown alias, `400` on alias grammar |
| `GET /api/memories?project=&status=&type=&component=&path=` | memory cards across projects — same filters as `memory ls`; `memory_errors` lists files that failed to load |
| `GET /api/memories/:project/:slug` | one memory: frontmatter + body; `404` on unknown slug, `400` on grammar |
| `GET /api/overview` | the Overview screen payload: `needs_me`, `drift`, `projects`, `github`, `daemon`, `generated_at` — same shape as `cadence overview --json` |
| `GET /api/stream` | server-sent events — `event: issues` on tracker change, `event: jobs`/`event: agents` on daemon state change; `: ping` immediately and every 15 s of silence; `405` on HEAD; deltas only (baselines at connect) |

### API — writes

Every write goes through `issue::write` — the same functions the CLI
runs — so the API is a second front door, not a second writer. Each
successful call is exactly one git commit whose subject carries the
actor: `CAD-16: set status=review (operator (ui))`.

| Route | Body | Returns |
|---|---|---|
| `POST /api/issues` | `{project, title, priority?, owner?, component?, tags?, parent?, blocked_by?}` | `201` |
| `PATCH /api/issues/:id` | `{status?, priority?, owner?, component?, tags?, title?, body?, if_rev?}` — `""` clears owner/component; `tags` replaces the list (`[]` clears) under the CLI's validation; `body` replaces the markdown only | `200` |
| `POST /api/issues/:id/links` | `{type: blocked_by\|relates\|parent\|duplicate_of, target, if_rev?}` | `200` |
| `DELETE /api/issues/:id/links` | same shape | `200` |
| `POST /api/issues/:id/refs` | `{kind, url\|path, label?, if_rev?}` — exactly one of url/path | `200` |
| `POST /api/issues/:id/comments` | `{body, if_rev?}` — author `operator`, kind `ui`, markdown stored verbatim | `200` |
| `POST /api/issues/:id/artifacts?name=<base>` | raw bytes, create-only | `200` |
| `POST /api/memories/:project/:slug/accept` | `{body?}` — curator-gated like `memory accept`; `body` replaces the markdown as the curator's edit | `200` |
| `POST /api/memories/:project/:slug/reject` | `{}` — curator-gated like `memory reject` | `200` |

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

A `--read-only` board short-circuits even earlier: every write route
answers `403` (`check: "read_only"`) before the content-type check.

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

## Overview — needs-me + deploy drift

The landing screen answers two questions at a glance: *what is waiting
on a human or the PM right now, and with which command*, and *is what
we merged actually running*. `GET /api/overview` derives the whole
payload at read time — nothing is stored; `cadence overview [--json]
[--watch <secs>]` renders the same data as an aligned terminal list.

**Needs me** — one row per item, ranked by urgency then age, each with
`{kind, title, age, project, link, command}`:

| Rank | Kind | Command |
|---|---|---|
| 10 | `merge` — open PR with `qa-verdict=success` and green checks | `gh pr merge <n> --repo <slug> --squash --admin --match-head-commit <sha>` |
| 20 | `approval` — a brokered permission request is open | `cadence agent respond <a> --request <h> --decision accept` (provider input requests: `--answers-file <f>`) |
| 30 | `fenced` — agent in `attention` | `cadence agent unfence <a>` |
| 40 | `stalled` — turn silent past the fence threshold | `cadence agent show <a>` |
| 50 | `drift` — merged commits not running while every pane is idle | `cadence daemon restart --when-idle --ui` |
| 60 | `pr_no_verdict` — open PR with no `qa-verdict` status | `gh pr view <n> --repo <slug>` |
| 70 | `review_no_pr` — issue in `review` with no open `pr` ref and no `cadence/<id>-…` PR branch | `cadence issue show <id>` |
| 80 | `blocked_ready` — every `blocked_by` target is `done` | `cadence issue set <id> status=ready` |
| 90 | `ci_red` — default-branch commit status failing | `gh run list --repo <slug>` |
| 100 | `inbox_unread` — unread messages on an `inbox` endpoint | `cadence inbox <a>` |
| 110 | `tracker_behind` — tracker repo behind `@{upstream}` | `cadence issue sync` |

GitHub data (open PRs, default-branch CI) comes from `gh` behind a
60-second cache in the state dir (`overview-gh.json`, keyed by the
slug set, written temp-then-rename); an outage serves the last good
body as `github.state: "stale"` — or `"unavailable"` when there is no
good body — and the screen still renders. Daemon-dependent rows
(approvals, fenced, stalled, inbox, drift) vanish when the socket is
down, reported as `daemon.reachable: false`. Reachability is the
`health` RPC — a daemon that predates `daemon_info` stays reachable
(its agent rows appear) while drift reports the build as unknown.

**Deploy drift** — the daemon reports its `build_commit` via the
`daemon_info` RPC (`build.rs` compiles `CADENCE_BUILD_COMMIT`/
`CADENCE_BUILD_TIME`/remote/root into every binary; `cadence
--version` prints them and `/api/meta` repeats them). The tracker
project whose repo matches that build — normalised remote first, then
checkout path — gets a `rev-list --count <build_commit>..<default
branch>` walk: `drift.count` commits not yet running, subjects bounded
at 20, squash-merged PR numbers parsed from `(#n)`. `unknown` or
unresolvable commits are `known: false` — "cannot tell", never zero.

**Projects** — one row each: open counts by derived status plus the
oldest `review` issue's age.

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
board links straight to it. The Memory tab lists every project's
memories with status/type/component/path filters, opens a detail
panel, and accepts or rejects proposed entries through the same
guarded write path.

A filter bar above the columns slices the board by tag, epic, owner and
component — chips with counts, multi-select: tags narrow (all of them),
the other three widen within themselves (any of them). The selection
lives in the URL (`?project=cadence&tag=ui,api&epic=CAD-38&owner=ann&
component=adapter&group=epic`), so a filtered view is a link. "group by
epic" renders one swimlane per epic with its progress bar (done ÷
children, dropped excluded — over all children, not only the visible
ones) and a last lane for issues with no epic. Cards show their tags;
the drawer edits them through the same PATCH, `if_rev` included, with
the project's declared tags as toggle chips.

## Seeding

`scripts/seed-pm.sh [pm-dir]` runs the whole dataset through the CLI —
one commit per write, ending with `issue lint`. It refuses to run over
an existing `pm.yaml`; point `CADENCE_PM_DIR` at a fresh dir for a test
seed.
