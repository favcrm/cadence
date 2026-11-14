# Bridge (`@cadence/app-kit`) and UI (`@cadence/screen-ui`)

Always read the target repo's pinned `@cadence/app-kit` (`package.json`, `README.md`, `types/index.d.ts`, `src/receiver.mjs`) and `@cadence/screen-ui` (`DESIGN.md`, `src/index.ts`) before choosing bridge helpers or capabilities.

## 1. Receiver Initialization (`createScreenBridge` vs `createScreenReceiver`)

- **Modern `@cadence/app-kit` (`0.5.0`):** Use `createScreenBridge` where available. Admits up to **6** declared `ready` capabilities and supports `screen-locale.v1` (`"en" | "zh-TW"`), `screen-audio.v1`/`.v2`, `screen-records.v1`, `ownLeaveState()`, and `patchRoute()`.
- **Legacy `@cadence/app-kit` (`0.3.x`, still vendored in some apps):** Only exposes `createScreenReceiver`, enforces a **4** `ready` capability limit (`readyAccepts <= 4`), and may use legacy locale tags such as `crm-locale.v1`.
- Never silently assume `createScreenBridge` or a `6`-capability budget exists until verified in the target repo's installed `app-kit`.

## 2. Exact Wire Constants & Error Classes

Exported by `@cadence/app-kit` (`src/index.mjs`, `src/receiver.mjs`, `src/refusal.mjs`):

| Constant / Surface | Limit / Behavior | Design Implication |
| :--- | :--- | :--- |
| `ACTION_ARGS_BYTES` | `16 KiB` (`16 * 1024`), raised to `327,680` B (`maxArgsBytes`) for `screen-records.v1` | Keep interactive action payloads compact; use CSV/chunked flows or artifact/media uploads for bulk data. |
| `MAX_STATE_BYTES` | `32 KiB` | Persist large domain datasets in backend storage/records, not ephemeral bridge state. |
| `MAX_PUSH_BYTES` | `128 KiB` | Paginate list snapshots pushed from host to iframe. |
| `MAX_REPLY_DATA_BYTES` | `1 MiB` | Bound RPC response payloads. |
| `MAX_ASSET_BYTES` | `ceil((96 * 1024 * 4) / 3) + 32` (~`128 KiB` encoded `96 KiB` asset) | Compress or offload heavy media via asset/artifact slots. |
| `MAX_AUDIO_UPLOAD_BYTES` | `50 MiB` (`SCREEN_AUDIO_V1` / `SCREEN_AUDIO_V2`) | Stream/upload audio only through audio capability handlers. |
| `MAX_AUDIO_TRANSCRIPT_CHARS` | `200,000` | Truncate/paginate ultra-long transcripts in views. |
| `DRAWER_CONTEXT_BYTES` | `1,024` bytes | Pass IDs and minimal cursor keys in drawer context, never whole records. |
| `DEFAULT_TIMEOUT_MS` | `15,000` ms | Ensure interactive actions fail/reply quickly; do not leave pending promises dangling. |

**Strict error/refusal handling:** Catch `ScreenBridgeError`, `ScreenClosedError`, `ScreenRefusalError`, and `ScreenTimeoutError` (or inspect `REFUSAL_STATUS` via `readRefusal`). Render an explicit blocked/unavailable/retryable UI state—never coerce a refusal or timeout into optimistic local success.

## 3. Host vs. App Drawer & Surface Boundaries

- Declare drawer widths via `size: "narrow" | "wide"` and pass `<= 1024` bytes of `context`.
- The host owns global chrome, sidebar/screen navigation, and `HOST_DRAWER_VERBS` drawers.
- **No nested drawers/modals:** Never open an app-owned drawer, modal, or confirmation sheet inside an active host drawer. Check the current surface/drawer mode (`screen-surfaces.v1` or `DrawerShell` context) first.
- Check host `screen-surfaces.v1` support before adopting newer surface controls over legacy `screen-presentation.v1`.

## 4. Routing, Unsaved Draft Guards & Sandboxed `<form>` Handling

- **Sandboxed `<form>` bug (`sandbox="allow-scripts"` without `allow-forms`):**
  - Cadence mounts app iframes without `allow-forms`. Standard `<form>` submits and Enter-key submissions fail or log sandbox violations unless handled in JS.
  - Install a delegated `submit` handler early in app boot (before starting the bridge channel, matching the `installSandboxFormActions(document)` pattern): call `event.preventDefault()` on matching forms and route the interaction directly to the form's explicit button handler / async submit callback.
- **Navigation & dirty-draft protection:**
  - Use `@cadence/screen-ui` `NavigationProvider`, `useNavigation()`, and `activateNavigation(adapter, href, event, options)` with `modifiedClicks: "browser" | "prevent"` (only host frames with usable real URLs should allow modified browser clicks).
  - Sync section switches cleanly with `patchRoute()` and the declared `navigation.section_query` key in `screens.json`.
  - Guard unsaved edits with `ownLeaveState()` so navigation away prompts rather than silently dropping user drafts.

## 5. `@cadence/screen-ui` Component & Visual Rules

Exported primitives (`ui/screen-ui/src/index.ts`):
- Layout & data: `DataTable`, `Field`, `Detail`, `DrawerShell`, `LocaleTextProvider`, `NavigationProvider`, `useNavigation`, `activateNavigation`
- Feedback & actions: `Loading`, `ErrorNotice`, `Notice`, `EmptyState`, `ConfirmDialog`, `Button`, `Link`, `Select`
- Data fetching helpers: `ApiError`, `Resource`, `QueryCache`, `useQuery`, `useResource`, `useMaybeResource`

Visual & UX requirements from `ui/screen-ui/DESIGN.md`:
- Compact, calm SaaS density: `28px` desktop list/control heights, `36px` on narrow viewports.
- Support both dark and light theme semantic tokens (`theme.css`, `foundation.css`, `controls.css`, `drawer.css`)—never add a second CSS reset or custom color palette.
- Honor reduced-motion preferences, full keyboard focus rings, and desktop + narrow responsive stacking.
- Provide full `"en"` and `"zh-TW"` copy through `LocaleTextProvider` (or app translation dictionaries) for every heading, empty state, validation message, error state, and `section_query` tab label.
- **Do not use legacy `.crm-*` selectors** in greenfield apps; only retain existing `.crm-*` references when maintaining unmigrated legacy components.
