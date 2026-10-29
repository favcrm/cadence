# `@cadence/app-kit`

The app side of the Cadence screen bridge, implemented once. Transport and
lifecycle only: boot, `init`, `ready`, the closed PUSH union,
`call`/`slot`/`tool`, assets, bounded `state`, teardown. What a PUSH *means*
stays in the app.

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

## Usage

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
await receiver.closeDrawer();          // host returns the frame to the outlet
```

`openDrawer`/`closeDrawer` send the host UI verbs `screen.drawer.open` and
`screen.drawer.close`. They are shell verbs, not app capabilities, so they are
not required in the PUSH's `actions`. A host that never opts in — or opts in
but never sends a presentation — leaves the app in its own drawer; the helpers
reject with `ScreenClosedError`, which is the intended fallback.

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
  next PUSH. Without it, keep waiting for the PUSH.
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
- `args`/`input` are plain objects within 16 KiB; `anchor` is exactly
  `{x,y,w,h}` in frame coordinates (or `"footer"`); ids follow
  `ACTION_ID`, tool aliases `TOOL_ALIAS`, `request_id` `TOOL_REQUEST_ID`.
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

What stays app-owned: the closed verb set and per-verb arg shapes (the PUSH
`actions` advertisement), payload validation in `onPush`, and which asset refs
are legitimate.

Defaults: `accepts: ["screen.v2"]`, `timeoutMs: 15000`, boot read from
`#cadence-screen-boot`. A reply that is neither `ok` nor a well-formed
`{code, text}` refusal rejects with `ScreenRefusalError` (status from
`REFUSAL_STATUS`); no reply rejects with `ScreenTimeoutError` (504). Every
protocol violation calls `onError` and closes the port.

## Migrating a repo

1. Replace the hand-written receiver/bridge with `createScreenReceiver`; keep
   the app's projection and payload validation in `onPush`.
2. Keep `cadence app check` (not a local copy) in CI.
3. Keep the app's build and bundler: this package does not build screens.

## Tests

```sh
pnpm test:app-kit
```
