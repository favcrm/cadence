# CAD-1969 light-mode reference

The owner accepted the illustrated visual direction on 2026-10-31. The preserved
synthetic reference is `cad-1969-light-mode-reference.html`, the source of the
preview published at `/20261031-101821/cadence-light-mode/`. Its original header
says draft because it predates that acceptance. Its simulated messaging and
disabled controls are not product requirements.

| Reference | Product owner |
| --- | --- |
| White canvas, silver navigation and neutral selected row | `screen-ui/src/theme.css`, `src/ui/Sidebar.tsx`, `src/ui/NavList.tsx`, `src/styles.css` |
| Assistant conversation, composer and to-do/updates | `src/features/home/Home.tsx`, `NeedsRail.tsx`, `src/styles.css` |
| Project metrics, cards and actions | `src/features/projects/ProjectsOverview.tsx`, `src/styles.css` |
| Explicit light/dark/system preference and frame propagation | `src/lib/theme.ts` and app frame host; unchanged by this visual change |

The real board retains its live data, read-only and error states, additional
navigation, controls and existing project actions. Those differ deliberately
from the illustrative preview. Embedded apps own their own document styles;
the reference does not authorize redesigning them.
