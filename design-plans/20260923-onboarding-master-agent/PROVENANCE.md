# Provenance and decision ledger

Date: 2026-09-23. What is accepted, what is pending, and where each piece of this
plan came from. Update this file when a decision changes; do not rewrite history.

## Files

| File | What it is |
|---|---|
| [PLAN.md](PLAN.md) | Consolidated plan (readable source of the Plan tab) |
| [ROADMAP.md](ROADMAP.md) | Milestones and tickets, generated from the tracker |
| [RESEARCH.md](RESEARCH.md) | State of the art, checked 2026-09-23 |
| [DIRECTION.md](DIRECTION.md) | Market, positioning, go-to-market |
| [TECHNICAL.md](TECHNICAL.md) | Install, identity, chat, data, platforms, sandbox, UI |
| [index.html](index.html) | Interactive preview: all tabs plus the clickable prototype |
| [../../docs/design/AGENT-FILESYSTEM.md](../../docs/design/AGENT-FILESYSTEM.md) | Agents, roles and teams as Markdown files |
| [../../docs/design/LEARNING-LOOP.md](../../docs/design/LEARNING-LOOP.md) | Reports → verification → vault → context packs |

Preview host path: `preview.simplebuild.site/20260923-091413/cadence-onboarding/`
(signed links expire after 24 h; re-mint with the preview-html skill's
`mint-url.sh`). Mockup data is synthetic.

## Decisions

| Date | Decision | By | Status | Record |
|---|---|---|---|---|
| 2026-09-23 | Audience: public developers, Linux + macOS | operator | accepted | PLAN.md |
| 2026-09-23 | Default autonomy: plan freely, ask to dispatch; operator merges | operator | accepted | PLAN.md |
| 2026-09-23 | Master providers: Claude, Codex, Pi (structured); Devin/Cursor as workers | operator | accepted | TECHNICAL.md |
| 2026-09-23 | Chat-first home | operator | accepted | TECHNICAL.md |
| 2026-09-23 | AgenticOS is one example of a connected platform; target its v2 | operator | accepted | TECHNICAL.md |
| 2026-09-23 | Users never start empty: durable store, restore, import | operator | accepted | TECHNICAL.md |
| 2026-09-23 | Organization: one master; one PM per team; dev, QA, DevOps | operator | accepted | PLAN.md |
| 2026-09-23 | Go: commit, file tickets, start P0 with the sandbox | operator | accepted | CAD-309..333 |
| 2026-09-23 | #1 Agents as folders `agents/<slug>/` with SOUL.md, AGENT.md, MEMORY.md; all information in Markdown | operator direction | design record proposed | AGENT-FILESYSTEM.md, CAD-338 |
| 2026-09-23 | #2 Memory policy: logic checks + cross-vendor curator (project/role), quorum (company), external quarantine | operator | approved; ADR text pending | LEARNING-LOOP.md, CAD-347 |
| 2026-09-23 | #3 Unblock memory: one daemon identity check for all agents; trust matrix provenance × evidence; clear the 11 stuck proposals | operator | confirmed 2026-09-23 | LEARNING-LOOP.md, CAD-381, CAD-382, CAD-260 |

## Pending operator decisions

- Accept the ADR texts for #1 (CAD-338) and #2 (CAD-347) before code.
- Phone push notifications would reverse the 2026-09-21 "UI is the alert
  destination" ruling — only if wanted.
- Pi on AgenticOS credits as a default for users without a model key.

## Tickets

Filed 2026-09-23: CAD-309–CAD-333, CAD-338–CAD-369, CAD-378, CAD-381, CAD-382
and AOS-49. Milestone tags `m0-safe` … `m6-autonomy`, `gtm`. Direction comments
on CAD-77, CAD-67, CAD-116, CAD-260, CAD-347. See [ROADMAP.md](ROADMAP.md).

## Evidence gathered in this session

- Repository surveys of the UI, daemon/API, persistence, team/memory state and
  AgenticOS v1/v2 (summarised in TECHNICAL.md and RESEARCH.md).
- Live API timings on this host (357 issues, 78 agents).
- Market and memory claims verified against primary sources; corrections applied
  (Copilot's 28-day rule is from the docs; self-preference "can be more than 50%";
  self-generated skills below baseline in SkillsBench v4; Letta memory is
  git-backed files with frontmatter).
