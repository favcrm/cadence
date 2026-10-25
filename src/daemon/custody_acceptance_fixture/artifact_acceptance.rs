// ONE consolidated encrypted-artifact acceptance check — SELF-CONTAINED.
// Uses the REAL `custody_acceptance_fixture::Rig` (a real daemon + real
// `custody-encrypted.json` + real 32-byte key + real `vault/custody.sqlite3`)
// and the REAL `crate::backup::{backup,export}` entry points.
//
// UNCOMPILED/UNEXECUTED. Mount as a NEW FILE
// `src/daemon/custody_acceptance_fixture/artifact_acceptance.rs` and add
// `mod artifact_acceptance;` to `src/daemon/custody_acceptance_fixture.rs`
// (after `mod authority5;`). Because `authority5`'s helpers are private to
// its own submodule (`fn`, not `pub(super)`), this file brings its OWN
// owner-private copies — adapted from authority5, never editing or
// depending on authority5's internals. `Rig`, `create`, `setup_rpc`, `must`,
// `digest`, `A`, `B` come from `super::*` (custody_acceptance_fixture.rs
// scope), where `setup_rpc`/`create`/`must`/`digest` ARE defined at that
// level — NOT inside `authority5`.
//
// REV 2 fixes the writer-flagged defects:
//   * key path is NOT assumed `<root>/keys/key` — it is read from the real
//     `custody-encrypted.json` `key_file` field (the owner record), matching
//     the fixture's actual provisioned location;
//   * `file_hash`/`tree_hashes` handle a FILE correctly (no `read_dir` on a
//     file) — `file_hash` hashes bytes, `tree_hashes` walks a directory;
//   * watcher calibration drains BEFORE the physical-baseline snapshot and
//     the artifact path, so the counterexample's drain is clean;
//   * every helper is defined here — `Rig` is `pub` (in
//     `custody_acceptance_fixture.rs`); `create`/`setup_rpc`/`must`/`digest`/
//     `A`/`B` are `fn` inside `authority5` and NOT reachable via `super::*`
//     from a sibling, so this file carries its own copies.
//
// Helpers below are owner-private to this file — adapted from authority5,
// never editing or depending on authority5's internals.

use super::Rig;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

// ---------- owner-private helpers (self-contained, never exported) ----------

const A: &str = "q7k9m2x4v8b1n6p3z5c0a4w7e2r9t6y5u8i1o3p7l4k";
const B: &str = "h3f5d7s9a2g4j6k8l1q3w5e7r9t2y4u6i8o1p3z5x7v";
const WAIT: Duration = Duration::from_secs(15);

fn must<T, E>(r: std::result::Result<T, E>, stage: &'static str) -> T {
    r.unwrap_or_else(|_| panic!("{stage}"))
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The SMTP enroll payload — adapted from authority5::create. The rig's
/// `smtp_port` is the live fixture listener; the record lands custody:file
/// until the encrypted manager owns it (enrolled encrypted under
/// `connection_custody_unlock` here).
fn create(rig: &Rig, account: &str, secret: &str) -> Value {
    json!({"provider":"smtp","account":account,"shape":"smtp","host":"localhost",
        "port":rig.smtp_port,"tls_mode":"implicit","username":"fixture-user","secret":secret,
        "sender":"sender@example.test","scopes":["email:send"],"accept_same_uid_risk":true})
}

/// Explicit scoped operator setup — adapted from authority5::setup_rpc.
/// Native requests never call this; it is the oracle/setup path only.
fn setup_rpc(rig: &Rig, method: &str, params: Value) -> Value {
    must(
        crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
            crate::client::rpc_private_timeout(&rig.state_dir, method, params, WAIT)
        }),
        "synthetic authority setup/control failed",
    )
}

/// The real key path, read from the owner-private `custody-encrypted.json`
/// `key_file` field — never assumed `<root>/keys/key`.
fn configured_key_path(state_dir: &Path) -> PathBuf {
    let rec = std::fs::read_to_string(state_dir.join("custody-encrypted.json"))
        .expect("custody-encrypted.json missing on encrypted rig");
    let v: serde_json::Value = serde_json::from_str(&rec).expect("record is not json");
    let k = v["key_file"].as_str().expect("key_file absent from record");
    PathBuf::from(k)
}

/// Native inotify watcher on the real configured key (and separately the
/// vault db). Adapted from authority5's KeyWatch — private to this file.
/// Calibrated: `drain` asserts WouldBlock, IN_Q_OVERFLOW/IN_IGNORED are hard
/// failures (lost observation ≠ absence of access).
struct KeyWatch(OwnedFd);
impl KeyWatch {
    fn on(path: &Path) -> Self {
        let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        assert!(raw >= 0, "inotify_init1 unavailable");
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let wd = unsafe {
            libc::inotify_add_watch(
                fd.as_raw_fd(),
                c.as_ptr(),
                libc::IN_OPEN
                    | libc::IN_ACCESS
                    | libc::IN_MODIFY
                    | libc::IN_ATTRIB
                    | libc::IN_DELETE_SELF
                    | libc::IN_MOVE_SELF,
            )
        };
        assert!(wd >= 0, "inotify_add_watch failed on {}", path.display());
        Self(fd)
    }
    /// Drain pending events; the event count. WouldBlock → 0; a queue
    /// overflow or dropped watch fails (not a silent "no access").
    fn drain(&self) -> usize {
        let mut total = 0;
        loop {
            let mut buf = [0u8; 8192];
            let n = unsafe { libc::read(self.0.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                assert_eq!(
                    std::io::Error::last_os_error().kind(),
                    std::io::ErrorKind::WouldBlock,
                    "key watcher read failed"
                );
                return total;
            }
            assert!(n > 0, "key watcher unexpectedly closed");
            let mut off = 0usize;
            while off + std::mem::size_of::<libc::inotify_event>() <= n as usize {
                let ev = unsafe {
                    std::ptr::read_unaligned(buf.as_ptr().add(off).cast::<libc::inotify_event>())
                };
                assert_eq!(
                    ev.mask & (libc::IN_Q_OVERFLOW | libc::IN_IGNORED),
                    0,
                    "key watcher lost observation capacity"
                );
                off += std::mem::size_of::<libc::inotify_event>() + ev.len as usize;
                total += 1;
            }
            assert_eq!(off, n as usize, "malformed inotify event");
        }
    }
    /// Prove the watch is armed: a real open+read of the key fires. The
    /// watch then drains clean so the counterexample's silence is real.
    fn calibrate(&self, key: &Path) {
        assert_eq!(self.drain(), 0, "watch dirty before calibration");
        use std::io::Read;
        let mut f = std::fs::File::open(key).expect("calibration key unreadable");
        let mut buf = [0u8; 32];
        f.read_exact(&mut buf).expect("calibration key read failed");
        assert!(self.drain() > 0, "watch did not observe a real key access");
    }
}

/// sha256 of a single file's bytes — `file_hash`, NOT a directory walk.
fn file_hash(path: &Path) -> String {
    digest(&std::fs::read(path).expect("file_hash read failed"))
}

/// sha256 of every file under `dir` → (relpath → hex). The physical
/// unchanged proof for the vault tree. `dir` must be a directory; the key
/// is hashed via `file_hash`, not this.
fn tree_hashes(dir: &Path, base: &Path, out: &mut BTreeMap<PathBuf, String>) {
    if !dir.is_dir() {
        return;
    }
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            tree_hashes(&p, base, out);
        } else if p.is_file() {
            let rel = p.strip_prefix(base).unwrap_or(&p).to_path_buf();
            out.insert(rel, file_hash(&p));
        }
    }
}

/// Byte-window substring search over every produced file — catches a secret
/// embedded mid-file, not whole-file equality.
fn contains_bytes(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || hay.len() < needle.len() {
        return false;
    }
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Recursively collect (relpath, bytes) under `dir` for the absence scan.
fn bundle(dir: &Path, base: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
    if !dir.is_dir() {
        return;
    }
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            bundle(&p, base, out);
        } else if p.is_file() {
            let rel = p.strip_prefix(base).unwrap_or(&p).to_path_buf();
            out.push((rel, std::fs::read(&p).unwrap()));
        }
    }
}

// =====================================================================
// The consolidated check.
// =====================================================================
#[test]
fn encrypted_artifact_exclusion_and_key_isolation() {
    // ---- real rig ----
    let root = must(
        tempfile::Builder::new()
            .prefix("enc-art-")
            .tempdir_in(std::env::temp_dir()),
        "private isolated root failed",
    );
    must(
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)),
        "fixture root mode failed",
    );
    let rig = must(
        Rig::start(root.path(), A.as_bytes(), B.as_bytes()),
        "real encrypted rig failed",
    );

    // ---- the real configured key: read from the owner record, never assumed ----
    let key_path = configured_key_path(&rig.state_dir);
    assert!(key_path.is_file(), "configured key absent at {key_path:?}");
    let key_bytes = std::fs::read(&key_path).unwrap();
    assert_eq!(key_bytes.len(), 32, "configured key is not 32 bytes");
    let key_parent = key_path.parent().unwrap().to_path_buf();
    let vault_db = rig.state_dir.join("vault").join("custody.sqlite3");

    // ---- init → unlock → enroll real ciphertext → lock ----
    setup_rpc(
        &rig,
        "connection_custody_initialize",
        json!({"accept_same_uid_risk":true}),
    );
    setup_rpc(
        &rig,
        "connection_custody_unlock",
        json!({"accept_same_uid_risk":true}),
    );
    let created = setup_rpc(&rig, "connection_create", create(&rig, "mail-acct", A));
    let id = created["connection"]["id"].as_str().unwrap().to_string();
    assert!(
        must(rig.core_record(&id), "core record absent")
            .custody
            .starts_with("encrypted-sqlite-v1:"),
        "enrollment did not produce an encrypted reference"
    );
    assert!(vault_db.is_file(), "vault db absent after enroll");
    let secret_a = A.as_bytes();
    let secret_b = B.as_bytes();
    setup_rpc(&rig, "connection_custody_lock", json!({}));

    // ---- physical baselines (after lock, before artifacts) ----
    let key_hash_before = file_hash(&key_path);
    let mut vault_before = BTreeMap::new();
    tree_hashes(
        &rig.state_dir.join("vault"),
        &rig.state_dir,
        &mut vault_before,
    );

    // ---- watchers on key + vault db; calibrate BEFORE the artifact path ----
    let watch = KeyWatch::on(&key_path);
    let vault_watch = KeyWatch::on(&vault_db);
    watch.calibrate(&key_path); // armed proof + clean drain
    vault_watch.calibrate(&vault_db);

    // ---- fresh key-parent CHILD bundle SUCCEEDS (disjoint child admission) ----
    let fresh_child = key_parent.join("artifact-export");
    let out_result = crate::backup::export(&rig.state_dir, &fresh_child);
    assert!(
        out_result.is_ok(),
        "fresh key-parent child export must succeed"
    );

    // ---- produced export: byte scan ----
    let mut files = Vec::new();
    bundle(&fresh_child, &fresh_child, &mut files);
    assert!(!files.is_empty(), "export produced no files");
    for (rel, bytes) in &files {
        let name = rel.file_name().unwrap().to_string_lossy();
        assert!(
            !name.contains("custody-encrypted.json"),
            "config leaked: {name}"
        );
        assert!(!name.contains("custody.sqlite3"), "vault db leaked: {name}");
        assert!(
            !name.ends_with("-wal") && !name.ends_with("-shm"),
            "vault journal leaked: {name}"
        );
        assert!(
            !contains_bytes(bytes, &key_bytes),
            "key bytes leaked into {name}"
        );
        assert!(
            !contains_bytes(bytes, secret_a),
            "SMTP secret A leaked into {name}"
        );
        assert!(
            !contains_bytes(bytes, secret_b),
            "SMTP secret B leaked into {name}"
        );
    }

    // ---- produced backup into a disjoint dir: same exclusion ----
    let bak_out = root.path().join("bak-out");
    std::fs::create_dir_all(&bak_out).unwrap();
    std::fs::set_permissions(&bak_out, std::fs::Permissions::from_mode(0o700)).unwrap();
    crate::backup::backup(&rig.state_dir, &bak_out, 3, "pre")
        .expect("backup of configured state must succeed");
    let mut bfiles = Vec::new();
    bundle(&bak_out, &bak_out, &mut bfiles);
    assert!(!bfiles.is_empty(), "backup produced no files");
    for (rel, bytes) in &bfiles {
        let name = rel.file_name().unwrap().to_string_lossy();
        assert!(
            !name.contains("custody-encrypted.json"),
            "config leaked: {name}"
        );
        assert!(!name.contains("custody.sqlite3"), "vault db leaked: {name}");
        assert!(
            !name.ends_with("-wal") && !name.ends_with("-shm"),
            "vault journal leaked: {name}"
        );
        assert!(
            !contains_bytes(bytes, &key_bytes),
            "key bytes leaked into {name}"
        );
        assert!(
            !contains_bytes(bytes, secret_a),
            "SMTP secret A leaked into {name}"
        );
        assert!(
            !contains_bytes(bytes, secret_b),
            "SMTP secret B leaked into {name}"
        );
    }

    // ---- watchers: no open/read of key or vault db through the whole path ----
    assert_eq!(watch.drain(), 0, "artifact path opened or read the key");
    assert_eq!(
        vault_watch.drain(),
        0,
        "artifact path opened or read the vault db"
    );

    // ---- physical unchanged: key file hash + vault tree identical ----
    assert_eq!(
        file_hash(&key_path),
        key_hash_before,
        "key file changed under artifact"
    );
    let mut vault_after = BTreeMap::new();
    tree_hashes(
        &rig.state_dir.join("vault"),
        &rig.state_dir,
        &mut vault_after,
    );
    assert_eq!(
        vault_before, vault_after,
        "vault tree changed under artifact"
    );

    // ---- REUSED key-parent output refuses (the key must not be inside a
    //      reusable artifact root; distinct from the fresh disjoint child) ----
    let reused = crate::backup::export(&rig.state_dir, &key_parent);
    assert!(reused.is_err(), "reused key-parent output must refuse");

    // ---- absent-config on encrypted-referenced state refuses with ZERO
    //      namespace/prune effect ----
    let rec = rig.state_dir.join("custody-encrypted.json");
    let rec_bytes = std::fs::read(&rec).unwrap();
    std::fs::remove_file(&rec).unwrap();
    let absent_out = root.path().join("absent-out");
    std::fs::create_dir_all(&absent_out).unwrap();
    let before_n = std::fs::read_dir(&absent_out).unwrap().count();
    let denied = crate::backup::backup(&rig.state_dir, &absent_out, 3, "pre");
    assert!(
        denied.is_err(),
        "absent-config on encrypted state must refuse"
    );
    let after_n = std::fs::read_dir(&absent_out).unwrap().count();
    assert_eq!(
        after_n, before_n,
        "absent-config refusal left namespace files behind"
    );
    std::fs::write(&rec, &rec_bytes).unwrap();
    std::fs::set_permissions(&rec, std::fs::Permissions::from_mode(0o600)).unwrap();

    must(rig.finish(), "real rig cleanup failed");
}
