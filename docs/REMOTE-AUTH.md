# AgenticOS issuer sign-in (CAD-539, first increment)

This increment authenticates a named principal with an AgenticOS issuer. It does
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

## Remaining cloud contract

Board-host audience binding, board bearer exchange, service token administration,
short-lived access with refresh, per-agent enrollment/roles, active-channel
revocation, durable sleep-safe delivery, and actual hosted Cadence command
transport remain dependencies. Current device `read/draft/send` scopes belong
to the issuer contract; they do not grant Cadence operator privileges. No board
cookie authentication or assertion guard is weakened by this increment.
