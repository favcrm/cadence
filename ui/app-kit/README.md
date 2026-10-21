# `@cadence/app-kit`

The app side of the Cadence screen bridge, and the bundle checks every app
repo keeps re-implementing — once, dependency-free.

The wire protocol itself is owned by
`ui/src/features/workspace-apps/screen/screenProtocol.ts`. This package
implements the child side of that document (boot, `init`, `ready`, the closed
PUSH union, `call`/`slot`/`tool`, assets, `state`, teardown) and nothing
beyond it. What a PUSH *means* stays in the app.

## Why

`social-content` (`src/screens/receiver.mjs` + `bridge.mjs`),
`meeting-minutes` (`src/screens/receiver.mjs`) and `waba5`
(`src/bridge/*`) each implement the same handshake, request bookkeeping,
refusal mapping and teardown. The bundle checks are copied per repo as well
(`check_app_views.mjs`, `build_screen.mjs`, `build-ui-package.mjs`).

## Receiver

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
protocol violation calls `onError` and closes the port — the kit never
guesses.

## Bundle check

```sh
npx cadence-app-check .     # or: node bin/check-bundle.mjs <app-repo>
```

Checks `app/app.md` frontmatter (`app`, `title`, `version`, `summary`), the
`listing` assets, every `app/views/*.json` (`app-views/v1`, unique ids,
manifest match), at least one workflow with `title`/`goal` and a body, and
non-empty rubrics. Each finding carries a stable code from `BUNDLE_CODES`.

## Migrating a repo

1. `npm i @cadence/app-kit` (or the pinned tarball) next to `@cadence/screen-ui`.
2. Replace the hand-written receiver/bridge with `createScreenReceiver`; keep
   the app's projection and its payload validation in `onPush`, and port the
   per-app refusal status overrides only where they differ from the default.
3. Replace `scripts/check_app_views.mjs` with `cadence-app-check .` in CI.
4. Keep the app's build: this kit does not bundle screens or ship UI.

## Packaging

Local-only, source-pinned, like `@cadence/screen-ui`:

```sh
node ui/scripts/package-app-kit.mjs --source-tree <checkout> \
  --source-sha <40-hex> --out <dir>
```

Prints `{name, version, source_sha, file, sha256}`. Publishing is a separate
operator step.

## Tests

```sh
node --test ui/app-kit/tests/*.test.mjs
```
