# Design contract: CAD-1019 remote CLI — login maps the org; allowlisted commands on the remote daemon

Phase-1 contract for the remote CLI transport. No implementation is in this
PR. Authority chain: `cadence login` device grant (CAD-539) → org registry
record (CAD-657) → remote command transport over the board host (this
contract). CAD-913 (wiki) is the intended second consumer of the same
transport, not a parallel one.

## Evidence answers to the phase-1 questions

**Q1 — can the CLI reuse the board's device sign-in?** The board routes
`POST /api/session/device/{code,poll}` (`src/ui/operator.rs`, WRITE_ROUTES at
`src/ui/operator.rs:352-353`; handlers `device_code` at `:1445` and
`device_poll` at `:1541`) are deliberately *not* reusable by a non-browser CLI:
`device_origin` (`operator.rs:1390`) refuses `Origin::Public`, so the routes
never serve on `<slug>.cadencecloud.app`; `device_attribution`
(`operator.rs:1414`) runs process attribution on the TCP peer and refuses any
caller it cannot attribute, which a remote client can never satisfy; and the
result is a cookie + `X-Cadence-Session` page key meant for a same-origin
browser. The reusable piece is the *issuer-side* flow, already implemented
twice for two surfaces (`src/remote_auth.rs` for `cadence login`,
`src/device_login.rs` for the board): `POST /v1/device/code`,
`POST /v1/device/token` (RFC 8628 flat shape), `GET /v1/runtime/session`
bearer verify — all on AgenticOS `origin/main`
(`apps/api/src/index.ts:236-237` mounts `createDeviceGrantRoutes` /
`createRuntimeRoutes`; `apps/api/src/device.ts:725`+ implements
`/v1/runtime/session`). `DEVICE_GRANT_ENABLED` gates the routes
(`device.ts:140`, flag at `apps/api/src/env.ts:226`); staging/prod switch-on
is the AOS dependency, not new code.

**Q2 — does the ingress admit a non-browser client?** Partially, and that is
the design hinge. On `*.cadencecloud.app` the worker's catch-all
(`apps/api/src/board/routes.ts` final `app.all("*")`) forwards **only**
requests carrying a `__Host-aos-board-session` cookie; anything else gets a
`302` to `/v2/board/authorize`, which requires a better-auth *browser*
session and answers `403` HTML to non-members — a non-browser bearer client
cannot pass. `POST /__platform/session` is the only non-cookie admission, and
it demands a platform-signed Ed25519 assertion (`aud` = board host, `exp ≤
iat+60s`, single-use `jti`); the AOS-49 `agc_` device credential is **not** an
assertion and mints nothing today — no route on AOS `main` turns `agc_` into a
board assertion (`git grep mintBoardAssertion` shows only
`board/routes.ts` `boardAuthorize`). Forwarded traffic is constrainted by the
relay: only `RELAY_FORWARD_HEADERS` survive — which **includes
`authorization`, `cookie`, `x-cadence-session`, `x-cadence-board`, `origin`**
(`apps/api/src/board/relay-proof.ts:5-18`, mirrored in
`infra/runtime-image/board-relay.mjs:5-18`) — plus a 16 MiB body cap
(`relay-proof.ts:25`) and a per-request signed relay proof bound to
method+target+headers+body (`board-relay.mjs:120-160`). No Cloudflare Access
gate exists on board hosts in the inspected tree (`git grep CF-Access` on
`origin/main` is empty); CAD-729's opt-in `access-issuer.json` speaks only to
the *issuer* origin. So: a bearer-token CLI needs **one new board-host
admission route** on the AOS worker; everything after the cookie check
(transport of `Authorization`, the body cap, the container relay) already
works.

**Q3 — do the board HTTP APIs cover the launch verbs?** Coverage is partial
and the shapes differ, so a CLI↔board adapter is needed anyway:

| CLI verb | Board route today | Caller rule today |
|---|---|---|
| `issue ls` | `GET /api/issues` (query filters, `serve.rs:1180`+) | any session |
| `issue show` | `GET /api/issues/<id>` (`serve.rs:1570`+ `strip_prefix`) | any session |
| `issue new` | `POST /api/issues` | `AgentAllowed` (`operator.rs:216`) |
| `issue comment` | `POST /api/issues/*/comments` | `AgentAllowed` (`operator.rs:224`) |
| `message send` | `POST /api/threads/*/messages` | `OperatorOnly` (`operator.rs:244`) — semantics differ from the daemon's `message_send` |
| `message inbox` | none | — |
| `agent list` | `GET /api/agents` (`serve.rs:1244`) | any session |

Two gaps matter: several CLI verbs are tracker-file operations (`issue ls`
reads `board::load_all` on `Pm::open_default()` — `src/issue/cli.rs:682`,
`src/issue/mod.rs:204`), not daemon RPCs, so "remote `issue ls`" needs a
server-side read the board read-model already computes; and `message
send`/`agent` verbs have no one-to-one board route. Rather than teach the CLI
two protocols (board REST + local RPC), the transport is a single
allowlisted **CLI-command relay** (§Invariants): the board answers a new
family of routes that map each allowlisted verb to exactly the read-model or
daemon-RPC call the local CLI would make, reusing the existing caller classes
as the gate.

**Q4 — login→org mapping.** `GET /v1/runtime/session` on AOS `main` returns
`data.workspace` via `toWireWorkspace` (`apps/api/src/workspace.ts:65-70`),
which includes **`slug`**. `cadence login` therefore records, per org:
`org_id = workspace.id`, `endpoint = https://<slug>.cadencecloud.app`,
issuer, credential reference, subject id. It writes into CAD-657's
`Registry`/`Connection` model (PR #415, head
`5e5a4c6a26ecc68de1719a77320868c04c20219c`: `src/cli/org.rs` —
`Destination::Remote{endpoint, org_id}` already exists and is deliberately
inert) — **no second registry**. `org switch`/`--org`/`CADENCE_ORG`
precedence and the "remote fails before local fallback" refusal are already
shaped in `org.rs::select`/`resolve`; this contract adds the remote arm to
`resolve`. Default selection rule from the ticket: select on login only when
no default exists or `--use` is passed; a second workspace login adds an org
without moving the default.

## Invariants

- I1: A remote command carries exactly one credential — the stored `agc_`
  device bearer for the resolved org — to exactly one destination:
  `https://<workspace.slug>.cadencecloud.app`, the slug taken verbatim from
  the issuer's `/v1/runtime/session` answer at login. The slug is never
  derived from `workspace.id`, never read from a redirect `Location`, and
  never recomputed per request.
- I2: The remote surface is a verb allowlist, not an RPC tunnel. The server
  side dispatches only the named verbs; anything else — including every
  operator-only daemon verb — is refused remotely with the same refusal the
  daemon gives, and no request field names a caller, org, or destination.
- I3: No fallback. Wrong audience, expired/revoked token, unknown org, a
  redirect, an unavailable remote, or a refused verb ends the command with an
  error; it never silently re-resolves to local or another org.
- I4: Destination resolution is pinned once per command. `org switch` during
  a running command cannot move it; `CADENCE_ALIAS`-bound managed callers
  keep CAD-657's rule (inherited pins win; `--org` conflicts, never
  overrides).
- I5: Login never changes an existing default org without `--use`, and
  refuses to overwrite a stored credential for a different issuer/org
  (existing `remote_auth::save` rule).
- I6: Status/inspect output shows org, endpoint, mode and credential
  presence — never token material.

## Chosen transport

**Reuse the board session + board API, minus the cookie.** The AOS-49 device
credential already carries `read draft` scopes and workspace binding; what it
lacks is admission to the board host. One new worker route admits it:

- `POST https://<slug>.cadencecloud.app/__platform/cli/session` — body
  `{agc_token}` is wrong; instead the **token travels as
  `Authorization: Bearer <agc_>`** (the relay already forwards that header),
  the worker resolves slug → workspace, verifies the `agc_` against the
  issuer's `/v1/runtime/session` equivalent check *inside the API worker*
  (it owns D1 — `resolveDeviceCredential` in `apps/api/src/device.ts:96`
  already maps bearer → `{userId, workspaceId, scopes}`), binds
  `workspaceId == slug's workspace`, requires `read` scope, mints a
  **short-lived CLI session** (server-side row, e.g. 12 h idle / 24 h
  absolute like `REMOTE_*` in `device_login.rs:26-28`), and returns it as a
  JSON body field (not `Set-Cookie` — the worker's outbound filter drops
  non-`__Host-` cookies, and a CLI holding a JSON token is simpler than
  faking cookie semantics). Alternatively the mint happens inside the
  container; see AOS asks below — the contract only fixes that the token is
  verified by the API worker's own store, scoped to the slug's workspace, and
  never forwarded elsewhere.
- Every subsequent CLI call:
  `POST https://<slug>.cadencecloud.app/__platform/cli/v1/<verb>` with
  `Authorization: Bearer <cli-session>` (or, per implementation choice, the
  `agc_` re-verified per call — trading session state for verification cost;
  decide in review). The worker checks the CLI session's workspace binding
  against the slug and forwards. Cadence's board then authenticates the
  request **without** a cookie on a dedicated `Origin::PublicCli`-style
  origin arm, attributes it to the named principal (a `Caller::Named`
  variant), and applies the allowlist below.

Why not reuse `/__platform/session` assertions: the assertion mint lives on
the app origin behind a browser session; minting assertions for a device
credential would need a new mint anyway, plus JWKS handling for a 60-second
credential on every CLI call — strictly more machinery for the same binding.

Why not a cookie session: the device-*board* routes refuse `Origin::Public`
by construction, and correctly so — they mint operator-class cookies gated
by local attribution. The remote CLI session is a `Named`-class credential
(member/owner role from the issuer), never the operator loopback session.

## Verb allowlist (v1)

Reads: `issue ls`, `issue show`, `issue history`, `agent list`,
`agent show`, `message read`, `message inbox`, `team list`, `status`.
Writes: `issue new`, `issue comment`, `issue set` (fields only — no
`start`, no `dispatch`), `message send` (to a named alias; lands as a
`Named` caller, so an agent recipient sees a member, not "operator").

Refused remotely (non-exhaustive, default-deny): every operator-only daemon
verb (`agent join/stop/resume`, `daemon *`, `operator_*`, `rollout`,
`master/*` decisions, `approve`, `merge`, `secrets`), every verb that names
a local path (`issue init`, `attach`, wiki `put` under CAD-913's separate
chunked contract), and `issue start`/`dispatch` (they mint worktrees and
claim ownership — local-machine semantics). Operator-only verbs reachable in
v2 need the remote operator proof designed with CAD-657's source-authority
decision — out of scope here.

## Auth chain

`cadence login --issuer <api-origin>` (existing `remote_auth`) → device grant
→ `agc_` + `/v1/runtime/session` → record `{org_id: workspace.id, slug:
workspace.slug, endpoint: https://<slug>.cadencecloud.app}` in the CAD-657
registry + credential in the existing `credential.json` hygiene (0600, dir
0700, no symlink, per-org file or keyed record). Remote call: resolve org →
load credential → `POST /__platform/cli/session` (bearer `agc_`) if no live
CLI session → `POST /__platform/cli/v1/<verb>` (bearer CLI session) → Cadence
board admits on a new origin arm → handler runs the verb's existing
daemon-RPC/read-model path with `Caller::Named`. Every refusal is a fixed
string; issuer bodies and tokens are never echoed anywhere (existing
`remote_auth`/`device_login` rule).

## Refusal cases (acceptance-bound)

- Token for workspace A sent to B's host: worker binds bearer → workspaceId ≠
  slug's workspace → 403, no forward.
- Wrong audience/revoked/expired `agc_`: session mint refused 401/403; an
  expired *stored* credential fails before any request
  (`remote_auth::status`-style check) with a re-login pointer.
- Redirect (3xx) anywhere in the chain: refused; the client follows none
  (existing `max_redirects(0)` + explicit 3xx refusal).
- Unavailable remote / unknown slug / sleeping container that the route
  cannot wake: bounded-timeout error; **no** local fallback (I3). Note for
  implementation: the CLI route needs a wake rule — either reuse
  `wakeBoardRuntime` on a verified credential, or document that the remote
  CLI requires an awake board (container cold-start latency is a UX cost,
  not a security decision — flag for review).
- Verb not on the allowlist: `404`/`403` fixed refusal, server-side, before
  dispatch — a forged `verb` field or a path smuggle (`<verb>` containing
  `/`, `%2f`, `..`) fails routing.
- Agent-tied or unattributable TCP peer at the board: existing attribution
  keeps running *under* the new origin arm — a CLI session cannot launder an
  agent's writes into `Named` (the session header is checked only after
  attribution, and an agent peer presenting one is refused
  `session_from_agent`, same as today).

## What AgenticOS must change (AOS ticket to file)

1. `POST /__platform/cli/session` on board hosts: verify
   `Authorization: Bearer <agc_>` via `resolveDeviceCredential`, require
   `read` scope and `principal.workspaceId == workspaceIdForSlug(host)`,
   throttle like the device endpoints, mint/return a bounded CLI session
   (or forward a verified request to the container mint — either side, but
   the *worker* must reject cross-workspace tokens itself so a wrong-host
   token never reaches a container).
2. Admit `/__platform/cli/*` in the board-host dispatcher for requests
   carrying that CLI session (a `Cookie`-free path, so `hasBoardSession`
   stays untouched); forwarded requests keep the relay header allowlist —
   `authorization` already passes.
3. Production/staging switch-on of `DEVICE_GRANT_ENABLED` (flag exists).
4. Document the CLI surface's wake policy (see open question above).

## Adversarial tests

| Test name | Proves | Guard | Fails without the guard because |
|---|---|---|---|
| `remote_verb_allowlist_refuses_unlisted` | I2 | server-side verb table | a forged `verb=shutdown`/`operator_secret_rotate` dispatches |
| `remote_agent_caller_cannot_mint_or_use_cli_session` | I2 | attribution before session check | an agent peer's writes land as `Named` |
| `remote_detached_child_no_ambient_authority` | I4 | caller binding from connection, not env | a `setsid` child inherits a pin it never had |
| `cross_workspace_token_refused` | I1 | worker slug↔workspace bind | workspace-A token reads B's board |
| `redirect_never_followed_token_never_forwarded` | I3 | `max_redirects(0)` + 3xx refusal | bearer rides to `Location` host |
| `unavailable_remote_no_local_fallback` | I3 | resolve-once + explicit error | command silently opens the local tracker |
| `second_login_keeps_default` | I5 | select-once rule | workspace B login moves the default |
| `default_switch_mid_command_noop` | I4 | pinned destination per invocation | `org switch` mid-`message send` reroutes the send |
| `forged_session_field_refused` | I2 | `deny_unknown_fields`/verb schema | a body field names org/caller/destination |
| `concurrent_logins_different_orgs_keep_both` | I5 | existing credential lock + registry lock | one login clobbers the other |

## Live probes this design still needs (none taken)

Phase 1 was code-reading only — no requests were made to any live or staging
host (`demo-company.cadencecloud.app`, tailnet 9460/9461, CDP 9222 are owned
elsewhere). The contract rests on static evidence; the following live probes
remain unverified and are listed for the PM to arrange ownership:

- `GET {issuer}/.well-known/agenticos-board-jwks.json` reachable from a CLI
  network — the contract assumes the JWKS endpoint is public per contract §6,
  unverified against the deployed worker.
- `DEVICE_GRANT_ENABLED` state on staging/prod — code shows the flag gates
  `/v1/device/*` and `/v1/runtime/*`; the deployed value was not probed.
- Whether a board-host request carrying `Authorization` but no cookie is
  answered by the worker's redirect without touching the container — inferred
  from `hasBoardSession` in `app.all("*")`; a probe would confirm no edge
  rule (e.g. Cloudflare config outside the repo) alters it.
- `POST /__platform/cli/session` end-to-end mint — cannot exist until the AOS
  change lands; the acceptance smoke test is that probe, deferred to
  implementation and a designated staging slot.
- Redirect behaviour of the board host on expired/missing session (302 →
  authorize) — read from code, not exercised.

## Out of scope

Renewal (CAD-740), multi-org UX beyond CAD-657, operator-only verbs over the
remote path, generic RPC passthrough (CAD-913's stance kept), wiki chunking
(CAD-913 itself), container wake policy details (flagged above),
`DEVICE_GRANT_ENABLED` rollout approval (AOS-side ops decision), and all live
probing (listed above for PM-arranged ownership).
