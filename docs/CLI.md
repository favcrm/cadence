# CLI: which verb ships a build

Two loops, decided by the store's mode, not by what the caller types.

## Production: `cadence update`

One verb ships a build to a production store: it claims the rollout lease,
backs up the store, records the backup as the lease receipt, installs the
attested build side by side, drains, switches, health-checks and releases
the lease.

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

## Exit codes

A failure prints one JSON object on stderr (`{"error", "kind"}`) and exits
with the code for that kind. The table — code, kind, meaning and whether a
retry is safe — lives in `skills/cadence/SKILL.md` under "Errors and exit
codes"; it is the one copy, so this file does not repeat the numbers.
Verbs that wrap another program pass its exit code through, and `--help`
and `--version` exit 0.
