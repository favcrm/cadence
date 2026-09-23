# Cadence plan, roadmap, research and prototype — v4

Status: direction accepted by the operator on 2026-09-23 ("go"); tracked as CAD-309..333 and AOS-49. Implementation has started (CAD-310, PR #179). Designs here are still proposals until their ADRs/tickets land.

- `index.html` — six tabs: Plan (consolidated: org, learning loop, roles,
  rules, milestones, decisions), Roadmap (generated from the tracker by
  milestone tag), Research (state of the art, checked 2026-09-23), Direction
  (market and go-to-market), Details (technical proposal) and Prototype. Synthetic data, no live actions. Light/dark toggle,
  390px viewport toggle, daemon-offline and sandbox-banner simulations.
- Preview: `https://preview.simplebuild.site/20260923-091413/cadence-onboarding/`
  (signed link, 24h). Re-mint with
  `~/.agents/skills/preview-html/scripts/mint-url.sh /20260923-091413/cadence-onboarding/`.

## v4 changes

- Target org: one master assistant → one PM per team → dev, QA, DevOps (researcher
  on demand) from a company role catalog (description + preferred provider/model/effort).
- Learning loop: typed reports (six-field reflection + context feedback) → logic
  checks → cross-vendor curator → git company vault → context packs verified at SHA →
  feedback, consolidation and A/B measurement.
- Prototype: Teams & roles screen (Org, Roles editor, Agents, Reports) and Company
  vault screen (company docs, lessons, verification queue, context packs).
- Breakdown filed: CAD-338–CAD-369 and CAD-378 under the existing epics CAD-75
  (team hierarchy), CAD-65 (knowledge layer) and CAD-329/330/331; milestone tags
  m0-safe … m6-autonomy and gtm on 86 issues; direction comments on CAD-77, CAD-67,
  CAD-116, CAD-260.

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
| Team org (under CAD-75) | — | CAD-338 role catalog ADR · CAD-339 master · CAD-340 independent review · CAD-341 reports · CAD-342 questions · CAD-343 Teams UI · CAD-344 many teams · CAD-345 doc reconciliation · CAD-378 write leases |
| Learning + vault (under CAD-65) | — | CAD-346 vault · CAD-347 memory policy ADR · CAD-348 verification runner · CAD-349 curator · CAD-350 extraction · CAD-351 context packs · CAD-352 consolidation · CAD-353 skills · CAD-354 metrics · CAD-355 notes into git · CAD-356 manifests/code maps · CAD-357 Vault UI |
| Plans / delegation / platforms children | CAD-329/330/331 | CAD-358–360 · CAD-361–364 · CAD-365–369 |

Milestones are tags: `cadence issue ls --tag m2-one-team` (m0-safe, m1-first-chat,
m2-one-team, m3-team-learns, m4-many-teams, m5-ships, m6-autonomy, gtm).

Research verified against primary sources on 2026-09-23 (sources in the Direction tab footer).
