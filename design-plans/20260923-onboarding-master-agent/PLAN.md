# Plan — an autonomous dev team that learns, on your machine

Date: 2026-09-23. Status: direction accepted by the operator ("go", 2026-09-23);
individual designs stay proposals until their ADR or ticket lands. Readable
source of the Plan tab in [index.html](index.html). Companion records:
[ROADMAP.md](ROADMAP.md) (milestones and tickets), [RESEARCH.md](RESEARCH.md)
(state of the art), [DIRECTION.md](DIRECTION.md) (market and go-to-market),
[TECHNICAL.md](TECHNICAL.md) (install, identity, platforms, sandbox),
[PROVENANCE.md](PROVENANCE.md) (decision ledger). Architecture records:
[agent filesystem](../../docs/design/AGENT-FILESYSTEM.md) and
[learning loop](../../docs/design/LEARNING-LOOP.md).

## Summary

One **master assistant** talks with the operator. It runs **projects** — a PM
session per project, with developer, QA and DevOps sessions on the agents the
operator already pays for. Every agent is a folder of Markdown files (`agents/<slug>/SOUL.md`,
`AGENT.md`, `MEMORY.md`) and can run many sessions at once. Every finished job, question
or blocker produces a **report**; a **verifier** (deterministic checks plus a
curator from a different vendor) turns reports into **verified memory**; every
agent starts its next task with a small, cited **context pack** instead of
re-reading. The operator approves what matters.

## 1. Organization

```mermaid
flowchart LR
  OP[Operator] -->|goals, approvals| M[Master · assistant]
  M -->|starts sessions from agent files, proposes plans| T1[PM · project A]
  M --> T2[PM · project B]
  T1 --> D[Dev x1-4] & Q[QA] & O[DevOps]
  D & Q & O -.->|reports, questions| T1
  T1 -.->|escalations, digest| M
  D & Q & O -.->|reports| C[Curator]
  C -->|verified items| V[(Company vault · Markdown in git)]
  V -->|context packs| D & Q & O & T1
```

- **Responsibility flows down.** The master proposes a project plan and its
  staffing (agents × sessions); the operator approves. The PM owns the plan and dispatches inside it. One
  writer per code area; research and review run in parallel.
- **Reports flow up.** Every job, question or blocker files a typed report: the
  six-field reflection, evidence, constraints copied verbatim, and feedback on
  the context it was given.
- **Questions climb, then stop.** Worker → PM → master → operator. The vault is
  checked first; a question stops at the first level that can answer it, and the
  answer becomes a decision nobody asks twice.

## 2. How the team learns

| Step | What happens | Precedent |
|---|---|---|
| 1 Report | Typed claims (fact, strategy, pitfall, procedure, question) with evidence: `file:line@SHA`, command + exit code, test id | ReasoningBank, SWE-Exp, MAST |
| 2 Checks | Resolve citations at HEAD, re-run cited tests in a build slot, secret scan, de-duplicate, detect contradictions, tag external-derived content | Copilot Memory; DGM's faked logs |
| 3 Curator | A different vendor from the proposer: ADD / UPDATE / INVALIDATE / NOOP / ESCALATE | ACE, Mem0, self-preference study |
| 4 Vault | One Markdown item per file in git with scope, evidence, provenance, validity and counters; contradictions invalidate, nothing is silently deleted | Zep, Letta |
| 5 Pack | At session start: SOUL + AGENT + MEMORY index, contract, top cited items "verified at SHA" — re-checked at build, stale items withheld with a reason | Copilot Memory, Claude Code memory, Agent Skills |
| 6 Feedback | The next report marks items used, helpful or wrong and lists files it still re-read; harmful items demote; unused items expire after 28 days | ACE counters, Copilot Memory |
| 7 Consolidation | Nightly, the curator proposes a vault change as a reviewed diff — never an in-place rewrite | Managed Agents Dreams |
| 8 Measure | A/B with and without packs on matched tasks: reuse, helpful/harm, stale rate, reads avoided, tokens, time, merges without rework | Copilot A/B, VibeMemBench |

Details and the trust matrix: [learning loop](../../docs/design/LEARNING-LOOP.md).

## 3. Agents are folders of Markdown

Decision #1 (operator, 2026-09-23): the system is a filesystem, and all
information is Markdown. There is no separate role layer: an **agent** is the
definition and runs as many **sessions** as needed; all sessions share its soul,
contract and memory.

```text
~/pm/
  agents/<slug>/       SOUL.md  AGENT.md  MEMORY.md  memory/  journal/
  projects/<slug>/     PROJECT.md  shared/  memory/  tickets/<ID>/   # PROJECT.md: agents × sessions
  company/  lessons/  decisions/  faq/  inbox/
```

Full design, file contracts and who may write what:
[agent filesystem](../../docs/design/AGENT-FILESYSTEM.md).

| Agent | Mandate | Preferred | Fallback | Sessions |
|---|---|---|---|---|
| Master · assistant | Talks with the operator; creates teams and plans; routes questions; daily digest. Never implements. | claude · opus · high | codex · gpt-5.5 · high | 1 per install |
| PM | Owns one team's plan; testable acceptance before code; dispatches inside the plan; collects reports | codex · gpt-5.5 · medium | claude · sonnet · medium | 1 per team |
| Developer | One issue, own worktree, tests, PR with SHA, report | codex · gpt-5.5-codex · high | claude · sonnet · high | 1–4 per team |
| QA reviewer | Fresh-context review of the exact commit against the contract; verdict pinned to SHA | claude · opus · high | codex · gpt-5.5 · high | 1 per team |
| DevOps | Merge queue with PASS on head; deploys and sends as effects; host health | claude · sonnet · medium | pi · glm-5.3-flash · low | 1 per team |
| Researcher | Spikes, specs, ADR drafts with sources | claude · opus · high | codex · gpt-5.5 · high | on demand |
| Curator | Verifies candidates, merges duplicates, retires stale items, drafts skills | codex · gpt-5.5 · medium | claude · sonnet · medium | 1 per company |

Defaults are illustrative; model names follow each CLI's aliases.

## 4. Operating rules (enforced by the daemon, not the prompt)

- **Contract first.** The PM writes testable acceptance before code; QA verifies
  against the contract, never the author's summary.
- **Independent review.** Fresh context always; a different vendor by default,
  with pairings chosen by measured review precision.
- **One writer per code area.** Path-scoped write leases; parallel work is reads.
- **Separation of duties.** No agent reviews, merges or accepts its own work or
  lessons, edits its own `AGENT.md`, or treats another agent's message as consent.
- **Evidence over self-report.** "Done" is a claim until a verdict pins it to a
  commit; "tests passed" is re-run before a lesson is accepted.
- **Lean context.** No repo overviews in packs; short instruction files;
  byte-stable prefixes; fresh sessions over stale revivals.

## 5. Milestones

| Milestone | Outcome | Exit test |
|---|---|---|
| M0 Safe foundation | Sandbox, real backups, event roll-up | Backup → restore round-trip in a sandbox; production untouched |
| M1 First conversation | Install → wizard → chat with a master that remembers | Clean box to chat in < 5 min; kill the session mid-plan → it resumes |
| M2 One governed team | Master → one team from agent files; plans, gate, reviews, reports | A 3-issue plan lands with fresh-context verdicts, typed reports, and a question answered at the lowest level |
| M3 The team learns | Verified memory, Markdown vault, packs, metrics | A lesson from one worker is verified by another vendor and reused by a different worker with measured savings; a stale item is withheld; a planted poisoned item is rejected |
| M4 Many teams | Several teams under one master | Two teams run concurrently with no cross-team leakage |
| M5 Ships | Connected platforms and effects | Preview deploys auto; production and sends only on the operator's press |
| M6 Autonomy dial | Routine merges automatic per project | Only after CAD-225 passes |
| GTM | Partners after M1 · beta after M3 · GA after M5 | Legal review of provider terms before beta |

Tickets per milestone: [ROADMAP.md](ROADMAP.md).

## 6. Decisions

| # | Topic | Decision | Status |
|---|---|---|---|
| 1 | Agent definitions | Filesystem, all Markdown: `agents/<slug>/SOUL.md`, `AGENT.md`, `MEMORY.md`; no role layer — an agent runs many sessions; no teams for now — staffing in `PROJECT.md`; project artifacts under `projects/<slug>/` (CAD-338, answers CAD-116) | operator direction 2026-09-23; design record proposed |
| 2 | Memory policy | Logic checks + cross-vendor curator for project/role scope; two-review quorum for company scope; external content quarantined (CAD-347) | approved 2026-09-23 |
| 3 | Unblocking memory | Separate *who proposed* from *is it true*: one daemon identity check for every launched agent (CAD-381); trust matrix provenance × evidence; the 11 stuck lessons move through checks → curator → operator queue (CAD-382) | confirmed 2026-09-23 |
| — | Organization | One master; a PM session per project; dev, QA, DevOps sessions; curator at company level; no teams for now | operator direction |
| — | Autonomy | Plan freely, ask to dispatch; operator merges and presses effects | decided |
| — | Master providers | Structured (Claude, Codex, Pi); unmodified official binaries only | decided |
| — | Platforms | Contract-first; Cloudflare own account first; AgenticOS v2 via AOS-49 | decided |

## 7. What we will not do

- Run a hosted Cadence service — hosting is AgenticOS's job (AOS-34).
- Run an always-on all-agent chat, or peer swarms without an owner.
- Inject unverified memory, or promote anything derived from external content
  without the operator.
- Let any agent grade, merge or accept its own work, edit its own `AGENT.md`,
  see a credential, or press an effect.
- Turn on auto-merge before the unattended acceptance check passes.
