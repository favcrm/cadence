# AgenticOS issuer sign-in and hosted enrollment (CAD-539, CAD-717, CAD-729)

CAD-539 authenticates a named principal with an AgenticOS issuer. It does
**not** connect issue, message or team commands to hosted Cadence. Those commands
still use the local daemon. Successful authentication reports
`hosted_cadence: not_configured`; do not treat it as remote connectivity.

The protocol is pinned to AOS-49 draft PR 62, head
`5c19a64ce1a530ba6728ceb49cd39fc41bff1300`. That PR has device API routes and a
consent mockup; its web approval page is absent and the routes are not merged
into AgenticOS main. These commands therefore require an explicitly configured
issuer with that contract deployed before real sign-in is possible.

## Browser or device sign-in

```sh
cadence login --issuer https://your-agenticos-api.example --org ws_company
```

The CLI requests `read draft`, prints the verification URL and user code,
opens the browser, and polls the issuer. `--no-open` prints the same URL/code
for completion on another device. No browser cookies are copied. Consent must
approve the exact workspace ID supplied with `--org`; a different organization
fails before persistence. Denial, expiry, slow-down and pending states follow
the device contract. There is no automatic retry after an ambiguous failed
exchange; restart login rather than reusing an uncertain one-time grant.

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

## Remote org transport (CAD-1019)

`cadence login --issuer <api-origin> --org <workspace-id> --slug <slug>`
runs the hosted-cadence owner-consent device grant (PKCE, `hcd_` device
code, `hct_` bridge credential) and records an org whose connection is
`Remote { endpoint: https://<slug>.cadencecloud.app, org_id }` — slug,
endpoint and org id all come from the issuer's verified grant, never
derived from each other. Login never moves the saved default: without
`--use` the org is recorded but the existing destination (ambient local
on a fresh machine) stays selected; pass `--use` to make the remote org
the default. The `hct_` is stored at
`$XDG_CONFIG_HOME/cadence/remote-auth/cli-<slug>.json` (0600) and is
bound to the org id and endpoint it was issued for — it is never sent
to another host.

### First login on a fresh machine

The credential directory is created by login itself (0700, owner-only).
The operator then establishes the independent issuer pin before any
request goes out: login refuses with an actionable message until the
exact issuer origin sits in a 0600 `trusted-issuer` file inside that
directory. The CLI never creates the pin — it is the operator's own
trust decision:

```sh
cadence login --issuer https://your-agenticos-api.example --org ws_company --slug company
# → refused: "Trusted issuer pin is missing. Create it yourself, then retry: …"
#   (the refusal prints the exact pin path — on this host it is
#   ${XDG_CONFIG_HOME:-$HOME/.config}/cadence/remote-auth/trusted-issuer,
#   or the --auth-dir you passed)
printf '%s\n' 'https://your-agenticos-api.example' > "${XDG_CONFIG_HOME:-$HOME/.config}/cadence/remote-auth/trusted-issuer"
chmod 600 "${XDG_CONFIG_HOME:-$HOME/.config}/cadence/remote-auth/trusted-issuer"
cadence login --issuer https://your-agenticos-api.example --org ws_company --slug company
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
`message_inbox`, `issue_new`, `issue_comment`, `issue_set` — and
`arguments` is a strict per-verb payload. Any
other command is refused locally and never sent.

A sleeping company answers `503 {"state":"waking","retry_after_s":N}`
plus `Retry-After`; the CLI waits and retries inside `--wake-timeout`
(default 120 s), printing `waking <org>… (Ns)` on stderr while stdout
stays JSON. The budget lapses into exit 75 (`busy`). `wake_failed`, an
unavailable remote, a redirect, or a refused credential (`401`/`403`)
end the command with a `cadence login` hint — there is never a fallback
to local or another org, and no token is ever printed. Operator-only
and every unlisted verb stay refused remotely; the remote path carries
no operator proof, and the hosted `issue_set` verb accepts no `force`
field — the `status=done` evidence gate has no remote override.

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
bounded by the 32 KB cap — never remote paths. `message send`, `issue
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
