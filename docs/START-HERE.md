# Start here: Cadence project context

Source baseline: `ceee1cf` (2026-09-21). This is a navigation guide, not a live
status feed. Check the current source revision and running build before acting.
A merged feature is not necessarily installed, enabled or proven unattended.

Cadence is a local Rust controller for a development team: it coordinates
provider agents, durable messages, tasks, project issues, independent review and
operational evidence. The intended outcome is a human goal carried through
verified delivery with routine coordination handled by the system.

## Read in this order

| Question | Reviewed source / starting point |
|---|---|
| What are we building, and what is outside scope? | [Charter](CHARTER.md) — goal, principles, non-goals and roadmap. Its autonomy labels are planning context; verify current capability evidence below. |
| How is it built? | [Architecture and code map](ARCHITECTURE.md) — boundaries, durable stores, adapters and file ownership. |
| What experience are we aiming for? | [Development-team design proposal](design/DEVELOPMENT-TEAM.md) — feedback, memory, project context and staged acceptance. Proposed behavior is labelled. |
| Where is the plan for onboarding, the master agent and the autonomous team? | [Design plan](../design-plans/20260923-onboarding-master-agent/README.md) (Markdown: plan, roadmap, research, technical, direction, decision ledger), plus the proposed records [agent filesystem](design/AGENT-FILESYSTEM.md) and [learning loop](design/LEARNING-LOOP.md). |
| How do roles deliver work? | [Team](TEAM.md), [session workflow](SESSION.md), [role briefings](roles/pm.md). Historical aliases/model assignments are not live registry evidence. |
| What are the task/message contracts? | [Jobs](JOBS.md), [protocol](PROTOCOL.md), [steering ADR](adr/0002-steering-contract.md). |
| Where are projects, issues and memories? | [Board and tracker](BOARD.md), [workspaces](WORKSPACES.md), [worktree-policy ADR](adr/0003-worktree-policy.md). |
| How is identity and role policy designed? | [Role-profiles ADR](adr/0001-role-profiles.md), [audit](AUDIT.md), [risk rules](roles/risk-classes.md). An ADR is not proof every phase shipped. |
| How do reports reach the team? | [Report relay](CAD213-REPORT-RELAY.md) and the report section of [session workflow](SESSION.md). Configuration and an active consumer are separate from enqueue success. |
| What was the original plan? | [Implementation plan](IMPLEMENTATION-PLAN.md), [dogfood retrospective](DOGFOOD.md). Read as dated context; proposed CLI examples may not match the current binary. |

## First task checklist

1. Identify the project/repository, current source SHA, installed CLI/daemon/UI
   build, and whether the agent is actually registered. Do not infer these from
   a previous chat or a similarly named external collaboration agent.
2. Read the issue, acceptance criteria, dependencies, latest evidence and owner.
   Check for an existing branch/PR before starting a second implementation.
3. Read the relevant design/ADR and code-map row. Resolve a spec conflict in the
   task record rather than silently replacing the user's objective.
4. Retrieve accepted, relevant project lessons. Proposed, contradicted and stale
   claims need review; a memory cannot grant permissions. See the current
   [memory contract](BOARD.md#project-memory--cadence-memory).
5. Record the worktree, exact baseline, reviewer/result route and validation
   plan. Preserve unrelated work and use the repository's suite/resource rules.

For PMs, start with goal, dependencies and stage wait times. For developers, read
acceptance and the affected module. For QA, inspect the exact diff and evidence
independently. For DevOps, read release compatibility, live state and recorded
merge/rollout authority. Curators need original evidence and prior lesson
versions, not only the short injected summary.

## Capability boundary at the source baseline

Durable message/job storage, issue/worktree tools, scoped memory, report intake,
relay foundations, monitor observations, UI alerts, provider profile display,
build/test slots and structured review evidence exist in source. This list does
not certify every adapter, configuration or deployment.

The [bounded acceptance harness](../tests/cad225_acceptance.sh) intentionally
separates supported cases from missing complete-loop capabilities. At this
baseline those include unattended scheduling, live quota recovery, actual
policy-bound merge execution, remote-head movement, UI escalation completion,
deployed real-provider proof, and independently curated lesson reuse. The
coordinator follow-up is tracked separately in CAD-176. Do not infer full
autonomy from a green subset or change the harness labels to claim completion.

Live assignments, queue ages and quota belong in runtime state. Plans,
acceptance, reviews and release evidence belong in the tracker/artifacts.
Source documentation explains the contract; it must not invent current status.

## Keeping context current

A behavior change updates its owning contract/spec, affected examples and tests
in the same PR. Update this index when an entry point changes, and the code map
when module boundaries move. Retain ADR history when a decision is superseded.
For a status claim, include its source revision and distinguish designed,
implemented, tested, merged, deployed and operationally verified.

A future generated graph should index these documents and source with revision,
source location and extracted-versus-inferred edges. It is a derived search and
navigation aid, not a replacement for reviewed intent or live status.
