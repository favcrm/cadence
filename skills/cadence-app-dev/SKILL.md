---
name: cadence-app-dev
description: Design, build, audit, test, and package sandboxed Cadence apps using @cadence/app-kit, @cadence/screen-ui, app-storage/v1, and the cadence app CLI.
---

# Cadence App Development

Use this skill when building a new Cadence app, auditing or fixing an existing app's UI/UX or bridge integration, adding storage/skills/navigation manifests, or preparing a workspace or catalog app package.

## Authority & Anti-Skew Rule

This skill teaches architecture, workflows, and invariants—**never treat it as a substitute for the target repository's pinned contracts**. Existing apps span multiple `@cadence/app-kit` and `@cadence/screen-ui` versions (for example `0.3.x` vendored checkouts vs `0.5.0`).

Before writing code or diagnosing a bridge bug, inspect the target repository's pinned files first:

1. **Bridge contract & caps:** `@cadence/app-kit` `package.json`, `README.md`, `types/index.d.ts`, and `src/receiver.mjs` (or vendored equivalent). Check whether the repo exposes `createScreenBridge` (`0.5.0`) or only `createScreenReceiver` (`0.3.x`), and verify the exact `ready` capability limit (`6` on `0.5.0`, `4` on `0.3.x`), locale contract (`screen-locale.v1` `"en" | "zh-TW"` vs legacy `crm-locale.v1`), and payload byte caps.
2. **Visual & layout rules:** `@cadence/screen-ui` `DESIGN.md`, `src/index.ts`, and `primitives.css` (if installed/vendored). Reuse shared primitives (`DataTable`, `Field`, `Detail`, `DrawerShell`, `Loading`, `ErrorNotice`, `Notice`, `EmptyState`, `ConfirmDialog`, `Button`, `Link`, `Select`, `LocaleTextProvider`, `NavigationProvider`) rather than re-creating resets, tokens, or ad-hoc wrappers.
3. **App manifests & scripts:** Root `package.json` (`cadence: { app, dev, check }`), `app/app.md`, `app/screens/*/screens.json`, `app/app-storage.json`, `app/app-chat.json`, and `app/skills/<tag>/`.
4. **Canonical repo guides (in the `cadence` repo):** `docs/guides/app-development.md`, `docs/guides/app-toolchain.md`, `docs/guides/app-catalog.md`, `docs/guides/app-artifact-release.md`, and `docs/guides/app-local-development.md`.

## Choose the App Archetype

| Archetype | Typical Examples | Primary Primitives & Contracts | Key Constraints to Design Around |
| :--- | :--- | :--- | :--- |
| **System-of-Record** | Accounting, CRM, HR, Inventory | `app-storage/v1`, `DataTable`, `Detail`, `Field`, `ConfirmDialog`, `screen-records.v1` | Immutable `migrations` once shipped; integer minor units for money; explicit workflow state transitions; bilingual `en` + `zh-TW`; `320 KiB` CSV record limit (`maxArgsBytes: 327680`). |
| **Media / Capture / Publishing** | Social studio, Meeting Minutes, CMS | `screen-audio.v1`/`.v2`, asset slots (`publication_slot`, `required_asset_slot`), `cadence app binding` / `effect` | Upload media/audio via dedicated channels (`MAX_AUDIO_UPLOAD_BYTES = 50 MiB`, `200,000` transcript chars, `MAX_ASSET_BYTES` ~`128 KiB` encoded for `96 KiB` images); never stuff media blobs into `16 KiB` action args. |
| **Channel Bridges** | Inbox, Support triage, Notifications | Live pushes (`MAX_PUSH_BYTES = 128 KiB`), `app-chat/v1`, `NavigationProvider` | Idempotent action handlers inside `15,000 ms` default RPC timeout; clear unread/badge state synced cleanly; keep host-vs-app overlays unambiguous. |
| **Document / Workflow** | Proposals, Approvals, Checklists | `NavigationProvider` (`ownLeaveState`, `patchRoute`), `DrawerShell`, `screen-surfaces.v1` | Dirty-draft navigation guard via `ownLeaveState()`; `section_query` deep links per screen; never nest app modals/drawers inside a host drawer. |

## Non-Negotiable Pitfalls & Invariants

- **Sandboxed `<form>` submission (`sandbox="allow-scripts"` without `allow-forms`):** Native `<form>` submission and Enter-key submits fail silently or trigger browser sandbox errors inside Cadence's iframe host unless you install `installSandboxFormActions(document)` (converting `submit` / Enter into explicit button clicks or handlers). See [references/bridge-and-ui.md](references/bridge-and-ui.md).
- **Host vs App Drawer Boundary & No Nested Drawers:**
  - The Cadence host owns the board-wide frame, navigation chrome, install consent, and host drawers (`HOST_DRAWER_VERBS`).
  - Keep drawer `context` under `DRAWER_CONTEXT_BYTES` (`1024` bytes) and declare `size: "narrow" | "wide"` cleanly.
  - Never open a second app drawer/modal on top of an active host drawer; check drawer/surface mode before rendering local overlays.
- **Keep `ready` Capabilities Within Pinned Bounds:** `createScreenBridge` (`0.5.0`) admits up to `6` declared `ready` capabilities; older `createScreenReceiver` (`0.3.x`) caps at `4`. Always check the installed `app-kit` version before adding a capability.
- **Respect Wire Caps & Timeouts:**
  - `ACTION_ARGS_BYTES`: `16 KiB` (`16 * 1024`), overridden to `327,680` bytes for `screen-records.v1`.
  - `MAX_STATE_BYTES`: `32 KiB`; `MAX_PUSH_BYTES`: `128 KiB`; `MAX_REPLY_DATA_BYTES`: `1 MiB`; `DEFAULT_TIMEOUT_MS`: `15,000` ms.
  - Validate malformed/missing host receipts strictly—fail closed or show an explicit error/unavailable state rather than inventing optimistic success.
- **Immutable `app-storage.json` & Money Precision:**
  - Once a storage schema migration has shipped, treat existing `migrations` as append-only immutable history—never edit or delete prior SQL/migration entries; append a new migration instead.
  - Store currency as integer minor units (cents), validate balanced double-entry / `append_only` rules and state transitions server-side, and remember: native workspace installs refuse `app-storage`, whereas hosted org environments validate and provision it separately.
- **Bilingual Copy (`en` + `zh-TW`) & Navigation Parity:**
  - Follow `screen-locale.v1` (`"en" | "zh-TW"`) through `LocaleTextProvider` (or the app's translation dictionary) across headings, navigation tabs, form validation, empty states, and error notices—do not leave raw keys or hardcoded English labels on screens that support both locales.
  - Keep translation key completeness tests covering both locales when tests exist.
  - Configure `navigation.section_query` in `screens.json` intentionally so host tabs/sidebar links synchronize with `useNavigation()` / `patchRoute()`.
- **Visual Consistency & No Legacy Class Leakage:** Follow `@cadence/screen-ui` `DESIGN.md` compact SaaS density (`28px` desktop / `36px` narrow list controls, calm neutral chrome, dark & light token parity, reduced-motion support). Do not introduce new `.crm-*` legacy classes in greenfield apps.
- **No Shell Metacharacters in `package.json` `cadence` Commands:** `cadence.dev` and `cadence.check` reject `;`, `&&`, `||`, `|`, `<`, `>`, backticks, and `$()`. Invoke a script file or single binary directly.
- **Synthetic Data Only:** All test fixtures, `cadence app fixtures` seeds, docs, and examples must use synthetic, non-sensitive data—never real customer records, credentials, host paths, or commercial secrets.

## Standard Engineering Loop

1. **Inspect pinned contracts & existing manifests** before planning changes.
2. **Author or update manifests & code** (`app/app.md`, `app/screens/*/screens.json`, `app/app-storage.json`, optional `app/app-chat.json` and `app/skills/<tag>/`, plus TypeScript/UI code using `@cadence/app-kit` + `@cadence/screen-ui`).
3. **Run local dev & isolation harnesses:**
   - Start isolated local serving via `cadence app dev --package .` (binds `127.0.0.1` ports `3110..=3199` with scrubbed `CADENCE_APP_*` env vars).
   - Exercise iframe bridge behavior locally with `cadence app frame-kit` (validating `--parent-origin` against `https://` or `http://localhost:<port>` / `http://<name>.localhost:<port>`) and deterministic seed scenarios with `cadence app fixtures`.
4. **Verify static & build gates:**
   - Run the app's typecheck/unit/translation tests plus `package.json` `cadence.check`.
   - Run `cadence app check <source> --json` and inspect `cadence app inspect <source> --json` output. Do **not** treat `cadence app inspect` alone—or `--integrity-only` on `frame-kit` / `fixtures`—as sufficient install or release compatibility validation.
   - Verify loading, empty, error, blocked, success, desktop/narrow viewports, keyboard navigation, dark/light themes, and both `"en"` and `"zh-TW"` locales.
5. **Catalog / Org Actions Only When Authorized:** Never run `install`, `upgrade`, `release-install`, `release-update`, `release-revoke`, `binding`, or `effect` approvals against a live workspace or org unless explicitly instructed.

## References

Read the relevant reference file when working on a specific layer:

- [references/contracts-and-storage.md](references/contracts-and-storage.md) — `package.json` `cadence` block, `app.md`, `screens.json`, `app-storage.json`, `app-chat.json`, and bundled `app/skills/<tag>` package grammar & caps.
- [references/bridge-and-ui.md](references/bridge-and-ui.md) — `@cadence/app-kit` (`createScreenBridge` vs `createScreenReceiver`), payload limits, `installSandboxFormActions`, `NavigationProvider` / `ownLeaveState` / `patchRoute`, drawer contracts, and `@cadence/screen-ui` rules.
- [references/release-and-catalog.md](references/release-and-catalog.md) — Consuming versioned `cadence-app-toolchain` archives, embedded skill access, offline validation vs local/hosted `--org` catalog & artifact release/binding/effect flows.
