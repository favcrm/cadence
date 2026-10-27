//! CAD-1759 independent refusal acceptance: a stale pause writer must not
//! overwrite a longer pause committed by another process. Written by
//! cc13-rev-sol, not the guard implementer. Install unchanged as this new
//! file and register it with `#[cfg(test)] mod
//! cad1759_pause_race_refusal_acceptance;` in `gh_guard.rs`.
//!
//! A child calls the real `record_pause`, reading old JSON through a FIFO.
//! Keeping the FIFO writer open parks that read before EOF. The parent
//! probes the real sidecar's exclusive lock, without waiting for it:
//! - unlocked implementation: commit the longer pause, then release the
//!   stale child; its later rename must NOT shorten that committed pause;
//! - locked implementation: insertion is refused while the child reads;
//!   release/finish that writer, then commit the longer pause.
//!
//! Finally another real short write must retain the committed longer one.
//! The kernel/FIFO handshakes select the ordering, not timing sleeps.
//!
//! Covers the successful-lock path, across processes. It does not approve
//! or exercise the separate lock-timeout/open-failure fallback policy.
//! No daemon, gh executable, credentials, global binding or fake guard.

use super::{now, record_pause};
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const CHILD_DIR: &str = "CAD1759_PAUSE_RACE_CHILD_DIR";
const TEST: &str = "cad1759_committed_longer_pause_refuses_stale_writer";
const FIXTURE_TIMEOUT: Duration = Duration::from_secs(60);

/// Only the test's own child is ever waited on or killed.
struct OwnChild(Option<Child>);

impl OwnChild {
    fn wait(mut self) -> Output {
        let deadline = Instant::now() + FIXTURE_TIMEOUT;
        loop {
            if self.0.as_mut().unwrap().try_wait().unwrap().is_some() {
                return self.0.take().unwrap().wait_with_output().unwrap();
            }
            assert!(Instant::now() < deadline, "short writer did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

impl Drop for OwnChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}

fn connect_snapshot(file: &Path, child: &mut OwnChild) -> File {
    let deadline = Instant::now() + FIXTURE_TIMEOUT;
    loop {
        match OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(file)
        {
            Ok(writer) => return writer,
            Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {}
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => panic!("opening fixture FIFO: {error}"),
        }
        assert!(
            child.0.as_mut().unwrap().try_wait().unwrap().is_none(),
            "short writer exited before opening its snapshot"
        );
        assert!(Instant::now() < deadline, "short writer never opened FIFO");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn writer_holds_sidecar(dir: &Path) -> bool {
    let observer = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("gh-rate-limit.json.lock"))
        .unwrap();
    loop {
        // SAFETY: observer owns this live descriptor. Closing it releases
        // a successful probe; no production lock or process is touched.
        if unsafe { libc::flock(observer.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return false;
        }
        match std::io::Error::last_os_error().kind() {
            ErrorKind::WouldBlock => return true,
            ErrorKind::Interrupted => continue,
            error => panic!("exclusive sidecar probe failed: {error:?}"),
        }
    }
}

fn committed_until(dir: &Path) -> i64 {
    let bytes = std::fs::read(dir.join("gh-rate-limit.json")).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("pause JSON was torn");
    json["until"]
        .as_i64()
        .expect("pause deadline is an integer")
}

#[test]
fn cad1759_committed_longer_pause_refuses_stale_writer() {
    // Re-enter this ONE test in a child, so an in-process mutex cannot
    // masquerade as board/daemon serialization. Never bind global state.
    if let Some(dir) = std::env::var_os(CHILD_DIR) {
        let dir = std::path::PathBuf::from(dir);
        assert!(dir.starts_with(std::env::temp_dir()));
        let returned = record_pause(&dir, Some(now() + 90));
        println!("SHORT_WRITER_RETURNED={returned}");
        return;
    }

    let root = tempfile::Builder::new()
        .prefix("c1759-race-")
        .tempdir_in(std::env::temp_dir())
        .unwrap();
    let dir = root.path();
    let file = dir.join("gh-rate-limit.json");
    let fifo = CString::new(file.as_os_str().as_bytes()).unwrap();
    // SAFETY: fifo is a live NUL-terminated path in this owned temp dir.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let module = module_path!().split_once("::").unwrap().1;
    let mut short = OwnChild(Some(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("{module}::{TEST}"),
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(CHILD_DIR, dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    let mut snapshot = connect_snapshot(&file, &mut short);
    snapshot.write_all(br#"{"until":0}"#).unwrap();
    // The child retains the old FIFO inode. A subsequent real writer
    // publishes a regular file at this pathname, just as rename does.
    std::fs::remove_file(&file).unwrap();
    let held = writer_holds_sidecar(dir);
    let requested = now() + 3000; // Both deadlines are inside the 65-minute ceiling.
    let (longer, output) = if held {
        println!("SIDECAR_INSERTION_REFUSED=true");
        // Do not manufacture a >2s lock timeout by waiting for a writer
        // behind the deliberately parked child. Finish its transaction.
        drop(snapshot);
        let output = short.wait();
        (record_pause(dir, Some(requested)), output)
    } else {
        println!("SIDECAR_INSERTION_REFUSED=false");
        let longer = record_pause(dir, Some(requested));
        assert_eq!(committed_until(dir), longer);
        println!("LONGER_COMMITTED_BEFORE_STALE_WRITER={longer}");
        drop(snapshot);
        (longer, short.wait())
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("SHORT_WRITER_RETURNED="),
        "child did not execute/succeed: {:?}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    print!("{stdout}");
    let published = committed_until(dir);
    println!("LONGER={longer} FINAL_VALID_JSON_UNTIL={published}");
    assert!(longer >= requested, "long writer did not publish its reset");
    assert!(
        published >= longer,
        "committed longer pause was shortened by {} seconds: longer={longer}, final={published}",
        longer - published
    );
    assert!(held, "writer did not hold the cross-process sidecar lock");
    assert!(record_pause(dir, Some(now() + 90)) >= longer);
    assert!(committed_until(dir) >= longer);
}
