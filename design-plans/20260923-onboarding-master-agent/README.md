# Onboarding, master agent and autonomous team — design plan

Date: 2026-09-23. Status: direction accepted by the operator ("go"); implementation
started with CAD-310 (PR #179). Individual designs stay proposals until their ADR
or ticket lands. Everything is written in Markdown; `index.html` is the interactive
preview of the same content plus a clickable prototype (synthetic data).

## Read in this order

1. [PLAN.md](PLAN.md) — the consolidated plan: organization, learning loop, agents
   as Markdown folders, operating rules, milestones, decisions.
2. [ROADMAP.md](ROADMAP.md) — milestones and tickets, generated from the tracker
   (`cadence issue ls --tag m2-one-team`).
3. [RESEARCH.md](RESEARCH.md) — state of the art for agent teams, memory and
   context, checked 2026-09-23.
4. [TECHNICAL.md](TECHNICAL.md) — install, identity, chat, data store, plans,
   platforms, sandbox, UI structure.
5. [DIRECTION.md](DIRECTION.md) — market, positioning, go-to-market.
6. [PROVENANCE.md](PROVENANCE.md) — decision ledger, pending decisions, evidence.

Architecture records:
[agent filesystem](../../docs/design/AGENT-FILESYSTEM.md) ·
[learning loop](../../docs/design/LEARNING-LOOP.md).

## Preview

`preview.simplebuild.site/20260923-091413/cadence-onboarding/` — signed links last
24 h; re-mint with the preview-html skill's `mint-url.sh`. The HTML mirrors these
Markdown files; when they disagree, the Markdown wins.

## Prototype → implementation

| Prototype element | Owner / contract |
|---|---|
| Setup wizard (start, environment, platforms, master, workspace, team, launch) | `cadence setup --json`, `cadence restore` (CAD-312, CAD-314, CAD-327) |
| Chat thread; plan, permission and effect cards | threads + payload events, broker, effect gate (CAD-319, CAD-328, CAD-366) |
| Teams & roles: Org, Roles, Agents, Reports | `vault/agents/<slug>/` files, reports (CAD-338, CAD-341, CAD-343) |
| Company vault: lessons, queue, context packs | vault writer, curator, pack builder (CAD-346, CAD-349, CAD-351, CAD-357) |
| Platforms, access matrix, AgenticOS connect | platform proxy, AOS-49 (CAD-365–CAD-369) |
| Settings → Data & backup, Sandbox | `cadence backup/export/restore`, `cadence sandbox` (CAD-314, CAD-310) |
