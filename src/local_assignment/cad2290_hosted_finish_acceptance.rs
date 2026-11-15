//! Independently authored CAD-2290 acceptance; implementers must not edit or
//! weaken it. It drives the real hosted `issue finish` release
//! (`finish_guidance::release_with`, which runs `release_hosted_lane`)
//! against a real git repository whose lane was opened by the real hosted lane
//! setup (`ensure_lane`, the call `start_session` and `activate` make). That
//! setup writes the lane's `.env` itself. The ticket keeps setup's record of
//! that file in the worktree's own git dir; this check treats the record as
//! opaque and asserts only what the caller sees.

use super::*;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const ORG: &str = "ws_test";
const ISSUE: &str = "CAD-1";
const LANE: &str = "test";

/// Lane paths these checks inspect. The fixture repository ignores each one
/// with the rule this repository's own `.gitignore` uses.
const ENV: &str = ".env";
const NODE_MODULES: &str = "ui/node_modules/x";
const PYCACHE: &str = "scripts/__pycache__/y.pyc";
const NOTE: &str = "claudedocs/note.md";

/// Hosted finish needs no board request; any request fails the release.
struct NoBoard;

impl CoordHttp for NoBoard {
    fn send(&mut self, method: &str, path: &str, _: Option<&Value>) -> Result<Reply> {
        Err(Error::rejected(format!(
            "unexpected board request during hosted finish: {method} {path}"
        )))
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// Who wrote the lane's `.env`.
enum EnvWriter {
    /// Hosted lane setup.
    Setup,
    /// A person, into a lane setup never touched: the exact bytes setup wrote
    /// for this path, as on a lane made before setup recorded its `.env`.
    Hand,
}

/// A merged, clean, idle hosted lane bound to a finished assignment.
struct Lane {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    path: PathBuf,
    auth: PathBuf,
    state: PathBuf,
}

impl Lane {
    fn new(writer: EnvWriter) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let auth = tmp.path().join("auth");
        let state = tmp.path().join("state");
        for dir in [&repo, &auth, &state] {
            fs::create_dir(dir).unwrap();
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        fs::write(
            repo.join(".gitignore"),
            ".cadence/\n.env\n__pycache__/\n*.py[cod]\n/ui/node_modules\nclaudedocs/\ndocs/*\n",
        )
        .unwrap();
        fs::write(repo.join("file"), "base\n").unwrap();
        fs::create_dir(repo.join("ui")).unwrap();
        fs::write(repo.join("ui/package.json"), "{}\n").unwrap();
        fs::create_dir(repo.join("scripts")).unwrap();
        fs::write(repo.join("scripts/tool.py"), "print('tool')\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-m", "base"]);
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        let base = git(&repo, &["rev-parse", "HEAD"]);
        let branch = layout::branch(LANE);

        let path = ensure_lane(&repo, LANE, Some(&base)).expect("real hosted lane setup");
        let env = path.join(ENV);
        assert!(
            env.is_file(),
            "hosted lane setup no longer writes {}",
            env.display()
        );
        if let EnvWriter::Hand = writer {
            let written = fs::read(&env).unwrap();
            let git_dir = git(&path, &["rev-parse", "--absolute-git-dir"]);
            // Drop the checkout with its own git dir, then check the branch
            // out again at the same path with plain git: no setup runs.
            git(&repo, &["worktree", "remove", path.to_str().unwrap()]);
            assert!(!path.exists());
            git(&repo, &["worktree", "add", path.to_str().unwrap(), &branch]);
            assert_eq!(git(&path, &["rev-parse", "--absolute-git-dir"]), git_dir);
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&env)
                .unwrap()
                .write_all(&written)
                .unwrap();
        }
        fs::write(path.join("file"), "lane work\n").unwrap();
        git(&path, &["commit", "-am", "lane work"]);
        git(&repo, &["merge", "--ff-only", &branch]);
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        for rel in [NODE_MODULES, PYCACHE] {
            let file = path.join(rel);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, "regenerable build output\n").unwrap();
        }
        // Idle: every file that is not ignored last changed hours ago.
        let past = std::time::SystemTime::now() - Duration::from_secs(2 * 3600);
        for rel in git(&path, &["ls-files"]).lines() {
            fs::OpenOptions::new()
                .write(true)
                .open(path.join(rel))
                .unwrap()
                .set_modified(past)
                .unwrap();
        }
        let mut ledger = Ledger::load(&auth, ORG).unwrap();
        let attempt: Attempt = serde_json::from_value(json!({
            "operation_id": "op_test", "issue_id": ISSUE, "action": "start", "target": null,
            "assignment_id": "hla_test", "generation": 1, "claim_id": "hlc_test",
            "state": "result", "fenced": false, "reassign_operation_id": null,
            "worktree": path, "lane": LANE, "lane_claim": "hlc_test", "lane_generation": 1
        }))
        .unwrap();
        ledger
            .attempts
            .insert(attempt_key(ISSUE, "start", None), attempt);
        ledger.save(&auth, ORG).unwrap();
        Self {
            _tmp: tmp,
            repo,
            path,
            auth,
            state,
        }
    }

    /// `issue finish CAD-1` on the hosted org.
    fn finish(&self) -> Result<Value> {
        let coord = Coordinator::new(NoBoard, ORG.into(), self.auth.clone()).unwrap();
        finish_guidance::release_with(&coord, ISSUE, None, &self.state, &|_, _| None)
    }

    fn tip(&self) -> Option<String> {
        let branch = layout::branch(LANE);
        (!git(&self.repo, &["branch", "--list", &branch]).is_empty())
            .then(|| git(&self.repo, &["rev-parse", &branch]))
    }

    fn registered(&self) -> bool {
        git(&self.repo, &["worktree", "list", "--porcelain"]).contains(self.path.to_str().unwrap())
    }

    /// Finish refuses with `named` in the reason and removes nothing: the
    /// checkout, its registration, its branch and every file in `kept`, byte
    /// for byte.
    fn assert_refused_and_kept(&self, named: &str, kept: &[&str]) {
        let tip = self.tip().expect("the lane branch exists before finish");
        let before: Vec<Vec<u8>> = kept
            .iter()
            .map(|rel| fs::read(self.path.join(rel)).unwrap())
            .collect();
        let error = self
            .finish()
            .expect_err("hosted finish must refuse this lane");
        assert_eq!(error.kind(), "rejected", "{error}");
        let reason = error.to_string();
        assert!(
            reason.contains(named),
            "the refusal must name {named}: {reason}"
        );
        assert!(self.path.is_dir(), "the lane checkout was removed");
        assert!(
            self.registered(),
            "the lane is no longer a registered worktree"
        );
        assert_eq!(
            self.tip(),
            Some(tip),
            "the lane branch moved or was deleted"
        );
        for (rel, bytes) in kept.iter().zip(before) {
            assert_eq!(
                fs::read(self.path.join(rel)).ok(),
                Some(bytes),
                "{rel} was not kept"
            );
        }
    }
}

#[test]
fn cad2290_setup_env_and_nested_build_caches_do_not_block_hosted_finish() {
    let lane = Lane::new(EnvWriter::Setup);
    let result = lane.finish().expect(
        "a merged, clean, idle hosted lane holding only its setup .env and nested build \
         caches must finish",
    );
    assert_eq!(result["finished"], true);
    assert_eq!(result["removed_worktree"], true);
    assert_eq!(result["deleted_branch"], true);
    assert!(!lane.path.exists(), "the lane checkout still exists");
    assert!(
        !lane.registered(),
        "the lane is still a registered worktree"
    );
    assert_eq!(lane.tip(), None, "the lane branch was not deleted");
}

#[test]
fn cad2290_setup_env_with_an_extra_line_refuses_and_keeps_every_file() {
    let lane = Lane::new(EnvWriter::Setup);
    fs::OpenOptions::new()
        .append(true)
        .open(lane.path.join(ENV))
        .unwrap()
        .write_all(b"OPERATOR_SETTING=keep\n")
        .unwrap();
    lane.assert_refused_and_kept(ENV, &[ENV, NODE_MODULES, PYCACHE]);
}

#[test]
fn cad2290_env_with_no_setup_record_refuses_and_keeps_every_file() {
    let lane = Lane::new(EnvWriter::Hand);
    lane.assert_refused_and_kept(ENV, &[ENV, NODE_MODULES, PYCACHE]);
}

#[test]
fn cad2290_claudedocs_beside_the_setup_env_refuses_and_keeps_every_file() {
    let lane = Lane::new(EnvWriter::Setup);
    fs::create_dir(lane.path.join("claudedocs")).unwrap();
    fs::write(lane.path.join(NOTE), "operator notes\n").unwrap();
    lane.assert_refused_and_kept("claudedocs", &[ENV, NODE_MODULES, PYCACHE, NOTE]);
}
