//! Physical quiesced-running standalone capture. Only the retained dispatcher
//! can call this after actual committed witness readback; never a guest file,
//! snapshot marker, shell exit/upload or serialized RuntimeProof factory.
use super::super::{refused, Result};
use super::{dispatcher, lifecycle};
use crate::store::Binding;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::OpenOptions;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::time::{Duration, Instant};
const DB: &str = "/srv/cadence/protected/store/cadence.db";
const MAX_BYTES: u64 = 512 * 1024 * 1024;
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Witness {
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
fn check(until: Instant) -> Result<()> {
    super::super::Deadline(until).check()?;
    dispatcher::physical_check(until)
}
fn source(until: Instant) -> Result<Connection> {
    check(until)?;
    let conn = Connection::open_with_flags(
        DB,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| refused())?;
    conn.busy_timeout(Duration::from_millis(10))
        .map_err(|_| refused())?;
    conn.progress_handler(10000, Some(move || Instant::now() >= until));
    conn.execute_batch("BEGIN").map_err(|_| refused())?;
    check(until)?;
    Ok(conn)
}
fn validate(conn: &Connection, w: &Witness) -> Result<()> {
    let versions: Vec<i64> = {
        let mut s = conn
            .prepare("SELECT version FROM schema_version")
            .map_err(|_| refused())?;
        let rows = s
            .query_map([], |r| r.get(0))
            .map_err(|_| refused())?
            .collect::<rusqlite::Result<_>>()
            .map_err(|_| refused())?;
        rows
    };
    if versions != [32] {
        return Err(refused());
    }
    let identity: (i64, String, String, i64, String) = conn
        .query_row(
            "SELECT count(*),database_id,incarnation,epoch,operation FROM store_incarnation",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .map_err(|_| refused())?;
    if identity
        != (
            1,
            w.database_id.clone(),
            w.incarnation.clone(),
            w.database_epoch as i64,
            w.operation.clone(),
        )
    {
        return Err(refused());
    }
    let latch:(i64,i64,i64,Vec<u8>,String,String,i64)=conn.query_row("SELECT count(*),closed,witness_done,challenge,attempt,artifact,epoch FROM closure_state",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).map_err(|_|refused())?;
    if latch
        != (
            1,
            1,
            1,
            w.challenge.clone(),
            w.attempt.clone(),
            w.artifact.clone(),
            w.database_epoch as i64,
        )
    {
        return Err(refused());
    }
    let highwater: i64 = conn
        .query_row(
            "SELECT sequence FROM store_business_highwater WHERE id=1",
            [],
            |r| r.get(0),
        )
        .map_err(|_| refused())?;
    let captured: i64 = conn
        .query_row(
            "SELECT business_highwater FROM store_witness_capture WHERE witness_seq=?1",
            [w.sequence as i64],
            |r| r.get(0),
        )
        .map_err(|_| refused())?;
    let record:(Vec<u8>,String,String,i64,i64)=conn.query_row("SELECT challenge,attempt,artifact_identity,epoch,db_schema FROM owner_witness WHERE seq=?1",[w.sequence as i64],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).map_err(|_|refused())?;
    if highwater < 0
        || highwater as u64 != w.business_highwater
        || captured != highwater
        || record
            != (
                w.challenge.clone(),
                w.attempt.clone(),
                w.artifact.clone(),
                w.database_epoch as i64,
                32,
            )
    {
        return Err(refused());
    }
    Ok(())
}
pub(super) fn database_current(binding: &Binding, until: Instant) -> Result<()> {
    let conn = source(until)?;
    let identity: (i64, String, String, i64, String) = conn
        .query_row(
            "SELECT count(*),database_id,incarnation,epoch,operation FROM store_incarnation",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .map_err(|_| refused())?;
    let versions: (i64, i64) = conn
        .query_row("SELECT count(*),version FROM schema_version", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map_err(|_| refused())?;
    let latch: (i64, i64, i64) = conn
        .query_row(
            "SELECT count(*),closed,witness_done FROM closure_state",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|_| refused())?;
    if identity
        != (
            1,
            binding.database_id.clone(),
            binding.incarnation.clone(),
            binding.database_epoch as i64,
            binding.operation.clone(),
        )
        || versions != (1, 32)
        || latch != (1, 0, 0)
    {
        return Err(refused());
    }
    check(until)
}
pub(super) fn validate_witness(binding: &Binding, value: &Value) -> Result<()> {
    let w: Witness = serde_json::from_value(value.clone()).map_err(|_| refused())?;
    if w.sequence == 0
        || w.sequence > super::MAX_SAFE
        || w.business_highwater > super::MAX_SAFE
        || w.database_id != binding.database_id
        || w.incarnation != binding.incarnation
        || w.database_epoch != binding.database_epoch
        || w.operation != binding.operation
        || w.challenge != binding.challenge
        || w.attempt != binding.attempt
        || w.artifact != binding.artifact
    {
        return Err(refused());
    }
    let until = Instant::now() + Duration::from_secs(10);
    let conn = source(until)?;
    validate(&conn, &w)?;
    check(until)
}
pub(super) fn capture(attempt: &str, value: &Value, until: Instant) -> Result<()> {
    if !super::hex(attempt, 32) {
        return Err(refused());
    }
    let w: Witness = serde_json::from_value(value.clone()).map_err(|_| refused())?;
    if w.attempt != attempt {
        return Err(refused());
    }
    check(until)?;
    let src = source(until)?;
    validate(&src, &w)?;
    let path = format!("/srv/cadence/protected/store/capture-{attempt}.db");
    // Create-new and retain the actual FD. Failure never deletes/reuses an
    // ambiguous artifact. No caller-supplied path/archive/WAL is admitted.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|_| refused())?;
    let initial = file.metadata().map_err(|_| refused())?;
    let mut dst = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| refused())?;
    dst.busy_timeout(Duration::from_millis(10))
        .map_err(|_| refused())?;
    dst.progress_handler(10000, Some(move || Instant::now() >= until));
    {
        let backup = rusqlite::backup::Backup::new(&src, &mut dst).map_err(|_| refused())?;
        loop {
            check(until)?;
            if file.metadata().map_err(|_| refused())?.size() > MAX_BYTES {
                return Err(refused());
            }
            match backup.step(128).map_err(|_| refused())? {
                rusqlite::backup::StepResult::Done => break,
                rusqlite::backup::StepResult::More => {}
                rusqlite::backup::StepResult::Busy | rusqlite::backup::StepResult::Locked => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                _ => return Err(refused()),
            }
        }
    }
    dst.pragma_update(None, "journal_mode", "DELETE")
        .map_err(|_| refused())?;
    validate(&dst, &w)?;
    let integrity: String = dst
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .map_err(|_| refused())?;
    if integrity != "ok" {
        return Err(refused());
    }
    dst.close().map_err(|_| refused())?;
    file.sync_all().map_err(|_| refused())?;
    let captured = file.metadata().map_err(|_| refused())?;
    let named = std::fs::symlink_metadata(&path).map_err(|_| refused())?;
    if !captured.is_file()
        || captured.nlink() != 1
        || captured.uid() != 0
        || captured.gid() != 0
        || captured.mode() & 0o7777 != 0o600
        || captured.size() == 0
        || captured.size() > MAX_BYTES
        || (initial.dev(), initial.ino()) != (captured.dev(), captured.ino())
        || (named.dev(), named.ino()) != (captured.dev(), captured.ino())
    {
        return Err(refused());
    }
    for suffix in ["-wal", "-shm", "-journal"] {
        if std::fs::symlink_metadata(format!("{path}{suffix}")).is_ok() {
            return Err(refused());
        }
    }
    let digest = crate::adapter::pi_guest::execfd::sha256_fd(&file, captured.size())
        .map_err(|_| refused())?;
    check(until)?;
    validate(&src, &w)?;
    // Host retains the actual qualified provider process/private carrier,
    // validates these exact standalone bytes and stores immutable owner data.
    // This transfer is NOT itself external validation, qualification or FINAL.
    lifecycle::store_capture_begin(attempt, value, captured.size(), &digest, until)?;
    use std::os::unix::fs::FileExt;
    let mut offset = 0u64;
    let mut bytes = vec![0u8; 16384];
    while offset < captured.size() {
        check(until)?;
        let n = file.read_at(&mut bytes, offset).map_err(|_| refused())?;
        if n == 0 {
            return Err(refused());
        }
        lifecycle::store_capture_chunk(attempt, offset, &bytes[..n], until)?;
        offset += n as u64;
    }
    let after = file.metadata().map_err(|_| refused())?;
    if (
        captured.dev(),
        captured.ino(),
        captured.size(),
        captured.ctime(),
        captured.ctime_nsec(),
        captured.mtime(),
        captured.mtime_nsec(),
    ) != (
        after.dev(),
        after.ino(),
        after.size(),
        after.ctime(),
        after.ctime_nsec(),
        after.mtime(),
        after.mtime_nsec(),
    ) {
        return Err(refused());
    }
    validate(&src, &w)?;
    check(until)?;
    lifecycle::store_capture_commit(attempt, &digest, until)
}
