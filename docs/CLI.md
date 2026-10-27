# CLI: which verb ships a build

Two loops, decided by the store's mode, not by what the caller types.

## Production: `cadence update`

One verb ships a build to a production store: it claims the rollout lease,
backs up the store, records the backup as the lease receipt, installs the
attested build side by side, drains, switches, health-checks and releases
the lease.

An already-linked release is not necessarily active. If the daemon still
answers with another build or is down, `update` finishes activation without
reinstalling the release. It still reads the target's schema and takes a
fresh backup under the current rollout lease before switching. A linked
target whose schema is older than the store refuses before any update
side effect. When the target daemon is already running, the update remains
a no-op.

```
cadence update --check                                   # what would happen
cadence update                                           # newest approved candidate
cadence update --to <full-40-hex-sha>                    # pin a commit
cadence update --check --to <sha>                        # report (and verify) that sha
cadence update --rollback
cadence update --as operator:<name>                      # explicit audit label (optional)
```

`cadence update` always maintains a local install: a saved remote
default org never routes it, and no registry row is created or switched
to make it work. A bare run binds the `local` connection when one is
registered, else the ambient state/tracker roots. An explicit selector
stays deliberate: `--org <remote>` or `CADENCE_ORG=<remote>` refuses as
local-only before any transport, credential read or container wake, and
`--org local`/`CADENCE_ORG=local` selects the registered local
connection. A `--state-dir` pin stays a plain local binding.

`cadence update` is operator-only and proves it the same way
`rollout claim --as` does: the process must run outside every pane and
managed endpoint, which an agent, a pane descendant or an unproven
caller can never satisfy — a forged `--as` or scrubbed environment
changes nothing. `--as operator:<name>` is now optional: it only names
the audit label. Without it, the run is labelled from the proven local
OS identity (the real uid and pid), never from `$USER`,
`$CADENCE_ALIAS` or a client-supplied field.

On a runtime the tracker's `pm.yaml` marks image-managed (`[hosted]`
`lease` configured), `cadence update` refuses before touching anything:
AgenticOS owns the binary lifecycle there and ships updates by
replacing the container image. A `CADENCE_AGENTICOS_URL` integration or
a container marker alone is not a managed runtime — a native host that
talks to AgenticOS still self-updates. Updating an image-managed
deployment means shipping a new image, not running `cadence update`
inside it.

A native update also synchronizes bundled public guidance
(`~/.agents/skills`, plus provider links) from the build that is active
when the command finishes. That includes a run that is already up to date
and both explicit and automatic rollback. The sync is the existing
non-force install: missing or stale managed files and repairable links are
written from that build's embedded bytes, and a second run with matching
files writes nothing. A build that predates `cadence skill sync` exposes
only a forced manual install, so its bytes are first staged outside the
home and then applied with the same non-force rules — a repeat run still
writes nothing and matching files keep their timestamps. It does not
restart a healthy daemon only to refresh
skills. `--check` and `update status` stay read-only; the target build's
guidance is not claimed verified before activation. `cadence skill install
[name]` remains the manual repair. A guidance failure is reported as
needs-attention and does not roll back a healthy binary or change the
update's exit status. File match and provider links do not mean an agent
session has reloaded the guidance — start a fresh session or reread it at
a task boundary. Client-only update does not install guidance or start a
daemon. A sandbox profile still does not write the shared home.

`--to` runs the same checks as `upgrade --sha`: the sha is on main, CI's
`test` job passed on that exact sha, the sha256 and manifest match, and the
build-provenance attestation verifies for the repository. A sha that is not
on main, or has no attested build, is refused with a sentence naming the
reason. Without `--to`, behavior is unchanged.

`--release-source https://<host>` (CAD-1455, opt-in) consumes the published
release from an owned endpoint instead of live GitHub queries: the catalogue
selects the subject (discovery only), the retained package's receipt,
build/promotion/completion Sigstore bundles and signed source-facts verify
offline, and the publication-time snapshot answers on-main, ancestry and
schema. An explicit source failure refuses — there is no silent GitHub API
fallback. Omit it to keep the default GitHub route; see
docs/RELEASING.md "Owned release source".

## Client only: `cadence update` (CAD-1425, CAD-1437)

One command serves both installations. A client-only installation
(`install.sh --client-only`, receipt `<prefix>/client-install.json`) selects
the public updater from its receipt, before any org or state work:

```
cadence update --check                    # selects the target and reports it; no download, no verify, no change
cadence update                            # newest public release, forward only
cadence update --version v0.2.0           # a newer pinned release
```

On a client-only installation `update` accepts only `--check`,
`--version TAG` and `--release-source` (the owned `public-client`
endpoint; CAD-1455 — same offline proof, forward-only rule and explicit
failure refusal). Every other native option (`--rollback`, `--to`,
`--now`, `--drain`,
`--keep`, `--json`, `--progress`, `--backup-dir`, `--repo`, `--link`,
`--releases-dir`, `--as`, even at its default value), `update status`, and a
global `--org`, `--state-dir` or `--wake-timeout` refuse with exit 3 before
any network, state or native work. A saved default org is ignored.

On an ordinary daemon installation `update` keeps the native behaviour above,
and `--version` refuses there with exit 3. A client-only installation is
identified only by its receipt; a receipt that is present but malformed
refuses and never falls back to a public update.

The same updater runs only from a client-only installation. It resolves locally whatever the
saved org default is, never uses or changes remote credentials, and never
contacts a tenant. `--check` only reports the selected target; it does not
download or verify the artifact. A real upgrade fetches only the fixed public
GitHub release through the operator's own GitHub CLI (`gh`) session, and runs a
candidate only after its build-provenance attestation verifies and its
manifest matches this platform and version. It does not downgrade, delete old
releases or touch the daemon.

From a client-only installation, local daemon and state commands refuse with
exit 3. Remote org verbs route to their org, and bounded
login, auth, org and offline management stays available.

This mode needs a published release whose `cadence update --help` carries the
client-only marker (`Upgrade a client-only installation`). The published
v0.1.1 predates the unified command and still has only `client-update`; it
cannot be upgraded to `update` in place. The one-time migration is a reinstall
with the `install.sh --client-only` shipped with the next signed
unified-command release. Two rejections follow from the marker:
- A new candidate run through the old v0.1.1 updater is refused: that updater
  has no `update` client support and no `client-update` probe matches it.
- v0.1.1's native-only `update --help` lacks the marker, so the new installer
  and the new updater refuse v0.1.1 as a client.
The old `client-update` command is removed with no alias.

## Development: `cadence dev`

`cadence dev` starts a disposable local Cadence with its own state dir,
tracker and board port (3110-3199). `cadence sandbox` remains a supported
command alias. This manages a development instance, not containers or VMs;
per-agent filesystem confinement and provider sandboxes remain separate.

```
cadence dev up [--name <n>] [--build <path>]   # create/start; writes the dev marker into the store
eval "$(cadence dev env)"                      # point this shell at it
cadence dev reload [--build <path>]            # restart from a local binary
cadence dev status | down | env | ls | reset <name>
```

Dev commands stay local even when the saved org selects hosted production.
They do not change that selection or access the hosted org's credentials,
transport or container wake path. An explicitly remote `--org` or
`CADENCE_ORG` refuses; explicit local state/tracker bindings and managed
callers' identity restrictions remain in force.

`dev reload` defaults to the newest of the repo's `target/release/cadence` and
`target/debug/cadence`. It claims no rollout lease, checks no attestation and
takes no backup.

For source-build tracker smoke, put the selected build directory on the
development process's `PATH` as well. Tracker Git hooks invoke
`cadence issue lint`; an older installed CLI can otherwise reject a new
schema even though `--build` selected the new daemon. Keep that executable
binding inside the disposable instance; do not replace the production binary
to make a local smoke pass.

Daemon and board startup must return valid JSON with a nonempty string
`state`; malformed or missing startup state is an error, not null success.
Initialization and stop commands are checked by exit status only.

### What makes a store a dev store

A store may change build without the rollout lease, and may run an unattested
binary, only when both hold:

1. it carries the dev marker `<state>/.cadence-dev`, written by `dev up`, which
   names the store's own resolved path (a copy elsewhere proves nothing); and
2. its root is a direct child of the sandbox base (`$CADENCE_SANDBOX_ROOT`,
   else `$XDG_STATE_HOME/cadence-sandbox`).

`CADENCE_PROFILE=sandbox:<name>` is not consulted for this: exporting it on
any other store unlocks nothing. `dev reload` refuses (and stops and starts
nothing) on a plain `--state-dir`, a marker copied outside the base, the
production state dir, or an unmarked store with the profile exported.
Production's rules (lease, backup receipt, attestation) are unchanged.
A sandbox created before the dev marker existed (before #831) loses the
lease exemption until `cadence dev up` re-marks it: run `cadence dev up` once
per old sandbox after the update that ships this change.

### Hosted production is not a dev instance

AgenticOS owns an image-managed runtime's binary lifecycle; update its image
rather than running the native updater inside it. Local development uses
separate state, tracker and non-production credentials. Do not remove agent
confinement or hosted write fencing because an outer container exists.

Daemon startup and the updater share a reader in the lifecycle module.
A missing `pm.yaml` or absent `hosted:` table leaves hosted lifecycle off;
other read failures, malformed YAML and invalid `hosted:` tables refuse.
A present null value or bare `hosted:` key is invalid; use
`hosted: { lease: off }` to explicitly disable lifecycle.
Reading this configuration acquires or renews no lease.

## Recovery / internals: `rollout` and `upgrade`

These are hidden from `cadence --help`, work unchanged, and exist for recovery
and scripts. Prefer `cadence update`; it does their steps in order.

- `cadence rollout claim | status | release | handoff | backup | grant ...`:
  the durable lease that gates a build change and a schema crossing.
  `rollout status` with no backup receipt prints a hint naming `cadence update`.
- `cadence upgrade --sha <sha> | --latest-main [--dry-run] [--restart]`: the
  low-level verified install that `update` calls. `--allow-unattested` rolls
  back to a hand-built release already on disk.

## Finding a command

`cadence commands` and `cadence schema` are offline. They do not open a
daemon, tracker, login, or organization, and they do not authorize the
command they describe. The live parser and handler stay authoritative.
Neither command reads credentials or defaults, and neither has a `--json`
flag: stdout is one JSON object.

```
cadence commands                         # core verbs
cadence commands "comment issue"         # at most five paths
cadence commands "issue comment" --limit 20
cadence commands --all                   # every public path
cadence schema issue comment
cadence schema agent wait
```

`commands` with a query returns `{schema_version, commands, total, truncated}`.
Each hit has `path`, `aliases`, and `summary`. No match exits 0 with an
empty `commands` array. `--limit` is 1–20 and needs a query. `--all`
conflicts with a query and with `--limit`. A bad combination exits 2 with
the usual usage error on stderr.

`schema` returns `{schema_version, path, usage, description, aliases, arguments}`
for that one path. Argument entries are parser metadata (identifier,
spelling, aliases, required, arity, action, possible values, help), not
default values. A public alias resolves to the canonical path. An unknown
or hidden path exits 2 and writes nothing to stdout.

These streams stay raw and are not part of discovery: inbox and events
NDJSON, logs and content bytes, a bare `build-slot acquire` token, and
`build-slot run` child stdout.

## Boards and placement

Two selectors, never one:

- `--org <slug>` selects whose board the command acts on — `local` (the
  default) or a hosted workspace. It never says where an agent runs.
- `--placement local|cloud|queue` filters `agent list` and `status` by
  where the agent's process runs: `local` on the daemon's host, `cloud`
  for a provider-hosted session (`devin · cloud`), `queue` for a mailbox
  with no process. The value is derived from `(provider, endpoint_kind)`;
  the flag only selects it.

`--org acme --placement local` is therefore the local-assignment plane:
the board owns the ticket and this machine runs it. `--org local` alone
is the standalone board, not "the local agent".

The hosted binding lives under the `agent` noun:
`agent connect|watch|assignments|disconnect|reassign`, each taking
`--org <hosted>`. `local-agent` remains a hidden alias for one release;
`local-agent report` is still the explicit claim reporter.

`cadence send <alias> --wait[=SECS]` waits for the turn's terminal state
(the same path as the hidden `message ask`). A remote send is a durable
enqueue only and refuses `--wait`.

## Settings > Agents management (CAD-1586)

`cadence settings agents` manages the same authoritative document and revision
as Settings > Agents. It never writes SQLite, `pm.yaml` or generated profiles
directly. JSON output separates saved configuration, host observations,
catalog results and history.

| Family | Operations |
| --- | --- |
| `show`, `apply`, `history` | Inspect the snapshot; atomically apply `--file <config.json>` or `--file -`; inspect recorded history |
| `harness` | `list`, `show`, `put`, `remove`; installed/version/live remain observations |
| `provider` | `list`, `show`, `put`, `remove`; inference-provider metadata, not connection credentials |
| `model` | `list`, `show`, `put`, `rename`, `remove`, `scope`; effective Pi master/worker allowlists are reported |
| `catalog` | `refresh`, `import --model <id>` or `--all-new`; imported Pi models start `off` |
| `role` | `list`, `set`, `clear`; team-role defaults and inherited provider selections |
| `profile` | `list`, `show`, `put`, `rename`, `remove`, `preview`, `sync` |
| `pin` | `list`, `put`, `remove`; metadata only, never package installation |
| `grant` | `request`, `list`, `allow`, `reject`, `revoke`; exact settings authority, not blanket agent privileges |

Use `cadence schema settings agents <family> <operation>` for exact fields.
Sparse `put` updates preserve omitted fields; creation requires the fields
that have no stored value. Pi models created without a scope start `off`.
Native-CLI models do not use the Pi scope gate.

Document changes require `--expected-revision` from the observed snapshot.
A stale revision exits 5 without partially updating configuration; the CLI
does not retry against a newer document. Model renames update settings
references atomically. Active-use and reference checks remain enforced.

```sh
REV=$(cadence settings agents show | jq -r '.revision')
cadence settings agents harness list
cadence settings agents model list
REV=$(cadence settings agents profile put review --role dev --model inherit \
  --job-file review-job.md --expected-revision "$REV" | jq -r '.revision')
cadence settings agents profile preview review --harness pi \
  --expected-revision "$REV"
cadence settings agents profile sync review --expected-revision "$REV"
```

Capture the new revision after each configuration change. Saving does not
sync exports. Sync reports per-file status and a `snapshot`; it preserves
unmanaged files. Preview starts no process and proves neither authentication
nor future launch success. Launch saved profiles through the existing
`cadence join ... --profile review` path.

### Managing-agent authorization

An unapproved agent can petition and list its own requests, not read or edit
organization-wide settings. Grant decisions require proven operator authority
(a verified, appropriately scoped human owner on the hosted path).

```sh
# Agent: request the exact operation, payload and observed revision.
cadence settings agents grant request model.scope --expected-revision "$REV" \
  --payload '{"id":"vendor/model","scope":"master"}' --reason "Recover master"
# Operator: inspect the request, then approve it against the observed revision.
cadence settings agents grant list
cadence settings agents grant allow "$REQUEST_ID" --expected-revision "$REV"
# The same agent: execute the approved operation.
cadence settings agents model scope vendor/model master \
  --expected-revision "$REV" --grant "$GRANT_ID"
```

Reads also accept `--grant`. Grants bind the real requester, registration,
organization, operation, payload and revision; they are single-use, revocable
and expire after 15 minutes. Requester identity is never a CLI field.
For `apply` or profile job files, approval binds the payload read from the file,
not its pathname. Changed content is refused. Optional `--digest sha256:<hex>`
checks canonical payload JSON, not a raw-file `sha256sum`. An argv-only master
permission cannot authorize a Settings mutation or issue another grant.
App-scoped assistant turns gain no settings authority.

### Master recovery and hosted use

`cadence master status [--grant <id>]` reports model selection, configuration
launch refusal, applicable allowlist and queue state. Unknown authentication
is reported as unknown. A new send blocked by known configuration returns
`master_unlaunchable` before enqueue; a busy master is a different condition.
An already-accepted stable message ID is reconciled first: an identical
retry returns its existing receipt even if the master is now blocked, while
reuse with a different envelope is rejected. Neither retry queues another copy.

After an authorized model-scope repair, `cadence master resume --grant <id>`
uses an exact `master.resume` grant and the existing resume implementation.
It resumes queued work without resubmitting messages. It neither unfences an
unknown outcome nor grants start/stop/deployment authority.

Owner-operated hosted commands use the explicitly selected `--org`. Request
Settings access with `cadence login --settings`; existing `cli.read`/`cli.write`
credentials do not imply `settings.read`/`settings.write`. The gateway
counterpart is AOS-290. Both gateway and native runtime must support the new
operations; an unavailable or unauthorized remote operation never falls back
to local settings.

Managed outside workers cannot reuse the stored owner's remote credential:
these Settings and master operations refuse with
`settings_agent_session_required` before loading credentials. Agent-specific
hosted CLI login is deferred to CAD-1592. Managing agents running on the local
or hosted runtime use that runtime's local daemon and exact grants instead.


## Observation output

`status`, `overview`, `memory ls`, and `build-slot status` print a table
on a terminal. A pipe, `CADENCE_JSON=pretty` or `compact`, or an explicit
`--json` prints the same payload as JSON. `--json` wins over
`CADENCE_JSON=table`. `CADENCE_JSON=table` keeps the table when stdout
is a pipe. Any other `CADENCE_JSON` value warns once on stderr and is
ignored.

`issue ls --fields` and `--summary`, and `--fields` on `issue epic ls`,
`milestone ls`, and `memory ls`, follow that decision. They do not need
a separate `--json` when stdout is already JSON, and they are refused
on a table.

With `--watch`, machine mode writes one compact JSON value per line and
no terminal-control bytes, including when `CADENCE_JSON=pretty`. A
terminal watch without JSON still redraws the table in place.

`build-slot release` prints the daemon receipt as JSON. `released: false`
stays a failure receipt; it is not rewritten as a success sentence.
`build-slot acquire` without `--json` is still the bare token.

## Portable coordination export and inspection

Use the native exporter for an authorized local source. A dirty tracker can
be captured privately without committing, stashing, or changing its live
HEAD, index, hooks, or files:

```sh
cadence export --state-dir /path/to/source-state \
  --portable --tracker /path/to/source-tracker \
  --snapshot-working-tree --out /private/new-package

cadence --org local inspect-portable /private/new-package
```

`--snapshot-working-tree` includes tracked modifications and deletions plus
non-ignored untracked regular files. Ignored tracked files remain included.
File/directory replacements are captured as deleted tracked files plus new
regular files, not rejected as directory members.
It preserves raw bytes and executable-bit semantics. Source hooks and
clean/smudge filters never run. This snapshot does not refuse regular
installed hooks in `.git/hooks`, or a plain `[core]` `hooksPath` key. The
`hooksPath` value is ignored rather than followed; hook files and `.git`
metadata are not package members, and the source checkout is not modified.
That allowance is snapshot-only. Ordinary export still refuses active hooks
and `core.hooksPath`. There is no bypass flag and no source-hook cleanup.
Import, activation, and app-data export are unchanged.

Existing metadata and config restrictions otherwise remain in force:
linked worktrees, config includes, prohibited path or command config keys,
Git filters, unmerged or sparse index entries, symlinks,
hardlinks, nonregular files, and unrepresentable paths still refuse rather
than being silently omitted. The output must be new and outside the source
tracker, including through symlinked parents. Without the flag, the existing
clean-tracker export is unchanged.

The capture is a private clean Git snapshot, not a change to the source.
The generated snapshot's Git objects are packed before metadata admission,
so distinct captured files do not each consume a loose-object metadata entry.
This does not run maintenance on the source or raise its metadata limits.
The existing exporter takes its online database snapshot, applies the
coordination projection and credential scrub/scan, and builds the package.
Independent package validation, comparison with the captured tracker inventory,
and a final source-tracker recheck must pass before no-replace publication.
Owned temporary capture and staging directories are cleaned on refusal.
A source-change refusal is not an automatic retry.

The coordination projection removes related private rows in one transaction
on the private database copy. Foreign keys remain enabled; their checks are
deferred until commit so removing human-room topics and metadata does not
fail on an intermediate state. A foreign-key failure at commit refuses the
export without publishing a package. The source database is unchanged.

Credential scanning treats the exact CLI flag `--author` as attribution,
not an authentication argument. Sensitive argument names and `AUTHOR=...`
assignments retain their credential checks. Recognizable provider tokens
still refuse, including when supplied as `--author` values.

History remains strict by default: credential-bearing message bodies refuse
export. With explicit operator consent, add `--redact-history-secrets` to a
`--portable` export (with or without `--snapshot-working-tree`) to sanitize
UTF-8 text in `messages.body` on the private copy only. The existing redactor
replaces credential-shaped spans, including warn-only findings and private-key
blocks, so this deliberately trades exported-history fidelity for sanitization.
Source history, message IDs, structured fields and tracker files are unchanged.
Non-text and invalid-UTF-8 bodies are not handled by this extra pass; all cells
still face the normal final scan. Credentials elsewhere still refuse export.

```sh
cadence export --state-dir /path/to/source-state \
  --portable --tracker /path/to/source-tracker \
  --snapshot-working-tree --redact-history-secrets --out /private/new-package
```

Redaction runs before the private-copy `VACUUM` so old bytes are not retained
in freed pages. The manifest and both export/inspection receipts report
`redacted_message_bodies`: a number (including zero) records opt-in and counts
changed bodies, not credential occurrences; a missing manifest field or null
receipt field means no opt-in. Exclusions disclose the transformation. This
metadata is exporter-declared provenance, not proof of source fidelity, and
does not bypass inspection, import admission, or any other credential check.

Portable `pm.yaml` validation accepts tracker metadata plus the native closed
`pi`, `host`, and `hosted` configuration sections. Files are copied byte for
byte, including comments; settings are not stripped or reserialized. Unknown
fields and non-mapping runtime sections refuse. Blocking credential findings
in raw configuration text or decoded runtime string values refuse export,
inspection, and inactive restore without echoing the values.

These sections remain configuration data, not credentials or permission to
start executors or acquire a lease. Inactive guards remain in force. Receivers
must include this configuration decoder: older builds can reject these keys
even when the portable format and database schema versions match.

Success prints JSON with `manifest_sha256`, `exporter_revision`, schema,
tracker revision, member count/bytes, exclusions, and `source_capture`
(original HEAD, snapshot HEAD, file/byte counts, inventory SHA-256 and capture
policy). Keep this receipt privately alongside, not inside, the package. The database and
tracker are **not a transactionally atomic snapshot**; the receipt explicitly
sets `atomic_database_and_tracker=false`.

`inspect-portable` independently checks an existing package's exact file set,
paths, hashes and sizes, then SQLite integrity, catalog/schema and the
inactive authority exclusions. It refuses WAL-mode database headers before
opening SQLite, preventing sidecar writes into the supplied package; native
exports use rollback mode. It prints JSON and does not restore or start a
daemon. It uses the normal local command authorization path; `--org local`
avoids a saved remote-org default. It neither authenticates the producer nor
proves present-day source fidelity, and always reports `import_ready=false`.
Destination compatibility and owner authorization remain separate checks.

This is a **coordination package, not a full workspace backup**:
`full_workspace=false`. App/business data, credentials/grants, local
checkouts and runtime files are excluded; execution authority is removed
and imported actors/dispatch disabled. Manifest SHA-256 detects changed
bytes, not a producer who rewrites both the package and its manifest.
Neither command grants an agent new source access or production privileges,
uploads data, imports a tenant, reconnects executors or activates a root.

## Hosted portable migration

`cadence --org <saved-org> restore --portable` uses the registered issuer's
`/v1/hosted-cadence/portable-migrations` API. It requires a compatible hosted
deployment, an enrolled CLI session and current workspace-owner membership;
the workspace header is a binding, not authority. Local controller admission
and activation are unchanged.

- `restore --portable /private/package --preflight` verifies the package
  locally and reads the destination binding without uploading or replacing.
- `restore --portable /private/package --request-id <stable-id>` uploads an
  inactive import. Keep the request id before sending anything.
- Add `--replace` and the exact `--expected-package-digest`,
  `--expected-destination-digest` and `--expected-generation` from preflight
  for an explicit replacement. The hosted service retains and verifies the
  old root before selecting a new epoch.
- `restore --portable --status <operation-id>` reads durable status.
  If the begin response was lost, use `--status request:<stable-id>`.
  Neither status form needs the source package or repeats a mutation.
- `restore --portable --rollback <operation-id>` restores the retained root
  with the **original** `--request-id` and the current destination digest and
  generation from status. Rollback is a separate, explicitly bound action.

The remote destination digest binds workspace/root/generation/lease facts;
it is not a checksum of destination file bytes. Retained-root byte verification
is separate. Uploads use the sealed packed-v1 member inventory and are checked
against the local manifest; the client sends each step at most once. Before any
manifest or pack upload, the
client reads `GET /:operationId/inputs` and skips only the manifest and packs
the server reports as durably completed. Untouched pending spans upload under
the same operation and request id. An unknown or unrecognized input effect
stops the upload with no re-send; recover with `--status <operation-id>`
(or `--status request:<stable-id>`), never by inventing a new id or repeating
an upload. Packages above 19,998 logical data members are refused; the
manifest's own inventory entry is the 19,999th.

Imports, replacements and rollbacks remain execution-disabled. A replacement
does not manufacture native activation receipts, enable imported actors or
start a master; ordinary wake/store access remains held. A completed mutation
must carry its terminal receipt. Pending, failed and unknown outcomes exit
nonzero; a successful status query can report any recorded phase without
claiming that the migration completed.

### Offline activation (AOS-339)

A hosted workspace whose root is replaced stays inactive: the imported snapshot
carries the native `committed_inactive` marker, and the daemon refuses to start
on it. An applier container cannot reach the live controller's operator
connection, so the activation runs offline, on a destination that does not yet
exist:

- `restore --portable <package> --activate-preflight` is read-only. It prints
  the package digest and the exact `required_reconciliation` list (every
  non-terminal message, job, task and monitor with its current status). Nothing
  is written.
- `restore --portable <package> --state-dir <new-dir> --activate-disabled
  --reconciliation <file>` restores the package into `<new-dir>` and activates
  it in one step, manual mode with execution disabled. `<new-dir>` must not
  exist; an existing empty directory, a managed root or a live state dir is
  refused before any write, and a refusal leaves no destination behind. The
  reconciliation file names every row from the preflight, each `retain_inert`;
  a missing, extra, duplicate or wrong-status entry refuses.

The output starts fenced: the daemon admits the root, opens the store without a
restart sweep, starts no actor and refuses every RPC outside the read-only
allowlist. The live `--activate` form (a controller operator connection, on an
existing root) is unchanged and remains the only path for a root that already
exists.

## Exit codes

A failure prints one JSON object on stderr (`{"error", "kind"}`) and exits
with the code for that kind. The table — code, kind, meaning and whether a
retry is safe — lives in `skills/cadence/SKILL.md` under "Errors and exit
codes"; it is the one copy, so this file does not repeat the numbers.
Verbs that wrap another program pass its exit code through, and `--help`
and `--version` exit 0.
