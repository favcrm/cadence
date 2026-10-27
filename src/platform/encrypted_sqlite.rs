//! CAD-1128: dedicated encrypted SQLite custody for Connections.
//!
//! One manager-owned serialized rollback connection (DELETE journal,
//! synchronous=EXTRA) holds immutable ciphertext rows under
//! XChaCha20-Poly1305 with the operator-provisioned 32-byte key held
//! outside the state dir. The core store keeps the locator; only the
//! core reference is authority — an authentic vault row without a
//! matching core custody reference is an orphan and never executes.
//!
//! Honest scope (contract I2): encryption here is persistence
//! confidentiality, not isolation — a same-UID agent can still read
//! the key file and database. `custody_isolation` stays None; the
//! operator must explicitly accept that risk at initialize/unlock.
//!
//! Failure model (contract §journal/transaction policy): short
//! `BEGIN IMMEDIATE` writes; COMMIT-busy is explicitly rolled back
//! and verified before the refusal returns; any unknown commit or
//! rollback outcome fences the manager — it drops the key and serves
//! `storage_unavailable` until a fresh open.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rusqlite::{
    params,
    types::{Type, ValueRef},
    Connection, OpenFlags,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use zeroize::{Zeroize, Zeroizing};

use crate::error::{Error, Result};

#[cfg(all(test, target_os = "linux", feature = "test-seam"))]
pub(crate) mod custody6_hooks;

/// The custody-tag prefix written into `platform_credentials.custody`
/// for a vault-backed record. The full reference is `TAG_PREFIX` +
/// a 32-hex-char row locator.
pub const TAG_PREFIX: &str = "encrypted-sqlite-v1:";

/// Fixed public error codes (contract §Envelope/errors). Nothing
/// here formats a secret, path or raw SQLite/provider text.
const CODE_LOCKED: &str = "locked";
const CODE_KEY_UNAVAILABLE: &str = "key_unavailable";
const CODE_INTEGRITY: &str = "integrity_failure";
const CODE_INVALID_STORE: &str = "invalid_store";
const CODE_UNSUPPORTED_FORMAT: &str = "unsupported_format";
const CODE_CONFLICT: &str = "conflict";
const CODE_BUSY: &str = "busy";
const CODE_STORAGE: &str = "storage_unavailable";

/// Byte/field bounds (contract §Envelope, addressing and bounds).
const MAX_FIELD: usize = 64;
const WORKSPACE_HEX: usize = 32;
const MAX_SECRET: usize = 65_536;
const MAX_CIPHERTEXT: usize = TAG_BYTES + MAX_SECRET;
const MAX_EXCHANGE: usize = 16;
const NONCE_BYTES: usize = 24;
const TAG_BYTES: usize = 16;
const LOCATOR_BYTES: usize = 16;
const KEY_BYTES: usize = 32;
const KEY_ID_BYTES: usize = 16;

const SCHEMA_VERSION: i64 = 1;
const ENVELOPE_VERSION: i64 = 1;
const ALGORITHM: &str = "xchacha20poly1305";
// The record/header domain strings and the length-prefixed framing live in
// the shared custody primitive (`cadence_core::connection_dispatch`), which
// is byte-identical to this module. Delegating keeps ONE AAD implementation
// across native and hosted while the frozen CAD-1128 checks keep pinning the
// bytes: a change there fails them.

const BUSY_TIMEOUT_MS: u64 = 5_000;

fn invalid(code: &'static str, message: &str) -> Error {
    if code == CODE_BUSY {
        return Error::busy_coded(code, message);
    }
    Error::invalid(code, message)
}

fn sql_busy(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::SqliteFailure(code, _) if code.code == rusqlite::ErrorCode::DatabaseBusy)
}
fn sql_refusal(error: &rusqlite::Error) -> Error {
    if sql_busy(error) {
        invalid(CODE_BUSY, "custody writer busy")
    } else if matches!(error, rusqlite::Error::SqliteFailure(code, _) if code.code == rusqlite::ErrorCode::ConstraintViolation)
    {
        invalid(CODE_CONFLICT, "custody immutable row conflict")
    } else {
        invalid(CODE_STORAGE, "custody write refused")
    }
}

/// What the public record columns must match before a decrypt is
/// admitted — the core identity the caller expects, bound into AAD.
/// All fields are supplied by the authoritative core record, never
/// by the caller or the stored row alone.
#[derive(Clone, Debug)]
pub struct ExpectedIdentity {
    pub workspace_id: String,
    pub connection_id: String,
    pub provider: String,
    pub account: String,
    pub credential_revision: u64,
    pub exchange: String,
    /// The 16-byte key id this daemon's configured key claims.
    pub key_id: [u8; KEY_ID_BYTES],
}

/// One vault row's public metadata — everything except the
/// ciphertext-bearing plaintext fields. `reference` is the full
/// `encrypted-sqlite-v1:<hex>` locator string.
#[cfg(all(test, unix, feature = "test-seam"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RowMetadata {
    pub reference: String,
    pub workspace_id: String,
    pub connection_id: String,
    pub provider: String,
    pub account: String,
    pub exchange: String,
    pub credential_revision: u64,
    /// SHA-256 of `nonce || ciphertext` — a tamper-evident digest of
    /// the physical row contents, public because it reveals nothing.
    pub nonce_and_ciphertext_digest: [u8; 32],
}

/// Owner-selected daemon configuration for encrypted custody.
/// Never sourced from an RPC request or PM: the operator sets it
/// through daemon configuration (ServeOptions/fixture), exactly like
/// `smtp_internal`/`hosted_email`.
#[derive(Clone, Debug)]
pub struct EncryptedCustodyConfig {
    /// Absolute path to the owner-provisioned 32-byte key file —
    /// must live OUTSIDE the state dir, backup/export roots and any
    /// agent-readable path it nominally protects against.
    pub key_file: PathBuf,
    /// Dedicated vault directory — defaults derived under
    /// `<state_dir>/vault` by the daemon; kept explicit so tests can
    /// point it at a fixture path.
    pub vault_dir: PathBuf,
}

/// The unlocked key material — zeroized on drop, never Debug/Clone.
struct Key {
    bytes: Zeroizing<[u8; KEY_BYTES]>,
    id: [u8; KEY_ID_BYTES],
}

impl Key {
    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new_from_slice(self.bytes.as_slice())
            .expect("32-byte key always makes an XChaCha20 cipher")
    }
}

/// Manager state — the ONLY connection into the vault database.
/// `fenced` latches an unknown commit/rollback outcome: a manager that
/// cannot prove its last operation is dead drops its key and refuses
/// every later call until explicitly reopened.
struct Inner {
    // Close SQLite before dropping the probe fd (POSIX lock lifetime).
    conn: Connection,
    files: Option<super::encrypted_fs::VaultFiles>,
    key: Option<Key>,
    fenced: bool,
}

/// The one serialized vault manager. `Mutex<Inner>` IS the custody
/// serialization for encrypted records — every encrypted read/write
/// holds it for the duration of its SQL work.
pub struct EncryptedVault {
    dir: PathBuf,
    key_file: PathBuf,
    inner: Mutex<Inner>,
}

/// Record AAD v1 — the exact concatenation the contract pins. Delegates to
/// the shared custody primitive so native and hosted cannot drift; the value
/// is byte-identical and the frozen CAD-1128 checks still pin it.
fn record_aad(id: &ExpectedIdentity, locator: &[u8; LOCATOR_BYTES]) -> Vec<u8> {
    // Borrow the identity fields straight into the shared AAD builder: the
    // native bytes are unchanged, and no record just to compute them is
    // copied.
    cadence_core::connection_dispatch::native_record_aad_borrowed(
        cadence_core::connection_dispatch::NativeCustodyIdentityRefV1 {
            workspace_id: &id.workspace_id,
            connection_id: &id.connection_id,
            provider: &id.provider,
            account: &id.account,
            credential_revision: id.credential_revision,
            key_id: &id.key_id,
            exchange: &id.exchange,
        },
        locator,
    )
}

/// Header AAD v1 — authenticates the empty-plaintext key-confirmation
/// row under the same domain discipline. Delegated for the same reason.
fn header_aad(workspace_id: &str, key_id: &[u8; KEY_ID_BYTES]) -> Vec<u8> {
    cadence_core::connection_dispatch::native_header_aad(workspace_id, key_id)
}

/// Validate a stored identifier column BEFORE it becomes a Rust
/// string or AAD input: 1..=64 UTF-8 bytes, no normalization.
fn bounded_str(value: &[u8], max: usize) -> Result<String> {
    if value.is_empty() || value.len() > max {
        return Err(invalid(
            CODE_INVALID_STORE,
            "custody row field out of bounds",
        ));
    }
    String::from_utf8(value.to_vec())
        .map_err(|_| invalid(CODE_INVALID_STORE, "custody row field is not UTF-8"))
}

// Adapted from the corrected cad1128-confidentiality-swe B2 artifact.
// No allocation occurs until raw storage class and exact length pass.
fn take_owned(v: ValueRef<'_>, want: Type, min: usize, max: usize) -> rusqlite::Result<Vec<u8>> {
    let bytes = match (want, v) {
        (Type::Text, ValueRef::Text(b)) | (Type::Blob, ValueRef::Blob(b)) => b,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    if bytes.len() < min || bytes.len() > max {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(bytes.to_vec())
}
fn take_text(
    v: ValueRef<'_>,
    min: usize,
    max: usize,
    grammar: impl FnOnce(&str) -> bool,
) -> rusqlite::Result<String> {
    let ValueRef::Text(bytes) = v else {
        return Err(rusqlite::Error::InvalidQuery);
    };
    if bytes.len() < min || bytes.len() > max {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| rusqlite::Error::InvalidQuery)?;
    if !grammar(text) {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(text.to_string())
}
fn workspace_valid(s: &str) -> bool {
    s.len() == WORKSPACE_HEX
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn exchange_valid(s: &str) -> bool {
    matches!(s, "smtp" | "token" | "consent" | "native-model-v1")
}

fn lower_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

impl EncryptedVault {
    /// Construct a manager for `config` — does NOT open the database
    /// or read the key. A configured-but-locked daemon serves fixed
    /// `locked` refusals until `initialize`/`unlock` runs.
    pub fn new(config: EncryptedCustodyConfig) -> Result<Self> {
        if !config.key_file.is_absolute() {
            return Err(invalid(
                CODE_KEY_UNAVAILABLE,
                "custody key file path is not absolute",
            ));
        }
        if !config.vault_dir.is_absolute() {
            return Err(invalid(
                CODE_INVALID_STORE,
                "custody vault directory is not absolute",
            ));
        }
        Ok(Self {
            dir: config.vault_dir.clone(),
            key_file: config.key_file,
            inner: Mutex::new(Inner {
                conn: Connection::open_in_memory()
                    .map_err(|_| invalid(CODE_STORAGE, "custody manager setup failed"))?,
                files: None,
                key: None,
                fenced: true, // Nothing is usable until a real open happens.
            }),
        })
    }

    /// The configured key's id while unlocked — `None` locked.
    /// Enrollment uses it to bind the AAD; never the key itself.
    pub fn key_id(&self) -> Option<[u8; KEY_ID_BYTES]> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .key
            .as_ref()
            .map(|k| k.id)
    }

    /// Close the physical custody handle before a fresh recovery attempt.
    fn reset_locked(inner: &mut Inner) -> Result<()> {
        inner.key = None;
        inner.fenced = true;
        let empty = Connection::open_in_memory()
            .map_err(|_| invalid(CODE_STORAGE, "custody handle invalidation failed"))?;
        let old = std::mem::replace(&mut inner.conn, empty);
        drop(old);
        inner.files = None;
        Ok(())
    }

    pub(crate) fn fence(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let _ = Self::reset_locked(&mut inner);
    }

    /// Read the owner-provisioned key file with every filesystem
    /// check the contract names: owner-only traversal, no symlink
    /// components, regular file owned by this process's uid,
    /// mode 0600, single link, exactly 32 bytes.
    fn read_key_file(&self) -> Result<Key> {
        // Each component opens relative to its pinned parent descriptor.
        // The final descriptor, not a raced pathname, is validated and read.
        let file = super::encrypted_fs::owner_file(&self.key_file)?;
        use std::io::Read;
        let mut bytes = Zeroizing::new([0u8; KEY_BYTES]);
        let mut file = file.take((KEY_BYTES as u64) + 1);
        let mut read = 0usize;
        loop {
            match file.read(&mut bytes[read..]) {
                Ok(0) => break,
                Ok(n) => read += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    return Err(invalid(
                        CODE_KEY_UNAVAILABLE,
                        "custody key file is unreadable",
                    ))
                }
            }
        }
        if read != KEY_BYTES {
            return Err(invalid(
                CODE_KEY_UNAVAILABLE,
                "custody key file must hold exactly 32 bytes",
            ));
        }
        // The file must end exactly at 32 — a longer file is foreign.
        let mut extra = [0u8; 1];
        if file.read(&mut extra).unwrap_or(1) != 0 {
            return Err(invalid(
                CODE_KEY_UNAVAILABLE,
                "custody key file must hold exactly 32 bytes",
            ));
        }
        let id = crate::secret::fingerprint_key_id(bytes.as_slice());
        Ok(Key { bytes, id })
    }

    /// Open the existing database without CREATE and enforce the
    /// contract's effective-settings readback. Called only inside
    /// `inner`'s mutex; the caller has already read the key file.
    fn open_existing(dir: &Path) -> Result<(Connection, super::encrypted_fs::VaultFiles)> {
        let path = dir.join("custody.sqlite3");
        // Pre-open descriptor/header/sidecar checks; preserve valid hot journals
        // for SQLite recovery, never convert/delete a WAL topology.
        let files = super::encrypted_fs::VaultFiles::open(dir, false)?;
        {
            use std::os::unix::fs::FileExt;
            let mut header = [0u8; 100];
            let n = files
                .db
                .read_at(&mut header, 0)
                .map_err(|_| invalid(CODE_INVALID_STORE, "custody store is unreadable"))?;
            if n < 100
                || &header[..16] != b"SQLite format 3\0"
                || header[18] != 1
                || header[19] != 1
            {
                return Err(invalid(
                    CODE_UNSUPPORTED_FORMAT,
                    "custody database format is unsupported",
                ));
            }
        }
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|_| invalid(CODE_INVALID_STORE, "custody store could not be opened"))?;
        // SQLite NOFOLLOW uses the admitted owner/root-controlled pathname.
        // Pinned parents and inode rechecks refuse drift; this is not a
        // defense against an active same-UID adversary replacing safe paths.
        files.check_connection(&conn)?;
        conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS))
            .map_err(|_| invalid(CODE_STORAGE, "custody store refused busy policy"))?;
        Self::enforce_settings(&conn)?;
        Self::check_schema(&conn)?;
        files.check_connection(&conn)?;
        Ok((conn, files))
    }

    /// Set and read back the contract's fixed PRAGMA set. A mismatch
    /// or unsupported setting refuses — never a silent downgrade.
    fn enforce_settings(conn: &Connection) -> Result<()> {
        // Adapted B1 repair from cad1128-confidentiality-swe: assignment
        // forms do not return rows. Separate effective-policy queries follow.
        conn.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=EXTRA; PRAGMA temp_store=MEMORY; PRAGMA trusted_schema=OFF;")
            .map_err(|_| invalid(CODE_STORAGE, "custody policy assignment failed"))?;
        let journal: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .map_err(|_| invalid(CODE_STORAGE, "custody journal policy failed"))?;
        if journal != "delete" {
            return Err(invalid(
                CODE_UNSUPPORTED_FORMAT,
                "custody journal mode is not DELETE",
            ));
        }
        let synchronous: i64 = conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .map_err(|_| invalid(CODE_STORAGE, "custody synchronous policy failed"))?;
        if synchronous != 3 {
            return Err(invalid(CODE_STORAGE, "custody synchronous is not EXTRA"));
        }
        let temp: i64 = conn
            .query_row("PRAGMA temp_store", [], |r| r.get(0))
            .map_err(|_| invalid(CODE_STORAGE, "custody temp_store policy failed"))?;
        if temp != 2 {
            return Err(invalid(CODE_STORAGE, "custody temp_store is not MEMORY"));
        }
        let trusted: i64 = conn
            .query_row("PRAGMA trusted_schema", [], |r| r.get(0))
            .map_err(|_| invalid(CODE_STORAGE, "custody trusted_schema policy failed"))?;
        if trusted != 0 {
            return Err(invalid(CODE_STORAGE, "custody trusted_schema is not OFF"));
        }
        Ok(())
    }

    /// Re-check effective settings before every vault lookup/mutation
    /// (contract: fixed settings are observed behavior, never assumed).
    fn recheck_settings(conn: &Connection) -> Result<()> {
        let journal: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .map_err(|_| invalid(CODE_STORAGE, "custody journal read failed"))?;
        let synchronous: i64 = conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .map_err(|_| invalid(CODE_STORAGE, "custody synchronous read failed"))?;
        let temp: i64 = conn
            .query_row("PRAGMA temp_store", [], |r| r.get(0))
            .map_err(|_| invalid(CODE_STORAGE, "custody temp_store read failed"))?;
        let trusted: i64 = conn
            .query_row("PRAGMA trusted_schema", [], |r| r.get(0))
            .map_err(|_| invalid(CODE_STORAGE, "custody trust read failed"))?;
        let busy: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .map_err(|_| invalid(CODE_STORAGE, "custody timeout read failed"))?;
        if journal != "delete"
            || synchronous != 3
            || temp != 2
            || trusted != 0
            || busy != BUSY_TIMEOUT_MS as i64
        {
            return Err(invalid(
                CODE_STORAGE,
                "custody effective settings drifted from policy",
            ));
        }
        Ok(())
    }

    fn validate_identity(id: &ExpectedIdentity) -> Result<()> {
        if !workspace_valid(&id.workspace_id)
            || id.connection_id.len() > MAX_FIELD
            || id.provider.len() > MAX_FIELD
            || id.account.len() > MAX_FIELD
            || id.credential_revision == 0
            || id.credential_revision > i64::MAX as u64
            || !exchange_valid(&id.exchange)
            || crate::proto::identifier(&id.connection_id, "Connection").is_err()
            || super::connections::provider_identifier(&id.provider, "Provider").is_err()
            || super::connections::provider_account_identifier(&id.provider, &id.account).is_err()
        {
            return Err(invalid(CODE_INVALID_STORE, "custody identity is invalid"));
        }
        Ok(())
    }

    fn check_settings_or_fence(inner: &mut Inner) -> Result<()> {
        let checked = inner
            .files
            .as_ref()
            .ok_or_else(|| invalid(CODE_STORAGE, "custody files unavailable"))
            .and_then(|files| files.check_connection(&inner.conn))
            .and_then(|_| Self::recheck_settings(&inner.conn));
        if let Err(error) = checked {
            let _ = Self::reset_locked(inner);
            return Err(error);
        }
        Ok(())
    }

    /// Exactly the contract's v1 schema — present and nothing else.
    fn check_schema(conn: &Connection) -> Result<()> {
        let user_version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(|_| invalid(CODE_INVALID_STORE, "custody schema is unreadable"))?;
        if user_version != SCHEMA_VERSION {
            return Err(invalid(
                CODE_UNSUPPORTED_FORMAT,
                "custody schema version is unsupported",
            ));
        }
        // One header row, always.
        let headers: i64 = conn
            .query_row("SELECT count(*) FROM vault_header", [], |r| r.get(0))
            .map_err(|_| invalid(CODE_INVALID_STORE, "custody header is absent"))?;
        if headers != 1 {
            return Err(invalid(
                CODE_INVALID_STORE,
                "custody header must be exactly one row",
            ));
        }
        Ok(())
    }

    /// `initialize`: create a fresh vault directory + database + header
    /// only when nothing exists. Never truncates, repairs or rekeys an
    /// existing store.
    pub fn initialize(&self, workspace_id: &str, core_has_encrypted_reference: bool) -> Result<()> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if core_has_encrypted_reference || !workspace_valid(workspace_id) {
            return Err(invalid(
                CODE_CONFLICT,
                "encrypted authority already exists or workspace is invalid",
            ));
        }
        Self::reset_locked(&mut inner)?;
        let key = self.read_key_file()?;
        let nonce = Self::fresh_nonce()?;
        let aad = header_aad(workspace_id, &key.id);
        let tag = key
            .cipher()
            .encrypt(
                &nonce,
                Payload {
                    msg: b"",
                    aad: &aad,
                },
            )
            .map_err(|_| invalid(CODE_INTEGRITY, "custody header seal failed"))?;
        // Exclusive descriptor-relative 0600 DB precreation and pinned parents;
        // no chmod/unlink of existing foreign material or failed initialization.
        let files = super::encrypted_fs::VaultFiles::open(&self.dir, true)?;
        let conn = Connection::open_with_flags(
            self.dir.join("custody.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|_| invalid(CODE_STORAGE, "custody store open failed"))?;
        files.check_connection(&conn)?;
        conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS))
            .map_err(|_| invalid(CODE_STORAGE, "custody busy policy failed"))?;
        Self::enforce_settings(&conn)?;
        // Adapted corrected B5 artifact: schema and header share ONE commit;
        // BEGIN, statement, and COMMIT errors are distinct phases.
        if let Err(error) = conn.execute_batch("BEGIN IMMEDIATE") {
            if !conn.is_autocommit() {
                return Err(invalid(
                    "outcome_unknown",
                    "custody initialization begin state unknown",
                ));
            }
            return Err(sql_refusal(&error));
        }
        let setup = conn.execute_batch(
            "CREATE TABLE vault_header(
                singleton INTEGER PRIMARY KEY CHECK(singleton=1),
                schema INTEGER NOT NULL CHECK(schema=1), envelope INTEGER NOT NULL CHECK(envelope=1),
                algorithm TEXT NOT NULL CHECK(algorithm='xchacha20poly1305'), workspace_id TEXT NOT NULL,
                key_id BLOB NOT NULL CHECK(length(key_id)=16), nonce BLOB NOT NULL CHECK(length(nonce)=24),
                tag BLOB NOT NULL CHECK(length(tag)=16));
             CREATE TABLE records(
                locator BLOB PRIMARY KEY CHECK(length(locator)=16), workspace_id TEXT NOT NULL,
                connection_id TEXT NOT NULL, provider TEXT NOT NULL, account TEXT NOT NULL,
                credential_revision INTEGER NOT NULL CHECK(credential_revision>0),
                key_id BLOB NOT NULL CHECK(length(key_id)=16), exchange TEXT NOT NULL CHECK(length(exchange)<=16),
                schema INTEGER NOT NULL CHECK(schema=1), envelope INTEGER NOT NULL CHECK(envelope=1),
                algorithm TEXT NOT NULL CHECK(algorithm='xchacha20poly1305'),
                nonce BLOB NOT NULL UNIQUE CHECK(length(nonce)=24),
                ciphertext BLOB NOT NULL CHECK(length(ciphertext) BETWEEN 17 AND 65552));
             PRAGMA user_version=1;"
        ).and_then(|_| conn.execute(
            "INSERT INTO vault_header VALUES(1,1,1,'xchacha20poly1305',?1,?2,?3,?4)",
            params![workspace_id, key.id.as_slice(), nonce.as_slice(), tag]).map(|_| ()));
        if setup.is_err() {
            if conn.execute_batch("ROLLBACK").is_ok() && conn.is_autocommit() {
                return Err(invalid(CODE_STORAGE, "custody initialization rolled back"));
            }
            return Err(invalid(
                "outcome_unknown",
                "custody initialization rollback unknown",
            ));
        }
        if files.check_connection(&conn).is_err() {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(invalid(
                "outcome_unknown",
                "custody initialization filesystem changed",
            ));
        }
        if let Err(error) = conn.execute_batch("COMMIT") {
            // Only active COMMIT-Busy has a positively known rollback path.
            if matches!(error, rusqlite::Error::SqliteFailure(ref code, _) if code.code == rusqlite::ErrorCode::DatabaseBusy)
                && !conn.is_autocommit()
                && conn.execute_batch("ROLLBACK").is_ok()
                && conn.is_autocommit()
            {
                return Err(invalid(
                    CODE_BUSY,
                    "custody initialization rolled back after busy",
                ));
            }
            return Err(invalid(
                "outcome_unknown",
                "custody initialization commit unknown",
            ));
        }
        if !conn.is_autocommit() {
            return Err(invalid(
                "outcome_unknown",
                "custody initialization transaction unknown",
            ));
        }
        files
            .check_connection(&conn)
            .map_err(|_| invalid("outcome_unknown", "custody filesystem changed after commit"))?;
        files.sync()?;
        inner.conn = conn;
        inner.files = Some(files);
        inner.key = Some(key);
        inner.fenced = false;
        Ok(())
    }

    /// `unlock`: read the key file, open the existing store,
    /// authenticate the header against the supplied workspace. On any
    /// failure the manager stays locked with a fixed diagnostic.
    pub fn unlock(&self, workspace_id: &str) -> Result<()> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        Self::reset_locked(&mut inner)?;
        let key = self.read_key_file()?;
        let (conn, files) = Self::open_existing(&self.dir)?;
        let (stored_ws, key_id, nonce, tag): (String, Vec<u8>, Vec<u8>, Vec<u8>) = conn
            .query_row(
                "SELECT workspace_id,key_id,nonce,tag,schema,envelope,algorithm FROM vault_header WHERE singleton=1",
                [],
                |r| {
                    if r.get::<_, i64>(4)? != SCHEMA_VERSION || r.get::<_, i64>(5)? != ENVELOPE_VERSION {
                        return Err(rusqlite::Error::InvalidQuery);
                    }
                    let _algorithm = take_text(r.get_ref(6)?, ALGORITHM.len(), ALGORITHM.len(), |s| s == ALGORITHM)?;
                    Ok((take_text(r.get_ref(0)?, WORKSPACE_HEX, WORKSPACE_HEX, workspace_valid)?,
                        take_owned(r.get_ref(1)?, Type::Blob, KEY_ID_BYTES, KEY_ID_BYTES)?,
                        take_owned(r.get_ref(2)?, Type::Blob, NONCE_BYTES, NONCE_BYTES)?,
                        take_owned(r.get_ref(3)?, Type::Blob, TAG_BYTES, TAG_BYTES)?))
                },
            )
            .map_err(|_| invalid(CODE_INVALID_STORE, "custody header is absent"))?;
        if stored_ws != workspace_id
            || key_id.as_slice() != key.id
            || nonce.len() != NONCE_BYTES
            || tag.len() != TAG_BYTES
        {
            return Err(invalid(
                CODE_INTEGRITY,
                "custody key or store does not match this workspace",
            ));
        }
        let aad = header_aad(workspace_id, &key.id);
        let nonce_arr: &[u8; NONCE_BYTES] = nonce
            .as_slice()
            .try_into()
            .map_err(|_| invalid(CODE_INVALID_STORE, "custody header nonce is malformed"))?;
        key.cipher()
            .decrypt(
                nonce_arr.into(),
                Payload {
                    msg: &tag,
                    aad: &aad,
                },
            )
            .map_err(|_| invalid(CODE_INTEGRITY, "custody key failed its confirmation"))?;
        files.check_connection(&conn)?;
        inner.conn = conn;
        inner.files = Some(files);
        inner.key = Some(key);
        inner.fenced = false;
        Ok(())
    }

    /// `lock`: drop the cached key/cipher; refuse `busy` while another
    /// operation owns the section (contract: no deferred lock).
    /// Locking is always safe to acknowledge when already locked.
    pub fn lock(&self) -> Result<()> {
        match self.inner.try_lock() {
            Ok(mut inner) => {
                Self::reset_locked(&mut inner)?;
                inner.fenced = false;
                Ok(())
            }
            Err(std::sync::TryLockError::Poisoned(e)) => Self::reset_locked(&mut e.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => Err(invalid(
                CODE_BUSY,
                "custody is in use — lock is not deferred",
            )),
        }
    }

    /// Public status: locked/unlocked + fenced, never key material.
    pub fn status(&self) -> Value {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        json!({
            "configured": true,
            "unlocked": inner.key.is_some() && !inner.fenced,
            "fenced": inner.fenced,
            "isolation": "none",
            "protected": false,
        })
    }

    fn fresh_nonce() -> Result<XNonce> {
        let mut bytes = [0u8; NONCE_BYTES];
        getrandom::fill(&mut bytes)
            .map_err(|_| invalid(CODE_STORAGE, "entropy for a custody nonce is unavailable"))?;
        Ok(XNonce::from(bytes))
    }

    /// Write one immutable ciphertext row — the vault side of an
    /// enrollment/rotation attempt. Returns the full `TAG_PREFIX<hex>`
    /// reference. This is the ONLY way a row lands; no update/delete
    /// path exists in v1.
    pub fn write_row(&self, id: &ExpectedIdentity, plaintext: &[u8]) -> Result<String> {
        if plaintext.is_empty() || plaintext.len() > MAX_SECRET {
            return Err(invalid(
                CODE_INVALID_STORE,
                "custody secret exceeds its supported bounds",
            ));
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.fenced {
            return Err(invalid(
                CODE_STORAGE,
                "custody manager is fenced after an unknown outcome",
            ));
        }
        if inner.key.is_none() {
            return Err(invalid(CODE_LOCKED, "custody is locked"));
        }
        Self::check_settings_or_fence(&mut inner)?;
        let key = inner
            .key
            .as_ref()
            .ok_or_else(|| invalid(CODE_LOCKED, "custody is locked"))?;
        if id.key_id != key.id {
            return Err(invalid(CODE_INTEGRITY, "custody key identity mismatch"));
        }
        Self::validate_identity(id)?;
        // Identity bounds before any crypto or SQL.
        if id.workspace_id.len() != WORKSPACE_HEX
            || !id
                .workspace_id
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || id.connection_id.len() > MAX_FIELD
            || id.connection_id.is_empty()
            || id.provider.len() > MAX_FIELD
            || id.provider.is_empty()
            || id.account.len() > MAX_FIELD
            || id.account.is_empty()
            || id.exchange.len() > MAX_EXCHANGE
            || id.exchange.is_empty()
            || id.credential_revision == 0
        {
            return Err(invalid(
                CODE_INVALID_STORE,
                "custody identity fields are out of bounds",
            ));
        }
        let mut locator = [0u8; LOCATOR_BYTES];
        getrandom::fill(&mut locator)
            .map_err(|_| invalid(CODE_STORAGE, "entropy for a custody locator is unavailable"))?;
        let nonce = Self::fresh_nonce()?;
        // Contract: a nonce equal to the header's or any existing
        // row's refuses before encryption — the single actual key
        // makes that equality meaningful.
        let collision: bool = inner
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM vault_header WHERE nonce=?1)
                 OR EXISTS(SELECT 1 FROM records WHERE nonce=?1)",
                params![nonce.as_slice()],
                |r| r.get::<_, i64>(0),
            )
            .map(|v| v != 0)
            .map_err(|_| invalid(CODE_STORAGE, "custody nonce check failed"))?;
        if collision {
            return Err(invalid(CODE_CONFLICT, "custody nonce collision"));
        }
        let aad = record_aad(id, &locator);
        let ciphertext = key
            .cipher()
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| invalid(CODE_INTEGRITY, "custody sealing failed"))?;
        if ciphertext.len() > MAX_CIPHERTEXT {
            return Err(invalid(
                CODE_INVALID_STORE,
                "custody ciphertext exceeds its bound",
            ));
        }
        // Phase-aware handling: BEGIN refusal is not COMMIT uncertainty.
        // Only active COMMIT-Busy + positive rollback proves nonpublication.
        if let Err(error) = inner.conn.execute_batch("BEGIN IMMEDIATE") {
            if !inner.conn.is_autocommit() {
                let _ = Self::reset_locked(&mut inner);
                return Err(invalid("outcome_unknown", "custody begin state unknown"));
            }
            return Err(sql_refusal(&error));
        }
        let inserted = inner.conn.execute(
            "INSERT INTO records
             (locator,workspace_id,connection_id,provider,account,credential_revision,
              key_id,exchange,schema,envelope,algorithm,nonce,ciphertext)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,1,1,'xchacha20poly1305',?9,?10)",
            params![
                locator.as_slice(),
                id.workspace_id,
                id.connection_id,
                id.provider,
                id.account,
                id.credential_revision as i64,
                key.id.as_slice(),
                id.exchange,
                nonce.as_slice(),
                ciphertext
            ],
        );
        if let Err(error) = inserted {
            if inner.conn.execute_batch("ROLLBACK").is_ok() && inner.conn.is_autocommit() {
                return Err(sql_refusal(&error));
            }
            let _ = Self::reset_locked(&mut inner);
            return Err(invalid("outcome_unknown", "custody write rollback unknown"));
        }
        if inner
            .files
            .as_ref()
            .ok_or_else(|| invalid(CODE_STORAGE, "custody files unavailable"))
            .and_then(|files| files.check_connection(&inner.conn))
            .is_err()
        {
            let known = inner.conn.execute_batch("ROLLBACK").is_ok() && inner.conn.is_autocommit();
            let _ = Self::reset_locked(&mut inner);
            return Err(invalid(
                if known {
                    CODE_STORAGE
                } else {
                    "outcome_unknown"
                },
                "custody filesystem changed",
            ));
        }
        if let Err(error) = inner.conn.execute_batch("COMMIT") {
            if sql_busy(&error)
                && !inner.conn.is_autocommit()
                && inner.conn.execute_batch("ROLLBACK").is_ok()
                && inner.conn.is_autocommit()
            {
                return Err(invalid(CODE_BUSY, "custody commit rolled back after busy"));
            }
            let _ = Self::reset_locked(&mut inner);
            return Err(invalid("outcome_unknown", "custody commit outcome unknown"));
        }
        if !inner.conn.is_autocommit() {
            let _ = Self::reset_locked(&mut inner);
            return Err(invalid("outcome_unknown", "custody commit state unknown"));
        }
        if Self::check_settings_or_fence(&mut inner).is_err() {
            return Err(invalid(
                "outcome_unknown",
                "custody filesystem or policy changed after commit",
            ));
        }
        Ok(format!("{}{}", TAG_PREFIX, lower_hex(&locator)))
    }

    /// Authenticated decrypt of exactly the row `reference` names,
    /// under the *expected* core identity — both the stored row and
    /// the caller's expectation must match AAD before any plaintext
    /// returns. A wrong/substituted reference is `integrity_failure`.
    pub fn read_exact(&self, reference: &str, id: &ExpectedIdentity) -> Result<Zeroizing<Vec<u8>>> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.fenced {
            return Err(invalid(
                CODE_STORAGE,
                "custody manager is fenced after an unknown outcome",
            ));
        }
        if inner.key.is_none() {
            return Err(invalid(CODE_LOCKED, "custody is locked"));
        }
        Self::check_settings_or_fence(&mut inner)?;
        Self::validate_identity(id)?;
        let key = inner
            .key
            .as_ref()
            .ok_or_else(|| invalid(CODE_LOCKED, "custody is locked"))?;
        let locator_hex = reference
            .strip_prefix(TAG_PREFIX)
            .ok_or_else(|| invalid(CODE_INVALID_STORE, "custody reference is malformed"))?;
        if locator_hex.len() != 32
            || !locator_hex
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(invalid(
                CODE_INVALID_STORE,
                "custody reference is malformed",
            ));
        }
        let locator: [u8; LOCATOR_BYTES] = (0..LOCATOR_BYTES)
            .map(|i| u8::from_str_radix(&locator_hex[i * 2..i * 2 + 2], 16).unwrap_or(0))
            .collect::<Vec<u8>>()
            .try_into()
            .map_err(|_| invalid(CODE_INVALID_STORE, "custody reference is malformed"))?;
        let matches: i64 = inner
            .conn
            .query_row(
                "SELECT count(*) FROM records WHERE locator=?1",
                params![locator.as_slice()],
                |r| r.get(0),
            )
            .map_err(|_| invalid(CODE_INVALID_STORE, "custody reference lookup failed"))?;
        if matches != 1 {
            return Err(invalid(
                if matches == 0 {
                    CODE_INTEGRITY
                } else {
                    CODE_INVALID_STORE
                },
                "custody reference is absent or duplicated",
            ));
        }
        // Raw bounds and grammar precede owned conversion.
        let row = inner.conn.query_row(
            "SELECT workspace_id,connection_id,provider,account,credential_revision,
                        key_id,exchange,schema,envelope,algorithm,nonce,ciphertext
                 FROM records WHERE locator=?1",
            params![locator.to_vec()],
            |r| {
                Ok((
                    take_text(r.get_ref(0)?, WORKSPACE_HEX, WORKSPACE_HEX, workspace_valid)?
                        .into_bytes(),
                    take_text(r.get_ref(1)?, 1, MAX_FIELD, |s| {
                        crate::proto::identifier(s, "Connection").is_ok()
                    })?
                    .into_bytes(),
                    take_text(r.get_ref(2)?, 1, MAX_FIELD, |s| {
                        super::connections::provider_identifier(s, "Provider").is_ok()
                    })?
                    .into_bytes(),
                    take_text(r.get_ref(3)?, 1, MAX_FIELD, |s| {
                        super::connections::provider_account_identifier(&id.provider, s).is_ok()
                    })?
                    .into_bytes(),
                    r.get::<_, i64>(4)?,
                    take_owned(r.get_ref(5)?, Type::Blob, KEY_ID_BYTES, KEY_ID_BYTES)?,
                    take_text(r.get_ref(6)?, 1, MAX_EXCHANGE, exchange_valid)?.into_bytes(),
                    r.get::<_, i64>(7)?,
                    r.get::<_, i64>(8)?,
                    take_text(r.get_ref(9)?, ALGORITHM.len(), ALGORITHM.len(), |s| {
                        s == ALGORITHM
                    })?
                    .into_bytes(),
                    take_owned(r.get_ref(10)?, Type::Blob, NONCE_BYTES, NONCE_BYTES)?,
                    take_owned(r.get_ref(11)?, Type::Blob, TAG_BYTES + 1, MAX_CIPHERTEXT)?,
                ))
            },
        );
        let (
            ws,
            conn_id,
            provider,
            account,
            revision,
            key_id,
            exchange,
            schema,
            envelope,
            algorithm,
            nonce,
            ciphertext,
        ) = match row {
            Ok(row) => row,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err(invalid(CODE_INTEGRITY, "custody row is absent"))
            }
            Err(_) => return Err(invalid(CODE_STORAGE, "custody read failed")),
        };
        if schema != SCHEMA_VERSION
            || envelope != ENVELOPE_VERSION
            || nonce.len() != NONCE_BYTES
            || key_id.len() != KEY_ID_BYTES
            || ciphertext.len() < TAG_BYTES + 1
            || ciphertext.len() > MAX_CIPHERTEXT
        {
            return Err(invalid(
                CODE_UNSUPPORTED_FORMAT,
                "custody row shape is unsupported",
            ));
        }
        // Stored identity must equal expected — the substitution check
        // the frozen test exercises.
        let stored = ExpectedIdentity {
            workspace_id: bounded_str(&ws, WORKSPACE_HEX)?,
            connection_id: bounded_str(&conn_id, MAX_FIELD)?,
            provider: bounded_str(&provider, MAX_FIELD)?,
            account: bounded_str(&account, MAX_FIELD)?,
            credential_revision: u64::try_from(revision)
                .map_err(|_| invalid(CODE_INVALID_STORE, "custody revision is malformed"))?,
            exchange: bounded_str(&exchange, MAX_EXCHANGE)?,
            key_id: key_id
                .as_slice()
                .try_into()
                .map_err(|_| invalid(CODE_INVALID_STORE, "custody key id is malformed"))?,
        };
        if stored.workspace_id != id.workspace_id
            || stored.connection_id != id.connection_id
            || stored.provider != id.provider
            || stored.account != id.account
            || stored.credential_revision != id.credential_revision
            || stored.key_id != id.key_id
            || stored.exchange != id.exchange
            || algorithm != ALGORITHM.as_bytes()
        {
            return Err(invalid(
                CODE_INTEGRITY,
                "custody row identity does not match core authority",
            ));
        }
        let aad = record_aad(id, &locator);
        let nonce_arr: &[u8; NONCE_BYTES] = nonce
            .as_slice()
            .try_into()
            .map_err(|_| invalid(CODE_INVALID_STORE, "custody nonce is malformed"))?;
        key.cipher()
            .decrypt(
                nonce_arr.into(),
                Payload {
                    msg: &ciphertext,
                    aad: &aad,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| invalid(CODE_INTEGRITY, "custody ciphertext failed authentication"))
    }

    /// Bounded public metadata for every row naming `connection_id` —
    /// test/diagnostic inspection under the manager's serialization.
    /// No plaintext, no secret-bearing field is returned.
    #[cfg(all(test, unix, feature = "test-seam"))]
    pub(crate) fn rows_for(&self, connection_id: &str) -> Result<Vec<RowMetadata>> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.fenced {
            return Err(invalid(
                CODE_STORAGE,
                "custody manager is fenced after an unknown outcome",
            ));
        }
        Self::check_settings_or_fence(&mut inner)?;
        let mut stmt = inner
            .conn
            .prepare(
                "SELECT locator,workspace_id,connection_id,provider,account,exchange,
                        credential_revision,nonce,ciphertext
                 FROM records WHERE connection_id=?1 ORDER BY locator",
            )
            .map_err(|_| invalid(CODE_STORAGE, "custody inspection failed"))?;
        let rows = stmt
            .query_map(params![connection_id], |r| {
                let locator = take_owned(r.get_ref(0)?, Type::Blob, LOCATOR_BYTES, LOCATOR_BYTES)?;
                let nonce = take_owned(r.get_ref(7)?, Type::Blob, NONCE_BYTES, NONCE_BYTES)?;
                let ciphertext =
                    take_owned(r.get_ref(8)?, Type::Blob, TAG_BYTES + 1, MAX_CIPHERTEXT)?;
                let provider = take_text(r.get_ref(3)?, 1, MAX_FIELD, |s| {
                    super::connections::provider_identifier(s, "Provider").is_ok()
                })?;
                let revision: i64 = r.get(6)?;
                if revision < 1 {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                let mut joined = nonce.clone();
                joined.extend_from_slice(&ciphertext);
                Ok(RowMetadata {
                    reference: format!("{}{}", TAG_PREFIX, lower_hex(&locator)),
                    workspace_id: take_text(
                        r.get_ref(1)?,
                        WORKSPACE_HEX,
                        WORKSPACE_HEX,
                        workspace_valid,
                    )?,
                    connection_id: take_text(r.get_ref(2)?, 1, MAX_FIELD, |s| {
                        crate::proto::identifier(s, "Connection").is_ok()
                    })?,
                    account: take_text(r.get_ref(4)?, 1, MAX_FIELD, |s| {
                        super::connections::provider_account_identifier(&provider, s).is_ok()
                    })?,
                    provider,
                    exchange: take_text(r.get_ref(5)?, 1, MAX_EXCHANGE, exchange_valid)?,
                    credential_revision: revision as u64,
                    nonce_and_ciphertext_digest: crate::secret::digest_bytes(&joined),
                })
            })
            .map_err(|_| invalid(CODE_STORAGE, "custody inspection failed"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|_| invalid(CODE_STORAGE, "custody inspection failed"))?);
        }
        Ok(out)
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_file(dir: &Path) -> PathBuf {
        use std::io::Write;
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let keys = dir.join("keys");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&keys)
            .unwrap();
        let path = keys.join("key");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(&[7u8; 32]).unwrap();
        path
    }

    fn vault() -> (tempfile::TempDir, EncryptedVault) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let key = key_file(dir.path());
        let vault = EncryptedVault::new(EncryptedCustodyConfig {
            key_file: key,
            vault_dir: dir.path().join("vault"),
        })
        .unwrap();
        (dir, vault)
    }

    fn identity() -> ExpectedIdentity {
        ExpectedIdentity {
            workspace_id: "ab".repeat(16),
            connection_id: "conn-a".into(),
            provider: "smtp".into(),
            account: "owner".into(),
            credential_revision: 1,
            exchange: "smtp".into(),
            key_id: crate::secret::fingerprint_key_id(&[7u8; 32]),
        }
    }

    #[test]
    fn write_and_exact_read_roundtrip() {
        let (_dir, vault) = vault();
        vault.initialize(&identity().workspace_id, false).unwrap();
        let reference = vault.write_row(&identity(), b"synthetic-secret").unwrap();
        assert!(reference.starts_with(TAG_PREFIX));
        let out = vault.read_exact(&reference, &identity()).unwrap();
        assert_eq!(out.as_slice(), b"synthetic-secret");
    }

    #[test]
    fn substituted_identity_refuses_and_preserves_row() {
        let (_dir, vault) = vault();
        vault.initialize(&identity().workspace_id, false).unwrap();
        let reference = vault.write_row(&identity(), b"payload").unwrap();
        let mut wrong = identity();
        wrong.account = "other".into();
        assert!(matches!(
            vault.read_exact(&reference, &wrong),
            Err(ref e) if e.code() == Some(CODE_INTEGRITY)
        ));
        assert_eq!(
            vault
                .read_exact(&reference, &identity())
                .unwrap()
                .as_slice(),
            b"payload"
        );
    }

    #[test]
    fn locked_read_refuses() {
        let (_dir, vault) = vault();
        vault.initialize(&identity().workspace_id, false).unwrap();
        let reference = vault.write_row(&identity(), b"payload").unwrap();
        vault.lock().unwrap();
        assert!(matches!(
            vault.read_exact(&reference, &identity()),
            Err(ref e) if e.code() == Some(CODE_LOCKED)
        ));
        vault.unlock(&identity().workspace_id).unwrap();
        assert_eq!(
            vault
                .read_exact(&reference, &identity())
                .unwrap()
                .as_slice(),
            b"payload"
        );
    }
}

#[cfg(all(test, unix))]
#[path = "encrypted_sqlite/cad1085_native_purpose_acceptance_20261027.rs"]
mod cad1085_native_purpose_acceptance_20261027;
