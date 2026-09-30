//! Local result custody and explicit hosted queued receipt. No task application.
//!
//! Explicit paths and immutable destination pins never consult the daemon,
//! credentials, environment defaults or the selected org connection.
use crate::{Error, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const MAX_PENDING: usize = 128;
pub const MAX_COMMAND_BYTES: usize = 32_768;
const VERSION: &str = "hosted-cadence-result.v1";
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const APPLICATION_ID: u32 = 0x434f4231; // COB1; format guard, not authentication.
const SCHEMA_VERSION: u32 = 2;
const DB_NAME: &str = "results.sqlite3";
const SCHEMA_SQL: &str = "CREATE TABLE pending_results (
    command_id TEXT PRIMARY KEY, destination TEXT NOT NULL,
    payload TEXT NOT NULL, digest TEXT NOT NULL, stored_at_ms INTEGER NOT NULL
)";
const RECEIPT_SQL: &str = "CREATE TABLE queued_receipts (
    command_id TEXT PRIMARY KEY, receipt TEXT NOT NULL
)";
const MAX_RESPONSE_BYTES: u64 = 4096;

fn invalid() -> Error {
    Error::rejected("Invalid offline result command or destination")
}
fn corrupt() -> Error {
    Error::internal("Offline result custody is invalid; retain it for inspection")
}
// Shared AgenticOS hosted id contract: ASCII letters/digits/'_'/'-', 1..=200
// (packages/contracts/src/hosted-ids.ts). The old 128 cap rejected issued ids.
const MAX_IDENTIFIER: usize = 200;
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENTIFIER
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

// Field declaration order is the AOS-64 canonical UTF8 JSON digest contract.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireCommand {
    version: String,
    command_id: String,
    kind: String,
    assignment_id: String,
    task_id: String,
    #[serde(deserialize_with = "decoded_revision")]
    task_revision: u64,
    turn_id: String,
    reported_head_sha: String,
    text: String,
}

fn decoded_revision<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<u64, D::Error> {
    use serde::de::Error as _;
    let number = serde_json::Number::deserialize(deserializer)?;
    // Match the server's decoded JS Number semantics, including 1.0/1e0,
    // then emit an integer in the canonical JSON representation.
    let value = number
        .as_f64()
        .ok_or_else(|| D::Error::custom("Invalid revision"))?;
    if !value.is_finite() || value < 1.0 || value > MAX_SAFE_INTEGER as f64 || value.fract() != 0.0
    {
        return Err(D::Error::custom("Invalid revision"));
    }
    Ok(value as u64)
}

/// Only parsing can construct this validated canonical command.
#[derive(Clone, PartialEq, Eq)]
pub struct ResultCommand {
    command_id: String,
    canonical: String,
    digest: String,
}
impl fmt::Debug for ResultCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResultCommand")
            .field("command_id", &self.command_id)
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}
impl ResultCommand {
    pub fn parse_json(input: &str) -> Result<Self> {
        // Bound raw parsing separately; whitespace need not affect canonical size.
        if input.len() > MAX_COMMAND_BYTES * 2 {
            return Err(invalid());
        }
        let command: WireCommand = serde_json::from_str(input).map_err(|_| invalid())?;
        if command.version != VERSION
            || command.kind != "agent_result"
            || ![
                &command.command_id,
                &command.assignment_id,
                &command.task_id,
                &command.turn_id,
            ]
            .into_iter()
            .all(|s| identifier(s))
            || command.task_revision == 0
            || command.task_revision > MAX_SAFE_INTEGER
            || !matches!(command.reported_head_sha.len(), 40 | 64)
            || !command
                .reported_head_sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || command.text.is_empty()
        {
            return Err(invalid());
        }
        // serde_json rejects lone escaped surrogates; Rust strings are scalars.
        let canonical = serde_json::to_string(&command).map_err(|_| invalid())?;
        if canonical.len() > MAX_COMMAND_BYTES {
            return Err(invalid());
        }
        let digest = format!("{:x}", Sha256::digest(canonical.as_bytes()));
        Ok(Self {
            command_id: command.command_id,
            canonical,
            digest,
        })
    }
    pub fn canonical_json(&self) -> &str {
        &self.canonical
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

/// Structural original destination, never a credential or authenticated identity.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationPin {
    organization_id: String,
    audience: String,
    subject_id: String,
    agent_id: String,
}
impl DestinationPin {
    pub fn new(org: &str, audience: &str, subject: &str, agent: &str) -> Result<Self> {
        if ![org, subject, agent].into_iter().all(identifier) || !canonical_origin(audience) {
            return Err(invalid());
        }
        Ok(Self {
            organization_id: org.into(),
            audience: audience.into(),
            subject_id: subject.into(),
            agent_id: agent.into(),
        })
    }
    pub fn organization_id(&self) -> &str {
        &self.organization_id
    }
    pub fn audience(&self) -> &str {
        &self.audience
    }
    pub fn subject_id(&self) -> &str {
        &self.subject_id
    }
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }
    fn canonical(&self) -> Result<String> {
        // Deserialize is useful for storage only, and cannot bypass constructor validation.
        Self::new(
            &self.organization_id,
            &self.audience,
            &self.subject_id,
            &self.agent_id,
        )?;
        serde_json::to_string(self).map_err(|_| corrupt())
    }
}

fn canonical_origin(input: &str) -> bool {
    if input.len() > 2048 {
        return false;
    }
    let Ok(uri) = input.parse::<ureq::http::Uri>() else {
        return false;
    };
    let Some(authority) = uri.authority() else {
        return false;
    };
    if uri.scheme_str() != Some("https") || input != format!("https://{authority}") {
        return false;
    }
    let host = authority.host();
    let canonical_host = if let Some(ip) = host.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
    {
        ip.parse::<Ipv6Addr>()
            .is_ok_and(|ip| ip.to_ipv4_mapped().is_none() && host == format!("[{ip}]"))
    } else if let Ok(ip) = host.parse::<Ipv4Addr>() {
        host == ip.to_string()
    } else {
        !host.is_empty()
            && host
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b".-".contains(&b))
            && host
                .split('.')
                .all(|label| !label.is_empty() && !label.starts_with('-') && !label.ends_with('-'))
            && !host
                .rsplit('.')
                .next()
                .is_some_and(|label| label.bytes().all(|b| b.is_ascii_digit()))
            && !host.rsplit('.').next().is_some_and(|label| {
                label.strip_prefix("0x").is_some_and(|hex| {
                    !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit())
                })
            })
    };
    if !canonical_host {
        return false;
    }
    match authority.port() {
        Some(port) => {
            port.as_u16() != 443 && authority.as_str() == format!("{host}:{}", port.as_u16())
        }
        None => authority.as_str() == host,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalReceipt {
    command_id: String,
    digest: String,
    destination: DestinationPin,
    stored_at_ms: i64,
}
impl LocalReceipt {
    pub fn command_id(&self) -> &str {
        &self.command_id
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn destination(&self) -> &DestinationPin {
        &self.destination
    }
    pub fn stored_at_ms(&self) -> i64 {
        self.stored_at_ms
    }
    pub fn state(&self) -> &'static str {
        "local_pending"
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingResult {
    receipt: LocalReceipt,
    command: ResultCommand,
    queued: Option<QueuedReceipt>,
}
impl PendingResult {
    pub fn receipt(&self) -> &LocalReceipt {
        &self.receipt
    }
    pub fn command(&self) -> &ResultCommand {
        &self.command
    }
    pub fn queued_receipt(&self) -> Option<&QueuedReceipt> {
        self.queued.as_ref()
    }
    pub fn state(&self) -> &'static str {
        if self.queued.is_some() {
            "remote_queued"
        } else {
            "local_pending"
        }
    }
}

/// A server's queued-custody acknowledgement, never an applied-task witness.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueuedReceipt {
    command_id: String,
    state: String,
    accepted_at: u64,
    expires_at: u64,
    digest: String,
}
impl QueuedReceipt {
    pub fn state(&self) -> &str {
        "remote_queued"
    }
    pub fn command_id(&self) -> &str {
        &self.command_id
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn accepted_at(&self) -> u64 {
        self.accepted_at
    }
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueuedResponse {
    ok: bool,
    receipt: QueuedReceipt,
}

/// Separate offline file. Does not open the daemon Store or perform recovery.
pub struct ResultOutbox {
    conn: Mutex<Connection>,
}
impl ResultOutbox {
    pub fn open(dir: &Path) -> Result<Self> {
        if !dir.is_absolute() {
            return Err(invalid());
        }
        match fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.into()),
        }
        private_metadata(&fs::symlink_metadata(dir)?, true)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(dir.join("init.lock"))?;
        private_metadata(&lock.metadata()?, false)?;
        // Initialization only: ordinary writes serialize through SQLite IMMEDIATE.
        acquire_init_lock(&lock)?;
        validate_journal_path(dir)?;
        let path = dir.join(DB_NAME);
        let created = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
        {
            Ok(file) => {
                private_metadata(&file.metadata()?, false)?;
                true
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(e) => return Err(e.into()),
        };
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)?;
        private_metadata(&file.metadata()?, false)?;
        let old_version = if created {
            SCHEMA_VERSION
        } else {
            verify_header(&mut file)?
        };
        if !created {
            let read_only = Connection::open_with_flags(
                &path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )?;
            read_only.busy_timeout(Duration::from_secs(5))?;
            match read_only.query_row("SELECT COUNT(*) FROM sqlite_schema", [], |row| {
                row.get::<_, i64>(0)
            }) {
                Ok(_) => verify_schema(&read_only, old_version)?,
                Err(rusqlite::Error::SqliteFailure(code, _))
                    if code.extended_code == rusqlite::ffi::SQLITE_READONLY_ROLLBACK =>
                {
                    // A valid hot rollback journal needs writes even to inspect
                    // schema. Recover only a bounded private copy first, leaving
                    // original foreign DB/journal bytes untouched on refusal.
                    recovery_preflight(dir, &path, old_version)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        let mut conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "DELETE")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        if created {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(SCHEMA_SQL)?;
            tx.execute_batch(RECEIPT_SQL)?;
            tx.pragma_update(None, "application_id", APPLICATION_ID)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
            // Persist the creation entry as well as SQLite's synced transaction.
            File::open(dir)?.sync_all()?;
        }
        let app_id: u32 = conn.pragma_query_value(None, "application_id", |r| r.get(0))?;
        let version: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if app_id != APPLICATION_ID || version != old_version {
            return Err(corrupt());
        }
        verify_schema(&conn, old_version)?;
        if old_version == 1 {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(RECEIPT_SQL)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
            verify_schema(&conn, SCHEMA_VERSION)?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn enqueue(
        &self,
        destination: &DestinationPin,
        command: &ResultCommand,
    ) -> Result<LocalReceipt> {
        let pin = destination.canonical()?;
        let mut conn = self.conn.lock().map_err(|_| corrupt())?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx.query_row(
            "SELECT command_id,destination,payload,digest,stored_at_ms FROM pending_results WHERE command_id=?",
            [&command.command_id], raw_row,
        ).optional()?;
        if let Some(raw) = existing {
            let original = decode_row(raw)?;
            if original.receipt.destination != *destination || original.command != *command {
                return Err(Error::rejected(
                    "Offline command conflicts with original result or destination",
                ));
            }
            tx.commit()?;
            return Ok(original.receipt);
        }
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM pending_results", [], |r| r.get(0))?;
        if count >= MAX_PENDING as i64 {
            return Err(Error::rejected(
                "Offline result outbox is full; pending custody was retained",
            ));
        }
        let stored_at_ms = i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| corrupt())?
                .as_millis(),
        )
        .map_err(|_| corrupt())?;
        tx.execute(
            "INSERT INTO pending_results VALUES (?, ?, ?, ?, ?)",
            params![
                command.command_id,
                pin,
                command.canonical,
                command.digest,
                stored_at_ms
            ],
        )?;
        tx.commit()?;
        Ok(LocalReceipt {
            command_id: command.command_id.clone(),
            digest: command.digest.clone(),
            destination: destination.clone(),
            stored_at_ms,
        })
    }

    pub fn pending_for(&self, destination: &DestinationPin) -> Result<Vec<PendingResult>> {
        let pin = destination.canonical()?;
        let conn = self.conn.lock().map_err(|_| corrupt())?;
        let mut stmt = conn.prepare(
            "SELECT command_id,destination,payload,digest,stored_at_ms
            FROM pending_results WHERE destination=? ORDER BY stored_at_ms,command_id LIMIT 129",
        )?;
        let rows = stmt
            .query_map([pin], raw_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if rows.len() > MAX_PENDING {
            return Err(corrupt());
        }
        rows.into_iter()
            .map(|raw| decode_row_with_receipt(&conn, raw))
            .collect()
    }

    /// Look up by immutable command ID; selection never trusts a current org or caller pin.
    pub fn get(&self, command_id: &str) -> Result<PendingResult> {
        if !identifier(command_id) {
            return Err(invalid());
        }
        let conn = self.conn.lock().map_err(|_| corrupt())?;
        let raw = conn.query_row(
            "SELECT command_id,destination,payload,digest,stored_at_ms FROM pending_results WHERE command_id=?",
            [command_id], raw_row,
        ).optional()?.ok_or_else(|| Error::rejected("Offline result command is not retained"))?;
        decode_row_with_receipt(&conn, raw)
    }

    fn record_queued(&self, queued: &QueuedReceipt) -> Result<QueuedReceipt> {
        let mut conn = self.conn.lock().map_err(|_| corrupt())?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = tx.query_row(
            "SELECT command_id,destination,payload,digest,stored_at_ms FROM pending_results WHERE command_id=?",
            [&queued.command_id], raw_row,
        ).optional()?.ok_or_else(|| Error::rejected("Offline result command is not retained"))?;
        let local = decode_row(raw)?;
        validate_queued(queued, local.receipt())?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT receipt FROM queued_receipts WHERE command_id=?",
                [&queued.command_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            let original: QueuedReceipt = serde_json::from_str(&existing).map_err(|_| corrupt())?;
            validate_queued(&original, local.receipt()).map_err(|_| corrupt())?;
            if original != *queued {
                return Err(Error::rejected(
                    "Hosted queued receipt conflicts with original custody",
                ));
            }
            tx.commit()?;
            return Ok(original);
        }
        let json = serde_json::to_string(queued).map_err(|_| corrupt())?;
        tx.execute(
            "INSERT INTO queued_receipts VALUES (?, ?)",
            params![queued.command_id, json],
        )?;
        tx.commit()?;
        Ok(queued.clone())
    }
}

// Offline v1 normally occupies less than 5 MiB. The larger explicit recovery
// copy bound tolerates SQLite page overhead while refusing unbounded input.
const MAX_RECOVERY_FILE_BYTES: u64 = 16 * 1024 * 1024;
fn validate_journal_path(dir: &Path) -> Result<()> {
    for name in ["results.sqlite3-wal", "results.sqlite3-shm"] {
        match fs::symlink_metadata(dir.join(name)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
            Ok(_) => return Err(corrupt()),
        }
    }
    validate_journal_file(&dir.join("results.sqlite3-journal"))
}
fn validate_journal_file(path: &Path) -> Result<()> {
    // A concurrent SQLite DELETE-mode commit can unlink the journal after
    // open() and before fstat(). Recheck that one disappearing path; keep the
    // ordinary private-file rule for every still-linked journal.
    for _ in 0..3 {
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        if journal_needs_recheck(path, &file)? {
            continue;
        }
        return Ok(());
    }
    Err(Error::rejected(
        "Offline outbox journal changed during inspection; custody was retained",
    ))
}
fn journal_needs_recheck(path: &Path, file: &File) -> Result<bool> {
    let metadata = file.metadata()?;
    if metadata.nlink() == 0 {
        if metadata.uid() != unsafe { libc::geteuid() } || !private_file_fields(&metadata) {
            return Err(unsafe_private_path());
        }
        if metadata.len() > MAX_RECOVERY_FILE_BYTES {
            return Err(corrupt());
        }
        // The opened inode is no longer reachable through this name. Do not
        // reject a completed SQLite transaction; inspect any replacement on
        // the next iteration, including symlinks and hardlinks.
        return match fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Ok(_) => Ok(true),
            Err(e) => Err(e.into()),
        };
    }
    private_metadata(&metadata, false)?;
    if metadata.len() > MAX_RECOVERY_FILE_BYTES {
        return Err(corrupt());
    }
    Ok(false)
}
fn recovery_preflight(dir: &Path, original: &Path, expected_version: u32) -> Result<()> {
    let scratch = tempfile::Builder::new()
        .prefix("recovery-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(dir)?;
    private_metadata(&fs::symlink_metadata(scratch.path())?, true)?;
    let copy = scratch.path().join(DB_NAME);
    copy_private_file(original, &copy)?;
    copy_private_file(
        &dir.join("results.sqlite3-journal"),
        &scratch.path().join("results.sqlite3-journal"),
    )?;
    let conn = Connection::open_with_flags(
        &copy,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    conn.busy_timeout(Duration::from_secs(5))?;
    // The first schema read performs recovery on the disposable copy only.
    verify_schema(&conn, expected_version)?;
    let app: u32 = conn.pragma_query_value(None, "application_id", |r| r.get(0))?;
    let version: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if app != APPLICATION_ID || version != expected_version {
        return Err(corrupt());
    }
    // Copy/recovery/schema failures retain originals. This preflight is not
    // an authoritative data snapshot and cannot exclude same-UID replacement.
    Ok(())
}
fn copy_private_file(source: &Path, destination: &Path) -> Result<()> {
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(source)?;
    let metadata = input.metadata()?;
    private_metadata(&metadata, false)?;
    if metadata.len() > MAX_RECOVERY_FILE_BYTES {
        return Err(corrupt());
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(destination)?;
    let copied = std::io::copy(
        &mut input.by_ref().take(MAX_RECOVERY_FILE_BYTES + 1),
        &mut output,
    )?;
    if copied != metadata.len() || copied > MAX_RECOVERY_FILE_BYTES {
        return Err(corrupt());
    }
    Ok(())
}

fn acquire_init_lock(file: &File) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if !matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
        ) {
            return Err(error.into());
        }
        if Instant::now() >= deadline {
            return Err(Error::rejected(
                "Offline outbox initialization is busy; custody was retained",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn verify_schema(conn: &Connection, version: u32) -> Result<()> {
    let mut stmt =
        conn.prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name")?;
    let objects = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // The exact v1 DDL fixes affinity/nullability/PK, and excludes all triggers,
    // views, extra tables/indexes and other mutating objects. Compare the exact
    // own DDL: stripping whitespace can confuse TEXT NOT NULL with TEXTNOTNULL.
    // This is format
    // compatibility, not protection against a hostile same-UID file owner.
    if objects.len() != if version == 1 { 2 } else { 4 } {
        return Err(corrupt());
    }
    let (kind, name, table, sql) = &objects[0];
    if kind != "index"
        || name != "sqlite_autoindex_pending_results_1"
        || table != "pending_results"
        || sql.is_some()
    {
        return Err(corrupt());
    }
    let table_index = if version == 1 { 1 } else { 2 };
    let (kind, name, table, sql) = &objects[table_index];
    let actual_sql = sql.as_deref().ok_or_else(corrupt)?;
    if kind != "table"
        || name != "pending_results"
        || table != "pending_results"
        || actual_sql != SCHEMA_SQL
    {
        return Err(corrupt());
    }
    if version == 2 {
        let (kind, name, table, sql) = &objects[1];
        if kind != "index"
            || name != "sqlite_autoindex_queued_receipts_1"
            || table != "queued_receipts"
            || sql.is_some()
        {
            return Err(corrupt());
        }
        let (kind, name, table, sql) = &objects[3];
        if kind != "table"
            || name != "queued_receipts"
            || table != "queued_receipts"
            || sql.as_deref() != Some(RECEIPT_SQL)
        {
            return Err(corrupt());
        }
    }
    Ok(())
}

type RawRow = (String, String, String, String, i64);
fn raw_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}
fn decode_row(
    (command_id, destination, payload, digest, stored_at_ms): RawRow,
) -> Result<PendingResult> {
    let pin: DestinationPin = serde_json::from_str(&destination).map_err(|_| corrupt())?;
    if pin.canonical().map_err(|_| corrupt())? != destination {
        return Err(corrupt());
    }
    let command = ResultCommand::parse_json(&payload).map_err(|_| corrupt())?;
    if command.command_id != command_id
        || command.canonical != payload
        || command.digest != digest
        || stored_at_ms < 0
    {
        return Err(corrupt());
    }
    Ok(PendingResult {
        receipt: LocalReceipt {
            command_id,
            digest,
            destination: pin,
            stored_at_ms,
        },
        command,
        queued: None,
    })
}
fn decode_row_with_receipt(conn: &Connection, raw: RawRow) -> Result<PendingResult> {
    let mut pending = decode_row(raw)?;
    let json: Option<String> = conn
        .query_row(
            "SELECT receipt FROM queued_receipts WHERE command_id=?",
            [pending.receipt.command_id()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(json) = json {
        let queued: QueuedReceipt = serde_json::from_str(&json).map_err(|_| corrupt())?;
        validate_queued(&queued, pending.receipt()).map_err(|_| corrupt())?;
        if serde_json::to_string(&queued).map_err(|_| corrupt())? != json {
            return Err(corrupt());
        }
        pending.queued = Some(queued);
    }
    Ok(pending)
}
fn validate_queued(receipt: &QueuedReceipt, local: &LocalReceipt) -> Result<()> {
    if receipt.command_id != local.command_id
        || receipt.digest != local.digest
        || receipt.state != "queued"
        || receipt.accepted_at == 0
        || receipt.accepted_at > MAX_SAFE_INTEGER
        || receipt.expires_at <= receipt.accepted_at
        || receipt.expires_at > MAX_SAFE_INTEGER
        || receipt.digest.len() != 64
        || !receipt
            .digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::rejected(
            "Hosted queued receipt does not match local custody",
        ));
    }
    Ok(())
}

/// Send only the stored canonical command to its stored board origin. The
/// transport receives no caller-selected URL or actor fields. A failed or
/// ambiguous HTTP exchange leaves local custody pending for an explicit retry.
pub fn deliver_with<F>(
    outbox: &ResultOutbox,
    command_id: &str,
    expected_organization_id: &str,
    expected_audience: &str,
    child_bearer: &str,
    post: F,
) -> Result<QueuedReceipt>
where
    F: FnOnce(&str, &str, &str) -> Result<(u16, Vec<u8>)>,
{
    let bytes = child_bearer.as_bytes();
    if bytes.len() != 47
        || !bytes.starts_with(b"hct_")
        || !bytes[4..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(b))
    {
        return Err(Error::rejected(
            "An enrolled hosted child bearer is required",
        ));
    }
    let pending = outbox.get(command_id)?;
    let pin = pending.receipt().destination();
    if pin.organization_id() != expected_organization_id || pin.audience() != expected_audience {
        return Err(Error::rejected(
            "Hosted result destination differs from explicit enrollment confirmation",
        ));
    }
    DestinationPin::new(
        pin.organization_id(),
        pin.audience(),
        pin.subject_id(),
        pin.agent_id(),
    )?;
    let url = format!(
        "{}/__platform/hosted-cadence/{}/results",
        pin.audience(),
        pin.organization_id()
    );
    let (status, body) = post(&url, child_bearer, pending.command().canonical_json())
        .map_err(|_| Error::rejected("Hosted result send uncertain; local custody retained"))?;
    if status != 202 || body.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(Error::rejected(
            "Hosted result was not acknowledged; local custody retained",
        ));
    }
    let parsed: QueuedResponse = serde_json::from_slice(&body).map_err(|_| {
        Error::rejected("Hosted result returned an invalid receipt; local custody retained")
    })?;
    if !parsed.ok {
        return Err(Error::rejected(
            "Hosted result was not acknowledged; local custody retained",
        ));
    }
    validate_queued(&parsed.receipt, pending.receipt())?;
    outbox.record_queued(&parsed.receipt)
}
fn private_metadata(meta: &fs::Metadata, directory: bool) -> Result<()> {
    let safe = meta.uid() == unsafe { libc::geteuid() }
        && if directory {
            meta.is_dir() && meta.mode() & 0o777 == 0o700
        } else {
            private_file_fields(meta) && meta.nlink() == 1
        };
    if safe {
        Ok(())
    } else {
        Err(unsafe_private_path())
    }
}
fn private_file_fields(meta: &fs::Metadata) -> bool {
    meta.is_file() && meta.mode() & 0o777 == 0o600
}
fn unsafe_private_path() -> Error {
    Error::rejected("Offline outbox paths must be private, owned and not symlinks")
}
fn verify_header(file: &mut File) -> Result<u32> {
    let mut header = [0u8; 100];
    file.read_exact(&mut header).map_err(|_| corrupt())?;
    // SQLite's file-format specification: big-endian user_version at60,
    // application_id at68. Reject foreign/WAL files before a writable SQLite open.
    let version = u32::from_be_bytes(header[60..64].try_into().map_err(|_| corrupt())?);
    let app_id = u32::from_be_bytes(header[68..72].try_into().map_err(|_| corrupt())?);
    if &header[..16] != b"SQLite format 3\0"
        || header[18] != 1
        || header[19] != 1
        || app_id != APPLICATION_ID
        || !matches!(version, 1 | SCHEMA_VERSION)
    {
        return Err(corrupt());
    }
    Ok(version)
}

#[cfg(test)]
mod hosted_id_boundary_tests {
    use super::*;

    fn wire(id: &str) -> serde_json::Value {
        serde_json::json!({
            "version": VERSION, "commandId": id,
            "kind": "agent_result", "assignmentId": "assignment-1",
            "taskId": "task-1", "taskRevision": 1, "turnId": "turn-1",
            "reportedHeadSha": "a".repeat(40), "text": "finished"
        })
    }
    fn pin() -> DestinationPin {
        DestinationPin::new(
            "org-1",
            "https://gateway.example.invalid",
            "subject-1",
            "agent-1",
        )
        .unwrap()
    }

    #[test]
    fn command_and_pin_ids_obey_the_1_to_200_hosted_contract() {
        for id in [
            "a".repeat(128),
            "a".repeat(129),
            "a".repeat(200),
            format!("{}_-", "z".repeat(198)),
            "0".repeat(200),
        ] {
            let command = ResultCommand::parse_json(&wire(&id).to_string())
                .unwrap_or_else(|_| panic!("{}-char command id was refused", id.len()));
            assert_eq!(command.command_id, id);
            assert!(DestinationPin::new(&id, "https://gateway.example.invalid", &id, &id).is_ok());
        }
        for id in [
            "a".repeat(201),
            "a".repeat(500),
            String::new(),
            "has space".to_owned(),
            "has.dot".to_owned(),
            "has/slash".to_owned(),
            "non-ascii-é".to_owned(),
            "utf8-😀".to_owned(),
        ] {
            assert!(
                ResultCommand::parse_json(&wire(&id).to_string()).is_err(),
                "invalid command id {id:?} was accepted"
            );
            // Each pinned field is checked while the others stay valid.
            for (org, subject, agent) in [
                (id.as_str(), "subject-1", "agent-1"),
                ("org-1", id.as_str(), "agent-1"),
                ("org-1", "subject-1", id.as_str()),
            ] {
                assert!(
                    DestinationPin::new(org, "https://gateway.example.invalid", subject, agent)
                        .is_err(),
                    "pin accepted bad id {id:?} in ({org:?}, {subject:?}, {agent:?})"
                );
            }
        }
    }

    #[test]
    fn long_id_custody_retains_reopens_and_reads_back() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("outbox");
        let id = format!("cmd-{}", "x".repeat(196));
        assert_eq!(id.len(), 200);
        let command = ResultCommand::parse_json(&wire(&id).to_string()).unwrap();
        let destination = pin();
        let receipt = ResultOutbox::open(&dir)
            .unwrap()
            .enqueue(&destination, &command)
            .unwrap();
        assert_eq!(receipt.command_id(), id);
        let reopened = ResultOutbox::open(&dir).unwrap();
        let row = reopened.get(&id).unwrap();
        assert_eq!(row.receipt(), &receipt);
        assert_eq!(row.command(), &command);
        assert_eq!(reopened.pending_for(&destination).unwrap().len(), 1);
        // Idempotent retain must not duplicate or conflict on the long id.
        assert_eq!(reopened.enqueue(&destination, &command).unwrap(), receipt);
    }

    #[test]
    fn oversized_lookup_refuses_before_any_custody_read() {
        let outbox = ResultOutbox {
            conn: Mutex::new(Connection::open_in_memory().unwrap()),
        };
        // Lookup validation happens before SQLite: an oversized or malformed id
        // must be an invalid-input rejection even though this handle has no
        // pending_results table at all.
        for bad in ["a".repeat(201), "has.dot".to_owned(), String::new()] {
            match outbox.get(&bad) {
                Err(Error::Rejected(message)) => assert_eq!(
                    message, "Invalid offline result command or destination",
                    "{bad:?}"
                ),
                other => panic!("{bad:?} lookup reached custody: {other:?}"),
            }
        }
        // A valid id reaches the database and fails on the missing table,
        // proving the boundary refusal above happened before any custody read.
        match outbox.get("valid-id") {
            Err(Error::Internal(_)) => {}
            other => panic!("valid lookup did not reach custody: {other:?}"),
        }
    }

    #[test]
    fn wire_id_fields_share_the_200_character_boundary() {
        for field in ["commandId", "assignmentId", "taskId", "turnId"] {
            for (length, ok) in [(128usize, true), (129, true), (200, true), (201, false)] {
                let mut value = wire("cmd");
                value[field] = serde_json::json!("i".repeat(length));
                assert_eq!(
                    ResultCommand::parse_json(&value.to_string()).is_ok(),
                    ok,
                    "{field} length {length}"
                );
            }
        }
    }
}

#[cfg(test)]
mod journal_race_tests {
    use super::*;

    #[test]
    fn deleted_sqlite_journal_descriptor_is_rechecked_without_accepting_replacements() {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let database = root.path().join(DB_NAME);
        let journal = root.path().join("results.sqlite3-journal");
        let conn = Connection::open(&database).unwrap();
        conn.execute_batch("PRAGMA journal_mode=DELETE; CREATE TABLE values_test (value INTEGER);")
            .unwrap();
        fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();

        conn.execute_batch("BEGIN IMMEDIATE; INSERT INTO values_test VALUES (1);")
            .unwrap();
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&journal)
            .unwrap();
        assert_eq!(file.metadata().unwrap().nlink(), 1);
        conn.execute_batch("COMMIT;").unwrap();
        let unlinked = file.metadata().unwrap();
        assert_eq!(unlinked.nlink(), 0);
        assert!(!journal.exists());
        // The former blanket file rule rejected this ordinary SQLite commit.
        assert!(private_metadata(&unlinked, false).is_err());
        assert!(!journal_needs_recheck(&journal, &file).unwrap());
        validate_journal_file(&journal).unwrap();

        let outside = root.path().join("outside");
        fs::write(&outside, b"untouched").unwrap();
        std::os::unix::fs::symlink(&outside, &journal).unwrap();
        assert!(journal_needs_recheck(&journal, &file).unwrap());
        assert!(validate_journal_file(&journal).is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"untouched");
        fs::remove_file(&journal).unwrap();

        conn.execute_batch("BEGIN IMMEDIATE; INSERT INTO values_test VALUES (2);")
            .unwrap();
        let hardlink = root.path().join("hardlink");
        fs::hard_link(&journal, &hardlink).unwrap();
        assert!(validate_journal_file(&journal).is_err());
        fs::remove_file(&hardlink).unwrap();
        conn.execute_batch("COMMIT;").unwrap();

        let non_private = root.path().join("non-private-journal");
        fs::write(&non_private, b"not private").unwrap();
        fs::set_permissions(&non_private, fs::Permissions::from_mode(0o644)).unwrap();
        let non_private_file = File::open(&non_private).unwrap();
        fs::remove_file(&non_private).unwrap();
        assert!(journal_needs_recheck(&non_private, &non_private_file).is_err());

        let fifo = root.path().join("fifo-journal");
        let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
        let fifo_file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(&fifo)
            .unwrap();
        fs::remove_file(&fifo).unwrap();
        assert!(journal_needs_recheck(&fifo, &fifo_file).is_err());
    }
}
