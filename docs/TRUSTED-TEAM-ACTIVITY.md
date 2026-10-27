# Trusted team activity (CAD-1374)

Status: this document describes the hosted owner activity contract and the
retention rule for open mutation intents. The export and list routes are
not part of the `prune_stream` change in `src/store/messages.rs`.

## Read API

- `GET /api/activity/export` and `GET /api/activity`.
- Scope: hosted owner, fresh session scope only. Other roles and stale
  sessions are refused before any read.
- Filters: `sub` (the acting performer), `action`, `target`, `outcome`,
  `from`, `until`, `limit`, `cursor`.
- `limit` defaults to 100 and is capped at 200.
- Page keys: `schema_version`, `company`, `interval`, `events`,
  `row_count`, `next_cursor`, `complete`, `coverage`.
- Follow `next_cursor` by passing it as the `cursor` query parameter.
  `complete` means only that the captured interval was read to its end.
  It is not a claim about every source or writer.
- `coverage.gaps` is nested under `coverage` and holds
  `missing_record_files`, `absent_receipt_tables` and
  `receipts_without_actor_column`.
- An export's actor is not the filtered actor. The export names the
  performer of each row; `sub` narrows only the list.
- Raw credentials and request bodies are never returned. Receipts carry
  the static action, resource, outcome and reference fields only.
- Unknown legacy rows are shown as unknown. Nothing is backfilled by guess.
- Member comments, wiki prefixes and agent proof are unchanged by this
  contract.

Privacy: a `signed_session_snapshot` (UID, company, session reference,
root role basis) is attribution, not current authority.
Initiator, approver, executing agent and target are distinct fields.
A prepared record is not a completed one, and an uncertain record is not
an external success. No record claims exactly-once delivery.

## Coverage matrix

Each family below uses the activity coverage that exists today.

| Family | Covered actions |
| --- | --- |
| Chat | verified chat, inserted conversation |
| Content | records and CSV revisions |
| Explorer | favorites, default, install request |
| PM issue and wiki | the 11 actions listed below |
| Apps | capability, run approval, create, cancel, dispatch, workspace, consent |
| Session | open, logout, ID all revoke, rotate, stolen, preauth denial |
| Activity | owner denial, export prepared |

PM issue actions: `issue_created`, `issue_updated`, `issue_link_changed`,
`issue_ref_added`, `issue_comment_added`, `issue_artifact_added`.
PM wiki actions: `wiki_written`, `wiki_blob_uploaded`,
`wiki_directory_created`, `wiki_moved`, `wiki_removed`.

AOS membership, invite and email facts stay in the separate D1 store.
They are not merged into the Cadence activity query.

## Declared gaps

This contract does not claim full or operator-approved coverage for:

- session expiry, sweep, and open-link events;
- generic HTTP refusals;
- other grants and local PM pane behavior;
- legacy untagged PM rows and history from before tagging.

## Retention of open mutation intents

Ordinary `events` rows on the daemon stream are kept to the newest 200 per
`wal_pass` (`DAEMON_EVENTS_KEEP` in `src/daemon/watch.rs`). The rolling
`ROLLABLE_EVENT_KINDS` rule in `src/store/events.rs` is separate: it
covers only `submitting` and `submitted` rows, 7 days, rolled in batches
of 5,000 rows.

`Store::prune_stream` keeps an open intent on the daemon stream:

- `app_workspace_mutation_prepared` is kept until a matching
  `app_workspace_mutation_finished` exists;
- `board_session_mutation_prepared` is kept until a matching
  `board_session_mutation_finished` exists;
- `pm_mutation_prepared` is always kept. Its completion is recorded by a
  Git commit trailer, not a SQLite finish row, and this statement cannot
  read Git. The PM history scan is bounded to 16 MiB of reachable HEAD
  output; the bound limits the query and does not delete Git commits.

A match needs the same alias (daemon), a mapped finished kind, schema 1,
a valid finish outcome, and the same `request_id`, `correlation_id`,
`company` (NULL-safe), `action` and `resource`. A row that is malformed or
unmatched stays. A finished row for a different request or company does
not release a prepared row.

Completed histories remain capped at 200. Protected open intents may sit
above the cap. Ordinary noise is capped but is not a forever guarantee.

Application, message and revision owners control their own lifecycle.
Agent removal, uninstall and admin filesystem actions can reduce history.
There is no end-user activity delete API, and this is not a commercial
forever claim.

Approval and verdict streams are governance-reserved and are not touched by
this change.

## Backup

- Supported: `backup::backup` takes an online SQLite snapshot of the core
  and app-records database, including the WAL, while the daemon runs, and
  writes a verified manifest.
- `backup::restore` refuses while the live `cadence.lock` is held. Restore
  has a `force: true` / `--force` option for an existing target, and it
  takes a pre-restore backup first. `force` never bypasses the live-lock
  refusal.
- Core-only restore keeps the original Auth and PM artifacts outside the
  backup. Cookies keep working only because the original Auth file is kept,
  not because the backup recovers it. There is no full Auth or PM recovery
  claim, and this document gives no credential restore recipe.
- Backup retention (default 7) is the scoped directory policy. It is not
  an activity-event TTL.
- Admins with local DB access can change files or tables. The activity
  record is not tamper-proof against them. Restore is an operator action.
