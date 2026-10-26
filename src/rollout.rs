//! Rollout lease (CAD-268).
//!
//! One active lease, held in the daemon's database, gates `daemon restart`
//! and any schema crossing. Callers inside a cadence pane are
//! `$CADENCE_ALIAS`. Everyone else must pass `--as <identity>`. A path
//! or `HEAD` is never an identity.
//!
//! The lease table is created by schema 12. A database that is still on
//! schema 11 has no lease table, and the binary that wrote it has no
//! `cadence rollout` command, so it cannot record a backup receipt.
//! `authorize_migration` therefore refuses that crossing by default
//! (`no table` = `no lease`) and leaves the file untouched. The single
//! introduction of this feature is the explicit opt-in
//! `CADENCE_ROLLOUT_BOOTSTRAP=1`, which allows only schema 11 → 12 when
//! `rollout_leases` is absent. It does not authorize any later crossing.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::peer::proc_starttime;
use crate::store::{self, Store};

#[path = "rollout_caller.rs"]
mod caller;
#[path = "rollout_gate.rs"]
mod gate;
#[path = "rollout_grants.rs"]
mod grants;
#[path = "rollout_handoff.rs"]
mod handoff;
#[path = "rollout_host.rs"]
mod host;
#[path = "rollout_lease.rs"]
mod lease;
#[path = "rollout_migration.rs"]
mod migration;

use caller::*;
pub use caller::{
    db_file, parse_ttl, resolve_caller, resolve_caller_with, set_forwarded_identity, store_schema,
    unix_now, Caller, MigrationPermit, RestartTicket, SchemaCrossing,
};
use gate::*;
pub use gate::{ingest_gate_log, GateIngest};
use grants::*;
pub use grants::{
    canonical_state_dir, delegate_alias, grant, granted_lease_holder, note_restart_proceeded,
    require_delegate_grant, revoke, staging_delegate, staging_delegations, staging_register,
    staging_revoke, STAGING_OPS,
};
pub use handoff::{handoff, record_backup};
pub use host::require_operator;
use host::*;
pub use lease::{
    claim, reject_registered_alias, release, release_forced, renew, status, ClaimRequest,
};
pub use migration::{
    authorize_daemon_spawn, authorize_direct_run, authorize_migration, authorize_spawn_for,
    begin_restart, enforce_running_build, ensure_lease_tables, recheck_restart, sandbox_exempt,
    upsert_daemon_build,
};

/// Current store schema. v12 introduced `rollout_leases` and
/// `daemon_build`; v13 adds conversation threads (CAD-319); v14 adds
/// `agents.pid_start`, the recorded pid's process start time (CAD-385);
/// v15 adds `messages.priority` (CAD-158); v16 adds
/// `messages.issue`/`messages.worktree`, the dispatch lane a kickoff
/// belongs to (CAD-467); v17 adds the platform custody, grant and
/// project-default tables (CAD-366); v18 adds the pending-effect and
/// draft tables (CAD-506); v19 adds `app_grants.install_id`, the
/// install an app-derived grant belongs to (CAD-577).
/// CAD-753 keeps app record data in per-installation SQLite files;
/// this version tracks core tables only.
/// The newest migration in `store` writes this number.
/// v33 (CAD-1129): `app_favorites`, `app_install_requests` and
/// `app_update_checks` for the apps Explorer — new tables only.
/// v34 (CAD-1246): `events_alias_kind` on `events(alias, kind, seq)` —
/// an index only, so per-alias lookups stop full-scanning `events`.
/// v35 (CAD-1177): `app_tool_claims` and `app_tool_results`, the
/// standalone tool-invocation receipts — new tables only.
/// v36 (CAD-1282): `app_run_failures`, retrofitted for stores that passed the
/// `version < 20` block before CAD-1171 added it — one idempotent table.
/// v37 (CAD-1168): the retained `chat_files` table; v38 adds durable agent
/// and message participant identity; v39 adds topics, subscriptions and the
/// shared transcript; v40 (CAD-1393) adds company document template versions
/// and their drafts. v41 (CAD-1380/AOS-216) adds the `assistants`,
/// `assist_requests` and `job_subscribers` tables; v42 adds member job-request
/// provenance and `assist_stall_checks`; v43 adds `assist_job_shares`; v44
/// (CAD-1412) adds human principals, rooms, membership and read markers; v45
/// (CAD-1365) adds the metadata-only `app_media_*` tables; v46 (CAD-1337)
/// adds authenticated upload ownership and location metadata.
/// v47 (CAD-1423) adds notification preferences, device push consents and
/// private opaque notification intents; v48 (CAD-1420) adds the assistant
/// bootstrap state, template version and revision columns on the `assistants`
/// binding row.
/// v49 (CAD-914) adds resumable wiki transfer jobs and entry progress.
/// v50 (CAD-1482) adds retained image source lineage.
/// v51 (CAD-792) adds `app_run_artifact_fetches`, the successful worker
/// dependency-fetch receipt.
/// v52 (CAD-1548) adds `settings_authority` beside `model_defaults`.
/// v53 (CAD-804) adds `agents.gc_keep`, the explicit keep every agent-gc
/// path honours.
/// v54 (CAD-1239) adds the retired-agent archive marker, lifecycle revision
/// and retention triggers.
/// v55 (CAD-1353) adds `messages.stall_secs`, the budget chosen for one
/// dispatch, leaving `jobs.stall_secs` as the shared default.
/// v56 (CAD-1726) adds `artifacts`, the agent canvas artifact versions.
/// v57 (CAD-1853) adds the cloud-agent connection, its per-thread remote
/// session bindings, the explicit local assistant consent and the durable
/// operation correlation rows.
pub const SCHEMA_VERSION: i64 = 57;

/// Last schema that has no lease table. The bootstrap opt-in covers
/// only this version.
pub const PRE_LEASE_SCHEMA: i64 = 11;

#[cfg(test)]
mod tests {
    use super::caller::{BOOTSTRAP_FORCE, MIGRATION_HOLDER};
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn sibling_writer_rechecks_latch_after_open_before_callback() {
        for closed in [0, 1, 2] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("cadence.sqlite3");
            let conn = connect(&path).unwrap();
            conn.execute_batch("CREATE TABLE evidence(value INTEGER)")
                .unwrap();
            // A different connection closes the check/open gap. This is
            // refusal evidence only, never an external owner permit.
            let closer = Connection::open(&path).unwrap();
            closer
                .execute_batch("CREATE TABLE closure_state(id INTEGER PRIMARY KEY, closed INTEGER)")
                .unwrap();
            closer
                .execute("INSERT INTO closure_state VALUES(1, ?1)", [closed])
                .unwrap();
            let mut called = false;
            let result = immediate(&conn, |tx| {
                called = true;
                tx.execute("INSERT INTO evidence VALUES(1)", [])?;
                Ok(())
            });
            assert!(result.is_err(), "latch {closed} admitted a sibling writer");
            assert!(!called, "callback ran after latch {closed}");
            assert!(conn.is_autocommit());
            assert_eq!(
                conn.query_row("SELECT COUNT(*) FROM evidence", [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }

    #[test]
    fn sibling_writer_panic_rolls_back_before_connection_reuse() {
        let dir = tempfile::tempdir().unwrap();
        let conn = connect(&dir.path().join("cadence.sqlite3")).unwrap();
        conn.execute_batch("CREATE TABLE evidence(value INTEGER)")
            .unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<()> = immediate(&conn, |tx| {
                tx.execute("INSERT INTO evidence VALUES(1)", [])?;
                panic!("sibling writer callback panic");
            });
        }));
        assert!(outcome.is_err());
        assert!(conn.is_autocommit(), "panic left a live write transaction");
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM evidence", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        immediate(&conn, |tx| {
            tx.execute("INSERT INTO evidence VALUES(2)", [])?;
            Ok(())
        })
        .unwrap();
    }

    fn caller(name: &str) -> Caller {
        Caller {
            identity: name.into(),
            source: "as",
        }
    }

    fn fresh(dir: &tempfile::TempDir) -> PathBuf {
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let db = db_file(&state);
        Store::open(&db).unwrap();
        state
    }

    fn claim_as(
        state: &Path,
        name: &str,
        now: f64,
        ttl: Duration,
        takeover: bool,
    ) -> Result<Value> {
        let c = caller(name);
        claim(
            state,
            &ClaimRequest {
                caller: &c,
                reason: "test",
                target: Some("abcdef1"),
                ttl,
                takeover,
                now,
            },
        )
    }

    fn checkpoint(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
        drop(conn);
        let _ = std::fs::remove_file(sidecar(path, "-wal"));
        let _ = std::fs::remove_file(sidecar(path, "-shm"));
    }

    fn downgrade_to_v11(state: &Path) -> PathBuf {
        let db = db_file(state);
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "DROP TABLE IF EXISTS rollout_leases;
             DROP TABLE IF EXISTS daemon_build;
             UPDATE schema_version SET version=11;
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .unwrap();
        drop(conn);
        let _ = std::fs::remove_file(sidecar(&db, "-wal"));
        let _ = std::fs::remove_file(sidecar(&db, "-shm"));
        db
    }

    #[test]
    fn identity_comes_from_alias_or_as_never_from_a_path() {
        let err = resolve_caller_with(None, None).unwrap_err();
        assert!(err.to_string().contains("--as"), "{err}");
        let c = resolve_caller_with(None, Some("operator:ada")).unwrap();
        assert_eq!(c.identity, "operator:ada");
        assert_eq!(c.source, "as");
        let c = resolve_caller_with(Some("pane"), None).unwrap();
        assert_eq!((c.identity.as_str(), c.source), ("pane", "alias"));
        let err = resolve_caller_with(Some("pane"), Some("operator:ada")).unwrap_err();
        assert!(err.to_string().contains("CADENCE_ALIAS"), "{err}");
        let err = resolve_caller_with(None, Some("/tmp/worktree")).unwrap();
        assert_eq!(err.identity, "/tmp/worktree");
    }

    #[test]
    fn second_claim_names_holder_target_and_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let ok = claim_as(&state, "alice", 1_000.0, Duration::from_secs(60), false).unwrap();
        assert_eq!(ok["held"], true);
        assert_eq!(ok["holder"], "alice");
        assert_eq!(ok["target"], "abcdef1");
        let err = claim_as(&state, "bob", 1_010.0, Duration::from_secs(60), false).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("alice"), "{text}");
        assert!(text.contains("abcdef1"), "{text}");
        assert!(text.contains("1060.000"), "{text}");
        let events = events_of(&state);
        assert!(events.iter().any(|e| e.0 == "rollout_claim"));
        assert!(events.iter().any(|e| e.0 == "rollout_claim_refused"));
    }

    #[test]
    fn release_and_handoff_are_holder_only() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        let err = release(&state, &caller("bob")).unwrap_err();
        assert!(err.to_string().contains("only the holder"), "{err}");
        let err = handoff(&state, &caller("bob"), "carol").unwrap_err();
        assert!(err.to_string().contains("only the holder"), "{err}");
        handoff(&state, &caller("alice"), "carol").unwrap();
        let status = status_at(&state);
        assert_eq!(status["holder"], "carol");
        assert_eq!(status["previous_holder"], "alice");
        release(&state, &caller("carol")).unwrap();
        let status = super::status(&state).unwrap();
        assert_eq!(status["held"], false);
        let events = events_of(&state);
        assert!(events.iter().any(|e| e.0 == "rollout_handoff"));
        assert!(events.iter().any(|e| e.0 == "rollout_release"));
    }

    #[test]
    fn renew_extends_only_the_holders_own_live_lease() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        // CAD-482: the claim is an operator action; assert it in-band so
        // this runs identically in a pane and in CI.
        let seam = state.join("seam");
        std::fs::create_dir_all(&seam).unwrap();
        std::fs::write(seam.join("token"), "test-token").unwrap();
        crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
            claim_as(&state, "alice", 1_000.0, Duration::from_secs(60), false).unwrap();
            // Another identity cannot renew it.
            let err = renew(
                &state,
                &caller("bob"),
                "bob's work",
                Some("fffffff"),
                Duration::from_secs(3600),
                1_010.0,
            )
            .unwrap_err();
            assert!(err.to_string().contains("only the holder"), "{err}");
            // The holder's renewal moves the expiry in place: the same
            // row, the same claimed_at, no release and re-claim. The
            // renewing run's reason and target land on the row too, so
            // `rollout status` reports what is actually running
            // (CAD-561 r4).
            let before = status_at(&state);
            let renewed = renew(
                &state,
                &caller("alice"),
                "cadence update",
                Some("1234abc"),
                Duration::from_secs(3600),
                1_010.0,
            )
            .unwrap();
            assert_eq!(renewed["renewed"], true);
            assert_eq!(renewed["expires_at"].as_f64().unwrap(), 1_010.0 + 3600.0);
            let after = status_at(&state);
            assert_eq!(after["claimed_at"], before["claimed_at"]);
            assert_eq!(after["expires_at"].as_f64().unwrap(), 1_010.0 + 3600.0);
            assert_eq!(after["reason"], "cadence update");
            assert_eq!(after["target"], "1234abc");
            // An expired lease is not renewable — the holder takes over
            // or claims afresh.
            let err = renew(
                &state,
                &caller("alice"),
                "cadence update",
                Some("1234abc"),
                Duration::from_secs(60),
                9_999.0,
            )
            .unwrap_err();
            assert!(err.to_string().contains("--takeover"), "{err}");
        });
        let events = events_of(&state);
        assert!(events.iter().any(|e| e.0 == "rollout_renew"));
    }

    #[test]
    fn expired_claim_refusal_still_allows_takeover() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(&state, "alice", 1_000.0, Duration::from_secs(10), false).unwrap();
        let err = claim_as(&state, "bob", 1_020.0, Duration::from_secs(30), false).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("alice"), "{text}");
        assert!(text.contains("--takeover"), "{text}");
        let view = super::status(&state).unwrap();
        assert_eq!(view["expired"], true, "{view}");
        assert_eq!(view["holder"], "alice");
        let err = release(&state, &caller("alice")).unwrap_err();
        assert!(err.to_string().contains("--takeover"), "{err}");
        let ok = claim_as(&state, "bob", 1_020.0, Duration::from_secs(30), true).unwrap();
        assert_eq!(ok["holder"], "bob");
        assert_eq!(ok["previous_holder"], "alice");
        let events = events_of(&state);
        let takeover = events
            .iter()
            .find(|e| e.0 == "rollout_takeover")
            .map(|e| &e.1)
            .unwrap();
        assert!(takeover.contains("alice"), "{takeover}");
        assert!(takeover.contains("previous_holder"), "{takeover}");
    }

    #[test]
    fn parallel_claims_have_one_winner_and_the_loser_names_them() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for name in ["alice", "bob"] {
            let state = state.clone();
            let barrier = Arc::clone(&barrier);
            let name = name.to_string();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                claim_as(&state, &name, 5_000.0, Duration::from_secs(60), false)
            }));
        }
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let wins = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(wins, 1, "{results:?}");
        let winner = results.iter().find_map(|r| r.as_ref().ok()).unwrap()["holder"]
            .as_str()
            .unwrap()
            .to_string();
        let loser = results.iter().find_map(|r| r.as_ref().err()).unwrap();
        assert!(
            loser.to_string().contains(&winner),
            "{loser} winner {winner}"
        );
    }

    #[test]
    fn backup_records_sha_and_schema_for_the_holder_only() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        let db = db_file(&state);
        checkpoint(&db);
        let backup = dir.path().join("backup.sqlite3");
        std::fs::copy(&db, &backup).unwrap();
        let err = record_backup(&state, &caller("bob"), &backup).unwrap_err();
        assert!(err.to_string().contains("alice"), "{err}");
        let bad = dir.path().join("not-sqlite");
        std::fs::write(&bad, b"hello").unwrap();
        let err = record_backup(&state, &caller("alice"), &bad).unwrap_err();
        assert!(err.to_string().contains("not a SQLite"), "{err}");
        let recorded = record_backup(&state, &caller("alice"), &backup).unwrap();
        assert_eq!(recorded["schema_version"], SCHEMA_VERSION);
        assert_eq!(recorded["sha256"].as_str().unwrap().len(), 64);
        let events = events_of(&state);
        assert!(events.iter().any(|e| e.0 == "rollout_backup_recorded"));
    }

    #[test]
    fn lower_schema_without_a_receipt_is_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let db = downgrade_to_v11(&state);
        checkpoint(&db);
        let before = std::fs::read(&db).unwrap();
        let err = authorize_migration(&db).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("cadence rollout claim"), "{text}");
        assert!(text.contains("cadence rollout backup"), "{text}");
        assert!(text.contains("not modified"), "{text}");
        assert!(text.contains("CADENCE_ROLLOUT_BOOTSTRAP"), "{text}");
        assert_eq!(std::fs::read(&db).unwrap(), before);
        assert!(!sidecar(&db, "-wal").exists());
        let log = std::fs::read_to_string(state.join(GATE_LOG)).unwrap();
        assert!(log.contains("rollout_migration_refused"), "{log}");
    }

    #[test]
    fn matching_receipt_allows_the_migration_and_a_fresh_dir_does_not_need_one() {
        let dir = tempfile::tempdir().unwrap();
        assert!(authorize_migration(&db_file(&dir.path().join("missing")))
            .unwrap()
            .crossing
            .is_none());
        let state = fresh(&dir);
        let db = downgrade_to_v11(&state);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        checkpoint(&db);
        let backup = dir.path().join("backup.sqlite3");
        std::fs::copy(&db, &backup).unwrap();
        record_backup(&state, &caller("alice"), &backup).unwrap();
        let _holder = hold_migration("alice");
        let permit = authorize_migration(&db).unwrap();
        assert_eq!(permit.crossing.unwrap().reason, "backup_receipt");
        let store = Store::open(&db).unwrap();
        drop(store);
        let version: i64 = Connection::open(&db)
            .unwrap()
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn bootstrap_allows_only_the_lease_table_introduction() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let db = downgrade_to_v11(&state);
        BOOTSTRAP_FORCE.with(|c| c.set(Some(true)));
        let permit = authorize_migration(&db);
        BOOTSTRAP_FORCE.with(|c| c.set(None));
        assert_eq!(
            permit.unwrap().crossing.unwrap().reason,
            "lease_table_introduction"
        );

        let state = dir.path().join("state-b");
        std::fs::create_dir_all(&state).unwrap();
        Store::open(&db_file(&state)).unwrap();
        let db = downgrade_to_v11(&state);
        claim_as(&state, "alice", unix_now(), Duration::from_secs(60), false).unwrap();
        BOOTSTRAP_FORCE.with(|c| c.set(Some(true)));
        let err = authorize_migration(&db);
        BOOTSTRAP_FORCE.with(|c| c.set(None));
        let err = err.unwrap_err();
        assert!(err.to_string().contains("backup"), "{err}");
    }

    /// Sets an env var for one test and restores it on drop, so a panic
    /// cannot leak it into the tests that run after.
    struct EnvGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    /// CAD-310: a sandbox's own state dir starts a different build and
    /// crosses a schema with no identity and no lease; the same dir
    /// without its marker still refuses both.
    #[test]
    fn a_marked_sandbox_state_dir_needs_no_lease_for_a_new_build_or_schema() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sbx");
        let state = root.join("state");
        std::fs::create_dir_all(&state).unwrap();
        Store::open(&db_file(&state)).unwrap();
        let conn = connect_ensured(&db_file(&state)).unwrap();
        upsert_daemon_build(&conn, "deadbeef", 1.0).unwrap();
        drop(conn);
        let nobody = || Err(Error::rejected("no rollout identity"));
        assert!(authorize_spawn_for(&state, nobody()).is_err());
        // CAD-1187: the root marker alone no longer exempts — the store
        // must hold the dev marker and sit under the sandbox base.
        std::fs::write(root.join(".cadence-sandbox"), r#"{"name":"sbx"}"#).unwrap();
        let _base = EnvGuard::set("CADENCE_SANDBOX_ROOT", dir.path());
        assert!(!sandbox_exempt(&state));
        assert!(authorize_spawn_for(&state, nobody()).is_err());
        let dev = serde_json::json!({
            "v": 1, "name": "sbx", "state_dir": crate::sandbox::resolved(&state),
        });
        std::fs::write(state.join(crate::sandbox::DEV_MARKER), dev.to_string()).unwrap();
        assert!(sandbox_exempt(&state));
        assert!(authorize_spawn_for(&state, nobody()).is_ok());
        let db = downgrade_to_v11(&state);
        assert!(authorize_migration(&db).is_ok());
        std::fs::remove_file(root.join(".cadence-sandbox")).unwrap();
        assert!(authorize_migration(&db).is_err());
    }

    #[test]
    fn same_build_starts_without_a_lease_and_a_different_build_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let conn = connect_ensured(&db_file(&state)).unwrap();
        assert!(authorize_spawn_for(&state, resolve_caller_with(None, None)).is_ok());
        upsert_daemon_build(&conn, crate::overview::BUILD_COMMIT, 1.0).unwrap();
        assert!(authorize_spawn_for(&state, resolve_caller_with(None, None)).is_ok());
        upsert_daemon_build(&conn, "deadbeef", 1.0).unwrap();
        let err = authorize_spawn_for(&state, Ok(caller("operator:ada"))).unwrap_err();
        assert!(err.to_string().contains("deadbeef"), "{err}");
        claim_as(
            &state,
            "operator:ada",
            unix_now(),
            Duration::from_secs(60),
            false,
        )
        .unwrap();
        assert!(authorize_spawn_for(&state, Ok(caller("operator:ada"))).is_ok());
    }

    #[test]
    fn restart_recheck_reports_release_handoff_and_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        let ticket = begin_restart(&state, &caller("alice")).unwrap();
        release(&state, &caller("alice")).unwrap();
        let err = recheck_restart(&state, &ticket).unwrap_err();
        assert!(err.to_string().contains("released"), "{err}");
        assert!(err.to_string().contains("before shutdown"), "{err}");

        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        let ticket = begin_restart(&state, &caller("alice")).unwrap();
        handoff(&state, &caller("alice"), "carol").unwrap();
        let err = recheck_restart(&state, &ticket).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("handed off"), "{text}");
        assert!(text.contains("carol"), "{text}");

        let conn = connect(&db_file(&state)).unwrap();
        conn.execute(
            "UPDATE rollout_leases SET expires_at=1 WHERE status='active'",
            [],
        )
        .unwrap();
        let id: i64 = conn
            .query_row(
                "SELECT id FROM rollout_leases WHERE status='active'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        drop(conn);
        let ticket = RestartTicket {
            lease_id: id,
            holder: "carol".into(),
        };
        let err = recheck_restart(&state, &ticket).unwrap_err();
        assert!(err.to_string().contains("expired"), "{err}");
        let status: String = Connection::open(db_file(&state))
            .unwrap()
            .query_row(
                "SELECT status FROM rollout_leases WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            status, "active",
            "an expired recheck must leave takeover possible"
        );
    }

    #[test]
    fn open_store_reaches_the_current_schema() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.sqlite3");
        Store::open(&db).unwrap();
        let version: i64 = Connection::open(&db)
            .unwrap()
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        // CAD-818: v38 adds durable topic identity and v39 adds topics,
        // part of the reviewed schema the rollout carries. CAD-1393: v40
        // adds the business template tables. CAD-1380/AOS-216: v41 adds
        // the `assistants`, `assist_requests` and `job_subscribers`
        // tables; v42 adds the member-request provenance columns and
        // `assist_stall_checks`; v43 adds the owner-granted
        // `assist_job_shares` entitlement; CAD-1412: v44 adds the human
        // room tables, v45 (CAD-1365) adds the app_media_* tables;
        // CAD-1337 v46 adds authenticated upload ownership/location;
        // CAD-1423 v47 adds notification preferences, devices and intents;
        // and CAD-1420 v48 adds the three `assistants` bootstrap lifecycle
        // columns on the binding row.
        // CAD-914 v49 adds resumable wiki transfer jobs.
    }

    struct MigrationHolder;
    impl Drop for MigrationHolder {
        fn drop(&mut self) {
            MIGRATION_HOLDER.with(|slot| *slot.borrow_mut() = None);
        }
    }
    fn hold_migration(name: &str) -> MigrationHolder {
        MIGRATION_HOLDER.with(|slot| *slot.borrow_mut() = Some(name.to_string()));
        MigrationHolder
    }

    #[test]
    fn gate_log_ingests_only_migration_refusals_and_does_not_replay() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let log = state.join(GATE_LOG);
        let valid = r#"{"kind":"rollout_migration_refused","payload":{"from":11,"to":12,"reason":"no matching backup receipt"},"at":1}"#;
        std::fs::write(
            &log,
            format!(
                "{valid}\n{{\"kind\":\"rollout_release\",\"payload\":{{\"holder\":\"operator:ada\"}},\"at\":1}}\nnot-json\n{{\"kind\":\"rollout_migration_refused\",\"payload\":{{\"from\":\"nope\"}},\"at\":1}}\n"
            ),
        )
        .unwrap();
        let conn = Connection::open(db_file(&state)).unwrap();
        let stats = ingest_gate_log(&state, &conn).unwrap();
        assert_eq!(stats.inserted, 1);
        assert_eq!(stats.skipped, 3);
        assert!(!log.exists());
        let events = events_of(&state);
        let refused: Vec<_> = events
            .iter()
            .filter(|event| event.0 == "rollout_migration_refused")
            .collect();
        assert_eq!(refused.len(), 1, "{events:?}");
        assert!(
            refused[0].1.contains("\"source\":\"gate_log\""),
            "{}",
            refused[0].1
        );
        assert!(events.iter().all(|event| event.0 != "rollout_release"));
        let at: f64 = conn
            .query_row(
                "SELECT at FROM events WHERE kind='rollout_migration_refused'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(at > 1_000_000.0, "line at=1 must not be trusted: {at}");
        let again = ingest_gate_log(&state, &conn).unwrap();
        assert_eq!(again.inserted, 0);

        std::fs::write(&log, format!("{valid}\n{valid}\n")).unwrap();
        let before: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        conn.execute_batch(&format!(
            "CREATE TRIGGER fail_second BEFORE INSERT ON events \
             WHEN (SELECT COUNT(*) FROM events) >= {} \
             BEGIN SELECT RAISE(ABORT, 'boom'); END;",
            before + 1
        ))
        .unwrap();
        assert!(ingest_gate_log(&state, &conn).is_err());
        assert!(!log.exists());
        let mid: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mid, before + 1);
        let replay = ingest_gate_log(&state, &conn).unwrap();
        assert_eq!(replay.inserted, 0);
        let after: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(after, mid);
    }

    #[test]
    fn backup_refuses_the_live_database_a_symlink_and_an_old_copy() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        let db = db_file(&state);
        checkpoint(&db);
        let err = record_backup(&state, &caller("alice"), &db).unwrap_err();
        assert!(err.to_string().contains("live database"), "{err}");
        let link = dir.path().join("link.sqlite3");
        std::os::unix::fs::symlink(&db, &link).unwrap();
        let err = record_backup(&state, &caller("alice"), &link).unwrap_err();
        assert!(err.to_string().contains("live database"), "{err}");
        let dotted = dir.path().join("nested");
        std::fs::create_dir_all(&dotted).unwrap();
        let via_dots = dotted.join("../state/cadence.sqlite3");
        let err = record_backup(&state, &caller("alice"), &via_dots).unwrap_err();
        assert!(err.to_string().contains("live database"), "{err}");
        std::fs::write(sidecar(&db, "-wal"), b"wal-bytes-padding-over-32-bytes!!").unwrap();
        let err = record_backup(&state, &caller("alice"), &sidecar(&db, "-wal")).unwrap_err();
        assert!(err.to_string().contains("live database"), "{err}");
        let old = dir.path().join("old.sqlite3");
        std::fs::copy(&db, &old).unwrap();
        let file = std::fs::File::options().write(true).open(&old).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH).unwrap();
        drop(file);
        let err = record_backup(&state, &caller("alice"), &old).unwrap_err();
        assert!(
            err.to_string().contains("before this lease was claimed"),
            "{err}"
        );
    }

    #[test]
    fn non_holder_cannot_migrate_even_with_a_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let db = downgrade_to_v11(&state);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        checkpoint(&db);
        let backup = dir.path().join("backup.sqlite3");
        std::fs::copy(&db, &backup).unwrap();
        record_backup(&state, &caller("alice"), &backup).unwrap();
        checkpoint(&db);
        let before = std::fs::read(&db).unwrap();
        let _intruder = hold_migration("bob");
        let err = authorize_migration(&db).unwrap_err();
        drop(_intruder);
        assert!(err.to_string().contains("backup"), "{err}");
        assert_eq!(std::fs::read(&db).unwrap(), before);
        assert!(!sidecar(&db, "-wal").exists());
        let version: i64 = open_peek(&db)
            .unwrap()
            .conn
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 11);
    }

    #[test]
    fn peek_sees_a_receipt_that_lives_only_in_the_wal() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let db = downgrade_to_v11(&state);
        checkpoint(&db);
        let main_before = std::fs::read(&db).unwrap();
        let conn = Connection::open(&db).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "wal_autocheckpoint", 0i64)
            .unwrap();
        conn.set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .unwrap();
        ensure_lease_tables(&conn).unwrap();
        let now = unix_now();
        conn.execute(
            "INSERT INTO rollout_leases(
                holder, holder_source, host_note, reason, schema_from, schema_to,
                backup_path, backup_sha256, backup_schema, backup_taken_at,
                claimed_at, expires_at, status)
             VALUES('alice','as','','wal',11,12,'/tmp/b','abc',11,?1,?1,?2,'active')",
            params![now, now + 3600.0],
        )
        .unwrap();
        drop(conn);
        assert_eq!(std::fs::read(&db).unwrap(), main_before);
        assert!(wal_nonempty(&db));
        let _holder = hold_migration("alice");
        let permit = authorize_migration(&db).unwrap();
        assert_eq!(permit.crossing.unwrap().reason, "backup_receipt");
        assert_eq!(std::fs::read(&db).unwrap(), main_before);
    }

    #[test]
    fn force_release_records_the_ousted_holder_and_rejects_agent_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        let err = release_forced(
            &state,
            &Caller {
                identity: "pane".into(),
                source: "alias",
            },
            "holder died",
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("operator"), "{err}");
        Connection::open(db_file(&state))
            .unwrap()
            .execute(
                "INSERT INTO agents(alias, provider, endpoint_kind, role, cwd, sandbox, state, created, updated)
                 VALUES('ada','fake','fake','worker','/','none','stopped',0,0)",
                [],
            )
            .unwrap();
        let err = reject_registered_alias(&state, &caller("ada")).unwrap_err();
        assert!(err.to_string().contains("registered agent alias"), "{err}");
        let err = release(&state, &caller("operator:ada")).unwrap_err();
        assert!(err.to_string().contains("only the holder"), "{err}");
        assert!(
            !err.to_string().contains("release --force"),
            "a live lease must not advertise force-release: {err}"
        );
        let released =
            release_as_plain_operator(&state, "operator:ada", "holder died", Some("alice"))
                .unwrap();
        assert_eq!(released["holder"], "alice");
        assert_eq!(released["forced"], true);
        let events = events_of(&state);
        let release = events
            .iter()
            .find(|event| event.0 == "rollout_release" && event.1.contains("ousted_holder"))
            .map(|event| &event.1)
            .unwrap();
        assert!(release.contains("alice"), "{release}");
        assert!(release.contains("holder died"), "{release}");
        assert!(parse_ttl("8d").unwrap_err().to_string().contains("7 days"));
        assert!(parse_ttl("7d").is_ok());
    }

    #[test]
    fn backup_refuses_a_hard_link_and_a_copy_that_lacks_this_lease_row() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let db = db_file(&state);
        checkpoint(&db);
        let pre_claim = dir.path().join("pre-claim.sqlite3");
        std::fs::copy(&db, &pre_claim).unwrap();
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        checkpoint(&db);
        let link = dir.path().join("hardlink.sqlite3");
        std::fs::hard_link(&db, &link).unwrap();
        let err = record_backup(&state, &caller("alice"), &link).unwrap_err();
        assert!(err.to_string().contains("hard link or bind mount"), "{err}");
        let wal = sidecar(&db, "-wal");
        std::fs::write(&wal, vec![0u8; 64]).unwrap();
        let wal_link = dir.path().join("wal-hardlink");
        std::fs::hard_link(&wal, &wal_link).unwrap();
        let err = record_backup(&state, &caller("alice"), &wal_link).unwrap_err();
        assert!(err.to_string().contains("hard link or bind mount"), "{err}");
        let _ = std::fs::remove_file(&wal);
        let _ = std::fs::remove_file(sidecar(&db, "-shm"));

        let file = std::fs::File::options()
            .write(true)
            .open(&pre_claim)
            .unwrap();
        file.set_modified(SystemTime::now()).unwrap();
        drop(file);
        let err = record_backup(&state, &caller("alice"), &pre_claim).unwrap_err();
        assert!(
            err.to_string().contains("does not contain this lease"),
            "{err}"
        );

        let stripped = dir.path().join("stripped.sqlite3");
        std::fs::copy(&db, &stripped).unwrap();
        {
            let conn = Connection::open(&stripped).unwrap();
            conn.execute("DELETE FROM rollout_leases", []).unwrap();
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .unwrap();
        }
        let err = record_backup(&state, &caller("alice"), &stripped).unwrap_err();
        assert!(
            err.to_string().contains("does not contain this lease"),
            "{err}"
        );

        let foreign = dir.path().join("foreign.sqlite3");
        let conn = Connection::open(&foreign).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_version(version INTEGER NOT NULL);
             INSERT INTO schema_version(version) VALUES(12);",
        )
        .unwrap();
        drop(conn);
        let err = record_backup(&state, &caller("alice"), &foreign).unwrap_err();
        assert!(
            err.to_string().contains("does not contain this lease"),
            "{err}"
        );
    }

    #[test]
    fn refused_start_does_not_rewrite_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let db = db_file(&state);
        {
            let conn = connect_ensured(&db).unwrap();
            upsert_daemon_build(&conn, "deadbeef", 1.0).unwrap();
        }
        checkpoint(&db);
        let before = sqlite_family(&db);
        let err = authorize_spawn_for(&state, Ok(caller("operator:intruder"))).unwrap_err();
        assert!(err.to_string().contains("deadbeef"), "{err}");
        assert_eq!(sqlite_family(&db), before);
        let log = std::fs::read_to_string(state.join(GATE_LOG)).unwrap();
        assert!(log.contains("rollout_start_refused"), "{log}");
        assert!(events_of(&state)
            .iter()
            .all(|event| event.0 != "rollout_start_refused"));
        let conn = Connection::open(&db).unwrap();
        let stats = ingest_gate_log(&state, &conn).unwrap();
        assert_eq!(stats.inserted, 1);
        let refused = events_of(&state)
            .into_iter()
            .find(|event| event.0 == "rollout_start_refused")
            .unwrap();
        assert!(
            refused.1.contains("\"source\":\"gate_log\""),
            "{}",
            refused.1
        );
        assert!(refused.1.contains("deadbeef"), "{}", refused.1);
    }

    #[test]
    fn forwarded_rollout_identity_is_visible_to_other_threads() {
        // A local lock, not the process-wide `FORWARDED_AS`: that one
        // can never be reset, so setting it here would make later
        // `cargo test --lib rollout` cases order-dependent.
        let slot = std::sync::OnceLock::new();
        remember_forwarded(&slot, Some("operator:thread-proof".into()));
        let seen =
            std::thread::scope(|scope| scope.spawn(|| read_forwarded(&slot)).join().unwrap());
        assert_eq!(seen.as_deref(), Some("operator:thread-proof"));
    }

    #[test]
    fn live_force_release_must_name_the_holder_and_the_hint_is_only_for_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        let err = release_forced(&state, &caller("operator:ada"), "oust", None).unwrap_err();
        assert!(err.to_string().contains("--holder"), "{err}");
        assert!(status_at(&state)["held"].as_bool().unwrap(), "{state:?}");
        let err = release_forced(&state, &caller("operator:ada"), "oust", Some("bob")).unwrap_err();
        assert!(err.to_string().contains("does not match"), "{err}");
        assert_eq!(status_at(&state)["holder"], "alice");
        let err = release(&state, &caller("bob")).unwrap_err();
        assert!(
            !err.to_string().contains("release --force"),
            "healthy lease: {err}"
        );
        let err = begin_restart(&state, &caller("bob")).unwrap_err();
        assert!(
            !err.to_string().contains("release --force"),
            "healthy lease restart: {err}"
        );

        let expired = dir.path().join("expired");
        std::fs::create_dir_all(&expired).unwrap();
        Store::open(&db_file(&expired)).unwrap();
        claim_as(&expired, "alice", 1_000.0, Duration::from_secs(10), false).unwrap();
        let err = release(&expired, &caller("alice")).unwrap_err();
        assert!(err.to_string().contains("--takeover"), "{err}");
        assert!(err.to_string().contains("release --force"), "{err}");
        let err = begin_restart(&expired, &caller("bob")).unwrap_err();
        assert!(err.to_string().contains("release --force"), "{err}");
        let released =
            release_as_plain_operator(&expired, "operator:ada", "expired holder", None).unwrap();
        assert_eq!(released["forced"], true);
        assert_eq!(released["holder"], "alice");
    }

    #[test]
    fn enforce_running_build_fails_closed_when_the_record_cannot_be_read() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.sqlite3");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE daemon_build(id INTEGER PRIMARY KEY);")
            .unwrap();
        let err = enforce_running_build(&conn).unwrap_err();
        assert!(
            err.to_string()
                .contains("could not read the recorded build"),
            "{err}"
        );
    }

    fn sqlite_family(db: &Path) -> Vec<(String, Option<Vec<u8>>)> {
        ["", "-wal", "-shm"]
            .into_iter()
            .map(|suffix| {
                let path = if suffix.is_empty() {
                    db.to_path_buf()
                } else {
                    sidecar(db, suffix)
                };
                (suffix.to_string(), std::fs::read(path).ok())
            })
            .collect()
    }

    fn events_of(state: &Path) -> Vec<(String, String)> {
        let conn = Connection::open(db_file(state)).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT kind, payload FROM events WHERE alias='daemon' AND kind LIKE 'rollout_%'",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    fn status_at(state: &Path) -> Value {
        super::status(state).unwrap()
    }

    /// Success-path force-release from a process `operator_proof` accepts:
    /// reparented to init, `CADENCE_ALIAS` removed, stdio off any pane pty.
    fn release_as_plain_operator(
        state: &Path,
        identity: &str,
        reason: &str,
        holder: Option<&str>,
    ) -> Result<Value> {
        run_operator_probe(state, identity, reason, holder, true)
    }

    fn run_operator_probe(
        state: &Path,
        identity: &str,
        reason: &str,
        holder: Option<&str>,
        reparent: bool,
    ) -> Result<Value> {
        if let Some(home) = std::env::var_os("HOME") {
            let live = PathBuf::from(home).join(".local/state/cadence");
            assert!(
                !state.starts_with(&live),
                "refusing to probe the live state dir {}",
                live.display()
            );
        }
        let scratch = tempfile::tempdir().unwrap();
        let req = scratch.path().join("req.json");
        let pid_path = scratch.path().join("pid");
        let out_path = scratch.path().join("out.json");
        let err_path = scratch.path().join("err");
        std::fs::write(
            &req,
            serde_json::json!({
                "state": state,
                "identity": identity,
                "reason": reason,
                "holder": holder,
            })
            .to_string(),
        )
        .unwrap();
        let exe = std::env::current_exe().unwrap();
        let mut cmd = if reparent {
            let mut cmd = std::process::Command::new("setsid");
            cmd.arg("-f")
                .arg("env")
                .arg("-u")
                .arg("CADENCE_ALIAS")
                .arg("-u")
                .arg("CADENCE_STATE_DIR")
                .arg(&exe);
            cmd
        } else {
            let mut cmd = std::process::Command::new("env");
            cmd.arg("-u")
                .arg("CADENCE_ALIAS")
                .arg("-u")
                .arg("CADENCE_STATE_DIR")
                .arg(&exe);
            cmd
        };
        let err_file = std::fs::File::create(&err_path).unwrap();
        cmd.args([
            "--ignored",
            "rollout_operator_shell_probe",
            "--test-threads",
            "1",
        ])
        .env("CADENCE_ROLLOUT_OPERATOR_PROBE", &req)
        .env("CADENCE_ROLLOUT_OPERATOR_PROBE_PID", &pid_path)
        .env("CADENCE_ROLLOUT_OPERATOR_PROBE_OUT", &out_path)
        .env_remove("CADENCE_ALIAS")
        .env_remove("CADENCE_STATE_DIR")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(err_file));
        if reparent {
            let status = cmd.status().unwrap();
            assert!(status.success(), "setsid exited {status}");
            wait_for_operator_probe(&pid_path, &out_path, &err_path)
        } else {
            let status = cmd.status().unwrap();
            let stderr = std::fs::read_to_string(&err_path).unwrap_or_default();
            assert!(status.success(), "probe harness failed: {stderr}");
            let text = std::fs::read_to_string(&out_path)
                .unwrap_or_else(|error| panic!("probe wrote no result ({error}): {stderr}"));
            decode_operator_probe(&text)
        }
    }

    fn wait_for_operator_probe(pid_path: &Path, out_path: &Path, err_path: &Path) -> Result<Value> {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut pid = None;
        loop {
            if pid.is_none() {
                pid = std::fs::read_to_string(pid_path)
                    .ok()
                    .and_then(|text| text.trim().parse::<u32>().ok());
            }
            if out_path.exists() {
                let text = std::fs::read_to_string(out_path).unwrap();
                reap_operator_probe(pid);
                return decode_operator_probe(&text);
            }
            if std::time::Instant::now() > deadline {
                reap_operator_probe(pid);
                let stderr = std::fs::read_to_string(err_path).unwrap_or_default();
                panic!("operator probe timed out: {stderr}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn reap_operator_probe(pid: Option<u32>) {
        let Some(pid) = pid else {
            return;
        };
        if pid <= 1 || pid == std::process::id() {
            return;
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline && unsafe { libc::kill(pid as i32, 0) } == 0 {
            std::thread::sleep(Duration::from_millis(20));
        }
        if unsafe { libc::kill(pid as i32, 0) } == 0 {
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
    }

    fn decode_operator_probe(text: &str) -> Result<Value> {
        let payload: Value = serde_json::from_str(text).unwrap();
        if payload["ok"].as_bool() == Some(true) {
            Ok(payload["value"].clone())
        } else {
            Err(Error::rejected(
                payload["error"]
                    .as_str()
                    .unwrap_or("operator probe failed")
                    .to_string(),
            ))
        }
    }

    fn register_pane(state: &Path, alias: &str, pid: u32) {
        Connection::open(db_file(state))
            .unwrap()
            .execute(
                "INSERT INTO agents(alias, provider, endpoint_kind, role, cwd, sandbox, \
                 state, created, updated, pid, generation) \
                 VALUES(?1,'cursor','pty','worker','/tmp','none','idle',0,0,?2,'g1')",
                rusqlite::params![alias, i64::from(pid)],
            )
            .unwrap();
    }

    fn write_enrolled_root(state: &Path, pid: u32) {
        let starttime = proc_starttime(pid).unwrap();
        let doc = serde_json::json!({
            "format": "cadence-slots",
            "version": 2,
            "enrollments": [{
                "root": {"pid": pid, "starttime": starttime, "uid": unsafe { libc::getuid() }}
            }]
        });
        std::fs::write(state.join("slots.json"), doc.to_string()).unwrap();
    }

    /// Re-exec target for [`run_operator_probe`]. Ignored in a normal
    /// run; the probe sets `CADENCE_ROLLOUT_OPERATOR_PROBE` and passes
    /// `--ignored`.
    #[test]
    #[ignore = "re-exec helper for force-release operator proof"]
    fn rollout_operator_shell_probe() {
        let Ok(req_path) = std::env::var("CADENCE_ROLLOUT_OPERATOR_PROBE") else {
            return;
        };
        let pid_path = std::env::var("CADENCE_ROLLOUT_OPERATOR_PROBE_PID").unwrap();
        let out_path = std::env::var("CADENCE_ROLLOUT_OPERATOR_PROBE_OUT").unwrap();
        std::fs::write(&pid_path, std::process::id().to_string()).unwrap();
        let req: Value = serde_json::from_str(&std::fs::read_to_string(req_path).unwrap()).unwrap();
        let state = PathBuf::from(req["state"].as_str().unwrap());
        let released = release_forced(
            &state,
            &caller(req["identity"].as_str().unwrap()),
            req["reason"].as_str().unwrap(),
            req["holder"].as_str(),
        );
        let payload = match released {
            Ok(value) => serde_json::json!({"ok": true, "value": value}),
            Err(error) => serde_json::json!({"ok": false, "error": error.to_string()}),
        };
        // The reparented parent polls for `out.json` and reads it the
        // moment it exists; `fs::write` creates the file before it
        // writes, so publish by rename — the parent sees all or nothing
        // (CAD-421).
        let partial = format!("{out_path}.partial");
        std::fs::write(&partial, payload.to_string()).unwrap();
        std::fs::rename(&partial, out_path).unwrap();
    }

    #[test]
    fn daemon_pid_for_proof_is_zero_when_stopped_and_the_lock_holder_when_running() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        std::fs::create_dir_all(state).unwrap();
        assert_eq!(daemon_pid_for_proof(state).unwrap(), 0);
        let chain = crate::adapter::pty::caller_chain(std::process::id()).unwrap();
        assert!(
            !chain.contains(&0),
            "sentinel 0 is on the ancestry: {chain:?}"
        );
        let path = state.join("cadence.lock");
        std::fs::write(&path, "").unwrap();
        // Released with `flock`, not by close: a sibling test's fork
        // shares the descriptor until its child execs, and a closed
        // holder would still read as the daemon (CAD-389).
        let held = crate::worktree::TestFileLock::acquire(&path);
        assert_eq!(daemon_pid_for_proof(state).unwrap(), std::process::id());
        held.release();
        assert_eq!(daemon_pid_for_proof(state).unwrap(), 0);
    }

    #[test]
    fn flock_holder_rereads_proc_locks_that_skipped_a_live_holder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cadence.lock");
        std::fs::write(&path, "").unwrap();
        let held = crate::worktree::TestFileLock::acquire(&path);
        // First read stands in for a page boundary that lost the line.
        let mut reads = 0;
        let holder = flock_holder_from(&path, || {
            reads += 1;
            if reads == 1 {
                Ok(String::new())
            } else {
                std::fs::read_to_string("/proc/locks")
            }
        })
        .unwrap();
        assert_eq!(holder, Some(std::process::id()));
        // A real read under lock churn can skip the line too.
        assert!(reads >= 2, "{reads}");

        let mut reads = 0;
        let err = flock_holder_from(&path, || {
            reads += 1;
            Ok(String::new())
        })
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("is locked but its holder is not in /proc/locks"),
            "{err}"
        );
        assert_eq!(reads, PROC_LOCKS_READS);
        held.release();
    }

    #[test]
    fn force_release_refuses_a_caller_whose_ancestry_includes_a_pane() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        register_pane(&state, "pane-a", std::process::id());
        let err = run_operator_probe(&state, "operator:ada", "from a pane", Some("alice"), false)
            .unwrap_err();
        assert!(
            err.to_string().contains("on its ancestry") && err.to_string().contains("pane-a"),
            "{err}"
        );
        assert!(status_at(&state)["held"].as_bool().unwrap(), "{err}");
    }

    /// CAD-385: the operator-proof pane deny list checks each row's
    /// recorded start time. A row whose pid now names a different
    /// process drops out (as if unregistered); a row with no recorded
    /// start stays (fail closed) — and on a store older than v14, which
    /// has no `pid_start` column, every row is such a row.
    #[test]
    fn registered_panes_drop_a_reused_pid_and_keep_a_row_without_start() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let me = std::process::id();
        let parent = std::os::unix::process::parent_id();
        let mut other = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let conn = Connection::open(db_file(&state)).unwrap();
        for (alias, pid, start) in [
            ("live", me, proc_starttime(me).map(|t| t as i64)),
            (
                "reused",
                parent,
                proc_starttime(parent).map(|t| t as i64 - 1),
            ),
            ("legacy", other.id(), None),
        ] {
            conn.execute(
                "INSERT INTO agents(alias, provider, endpoint_kind, role, cwd, sandbox, \
                 state, created, updated, pid, pid_start, generation) \
                 VALUES(?1,'cursor','pty','worker','/tmp','none','idle',0,0,?2,?3,'g1')",
                rusqlite::params![alias, i64::from(pid), start],
            )
            .unwrap();
        }
        assert_eq!(
            registered_panes(&state).unwrap(),
            HashMap::from([(me, "live".to_string()), (other.id(), "legacy".to_string())])
        );
        conn.execute_batch("ALTER TABLE agents DROP COLUMN pid_start")
            .unwrap();
        drop(conn);
        assert_eq!(registered_panes(&state).unwrap().len(), 3);
        let _ = other.kill();
        let _ = other.wait();
    }

    #[test]
    fn force_release_refuses_an_enrolled_root_on_the_ancestry() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        write_enrolled_root(&state, std::process::id());
        let err = run_operator_probe(
            &state,
            "operator:ada",
            "from an endpoint",
            Some("alice"),
            false,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("enrolled managed endpoint"),
            "{err}"
        );
        assert!(status_at(&state)["held"].as_bool().unwrap(), "{err}");
    }

    #[test]
    fn force_release_accepts_a_plain_operator_when_the_daemon_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        assert_eq!(daemon_pid_for_proof(&state).unwrap(), 0);
        let released =
            release_as_plain_operator(&state, "operator:ada", "daemon stopped", Some("alice"))
                .unwrap();
        assert_eq!(released["forced"], true);
        assert_eq!(released["holder"], "alice");
        assert!(!status_at(&state)["held"].as_bool().unwrap());
    }

    #[test]
    fn force_release_accepts_a_plain_operator_when_the_daemon_is_running() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        let path = state.join("cadence.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        use std::os::unix::io::AsRawFd;
        assert_eq!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert_eq!(daemon_pid_for_proof(&state).unwrap(), std::process::id());
        let released =
            release_as_plain_operator(&state, "operator:ada", "daemon running", Some("alice"))
                .unwrap();
        drop(file);
        assert_eq!(released["forced"], true);
        assert_eq!(released["holder"], "alice");
        assert!(!status_at(&state)["held"].as_bool().unwrap());
    }

    // ---- CAD-1024 staging delegation: grant store + allowlist (PR-2) ----
    // These admit nothing yet — PR-3 adds the `delegate:` caller shape. Each
    // is written to fail without its guard; the mutation proofs are in the
    // PR body.

    fn staging_dir(dir: &tempfile::TempDir) -> PathBuf {
        let state = dir.path().join("staging-state");
        std::fs::create_dir_all(&state).unwrap();
        Store::open(&db_file(&state)).unwrap();
        state
    }

    fn ops(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// I2/I5: `staging delegate` refuses a state dir that was never
    /// registered — there is no grant without the allowlist row.
    #[test]
    fn delegate_refuses_unregistered_dir() {
        let dir = tempfile::tempdir().unwrap();
        let state = staging_dir(&dir);
        let err = staging_delegate(
            &state,
            "w1",
            &ops(&["rollout_claim"]),
            Duration::from_secs(3600),
            "operator",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("not a registered staging"),
            "{err}"
        );
    }

    /// I5: a staging dir overlapping the production state dir
    /// (`$HOME/.local/state/cadence` when HOME is the temp root) or `~/pm` can
    /// never be registered, so no grant can ever be created for it. Runs with
    /// HOME pointed at a temp root so the production dir resolves inside it.
    #[test]
    fn staging_register_refuses_production_overlap() {
        let dir = tempfile::tempdir().unwrap();
        // Resolve the production dirs the way `refuse_production_staging`
        // does: HOME-scoped `local_state_dir` and `home_default_dir` land
        // under the temp HOME the harness exports, so a nested dir overlaps.
        // Drive the guard directly for the deterministic assertion.
        let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
        let prod_state = crate::sandbox::resolved(&home.join(".local/state/cadence"));
        // A dir nested inside the production state dir.
        let nested = prod_state.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let err = refuse_production_staging(&nested).unwrap_err();
        assert!(
            err.to_string().contains("overlaps a production path"),
            "{err}"
        );
        // And the production dir itself.
        std::fs::create_dir_all(&prod_state).unwrap();
        let err = refuse_production_staging(&prod_state).unwrap_err();
        assert!(
            err.to_string().contains("overlaps a production path"),
            "{err}"
        );
        // A sibling outside production registers fine (proved in the
        // lifecycle test); here just assert the guard does not refuse it.
        let ok = dir.path().join("staging-state");
        std::fs::create_dir_all(&ok).unwrap();
        refuse_production_staging(&ok).unwrap();
    }

    /// I2: a board port outside 3110–3199 refuses registration — 3010 is the
    /// production board and can never be staging.
    #[test]
    fn staging_register_refuses_production_port() {
        let dir = tempfile::tempdir().unwrap();
        let state = staging_dir(&dir);
        for port in [3010u16, 3109, 3200] {
            let err = staging_register(&state, port, "operator").unwrap_err();
            assert!(err.to_string().contains("3110"), "{port}: {err}");
        }
        staging_register(&state, 3150, "operator").unwrap();
    }

    /// I3/I6/I7: a grant binds `delegate:<alias>`'s ops + this state dir, is
    /// superseded by a fresh grant, expires on `expires_at`, revokes, and
    /// every transition lands an audit event.
    #[test]
    fn delegate_grant_lifecycle_and_audit() {
        let dir = tempfile::tempdir().unwrap();
        let state = staging_dir(&dir);
        staging_register(&state, 3150, "operator").unwrap();

        let ok = staging_delegate(
            &state,
            "w1",
            &ops(&["rollout_claim", "daemon_start"]),
            Duration::from_secs(3600),
            "operator",
        )
        .unwrap();
        assert_eq!(ok["granted"], "w1");
        assert_eq!(ok["ops"][0], "rollout_claim");
        assert!(ok["expires_at"].as_f64().unwrap() > unix_now());

        // A non-allowlisted op refuses before any write.
        let err = staging_delegate(
            &state,
            "w1",
            &ops(&["ui_tailscale"]),
            Duration::from_secs(3600),
            "operator",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("not in the v1 staging allowlist"),
            "{err}"
        );

        // `staging delegations` lists the live grant, not a revoked one.
        let listed = staging_delegations(&state).unwrap();
        assert_eq!(listed["delegations"].as_array().unwrap().len(), 1);
        assert_eq!(listed["delegations"][0]["alias"], "w1");

        // Revoke ends it; a second use (listed) is gone.
        staging_revoke(&state, "w1", "operator").unwrap();
        let listed = staging_delegations(&state).unwrap();
        assert_eq!(listed["delegations"].as_array().unwrap().len(), 0);
        let err = staging_revoke(&state, "w1", "operator").unwrap_err();
        assert!(err.to_string().contains("no live staging grant"), "{err}");

        // Audit: register + delegate + revoke events on the daemon stream.
        let conn = Connection::open(db_file(&state)).unwrap();
        let mut st = conn
            .prepare("SELECT kind FROM events WHERE alias='daemon' AND kind LIKE 'staging%'")
            .unwrap();
        let kinds: Vec<String> = st
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        for want in ["staging_register", "staging_delegate", "staging_revoke"] {
            assert!(kinds.iter().any(|k| k == want), "{want} missing: {kinds:?}");
        }
    }

    /// I3: a grant's `expires_at` in the past refuses listing (expired is not
    /// live) — and a delegate with a 0s/oversized ttl is refused up front.
    #[test]
    fn delegate_ttl_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let state = staging_dir(&dir);
        staging_register(&state, 3150, "operator").unwrap();
        for ttl in [Duration::ZERO, Duration::from_secs(8 * 24 * 3600)] {
            let err = staging_delegate(&state, "w1", &ops(&["rollout_claim"]), ttl, "operator")
                .unwrap_err();
            assert!(err.to_string().contains("ttl"), "{err}");
        }
    }

    /// I3: a grant on dir A never applies to dir B — the grant row records
    /// A's canonical dir, and `staging_delegations(B)` reads only B's store.
    #[test]
    fn delegate_use_binds_state_dir() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        for s in [&a, &b] {
            std::fs::create_dir_all(s).unwrap();
            Store::open(&db_file(s)).unwrap();
            staging_register(s, 3150, "operator").unwrap();
        }
        staging_delegate(
            &a,
            "w1",
            &ops(&["rollout_claim"]),
            Duration::from_secs(3600),
            "operator",
        )
        .unwrap();
        // The grant lives in A's store only; B's delegations list is empty.
        assert_eq!(
            staging_delegations(&b).unwrap()["delegations"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            staging_delegations(&a).unwrap()["delegations"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    // ---- CAD-1024 PR-3: the delegate caller shape ----

    /// The delegate caller for `alias`: `source:"delegate"`, literal
    /// `delegate:<alias>` identity — the lease holder.
    fn delegate(alias: &str) -> Caller {
        Caller {
            identity: format!("delegate:{alias}"),
            source: "delegate",
        }
    }

    /// Register `alias` in `state`'s agents table with the test process's pid
    /// as its live endpoint — the I4 ancestry derivation then derives this
    /// process to `alias`.
    fn register_self_as(state: &Path, alias: &str) {
        let conn = Connection::open(db_file(state)).unwrap();
        let pid = std::process::id() as i64;
        let start = crate::peer::proc_starttime(std::process::id()).unwrap() as i64;
        conn.execute(
            "INSERT INTO agents(alias,provider,endpoint_kind,role,cwd,sandbox,state,created,updated,pid,pid_start)
             VALUES(?1,'fake','pty','worker','/','none','stopped',0,0,?2,?3)",
            params![alias, pid, start],
        )
        .unwrap();
    }

    /// `resolve_caller` parses `delegate:<alias>` to `source:"delegate"` and
    /// the literal `delegate:<alias>` identity; `delegate:` alone and
    /// `delegate:operator:*` refuse.
    #[test]
    fn delegate_caller_shape() {
        let c = resolve_caller_with(Some("w1"), Some("delegate:w1")).unwrap();
        assert_eq!((c.identity.as_str(), c.source), ("delegate:w1", "delegate"));
        assert_eq!(delegate_alias(&c), Some("w1"));
        // env alias must match — a mismatch is the existing refusal.
        let err = resolve_caller_with(Some("w1"), Some("delegate:w2")).unwrap_err();
        assert!(err.to_string().contains("CADENCE_ALIAS"), "{err}");
        // `delegate:` never names operator.
        let err = resolve_caller_with(None, Some("delegate:operator:x")).unwrap_err();
        assert!(err.to_string().contains("never operator"), "{err}");
        let err = resolve_caller_with(None, Some("delegate:")).unwrap_err();
        assert!(err.to_string().contains("delegate"), "{err}");
    }

    /// I3/I4: a delegate with a live grant for `op` is admitted; the same
    /// delegate is refused on an op outside the grant, on another state dir,
    /// and with a forged/mismatched env alias.
    #[test]
    fn require_delegate_grant_admits_and_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let state = staging_dir(&dir);
        staging_register(&state, 3150, "operator").unwrap();
        register_self_as(&state, "w1");
        staging_delegate(
            &state,
            "w1",
            &ops(&["rollout_claim", "daemon_start"]),
            Duration::from_secs(3600),
            "operator",
        )
        .unwrap();
        let pid = std::process::id();
        // Admitted on a granted op.
        assert_eq!(
            require_delegate_grant_inner(&state, &delegate("w1"), "rollout_claim", Some("w1"), pid)
                .unwrap(),
            "w1"
        );
        // An op outside the grant refuses.
        let err = require_delegate_grant_inner(&state, &delegate("w1"), "ui_stop", Some("w1"), pid)
            .unwrap_err();
        assert!(err.to_string().contains("outside its grant"), "{err}");
        // A forged env alias refuses.
        let err =
            require_delegate_grant_inner(&state, &delegate("w1"), "rollout_claim", Some("w2"), pid)
                .unwrap_err();
        assert!(err.to_string().contains("CADENCE_ALIAS"), "{err}");
        // No env at all refuses.
        let err = require_delegate_grant_inner(&state, &delegate("w1"), "rollout_claim", None, pid)
            .unwrap_err();
        assert!(err.to_string().contains("no CADENCE_ALIAS"), "{err}");
        // A different delegate with no grant refuses.
        register_self_as(&state, "w2");
        let err =
            require_delegate_grant_inner(&state, &delegate("w2"), "rollout_claim", Some("w2"), pid)
                .unwrap_err();
        assert!(err.to_string().contains("no live grant"), "{err}");
    }

    /// I4: `alias` registered only in dir B never proves the shell's identity
    /// for dir A — the endpoint must be in *this* state dir's agents table.
    #[test]
    fn delegate_cross_statedir_alias_refused() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        for s in [&a, &b] {
            std::fs::create_dir_all(s).unwrap();
            Store::open(&db_file(s)).unwrap();
            staging_register(s, 3150, "operator").unwrap();
        }
        // `w1` registered only in B; a grant for `w1` on A still refuses
        // because the ancestry endpoint is not A's.
        register_self_as(&b, "w1");
        staging_delegate(
            &a,
            "w1",
            &ops(&["rollout_claim"]),
            Duration::from_secs(3600),
            "operator",
        )
        .unwrap();
        let err = require_delegate_grant_inner(
            &a,
            &delegate("w1"),
            "rollout_claim",
            Some("w1"),
            std::process::id(),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("not registered in this state dir"),
            "{err}"
        );
    }

    /// I3: an expired grant refuses; a revoked grant refuses a second call.
    #[test]
    fn delegate_expired_and_revoked_refuse() {
        let dir = tempfile::tempdir().unwrap();
        let state = staging_dir(&dir);
        staging_register(&state, 3150, "operator").unwrap();
        register_self_as(&state, "w1");
        let pid = std::process::id();
        staging_delegate(
            &state,
            "w1",
            &ops(&["rollout_claim"]),
            Duration::from_secs(1),
            "operator",
        )
        .unwrap();
        // Force expiry by backdating the row.
        let conn = Connection::open(db_file(&state)).unwrap();
        conn.execute("UPDATE staging_grants SET expires_at=1", [])
            .unwrap();
        drop(conn);
        let err =
            require_delegate_grant_inner(&state, &delegate("w1"), "rollout_claim", Some("w1"), pid)
                .unwrap_err();
        assert!(err.to_string().contains("no live grant"), "{err}");
        // A fresh grant, then revoke: the second call refuses.
        staging_delegate(
            &state,
            "w1",
            &ops(&["rollout_claim"]),
            Duration::from_secs(3600),
            "operator",
        )
        .unwrap();
        staging_revoke(&state, "w1", "operator").unwrap();
        let err =
            require_delegate_grant_inner(&state, &delegate("w1"), "rollout_claim", Some("w1"), pid)
                .unwrap_err();
        assert!(err.to_string().contains("no live grant"), "{err}");
    }

    /// I3/I6: `claim` runs the delegate gate for a `delegate:` caller — here
    /// the env binding refuses (no CADENCE_ALIAS in the test process), proving
    /// `claim` reaches `require_delegate_grant` and never operator_proof.
    #[test]
    fn delegate_claim_runs_the_grant_gate() {
        let dir = tempfile::tempdir().unwrap();
        let state = staging_dir(&dir);
        staging_register(&state, 3150, "operator").unwrap();
        register_self_as(&state, "w1");
        staging_delegate(
            &state,
            "w1",
            &ops(&["rollout_claim"]),
            Duration::from_secs(3600),
            "operator",
        )
        .unwrap();
        let err = claim(
            &state,
            &ClaimRequest {
                caller: &delegate("w1"),
                reason: "stage",
                target: Some("abc1234"),
                ttl: Duration::from_secs(3600),
                takeover: false,
                now: unix_now(),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("CADENCE_ALIAS"), "{err}");
    }
}
