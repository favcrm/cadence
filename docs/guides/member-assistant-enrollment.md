# Native member-assistant enrollment and acceptance

This is the Native-side runbook for CAD-1603, with platform issuance owned by
AOS-298 and genuine provider readiness owned by CAD-1606. It does not deploy an
instance or authorize credential operations. The enrolled-company set is unknown;
an absent credential on one inspected board is not a fleet inventory.

**Do not use this procedure to change `demo-company`.** That company currently
uses Worker/Pi Durable, not a Native tenant daemon. Its private profiles and model
calls follow the Worker contract. Do not restore a container, enroll a Native
credential into production, or change its provider to obtain Native evidence.

## 1. Establish an authorized Native test scope

Before minting anything, the instance owner records the isolated Native state and
tracker paths, candidate binary/revision, board port (3110–3199), process owner,
permitted operations, rollback artifact, and any already-authorized network
mapping. Production paths, port 3010 and the installed binary are not test targets.

The platform owner supplies the exact workspace ID, canonical board host, pinned
issuer, named test member and credential-rotation owner. These are distinct:

| Field | Required value |
| --- | --- |
| Native platform | `hosted-board` |
| Native account | Exact configured board host, including a port when present; no scheme or path |
| Native declared scopes | Exactly `assist.revalidate` |
| Platform `organization_id` | That board's actual workspace ID, not its display name |
| Platform `audience` | Canonical board origin derived by the platform from that workspace's slug/configuration |
| Platform `requested_capabilities` | Exactly `["assist.revalidate"]`; no bridge, results, wiki, CLI or web capabilities |
| Platform `ttl_seconds` | Integer from 300 to 2592000 (30 days); owner chooses the bounded test lifetime |

Native enroll/rotate checks the account against its configured board identity
and the declared scope list before custody writes. An opaque `hcs_` token cannot
be decoded locally to prove its workspace/audience: the platform's mint binding
and a successful live member projection are still required. `platform accounts`
is metadata inspection, not remote validation.

A local operator connection and a platform workspace-owner session are different
authorities. Neither an agent alias nor a member envelope substitutes for either.
Keep credentials out of agent chat, browser JavaScript, command arguments, shell
tracing, logs, tickets and repository files.

## 2. Credential-free local preflight

From your own shell, create a fresh disposable instance. This block does not
configure board identity, enroll a token, select a provider or prove a member
conversation. Preserve caller identity variables; do not use another person's
operator session or export a global-provider opt-in.

```sh
export CADENCE_SANDBOX_ROOT="$(mktemp -d /tmp/cad1603.XXXXXX)"
CADENCE_SANDBOX_ALLOW_GLOBAL=0 cadence dev up member-readiness
CADENCE_SANDBOX_ALLOW_GLOBAL=0 cadence dev status member-readiness
cadence --state-dir "$CADENCE_SANDBOX_ROOT/member-readiness/state" platform accounts
CADENCE_SANDBOX_ALLOW_GLOBAL=0 cadence dev down member-readiness
CADENCE_SANDBOX_ALLOW_GLOBAL=0 cadence dev status member-readiness
```

`up` prints the isolated state/tracker paths, selected port and loopback URL.
Check those paths and `dev: true`; the port must stay in 3110–3199. A fresh
instance may list only the built-in `local` publication account. That is not a
`hosted-board` enrollment. An unsigned `/api/assist/state` refuses with 401;
that refusal is not evidence of successful member projection. `down` stops only
this owned instance and retains its files; final status must show both processes
stopped. Never reset or delete another instance's files.

Without `--build`, `dev up` uses the invoking installed binary. To qualify a
candidate, use `dev up member-readiness --build /absolute/path/to/candidate`
in a fresh owned scope and record the actual binary revision. An installed-binary
preflight does not qualify an unmerged candidate. Existing build-slot and
source-review requirements still apply.

A bare local sandbox is not a hosted Native runtime. Native revalidation uses
its pinned issuer's `/__platform/assistant/revalidate`; the hosted
`api.internal` handler derives the company from provisioned runtime context and
checks the lease. The public API origin is not interchangeable with that internal
route. Do not fabricate a Host, issuer, runtime context, lease or signed member
assertion to make a local test pass.

## 3. Platform-side issuance: owner-only handoff

AOS-298 owns the trusted, operator-runnable issuance/handoff. Confirm that the
chosen deployment actually supports it before minting. The existing API source
mounts these endpoints at `/v1/hosted-cadence/service`:

- `POST /credentials`: exactly `organization_id`, `audience`, `label`,
  `requested_capabilities`, `ttl_seconds`.
- `GET /credentials?organization_id=...`: metadata list; no token recovery.
- `POST /credentials/:id/rotate`: exactly `organization_id`, `audience`,
  `ttl_seconds`.
- `POST /credentials/:id/revoke`: exactly `organization_id`, `audience`.

Issuance requires the service/gateway feature gates and a live workspace-owner
cookie session; an Authorization bearer is refused. The server derives the board
audience from the live workspace slug and checks owner membership again at the
write. A supplied foreign board origin is not a valid alternate target.
Cookie-authenticated issuance, rotation and revocation POSTs also require an
`Origin` admitted by the deployment's `trustedOrigins` configuration; a missing
or untrusted Origin returns 403 even with a valid owner cookie. Requests marked
`Sec-Fetch-Site: cross-site` are refused. These headers do not replace owner
authentication or authorize an agent to perform the handoff.

Issue returns the access token once. Use only the approved owner-controlled
handoff that feeds that token to Native stdin without printing or exposing it to
browser scripts. This guide does not invent a `cadence` issuance command or a
secret-export fallback. If that handoff is unavailable, stop on AOS-298 before
minting. Retain only credential ID, workspace, audience, scopes, issuance/expiry
and a scheduled rotation date in the operator's non-secret operational record.

## 4. Native enrollment: operator decision required

On the approved Native instance, the operator obtains `NATIVE_STATE` and
`BOARD_HOST` from the scope record, not from a chat claim. Inspect that instance:

```sh
cadence --state-dir "${NATIVE_STATE:?approved Native state path required}" platform accounts
```

First enrollment refuses `custody_unprotected`: Native custody is readable by
same-UID unconfined agents. File modes and encrypted-at-rest storage do not remove
that exposure. Only the actual operator may decide whether to accept it. Do not
clear agent identity, borrow an operator session or record that decision for them.

Only after that explicit decision, the approved handoff feeds one token line to:

```sh
cadence --state-dir "${NATIVE_STATE:?approved Native state path required}" \
  platform enroll hosted-board --account "${BOARD_HOST:?configured board host required}" \
  --scope assist.revalidate --token-stdin --accept-same-uid-risk
cadence --state-dir "$NATIVE_STATE" platform accounts
```

The first command is the **consumer** of the approved private stdin handoff, not
an instruction to paste a token into an echoing terminal. Do not use `--token` or
send the JSON mint response verbatim. The resulting metadata must name
`hosted-board`, the exact host and only `assist.revalidate`. It proves a local
record was saved, not that its bearer works upstream. First acceptance is audited
as `custody_risk_accepted: "same-uid"`. Rotation inherits that recorded choice.
An instance configured for encrypted custody currently refuses new non-SMTP
credential enrollment; do not disable its custody configuration to bypass that.

## 5. Provider readiness and genuine member acceptance

Native private assistants select their first launch model from the daemon's
stored **worker** model policy, not the Master's model. The model is then pinned
on the assistant agent row. Inspect the isolated instance's settings through its
existing operator interface; a Master reply does not qualify the member path.

For the reviewed hosted AgenticOS Pi path, see
[Hosted AgenticOS Pi workers](agenticos-pi-workers.md). It requires the reviewed
catalog and provisioned, lease-bound `gateway.internal` runtime. The literal SDK
placeholder is not an API credential, and this is not a public inference URL for
local workers. Do not copy the operator's Pi login, invent a gateway proxy or
switch providers to satisfy CAD-1606. Generic Pi authentication is not evidence
of platform-held, test-scoped credentials. Missing approved runtime/provider
provisioning remains a CAD-1606 blocker.

With both prerequisites actually ready, use the named member's normal verified
company-board sign-in. Open that member's private Home assistant, never a Master
or an operator's local Home as a substitute:

1. Read state and history through the normal `/api/assist/*` relay. Require a
   successful response for the signed member's own assistant. A rendered shell,
   saved credential, 403, empty fixture or cached binding is not successful proof
   of first projection under live platform authority.
2. Send a plain work request and observe a genuine managed-model reply in that
   same member transcript. Record the actual runtime/model and non-secret scope.
3. For identity acceptance, name the assistant and choose its communication style
   conversationally; keep the user's preferred address and company name separate.
   Exercise skip, later and resume without blocking the work request. Rename and
   update style later, reload, and verify both persist with the same avatar pin.
4. Confirm truthful history and that another member cannot read the private
   profile/transcript. Record the actual user-visible outcome and failures, not
   fabricated PASS rows. Do not substitute a fixture, replay or another company's
   existing workspace/Master reply.

CAD-1817 remains pending genuine candidate acceptance until these observations
exist. Source review, CI, a credential-free preflight and a Worker conversation
are separate evidence, not substitutes.

## 6. Expiry, rotation, refusal and retirement

The platform service credential expires at `expires_at`, at most 30 days after
issuance. Short-lived bridge/member credentials have separate lifetimes; renewing
a login does not extend this service credential. A missing, revoked, expired or
unloadable service credential prevents first projection and service-backed live
authority checks. A configured but unloadable credential never falls back to the
member bearer. Existing history is not proof of renewed authority.

The named platform credential owner schedules rotation **before** expiry and
coordinates it with the Native instance owner. Platform rotation immediately
revokes the predecessor and returns a successor once; there is no grace-period
promise. Native must receive that successor via the same approved private stdin
handoff, using the existing account:

```sh
cadence --state-dir "${NATIVE_STATE:?approved Native state path required}" \
  platform rotate hosted-board --account "${BOARD_HOST:?configured board host required}" \
  --scope assist.revalidate --token-stdin
cadence --state-dir "$NATIVE_STATE" platform accounts
```

Record the new upstream credential ID/expiry, rotation date and Native fingerprint,
then repeat the real member verification. If either side fails, stop and report
the failure; do not retry an uncertain issuance or restore the revoked bearer.

When custody is absent, the board can show:

```text
403 forbidden — platform assistant provision refused: no enrolled hosted-board
service credential for '<configured-board-host>' — projection requires the
board's own enrolled authority
```

An unconfigured identity, foreign account, wrong scope, unreadable custody or live
revalidation refusal has a different cause. Keep that original diagnostic; do not
turn it into an empty transcript or weaken the guard.

On retirement, the platform owner revokes the test service credential through the
same owner-gated API, and the Native operator removes its local authority:

```sh
cadence --state-dir "${NATIVE_STATE:?approved Native state path required}" \
  platform revoke hosted-board --account "${BOARD_HOST:?configured board host required}" \
  --reason "isolated member-assistant scope retired"
```

The platform owner also retires the test member/provider access through its
supported lifecycle. Revoking `hosted-board` does not erase member data or stop a
model actor by itself. Stop only the owned test processes with `dev down`, remove
only that scope's private transfer material, and retain non-secret outcome and
retirement records. Never delete custody, assistant rows or production state by
hand.
