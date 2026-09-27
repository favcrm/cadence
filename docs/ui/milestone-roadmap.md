# Capability outcomes and delivery checkpoints

Epics describe a coherent capability or outcome and the tasks that deliver it.
Milestones describe a checkpoint: what must be true, its owner, its optional
calendar target, and the evidence supporting achievement. A milestone can
include parts of several epics, and an epic can contribute to several milestones.

The board presents Epics by delivery stage and current tasks, and Milestones
as an ordered roadmap with criteria and calendar information first. Detailed
work, contributors, blockers, dependencies, and evidence expand on demand.
Overview shows explicitly active milestones, or the first planned milestone
in declared roadmap order. Inferred issue tags never manufacture an active
checkpoint or a date.

## Authoring

Milestones are stored in each project's `PROJECT.md` YAML frontmatter.
The board and `cadence milestone ls|show` read this configuration; this change
adds no metadata editor or new write route. This is declarative project planning
metadata, not an operator approval record. Epic stage gates remain unchanged.

```yaml
---
milestones:
  - id: beta
    title: Internal beta ready
    description: The team can complete the main journey without assistance.
    owner: pm
    status: active
    start_date: '2026-10-01'
    target_date: '2026-10-15'
    exit: Main journey verified, critical defects resolved, and pilot guide published.
    evidence:
      - docs/qa/beta-verification.md
      - https://example.com/review/beta
  - id: pilot
    title: Pilot launch
    status: planned
    depends_on: [beta]
    target_date: '2026-10-30'
    exit: Pilot users onboarded and support ownership confirmed.
---
```

Fields beyond `id` are optional:

| Field | Meaning |
| --- | --- |
| `title`, `description` | Checkpoint name and outcome/context |
| `owner` | Person or agent accountable for the checkpoint |
| `status` | `planned` (default), `active`, `achieved`, or `cancelled` |
| `start_date`, `target_date` | Optional calendar dates, not timestamps |
| `completed_date` | Actual achievement date; requires `status: achieved` |
| `exit` | Observable completion criteria; retains the existing key |
| `evidence` | Supporting document references or HTTP(S) links |
| `depends_on` | Other declared milestones in this project |

Dates must be real `YYYY-MM-DD` dates; start cannot follow target. The UI formats
calendar dates in UTC, avoiding a previous-day shift in western timezones.
Dependencies must exist, cannot reference themselves, and cannot form cycles.
The API reports calendar schedule separately from work health and checkpoint
status. An achieved or cancelled milestone is not shown as overdue.

## Scope and progress

A task's explicit `milestone` field wins, then its legacy `m<n>-…` tag, then its
parent epic's assignment as a default. Inheritance is limited to a parent epic
in the same project. Each task contributes to one checkpoint. Epics are
contributors, not additional task weight. Milestone task lists and epic progress
within a milestone use the same scoped tasks; another checkpoint's task blocker
or unfinished epic slice does not leak into this checkpoint's health.

Progress is size-weighted task completion, with dropped tasks excluded from the
work total. A checkpoint is achieved only when its status is recorded as such;
100% task progress never changes that status. Empty scope has unknown progress.
Missing owner/dates/criteria stay visibly unset. Old API responses remain
supported without inventing new metadata.

## Design references

- [Atlassian: Epics](https://www.atlassian.com/agile/project-management/epics)
- [Linear: Project milestones](https://linear.app/docs/project-milestones)
- [GitLab: Milestones](https://docs.gitlab.com/user/project/milestones/)
