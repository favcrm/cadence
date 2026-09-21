//! CAD-234: `issue finish --merged` preview when a tracker worktree
//! path is already gone.
//!
//! The CAD-232 audit's thirteen `would-finish` rows are one shape: the
//! recorded directory is absent and the local branch ref still exists,
//! either pruned or still registered at that missing path. A retained
//! root-refresh lane is a different live checkout — sometimes the same
//! branch moved, sometimes an unrelated worktree. Both must stay, and
//! a present path must still refuse while a process stands in it.
//! Synthetic repos only; nothing here touches a host worktree.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use cadence_agent::issue::model::{Front, Ref};
use cadence_agent::issue::parse;
use cadence_agent::issue::{finish, Pm};
use serde_json::Value;
use tempfile::TempDir;

struct Kill(Option<Child>);

impl Drop for Kill {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_EDITOR", "true")
        .env("GIT_MERGE_AUTOEDIT", "no")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {} in {}: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .unwrap()
        .status
        .success()
}

fn work_ref(kind: &str, path: &str) -> Ref {
    Ref {
        kind: kind.to_string(),
        url: None,
        path: Some(path.to_string()),
        label: None,
        closed: None,
        worktree: None,
        cargo_target: None,
        agent: None,
    }
}

fn write_issue(pm: &Path, id: &str, title: &str, wt: &Path, branch: &str) {
    let mut front = Front::new(id, title, "2026-09-21T00:00:00Z");
    front.status = "doing".to_string();
    front.refs = vec![
        work_ref("worktree", &wt.display().to_string()),
        work_ref("branch", branch),
    ];
    let dir = pm.join("demo").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let text = parse::render(&front, "body\n").unwrap();
    std::fs::write(dir.join("issue.md"), text).unwrap();
}

fn add_lane(repo: &Path, path: &Path, branch: &str, file: &str, merge: bool) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    git(
        repo,
        &["worktree", "add", "-b", branch, path.to_str().unwrap()],
    );
    std::fs::write(path.join(file), "x").unwrap();
    git(path, &["add", "-A"]);
    git(path, &["commit", "-qm", file]);
    if merge {
        git(repo, &["merge", "--no-edit", branch]);
    }
}

fn row<'a>(plan: &'a Value, id: &str) -> &'a Value {
    plan["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["issue"] == id)
        .unwrap_or_else(|| panic!("missing row {id} in {plan}"))
}

fn assert_preview(plan: &Value, id: &str, outcome: &str, reason: Option<&str>) -> Value {
    let row = row(plan, id).clone();
    assert_eq!(row["outcome"], outcome, "{id}: {plan}");
    match reason {
        Some(reason) => assert_eq!(row["reason"], reason, "{id}: {plan}"),
        None => assert!(row["reason"].is_null(), "{id}: {plan}"),
    }
    row
}

#[test]
fn finish_preview_classifies_missing_worktrees_without_finishing_them() {
    let tmp = TempDir::new().unwrap();
    let pm_dir = tmp.path().join("pm");
    let repo_dir = tmp.path().join("repo");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    let pm = Pm::init(&pm_dir).unwrap();
    git(&repo_dir, &["init", "-b", "main"]);
    git(&repo_dir, &["config", "user.email", "t@t"]);
    git(&repo_dir, &["config", "user.name", "t"]);
    std::fs::write(repo_dir.join("f"), "one").unwrap();
    git(&repo_dir, &["add", "f"]);
    git(&repo_dir, &["commit", "-qm", "init"]);
    let repo = repo_dir.canonicalize().unwrap();

    let wt = |name: &str| -> PathBuf { repo.join(".cadence").join("wt").join(name) };
    let branch = |name: &str| -> String { format!("cadence/{name}") };

    // Present lanes: one idle and merged, one merged with a process
    // standing in it. The idle lane sorts last so a real sweep's
    // `worktree prune` cannot change how earlier rows were classified.
    add_lane(
        &repo,
        &wt("p-2-busy"),
        &branch("p-2-busy"),
        "busy.txt",
        true,
    );
    add_lane(
        &repo,
        &wt("p-3-gone"),
        &branch("p-3-gone"),
        "gone.txt",
        true,
    );
    add_lane(
        &repo,
        &wt("p-4-nobranch"),
        &branch("p-4-nobranch"),
        "none.txt",
        true,
    );
    add_lane(
        &repo,
        &wt("p-5-recorded"),
        &branch("p-5-recorded"),
        "moved.txt",
        true,
    );
    add_lane(
        &repo,
        &wt("p-6-stale"),
        &branch("p-6-stale"),
        "stale.txt",
        true,
    );
    add_lane(
        &repo,
        &wt("p-7-present"),
        &branch("p-7-present"),
        "present.txt",
        true,
    );
    add_lane(
        &repo,
        &wt("p-8-live"),
        &branch("p-8-live"),
        "live.txt",
        true,
    );
    // Retained lane whose branch is not any missing ref. No tracker
    // row — the sweep must leave the directory and the branch alone.
    add_lane(
        &repo,
        &wt("root-refresh"),
        &branch("root-refresh"),
        "refresh.txt",
        false,
    );

    std::fs::remove_dir_all(wt("p-3-gone")).unwrap();
    git(&repo, &["worktree", "prune"]);
    std::fs::remove_dir_all(wt("p-4-nobranch")).unwrap();
    git(&repo, &["worktree", "prune"]);
    git(&repo, &["branch", "-D", &branch("p-4-nobranch")]);
    let refresh = wt("p-5-refresh");
    git(
        &repo,
        &[
            "worktree",
            "move",
            wt("p-5-recorded").to_str().unwrap(),
            refresh.to_str().unwrap(),
        ],
    );
    std::fs::remove_dir_all(wt("p-6-stale")).unwrap();
    git(&wt("p-7-present"), &["checkout", "--detach"]);
    git(&repo, &["branch", "-D", &branch("p-7-present")]);

    std::fs::create_dir_all(pm_dir.join("demo")).unwrap();
    std::fs::write(
        pm_dir.join("demo").join("project.yaml"),
        "key: demo\nprefix: P\n",
    )
    .unwrap();

    write_issue(&pm_dir, "P-2", "Busy", &wt("p-2-busy"), &branch("p-2-busy"));
    write_issue(&pm_dir, "P-3", "Gone", &wt("p-3-gone"), &branch("p-3-gone"));
    write_issue(
        &pm_dir,
        "P-4",
        "NoBranch",
        &wt("p-4-nobranch"),
        &branch("p-4-nobranch"),
    );
    write_issue(
        &pm_dir,
        "P-5",
        "Moved",
        &wt("p-5-recorded"),
        &branch("p-5-recorded"),
    );
    write_issue(
        &pm_dir,
        "P-6",
        "Stale",
        &wt("p-6-stale"),
        &branch("p-6-stale"),
    );
    write_issue(
        &pm_dir,
        "P-7",
        "Present",
        &wt("p-7-present"),
        &branch("p-7-present"),
    );
    write_issue(&pm_dir, "P-8", "Live", &wt("p-8-live"), &branch("p-8-live"));

    let held = Kill(Some(
        Command::new("sleep")
            .arg("300")
            .current_dir(wt("p-2-busy"))
            .spawn()
            .unwrap(),
    ));
    let pid = held.0.as_ref().unwrap().id().to_string();

    let sweep =
        |dry_run: bool| finish::sweep(&pm, Some("demo"), false, dry_run, "", &state).unwrap();
    let rev = |dir: &Path| -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let pm_head_before = rev(&pm_dir);
    let repo_head_before = rev(&repo);
    let issue_bytes = |id: &str| -> Vec<u8> {
        std::fs::read(pm_dir.join("demo").join(id).join("issue.md")).unwrap()
    };
    let before: Vec<_> = ["P-2", "P-3", "P-4", "P-5", "P-6", "P-7", "P-8"]
        .into_iter()
        .map(|id| (id, issue_bytes(id)))
        .collect();

    let plan = sweep(true);
    assert_eq!(plan["dry_run"], true);
    assert_eq!(plan["refused"], 1, "{plan}");
    assert_eq!(plan["rows"].as_array().unwrap().len(), 7, "{plan}");

    let live = assert_preview(&plan, "P-8", "would-finish", None);
    assert_eq!(live["path_state"], "present", "{live}");
    assert_eq!(live["branch_state"], "present", "{live}");
    assert_eq!(live["merged_by"], "ancestry", "{live}");
    assert_eq!(
        live["live_path"],
        wt("p-8-live").display().to_string(),
        "{live}"
    );

    let busy = row(&plan, "P-2");
    assert_eq!(busy["outcome"], "refused", "{busy}");
    assert!(
        busy["reason"].as_str().unwrap_or_default().contains(&pid),
        "ownership guard must still name the process: {busy}"
    );
    assert_eq!(busy["path_state"], "present", "{busy}");
    assert_eq!(busy["branch_state"], "present", "{busy}");

    let gone = assert_preview(
        &plan,
        "P-3",
        "skipped",
        Some("reconcile: missing-worktree, branch-present"),
    );
    assert_eq!(gone["path_state"], "missing", "{gone}");
    assert_eq!(gone["branch_state"], "present", "{gone}");
    assert!(gone["live_path"].is_null(), "{gone}");
    assert_eq!(gone["merged_by"], "ancestry", "{gone}");

    let missing_branch = assert_preview(
        &plan,
        "P-4",
        "skipped",
        Some("reconcile: missing-worktree, branch-missing"),
    );
    assert_eq!(missing_branch["path_state"], "missing", "{missing_branch}");
    assert_eq!(
        missing_branch["branch_state"], "missing",
        "{missing_branch}"
    );
    assert!(missing_branch["live_path"].is_null(), "{missing_branch}");
    assert!(missing_branch["merged_by"].is_null(), "{missing_branch}");

    let moved = assert_preview(
        &plan,
        "P-5",
        "skipped",
        Some("reconcile: path-branch-mismatch"),
    );
    assert_eq!(moved["path_state"], "missing", "{moved}");
    assert_eq!(moved["branch_state"], "elsewhere", "{moved}");
    assert_eq!(moved["live_path"], refresh.display().to_string(), "{moved}");
    assert_eq!(moved["merged_by"], "ancestry", "{moved}");

    let stale = assert_preview(
        &plan,
        "P-6",
        "skipped",
        Some("reconcile: missing-worktree, branch-present"),
    );
    assert_eq!(stale["path_state"], "missing", "{stale}");
    assert_eq!(stale["branch_state"], "present", "{stale}");
    assert_eq!(
        stale["live_path"],
        wt("p-6-stale").display().to_string(),
        "a stale admin entry still names the recorded path: {stale}"
    );

    let present_gone_branch = assert_preview(&plan, "P-7", "skipped", Some("branch missing"));
    assert_eq!(present_gone_branch["path_state"], "present");
    assert_eq!(present_gone_branch["branch_state"], "missing");
    assert!(present_gone_branch["live_path"].is_null());

    // Dry-run changes nothing: dirs, branches, tracker bytes, tracker git.
    assert!(wt("p-2-busy").is_dir());
    assert!(wt("p-8-live").is_dir());
    assert!(!wt("p-3-gone").exists());
    assert!(refresh.is_dir());
    assert!(wt("root-refresh").is_dir());
    assert!(branch_exists(&repo, &branch("p-3-gone")));
    assert!(branch_exists(&repo, &branch("p-5-recorded")));
    assert!(branch_exists(&repo, &branch("p-6-stale")));
    assert!(branch_exists(&repo, &branch("root-refresh")));
    assert!(!branch_exists(&repo, &branch("p-4-nobranch")));
    for (id, bytes) in &before {
        assert_eq!(&issue_bytes(id), bytes, "{id} changed during dry-run");
        assert!(
            !String::from_utf8_lossy(bytes).contains("closed:"),
            "{id} must stay open"
        );
    }
    assert_eq!(
        rev(&pm_dir),
        pm_head_before,
        "dry-run must not commit the tracker"
    );
    assert_eq!(
        rev(&repo),
        repo_head_before,
        "dry-run must not move the repo"
    );

    let applied = sweep(false);
    assert_eq!(applied["dry_run"], false);
    assert_eq!(applied["refused"], 1, "{applied}");
    assert_preview(&applied, "P-8", "finished", None);
    assert_eq!(row(&applied, "P-8")["deleted_branch"], true, "{applied}");
    assert_eq!(row(&applied, "P-8")["removed_worktree"], true, "{applied}");
    let busy = row(&applied, "P-2");
    assert_eq!(busy["outcome"], "refused", "{busy}");
    assert!(
        busy["reason"].as_str().unwrap_or_default().contains(&pid),
        "a real sweep must still refuse the in-use worktree: {busy}"
    );
    for (id, reason) in [
        ("P-3", "reconcile: missing-worktree, branch-present"),
        ("P-4", "reconcile: missing-worktree, branch-missing"),
        ("P-5", "reconcile: path-branch-mismatch"),
        ("P-6", "reconcile: missing-worktree, branch-present"),
        ("P-7", "branch missing"),
    ] {
        assert_preview(&applied, id, "skipped", Some(reason));
    }

    assert!(
        !wt("p-8-live").exists(),
        "the present idle lane still finishes"
    );
    assert!(!branch_exists(&repo, &branch("p-8-live")));
    assert!(wt("p-2-busy").is_dir());
    assert!(branch_exists(&repo, &branch("p-2-busy")));
    assert!(branch_exists(&repo, &branch("p-3-gone")));
    assert!(!branch_exists(&repo, &branch("p-4-nobranch")));
    assert!(refresh.is_dir());
    assert!(branch_exists(&repo, &branch("p-5-recorded")));
    assert!(branch_exists(&repo, &branch("p-6-stale")));
    assert!(wt("p-7-present").is_dir());
    assert!(wt("root-refresh").is_dir());
    assert!(branch_exists(&repo, &branch("root-refresh")));
    for id in ["P-2", "P-3", "P-4", "P-5", "P-6", "P-7"] {
        let bytes = issue_bytes(id);
        let previous = before.iter().find(|(i, _)| *i == id).unwrap();
        assert_eq!(
            bytes, previous.1,
            "{id} tracker changed during the real sweep"
        );
        assert!(
            !String::from_utf8_lossy(&bytes).contains("closed:"),
            "{id} must stay open until an explicit finish"
        );
    }
    assert!(
        String::from_utf8_lossy(&issue_bytes("P-8")).contains("closed:"),
        "a present merged lane still closes on a real sweep"
    );

    drop(held);
}
