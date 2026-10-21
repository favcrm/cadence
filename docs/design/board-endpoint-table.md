# Board endpoint table: inventory, precedence audit and migration order

Status: **spike**, CAD-1664. No code in this document is shipped; it exists to
decide whether the board's two hand-ordered dispatch chains can be replaced by
one declared table without changing which handler wins for any existing path.

## 1. What exists

| Piece | Location | Shape |
|---|---|---|
| Read chain | `src/ui/serve.rs` `handle` | 25 top-level arms; 12 `strip_prefix` parsers; 68 `/api` literals |
| Write chain | `src/ui/write_path.rs` `write_route` | 21 top-level arms; 5 `strip_prefix` parsers; 44 `/api` literals |
| Write class table | `src/ui/operator.rs` `WRITE_ROUTES` | 189 rows, fail-closed default `OperatorOnly` |
| Per-area parsers | `src/ui/<area>.rs` `route()` | 20 modules; return a typed `Route` enum or `None` |

The counts are a static grep over this revision, not a runtime census; every
line reference was checked against the same tree.

Adding one endpoint touches eight places: the area's `route()` arm, the body
struct, the handler, the dispatch arm in one chain, a `WRITE_ROUTES` row, the
TypeScript type, `api.ts`, and the docs row. The dispatch arm and the class row
agree only by convention; the chains' order is load-bearing and documented in
comments (`src/ui/serve.rs:2073`, "ordered before every
`/api/app-installations/` arm").

## 2. Arm inventory

### 2.1 Read chain (`serve.rs`, in order)

| Line | Form | Notes |
|---|---|---|
| 1243 | `GET /unsubscribe/{token}` | before every session gate |
| 1310 | `/__platform/*` | platform surface |
| 1330 | `strip_prefix /api/cli/` | passthrough envelope |
| 1339 | `strip_prefix /api/wiki/` | read envelope |
| 1360 | `strip_prefix /api/assist/` | |
| 1368 | `strip_prefix /api/internal/notifications/` | |
| 1380 | `strip_prefix /api/member-assistant/` | |
| 1389 | `strip_prefix /api/human-threads` | checked again on write side |
| 1993 | `strip_prefix /api/wiki/` | second wiki arm, later in the chain |
| 2001 | `strip_prefix /api/projects/` | project + workflow reads |
| 2241 | `/api/app-installations/{id}/conversations` | suffix-filtered |
| 2257 | `/api/app-installations` and `/{id}` | one block, two classes (list vs show) |
| 2287 | `/api/app-catalog*`, `/api/app-home`, `/api/app-favorites`, `/api/app-requests` | OR-group, one handler |
| 2312 | `strip_prefix /api/memories/` | |
| 2337-2348 | `/api/master/models`, `/summary`, `/state` | three exact arms |
| 2396 | `strip_prefix /api/agents/` | |
| 2414 | `strip_prefix /api/issues/` | |
| 2094-2228 | `read_admit` arms | 10 modules via the one admit path (CAD-1648) |

### 2.2 Write chain (`write_path.rs`, in order)

| Line | Form |
|---|---|
| 994, 1010 | `/api/session`, `/api/session/logout`, `/api/session/device/{code,poll}` |
| 1025, 1060 | `/api/setup`, `/api/update`, `/api/update/check` |
| 1073, 1086, 1095, 1108 | `/api/settings/agents`, `/api/settings/model-defaults`, `/api/locale`, `/api/reports` |
| 1125, 1168 | `strip_prefix /api/monitors/`, `/api/memories/` |
| 1438 | `/api/app-installations/{id}/conversations` |
| 1462 | OR-group: 7 exact explorer paths plus `starts_with /api/app-installations/` **and** `ends_with` one of `/update-check`, `/remove-preview`, `/remove`, `/restore` |
| 1507 | `/api/app-installations/migrate`, `/{id}/recover`, `/{id}/upgrade`, `/{id}/upgrade/check`, `/{id}/upgrade/recover`, `/migrations/{id}/recover` |
| 1537, 1547 | `/api/app-installations/check`, `/api/app-installations` |
| 1647 | `/api/app-installations/upload`, `/upload/check` |
| 1732, 1745 | `/api/master/command`, `/api/chat/upload` |
| 1786 | `/api/topics` |
| 1844, 1858 | `strip_prefix /api/issues/`, `/api/wiki/` |

## 3. Precedence audit

The question: does replacing first-match-wins with a most-specific-match router
(such as `matchit`) change any winner? Three constructs decide it.

1. **Suffix-dispatched groups — this one narrows.** `starts_with` plus
   `ends_with` (`write_path.rs:1462`) constrains only the suffix, not the number
   of segments. `app_explorer::write` then reads the first two segments and
   ignores any further ones (`src/ui/app_explorer.rs:170-203`), so today
   `POST /api/app-installations/inst-1/restore/restore` reaches
   `app_workspace_restore` exactly like the two-segment path. A table row
   `/api/app-installations/{id}/restore` binds one id segment and would answer
   `404` to the extra-segment form.

   That narrowing is correct — the extra segments are malformed and no in-repo
   caller emits them — but it is a behaviour change, not an equivalence. The
   migration states it explicitly and pins it with a test: the two-segment path
   keeps its handler, the three-segment path becomes `404`. Contrast the
   conversations arm (`write_path.rs:1438`), which already filters the tail to a
   single segment (`!contains('/')`) and therefore can be expressed exactly.
2. **Same-stem recursion.** `/api/app-installations/migrations/{id}/recover`
   versus `/api/app-installations/{id}/recover` (`write_path.rs:1488-1504`).
   Today both are computed before one `||` arm and cannot both match: the
   `/{id}/recover` parse filters out any tail containing `/`, so
   `migrations/x/recover` reaches only the `migrations` pattern. As table rows
   the two patterns also cannot overlap, because one segment of literal
   `migrations` is part of the first. No flip — and this is the only same-stem
   recursion in the chain.
3. **OR-groups sharing one handler.** `/api/app-catalog/install`,
   `/api/app-catalog/git-check`, `/api/app-catalog/request`, `/api/app-favorites`,
   `/api/app-favorites/default`, `/api/app-favorites/opened`,
   `/api/app-requests/dismiss`. As a table these become seven rows with one
   handler id; no overlap.

**Finding:** no pair in either chain depends on first-match *over* most-specific
for the paths the board actually serves. The one place the rules would disagree
is the malformed extra-segment form above, where a table is deliberately
stricter. The one pair where order mattered (`migrations/{id}/recover`)
resolves the same way under both rules. The audit is static: it compared the arms
as written, not observed responses.

**Caveat:** two arms are *conditional on method* rather than on the path
(`write_path.rs:1025` answers `405` for `/api/setup` instead of dispatching, and
`serve.rs` answers `405` when a `route()` returns a write variant on the read
surface). The table must express this as a method on the row, not as an arm that
returns 405 by hand.

## 4. Proposed shape

```rust
struct Endpoint {
    id: RouteId,          // one variant per handler
    method: Method,       // one row per accepted method
    pattern: &'static str, // "/api/app-installations/{id}/remove"
    class: RouteClass,    // OperatorOnly | ScreenRead | AgentAllowed | Session | ...
}

static ROUTER: LazyLock<Router<RouteId>> = ...; // matchit, built once

fn route(method: &str, path: &str) -> Outcome {
    match ROUTER.at(path) {
        Ok(m) if m.value.method_matches(method) => Outcome::Matched(m.value, m.params),
        Ok(m) => Outcome::MethodNotAllowed(m.value.allowed()),
        Err(NoMatch) => Outcome::NotFound,
    }
}
```

- Dispatch is `match endpoint.id { … }` with one uniform handler signature
  (`fn(&mut Ctx) -> HttpResp`), where `Ctx` carries the request, state, PM dir,
  opts, query and the bound parameters.
- `class` and dispatch come from the same row, so `WRITE_ROUTES` is deleted
  rather than kept in sync.
- A missing row is `404` before any guard — strictly stricter than today's
  fail-closed `OperatorOnly` default, because there is no handler to reach.

## 5. Invariants the migration must keep

1. A path that matches no row is `404`; it never reaches a guard or a handler.
2. A matched row with the wrong method is `405` with an `Allow` header, matching
   today's per-arm refusals.
3. `/unsubscribe/{token}`, the device sign-in exchange and the app-screen frame
   keep their current admission class; none becomes a generic read.
4. Every row binds exactly the parameters its handler reads; a row that binds
   fewer (or different) parameters is a handler bug the table cannot catch, so
   the uniform `Ctx` makes the parameter names part of the handler signature.
5. `read_admit`'s order (405, then guard, then handler) survives as the
   dispatcher's order for every `Read` class row.
6. The suffix-group narrowing is recorded: `/{id}/<action>/<anything>` is `404`
   after the migration, where today it reaches the action handler.

## 6. Migration order

| Slice | Scope | Checks |
|---|---|---|
| 1 | Write chain's exact arms (`/api/session*`, `/api/setup`, `/api/update*`, `/api/locale`, `/api/reports`, `/api/settings/*`) | existing route tests; 405 and session refusals pinned |
| 2 | Write chain's explorer group and the `/api/app-installations/` suffix families | explorer tests; `migrations/{id}/recover` precedence test |
| 3 | Read chain prefix groups, one stem at a time (`/api/wiki/`, `/api/projects/`, `/api/agents/`, `/api/issues/`) | each group's `route()` tests plus a board smoke |
| 4 | Inline exact read arms (`/api/master/*`, `/api/app-catalog*`, …) | as above |
| 5 | Delete both chains and `WRITE_ROUTES`; the table is the only authority | full required CI |

Each slice keeps both chains until its group is migrated, which is why the order
puts whole stems in one slice rather than one path per slice: a stem split
across two mechanisms is exactly the drift this work removes.

Revert point: each slice is a commit; the chains are untouched until slice 5, so
reverting a slice restores the previous behaviour without touching the other
groups.

## 7. What this spike does not settle

- Router mechanics: §8 decides against a router for now, so there is nothing to
  prove there until a catch-all row or ~100 rows arrive.
- Whether `Ctx` can stay borrow-friendly for handlers that mutate the request
  (uploads read the body) — slice 1 answers it on the smallest handler.
- The TypeScript side. Codegen from Rust is not currently possible — 3 of the UI's
  102 interfaces have a Rust `Serialize` counterpart; the rest are `json!`
  literals. That is a separate prerequisite, not part of this migration.

## 8. Parameterised rows: per-segment validation (CAD-1677)

Slice 2 cannot move on segment patterns alone: some shapes validate the values
they bind, and a plain `{id}` row would drop that check. The `/api/app-installations/`
write stem, as it stands:

| Arm (`src/ui/write_path.rs`) | Shape today | Validation | Failure today |
|---|---|---|---|
| 1438 | `POST /{id}/conversations` | id non-empty, no `/` | falls through → `404 no such write route` |
| 1462 | `POST /{id}/{update-check,remove-preview,remove,restore}`, **any** middle segments | id non-empty | empty id → `400 missing installation id` inside `app_explorer::write`; extra segments dispatch when the second segment is a verb |
| 1488-1531 | `/migrate`, `/{id}/recover`, `/{id}/upgrade`, `/{id}/upgrade/check`, `/{id}/upgrade/recover`, `/migrations/{id}/recover` | id non-empty, no `/` | falls through → `404 no such write route` |
| 1537, 1547 | `/check`, exact `/api/app-installations` | none | — |
| 1647 | `/upload`, `/upload/check` | none | — |
| `app_screens::mount_route` | `POST /{id}/screens/{tag}/mount` | id: non-empty, ≤128, `[A-Za-z0-9_-]`; tag: `valid_tag` | falls through → `404 no such write route` |

**Row shape.** A row binds named parameters and may validate them; the failure
status and message are part of the row because they differ today.

```rust
struct Endpoint {
    id: RouteId,
    method: &'static str,
    pattern: &'static str,            // "/api/app-installations/{id}/remove"
    class: RouteClass,
    bindings: &'static [Binding],
}

struct Binding {
    name: &'static str,
    validate: fn(&str) -> bool,
    invalid: (u16, &'static str),     // what that arm answers today
}
```

`lookup` returns the row and its validated bindings. A validator that fails
answers the row's `invalid` pair and never reaches admission or a handler. The
pair is per row, so `mount` keeps `404 no such write route` and the explorer id
keeps `400 missing installation id`; a single global "404 on invalid" would
change an observable status.

**Precedence.** One row pair really does overlap:
`POST /api/app-installations/migrations/upgrade/recover` satisfies both
`/migrations/{id}/recover` (id `upgrade`) and `/{id}/upgrade/recover` (id
`migrations`) — the two parses are computed before one `||` arm and the current
code takes catalog recovery first (`write_path.rs:1476-1511`), relaying
`app_workspace_migration_recover` with id `upgrade`. The table must declare
`/migrations/{id}/recover` first and a test must pin that exact path, the chosen
operation and the bound id. `/migrations/{id}/recover` versus `/{id}/recover` has
a different segment count and cannot overlap; the two are ordinary matching
cases, not an overlap test.

**Dispatch is broader than classification today.** The suffix family dispatches
`/{id}/<verb>/<anything>` (the handler reads two segments and ignores the rest)
while its `WRITE_ROUTES` pattern `/{id}/<verb>` binds one segment, so such a
request is already classified `OperatorOnly` by the fail-closed default. When
the second segment is a supported verb the request still reaches the relay —
`POST /api/app-installations/inst-1/update-check/update-check` performs an
update check today. The table adopts the narrow shape for both, which is
invariant 6's narrowing: those spellings become pre-admission `404`s. That is an
intentional behaviour change, not an equivalence; the implementation tests it as
such. No caller gains authority (the wide spelling is operator-only today), but
an admitted operator loses a spelling that worked.

**Empty segments are a binding question, not a matcher detail.** Today
`POST /api/app-installations//remove` reaches `app_explorer::write` and answers
`400 missing installation id`, because the arm matches on suffix alone. A
`{id}` binding that rejects an empty segment would turn that into `404`, and a
binding that accepts it lets the row's validator answer `400`. The explorer row
must accept the empty segment and answer `400`; the mount row rejects it with
`404 no such write route`, as `mount_route` does today. The pattern language
therefore needs an explicit empty-segment rule per binding, not a global one.

**Router.** Still no dependency. After this slice the table holds roughly 30 rows
and a linear scan is free; precedence is explicit and tested. Buy a router only
when a catch-all row appears or the table passes ~100 rows.

**Complete slice-2 row list** (all `POST`): the six rows of the recovery/migrate
family, `check`, the exact install path, `upload`, `upload/check`, the explorer
exact paths (`/api/app-catalog/{install,git-check,request}`,
`/api/app-favorites`, `/api/app-favorites/{default,opened}`,
`/api/app-requests/dismiss`), the four `/{id}/{update-check,remove-preview,remove,restore}`
rows, `/{id}/conversations`, and `/{id}/screens/{tag}/mount`. The read half of
the same stem is slice 3; `serialized_write` is untouched.

**Checks the implementation runs for each moved row:** the row answers the same
status and body for a valid path, the same `invalid` pair for its failing
validator, `405` for a wrong method, and `404` for the malformed extra-segment
form (invariant 6). The precedence pair gets its own test.
