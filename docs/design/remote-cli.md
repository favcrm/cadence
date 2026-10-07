# Design contract: CAD-1019 remote CLI — login maps the org; allowlisted commands on the remote daemon

Phase-1 contract for the remote CLI transport. No implementation is in this
PR. Revised per PM direction: login rides the **hosted-cadence device grant**
(`hcd_`/`hct_`, the AOS-76 owner-consent flow already implemented in
`src/remote_enrollment.rs`), and remote calls go through AOS-128's
`POST /__platform/cli/call` — the generalization of AOS-122's
`/__platform/wiki/{authorize,call}` envelope proxy. CAD-913 (wiki) shares this
transport rather than duplicating it.

## Evidence answers to the phase-1 questions (revised)

**Q1 — which sign-in does the CLI reuse?** Not the board's
`/api/session/device/*` routes (they refuse `Origin::Public` at
`src/ui/operator.rs:1390` and run `/proc` peer attribution at `:1414` — a
remote caller can never satisfy either). And not the plain `agc_` tools grant:
the PM direction and the AOS-122 precedent both point to the **hosted-cadence
device grant** — `POST /v1/hosted-cadence/device/{code,token}` with PKCE,
owner-only approval, minting an `hct_` bridge credential
(`apps/api/src/hosted-cadence/device.ts`, on `origin/cadence/aos-122-wiki-authority`
head `62f6ad0`; Cadence's client already exists as
`src/remote_enrollment.rs::enroll_browser`, `:1136`). Login no longer means
the 30-day `agc_` contract; see "Credential lifetime" below.

**Q2 — does the ingress admit a non-browser client?** Yes for the new shape.
AOS-122 adds board-host routes that never consult the cookie:
`POST /__platform/wiki/authorize` (bridge bearer → signed `wikienv_` actor
envelope) and `POST /__platform/wiki/call` (envelope + still-live bearer →
forward to `/api/wiki/<action>` inside the awake container)
— `apps/api/src/board/routes.ts` on the AOS-122 branch, `:1245-1400`. AOS-128
generalizes the second hop to `/__platform/cli/call` carrying a **cli actor
envelope** and a verb name; no feature flag. The worker performs auth *before*
any runtime resolution (`checkWikiCall` model,
`hosted-cadence/wiki.ts:439-560`): signature vs `loadBoardKeys`, `aud` =
public board host, `organization_id` = the slug's workspace, the action
inside granted scopes, and the presenting bearer still resolving to the exact
issuing credential — so a revoked or sibling credential never reaches a
container. The container relay already forwards `authorization` and caps
bodies at 16 MiB (`board/relay-proof.ts:5-25`). Wake semantics change: the
wiki `/call` is awake-only (`runtime_unavailable` 503 otherwise); the PM
notes AOS-128 specifies a `503` + `Retry-After` *waking* reply the CLI waits
on — details land with AOS-128.

**Q3 — do the board HTTP APIs cover the launch verbs?** Same answer as
before, and the transport now absorbs it: verbs map to a `cli.call`-style
server dispatch, not per-verb REST. The verb table (server-side, inside the
container) names each allowed command and maps it to the read-model or
daemon-RPC call the local CLI would make (`issue ls` is a tracker read —
`src/issue/cli.rs:682` opens `Pm::open_default` — so the remote equivalent is
the board read-model's board slice, `serve.rs:1180`; `agent list` →
`GET /api/agents` data, `serve.rs:1244`; writes like `issue new`/`comment`
→ the existing `AgentAllowed` routes' handlers, `operator.rs:216-225`).

**Q4 — login→org mapping.** The hosted device grant pins `organization_id`,
`organization_slug` and `audience` (the board origin) server-side at approve
time (`hosted-cadence/device.ts:300-330` — the bridge insert joins
`workspaces.slug` and refuses a mismatch). `cadence login` therefore records
into CAD-657's registry (`src/cli/org.rs` `Destination::Remote{endpoint,
org_id}`, inert until this work): `org_id = organization_id`,
`endpoint = <audience>` (which is `https://<slug>.cadencecloud.app`), plus
the enrollment record reference. The slug comes only from the issuer's grant,
never derived from `org_id` — same rule as `platform_account.rs`. Select as
default only when none exists or `--use` is passed.

## Credential lifetime (revised — load-bearing)

The browser-minted `hct_` bridge expires at `min(now + 300s, owner-session
expiry)` (`hosted-cadence/device.ts:288-296`) and `enroll_browser`'s child
inherits that bound — a 5-minute credential cannot serve a persistent CLI
login. Two consequences:

- **The stored credential is the cli envelope chain, not the bridge.** Login
  = `hcd_` device grant → `hct_` bridge → enroll a `cli`-client child (the
  `enroll` step `remote_enrollment.rs` already runs for the result sender,
  with `requested_capabilities` = the CLI set) → each remote call mints a
  ≤300 s `cli actor envelope` at `/__platform/cli/authorize`. What persists
  locally is the enrollment record (`remote_enrollment.rs`'s `Sealed`
  hygiene: 0700 dir, 0600 record, checksum, lock). When the bridge expires,
  CLI commands fail with "re-login" — renewal is CAD-740's scope, and this
  contract does not extend the bridge TTL.
- **That expiry is a design risk to flag in review**: 5-minute login means
  remote CLI is effectively per-operation unless AOS-128 also defines a
  longer-lived grant for interactive use (a "cli session" capability or a
  longer bridge TTL under the owner session's bound). The contract holds
  either way; the acceptance smoke test will pin which.

## Invariants

- I1: A remote command carries exactly one credential — the cli actor
  envelope (plus its `hct_` issuer underneath, sent as the call's bearer per
  the AOS-122 revalidation pattern) — to exactly one destination: the
  `audience`/endpoint the issuer recorded at login. The host is never
  derived from `org_id`, never taken from a redirect `Location`, never
  recomputed per request.
- I2: `/__platform/cli/call` is a verb allowlist, not an RPC tunnel. The
  worker refuses a verb outside the granted capabilities before resolving a
  runtime; Cadence's handler refuses anything its table doesn't name; no
  request field names a caller, org or destination (forbidden-field refusal,
  the `checkWikiCall` `forbidden` list pattern at `wiki.ts:478`).
- I3: No fallback. Wrong audience, expired/revoked credential, wrong-org
  envelope, a redirect, an unavailable or waking remote, or a refused verb
  ends the command with an error; it never re-resolves to local or another
  org. A `503` waking reply with `Retry-After` is waited on per AOS-128's
  rule — bounded, counted, and still fails closed after the bound.
- I4: Destination resolution is pinned once per command; `org switch` during
  a running command cannot move it; `CADENCE_ALIAS`-bound callers keep
  CAD-657's rule (inherited pins win; `--org` conflicts, never overrides).
- I5: Login never changes an existing default org without `--use`; a second
  workspace login adds an org and leaves the default untouched; concurrent
  logins for different orgs cannot clobber each other's records (existing
  `enrollment.lock` + registry lock).
- I6: Status/inspect show org, endpoint, mode, enrollment expiry and login
  state — never `hcd_`/`hct_`/envelope material.

## Verb allowlist (v1)

Reads: `issue ls`, `issue show`, `issue history`, `agent list`,
`agent show`, `message read`, `message inbox`, `team list`, `status`.
Writes shipped by CAD-1179: `issue new`, `issue comment`, `issue set`
(frontmatter fields only — status/priority/owner/component/title/tags/
type/milestone/size; body edits are not part of the field vocabulary).
`message send` (to a named alias — lands as the cli actor's derived
handle, never "operator") stays in the server verb table but has no
client mapping in this build — the milestone sends the three `issue_*`
write verbs only.

Client-side refusals (CAD-1179 — refused before any bytes leave the
process): `issue new` without `--project`, or with `--id`/`--status`;
`issue comment` with `--author`/`--kind` (authorship is the envelope
actor); `issue set` without a nonempty `--if-rev`, with `--force` (the hosted
route accepts no force field), or naming more than
one ticket (revisions are per-ticket — the hosted route checks it under
the tracker lock via `set_fields_if_rev`, and a stale token answers the
conflict payload rather than writing).

Refused remotely (default-deny): every operator-only daemon verb
(`agent join/stop/resume`, `daemon *`, `operator_*`, `rollout`, `master/*`,
`approve`, `merge`, secrets), verbs naming local paths (`issue init`,
`attach`, chunked wiki `put` — CAD-913's own envelope contract), and
`issue start`/`dispatch` (worktree + ownership semantics are local).
Operator-only verbs need the remote operator proof tied to CAD-657's
source-authority decision — out of scope for v1.

## Auth chain

`cadence login --issuer <api-origin> --org <workspace-id>` →
`/v1/hosted-cadence/device/code` (PKCE, `requested_capabilities` = the CLI
set) → owner approves on the app → `/v1/hosted-cadence/device/token` → `hct_`
bridge + minted cli-actor enrollment record (locally sealed) → each command:
`POST https://<slug>.cadencecloud.app/__platform/cli/authorize` (bridge
bearer → envelope, ≤300 s, `aud` = board host, `organization_id` = slug's
workspace) → `POST …/__platform/cli/call` `{envelope, verb, arguments}`
with the same bearer → worker verifies signature + live credential rebind +
verb scope, wakes/peeks runtime per AOS-128, forwards the allowlisted verb to
the container's `/api/cli/<verb>` (or equivalent dispatch) with the envelope
as bearer → Cadence re-verifies the envelope (JWKS via the configured
issuer), derives the named actor, runs the verb's existing daemon/read-model
call, refuses operator-class work. `Set-Cookie` plays no part; the relay's
header allowlist already passes `authorization`.

## Refusal cases (acceptance-bound)

- Envelope for workspace A at B's host: `aud`/org mismatch fails the
  worker's `checkWikiCall`-equivalent **and** the container's own verify —
  two independent gates, no forward.
- Revoked/rotated/expired `hct_`: the worker's live rebind (`resolveBearer`)
  refuses before wake; a stolen envelope alone is useless because its
  `credential_id` no longer resolves.
- Redirects: none followed; the CLI's ureq agent keeps `max_redirects(0)`
  and treats 3xx as failure (existing `remote_auth`/`remote_enrollment`
  rule).
- Unavailable remote, unknown slug, `runtime_unavailable` past the
  `Retry-After` bound: fixed-string error, no local fallback (I3).
- Unallowlisted verb, or a verb field carrying `/`, `%2f`, `..`, or a
  forbidden identity key: refused at the schema before any runtime touch.
- Agent-tied TCP peer presenting a cli session at the board: refused —
  attribution still runs under the new arm; the envelope names a principal,
  and a pane's socket identity can't mint one (container-side check).

## What AgenticOS must change (AOS-128)

1. `POST /__platform/cli/authorize` on board hosts — `hct_`/`hcs_` bridge
   bearer → signed cli actor envelope; same verify-then-mint shape as
   `wiki/authorize` (`resolveWikiBearer` → `authorizeWikiActor`), with a
   `cli.*` capability vocabulary instead of `wiki.*` prefixes.
2. `POST /__platform/cli/call` — envelope + verb + arguments; live bearer
   rebind; verb allowlist enforced before runtime resolution; forwards to
   the container's CLI dispatch (new Cadence-side route family, its own
   handler — not the wiki one). No feature flag per the PM note.
3. Wake semantics: `503` + `Retry-After` on a sleeping-but-wakeable runtime;
   the exact envelope/wait contract lands with AOS-128 (PM note). The
   cookie catch-all is untouched.
4. Production switch-on of the hosted-cadence device grant flags
   (`HOSTED_CADENCE_DEVICE_ENABLED`, `HOSTED_CADENCE_GATEWAY_ENABLED` —
   `hosted-cadence/device.ts:174-185`) and the CLI capability vocabulary in
   `HOSTED_CADENCE_CAPABILITIES` (`contracts/src/hosted-cadence-auth.ts:7-18`).

## Adversarial tests

| Test name | Proves | Guard | Fails without the guard because |
|---|---|---|---|
| `cli_verb_allowlist_refuses_unlisted` | I2 | server verb table | `verb=shutdown`/`operator_secret_rotate` dispatches |
| `cli_envelope_cross_workspace_refused` | I1 | `aud`/org check at worker AND container | A's envelope reads B's board |
| `cli_revoked_bridge_kills_envelope` | I3 | live bearer rebind at call | a stolen envelope outlives its credential |
| `cli_redirect_never_followed` | I3 | `max_redirects(0)` + 3xx refusal | bearer rides to `Location` |
| `cli_unavailable_no_local_fallback` | I3 | resolve-once + error | command opens the local tracker instead |
| `cli_second_login_keeps_default` | I5 | select-once rule | workspace B login moves the default |
| `cli_switch_mid_command_noop` | I4 | pinned destination per invocation | `org switch` mid-send reroutes it |
| `cli_forged_verb_field_refused` | I2 | strict schema + forbidden keys | a body field names actor/org/destination |
| `cli_agent_peer_cannot_mint` | I2 | container-side attribution | a pane's socket identity mints a `Named` call |
| `cli_concurrent_logins_keep_both_orgs` | I5 | enrollment + registry locks | one login clobbers the other's record |
| `cli_waking_respects_retry_after_bound` | I3 | bounded wait, counted retries | a sleeping remote hangs or spins forever |

## Live probes this design still needs (none taken)

Phase 1 was code-reading only — no requests to any live or staging host
(`demo-company.cadencecloud.app`, tailnet 9460/9461, CDP 9222 are owned
elsewhere). Unverified, listed for PM-arranged ownership:

- `HOSTED_CADENCE_DEVICE_ENABLED`/`HOSTED_CADENCE_GATEWAY_ENABLED` state on
  staging — flag-gated routes; deployed values not probed.
- `GET {issuer}/.well-known/agenticos-board-jwks.json` reachability for the
  container-side envelope verify (AOS-122's `WIKI_ACTOR_JWKS_PATH` pattern —
  hosted deployments resolve `http://api.internal` internally; the public
  path is an alias).
- Whether AOS-128's `cli/call` wakes on a sleeping runtime (PM note says
  503 + `Retry-After`) vs the wiki contract's refuse — read only; the smoke
  test exercises it.
- `POST /__platform/cli/{authorize,call}` end-to-end — cannot exist until
  AOS-128 lands; the acceptance smoke test is that probe, deferred to
  implementation and a designated staging slot.
- Envelope TTL/refresh behavior on a real bridge — confirmed ≤300 s in code;
  no live check.

## Out of scope

Renewal and any bridge-TTL extension (CAD-740 / an AOS decision — the
5-minute login bound is flagged above as the design risk review must weigh),
multi-org UX beyond CAD-657, operator-only verbs over the remote path,
generic RPC passthrough (CAD-913's stance kept), the wiki actions themselves
(CAD-913), container-side CLI dispatch internals (phase 2), all live probing
(above).
