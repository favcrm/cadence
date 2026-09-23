# Cadence project workspace and verified learning audit

Date: 2026-09-21. Scope: the user's Cadence UI at the Tailscale URL, inspected through its equivalent local service http://127.0.0.1:3010. No UI actions that mutate issues, memory, agents or approvals were taken. Browser session cadence-ux-audit uses AWS-local Chrome; the default remote backend cannot inspect this host's loopback.

Runtime source: ceee1cfed052f08ede1180880bb7c7debde75ec1. Current merged source: b59e0384cbd248f642b8b3e3cd69ebf839847e34. The relevant UI components remain present on current main; upgrading alone does not replace the static Plan page or raw issue reader. Screenshots capture desktop 1280px and mobile 390px views.

## Design language

- Audited surface: selected-project work management, including Overview, Board, issue detail, Agents, Memory and Plan.
- Design sources: rendered UI and traced ui/src/App.tsx, components/{Overview,Board,FilterBar,Drawer,Md,Memory,Plan,Agents}.tsx and styles.css at the runtime/current revisions. The Plan page's references to a sister project's design system are historical copy, not proof of another current governing source.
- Documented decisions: dark ink surfaces, teal interaction accent, IBM Plex prose/mono typography, sidebar/topbar, overlay detail; preserve these rather than invent a redesign.
- Governing owners and consumers: App owns selected project/tab and supplies project to Board/Memory; Overview and Agents receive global payloads; Plan receives no project/context input. Drawer already uses Md for comments and edit preview, but renders the primary issue file in preformatted raw form.
- Explicit exceptions: None documented.

## Findings

| # | Problem | Evidence | Proposed change | Scope | Confidence |
| --- | --- | --- | --- | --- | --- |
| 1 | Selecting Cadence does not scope the overview/agent context, so unrelated pending work competes with the chosen project. | URL is ?project=cadence and sidebar highlights Cadence, while Overview shows other projects' merge requests and agent exceptions; Cadence Board's Runtime strip shows AOS/FAV work. App supplies no project to Overview/Agents and Board consumes the global agent payload. Screenshots overview, board and agents. | Make project selection consistently scope operational presentation; keep host-global/unassigned items in an explicitly labelled separate section. | App, Overview, Board runtime strip, Agents; resolve backend membership evidence where needed rather than guessing from aliases. | High |
| 2 | Primary issue content is harder to read than its comments: users first get YAML and horizontally clipped source in a narrow drawer. | CAD-238 screenshot shows raw frontmatter dominating the reader; Drawer.tsx:533 uses pre with !whitespace-pre for file/body, while comments already use Md at line828. User directly reports hard-to-read content. | Render the issue body with the existing Md component by default and retain raw frontmatter/source behind an explicit source view. | Drawer and its existing Markdown presentation; preserve source and editing semantics. | High |
| 3 | Plan prose overflows a phone viewport. | At390px viewport, document scrollWidth=678px; the first grid child is662px wide, and ordinary prose is clipped in the screenshot. Plan.tsx:28-29 grid/document child lacks a shrink constraint while nested wide content establishes intrinsic width. | Constrain the document grid child to min-width:0 and confine wide code/tables to their own scroll containers. | Plan document layout; verify390px and desktop. | High |

## Improve first

Fix selected-project scope first: it determines which pending items users believe they own. Readability is the next small UI change. Neither correction requires an unrelated visual redesign.

## Product direction requested by the user

These are proposed requirements, separate from the three verified presentation findings above.

**A bounded action queue.** A read-only overview API snapshot returned53 Needs me entries across nine categories, including10 missing-verdict PRs,9 stalled agents and9 newly unblocked issues; these are observations, not53 confirmed human decisions. Replace the long undifferentiated feed with grouped unresolved causes: Needs your decision, Team handling, Waiting on dependency, and Resolved history. Each row names project/outcome, owner, reason, next action and observation age. Correlate multiple symptoms of one task/agent; do not hide unresolved work or call acknowledgement resolution. Keep commands in details instead of making clipped shell text the primary UI. Board filters should expose selected filters compactly, with the full owner/component list on demand; retain a table view and a short working set beside the full backlog. Existing scopes CAD-82/137/225/226.

**Readable evidence.** Give issues/specs/memories a document reader, with summary and acceptance first, then evidence/review/activity. Expand long reflections on demand. Use named document links instead of filenames as the only description. On Agents show a compact current-job row; expand full error text and quota provenance. Preserve unknown values honestly, but avoid repeating the same multi-line telemetry explanation in every row. Existing document rendering owners should be reused.

**Project context and plan.** The current Plan component is hardcoded Cadence implementation documentation: Files are the truth, Issue folder, Architecture, API surface, and old iterations. It neither reads the selected project's documents nor verifies a plan against them. Move that content to Cadence help. A project workspace should expose Scope, Architecture/module map, Design specs, ADRs, Development/QA, Roadmap and Memory, with document owner, source revision and current/superseded status.

For each planned outcome require links from goal -> scope/spec -> affected modules -> acceptance -> validation -> review evidence. The planner reads the registered project's context manifest first, states missing/conflicting sources, records source revisions and planned paths, and separates documented facts from assumptions. QA checks both implementation and compliance with those sources. Changed source documents invalidate only affected plan evidence and trigger revalidation. Human escalation is for consequential unresolved scope/authority conflict, not every routine task. Do not load every project document into every agent prompt.

Cadence already has docs/START-HERE.md, CHARTER.md, ARCHITECTURE.md, ADRs and design/DEVELOPMENT-TEAM.md in merged source. The last is explicitly a proposal. A graph can index these later; it must not become the source of product intent. Consolidate this direction under CAD-224 and CAD-65/67 rather than open a ticket per document or UI tab.

## Two-reviewer memory contract

The user clarified REVIEW AND IMPROVE, not delete memory. Inventory at inspection:11 proposed,0 accepted,0 load errors; lint passes. Lint and author confidence do not establish correctness. The UI currently offers accept without showing two independent review receipts; the current schema stores no two-reviewer quorum.

Proposed lifecycle: capture one scoped reusable claim at delivery/incident boundaries -> source/evidence and applicability attached -> two independent reviewers -> accept unchanged reviewed content -> retrieve by project/role/task -> record usefulness or contradiction -> revalidate/supersede with retained history.

Both reviewers must be distinct authenticated identities and neither can be the author. Require evidence of source checking/reproduction from reviewer A and applicability/counterexample checking from reviewer B; both must decide on the entire same version. Bind receipts to a digest of the claim, rationale, source/evidence, scope and applicability/version fields (excluding mutable status/review timestamps). Two receipts for a stale revision do not count. Changes to reviewed content reset the quorum. Disagreement, missing evidence, rejection or stale applicability leaves the claim uninjected. No timeout auto-accept. The approving actor relays the two decisions; editing while accepting cannot bypass re-review. Apply the rule to accept, verify/revalidation and supersede across CLI/API/UI, under lock with revision-conflict checks.

Show Proposed (0/2,1/2), Verified (2/2), Needs recheck, Rejected/Superseded, with reviewer identities, evidence and applicable version. Only accepted, relevant and currently valid lessons enter PM planning, developer kickoff/resume and QA/Ops context. Retrieve using planned modules/paths before the first commit exists. Raw transcripts, current queues and quota snapshots are operational evidence, not permanent memory. Content review is currently manual; stronger enforcement belongs to existing CAD-191/192/193 and retrieval parity to CAD-194/195.

## Memory audit receipts

Two independent non-author agents, /root/luna_watchdog (A) and /root/luna_dev_acceptance (B), completed assessments of all11 stored files. A's full exact-file hashes match the captured manifest. B reported matching hash prefixes; its report is a manually attributed review receipt, not an authenticated product quorum. No memory changed during review. A's final report has6 needs-revision and5 supported; B reports7 supported,3 needs-revision and1 unverified.

| Stored slug | Reviewer A | Reviewer B | Consolidated disposition |
| --- | --- | --- | --- |
| a-plan-that-cites-bare-risk-clas | Needs revision | Supported | Hold: resolve overbroad un-recheckable claim and historical policy evidence. |
| an-acceptance-check-of-the-shape | Needs revision | Needs revision | Revise shell exit-status/always-passes wording. |
| browser-agents-cannot-reach-loop | Needs revision | Needs revision | Revise: current local-browser counterexample disproves universal claim. |
| delivery-retry-budget-survives-gate-waits | Needs revision | Needs revision | Revise stale pending-implementation/validation wording against landed fix. |
| empty-git-diff-output-on-this-ho | Needs revision | Supported | Hold: separate historical wrapper defect from current documented mitigation. |
| env-target-dir-can-break-fixture-paths | Needs revision | Supported | Hold: tighten invocation/test scope and source revision; do not unset the variable in tests of its intended precedence. |
| filtered-tests-need-exact-nonempty-receipts | Supported | Supported | Two content reviews support unchanged claim; not yet accepted in product. |
| qa-verdicts-expire-on-head-change | Supported | Supported | Two content reviews support unchanged claim; not yet accepted in product. |
| queued-delivery-is-not-execution | Supported | Supported | Two content reviews support unchanged claim; not yet accepted in product. |
| reading-a-pr-with-git-checkout-r | Supported | Unverified | Hold: B could not independently establish original scratch evidence; fresh isolated reproduction can resolve. |
| shared-structs-need-all-target-validation | Supported | Supported | Two content reviews support unchanged claim; not yet accepted in product. |

Result:4 records supported by both reviewers,6 need correction or resolution of a reviewer disagreement,1 lacks sufficient independent evidence for the second reviewer. This is not a majority vote: a single unresolved objection blocks promotion. All11 remain proposed and uninjected; none was deleted or silently rewritten. Reviewed exact-file hashes are in20260921-evidence/memory-review-manifest.json; attributed reports are alongside it. Revised claims need both reviewers to inspect the new version.

Reviewer B also independently confirmed that the proposed project-context manifest is not implemented in the current Project schema/API or bounded role-specific dispatch context. Existing source docs are useful starting points but are not automatically consumed by the Plan UI.

Already-observed counterexample: the proposed universal claim that browser agents cannot inspect local Cadence is contradicted by this live walkthrough. The useful lesson is to choose a browser backend that can reach the target; replacement wording still requires two reviews. Original content is retained.

A second counterexample is the shell lesson's assertion that command substitution discards the no-match exit status and the example always passes. Root reproduced count=0, assignment exit=1, subsequent nonempty-string check exit=0 when execution continues; under bash -e the assignment exits1 before the check. The narrower lesson (numeric string nonemptiness is not a match-count assertion) remains useful, but the original wording needs revision. A first reviewer initially marked it supported; the counterexample was returned for correction. Independent review is valuable only when it can overturn a plausible claim.

## Evidence captures

- /tmp/cadence-ux-overview.png
- /tmp/cadence-ux-board.png
- /tmp/cadence-ux-issue.png
- /tmp/cadence-ux-memory.png
- /tmp/cadence-ux-memory-detail.png
- /tmp/cadence-ux-plan.png
- /tmp/cadence-ux-mobile-plan.png
- /tmp/cadence-ux-agents.png

Persistent copies are in design-plans/20260921-evidence/ alongside the exact-memory file hash manifest and independent review receipts.

## Acceptance examples for subsequent implementation

- Select project A, then B: project-owned queue, agents, docs and memory change together; explicitly global host alerts remain labelled global. Unassigned ownership is shown as unknown, never guessed from an alias.
- Open an issue with a long title, frontmatter and Markdown acceptance: the normal reader shows content and acceptance without horizontal prose scrolling; source is still available explicitly. At390px and1280px, only wide code/table regions may scroll horizontally.
- Open Plan for a project with registered docs: show actual scope/design sources and revisions, linked outcomes and acceptance. Missing docs produce an explicit missing-context state, not Cadence's own historical implementation page.
- Change a referenced architecture/spec revision: affected plan evidence becomes needs-recheck; unrelated accepted tasks are not blanket-invalidated.
- Two different reviewers approve identical memory content: eligibility becomes2/2. One reviewer twice, an author review, one rejection, unavailable evidence, changed scope/body/source or an old content hash cannot satisfy the gate. A superseding lesson needs its own two reviews. A stale timestamp refresh cannot restore eligibility by itself.
- A dispatcher for project A cannot inject project B lessons, unreviewed proposals or contradicted content. It can retrieve relevant accepted guidance using planned paths before any code commit exists and report why each lesson was included or withheld.

No product implementation, runtime upgrade, memory acceptance or monitor activation is claimed by this audit.

## Process reflection

This walkthrough directly disproved a high-confidence environmental memory, and a short counterexample corrected an initially favourable review of a shell claim. Capture applicability and invalidation conditions at proposal time; require reviewers to try falsifying the claim, not just find a supporting passage. Keep missing evidence visible and unaccepted. Batch curation into bounded reviews with explicit outcomes and a stop condition; do not let a proposal queue turn into unlimited historical research. Product validation should include this same browser workflow and project/document selection, not only source inspection or a passing memory lint.
