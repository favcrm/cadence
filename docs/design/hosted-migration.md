# Cadence production migration to AgenticOS (CAD-529)

This is a preparation and rehearsal contract, not permission to cut over.
CAD-525 targets the end-to-end local worker proof on October 2, rehearsal on
October 6, controlled cutover on October 7 and acceptance after 48 hours on
October 9, 2026. The existing production rollout owner performs any production
freeze, transfer or restart. Check `cadence rollout status` at execution time;
this document does not acquire or transfer that ownership.

## Inventory before choosing a transfer

| State or authority | Treatment | Required evidence |
| --- | --- | --- |
| SQLite coordinator store | Consistent verified snapshot; portable export sanitizes live endpoint authority | Schema, integrity, digest, table counts, message states and restored row-content match |
| Queued assignments/results, message order and completed receipts | Retain in store, then reconcile routing before dispatch | Queue count/state inventory; no unexplained missing message; no replay of uncertain effects |
| Running/submitting turns and `unknown` effects | Drain or reconcile on source before final export | Every turn has a correlated result or explicit reconciled disposition; no unresolved unknowns |
| Agent generations, turn tokens, source PIDs | Never transfer as live authority | Export nulls/scrubs them; fresh remote enrollment and credentials |
| Tracker repository and unpushed commits | Separate clean pinned clone at the freeze boundary | Exact commit, clean status, intended remote and hosted tracker configuration |
| Tracker hooks, host/repo paths and writer configuration | Review and regenerate for cloud | Local paths have an owner; hooks cannot push to an unintended tracker; cloud is authoritative |
| Roles, briefings, review evidence and other runtime files | Explicitly inventory; preserve necessary evidence through an approved private transfer | File owners and classifications; source export does not contain these files |
| Sockets, singleton locks, daemon instance, process/session handles | Recreate on target; never reuse as cloud authority | No copied socket/lock considered proof of ownership; actors are re-enrolled |
| Provider auth, `.env`, org/bridge tokens and private files | Excluded; reauthorize or provision via supported secret mechanism | No secrets printed in receipts, tracker or PR; browser/token flow owned by CAD-539/AOS-49; hosted token/org validation depends on AOS-62 |
| Developer checkouts, worktrees and unfinished code | Keep local; map each assignment to its local checkout | Local teams reconnect outbound with distinct PM/implementer/reviewer identities |

`cadence export` is a store transfer primitive, **not a full runtime migration**.
Its exclusions are documented in `src/backup/mod.rs` and SESSION.md. Review
`private/`, `sessions/`, `briefings/`, `reviews/`, `agents/`, `roles/`, operator
configuration and tracker configuration separately. Preserve required evidence
without copying credentials or pretending source provider sessions are resumable
cloud actors. Warn-only secret findings require operator review; the pattern
scanner is not proof that arbitrary prose contains no sensitive data.

## Offline rehearsal command

Use an explicitly owned `/tmp` directory containing an **offline rehearsal
copy** of the store and an existing regular `cadence.lock`. Use a clean isolated
tracker clone. Never point this at the production state or `~/pm`; this command
refuses those production paths and state outside `/tmp`.

Start with `--dry-run`, which mutates nothing (see the isolation contract
below), then repeat the same arguments without it:

```bash
python3 scripts/rehearse-hosted-migration.py --dry-run \
  --cadence /absolute/path/to/a/reviewed/cadence \
  --source-state /tmp/my-cad529/offline-state \
  --tracker /tmp/my-cad529/tracker-source \
  --out /tmp/my-cad529/rehearsal-1
```

The tool holds the source singleton flock so a daemon cannot start there,
refuses running/submitting/unknown deliveries, runs the existing export and
restore CLI into new isolated directories, verifies the portable manifest,
integrity/schema/counts/row content and snapshots the clean tracker at an exact
commit. It checks source content and tracker identity again for concurrent
changes. It starts no daemon, sends no assignments and makes no network request.
The output directory is private; do not publish the store or tracker clone.
`receipt.json` contains counts and digests, not message contents or credentials.

The source flock **does not freeze standalone tracker or CLI writers**. Use only
an offline copy with every writer excluded. Content comparisons detect ordinary
concurrent changes but are not a production all-writer atomic transaction. If a
check fails, no success receipt is written. Keep the failed output private for
inspection and use a new output directory on the next attempt; the tool never
overwrites or cleans another lane's data.

### Isolation contract

A rehearsal reads repositories and runs the real CLI, so admission must precede
the earliest repository/config reader. The following properties are tested on
owned offline inputs. They do not provide filesystem custody against concurrent
same-UID replacement between a check and a later read.

**Only ordinary self-contained Git layouts are supported.** Filesystem-only
discovery must find a real `.git` directory inside the admitted source/tracker/
output roots before repository Git runs. A central admission check repeats for
every `git -C` invocation and local clone source, including index discovery,
status, snapshot and checkout. It includes the output clone before checkout.
Recorded paths with an existing parent but no contained repository are refused:
downstream Git could otherwise discover an ancestor outside the roots. A missing
parent is skipped only where the export/restore path also skips Git.

Metadata admission rejects inventories exceeding 10000 entries, foreign-owned
entries, symlinks, special files, hardlinked regular files and active hooks.
Gitfiles, linked worktrees/common directories and all object alternates are
unsupported, including quoted alternate syntax, symlinked `info`/`alternates`,
and symlinked object/pack directories or pack files. Prepare an ordinary owned
offline repository separately; the rehearsal never repairs these layouts or
modifies the original to make it acceptable.

Before repository Git may load config, Git parses only key names from the
admitted config file using `config --no-includes --file ... --name-only --list`
in private nonrepository scratch. No config values enter evidence. Includes and
conditional includes are refused. The conservative key allowlist permits normal
core repository metadata, `core.splitIndex`, user name/email, remote url/fetch
and branch remote/merge. Path/command settings such as `core.worktree`,
`core.hooksPath`, `core.attributesFile`, `core.fsmonitor` and unknown keys refuse
admission. Unsupported-layout errors propagate as refusals, never "no repo".

**The repository it reads is never written.** `git status` refreshes cached stat
data and rewrites the index it opens, so pointing an unguarded status check at a
live checkout mutates `.git/index`. Every git read here runs against a throwaway
copy of the index (`GIT_INDEX_FILE`); the tracker's own metadata inventory is
unchanged afterwards. An existing split index is refused before refresh; even a
copied split index references live shared-index files. A full index with
`core.splitIndex=true` remains supported because status explicitly overrides the
actual key with `core.splitIndex=false` and uses `--no-optional-locks`. The tests
check all metadata paths, types, modes, ownership, link counts, nanosecond mtimes,
sizes, regular-file digests and symlink targets, not just the main index.
The test makes the fixture live-shaped — stat data
deliberately stale, so a refresh has something to write — and asserts that the
plain command it replaced *does* change the index.

**Children inherit nothing.** `run()` builds each child's environment from an
allowlist (`PATH`), never from the caller's. A `GIT_DIR`, `GIT_WORK_TREE`,
`GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY`, `CADENCE_STATE_DIR` or
`CADENCE_PM_DIR` exported into the rehearsal cannot reach git or the CLI, and a
variable added to the parent later cannot silently become inherited. `HOME` and
`TMPDIR` point into a private scratch directory, and global/system git config is
read from `/dev/null`. This is not cosmetic: `GIT_DIR` beats `git -C`, and with
the caller's environment the same `cadence export` records the decoy checkout
and its remote instead of the rehearsal's. The test proves both halves.

**No credentialed URL reaches any output.** `cadence export`'s `discover_repos`
and `cadence restore`'s `plan_remap` run `git -C <dir> remote get-url origin`
for every value in `agents.cwd`, `jobs.repo`, `jobs.spec_path`,
`tasks.worktree` and `tasks.spec_path`. The directory is the value itself when
it is one, **otherwise its parent** — so a `spec_path` whose file was deleted
still causes a read of the checkout that held it. An offline copy of a real
store names live developer checkouts, and such a remote may embed
`user:token@`. Two independent guards apply.

First, before the export runs, the rehearsal resolves every path column by that
same rule and refuses any source that reaches a directory outside its own
source, tracker and output roots — so it reads no repository it does not own.
NULL or remap those columns in the offline copy first (for example
`UPDATE agents SET cwd=NULL;` on the copy, never on production). Second, every
remote that does enter evidence is passed through the same `strip_credentials`
rule as `src/backup/mod.rs`, and the manifest, exported row content, receipt and
stdout are scanned for any URL carrying userinfo; one match refuses the run.

**`--dry-run` mutates nothing.** It creates no output directory, writes no
index, adds no file to the source state, and takes the source flock only long
enough to learn whether a daemon holds it before releasing it — an advisory lock
leaves nothing on disk. It still reports the tracker commit, the store
inventory, the recorded checkout paths, and the directories, commands and checks
a real run would use. Every refusal above (foreign owner, active daemon, dirty
tracker, non-`/tmp` path, foreign checkout) fires in dry-run too.

Validation uses small offline SQLite fixtures, including real CLI export/restore
when an explicit existing binary is supplied:

```bash
CADENCE_REHEARSAL_BINARY=/absolute/path/to/cadence \
  python3 scripts/test-hosted-migration.py -v
```

Negative source fixtures prove real Git can reach synthetic outside configuration
or objects before testing refusal, then assert no Git repository command reaches
the refused repository, no output is created and owned metadata remains
unchanged. Python read canaries additionally cover symlinked alternate paths.
These witnesses are distinct from physical syscall tracing and actual CLI
export/restore evidence; synthetic export/restore controls are not real CLI proof.
CI supplies its freshly built release CLI for the three real-CLI cases. A local
run without `CADENCE_REHEARSAL_BINARY` reports those existing cases as skipped.

This proves the bounded offline store/tracker preparation path and the tested
admission contract, not real production cloud storage, lease fencing, auth, queue
acceptance or rollback. The receipt names these unproven gates. No Rust build is
required; an existing binary's provenance must accompany the evidence, and
compilation remains subject to normal build-slot admission or CI.

The metadata walk and later Git/CLI reads are separate operations. A concurrent
same-UID writer can replace paths after admission; no immutable snapshot, syscall-
complete host isolation or atomic all-writer freeze is claimed. Exclude every
writer while preparing and rehearsing the owned offline inputs. Source/metadata
comparisons are evidence of these fixtures, not a cure for that race.

## Single-writer handoff: blocking dependency

CAD-538 owns the hosted lifecycle implementation. CompanyControl grants the
runtime lease before startup. The supported runtime seam is
`POST http://lease.internal/renew`: 204 means the host-assigned instance still
holds the lease; 409 means lost and must fence. There is no container acquire,
release or epoch endpoint. Unknown/failed renewal cannot authorize writes.

The current Cadence HTTP lease provider is a fail-closed stub. AgenticOS's
state bridge currently owns renewal/fencing with Cadence hosted hooks off.
Switch ownership in one reviewed integration change: **never run both heartbeat
owners**. A local `file:` lease is useful for tests on one filesystem but does
not fence a writer on another host. Cloud handoff must prove the actual
CompanyControl/gateway authority, stale-instance rejection and all store/tracker
writer surfaces; this rehearsal does not claim that proof.

Before production transfer, the operator must freeze the daemon, board-mediated
mutations, standalone CLI tracker mutations, sync/push hooks and any external
writer. Merely stopping the daemon or setting a local read-only preference is
insufficient. No automatic production freeze is implemented here. Record an
accountable owner and audited controls for every writer, including old local
credentials, before approving cutover.

## Operator cutover handoff

1. Confirm reviewed image/binary, schema compatibility, org identity, credentials,
   durable storage authority and rollback owner. CAD-539/AOS-49 must establish
   browser/token enrollment, and AOS-62 must establish hosted token validation
   and organization-scoped authorization; cloud durable inbox acceptance remains a separate
   CAD-525 gate. Do not return an accepted receipt for memory-only work.
2. Stop new assignments, finish/reconcile active work and record queue dispositions.
   Confirm the above all-writer freeze. Record the final source store receipt and
   clean tracker commit together; retain a private pre-cutover recovery copy.
3. Import through AgenticOS's supported durable filesystem/storage path while the
   target is not serving writes. Verify store/tracker identity and resolve all
   excluded runtime files and path mappings. This repository does not invent an
   AgenticOS upload endpoint or write directly to CompanyControl storage.
4. With one lease/fence owner, validate readiness, fresh enrollment and stale
   source/instance write rejection. Only then enable cloud writes. Record the
   first cloud write boundary and the desired/observed image version.
5. Reconnect a local team using fresh credentials, reconcile retained queues before
   enabling delivery, and demonstrate result submission during container sleep,
   wake, durable application and deduplication. Heartbeats must not defeat sleep.
6. Keep local production writers disabled. Observe real team operation for 48 hours;
   attach independent evidence per CAD-525 criterion rather than marking the epic
   complete merely because export/restore worked.

## Rollback contract with CAD-530

Before the first cloud write, revoke/stop target authority, prove it cannot write,
and have the rollout owner restore source authority using the verified pre-cutover
state. Never allow both writers while checking which one is healthy.

After the first cloud write, the pre-cutover local snapshot is stale. Freeze cloud
writes and persist/reconcile every accepted external command and applied receipt
before exporting current authoritative state. Check reverse schema compatibility
and restore into an isolated rehearsal first. Revoke cloud authority before
re-enabling the local writer. If accepted inbox commands live outside the store,
the rollback needs their supported transfer/drain procedure as well; store restore
alone is insufficient. If reconciliation or compatibility is unproven, remain
frozen and escalate rather than discard work or replay an uncertain mutation.

The AgenticOS state bridge has measured a final subsecond hard-kill loss window.
Do not claim zero loss from a successful graceful restore or receipt hash.
CAD-530 must independently rehearse the complete pre/post-write rollback boundary
and record which accepted commands survive before cutover is approved.

## Local development after migration

Use a separate temp state, tracker clone, non-production credentials and board
port 3110–3199. Pin local workers to their org/connection; switching an operator's
active org must not reroute running results. Keep the local daemon for dev and
tests with UI hot reload and a documented backend watch/restart loop. Never reuse
production state, port 3010, installed binary or cloud credentials for dev.
Full development-loop evidence is a separate CAD-525 gate, not a consequence of
this store rehearsal.
