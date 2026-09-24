# Cadence team: roles, responsibilities, routing

This is the operating model cadence runs on, and the model its own
development follows. The goal and roadmap are in `docs/CHARTER.md`.
Each role has a briefing under `docs/roles/`; an agent's briefing is its
standing instruction set.

## Roles at a glance

| Role | Alias | Runs on | Owns | Never |
|---|---|---|---|---|
| Operator | (human) | terminal, board, phone via tailnet | Goals, priorities between epics, approval of class `human` changes, credentials, accounts | Routine review or merges |
| PM | `fable-cc` | Claude Code session, inbox endpoint | Roadmap to issues, priority, dispatch, capacity, escalations, these docs; memory finalization (accept, reject, verify) from a native `pm` endpoint | Implement, review or merge PRs |
| Researcher | `rsch-1` | managed Claude, Opus, effort high | Answers with sources: prior art, library and API facts, feasibility, cost | Write product code, decide |
| Architect | `arch-1` | managed Claude, Opus, effort high | Options and trade-offs, ADRs, specs with runnable acceptance checks, ticket breakdown | Merge, implement beyond a spike |
| Developer | `dev-<n>` (and existing Devin aliases) | Devin pty in bypass mode; Claude, Cursor or Codex as alternates | One cohesive delivery per worktree: code, tests, docs, PR, qa note, review rounds | Touch another lane, merge, review |
| QA reviewer and memory reviewer | `qa-1` | managed Claude, Opus, effort high | Independent review, verdict pinned to a SHA, risk class, round-N kickoffs, residue issues, security flags; one independent review receipt on memory lessons | Merge, push to an author's branch, finalize a lesson |
| DevOps | `ops-1` | managed Claude, Opus, effort medium | Merge queue, combined gates, auto merges, post-merge ops, daemon restarts, host care | Merge class `human` without approval |

Names for new agents follow `<role>-<n>`. Existing Devin workers keep
their aliases until they are retired. An alias is a display name; the
role keys below are what the code stores.

## Role vocabulary

Three role fields exist; they are not interchangeable.

| Field | Values | Set by | Decides |
|---|---|---|---|
| Team role | `pm`, `research`, `architect`, `dev`, `qa`, `devops`, `worker` | `--team-role` on launch; `ops` is accepted as input and stored as `devops` | Model-default lookup; `pm` also marks a group root. Grants no authority |
| Runtime role | `pm`, `worker` | `--role` on register/join (default `worker`) | Authority: only `pm` may finalize memory (accept, reject, verify); `pm` also marks a group root |
| Context role | `pm`, `dev`, `qa`, `ops` | `roles:` in `docs/cadence/project-context.yaml`, `?role=` on the context API | Which project documents a role is shown |

The docs call the role DevOps, and its briefing is
`docs/roles/devops.md`. Team roles store `devops`; `ops` is accepted
only as launch input. The context API still stores and validates `ops`
(the manifest's `roles:` and `?role=`) until a follow-up moves it to
`devops`, so write `ops` there today. `ops-1` is a historical agent
alias.

## Delivery scope and working set

Track a user outcome, independent defect, or material dependency as an issue.
Keep implementation steps, related tests/docs, review corrections and routine
operational receipts in that issue's checklist or comments. Search existing
work before creating another ticket; preserve evidence when consolidating a
duplicate, and do not label unimplemented work done.

A PR is one self-contained, reviewable delivery slice. It can address multiple
related issues; a large outcome can need multiple PRs. Include related tests
and documentation in the same change. Split when independent rollback,
security or migration boundaries, dependencies, or reviewer comprehension
require it. Neither a checklist item nor an agent handoff requires its own PR;
unrelated work should not be bundled merely to reduce PR count.

Before dispatch, record observable acceptance, scope, dependencies, one writer
per area, the reviewer/result route and proportionate validation in the
existing issue/spec. Do not add a separate planning ceremony to a routine fix.
Use architecture review early where the decision is consequential.

Start with at most three active delivery outcomes, normally two development
streams and one review stream, and about five ready outcomes. Parent rollups
and roadmap ideas are not active developer work. Adjust this working target
using measured review/CI capacity within the host ceiling below; clear review
bottlenecks before starting more work. Reconcile stale doing/review labels
against actual owners, PRs and remaining acceptance, rather than inferring
completion from a single merged PR.

Use supported focused checks during iteration and required full gates on the
review candidate. All configured resource-admission and exact-revision review
rules still apply. Deterministic tooling watches long-running CI; agents act on
completion, failure or a meaningful blocker instead of repeated unchanged
status turns. Reuse eligible unchanged evidence, never a stale-head verdict.

After delivery, perform guarded cleanup and record one compact reflection at
the existing outcome. Curate reusable lessons in batches. Measure completed
outcomes, lead time, review age, rework, escaped defects and CI minutes per
accepted outcome; ticket and PR counts alone are not productivity measures.
These are team operating rules; they do not imply automated WIP enforcement.

## Lifecycle of a piece of work

```
goal (operator) ──► research question ──► rsch-1: research note (sources, recommendation)
                                              │
                    PM ranks ◄── arch-1: ADR/spec + acceptance checks + proposed tickets
                      │
                      ▼
            cadence dispatch <ISSUE> --to dev-n --note <kickoff> --reply-to qa-1
                      │
                      ▼
      dev-n: branch, code, tests, PR, qa note ──► qa-1 (automatic: result routes to qa-1)
                                                     │
                     blocked ◄───────────────────────┤ review: gates, read, hands-on
       round-N kickoff straight back to dev-n        │
                                                     ▼ pass (verdict + risk class)
                                               ops-1: freeze, combined gate
                                                     │
                          class auto ────────────────┼──────────── class human
                          ops-1 merges               │             APPROVAL NEEDED → PM → operator
                                                     ▼
                           post-merge: rebuild, board, daemon restart (when-idle),
                           tracker done, worktree finish, "main moved" notices, smoke
                                                     │
                                                     ▼
                           one-line report to PM ──► retro and memory proposals (2 reviews, PM accepts)
```

## Routing: who sends what, how

| From | To | What | Command |
|---|---|---|---|
| PM | Researcher | A question with the decision it feeds | `cadence send rsch-1 --reply-to fable-cc --text "…"` |
| PM | Architect | A goal plus research notes | `cadence send arch-1 --reply-to fable-cc --text "…"` |
| PM | Developer | Kickoff for one delivery outcome, with related issue links | `cadence dispatch <ID> --to <dev> --note <kickoff> --reply-to qa-1` |
| Developer | QA | Completion (automatic) and the qa note | `cadence message result …`; `mail-post.sh qa-1 qa …` |
| QA | Developer | Round-N kickoff | `cadence send <dev> --reply-to qa-1 --text "read <note> — …"` |
| QA | DevOps | Pass | `cadence send ops-1 --reply-to fable-cc --text "merge-ready #<pr> head <sha> risk <class> verdict <note>"` |
| QA | PM | Escalation, SECURITY | `cadence send fable-cc --text "…"` |
| DevOps | PM | Approval request (class `human`), merged digest (class `auto`), smoke failure | `cadence send fable-cc --text "…"` |
| PM | DevOps | Operator approval | `cadence send ops-1 --text "OPERATOR APPROVED #<pr> at <full sha>"` |

Managed agents (`qa-1`, `ops-1`, `rsch-1`, `arch-1`) report by the final
message of their turn; pty developers report with `cadence message result`.

## Artifacts: where things live

| Artifact | Location | Written by |
|---|---|---|
| Goal, roadmap, principles | `docs/CHARTER.md` | PM |
| Roles and briefings (canonical) | `docs/TEAM.md`, `docs/roles/*.md` | PM |
| Briefings the agents load at runtime | `~/.local/state/cadence/roles/*.md`, a copy of the canonical files | PM syncs them after every merge that changes `docs/roles/`. There is no `team.yaml` yet: CAD-76 delivered ADR-0001 only, no project has the file, and the only readers are best-effort `roles.pm.alias` lookups for report and relay notices. CAD-338 plans `PROJECT.md` in its place |
| Merge risk classes | `docs/roles/risk-classes.md` | PM, operator decides changes |
| Issues, epics, comments | tracker `~/pm/<project>/<ID>/` via `cadence issue` | everyone through the CLI |
| Research notes | tracker artifact on the issue (`cadence issue attach`) | Researcher |
| Design decisions | `docs/adr/NNNN-<slug>.md` via PR | Architect |
| Kickoffs, qa notes, verdicts | `/var/www/agent-notes/` via `note-publish.sh` | PM, developers, QA |
| Project memory (lessons) | tracker `memory/` via `cadence memory` | proposed by any `pm`/`worker` agent endpoint; reviewed by two others; finalized by a `pm` endpoint (see Memory acceptance) |
| Review reports, flake ledger | `<state>/reviews/` | `cadence review` |

## Memory acceptance

The gate is code (`src/memory/`); the full contract is
[BOARD.md, project memory](BOARD.md#project-memory--cadence-memory).
Authority comes from the endpoint the daemon authenticates, not from a
team role or an alias:

1. **Propose.** Any agent endpoint with runtime role `pm` or `worker`.
   The daemon derives the proposer from the Unix socket peer: a pty
   pane, or a managed endpoint by its enrollment (CAD-381). An inbox
   such as `fable-cc` has no process to authenticate, and a shell outside
   every agent tree (the operator's included) has no agent identity, so
   neither can propose, review or finalize.
2. **Review.** `cadence memory review <slug> --operation accept --verdict
   pass|revise --digest <sha256> --evidence "…"`. Two distinct `pm` or
   `worker` endpoints, neither the proposer nor a contributor, must pass
   the same semantic digest with non-empty evidence. One `revise` blocks
   the cycle; an edit to the claim or its scope changes the digest and
   voids earlier receipts.
3. **Finalize.** `cadence memory accept <slug>` from a runtime-`pm`
   endpoint that did not propose the lesson. `memory reject` is PM-only
   too. Review receipts alone never make a lesson retrievable. QA
   supplies a receipt; the PM's acceptance is a separate act.
4. **Verify.** `memory review --operation verify` on an accepted lesson
   opens a fresh cycle that needs two new receipts and a PM `memory
   verify`. The lesson is withheld while that cycle is open. Only a
   finalized verify stamps `verified_at` and clears `stale:`.
5. **Retrieve (CAD-203).** An eligible lesson is injected labelled
   `verified <date>` inside the project's freshness window (default 30
   days, `memory: {stale_days: N}` in `project.yaml`), `unverified`
   otherwise; age alone never withholds. Only a `stale: <why>` mark in
   the lesson's frontmatter withholds it. No citation re-check or
   contradiction check runs yet.
6. **Deliver.** Lessons ride plain `cadence dispatch` kickoffs as a
   `Lessons:` file, and project `rule`s appear in briefings. `--job`
   kickoffs are daemon-templated and carry no lessons (CAD-194).

## Guardrails (each one learned from an incident)

1. Review happens in a detached checkout, never in the author's worktree.
2. Net-deletion check before anything else on a rebased PR.
3. Verdicts name the exact head SHA and are void once the head moves; `scripts/qa-verdict.sh` refuses a stale head and merges use `--match-head-commit`, so a push after the verdict cannot land unreviewed.
4. A PR entering the merge queue is frozen by message naming the exact SHA.
5. When main moved, gate the merge result, or one combined tree for several PRs, and check that main's tree equals the gated tree after merging.
6. A failing test is rerun isolated on the PR tree and on main before anyone blames the PR.
7. One full integration suite per host, under `CADENCE_SUITE_LOCK`. The
   optional CAD-173 nextest wrapper verifies the reviewed executable
   digest, takes this lock before launching the process-per-test harness,
   and forces zero retries. A review child receives an explicit outer-held
   marker and never nests `flock`. The activation candidate records JUnit
   testcase names, outcomes, counts and durations; a missing or empty report
   is blocking evidence, and the full and isolated commands must use the same
   exact-filter backend. Human approval still gates this config change.
8. Never paste other processes' command lines or tool output verbatim into PRs, issues or notes; scan for secrets before any outward write.
9. A pty pane that probes idle while its message is still running has stopped; a numbered menu is an approval prompt, not work.
10. Git commands that may open an editor run with `GIT_EDITOR=true`.
11. Nobody kills processes they did not start, and nobody uses `--force`, without operator approval.
12. A read-only or `--dry-run` command writes nothing at all, including handoff and report files.
13. A destructive sweep is scoped by the flags it was given: `--project` and friends are forwarded to everything the command calls.

## Capacity

- Host ceiling: at most five developer lanes plus one review suite. The smaller
  delivery working set above is the default; unused capacity is not a reason
  to exceed review capacity.
- Disk under 20 GiB free: DevOps deletes `target/` in worktrees of merged issues first.
- Idle agents with nothing queued are stopped (resumable); reserves are resumed on demand.

## Model and effort budget

| Role | Model | Effort | Why |
|---|---|---|---|
| QA, Researcher, Architect | Opus | high | Judgement and adversarial reading find the bugs gates miss |
| DevOps | Opus | medium | Procedure-heavy, low ambiguity |
| Developers | Devin default | n/a | Long autonomous turns in a terminal |
| Subagents for code reads | Opus | default | Always pass the model explicitly |

Daemon-wide model defaults are a host fallback for **new** registrations.
Precedence is explicit launch model, then the team-role default, then the
provider baseline, then the provider-native default. `--team-role` (`ops`
stored as `devops`) chooses that lookup only. Runtime `pm` / `worker`
authorization is unchanged, including memory finalization. Resume keeps
the model saved on the agent. Codex, Claude, and Cursor accept a launch
model; Devin does not, and the settings page says so. The board Settings
tab edits the daemon document; it is not scoped to the selected project.
Recovery is a provider or role reset in that document, or an explicit
next-launch model set/clear on one agent. Neither rewrites other agents.

## Skills per role

Skills are loaded from `~/.claude/skills` (Claude agents) and named in
each briefing.

| Role | Skills |
|---|---|
| PM | `cadence`, `agent-handover`, `to-tickets`, `triage`, `planning-with-files`, `wayfinder` |
| Researcher | `research`, `firecrawl`, `cf-crawl`, Context7 and DeepWiki tools, web search and fetch |
| Architect | `grilling`, `to-spec`, `to-tickets`, `codebase-design`, `domain-modeling`, `improve-codebase-architecture`, `wayfinder` |
| Developer | `tdd`, `implement`, `diagnosing-bugs`, `resolving-merge-conflicts`, `code-simplifier`, `cadence` |
| QA | `code-review`, `security-assessment`, `agent-browser` (UI hands-on), `cadence`, `agent-handover` |
| DevOps | `cadence`, `agent-handover` |
