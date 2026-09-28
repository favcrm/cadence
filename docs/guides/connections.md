# Provider connections

Connections identify an exact provider account in this workspace. An operator
can inspect reviewed capabilities and manage supported credential custody.
Creating or selecting a connection grants no worker, app or run permissions.
The Settings Connections page and executable app bindings are separate work.
These provider accounts are different from local organization/socket routing.

## Inspect configuration

```sh
cadence connection providers
cadence connection ls
cadence connection show CONNECTION_ID
cadence connection check CONNECTION_ID
```

The commands require the running daemon and proven operator authority. There
is no privileged offline fallback. JSON receipts describe the provider's
reviewed capabilities, exact account, connection ID and credential revision.
Provider output separates `descriptor_available` from `manifest_status`, reviewed and reported pins. Neither field proves upstream connectivity. Unknown or unavailable providers stay unavailable. A declaration is not proof
that the deployed service has the reviewed contract.

Check inspects local configuration. It does not send a provider effect or
claim successful upstream connectivity. Inspect provider registration,
custody availability and deployment pin status separately. Missing or mismatched
trusted deployment metadata does not enable reviewed read/draft behavior.

Local outbox is a built-in connection, with no credential enrollment. Its
presence grants no publishing rights. Its reviewed `blog.publish` and
`social.post` capabilities both map to the Local sink. AgenticOS `social.post`
maps to its upstream approval handoff, rather than the Local sink. These shared
capability names describe compatible purposes; each provider retains its exact
reviewed tools, scopes and effect classification. Neither descriptor grants
execution or substitutes for app/run/effect approval. Built-in references cannot be rotated or
revoked through credential management. Existing app/run approvals and effect
release policies remain separate.

## Supported token enrollment

Use only a provider whose reviewed descriptor explicitly offers token
enrollment. Feed the scoped token through stdin, rather than command arguments:

```sh
cadence connection create PROVIDER --account ACCOUNT --scope SCOPE --token-stdin
cadence connection rotate CONNECTION_ID --token-stdin
cadence connection revoke CONNECTION_ID
```

Token input is bounded to 8192 bytes, must be UTF-8 and must be nonempty; trailing
line endings are removed. Never put a credential into a guide, app bundle,
project file or browser storage. The daemon stores bytes using its existing
custody implementation and returns metadata only.

Existing same-uid custody exposure remains explicit. First enrollment can
refuse `custody_unprotected`; use `--accept-same-uid-risk` only when the operator
chooses to accept that existing risk. Rotation accepts optional repeated
`--scope` arguments; omitting them keeps the current scopes. It cannot target a
different provider or account.

Rotation preserves connection identity and advances its credential revision.
Revocation removes its existing grants/defaults and closes its pending effects
through the existing custody lifecycle. Re-enrolling the same provider/account
creates a new connection incarnation. An old ID cannot modify the replacement.
No management command derives a new grant or approves an app/run.

The reviewed `agenticos_external` provider accepts a company-scoped AgenticOS
device credential through token enrollment. Its account must be the exact
canonical `ws_<lowercase UUID>` workspace ID returned by the device exchange;
Cadence validates that shape but cannot derive the workspace from an opaque
bearer during enrollment. Other underscore provider or account names remain
invalid. Request
`provider.read` and/or `provider.draft` at the AgenticOS consent screen and
declare only the granted scopes on the connection. `cadence login` currently
requests the separate `read draft` audience and does not enroll this connection.
The external provider also needs an exact trusted deployment pin, app binding,
quoted run and explicit run approval before any paid call. Connection checks
are local and do not verify the credential against AgenticOS.

The separate hosted `agenticos` account retains Cadence's Send approval gate
without independently supplied deployment metadata. A matching trusted
embedding assertion is not live remote verification.

## HTTP contract

All these routes require the same proven operator boundary as native RPC:

| Method | Route | Request |
|---|---|---|
| GET | `/api/connection-providers` | No query |
| GET | `/api/connections` | No query |
| GET | `/api/connections/:id` | No query |
| POST | `/api/connections` | `provider`, `account`, `shape: "token"`, `token`, `scopes`; optional `accept_same_uid_risk` |
| POST | `/api/connections/:id/rotate` | `token`; optional `scopes`, `accept_same_uid_risk` |
| POST | `/api/connections/:id/revoke` | `{}` |
| POST | `/api/connections/:id/status` | `{}`; local configuration check only |

Unknown or duplicate fields, unsupported queries and wrong types refuse.
Credential-bearing requests are limited to 16 KiB; JSON responses to 64 KiB.
The path supplies identity: body fields cannot substitute another provider,
account or connection. Stolen operator cookies/session keys do not confer
operator authority on managed native peers or detached descendants. HTTP errors
do not reflect supplied credentials. No arbitrary provider URLs or tool calls
are admitted.

These APIs do not bind app slots, choose destinations, execute app resources,
fetch remote media or release sends. Future run/context bindings must pin the
connection incarnation and revision with actual run assignment, resource/action
and immutable approval provenance before enabling provider calls.
