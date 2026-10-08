//! CAD-1190 independent acceptance check (written by the reviewer, not the
//! implementer; AGENTS.md "Gates and security work").
//!
//! The cheaper acquire snapshot must not weaken CAD-852/CAD-1167 crash
//! detection: after a writer dies, EVERY file it left behind must still
//! refuse a scoped write that declares it, however many files the crash
//! left and whether they sit in a new untracked directory or inside one
//! that was already untracked (foreign) when the snapshot was taken.
use super::*;

const LEFT: usize = 150;

fn tracker() -> (tempfile::TempDir, Pm) {
    let dir = tempfile::tempdir().unwrap();
    let pm = Pm::init(&dir.path().join("pm")).unwrap();
    (dir, pm)
}

/// Take the lock, write `files` while holding it, then die without
/// releasing the fence (a crashed writer).
fn crash_writing(pm: &Pm, files: &[String]) {
    let mut lock = pm.lock().unwrap();
    std::thread::sleep(Duration::from_millis(30));
    for f in files {
        let p = pm.dir.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "half-written\n").unwrap();
    }
    lock.armed = false;
    drop(lock);
}

fn assert_every_leftover_refuses(pm: &Pm, files: &[String]) {
    let admitted: Vec<&String> = files
        .iter()
        .filter(|f| match pm.lock_for_paths(&[pm.dir.join(f)]) {
            Ok(lock) => {
                drop(lock);
                true
            }
            Err(_) => false,
        })
        .collect();
    assert!(
        admitted.is_empty(),
        "{} of {} crash leftovers were admitted to a scoped write that declares them, e.g. {:?}",
        admitted.len(),
        files.len(),
        admitted.iter().take(3).collect::<Vec<_>>()
    );
}

#[test]
fn cad1190_every_leftover_in_a_new_untracked_dir_refuses_its_scoped_write() {
    let (_d, pm) = tracker();
    let files: Vec<String> = (0..LEFT).map(|i| format!("crashed/f{i:03}.md")).collect();
    crash_writing(&pm, &files);
    assert_every_leftover_refuses(&pm, &files);
}

#[test]
fn cad1190_every_leftover_inside_a_foreign_untracked_dir_refuses_its_scoped_write() {
    let (_d, pm) = tracker();
    std::fs::create_dir_all(pm.dir.join("foreign")).unwrap();
    std::fs::write(pm.dir.join("foreign/old.txt"), "operator's own\n").unwrap();
    // A clean write records `foreign/` in the snapshot.
    drop(pm.lock().unwrap());
    std::thread::sleep(Duration::from_millis(30));
    let files: Vec<String> = (0..LEFT).map(|i| format!("foreign/new{i:03}.md")).collect();
    crash_writing(&pm, &files);
    assert_every_leftover_refuses(&pm, &files);
}
