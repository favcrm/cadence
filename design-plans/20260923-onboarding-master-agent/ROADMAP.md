# Roadmap — MVP first, then refine

Date: 2026-09-23. Generated from the tracker. The MVP is tag `mvp`; refine phases are the milestone tags. Regenerate rather than hand-edit.

## MVP — hand work to an agent team

- **Exit test:** The eight use cases in PLAN.md work end to end on a clean Linux box.
- 0/23 done

| Ticket | Title | Status |
|---|---|---|
| CAD-310 | Sandbox profile: cadence sandbox up/env/down/reset/ls with isolated state, tracker, port and gated global side effects | doing |
| CAD-311 | Release workflow and install.sh: linux x86_64/aarch64 + macOS arm64, UI embedded, checksums, --prefix | backlog |
| CAD-312 | cadence setup: idempotent first run with --json checks {check,status,detail,fix} and single-use login link | backlog |
| CAD-313 | ADR + implementation: operator identity for the web UI (0600 token -> single-use link -> HttpOnly cookie) | doing |
| CAD-319 | Threads store and payload events (assistant_text, tool_call redacted, tool_result, turn_result) | backlog |
| CAD-320 | Claude adapter: persist intermediate assistant text and tool I/O summaries | backlog |
| CAD-323 | Real interrupt: stop the provider turn and reconcile its effects | backlog |
| CAD-324 | Continuity packs: summary + last N turns + plan state + preferences on new/lost/compacted sessions | backlog |
| CAD-325 | Read model: indexed tracker and cached overview; SSE carries entity diffs | backlog |
| CAD-326 | UI foundation: router, query cache, feature folders, light + dark tokens | backlog |
| CAD-327 | Setup wizard: start (fresh/restore/tracker/import), environment, platforms, master, workspace, team, launch | backlog |
| CAD-328 | Chat-first Home: thread, plan/permission/effect cards, Needs-you rail, since-you-left | backlog |
| CAD-338 | Agent filesystem ADR (amends ADR-0001): agents/<slug>/ SOUL.md AGENT.md MEMORY.md with many sessions; staffing in PROJECT.md; all Markdown | backlog |
| CAD-339 | Master role: company assistant that creates teams from the catalog, routes escalations and sends a daily digest; never implements | backlog |
| CAD-341 | Structured reports: done / question / blocked with the six-field reflection and context feedback, stored per task | doing |
| CAD-358 | cadence project new: register a repo and seed its context manifest and team.yaml | backlog |
| CAD-359 | Plan proposals: plan.yaml schema, plan_proposed event and plan card | backlog |
| CAD-360 | Plan approval commit and daemon dispatch gate: refuse dispatch outside approved plans with a named reason | backlog |
| CAD-361 | Staff and dispatch from approved plans via team.yaml roles; the team PM dispatches | backlog |
| CAD-362 | Reviewer routing: cross-vendor QA assignment, verdict → DevOps merge-queue handoff | backlog |
| CAD-363 | Merges, effects and operator questions in Needs-you: one decision per cause | backlog |
| CAD-396 | Backup hardening after CAD-314: retention trusts manifest-named files, export keeps messages.turn_id, port #196's no-clobber restore | doing |
| CAD-405 | Work model: explicit epic/task types, epic stages with exit criteria, computed weighted progress and health, milestones in PROJECT.md | backlog |

## Refine 1 — Learn

- **Exit test:** A lesson from one worker is verified by another vendor and reused by a different worker; stale withheld; poisoned rejected.
- 0/25 done

| Ticket | Title | Status |
|---|---|---|
| CAD-66 | Knowledge graph per project via graphify: built from repo, tracker and notes; refreshed on merge; shown on the board | backlog |
| CAD-67 | LLM wiki: agent-maintained project pages with provenance and staleness checks | dropped |
| CAD-111 | Automatic retro per merged issue: rounds, what review caught, flakes, lead time, proposed lessons | backlog |
| CAD-192 | Evidence-backed memory capture: structured required source, secret scan on propose | backlog |
| CAD-193 | Memory lifecycle enforcement: prior-status check, verify must cite evidence, atomic supersede | backlog |
| CAD-194 | Job dispatch injects no memory, and path scope needs recorded commits so new issues match nothing | backlog |
| CAD-197 | Auditor agents cannot deliver artifacts: codex-1 attach and notify failed with bwrap RTM_NEWADDR | backlog |
| CAD-203 | Stale and contradicted accepted lessons stay eligible for injection: match filters on status only | doing |
| CAD-260 | Unblock memory review: define native-identity proof for proposers, or an operator curation path | backlog |
| CAD-346 | Company vault: git layout (company, teams, projects, playbooks, lessons, decisions, faq, inbox) with index and lint | backlog |
| CAD-347 | Memory policy v2 (ADR): logic checks + cross-vendor curator for project/role scope, quorum for company scope, external quarantine, validity and expiry, counters | backlog |
| CAD-348 | Verification runner: resolve citations at HEAD, re-run cited tests in a build slot, secret scan, dedupe, contradiction check | backlog |
| CAD-349 | Curator role and queue: ADD / UPDATE / INVALIDATE / NOOP / ESCALATE verdicts with operator escalation | backlog |
| CAD-350 | Candidate extraction from reports and retros: typed claims (fact, strategy, pitfall, procedure, faq) with evidence | backlog |
| CAD-351 | Context pack builder: one path for issue dispatch, job dispatch and continuity; items verified at SHA; planned paths; budgets | backlog |
| CAD-352 | Nightly consolidation as a reviewed vault PR: merge duplicates, invalidate stale, propose skills; never rewrite in place | backlog |
| CAD-353 | Skills from procedures: promote to Agent Skills only after an evaluation shows benefit | backlog |
| CAD-354 | Learning metrics and A/B harness: reuse, helpful/harm, stale rate, reads avoided, tokens and time; planted-memory red-team | backlog |
| CAD-355 | Move agent notes (/var/www/agent-notes) into git so notes and retros are portable | backlog |
| CAD-356 | Project context manifests for every project and commit-keyed code maps as pack sources | backlog |
| CAD-357 | Vault UI: overview, company docs, lessons, verification queue, context-pack preview | backlog |
| CAD-381 | One identity verifier for every daemon-launched endpoint (pty and managed): reuse the CAD-230 enrollment for memory, reviews and reports | doing |
| CAD-382 | Clear the stuck memory backlog: the 11 proposed lessons become agent-claimed candidates and run checks → curator → operator queue | doing |
| CAD-392 | Home directory ~/.cadence (CADENCE_HOME) and backward-compatible tracker migration: expand → roll out → migrate → contract | backlog |
| CAD-394 | Pilot a typed decision model (TypeSafe Jev 1.13 via OpenRouter) for ticket classification, tagging and routing — offline replay first | backlog |

## Refine 2 — Reach (plus macOS, clean-machine CI, Pi)

- **Exit test:** Two projects run concurrently with no cross-project leakage.
- 0/6 done

| Ticket | Title | Status |
|---|---|---|
| CAD-80 | Budgets per level: tokens, quota and wall-clock per role and per epic, with quota-aware routing | backlog |
| CAD-82 | Overview dashboard: one screen across all projects for what needs me, who is doing what, what is in flight, health and flow | backlog |
| CAD-114 | Provider quota visibility: Devin ACU and Codex quota in doctor --host and the Overview | backlog |
| CAD-212 | Safe agent provider replacement: drain, checkpoint, validate and transfer queued work without duplicate delivery | backlog |
| CAD-226 | Agent inbox UX: accountable consumers, delivery stages and actionable backlog | backlog |
| CAD-344 | Multiple teams under one master: cross-team capacity and priorities, isolation, daily digest | backlog |

## Refine 3 — Ship

- **Exit test:** Preview deploys automatic; production and sends only on your press.
- 0/7 done

| Ticket | Title | Status |
|---|---|---|
| AOS-49 | Agent credential exchange for local runtimes (Cadence): device-style code, email-OTP sign-in, scope consent, revocable credential | backlog |
| CAD-331 | P4 Connected platforms: contract ADR, credential proxy, effect gate (read/draft/send), Cloudflare platform, AgenticOS v2 adapter | backlog |
| CAD-365 | ADR: connected-platform contract — credential exchange, MCP tools with declared effects, pending-effect API | backlog |
| CAD-366 | Platform proxy: credential custody, per-agent scopes, effect gate | backlog |
| CAD-367 | Cloudflare (own account) platform: preview deploy automatic, production deploy as an effect | backlog |
| CAD-368 | AgenticOS v2 platform adapter | backlog |
| CAD-369 | Platforms UI: services, access matrix, pending effects | backlog |

## Refine 4 — Autonomy

- **Exit test:** Only after CAD-225 is green.
- 0/4 done

| Ticket | Title | Status |
|---|---|---|
| CAD-86 | Overview slice 4: flow and quality metrics | backlog |
| CAD-139 | Idea pipeline: an idea ticket triggers research and a plan draft, then waits for the operator's decision | ready |
| CAD-225 | Unattended team acceptance: goal to reviewed merge with restart and provider failure recovery | backlog |
| CAD-332 | Autonomy dial: routine merges automatic per project, unlocked only after CAD-225 | backlog |
