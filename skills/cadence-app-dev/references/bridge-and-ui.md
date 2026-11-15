# Bridge (`@cadence/app-kit`) and UI (`@cadence/screen-ui`)

Always read the target repo's pinned `@cadence/app-kit` (`package.json`, `README.md`, `types/index.d.ts`, `src/receiver.mjs`) and `@cadence/screen-ui` (`DESIGN.md`, `src/index.ts`) before choosing bridge helpers or capabilities.

## 1. Receiver Initialization (`createScreenBridge` vs `createScreenReceiver`)

- **Modern `@cadence/app-kit` (`0.5.0`):** Use `createScreenBridge` where available. Admits up to **6** declared `ready` capabilities and supports `screen-locale.v1` (`"en" | "zh-TW"`), `screen-audio.v1`/`.v2`, `screen-records.v1`, `ownLeaveState()`, and `patchRoute()`.
- **Legacy `@cadence/app-kit` (`0.3.x`, still vendored in some apps):** Only exposes `createScreenReceiver`, enforces a **4** `ready` capability limit (`readyAccepts <= 4`), and may use legacy locale tags such as `crm-locale.v1`.
- Never silently assume `createScreenBridge` or a `6`-capability budget exists until verified in the target repo's installed `app-kit`.

## 2. Wire Limits, Exported Constants & Error Classes

Exported by `@cadence/app-kit` (`src/index.mjs`, `src/receiver.mjs`, `src/refusal.mjs`):

| Constant / Surface | Limit / Behavior | Design Implication |
| :--- | :--- | :--- |
| `action_args` tiers (default `argBytes: 16 KiB`) | `16 KiB` (`16 * 1024`) default; `48 KiB` (`49,152` B) for `records.create\|update\|batch`, `content.save`, `screen.preview.show`, `whatsapp.*`; `320 KiB` (`327,680` B) for `records.csv.*` | Keep interactive action payloads compact (override `argBytes` per verb only within host-permitted caps) and use CSV/artifact/media flows for bulk data. |
| `MAX_STATE_BYTES` | `32 KiB` | Persist large domain datasets in backend storage/records, not ephemeral bridge state. |
| `MAX_PUSH_BYTES` | `128 KiB` | Paginate list snapshots pushed from host to iframe. |
| `MAX_REPLY_DATA_BYTES` | `1 MiB` | Bound RPC response payloads. |
| `MAX_ASSET_BYTES` | `ceil((96 * 1024 * 4) / 3) + 32` (~`128 KiB` encoded `96 KiB` asset) | Compress or offload heavy media via asset/artifact slots. |
| `MAX_AUDIO_UPLOAD_BYTES` | `50 MiB` (`SCREEN_AUDIO_V1` / `SCREEN_AUDIO_V2`) | Stream/upload audio only through audio capability handlers. |
| `MAX_AUDIO_TRANSCRIPT_CHARS` | `200,000` | Truncate/paginate ultra-long transcripts in views. |
| `DRAWER_CONTEXT_BYTES` | `1,024` bytes | Pass IDs and minimal cursor keys in drawer context, never whole records. |
| `DEFAULT_TIMEOUT_MS` / `timeoutMs` | `createScreenBridge` defaults `timeoutMs` to `65,000` ms; low-level `createScreenReceiver` defaults to `DEFAULT_TIMEOUT_MS` (`15,000` ms) | Ensure interactive actions fail/reply promptly and pick a timeout appropriate to the host operation. |

**Origin & boot validation:** At runtime, `@cadence/app-kit`'s `validateBoot()` validates `#cadence-screen-boot` (`parent_origin` must be `https:` or `http://localhost[:port]` / `http://<name>.localhost[:port]`).

**Strict error/refusal handling:** Catch `ScreenBridgeError`, `ScreenClosedError`, `ScreenRefusalError`, and `ScreenTimeoutError` (or inspect `REFUSAL_STATUS` via `readRefusal`). Render an explicit blocked/unavailable/retryable UI state—never coerce a refusal or timeout into optimistic local success.

## 3. Host vs. App Drawer & Surface Boundaries

- Declare drawer widths via `size: "narrow" | "wide"` and pass `<= 1024` bytes of `context`.
- The host owns global chrome, sidebar/screen navigation, and `HOST_DRAWER_VERBS` drawers.
- **No nested drawers/modals:** Never open an app-owned drawer, modal, or confirmation sheet inside an active host drawer. Check the current surface/drawer mode (`screen-surfaces.v1` or `DrawerShell` context) first.
- Check host `screen-surfaces.v1` support before adopting newer surface controls over legacy `screen-presentation.v1`.

## 4. Routing, Unsaved Draft Guards & Sandboxed `<form>` Handling

- **Sandboxed `<form>` bug (`sandbox="allow-scripts"` without `allow-forms`):**
  - Cadence mounts app iframes without `allow-forms`. Standard `<form>` submits and Enter-key submissions fail or log sandbox violations unless handled in JS.
  - Attach explicit `<form onSubmit={(event) => { event.preventDefault(); ... }}>` handlers or install an early document capture `submit` listener calling `event.preventDefault()` (or check whether your pinned `@cadence/app-kit` version exports a sandbox form helper) so form submit / Enter routes into JS callbacks without triggering browser native form submission.
- **Navigation & dirty-draft protection:**
  - Use `@cadence/screen-ui` `NavigationProvider` and `useNavigation()` (`NavigationAdapter` with `modifiedClicks: "browser" | "prevent"`; only host frames with usable real URLs should allow modified browser clicks).
  - Sync section switches cleanly with `@cadence/app-kit`'s `bridge.patchRoute()` and the declared `navigation.section_query` key in `screens.json`.
  - Guard unsaved edits with `@cadence/app-kit`'s `bridge.ownLeaveState()` so navigation away prompts rather than silently dropping user drafts.

## 5. `@cadence/screen-ui` Component & Visual Rules

Exported primitives (`ui/screen-ui/src/index.ts`):
- Layout & data: `DataTable`, `Field`, `Detail`, `DrawerShell`, `LocaleTextProvider`, `NavigationProvider`, `useNavigation`
- Feedback & actions: `Loading`, `ErrorNotice`, `Notice`, `EmptyState`, `ConfirmDialog`, `Button`, `Link`, `Select`
- Data fetching helpers: `ApiError`, `Resource`, `QueryCache`, `useQuery`, `useResource`, `useMaybeResource`

Visual & UX requirements from `ui/screen-ui/DESIGN.md`:
- Compact, calm SaaS density: `28px` desktop list/control heights, `36px` on narrow viewports.
- Support both dark and light theme semantic tokens (`theme.css`, `foundation.css`, `controls.css`, `drawer.css`)—never add a second CSS reset or custom color palette.
- Honor reduced-motion preferences, full keyboard focus rings, and desktop + narrow responsive stacking.
- Provide full `"en"` and `"zh-TW"` copy through `LocaleTextProvider` (or app translation dictionaries) for every heading, empty state, validation message, error state, and `section_query` tab label.
- **Do not use legacy `.crm-*` selectors** in greenfield apps; only retain existing `.crm-*` references when maintaining unmigrated legacy components.
