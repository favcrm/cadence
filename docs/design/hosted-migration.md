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

```bash
python3 scripts/rehearse-hosted-migration.py \
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

Validation uses small offline SQLite fixtures, including real CLI export/restore
when an explicit existing binary is supplied:

```bash
CADENCE_REHEARSAL_BINARY=/absolute/path/to/cadence \
  python3 scripts/test-hosted-migration.py -v
```

This proves the offline store/tracker preparation path, not real production
cloud storage, lease fencing, auth, queue acceptance or rollback. The receipt
names these unproven gates. No Rust build is required; an existing binary's
provenance must accompany the evidence, and compilation remains subject to
normal build-slot admission or CI.

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
