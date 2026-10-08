//! CAD-1189: the apps-only flock. Catalog writers (install, upgrade,
//! recover, migrate) and runtime admission take it; ticket writers never
//! do. Lock order is apps lock, then PM lock. Same kernel-flock style as
//! `pmlock`: it dies with its process and never goes stale.
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::issue::pmlock::Unlock;
use crate::issue::Pm;

const FILE: &str = "cadence-apps.flock";
const WAIT: Duration = Duration::from_secs(15);

pub(super) fn acquire(pm: &Pm) -> Result<Unlock> {
    let dot = pm.dir.join(".git");
    let git_dir = if dot.is_dir() {
        dot
    } else {
        crate::issue::hooks::git_dir(&pm.dir)
            .ok_or_else(|| Error::rejected("the PM directory is not a git repository"))?
    };
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(git_dir.join(FILE))?;
    if !file.metadata()?.is_file() {
        return Err(Error::rejected("the apps lock is not a regular file"));
    }
    let deadline = Instant::now() + WAIT;
    loop {
        // SAFETY: a valid descriptor owned by `file`.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Unlock(file));
        }
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(e.into());
        }
        if Instant::now() >= deadline {
            return Err(Error::busy(
                "another app catalog writer or runtime admission holds .git/cadence-apps.flock",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
