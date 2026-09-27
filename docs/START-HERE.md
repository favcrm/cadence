# Start here: Cadence project context

Navigation updated 2026-09-26. This is a navigation guide, not a live
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
| How do contributors deliver and validate work? | [Contribution guide](../CONTRIBUTING.md), [repository instructions](../AGENTS.md) and the [agent protocol](../skills/cadence/SKILL.md). Native agents read their generated briefing; external agents must not claim a native identity. |
| What are the task/message contracts? | [RPC vocabulary](../src/proto.rs), [job handlers](../src/daemon/jobs_rpc.rs), [message handlers](../src/daemon/messages_rpc.rs) and [durable storage](../src/store/mod.rs). Use `cadence job --help` and `cadence message --help` for the installed CLI. |
| Where are projects, issues and memories? | [Board and tracker](BOARD.md), [issue commands](../src/issue/mod.rs), [worktree operations](../src/worktree.rs) and [memory implementation](../src/memory/mod.rs). |
| How is identity and role policy enforced? | [Caller rules](../src/daemon/caller_rule.rs), [daemon identity](../src/daemon/identity.rs), [HTTP admission](../src/ui.rs), [audit](AUDIT.md) and [risk rules](roles/risk-classes.md). Source policy is not proof of a live deployment. |
| How do agents act on external platforms? | [Platform contract schemas](../contracts/), [effect handlers](../src/daemon/effect_rpc.rs) and [platform implementation](../src/platform/). Inspect the relevant adapter and its tests before claiming a supported effect. |
| How do reports reach the team? | [Report intake](../src/issue/report.rs) and [relay implementation](../src/issue/relay.rs). Configuration and an active consumer are separate from enqueue success. |

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

## Capability evidence

Durable message/job storage, issue/worktree tools, scoped memory, report intake,
relay foundations, monitor observations, UI alerts, provider profile display,
build/test slots and structured review evidence exist in source. This list does
not certify every adapter, configuration or deployment.

The [bounded acceptance harness](../tests/cad225_acceptance.sh) intentionally
separates supported cases from missing complete-loop capabilities. Read its
current labels and the associated ticket evidence for the revision you are
assessing. Do not infer full autonomy from a green subset or change the harness
labels to claim completion.

Live assignments, queue ages and quota belong in runtime state. Plans,
acceptance, reviews and release evidence belong in the tracker/artifacts.
Source documentation explains the contract; it must not invent current status.

## Keeping context current

This guide links to files shipped in a clean checkout. Additional session notes,
ADRs and design plans may exist in an operator's local `docs/` or tracker, but
are not clone-complete references. Ask the task owner for the relevant reviewed
artifact when an issue depends on one. Do not treat a local file's existence as
proof that it is current, reviewed or safe to publish.

A behavior change updates its owning contract/spec, affected examples and tests
in the same PR. Update this index when an entry point changes, and the code map
when module boundaries move. Retain ADR history when a decision is superseded.
For a status claim, include its source revision and distinguish designed,
implemented, tested, merged, deployed and operationally verified.

A future generated graph should index these documents and source with revision,
source location and extracted-versus-inferred edges. It is a derived search and
navigation aid, not a replacement for reviewed intent or live status.
