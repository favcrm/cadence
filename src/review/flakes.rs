//! Flake ledger reads, sightings and serialized append operations.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::time;

/// `<state>/reviews/flakes.jsonl` — one line per sighting of a test that
/// failed in the full run but passed alone on the gated tree and on the
/// base. The ledger is the quarantine: no attribute in the code.
pub const FLAKE_LEDGER: &str = "flakes.jsonl";
/// Distinct PR heads a flake needs sightings on before it stops
/// blocking — repeated reviews of one head never qualify on their own.
pub const KNOWN_FLAKE_HEADS: usize = 3;
pub(super) const FLAKE_WINDOW_SECS: i64 = 14 * 24 * 60 * 60;
const FLAKE_LOCK_WAIT: Duration = Duration::from_secs(5);

/// What the ledger holds for one `(repo, test)` after a sighting.
#[derive(Debug, PartialEq)]
pub struct Sightings {
    /// Every historical sighting, this one included.
    pub total: u64,
    /// Distinct nonempty heads dated within the last 14 days.
    pub heads: usize,
}

impl Sightings {
    pub fn known_flake(&self) -> bool {
        self.heads >= KNOWN_FLAKE_HEADS
    }
}

/// Append `entry` (`repo`, `test`, `head`, …) and return the sightings
/// of its `(repo, test)` now on record. The read and the append happen
/// under an exclusive `flock` on the ledger, so concurrent reviews see
/// exact counts; each line is one `write_all`, so appends never fuse.
/// Malformed lines are skipped.
pub(super) fn normalize_flake_repo(repo: &str) -> Result<String> {
    let repo = repo.trim();
    let slug = repo
        .strip_prefix("https://github.com/")
        .or_else(|| repo.strip_prefix("http://github.com/"))
        .or_else(|| repo.strip_prefix("git@github.com:"))
        .unwrap_or(repo)
        .trim_end_matches('/');
    let slug = slug.strip_suffix(".git").unwrap_or(slug);
    let parts: Vec<_> = slug.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || *part == "."
                || *part == ".."
                || !part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        })
    {
        return Err(Error::rejected(
            "flake repo must be owner/name or a GitHub repository URL",
        ));
    }
    Ok(slug.to_ascii_lowercase())
}

pub(super) fn lock_flake_file(file: &std::fs::File, wait: Duration) -> Result<()> {
    use std::os::unix::io::AsRawFd;
    let deadline = Instant::now() + wait;
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::WouldBlock
            && error.kind() != std::io::ErrorKind::Interrupted
        {
            return Err(error.into());
        }
        if Instant::now() >= deadline {
            return Err(Error::rejected(
                "timed out waiting for the flake ledger lock",
            ));
        }
        std::thread::sleep(
            Duration::from_millis(10).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

fn ledger_entries(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|row| row["test"].as_str().is_some())
        .collect()
}

pub(super) fn flake_sightings(rows: &[Value], entry: &Value, now: i64) -> Sightings {
    let repo = entry["repo"]
        .as_str()
        .and_then(|r| normalize_flake_repo(r).ok());
    let matching: Vec<_> = rows
        .iter()
        .filter(|row| {
            row["test"] == entry["test"]
                && (row.get("repo").is_none()
                    || row["repo"].is_null()
                    || row["repo"]
                        .as_str()
                        .and_then(|r| normalize_flake_repo(r).ok())
                        .is_some_and(|r| Some(r) == repo))
        })
        .collect();
    let heads: BTreeSet<_> = matching
        .iter()
        .filter(|row| {
            row["at"]
                .as_str()
                .and_then(time::parse_iso)
                .is_some_and(|at| at >= now - FLAKE_WINDOW_SECS && at <= now)
        })
        .filter_map(|row| row["head"].as_str().filter(|head| !head.is_empty()))
        .collect();
    Sightings {
        total: matching.len() as u64,
        heads: heads.len(),
    }
}

/// Read historical sightings without creating or modifying the ledger.
/// Test matching is exact; stale and undated records remain visible.
pub fn read_flakes(ledger: &Path, test: Option<&str>) -> Result<Vec<Value>> {
    let text = match std::fs::read_to_string(ledger) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    Ok(ledger_entries(&text)
        .into_iter()
        .filter(|row| test.is_none_or(|test| row["test"].as_str() == Some(test)))
        .collect())
}

pub fn print_flakes(state_dir: PathBuf, test: Option<String>) -> Result<()> {
    for row in read_flakes(
        &state_dir.join("reviews").join(FLAKE_LEDGER),
        test.as_deref(),
    )? {
        println!("{}", serde_json::to_string(&row)?);
    }
    Ok(())
}

pub fn record_flake(ledger: &Path, entry: &Value) -> Result<Sightings> {
    use std::io::{Read, Write};
    let mut entry = entry.clone();
    let repo = entry["repo"]
        .as_str()
        .ok_or_else(|| Error::rejected("flake entry requires repo"))?;
    entry["repo"] = json!(normalize_flake_repo(repo)?);
    if entry["test"].as_str().is_none_or(str::is_empty) {
        return Err(Error::rejected("flake entry requires a nonempty test"));
    }
    if let Some(dir) = ledger.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(ledger)?;
    lock_flake_file(&file, FLAKE_LOCK_WAIT)?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    let mut rows = ledger_entries(&text);
    rows.push(entry.clone());
    let seen = flake_sightings(&rows, &entry, time::now_epoch());
    file.write_all(format!("{}\n", serde_json::to_string(&entry)?).as_bytes())?;
    Ok(seen)
}
