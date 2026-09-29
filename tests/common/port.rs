use std::net::TcpListener;
use std::path::Path;

/// A board port in 3110-3199 — never production's 3010 — held for
/// the test's lifetime. Tests may run as separate processes (nextest)
/// or as threads (cargo test), so the lease is an exclusive `flock` on
/// `/tmp/cadence-test-ports/<port>.lock`: it excludes other threads and
/// other processes alike, and the kernel releases it when the test
/// ends, however it ends. The scan starts at a pid-derived offset so
/// concurrent processes rarely contend, and a port something else
/// already listens on is skipped. `sandbox up`'s free pick shares the
/// range; under `CADENCE_TEST_PORT_LOCK_DIR` (tests/sandbox.rs sets it
/// to this dir) it honours these leases and holds its own while its
/// board binds, so neither side can take the other's port mid-pick.
pub struct PortLease {
    pub port: u16,
    _lock: std::fs::File,
}

pub fn test_port() -> PortLease {
    use std::os::fd::AsRawFd;
    let dir = Path::new("/tmp/cadence-test-ports");
    std::fs::create_dir_all(dir).unwrap();
    let span = 90;
    let start = std::process::id() as usize * 31 % span;
    for i in 0..span {
        let port = 3110 + ((start + i) % span) as u16;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!("{port}.lock")))
            .unwrap();
        // SAFETY: plain syscall on a descriptor this function owns.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            continue;
        }
        // The file's contents name whose claim the holder protects —
        // clear whatever the previous holder left so a fenced port is
        // never mistaken for a sandbox's own claim.
        lock.set_len(0).unwrap();
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return PortLease { port, _lock: lock };
        }
    }
    panic!("no free port in 3110-3199");
}
