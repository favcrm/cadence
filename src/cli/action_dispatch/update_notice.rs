//! Advisory notices for client-only installations. The updater remains authoritative.

use super::client_update;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{IsTerminal, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) const REFRESH_ARG: &str = "__client-update-notice-refresh";
const DAY: u64 = 24 * 60 * 60;
const HOUR: u64 = 60 * 60;
const MAX_CACHE_BYTES: u64 = 8192;

#[derive(Default, Deserialize, Serialize)]
struct Cache {
    current: String,
    target: Option<String>,
    checked_at: u64,
    attempted_at: u64,
    shown_at: u64,
    shown_target: Option<String>,
}

fn allowed() -> bool {
    std::env::var_os("CADENCE_NO_UPDATE_NOTIFIER").as_deref() != Some(std::ffi::OsStr::new("1"))
        && std::env::var_os("CADENCE_ALIAS").is_none()
        && std::env::var_os("CADENCE_GRANT_TOKEN").is_none()
        && std::env::var_os("CI").is_none()
}

pub(super) fn eligible(cli: &super::Cli) -> bool {
    allowed()
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal()
        && !matches!(
            cli.command,
            super::Commands::Update { .. }
                | super::Commands::Search { .. }
                | super::Commands::Schema { .. }
                | super::Commands::Skill { .. }
                | super::Commands::Remote { .. }
        )
        && !cli
            .command_line
            .as_ref()
            .is_some_and(|ids| ids.iter().any(|id| id == "json" || id == "offline"))
        && !matches!(
            std::env::var("CADENCE_JSON").as_deref(),
            Ok("compact" | "pretty")
        )
}

fn now() -> Option<u64> {
    Some(SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs())
}

fn cache_path(prefix: &Path) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    let digest = Sha256::digest(prefix.as_os_str().as_encoded_bytes());
    Some(
        base.join("cadence")
            .join(format!("client-update-notice-{digest:x}.json")),
    )
}

// One non-blocking lock protects the cache. No lock is held across network I/O.
fn cache_file(prefix: &Path) -> Option<File> {
    let path = cache_path(prefix)?;
    fs::create_dir_all(path.parent()?).ok()?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    // SAFETY: flock operates on the open file descriptor and is released when it is dropped.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return None;
    }
    Some(file)
}

fn read_cache(file: &mut File, current: &str) -> Cache {
    let mut bytes = Vec::new();
    let cache = file.seek(SeekFrom::Start(0)).ok().and_then(|_| {
        (&mut *file)
            .take(MAX_CACHE_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > MAX_CACHE_BYTES {
            return None;
        }
        serde_json::from_slice::<Cache>(&bytes).ok()
    });
    cache
        .filter(|cache| cache.current == current)
        .unwrap_or_else(|| Cache {
            current: current.to_owned(),
            ..Cache::default()
        })
}

fn write_cache(file: &mut File, cache: &Cache) -> Option<()> {
    let bytes = serde_json::to_vec(cache).ok()?;
    file.seek(SeekFrom::Start(0)).ok()?;
    file.set_len(0).ok()?;
    file.write_all(&bytes).ok()?;
    Some(())
}

/// Called after a successful interactive command, without waiting for the network.
pub(super) fn after_success() {
    let Some((prefix, current)) = client_update::notice_install() else {
        return;
    };
    let (Some(now), Some(mut file)) = (now(), cache_file(&prefix)) else {
        return;
    };
    let mut cache = read_cache(&mut file, &current);
    let newer = cache
        .target
        .as_ref()
        .filter(|target| {
            matches!((client_update::version_key(&current), client_update::version_key(target)),
            (Some(have), Some(want)) if want > have)
        })
        .cloned();
    let show = newer.filter(|target| {
        cache.checked_at != 0
            && now.saturating_sub(cache.checked_at) < DAY
            && (cache.shown_target.as_ref() != Some(target)
                || now.saturating_sub(cache.shown_at) >= DAY)
    });
    if let Some(target) = &show {
        cache.shown_target = Some(target.clone());
        cache.shown_at = now;
    }
    let refresh = (cache.checked_at == 0 || now.saturating_sub(cache.checked_at) >= DAY)
        && (cache.attempted_at == 0 || now.saturating_sub(cache.attempted_at) >= HOUR);
    if show.is_none() && !refresh {
        return;
    }
    if refresh {
        cache.attempted_at = now;
    }
    if write_cache(&mut file, &cache).is_none() {
        return;
    }
    drop(file);
    if let Some(target) = show {
        let _ = writeln!(std::io::stderr(), "Cadence update available: {current} → {target}. Run cadence-client update --check to review it.");
    }
    if refresh {
        if let Ok(exe) = std::env::current_exe() {
            let mut command = Command::new(exe);
            command
                .arg(REFRESH_ARG)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let _ = cadence_agent::reaper::spawn(&mut command);
        }
    }
}

/// Internal detached process; its only side effect is refreshing the advisory cache.
pub(super) fn refresh() {
    if !allowed() {
        return;
    }
    let Some((prefix, current)) = client_update::notice_install() else {
        return;
    };
    let Ok(target) = client_update::latest_tag(None) else {
        return;
    };
    let (Some(now), Some(mut file)) = (now(), cache_file(&prefix)) else {
        return;
    };
    let mut cache = read_cache(&mut file, &current);
    cache.target = Some(target);
    cache.checked_at = now;
    let _ = write_cache(&mut file, &cache);
}
