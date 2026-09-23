# Research — what works for teams of coding agents

Date: 2026-09-23. Status: research record for the consolidated plan
([PLAN.md](PLAN.md)). Vendor numbers are self-reported; many 2026 arXiv items
are preprints and directional. Key memory claims were re-checked against
primary sources on 2026-09-23 and corrected where noted. Nothing here is a
decision by itself — decisions are recorded in [PROVENANCE.md](PROVENANCE.md).

## Bottom line

A shallow hierarchy with **one writer per code area**, a PM that writes
**testable acceptance before code**, reviewers that **start fresh**, typed
handoffs with constraints copied verbatim, and small **verified** context loaded
on demand. Memory helps only when it is verified, cited, scoped and measured —
in a September 2026 benchmark most memory systems did not beat memory-off.

## 1. Team topology

| Source | Finding | Take |
|---|---|---|
| Anthropic, multi-agent research system (2025-06) | Lead + parallel subagents: +90.2% over one agent at ~15× tokens; subagents store work externally and pass references; "most coding tasks involve fewer truly parallelizable tasks than research" | Pass artifacts by reference; don't fan out writes |
| Cognition, "Don't build multi-agents" (2025-06) → "multi-agents working" (2026-04) | Works when "writes stay single-threaded": fresh-context review loops, a "smart friend" model, map-reduce-and-manage; swarms "mostly a distraction" | One writer per area |
| MAST, arXiv 2503.13657 (NeurIPS 2025) | 1,642 traces: ~44% specification, 32% misalignment, 24% verification failures; sharper role specs +9.4%, objective-level verification +15.6% | Contract first; QA against the objective |
| Google/MIT scaling study, arXiv 2512.08296 (2026-01) | Coordinator +80.9% on parallelizable tasks; every multi-agent variant −39–70% on sequential tasks; independent agents amplify errors 17.2× vs 4.4× coordinated | Choose topology per task; keep a single-agent baseline |
| Claude Code agent teams and Projects (2026) | Teams: lead, shared task list with locks, 3–5 teammates, no same-file edits, no nested teams. Projects (2026-09-17): coordinator "sees what threads report back, not every step"; shared `MEMORY.md` index; per-role model and effort | Closest analog to master → PM → workers |
| Factory Missions (2026) | Validation contract written first; fresh-context workers and validators; validators from another vendor; 2–4 validation rounds at ~12× tokens | Contract first; independent validation |
| Cursor; Anthropic C compiler (2026) | Lock-based peers: 20 agents at the throughput of 2–3; a single integrator gate became the bottleneck; 16 lock-only peers "frequently broke existing functionality" | No peer swarms, no single serial gate; keep a regression oracle |

## 2. Roles, model routing and review

- **Roles are files.** Claude subagent frontmatter sets tools, model, effort,
  permissions, isolation, memory and turn limits; Codex agents use TOML. One
  versioned spec per role, compiled per vendor at launch.
- **Mix models by role.** Mixed-model teams: up to +44% accuracy at equal cost,
  or equal accuracy at up to 12× lower cost (arXiv 2606.20629). An Opus advisor
  lifted Sonnet +2.7 pp at 11.9% lower cost (Anthropic, 2026-04).
- **Cross-vendor review is asymmetric.** Claude reviewing Codex raised pass rate
  71.6% → 89.7%; Codex reviewing Claude lowered it 91.4% → 82.8% (arXiv
  2607.21656; reviewer could not run code). Separate-session review beat
  same-session review (arXiv 2603.12123). → pair reviewers by measured precision.
- **Reviewer reliability.** Tool-using judges "dramatically outperform" plain
  judges (Agent-as-a-Judge); LLM review F1 fell from 0.657 on diffs under 10
  lines to 0.043 over 150 lines (arXiv 2606.15689). → small diffs, risk tiers.
- **Self-preference.** In rubric-based evaluation, judges "can be more than 50%
  more likely" to wrongly pass their own model's output (arXiv 2604.06996).

## 3. Communication and escalation

- Typed handoffs: Cursor worker reports carry notes, concerns, deviations,
  findings; Factory keeps contract, features and knowledge base as shared state.
- Compressed handoffs keep facts but drop constraints (survival 0.80 → 0.57 at
  25 words); stating constraints explicitly cut leakage below 15% (arXiv 2608.29028).
- Asking clarifying questions improved results by up to 74% (Ambig-SWE, ICLR 2026).
- A2A v1.0 (2026-03) models an `input-required` task state. Claude Code treats an
  approval relayed by another agent as untrusted.

## 4. Learning from experience

| Approach | Evidence | Verdict |
|---|---|---|
| ACE, arXiv 2510.04618 | Itemized bullets with helpful/harmful counters, delta merges; +10.6% agents, +8.6% finance; shows "context collapse" from iterative rewriting | Adopt as the template |
| ReasoningBank, arXiv 2509.25140 | Strategies from successes *and* failures; +8.3 / +7.2 / +4.6 on WebArena across backbones | Adopt failure distillation |
| ExpeL, AWM (arXiv 2308.10144, 2409.07429) | Voting lifecycle; successful runs become workflows | Adopt |
| SWE-Exp, arXiv 2507.23361 | Experience bank from repairs; 73.0% SWE-bench Verified | Adopt |
| Dynamic Cheatsheet, arXiv 2504.07952 | Whole-memory rewrites collapse over time (per ACE) | Avoid full rewrites |
| Darwin Gödel Machine (Sakana, 2025-05) | Self-improving agent removed hallucination markers and faked test logs | Never let an agent grade itself |
| 2026 transfer studies (arXiv 2604.14004, 2604.27003, 2602.08316) | Abstract know-how transfers; step-by-step traces hurt; unfiltered experience helps little | Store lessons, not transcripts |

## 5. Memory systems in production

| System | What it does | Take |
|---|---|---|
| GitHub Copilot Memory (blog 2026-01-15; docs) | Facts with code citations, verified "against the current branch before using"; unused entries deleted after 28 days (docs); A/B merge rate 90% vs 83% | Cite, validate at use, expire |
| Claude Managed Agents memory + Dreams (docs, 2026) | Stores `read_only` or `read_write`; every change "creates an immutable memory version"; Dreams produce "a new, reorganized memory store" — input never modified | Consolidate as a reviewed new version |
| Letta context repositories (2026-02-12) | Git-backed memory files with frontmatter; every change versioned with a commit | Matches a git vault |
| Claude Code memory | Layered instruction files; an index (first 200 lines) plus topic files on demand | Index + topics |
| Mem0; Zep/Graphiti (arXiv 2504.19413, 2501.13956) | ADD/UPDATE/DELETE/NOOP; contradictions invalidate rather than delete | Operation set, validity intervals |
| Codex memories; Devin knowledge | Codex: "a helpful recall layer, not the only source for rules"; Devin suggests, a human approves | Suggest-then-approve |

## 6. Verification and safety

- Poisoning: AgentPoison (>80% success with <0.1% poisoned, arXiv 2407.12784),
  MINJA (arXiv 2503.03704), MemoryGraft — fake "successful experiences" (arXiv
  2512.16962). OWASP Agentic Top 10 lists memory poisoning (ASI06).
- Defenses: provenance, citation checks at use, read-only shared stores with a
  proposal queue, consensus checks (A-MemGuard: ">95%" fewer successful attacks,
  arXiv 2510.02373), quarantine for external-derived content.
- Re-run "tests passed" before accepting a lesson (DGM faked logs).

## 7. Skills, instruction files and vaults

- Agent Skills (open standard since 2025-12) load in stages. Curated skills lifted
  average pass rate 33.9% → 50.5%; self-generated skills landed *below* the
  no-skills baseline in all three tested configurations (SkillsBench v4, arXiv
  2602.12670, 2026-06).
- AGENTS.md-style context files "do not generally improve task success rates,
  while increasing inference cost by over 20%" (arXiv 2602.11988). Keep files
  short: non-standard rules only, no repo overviews.
- LLM-maintained wikis (Karpathy's llm-wiki gist): raw sources untouched, an
  LLM-owned Markdown wiki with index, append-only log and a lint pass. Google Code
  Wiki regenerates after every change.
- Scopes: company (read-only) → team → project → role → agent.

## 8. Context engineering

- Context rot: all 18 models tested degraded as input grew (Chroma, 2025-07).
- Progressive disclosure; clearing stale tool results cut 84% of tokens and, with
  a memory tool, gave +39% (Anthropic, 2025-09).
- Ranked repo maps (Aider ~1k tokens); RepoGraph +32.8% relative; content-hash
  index reuse (Cursor, 7.87 s → 0.53 s to first query).
- Cache economics: static content first, byte-stable prefixes.

## 9. Evaluation

- About half of SWE-bench-passing PRs would not be merged (METR, 2026-03).
- VibeMemBench (arXiv 2609.23570, 2026-09): 11 of 12 memory-system/solver
  pairings failed to beat memory-off; verified past experience gave +1.1 to +4.5.
- Role benchmarks: SWE-bench Pro, SWE-Lancer manager tasks (PM), Terminal-Bench 2.0
  (DevOps), TheAgentCompany (whole team).
- Our own replay suite from closed tickets: merges without rework, cost and time
  per merged PR, escalations, review precision on seeded bugs — by vendor pair.

## What we adopt — and where

| Principle | Ticket |
|---|---|
| Shallow hierarchy, one writer per code area | CAD-339, CAD-77, CAD-378 |
| Contract before code | CAD-360, CAD-298, CAD-160 |
| Fresh-context review, pairing by measured precision | CAD-340, CAD-362, CAD-120 |
| Typed reports with verbatim constraints | CAD-341 |
| Ask, don't guess; human consent only | CAD-342 |
| Verified, cited, expiring memory; trust matrix | CAD-347, CAD-348, CAD-349, CAD-381, CAD-382 |
| Itemized Markdown vault; reviewed consolidation | CAD-346, CAD-352 |
| Lean packs, skills by name | CAD-351, CAD-356 |
| Skills only with evidence | CAD-353 |
| Measure and demote; poisoning red-team | CAD-354 |
