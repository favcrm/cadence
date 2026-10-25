// Revised adversarial collision checks for the LANDED artifact
// namespace API — `src/backup/namespace.rs::ArtifactDir`.
// UNCOMPILED/UNEXECUTED. Mount inside `src/backup/namespace.rs`'s own
// `#[cfg(test)] mod tests` (it is `pub(super)`, so the tests must be
// crate-internal beside it — `use super::*` supplies ArtifactDir, File,
// OsStr, MetadataExt, Path, PathBuf, Result, Error).
//
// Each check plants a real foreign object (regular file, symlink or
// hardlink to an outside target) at a collision name and asserts the
// operation refuses AND the planted bytes/inode are unchanged — the
// asserted byte-identity/control outcomes are retained from the earlier
// draft. Every counterexample pairs a working positive control; no test
// asserts red from a setup or compile failure.
//
// What this exercises that the old `Reserved` sketch could not prove:
//   * create() registers the created inode in `owned` and validates
//     uid/mode/nlink — a foreign-inode replacement between stat and unlink
//     is caught by `same()`, not a bool flag;
//   * publish() is a real linkat no-replace on the pinned dir fd;
//   * Drop cleans ONLY `owned` names — never a prospective/globbed path;
//   * sqlite_sidecars() reserves journal names exclusively and refuses a
//     changed existing inode.

use std::ffi::OsStr;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

fn bytes_of(p: &std::path::Path) -> Vec<u8> {
    std::fs::read(p).unwrap_or_else(|_| Vec::new())
}

fn private_dir(root: &tempfile::TempDir, name: &str) -> PathBuf {
    let d = root.path().join(name);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
    d
}

// ---------- create(): exclusive + inode registration ----------

/// create() claims a fresh name exclusively and registers it; a second
/// create on the same name refuses — never truncates the first.
#[test]
fn create_is_exclusive_and_second_claim_refuses() {
    let root = tempfile::TempDir::new().unwrap();
    let dir = private_dir(&root, "out");
    let mut ns = ArtifactDir::open(&dir, false).unwrap();
    let f = ns.create(OsStr::new("a.txt")).unwrap();
    drop(f);
    // Second exclusive create on the same name refuses.
    assert!(ns.create(OsStr::new("a.txt")).is_err());
    // The file we made is still our content.
    assert_eq!(bytes_of(&dir.join("a.txt")), Vec::<u8>::new());
}

/// A planted regular file at the create name refuses and is left
/// byte-identical — the old `Reserved` could not prove this because its
/// `create` was path-based with no inode registration.
#[test]
fn create_refuses_planted_regular_file() {
    let root = tempfile::TempDir::new().unwrap();
    let dir = private_dir(&root, "out");
    let planted = dir.join("key");
    std::fs::write(&planted, b"PLANTED-KEY-BYTES").unwrap();
    let mut ns = ArtifactDir::open(&dir, false).unwrap();
    assert!(ns.create(OsStr::new("key")).is_err());
    assert_eq!(bytes_of(&planted), b"PLANTED-KEY-BYTES");
}

/// A planted symlink at the create name refuses without following it —
/// `O_NOFOLLOW` on the openat means the link target is never written.
#[test]
fn create_refuses_planted_symlink_without_following() {
    let root = tempfile::TempDir::new().unwrap();
    let dir = private_dir(&root, "out");
    let target = root.path().join("secret-target");
    std::fs::write(&target, b"REAL-SECRET").unwrap();
    std::os::unix::fs::symlink(&target, dir.join("key")).unwrap();
    let mut ns = ArtifactDir::open(&dir, false).unwrap();
    assert!(ns.create(OsStr::new("key")).is_err());
    // The symlink survives and its target is untouched.
    assert!(dir.join("key").symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(bytes_of(&target), b"REAL-SECRET");
}

// ---------- publish(): linkat no-replace + inode revalidation ----------

/// publish() links a created temp to a fresh final name; the final inode
/// is the same one we created, and the temp name is gone. Positive control.
#[test]
fn publish_links_temp_to_final_atomically() {
    let root = tempfile::TempDir::new().unwrap();
    let dir = private_dir(&root, "out");
    let mut ns = ArtifactDir::open(&dir, false).unwrap();
    {
        use std::io::Write;
        let mut f = ns.create(OsStr::new(".m.partial")).unwrap();
        f.write_all(b"manifest-body").unwrap();
    }
    ns.publish(OsStr::new(".m.partial"), OsStr::new("manifest.json")).unwrap();
    assert_eq!(bytes_of(&dir.join("manifest.json")), b"manifest-body");
    assert!(!dir.join(".m.partial").exists());
}

/// publish() refuses when the final name already holds a foreign object —
/// `linkat` fails EEXIST, and the planted inode (dev+ino) AND bytes are
/// provably unchanged, not merely equal content.
#[test]
fn publish_refuses_colliding_final_leaving_planted_bytes() {
    let root = tempfile::TempDir::new().unwrap();
    let dir = private_dir(&root, "out");
    let planted = dir.join("manifest.json");
    std::fs::write(&planted, b"PLANTED-FINAL").unwrap();
    let before = std::fs::symlink_metadata(&planted).unwrap();
    let mut ns = ArtifactDir::open(&dir, false).unwrap();
    {
        use std::io::Write;
        let mut f = ns.create(OsStr::new(".m.partial")).unwrap();
        f.write_all(b"ours").unwrap();
    }
    assert!(ns.publish(OsStr::new(".m.partial"), OsStr::new("manifest.json")).is_err());
    let after = std::fs::symlink_metadata(&planted).unwrap();
    assert_eq!(after.dev(), before.dev(), "planted inode dev changed");
    assert_eq!(after.ino(), before.ino(), "planted inode ino changed");
    assert_eq!(bytes_of(&planted), b"PLANTED-FINAL");
}

/// publish() refuses when the final name is a symlink — the link fails
/// EEXIST, the link and its target are untouched.
#[test]
fn publish_refuses_symlink_final() {
    let root = tempfile::TempDir::new().unwrap();
    let dir = private_dir(&root, "out");
    let target = root.path().join("key-real");
    std::fs::write(&target, b"KEY").unwrap();
    std::os::unix::fs::symlink(&target, dir.join("manifest.json")).unwrap();
    let mut ns = ArtifactDir::open(&dir, false).unwrap();
    {
        use std::io::Write;
        let mut f = ns.create(OsStr::new(".m.partial")).unwrap();
        f.write_all(b"ours").unwrap();
    }
    assert!(ns.publish(OsStr::new(".m.partial"), OsStr::new("manifest.json")).is_err());
    assert!(dir.join("manifest.json").symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(bytes_of(&target), b"KEY");
}

/// publish() refuses when the final name is a hardlink to an outside
/// sensitive file — it must not unlink/replace the link. The planted
/// inode's identity (dev+ino) AND its bytes are asserted unchanged after
/// the refused publish, so the shared-inode outcome is explicit, not
/// inferred from content alone.
#[test]
fn publish_refuses_hardlink_to_external_inode() {
    let root = tempfile::TempDir::new().unwrap();
    let dir = private_dir(&root, "out");
    let external = root.path().join("external-key");
    std::fs::write(&external, b"EXTERNAL-KEY-BYTES").unwrap();
    std::fs::hard_link(&external, dir.join("manifest.json")).unwrap();
    // Record the planted inode identity before the operation.
    let before = std::fs::symlink_metadata(dir.join("manifest.json")).unwrap();
    let ext_meta = std::fs::metadata(&external).unwrap();
    assert_eq!(before.dev(), ext_meta.dev(), "hardlink dev differs");
    assert_eq!(before.ino(), ext_meta.ino(), "hardlink ino differs");

    let mut ns = ArtifactDir::open(&dir, false).unwrap();
    {
        use std::io::Write;
        let mut f = ns.create(OsStr::new(".m.partial")).unwrap();
        f.write_all(b"ours").unwrap();
    }
    assert!(ns.publish(OsStr::new(".m.partial"), OsStr::new("manifest.json")).is_err());

    // The final path still names the SAME external inode — publish neither
    // unlinked nor replaced it — and its bytes hold.
    let after = std::fs::symlink_metadata(dir.join("manifest.json")).unwrap();
    assert_eq!(after.dev(), ext_meta.dev(), "final inode dev changed under publish");
    assert_eq!(after.ino(), ext_meta.ino(), "final inode ino changed under publish");
    assert_eq!(bytes_of(&external), b"EXTERNAL-KEY-BYTES");
    assert_eq!(bytes_of(&dir.join("manifest.json")), b"EXTERNAL-KEY-BYTES");
}

// ---------- foreign-inode replacement between create and publish ----------

/// If the temp inode is swapped out from under us (a same-UID adversary
/// unlinks our temp and plants a different inode at that name) `publish`
/// must refuse — `same()` revalidates dev+ino+uid+mode before linking.
/// This is the foreign-inode counterexample the bool-flag design missed.
#[test]
fn publish_refuses_swapped_temp_inode() {
    let root = tempfile::TempDir::new().unwrap();
    let dir = private_dir(&root, "out");
    let mut ns = ArtifactDir::open(&dir, false).unwrap();
    let original = dir.join(".m.partial");
    {
        use std::io::Write;
        let mut f = ns.create(OsStr::new(".m.partial")).unwrap();
        f.write_all(b"ours").unwrap();
    }
    // Same-UID swap: remove our temp, plant a different inode at the name.
    std::fs::remove_file(&original).unwrap();
    std::fs::write(&original, b"FOREIGN-REPLACEMENT").unwrap();
    assert!(ns.publish(OsStr::new(".m.partial"), OsStr::new("manifest.json")).is_err());
}

// ---------- sqlite_sidecars(): exclusive journal reservation ----------

/// sqlite_sidecars reserves -journal/-wal/-shm exclusively; a pre-existing
/// foreign file at a sidecar name refuses (a changed existing inode is a
/// conflict, never adopted).
#[test]
fn sqlite_sidecars_refuses_preexisting_foreign_journal() {
    let root = tempfile::TempDir::new().unwrap();
    let dir = private_dir(&root, "out");
    // Plant a foreign -wal where the snapshot will look.
    std::fs::write(dir.join("db.sqlite3-wal"), b"FOREIGN-WAL").unwrap();
    let mut ns = ArtifactDir::open(&dir, false).unwrap();
    assert!(ns.sqlite_sidecars(OsStr::new("db.sqlite3")).is_err());
    assert_eq!(bytes_of(&dir.join("db.sqlite3-wal")), b"FOREIGN-WAL");
}

/// Positive: on a clean dir, sqlite_sidecars reserves all three names
/// exclusively (a second call sees them as ours, not foreign).
#[test]
fn sqlite_sidecars_reserves_owned_journal_names() {
    let root = tempfile::TempDir::new().unwrap();
    let dir = private_dir(&root, "out");
    let mut ns = ArtifactDir::open(&dir, false).unwrap();
    ns.sqlite_sidecars(OsStr::new("db.sqlite3")).unwrap();
    for suf in ["-journal", "-wal", "-shm"] {
        assert!(dir.join(format!("db.sqlite3{suf}")).exists());
    }
    // Re-reserving our own names is fine (same inode re-validated).
    ns.sqlite_sidecars(OsStr::new("db.sqlite3")).unwrap();
}

// ---------- Drop: only owned names are cleaned ----------

/// Drop removes only names `create`/`publish` registered — a foreign
/// object at a prospective name (e.g. a `manifest.json` we never created)
/// is left byte-identical. No prospective-name or glob cleanup.
#[test]
fn drop_removes_only_owned_names() {
    let root = tempfile::TempDir::new().unwrap();
    let dir = private_dir(&root, "out");
    let foreign = dir.join("manifest.json");
    std::fs::write(&foreign, b"FOREIGN").unwrap();
    {
        let mut ns = ArtifactDir::open(&dir, false).unwrap();
        // We create (own) a temp but never publish; Drop must remove only it.
        let _f = ns.create(OsStr::new(".tmp.ours")).unwrap();
        // ns drops here uncommitted.
    }
    // Our owned temp is gone; the foreign file is untouched.
    assert!(!dir.join(".tmp.ours").exists());
    assert_eq!(bytes_of(&foreign), b"FOREIGN");
}

/// A foreign hardlink to one of our owned names does NOT cause our drop to
/// delete the underlying bytes the foreign link shares — `same()` +
/// unlinkat removes only the name we own; the external link survives.
#[test]
fn drop_does_not_follow_foreign_hardlink() {
    let root = tempfile::TempDir::new().unwrap();
    let dir = private_dir(&root, "out");
    let ext = root.path().join("ext");
    {
        let mut ns = ArtifactDir::open(&dir, false).unwrap();
        {
            use std::io::Write;
            let mut f = ns.create(OsStr::new("ours")).unwrap();
            f.write_all(b"OURS").unwrap();
        }
        // A same-UID adversary hardlinks our owned name to an external path.
        std::fs::hard_link(dir.join("ours"), &ext).unwrap();
    }
    // Drop removed our owned name; the external link still reads our bytes
    // (shared inode) but the namespace no longer carries it.
    assert!(!dir.join("ours").exists());
    assert_eq!(bytes_of(&ext), b"OURS");
}
