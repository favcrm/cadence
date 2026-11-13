# `@cadence/app-kit`

The app side of the Cadence screen bridge, implemented once with zero runtime
dependencies and zero domain-specific assumptions:

1. **`createScreenBridge(options)`** — reactive `screen.v2` store (`subscribe` +
   `getState`), closed `ScreenPush` validation, host drawer verb separation,
   per-verb argument allowlists and byte caps, concurrency limits, reply size
   and secret-key guards, write vs read retirement, route patching, and scoped
   unsaved-edit leave state.
2. **`createScreenReceiver(options)`** — low-level transport and lifecycle:
   boot, `init`, `ready`, the closed PUSH union, `call`/`slot`/`tool`, assets,
   drawer surfaces + context, `ownLeaveState`, bounded `state`, teardown.

The wire protocol is owned by
`ui/src/features/workspace-apps/screen/screenProtocol.ts`; this package
follows that document and does not extend it.

## Not a toolchain

Cadence already owns the developer surface, and this package deliberately
does not duplicate it:

| Need | Use |
| --- | --- |
| offline bundle validation | `cadence app check <dir>` (host-owned; caps, entry grammar, descriptors, workflows, `requires`) |
| fixtures | `cadence app fixtures --bundle <dir>` |
| offline frame / kit CSS | `cadence app frame-kit --out <dir> [--bundle <dir>]` |
| preview | `cadence app dev --source <checkout> <name>` |
| starter project + pinned deps | the `cadence-app-toolchain-<tag>.tar.gz` release asset |

This package is the one piece those do not provide: a receiver an app screen
runs in the browser. It ships inside the toolchain rather than as a parallel
CLI.

## Usage (`createScreenBridge`)

```js
import { createScreenBridge } from "@cadence/app-kit";

export const CALL_FIELDS = {
  "records.list": ["query", "limit", "cursor"],
  "records.show": ["record_id"],
  "records.create": ["record_id", "profile"],
  "records.update": ["record_id", "expected_revision", "profile"],
  "screen.route.patch": ["section", "view", "record_id"],
};

export const bridge = createScreenBridge({
  tag: "notes",
  appName: "notes",
  records: true,
  surfaces: true,
  locale: true,
  calls: CALL_FIELDS,
  argBytes: { "records.create": 49152, "records.update": 49152 },
  writeVerbs: ["records.create", "records.update"],
  observations: ["records.list", "records.show"],
  route: { sections: ["all", "open"], defaultSection: "all" },
});
```

## Presentation and the board-wide drawer

A screen opts into the host's `screen-presentation.v1` contract by naming it in
`accepts` (with `screen.v2`). The host then tells the frame which surface it is
presented in — the app outlet (`"outlet"`) or the board-wide drawer
(`"drawer"`). The drawer is host-owned: the host owns its geometry, scrim and
close control, while the app draws only its surface full-bleed.

```js
const receiver = createScreenReceiver({
  accepts: ["screen.v2", "screen-actions.v1", "screen-presentation.v1"],
  onPresentation(mode) { renderSurface(mode); }, // "outlet" | "drawer"
  onPush(push) { /* ... */ },
});

receiver.presentation();          // current surface, defaults to "outlet"
await receiver.openDrawer("settings"); // host opens the board-wide drawer
await receiver.openDrawer("draft", { size: "wide" }); // a wider panel
await receiver.closeDrawer();          // host returns the frame to the outlet
```

`openDrawer(surface, { size, context })` takes an optional `size` of `"narrow"`
(the default panel) or `"wide"` (about 736px at desktop widths; the panel is the
full viewport at the board's narrow breakpoint either way), plus an optional
bounded plain-JSON `context` (`<= 1024` bytes) on `screen-surfaces.v1` read by
the drawer child via `drawerContext()`. Any other size or invalid context
rejects with a `TypeError` before it is sent, and a host refuses it in place.

`openDrawer`/`closeDrawer` send the host UI verbs `screen.drawer.open` and
`screen.drawer.close`. They are shell verbs, not app capabilities, so they are
not required in the PUSH's `actions`. A host that never opts in — or opts in
but never sends a presentation — leaves the app in its own drawer; the helpers
reject with `ScreenClosedError`, which is the intended fallback.

`screen-surfaces.v1` is the independent-overlay variant: the host names each
mount's role (`"primary"` or `"drawer"`), and the optional `onSurface` callback
receives `(role, surface, modal)`. `receiver.surfaceRole()` and
`receiver.surface()` expose the same values after the host message. The drawer
helpers work under either opt-in; a drawer mount never opens another drawer.

```js
import { createScreenReceiver } from "@cadence/app-kit";

const receiver = createScreenReceiver({
  onPush(push) {
    // validate and project this app's payload; a throw tears the port down
    render(project(push));
  },
  onError(reason) {
    showBanner(reason); // the host already closed the port
  },
});

const result = await receiver.call("social.drafts.discard", { draft_id });
const asset = await receiver.asset("draft-image:d1:0");
receiver.state(JSON.stringify(localDraft)); // ≤ 32 KiB
```

### Generic locale, audio capture, leave guard, and records screens

Any `screen.v2` screen may opt into `screen-locale.v1` to receive the
host-selected UI locale (`push.presentation_locale`: `"en" | "zh-TW"`), and may
call `ownLeaveState()` after an accepted `screen.v2` PUSH to publish
`{ v: 2, op: "leave", unsaved }` with symbol-fenced handle ownership.

Any `screen.v2` screen may opt into `screen-audio.v1` (`audio: true` on
`createScreenBridge`, or `accepts: ["screen.v2", "screen-audio.v1"]` on
`createScreenReceiver`). When the host projects `audio_capture: 1` for the live
mount (`bridge.canAudioCapture()`), calling
`bridge.audioCapture(scope, { mode: "record" | "upload" })` asks the top-level
host shell to run microphone recording or audio file upload outside the
sandboxed iframe and resolves `{ transcript, duration_seconds, held_at, source }`.
Unlike `call`/`tool`, interactive audio capture waits without a wall-clock RPC
timeout unless `timeoutMs` is passed explicitly, and cancelling or refusing the
modal (`cancelled`, `owner_required`, `conflict`, `unavailable`) leaves the
open screen bridge intact.

A records screen names `screen-records.v1` (with `screen.v2`) in `accepts`; the
host then sends the records projection, which the app validates in `onPush` (or
delegates to `createScreenBridge`). Records CSV calls are larger than the
default action budget, so the receiver takes a `maxArgsBytes` limit (the host
allows 320 KiB for `records.csv.*`; keep the app's own per-verb bounds too).

```js
const receiver = createScreenReceiver({
  accepts: ["screen.v2", "screen-records.v1", "screen-locale.v1"],
  maxArgsBytes: 327680,
  onPush(push) { validateAndRender(push); },
});
await receiver.call("records.csv.preview", { csv_text });
```

Wire guarantees the receiver enforces (mirroring the Social Content
reference receiver and `screenProtocol.ts`):

- `call`/`slot`/`tool` and `asset` open only after a `screen.v2` PUSH; before
  that, and on a v1-only mount, they reject with `ScreenClosedError`
  (`code: "gone"`) instead of posting a request the host cannot answer.
- A host that cannot serve an asset ref sends nothing. `asset(ref, { timeoutMs })`
  rejects with `ScreenTimeoutError` when the host has not answered in time and
  frees the ref for a re-request; the host's one late answer is dropped, not a
  teardown. Without `timeoutMs` the request waits as before.
- A call reply may name the asset refs it made servable (`asset_refs`, a host
  field older hosts never send); `assetAdmitted(ref)` is then true, so an app
  can request a just-returned draft image at once instead of waiting for the
  next PUSH. Without it, keep waiting for the PUSH. It stays true once set, so
  still check the latest reply's data (a draft whose image was removed).
- `tool` additionally requires `accepts` to include `screen-actions.v1`; the
  host closes a port that sends a tool call without the negotiation. One tool
  call is outstanding at a time — a second is refused locally with
  `conflict` (the host closes the mount on concurrent tool calls). A local
  timeout does not cancel the host invocation, so the gate stays closed until
  the matching `tool-reply` (even a late one) or teardown. A retired id keeps
  its kind: a valid duplicate is ignored and never releases another
  request's gate; a wrong-kind or malformed late reply is torn down.
- A v1-only mount sends the bare `{op:"ready"}` envelope; `screen.v1` is a
  local PUSH filter, never a name on the wire (the host refuses it).
- A slot's `pending` event arrives through `onSlotState` and leaves the
  promise open; only `done`/`refused` settle it, so the receipt is never lost.
- Host replies must be `v:2` and answer the request's own kind (`call`→`reply`,
  `slot`→`slot-state`, `tool`→`tool-reply`); anything else tears the port down.
- Asset replies carry `data:image/(jpeg|png|webp);base64,…` within the 96 KiB
  image's encoded allowance (`MAX_ASSET_BYTES`).
- `args`/`input` are plain objects within 16 KiB by default; `maxArgsBytes`
  raises that for a host verb with a larger allowance (records CSV is 320 KiB).
  `anchor` is exactly `{x,y,w,h}` in frame coordinates (or `"footer"`); ids
  follow `ACTION_ID`, tool aliases `TOOL_ALIAS`, `request_id` `TOOL_REQUEST_ID`.
- `tool` takes an optional fourth argument, `generation_scope`
  (`{draft_id, operation: "caption"|"image", revision}`), carried on the wire
  so the host binds the generation to that draft revision.
- Boot `generation` is a non-negative safe integer; `tag` is 1–32 lowercase
  letters/digits/dashes, and an `http` parent is only the host's
  `*.localhost:<port>` origin.
- The first accepted PUSH pins `v`/`install_id`/`digest`/`context_id`; any
  later disagreement tears the port down.
- Teardown rejects every outstanding request with `ScreenClosedError`, so no
  caller is left pending on a dead mount.
- A slot's `pending` is progress, not a result: it goes to `onSlotState` and
  the promise stays open until `done`/`refused` (which resolve the host's
  event, receipt included). The optional anchor defaults to `"footer"`.

Defaults: `accepts: ["screen.v2"]`, `timeoutMs: 15000`, boot read from
`#cadence-screen-boot`. A reply that is neither `ok` nor a well-formed
`{code, text}` refusal rejects with `ScreenRefusalError` (status from
`REFUSAL_STATUS`); no reply rejects with `ScreenTimeoutError` (504). Every
protocol violation calls `onError` and closes the port.

## Migrating a repo

1. Replace the hand-written receiver/bridge with `createScreenBridge` (or
   `createScreenReceiver` for custom projections).
2. Keep `cadence app check` (not a local copy) in CI.
3. Keep the app's build and bundler: this package does not build screens.

## Tests

```sh
bun run test:app-kit
```

Run it from `ui/`, where the `test:app-kit` script lives.
