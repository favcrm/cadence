//! Finite Store lifecycle adapter. Selectors are not authority: only the
//! constructor's authenticated private owner relay can produce a grant.
//! The relay retains path custody, consumes the external obligation before
//! effects, and reports lost acknowledgements as UNKNOWN (never retries).

use crate::error::{Error, Result};
#[path = "owner_transport.rs"]
mod transport;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
pub(crate) use transport::StoreOwnerGrant;

pub(super) const MAX_OWNER_INTEGER: i64 = 9_007_199_254_740_991;
/// Fixed independently owner-provisioned writable enclave, never elected from
/// a restored snapshot, caller path or environment variable.
pub(crate) const DATABASE_PATH: &str = "/srv/cadence/protected/store/cadence.db";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Purpose {
    Init,
    Restore,
    Open,
    Close,
    Witness,
}

/// Wire selectors for the fixed owner operation. Public fields deliberately
/// confer no authority; Store APIs accept StoreOwnerGrant, never Binding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binding {
    pub database_id: String,
    pub incarnation: String,
    pub database_epoch: u64,
    pub operation: String,
    pub purpose: Purpose,
    pub path: String,
    pub challenge: Vec<u8>,
    pub attempt: String,
    pub artifact: String,
    pub deadline_unix: i64,
    pub source: Option<RestoreSource>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RestoreSource {
    pub incarnation: String,
    pub database_epoch: u64,
    /// Digest of the externally validated, standalone SQLite artifact,
    /// NOT a marker inside the restored guest snapshot.
    pub sha256: String,
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}

impl Binding {
    pub(crate) fn validate(&self) -> Result<()> {
        if !identifier(&self.database_id)
            || !identifier(&self.incarnation)
            || !identifier(&self.operation)
            || !identifier(&self.attempt)
            || !identifier(&self.artifact)
            || self.challenge.len() != 32
            || self.database_epoch == 0
            || self.database_epoch > MAX_OWNER_INTEGER as u64
            || self.challenge.iter().all(|b| *b == 0)
            || self
                .deadline_unix
                .saturating_sub(super::super::now() as i64)
                > 300
            || self.deadline_unix <= super::super::now() as i64
            || !std::path::Path::new(&self.path).is_absolute()
            || self.path.len() > 4096
            || self.path.as_bytes().contains(&0)
        {
            return Err(Error::rejected("invalid or expired Store owner binding"));
        }
        match (&self.source, self.purpose) {
            (Some(source), Purpose::Restore)
                if identifier(&source.incarnation)
                    && source.incarnation != self.incarnation
                    && source.database_epoch > 0
                    && source.database_epoch < self.database_epoch
                    && source.sha256.len() == 64
                    && !source.sha256.bytes().all(|b| b == b'0')
                    && source
                        .sha256
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
            {
                Ok(())
            }
            (None, Purpose::Init | Purpose::Open | Purpose::Close | Purpose::Witness) => Ok(()),
            _ => Err(Error::rejected(
                "Store restore lineage is absent or inconsistent",
            )),
        }
    }
}

pub(in crate::store) struct StoreOpenPermit {
    grant: Arc<StoreOwnerGrant>,
}
impl StoreOpenPermit {
    pub(super) fn issue(grant: StoreOwnerGrant) -> Result<Self> {
        grant.binding().validate()?;
        if !matches!(
            grant.binding().purpose,
            Purpose::Init | Purpose::Restore | Purpose::Open
        ) {
            return Err(Error::rejected(
                "Store open permit authorizes a different operation",
            ));
        }
        grant.recheck()?;
        Ok(Self {
            grant: Arc::new(grant),
        })
    }
    pub(in crate::store) fn binding(&self) -> &Binding {
        self.grant.binding()
    }
    /// Burn externally before any SQLite connection/file creation. Failure
    /// leaves the outcome UNKNOWN; the grant itself forbids another consume.
    pub(in crate::store) fn take(&self) -> Result<()> {
        self.grant.consume()
    }
    pub(in crate::store) fn recheck(&self) -> Result<()> {
        self.grant.recheck()
    }
    pub(in crate::store) fn current(&self) -> CurrentDatabase {
        CurrentDatabase {
            grant: self.grant.clone(),
        }
    }
}

pub(in crate::store) struct CurrentDatabase {
    grant: Arc<StoreOwnerGrant>,
}
impl CurrentDatabase {
    pub(super) fn binding(&self) -> &Binding {
        self.grant.binding()
    }
    pub(super) fn recheck(&self) -> Result<()> {
        self.grant.recheck()
    }
    /// This check runs while BEGIN IMMEDIATE is held. A swapped incarnation,
    /// missing row, malformed epoch or extra identity row can never default-open.
    pub(super) fn check_identity(&self, conn: &Connection) -> Result<()> {
        let binding = self.binding();
        check_identity(conn, binding, false)?;
        check_open_latch(conn, binding.database_epoch)
    }
}

pub(in crate::store) const IDENTITY_SCHEMA: &str = "
CREATE TABLE store_incarnation(
    id INTEGER PRIMARY KEY CHECK(id=1),
    database_id TEXT NOT NULL,
    incarnation TEXT NOT NULL,
    epoch INTEGER NOT NULL CHECK(epoch>0),
    operation TEXT NOT NULL,
    artifact TEXT NOT NULL);
CREATE TABLE store_business_highwater(
    id INTEGER PRIMARY KEY CHECK(id=1),
    sequence INTEGER NOT NULL CHECK(sequence>=0));
INSERT INTO store_business_highwater VALUES(1,0);
CREATE TABLE store_witness_capture(
    witness_seq INTEGER PRIMARY KEY,
    business_highwater INTEGER NOT NULL CHECK(business_highwater>=0),
    captured_at REAL NOT NULL);
";

/// Even an absent/partially initialized file in the fixed protected enclave
/// cannot be elected as legacy. Recheck the SQLite main filename under the
/// writer lock as well, so an alternate raw/side spelling cannot bypass this.
pub(in crate::store) fn refuse_legacy_path(path: &std::path::Path) -> Result<()> {
    let enclave = std::path::Path::new(DATABASE_PATH)
        .parent()
        .ok_or_else(|| Error::rejected("protected Store enclave is invalid"))?;
    // Direct reserved selectors refuse without probing the real enclave.
    // An unresolved symlink cannot safely elect a fresh legacy database: it
    // could follow a dangling chain into an absent protected destination.
    if path.starts_with(enclave) {
        return Err(Error::rejected(
            "raw/legacy writer refused in the protected Store enclave",
        ));
    }
    let resolved = match path.canonicalize() {
        Ok(path) => Some(path),
        Err(_) => {
            if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(Error::rejected(
                    "unresolved raw/legacy writer alias refused",
                ));
            }
            path.parent()
                .and_then(|p| p.canonicalize().ok())
                .zip(path.file_name())
                .map(|(p, name)| p.join(name))
        }
    };
    if resolved.is_some_and(|p| p.starts_with(enclave)) {
        return Err(Error::rejected(
            "raw/legacy writer refused in the protected Store enclave",
        ));
    }
    Ok(())
}

/// The authenticated constructor additionally retains exclusive custody of
/// this parent directory/inode for the entire operation. Lexical checks alone
/// are not path authority and cannot protect against a guest swap.
pub(in crate::store) fn check_path(path: &std::path::Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::rejected("Store path has no parent"))?;
    if path != std::path::Path::new(DATABASE_PATH)
        || parent.canonicalize()? != parent
        || std::fs::symlink_metadata(path).is_ok_and(|m| !m.file_type().is_file())
    {
        return Err(Error::rejected(
            "Store owner path is not the exact canonical regular database",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let directory = std::fs::symlink_metadata(parent)?;
        if directory.uid() != 21000 || directory.mode() & 0o7777 != 0o700 {
            return Err(Error::rejected(
                "protected database directory custody refused",
            ));
        }
        for ancestor in parent.ancestors() {
            let m = std::fs::symlink_metadata(ancestor)?;
            if !m.is_dir() || !matches!(m.uid(), 0 | 21000) || m.mode() & 0o022 != 0 {
                return Err(Error::rejected(
                    "protected database ancestor custody refused",
                ));
            }
        }
        if let Ok(m) = std::fs::symlink_metadata(path) {
            if m.uid() != 21000 || m.mode() & 0o7777 != 0o600 || m.nlink() != 1 {
                return Err(Error::rejected("protected database inode custody refused"));
            }
        }
    }
    Ok(())
}

fn read_file(path: &std::path::Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    Ok(options.open(path)?)
}

pub(in crate::store) fn insert_identity(conn: &Connection, b: &Binding) -> Result<()> {
    conn.execute(
        "INSERT INTO store_incarnation(id,database_id,incarnation,epoch,operation,artifact)
         VALUES(1,?1,?2,?3,?4,?5)",
        rusqlite::params![
            b.database_id,
            b.incarnation,
            b.database_epoch as i64,
            b.operation,
            b.artifact
        ],
    )?;
    Ok(())
}

fn require_standalone(path: &std::path::Path) -> Result<()> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let side = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
        match std::fs::symlink_metadata(side) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            _ => return Err(Error::rejected("protected opening requires an externally validated standalone image; unresolved journal/WAL refused")),
        }
    }
    Ok(())
}

pub(in crate::store) fn preflight_existing(path: &std::path::Path, b: &Binding) -> Result<()> {
    require_standalone(path)?;
    // Immutable URI forbids SQLite journal recovery/SHM creation. Percent
    // encoding prevents path components from selecting URI options.
    let mut uri = String::from("file:");
    for byte in b.path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-_.~".contains(&byte) {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri.push_str("?immutable=1");
    let ro = Connection::open_with_flags(
        uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    validate_open_tx(&ro, b)?;
    // immutable=1 reports journal_mode=delete even for a WAL header; inspect
    // the held standalone file header, never issue a value-bearing pragma.
    use std::io::Read;
    let mut file = read_file(path)?;
    let mut header = [0u8; 100];
    file.read_exact(&mut header)?;
    if &header[..16] != b"SQLite format 3\0"
        || (header[18..20] != [2, 2]
            && !(b.purpose == Purpose::Restore && header[18..20] == [1, 1]))
    {
        return Err(Error::rejected(
            "protected existing database is not a standalone WAL image",
        ));
    }
    if let Some(source) = &b.source {
        use sha2::{Digest, Sha256};
        let mut file = read_file(path)?;
        let mut digest = Sha256::new();
        let mut chunk = [0u8; 65536];
        loop {
            let n = file.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            digest.update(&chunk[..n]);
        }
        if format!("{:x}", digest.finalize()) != source.sha256 {
            return Err(Error::rejected(
                "restored database does not match the externally validated artifact",
            ));
        }
    }
    require_standalone(path)?;
    Ok(())
}

fn check_open_latch(conn: &Connection, expected_epoch: u64) -> Result<()> {
    let (count, closed, done, epoch): (i64, i64, i64, i64) = conn.query_row(
        "SELECT (SELECT count(*) FROM closure_state),closed,witness_done,epoch
         FROM closure_state WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    if count != 1 || closed != 0 || done != 0 || epoch <= 0 || epoch as u64 != expected_epoch {
        return Err(Error::rejected(
            "protected writer refuses closed, missing, malformed or stale latch",
        ));
    }
    Ok(())
}

/// Used both in immutable preflight and again inside BEGIN IMMEDIATE before
/// rebind/recovery. It never creates missing schema or defaults malformed rows.
pub(in crate::store) fn validate_open_tx(conn: &Connection, b: &Binding) -> Result<()> {
    let versions: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT version FROM schema_version")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    if versions.as_slice() != [super::PROTECTED_SCHEMA_MAX] {
        return Err(Error::rejected("protected Store schema must be the exact reviewed version; migration needs a new owner-validated artifact"));
    }
    let restored = b.purpose == Purpose::Restore;
    check_identity(conn, b, restored)?;
    let count: i64 = conn.query_row("SELECT count(*) FROM closure_state", [], |r| r.get(0))?;
    let (closed, done, epoch): (i64, i64, i64) = conn.query_row(
        "SELECT closed,witness_done,epoch FROM closure_state WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let expected_epoch = if restored {
        b.source
            .as_ref()
            .ok_or_else(|| Error::rejected("restore source absent"))?
            .database_epoch
    } else {
        b.database_epoch
    };
    if count != 1
        || epoch <= 0
        || epoch as u64 != expected_epoch
        || (closed, done) != if restored { (1, 1) } else { (0, 0) }
    {
        return Err(Error::rejected(
            "protected opening refuses sealed, malformed, unwitnessed or stale latch",
        ));
    }
    if restored {
        let witnesses: i64 = conn.query_row(
            "SELECT count(*) FROM owner_witness w JOIN closure_state c ON c.id=1
             JOIN store_witness_capture capture ON capture.witness_seq=w.seq
             JOIN store_business_highwater business ON business.id=1
             WHERE w.challenge=c.challenge AND w.attempt=c.attempt
             AND w.artifact_identity=c.artifact AND c.artifact=?2
             AND w.epoch=c.epoch AND w.db_schema=?1
             AND capture.business_highwater=business.sequence AND business.sequence>=0",
            rusqlite::params![super::PROTECTED_SCHEMA_MAX, b.artifact],
            |r| r.get(0),
        )?;
        if witnesses != 1 {
            return Err(Error::rejected(
                "restored image has no exact one-use owner witness",
            ));
        }
    }
    Ok(())
}

pub(super) fn check_identity(conn: &Connection, binding: &Binding, source: bool) -> Result<()> {
    let count: i64 = conn.query_row("SELECT count(*) FROM store_incarnation", [], |r| r.get(0))?;
    let (database_id, incarnation, epoch, operation): (String, String, i64, String) = conn
        .query_row(
            "SELECT database_id,incarnation,epoch,operation FROM store_incarnation WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
    let (expected_incarnation, expected_epoch) = if source {
        let lineage = binding
            .source
            .as_ref()
            .ok_or_else(|| Error::rejected("no external restore lineage"))?;
        (&lineage.incarnation, lineage.database_epoch)
    } else {
        (&binding.incarnation, binding.database_epoch)
    };
    if count != 1
        || database_id != binding.database_id
        || &incarnation != expected_incarnation
        || epoch <= 0
        || epoch as u64 != expected_epoch
        || (!source && operation != binding.operation)
    {
        return Err(Error::rejected(
            "Store owner permit is bound to a different database incarnation/epoch",
        ));
    }
    Ok(())
}

/// Committed readback from `witness_owned`, not caller-supplied facts. Private
/// fields and serialize-only access let the actual registered root encoder
/// emit this result without a generic publish-JSON or DTO constructor.
/// Serialization is DATA, not caller/runtime custody, an authoritative digest,
/// an uploaded artifact or externally validated FINAL/retirement evidence.
#[derive(Serialize)]
pub(crate) struct QuiescedWitness {
    sequence: u64,
    business_highwater: u64,
    database_id: String,
    incarnation: String,
    database_epoch: u64,
    operation: String,
    challenge: Vec<u8>,
    attempt: String,
    artifact: String,
}

impl super::super::Store {
    /// Protected mode is selected by this authentic owner capability, never by
    /// agent_uid, a lease, an OpenMode selector or a restored shutdown marker.
    pub(crate) fn open_owned(
        grant: StoreOwnerGrant,
    ) -> Result<(Self, super::super::RecoveryOutcome)> {
        let permit = StoreOpenPermit::issue(grant)?;
        Self::open_protected(&permit)
    }
    pub(crate) fn close_owned(&self, grant: StoreOwnerGrant, reason: &str) -> Result<()> {
        let permit = super::OwnerMaintenancePermit::issue(grant)?;
        self.propose_close(&permit, reason)
    }
    /// A witness sequence is not the business highwater, a capture/upload,
    /// external artifact validation or FINAL. Those remain distinct owner steps.
    pub(crate) fn witness_owned(&self, grant: StoreOwnerGrant) -> Result<QuiescedWitness> {
        let binding = grant.binding().clone();
        let permit = super::OwnerMaintenancePermit::issue(grant)?;
        let sequence = self.witness_commit(&permit)?;
        let business_highwater: i64 = self.read_tx(|tx| {
            tx.query_row(
                "SELECT business_highwater FROM store_witness_capture WHERE witness_seq=?1",
                [sequence as i64],
                |r| r.get(0),
            )
        })?;
        let business_highwater = u64::try_from(business_highwater)
            .map_err(|_| Error::rejected("malformed witness business highwater"))?;
        Ok(QuiescedWitness {
            sequence,
            business_highwater,
            database_id: binding.database_id,
            incarnation: binding.incarnation,
            database_epoch: binding.database_epoch,
            operation: binding.operation,
            challenge: binding.challenge,
            attempt: binding.attempt,
            artifact: binding.artifact,
        })
    }
}
