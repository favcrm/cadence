# Direction — product and go-to-market

Date: 2026-09-23. Status: proposal; readable source of the Direction tab in
[index.html](index.html). Market facts were verified against primary sources on
2026-09-23 (see sources at the end); private-company revenue figures are left out.

## Market, September 2026

| | Single vendor | Multi provider |
|---|---|---|
| **Cloud** | Claude Code Projects (coordinator, 2026-09-17), Devin, Jules, Codex cloud, Kiro | GitHub Agent HQ (Copilot + Claude + Codex), Factory Missions, Antigravity, Augment Cosmos; Terragon shut down 2026-02-09 |
| **Local** | Claude agent teams, Codex app + Symphony, Sculptor | Conductor, Superset, Claude Squad, Nimbalyst, Vibe Kanban (community), Paseo, opencode → **Cadence: local · multi · governed** |

- **Vendors bundled the master.** Claude Code Projects: "Claude scopes the
  request, delegates the work, coordinates parallel threads, reviews the outputs"
  — public beta on Pro/Max, cloud today, local "coming very soon".
- **The local quadrant is crowded but shallow:** session runners with worktrees
  and a diff view; none has plans, independent review, approval gates or memory.
- **Free tools die, open local tools survive:** Terragon and bloop (Vibe Kanban's
  company) shut down in 2026; Vibe Kanban lives on "open source and community
  maintained".
- **Trust is the bottleneck:** Stack Overflow 2025 — 3.1% of developers highly
  trust AI output; 46% distrust vs 33% trust.

## Positioning

> Cadence is the open-source control plane that turns the coding agents you
> already pay for into a governed team that learns. One master plans with you,
> PM sessions run each project, workers from any vendor deliver, an independent reviewer
> checks every commit, verified lessons make the next task cheaper, and nothing
> merges, sends or deploys without your approval. It runs on your machine.

| Wedge | Why defensible | Proof |
|---|---|---|
| Cross-vendor review | No vendor ships "our rival grades our output"; fresh-context reviewers paired by measured precision | Defects caught per reviewer/author pair from our verdict store |
| Verified team memory | Most memory systems did not beat memory-off (2026); verified, cited, measured memory did | Reads and tokens saved per task, A/B |
| Evidence and governance | Plans, gates, separation of duties, approvals with an audit trail | `cadence audit` provenance report |
| Local, MIT, Markdown | Headless daemon; the whole company brain is readable Markdown in git | Backup/restore bundle; vault in any editor |

Category: "agent delivery control plane". Avoid leading with "parallel agents",
"autonomous" or "replace developers".

## Who it is for, in order

1. **Multi-subscription power developers** (launch ICP) — already pay for two or
   more of Claude, ChatGPT/Codex, Cursor; pain: babysitting panes, unreviewed code,
   lost context.
2. **Small product teams and agencies (2–15 devs)** — audit trails, shared
   tracker, separation of duties, spend across client repos.
3. **Businesses via AgenticOS** — AgenticOS hosts Cadence as its agent runtime
   (AOS-34); Hong Kong / zh-HK first.

## Product direction

| Horizon | Theme | Scope |
|---|---|---|
| Now | Governed team that learns, on one machine | M0–M3 |
| Next | Ship and see cost | M5 platforms; vendor cloud agents as workers (Codex cloud, Jules, Cursor cloud agents, Devin cloud — CAD-243); cost per plan |
| Later | Many projects and self-improvement | M4, M6; scorecards drive routing; hosted runtime inside AgenticOS |

Strategic move: treat vendor orchestrators as **workers**. When Claude Projects
or Codex cloud run a sub-goal, Cadence still owns the plan, the independent
review, the approval and the memory.

## Business model

| Layer | Offer | Price direction |
|---|---|---|
| Cadence (MIT) | Full local product, single operator, all providers | Free |
| Platform revenue | AgenticOS managed services, gateway credits, hosting | AgenticOS plans (HK$980/mo proposed, AOS-28) + usage |
| Cadence Team (later) | Multiple operators, SSO, audit export, policy packs | Per seat, when ICP 2 asks |

## Strategic risks

| Risk | Response |
|---|---|
| Vendors ship local masters | Neutrality, governance, memory; integrate their orchestrators as workers |
| Provider terms | Anthropic's current legal page allows "an end user … signing in to the unmodified Claude Code binary with their own Claude subscription", and forbids offering Claude.ai login, routing requests through plan credentials on users' behalf, and paying for, reselling or intermediating usage. Cadence drives the unmodified binary, the user signs in, Cadence never touches tokens; AgenticOS-hosted mode uses Pi via gateway/API keys. OpenAI: no primary policy found — treat as permitted-but-unwritten. Legal review before launch (CAD-333). |
| Crowded local quadrant | Don't compete on "parallel"; publish review and learning data nobody else has |
| Rising per-token costs | Cost per plan up front; route cheap work to cheap agents |
| Complexity | Chat-first home, restore/import, "Approve plans" default |

## Marketing

- **Message pillars:** independent review of every commit · you approve what
  ships · your subscriptions, your machine · it learns — verified, not guessed.
- **Proof, not claims:** Cadence builds itself (11 PRs merged through it in one
  day); publish incidents the system caught.
- **Flagship content:** a recurring "who catches whose bugs" report from our
  verdict store; a learning report (reads and tokens saved, harm rate).
- **Demo:** one chat → plan card → three vendors → a fresh-context reviewer
  rejects a PR → fix → merge click → preview deploy → next task starts with the
  lesson.
- **Growth loops:** opt-in "Reviewed by Cadence" PR footer; install by agent
  prompt; skill listings; honest comparison pages; shared playbooks.
- **Channels:** X + GitHub launch day → Product Hunt → Show HN 2–4 days later
  with a technical angle → provider communities → podcasts; AgenticOS SMB channel
  in HK (en + zh-HK).

## Launch plan

1. **L0 design partners** (during M0–M1): 10–20 multi-subscription developers,
   2–3 agencies. Gate: 10 reach an approved plan unaided.
2. **L1 public beta** (after M3): README GIF, docs, demo, first review and learning
   reports, Discord. Gate: provider-terms legal review; clean-machine CI green;
   telemetry off by default.
3. **L2 platforms GA** (after M5): "ship from chat"; joint AgenticOS announcement.

## Metrics

| Metric | Definition | Direction |
|---|---|---|
| North star | Verified merges per active install per week | up |
| Activation | Install → first approved plan within 24 h | > 40% |
| Time to first plan | `curl | sh` → plan card | < 10 min |
| Trust | Plans approved without edits; review rounds per PR | up; ≤ 2 |
| Learning | Reads avoided per task; lesson reuse, helpful and harm rates | up; harm → 0 |
| Human minutes per merged PR | From the charter | down |

## Nice to have (after the MVP)

| Idea | Value | Note |
|---|---|---|
| Vendor cloud agents as workers | high | Strategic hedge |
| Cost and quota per plan | high | Builds on CAD-114/80 |
| Shareable run report | high | Growth loop |
| ⌘K palette, "ask master about this" | medium | From any issue, PR or agent |
| GitHub App | medium | Label an issue → plan card |
| Scheduled goals | medium | Recurring plans wait for approval |
| Local models via Pi | medium | Private/offline work |
| Voice to master (en + zh-HK) | medium | Reuses AgenticOS speech research |
| Phone approvals (PWA push) | medium | Reverses the 09-21 "UI is the alert destination" ruling — needs a decision |
| Multi-machine view; team mode | medium | After the auth ADR |

## Sources (retrieved 2026-09-23)

claude.com/blog/projects-redesigned · code.claude.com/docs/en/claude-projects ·
code.claude.com/docs/en/legal-and-compliance · support.claude.com article
15036540 · theregister.com 2026-02-20 · learn.chatgpt.com/docs/app-server ·
github.blog changelog 2026-02-04 and usage-based billing (2026-04-27) ·
factory.com/news/5-billion-valuation · cursor.com/blog/joining-spacex ·
vibekanban.com/blog/shutdown · Terragon shutdown notice (mirror) ·
survey.stackoverflow.co/2025/ai.
