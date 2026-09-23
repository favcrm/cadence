# Cadence plan, direction and prototype — v3

Status: for review (2026-09-23). Not accepted; nothing here is implemented.

- `index.html` — four tabs: Plan (one page), Direction (market, positioning,
  business model, marketing, launch, metrics, nice-to-have), Details (full
  technical proposal) and Prototype (clickable). Synthetic data, no live actions. Light/dark toggle,
  390px viewport toggle, daemon-offline and sandbox-banner simulations.
- Preview: `https://preview.simplebuild.site/20260923-091413/cadence-onboarding/`
  (signed link, 24h). Re-mint with
  `~/.agents/skills/preview-html/scripts/mint-url.sh /20260923-091413/cadence-onboarding/`.

## v3 changes

- Consolidated one-page Plan tab.
- Direction tab from 2026-09-23 market research: governed, cross-vendor
  review positioning; ICP order; OSS + platform revenue model; launch plan;
  north-star metric; ranked nice-to-haves.

## v2 changes

- Current UI review with live measurements (issues 16.4 s, overview 29 s,
  agents 5.8 s on 357 issues / 78 agents) and a current → proposed map.
- Data store & continuity: threads separate from provider sessions,
  continuity packs, read model, real backup/export/restore, retention.
  Wizard gains a Start step (fresh / restore / connect tracker / import).
- AgenticOS is now one example of a generic connected-platform contract
  (credential exchange, MCP tools with declared effects, effect gate).
  Targets AgenticOS v2 (staging c330262): no tokens yet (AOS-14), no
  approvals (in-thread "Post now" → Cadence effect cards), deploy via the
  user's own Cloudflare account until Container gadgets (AOS-32) land.
  Maps AOS-34's six Cadence gaps onto phases.

## Decisions taken in review

| Question | Decision |
|---|---|
| Audience | Public developers, Linux + macOS |
| Default autonomy | Plan freely, ask to dispatch; human merges |
| Master providers | Claude, Codex, Pi (structured); Devin/Cursor workers only |
| Layout | Chat-first Home |
| Managed services | Generic platform contract; AgenticOS v2 is the reference |
| Continuity | Durable store; users never start empty |

## Mockup → implementation mapping

| Mockup element | Owner / new contract |
|---|---|
| Sidebar, topbar, chips, sheet | `ui/src/components/*`, `styles.css` tokens (add light theme) |
| Setup wizard incl. Start step | new route; `cadence setup --json`; `cadence restore` |
| Chat thread, plan / permission / effect cards | threads store, payload events, `/api/threads/*`, broker, effect gate |
| Since you left, continuity notice | continuity pack + thread history |
| Needs-you rail | `overview.rs` needs-me narrowed to decisions |
| Agents transcript / terminal | thread events; tmux control-mode stream for pty |
| Platforms screen, access matrix | platform proxy scope table |
| AgenticOS connect modal | proposed AOS-14 exchange, styled with AgenticOS v2 tokens |
| Settings → Data & backup | `cadence backup/export/restore`, retention |
| Settings → Sandbox | `cadence sandbox up/reset`, `CADENCE_PROFILE` |

## Tracker

| Phase | Epic | Children |
|---|---|---|
| P0 Foundation | CAD-309 | CAD-310 sandbox · CAD-311 release/install · CAD-312 setup · CAD-313 operator auth ADR · CAD-314 backup/export/restore · CAD-315 macOS · CAD-316 event roll-up · CAD-317 clean-machine CI |
| P1 Chat & continuity | CAD-318 | CAD-319 threads · CAD-320 Claude payloads · CAD-322 Pi adapter · CAD-323 interrupt · CAD-324 continuity packs · CAD-325 read model · CAD-326 UI foundation · CAD-327 wizard · CAD-328 chat Home |
| P2 Plans | CAD-329 | |
| P3 Delegation | CAD-330 | |
| P4 Platforms | CAD-331 | relates AOS-49 (AgenticOS credential exchange) |
| P5 Autonomy dial | CAD-332 | blocked by CAD-225 |
| Go-to-market | CAD-333 | |

Research verified against primary sources on 2026-09-23 (sources in the Direction tab footer).
