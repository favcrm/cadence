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
owner: cookie-cesium        # optional — the lane doing the work
claim:                      # optional (CAD-383) — who holds the issue while
  by: pm-opus               #  it is in flight, since when, and a note; set
  at: 2026-09-23T14:31:14Z  #  by `issue claim` and by the start/dispatch
  note: claude subagent lane #  that puts it into work — see Claims below
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
| `status` | A container's status rolls up from its children; else a bound job's task states (`dispatched`/`running`/`revising` → `doing`, `review` and `verified` → `review`, `done` → `done`, `blocked` only flags `blocked_reason`); else the latest note carrying `Issue: <id>`; else the file. `verified` stays `review` until `job accept`. `status_source` says which: `rollup` \| `job` \| `notes` \| `file`. Draft tasks are unstarted templates and never drive the board. |
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
cadence project new reminders --repo ~/code/reminders [--prefix REM] \
    [--goal "…"] [--agent pm=1,dev=2] [--issue CAD-9]
                                            # CAD-358, the operator or the master
                                            # (by connection), via the
                                            # daemon: project.yaml + a seeded
                                            # PROJECT.md (goal, agents, default
                                            # stages, no milestones) in one commit;
                                            # re-running it changes nothing; a
                                            # different repo, key `agents`, a bad
                                            # key, a non-git path, the tracker or
                                            # the daemon state dir writes nothing
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
                                            # epics = type: epic, or issues with
                                            # children: total, counts per status,
                                            # done_ratio (done ÷ total − dropped),
                                            # blocked, owners + a `work` block:
                                            # stage, size-weighted progress, health
cadence issue epic show CAD-38 [--json]     # the epic's row + its children:
                                            # status, owner, priority, tags; stage
                                            # exit criterion and health reasons
cadence issue epic stage CAD-38 verify [--note why]
                                            # CAD-405 stage move via the daemon: one
                                            # commit; one stage forward, any back;
                                            # forward into build/release = operator
cadence issue project approve-work <key>    # CAD-405, operator only: PROJECT.md
                                            # stages/operator_stages take effect
                                            # only while they match this approval
cadence milestone ls|show [m2] [--project p] [--json]
                                            # milestones (PROJECT.md, `milestone:`
                                            # or an m<n>-… tag) with rolled-up
                                            # weighted progress and health
cadence issue ls --at <rev> [--project p] [--json]
                                            # the board as it was at <rev> —
                                            # cards report status_source: file
cadence issue show CAD-16 [--json]
cadence issue acceptance CAD-16 --from acceptance.md
                                            # replace or insert the unique
                                            # level-two Acceptance checklist;
                                            # input is nonempty - [ ]/- [x]
                                            # lines and unrelated body stays
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
    [--by <who>] [--take-over <reason>]     # refused on a doing/review issue
                                            # someone else holds (see Claims)
cadence issue claim CAD-16 [--by <who>]     # record/refresh a claim the dispatch
    [--note <text>] [--take-over <reason>]  # check sees; backlog|ready → doing
cadence issue release CAD-16 [--by <who>]   # the holder gives the claim up
    [--note <text>]
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

Acceptance authoring is deliberately separate from dispatch. The command
accepts a nonempty checklist file with explicit checked or unchecked state and
replaces the issue's unique level-two Acceptance section, or inserts that
section when it is absent. It refuses duplicate headings, malformed or empty
input, missing issues and missing files before writing. issue show --json
returns ordered acceptance items with text, checked and done fields; the
existing global checks counter remains for compatibility. `dispatch` reads the
same section-scoped items: an issue with none is warned about, not refused
(CAD-159 — see Dispatch and finish).

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
`owner` when empty (`--owner`, else the resolved actor), records the
requester as the `claim` when the issue has none, and prints
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
- The worktree gets a `.env` with `CARGO_BUILD_JOBS` and
  `CADENCE_BUILD_SLOT` for the slot service (docs/PROTOCOL.md). To
  keep `issue finish`'s clean-tree guard green, `issue start` appends
  the root-anchored patterns `/.env` and `/.env.tmp` to the repo's
  `info/exclude` — under flock, idempotent — which for a linked
  worktree is the COMMON git dir: the entries are permanent and cover
  every worktree's and the main checkout's root `.env`. Deleting them
  is manual (edit `<common-git-dir>/info/exclude`); `issue finish`
  deliberately does not, because other worktrees still use theirs.
- One tracker commit records a `branch` ref (label = repo basename)
  and a `worktree` ref (absolute path), the status/owner updates and
  the CAD-42 `Issue:`/`Actor:` trailers under subject
  `<ID>: start <branch>`.
- One issue, one lane (CAD-274): when the issue already has exactly
  one open worktree ref, `issue start` reuses that lane — with or
  without `--name`, and even after the title (hence the default slug)
  changed. It re-applies the cargo target config, re-attaches the
  branch if the dir was removed, and returns `created: false`; no
  worktree, branch or ref is minted and no commit lands unless the
  refs needed a fix (a stale recorded `cargo_target`, a missing half
  of the pair) — then one `<ID>: start <branch> (refs refreshed)`
  commit. A `--name` for a different slug is refused, naming the open
  lane's slug and path; so are two or more open worktree refs (listed
  — close the stale ones with `issue finish <ID> --worktree <path>`).
  A new `--name` needs every earlier lane finished first. Without an
  open ref, a start under the same names as a finished lane re-opens
  its closed refs rather than duplicating them. It refuses — naming
  both — when the branch exists but points somewhere unrelated to a
  recorded worktree.
- `--job --pm <alias> --spec <file> [--assignee <alias>]` opens the
  M3 job through `job_new`: the job carries `--issue`, `--repo` and
  `--base-ref <base sha>`, and the `task_worktree`/`task_branch`/
  `task_base_sha`/`task_assignee` params scope the default `<job>-t1`
  task — exactly one task, already bound to the worktree — and
  `task_acceptance` carries the issue's acceptance items (the same
  listing `dispatch` inlines, below), so the task's kickoff lists
  them. The daemon,
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

**Acceptance (CAD-159, ADR-0002 phase 1).** Dispatch reads the issue's
`## Acceptance` items through the same section-scoped readback as
`issue show --json` (a bare `- [ ]` stub is not an item). With at
least one item the kickoff lists them on its single line —
` Acceptance: 1) [ ] "first"; 2) [x] "second".` appended to the plain
template, or the `<job>-t1` task's acceptance on the `--job` path,
which the daemon's kickoff inlines; both paths use the same listing.
Each item is numbered and its text is a JSON string literal (CAD-300),
so text such as `a; [x] b` stays one unchecked item —
`1) [ ] "a; [x] b"` — and cannot read as an extra or checked one. The
quoting escapes `"` and `\`, writes tab as `\t` and every other control
character and U+2028/U+2029 as `\uXXXX`, so the kickoff stays one
control-free line and the text is recoverable exactly
(`dispatch::parse_acceptance_listing` reads it back). Criteria are
never truncated, dropped or replaced by a pointer (CAD-160): when the
plain kickoff would pass the 4000-char pty ceiling, the title (or
`--summary`) is shortened and ends in `…`; when the items cannot fit
even so, the dispatch refuses before anything is created, naming the
ceiling and the note. On the `--job` path the task stores the whole
listing, and dispatch builds the kickoff `job dispatch` will send (same
spec path, scope, criteria and report contract, at the assignee's own
ceiling — 48000 bytes for a Devin cloud session) before `issue start`.
Its scope is the lane `issue start` will bind, resolved by the same
function start uses — the issue's open worktree when it has one
(CAD-274), else one named from `--name` or the title — so a lane
started earlier under another `--name` is measured as it is (CAD-388
R2-1). A list that cannot fit refuses naming the ceiling and the spec
file, leaving no worktree, branch or job (see JOBS.md). With **no** items, both paths **warn and
still dispatch** (PM decision 2026-09-23, ADR-0002 §8.3 — refusal is a
follow-up once live issues are backfilled): the warning names the
issue and `cadence issue acceptance <ID> --from <file>`, prints on
stderr as `warning: …`, and the JSON carries it as
`acceptance: {items, criteria, warning}` (`warning` is `null` when
items exist). It is recorded exactly once, as an `Acceptance warning:`
line on that dispatch's `Dispatched to …` tracker comment — a
duplicate run records nothing, and its warning says the kickoff was
not re-sent rather than `Dispatched anyway.` — so the PM counts unspecified
dispatches with `grep -l 'Acceptance warning:' <pm>/*/*/comments/*`.

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

### Claims — who may start or dispatch an issue in flight (CAD-383)

Two sessions implementing the same ticket is the failure this prevents:
a PM had set `owner`, but neither `dispatch` nor `issue start` read it.
Now both check the issue's **holders** — `claim.by` (the PM that took
it) and `owner` (the lane doing the work) — against the **requesters**
a command speaks for:

| Command | Requesters |
| --- | --- |
| `dispatch` (plain and `--job`) | the PM (`--reply-to`, default `CADENCE_ALIAS`) and the worker (`--to`) |
| `issue start` | `--by`, else `--pm` (with `--job`), else `CADENCE_ALIAS`, else `operator` — plus the owner it would record (`--owner`/`--assignee`) |
| `issue claim` / `issue release` | `--by`, else `CADENCE_ALIAS`, else `operator` |

- **Shared name → unchanged.** Re-dispatching to the same worker, the
  claiming PM handing the issue to another of its workers, dispatching
  to the current owner, and an idempotent re-start all go through as
  before.
- **No shared name, status `doing`/`review` → refused** before anything
  is created (no worktree, branch, commit, message or job), naming the
  holder and the claim age: `D-1 is doing and held by pm-a (owner w1),
  claimed 2h ago — dispatch by pm-b → w3 refused …`. The error's code is
  `claimed`.
- **`--take-over "<reason>"`** (on `dispatch`, `issue start`, `issue
  claim`) proceeds anyway. The reason is required (one line, ≤500
  bytes). The take-over replaces `claim` (by the requester, reason as
  its note) and `owner` (the new worker, else the requester) and is
  recorded as a `Take-over by <who> from <holder>, claimed <age>:
  <reason>` comment in its own commit — `claim take-over by <who> from
  <holder>` in `issue log` (kind `claim`). It commits before the lane
  is touched, so a later lane failure leaves the take-over standing.
- **`backlog`/`ready` with an owner → warning only** (`warning: …` on
  stderr, `claim.warning` in the JSON), and the owner is kept. An owner
  there is a triage assignment or the project's `default_owner`, not
  work in flight; refusing would block every dispatch of an issue
  created with a default owner. Done/dropped issues are not protected
  either. Unowned, unclaimed issues pass silently in every status.

The start that puts an unclaimed issue into work records the claim —
`dispatch` records the PM, `issue start` the requester — in the same
commit as the refs, so the dispatching PM stays a holder after the
worker becomes `owner`. Claim age is `now − claim.at`; an issue owned
before claims existed is dated by the tracker commit that last changed
its `owner:` line (a bounded `git log -G '^owner:'`, "age unknown" past
the bound).

**A PM whose lanes run outside cadence** (Claude Code subagents, Codex,
a human) leaves no dispatch message ref — the claim is the only signal,
so record it before the lane starts:

```bash
cadence issue claim CAD-383 --by pm-opus --note "claude subagent, branch fix/cad383-owner-check"
```

One commit: the claim, `backlog|ready → doing`, and a `Claimed by …`
comment. `owner` stays the lane: a claim leaves it alone, so the worker
a later `dispatch` names becomes owner (a take-over displaces the old
lane and makes the claimant owner until it dispatches one). Re-claiming your own claim refreshes `at` —
a heartbeat for long work. A claim on someone else's doing/review
issue refuses unless `--take-over`; so does the owner lane claiming an
issue its PM holds. `cadence issue release CAD-383 [--note …]` clears
the claim (and `owner` when it is the releaser); status is left alone,
and only a holder may release. `issue set owner=…` and the board's
PATCH are manual edits and are not checked.

Claims show up where people look: `issue show`/`issue ls --json` and
the board cards carry `claim: {by, at, note, age_secs}`; `cadence status`
lists each agent's owned or claimed doing/review issues with their age
(`CAD-383(2h)`) and a `claims:` footer line for holders with no agent
row; `cadence overview` (and the board's project summary) lists every
in-flight claim under its project. `cadence job dispatch <task>` is not
checked — it re-sends a task whose job was minted through the checked
`issue start --job`/`dispatch --job` path.

`issue finish <ID>` is the other end — safe cleanup of the recorded
worktree+branch pair. An issue with several open worktree refs needs
`--worktree <path>` to name one (a bare finish refuses, listing them);
when that directory is already gone, `--worktree` only marks its
worktree/branch refs `closed: true` in one tracker commit — no git
state is touched and a surviving branch is kept (`refs_only: true`,
`branch_note` says so). It refuses, naming what it found, while:

- the issue's `owner` agent has a queued/running message or a pty pane
  that probes busy (the daemon must be reachable — it refuses rather
  than guesses; an `inbox` owner is a durable mailbox, never busy),
- the worktree has uncommitted changes — ignored paths like the
  `ui/node_modules` build symlink don't count (the refusal lists what
  does),
- any non-ignored file (tracked or untracked) in the worktree was
  modified within the last 30 minutes (`ACTIVE_WINDOW` in
  `src/issue/finish.rs`) — a worker cadence cannot see, such as a
  subagent with no registered pane, still leaves fresh files; the
  refusal names the newest file and its age. A fresh checkout counts:
  a lane started minutes ago is in use (CAD-275),
- the branch has not started — zero commits beyond the commit it was
  cut from (the oldest entry of its reflog) — which is never "merged",
  even though its tip sits on the default branch; `--force` finishes
  an abandoned lane and reports `not_started: true` (CAD-275), or
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
`author`, authenticated `author_proof`, `source`, `created`,
`verified_at`, optional `stale`, review cycle/receipts and optional
`supersedes`)
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
cadence memory review <slug> --operation accept --verdict pass \
    --digest <sha256> --evidence 'source and applicability checks'
cadence memory accept pipe-drain [--project demo]   # PM finalization only
cadence memory reject <slug>                       # PM only
cadence memory supersede <old> <new>               # currently refused
cadence memory verify <slug>                       # fresh review cycle + PM
cadence memory ls [--project k] [--status s] [--type t] [--component c]
                  [--path f] [--stale [--days 30]] [--json]
cadence memory show <slug> [--json]
cadence memory match --issue <ID> [--provider p] [--json]
cadence memory lint [--project k]
```

Every authority-bearing proposal/review/finalization is a daemon RPC.
The daemon derives exactly one live agent endpoint from the Unix socket
peer through its one caller-identity verifier (CAD-381): a pty pane by
current adapter ownership, endpoint generation and process start, or a
managed (headless claude/codex) endpoint by its daemon-minted CAD-230
enrollment (provider pid + start time + uid + owner generation), plus the
registration incarnation. A caller outside every agent tree has no agent
identity and is refused — which is not operator proof: operator authority
needs its own positive proof (CAD-276, CAD-313). Two endpoints on one
ancestry are ambiguous and refused. Request aliases, `CADENCE_ALIAS`,
operator UI labels and external identities cannot create proof.
Two distinct non-author PM/worker endpoint identities, displayed with
unique aliases, must pass the same semantic SHA-256 revision (claim/body,
trusted author/contributors, type, source, confidence, scope and supersede
target); an authenticated PM endpoint then finalizes. Body edits, stale
digests, duplicate aliases, missing evidence, disagreement and reused
endpoint identities fail closed.
Legacy accepted records remain visible with a review-blocked reason and
are excluded from matching until a corrected native proposal is reviewed.
`verify` opens a fresh receipt cycle; a timestamp alone cannot revalidate.
Worker receipts do not make a record retrievable: acceptance and each
verify cycle need a durable PM finalization receipt bound to the same
semantic digest. A finalized cycle is consumed; a later verify starts the
next cycle and cannot reuse its receipts.
Supersede is refused until crash-atomic pair recovery exists. Every write
is one tracker commit carrying `Memory: <slug>` and `Actor:` trailers —
never `Issue:`. `issue lint`
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
`decision`, then confidence, then most recently verified (unverified
and decayed last), each with its evidence label, followed by withheld
entries and their reasons (`--json`: `matched[].evidence`,
`withheld[]`).

Evidence freshness (CAD-203) is separate from review status. Accepting
a lesson records the review; it does not re-check the evidence, so only
a PM-finalized `verify` cycle stamps `verified_at` and clears `stale`.
Retrieval reads the verify finalization receipt itself, and every
injected lesson carries an evidence label:

- `verified <date>` — last finalized verify is inside the project's
  freshness window: `memory: {stale_days: N}` in `project.yaml`,
  default 30 days;
- `unverified (last verified <date>)` — the verify has aged past the
  window. Age alone never withholds: the lesson decays and is still
  injected;
- `unverified` — never verified. Older records whose `verified_at` was
  stamped at accept time read the same way; the files are not rewritten.

A lesson is **withheld** only when its evidence is explicitly stale —
`stale: <why>` in its frontmatter (set by a curator today, by CAD-111's
citation re-check later; the next finalized verify clears it). The
withholding is recorded with its reason so "why did I not get this?" is
answerable: in the lessons file (`## Withheld` section), the dispatch
comment (`Lessons withheld: <slug> (<reason>)`), the dispatch JSON
(`lessons_withheld`), `memory match` and the `issue context` memory
manifest. Contradiction is not a withholding reason here.

Injection happens in two places:

- `dispatch` renders the matched accepted set to
  `<state>/dispatch/<message-id>-lessons.md` (≤ 12 entries, ≤ 4 KiB),
  appends `Lessons: <file>.` to the kickoff body, records the slugs in
  the dispatch comment (`Lessons injected: …`) and returns `lessons`,
  `lessons_withheld` and `lessons_file` in the JSON. `--no-lessons` skips it; `--job`
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
  project's accepted `rule`s not marked stale (each with its evidence
  label) for the worker's cwd — ≤ 8 entries and ≤ 4 KiB, same bound
  as the dispatch lessons file. An over-budget rule is skipped, not a
  stop; omitted rules are counted.

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
`verify` is the refresh. Retrieval labels by its own window as above.

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

Writers are attributed per request. `Tailscale-User-Login` resolves
the actor to `<login> (tailscale)` — the tracker commit's `Actor:`
trailer names the person who wrote — only for a request **proven** to
come through the `tailscale serve` HTTPS proxy (CAD-336). The HTTPS
proxy is the one path that replaces client-sent `Tailscale-User-*`
headers with the login tailscaled itself resolved; Host and a loopback
peer prove nothing, since any local process can send both. The proof
(`src/tailnet_proof.rs`) runs these checks in order and fails closed
on the first that cannot be read or does not hold:

| Check | Holds when |
|---|---|
| `loopback` | the TCP peer is a loopback address |
| `tailscaled_socket` | tailscaled's LocalAPI socket is at `/run/tailscale/tailscaled.sock` (or `/var/run/…`) and is a socket, not a symlink — its owner is tailscaled's uid |
| `localapi` | the LocalAPI answers `status`, `prefs` and `serve-config` (read-only, over that socket; cached 2 s) |
| `kernel_networking` | `status.TUN` is true. Under userspace networking tailscaled itself dials `127.0.0.1:<port>` for any tailnet peer the ACL lets reach the port — tagged nodes too — so its sockets carry that peer's bytes |
| `not_operator_user` | the board's uid is not tailscaled's `OperatorUser` (`prefs`, the name resolved to a uid; a name that resolves to no user refuses) |
| `operator_latched` | the board's uid was never the operator during this board process's life: read at board startup and at every later read. One sighting, or a failed startup read, refuses tailnet identity until the board restarts |
| `no_tcp_forwarder` | no `TCPForward` handler (`tailscale serve --tcp=N tcp://…`) anywhere in the serve config — background, foreground sessions, services — targets the board's port. A raw forwarder passes the client's headers through untouched |
| `client_socket` | the connection's client socket is listed in `/proc/net/tcp{,6}` |
| `socket_owner` | that socket was created by tailscaled's uid — the table's `uid` column, readable for another user's socket |
| `foreign_uid` | tailscaled's uid is not the board's; otherwise any same-uid process could pose as it |

A request that fails is attributed to its own peer process —
`operator (ui)`, or the agent it is tied to (see
[Write identity](#write-identity)) — and its identity headers are never
read. A request that is proven but carries **no** `Tailscale-User-Login`
(a Funnel client from the internet, a tagged node) names nobody: its
writes are refused (`403`, `check: "caller_identity"`), never written
as `operator (ui)`. `GET /api/meta` reports `{read_only, actor,
tailnet_proof, tailnet_url, version, build_commit, build_time,
daemon}`: `actor` is the identity this request would write as,
`tailnet_proof` is `null` for a request that is not tailnet-shaped,
else `{"proven": true, "login": true|false}` or `{"proven": false,
"check", "why"}` naming the check that refused. `ui tailscale status`
sends a local forged login and prints the check that ignored it (or
`FORGEABLE` if it resolved).

**The boundary.** Tailnet logins are trusted only when the board's
user is **not** tailscaled's operator user. That user may reconfigure
`tailscale serve` without root, so it — and every process running as
it, agents included — can make tailscaled dial the board at any time:
add a TCP forwarder, open a connection through it, remove the
forwarder, and send forged headers later. No read of the serve config
can see a connection that is already open, so `no_tcp_forwarder` only
catches a standing or accidental forwarder; `not_operator_user` and
`operator_latched` shut the deliberate case out. The latch matters
because the operator can clear itself (`tailscale set --operator=`
needs no root) after opening such a connection: a board that ever saw
its user as operator never trusts a tailnet login again. **After
clearing the operator, restart the board** (`cadence ui stop` then
`cadence ui start`, or `ui tailscale start`, which restarts it) — that
also drops every connection the old process held.

**What is proven**, when every check holds: the connection was opened
by tailscaled (its uid, not the board's), which runs in kernel
networking mode, whose operator user is not the board's user, and whose
serve config — as read at most 2 s earlier — forwards no raw TCP to the
board, and the board's user was not the operator at any point in this
board process's life. Then a process at the board's uid can neither
open such a connection nor make tailscaled open one — **unless it can
become root** (next list).

**What is not proven:**

- **root — including a board user that can gain root.** Where the
  board's user has passwordless sudo (`NOPASSWD`, or membership in a
  group sudo lets run without a password), any same-user process — an
  agent included — can run `sudo tailscale serve --tcp …` and make
  tailscaled connect as root. On such a host tailnet logins are
  **never** trustworthy against same-user processes, whatever the
  checks say. **This host is such a host:** the board user `ubuntu` is
  in the `sudo` group and `sudo -n true` succeeds. Trusting tailnet
  logins needs a board user without passwordless sudo;
- **tailscaled's operator user, if it is another account.** It can
  still make tailscaled dial the board as above; set no operator, or
  one you trust as much as root;
- **Funnel and tagged nodes.** They reach the board through the real
  HTTPS proxy without a login: proven to come through serve, but naming
  nobody — so their writes are refused. Funnel on the board's port
  needs root or the operator user; cadence never enables it;
- **this node itself.** A local process can open the tailnet URL like
  any tailnet client; the proxy then names this node's owner.

The proof also fails closed on a tailscaled whose LocalAPI socket is
elsewhere, that runs as the board's own uid, or whose LocalAPI refuses
the board's user: tailnet logins are then never recorded.

**Keeping tailnet attribution.** A host where `ui tailscale start` ran
without sudo has made the board's user tailscaled's operator
(`tailscale set --operator=$USER`), and every tailnet request is then
refused at `not_operator_user`: no login is recorded, and a tailnet
write falls to the peer-process rule — `403` wherever the daemon's
agent store exists, because tailscaled's socket belongs to another
user (reads still work). To keep tailnet attribution, clear the operator
with `sudo tailscale set --operator=` and **restart the board**
(`operator_latched` refuses until then), on a board user without
passwordless sudo (above). Serve edits then need root:
`sudo tailscale serve --bg --https=<port> http://127.0.0.1:<ui port>`
before `ui tailscale start` (which reuses an identical mapping), and
`sudo tailscale serve --https=<port> off` in place of the removal
`ui tailscale stop` would do.

Threat model in four lines:

1. **Tailnet-only** — `tailscale serve`, never `funnel`; nothing is
   exposed outside your tailnet.
2. **Loopback bind** — the board binds `127.0.0.1`: reachable from the
   tailnet only through tailscaled, and from any local process
   directly.
3. **Host + Origin allowlists** — the tailnet name is the only new
   allowed Host; `https://<dns>:<port>` the only new write Origin.
   Everything else is `421`/`403` exactly as before.
4. **Header trust rule** — `Tailscale-User-*` identity headers count
   only from a peer the checks above prove to be the serve HTTPS
   proxy; from anything else they are ignored.

`--read-only` on `tailscale start` is the browse-only share: every
write route answers `403` with `check: "read_only"` and the SPA hides
quick-add, drag, edit, link/ref, attach and comment controls.

### API — reads

| Route | Returns |
|---|---|
| `GET /api/health` | `ok`, `pm_dir`, `pm_present`, counts, `daemon`, `embedded` |
| `GET /api/meta` | `read_only`, `actor` (the request's resolved write identity), `tailnet_proof` (`null`, or whether the tailnet proxy was proven and which check refused), `tailnet_url`, `operator` (CAD-432, only with `?operator=1` — the proof walks `/proc`, so the SPA asks once per page load; `null` otherwise: this client passes the operator decisions' checks — a writable board, a caller tied to no agent, CAD-276's positive proof on the peer, and the same proof on the board process itself, whose daemon connection relays the decision; the UI offers operator decisions such as stage moves only then), the serving binary's `version`/`build_commit`/`build_time`, plus the daemon's `daemon_info` when reachable |
| `GET /api/setup[?fresh=1]` | the `/setup` wizard's checks (CAD-327): `cadence setup`'s list run **detect only** — no `apply`, nothing created, started or written; the `ui` check is answered by the serving board. `{checks: [{check, status, detail, fix, group}], master: {providers: [{bin, ready, start, warning}]}, checked_at, detect_only: true}`, `group` one of `environment`/`provider`/`master`. `master.providers` (CAD-448) is the master step's provider choice — every CLI `master start` accepts, each ready one with its exact `master start --provider <bin>` command once the `master` check's own prerequisites are met (the offer then absorbs the check's bare `master start` fix: one command for the action); a host that cannot confine adds `--unconfined` and carries the `UNCONFINED_WARNING` risk text in `warning`, and `master_login` states the same risk. Provider probes follow setup's rules (a version token and an exit code or a file's presence — never their output), each bounded at 5 s. One run is reused for 60 s and concurrent requests share it; `fresh=1` (or `true`) re-runs unless the last run is under 5 s old, and each answer says `ran_now`, `age_ms` and `recheck_in_ms`. **Operator on the host only:** `403` before any probe runs on a read-only board (`check: "read_only"`), for a request through the tailnet (`"tailnet"`, proven or not) and for a non-loopback peer (`"loopback"`) — the payload shows HOME's layout, installed CLIs and their sign-in state, and the daemon's pid and socket. Write methods answer `405` |
| `GET /api/projects` | folders, prefixes, components, declared tags, repos, issue counts |
| `GET /api/issues?project=` | card views: derived status, readiness, `tags`, counts, `rev`, the CAD-405 `work` block (type, milestone, size/weight; stage, progress and health on epics). The `issue ls` filters, combinable: `tag=` (repeat or comma-join — all of), `status=` (repeat or comma-join — any of), `epic=<ID>`, `owner=`, `component=`, `priority=`, `open=1`; `400` on a value that could never match (unknown status/priority, bad tag or id grammar) |
| `GET /api/epics?project=` | epics (`type: epic` or issues with children) — the `issue epic ls --json` payload: `total`, `counts` per status, `done_ratio`, `blocked`, `owners`, `children`, and the CAD-405 `work` block (stage, weighted progress, health — docs/design/WORK-MODEL.md) |
| `GET /api/milestones?project=` | CAD-432: the `cadence milestone ls --json` rows — per (project, milestone): `title`/`exit` from `PROJECT.md`, `configured`, size-weighted `progress` over its work items, `health` (the worst of its epics', at risk too on a blocked loose item) with `reasons`, its `epics` and loose `issues`; `400` on a bad key |
| `GET /api/issues/:id` | the drawer payload: frontmatter, body, links both ways, refs, files, comments, notes chain, merged activity |
| `GET /api/issues/:id/file` | raw `issue.md`, `text/markdown` |
| `GET /api/issues/:id/activity` | the merged activity stream only |
| `GET /api/issues/:id/history?limit=N` | parsed git history for the issue — same entries as `issue log` (default limit 50, `400` on a bad limit); an epic's stage move is a `stage` entry with `from`, `to` and `note`, a plan decision a `plan` entry (CAD-432) |
| `GET /api/issues/:id/artifacts/:name` | one artifact file — inline for a small safe list (`text/plain` for md/txt/logs/code, images), **`Content-Disposition: attachment` for everything else, always for html/svg/xml/js/pdf**. Every artifact response carries `Content-Security-Policy: sandbox; default-src 'none'` and `Cache-Control: no-store`; names must satisfy the write grammar (no `/`, no leading dot); symlinks → `404` |
| `GET /api/agents` | worker agent rows enriched with the exact task/issue binding (`agent.tasks` × `jobs.issue_id`) + running/queued/fenced/parked totals and `by_issue` for card strips; `endpoint_kind: inbox` mailboxes are counted separately under `inboxes`; `daemon:"unreachable"` when the socket is down |
| `GET /api/agents/:alias` | the agent drawer: `agent_show` + last 20 events + `tasks`/`on` bindings + `recovery`/`resume` commands; `404` on unknown alias, `400` on alias grammar |
| `GET /api/memories?project=&status=&type=&component=&path=` | memory cards across projects — same filters as `memory ls`; `memory_errors` lists files that failed to load |
| `GET /api/memories/:project/:slug` | one memory: frontmatter + body; `404` on unknown slug, `400` on grammar |
| `GET /api/overview` | the Overview screen payload: `needs_me`, `drift`, `projects`, `github`, `daemon`, `generated_at` — same shape as `cadence overview --json` |
| `GET /api/stream` | server-sent events — `event: issues` on tracker change, `event: jobs`/`event: agents`/`event: monitoring` on daemon state change; each frame's data names the board resources it invalidates (`{"resources":["agents","issue","overview"]}`); `: ping` immediately and every 15 s of silence; `405` on HEAD; deltas only (baselines at connect) |
| `GET /api/threads/<alias>?after=&limit=` | one page of the agent's durable chat (CAD-319) over the daemon's `thread_read`: `{thread, entries, cursor}`; `?tail=1` (the newest page) or `?before=<seq>` read backwards and add `more_before` (CAD-328; `400` combined with `after`); `404` unknown alias, `501` a daemon without threads. Unscoped in the MVP: any caller that reaches the board reads any agent's thread, as with `/api/agents/<alias>`. A live turn token quoted in thread prose is only redacted at `export` |
| `GET /api/threads/<alias>/stream` | server-sent events — one `event: entry` frame per thread entry with `id: <seq>`; resumes after `Last-Event-ID` (else `?after=`); `: ping` on each 15 s idle poll; `event: error` while the daemon is unreachable; `405` on HEAD |
| `GET /api/master/summary?since=<epoch secs>` | "since you left" (CAD-328) over the daemon's `master_summary` (CAD-339), never posted to the thread: plans proposed and decided, tickets moved, reports, open questions, `routing_backlog`; `400` without a numeric `since`, `501 unsupported_daemon` on a daemon without the master, `503` daemon down |

### API — writes

Every issue write goes through `issue::write` — the same functions the
CLI runs — so the API is a second front door, not a second writer. Memory
authority is separate: browser HTTP has no native socket/PTY identity and
is refused after the normal write guards. Each successful issue call is
exactly one git commit whose subject carries the actor:
`CAD-16: set status=review (operator (ui))`.

| Route | Body | Returns |
|---|---|---|
| `POST /api/issues` | `{project, title, priority?, owner?, component?, tags?, parent?, blocked_by?}` | `201` |
| `PATCH /api/issues/:id` | `{status?, priority?, owner?, component?, tags?, title?, body?, if_rev?}` — `""` clears owner/component; `tags` replaces the list (`[]` clears) under the CLI's validation; `body` replaces the markdown only | `200` |
| `POST /api/issues/:id/links` | `{type: blocked_by\|relates\|parent\|duplicate_of, target, if_rev?}` | `200` |
| `DELETE /api/issues/:id/links` | same shape | `200` |
| `POST /api/issues/:id/refs` | `{kind, url\|path, label?, if_rev?}` — exactly one of url/path | `200` |
| `POST /api/issues/:id/comments` | `{body, if_rev?}` — author is the derived caller (`operator`, or a pane's alias), kind `ui`, markdown stored verbatim | `200` |
| `POST /api/issues/:id/artifacts?name=<base>` | raw bytes, create-only | `200` |
| `POST /api/memories/:project/:slug/accept` | `{body?}` — guarded route shape, then refused because HTTP cannot prove a native agent endpoint; body edits are never accepted | `400` with an actionable refusal |
| `POST /api/memories/:project/:slug/reject` | `{}` — refused for the same missing native endpoint proof | `400` with an actionable refusal |
| `POST /api/plans/:epic/approve` | `{}` — the operator approves a proposed plan (CAD-328) over the daemon's operator-only `plan_approve` (CAD-360): the plan's backlog tickets move to ready in one commit. Read-only, the write guards and caller attribution run first; a caller attributed to an agent gets `403 operator_only`. No identity field is read (`deny_unknown_fields`); and the board runs CAD-276's positive operator proof on its own TCP peer (`crate::peer::tcp_peer_operator_proof`: no pane or managed provider on its ancestry, not a descendant of the daemon process, no agent environment, session leader on its ancestry — a `tailscale serve` request proven by CAD-336 passes as its login), refusing `403 operator_proof` before anything is written; the daemon then checks the board's own connection again | `200` with the decision; `409` already decided; `404` unknown epic; `501` a daemon without plans |
| `POST /api/plans/:epic/reject` | `{reason}` — same path and refusals; a missing or blank reason is `400 reason_required` before the daemon is asked | same |
| `POST /api/delivery/:id/merge` | `{}` — the operator's merge decision on a ticket the worker loop PASSed (CAD-431): the same write path, `403 operator_only` for an agent and `403 operator_proof` without positive operator proof, before anything runs. The board process then runs `cadence delivery merge`'s steps with the operator's own `gh` (the daemon never runs it): re-read the PR, check the PASS stands on that head with green CI, `gh pr merge --auto --squash --match-head-commit <reviewed sha>`, record `enqueued` | `200` with the loop record; `400` not ready (no PASS, head moved, CI not green) |
| `POST /api/delivery/:id/decline` | `{reason}` — same path and refusals; relays the daemon's operator-only `delivery_decline`; a blank reason is `400 reason_required` | `200` with the loop record |
| `POST /api/epics/:epic/stage` | `{stage, note?}` — CAD-432: move an epic's stage over the daemon's `epic_stage` (CAD-405); the board never writes the tracker for it. The relay runs on the board's own connection, which the daemon attributes to the operator, so **every** board move needs the operator, not only moves into an `operator_stages` stage: read-only, the write guards, `403 operator_only` for an agent-attributed caller and `403 operator_proof` for a peer without the positive proof — the plan routes' path — before anything is written; no identity field is read (`deny_unknown_fields`). The relay sets `operator_decision: true`, and the daemon then requires the operator on the board's connection for any target, so a board an agent started is refused too (`403 operator_proof`) instead of landing the move as that agent's. Agents move stages with `cadence issue epic stage` over their own connection. The daemon's rules still apply: one step forward, any step back to the floor | `200` `{epic, from, to, forward, needs_operator, by, at, …}`; `400` a skip or unknown stage; `409` already there; `404` unknown epic |
| `POST /api/issues/:id/answers` | `{question, text}` — the operator's answer (CAD-328) to the open question report `question` on `:id`: an `answer` task report (CAD-341) authored `operator`, never from the request or the board's environment; `403 operator_only` for an agent caller and `403 operator_proof` for a peer that is not provably the operator (same proof as the plan routes); `400` when `question` is not a question report on the ticket or `text` is blank. Then a best-effort `reports_changed` so the master's report router (CAD-339) picks it up at once | `201` `{issue, card, warnings}` |
| `POST /api/threads/:alias/messages` | `{text, message?}` — the operator's chat message (CAD-319): starts the thread on first use and queues `text` to the agent like `cadence send`. Refused `403 caller_agent` when the caller is attributed to an agent (pane or managed endpoint tool process); the daemon refuses agent connections again. Tied to no agent is the operator by default, not positive proof (CAD-313) | `200` with the send receipt and `thread` |

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

### Write identity

The cross-site guards below stop browsers, not local processes: any
process with a shell can send `X-Cadence-Board: 1` and a board Origin.
So every write — issue routes, monitor acks and model defaults —
derives its caller from the same shared module as the daemon's
pane-attention verbs (`agent answer`; `src/peer.rs`, CAD-254,
CAD-263), never from anything the request says:

1. The TCP peer's process: the connection's client-side socket in
   `/proc/net/tcp{,6}` gives an inode; every process holding
   `socket:[inode]` in `/proc/<pid>/fd` is a peer.
2. Each peer is matched against the daemon's live registered panes
   (`agent_list`: `pty` endpoints with a pid and generation). A peer
   is that pane's agent when either **process signal** holds:
   - the pane pid is on the peer's `/proc` ancestry — the only
     unforgeable signal: a process can leave a pane's ancestry but
     never join another's;
   - one of the peer's stdio fds (0–2) is the pane's pts — a `setsid`
     or double-forked child of a pane keeps its stdio, so it stays
     attributed after the detach. This tie is **caller-choosable**:
     any same-user process can open another pane's `/dev/pts/N` onto
     its stdio (see the residual below).

3. Each peer is also matched against the daemon's live **managed
   endpoints** (`agent_list`: a `claude` or `codex` `managed` /
   `managed-ws` endpoint with a pid — the provider process the daemon
   launched, CAD-335). A peer is that endpoint's agent when the
   provider pid is on its `/proc` ancestry: a headless `claude -p`
   running its Bash tool, or a codex app-server's shell, that curls the
   board writes as that agent. Ancestry is the only managed signal —
   the provider's stdio is pipes and log files the daemon holds, so
   there is no pty to tie. The pid is the row's live pid: the daemon
   clears it when the endpoint closes, stops or errors.

   Both kinds of row count only while the start time the daemon
   recorded with the pid (`pid_start`, CAD-385) matches the live
   process: a row whose pid was reused ties nothing, and a tie to a row
   with no recorded start (an older daemon's list) refuses the write,
   naming the remedy.

   The pane's `CADENCE_ALIAS` in the peer's environment is **not** a
   board signal on its own: any process can export it, so
   `CADENCE_ALIAS=B curl …` from a pane-less process writes as
   `operator (ui)`, never as agent B. (`agent answer` does consult it,
   only to *refuse* a caller tied to the target pane and to label its
   audit stamp — there it can narrow, never authorize.)

| Peer | Writes as |
|---|---|
| tied to exactly one registered pane, or descends from exactly one live managed endpoint's provider | that agent's alias — commit subject, `Actor:` trailer, comment author, ack `by`; never `operator`. A model-defaults write is refused (`403`, `check: "operator_only"`): the board relays it over its own daemon connection, which the daemon's operator gate sees instead of the caller (CAD-337) |
| walks cleanly and is tied to no agent (the operator's browser, an ssh tunnel, the loopback gateway, the tailnet `socat` relay) | `operator (ui)` |
| proven `tailscale serve` proxy (see Remote access) | `<login> (tailscale)` |
| cannot be attributed — socket owned by another user's process, ancestry unreadable, tied to several agents, or a store exists but the daemon cannot list its agents | refused: `403`, `check: "caller_identity"`, naming why |

**"Tied to no agent" is not proof of the operator.** It is the absence
of every signal above, and today it still defaults to `operator (ui)`
because the operator's own peer (a browser behind the tailnet `socat`
relay or an ssh tunnel) carries no signal either. Phase 1 of CAD-335
(this rule) only stops a managed agent's *ordinary* processes from
landing there. Phase 2 (CAD-335 items 1 and 3, with ADR 0004 /
CAD-313's operator session) replaces the default with positive proof
and refuses every unattributed write.

The board and `agent answer` also differ in how they place a caller
tied to no pane: `answer` requires positive terminal evidence (a pts
that is no pane's) to stamp `operator`, because it gates a pane's own
menu; the board writes as `operator (ui)`, because the operator's own
peer is a browser or tunnel that holds no terminal at all.

No `cadence.sqlite3` in the state dir means no agent was ever
registered, so there is provably no pane or managed endpoint and writes
are the operator's.
A different host (non-loopback peer with no local client socket) is
never a pane here. Reads are unchanged.

The limit: identity follows the process that holds the TCP connection
and the process signals it carries. A same-user relay a pane can reach
but that carries neither still writes as the relay — exactly as it
would to the daemon's socket:

- **the loopback gateway** (nginx or any long-lived proxy the operator
  started): its worker holds the connection, descends from no pane,
  carries no pane's `CADENCE_ALIAS` and holds no pane pty — `operator
  (ui)`, or refused when it runs as another user;
- **an ssh tunnel** (`ssh -L` from a pane back to this host): the
  board's peer is the host's `sshd` session process, whose ancestry
  is `sshd` and which holds no pane pty — `operator (ui)`;
- **a detach with redirected stdio** (`setsid -f … </dev/null
  >/dev/null 2>&1`): no ancestry and no pane pty — `operator (ui)`,
  whatever `CADENCE_ALIAS` it still carries.
- **a detached or orphaned managed-endpoint process** (phase-2
  remainder, CAD-335): a managed provider's `setsid -f` or
  double-forked descendant leaves the provider's ancestry, and a
  managed endpoint has no pty to fall back on, so it is `operator
  (ui)`. So is a tool process orphaned when its provider died — once
  the provider is gone, nothing proves the orphan was ever its. A
  managed endpoint whose pid is not recorded (the launch has not
  finished) matches nothing either; a recycled provider pid could
  attribute an unrelated process *to* that agent (a narrowing, never
  operator).

- **a borrowed pane pty** (accepted residual, CAD-276): a same-user
  process on no pane's ancestry that opens a registered pane's
  `/dev/pts/N` onto its stdio is attributed as *that pane's* agent —
  lateral authorship forgery between agents, with no privilege beyond
  what the same process already had as `operator (ui)`. The pty tie is
  kept because dropping it would send a `setsid` child of a pane back
  to `operator (ui)`, an escalation. `tests/board.rs` pins the
  residual so a fix flips it deliberately.

Same-user is not a hostile isolation boundary; this closes the direct
path (a pane's `curl` approving its own work as `operator`) and the
casual detach, not every deliberate relay. The root weakness — a local
caller tied to no agent defaults to `operator (ui)` — is CAD-335 phase 2
(operator by positive proof, as `slot_reconcile` already requires, and
ADR 0004's operator session for the browser).

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
write the tracker; a process tied to a pane writes as that agent
(see [Write identity](#write-identity)). That is the threat model: a private repo on a
single-operator host, loopback plus the guards above. Auth is deferred
to I3+.

## Overview — needs-me + deploy drift

The landing screen answers two questions at a glance: *what is waiting
on a human or the PM right now, and with which command*, and *is what
we merged actually running*. `GET /api/overview` derives the whole
payload at read time — nothing is stored; `cadence overview [--json]
[--watch <secs>]` renders the same data as an aligned terminal list.

**Needs me** — one row per subject, ranked by urgency then age, each with
`{kind, title, age, project, link, command, audience, audience_reason}`.
*Class* is where a kind starts; *Owner* is who must act on it:

| Rank | Kind | Class | Owner | Command |
|---|---|---|---|---|
| 10 | `merge` — open PR with `qa-verdict=success` and green checks | team | the issue owner (`cadence/<id>-…` head branch) | `gh pr merge <n> --repo <slug> --squash --admin --match-head-commit <sha>` |
| 20 | `approval` — a brokered permission request is open | operator | — | `cadence agent respond <a> --request <h> --decision accept` (provider input requests: `--answers-file <f>`) |
| 20 | `approval_menu` — a sampled pty approval menu | team | the agent's PM (`params.upstream`) | `cadence agent answer <a> <choice>` |
| 30 | `fenced` — agent in `attention` | operator | — (only the operator may unfence or reconcile, CAD-374; the PM escalates) | `cadence agent unfence <a>` |
| 40 | `stalled` — turn silent past the fence threshold | team | the agent's PM | `cadence agent show <a>` |
| 40 | `silent_end` — turn ended at an idle pane, never reported | team | the agent's PM | `cadence send <a> --nudge --text "finish and report …"` (a nudge owns no turn, so it pastes past the unreported one; `cadence agent attach <a>` is the manual alternative — CAD-250) |
| 40 | `awaiting_report` — a delivered pty turn still owes its report while other turns queue behind it (CAD-250) | team | the agent's PM | `cadence agent show <a>` |
| 50 | `drift` — merged commits not running while every pane is idle | dependency | — | `cadence daemon restart --when-idle --ui` |
| 60 | `pr_no_verdict` — open PR with no `qa-verdict` status | team | the issue owner | `gh pr view <n> --repo <slug>` |
| 70 | `review_no_pr` — issue in `review` with no open `pr` ref and no `cadence/<id>-…` PR branch | team | the issue owner | `cadence issue show <id>` |
| 80 | `blocked_ready` — every `blocked_by` target is `done` | team | the issue owner | `cadence issue set <id> status=ready` |
| 85 | `intake` — an untriaged `cadence report` | team | the issue owner | `cadence report show <id>` |
| 90 | `ci_red` — the newest default-branch SHA with a `ci.yml` verdict failed (`failure`, `timed_out`, `startup_failure`); pending and cancelled SHAs neither raise nor clear it | team | none | `gh run view <run> --repo <slug>` |
| 92 | `ci_unverified` — a default-branch SHA whose `ci.yml` push run was cancelled (or never ran) and no later SHA's own run has passed; clears once one does, while the SHA keeps its label | team | none | `gh run rerun <run> --repo <slug>` (cancelled), else `gh run list --repo <slug> --workflow ci.yml --branch <branch>` |
| 95 | `inbox_stale` — a mailbox past its unread threshold with no recent read | team | the inbox owner (group root, else `operator`) | `cadence inbox <a>` |
| 100 | `inbox_unread` — unread messages on an `inbox` endpoint | info | — | `cadence inbox <a>` |
| 110 | `tracker_behind` — tracker repo behind `@{upstream}` | info | — | `cadence issue sync` |

**Audience** (CAD-253) — `cadence overview` resolves who each row is
for and both the CLI and the board only render it: "Needs your
decision" is `audience: operator`, "Team handling" is `team`, then
`dependency` and `info`. A team-class row escalates to `operator` when
no live owner can act on it — `audience_reason` says why:

| `audience_reason` | When |
|---|---|
| `no owner` | the row names no owner (a root agent, an unowned issue, CI) |
| `owner is the operator` | the owner is `operator` |
| `owner <a> is absent` | no registered agent has that alias |
| `owner <a> is dead` / `is fenced` / `is stopped` | `agent_list` says so (`dead`, state `attention`, state `stopped`) |
| `owner <a> has no inbox consumer` | the owner is a mailbox with a stale inbox |
| `owner <a> unknown — daemon unreachable` | liveness cannot be read |
| `unhandled <n>m` | the owner is live but the row's condition has stood past 60 minutes (`ESCALATE_AFTER_SECS`), counted from `since` |

A team row with a live owner inside the hour reads `owner <a> can act`;
operator-class rows read `operator decision`; `dependency` and `info`
rows never escalate and carry no reason. A merged row takes its most
urgent cause's audience (each entry in `causes` keeps its own
`audience` and `since`). "Needs your decision" always shows, reading
"Nothing needs your decision" when no row is the operator's.

The unhandled clock is `since` — epoch seconds when the row's
*condition* began, never its subject's age (an issue created a month
ago that entered review ten minutes ago has been unhandled ten
minutes):

| Kind | `since` |
|---|---|
| `review_no_pr`, `intake` | when the issue entered its effective status: a file status is the tracker's last commit changing the `status:` line (one bounded `git log -G` per issue, cached per build, 3 s budget per build — past it, rows get no clock and one `degraded` note); a notes status is the deriving note's time; a rollup or job status has none |
| `blocked_ready` | the newest of its blockers' status clocks (when the last one reached done) |
| `fenced` | the earliest `completed` of the agent's `unknown` messages; a fence without one (a disconnect while idle, a restart mismatch) uses the row's `updated` — every write that enters `attention` stamps it, so it is a lower bound on time fenced |
| `stalled` / `silent_end` | `now − silent_secs` / `now − ended_secs` from the daemon's stall view |
| `awaiting_report` | `now − awaiting_report.since_secs` — delivery, or the latest ack |
| `inbox_stale` | `now − oldest_unread_age_secs` |
| `merge` | the latest check completion or status post on the head's rollup (merge-ready since) |
| `pr_no_verdict` | the earliest check start on the head's rollup — a head with no checks has none |
| `ci_red`, `ci_unverified` | the classified SHA's run `created_at` |
| `approval_menu`, and anything else | none |

A row whose `since` is null escalates only when its owner cannot act,
and its reason never says `unhandled`. `age` is unchanged — it still
orders rows inside a rank.

Both CI rows share one subject, `ci:<slug>@<branch>`, so a red and
unverified main is one row with two causes. They come from the
branch's `ci.yml` push runs (`actions/workflows/ci.yml/runs`), never
the legacy commit-status API — Actions writes check runs, so that API
reads `pending, total_count: 0` on a red main. Each of the last 20
first-parent SHAs (`git log --first-parent` in the declared clone,
local refs only) is `passed`, `failed`, `pending`, `cancelled` or
`missing`, judged only by its own run: another workflow (Handover), a
later SHA or an absent run never makes a SHA `passed`. A cancelled or
missing SHA shows `covered by <sha>` once a later SHA's own run passes.
The payload carries the classification as `main_ci[]`; a repo without a
`ci.yml` workflow has no block.

GitHub data (open PRs, default-branch CI runs) comes from `gh` behind a
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
opens an `EventSource` on `/api/stream` and each frame refetches only the
resources its data names (`ui/src/lib/cache.ts`: one store per resource,
requests coalesced, a failed refresh keeps the last good payload as
stale) — EventSource reconnects on its own and the 30 s/focus poll stays
as the fallback. Issue counts in the sidebar, board header and overview
summary all derive from `ui/src/lib/counts.ts` over the cards, with epics,
done and dropped named as exclusions. A project's Agents view binds an
agent by dispatch (a job task on one of the project's issues) or by
ownership (owner of a `doing`/`review` issue) — the `cadence status`
ISSUES rule (`ui/src/lib/scope.ts`). The Agents screen ranks
fenced agents first, shows the daemon's recovery text verbatim, and
opens a drawer with identity, params, capabilities, tasks, bound
issues, running messages, and the event tail. A fence banner on the
board links straight to it. The Memory tab lists every project's
memories with status/type/component/path filters, opens a detail
panel. Memory curation is read-only in the browser: proposed entries show
their native quorum/finalization state, while accept, reject, and supersede
require an authenticated native agent endpoint.

The SPA has real routes (CAD-326, `ui/src/lib/router.ts`): `/` Home,
`/overview` (the team overview: needs, drift, monitors, projects),
`/projects[/:slug[/epics|/milestones|/context]]`, `/agents[/:alias]`, `/setup` and
`/settings[/memory]`; the nav shows Home, Projects, Agents and Settings,
and Home's rail links to the overview.

Home is chat-first (CAD-328, `ui/src/features/home/`): the master's
thread (`/api/threads/master`) rendered by kind — the operator's
message, `assistant_text` as commentary, runs of `tool_call` /
`tool_result` as one collapsed line, `turn_result` as the answer — and
streamed live with `streamInto`: entries merge by `seq`, so a reconnect
(`?after=<last seq>`) neither drops nor repeats one. It opens on the
newest page (`?tail=1`, 200 entries) and pages back with "load earlier"
(`?before=`); at most 300 items render at once, and the composer keeps
its draft to itself, so typing never re-renders the thread. The composer posts
with a client message id, shows the message at once and retires it when
the stored `operator` entry with that id arrives; it is disabled, with
the reason, on a read-only board or while the master is not running.
A plan the thread mentions (or a `plan` row in Needs you) renders as a
card — goal, tickets with size and acceptance, weighted progress, and
Approve / Reject-with-reason through the plan endpoints above. The
Needs-you rail is the overview's operator rows (`plan` and `question`
rows from CAD-339 first), each with owner, age and one action; a
question is answered in place through `/answers`. A return after an
hour (last-seen per browser in localStorage) shows a "since you left"
card from `/api/master/summary`; a daemon without it shows "not
available". Fields the master adds are read through adapters
(`needs.ts`, `sinceLeft.ts`, `plan.ts`), so a missing one degrades a row
instead of breaking the screen. The server answers any client route with `index.html` and a
missing file (under `/assets/` or with an extension) with 404. Links from
before the router (`/?tab=board&project=cadence`) redirect to their
route. Colours are CSS variables with a light and a dark theme — system
preference unless the header toggle stored a pick.

`/setup` (CAD-327, MVP) walks environment → agent CLIs (version and
sign-in) → master agent → first project (`cadence project new`, CAD-358)
→ Go to Home, from `GET /api/setup`. It only shows: each check that needs
work carries its copy-paste command, and "re-check" runs the probes
again; nothing is applied from the browser. The master step (CAD-448)
offers every provider `master start` accepts that is installed and
signed in — Claude today — with its exact `master start --provider
<bin>` command, and reports the master's own Claude login (its separate
`CLAUDE_CONFIG_DIR`, CAD-439) with the login command while it has none.
Platforms and import are listed as later. Home shows a link to `/setup`
while a required check — state dir, tracker, daemon, master, the
master's own login, and a master-capable CLI signed in — is not ready
and has a fix to run: a check without one (`master` in a build without
`master start`) never holds the link open. Dismissing it is remembered
per browser (`localStorage`, and for the page when storage is blocked).
A read-only board shows neither the link nor the steps — "setup runs on
the host" — and a tailnet viewer's refused request reads the same.

A filter bar above the columns slices the board by tag, epic, owner and
component — chips with counts, multi-select: tags narrow (all of them),
the other three widen within themselves (any of them). The selection
lives in the URL (`/projects/cadence?tag=ui,api&epic=CAD-38&owner=ann&
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
