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
