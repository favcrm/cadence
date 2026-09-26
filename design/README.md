# Design kit

Every mockup starts from the app's real design tokens, so it looks native
and costs little to build. The kit is three files:

- `tokens.css` — generated; the theme from `ui/src/styles.css` as CSS
  custom properties: `:root` (dark defaults), `prefers-color-scheme`
  light, and `[data-theme="light"|"dark"]` overrides. The `[data-theme]`
  rules are not tied to `:root`, so a mockup can theme a subtree
  (`<section data-theme="light">`) without flipping the page.
- `tokens.json` — generated; the same values for non-CSS tools.
- `kit.html` + `kit.css` — a zero-build starter: the board's shell (left
  nav, header chips, right rail) and small component classes matching the
  board (`.btn`, `.chip`, `.badge`, `.card`, `.field`, `.tree`/`.trow`,
  `.tbl`, `.tabs`/`.tab`, `.scrim`, `.modal`, `.drop`, `.toast`,
  `.empty`), plus a theme toggle with the same `cadence-theme` semantics
  as the app. `kit.html` renders every component in both themes.
- `mockups/` — mockups built on the kit (e.g. `mockups/wiki/`).

## Start a mockup

1. Copy `kit.html` to your mockup page. Keep the three `<link>`/`<script>`
  blocks in `<head>` (fonts, `tokens.css`, the theme boot), and the
  toggle script at the bottom.
2. Keep the `.shell` skeleton (`.side`, `.topbar`, `.body`), replace the
  nav items, crumb, chips and content. Drop `.rail` and add `no-rail` to
  `.body` for a full-width page.
3. Use the classes, not values — never write a hex or a font name into a
  mockup; everything the kit styles reads the tokens.
4. Open the file in a browser. No build step.

## Keeping tokens fresh

`tokens.css` and `tokens.json` are generated — never edit them:

    scripts/design-kit           regenerate after a styles.css change
    scripts/design-kit --check   fail when the outputs are stale

The generator reads the `@theme` block and the light overrides from
`ui/src/styles.css` (and verifies the file's two light blocks agree).
CI runs `--check` in the `fmt` job, so tokens cannot drift from the theme.

## The `design_kit` convention

A project's `PROJECT.md` may declare, in its frontmatter:

    design_kit: design/

The value is a repo-relative path to the project's design kit — the
directory holding `tokens.css`, `kit.html` and `kit.css`. An agent asked
to mock a screen for that project reads its kit from that path rather
than inventing colours. Projects without the key have no kit; the cadence
repo's own kit is this directory. (The key is a documented convention
only — no code reads it yet, so adding it to a PROJECT.md is safe and inert.)
