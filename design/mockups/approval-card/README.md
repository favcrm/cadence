# Approve change — approval card (review draft, real UI)

Static mockup of CAD-1946's operator surface, built on the board's **own compiled
CSS** rather than the design kit: the shell, sidebar, header, chips, drawer and
buttons are the classes the live board uses.

- **Source:** `design/mockups/approval-card/{index.html,card.css,card.js}`
- **Preview:** `https://preview.simplebuild.site/20261101-151847/approval-card-rebased/`
  (signed; the `?t=` token survives state changes)
- **Status:** review draft. Nothing accepted, nothing implemented.

## How it is built

| Piece | Owner |
|---|---|
| Shell, `aside`, `header` | the live board's classes (`grid lg:grid-cols-[208px_minmax(0,1fr)]`, `navlink`, `proj`, `app-header`, `header-breadcrumb`) |
| Drawer | `rc-sheet bg-ink-875 border-l border-ink-700` — the hire sheet's own pattern, including its bottom-sheet rule under 640px |
| Buttons | `btn btn-primary`, `btn btn-ghost btn-sm` — read off the live hire sheet |
| Colours, type | `--color-*` and `--font-*` from the board's compiled stylesheet |
| `card.css` | only what the board lacks: the status dot, row layout, progress steps |

`index.css` is **not** committed. This preview reuses the compiled board
stylesheet from the previous `approval-card-real` preview (SHA-256
`b22460d705140f9286121999a807651e15fa0a05181f3650151e26cc3affcccf`);
it was not refreshed from the live board for this rebase.

## States

ready · one thing missing · head moved · queue ejected · board unreachable ·
step-up · approved → merging.

Rows expand to say what each item means. **Approve and merge** only when
everything else holds. Head, base, class and policy digest behind **Details**.
Nothing merges until you approve; while merging, **Take my approval back**
stays until it lands.

Width follows the window: the drawer is `min(460px,100%)` and becomes the
bottom sheet the board already uses below its breakpoint. Set the viewport to
390 to see that, rather than a fake phone frame.

## Approximations

- The page behind the drawer is synthetic; the drawer is the real component
  class, not the real React component.
- Step-up is a button, not a passkey ceremony.
- No endpoint is called; nothing is approved, merged or revoked.
