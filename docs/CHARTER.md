# Cadence charter

## Background

Cadence coordinates human and agent work in native and hosted workspaces,
including software development and business projects. It records work,
assignments, evidence and decisions while the owning workspace and connected
systems enforce identity, permissions and external effects. A project
coordinates work that belongs to it; standalone app work need not invent one.

On 2026-09-19 the project merged eleven PRs through cadence itself.
Every incident that day (a disk that filled, a permission menu that sat
for five hours, a rebase that silently reverted merged work, a credential
that leaked into a public PR) was found by a human reading panes. That
is the gap this charter is about.

## Goal

An agentic work system: people state goals and approve decisions where
policy requires them; agents research, design, implement, verify and carry
authorized work through delivery. The system detects, reports and safely
recovers from failures without presenting an uncertain result as success.

## Principles

1. **Evidence over self-report.** Verify the outcome with an appropriate
   check and durable receipt, revision-pinned review or live probe. A worker's
   "done" is a claim.
2. **Separation of duties.** An author cannot provide a required
   independent review or approval. Authorized automation may deliver only
   after the applicable controls pass. A memory proposer cannot accept its
   own lesson.
3. **Fail closed.** When the system cannot tell (pane state, ownership,
   merge status), it refuses and says why rather than guessing.
4. **People decide where required, not by default.** Agents carry out
   authorized work; the applicable policy and delivery gate determine the
   required reviews and approvals. A policy goal never waives a live gate.
5. **Durable, attributable evidence.** Issues, decisions and results
   retain provenance and history in their authoritative native or hosted
   store. A chat statement alone is not proof of an approval or effect.
6. **Detect, then automate.** Every manual fix a human had to make
   becomes an issue that makes the system detect or prevent it.
7. **Resources are finite.** Follow the active host and project admission
   limits; disk, pipes and provider state must remain observable.

## Non-goals

- Replacing provider tools. Cadence drives Devin, Claude, Codex and
  Cursor as they are.
- Unauthorized external effects. Sending, publishing, deployment and
  other outward actions remain subject to the applicable permission and
  approval policy; a workflow cannot grant its own authority.

## Autonomy ladder

| Level | What runs without a human | Status |
|---|---|---|
| L1 | The PM dispatches; a human reviews and merges | reached |
| L2 | A reviewer agent reviews, a DevOps agent prepares merges, a human approves each | reached 2026-09-19 |
| L3 | Routine merges land automatically; the human approves substantial ones | target; verify the active gate |
| L4 | Research and design agents turn goals into specs and tickets; the PM plans from the roadmap without prompting | next |
| L5 | The system tunes itself: retros feed memory, scorecards drive routing, metrics drive the roadmap | later |

## Success metrics

Measured weekly from the tracker, events and verdicts:

| Metric | Direction |
|---|---|
| Lead time: issue ready to merged | down |
| Review rounds per PR | down; above 2 escalates |
| Human minutes per merged PR | down |
| Share of merges that needed the operator | down, but never zero for class `human` |
| Stalls found by a human versus by the system | toward system |
| Incidents (data loss, leak, broken main) | zero |

## Roadmap

Epics in the tracker (`cadence issue epic ls`). Order is the PM's
current priority; the PM re-ranks it as evidence arrives.

1. **Close the detection gaps** (L3 hardening): CAD-102 approval menus,
   CAD-105 silently ended turns, CAD-108 secret redaction, a pre-publish
   secret scan, CAD-106 sweep safety.
2. **Resource hygiene** (CAD-91): session start and end verbs (CAD-92),
   idle auto-stop, timed gc, shared build dirs.
3. **Operating model in the product** (CAD-75): role profiles
   (`team.yaml`, CAD-76), task roles with enforced separation (CAD-78),
   multi-level groups (CAD-77), the DevOps role as a first-class verb
   (`cadence land`, CAD-79).
4. **Research and design agents** (L4): research notes and ADRs as
   tracker artifacts, specs with runnable acceptance checks, tickets
   generated from specs.
5. **Overview and metrics** (CAD-82): cross-project agents and work,
   pipeline view, flow metrics, daily digest.
6. **Knowledge layer** (CAD-65): project memory with an independent
   curator, knowledge graph, LLM wiki.
7. **Self-improvement** (L5): automatic retros per merged issue, agent
   scorecards, lessons proposed from retros.

See the tracked [agent protocol](../skills/cadence/SKILL.md) for delivery and
reporting responsibilities. Live role assignments come from the agent registry
and generated briefings.
