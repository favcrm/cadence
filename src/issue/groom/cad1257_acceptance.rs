//! CAD-1257 independent acceptance check (written by the reviewer, not the
//! implementer; AGENTS.md "Gates and security work").
//!
//! Groom now judges each candidate on a LOCK-FREE snapshot and takes the PM
//! lock only to write. A change to the candidate that lands after the
//! snapshot but before the lock must never be overwritten by the stale
//! verdict. Injected exactly in that window through the lane's own
//! `groom-line-times` seam (which fires after the snapshot is read and
//! before the lock is taken), so only the locked re-validation can catch it.
use super::*;
use crate::issue::write::{new_issue, project_add, save_front};
use std::cell::Cell;

fn edit(pm: &Pm, id: &str, f: impl FnOnce(&mut Front)) {
    let (_p, dir) = issue_dir(pm, id).unwrap();
    let (mut front, body) = load_front(&dir).unwrap();
    f(&mut front);
    save_front(&dir, &front, &body).unwrap();
}

#[test]
fn cad1257_a_change_after_the_snapshot_is_never_overwritten() {
    let tmp = tempfile::tempdir().unwrap();
    let pm = Pm::init(&tmp.path().join("pm")).unwrap();
    project_add(&pm, "cadence", "CAD", &[], &[], &[], None).unwrap();
    let id = new_issue(
        &pm,
        tmp.path(),
        Some("cadence"),
        "dormant backlog ticket",
        None,
        None,
        &[],
        None,
        None,
        &[],
        None,
        None,
        "t",
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    // Dormant: created well before the grace window, so it is eligible.
    edit(&pm, &id, |f| {
        f.paths = vec!["src.rs".to_string()];
        f.created = time::iso(time::now_epoch() - (GROOM_GRACE_SECS + 86_400));
    });

    // Once, right after the per-candidate snapshot and before the lock, a
    // human moves the ticket to doing (under its own PM lock).
    let fired = Cell::new(false);
    let (writer_pm, writer_id) = (Pm::at(&pm.dir).unwrap(), id.clone());
    crate::issue::lockseam::set_hook(Some(Box::new(move |point| {
        if point == "groom-line-times" && !fired.replace(true) {
            let _w = writer_pm.lock().unwrap();
            edit(&writer_pm, &writer_id, |f| f.status = "doing".to_string());
        }
    })));
    let out = groom_with_hooks(&pm, None, GROOM_GRACE_SECS, false, "t", || {}, || {});
    crate::issue::lockseam::set_hook(None);
    let out = out.unwrap();

    let (_p, dir) = issue_dir(&pm, &id).unwrap();
    let (front, _) = load_front(&dir).unwrap();
    assert_eq!(
        front.status, "doing",
        "the stale verdict overwrote a status changed after the snapshot: {out}"
    );
    assert!(
        front.last_groomed_at.is_none(),
        "a ticket that left backlog after the snapshot was stamped on the stale verdict: {out}"
    );
    assert!(
        !front.tags.iter().any(|t| t == TRIAGE_TAG),
        "a ticket that left backlog after the snapshot was flagged on the stale verdict: {out}"
    );
}
