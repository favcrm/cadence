# App view contract — v1 (CAD-861, toward CAD-811)

A **versioned, data-only** grammar for the record views a workspace app
package may describe for the trusted host shell. This directory is the
contract; the board UI under `ui/src/features/app-shell/app-views/` is
its reference consumer (strict validator + one shared renderer +
dev-only preview). Both sides of the seam live in this repo today; the
package side of the seam — a workspace bundle shipping a descriptor
file, the install validator accepting it, the host loading a *verified*
descriptor for a real installation — is later CAD-811 work and is **not**
delivered here.

## Contents

| File | What it is |
|---|---|
| `app-view.schema.json` | JSON Schema (draft 2020-12) for one descriptor: `{contract, app, title, summary?, views[]}`. `contract` is exactly `"app-views/v1"`. |
| `examples/crm.json` | The CRM worked example — customers table, customer detail, disabled create-form preview. Mirrors `ui/.../app-views/examples.ts`. |
| `examples/social-content.json` | The Social Content worked example — caption-run table, run detail, disabled caption-form preview. A contract example inside any installation, never an installed Social surface. |

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
declared field ids and validates them through the same strict gate:
unknown keys refuse, enum values must be declared, strings are length-
and control-character-bounded. Today the rows are literal in
`examples.ts`; when live records exist they will flow through the same
projection — descriptor declares the shape, the host picks the data.

## Versioning

Same rule as `connected-platform`: the path is the version.
`contracts/app-views/v<N>/` is additive-or-clarifying only inside a
version; a renamed field, tightened enum or changed semantic ships as
`v<N+1>/` alongside. The `contract` field inside the descriptor mirrors
the directory (`"app-views/v1"`), and the consumer refuses any other
tag.

## How consumers use it

- **Board UI** (`ui/src/features/app-shell/app-views/contract.ts`):
  `parseAppView` fails closed on the first violation with a dotted path
  (`AppViewContractError`); `fixtureRows` validates record-shaped data
  against a parsed view; `AppView` renders one view at a time as React
  text; `AppViewContractPreview` mounts the worked examples behind
  `?contract-preview=crm|social-content`, dev-only. Direct first-mount links
  and same-installation view changes preserve preview state; changing the
  installation clears both preview query keys and opens its normal outlet.
- **Tests**: `ui/tests/appViewContract.test.ts` parses the schema-side
  examples, checks full parity with the renderer descriptors, asserts
  malformed/forbidden/oversized and non-JSON descriptors refuse, and
  mounts the renderer over both fixtures to check text-only output.
  Running these tests is validation evidence, not supplied by this document.
- A future package loader must run `parseAppView` (or an equivalent
  schema check plus the consumer rules listed above) on descriptor
  bytes *before* any descriptor reaches the renderer — the renderer
  assumes validated input.

## Remaining CAD-811 integration (not this increment)

- A bundle-side descriptor file (name/format inside the A1 bundle) and
  the install validator accepting it — today's `app.md` front matter
  refuses `ui`/`schema`/`actions` keys, so descriptors are not
  installable yet.
- The host loading a descriptor from a *verified installation receipt*
  (not from this dev fixture map) and wiring `createView` to the host's
  real record-create path.
- Live record projection: `fixtureRows` becomes a fetch through the
  existing scoped host-actions client; the descriptor keeps declaring
  only shape, never query or path.
