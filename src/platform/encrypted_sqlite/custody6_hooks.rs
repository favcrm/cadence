// Independent test owner: cad1085-security. Test-only sole-manager observations.
// Mount as pub(crate) mod custody6_hooks in encrypted_sqlite.rs, cfg below.
#[cfg(not(all(test, target_os = "linux", feature = "test-seam")))]
compile_error!("custody6 hooks must never enter ordinary/release artifacts");

use super::{EncryptedVault, Error, Result};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

#[derive(Debug)]
pub(crate) struct Policy {
    pub journal: String,
    pub synchronous: i64,
    pub temp_store: i64,
    pub trusted_schema: i64,
    pub busy_timeout: i64,
    pub autocommit: bool,
    pub readonly: bool,
    pub physical: bool,
    pub has_key: bool,
}
#[derive(Default)]
struct Counts {
    begin: AtomicU64,
    insert: AtomicU64,
    commit: AtomicU64,
    rollback: AtomicU64,
    all: AtomicU64,
}
#[derive(Clone)]
pub(crate) struct SqlProbe(Arc<Counts>);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SqlCounts {
    pub begin: u64,
    pub insert: u64,
    pub commit: u64,
    pub rollback: u64,
    pub all: u64,
}
impl SqlProbe {
    pub(crate) fn counts(&self) -> SqlCounts {
        SqlCounts {
            begin: self.0.begin.load(Ordering::SeqCst),
            insert: self.0.insert.load(Ordering::SeqCst),
            commit: self.0.commit.load(Ordering::SeqCst),
            rollback: self.0.rollback.load(Ordering::SeqCst),
            all: self.0.all.load(Ordering::SeqCst),
        }
    }
}
impl EncryptedVault {
    // Queries the actual existing connection. Never opens/clones a vault reader.
    pub(crate) fn custody6_policy(&self) -> Result<Policy> {
        let i = self
            .inner
            .lock()
            .map_err(|_| Error::internal("custody6 poisoned"))?;
        let c = &i.conn;
        Ok(Policy {
            journal: c.query_row("PRAGMA journal_mode", [], |r| r.get(0))?,
            synchronous: c.query_row("PRAGMA synchronous", [], |r| r.get(0))?,
            temp_store: c.query_row("PRAGMA temp_store", [], |r| r.get(0))?,
            trusted_schema: c.query_row("PRAGMA trusted_schema", [], |r| r.get(0))?,
            busy_timeout: c.query_row("PRAGMA busy_timeout", [], |r| r.get(0))?,
            autocommit: c.is_autocommit(),
            readonly: c.is_readonly(rusqlite::DatabaseName::Main)?,
            physical: i.files.is_some(),
            has_key: i.key.is_some(),
        })
    }
    pub(crate) fn custody6_probe(&self, deny_rollback: bool) -> Result<SqlProbe> {
        let i = self
            .inner
            .lock()
            .map_err(|_| Error::internal("custody6 poisoned"))?;
        if i.fenced || i.key.is_none() || i.files.is_none() || !i.conn.is_autocommit() {
            return Err(Error::internal(
                "custody6 needs a real unlocked idle connection",
            ));
        }
        // This mapping is pinned to inspected rusqlite 0.32 + bundled SQLite
        // 3.46.0: SQLite authorizes COMMIT by name; rusqlite maps that name to
        // TransactionOperation::Unknown. Reject a changed engine, never infer
        // a successful COMMIT from the callback (it observes preparation only).
        if rusqlite::version() != "3.46.0" {
            return Err(Error::internal(
                "custody6 SQL phase mapping needs owner revision",
            ));
        }
        i.conn.set_prepared_statement_cache_capacity(0);
        let counts = Arc::new(Counts::default());
        let seen = counts.clone();
        i.conn.authorizer(Some(move |ctx: AuthContext<'_>| {
            seen.all.fetch_add(1, Ordering::SeqCst);
            match ctx.action {
                AuthAction::Transaction {
                    operation: TransactionOperation::Begin,
                } => {
                    seen.begin.fetch_add(1, Ordering::SeqCst);
                }
                AuthAction::Transaction {
                    operation: TransactionOperation::Unknown,
                } => {
                    seen.commit.fetch_add(1, Ordering::SeqCst);
                }
                AuthAction::Transaction {
                    operation: TransactionOperation::Rollback,
                } => {
                    seen.rollback.fetch_add(1, Ordering::SeqCst);
                    if deny_rollback {
                        // A real SQLite SQLITE_AUTH error on real ROLLBACK.
                        // No synthetic Error, manager flag, key or row changed.
                        return Authorization::Deny;
                    }
                }
                AuthAction::Insert {
                    table_name: "records",
                } => {
                    seen.insert.fetch_add(1, Ordering::SeqCst);
                }
                _ => {}
            }
            Authorization::Allow
        }));
        Ok(SqlProbe(counts))
    }
    pub(crate) fn custody6_clear_probe(&self) -> Result<()> {
        let i = self
            .inner
            .lock()
            .map_err(|_| Error::internal("custody6 poisoned"))?;
        i.conn
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
        Ok(())
    }
    // Deliberate real policy violation, distinct from a fake refusal injection.
    pub(crate) fn custody6_downgrade_sync(&self) -> Result<()> {
        let i = self
            .inner
            .lock()
            .map_err(|_| Error::internal("custody6 poisoned"))?;
        if i.key.is_none() || i.files.is_none() || !i.conn.is_autocommit() {
            return Err(Error::internal(
                "custody6 downgrade needs real idle connection",
            ));
        }
        i.conn.execute_batch("PRAGMA synchronous=OFF")?;
        Ok(())
    }
}
