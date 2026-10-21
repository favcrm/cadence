//! The write side — the only writer for issue folders. Every op takes
//! the PM lock, mutates files (`issue.md` via temp+rename; comments and
//! artifacts create-only), then makes one git commit. The HTTP API in
//! `ui.rs` calls these same functions with the derived board caller —
//! `actor = "operator (ui)"`, or a pane's alias (CAD-254).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::model::{self, Front, Ref};
use crate::issue::{board, parse, project, time, Pm, PmLock};

mod done;
mod edit;
mod new;
mod plan;
mod projects;
mod rev;
mod stage;

use done::check_done_evidence;
pub(crate) use done::{
    apply_pairs, commit_staged, commit_staged_scoped, mark_done_on_merge_locked,
    mark_done_on_merge_waiting_with, stage, RealWait, WaitClock,
};
pub use done::{mark_done_on_merge, mark_done_on_merge_waiting, DoneWriteFailure};
pub use edit::{
    add_comment, add_ref, add_report, attach, attach_bytes, close_ref, link, patch_issue,
    set_fields, set_fields_if_rev, tag_edit, IssuePatch,
};
pub(crate) use edit::{
    apply_link, check_set_gates, comment_author, commit_front_with_comment, new_ref, sets_done,
};
pub(crate) use new::next_id;
pub use new::{master_issue_new_limits, new_issue};
pub(crate) use plan::{check_structure, create_granted_plan, granted_semantics, GrantedPreview};
pub use plan::{create_plan, decide_plan};
use projects::check_component;
pub(crate) use projects::{check_tags, issue_dir, load_front, save_front, split_tags};
pub use projects::{project_add, set_acceptance, set_project_policy};
use rev::check_rev;
pub use rev::issue_rev;
pub(crate) use rev::{
    actor_who, atomic_write, attach_foreign, attach_post_warnings, blocked_warnings_best_effort,
    commit, commit_scoped, commit_who, commit_who_scoped, create_exclusive, file_preimage,
    post_commit_warning, restore_preimage, write_new,
};

#[cfg(test)]
use new::lock_for_new_issue_rounds;

#[cfg(test)]
use rev::{write_new_seq, TEMP_ATTEMPTS};
pub use stage::move_stage;
use stage::{check_milestone, check_type_change};

#[cfg(test)]
mod tests {
    use super::*;

    /// AOS-274: the recorded policy advances by one version per write,
    /// keeps the minted identity, refuses private until the isolation
    /// gate passes, and refuses both an unsupported grant and an unknown
    /// project.
    #[test]
    fn setting_a_policy_bumps_the_version_and_refuses_private() {
        let (_dir, pm) = tracker();
        let key = "cadence";
        let before = project::load(&pm.dir.join(key).join("project.yaml")).unwrap();
        let start = before.policy.as_ref().map(|p| p.version).unwrap_or(0);
        let id = before.id.clone().expect("creation mints an identity");

        let out =
            set_project_policy(&pm, key, project::Visibility::WorkspaceVisible, &[], "").unwrap();
        assert_eq!(out["policy"]["version"], start + 1);
        assert_eq!(out["policy"]["visibility"], "workspace_visible");
        let loaded = project::load(&pm.dir.join(key).join("project.yaml")).unwrap();
        assert_eq!(loaded.id, Some(id), "the identity must not change");
        assert_eq!(loaded.policy.as_ref().unwrap().version, start + 1);

        // A grant is recorded and the version advances again.
        let out = set_project_policy(
            &pm,
            key,
            project::Visibility::WorkspaceVisible,
            &["users/mia".to_string()],
            "",
        )
        .unwrap();
        assert_eq!(out["policy"]["version"], start + 2);
        assert_eq!(out["policy"]["members"][0], "users/mia");
        let before = std::fs::read(pm.dir.join(key).join("project.yaml")).unwrap();

        // Private is refused until the isolation gate passes.
        set_project_policy(&pm, key, project::Visibility::Private, &[], "").unwrap_err();

        // Reserved grantee kinds and unknown projects are refused.
        set_project_policy(
            &pm,
            key,
            project::Visibility::WorkspaceVisible,
            &["agents/dev".to_string()],
            "",
        )
        .unwrap_err();
        set_project_policy(&pm, "ghost", project::Visibility::WorkspaceVisible, &[], "")
            .unwrap_err();
        assert!(!pm.dir.join("ghost").exists());

        // A key that is not the tracker grammar never joins the filesystem,
        // so it cannot create or edit a project outside this pm dir.
        for bad in [
            "../aos274-escape",
            "cadence/../../aos274-escape",
            "",
            "..",
            "/tmp",
        ] {
            set_project_policy(
                &pm,
                bad,
                project::Visibility::WorkspaceVisible,
                &["users/mia".to_string()],
                "",
            )
            .unwrap_err();
        }
        let escape = pm.dir.join("..").join("aos274-escape").join("project.yaml");
        assert!(!escape.exists(), "traversal key wrote outside the tracker");
        assert_eq!(
            std::fs::read(pm.dir.join(key).join("project.yaml")).unwrap(),
            before,
            "a refused policy mutation changed the real project"
        );
    }

    /// A commit the tracker refuses leaves the record exactly as it was —
    /// the rule every other writer here follows, which this verb was
    /// missing: the version had already been bumped on disk when the
    /// commit failed, so a refused write looked applied.
    #[test]
    fn a_policy_write_whose_commit_is_refused_leaves_the_record_alone() {
        let (_dir, pm) = tracker();
        let key = "cadence";
        let path = pm.dir.join(key).join("project.yaml");
        let before = std::fs::read(&path).unwrap();
        let hook = failing_hook(&pm);

        assert!(
            set_project_policy(&pm, key, project::Visibility::WorkspaceVisible, &[], "").is_err(),
            "the refused commit was reported as a success"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "a refused commit left the changed record behind"
        );
        assert!(clean(&pm), "a refused policy write left something behind");
        std::fs::remove_file(hook).unwrap();
    }

    /// CAD-908: a created file is whole, a collision renames, and no
    /// temp file survives either outcome.
    #[test]
    fn create_exclusive_leaves_no_temp_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let a = create_exclusive(dir.path(), "note.md", b"first").unwrap();
        let b = create_exclusive(dir.path(), "note.md", b"second").unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), b"first");
        assert_eq!(b.file_name().unwrap(), "note-1.md");
        assert_eq!(std::fs::read(&b).unwrap(), b"second");
        let err = write_new(&a, b"third").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&a).unwrap(), b"first");
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        assert_eq!(names, ["note-1.md", "note.md"], "a temp file survived");
    }

    /// CAD-971: leftover temps (a planted file, a dangling symlink) do
    /// not read as a target conflict — the next sequence number wins,
    /// the planted entries stay untouched, and a real target collision
    /// still reports AlreadyExists.
    #[test]
    fn write_new_skips_a_leftover_temp() {
        let dir = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        let planted = dir.path().join(format!(".x.md.tmp-{pid}-7"));
        std::fs::write(&planted, b"leftover").unwrap();
        let dangling = dir.path().join(format!(".x.md.tmp-{pid}-8"));
        std::os::unix::fs::symlink("/nonexistent/target", &dangling).unwrap();
        let target = dir.path().join("x.md");
        let mut n = 7;
        write_new_seq(&target, b"real", || {
            n += 1;
            n - 1
        })
        .unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"real");
        assert_eq!(std::fs::read(&planted).unwrap(), b"leftover");
        assert!(dangling.symlink_metadata().is_ok());
        assert!(!std::path::Path::new("/nonexistent/target").exists());
        // A real collision is still AlreadyExists, and still bounded.
        let err = write_new_seq(&target, b"again", || {
            n += 1;
            n - 1
        })
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    }

    /// Every temp name taken: the write gives up (bounded) with an
    /// error that is not AlreadyExists, and leaves nothing at the target.
    #[test]
    fn write_new_gives_up_when_every_temp_name_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        std::fs::write(dir.path().join(format!(".y.md.tmp-{pid}-0")), b"p").unwrap();
        let target = dir.path().join("y.md");
        let mut calls = 0;
        let err = write_new_seq(&target, b"x", || {
            calls += 1;
            0
        })
        .unwrap_err();
        assert_ne!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(calls, TEMP_ATTEMPTS);
        assert!(!target.exists());
    }

    /// A write that cannot complete leaves no file under the final
    /// name: the target directory is missing, so staging fails first.
    #[test]
    fn write_new_failure_leaves_nothing_at_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("gone").join("x.md");
        assert!(write_new(&target, b"x").is_err());
        assert!(!target.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    /// A tracker with project `cadence` and one ticket, CAD-1.
    fn tracker() -> (tempfile::TempDir, Pm) {
        let dir = tempfile::tempdir().unwrap();
        let pm = Pm::init(&dir.path().join("pm")).unwrap();
        project_add(&pm, "cadence", "CAD", &[], &[], &[], None).unwrap();
        new_issue(
            &pm,
            dir.path(),
            Some("cadence"),
            "ticket",
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
        (dir, pm)
    }

    /// A pre-commit hook that refuses every commit; remove it to heal.
    fn failing_hook(pm: &Pm) -> PathBuf {
        let hooks = crate::issue::git(&pm.dir, &["rev-parse", "--git-path", "hooks"]).unwrap();
        let hooks = pm.dir.join(hooks);
        std::fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        hook
    }

    /// Nothing staged, nothing changed in the work tree.
    fn clean(pm: &Pm) -> bool {
        crate::issue::git(&pm.dir, &["diff", "--cached", "--quiet"]).is_ok()
            && crate::issue::git(&pm.dir, &["status", "--porcelain"]).is_ok_and(|s| s.is_empty())
    }

    /// The tracker's own records are not caller attachments: a copy of a
    /// correction record onto another ticket must not be writable, or a
    /// listing would read it as that ticket's reviewed correction.
    #[test]
    fn attach_refuses_the_tracker_owned_records() {
        let (_dir, pm) = tracker();
        for name in model::RESERVED_ARTIFACT_NAMES {
            assert!(
                attach_bytes(&pm, "CAD-1", name, b"{}", true, "t").is_err(),
                "{name} was accepted as a caller attachment"
            );
            let (_project, dir) = issue_dir(&pm, "CAD-1").unwrap();
            assert!(
                !dir.join("artifacts").join(name).exists(),
                "{name} was written to artifacts/"
            );
        }
    }

    /// A forced done over several ids keeps its reason in the commit
    /// subject even when a later id was already done (CAD-887 review).
    #[test]
    fn forced_done_keeps_its_reason_when_a_later_id_is_already_done() {
        let (tmp, pm) = tracker();
        new_issue(
            &pm,
            tmp.path(),
            Some("cadence"),
            "second",
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
        set_fields(
            &pm,
            &["CAD-2".to_string()],
            &["status=done".to_string()],
            "t",
            Some("first"),
        )
        .unwrap();
        set_fields(
            &pm,
            &["CAD-1".to_string(), "CAD-2".to_string()],
            &["status=done".to_string()],
            "t",
            Some("because reasons"),
        )
        .unwrap();
        let subject = crate::issue::git(&pm.dir, &["log", "-1", "--format=%s"]).unwrap();
        assert!(subject.contains("forced: because reasons"), "{subject}");
    }

    /// `issue edit` whose commit is refused leaves issue.md as it was
    /// and removes the attachment and comment files it created.
    #[test]
    fn edit_commit_failure_rolls_everything_back() {
        let (tmp, pm) = tracker();
        let before = std::fs::read(pm.dir.join("cadence/CAD-1/issue.md")).unwrap();
        let attach = tmp.path().join("a.txt");
        std::fs::write(&attach, "x").unwrap();
        let hook = failing_hook(&pm);
        let spec = crate::issue::edit::EditSpec {
            set: vec!["priority=P0".to_string()],
            attach: vec![attach],
            comment: Some("a comment".to_string()),
            ..Default::default()
        };
        assert!(crate::issue::edit::edit(&pm, "CAD-1", &spec, "t", None).is_err());
        assert_eq!(
            std::fs::read(pm.dir.join("cadence/CAD-1/issue.md")).unwrap(),
            before
        );
        let count = |sub: &str| {
            std::fs::read_dir(pm.dir.join("cadence/CAD-1").join(sub))
                .map(|rd| rd.count())
                .unwrap_or(0)
        };
        assert_eq!(count("artifacts"), 0);
        assert_eq!(count("comments"), 0);
        assert!(clean(&pm));
        std::fs::remove_file(hook).unwrap();
        crate::issue::edit::edit(&pm, "CAD-1", &spec, "t", None).unwrap();
    }

    /// CAD-1522: `--unref` removes exactly the named ref, and an
    /// `--unref` whose target is not there refuses the whole edit — the
    /// caller asked for a correction that cannot be made, and the other
    /// parts of the same call must not land either. Before this, a ref
    /// added by mistake could not be removed at all, and a stale `pr:`
    /// ref pointing at another open PR makes `issue reconcile` return
    /// `held(open-pr)` forever (observed on ticket CAD-1514).
    #[test]
    fn edit_unref_removes_one_ref_and_refuses_an_absent_target() {
        let (_tmp, pm) = tracker();
        let url = |n: u32| format!("https://github.com/favcrm/cadence/pull/{n}");
        let edit = |spec: crate::issue::edit::EditSpec| {
            crate::issue::edit::edit(&pm, "CAD-1", &spec, "t", None)
        };
        let refs = || -> Vec<String> {
            let (front, _) = load_front(&pm.dir.join("cadence/CAD-1")).unwrap();
            front.refs.iter().filter_map(|r| r.url.clone()).collect()
        };
        let commits = || -> u32 {
            crate::issue::git(&pm.dir, &["rev-list", "--count", "HEAD"])
                .unwrap()
                .trim()
                .parse()
                .unwrap()
        };

        edit(crate::issue::edit::EditSpec {
            refs: vec![format!("pr:{}", url(1)), format!("pr:{}", url(2))],
            ..Default::default()
        })
        .unwrap();
        assert_eq!(refs(), vec![url(1), url(2)]);

        let before = std::fs::read(pm.dir.join("cadence/CAD-1/issue.md")).unwrap();
        let err = edit(crate::issue::edit::EditSpec {
            unrefs: vec![format!("pr:{}", url(9))],
            set: vec!["priority=P0".to_string()],
            ..Default::default()
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("has no pr ref"), "{err}");
        assert_eq!(
            refs(),
            vec![url(1), url(2)],
            "a refused --unref removed a ref"
        );
        assert_eq!(
            std::fs::read(pm.dir.join("cadence/CAD-1/issue.md")).unwrap(),
            before,
            "a refused --unref still wrote the rest of the edit"
        );

        let commits_before = commits();
        edit(crate::issue::edit::EditSpec {
            unrefs: vec![format!("pr:{}", url(1))],
            ..Default::default()
        })
        .unwrap();
        assert_eq!(refs(), vec![url(2)], "the wrong ref was removed");
        assert_eq!(
            commits(),
            commits_before + 1,
            "removal lands as exactly one tracker commit"
        );
    }

    #[test]
    fn lint_reports_duplicate_ids_across_project_folders() {
        let (_tmp, pm) = tracker();
        // Plant a legacy tracker with two project folders owning the
        // same prefix; project_add must refuse to create this state.
        let mut other = project::load(&pm.dir.join("cadence/project.yaml")).unwrap();
        other.key = "other".to_string();
        std::fs::create_dir_all(pm.dir.join("other")).unwrap();
        std::fs::write(
            pm.dir.join("other/project.yaml"),
            serde_yaml::to_string(&other).unwrap(),
        )
        .unwrap();
        let duplicate_dir = pm.dir.join("other/CAD-1");
        std::fs::create_dir_all(&duplicate_dir).unwrap();
        std::fs::copy(
            pm.dir.join("cadence/CAD-1/issue.md"),
            duplicate_dir.join("issue.md"),
        )
        .unwrap();

        let report = crate::issue::lint::run(&pm, None).unwrap();
        let errors = report["errors"].as_array().unwrap();
        assert_eq!(report["ok"], false, "{report}");
        assert!(
            errors.iter().any(|e| {
                e.as_str().is_some_and(|e| {
                    e.contains("CAD-1: duplicated id")
                        && e.contains("cadence/CAD-1")
                        && e.contains("other/CAD-1")
                })
            }),
            "{report}"
        );
    }

    #[test]
    fn project_add_refuses_another_projects_normalized_prefix() {
        let (_tmp, pm) = tracker();
        let head = crate::issue::git(&pm.dir, &["rev-parse", "HEAD"]).unwrap();
        let err = project_add(&pm, "other", "cad", &[], &[], &[], None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("Prefix CAD") && err.contains("cadence"),
            "{err}"
        );
        assert!(!pm.dir.join("other").exists());
        assert_eq!(
            crate::issue::git(&pm.dir, &["rev-parse", "HEAD"]).unwrap(),
            head
        );
        assert!(clean(&pm), "refused project add left tracker changes");
    }

    /// CAD-449: a failed done commit restores `issue.md` and its index
    /// entry — the next writer's `git add -A` finds nothing of it.
    #[test]
    fn a_failed_done_write_leaves_nothing_behind() {
        let (_dir, pm) = tracker();
        let (_, dir) = issue_dir(&pm, "CAD-1").unwrap();
        let before = std::fs::read(dir.join("issue.md")).unwrap();
        assert!(clean(&pm));
        let hook = failing_hook(&pm);
        let e = mark_done_on_merge(
            &pm,
            "CAD-1",
            "o/r#1 merged at x",
            "operator (delivery o/r#1)",
            None,
        )
        .unwrap_err();
        assert!(e.to_string().contains("commit"), "{e}");
        assert_eq!(std::fs::read(dir.join("issue.md")).unwrap(), before);
        assert!(clean(&pm), "a failed done write left something staged");
        std::fs::remove_file(hook).unwrap();
        // A status changed since the merge (`expect` no longer holds) is
        // left alone and nothing is written.
        let commits = crate::issue::git(&pm.dir, &["rev-list", "--count", "HEAD"]).unwrap();
        assert_eq!(
            mark_done_on_merge(&pm, "CAD-1", "w", "operator", Some("doing")).unwrap(),
            Some("backlog".to_string())
        );
        assert_eq!(load_front(&dir).unwrap().0.status, "backlog");
        assert_eq!(
            crate::issue::git(&pm.dir, &["rev-list", "--count", "HEAD"]).unwrap(),
            commits
        );
        assert_eq!(
            mark_done_on_merge(
                &pm,
                "CAD-1",
                "o/r#1 merged at x",
                "operator (delivery o/r#1)",
                Some("backlog"),
            )
            .unwrap(),
            None
        );
        assert_eq!(load_front(&dir).unwrap().0.status, "done");
        // Done already: left alone.
        assert_eq!(
            mark_done_on_merge(&pm, "CAD-1", "again", "operator", None).unwrap(),
            Some("done".to_string())
        );
    }

    /// CAD-449: the done write never waits for a busy tracker.
    #[test]
    fn a_busy_tracker_refuses_the_done_write_at_once() {
        let (_dir, pm) = tracker();
        let held = pm.lock().unwrap();
        let t = std::time::Instant::now();
        let e = mark_done_on_merge(&pm, "CAD-1", "w", "operator", None).unwrap_err();
        assert!(e.to_string().contains("locked"), "{e}");
        assert!(t.elapsed() < std::time::Duration::from_secs(2));
        drop(held);
    }

    /// CAD-1600: exhausting the id-allocation retries is CONTENTION,
    /// not a refusal. The error kind must be `busy` (exit 75, the one
    /// kind a caller may retry blindly) and not `rejected` (exit 3,
    /// which reads as "this will never work").
    #[test]
    fn an_exhausted_id_allocation_is_busy_not_rejected() {
        let (_dir, pm) = tracker();
        let project = crate::issue::project::list(&pm.dir)
            .unwrap()
            .into_iter()
            .find(|p| p.key == "cadence")
            .unwrap();
        let e = lock_for_new_issue_rounds(&pm, &project, None, 0)
            .expect_err("no rounds means no allocation");
        assert_eq!(e.kind(), "busy", "{e}");
        assert_eq!(
            crate::error::exit_code_for_kind(e.kind()),
            75,
            "a busy allocation must exit 75"
        );
    }

    /// CAD-449: a comment whose commit fails removes its file and its
    /// staging, so no later commit carries it.
    #[test]
    fn a_failed_comment_leaves_nothing_behind() {
        let (_dir, pm) = tracker();
        let hook = failing_hook(&pm);
        assert!(add_comment(&pm, "CAD-1", "hello", Some("w1"), None, None, "w1").is_err());
        assert!(clean(&pm), "a failed comment left something behind");
        std::fs::remove_file(hook).unwrap();
    }

    /// CAD-614: the master cannot stamp owner, id, or a non-backlog
    /// status. Anyone else still can set owner and id; status other
    /// than backlog is refused for every caller.
    #[test]
    fn master_issue_new_refuses_owner_id_and_non_backlog() {
        assert!(master_issue_new_limits(true, None, None, None).is_ok());
        assert!(master_issue_new_limits(true, None, None, Some("backlog")).is_ok());
        let owner = master_issue_new_limits(true, Some("bob"), None, None).unwrap_err();
        assert!(owner.to_string().contains("--owner"), "{owner}");
        assert!(owner.to_string().contains("actor=master"), "{owner}");
        let id = master_issue_new_limits(true, None, Some("CAD-9"), None).unwrap_err();
        assert!(id.to_string().contains("--id"), "{id}");
        let status = master_issue_new_limits(true, None, None, Some("ready")).unwrap_err();
        assert!(status.to_string().contains("backlog"), "{status}");
        assert!(master_issue_new_limits(false, Some("bob"), Some("CAD-9"), None).is_ok());
        let other = master_issue_new_limits(false, None, None, Some("ready")).unwrap_err();
        assert!(other.to_string().contains("backlog"), "{other}");
    }

    /// CAD-614: a create attributed to master records that actor, and
    /// `--file` body text replaces the empty acceptance stub.
    #[test]
    fn master_create_records_actor_and_file_body() {
        let (dir, pm) = tracker();
        new_issue(
            &pm,
            dir.path(),
            Some("cadence"),
            "from the master",
            None,
            None,
            &[],
            None,
            None,
            &[],
            None,
            Some("the note\n"),
            "master",
        )
        .unwrap();
        let md = std::fs::read_to_string(dir.path().join("pm/cadence/CAD-2/issue.md")).unwrap();
        assert!(md.contains("the note"), "{md}");
        let log = crate::issue::git(
            &pm.dir,
            &["log", "-1", "--format=%B", "--", "cadence/CAD-2/issue.md"],
        )
        .unwrap();
        assert!(log.contains("Actor: master"), "{log}");
    }

    /// CAD-756 fixtures: attach a ref or an artifact to CAD-1 the same
    /// way the real lanes leave them — front write or artifacts/ file.
    fn attach(pm: &Pm, edit: impl FnOnce(&mut model::Front)) {
        let (_p, dir) = issue_dir(pm, "CAD-1").unwrap();
        let (mut front, body) = load_front(&dir).unwrap();
        edit(&mut front);
        save_front(&dir, &front, &body).unwrap();
    }

    fn set_done(pm: &Pm, force: Option<&str>) -> Result<Value> {
        set_fields(
            pm,
            &["CAD-1".to_string()],
            &["status=done".to_string()],
            "t",
            force,
        )
    }

    /// The gate itself: a bare issue cannot be marked done — the
    /// front is untouched and nothing was staged for the commit.
    #[test]
    fn done_without_evidence_is_refused() {
        let (_tmp, pm) = tracker();
        let e = set_done(&pm, None).unwrap_err();
        assert!(e.to_string().contains("needs evidence"), "{e}");
        let (_p, dir) = issue_dir(&pm, "CAD-1").unwrap();
        let (front, _) = load_front(&dir).unwrap();
        assert_eq!(front.status, "backlog");
        assert!(clean(&pm));
    }

    fn patch_status(pm: &Pm, status: &str) -> Result<Value> {
        patch_issue(
            pm,
            "CAD-1",
            &IssuePatch {
                status: Some(status.to_string()),
                priority: None,
                owner: None,
                component: None,
                title: None,
                body: None,
                tags: None,
            },
            None,
            "t",
            None,
        )
    }

    /// CAD-971: the board's PATCH rides the same done-evidence gate as
    /// `issue set` — same error kind and message, nothing written.
    #[test]
    fn patch_done_without_evidence_is_refused_like_set() {
        let (_tmp, pm) = tracker();
        let via_set = set_done(&pm, None).unwrap_err();
        let via_patch = patch_status(&pm, "done").unwrap_err();
        assert!(matches!(via_patch, Error::Rejected(_)), "{via_patch:?}");
        assert_eq!(via_patch.to_string(), via_set.to_string());
        let (_p, dir) = issue_dir(&pm, "CAD-1").unwrap();
        let (front, _) = load_front(&dir).unwrap();
        assert_eq!(front.status, "backlog");
        assert!(clean(&pm));
    }

    #[test]
    fn patch_done_with_evidence_passes_and_dropped_is_exempt() {
        let (_tmp, pm) = tracker();
        patch_status(&pm, "dropped").unwrap();
        let (_tmp, pm) = tracker();
        let (_p, dir) = issue_dir(&pm, "CAD-1").unwrap();
        std::fs::create_dir_all(dir.join("artifacts")).unwrap();
        std::fs::write(dir.join("artifacts/review-r1.md"), "v\n").unwrap();
        patch_status(&pm, "done").unwrap();
    }

    /// A patch that does not change the status never asks the question.
    #[test]
    fn patch_done_to_done_and_other_fields_are_not_gated() {
        let (_tmp, pm) = tracker();
        set_done(&pm, Some("seed")).unwrap();
        patch_status(&pm, "done").unwrap();
        // A priority-only patch on a done issue never asks either.
        patch_issue(
            &pm,
            "CAD-1",
            &IssuePatch {
                status: None,
                priority: Some("P1".to_string()),
                owner: None,
                component: None,
                title: None,
                body: None,
                tags: None,
            },
            None,
            "t",
            None,
        )
        .unwrap();
    }

    #[test]
    fn done_with_pr_ref_passes() {
        let (_tmp, pm) = tracker();
        attach(&pm, |f| {
            f.refs.push(model::Ref {
                kind: "pr".to_string(),
                url: Some("https://github.com/o/r/pull/1".to_string()),
                path: None,
                label: None,
                closed: None,
                worktree: None,
                cargo_target: None,
                agent: None,
            })
        });
        set_done(&pm, None).unwrap();
    }

    #[test]
    fn done_with_finished_worktree_ref_passes() {
        let (_tmp, pm) = tracker();
        attach(&pm, |f| {
            f.refs.push(model::Ref {
                kind: "worktree".to_string(),
                path: Some("/x/lane".to_string()),
                closed: Some(true),
                url: None,
                label: None,
                worktree: None,
                cargo_target: None,
                agent: None,
            })
        });
        set_done(&pm, None).unwrap();
    }

    /// An OPEN worktree ref alone is not evidence — the lane may still
    /// hold unmerged work; finish is what proves survivability.
    #[test]
    fn done_with_open_worktree_ref_is_refused() {
        let (_tmp, pm) = tracker();
        attach(&pm, |f| {
            f.refs.push(model::Ref {
                kind: "worktree".to_string(),
                path: Some("/x/lane".to_string()),
                closed: None,
                url: None,
                label: None,
                worktree: None,
                cargo_target: None,
                agent: None,
            })
        });
        let e = set_done(&pm, None).unwrap_err();
        assert!(e.to_string().contains("needs evidence"), "{e}");
    }

    #[test]
    fn done_with_review_artifact_passes() {
        let (_tmp, pm) = tracker();
        let (_p, dir) = issue_dir(&pm, "CAD-1").unwrap();
        let artifacts = dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(artifacts.join("review-r1-t.md"), "verdict\n").unwrap();
        set_done(&pm, None).unwrap();
    }

    /// CAD-971: only a real artifact is evidence — a hidden temp, a
    /// directory named like a verdict and a symlink do not count.
    #[test]
    fn done_scan_ignores_dot_names_directories_and_symlinks() {
        let (_tmp, pm) = tracker();
        let (_p, dir) = issue_dir(&pm, "CAD-1").unwrap();
        let artifacts = dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(artifacts.join(".verdict-x.tmp"), "planted").unwrap();
        std::fs::write(artifacts.join(".review-r1.md"), "planted").unwrap();
        std::fs::create_dir(artifacts.join("review")).unwrap();
        std::fs::create_dir(artifacts.join("x-verdict.md")).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", artifacts.join("review-link.md")).unwrap();
        let e = set_done(&pm, None).unwrap_err();
        assert!(e.to_string().contains("needs evidence"), "{e}");
        let e = patch_status(&pm, "done").unwrap_err();
        assert!(e.to_string().contains("needs evidence"), "{e}");
        // A real regular verdict beside the junk is accepted.
        std::fs::write(artifacts.join("verdict-r1.md"), "v").unwrap();
        patch_status(&pm, "done").unwrap();
    }

    /// CAD-971: a failure AFTER the commit (the board no longer loads)
    /// never reports the landed PATCH as failed — one commit, `Ok`,
    /// the failure recorded in `post_commit_warnings`.
    #[test]
    fn patch_post_commit_failure_is_a_warning_not_an_error() {
        let (_tmp, pm) = tracker();
        let hook = pm.dir.join(".git/hooks/post-commit");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(
            &hook,
            "#!/bin/sh\nmkdir -p BAD && printf 'key: [unclosed\\n' > BAD/project.yaml\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let n = crate::issue::git(&pm.dir, &["rev-list", "--count", "HEAD"]).unwrap();
        let out = patch_status(&pm, "ready").unwrap();
        assert_eq!(out["committed"], true, "{out}");
        assert_eq!(out["warnings"], json!([]), "{out}");
        let warns = out["post_commit_warnings"].as_array().unwrap();
        assert!(
            warns[0]
                .as_str()
                .unwrap()
                .contains("blocked-by warnings unavailable"),
            "{out}"
        );
        let after = crate::issue::git(&pm.dir, &["rev-list", "--count", "HEAD"]).unwrap();
        assert_eq!(
            after.trim().parse::<u32>().unwrap(),
            n.trim().parse::<u32>().unwrap() + 1
        );
    }

    /// CAD-971: a failed tracker commit rolls the board PATCH back —
    /// the file is restored and the error surfaces.
    #[test]
    fn patch_commit_failure_restores_the_file() {
        let (_tmp, pm) = tracker();
        let (_p, dir) = issue_dir(&pm, "CAD-1").unwrap();
        let before = std::fs::read(dir.join("issue.md")).unwrap();
        let hook = pm.dir.join(".git/hooks/pre-commit");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let e = patch_status(&pm, "ready");
        assert!(e.is_err(), "{e:?}");
        assert_eq!(std::fs::read(dir.join("issue.md")).unwrap(), before);
    }

    #[test]
    fn done_forced_records_the_reason() {
        let (_tmp, pm) = tracker();
        set_done(&pm, Some("rolled forward by operator")).unwrap();
        let log = crate::issue::git(&pm.dir, &["log", "-1", "--format=%s"]).unwrap();
        assert!(log.contains("forced: rolled forward by operator"), "{log}");
    }

    #[test]
    fn force_without_reason_is_refused() {
        let (_tmp, pm) = tracker();
        let e = set_done(&pm, Some("   ")).unwrap_err();
        assert!(e.to_string().contains("needs a reason"), "{e}");
    }

    #[test]
    fn dropped_is_not_gated() {
        let (_tmp, pm) = tracker();
        set_fields(
            &pm,
            &["CAD-1".to_string()],
            &["status=dropped".to_string()],
            "t",
            None,
        )
        .unwrap();
    }

    /// Bulk stays all-or-nothing: one bare issue in a done batch fails
    /// the whole commit, including any sibling that carried evidence.
    #[test]
    fn bulk_done_refuses_all_or_nothing() {
        let (_tmp, pm) = tracker();
        new_issue(
            &pm,
            _tmp.path(),
            Some("cadence"),
            "two",
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
        attach(&pm, |f| {
            f.refs.push(model::Ref {
                kind: "pr".to_string(),
                url: Some("https://github.com/o/r/pull/1".to_string()),
                path: None,
                label: None,
                closed: None,
                worktree: None,
                cargo_target: None,
                agent: None,
            })
        });
        let e = set_fields(
            &pm,
            &["CAD-1".to_string(), "CAD-2".to_string()],
            &["status=done".to_string()],
            "t",
            None,
        )
        .unwrap_err();
        assert!(e.to_string().contains("CAD-2"), "{e}");
        let (_p, dir) = issue_dir(&pm, "CAD-1").unwrap();
        let (front, _) = load_front(&dir).unwrap();
        assert_eq!(front.status, "backlog", "evidenced sibling rolled back");
    }

    /// CAD-759: attach and comment each commit their new file in the
    /// same commit that names it — neither leaves the tree dirty, and
    /// a refused attach removes the dropped file like a refused
    /// comment does.
    #[test]
    fn writes_commit_their_files_and_leave_a_clean_tree() {
        let (dir, pm) = tracker();
        let src = dir.path().join("note.txt");
        std::fs::write(&src, "artifact bytes\n").unwrap();
        super::attach(&pm, "CAD-1", &src).unwrap();
        assert!(clean(&pm), "a committed attach left the tree dirty");
        assert!(pm.dir.join("cadence/CAD-1/artifacts/note.txt").is_file());
        add_comment(&pm, "CAD-1", "body", Some("w1"), None, None, "w1").unwrap();
        assert!(clean(&pm), "a committed comment left the tree dirty");

        // The refused half: the artifact is dropped, the commit is
        // refused, and the file comes back out with the staging.
        let hook = failing_hook(&pm);
        std::fs::write(&src, "artifact bytes 2\n").unwrap();
        let e = super::attach(&pm, "CAD-1", &src).unwrap_err();
        assert!(e.to_string().contains("commit"), "{e}");
        assert!(clean(&pm), "a refused attach left something behind");
        assert!(!pm.dir.join("cadence/CAD-1/artifacts/note-1.txt").exists());
        std::fs::remove_file(hook).unwrap();
    }
}
