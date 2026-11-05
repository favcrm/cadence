//! CAD-2178 independent acceptance: hosted `issue finish` deletes a worktree
//! and a branch, so the bad case must be refused by the real guard. Written
//! from the ticket, not from the implementation.
//!
//! Real temp git repos (with a bare origin), a real ledger file and the real
//! release code. The dirty, merged and flag cases enter through the actual
//! `issue finish` routing for a hosted org (`run_with_finish`); the rest call
//! `finish_guidance::release`, the function that routing hands over to.
//! A fresh checkout is "recently active" for 30 minutes, so every fixture ages
//! the mtimes of the lane's files; otherwise that block would hide the guard
//! under test.

use super::*;
use cadence_agent::issue::cli::IssueAction;
use cadence_agent::local_assignment::{finish_guidance, CoordHttp, Coordinator, Reply};
use std::cell::{Cell, RefCell};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

const ORG: &str = "ws_accept";
const ISSUE: &str = "CAD-1";

struct FakeRemote(Rc<Cell<usize>>);
impl CoordHttp for FakeRemote {
    fn send(&mut self, _: &str, _: &str, _: Option<&Value>) -> Result<Reply> {
        self.0.set(self.0.get() + 1);
        Err(Error::rejected("unexpected remote call"))
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn git_ok(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap()
        .status
        .success()
}

#[derive(Clone, Copy, PartialEq)]
enum Shape {
    /// Lane work landed on origin/main as one squash commit: ancestry is gone,
    /// the patch is applied.
    Squash,
    /// Lane work landed by fast-forward.
    FastForward,
    /// Lane work is on origin as the lane branch only; main never got it.
    PushedOnly,
}

fn lane_work(repo: &Path, lane: &Path, shape: Shape) {
    for name in ["a", "b"] {
        fs::write(lane.join(name), format!("{name}\n")).unwrap();
        git(lane, &["add", name]);
        git(lane, &["commit", "-m", name]);
    }
    match shape {
        Shape::Squash => {
            git(repo, &["merge", "--squash", "cadence/test"]);
            git(repo, &["commit", "-m", "squashed lane"]);
            git(repo, &["push", "origin", "main"]);
            assert!(
                !git_ok(
                    repo,
                    &["merge-base", "--is-ancestor", "cadence/test", "main"]
                ),
                "squash fixture must have lost ancestry"
            );
        }
        Shape::FastForward => {
            git(repo, &["merge", "--ff-only", "cadence/test"]);
            git(repo, &["push", "origin", "main"]);
        }
        Shape::PushedOnly => {
            git(lane, &["push", "origin", "cadence/test"]);
            assert!(git_ok(
                repo,
                &["rev-parse", "--verify", "refs/remotes/origin/cadence/test"]
            ));
        }
    }
}

struct Fx {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    lane: PathBuf,
    auth: PathBuf,
    state: PathBuf,
    calls: Rc<Cell<usize>>,
}

/// What the ledger records for the lane's attempt.
struct Rec {
    issue: &'static str,
    state: &'static str,
    claim: Option<&'static str>,
    lane_claim: Option<&'static str>,
    generation: Option<u64>,
    lane_generation: Option<u64>,
    fenced: bool,
    worktree: Option<PathBuf>,
}

impl Rec {
    fn ended(fx: &Fx) -> Rec {
        Rec {
            issue: ISSUE,
            state: "result",
            claim: Some("hlc_test"),
            lane_claim: Some("hlc_test"),
            generation: Some(1),
            lane_generation: Some(1),
            fenced: false,
            worktree: Some(fx.lane.clone()),
        }
    }
}

fn age(dir: &Path) {
    let past = SystemTime::now() - Duration::from_secs(2 * 3600);
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            if entry.file_name() != ".git" {
                age(&path);
            }
        } else if kind.is_file() {
            fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(past)
                .unwrap();
        }
    }
}

impl Fx {
    fn new(shape: Shape) -> Fx {
        Fx::build(move |repo, lane| lane_work(repo, lane, shape))
    }

    fn build(f: impl FnOnce(&Path, &Path)) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let (repo, origin) = (root.join("repo"), root.join("origin.git"));
        let (auth, state) = (root.join("auth"), root.join("state"));
        for dir in [&repo, &auth, &state] {
            fs::create_dir(dir).unwrap();
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        git(root, &["init", "--bare", "-b", "main", "origin.git"]);
        git(&repo, &["init", "-b", "main"]);
        for (k, v) in [
            ("user.email", "t@t"),
            ("user.name", "t"),
            ("commit.gpgsign", "false"),
        ] {
            git(&repo, &["config", k, v]);
        }
        fs::write(repo.join("file"), "base\n").unwrap();
        fs::create_dir_all(repo.join(".git/info")).unwrap();
        fs::write(repo.join(".git/info/exclude"), ".cadence/\n").unwrap();
        fs::write(repo.join(".gitignore"), "target/\ndocs/*\n.env\n").unwrap();
        git(&repo, &["add", ".gitignore"]);
        git(&repo, &["add", "file"]);
        git(&repo, &["commit", "-m", "base"]);
        git(
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(&repo, &["push", "origin", "main"]);
        git(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        let lane = repo.join(".cadence/wt/test");
        fs::create_dir_all(lane.parent().unwrap()).unwrap();
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "cadence/test",
                lane.to_str().unwrap(),
                "main",
            ],
        );
        f(&repo, &lane);
        git(&repo, &["push", "origin", "main"]);
        age(&lane);
        let fx = Fx {
            _tmp: tmp,
            repo,
            lane,
            auth,
            state,
            calls: Rc::new(Cell::new(0)),
        };
        fx.ledger(&Rec::ended(&fx));
        fx
    }

    fn ledger(&self, rec: &Rec) {
        let attempt = serde_json::json!({
            "operation_id": "op_test", "issue_id": rec.issue, "action": "start",
            "target": null, "assignment_id": "hla_test", "generation": rec.generation,
            "claim_id": rec.claim, "state": rec.state, "fenced": rec.fenced,
            "reassign_operation_id": null, "worktree": rec.worktree,
            "lane": rec.worktree.as_ref().map(|_| "test"),
            "lane_claim": rec.lane_claim, "lane_generation": rec.lane_generation
        });
        let ledger = serde_json::json!({
            "version": 1, "client_agent_id": "la_accept", "repo": null, "alias": null,
            "attempts": {"test": attempt}, "origins": {}
        });
        let path = self.auth.join(format!("local-assign-{ORG}.json"));
        fs::write(&path, serde_json::to_vec_pretty(&ledger).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn coord(&self) -> Coordinator<FakeRemote> {
        Coordinator::new(
            FakeRemote(self.calls.clone()),
            ORG.into(),
            self.auth.clone(),
        )
        .unwrap()
    }

    fn release(&self) -> Result<Value> {
        finish_guidance::release(&self.coord(), ISSUE, None, &self.state)
    }

    fn intact(&self) {
        assert!(self.lane.is_dir(), "lane directory was removed");
        assert!(
            git(&self.repo, &["worktree", "list", "--porcelain"])
                .contains(self.lane.to_str().unwrap()),
            "lane no longer registered"
        );
        assert!(
            git_ok(
                &self.repo,
                &["rev-parse", "--verify", "refs/heads/cadence/test"]
            ),
            "local branch was deleted"
        );
        assert_eq!(self.calls.get(), 0, "no remote call is allowed");
    }

    fn gone(&self) {
        assert!(!self.lane.exists(), "lane directory should be gone");
        assert!(
            !git(&self.repo, &["worktree", "list", "--porcelain"])
                .contains(self.lane.to_str().unwrap()),
            "lane still registered"
        );
        assert!(
            !git_ok(
                &self.repo,
                &["rev-parse", "--verify", "refs/heads/cadence/test"]
            ),
            "local branch should be deleted"
        );
    }
}

#[derive(Default, Clone, Copy)]
struct Flags {
    force: bool,
    keep_branch: bool,
    remote: bool,
    merged: bool,
    dry_run: bool,
}

/// `cadence --org <hosted> issue finish CAD-1 [flags]` through the real
/// remote routing. The closure only supplies the coordinator; everything the
/// command does with a hosted org happens in `run_with_finish`.
fn cli_finish(fx: &Fx, flags: Flags) -> (Result<i32>, Option<Value>) {
    let command = Commands::Issue {
        action: IssueAction::Finish {
            id: Some(ISSUE.into()),
            worktree: None,
            force: flags.force,
            keep_branch: flags.keep_branch,
            remote: flags.remote,
            merged: flags.merged,
            project: None,
            dry_run: flags.dry_run,
            json: false,
        },
    };
    let target = RemoteTarget {
        org: "synthetic".into(),
        endpoint: "https://synthetic.cadencecloud.app".into(),
        org_id: ORG.into(),
        identity: None,
    };
    let coord = fx.coord();
    let out = RefCell::new(None);
    let result = run_with_finish(
        target,
        command,
        1,
        Some(fx.state.clone()),
        Some("issue finish".into()),
        |_, issue, worktree, state| {
            let value = finish_guidance::release(&coord, issue, worktree, state.unwrap())?;
            *out.borrow_mut() = Some(value.clone());
            Ok(value)
        },
    );
    (result, out.into_inner())
}

fn refusal(result: Result<impl std::fmt::Debug>) -> String {
    let message = result.expect_err("must be refused").to_string();
    assert!(!message.contains("reconcile"), "{message}");
    message
}

/// Case: A dirty lane is refused through `issue finish`; the guard named is the
/// uncommitted-changes one, not a downstream git failure.
#[test]
fn dirty_lane_is_refused_and_left_intact() {
    let fx = Fx::new(Shape::Squash);
    fs::write(fx.lane.join("a"), "uncommitted edit\n").unwrap();
    age(&fx.lane);
    let (result, released) = cli_finish(&fx, Flags::default());
    let message = refusal(result);
    assert!(message.contains("uncommitted"), "{message}");
    assert!(!message.contains("pass --force"), "{message}");
    assert!(released.is_none());
    fx.intact();
}

struct Reap(std::process::Child);
impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Case: A process whose cwd is inside the lane blocks release.
#[test]
fn process_inside_the_lane_is_refused() {
    let fx = Fx::new(Shape::Squash);
    let child = Reap(
        Command::new("sleep")
            .arg("600")
            .current_dir(&fx.lane)
            .spawn()
            .unwrap(),
    );
    let message = refusal(fx.release());
    assert!(message.contains(&child.0.id().to_string()), "{message}");
    assert!(message.contains("cwd"), "{message}");
    fx.intact();
}

/// Case: An assignment that is still running or blocked is a turn inside the
/// lane.
#[test]
fn unfinished_assignment_is_refused() {
    let fx = Fx::new(Shape::Squash);
    for state in ["running", "blocked"] {
        let mut rec = Rec::ended(&fx);
        rec.state = state;
        fx.ledger(&rec);
        let message = refusal(fx.release());
        assert!(message.contains("not ended"), "{state}: {message}");
        fx.intact();
    }
}

/// Case: Work that exists only on origin's lane branch is refused: stricter
/// than native finish, which accepts "pushed".
#[test]
fn unmerged_but_pushed_branch_is_refused() {
    let fx = Fx::new(Shape::PushedOnly);
    let message = refusal(fx.release());
    assert!(message.contains("not shown merged"), "{message}");
    fx.intact();
}

/// Case: A merged, clean, idle lane releases through `issue finish`: squash
/// shape (no ancestry, patch applied) and fast-forward shape.
#[test]
fn merged_clean_idle_lane_is_released_through_issue_finish() {
    let fx = Fx::new(Shape::Squash);
    fx.intact();
    let (result, value) = cli_finish(&fx, Flags::default());
    assert_eq!(result.unwrap(), 0);
    let value = value.expect("routing must reach the release");
    assert_eq!(value["finished"], true, "{value}");
    assert!(value["merged_by"].is_string(), "{value}");
    fx.gone();

    let ff = Fx::new(Shape::FastForward);
    assert_eq!(ff.release().unwrap()["finished"], true);
    ff.gone();
}

/// Case: Options that would widen or soften the release are refused before any
/// removal, even on a lane that would otherwise release.
#[test]
fn widening_flags_are_refused_and_remove_nothing() {
    let fx = Fx::new(Shape::Squash);
    for (name, flags) in [
        (
            "--force",
            Flags {
                force: true,
                ..Flags::default()
            },
        ),
        (
            "--keep-branch",
            Flags {
                keep_branch: true,
                ..Flags::default()
            },
        ),
        (
            "--remote",
            Flags {
                remote: true,
                ..Flags::default()
            },
        ),
        (
            "--merged",
            Flags {
                merged: true,
                ..Flags::default()
            },
        ),
        (
            "--dry-run",
            Flags {
                dry_run: true,
                ..Flags::default()
            },
        ),
    ] {
        let (result, released) = cli_finish(&fx, flags);
        let message = refusal(result);
        assert!(message.contains(name), "{name}: {message}");
        assert!(released.is_none(), "{name} reached the release");
        fx.intact();
    }
}

/// Case: A lane the ledger cannot vouch for is refused, naming the manual path.
#[test]
fn unbound_or_mismatched_lane_is_refused_with_the_manual_path() {
    type Edit = fn(&mut Rec, &Fx);
    let edits: [(&str, Edit); 9] = [
        ("other issue", |r, _| r.issue = "CAD-9"),
        ("claim mismatch", |r, _| r.lane_claim = Some("hlc_other")),
        ("generation mismatch", |r, _| r.lane_generation = Some(2)),
        ("fenced, assignment still live", |r, _| {
            r.fenced = true;
            r.state = "running";
        }),
        ("fenced and ended, claim mismatch", |r, _| {
            r.fenced = true;
            r.lane_claim = Some("hlc_other");
        }),
        ("fenced and ended, generation mismatch", |r, _| {
            r.fenced = true;
            r.lane_generation = Some(2);
        }),
        ("no lane fields", |r, _| {
            r.worktree = None;
            r.claim = None;
        }),
        ("path outside the layout", |r, fx| {
            let outside = fx.repo.join("elsewhere");
            git(
                &fx.repo,
                &[
                    "worktree",
                    "add",
                    "-b",
                    "cadence/other",
                    outside.to_str().unwrap(),
                    "main",
                ],
            );
            r.worktree = Some(outside);
        }),
        ("not a checkout", |r, fx| {
            let dir = fx.repo.parent().unwrap().join("plain");
            fs::create_dir(&dir).unwrap();
            r.worktree = Some(dir);
        }),
    ];
    for (name, edit) in edits {
        let fx = Fx::new(Shape::Squash);
        let mut rec = Rec::ended(&fx);
        edit(&mut rec, &fx);
        fx.ledger(&rec);
        let message = refusal(fx.release());
        assert!(
            message.contains("git worktree remove"),
            "{name}: manual path missing: {message}"
        );
        fx.intact();
    }
}

/// An assignment cancelled with `reassign --cancel` is ended and fenced; with
/// every binding field intact its merged lane releases.
#[test]
fn cancelled_fenced_row_with_intact_binding_releases() {
    let fx = Fx::new(Shape::Squash);
    let mut rec = Rec::ended(&fx);
    rec.state = "cancelled";
    rec.fenced = true;
    fx.ledger(&rec);
    assert_eq!(fx.release().unwrap()["finished"], true);
    fx.gone();
}

fn tip(fx: &Fx) -> String {
    git(&fx.repo, &["rev-parse", "refs/heads/cadence/test"])
}

/// Refuse, with the branch tip unchanged and the lane intact.
fn assert_refused_keeping_tip(fx: &Fx) -> String {
    let before = tip(fx);
    let message = refusal(fx.release());
    assert_eq!(tip(fx), before, "branch tip moved");
    fx.intact();
    message
}

/// Data loss: the lane adds one more identical row to a run of repeated
/// lines. Reverse-applying the patch "works" by removing a different row.
#[test]
fn repeated_line_is_not_taken_for_merged() {
    let fx = Fx::build(|repo, lane| {
        fs::write(repo.join("rows"), "x\nx\nx\n").unwrap();
        git(repo, &["add", "rows"]);
        git(repo, &["commit", "-m", "rows"]);
        git(lane, &["merge", "--ff-only", "main"]);
        fs::write(lane.join("rows"), "x\nx\nx\nx\n").unwrap();
        git(lane, &["commit", "-am", "one more row"]);
    });
    assert_refused_keeping_tip(&fx);
    assert_eq!(git(&fx.repo, &["show", "cadence/test:rows"]), "x\nx\nx\nx");
}

/// Data loss: a mode-only commit (`chmod +x`).
#[test]
fn mode_only_commit_is_not_taken_for_merged() {
    let fx = Fx::build(|repo, lane| {
        fs::write(repo.join("run.sh"), "echo\n").unwrap();
        git(repo, &["add", "run.sh"]);
        git(repo, &["commit", "-m", "script"]);
        git(lane, &["merge", "--ff-only", "main"]);
        fs::set_permissions(lane.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        git(lane, &["commit", "-am", "make executable"]);
    });
    assert_refused_keeping_tip(&fx);
    assert!(git(&fx.repo, &["ls-tree", "cadence/test", "run.sh"]).starts_with("100755"));
}

/// Data loss: every non-merge commit is on main, but the merge commit
/// carries a unique file.
#[test]
fn merge_commit_only_work_is_not_taken_for_merged() {
    let fx = Fx::build(|repo, lane| {
        fs::write(repo.join("main-only"), "main\n").unwrap();
        git(repo, &["add", "main-only"]);
        git(repo, &["commit", "-m", "main"]);
        fs::write(lane.join("lane-only"), "lane\n").unwrap();
        git(lane, &["add", "lane-only"]);
        git(lane, &["commit", "-m", "lane"]);
        let lane_commit = git(lane, &["rev-parse", "HEAD"]);
        git(lane, &["merge", "--no-ff", "--no-commit", "main"]);
        fs::write(lane.join("merge-only"), "unique resolution\n").unwrap();
        git(lane, &["add", "merge-only"]);
        git(lane, &["commit", "-m", "merge with unique file"]);
        git(repo, &["cherry-pick", &lane_commit]);
    });
    assert_refused_keeping_tip(&fx);
    assert_eq!(
        git(&fx.repo, &["show", "cadence/test:merge-only"]),
        "unique resolution"
    );
}

/// Cadence's normal merges must still release.
#[test]
fn normal_squash_shapes_release() {
    // Multi-commit squash, the normal case (the `patch` arm).
    let multi = Fx::new(Shape::Squash);
    let v = multi.release().unwrap();
    assert_eq!(v["merged_by"], "patch", "{v}");
    multi.gone();

    // The same squash after main moved on with unrelated files.
    let moved = Fx::build(|repo, lane| {
        lane_work(repo, lane, Shape::Squash);
        for n in ["unrelated1", "unrelated2"] {
            fs::write(repo.join(n), "x\n").unwrap();
            git(repo, &["add", n]);
            git(repo, &["commit", "-m", n]);
        }
    });
    moved
        .release()
        .expect("squash with unrelated main commits must release");
    moved.gone();

    // Single-commit squash.
    let single = Fx::build(|repo, lane| {
        fs::write(lane.join("only"), "only\n").unwrap();
        git(lane, &["add", "only"]);
        git(lane, &["commit", "-m", "only"]);
        git(repo, &["merge", "--squash", "cadence/test"]);
        git(repo, &["commit", "-m", "squashed"]);
    });
    single.release().expect("single-commit squash must release");
    single.gone();
}

/// A squash whose file later changed on main. Direction is not asserted:
/// refusal is the safe direction; either outcome must leave a consistent
/// state. The outcome is printed for the report.
#[test]
fn squash_then_file_changed_on_main_is_consistent() {
    let fx = Fx::build(|repo, lane| {
        lane_work(repo, lane, Shape::Squash);
        fs::write(repo.join("a"), "a changed later on main\n").unwrap();
        git(repo, &["commit", "-am", "later change"]);
    });
    match fx.release() {
        Ok(v) => {
            eprintln!("LATER-CHANGE OUTCOME: released via {}", v["merged_by"]);
            fx.gone();
        }
        Err(e) => {
            eprintln!("LATER-CHANGE OUTCOME: refused: {e}");
            fx.intact();
        }
    }
}

/// Only build output may be deleted with the worktree.
#[test]
fn ignored_files_decide_release() {
    let fx = Fx::new(Shape::Squash);
    fs::create_dir_all(fx.lane.join("target/debug")).unwrap();
    fs::write(fx.lane.join("target/debug/out"), "bin").unwrap();
    fx.release().expect("target/ only must release");
    fx.gone();

    for file in ["docs/x.md", ".env"] {
        let fx = Fx::new(Shape::Squash);
        let path = fx.lane.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "operator data").unwrap();
        let message = assert_refused_keeping_tip(&fx);
        // `git status --ignored` collapses a wholly ignored directory, so the
        // refusal names `docs/` rather than `docs/x.md` (reported as a defect).
        let named = file.split_once('/').map_or(file, |(top, _)| top);
        assert!(message.contains(named), "{file}: {message}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "operator data");
    }
}
