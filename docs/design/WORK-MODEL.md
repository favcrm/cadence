# Work model: milestones, epics, tasks, stages and progress

Date: 2026-09-23. Status: **accepted 2026-09-23 (operator)**; implemented by CAD-405. Operator direction: learn from
this plan's own delivery and support epics with stages and progress, using generic
best practice, pragmatically. Builds on the tracker (`src/issue/`), where an epic is
today only implicit (an issue with children, depth ≤ 2), progress is a done ratio,
and milestones are a tag convention. Ticket: CAD-405 (relates CAD-81 first-class epics, CAD-85 pipeline view).

## Principles

- **One issue file, one writer.** Epics and tasks are the same Markdown issue with a
  `type`; nothing new to store or sync.
- **Status is for tasks, stage is for epics.** Tasks keep today's six statuses; an
  epic moves through a short list of stages with exit criteria.
- **Progress is computed, never typed.** Nobody edits a percentage.
- **Configurable per project, sensible defaults.** Stage lists and milestones live in
  `PROJECT.md`; a project that never configures them gets the defaults.

What experience taught (this plan, 2026-09-23): tag-based milestones worked for
filtering but hid progress; implicit epics made "is this an epic?" unclear; nothing
showed that an epic was stuck in review for days; parallel lanes duplicated work that
a visible owner + stage would have exposed.

## Hierarchy

| Level | Is | Holds | Typical size |
|---|---|---|---|
| **Milestone** | An outcome with an exit test | Epics and tasks (by reference) | weeks |
| **Epic** | A deliverable | Tasks (children, depth 1) | days to weeks |
| **Task** | One unit of work — for agents, one PR | Acceptance checklist | hours to a day |

Milestones group; they are not parents, so an issue belongs to one epic and at most
one milestone. Depth stays two levels (epic → task).

## Fields (issue frontmatter)

```markdown
---
id: CAD-341
type: task            # epic | task | bug | spike     (explicit; replaces "has children")
status: doing         # task workflow: backlog | ready | doing | review | done | dropped
stage: build          # epics only: from the project's stage list; written by a stage move
stage_at: 2026-09-23T10:00:00Z   # set with stage: when the epic entered it
milestone: m2         # optional; replaces the m2-* tag convention
parent: CAD-75        # the epic
size: M               # optional: S | M | L  (weights 1 | 3 | 8)
owner: dev#1
---
```

`kind` stays for intake reports (question, feedback, idea, bug).

Defaults keep every existing issue valid: no `type` means `epic` for an issue with
children or a plan and `task` otherwise; no `milestone` means the first `m<n>` /
`m<n>-…` tag (`m2-one-team` → `m2`); no `size` weighs as M. `type`, `milestone` and
`size` are settable (`cadence issue set CAD-1 type=spike milestone=m2 size=S`; an empty
value clears). `stage` and `stage_at` are not — only a stage move writes them.

## Stages (epics)

Default list, overridable in `PROJECT.md`:

| Stage | Exit criterion (checked in the epic's body) |
|---|---|
| `shape` | Goal, non-goals and acceptance written; tasks listed |
| `build` | All tasks done or dropped |
| `verify` | Independent review of the combined result; exit test of the milestone run where relevant |
| `release` | Merged, deployed or published as the project defines |
| `done` | Retro filed; lessons proposed |

Moving an epic to the next stage is a **decision** through the gate: the PM (or
master) proposes it when the exit criterion is met; routine moves are automatic,
`release` asks the operator unless the project's autonomy allows it. Every move is
a tracker commit, so "time in stage" is exact.

**As implemented (CAD-405).** `cadence issue epic stage <EPIC> <stage> [--note]`
goes through the daemon's `epic_stage` RPC and ends in one tracker commit
(`<EPIC>: stage build → verify — <note>`, `Actor:` trailer = the mover) that writes
`stage` and `stage_at`. Rules:

- **Forward one stage at a time** (each exit criterion is a gate); **back to any
  earlier stage** (sending an epic back grants nothing).
- **Who moves.** A *forward* move into one of the project's `operator_stages` needs
  the proven operator — the same connection-bound check as `plan approve`, so an
  agent pane or managed endpoint is refused. Default `operator_stages: [build,
  release]`: **shape → build is the operator's** (it is plan approval's twin: work
  may start) and **verify → release is the operator's** (it ships). **build →
  verify and release → done** are routine: any attributable caller (a pane agent's
  lane — PM or master — or the operator) moves them. **Moves back** are open to any
  attributable caller; re-entering an operator stage forward asks the operator
  again. A project's autonomy is expressed by its `operator_stages` list (`[]` =
  fully delegated) — but see *gate approval* below.
- **Gate approval.** `stages` and `operator_stages` decide who may move what, and
  `PROJECT.md` is a file any agent can edit, so they take effect **only once the
  operator approves them**: `cadence issue project approve-work <key>` (daemon RPC
  `project_work_approve`, operator connection only) records the sha256 digest of
  the normalized keys (stage ids in order; operator stages as a set) in the daemon
  store's approval stream with who and when — never inferred from a tracker commit,
  whose `git add -A` can sweep an agent's edit into an operator commit. While the
  file's gate keys differ from the defaults and from the approved digest, every
  reader and every stage move uses the **default** stages and operator stages and
  reports `config_unapproved`; `issue lint` warns. So reordering, dropping or
  renaming stages, or emptying `operator_stages`, gets an agent nothing, and a later
  edit to approved keys falls back again. `milestones` and `stage_limit_days` are not
  gates and stay agent-editable. An unreachable daemon reads as "no approvals"
  (the fail-safe side).
- **Plans.** A plan epic's stage follows the plan (table below): while `proposed`
  it is `shape` whatever `stage` says and cannot move (`cadence plan approve` is its
  shape → build); `rejected` is terminal and never moves; once `approved` it moves
  like any epic except that **build is the earliest stage it can reach** — the plan
  owns shape, so to reshape, reject or re-propose the plan. A recorded `stage: shape`
  on an approved plan (a hand edit) reads as build, so plan state and stage agree.
- **Migration.** An epic whose (derived) status is `done` and that has no recorded
  stage reads the last stage (`done`, source `status`), so finished epics do not
  show as shape. A stage read off the status was never entered, so **every move
  out of it needs the operator**, whatever the target — otherwise a pane could step
  from a derived `done` to verify (not an operator stage) and then "back" into build.
  No floor exception is needed: a status-derived stage only exists without a plan
  (an approved plan maps to `plan`), and the `default` source is always the first
  (floor) stage, whose only move is one step forward under the usual rule. Only a
  move back from a recorded (`field`) or plan-mapped stage into an operator stage is
  open to any attributable caller.
- A malformed `PROJECT.md` refuses every move; readers fall back to the defaults.
- The exit criterion of the stage being left is shown with the move (and in
  `issue epic show`), not machine-checked; automatic routine moves are a later cut.

## Progress (computed)

- **Epic progress** = Σ size-weights of done children ÷ Σ weights of non-dropped
  children (unsized = M). Shown with counts: open · doing · review · blocked.
- **Health**: `on track`; `at risk` when a child is blocked, the epic has been in one
  stage longer than the project's limit (default 5 days), or review is waiting on a
  missing reviewer; `stalled` when nothing moved for the limit × 2. Each at-risk
  reason names its owner and next action (one decision card per cause).
- **Milestone progress** = roll-up of its epics and loose tasks the same way, plus
  its exit test's last result.

**As implemented (CAD-405, `src/issue/work.rs`).** Progress reuses the plan's
weights (`plan::progress`: S=1, M=3, L=8, unsized = M, dropped excluded) over an
epic's children, with counts open (backlog + ready) · doing · review · blocked ·
done · dropped. Health is `on_track`, `at_risk` (an open child is blocked, or more
than `stage_limit_days` days in the current stage, measured in seconds: at risk
strictly after the limit) or `stalled` (in the stage for 2 × the limit or more).
**`stalled` is time in stage, not child activity** — an epic whose children keep
moving but whose stage does not is still stalled; the next action is to meet the
exit criterion and move the stage, or record why it waits. Each reason carries
`cause`, `owner`, `detail` and `next` (for a proposed plan: approve or reject the
plan). Time in stage is measured from `stage_at` (or the plan's `proposed_at` /
`decided_at` for a plan-mapped stage); an epic never moved has no entry time and
skips the time check, so existing epics are not flagged en masse. The terminal
stage and a rejected plan are never at risk. A milestone's progress rolls up its
loose non-epic issues plus every child of its epics; its health is the worst of its
epics', and at risk when a loose open issue is blocked. Cut for later: the
"missing reviewer" cause and the exit test's last result.

## `PROJECT.md` additions

```markdown
---
project: cadence
stages: [shape, build, verify, release, done]     # optional override
stage_limit_days: 5
operator_stages: [build, release]                 # optional; forward entry needs the operator
milestones:
  - {id: m0, title: Safe foundation, exit: "backup → restore round-trip; production untouched"}
  - {id: m2, title: One governed project, exit: "3-issue plan lands with pinned verdicts"}
---
```

The file is `<pm>/<key>/PROJECT.md`, next to `project.yaml`, and optional: absent
(or without frontmatter) means the defaults. Its frontmatter is parsed **leniently**
— other keys (`agents`, `autonomy`, …) belong to other readers — but the work keys
are checked: at least two unique stages, `operator_stages` drawn from them,
`stage_limit_days` ≥ 1, unique milestone ids. A stage may be `{id, exit}` to set
its exit criterion; a bare default name keeps the default one. When milestones are
declared, `issue set milestone=` must name one; otherwise any well-formed id works
and `cadence milestone ls` lists every milestone an issue names.

## Plans (CAD-359/360, shipped)

A plan is an epic proposed with its tickets and approved by the operator before any
work starts (`cadence plan propose|approve|reject|show`). It is stored on the epic as
`type: epic` plus `plan: {state, proposed_by, proposed_at, tickets, decided_by,
decided_at, reason}`; each ticket carries `plan_epic: <epic>` and `parent: <epic>`.
Until stages land, `plan.state` maps onto them without a migration:

| `plan.state` | Stage | Meaning |
|---|---|---|
| `proposed` | `shape` (awaiting approval) | Goal and tickets written; nothing may start |
| `approved` | `build` | Tickets moved to `ready`; dispatch allowed |
| `rejected` | terminal | Recorded with a reason; its tickets never start |

The dispatch gate (`issue start`, `dispatch`, job and monitor dispatch) decides
membership by the epic's `plan.tickets` list, cross-checked with each ticket's
`plan_epic`, and fails closed when either side is missing or unreadable. It is a
**process guard, not a security boundary**: `~/pm` is a git repo any local agent can
write, so the gate keeps honest work honest and the board truthful; it does not stop
a determined same-uid process.

**Rollout order.** Old binaries drop unknown frontmatter keys when they rewrite an
issue. The two-sided markers make a one-sided rewrite fail closed (tickets refuse
with `plan_missing`), but the reader must be on every host and session binary
before the first `cadence plan propose`.

## Workflows (CAD-487)

A workflow is a reusable plan file with inputs, kept in the tracker beside
`PROJECT.md` as `<pm>/<project>/workflows/<name>.md`. The format is the plan
format plus an `inputs:` frontmatter map — `name: {ask, optional}` — and
`{{name}}` placeholders anywhere in the file:

```markdown
---
title: "Code change: {{title}}"
goal: "{{goal}}"
inputs:
  title:    { ask: "Short name for the change" }
  goal:     { ask: "What is true when this lands?" }
  worker:   { ask: "Agent that implements it" }
  reviewer: { ask: "Agent that reviews it — never the worker" }
---

## Implement {{title}}
agent: {{worker}}
size: M
### Acceptance
- [ ] the change does what the goal says
```

`cadence plan propose --workflow <name> --input k=v …` renders it — placeholders
take the input values, `inputs:` drops out of the frontmatter — and the result
goes through the unchanged propose → approve → gate path above. A missing or
unknown input, or an unresolved `{{name}}`, is refused with a named reason.
`cadence workflow check <file|name>` verifies a template without proposing:
plan-parse, declared inputs, known agents (PROJECT.md `agents:`, `<pm>/agents/`,
the daemon registry), `depends_on` acyclic, acceptance on every ticket, and no
`reviewer:` equal to the ticket's own `agent:`; it exits non-zero on any
refusal. `workflow add|edit|ls|show` are the only writers — each write is one
tracker commit with `Actor:` recorded — and `show`/`ls` mark each workflow's
approval state.

Like `PROJECT.md`'s work keys, a workflow is gated by a digest of its **gate
keys**: the declared inputs, the ticket count, and each ticket's metadata
(`size`, `agent`, `depends_on`, plus `reviewer`/`tries`/`uses` — keys the plan
parser does not consume yet but a workflow may already carry). The operator's
`workflow approve` records that digest; an edit that changes it — CLI or hand —
unapproves the workflow and `propose --workflow` refuses `workflow_unapproved`
until it is re-approved. Wording-only edits keep the digest. Approval is a
process guard, not a security boundary, same as the plan gate.

## Views

- **Board** — tasks by status (today's board).
- **Epics** — one row per epic: stage, progress bar, counts, health, owner.
- **Milestone** — epics and tasks grouped under each milestone with its exit test.
- CLI: `cadence issue epic ls` gains stage, weighted progress and health;
  `cadence milestone ls|show`.

As implemented, one `work` block carries the model everywhere: `type` and
`type_source`, `milestone` and `milestone_source` (`field` | `tag`), `size`,
`weight`, and for epics `stage` (`id`, `source` = `field` | `plan` | `default`,
`since`, `exit`, `next`, `next_needs_operator`, `terminal`, `stages`, and — CAD-432 —
`moves`: every `{to, forward, needs_operator}` that `check_move` accepts now), `progress`
and `health` (`null` for non-epics), plus `config_error` when `PROJECT.md` is bad.
`config_unapproved` appears when unapproved gate keys were replaced by the defaults.
It is on every card of `GET /api/issues` and `issue ls --json`, on the detail of
`GET /api/issues/:id` and `issue show --json`, and on each row of `GET /api/epics`,
`issue epic ls|show --json`. The existing card fields (`done_ratio` is still the
count ratio) are unchanged. `issue epic ls` lists effective epics (explicit
`type: epic` or children); its table shows STAGE, weighted PROGRESS, the counts and
HEALTH. The board's Projects screen (CAD-432) shows them: a project's Epics tab
lists each epic's stage, weighted progress, health with its reasons and next actions,
and milestone; opening one shows its children, its stage history (who moved each
stage, when — `stage` entries of the issue history) and, to the proven operator only,
the legal moves (`POST /api/epics/:epic/stage`). The Milestones tab shows progress and
worst health per milestone (`GET /api/milestones`).

## Migration

Additive and backward compatible: `type` defaults to `epic` for issues that have
children and `task` otherwise; `m0-safe`… tags map to `milestone: m0`….

- **Issue frontmatter** is parsed leniently today, so new issue fields are safe for
  old binaries.
- **Project config is strict** (`deny_unknown_fields` in `src/issue/project.rs`):
  adding `stages` or `milestones` would break every older binary. Ship the reader
  first, roll it out, then write the keys — the same expand → migrate pattern as
  CAD-392.

**Compatibility as implemented (CAD-405).**

- **Readers ship before writers.** Nothing rewrites existing issues: `type` and
  `milestone` are derived when absent (children → epic; `m2-one-team` → `m2`), stage
  is derived from the plan or is the first stage, and `project.yaml` gains no key.
  Roll the binary out to every host and session before the first `issue set
  type=|milestone=`, `issue epic stage`, or a `PROJECT.md` with work keys.
- **An older binary rewriting an issue drops `type`, `stage`, `stage_at` and
  `milestone`** (and, before CAD-359, `size`). That degrades **fail-safe**: the type
  falls back to the children rule; the milestone falls back to its `m<n>` tag (keep
  the tag until every binary reads `milestone`); the stage falls back to the plan
  mapping or the first stage — an epic can read **earlier than it was, never
  later** — and its entry time becomes unknown, so no false "stalled". Moving it
  forward again needs the operator at `build`/`release` as before, so a dropped key
  can never skip a gate. The move stays in git history (`issue log`, `issue blame`)
  to restore it from.
- **A plan's state bounds its stage**: a rewrite or hand edit that sets `stage` on a
  proposed plan does not advance it, and an approved plan never reads before build —
  with `stage` dropped it reads build, never later than recorded.
- **`type`** other than `epic` is refused on an issue with a plan or children, so an
  epic cannot silently drop out of the epic views.
- **Lint**: an unknown `type` or a malformed `milestone` is an error (like a bad
  status); a stage outside the project's list, an undeclared milestone, a bad
  `stage_at`, a stage on a non-epic, a non-epic type with children and a malformed
  `PROJECT.md` are warnings — `PROJECT.md` can change under existing issues, and a
  warning never blocks tracker commits. Unapproved gate keys are a warning too
  (`config_unapproved` from `issue lint`, which asks the daemon; the commit hook,
  `sync` and `doctor` cannot, so they warn whenever the gate keys differ from the
  defaults).
