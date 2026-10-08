//! CAD-1191: a test seam marking the points where `issue sync`, `start`
//! and `finish` do network or worktree work. Production builds compile
//! the call to nothing; a test installs a callback that asserts the PM
//! write lock is free at that point.

#[cfg(test)]
use std::cell::RefCell;

#[cfg(test)]
type Hook = Box<dyn Fn(&str)>;

#[cfg(test)]
thread_local! {
    static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
}

/// Called immediately before slow work that must run without the PM lock.
#[inline]
pub(crate) fn outside_lock(_point: &str) {
    #[cfg(test)]
    HOOK.with(|h| {
        if let Some(f) = h.borrow().as_ref() {
            f(_point);
        }
    });
}

/// Install (or clear) the callback for this thread.
#[cfg(test)]
pub(crate) fn set_hook(hook: Option<Hook>) {
    HOOK.with(|h| *h.borrow_mut() = hook);
}

/// CAD-1191: the PM lock is free while `issue sync` fetches and pushes,
/// `issue start` builds the worktree and `issue finish` removes it.
#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::rc::Rc;

    use super::set_hook;
    use crate::issue::finish::{self, FinishArgs};
    use crate::issue::start::{self, StartArgs};
    use crate::issue::write::{new_issue, project_add};
    use crate::issue::{git, sync, Pm};

    fn sh(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    /// Installs a hook that records each seam point and asserts the PM
    /// lock can be taken (then released) right there.
    fn assert_lock_free_at(pm: &Pm) -> Rc<RefCell<Vec<String>>> {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let (log, dir) = (seen.clone(), pm.dir.clone());
        set_hook(Some(Box::new(move |point| {
            let pm = Pm::at(&dir).unwrap();
            let free = pm.try_lock().unwrap().is_some();
            assert!(free, "PM lock held during {point}");
            log.borrow_mut().push(point.to_string());
        })));
        seen
    }

    #[test]
    fn sync_fetches_and_pushes_without_the_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin.git");
        std::fs::create_dir(&origin).unwrap();
        sh(&origin, &["init", "--bare", "-b", "main"]);
        let pm = Pm::init(&tmp.path().join("pm")).unwrap();
        project_add(&pm, "cadence", "CAD", &[], &[], &[], None).unwrap();
        sh(&pm.dir, &["branch", "-M", "main"]);
        sh(
            &pm.dir,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        sh(&pm.dir, &["push", "origin", "main"]);
        new_issue(
            &pm,
            tmp.path(),
            Some("cadence"),
            "ahead",
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
        .unwrap();
        let seen = assert_lock_free_at(&pm);
        let out = sync::run(&pm, true, false, None).unwrap();
        set_hook(None);
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["pushed"], true, "{out}");
        assert_eq!(*seen.borrow(), ["sync-fetch", "sync-push"]);
    }

    /// A project with one code repo and one issue.
    fn lane_fixture(tmp: &Path) -> (Pm, PathBuf, String, PathBuf) {
        let code = tmp.join("code");
        std::fs::create_dir(&code).unwrap();
        sh(&code, &["init", "-b", "main"]);
        std::fs::write(code.join("f"), "one").unwrap();
        sh(&code, &["add", "f"]);
        sh(&code, &["commit", "-m", "one"]);
        let code = code.canonicalize().unwrap();
        let pm = Pm::init(&tmp.join("pm")).unwrap();
        project_add(
            &pm,
            "cadence",
            "CAD",
            &[code.to_string_lossy().into_owned()],
            &[],
            &[],
            None,
        )
        .unwrap();
        let id = new_issue(
            &pm,
            &code,
            Some("cadence"),
            "lane",
            None,
            None,
            &[],
            None,
            None,
            &[],
            None,
            Some("## Acceptance\n- [ ] x\n"),
            "t",
        )
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let state = tmp.join("state");
        std::fs::create_dir(&state).unwrap();
        (pm, code, id, state)
    }

    fn start_args(code: &Path) -> StartArgs {
        StartArgs {
            repo: Some(code.to_path_buf()),
            name: None,
            base: None,
            owner: None,
            job: None,
            by: Some("pm-a".into()),
            take_over: None,
        }
    }

    #[test]
    fn start_builds_the_worktree_without_the_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let (pm, code, id, state) = lane_fixture(tmp.path());
        let seen = assert_lock_free_at(&pm);
        let started = start::run(&pm, &id, &start_args(&code), "t", &state).unwrap();
        set_hook(None);
        assert!(PathBuf::from(started["worktree"].as_str().unwrap()).is_dir());
        assert_eq!(*seen.borrow(), ["start-worktree"]);
        // Lock 1 recorded the claim, lock 2 the refs: one commit each.
        let log = git(&pm.dir, &["log", "--format=%s", "-3"]).unwrap();
        assert!(log.contains(&format!("{id}: start ")), "{log}");
        assert!(log.contains("(claim)"), "{log}");
    }

    /// Finish with the checkout dir already gone closes refs only; both
    /// locks are still taken around an unlocked middle.
    #[test]
    fn finish_closes_refs_around_an_unlocked_middle() {
        let tmp = tempfile::tempdir().unwrap();
        let (pm, code, id, state) = lane_fixture(tmp.path());
        let started = start::run(&pm, &id, &start_args(&code), "t", &state).unwrap();
        let wt = PathBuf::from(started["worktree"].as_str().unwrap());
        std::fs::remove_dir_all(&wt).unwrap();
        sh(&code, &["worktree", "prune"]);
        let seen = assert_lock_free_at(&pm);
        let fargs = FinishArgs {
            force: false,
            keep_branch: false,
            remote: false,
            worktree: Some(&wt),
            close_if_gone: true,
        };
        let done = finish::run(&pm, &id, &fargs, "t", &state, None).unwrap();
        set_hook(None);
        assert_eq!(done["finished"], true, "{done}");
        assert_eq!(*seen.borrow(), ["finish-remove"]);
    }

    /// A claim taken over while the worktree is being built refuses the
    /// refs commit, names the claim, and leaves the lane to be reused.
    #[test]
    fn start_refuses_to_record_refs_when_the_claim_moved() {
        use crate::issue::write;
        let tmp = tempfile::tempdir().unwrap();
        let (pm, code, id, state) = lane_fixture(tmp.path());
        let (dir, rival_id) = (pm.dir.clone(), id.clone());
        set_hook(Some(Box::new(move |_| {
            let pm = Pm::at(&dir).unwrap();
            let _lock = pm.lock().unwrap();
            let (_, issue) = write::issue_dir(&pm, &rival_id).unwrap();
            let (mut front, body) = write::load_front(&issue).unwrap();
            front.claim.as_mut().unwrap().by = "pm-b".into();
            write::save_front(&issue, &front, &body).unwrap();
            pm.commit(&[issue.join("issue.md")], "rival takes over\n\nIssue: x\n")
                .unwrap();
        })));
        let err = start::run(&pm, &id, &start_args(&code), "t", &state).unwrap_err();
        set_hook(None);
        let msg = err.to_string();
        assert!(msg.contains("claim") && msg.contains("pm-b"), "{msg}");
        let (_, issue) = write::issue_dir(&pm, &id).unwrap();
        let (front, _) = write::load_front(&issue).unwrap();
        assert!(front.refs.iter().all(|r| r.kind != "worktree"), "{front:?}");
        // The lane survives: the new holder's re-run reuses it, no orphan.
        let mut again = start_args(&code);
        again.by = Some("pm-b".into());
        let out = start::run(&pm, &id, &again, "t", &state).unwrap();
        assert_eq!(out["created"], false, "{out}");
        let (front, _) = write::load_front(&issue).unwrap();
        assert_eq!(
            front.refs.iter().filter(|r| r.kind == "worktree").count(),
            1
        );
    }

    // CAD-1257: the checkup sweeps decide on lock-free snapshots.

    fn sweep_issue(pm: &Pm, dir: &Path, title: &str) -> String {
        new_issue(
            pm,
            dir,
            Some("cadence"),
            title,
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
            .to_string()
    }

    fn edit_front(pm: &Pm, id: &str, f: impl FnOnce(&mut crate::issue::model::Front)) {
        use crate::issue::write;
        let (_p, dir) = write::issue_dir(pm, id).unwrap();
        let (mut front, body) = write::load_front(&dir).unwrap();
        f(&mut front);
        write::save_front(&dir, &front, &body).unwrap();
    }

    /// A pass with nothing eligible never asks for the PM lock: it
    /// returns at once although another writer holds it.
    #[test]
    fn sweeps_with_nothing_eligible_take_no_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let pm = Pm::init(&tmp.path().join("pm")).unwrap();
        project_add(&pm, "cadence", "CAD", &[], &[], &[], None).unwrap();
        sweep_issue(&pm, tmp.path(), "fresh, unblocked");
        let _held = pm.lock().unwrap();
        let started = std::time::Instant::now();
        let g = crate::issue::groom::groom(
            &pm,
            None,
            crate::issue::groom::GROOM_GRACE_SECS,
            false,
            "t",
        )
        .unwrap();
        let b = crate::issue::blocked::sweep(
            &pm,
            crate::issue::blocked::PARK_GRACE_SECS,
            false,
            None,
            "t",
        )
        .unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(g["flagged"].as_array().unwrap().is_empty(), "{g}");
        assert!(b["parked"].as_array().unwrap().is_empty(), "{b}");
    }

    /// Groom loads the status clock and runs the product-repo `git log`
    /// with the PM lock free, then writes under the lock.
    #[test]
    fn groom_reads_history_without_the_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let code = tmp.path().join("code");
        std::fs::create_dir(&code).unwrap();
        sh(&code, &["init", "-b", "main"]);
        let pm = Pm::init(&tmp.path().join("pm")).unwrap();
        project_add(
            &pm,
            "cadence",
            "CAD",
            &[code.to_string_lossy().into_owned()],
            &[],
            &[],
            None,
        )
        .unwrap();
        let id = sweep_issue(&pm, tmp.path(), "old");
        edit_front(&pm, &id, |f| {
            f.paths = vec!["src.rs".into()];
            f.created = crate::issue::time::iso(crate::issue::time::now_epoch() - 30 * 86_400);
        });
        let seen = assert_lock_free_at(&pm);
        let out = crate::issue::groom::groom(
            &pm,
            None,
            crate::issue::groom::GROOM_GRACE_SECS,
            false,
            "t",
        )
        .unwrap();
        set_hook(None);
        assert_eq!(out["verdicts"][0]["id"], id, "{out}");
        assert_eq!(*seen.borrow(), ["groom-line-times", "groom-git-log"]);
        let log = git(&pm.dir, &["log", "--format=%s", "-1"]).unwrap();
        assert!(log.contains("groom"), "{log}");
    }

    /// The unblocked notice RPC goes out after the commit, with the PM
    /// lock free.
    #[test]
    fn blocked_notice_is_sent_after_commit_outside_the_lock() {
        use crate::issue::model::Claim;
        let tmp = tempfile::tempdir().unwrap();
        let pm = Pm::init(&tmp.path().join("pm")).unwrap();
        project_add(&pm, "cadence", "CAD", &[], &[], &[], None).unwrap();
        let a = sweep_issue(&pm, tmp.path(), "a");
        let b = sweep_issue(&pm, tmp.path(), "b");
        edit_front(&pm, &a, |f| {
            f.status = "backlog".into();
            f.blocked_by = vec![b.clone()];
            f.tags = vec![crate::issue::blocked::PARK_TAG.into()];
            f.claim = Some(Claim {
                by: "w1".into(),
                at: crate::issue::time::iso(crate::issue::time::now_epoch() - 96 * 3600),
                session: None,
                last_seen: None,
                note: None,
                stale: None,
            });
        });
        edit_front(&pm, &b, |f| f.status = "done".into());
        let state = tmp.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let seen = assert_lock_free_at(&pm);
        let out = crate::issue::blocked::sweep(
            &pm,
            crate::issue::blocked::PARK_GRACE_SECS,
            false,
            Some(&state),
            "t",
        )
        .unwrap();
        set_hook(None);
        assert_eq!(out["unblocked"][0]["id"], a, "{out}");
        assert_eq!(*seen.borrow(), ["blocked-notice"]);
        let log = git(&pm.dir, &["log", "--format=%s", "-1"]).unwrap();
        assert!(log.contains("unblocked"), "{log}");
    }
}
