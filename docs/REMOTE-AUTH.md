# AgenticOS issuer sign-in and hosted enrollment (CAD-539, CAD-717, CAD-729)

Ordinary `cadence login` establishes the remote CLI/wiki session described in
**Hosted login and remote org transport** below. Current workspace members do
not need an owner-only login for routine collaboration. That session is not a
provider billing or publishing credential.

The separate CAD-539 provider credential import (`--token-stdin`) does not
connect issue, message or team commands to hosted Cadence. Its
`hosted_cadence: not_configured` result describes that provider import, not an
ordinary hosted-org login. Source support is not deployment evidence; the
chosen issuer must serve the matching contract.

## Provider-adapter and publishing consent

Normal image-enabled apps use the signed-in funded-account media flow; that
path does not require `provider.draft`. The optional legacy external-adapter
and publisher setup commands below request separate financial consent:

```sh
cadence --org <slug> local-agent tool --lane provider.draft
cadence --org <slug> local-agent tool --lane publish.send
```

These use the existing device consent endpoints, not a new token issuer.
Success means `credential_saved`, with activation `not_checked`: neither
command claims that a tool, connection or sender is already usable. The raw
token file and a separate issuer/workspace/expiry binding stay private.

For legacy draft tools, the response supplies a `connection create`
argument vector and `stdin_file` for the **local operator**. That existing
scoped-token path accepts the `agenticos_external` provider and its canonical
workspace account; ordinary `platform enroll` is not that path. An unsupported
legacy account is reported as `config_required` before financial consent;
ordinary CLI/wiki collaboration is unaffected. Configure the external provider
door for the issuer, enroll or rotate the credential through native custody,
then select the connection in the authorized app. Existing custody policy,
descriptor checks, app authority and budgets remain; no worker grants are
created and there is no missing-enrollment token fallback.

A publish grant writes the private credential file without registering a
sender. If not already configured, publishing needs its explicit door URL and
credential-file configuration. The command does not restart the daemon, send,
spend or replace the CLI login. Standing publishing authorization still applies.
Generic device lanes remain owner-approved financial authorization, not an
additional owner requirement for ordinary CLI/wiki collaboration.

## Existing token and unattended verification

Supply a credential using stdin, never a raw argv argument:

```sh
secret-manager-read | cadence login --issuer https://your-agenticos-api.example --org ws_company --token-stdin
```

This verifies the credential through `/v1/runtime/session` before saving it.
The issuer must return a named principal, the exact organization, and `read`
scope. The CLI does not mint tokens. Manual/service token issuance is an
issuer dependency; the current PR issues opaque device credentials only.

For an ephemeral credential, set `CADENCE_TOKEN`, `CADENCE_ISSUER` and
`CADENCE_ORG` through your secret manager, then run `cadence auth status`.
Explicit status `--issuer` and `--org` override the corresponding environment
selectors. A present `CADENCE_TOKEN` always wins over stored credentials,
requires both selectors, and is never persisted. Invalid environment credentials
fail without falling back to stored credentials. `login --token-stdin` explicitly
uses stdin; browser login explicitly uses a new grant, regardless of environment.

All status checks contact the issuer, so expired/revoked credentials fail on
the next check. Redirects are refused; bearer credentials are never forwarded
to another host. CLI network connections require HTTPS origins without paths,
userinfo, query strings or fragments. No issuer response body or credential is
printed on errors.

## Storage and logout

Credentials use an explicit permission-restricted file fallback, not an OS
keychain: `$XDG_CONFIG_HOME/cadence/remote-auth/credential.json` (otherwise
`~/.config/cadence/remote-auth/credential.json`). Directory permissions are 0700
and file permissions 0600; symlinks and unsafe ownership/permissions are refused.
Concurrent saves are serialized. Use an absolute `--auth-dir` for isolated
development or a separate connection. A different issuer/organization cannot
replace an existing credential; org switching is separate CAD-657 work.

`cadence auth logout` removes the local credential only. Revoke the server
credential using AgenticOS connected devices. Logout does not clear an environment
token or terminate remote sessions. Device credentials currently expire after
30 days; the protocol has no refresh token or token expiry introspection field.
For imported tokens, expiry is unknown locally and checked by the issuer.

## Hosted login and remote org transport (CAD-1019, CAD-1329)

For a first login, run `cadence login`. The CLI opens the trusted hosted
AgenticOS portal, where you choose a workspace and approve access. To
preselect a workspace, use `cadence login <slug>`; the issuer verifies
that selection and returns the authoritative workspace ID and slug. The
CLI validates the returned slug and canonical audience before recording
the remote org and credential. Use `cadence login --use` (or
`cadence login <slug> --use`) to make it the saved default; without
`--use`, the current default is unchanged. `--no-open` prints the approval
URL and code without opening a browser.

An owner or current member can approve ordinary collaboration. Both the portal
and explicit org login request `cli.read`, `cli.write`, `wiki.read` and
`wiki.write`; saved renewal retains the consented set. `--web` requests its
separate CLI plus Publish Web set and does not silently add wiki consent.

The default issuer is the compiled exact HTTPS origin
`https://api-v2.agenticos.hk`; no issuer or workspace files are needed on
a fresh HOME. If a `trusted-issuer` pin already exists, it must match that
origin or login refuses without overwriting it. A custom issuer is an
advanced override: pass `--issuer <origin>` and establish its exact origin
in a private `trusted-issuer` file under the credential directory first.
The CLI never creates or replaces that pin. A mismatched issuer, symlink,
or unsafe permissions refuse.

The explicit legacy bound flow remains available as
`cadence login --issuer <origin> --org <workspace-id> --slug <slug>`; it
requires an existing issuer pin. The legacy `--token-stdin` flow also
requires explicit `--issuer` and `--org`. Browser login uses PKCE and
short-lived device codes; denial, expiry, mismatched identity or audience
refuse before recording the org or credential. Org records use the
verified workspace slug and canonical `https://<slug>.cadencecloud.app`
endpoint. Credentials are stored at
`$XDG_CONFIG_HOME/cadence/remote-auth/cli-<slug>.json` (0600), bound to
the verified workspace ID and endpoint, and never sent to another host.

### Advanced custom issuer trust

The credential directory is created by login itself (0700, owner-only).
Custom issuers require an independent exact-origin pin in a private
`trusted-issuer` file before any request goes out. The CLI never creates
or replaces this pin:

```sh
cadence login --issuer https://your-agenticos-api.example
# → refused with the exact trusted-issuer pin path
printf '%s\n' 'https://your-agenticos-api.example' > "${XDG_CONFIG_HOME:-$HOME/.config}/cadence/remote-auth/trusted-issuer"
chmod 600 "${XDG_CONFIG_HOME:-$HOME/.config}/cadence/remote-auth/trusted-issuer"
cadence login --issuer https://your-agenticos-api.example
# → device code prompt; approve in the browser
```

The pinned origin must byte-match `--issuer` (one optional trailing
newline). A wrong-issuer pin, a symlink, or loose permissions refuse
the same way.

When the resolved org is remote (`--org` > `CADENCE_ORG` > saved
default), an allowlisted command runs against the remote daemon instead
of the local socket:

```sh
cadence --org acme issue ls --status doing
cadence --org acme agent list
cadence --org acme status --json
```

The transport is the AgenticOS `POST /__platform/cli/{authorize,call}`
relay (AOS-128). Each command first exchanges the stored `hct_` for a
server-minted `cli actor envelope` at `/__platform/cli/authorize`
(≤300 s, `aud` = the board host, `organization_id` = the org's
workspace), then POSTs `{version, envelope, verb, arguments}` to
`/__platform/cli/call`. The worker re-verifies the signature, org,
audience and live bearer before any runtime is resolved or woken; the
container independently re-verifies the envelope before dispatching the
verb. `verb` is one of a fixed allowlist — `status`, `agent_list`,
`agent_show`, `issue_ls`, `issue_show`, `issue_history`, `message_read`,
`message_inbox`, `issue_new`, `issue_comment`, `issue_set`,
`project_policy`, `message_send` — and
`arguments` is a strict per-verb payload. Any
other command is refused locally and never sent.

A sleeping company answers `503 {"state":"waking","retry_after_s":N}`
plus `Retry-After`; the CLI waits and retries inside `--wake-timeout`
(default 120 s), printing `waking <org>… (Ns)` on stderr while stdout
stays JSON. The budget lapses into exit 75 (`busy`). There is never a
fallback to local or another org, and no token is ever printed. Operator-only
and every unlisted verb stay refused remotely; the remote path carries
no operator proof, and the hosted `issue_set` verb accepts no `force`
field — the `status=done` evidence gate has no remote override.

### Refusal categories and next action (CAD-1434)

Authorize, cli/wiki call and blob upload share one decoder. It reads the AOS
gateway envelope `{ok:false,error:{code,reason?}}` and shows only allowlisted
values, never raw server text, a token or a private path. Renewal is a
separate adapter: the issuer answers `{error:"<code>"}`, so renewal reads its
own shape (see the renewal rows below). The two are intentionally not merged.

The gateway's real refusal shapes (AOS-243):

- A policy outcome is `401` with `error.code: "unauthorized"` and a proven
  `error.reason`: `scope_not_granted`, `grant_narrowed` or `binding_mismatch`.
- The Cadence container's own envelope gate answers `403` with
  `error.code: "insufficient_scope"` when the signed grant does not cover the
  action or the target path. This is the path denial. See the layers below.
- `envelope_invalid` is a structural, expired or legacy envelope. It is not a
  proven session loss.
- A `401 unauthorized` with no reason means the bearer did not resolve, but the
  server cannot say whether it expired or was revoked.
- `503` means waking or unavailable. Without JSON, the HTTP status still
  classifies the refusal: `401` is uncertain, `403` is authorization, and any
  `5xx` is availability. No server reason or correlation is inferred.

| Category | When | Next action |
| --- | --- | --- |
| `authorization` | `401` with an allowlisted policy reason (`scope_not_granted`, `grant_narrowed`, `binding_mismatch`); `403` with `insufficient_scope` (container path or scope denial); or any `403` with `forbidden` or no safe code | Not a login problem. Read-only diagnosis (below): check the workspace role and target path. Not retried |
| `authentication` | A terminal session code: `invalid_token`, `invalid_grant`, `session_expired`, `session_revoked`, `lineage_terminal` | `cadence login` again, once. The only case that names login as the known remedy |
| `uncertain` | `401` with no reason, `envelope_invalid`, or a non-JSON `401` | Not confirmed expiry or revocation. Run the read-only diagnosis before any login. Not retried |
| `availability` | any `5xx`, or a renewal that fails transiently | Retry later. Saved credentials and any pending renewal are kept |
| `unknown` | Any other answer. A `400`/`409`/`413`/`422` keeps its safe code, such as `payload_too_large` | Report the status and code. Not retried |

### Where a write or upload is refused (AOS-243)

A wiki write or blob upload is refused at one of two layers. Neither needs a
new login, and neither is retried.

1. **Gateway admission** (AOS, before any byte is forwarded). It verifies the
   envelope signature, issuer, audience and organization; that the action's
   scope is granted; that the bearer still resolves to the same live
   credential; and that the signed grant is still inside the live ceiling. A
   refusal is `401` with a proven `reason` (for example `scope_not_granted`), or
   a generic `401 unauthorized` when the bearer does not resolve. After argument
   validation, it also checks every normalized target against the action's
   signed prefixes **before any runtime wake**: both move legs and the combined
   upload directory/filename. An out-of-prefix target is `403 insufficient_scope`;
   a malformed path is `400`. Root enumeration remains available under a narrow
   grant, with entries filtered by the container.
2. **Container enforcement** (the Cadence container, before any daemon
   mutation). `src/ui/wiki_envelope.rs` checks the requested path against the
   signed grant (`contains` for reads and writes, `contains_file` for uploads,
   checked before the body is buffered). A path outside the grant, or an action
   the grant does not cover, is `403 insufficient_scope`. The container then
   relays only a verified `users/` or `operator` identity to the daemon. The
   daemon's own `wiki::allowed` ACL runs last, under that attribution.

So a path denial appears as `403 insufficient_scope`, not as a gateway `401`.
Authorization is the category: check the granted scope, current membership and
target path. Routine shared wiki and issue work does not require an owner role.
Another user's private pages and agent-owned areas remain protected.

A board refusal also names its hop and a request id, for example
`(hop board, request 0b2e1f4a-…)`. The id is a fresh UUID per refusal, used to
correlate a report with the board's log. It is not a token or an identity. Only
the board hop is named; a refusal from the container or the daemon is shown
without a hop, never attributed to one.

Renewal (`/v1/hosted-cadence/cli-session/renew`) has its own reader. Only the
issuer's `invalid_token` (401) and `invalid_grant` (400) ask for a fresh login.
Any other answer keeps the saved login and the pending operation for a later
retry, with no consent prompt.

A mutation whose outcome is unknown (a dropped connection after dispatch, or a
5xx after forward) is never replayed automatically. Reconcile with a read
(`issue show`, `wiki ls`) before repeating the write.

### Hosted credential status (CAD-1434)

`cadence org list` and `cadence org inspect` are local-only: they make no
network call. For a remote org the `credential` object reports what this
machine holds, and `live_verified` is always `false`:

- `no_credential`: nothing stored; run `cadence login`.
- `renewable`: a renewable CLI session is stored and not yet past its idle or
  absolute expiry. The next command renews the short access token automatically.
- `access_valid_until_expiry`: a legacy login with a valid access token and no
  renewable session; it expires without renewal.
- `reauthentication_required`: the stored session is past its idle or absolute
  expiry, or the access token expired with no renewable session.
- `invalid_local_credential`: the file cannot be read or validated. This is a
  local problem, not proof that the server revoked it.
- `unavailable`: the local store cannot be inspected (for example a permission
  error). Only a missing file is `no_credential`; a stat failure is never
  reported as absence.

The `credential` object carries scope names, access expiry, session idle and
absolute expiry (kept even when the session has expired), whether a renewal is
pending, and two local observations: `stored_at` (when this machine last wrote
the credential file) and `observed_at` (when the status was read). These are
local facts only. They are not a server renewal result and prove no renewal
health; `live_verified` stays `false`. Renewal health is `unknown` until a command
observes a renewal result, and it is never inferred from a file timestamp. It
never carries a bearer or refresh token. `login_status` keeps its legacy values
(`local_peer_authority`, `not_connected`) for compatibility; it reads
`credential_saved` when a credential exists, instead of always `not_connected`.

### Access token lifetime and renewal model (CAD-1434)

Access tokens are deliberately short-lived (about 5 minutes). A command renews
the token through the saved renewable CLI session when it is within 60 seconds
of expiry, under a per-org lock with a persisted pending operation. A valid
session therefore survives many access expirations and idle gaps while its idle
and absolute expiry have not passed. Terminal session loss asks for one login;
a transient renewal failure keeps the saved login for a later retry. The access
token TTL is not extended to work around refusals.

### Remote ticket writes (CAD-1179)

Three write verbs run on a remote org. Authorship and provenance are the
enrolled identity inside the verified envelope — a caller can never name
an author, actor or operator in the request.

```sh
cadence --org acme issue new "title" --project key      # --project required
cadence --org acme issue comment CAD-1 -m "text"        # author = enrolled identity
cadence --org acme issue show CAD-1                      # reports the `rev` token
cadence --org acme issue set CAD-1 status=doing --if-rev fnv1a:0123…
```

Refusals happen locally, before a byte is sent: `issue new` without
`--project` (the hosted tracker has no cwd to derive one from) or with
`--id`/`--status`; `issue comment` with `--author` or `--kind`; `issue
set` without `--if-rev`, with an empty revision, with `--force`, or
naming more than one ticket. `--file` bodies are read client-side (`-` = stdin) and
bounded by the 32 KB cap — never remote paths. `issue
edit/attach`, provider verbs and operator verbs are not on the wire.

`issue set` is compare-and-swap: send the `rev` from `issue show`
(`--if-rev`); a stale revision answers the conflict payload (HTTP 409
at the route, `{conflict, current_rev, card}` in the body) and writes
nothing. `--if-rev` is also accepted locally, where it uses the same
under-lock check; without it, local `issue set` keeps its existing
unconditional behavior.

A wake retry is the only automatic retry — it fires only on the
pre-forward `waking` verdict. A timeout, dropped connection or error
after forward is an explicitly unresolved outcome: the mutation may or
may not have landed, so reconcile with `issue show`/`issue log` rather
than re-issuing blindly. A 200 confirms the write was accepted — not
that the tracker survived a later hard loss; durability is a hosted
operational concern outside this client feature.

Each collaborating writer enrolls independently: a separate
`XDG_CONFIG_HOME` (or separate account/machine) per identity, its own
`cadence login` grant — credentials are never copied between homes.

### Remote message send (CAD-1568)

`cadence send` (and its `message send` form) enqueue a durable coordination
message to the hosted **master**, for a verified human workspace owner.
This is not generic container execution. The recipient fence is the
canonical `master` agent only: any other recipient spelling is a typed
refusal, and the daemon resolves the exact `master` agent row — another
worker's session or thread id that happens to spell `master` is never
substituted, and no refusal silently reroutes or enqueues elsewhere.
Personal assistants require separate assistant authority and local workers
use the `local-agent`/local-assignment plane; both are unsupported
remote-send recipient classes. Member-to-member remote coordination and
the local-worker follow-up journey are explicitly deferred on the ticket —
refused, not silently fulfilled. Agent/service envelopes cannot inherit
their parent human's messaging or private readback authority.
Roll out the matching native-server authorization guard before exposing
the upgraded CLI to a hosted workspace. A CLI upgrade alone does not
establish these sender-ownership and private-readback guarantees.
This feature does not authorize a production rollout.

```sh
cadence login acme                                            # owner-approved hosted grant
cadence --org acme wiki put global/docs/kit.md --file kit.md  # stage inputs in the wiki
cadence --org acme send master --message ask-42 --text \
  "Please coordinate review of the staged kit — see global/docs/kit.md."
cadence --org acme message read ask-42     # your message: body + state + result
```

The wiki staging step requires an existing `wiki.write` grant; `cli.write`
alone permits sending a message, not writing wiki pages. Use already-staged
inputs when the login has only CLI scopes.

`--message <id>` is **required** on a remote send: it is the caller-chosen
stable id (identifier grammar, never `sys-*`) the send is deduplicated
and reconciled under. The send binds the recipient's Home
`message_conversation` at enqueue: a first-ever send creates the master's
Home thread transactionally with the durable message, and every later
send to it reports the same `conversation`. The successful reply is the
complete enqueue receipt — the echoed stable `message` id, the canonical
`alias` (`master`), the lifecycle `state`, the `duplicate` bool, the
immutable `workspace_id` (the workspace ID verified at login, not the
slug), the authenticated human's canonical `sender_subject`, the `cli-*`
display `sender`, and the stored `conversation`. That receipt proves
durable enqueue only — it is never the delivery, execution or result of
the message, which remain `message read`'s report.

A `2xx` answer without that complete correlated receipt — missing,
malformed, or bound to another message, alias, workspace or subject — is
`unknown`, exactly like a lost reply: reconcile by the stable `--message`
id. It is never reported as a success and never replayed. `message read
<id>` returns your own sent message's body plus `state`, sanitized
`result`, `error` and `awaiting_report`, with the same `workspace_id`,
`sender_subject`, `sender`, `alias` and `conversation` binding the send
receipt carried. Absent and foreign message IDs return the same
`not_found` refusal. Browser member-chat messages are not CLI-owned.

`--ready`/`--force`, `--nudge`, `--priority`/`--supersedes`, `--reply-to`
and `--task` are local-endpoint and routing surfaces: they refuse before
a request, they are not dropped. `--file` bodies are read client-side
(`-` = stdin) under the message enqueue cap; an oversized body is
refused whole, never truncated.

Remote refusals carry bounded codes, never raw server text:
`recipient_class_unsupported` (any target other than `master`),
`not_permitted` (not a human owner for master chat/passive inbox inspection,
or not a human principal for CLI message readback),
`message_id_conflict` (a `--message` id already used with different
content or owned by another principal — reconcile or choose a fresh id),
`rejected` (for example a reserved `sys-` id),
`insufficient_scope` (login lacks the operation's `cli.*` scope),
`invalid_request` (missing/empty body or id at the request boundary),
`not_found` (no CLI message visible to this human under that id).

If a send's outcome is lost (timeout, dropped connection, 5xx after the
call, or a 2xx whose receipt is not the complete correlated one above),
do not guess: run `cadence message read <id> --org <org>` using the
original human login and workspace. `not_found` does not prove that no
other principal owns the ID. After reconciling the destination and identity,
retry with the **same** `--message` id: the same principal/id/content does
not enqueue another message. This is enqueue dedupe, not exactly-once
downstream effects. Never blindly resubmit under a different ID.

Developer acceptance uses an isolated native daemon, the real AgenticOS
hosted handlers and durable stores, and the synthetic provider. Point at
an API checkout containing the native-forwarder and TLS-bridge test helpers
with its dependencies installed:

The `cli-runtime-test` feature is an explicit manual-harness opt-in.
Ordinary `test-seam` CI retains the CLI tests in their original binary
target and still runs the new daemon ownership/refusal acceptance checks.

```sh
CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 \
  cadence --state-dir "$HOME/.local/state/cadence" build-slot run build -- \
  cargo build --locked --features test-seam --bin cadence

CAD1568_AOS_API_DIR=/absolute/path/to/agenticos-v2/apps/api \
  CAD1568_CLI="$PWD/target/debug/cadence" \
  CARGO_BUILD_JOBS=2 CARGO_PROFILE_TEST_DEBUG=0 \
  cadence --state-dir "$HOME/.local/state/cadence" build-slot run test -- \
  cargo test --locked --features cli-runtime-test --lib \
    cad1568_real_hosted_message_roundtrip_and_refusals \
    -- --ignored --test-threads 1 --nocapture
```

Use the local admission broker's actual state directory above. Unlike
`--org local`, this does not export a local instance binding into the
tested CLI process; no caller identity variables are cleared.

The fixture launches that debug executable through its real entry point,
parser and dispatch for login, wiki staging/reference, send and exact-result
readback. The library no longer borrows the binary's CLI modules. The
`test-seam` build adds only an explicitly armed private-CA/loopback transport
for these subprocesses; certificate-chain, hostname, signed-envelope and
authority checks remain enabled. Release builds reject `test-seam`.
The scenario requires an external coordinator and refuses a real parent
`CADENCE_ALIAS` before constructing isolated synthetic-persona environments;
never unset a managed identity to run it.

This is not an installed release CLI or a deployed workspace. The controller
primes CLI and wiki JWKS caches over verified disposable TLS; fresh
handler-thread JWKS fetching and real master commissioning/model policy are
not proved. Production messaging runtimes, credentials and providers are not
exercised.

## Issuer-bound service enrollment (CAD-717)

The separate `cadence remote enrollment` commands prepare a local implementer
for the AOS-75 hosted result gateway. They do not route ordinary Cadence
commands to the cloud. AOS-75 is default-off on AgenticOS staging; an enabled,
deployed issuer and its owner-issued `hcs_` service credential are prerequisites.
The device `agc_` credential above cannot substitute for `hcs_`.

The operator first establishes an independent trust pin. In a private 0700
directory, place the exact HTTPS API origin in a 0600 `trusted-issuer` file,
with at most one final newline. This file must exist before bootstrap; the CLI
never creates or changes it. For example:

```sh
install -d -m 700 /absolute/private/hosted-worker
install -m 600 /dev/null /absolute/private/hosted-worker/trusted-issuer
printf '%s\n' 'https://your-agenticos-api.example' > /absolute/private/hosted-worker/trusted-issuer
secret-manager-read | cadence remote enrollment bootstrap \
  --issuer https://your-agenticos-api.example --org ws_company \
  --audience https://company.board.example --client-agent worker \
  --enrollment-dir /absolute/private/hosted-worker
```

Bootstrap refuses a different issuer before sending the service token. It
exchanges `hcs_` for an at-most-five-minute bridge token and enrolls one
implementer with `results.submit`. AgenticOS validates the service credential's
current owner, organization, board audience, scope, expiry and revocation. The
client checks the returned service principal, exact organization and audience,
bridge grant, server-assigned subject/bridge/agent IDs, role, capability and
expiry. It stores the child and service credentials in a separate private 0600
record; CLI output and Debug display only IDs and expiry.

`cadence remote enrollment status --enrollment-dir ...` shows the binding.
`renew` explicitly re-exchanges the stored service token; an expired or revoked
service token fails. `remove` deletes only the local record. To change issuer,
organization, board or client agent, remove the old record and bootstrap a new
directory. Enrollment/renewal and removal are serialized, so a completed remove
cannot be undone by an earlier in-flight renewal. A send holds a shared lock
while checking and using the child token; renewal/removal wait for it.

The local record has owner/mode checks, no-follow reads, atomic replacement and
a checksum against accidental corruption. The checksum is not a MAC: a hostile
process running as the same Unix user can read the tokens and recalculate it.
Use a separate OS account or stronger credential store if that adversary is in
scope. Every status/send rechecks the independent issuer pin. AOS-75 has no
child-token revocation introspection endpoint, so local preflight can detect
expiry and mismatch, but revocation after enrollment is rejected by the server
at result receipt. This is not a claim of local pre-network revocation proof.

CAD-716's send command loads the immutable outbox destination, uses
`remote_enrollment::with_current` to compare its org, board origin, subject and
agent, and invokes the pinned HTTP sender only inside that callback. Send takes
`--enrollment-dir`, not caller-provided `--org` or `--audience` assertions. The
sender remains subject to independent review and the production gateway stays
off.

## Short-lived hosted browser enrollment (CAD-729)

When the AOS-76 device and gateway flags are enabled on the chosen issuer, an
owner may approve a separate hosted browser grant:

```sh
cadence remote enrollment browser \
  --issuer https://your-agenticos-api.example --org ws_company \
  --audience https://company.board.example --client-agent worker \
  --enrollment-dir /absolute/private/hosted-worker
```

The same independently created `trusted-issuer` pin is required before the
first request. `--no-open` prints the URL and code for another device. The CLI
can opt in to a staging Cloudflare Access ingress pass by placing a separately
provisioned `access-issuer.json` in the same private enrollment directory with
mode 0600 and owner-only directory permissions:

```json
{"issuer":"https://your-agenticos-api.example","client_id":"<Access client ID>","client_secret":"<Access client secret>"}
```

The file's issuer must exactly match `trusted-issuer` and `--issuer`. The CLI
reads it only for browser enrollment and sends the two `CF-Access-Client-*`
headers only to that issuer's device code, device token and child enrollment
paths. It refuses redirects, ambient proxies, changed config, Access challenge
responses and malformed grants. Do not put this secret in argv or an environment
variable. Access opens the API ingress only; AgenticOS still requires PKCE,
owner consent, exact organization and audience, and its own `hct_` grant. The
service enrollment and board result sender do not use this Access pass.

The CLI requests only `bridge.enroll` and `results.submit`, proves possession with a
fresh PKCE verifier, and accepts only the issuer's matching owner grant. It
uses the one-time `hct_` bridge to enroll an implementer child through
`/v1/hosted-cadence/enroll`. It does not use the unrelated `agc_` tools login or
an `hcs_` service token. The record stores only the child, not the device code,
PKCE verifier, bridge bearer, or browser cookie. The child identity and
capability request are explicit so later team work can enroll a distinct
reviewer rather than sharing one identity.

The child expires no later than its bridge, which is capped at five minutes.
After expiry, repeat `browser` and owner consent; `renew` deliberately refuses
a browser record. This is a short-lived result sender, not persistent team
login. AOS-68's continuity routes are not mounted and no browser renewal or
sleep/wake guarantee is implied. Local removal does not revoke the hosted
credential; the server checks live membership and revocation at receipt.
Production flags remain off until a separate rollout approves them.

## Board sign-in through the device grant (CAD-777)

The same grant signs a remote operator into the board — no SSH, no
on-host link. Configure the triple once on the *daemon* (CAD-841):

```sh
cadence ui device-login set --issuer https://your-agenticos-api.example \
  --org ws_company \
  --subject op_1
```

Issuer + org + at least one subject, or none (env
`CADENCE_DEVICE_LOGIN_ISSUER` / `CADENCE_DEVICE_LOGIN_ORG` /
`CADENCE_DEVICE_LOGIN_SUBJECTS` — the last comma-separated — also feed
`--device-login-*` flags on `ui run`/`ui start`, a thin client that
pushes the same `set` before serving). The write goes through the
operator-secret RPC and lands in the daemon's store at
`<state>/operator/device-login.json` — it applies live to every board
on the state dir (no restart) and survives board restarts;
`cadence ui device-login show` prints it, `clear` removes it. The
subjects are the operator's allowlist: only those verified issuer
principals may mint a board session. Find yours with
`cadence auth status` (it prints `principal.subject_id`); a refusal
also names the subject it saw. Unconfigured daemons answer both routes
404.
`POST /api/session/device/code` requests a `read draft` grant and
returns the user code, verification link and a pending id.
`POST /api/session/device/poll` reports `pending`/`slow_down`/
`denied`/`expired`, and on approval verifies the credential through
`/v1/runtime/session` (named principal, exact workspace, `read`
scope) before the daemon mints a board session for an allowlisted
subject only — any other verified workspace member gets
`device_subject_not_allowed` (403, no cookie). Same cookie shape as
`/api/session`, 12 h idle / 24 h absolute, one per verified subject.
The device code stays server-side under a bounded
TTL-pruned pending map; the `agc_` is verified and dropped, never
stored or returned. Agent peers are refused without side effects, and
an ambiguous exchange is a new code, never a retry. This signs the
operator in; it enrolls no provider connection and grants no agent
anything.

## Remaining cloud contract

Browser-based hosted consent, assignment delivery, active-channel revocation,
durable sleep-safe application witness, and actual hosted Cadence command
transport remain dependencies. Current device `read/draft/send` scopes belong
to the issuer contract; they do not grant Cadence operator privileges. No board
cookie authentication or assertion guard is weakened by this increment.

### Read-only diagnosis before repeated login (CAD-1434)

Diagnose a hosted refusal in this order. Each step is read-only and none
requires another login:

1. `cadence org inspect <org>`: read `credential.state`, the scopes, the access
   expiry and the session idle/absolute expiry. `no_credential` or
   `reauthentication_required` means login is a real option. `renewable` only
   says a renewal can be attempted; it does not prove the server accepts the
   session, so it is not a health check.
2. Read the **already captured** safe error's HTTP status, `category`, `reason`
   and hop/request ID, if present. Do not re-run a write or upload to obtain
   diagnostics. `authorization` means check the workspace role and target path
   (a login does not change it); `authentication` names one fresh login as a
   remedy outside this read-only diagnosis; `uncertain` does not prove expiry;
   `availability` means wait, not replay an uncertain mutation; `unknown` means
   report the safe status and code. Do not blindly repeat login.
3. Record the captured safe error, org endpoint and client revision
   (`cadence --version`) on the ticket. Keep any already known issuer, gateway
   and daemon revisions separate; unknown versions remain unknown. Do not paste
   a token, the credential file or a raw response body.
4. Wiki writes and uploads: inspect the workspace role and intended target path
   using existing read-only evidence. A refused path is not a reason to try
   another shared root. If outcome reconciliation needs a new inspection, use
   only an explicitly read-only command such as `issue show` or `wiki ls`,
   within your existing permissions. A dropped connection or missing
   acknowledgment does not prove that the write did not land. Stop if these
   reads cannot reconcile the outcome; do not retry the write or upload.

Any fresh small **test write** is outside this read-only production diagnosis.
It requires an authorized isolated workspace and explicit operator authorization,
only after any prior uncertain outcome has been reconciled. It is not a
production retry, an authority change or a prerequisite for collecting diagnosis
evidence.
