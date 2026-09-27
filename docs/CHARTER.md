# Cadence charter

## Background

Cadence is a local controller that coordinates coding agents (Devin,
Claude, Codex, Cursor) through durable message queues, native terminals
and a git-native issue tracker. It started as plumbing: deliver a message
to a terminal agent and know whether it was done. Running real work
through it showed that plumbing is the easy half. The hard half is the
operating model around the agents: who decides what to build, who checks
it, who lands it, and how the system notices when something is stuck.

On 2026-09-19 the project merged eleven PRs through cadence itself.
Every incident that day (a disk that filled, a permission menu that sat
for five hours, a rebase that silently reverted merged work, a credential
that leaked into a public PR) was found by a human reading panes. That
is the gap this charter is about.

## Goal

A fully autonomous agentic development system: a human states goals and
approves substantial decisions; agents do the research, design,
implementation, review, merging and operations, and the system itself
detects, reports and recovers from its own failures.

## Principles

1. **Evidence over self-report.** Work is done when an independent
   check says so: a verdict pinned to a commit, a gate run, a live
   probe. A worker's "done" is a claim.
2. **Separation of duties.** The author never reviews or merges its own
   work. The proposer of a memory lesson never accepts it.
3. **Fail closed.** When the system cannot tell (pane state, ownership,
   merge status), it refuses and says why rather than guessing.
4. **Humans approve the substantial, not the routine.** Risk classes
   (`docs/roles/risk-classes.md`) decide which merges and actions need
   the operator. Everything else runs.
5. **Everything durable is in git.** Issues, notes, verdicts, role
   briefings and decisions live in files with history, not in a chat.
6. **Detect, then automate.** Every manual fix a human had to make
   becomes an issue that makes the system detect or prevent it.
7. **Resources are finite.** One full test suite per host at a time;
   disk, pipes and provider state are watched (`cadence doctor --host`).

## Non-goals

- A hosted service. Cadence runs on the operator's machine.
- Replacing provider tools. Cadence drives Devin, Claude, Codex and
  Cursor as they are.
- Unattended outward actions: releases, repository settings, anything
  public beyond a reviewed merge stay with the operator.

## Autonomy ladder

| Level | What runs without a human | Status |
|---|---|---|
| L1 | The PM dispatches; a human reviews and merges | reached |
| L2 | A reviewer agent reviews, a DevOps agent prepares merges, a human approves each | reached 2026-09-19 |
| L3 | Routine merges land automatically; the human approves substantial ones | **current** |
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
