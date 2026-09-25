---
title: "Code change: {{title}}"
goal: "{{goal}}"
inputs:
  title:    { ask: "Short name for the change (the epic's title)" }
  goal:     { ask: "What is true when this change lands?" }
  worker:   { ask: "Agent that implements it" }
  reviewer: { ask: "Agent that reviews the diff — never the worker" }
---

The software loop as a plan: one ticket implements, a second —
independent — reviews the result pinned to its head. Render it with
`cadence plan propose --workflow code-change --input title=…
--input goal=… --input worker=… --input reviewer=…`.

## Implement {{title}}
agent: {{worker}}
size: M

{{goal}}

Work in the lane worktree; every commit carries the `Issue:` trailer;
open the PR and report the head SHA.

### Acceptance
- [ ] the change does what the goal says
- [ ] the touched checks pass (`fmt`, `clippy`, the relevant tests)
- [ ] a PR names the head SHA under review

## Review {{title}}
agent: {{reviewer}}
size: S
depends_on: 1

Review the diff pinned to its head SHA. The reviewer is independent of
the worker — a worker never verdicts its own work.

### Acceptance
- [ ] a verdict is recorded against the reviewed head
