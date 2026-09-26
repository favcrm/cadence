# Projects UX consolidation

Inspected in Mac Chrome on 2026-09-26: all-project Issues, Cadence Issues in List/Board views, Epics, Milestones, Workflows (empty), and Context (loaded). No workflow was executed and no issue state was changed.

| Surface | Operator question | Current problem | Resulting direction |
| --- | --- | --- | --- |
| Projects overview | What is happening across projects? Where should I look? | Opens a large combined issue inventory; runtime warnings dominate. | Project summaries with active work, reviews, blockers and direct links. |
| Project overview | What is this project working toward, and what needs attention? | No entry point combining outcomes and current work. | Compact real-data summary, active epics and prioritized work, links to specialist tabs. |
| Issues | What is being worked on, and which ticket should I inspect? | Agent inventory, recovery instructions and dense metadata compete with tickets. | Compact expandable team state, readable list/board, focused card metadata. |
| Epics | Which outcomes are progressing or at risk? | Completed epics lead; long blocker lists dominate. | Active outcomes first, completed work opt-in, expandable blocker reasons. |
| Milestones | How far are we toward a release goal? | Repeats issue-level blockers; technical weight and unlisted labels dominate. | Keep weighted progress truthful, explain it, collapse detailed reasons. |
| Workflows | What repeatable work can I start? | Empty state leads with storage paths and a CLI. | Purpose-led empty state, retain setup instructions in details. App-provided workflows keep their app links. |
| Context | What should I read to understand this project? | Manifest, HEAD and retrieval internals precede documents. | Reading list first; provenance and retrieval diagnostics remain inspectable. Never hide actual source errors. |

## Information hierarchy
Projects is a portfolio entry point. Selecting a project opens its overview. Issues, Epics, Milestones, Workflows and Context remain distinct destinations. Home remains the global Master conversation and agent journal; Projects does not duplicate that conversation.

Overview counts come from issue records, excluding epic containers from task totals. “Doing” is a tracker status, not proof of a live running agent. Progress remains the existing weighted server-derived epic/milestone progress; no fabricated aggregate percent or delivery date. Projects with no issues show an honest empty state. Unknown/unavailable data is not shown as zero or healthy.

Preserve existing issue deep links, board/list preferences, filters, read-only behavior, project isolation, and all backend security contracts. Existing issue bookmarks with a view query still open Issues. No production rollout is part of this review.

## Navigation polish
The header shows the project breadcrumb; section links share the content left edge and mark the active destination with an underline. Links preserve browser Back navigation. Targets are 44px tall and wrap on narrow screens. Connection, refresh, theme and update controls share the header utility area and the Hugeicons set. The update control opens Reload/Later on demand, closes with Escape or an outside click, and no longer floats over the page by default.
