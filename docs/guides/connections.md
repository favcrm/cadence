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
For encrypted Connections, revocation atomically removes grants/defaults and
closes only unexecuted effects; executing or reconcile-needed effects retain
their uncertain history. Legacy custody retains its existing revoke behavior.
Re-enrolling the same provider/account creates a new connection incarnation.
An old ID cannot modify the replacement.
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

## Encrypted persistence in an isolated self-hosted instance

Encryption at rest is **not agent isolation**. Custody status deliberately
reports `isolation: "none"` and `protected: false`: an unconfined same-UID
process can still read owner-accessible key files, ciphertext or process memory.
Do not treat an unlocked vault or risk acknowledgement as protection from agents.

Before starting the isolated instance, its owner selects the external key through
`ISOLATED_STATE/custody-encrypted.json`, an owner-only `0600` path-only record:

```json
{"key_file":"/absolute/owner-private/connections.key"}
```

The dedicated key must be uniformly random, exactly 32 bytes, in a `0600`
single-link regular file under an owner-controlled `0700` directory outside the
state, tracker, app and artifact trees. Symlink or untrusted writable traversal
refuses. No key-generation, key-export, key-rotation or key-recovery command is provided.
Absent configuration retains legacy selection; missing encrypted configuration
or material never silently downgrades existing encrypted references.
Legacy metadata and artifact inspection allows owner-controlled, non-private
directories; it does not impose encrypted-key permissions on an unconfigured
instance. A present vault or any `encrypted-sqlite` reference still refuses
missing configuration. Opaque legacy references are not reinterpreted or made
usable; portable coordination exports discard their credential authority.

```sh
cadence --state-dir ISOLATED_STATE connection custody initialize --accept-same-uid-risk
cadence --state-dir ISOLATED_STATE connection custody unlock --accept-same-uid-risk
cadence --state-dir ISOLATED_STATE connection custody status
cadence --state-dir ISOLATED_STATE connection custody lock
```

Initialization provisions a fresh vault but leaves it locked. Review the current
Connections and grants before explicit unlock: reference authentication is not
proof that restored authority is the freshest snapshot. Restart always locks
custody. A contended encrypted operation returns busy without scheduling a later
lock, rotation or revoke.

Under this selection, new credential-bearing enrollment supports only the typed
SMTP shape. Direct authenticated owner Connections management and native operator
SMTP bind/rebind/show/test-send are the supported consumers. Password-only SMTP
rotation inherits verified settings under the same custody section and exact
caller revision used for publication. Use `connection rotate-smtp`, not the
legacy `platform rotate` grammar, for an encrypted sender.

Rotation leaves an existing SMTP binding stale. SMTP show reports that stale
state with the current sender metadata; sending still refuses until the operator
explicitly rebinds.

Generic effects, app capability/artifact execution, mounted screen services and
campaign sends refuse encrypted targets even when unlocked or granted. Encrypted
initialization, unlock, enrollment and credential use with hosted `smtp.internal`
also refuse before key access or relay work. Existing legacy records keep their
durable backend tags and legacy behavior; this is not a migration or erasure of
plaintext stores and historical backups.

On Linux, ordinary backup/export excludes the vault and external key and refuses
custody source/output overlaps. macOS retains ordinary backup/export only for
unconfigured legacy state; configured encrypted-state artifacts refuse on
non-Linux before output inspection or creation. These artifacts are not key
recovery: losing the key loses access to its ciphertext. No anti-rollback or
physical power-loss guarantee is implied.

These owner commands are not chat-driven setup or CLI browser enrollment.
Trusted credential-entry components, isolated agent access and hosted encrypted
execution are separate requirements; never paste credentials into agent chat.


## HTTP contract

All these routes require the same proven operator boundary as native RPC:

| Method | Route | Request |
|---|---|---|
| GET | `/api/connection-providers` | No query |
| GET | `/api/connections` | No query |
| GET | `/api/connections/:id` | No query |
| POST | `/api/connections` | `provider`, `account`, `shape`, `scopes`; `token` for token shape or SMTP `host`, `port`, `tls_mode`, `username`, `secret`, `sender` (optional `sender_name`); optional `accept_same_uid_risk` |
| POST | `/api/connections/:id/rotate` | `token` or fresh SMTP `secret` with optional inherited transport/sender fields; optional `scopes`, `accept_same_uid_risk`, `expected_connection_id`, `expected_revision` |
| POST | `/api/connections/:id/revoke` | Optional `expected_connection_id`, `expected_revision` |
| POST | `/api/connections/:id/status` | `{}`; local configuration check only |
| GET | `/api/connection-custody/status` | No body or `{}` |
| POST | `/api/connection-custody/initialize` | `accept_same_uid_risk: true` |
| POST | `/api/connection-custody/unlock` | `accept_same_uid_risk: true` |
| POST | `/api/connection-custody/lock` | No body or `{}` |

Encrypted rotate/revoke requires the exact expected connection ID and revision.
CLI and HTTP preflight may resolve that pair once through Connection show for
the invocation; a supplied expectation is never replaced or automatically retried.
Legacy callers may omit expectations, but supplied fields are never silently
dropped: the ID must be a string and the revision an integer in
`1..=9223372036854775807`. Null, wrong types and out-of-range revisions refuse.
Custody bodies accept acknowledgement only, never key bytes or configuration
paths. The closed request schemas live in
[`src/ui/connections.rs`](../../src/ui/connections.rs).

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
