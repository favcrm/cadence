//! CAD-2215: the one nullable workspace bind for the operator-provisioned
//! scoped-cursor keyring.
//!
//! `connection_metadata.cursor_keyring_fingerprint` is the only persisted
//! keyring authority. `NULL` means "never explicitly initialized"; the
//! migration creates the column and never writes a value. The explicit
//! operator RPC sets it exactly once, or confirms the identical value
//! idempotently. A different workspace, material or fingerprint refuses and
//! is never silently rewritten, and two concurrent initializations are
//! serialized by the one fenced writer lane, so conflicting material cannot
//! both succeed.

use super::{Store, StoreConn};
use crate::error::{Error, Result};

/// The column that records the admitted keyring fingerprint.
pub(crate) const COLUMN: &str = "cursor_keyring_fingerprint";

fn unavailable() -> Error {
    Error::invalid(
        "scoped_cursor_unavailable",
        "scoped cursor keyring unavailable",
    )
}

/// What one explicit initialization call did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CursorKeyringInit {
    /// The NULL fingerprint was bound for the first time.
    Initialized,
    /// The stored fingerprint already equalled the supplied one.
    Confirmed,
}

impl Store {
    /// The workspace this store's cursors are namespaced to and the admitted
    /// keyring fingerprint, if one was ever explicitly bound.
    pub(crate) fn cursor_keyring_metadata(&self) -> Result<(String, Option<String>)> {
        self.read_tx(|tx| {
            tx.query_opt(
                "SELECT workspace_id, cursor_keyring_fingerprint \
                   FROM connection_metadata WHERE singleton=1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )?
            .ok_or_else(unavailable)
        })
    }

    /// Bind `fingerprint` to `workspace` exactly once, or confirm the same
    /// value. Runs in the fenced producer transaction: the read and the
    /// conditional update are one writer critical section, so a concurrent
    /// different material cannot observe NULL after another writer bound it.
    /// `workspace` is the one the codec was built for; a store whose
    /// workspace changed underneath (a portable import) refuses.
    pub(crate) fn initialize_cursor_keyring(
        &self,
        workspace: &str,
        fingerprint: &str,
    ) -> Result<CursorKeyringInit> {
        if workspace.is_empty() || fingerprint.is_empty() {
            return Err(unavailable());
        }
        self.write_tx(|tx| {
            let (current_workspace, current) = tx
                .query_opt(
                    "SELECT workspace_id, cursor_keyring_fingerprint \
                       FROM connection_metadata WHERE singleton=1",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                )?
                .ok_or_else(unavailable)?;
            if current_workspace != workspace {
                return Err(unavailable());
            }
            match current {
                None => {
                    let bound = tx.execute(
                        "UPDATE connection_metadata SET cursor_keyring_fingerprint=?1 \
                          WHERE singleton=1 AND cursor_keyring_fingerprint IS NULL",
                        [fingerprint],
                    )?;
                    if bound != 1 {
                        return Err(unavailable());
                    }
                    Ok(CursorKeyringInit::Initialized)
                }
                Some(existing) if existing == fingerprint => Ok(CursorKeyringInit::Confirmed),
                Some(_) => Err(unavailable()),
            }
        })
    }
}
