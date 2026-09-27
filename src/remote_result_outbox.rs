//! Dormant offline result custody. No authentication, transport or task application.
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
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const MAX_PENDING: usize = 128;
pub const MAX_COMMAND_BYTES: usize = 32_768;
const VERSION: &str = "hosted-cadence-result.v1";
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const APPLICATION_ID: u32 = 0x434f4231; // COB1; format guard, not authentication.
const SCHEMA_VERSION: u32 = 1;
const DB_NAME: &str = "results.sqlite3";
const SCHEMA_SQL: &str = "CREATE TABLE pending_results (
    command_id TEXT PRIMARY KEY, destination TEXT NOT NULL,
    payload TEXT NOT NULL, digest TEXT NOT NULL, stored_at_ms INTEGER NOT NULL
)";

fn invalid() -> Error {
    Error::rejected("Invalid offline result command or destination")
}
fn corrupt() -> Error {
    Error::internal("Offline result custody is invalid; retain it for inspection")
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
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
}
impl PendingResult {
    pub fn receipt(&self) -> &LocalReceipt {
        &self.receipt
    }
    pub fn command(&self) -> &ResultCommand {
        &self.command
    }
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
        if !created {
            verify_header(&mut file)?;
            let read_only = Connection::open_with_flags(
                &path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )?;
            read_only.busy_timeout(Duration::from_secs(5))?;
            verify_schema(&read_only)?;
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
            tx.pragma_update(None, "application_id", APPLICATION_ID)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
            // Persist the creation entry as well as SQLite's synced transaction.
            File::open(dir)?.sync_all()?;
        }
        let app_id: u32 = conn.pragma_query_value(None, "application_id", |r| r.get(0))?;
        let version: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if app_id != APPLICATION_ID || version != SCHEMA_VERSION {
            return Err(corrupt());
        }
        verify_schema(&conn)?;
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
        rows.into_iter().map(decode_row).collect()
    }
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

fn verify_schema(conn: &Connection) -> Result<()> {
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
    // views, extra tables/indexes and other mutating objects. This is format
    // compatibility, not protection against a hostile same-UID file owner.
    let expected_sql: String = SCHEMA_SQL
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if objects.len() != 2 {
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
    let (kind, name, table, sql) = &objects[1];
    let actual_sql: String = sql
        .as_deref()
        .ok_or_else(corrupt)?
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if kind != "table"
        || name != "pending_results"
        || table != "pending_results"
        || actual_sql != expected_sql
    {
        return Err(corrupt());
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
    })
}
fn private_metadata(meta: &fs::Metadata, directory: bool) -> Result<()> {
    let safe = meta.uid() == unsafe { libc::geteuid() }
        && if directory {
            meta.is_dir() && meta.mode() & 0o777 == 0o700
        } else {
            meta.is_file() && meta.mode() & 0o777 == 0o600 && meta.nlink() == 1
        };
    if safe {
        Ok(())
    } else {
        Err(Error::rejected(
            "Offline outbox paths must be private, owned and not symlinks",
        ))
    }
}
fn verify_header(file: &mut File) -> Result<()> {
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
        || version != SCHEMA_VERSION
    {
        return Err(corrupt());
    }
    Ok(())
}
