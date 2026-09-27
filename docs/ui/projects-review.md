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
| Context | What do people and agents need to know to deliver this project? | Manifest, HEAD and retrieval internals precede documents; project knowledge has no editing workspace. | Project-scoped Wiki explorer and editor; repository references stay available in a disclosure. |

## Information hierarchy
Projects is a portfolio entry point. Selecting a project opens its overview. Issues, Epics, Milestones, Workflows and Context remain distinct destinations. Home remains the global Master conversation and agent journal; Projects does not duplicate that conversation.

Overview counts come from issue records, excluding epic containers from task totals. “Doing” is a tracker status, not proof of a live running agent. Progress remains the existing weighted server-derived epic/milestone progress; no fabricated aggregate percent or delivery date. Projects with no issues show an honest empty state. Unknown/unavailable data is not shown as zero or healthy.

Preserve existing issue deep links, board/list preferences, filters, read-only behavior, project isolation, and all backend security contracts. Existing issue bookmarks with a view query still open Issues. No production rollout is part of this review.

## Navigation polish
The header shows the project breadcrumb; section links share the content left edge and mark the active destination with an underline. Links preserve browser Back navigation. Targets are 44px tall and wrap on narrow screens. Connection, refresh, theme and update controls share the header utility area and the Hugeicons set. The update control opens Reload/Later on demand, closes with Escape or an outside click, and no longer floats over the page by default.


## Project Context workspace

Context now embeds the shared Wiki under `projects/<project-key>/`. The explorer, breadcrumbs, search, edit, upload, and history navigation stay in the project Context tab; URLs retain file/mode/search and an open issue drawer. Changing projects or tabs clears file selection. A root entry opens README.md when present; returning to the Context breadcrumb opens the folder view.

Wiki and Context share `FileExplorer`, `PagePane`, the existing editor and the same layout styles. Both fill the available viewport below their navigation, with scrolling inside the explorer and content pane. Context adds its project scope and repository references rather than a second layout. Desktop shows the explorer beside a flat document reader with one toolbar; smaller screens offer the same expandable file list in both destinations. Duplicate page titles, document cards, technical revision labels and the extra Context frame are removed. The editor fills the remaining pane space. Content stays left-aligned; prose has a readable line length while the workspace uses the full width.

Every mode also uses the shared `WikiToolbar`: reader, folders, attachments, editing, search, history and upload. Search has a visible submit action, a readable field at narrow widths, and 44px mobile search/filter targets. The editor shows unsaved/saving state, keeps the text fixed during a save, and sends only one request when keyboard saves repeat. “Back to page” describes leaving with a retained draft. A save that finishes after leaving does not redirect the user away from their newer destination. Markdown and Preview labels sit above their respective panes, including when stacked on mobile.

There is one store and one editor. Pages are Markdown in the versioned Wiki vault; attachments use the existing blob store. The repository manifest and pinned Git documents remain read-only behind Repository references, collapsed inside the reading pane. No automatic migration, copy, or root creation occurs. A missing root offers an explicit Create project context action; read-only clients cannot use it. Reads and writes still use the existing session and daemon allowlist. An unsigned Wiki response shows a sign-in state rather than weakening the HTTP gate.

API adaptation now reads the daemon's actual text/blob kinds, search matches, and commit-log history. New pages use the absent revision token `none`, move uses `{from,to}`, and a successful RPC envelope carrying `conflict: if_rev` rejects the save so a draft is preserved. The current history endpoint supports a log only; unsupported compare/restore controls are hidden. Text downloads produce Markdown, blob deep links use listing metadata, and read-only editors block typing. Drafts preserve empty edits and the last change when leaving before debounce.

Validation on 2026-09-27: UI typecheck, full UI test script, production build, and git diff --check pass. New DOM interaction tests use actual daemon response shapes and exercise default pages, scoped navigation, edit/draft recovery, successful/refused saves, read-only mode, search, history, project switching, invalid paths, sign-in, and missing-root behavior. They also cover the shared Wiki reader, current-file announcement and mobile file disclosure. Local Chrome visual checks cover Wiki and Context, light/dark desktop, 390px and 320px mobile, expanded/collapsed files, the editor, repository references, and the existing Home viewport. Panes fit the available viewport with no horizontal overflow. Mac CDP remains unavailable, so the requested Mac verification is pending. Earlier Mac checks listed above apply to the previously reviewed Home/Projects/milestone surfaces.

The repeated-save and late-redirect regression checks were each demonstrated failing before their fixes, then passed with the full UI script. Visible search submission, project scope, and preservation of newer drafts during an older save are also covered in DOM interactions. Local Chrome exercised search → page → history → edit → save with delayed fixture responses and repeated Ctrl+S; it restored the fixture text afterward. The shared button hover rule now excludes disabled controls so their surfaces remain consistent across themes.

Live-data preview: http://ip-172-31-1-32.tail9fcf30.ts.net:3167/projects/cadence/context
Isolated Context demonstration: http://ip-172-31-1-32.tail9fcf30.ts.net:3169/projects/roadmap-demo/context

The demonstration clearly labels fixture content, allows only in-memory Wiki edits, refuses other writes, and uses the existing isolated roadmap tracker for project navigation. Its untracked Vite config is excluded from the production change. It is a review surface, not evidence of an authenticated production write. Production rollout remains with its existing owner after independent review.
