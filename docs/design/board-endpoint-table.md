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

1. **Suffix-dispatched groups.** `starts_with` plus `ends_with`
   (`write_path.rs:1462`) has no segment count. As a table it becomes one row per
   action (`/api/app-installations/{id}/remove`, `…/restore`, …). Every such path
   has a fixed tail, so a pattern binds exactly the same paths. No flip.
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

**Finding:** no pair in either chain currently depends on first-match *over*
most-specific. The one pair where order mattered (`migrations/{id}/recover`)
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

- Whether `matchit`'s segment rules cover every existing pattern. The inventory
  above suggests they do; slice 1 must prove it with a test that builds the
  router from the table and asserts each row matches a synthesized concrete path.
- Whether `Ctx` can stay borrow-friendly for handlers that mutate the request
  (uploads read the body) — slice 1 answers it on the smallest handler.
- The TypeScript side. Codegen from Rust is not currently possible — 3 of the UI's
  102 interfaces have a Rust `Serialize` counterpart; the rest are `json!`
  literals. That is a separate prerequisite, not part of this migration.
