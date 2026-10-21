# Method-aware endpoint resolution

Status: **spike**, CAD-1694, revised after review. Decides how the endpoint table
resolves a request whose path is declared by both a read row and a write row,
before any read arm moves. No code here.

## 1. Why the current rule cannot carry the read half

`endpoints::resolve(path)` returns the first row whose pattern matches the path,
ignoring the method; `write_route` then answers 405 on a method mismatch. That is
sufficient while the table holds only write rows. It is not sufficient for the
read half: these paths are served by a read arm today, and several also carry a
write row.

| Pattern | Read arm (`serve.rs`) | Admission today | Write row |
|---|---|---|---|
| `/api/app-installations` | list | `admit_operator_read` | `POST` install, `OperatorOnly` |
| `/api/app-installations/{id}` | show | `admit_screen_read` (member-capable) | none — a bare `POST` falls through to `404 no such write route` |
| `/api/app-installations/{id}/conversations` | master's conversations | `admit_operator_read` | `POST`, `OperatorOnly` |
| `/api/wiki/*` | read envelope | the generic private-API gate (`admit_read`) | write envelope |
| `/api/issues/{id}` | issue read | the generic gate | `PATCH`, `AgentAllowed` |
| `/api/threads/{alias}[/stream]` | chat read | the generic gate — **not** `admit_operator_read`; a member reads it | `POST` on `/{alias}/messages` and `/{alias}/conversations` only |
| `/api/app-installations/{id}/screens/{tag}/mount` | — | — | `POST`, `ScreenRead` |

Only three of these are genuinely bidirectional on one path
(`/api/app-installations`, `/{id}/conversations`, `/api/wiki/*`). The rest are a
read-only path beside differently shaped write paths. A path-first resolver would
shadow the read on the bidirectional three, and would invent a bare `POST` on
`/api/threads/{alias}` if the read path were moved as a write row — so the
inventory, not the mechanism, is the risk.

## 2. Two gates, not one

Both chains run an admission **before** they look at the method, and the
migration must keep that:

| Chain | Before the method check |
|---|---|
| Read (`handle`) | Unsupported methods (`TRACE`, `OPTIONS`, …) are refused at `serve.rs:1184-1188` before any path lookup. Every private `/api/` read passes `admit_read` first (`serve.rs:1443-1464`), including paths no arm matches, so an anonymous caller gets the session refusal before the 404. |
| Write (`write_route`) | For every non-`Session` table row the current dispatcher calls `operator::admit` **before** comparing the method, so a read-only board answers `403 read_only` and an unauthenticated caller its guard refusal before any 405. |

Consequently a `MethodOnly` outcome must not be an early 405. The resolver
returns it, and the dispatcher applies the gate for the classes the path
declares, in the same order the chain uses today, and only then answers 405. The
`Allow` header is additive; the body stays `{"error": "method not allowed"}`.

**Mismatch statuses are per path today, and the table must not flatten them:**

| Request | Today |
|---|---|
| `POST /api/app-installations/{id}` | `404 no such write route` |
| `POST /api/threads/{alias}` | `404 no such thread write route` |
| `POST /api/issues/{id}` | `405 no such write route` (not `method not allowed`) |
| `POST /api/setup` | `405 setup is read-only here — GET only` |
| `GET` on a write-only row (e.g. `/api/update`) | `405 method not allowed` after admission |

So a row carries its mismatch pair — status, message — exactly as it carries its
`invalid` pair, and `MethodOnly` answers the pair of the rows it found. A row
that has no mismatch answer yet keeps the generic pair only where that is what
the code does today.

## 3. HEAD is a read

`handle` admits `GET` **and** `HEAD` into the read chain, and every
read arm serves both (`head_only` only strips the body in `send`). A resolver
that matches `GET` exactly would turn today's `HEAD` into a 405 on every migrated
read row.

Rule: `HEAD` resolves to the `GET` row of the same pattern and is admitted and
handled identically; only the response body is suppressed, as `send` already
does. Rows are declared `GET`; `HEAD` is never declared. `Allow` for a
read row therefore lists `GET, HEAD`.

## 4. Resolution outcomes

```rust
enum Outcome<'p> {
    Matched(Bound<'p>),          // one row owns (method, path)
    MethodOnly(&'static [&'static Endpoint]), // the path exists, the method does not
    Miss,                        // no row owns the path
}
```

- `Matched` — exactly one row for the path names the request's method, or the
  request is `HEAD` and a `GET` row owns the path.
- `MethodOnly` — at least one row owns the path and none matches. The dispatcher
  runs the path's admission gate, answers the mismatch pair, and returns; it
  never falls through to the older chain, because the path is table-owned.
- `Miss` — no row. The remaining chain runs unchanged, gate included.
- Validation stays where it is: after admission, before the handler.

No row declares an `Any` method. Every arm that would need one is a prefix family
(`/api/wiki/*`, `/api/issues/*`, `/api/threads/*`, the monitors and memories
stems) that stays on the chain until its own slice; when it moves, its action
parser moves with it, as slice 2 did for the explorer.

## 5. Keeping the classes apart

A read row and a write row on one pattern are two rows: same pattern, different
method, and each selects its own guard by method *and* direction:

| Class | Read row guard | Write row guard |
|---|---|---|
| `OperatorOnly` | `admit_operator_read_caller` (`board_caller(write=false)` + peer proof) | `admit` — write guard, then the same caller checks |
| `ScreenRead` | `admit_screen_read` (member-capable, agent-refusing) | `admit`, `ScreenRead` branch |
| `AgentAllowed`, `CallerScoped` | `board_caller(write=false, budget=None)` | `admit` with its write guard |
| `Session`, `RecipientToken`, `Refused` | none — no read row may carry these | unchanged |

The sketch in the first revision claimed a total, compile-time mapping; it is
not one. `read_guard(class)` over the existing enum accepts every variant, and
`board_caller` takes five arguments, so it cannot sit in a three-argument slot.
The honest contract is a construction rule plus a fail-closed runtime check:

- a read row may only be declared with `OperatorOnly`, `ScreenRead`,
  `AgentAllowed` or `CallerScoped` — checked at table construction (a `debug_assert`
  plus a table test that enumerates the rows), and refused with a `500` if it
  ever occurs in release;
- the read guard is one function with a `match` on class, whose
  `Session | RecipientToken | Refused` arm is unreachable by the rule above and
  answers the 500 refusal rather than admitting.

## 6. First read slice

The two installation reads and the conversations read, with the checks that keep
today's boundary:

1. `GET|HEAD /api/app-installations` — `OperatorOnly`, `app_workspace_list`.
   A member session is refused, as `admit_operator_read` refuses it today.
2. `GET|HEAD /api/app-installations/{id}` — `ScreenRead`, `app_workspace_show`.
   A member session reads it; an agent does not.
3. `GET|HEAD /api/app-installations/{id}/conversations` — `OperatorOnly`,
   `threads::conversations` with the install selector.

Checks: each admitted read answers byte-for-byte what its arm answers today
(status, body, `ETag`); the same request as a member, as an agent, and
anonymously answers the same status as today; `HEAD` answers the GET status and
headers with no body; `POST` on each pattern selects the write row and does not
change the read result; a wrong method on a write-only sibling keeps its current
mismatch status. The three arms are deleted from `serve.rs` in the same commit
that adds the rows.

## 7. What this spike does not settle

- The prefix families keep their parsers until their own slice; whether they
  express as patterns or as a row with a sub-parser is that slice's decision.
- `/api/threads/*` is **not** on this slice's list, and must not be migrated
  with `OperatorOnly`: today a member reads a transcript under the generic gate.
  That slice decides the class it actually has.
- Routers stay out at the current row count; `MethodOnly` is a second comparison
  on the same linear scan.
