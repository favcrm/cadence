# App view contract — v1 (CAD-861, CAD-867 live-read seam)

A **versioned, data-only** grammar for the record views a workspace app
package may describe for the trusted host shell. This directory is the
contract; the board UI under `ui/src/features/app-shell/app-views/` is
its reference consumer (strict validator + one shared renderer +
dev-only preview). Since CAD-864 the package side of the seam exists: a
workspace bundle may carry exactly `views/app-views-v1.json`, declared
by `needs.views.contract: app-views/v1` in `app.md`, installed through
the same verified bundle machinery and served back over the verified
installation receipt. CAD-867 adds a generic, read-only UI for an
installed descriptor paired with its separately pinned `app-bindings/v1`
receipt; full package/UI/actions acceptance remains open.

## Contents

| File | What it is |
|---|---|
| `app-view.schema.json` | JSON Schema (draft 2020-12) for one descriptor: `{contract, app, title, summary?, views[]}`. `contract` is exactly `"app-views/v1"`. |
| `examples/crm.json` | The CRM worked example — customers table, customer detail, disabled customer-form preview. Mirrors `ui/.../app-views/examples.ts`. |
| `examples/social-content.json` | The Social Content worked example — metadata table/detail plus a disabled caption-form preview. Synthetic preview fixtures may include extra fields; the live binding exposes only its closed metadata projection. |

## What a descriptor is

A descriptor declares **views**, not screens and not behaviour. Three
kinds, deliberately small:

- `table` — declared `fields` plus `columns` that may only name
  declared field ids.
- `detail` — declared `fields`; the renderer shows one record's
  label/value pairs.
- `form` — `previewOf`: the fields of the host's *future* create form,
  rendered disabled. A descriptor never carries a submit target or a
  live mutation.

Fields carry `id` (identifier grammar `[a-z][a-z0-9_-]{0,63}`), `label`
(plain text), `format` (`text | number | date | datetime | enum |
tags`), `kind` (`scalar | list`), `values` (required on `enum`,
forbidden otherwise), and `createView` (names a declared `form` view —
a hint for the host to wire its own create surface, never a route the
package controls).

In v1, lists support **text and tags only**; numeric, date, datetime
and enum fields are scalar-only. Calendar values must exist (including
Gregorian leap-year rules, years 0001–9999). Datetimes require an explicit
`Z` or signed timezone, hours 00–23 and minutes/seconds 00–59; fractional
seconds require a seconds component. Leap seconds are not supported.

## What a descriptor can never carry

Enforced recursively at every object, by both the schema
(`propertyNames`) and the consumer (`scanUnsafe`):

- **Executable surfaces**: `script`, `code`, `html`, `innerHTML`,
  `css`, `style`, `javascript`, `eval`, `import`, `module`, and the
  prototype-pollution keys `__proto__`/`prototype`/`constructor`.
- **URL/navigation escapes**: `url`, `uri`, `href`, `src`, `link`,
  `action`, `endpoint`. The renderer builds no anchors from descriptor
  data, so there is no legitimate place for these.
- **Scope, actor, or authority**: `install_id`, `context_id`,
  `workspace`, `project`, `project_link`, `actor`, `by`, `role`,
  `grant`, `scope`/`scopes`, `capability`/`capabilities`, `effect`/
  `effects`, `verified`, `digest`, `revision`. Identity comes from the
  trusted route plus verified receipts — a package can never choose an
  installation or context, claim an actor, or assert an effect.
- **Credentials and storage internals**: `secret`/`secrets`,
  `credential`/`credentials`, `token`, `password`, `sql`, `query`,
  `path`, `file`.

Beyond forbidden keys, the consumer enforces (schema cannot): unique
view ids and field ids, columns naming only declared fields, `enum`
requiring `values`, `tags` requiring `kind: "list"`, `createView`
naming a declared `form` view, bounded string lengths, bounded
view/field/column/row counts, a serialized-size ceiling, and shared
node-count and nesting-depth bounds, checked before JSON serialization.
The serialized-size ceiling counts UTF-8 bytes, not JavaScript string length.

A script-looking *string* inside a legitimate field — e.g. a customer's
name typed as `<script>alert(1)</script>` — is allowed and renders as
inert visible text; the contract's job is that no string can become an
executable element, event-handler attribute, navigation URL, or code. Safe
identifiers and labels may populate inert DOM/ARIA attributes.

## Fixture rows (dev preview only)

`fixtureRows` are **not** descriptor content and a package never ships
them. The dev preview's host code supplies synthetic rows keyed by
declared field ids and validates them through the strict gate. Installed
reads use the separate `liveRows` bound and the paired binding's field
allowlist; only mapped fields are rendered, unknown keys refuse, enum
values must be declared, and strings remain bounded text. Preview
fixtures stay synthetic and are never used as live-read fallbacks.

## Versioning

Same rule as `connected-platform`: the path is the version.
`contracts/app-views/v<N>/` is additive-or-clarifying only inside a
version; a renamed field, tightened enum or changed semantic ships as
`v<N+1>/` alongside. The `contract` field inside the descriptor mirrors
the directory (`"app-views/v1"`), and the consumer refuses any other
tag.

## How consumers use it

- **Board UI** (`ui/src/features/app-shell/app-views/`):
  `parseAppView` and `parseAppBinding` bound and cross-check the receipt's
  paired descriptor/binding; `viewReceipt.ts` retains the bundle and both
  file pins. `LiveAppView` routes declared table/detail ids through the
  operator-gated bound-read HTTP peer, passes the host-owned active context
  and all three pins, aborts superseded fetches, and renders only mapped
  fields with shared table/detail/cell primitives. The dev-only
  `AppViewContractPreview` still uses synthetic fixtures behind
  `?contract-preview=...`; it is not the live renderer.
- **Tests**: `ui/tests/appViewContract.test.ts` parses the schema-side
  examples, checks full parity with the renderer descriptors, asserts
  malformed/forbidden/oversized and non-JSON descriptors refuse, and
  mounts the renderer over both fixtures to check text-only output.
  Running these tests is validation evidence, not supplied by this document.
- **Package loader** (`src/issue/app_view.rs`, CAD-864):
  `parse_descriptor` is the Rust-side gate, mirroring every rule here
  rule-for-rule — forbidden keys recursively, unknown keys, identifier
  grammar, format/kind coherence, column and `createView`
  cross-references, and the same bounds (16 views, 24 fields, 12
  columns, 24 enum values, 64/80/120/280-char strings, 4 096 nodes,
  depth 24, 64 KiB serialized). Install and upgrade validate the bytes
  before they join the bundle; the workspace receipt re-parses the
  installed bytes on every read so a hand-edited installed descriptor
  can never be served as reviewed.

## The bundle seam (CAD-864)

- A package declares the descriptor in `app.md`:
  `needs: {views: {contract: app-views/v1}}` — a declaration, never a
  locator. The contract value is exactly `app-views/v1`; any other value
  (and any other `needs.views` key) refuses.
- The descriptor lives at exactly `views/app-views-v1.json` — one file,
  flat, the filename itself pinning the contract version. Any other
  name under `views/` (or a nested dir) refuses at scan time.
- Declaration and file are paired: either alone refuses install.
  `descriptor.app` must equal `app.md`'s `app` — the descriptor was
  reviewed with this bundle and never names an installation.
- Descriptor bytes are inside the bundle digest on both install
  transports (legacy `file.<rel>` arm; workspace `bundle_digest`), so
  any byte change is a structural change that re-gates approval, and
  the journaled install/upgrade apply paths re-validate it — a forged
  journal carrying a bad descriptor cannot apply.
- The verified workspace receipt (`app_workspace_show`, and its
  operator-gated `GET /api/app-installations/<id>` peer) carries
  `view_descriptor` (the validated JSON) and
  `view_descriptor_digest`. When present, the paired `app-bindings/v1`
  companion and `view_binding_digest` come from the same verified snapshot;
  descriptor-only bundles remain compatible and legacy project installs
  are unchanged.

## Remaining acceptance

The current UI seam is deliberately narrow: only receipt-paired `table` +
`detail` views with bound `list`/`show` reads render live; a declared
`app-actions/v2` CRM companion may additionally draw host-owned create/update
forms. The v1 form preview itself remains inert. Full CAD-867 acceptance remains
open pending real CRM/Social installation and context checks, scope/chat
preservation, independent/native action-guard review, and desktop/narrow QA
against the installed packages. The current fixture and synthetic-browser
checks are not full acceptance; this implementation does not claim broader
CAD-811 write support.
