# Roadmap — milestones and tickets

Date: 2026-09-23. Generated from the tracker (`cadence issue ls --json`, projects cadence and agenticos-v2). Milestones are tags: `cadence issue ls --tag <tag>`. **new** = filed for this plan; **existing** = prior work this plan builds on. Regenerate rather than hand-edit.

Three tracks run in parallel and meet at the public beta: **Shell** M0 → M1 → M5 · **Team** M2 → M4 → M6 · **Learning** M3 · **GTM** after M1, M3, M5.

## M0 · Safe foundation (Shell)

Sandbox, real backups, event roll-up.

- **Exit test:** Backup → restore round-trip in a sandbox; production untouched.
- **Depends on:** —
- **Tag:** `m0-safe` · 0/4 done

| Ticket | Title | Status | Pri | |
|---|---|---|---|---|
| CAD-309 | P0 Foundation: sandbox, install, setup, operator auth, backup/restore | doing | P1 | new |
| CAD-310 | Sandbox profile: cadence sandbox up/env/down/reset/ls with isolated state, tracker, port and gated global side effects | doing | P1 | new |
| CAD-314 | cadence backup/export/restore: SQLite online backup + manifest, bundle without credentials, restore with repo remap | doing | P1 | new |
| CAD-316 | Event retention: roll up delivery-noise events; agent rm keeps thread history | review | P2 | new |

## M1 · First conversation (Shell)

Install → wizard → chat with a master that remembers.

- **Exit test:** Clean box to chat in under 5 minutes; kill the master's session mid-plan and it resumes.
- **Depends on:** M0
- **Tag:** `m1-first-chat` · 0/16 done

| Ticket | Title | Status | Pri | |
|---|---|---|---|---|
| CAD-160 | Compose objective + outstanding criteria into every task-bound message; never truncate criteria (ADR-0002 phase 1) | review | P1 | existing |
| CAD-311 | Release workflow and install.sh: linux x86_64/aarch64 + macOS arm64, UI embedded, checksums, --prefix | backlog | P1 | new |
| CAD-312 | cadence setup: idempotent first run with --json checks {check,status,detail,fix} and single-use login link | backlog | P1 | new |
| CAD-313 | ADR + implementation: operator identity for the web UI (0600 token -> single-use link -> HttpOnly cookie) | doing | P1 | new |
| CAD-315 | macOS port: caller attribution and host doctor without /proc | backlog | P2 | new |
| CAD-317 | Sandbox S3: clean-machine install CI (ubuntu:24.04 + macos-14) with fake providers | backlog | P2 | new |
| CAD-318 | P1 Chat and continuity: threads, payload events, Pi adapter, continuity packs, read model, wizard, chat Home | backlog | P1 | new |
| CAD-319 | Threads store and payload events (assistant_text, tool_call redacted, tool_result, turn_result) | backlog | P1 | new |
| CAD-320 | Claude adapter: persist intermediate assistant text and tool I/O summaries | backlog | P2 | new |
| CAD-322 | Pi headless adapter (pi --mode rpc): streaming, tool progress, model/effort, cancel, resume | backlog | P1 | new |
| CAD-323 | Real interrupt: stop the provider turn and reconcile its effects | backlog | P2 | new |
| CAD-324 | Continuity packs: summary + last N turns + plan state + preferences on new/lost/compacted sessions | backlog | P1 | new |
| CAD-325 | Read model: indexed tracker and cached overview; SSE carries entity diffs | backlog | P1 | new |
| CAD-326 | UI foundation: router, query cache, feature folders, light + dark tokens | backlog | P2 | new |
| CAD-327 | Setup wizard: start (fresh/restore/tracker/import), environment, platforms, master, workspace, team, launch | backlog | P2 | new |
| CAD-328 | Chat-first Home: thread, plan/permission/effect cards, Needs-you rail, since-you-left | backlog | P2 | new |

## M2 · One governed team (Team)

Master → one team (PM, dev, QA, DevOps) from agent files; plans, dispatch gate, fresh-context reviews, typed reports, questions that climb.

- **Exit test:** A 3-issue plan lands with verdicts pinned to each head, a report per job, and one question answered at the lowest level.
- **Depends on:** M1 for the UI; daemon work can start now
- **Tag:** `m2-one-team` · 0/27 done

| Ticket | Title | Status | Pri | |
|---|---|---|---|---|
| CAD-77 | Multi-level groups: upstream chains, results one level up, escalation that bubbles when unanswered | dropped | P2 | existing |
| CAD-78 | Task roles in jobs: planner, implementer, reviewer, merger with enforced separation of duties | backlog | P2 | existing |
| CAD-116 | team.yaml schema + parser + cadence team show with drift report (ADR-0001 phase 2a) | backlog | P2 | existing |
| CAD-120 | Route qa-1's review through job verdict so the existing self-review gate is on the live path (ADR-0001 phase 3a) | backlog | P1 | existing |
| CAD-122 | Role capabilities: closed set, verb-to-capability map, daemon refusal with the rule named (ADR-0001 phase 3b) | backlog | P2 | existing |
| CAD-131 | Plumb effort for codex/cursor/devin: resolve (provider, model, effort) and compose slug suffixes, validating what the providers do not | backlog | P1 | existing |
| CAD-213 | Reliable report delivery: durable GitHub relay and quota-aware PM intake consumer | ready | P1 | existing |
| CAD-223 | Agent profile editor: provider capabilities, desired versus running model and effort | backlog | P1 | existing |
| CAD-224 | Goal intake: turn project outcomes into a dependency-aware executable plan | backlog | P1 | existing |
| CAD-298 | Dispatch refuses an issue with empty acceptance (switch CAD-159's warning to a refusal after backfill) | backlog | P2 | existing |
| CAD-329 | P2 Projects and plans: project new, plan propose, plan card approval, daemon dispatch gate | backlog | P2 | new |
| CAD-330 | P3 Delegation: workers from approved plans, cross-vendor reviewer routing, merges in Needs-you, terminal view | backlog | P2 | new |
| CAD-338 | ADR-0001 v2: company role catalog (vault/company/roles) + per-team team.yaml; one --role vocabulary | backlog | P1 | new |
| CAD-339 | Master role: company assistant that creates teams from the catalog, routes escalations and sends a daily digest; never implements | backlog | P1 | new |
| CAD-340 | Cross-vendor rules: QA provider differs from the author's, curator differs from the proposer's; fallback launch with recorded reason | backlog | P1 | new |
| CAD-341 | Structured reports: done / question / blocked with the six-field reflection and context feedback, stored per task | backlog | P1 | new |
| CAD-342 | Question routing up the tree: worker → PM → master → operator, vault/FAQ lookup first; answers become decisions and FAQ candidates | backlog | P2 | new |
| CAD-343 | Teams UI: Org, Roles editor (description, preferred provider/model/effort), Agents, Reports | backlog | P2 | new |
| CAD-345 | Reconcile team and memory docs with code: QA vs PM acceptance, role vocabularies, stale claims | backlog | P2 | new |
| CAD-358 | cadence project new: register a repo and seed its context manifest and team.yaml | backlog | P2 | new |
| CAD-359 | Plan proposals: plan.yaml schema, plan_proposed event and plan card | backlog | P2 | new |
| CAD-360 | Plan approval commit and daemon dispatch gate: refuse dispatch outside approved plans with a named reason | backlog | P1 | new |
| CAD-361 | Staff and dispatch from approved plans via team.yaml roles; the team PM dispatches | backlog | P2 | new |
| CAD-362 | Reviewer routing: cross-vendor QA assignment, verdict → DevOps merge-queue handoff | backlog | P1 | new |
| CAD-363 | Merges, effects and operator questions in Needs-you: one decision per cause | backlog | P2 | new |
| CAD-364 | Live read-only terminal view for pty agents (tmux control mode) | backlog | P3 | new |
| CAD-378 | One writer per code area: path-scoped write leases in dispatch; parallel work limited to reads (review, research, triage) | backlog | P1 | new |

## M3 · The team learns (Learning)

Verified memory in a Markdown company vault; context packs; learning metrics.

- **Exit test:** A lesson from one worker is verified by a curator of another vendor and reused by a different worker with measured savings; a stale item is withheld; a planted poisoned item is rejected.
- **Depends on:** CAD-381/382 (unblock), M2 reports
- **Tag:** `m3-team-learns` · 0/23 done

| Ticket | Title | Status | Pri | |
|---|---|---|---|---|
| CAD-66 | Knowledge graph per project via graphify: built from repo, tracker and notes; refreshed on merge; shown on the board | backlog | P3 | existing |
| CAD-67 | LLM wiki: agent-maintained project pages with provenance and staleness checks | dropped | P3 | existing |
| CAD-111 | Automatic retro per merged issue: rounds, what review caught, flakes, lead time, proposed lessons | backlog | P2 | existing |
| CAD-192 | Evidence-backed memory capture: structured required source, secret scan on propose | backlog | P1 | existing |
| CAD-193 | Memory lifecycle enforcement: prior-status check, verify must cite evidence, atomic supersede | backlog | P2 | existing |
| CAD-194 | Job dispatch injects no memory, and path scope needs recorded commits so new issues match nothing | backlog | P1 | existing |
| CAD-197 | Auditor agents cannot deliver artifacts: codex-1 attach and notify failed with bwrap RTM_NEWADDR | backlog | P2 | existing |
| CAD-203 | Stale and contradicted accepted lessons stay eligible for injection: match filters on status only | doing | P1 | existing |
| CAD-260 | Unblock memory review: define native-identity proof for proposers, or an operator curation path | backlog | P2 | existing |
| CAD-346 | Company vault: git layout (company, teams, projects, playbooks, lessons, decisions, faq, inbox) with index and lint | backlog | P1 | new |
| CAD-347 | Memory policy v2 (ADR): logic checks + cross-vendor curator for project/role scope, quorum for company scope, external quarantine, validity and expiry, counters | backlog | P1 | new |
| CAD-348 | Verification runner: resolve citations at HEAD, re-run cited tests in a build slot, secret scan, dedupe, contradiction check | backlog | P1 | new |
| CAD-349 | Curator role and queue: ADD / UPDATE / INVALIDATE / NOOP / ESCALATE verdicts with operator escalation | backlog | P1 | new |
| CAD-350 | Candidate extraction from reports and retros: typed claims (fact, strategy, pitfall, procedure, faq) with evidence | backlog | P2 | new |
| CAD-351 | Context pack builder: one path for issue dispatch, job dispatch and continuity; items verified at SHA; planned paths; budgets | backlog | P1 | new |
| CAD-352 | Nightly consolidation as a reviewed vault PR: merge duplicates, invalidate stale, propose skills; never rewrite in place | backlog | P2 | new |
| CAD-353 | Skills from procedures: promote to Agent Skills only after an evaluation shows benefit | backlog | P3 | new |
| CAD-354 | Learning metrics and A/B harness: reuse, helpful/harm, stale rate, reads avoided, tokens and time; planted-memory red-team | backlog | P2 | new |
| CAD-355 | Move agent notes (/var/www/agent-notes) into git so notes and retros are portable | backlog | P2 | new |
| CAD-356 | Project context manifests for every project and commit-keyed code maps as pack sources | backlog | P2 | new |
| CAD-357 | Vault UI: overview, company docs, lessons, verification queue, context-pack preview | backlog | P2 | new |
| CAD-381 | One identity verifier for every daemon-launched endpoint (pty and managed): reuse the CAD-230 enrollment for memory, reviews and reports | backlog | P1 | new |
| CAD-382 | Clear the stuck memory backlog: the 11 proposed lessons become agent-claimed candidates and run checks → curator → operator queue | backlog | P1 | new |

## M4 · Many teams (Team)

Several teams under one master: capacity, quota, isolation, digest.

- **Exit test:** Two teams on different projects run concurrently with no cross-team leakage.
- **Depends on:** M2
- **Tag:** `m4-many-teams` · 0/6 done

| Ticket | Title | Status | Pri | |
|---|---|---|---|---|
| CAD-80 | Budgets per level: tokens, quota and wall-clock per role and per epic, with quota-aware routing | backlog | P3 | existing |
| CAD-82 | Overview dashboard: one screen across all projects for what needs me, who is doing what, what is in flight, health and flow | backlog | P1 | existing |
| CAD-114 | Provider quota visibility: Devin ACU and Codex quota in doctor --host and the Overview | backlog | P1 | existing |
| CAD-212 | Safe agent provider replacement: drain, checkpoint, validate and transfer queued work without duplicate delivery | backlog | P1 | existing |
| CAD-226 | Agent inbox UX: accountable consumers, delivery stages and actionable backlog | backlog | P1 | existing |
| CAD-344 | Multiple teams under one master: cross-team capacity and priorities, isolation, daily digest | backlog | P2 | new |

## M5 · Ships (Shell)

Connected platforms: credential proxy, effect gate, Cloudflare, AgenticOS v2.

- **Exit test:** Preview deploys automatic; production deploys and sends only on your press; agents never see credentials.
- **Depends on:** M1; AOS-49
- **Tag:** `m5-ships` · 0/7 done

| Ticket | Title | Status | Pri | |
|---|---|---|---|---|
| AOS-49 | Agent credential exchange for local runtimes (Cadence): device-style code, email-OTP sign-in, scope consent, revocable credential | backlog | P2 | existing |
| CAD-331 | P4 Connected platforms: contract ADR, credential proxy, effect gate (read/draft/send), Cloudflare platform, AgenticOS v2 adapter | backlog | P2 | new |
| CAD-365 | ADR: connected-platform contract — credential exchange, MCP tools with declared effects, pending-effect API | backlog | P2 | new |
| CAD-366 | Platform proxy: credential custody, per-agent scopes, effect gate | backlog | P2 | new |
| CAD-367 | Cloudflare (own account) platform: preview deploy automatic, production deploy as an effect | backlog | P2 | new |
| CAD-368 | AgenticOS v2 platform adapter | backlog | P3 | new |
| CAD-369 | Platforms UI: services, access matrix, pending effects | backlog | P3 | new |

## M6 · Autonomy dial (Team)

Routine merges automatic per project; scorecards drive routing.

- **Exit test:** Only after the unattended acceptance check (CAD-225) is green.
- **Depends on:** CAD-225, M3
- **Tag:** `m6-autonomy` · 0/4 done

| Ticket | Title | Status | Pri | |
|---|---|---|---|---|
| CAD-86 | Overview slice 4: flow and quality metrics | backlog | P3 | existing |
| CAD-139 | Idea pipeline: an idea ticket triggers research and a plan draft, then waits for the operator's decision | ready | P2 | existing |
| CAD-225 | Unattended team acceptance: goal to reviewed merge with restart and provider failure recovery | backlog | P0 | existing |
| CAD-332 | Autonomy dial: routine merges automatic per project, unlocked only after CAD-225 | backlog | P3 | new |

## Go-to-market (GTM)

Design partners after M1 · public beta after M3 · GA after M5.

- **Exit test:** Legal review of provider terms before the beta.
- **Depends on:** M1, M3, M5
- **Tag:** `gtm` · 0/1 done

| Ticket | Title | Status | Pri | |
|---|---|---|---|---|
| CAD-333 | Go-to-market: design partners, provider-terms legal review, demo, OSS launch | backlog | P2 | new |
