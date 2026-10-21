# Method-aware endpoint resolution

Status: **spike**, CAD-1694. Decides how the endpoint table resolves a request
whose path is declared by both a read row and a write row, before any read arm
moves. No code here.

## 1. Why the current rule cannot carry the read half

`endpoints::resolve(path)` returns the first row whose pattern matches the
path, ignoring the method; `write_route` then answers 405 on a method mismatch.
That is sufficient while the table holds only write rows. It is not sufficient
for the read half, because these paths carry both directions today:

| Pattern | Read arm (`serve.rs`) | Write arm |
|---|---|---|
| `/api/app-installations` | list, `OperatorOnly` | `POST` install, `OperatorOnly` |
| `/api/app-installations/{id}` | show, `ScreenRead` (CAD-1537) | — (a bare `POST` 404s today) |
| `/api/app-installations/{id}/conversations` | master's conversations, `OperatorOnly` | `POST` conversation, `OperatorOnly` |
| `/api/wiki/*` | read envelope | write envelope |
| `/api/issues/{id}` | issue read | `PATCH` |
| `/api/threads/{alias}` | chat read, `/stream` suffix, `OperatorOnly` | conversation post |
| `/api/app-installations/{id}/screens/{tag}/mount` | — | `POST` mount, `ScreenRead` |

A path-first resolver would shadow one direction with the other on every row of
this table. That is the whole reason the read half has not moved.

## 2. Resolution outcomes

```rust
enum Outcome<'p> {
    Matched(Bound<'p>),          // one row owns (method, path)
    MethodOnly(Method),          // the path exists, method does not
    Miss,                        // no row owns the path
}
```

- `Matched` — exactly one row for the path also names the request's method.
- `MethodOnly` — rows exist for the path, none names the method. The dispatcher
  answers `405` with `Allow: <methods>` and then returns; it does not fall
  through to the older chain, because the path is table-owned.
- `Miss` — no row; the remaining chain runs unchanged.
- Validation stays where it is: after admission, before the handler. A matched
  row whose binding fails answers the row's `invalid` pair; `Bound::invalid` is
  unchanged.

**Status today, checked against both chains:** a known path with a wrong method
answers `405 method not allowed` in `write_route` and in the read arms that
check it; an unknown path reaches the chain and ends at `404`. `MethodOnly`
must therefore produce the same 405 body (`"method not allowed"`) and add
`Allow`; adding a header is the only observable change, and it is additive.

## 3. Why rows on one path do not collide

`RouteClass` already separates the directions, and the two admission entry
points differ in more than the class name:

| Class | Read entry | Write entry |
|---|---|---|
| `OperatorOnly` | `admit_operator_read` — `board_caller` plus peer proof, no write guard | `admit` — write guard, then the same caller checks |
| `ScreenRead` | `admit_screen_read` — member-capable, never an agent | `admit`, `ScreenRead` branch |
| `Session`, `RecipientToken` | — | handler owns the credential |
| `AgentAllowed`, `CallerScoped` | — | `admit` |

The write path adds the content-type/origin guard; the read path does not. So a
read row and a write row on the same pattern are two rows with the same
`pattern`, different `method`, and each keeps its own admission function
selected by `method`. **One row still means one class.**

## 4. Two remaining questions and their answers

**Is a row's method ever `Any`?** No, for the read half. Every arm that would
need `Any` — `/api/wiki/*`, `/api/session/device/*` — is a *prefix family* whose
own parser decides the action, and those stay on the chain until their slice.
One decision per row keeps `Allow` exact and keeps the resolver's preference
rule trivial: rows are compared on `(method, pattern)` and there is no fallback
order to reason about. Revisit only if a slice needs it.

**Where does the read guard dispatch happen?** In one place, by method:

```rust
fn read_guard(class: RouteClass) -> fn(&Request, &Path, &ServeOpts) -> Result<Caller, HttpResp>
```

`OperatorOnly` → `admit_operator_read_caller`, `ScreenRead` →
`admit_screen_read`, `AgentAllowed`/`CallerScoped` → `board_caller` (the same
call `read_admit` makes today for exoneration-free reads). A class with no read
rule is a compile error, which is what stops a read row from being added with a
write-only class.

## 5. First read slice

The app-installations reads, because their rows are already written:

1. `GET /api/app-installations` — `OperatorOnly`, `app_workspace_list`.
2. `GET /api/app-installations/{id}` — `ScreenRead`, `app_workspace_show`.
3. `GET /api/app-installations/{id}/conversations` — `OperatorOnly`,
   `threads::conversations` with the install selector.

Checks per row: the admitted path answers identically to the arm it replaces; an
anonymous read answers the same status it does today; a member session reads the
show but not the list; the write rows on the same patterns still answer 405 on
`GET` and still admit writes. Then the three arms are deleted from `serve.rs` in
the same commit that adds the rows — the path moves whole.

## 6. What this spike does not settle

- The prefix families (`/api/wiki/*`, `/api/issues/*`, `/api/monitors/*`,
  `/api/memories/*`, `/api/threads/*`) keep their own parsers until their slice;
  whether they express as patterns or as a row with a sub-parser is that
  slice's decision.
- Routers stay out at the current row count; the `MethodOnly` outcome is the
  same linear scan with one extra comparison.
