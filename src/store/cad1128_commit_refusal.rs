//! Independently authored CAD-1128 uncertainty acceptance support.
//! Test-only observation/veto on the actual Store connection. The sealed
//! authorizer, guarded writer and production error mapping remain unchanged.

#[cfg(not(all(test, target_os = "linux", feature = "test-seam")))]
compile_error!("CAD-1128 commit probe requires Linux unit tests with test-seam");

use super::Store;
use crate::error::{Error, Result};
use rusqlite::hooks::Action;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Default)]
struct ProbeState {
    armed: AtomicBool,
    target_write_seen: AtomicBool,
    target_updates: AtomicUsize,
    refused_commits: AtomicUsize,
    rollbacks: AtomicUsize,
}

pub(crate) struct SmtpCommitProbe {
    state: Arc<ProbeState>,
}

pub(crate) struct SmtpCommitCounts {
    pub target_updates: usize,
    pub refused_commits: usize,
    pub rollbacks: usize,
    pub armed: bool,
}

impl SmtpCommitProbe {
    pub(crate) fn counts(&self) -> SmtpCommitCounts {
        SmtpCommitCounts {
            target_updates: self.state.target_updates.load(Ordering::SeqCst),
            refused_commits: self.state.refused_commits.load(Ordering::SeqCst),
            rollbacks: self.state.rollbacks.load(Ordering::SeqCst),
            armed: self.state.armed.load(Ordering::SeqCst),
        }
    }
}

impl Store {
    /// Refuse only the next real COMMIT containing an update to this exact
    /// existing SMTP link row. Other writes/read transactions do not consume
    /// the one-shot fault. Hooks never execute SQL or replace the authorizer.
    pub(crate) fn cad1128_refuse_next_smtp_link_commit(
        &self,
        install: &str,
        context: &str,
    ) -> Result<SmtpCommitProbe> {
        let conn = self.conn();
        if !conn.is_autocommit() {
            return Err(Error::internal(
                "SMTP commit probe requires an idle actual core handle",
            ));
        }
        let target: i64 = conn.query_row(
            "SELECT rowid FROM crm_smtp_links WHERE install_id=?1 AND context_id=?2",
            rusqlite::params![install, context],
            |row| row.get(0),
        )?;
        let state = Arc::new(ProbeState::default());
        state.armed.store(true, Ordering::SeqCst);
        let updates = state.clone();
        conn.update_hook(Some(
            move |action: Action, database: &str, table: &str, rowid: i64| {
                if action == Action::SQLITE_UPDATE
                    && database == "main"
                    && table == "crm_smtp_links"
                    && rowid == target
                {
                    updates.target_updates.fetch_add(1, Ordering::SeqCst);
                    updates.target_write_seen.store(true, Ordering::SeqCst);
                }
            },
        ));
        let commits = state.clone();
        conn.commit_hook(Some(move || {
            if commits.target_write_seen.swap(false, Ordering::SeqCst)
                && commits.armed.swap(false, Ordering::SeqCst)
            {
                commits.refused_commits.fetch_add(1, Ordering::SeqCst);
                // SQLite itself refuses and rolls back the real transaction.
                // No synthetic Error or custody flag is supplied by this hook.
                true
            } else {
                false
            }
        }));
        let rollbacks = state.clone();
        conn.rollback_hook(Some(move || {
            rollbacks.target_write_seen.store(false, Ordering::SeqCst);
            rollbacks.rollbacks.fetch_add(1, Ordering::SeqCst);
        }));
        Ok(SmtpCommitProbe { state })
    }

    /// Remove only the probe hooks; the sealed authorizer is untouched.
    /// No pre-existing core commit/update/rollback hook exists in this source.
    pub(crate) fn cad1128_clear_smtp_commit_probe(&self) -> Result<()> {
        let conn = self.conn();
        conn.commit_hook(None::<fn() -> bool>);
        conn.update_hook(None::<fn(Action, &str, &str, i64)>);
        conn.rollback_hook(None::<fn()>);
        if !conn.is_autocommit() {
            return Err(Error::internal(
                "SMTP commit fault did not return core to autocommit",
            ));
        }
        let readable: i64 = conn.query_row("SELECT 1", [], |row| row.get(0))?;
        if readable != 1 {
            return Err(Error::internal(
                "actual core handle is not readable after commit fault",
            ));
        }
        Ok(())
    }
}
