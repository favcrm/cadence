# Work model: milestones, epics, tasks, stages and progress

Date: 2026-09-23. Status: **proposed** design record. Operator direction: learn from
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
stage: build          # epics only: from the project's stage list
milestone: m2         # optional; replaces the m2-* tag convention
parent: CAD-75        # the epic
size: M               # optional: S | M | L  (weights 1 | 3 | 8)
owner: dev#1
---
```

`kind` stays for intake reports (question, feedback, idea, bug).

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

## Progress (computed)

- **Epic progress** = Σ size-weights of done children ÷ Σ weights of non-dropped
  children (unsized = M). Shown with counts: open · doing · review · blocked.
- **Health**: `on track`; `at risk` when a child is blocked, the epic has been in one
  stage longer than the project's limit (default 5 days), or review is waiting on a
  missing reviewer; `stalled` when nothing moved for the limit × 2. Each at-risk
  reason names its owner and next action (one decision card per cause).
- **Milestone progress** = roll-up of its epics and loose tasks the same way, plus
  its exit test's last result.

## `PROJECT.md` additions

```markdown
---
project: cadence
stages: [shape, build, verify, release, done]     # optional override
stage_limit_days: 5
milestones:
  - {id: m0, title: Safe foundation, exit: "backup → restore round-trip; production untouched"}
  - {id: m2, title: One governed project, exit: "3-issue plan lands with pinned verdicts"}
---
```

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

## Views

- **Board** — tasks by status (today's board).
- **Epics** — one row per epic: stage, progress bar, counts, health, owner.
- **Milestone** — epics and tasks grouped under each milestone with its exit test.
- CLI: `cadence issue epic ls` gains stage, weighted progress and health;
  `cadence milestone ls|show`.

## Migration

Additive and backward compatible: `type` defaults to `epic` for issues that have
children and `task` otherwise; `m0-safe`… tags map to `milestone: m0`….

- **Issue frontmatter** is parsed leniently today, so new issue fields are safe for
  old binaries.
- **Project config is strict** (`deny_unknown_fields` in `src/issue/project.rs`):
  adding `stages` or `milestones` would break every older binary. Ship the reader
  first, roll it out, then write the keys — the same expand → migrate pattern as
  CAD-392.
