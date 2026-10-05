# App bindings contract — v1 (CAD-867, toward CAD-811)

A **versioned, data-only** companion to
[`app-views/v1`](../../app-views/v1/README.md). Where a view
descriptor declares *shapes* (tables, details, disabled form
previews), a binding file declares *which host read source* serves
each declared view live — without ever letting a package choose an
installation, actor, connection, query, or method. This directory is
the contract; the package-side gate is `src/issue/app_binding.rs`
(`parse_binding` + `validate_against`), which mirrors this grammar
rule-for-rule and fails closed.

A workspace bundle may carry exactly `bindings/app-bindings-v1.json`,
declared by `needs.bindings.contract: app-bindings/v1` in `app.md`,
validated through the same verified bundle machinery and served back
over the verified installation receipt. The companion **requires** the
descriptor it maps: a bundle with `needs.bindings` but no
`needs.views` (or a `bindings/` file with no `views/` descriptor)
refuses install — the binding is meaningless without the views it
names. A descriptor-only `app-views/v1` package still installs
unchanged: the companion is optional in one direction only.

## Contents

| File | What it is |
|---|---|
| `app-bindings.schema.json` | JSON Schema (draft 2020-12) for one binding file: `{contract, app, title, bindings[]}`. `contract` is exactly `"app-bindings/v1"`. |
| `examples/crm.json` | The CRM companion — maps the customers table and customer detail of `contracts/app-views/v1/examples/crm.json` onto the `customers` source over real `CustomerProfile` fields. |
| `examples/social-content.json` | The Social Content companion — maps the `subject` field on caption-run table/detail views to optional `snapshot.inputs.subject` metadata from `app_run_show`; it does not expose artifact content. |

## What a binding file is

`bindings[]` maps **descriptor view ids** to **host read sources**:

```json
{
  "contract": "app-bindings/v1",
  "app": "crm",
  "title": "CRM bindings",
  "bindings": [
    {
      "view": "customers",
      "source": "customers",
      "ops": ["list"],
      "fields": [
        { "field": "name", "key": "display_name", "format": "text" },
        { "field": "email", "key": "email", "format": "text" },
        { "field": "tags", "key": "tags", "format": "tags" },
        { "field": "consent_email", "key": "consent.email", "format": "enum" }
      ]
    }
  ]
}
```

- `view` — a view `id` the same bundle's `views/app-views-v1.json`
  declares. Only `table` and `detail` views may be bound; a `form`
  view is a disabled preview and can never carry a binding.
- `source` — the closed set of host read sources:
  - `customers` — the record store's `CustomerProfile` (kind
    `customer`): `record_id` (the row's `id` — the adapter renames it
    to this handle; the raw API emits `id`), `display_name`, `email`,
    `phone`, `tags`, `source`, `consent.email`, `consent.sms`.
    Consent values are `granted`/`denied`/`unknown`; `consent.sms`,
    `email`, `phone` and `source` may be absent on a record and the
    adapter omits the cell entirely (missing renders "—", never a raw
    `null`).
  - `caption-runs` — the run row's metadata-only surface
    (`app_run_show`): `id`, `state`, `context_id`, `snapshot_digest`,
    `snapshot.workflow.title`, `snapshot.inputs.subject`,
    `snapshot.context.id`. Run `state` is one of
    `awaiting_approval`/`approved`/`running`/`succeeded`/`failed`/
    `cancelled`. `context_id`/`snapshot.context.id` are absent on a
    contextless run and `snapshot.inputs.subject` is absent on
    workflows that declare no `subject` input (e.g.
    `source-instagram` declares `profile_handle`) — the adapter omits
    those cells. Artifact ids, digests, media types, sizes and —
    above all — **artifact content** (the caption text itself) are
    deliberately not in this projection. Run `created`/`updated`
    columns exist in storage but are **not** part of the `app_run_show`
    API projection, so no binding key may name them.
- `ops` — the closed read operations the binding admits: `list` (a
  `table` view's read) and `show` (a `detail` view's). A binding may
  declare one or both; an op unusable by the view's kind refuses.
- `fields[]` — per descriptor field id: the dotted `key` into the
  source's fixed projection and the `format` the host renders it
  with. `field` must equal a field `id` declared on that view;
  `format` must equal the descriptor field's `format` *and* be one
  the produced value can honestly fill, and the produced shape must
  match the descriptor field's `kind` (`scalar`/`list`). The honest
  pairings are closed: scalar string/digest producers fill only
  `text`; `tags` fills only `format:"tags", kind:"list"`; consent
  and run-state fill only `enum` and the descriptor `values` must
  equal the produced domain exactly. A scalar producer can never
  claim `number`, `date`, `datetime`, `enum` or a `list` kind — the
  host's cell consumer would refuse the value at render.

`app` must equal both the manifest's `app` and the descriptor's `app`
— all three documents are reviewed as one bundle and the binding can
never borrow another app's views or identity.

## What a binding file can never carry

Everything `app-views/v1` forbids — executable surfaces
(`script`/`code`/`html`/`eval`/`import`/prototype-pollution keys),
URL/navigation escapes (`url`/`href`/`src`/`link`/`action`/`endpoint`),
scope/actor/authority (`install_id`/`context_id`/`workspace`/
`project`/`actor`/`by`/`role`/`grant`/`scope`/`capability`/`effect`/
`verified`/`digest`/`revision`), credentials (`secret`/`credential`/
`token`/`password`) and storage internals (`sql`/`query`/`path`/`file`)
— plus the **invocation vocabulary** that would turn a data mapping
into behaviour: `method`, `call`, `rpc`, `tool`, `command`, `exec`,
`args`, `binding`, `connection`, `account`, `slot`, `request`,
`fetch`, `body`, `params`, `where`, `order_by`, `limit`, `cursor`,
`write`, `update`, `delete`, `send`, `approve`, `dispatch`, `run_id`,
`artifact`, `artifacts`, `snapshot`, `record`, `store`, `migration`.

A package therefore cannot select an installation (the verified
receipt's `install_id` is the host's), a context (the caller's),
a connection or account (the operator's bindings), a method (the
source name is a host adapter key, not an RPC name), a filter or
pagination (the host bounds reads), or any write — `list`/`show` are
the only verbs v1 knows.

Beyond forbidden keys, the gate enforces: unique bound view ids, ops
drawn only from `{list, show}` and usable by the view kind, fields
naming only declared descriptor field ids, `key` membership in the
source's reviewed projection table, format/type compatibility with
the produced shape, enum-domain equality, bounded string lengths, and
shared node-count/depth/serialized-size bounds checked before parse.
The serialized-size ceiling counts file bytes.

## The bundle seam (CAD-867)

- Declared in `app.md`:
  `needs: {bindings: {contract: app-bindings/v1}}` — the contract
  value is exactly `app-bindings/v1`; any other value (or any other
  `needs.bindings` key) refuses.
- Exactly `bindings/app-bindings-v1.json` — one file, flat; any other
  name under `bindings/` refuses at scan time.
- Declaration and file pair up — either alone refuses install.
  `needs.bindings` additionally requires `needs.views`, and the file
  requires its descriptor present and valid.
- `binding.app` = `descriptor.app` = `manifest.app` — one bundle's
  identity, never borrowed.
- Binding bytes are inside the bundle digest, so a byte change is a
  structural change that re-gates approval; the journaled
  install/upgrade apply paths re-validate it — a forged journal
  carrying a bad binding cannot apply.
- The verified workspace receipt (`app_workspace_show`, and its
  operator-gated `GET /api/app-installations/<id>` peer) carries
  `view_binding` (the validated JSON) and `view_binding_digest` —
  both present together, or both `null` when the bundle ships no
  companion. They are re-parsed from the SAME installed snapshot as
  the descriptor on every read, so a torn or stale pair is never
  served.

## The live read (CAD-867)

`app_view_read` is the operator-only RPC that serves a bound view's
**live** rows; the HTTP peer
`GET /api/app-installations/<install>/views/<view>/rows[/<record>]`
mirrors it under the operator read gate. The caller never names a
host `source`, query, actor or installation authority — it carries
the installation, the descriptor `view` id, the read `op`, and the
three pins it was authorized under. `source` and the allowed `op`
come from the *installed binding*, never the request.

**Request** — the only admitted fields:

| field | shape | when |
|---|---|---|
| `install_id` | identifier | always |
| `view_id` | descriptor view id | always |
| `op` | `"list"` \| `"show"` | always |
| `digest`, `view_descriptor_digest`, `view_binding_digest` | `sha256:<64hex>` | always — must equal the ONE verified snapshot's, else refused |
| `context_id` | identifier | required by `customers`; optional/live-proved on `caption-runs` |
| `record_id` | identifier | required on `show`; never on `list` |
| `query`, `limit` (1..=100), `cursor` | bounded | `customers` `list` only; refused on `show` and on `caption-runs` |

**Response** — a uniform envelope. `rows[]` is an array for **both**
`list` and `show` (show carries one row); each row is keyed by
descriptor `field` id. The reply echoes `view_id`, `op` and all three
pins it was authorized under; the `customers` `list` additionally
carries `truncated` and `next_cursor`.

**Read span & pins** — `with_runtime_snapshot` holds the PM lock for
the entire read: the digest triple is compared AND the typed producer
read runs inside the same callback, so a racing upgrade is ordered
before or after the whole read — never torn inside it. Rows are
projected from **typed, closed** producer maps (a `CustomerProfile`
deserialization for customers; a validated snapshot object whose
`material_digest` matches for runs), never a generic raw-JSON walk. A
**corrupt producer row fails the whole read** — a present-but-
wrong-typed optional field (`subject`, `workflow.title`,
`context.id`, `context_id`) is a bounded `rejected`, not a silent
omission; a genuinely absent/nullable optional omits the cell.

**Refusals** — a missing/wrong/stale/dup digest, an unknown or
unbound `view_id`, an op the binding does not admit, an out-of-bounds
selector, a corrupt or wrong-scoped row, and any write-shaped or
unknown field all fail closed: the RPC returns `rejected`; the HTTP
peer maps schema/binding refusals to `400`, an operator-gate refusal
to `403`, everything else to `503`.

## Remaining CAD-867/CAD-811 work (not this increment)

- The live renderer/adapter that consumes `rows[]` against the
  declared view shape.
- Typed forms and action verbs: `form` views stay disabled previews —
  no executable binding, no submit target.
- Actor/scope/revision-bound writes and the per-package domain
  stores/migrations of CAD-811.
