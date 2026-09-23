# Roadmap — milestones and tickets

Date: 2026-09-23. Generated from the tracker. Milestones are tags (`cadence issue ls --tag <tag>`); **core** marks the 30 tickets on the pragmatic path (tag `core`) — everything else is later. Regenerate rather than hand-edit.

## M0 · Safe foundation

- **Exit test:** Backup → restore round-trip in a sandbox; production untouched.
- **Tag:** `m0-safe` · 3 core · 1/5 done

| Ticket | Title | Status | Core |
|---|---|---|---|
| CAD-310 | Sandbox profile: cadence sandbox up/env/down/reset/ls with isolated state, tracker, port and gated global side effects | doing | core |
| CAD-316 | Event retention: roll up delivery-noise events; agent rm keeps thread history | review | core |
| CAD-396 | Backup hardening after CAD-314: retention trusts manifest-named files, export keeps messages.turn_id, port #196's no-clobber restore | doing | core |
| CAD-309 | P0 Foundation: sandbox, install, setup, operator auth, backup/restore | doing | later |
| CAD-314 | cadence backup/export/restore: SQLite online backup + manifest, bundle without credentials, restore with repo remap | done | later |

## M1 · First conversation

- **Exit test:** Clean box to chat in under 5 minutes; kill the master's session mid-plan and it resumes.
- **Tag:** `m1-first-chat` · 9 core · 0/16 done

| Ticket | Title | Status | Core |
|---|---|---|---|
| CAD-311 | Release workflow and install.sh: linux x86_64/aarch64 + macOS arm64, UI embedded, checksums, --prefix | backlog | core |
| CAD-312 | cadence setup: idempotent first run with --json checks {check,status,detail,fix} and single-use login link | backlog | core |
| CAD-313 | ADR + implementation: operator identity for the web UI (0600 token -> single-use link -> HttpOnly cookie) | doing | core |
| CAD-319 | Threads store and payload events (assistant_text, tool_call redacted, tool_result, turn_result) | backlog | core |
| CAD-324 | Continuity packs: summary + last N turns + plan state + preferences on new/lost/compacted sessions | backlog | core |
| CAD-325 | Read model: indexed tracker and cached overview; SSE carries entity diffs | backlog | core |
| CAD-326 | UI foundation: router, query cache, feature folders, light + dark tokens | backlog | core |
| CAD-327 | Setup wizard: start (fresh/restore/tracker/import), environment, platforms, master, workspace, team, launch | backlog | core |
| CAD-328 | Chat-first Home: thread, plan/permission/effect cards, Needs-you rail, since-you-left | backlog | core |
| CAD-160 | Compose objective + outstanding criteria into every task-bound message; never truncate criteria (ADR-0002 phase 1) | review | later |
| CAD-315 | macOS port: caller attribution and host doctor without /proc | backlog | later |
| CAD-317 | Sandbox S3: clean-machine install CI (ubuntu:24.04 + macos-14) with fake providers | backlog | later |
| CAD-318 | P1 Chat and continuity: threads, payload events, Pi adapter, continuity packs, read model, wizard, chat Home | backlog | later |
| CAD-320 | Claude adapter: persist intermediate assistant text and tool I/O summaries | backlog | later |
| CAD-322 | Pi headless adapter (pi --mode rpc): streaming, tool progress, model/effort, cancel, resume | backlog | later |
| CAD-323 | Real interrupt: stop the provider turn and reconcile its effects | backlog | later |

## M2 · One governed project

- **Exit test:** A 3-issue plan lands with verdicts pinned to each head, a report per job, and one question answered at the lowest level.
- **Tag:** `m2-one-team` · 8 core · 0/28 done

| Ticket | Title | Status | Core |
|---|---|---|---|
| CAD-338 | Agent filesystem ADR (amends ADR-0001): agents/<slug>/ SOUL.md AGENT.md MEMORY.md with many sessions; staffing in PROJECT.md; all Markdown | backlog | core |
| CAD-339 | Master role: company assistant that creates teams from the catalog, routes escalations and sends a daily digest; never implements | backlog | core |
| CAD-341 | Structured reports: done / question / blocked with the six-field reflection and context feedback, stored per task | backlog | core |
| CAD-359 | Plan proposals: plan.yaml schema, plan_proposed event and plan card | backlog | core |
| CAD-360 | Plan approval commit and daemon dispatch gate: refuse dispatch outside approved plans with a named reason | backlog | core |
| CAD-361 | Staff and dispatch from approved plans via team.yaml roles; the team PM dispatches | backlog | core |
| CAD-362 | Reviewer routing: cross-vendor QA assignment, verdict → DevOps merge-queue handoff | backlog | core |
| CAD-363 | Merges, effects and operator questions in Needs-you: one decision per cause | backlog | core |
| CAD-77 | Multi-level groups: upstream chains, results one level up, escalation that bubbles when unanswered | dropped | later |
| CAD-78 | Task roles in jobs: planner, implementer, reviewer, merger with enforced separation of duties | backlog | later |
| CAD-116 | team.yaml schema + parser + cadence team show with drift report (ADR-0001 phase 2a) | backlog | later |
| CAD-120 | Route qa-1's review through job verdict so the existing self-review gate is on the live path (ADR-0001 phase 3a) | backlog | later |
| CAD-122 | Role capabilities: closed set, verb-to-capability map, daemon refusal with the rule named (ADR-0001 phase 3b) | backlog | later |
| CAD-131 | Plumb effort for codex/cursor/devin: resolve (provider, model, effort) and compose slug suffixes, validating what the providers do not | backlog | later |
| CAD-213 | Reliable report delivery: durable GitHub relay and quota-aware PM intake consumer | ready | later |
| CAD-223 | Agent profile editor: provider capabilities, desired versus running model and effort | backlog | later |
| CAD-224 | Goal intake: turn project outcomes into a dependency-aware executable plan | backlog | later |
| CAD-298 | Dispatch refuses an issue with empty acceptance (switch CAD-159's warning to a refusal after backfill) | backlog | later |
| CAD-329 | P2 Projects and plans: project new, plan propose, plan card approval, daemon dispatch gate | backlog | later |
| CAD-330 | P3 Delegation: workers from approved plans, cross-vendor reviewer routing, merges in Needs-you, terminal view | backlog | later |
| CAD-340 | Cross-vendor rules: QA provider differs from the author's, curator differs from the proposer's; fallback launch with recorded reason | backlog | later |
| CAD-342 | Question routing up the tree: worker → PM → master → operator, vault/FAQ lookup first; answers become decisions and FAQ candidates | backlog | later |
| CAD-343 | Agents UI: definitions (SOUL, AGENT, MEMORY editor with provider/model/effort), live sessions, reports | backlog | later |
| CAD-345 | Reconcile team and memory docs with code: QA vs PM acceptance, role vocabularies, stale claims | backlog | later |
| CAD-358 | cadence project new: register a repo and seed its context manifest and team.yaml | backlog | later |
| CAD-364 | Live read-only terminal view for pty agents (tmux control mode) | backlog | later |
| CAD-378 | One writer per code area: path-scoped write leases in dispatch; parallel work limited to reads (review, research, triage) | backlog | later |
| CAD-393 | Board writes by managed agents are attributed to the operator: ui.rs write_caller treats any non-pane peer as Operator | dropped | later |

## M3 · The team learns

- **Exit test:** A lesson from one worker is verified by a curator of another vendor and reused by a different worker; a stale item is withheld; a planted poisoned item is rejected.
- **Tag:** `m3-team-learns` · 10 core · 0/25 done

| Ticket | Title | Status | Core |
|---|---|---|---|
| CAD-194 | Job dispatch injects no memory, and path scope needs recorded commits so new issues match nothing | backlog | core |
| CAD-203 | Stale and contradicted accepted lessons stay eligible for injection: match filters on status only | doing | core |
| CAD-260 | Unblock memory review: define native-identity proof for proposers, or an operator curation path | backlog | core |
| CAD-346 | Company vault: git layout (company, teams, projects, playbooks, lessons, decisions, faq, inbox) with index and lint | backlog | core |
| CAD-347 | Memory policy v2 (ADR): logic checks + cross-vendor curator for project/role scope, quorum for company scope, external quarantine, validity and expiry, counters | backlog | core |
| CAD-348 | Verification runner: resolve citations at HEAD, re-run cited tests in a build slot, secret scan, dedupe, contradiction check | backlog | core |
| CAD-349 | Curator role and queue: ADD / UPDATE / INVALIDATE / NOOP / ESCALATE verdicts with operator escalation | backlog | core |
| CAD-351 | Context pack builder: one path for issue dispatch, job dispatch and continuity; items verified at SHA; planned paths; budgets | backlog | core |
| CAD-381 | One identity verifier for every daemon-launched endpoint (pty and managed): reuse the CAD-230 enrollment for memory, reviews and reports | doing | core |
| CAD-382 | Clear the stuck memory backlog: the 11 proposed lessons become agent-claimed candidates and run checks → curator → operator queue | backlog | core |
| CAD-66 | Knowledge graph per project via graphify: built from repo, tracker and notes; refreshed on merge; shown on the board | backlog | later |
| CAD-67 | LLM wiki: agent-maintained project pages with provenance and staleness checks | dropped | later |
| CAD-111 | Automatic retro per merged issue: rounds, what review caught, flakes, lead time, proposed lessons | backlog | later |
| CAD-192 | Evidence-backed memory capture: structured required source, secret scan on propose | backlog | later |
| CAD-193 | Memory lifecycle enforcement: prior-status check, verify must cite evidence, atomic supersede | backlog | later |
| CAD-197 | Auditor agents cannot deliver artifacts: codex-1 attach and notify failed with bwrap RTM_NEWADDR | backlog | later |
| CAD-350 | Candidate extraction from reports and retros: typed claims (fact, strategy, pitfall, procedure, faq) with evidence | backlog | later |
| CAD-352 | Nightly consolidation as a reviewed vault PR: merge duplicates, invalidate stale, propose skills; never rewrite in place | backlog | later |
| CAD-353 | Skills from procedures: promote to Agent Skills only after an evaluation shows benefit | backlog | later |
| CAD-354 | Learning metrics and A/B harness: reuse, helpful/harm, stale rate, reads avoided, tokens and time; planted-memory red-team | backlog | later |
| CAD-355 | Move agent notes (/var/www/agent-notes) into git so notes and retros are portable | backlog | later |
| CAD-356 | Project context manifests for every project and commit-keyed code maps as pack sources | backlog | later |
| CAD-357 | Vault UI: overview, company docs, lessons, verification queue, context-pack preview | backlog | later |
| CAD-392 | Home directory ~/.cadence (CADENCE_HOME) and backward-compatible tracker migration: expand → roll out → migrate → contract | backlog | later |
| CAD-394 | Pilot a typed decision model (TypeSafe Jev 1.13 via OpenRouter) for ticket classification, tagging and routing — offline replay first | backlog | later |

## M4 · Many projects

- **Exit test:** Two projects run concurrently with no cross-project leakage.
- **Tag:** `m4-many-teams` · 0 core · 0/6 done

| Ticket | Title | Status | Core |
|---|---|---|---|
| CAD-80 | Budgets per level: tokens, quota and wall-clock per role and per epic, with quota-aware routing | backlog | later |
| CAD-82 | Overview dashboard: one screen across all projects for what needs me, who is doing what, what is in flight, health and flow | backlog | later |
| CAD-114 | Provider quota visibility: Devin ACU and Codex quota in doctor --host and the Overview | backlog | later |
| CAD-212 | Safe agent provider replacement: drain, checkpoint, validate and transfer queued work without duplicate delivery | backlog | later |
| CAD-226 | Agent inbox UX: accountable consumers, delivery stages and actionable backlog | backlog | later |
| CAD-344 | Multiple teams under one master: cross-team capacity and priorities, isolation, daily digest | backlog | later |

## M5 · Ships

- **Exit test:** Preview deploys automatic; production deploys and sends only on your press.
- **Tag:** `m5-ships` · 0 core · 0/7 done

| Ticket | Title | Status | Core |
|---|---|---|---|
| AOS-49 | Agent credential exchange for local runtimes (Cadence): device-style code, email-OTP sign-in, scope consent, revocable credential | backlog | later |
| CAD-331 | P4 Connected platforms: contract ADR, credential proxy, effect gate (read/draft/send), Cloudflare platform, AgenticOS v2 adapter | backlog | later |
| CAD-365 | ADR: connected-platform contract — credential exchange, MCP tools with declared effects, pending-effect API | backlog | later |
| CAD-366 | Platform proxy: credential custody, per-agent scopes, effect gate | backlog | later |
| CAD-367 | Cloudflare (own account) platform: preview deploy automatic, production deploy as an effect | backlog | later |
| CAD-368 | AgenticOS v2 platform adapter | backlog | later |
| CAD-369 | Platforms UI: services, access matrix, pending effects | backlog | later |

## M6 · Autonomy dial

- **Exit test:** Only after CAD-225 is green.
- **Tag:** `m6-autonomy` · 0 core · 0/4 done

| Ticket | Title | Status | Core |
|---|---|---|---|
| CAD-86 | Overview slice 4: flow and quality metrics | backlog | later |
| CAD-139 | Idea pipeline: an idea ticket triggers research and a plan draft, then waits for the operator's decision | ready | later |
| CAD-225 | Unattended team acceptance: goal to reviewed merge with restart and provider failure recovery | backlog | later |
| CAD-332 | Autonomy dial: routine merges automatic per project, unlocked only after CAD-225 | backlog | later |

## Go-to-market

- **Exit test:** Legal review of provider terms before the beta.
- **Tag:** `gtm` · 0 core · 0/1 done

| Ticket | Title | Status | Core |
|---|---|---|---|
| CAD-333 | Go-to-market: design partners, provider-terms legal review, demo, OSS launch | backlog | later |
