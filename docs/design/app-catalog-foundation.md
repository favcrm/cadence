# CAD-630 catalog foundation boundary

The contract is [workspace app ownership](app-ownership.md). This bounded
foundation identifies installations and preserves their existing storage;
daemon/HTTP install, list and show delivery follows separately. CAD-630 stays
open until those endpoints and their caller proofs pass. CAD-631 owns execution.

## Persistence

The PM root is explicitly workspace `default`, a single-workspace convention,
not tenant authentication. `.apps/catalog.yaml` uses schema 1. Installation
records remain schema 1 and SQLite remains schema 19. Legacy content and records
stay at `<project>/apps/<name>` and `<project>/apps/<name>.yaml`; no hidden
project, content move, run conversion or grant rewrite occurs.

Catalog identities are immutable installation IDs, with exact legacy aliases
and an optional original project link. Same-name legacy copies stay separate.
An app name alone never becomes a workspace-wide fallback. Resolvers verify
the recorded identity and confined storage location rather than trusting an
unvalidated path or UI-selected project. A removed/reinstalled app cannot match
its previous catalog identity. Future workspace storage uses
`.apps/installations/<id>/bundle` plus its adjacent install record.

## Migration and recovery

Mutation requires the existing canonical operator proof and existing PM write
lock. The lock also serializes cooperating app update/approve/remove operations.
It is a kernel lock on `.git/cadence-write.flock` that the kernel releases when
its holder dies; `.write.lock` remains only as a fence for older binaries. A
crash can leave a stale `.write.lock` and, if it died mid-write, interrupted git
state: the next writer refuses until that state is resolved. A `.write.lock`
this build did not write is never removed automatically; the rollout owner
clears it in a quiescent migration.

Build and validate the complete candidate before changing records. Preserve
nonempty IDs; mint a missing ID once. Save bounded original/staged record bytes
and catalog bytes, with SHA256 preimages, in a durable migration journal before
publication. Every record involved in the candidate is compared before writes,
including unchanged records. Publish missing-ID records before atomically
switching the catalog; readers never receive a partially published catalog.
Retain the journal as the explicit backup and recovery artifact.

Resume accepts only original or staged bytes and rechecks the complete source
set before advancing. Divergence leaves the artifact for operator inspection.
Explicit rollback restores only snapshots whose current bytes still match the
staged/original fingerprints; it cannot erase later app updates. Recovery uses
the same proof and lock as migration. Journal paths and staged record identity
transitions are validated; a forged journal cannot write arbitrary files or
alter bindings/team/source as a backfill. Symlink paths are refused.

No approval or app-derived grant is translated, widened, replayed or re-derived.
Legacy approval keys remain project/name. A previously missing ID never matched
an approval; assigning a fresh ID retains that refusal until explicit operator
reapproval. Pending effects and completed runs are untouched. Catalog ownership
does not itself grant access, and this library foundation exposes no new HTTP
or daemon route. The later endpoint slice must enforce actor proof and scoped
reads on both peers, including detached, concurrent and forged callers.

## Evidence required before delivery

Tests-first remote CI must establish a compiled failing regression before its
implementation. Prove separate same-name identity, unchanged records and grants,
duplicate/dangling/forged/symlink refusal, repeatable missing-ID backfill,
interrupted publication resume, divergent resume/rollback refusal, and ordering
against a cooperating PM writer. The configured native project has no recipes;
no local build admission bypass is permitted. Independent review and queue
authorization remain pinned to the final head.
