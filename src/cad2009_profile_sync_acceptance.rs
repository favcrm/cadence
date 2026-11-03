//! CAD-2009: native profile sync must distinguish partial effects from refusals.

use super::{create_catalogue_profile, export_path, snapshot, sync, GEN_MARKER};
use crate::issue::Pm;
use crate::store::Store;
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

struct Fixture {
    pm: Pm,
    store: Store,
    expected: String,
    pi: PathBuf,
    claude: PathBuf,
    fresh_pi: Vec<u8>,
    stale_pi: Vec<u8>,
    stale_claude: Vec<u8>,
    _dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::TempDir::new().unwrap();
        let pm_dir = dir.path().join("pm");
        fs::create_dir(&pm_dir).unwrap();
        fs::write(pm_dir.join("pm.yaml"), "schema: 1\n").unwrap();
        let pm = Pm::at(&pm_dir).unwrap();
        let store = Store::open(&dir.path().join("t.sqlite3")).unwrap();
        let role = crate::agent_roles::find("developer").unwrap();
        create_catalogue_profile(&store, "dev-1", role, role.look, None, None).unwrap();
        let expected = snapshot(&pm.dir, &store, &[]).unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_string();

        let generated = sync(&pm, &store, &[], &expected, Some("dev-1")).unwrap();
        let files = generated["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|file| file["status"] == "in sync"));
        let pi = export_path(&pm.dir, "pi", "dev-1");
        let claude = export_path(&pm.dir, "claude", "dev-1");
        let (fresh_pi, stale_pi) = make_stale(&pi);
        let (_, stale_claude) = make_stale(&claude);

        Self {
            pm,
            store,
            expected,
            pi,
            claude,
            fresh_pi,
            stale_pi,
            stale_claude,
            _dir: dir,
        }
    }
}

fn make_stale(path: &Path) -> (Vec<u8>, Vec<u8>) {
    let fresh = fs::read(path).unwrap();
    let marker = format!("---\n# {GEN_MARKER} ");
    assert!(fresh.starts_with(marker.as_bytes()));
    let mut stale = fresh.clone();
    stale.extend_from_slice(b"\nStale generated export body for CAD-2009 acceptance.\n");
    fs::write(path, &stale).unwrap();
    assert!(stale.starts_with(marker.as_bytes()));
    assert_ne!(stale, fresh);
    (fresh, stale)
}

fn lock_export(path: &Path) -> File {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    // A separately opened descriptor conflicts with sync's own flock even in
    // this process; holding the File makes the contention deterministic.
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0,
        "could not lock {}: {}",
        path.display(),
        std::io::Error::last_os_error()
    );
    lock
}

#[test]
fn cad2009_profile_sync_partial_export_effect_is_unknown_across_rpc() {
    let fixture = Fixture::new();
    let lock = lock_export(&fixture.claude);

    let error = sync(
        &fixture.pm,
        &fixture.store,
        &[],
        &fixture.expected,
        Some("dev-1"),
    )
    .unwrap_err();

    let pi_after = fs::read(&fixture.pi).unwrap();
    assert_ne!(
        pi_after, fixture.stale_pi,
        "the earlier Pi write must occur"
    );
    assert_eq!(
        pi_after, fixture.fresh_pi,
        "Pi must contain the actual regenerated export"
    );
    assert_eq!(
        fs::read(&fixture.claude).unwrap(),
        fixture.stale_claude,
        "the locked Claude export must remain untouched"
    );
    assert_eq!(error.kind(), "unknown", "{error}");
    let guidance = error.to_string().to_ascii_lowercase();
    assert!(
        guidance.contains("settings agents show")
            || ((guidance.contains("inspect") || guidance.contains("reconcil"))
                && (guidance.contains("export") || guidance.contains("profile"))),
        "partial sync must advise how to inspect or reconcile its effects: {error}"
    );

    let encoded = serde_json::to_vec(&crate::proto::err(&error)).unwrap();
    let frame = serde_json::from_slice(&encoded).unwrap();
    let wire_error = crate::proto::unwrap(frame).unwrap_err();
    assert_eq!(wire_error.kind(), "unknown", "{wire_error}");
    assert_eq!(
        wire_error.to_string(),
        error.to_string(),
        "RPC must retain reconciliation guidance"
    );
    drop(lock);
}

#[test]
fn cad2009_profile_sync_first_export_lock_is_a_pre_effect_rejection() {
    let fixture = Fixture::new();
    let lock = lock_export(&fixture.pi);

    let error = sync(
        &fixture.pm,
        &fixture.store,
        &[],
        &fixture.expected,
        Some("dev-1"),
    )
    .unwrap_err();

    assert_eq!(fs::read(&fixture.pi).unwrap(), fixture.stale_pi);
    assert_eq!(fs::read(&fixture.claude).unwrap(), fixture.stale_claude);
    assert_eq!(error.kind(), "rejected", "{error}");
    assert!(
        error.to_string().contains("busy"),
        "expected the held Pi lock to refuse sync: {error}"
    );
    let wire_error = crate::proto::unwrap(crate::proto::err(&error)).unwrap_err();
    assert_eq!(wire_error.kind(), "rejected", "{wire_error}");
    drop(lock);
}
