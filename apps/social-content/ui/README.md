# Content studio — clickable prototype (CAD-550)

The Social Content app's UI before anything is wired: all dummy data, no
backend, no network calls. Open it and click.

## Open it

- **From a file:** double-click `index.html` (works because the scripts use no
  `import`/`export` — module files are CORS-gated on `file://`; each file here
  is a dependency-ordered module registered on a single `SC` namespace, and
  remains a valid ES module to `import` when served).
- **Or any static server:** `python3 -m http.server 8080` in this directory →
  http://localhost:8080

Add `?freeze` to the URL to stop the ambient job simulation (the seeded
"running" run advancing) — useful for demos and screenshots.

## What's inside

| File | Role |
|---|---|
| `mock/data.js` | the whole fake database: 2 clients, 13 source posts, 9 posts across every state, runs, suggestions, one pending digest per client, one verify mismatch |
| `mock/api.js` | **the facade** — every screen calls `api.*`; nothing else sees the store. Wiring replaces only this file. |
| `lib/` | tiny DOM helper + shared widgets (status chips, IG preview, history, toasts) |
| `views/` | one file per screen: home, library, runs, editor, decide, automations, workflows, settings |
| `app.js` | shell: client switcher, nav, hash router, theme (same `cadence-theme` key as the board) |
| `screenshots/` | committed captures of every screen (dark + light + phone) |

## Try the touchpoints (design doc §5)

- **Take over** — open `p1` while the writer is drafting → *Take over*; the
  run log shows the caption need cleared.
- **Edit → revision by you** — change any caption, *Save caption*; history
  gains a `by you` revision and live checks re-run.
- **Ask the agent** — type "make it shorter" → *Ask*; a revise run writes a
  revision "writer (asked by you)" with an inline diff and *Undo*.
- **Accept / reject a suggestion** — Home rail or the post's own editor.
- **Approval voided by a later edit** — `p3` is approved inside digest d1;
  edit its caption and watch the digest row void and the post return to
  *in review*.
- **Receipt/verify mismatch** — Needs you → `p7`: approved rev pinned 2
  images + hash `31aa`; the receipt shows 1 image + `8f2c`.
- **Digest approve** — Needs you → hold-to-approve; unticked posts stay back.
- **Draft N posts** — Library → tick source posts → *Draft N posts* → watch
  the run progress in Runs.

## Look & feel

`styles.css` reuses the Cadence board's tokens (`--color-ink-*`, accent,
`--font-*`, card/field/chip classes) — same file structure, light and dark via
`data-theme` + `prefers-color-scheme`. Responsive to phone width.
