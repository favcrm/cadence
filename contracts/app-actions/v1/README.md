# App actions contract — v1 (CAD-964, toward CAD-811)

A **versioned, data-only** grammar for the record mutations a
workspace app package may *describe* for the trusted host. This
directory is the contract; `src/issue/app_action.rs` is its strict
reference validator. Since CAD-1415 a bundle may carry this file as
`app-actions.json` at its root: the installer admits it under the
stricter rules in "Admission and execution today" below, and the host
runs a declared action only as an operator-approved, allow-once
proposal. The manifest's `actions`/`needs.actions` frontmatter keys stay
gated (`GATED_KEYS`) and are still refused.

## Contents

| File | What it is |
|---|---|
| `app-actions.schema.json` | JSON Schema (draft 2020-12) for one descriptor: `{contract, app, title, summary?, actions[]}`. `contract` is exactly `"app-actions/v1"`. |
| `examples/crm.json` | The CRM worked example — `customer.create` and `customer.update` on the `customer` record kind, the first CRM customer-form case. |
| `examples/ledger.json` | A second, synthetic app (`ledger`, `account` record kind) — included so the grammar is proven not to be keyed to the CRM. |

## What a descriptor is

A descriptor declares **actions**, not behavior and not authority.
Each action is a closed declaration:

- `id` — a dotted identifier like `customer.create`
  (`[a-z][a-z0-9_-]{0,63}` segments, up to 4), unique within the
  descriptor. Data only — never a route.
- `title` — plain-text label.
- `operation` — a **closed semantic reference** to a host operation,
  from a fixed vocabulary. v1 knows exactly `record.create` and
  `record.update` — the smallest set covering the CRM customer-form
  case. `record.delete`, `custom`, `send`, an HTTP method, a URL or a
  SQL fragment are all refused; there is no arbitrary-endpoint escape.
- `record` — a reference to a **record kind** the package declares
  elsewhere (a future `domain/` contract — see "Remaining
  integration"). This descriptor only records the kind's *name* in the
  identifier grammar; it does not and cannot assert that the kind
  exists.
- `input.fields` — closed typed field definitions the host's own form
  controls would render. Each field has `id`, `label`, `type`
  (`text | number | date | datetime | enum | tags` — the same
  vocabulary the shared `Field` controls and `app-views/v1` use),
  `required`, and optional `values` / `maxLength` / `maxItems` bounds.
  `values` is required on `enum` and forbidden on every other type.

Field ids are unique within an action; action ids are unique within
the descriptor. Strings, counts, and the serialized byte size are all
bounded.

## What a descriptor can never carry

Enforced recursively at every object, by both the schema
(`propertyNames`) and the validator (`scan`):

- **Executable surfaces**: `script`, `code`, `html`, `innerHTML`,
  `css`, `style`, `javascript`, `eval`, `import`, `module`, and the
  prototype-pollution keys `__proto__`/`prototype`/`constructor`.
- **URL/navigation/transport escapes**: `url`, `uri`, `href`, `src`,
  `link`, `endpoint`, `method`, `route`. An action is never an HTTP
  call the package names.
- **Scope, actor, or authority**: `install_id`, `context_id`,
  `workspace`, `project`, `actor`, `by`, `role`, `grant`,
  `scope`/`scopes`, `capability`/`capabilities`, `caller`, `effect`/
  `effects`, `verified`, `digest`, `revision`. Identity is derived
  from the trusted route and verified receipts — a package can never
  choose an installation, claim an actor, or assert an effect.
- **Guard-weakening toggles**: `revision_pin`, `approval_pin`,
  `actor_request`, `guard`, `public`, `unauthenticated`,
  `skip_approval`. The operator-write floor, the context/digest checks
  and the `expected_revision` compare-and-set on `update` are
  **host-enforced and mandatory** — they are not optional booleans a
  package may switch off (or on to claim it opted in). A declaration
  can only ever narrow authority, never widen it.
- **Credentials and storage internals**: `secret`, `credential`,
  `token`, `password`, `sql`, `query`, `path`, `file`.
- **Arbitrary constraint languages**: `default`, `pattern`, `regex`,
  `expression`, `formula`, `schema`, `properties`, `items` — the field
  grammar is closed and typed, not a general JSON-Schema subset and
  not an expression evaluator.

The recursive scan runs **before** shape validation, so a hostile
payload cannot smuggle a forbidden key past the shape checks inside a
nested shell. Node-count, nesting-depth and serialized-byte budgets
are enforced the same way.

## Authority is the host's, always

This contract deliberately separates **declaration** from **power**.

- **The actor is derived, never declared.** Who may invoke an action
  is decided by the host from the connection (`caller_rule`) and the
  operator's grants — a package cannot request `operator`,
  `public-token`, or any actor class, and cannot even *name* one.
- **Guard floors are mandatory and invisible to the package.** An
  `update` carries a host-mandated expected-revision CAS; an
  effect-bearing verb carries a host-mandated digest-pinned approval.
  Because these are enforced by the operation's semantics, no field in
  this contract toggles them — hence `revision_pin`/`approval_pin`/
  `actor_request` are forbidden keys, not optional booleans.
- **Public unsubscribe stays a separate host-custodied token path.**
  `crm_unsubscribe_redeem`'s authority is the token itself; it is not a
  new action class a package declares here, and the closed operation
  vocabulary gives it no surface.
- **The parser admits data, not authority.** `parse`/`parse_str` in
  `src/issue/app_action.rs` answer only "is this a well-formed v1
  descriptor?" They execute nothing, install nothing, and grant
  nothing. Admission and execution are separate host steps (next
  section); the file is covered by the bundle digest the operator
  approves, like every other bundle file.

## Versioning

Same rule as `app-views`/`connected-platform`: the path is the
version. `contracts/app-actions/v<N>/` is additive-or-clarifying only
inside a version; a renamed field, a widened operation enum, or a
changed semantic ships as `v<N+1>/` alongside. The `contract` field
mirrors the directory and the validator refuses any other tag.

## How consumers use it

- **Validator** (`src/issue/app_action.rs`): `parse(&Value)` /
  `parse_str(&str)` fail closed on the first violation with a dotted
  path in the message. They return a fresh, typed `Descriptor` —
  callers downstream of validation work from the typed form, not the
  raw JSON.
- **Tests**: `tests/app_actions_contract.rs` validated every published
  example against the schema and the parser, with refusal vectors for the
  forged-authority, duplicate-id, wrong-tag, unsupported-operation,
  enum-coherence and bound cases. CAD-1073 retired that exhaustive suite;
  the retained CAD-1415 acceptance
  (`src/daemon/cad1415_app_actions_acceptance.rs`) exercises registered
  installs and actions through the production dispatch path, not every
  published grammar example.
- Every consumer runs the validator on descriptor bytes *before* it
  reads a descriptor; downstream code assumes validated input.

## Admission and execution today (CAD-1415)

- **Admission** (`validate_customer_bundle`, `src/issue/app_action.rs`):
  the descriptor's `app` must equal the manifest's app; every action must
  use the `customer` record kind, the only kind the host storage holds;
  every field must be one of the legacy customer fields with its exact
  type (`display_name`, `email`, `phone`, `source`: `text`; `tags`:
  `tags`); and no action id may collide with a host-registry assistant
  id. Any violation refuses the install.
- **Execution** (`src/daemon/app_record_actions_rpc.rs`): an agent
  invocation in a scoped chat turn only validates the input and parks a
  `pending_permission` operation with a host-drawn preview. The store
  changes only on the operator's native `allow_once` decision, after the
  operator proof, a re-derived operation digest, the same approved
  declaration and the expected record revision. There is no standing
  permission. Create refuses an existing id; update merges only the
  declared fields and preserves consent.

## Remaining CAD-811 integration

- **Domain kinds**: `record` is a forward reference to a record kind
  declared in a future `domain/` contract. There is no domain registry
  yet, so the validator checks only that the name is well-formed — it
  performs no fake cross-validation against undeclared kinds.
  Cross-checking `record` against a declared `domain/` schema is a
  requirement of that future admission slice, not of this contract.
- **More kinds and operations**: kinds beyond `customer`, and any
  operation beyond `record.create`/`record.update`, need that domain
  contract and a new host handler first.
- **Effect verbs**: no declared action can carry an outward effect in
  v1 — publication/send stays on the `CapabilityNeed`/`needs.
  capabilities` seam and its own gate.
