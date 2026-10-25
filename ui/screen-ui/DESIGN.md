# Cadence screen UI design

Design guidance for `@cadence/screen-ui` and app-owned screens. This file ships
inside the package as `DESIGN.md`; the source lives at `ui/screen-ui/DESIGN.md`
in the Cadence repository. Component exports and shipped CSS define the actual
API. Guidance here does not grant host capabilities or introduce new exports.

## Character

Compact, quiet, operational. Put records and the next useful action first.
Use flat surfaces, restrained borders and consistent alignment rather than
cards around every region. Match the core workspace instead of creating an
app-specific visual language. Support light and dark themes equally.

Keep a page title, search/filter controls, records and pagination. Remove
introductory copy that merely restates the title or explains an obvious control.
Retain information needed to make a decision: permissions, destructive effects,
validation, unsaved work, loading failures and the meaning of partial counts.

## Ownership and reuse

- Import `@cadence/screen-ui` primitives and its stylesheet. Reuse `Button`,
  `Select`, `Field`, `DataTable`, `DrawerShell`, loading, notice and empty-state
  components before adding app-specific equivalents.
- Theme, typography, primitive behavior and shared drawer structure belong to
  this package. Product query state, routes, business rules and feature-specific
  compositions belong to the app.
- A preview uses the same app components and styles as the installed screen.
  Preview host chrome is not a second design system.
- Scope density overrides to a named composition, not global `.btn`, `.field`
  or `.select-trigger` selectors. Do not copy core-shell CSS into app bundles.
- Change shared primitives in their source and use the normal package release
  process. Do not patch `node_modules` or replace a pinned archive in place.

## Theme and typography

Use semantic CSS variables from `src/theme.css`: ink levels for surfaces and
text, `--color-edge` for boundaries, `--color-accent` for active/focus states,
`--color-on-accent` for text on accent, and the status tokens for meaning.
Use the scrim token for overlays. Do not hardcode a light-only background or
use a status color as decorative branding. Theme selection supports explicit
`data-theme="light"` / `"dark"` and a system fallback.

Use `--font-sans` (IBM Plex Sans with system fallbacks), and `.num` /
`--font-mono` (IBM Plex Mono) for tabular numbers. Use the existing type scale:

| Token | Purpose | Size |
| --- | --- | --- |
| `--text-micro` | Small supporting metadata | .6875rem |
| `--text-label` | Control labels | .75rem |
| `--text-table` | Dense tabular content | .78125rem |
| `--text-secondary` | Secondary text | .8125rem |
| `--text-body` | Body text | .875rem |
| `--text-cardtitle` | Local headings | .9375rem |
| `--text-section` | Section titles | 1.125rem |
| `--text-drawer` | Detail titles | 1.25rem |

Keep metadata secondary without making essential content faint or tiny. Use
`.5rem` card and `.25rem` control rounding where the shared styles supply it;
no new radius scale or decorative gradients. Retain shared reduced-motion
behavior and short, restrained state transitions.

## Compact list composition

The accepted CRM list pattern is a composition, not a new SDK component:

| Relationship | Desktop / fine pointer | Narrow / coarse pointer |
| --- | --- | --- |
| Search, filter, action and pager control height | 28px | 36px |
| Horizontal control gap | 8px | 8px |
| Toolbar to table | 12px | 12px |
| Table to paginator | 12px | 12px |

Align search, filters and primary action on the same row when space permits.
Let search grow; keep the primary action at the trailing edge. Wrap against the
workspace's available width, not only the browser width. On narrow layouts,
allow full-width search and a second filter row without horizontal page scroll.
A wide data table may scroll within its own region.

The vertical gaps are measured between visible regions. An empty refresh-status
live region must not become an extra grid row; a meaningful loading/error state
may occupy space. Keep row heights, control baselines and paginator alignment
stable while data refreshes.

These heights are not global primitive defaults. The shipped stylesheet currently
uses 36px `.field`, 28px `.btn`, 24px `.btn-sm`, 32px `.select-trigger` and
28px `.select-sm`, with responsive adjustments. Some component size comments
lag that CSS. Check the rendered control rather than assuming size names match
across primitives; do not resize every form field to achieve compact lists.

## Search and pagination

Search must have an accessible name independent of its placeholder. Keep the
query visible; reset pagination when the search or filter changes. Preserve
normal keyboard use of inputs and selects. Empty results should identify the
active condition and offer a useful way back, not look like a loading failure.

Place the count or range at the leading edge of the paginator, and Previous,
page selector and Next together at the trailing edge. Disable impossible moves
and prevent duplicate navigation while busy. Label the selector accessibly.

Do not invent totals or page destinations. A cursor API can offer visited pages
and discover the next one; an array-backed list can select among its loaded
pages. Label loaded-record counts and visited-page navigation honestly.
`ListPager` in CRM is app-owned; it is not exported by `@cadence/screen-ui`.

## Copy, actions and tooltips

Prefer short action labels: “New customer”, “Save”, “Cancel”. Use one obvious
primary action per context. Keep secondary and destructive actions visually
subordinate. Do not replace a clear label with an unexplained icon to save space.

Move optional explanations of familiar controls into tooltips where appropriate.
A tooltip supplements an accessible name; it must not supply the only name.
Expose it on keyboard focus as well as hover, keep it dismissible, and do not
rely on hover-only content on touch screens. Use an existing accessible tooltip
implementation when available; `title` alone is not an accessible help system.
Keep required instructions, validation and consequences visible near the action
or field. Never hide permission or delivery-risk information in a tooltip.

## Drawers and host boundaries

A drawer inside a sandboxed app frame cannot cover the board. A React portal
within that document still stays inside the frame; accessing `parent.document`
is not a supported integration.

For a full-board modal drawer, core owns the fixed panel, the single scrim,
trusted dismissal, focus containment and background isolation. The page stays
in its normal layout container: opening or closing must not change its width,
position, toolbar wrapping or scroll. Do not promote the whole list frame into
drawer-sized bounds, hide or copy the list, or compensate with frozen geometry.
Only the drawer appears above the page; reserve scrollbar space during scroll
locking so the page does not shift.

The independent app contract is `screen-surfaces.v1`, with host-assigned
`primary` and `drawer` roles. The primary frame stays mounted in place; a
separately admitted child frame renders only the existing app-owned detail
content. Reuse `screen.drawer.open` / `screen.drawer.close` only when negotiated
and advertised. These are host-local presentation capabilities, not records
permissions. Core retains the parent action handle; native admission binds one
child to that live, consumed parent and refuses stale, foreign or nested
parents. No parent handle belongs in app messages, markup or a copied renderer.

The app owns detail content, tabs, forms and pinned header/footer structure.
Its drawer role fills the child frame and suppresses its local scrim and
duplicate close control. Desktop uses a full-height right panel; narrow
layouts use the viewport. Preserve the child frame and drafts across viewport
and same-scope detail navigation. Closing retires only the child, refreshes
primary data in place and restores the opener or heading without a reopen loop.
Keep menu-first, edit-second, drawer-last Escape handling.

An older or unauthorized host must not be treated as if it granted this
contract. Use its supported outlet presentation without blind host calls.
`screen-presentation.v1` remains the separate legacy presentation contract;
its whole-frame promotion is not the stable-list pattern for new drawers.

`DrawerShell`'s existing `scrim="board"` option only spans its own document.
It is not a host presentation request. A deliberately nonmodal outlet panel is
a different interaction: do not claim it blocks the whole board or layer a
second overlay behind it.

## Verification before adopting a change

Inspect the actual screen in both themes, at desktop and narrow widths and with
keyboard focus. Check aligned controls, equal list gaps, wrapping, real empty
and busy states, and honest pagination boundaries. For drawers, sample the
page's dimensions, screen position and scroll throughout opening and closing,
not just the settled panel. Verify one scrim, full-height geometry, internal
scrolling, trusted close/scrim/Escape, focus restoration, navigation cleanup,
primary-frame continuity and child-draft preservation. A screenshot alone
does not establish bridge admission or backend authority.

Package assembly is `ui/scripts/package-screen-ui.mjs`. It includes this file
in the source provenance and archive artifact inventory. A working-tree archive
is development evidence, not a published SDK release or an installed consumer
upgrade. See `docs/guides/app-toolchain.md` in the source repository for the
separate bridge receiver and frame-kit delivery contracts.
