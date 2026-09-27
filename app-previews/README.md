# App development (CAD-647)

Trusted development sources live here, **outside installable A1 bundles** in
`apps/`. The install validator stays strict. CAD-500 owns production custom
screens, sandbox and host SDK; CAD-633 owns real Social Content wiring after
CAD-631/632. Draft PR307 is a design reference, not a runtime dependency.

## Start

Node22+ and the pnpm version pinned in ui/package.json are required. From a clean
source checkout, one command installs the locked existing tools and starts HMR:

```sh
(cd ui && pnpm install --frozen-lockfile) && node scripts/app-dev.mjs social-content
```

Default: http://127.0.0.1:3186/. Existing private reachability, without tailnet,
firewall or public exposure changes:

```sh
node scripts/app-dev.mjs social-content --port 3186 --host 0.0.0.0 --allow-host ip-172-31-1-32.tail9fcf30.ts.net
```

The actual CLI bridge in a binary containing this change:

```sh
cadence app dev social-content --source /path/to/trusted/cadence-checkout --port 3186
```

`--source` explicitly authorizes that checkout's known Node harness. No installed
app/manifest or git URL is executed. The bridge dispatches before resolving
state or sandbox profiles; it uses no daemon, PM or approval. Its child inherits
only PATH/HOME/LANG/TERM/PNPM_HOME/XDG_CACHE_HOME, not native identity, state,
provider credentials or NODE_OPTIONS. Dependencies must already be installed.

## Boundary and ownership

Home opens the current Hong Kong planning week with a full-height day-row
Calendar/Board and a needs /
suggestions rail. Each calendar row is a day; cards show time, state and
destinations, remain distinct fixed-width cards in a horizontally scrollable day strip, including
narrow screens. Empty days are
explicit; there are no invented durations or hourly slots. Library is an immutable source-media grid: select sources,
Draft N posts, inspect the resulting fixture batch in Runs, then open its editor.
A source can have multiple derived drafts. Optional brand context persists across
app navigation; no site or project is mandatory.

Caption/media/time/destination changes create a material revision and invalidate
local review, approval and staged simulation. Cross-field actions require saving
or explicitly discarding unsaved caption/planning changes. Context changes clear
selection and Runs show only matching items, preserving original source brands. Choose a local plan, mark reviewed,
approve the current local revision, then stage in the fixture outbox. Planning
uses plain Hong Kong calendar date/time, never an external scheduler. Published
seed records and verification mismatches are illustrative; external receipts
are always absent. Ask appends an explicitly labelled fixture instruction rather
than generating copy; caption Undo is a new unapproved revision. Source inspectors
and editors trap focus, close on Escape and restore the opener when it remains.

Automations, Workflows and Settings are descriptor screens with unavailable live
controls disabled and explained. Uploads, real protected-term validation, passkeys,
agent generation, production approvals and publishing remain unavailable. Drawer
entry and view feedback respect reduced motion; full production exit/FLIP motion
and optimistic network rollback belong to later live integration.
The selector exposes loading, empty, error/retry and read-only scenarios.

State lives in this tab's memory. Compatible SocialContent.jsx and CSS edits use
React Fast Refresh/HMR and may retain editing state; store/sdk/fixture changes,
hook structure, imports or bootstrap edits can reload/reset it. Browser retention
must be verified separately from the HMR protocol. The banner labels source
**base revision** and **live working tree**; uncommitted code is not an installed
revision. Page refresh resets fixtures.

sdk.mjs is an app-local fixture facade, not the production privileged host SDK.
A second app can add its own index.html/UI/facade/fixtures under this directory
and reuse the runner. Live backends are unsupported: backend/proxy/token options
are refused. Ports are3110–3199 with strictPort; private sharing requires an
explicit existing tailnet hostname. API and platform routes are blocked. Vite
loads no env files or inherited VITE secrets; only APP_DEV_PUBLIC_* fields are
explicitly public. This trusted development server is not a capability sandbox.

Production installation remains immutable and hash reviewed. HMR never changes
permissions, approves an app, edits active runs or invokes outward effects.
Future CAD-500 builds require sandbox/capability review; active workflows/runs
must keep their pinned source revision when development source changes.

## Verification / cleanup

```sh
node --test scripts/app-dev/*.test.mjs
```

The owned Vite integration test proves CSS HMR plus changed output, API/private
file refusal and environment isolation. CI also tests the actual Rust bridge,
poisoned sandbox-state bypass and credential scrubbing. Do not bypass native
build-slot admission for local Rust gates. Browser/visual and live hosted runtime
evidence must be reported separately; HTTP/HMR are not proof of either.

The CAD647 author owns persistent private port3186 and its recorded PID/log.
Leave it available for operator review, then author/PM stops only that owned
process. Preview cleanup never touches another lane, production3010 or tailnet.

The preview imports generated `design/tokens.css` and `design/kit.css` directly.
App-local CSS only adapts layout using canonical variables. The theme control
cycles system/light/dark with the board's `cadence-theme` storage semantics.
Only the canonical design directory is additionally allowed for CSS imports;
board source, private repository files and live API routes remain unavailable.

The default illustrative Tuesday contains five source-derived posts, exposing
horizontal browsing immediately. Dates/counts stay outside the scroll region;
keyboard focus brings cards into view without changing planning semantics. Day-strip scrollbars are hidden; directional arrows appear on hover or keyboard focus when more cards exist, and remain visible on touch devices. Native wheel/touch/keyboard scrolling remains available; reduced-motion preferences disable smooth arrow scrolling.

Board uses full-height pipeline columns with fixed counts/headers, explicit empty states, natural-height cards and independent vertical bodies. Narrow screens browse columns horizontally in the remaining bounded pane; the suggestions rail remains reachable below. The week calendar hides its vertical scrollbar and supplies overflow-aware up/down day controls, retaining native keyboard/wheel/touch scrolling in its labelled focusable region. Only calendar and day-strip scrollbars are hidden; board column/outer browsing keeps native scrollbars.
