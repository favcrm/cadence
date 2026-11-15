---
name: cadence-app-dev
description: Design, build, audit, test, and package sandboxed Cadence apps using @cadence/app-kit, @cadence/screen-ui, app-storage/v1, and the cadence app CLI.
---

# Cadence App Development

Use this skill when building a new Cadence app, auditing or fixing an existing app's UI/UX or bridge integration, adding storage/skills/navigation manifests, or preparing a workspace or catalog app package.

## Authority & Anti-Skew Rule

This skill teaches architecture, workflows, and invariants—**never treat it as a substitute for the target repository's pinned contracts**. Existing apps span multiple `@cadence/app-kit` and `@cadence/screen-ui` versions (for example `0.2.0` vendored checkouts from toolchain `v0.3.20` vs `0.3.0` vs `0.4.0`+).

Before writing code or diagnosing a bridge bug, inspect the target repository's pinned files first:

1. **Bridge contract & caps:** `@cadence/app-kit` `package.json`, `README.md`, `types/index.d.ts`, and `src/receiver.mjs` (or vendored equivalent). Check whether the repo exposes `createScreenBridge` (`0.3.0`+) or only `createScreenReceiver` (`0.2.0`), and verify the exact `ready` capability limit (`6` on `0.4.0`+, where `0.4.0` added `screen-audio.v1` and `0.5.0`+ added `screen-audio.v2`; `5` on `0.3.0`; `4` on `0.2.0`), locale contract (`screen-locale.v1` `"en" | "zh-TW"` vs legacy `crm-locale.v1` on `0.2.0`), and payload byte caps.
2. **Visual & layout rules:** `@cadence/screen-ui` `DESIGN.md`, `src/index.ts`, and `@cadence/screen-ui/styles.css` (plus internal source CSS when working inside the monorepo). Reuse shared primitives (`DataTable`, `Field`, `Detail`, `DrawerShell`, `Loading`, `ErrorNotice`, `Notice`, `EmptyState`, `ConfirmDialog`, `Button`, `Link`, `Select`, `LocaleTextProvider`, `NavigationProvider`) rather than re-creating resets, tokens, or ad-hoc wrappers.
3. **App manifests & scripts:** In a detached app repo, outer repo root (`.`) holds `package.json` (`cadence: { app, dev, check }`), while `./app` is the bundle root containing `app.md`, `workflows/*.md`, `assets/*.svg`, `screens/<tag>/screens.json`, `app-storage.json`, `storage/<tag>.json`, `app-chat.json`, and `skills/<tag>/`.
4. **Hosted storage & canonical guides:** Inspect the hosted target repo's `app-storage/v1` contract (`agenticos-v2` `packages/contracts/src/app-storage.ts`) alongside Cadence guides (`docs/guides/app-development.md`, `docs/guides/app-toolchain.md`, `docs/guides/app-catalog.md`, `docs/guides/app-artifact-release.md`, `docs/guides/app-local-development.md`).

## Choose the App Archetype

| Archetype | Typical Examples | Primary Primitives & Contracts | Key Constraints to Design Around |
| :--- | :--- | :--- | :--- |
| **System-of-Record** | Accounting, CRM, HR, Inventory | `app-storage/v1`, `DataTable`, `Detail`, `Field`, `ConfirmDialog`, `screen-records.v1` | Immutable `migrations` once shipped; integer minor units for money; explicit workflow state transitions; bilingual `en` + `zh-TW`; `320 KiB` CSV record limit (`maxArgsBytes: 327680`). |
| **Media / Capture / Publishing** | Social studio, Meeting Minutes, CMS | `screen-audio.v1`/`.v2`, declared `needs.capabilities` slots (referenced by workflow `publication_slot`), plus workflow `capability_slots` (also declared in `app.md` `needs.capabilities` with `effect: read` or `draft`, referenced by `required_asset_slot` for reviewed binary asset receipts), `cadence app binding` / `effect` | Upload media/audio via dedicated channels (`MAX_AUDIO_UPLOAD_BYTES = 50 MiB`, `200,000` transcript chars, `MAX_ASSET_BYTES` ~`128 KiB` encoded for `96 KiB` images); never stuff media blobs into `16 KiB` action args. |
| **Channel Bridges** | Inbox, Support triage, Notifications | Live pushes (`MAX_PUSH_BYTES = 128 KiB`), `app-chat/v1`, `NavigationProvider` | Idempotent action handlers within bridge/receiver timeouts (`createScreenBridge` `65,000 ms` default, `createScreenReceiver` `DEFAULT_TIMEOUT_MS = 15,000 ms`); clear unread/badge state synced cleanly; keep host-vs-app overlays unambiguous. |
| **Document / Workflow** | Proposals, Approvals, Checklists | `@cadence/app-kit` `ownLeaveState()` / `patchRoute()`, `@cadence/screen-ui` `NavigationProvider` / `DrawerShell`, `screen-surfaces.v1` | Dirty-draft navigation guard via `ownLeaveState()`; `section_query` deep links per screen; never nest app modals/drawers inside a host drawer. |

## Non-Negotiable Pitfalls & Invariants

- **Sandboxed `<form>` submission (`sandbox="allow-scripts"` without `allow-forms`):** Native `<form>` submission and Enter-key submits fail silently or trigger browser sandbox errors inside Cadence's iframe host unless you attach explicit `onSubmit` handlers (`e.preventDefault()`) or install a capture/delegated `submit` listener that calls `event.preventDefault()` and invokes your JS submit handler (or check whether your pinned `@cadence/app-kit` exports a form helper). See [references/bridge-and-ui.md](references/bridge-and-ui.md).
- **Host vs App Drawer Boundary & No Nested Drawers:**
  - The Cadence host owns the board-wide frame, navigation chrome, install consent, and host drawers (`HOST_DRAWER_VERBS`).
  - Keep drawer `context` under `DRAWER_CONTEXT_BYTES` (`1024` bytes) and declare `size: "narrow" | "wide"` cleanly.
  - Never open a second app drawer/modal on top of an active host drawer; check drawer/surface mode (`bridge.getState().surface` or `receiver.surfaceRole()`) before rendering local overlays.
- **Keep `ready` Capabilities Within Pinned Bounds:** `createScreenBridge` (`0.4.0`+, including `0.4.x` and `0.5.0`+) admits up to `6` declared `ready` capabilities (`0.3.0` caps at `5`); legacy `createScreenReceiver` (`0.2.0`) caps at `4`. Always check the installed `app-kit` version before adding a capability.
- **Respect Wire Caps & Timeouts:**
  - Host `screen-records.v1` `action_args` tiers: `16 KiB` (`16,384` B) default (`argBytes` in `createScreenBridge`, including general `whatsapp.*` actions such as `whatsapp.status`, `whatsapp.messages`, `whatsapp.broadcast.prepare`, `whatsapp.broadcast.send`); `48 KiB` (`49,152` B) for `records.create|update|batch`, `content.save`, `screen.preview.show`, `whatsapp.reply.prepare|send`, and `whatsapp.template.prepare|send`; `80 KiB` (`81,920` B) for `works.create|patch`; `192 KiB` (`196,608` B) for `whatsapp.broadcast.draft`; and `320 KiB` (`327,680` B) for `records.csv.preview|import|confirm`.
  - `MAX_STATE_BYTES`: `32 KiB`; `MAX_PUSH_BYTES`: `128 KiB`; `MAX_REPLY_DATA_BYTES`: `1 MiB`; `createScreenBridge` default `timeoutMs`: `65,000` ms (`createScreenReceiver` `DEFAULT_TIMEOUT_MS`: `15,000` ms).
  - Validate malformed/missing host receipts strictly—fail closed or show an explicit error/unavailable state rather than inventing optimistic success.
- **Immutable `app-storage.json` & Money Precision:**
  - Once a storage schema migration has shipped, treat existing `migrations` as append-only immutable history—never edit or delete prior SQL/migration entries; append a new migration instead.
  - Store currency as integer minor units (cents), validate balanced double-entry / `append_only` rules and state transitions server-side. Remember: native `cadence app check <bundle-dir>` enforces `RequireNative` and **refuses** (exit `3`) any package declaring `needs.requires.contracts.app-storage: [1]` (`"this host supports none"`), and native workspace installs refuse `app-storage` packages. Run `cadence app inspect <bundle-dir> --json` (and `frame-kit`/`fixtures --integrity-only`) locally to validate bundle integrity and `app-storage.json` / `storage/<tag>.json` path-presence pairing, and validate the full `app-storage/v1` schema against the hosted target repo (`agenticos-v2` `packages/contracts/src/app-storage.ts`) + hosted `release-check`.
- **Bilingual Copy (`en` + `zh-TW`) & Navigation Parity:**
  - Follow `screen-locale.v1` (`"en" | "zh-TW"`) through `LocaleTextProvider` (or the app's translation dictionary) across headings, navigation tabs, form validation, empty states, and error notices—do not leave raw keys or hardcoded English labels on screens that support both locales.
  - Keep translation key completeness tests covering both locales when tests exist.
  - Configure `navigation.section_query` in `screens.json` intentionally so host tabs/sidebar links synchronize with `@cadence/screen-ui`'s `useNavigation()` and `@cadence/app-kit`'s `bridge.patchRoute()`.
- **Visual Consistency & No Legacy Class Leakage:** Follow `@cadence/screen-ui` `DESIGN.md` compact SaaS density (`28px` desktop / `36px` narrow controls in compact list compositions while keeping shipped `.field` `36px` / `.select-trigger` `32px` form defaults, calm neutral chrome, dark & light token parity, reduced-motion support). Do not introduce new `.crm-*` legacy classes in greenfield apps.
- **No Shell Metacharacters in `package.json` `cadence.dev`:** `cadence.dev` enforces a single command—rejecting newlines (`\n`/`\r`), unescaped backticks, and `$()` command substitution everywhere (even inside quotes) plus unquoted `;`, `&`, `|`, `<`, `>`, `(`, and `)`—whereas `cadence.check` runs via `sh -c`. Put multi-step dev orchestration in a script file.
- **Synthetic Data Only:** All test fixtures, `cadence app fixtures` seeds, docs, and examples must use synthetic, non-sensitive data—never real customer records, credentials, host paths, or commercial secrets.

## Standard Engineering Loop

1. **Inspect pinned contracts & existing manifests** before planning changes (`@cadence/app-kit`, `@cadence/screen-ui`, hosted `app-storage/v1` if used, and `./app/*` manifests).
2. **Author or update manifests & code** (under bundle root `./app`: `app.md`, `workflows/*.md`, `screens/<tag>/screens.json`, `app-storage.json`, `storage/<tag>.json`, optional `app-chat.json` and `skills/<tag>/`, plus TypeScript/UI code using `@cadence/app-kit` + `@cadence/screen-ui`).
3. **Run local dev & isolation harnesses:**
   - Start isolated local serving from repo root `.` via `cadence app dev --package .` (binds `127.0.0.1` ports `3110..=3199` in a cleared environment retaining allowlisted host vars plus injected `CADENCE_APP_*` vars).
   - Exercise iframe bridge behavior locally with `cadence app frame-kit` (strict `https:` or `http://localhost:<port>` / `http://<name>.localhost:<port>` parent-origin validation is enforced at runtime by `@cadence/app-kit`'s `validateBoot()`) and deterministic seed scenarios with `cadence app fixtures`.
4. **Verify static & build gates:**
   - Run the app's typecheck/unit/translation tests plus `package.json` `cadence.check`.
   - Run `cadence app check ./app --json` (targeting the bundle root directory) for non-`app-storage` bundles, or `cadence app inspect ./app --json` plus hosted `app-storage/v1` (`agenticos-v2` `packages/contracts/src/app-storage.ts`) + hosted `release-check` validation when the bundle declares `app-storage: [1]` (where `cadence app check` exits `3` because native Cadence lacks an `app-storage` runtime). Do **not** ship solely on `cadence app inspect` or `--integrity-only` (`frame-kit` / `fixtures`) output without target compatibility verification.
   - Verify loading, empty, error, blocked, success, desktop/narrow viewports, keyboard navigation, dark/light themes, and both `"en"` and `"zh-TW"` locales.
5. **Catalog / Org Actions Only When Authorized:** Never run `install`, `upgrade`, `release-install`, `release-update`, `release-revoke`, `binding`, or `effect` approvals against a live workspace or org unless explicitly instructed.

## References

Read the relevant reference file when working on a specific layer:

- [references/contracts-and-storage.md](references/contracts-and-storage.md) — `package.json` `cadence` block, `app.md`, `screens.json`, `app-storage.json`, `app-chat.json`, and bundled `app/skills/<tag>` package grammar & caps.
- [references/bridge-and-ui.md](references/bridge-and-ui.md) — `@cadence/app-kit` (`createScreenBridge` vs `createScreenReceiver`), payload limits, sandboxed `<form>` handling, `@cadence/screen-ui` `NavigationProvider` / `@cadence/app-kit` `ownLeaveState` / `patchRoute`, drawer contracts, and `@cadence/screen-ui` rules.
- [references/release-and-catalog.md](references/release-and-catalog.md) — Consuming versioned `cadence-app-toolchain` archives, embedded skill access, offline validation vs local/hosted `--org` catalog & artifact release/binding/effect flows.
