# Approve change — approval card (review draft)

Static mockup for CAD-1946's operator surface, produced to settle the card's
states and words before any product UI work.

- **Source:** `design/mockups/approval-card/{index.html,card.css,card.js}`
- **Design owners reused:** `design/tokens.css` + `design/kit.css` (copied beside
  the page so the published copy has no path outside its folder). No second
  palette, no new type scale.
- **Preview:** `https://preview.simplebuild.site/20261031-164011/approval-card/`
  (signed link; the `?t=` token is required and is preserved when the state
  changes, so a reviewer can switch states without losing access).
- **Status:** review draft. Nothing here is accepted, and none of it is
  implemented.

## What it demonstrates

| Control | States |
|---|---|
| State picker | ready · needs 1 more thing · head moved · queue ejected · hosted board unreachable · step-up · approved → merging |
| Width | 1280 and 390 |
| Theme | system, dark, light (tokens' own `data-theme`) |

Each row expands to say what the item means. "Approve and merge" appears only
when everything else holds; the header SHA, delivery class and policy digest sit
behind **Details**.

## Approximations, stated

- The card is shown in a bare shell, not the real board chrome. The real surface
  is the issue page's Approve change drawer.
- Checklist values are synthetic. No endpoint is called; nothing is approved,
  merged or revoked.
- Step-up is a button, not a real passkey ceremony.
- The real card must reuse the board's compiled components; this file is a
  static approximation of layout and words.
