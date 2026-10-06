# App actions contract — v2 (CAD-867)

`app-actions/v2` is a strict, data-only companion for explicitly declared
live forms. Its exact bundle path is
`actions/app-actions-v2.json`, declared by
`needs.actions.contract: app-actions/v2` in `app.md`. This first installed
adapter is deliberately closed to CRM customer create/update; it is not a
generic action engine. V1 action metadata remains inert.

An installed v2 action bundle must also declare and carry
`app-views/v1` and `app-bindings/v1`. Each action's `form_view` must resolve to a form in that same descriptor,
and its input fields must match that form's `previewOf` fields one-for-one
(id, label, format, list kind and enum values).
The v1 form previews remain disabled: the host draws a separate controller
from the validated v2 action fields and never submits through either preview
component.

## Files

- `app-actions.schema.json` — draft-2020-12 closed descriptor shape.
- `examples/crm.json` — the paired CRM create/update declarations.
- `src/issue/app_action_v2.rs` — bounded parser and same-snapshot cross-check.
  The v1 parser and grammar are unchanged.

## Closed CRM adapter

The descriptor must contain exactly these actions:

- `customer.create` / `record.create` / a declared form view
- `customer.update` / `record.update` / a declared form view

The CRM example pairs create and update with separate inert v1 previews.
The reference is descriptor-declared, not restricted to fixed form IDs; the
host draws its create/edit controller only after validating that exact form's
field shape against the action.

Both use these host-owned customer fields, in order:

| Field | Type | Required | Nullable |
|---|---|---:|---:|
| `display_name` | text | yes | no |
| `email` | text | no | yes |
| `phone` | text | no | yes |
| `source` | text | no | yes |
| `tags` | tags | no | no |

The declared limits are requests, never authority to exceed `CustomerProfile`
validation. The host enforces its own bounds even when the package claims a
larger limit, and enforces a smaller declared cap as a further restriction.
Only `email`, `phone` and `source` have nullable/clear semantics. Consent,
identity, source selection, revision, hidden defaults, code, URLs, SQL,
endpoints and arbitrary input keys are not part of this contract.

The parser retains the v1 bounds (16 actions, 64 fields/action, 24 enum
values, 64 KiB serialized JSON, 4096 nodes, nesting depth 24) and adds only
`form_view` and `nullable`. `nullable: true` is valid only on an optional text
field. `maxLength` is supported only on text/tags; `maxItems` only on tags.
The installed CRM adapter further rejects unsupported action IDs, records,
undeclared or non-form references, field shapes and mismatched preview fields.

## Trusted write wire

The UI's receipt is additional client defense, not authority. Every write is
operator-only at both HTTP and daemon boundaries. The daemon reparses actions,
view and binding from one verified bundle snapshot while holding the PM and
release lock span; the action bytes are already covered by the bundle digest,
so there is no fourth action pin.

RPC method: `app_view_action`.

Create:

```http
POST /api/app-installations/{install}/contexts/{context}/views/customer-create-form/actions/customer.create
Content-Type: application/json

{"digest":"sha256:<bundle>","descriptor":"sha256:<view>","binding":"sha256:<binding>","input":{"display_name":"Ada"}}
```

Update:

```http
POST /api/app-installations/{install}/contexts/{context}/views/customer-edit-form/actions/customer.update/records/{record}
Content-Type: application/json

{"digest":"sha256:<bundle>","descriptor":"sha256:<view>","binding":"sha256:<binding>","expected_revision":7,"input":{"display_name":"Ada","email":null,"tags":[]}}
```

The route supplies installation, context, form, action and (for update) record
identity. The request body accepts only the three digest pins, typed `input`,
and update-only `expected_revision`. It never accepts an actor, operation,
source, consent, input identity, or client-chosen create record ID. The RPC
uses `install_id`, `context_id`, `view_id`, `action_id`,
`view_descriptor_digest`, `view_binding_digest`, `digest` and `input`; update
also carries `record_id` and `expected_revision`.

Success returns the native host record receipt plus the three echoed pins:
`{"record":{...},"digest":"...","view_descriptor_digest":"...","view_binding_digest":"..."}`.
The record receipt carries its host-minted id, install/context scope, customer
kind, revision, profile and consent provenance. The host validates the entire
receipt and pins before the UI follows the returned record.

## Mutation semantics

- Create requires `display_name`; absent/null/blank optional text is absent,
  omitted tags become `[]`, and consent starts as unknown (SMS absent). The
  daemon mints a `cust-` UUID id.
- Update requires a host-captured positive `record_revision` from a current
  customer detail read. Missing `display_name`/null refuses. Omitted optional
  values stay unchanged; null or blank clears only nullable text; omitted tags
  stay unchanged and `tags: []` clears them.
- Update merges into the persisted `CustomerProfile`, copying consent unchanged
  from the current record. It cannot grant consent or supply provenance. The
  native record-store expected-revision transaction is the CAS, so concurrent
  updates with one revision cannot both win.
- The bundle, descriptor and binding pins, declared action/form, context
  ownership, operator connection, typed fields and CAS are re-proved by the
  daemon under one lock span. A stale or foreign request refuses without a
  write. The board's operator gate is at least as strict as that RPC.

This contract/example does not claim full CAD-867 acceptance. Real CRM and
Social contexts, desktop/narrow browser behavior, independent adversarial
RPC/HTTP acceptance, and browser QA remain separate gates.
