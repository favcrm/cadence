//! board_lanes: area tests split from tests/board.rs (CAD-537).
//! Board e2e: the `cadence issue` CLI against a temp PM dir, and the
//! `cadence ui` HTTP server in-process.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod board_common;
use board_common::*;

use serde_json::json;
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;
use std::time::Instant;

/// `cli_env` run from `cwd` — `issue start` resolves the repo from it.
fn cli_dir(pm: &Path, state: &Path, cwd: &Path, args: &[&str]) -> (bool, Value) {
    cli_run::<&str>(pm, state, Some(cwd), args, &[])
}

/// `cli` detached so the daemon sees a provably-operator caller (the
/// CAD-291/431 seam, `OperatorOutput::operator_output` in
/// tests/common/mod.rs): `setsid -f` reparents it off this process's
/// ancestry and the env carries no agent identity. The runner script
/// is the shared `op::exec_script()` (CAD-554).
fn cli_op(pm: &Path, state: &Path, args: &[&str]) -> (bool, Value) {
    let dir = state.join(format!("opx-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let (script, spec, out) = (dir.join("run.py"), dir.join("spec.json"), dir.join("out"));
    std::fs::write(&script, op::exec_script()).unwrap();
    let mut env: std::collections::BTreeMap<String, String> = std::env::vars()
        .filter(|(k, _)| k != "CADENCE_ALIAS" && k != "CADENCE_ROLLOUT_AS")
        .collect();
    env.insert("CADENCE_PM_DIR".into(), pm.to_str().unwrap().into());
    env.insert(
        "PATH".into(),
        format!(
            "{}:{}",
            Path::new(bin()).parent().unwrap().display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    let mut argv = vec![
        bin().to_string(),
        "--state-dir".into(),
        state.to_str().unwrap().into(),
    ];
    argv.extend(args.iter().map(|a| a.to_string()));
    std::fs::write(
        &spec,
        json!({"argv": argv, "env": env,
               "cwd": std::env::current_dir().unwrap()})
        .to_string(),
    )
    .unwrap();
    let status = Command::new("setsid")
        .arg("-f")
        .arg("python3")
        .arg(&script)
        .arg(&spec)
        .arg(&out)
        .arg(std::process::id().to_string())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "setsid -f failed: {status}");
    let deadline = Instant::now() + Duration::from_secs(120);
    while !out.exists() {
        assert!(
            Instant::now() < deadline,
            "operator cli {args:?} never finished"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let rc: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    let text = {
        let stdout = std::fs::read_to_string(dir.join("out.stdout")).unwrap();
        if stdout.is_empty() {
            std::fs::read_to_string(dir.join("out.stderr")).unwrap()
        } else {
            stdout
        }
    };
    let _ = std::fs::remove_dir_all(&dir);
    (
        rc["rc"].as_i64() == Some(0),
        serde_json::from_str(text.trim()).unwrap_or(Value::String(text)),
    )
}

#[test]
fn issue_start_creates_records_and_is_idempotent() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(
        cli(
            &pm,
            &state,
            &["issue", "new", "Start Work", "--project", "demo"]
        )
        .0
    );
    let base_commits = commits(&pm);

    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let wt = repo.join(".cadence/wt/d-1-start-work");
    assert_eq!(out["issue"], "D-1");
    assert_eq!(out["worktree"].as_str().unwrap(), wt.to_str().unwrap());
    assert_eq!(out["branch"], "cadence/d-1-start-work");
    assert_eq!(
        out["repo"].as_str().unwrap(),
        repo.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(out["trailer"], "Issue: D-1");
    assert_eq!(out["created"], true);
    assert_eq!(out["base"]["sha"].as_str().unwrap().len(), 40);
    assert!(wt.is_dir());
    // The worktree is checked out on the new branch.
    assert_eq!(
        git(&wt, &["symbolic-ref", "--short", "HEAD"]).1,
        "cadence/d-1-start-work"
    );
    // `.cadence/` ignore line added exactly once.
    let ignore = std::fs::read_to_string(repo.join(".gitignore")).unwrap();
    assert_eq!(
        ignore.lines().filter(|l| l.trim() == ".cadence/").count(),
        1
    );

    // One tracker commit: `<ID>: start <branch>` + CAD-42 trailers.
    assert_eq!(commits(&pm), base_commits + 1);
    let (_, body) = git(&pm, &["log", "-1", "--format=%B"]);
    assert!(body.contains("D-1: start cadence/d-1-start-work"), "{body}");
    assert!(body.contains("Issue: D-1"), "{body}");
    assert!(body.contains("Actor: operator"), "{body}");

    // Front: both refs, status doing, owner = resolved actor.
    let front = std::fs::read_to_string(pm.join("demo/D-1/issue.md")).unwrap();
    assert!(front.contains("kind: branch"), "{front}");
    assert!(front.contains("cadence/d-1-start-work"), "{front}");
    assert!(front.contains("kind: worktree"), "{front}");
    assert!(front.contains("status: doing"), "{front}");
    assert!(front.contains("owner: operator"), "{front}");

    // Second run: idempotent — same answer, created:false, no commit.
    let (ok, out2) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{out2}");
    assert_eq!(out2["created"], false);
    assert_eq!(out2["worktree"], out["worktree"]);
    assert_eq!(commits(&pm), base_commits + 1);
}

#[test]
fn issue_start_repo_resolution() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(
        cli(
            &pm,
            &state,
            &["issue", "new", "Repo Pick", "--project", "demo"]
        )
        .0
    );
    let repo_s = repo.to_str().unwrap().to_string();

    // --repo explicit.
    let (ok, out) = cli(
        &pm,
        &state,
        &[
            "issue", "start", "D-1", "--repo", &repo_s, "--name", "explicit",
        ],
    );
    assert!(ok, "{out}");
    assert!(out["worktree"].as_str().unwrap().ends_with("d-1-explicit"));

    // cwd inside the repo resolves without --repo. Each case starts
    // its own issue: D-1 already has an open lane, and a second
    // `--name` on it is refused (CAD-274).
    for id in ["D-2", "D-3"] {
        assert!(cli(&pm, &state, &["issue", "new", id, "--project", "demo"]).0);
    }
    let (ok, out) = cli_dir(
        &pm,
        &state,
        &repo,
        &["issue", "start", "D-2", "--name", "from-cwd"],
    );
    assert!(ok, "{out}");
    assert!(out["worktree"].as_str().unwrap().ends_with("d-2-from-cwd"));

    // Single-repo fallback (cwd is the test process — not a project repo).
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-3", "--name", "single"]);
    assert!(ok, "{out}");

    // Ambiguous: a second repo on a second project refuses with the list.
    let repo2 = _tmp.path().join("repo2");
    std::fs::create_dir_all(&repo2).unwrap();
    assert!(git(&repo2, &["init", "-b", "main"]).0);
    git(&repo2, &["config", "user.email", "t@t"]);
    git(&repo2, &["config", "user.name", "t"]);
    std::fs::write(repo2.join("g"), "y").unwrap();
    git(&repo2, &["add", "-A"]);
    assert!(git(&repo2, &["commit", "-qm", "init"]).0);
    let repo2_s = repo2.to_str().unwrap().to_string();
    assert!(
        cli(
            &pm,
            &state,
            &[
                "issue", "project", "add", "two", "--prefix", "T", "--repo", &repo_s, "--repo",
                &repo2_s
            ]
        )
        .0
    );
    assert!(cli(&pm, &state, &["issue", "new", "Ambig", "--project", "two"]).0);
    let (ok, err) = cli(&pm, &state, &["issue", "start", "T-1"]);
    assert!(!ok);
    let msg = err["error"].as_str().unwrap();
    assert!(msg.contains("--repo"), "{msg}");
    assert!(msg.contains(&repo_s) && msg.contains(&repo2_s), "{msg}");

    // A real repo undeclared on the issue's project refuses, naming
    // the declared repos — code-commit discovery only walks those.
    let before = git(&pm, &["rev-list", "--count", "HEAD"]).1;
    let (ok, err) = cli(
        &pm,
        &state,
        &[
            "issue",
            "start",
            "D-1",
            "--repo",
            &repo2_s,
            "--name",
            "undeclared",
        ],
    );
    assert!(!ok);
    let msg = err["error"].as_str().unwrap();
    assert!(msg.contains("not declared"), "{msg}");
    assert!(msg.contains(&repo_s), "{msg}");
    assert!(msg.contains("project.yaml"), "{msg}");
    assert!(!repo2.join(".cadence/wt/d-1-undeclared").exists());
    assert!(!git(&repo2, &["rev-parse", "--verify", "cadence/d-1-undeclared"]).0);
    assert_eq!(git(&pm, &["rev-list", "--count", "HEAD"]).1, before);

    // Bad --repo path refuses; bad --base refuses.
    let (ok, err) = cli(
        &pm,
        &state,
        &["issue", "start", "D-1", "--repo", "/nonexistent"],
    );
    assert!(!ok && err["error"].as_str().unwrap().contains("git repository"));
    let (ok, err) = cli(
        &pm,
        &state,
        &[
            "issue", "start", "D-1", "--name", "bad-base", "--base", "nope-ref",
        ],
    );
    assert!(!ok && err["error"].as_str().unwrap().contains("does not resolve"));

    // --job without --pm/--spec is a clap error, not a start (usage
    // text, not JSON — plain-text read).
    let (ok, usage) = cli_raw(&pm, &state, &["issue", "start", "D-1", "--job"]);
    assert!(!ok && usage.contains("--pm"), "{usage}");
}

#[test]
fn issue_start_conflicting_branch_refused_and_status_owner() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(cli(&pm, &state, &["issue", "new", "Clash", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "set", "D-1", "status=ready"]).0);

    // A pre-existing branch under the same name refuses, naming the
    // branch and the recorded worktree — nothing created, no commit.
    assert!(git(&repo, &["branch", "cadence/d-1-clash"]).0);
    let before = commits(&pm);
    let (ok, err) = cli(&pm, &state, &["issue", "start", "D-1", "--name", "clash"]);
    assert!(!ok);
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains("cadence/d-1-clash") && msg.contains("worktree"),
        "{msg}"
    );
    assert!(!repo.join(".cadence/wt/d-1-clash").exists());
    assert_eq!(commits(&pm), before);
    git(&repo, &["branch", "-D", "cadence/d-1-clash"]);

    // ready -> doing, --owner wins when empty.
    let (ok, _) = cli(&pm, &state, &["issue", "start", "D-1", "--owner", "alice"]);
    assert!(ok);
    let front = std::fs::read_to_string(pm.join("demo/D-1/issue.md")).unwrap();
    assert!(
        front.contains("status: doing") && front.contains("owner: alice"),
        "{front}"
    );

    // review stays review; an existing owner is kept.
    assert!(
        cli(
            &pm,
            &state,
            &["issue", "new", "In Review", "--project", "demo"]
        )
        .0
    );
    assert!(
        cli(
            &pm,
            &state,
            &["issue", "set", "D-2", "status=review", "owner=bob"]
        )
        .0
    );
    // CAD-383: bob holds the review issue — another requester is
    // refused; bob's own start keeps the status and the owner.
    let (ok, err) = cli(&pm, &state, &["issue", "start", "D-2"]);
    assert!(
        !ok && err["error"].as_str().unwrap().contains("bob"),
        "{err}"
    );
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-2", "--by", "bob"]);
    assert!(ok, "{out}");
    let front = std::fs::read_to_string(pm.join("demo/D-2/issue.md")).unwrap();
    assert!(
        front.contains("status: review") && front.contains("owner: bob"),
        "{front}"
    );
}

// ---- CAD-55: `cadence dispatch` + `cadence issue finish` ----

#[test]
fn issue_finish_pairs_branch_when_dir_name_differs() {
    // CAD-166: a worktree whose dir name is not its branch name (moved,
    // hand-recorded or adopted) must still finish its branch.
    let (_tmp, pm, state, repo) = start_fx();
    assert!(cli(&pm, &state, &["issue", "new", "Moved", "--project", "demo"]).0);
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let old = repo.join(".cadence/wt/d-1-moved");
    let new = repo.join(".cadence/wt/elsewhere");
    let moved = git(
        &repo,
        &[
            "worktree",
            "move",
            old.to_str().unwrap(),
            new.to_str().unwrap(),
        ],
    );
    assert!(moved.0, "{}", moved.1);
    let file = pm.join("demo/D-1/issue.md");
    let front = std::fs::read_to_string(&file).unwrap();
    let old_s = old.canonicalize().unwrap_or(old.clone());
    let recorded = if front.contains(old.to_str().unwrap()) {
        old.to_str().unwrap().to_string()
    } else {
        old_s.to_str().unwrap().to_string()
    };
    assert!(front.contains(&recorded), "{front}");
    std::fs::write(&file, front.replace(&recorded, new.to_str().unwrap())).unwrap();
    // Explicit identity: CI runners have no global git user.
    let committed = git(
        &pm,
        &[
            "-c",
            "user.name=hand",
            "-c",
            "user.email=hand@h",
            "commit",
            "-qam",
            "D-1: hand-move worktree ref",
        ],
    );
    assert!(committed.0, "{}", committed.1);
    land(&repo, &new, "moved.txt");

    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(ok, "{out}");
    assert_eq!(out["removed_worktree"], true, "{out}");
    assert_eq!(out["deleted_branch"], true, "branch must not leak: {out}");
    assert!(!new.exists());
    assert!(
        !git(
            &repo,
            &["rev-parse", "--verify", "--quiet", "cadence/d-1-moved"]
        )
        .0
    );
}

/// Rewrite `id`'s issue.md by hand and commit it — the tracker is plain
/// markdown any agent can edit, bypassing every CLI write check.
fn hand_edit(pm: &Path, id: &str, edit: impl Fn(String) -> String) {
    let file = pm.join(format!("demo/{id}/issue.md"));
    let before = std::fs::read_to_string(&file).unwrap();
    let after = edit(before.clone());
    assert_ne!(before, after, "hand edit changed nothing");
    std::fs::write(&file, after).unwrap();
    let committed = git(
        pm,
        &[
            "-c",
            "user.name=hand",
            "-c",
            "user.email=hand@h",
            "commit",
            "-qam",
            &format!("{id}: hand edit"),
        ],
    );
    assert!(committed.0, "{}", committed.1);
}

/// The recorded spelling of `path` in `id`'s issue.md — as given or
/// canonicalized, whichever the writer stored.
fn recorded_path(pm: &Path, id: &str, path: &Path) -> String {
    let front = std::fs::read_to_string(pm.join(format!("demo/{id}/issue.md"))).unwrap();
    let canon = path.canonicalize().unwrap_or(path.to_path_buf());
    [path, canon.as_path()]
        .iter()
        .map(|p| p.to_str().unwrap().to_string())
        .find(|p| front.contains(&format!("path: {p}\n")))
        .unwrap_or_else(|| panic!("{} not recorded: {front}", path.display()))
}

/// CAD-144: a ref value beginning with `-` reads as a git option
/// (`--upload-pack=<cmd>` executes over ssh/file remotes). `issue ref`
/// and `issue start` refuse to write one, and nothing is committed.
#[test]
fn issue_ref_and_start_refuse_option_like_values() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(cli(&pm, &state, &["issue", "new", "Lane", "--project", "demo"]).0);
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let before = commits(&pm);
    for value in ["-x", "--upload-pack=touch /tmp/cad144-never"] {
        for kind in ["branch", "worktree", "note"] {
            let (ok, err) = cli(&pm, &state, &["issue", "ref", "D-1", kind, "--", value]);
            assert!(!ok, "{kind} {value} was written: {err}");
            assert!(
                err["error"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("begins with '-'"),
                "{err}"
            );
        }
    }
    assert_eq!(commits(&pm), before, "a refused ref commits nothing");

    // `issue start` re-records the lane's branch from the tracker and
    // the checkout: a hand-recorded `-x` checked out in the lane must
    // not be written back.
    let wt = repo.join(".cadence/wt/d-1-lane");
    assert!(git(&wt, &["update-ref", "refs/heads/-x", "HEAD"]).0);
    assert!(git(&wt, &["symbolic-ref", "HEAD", "refs/heads/-x"]).0);
    hand_edit(&pm, "D-1", |f| {
        f.replace("path: cadence/d-1-lane\n", "path: '-x'\n")
    });
    let before = commits(&pm);
    let (ok, err) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(!ok, "start re-recorded '-x': {err}");
    assert!(
        err["error"]
            .as_str()
            .unwrap_or_default()
            .contains("begins with '-'"),
        "{err}"
    );
    assert_eq!(commits(&pm), before, "a refused start commits nothing");
}

/// CAD-144: finish refuses an option-like ref it reads from a
/// hand-edited tracker before any git command sees it — with a file
/// remote, `fetch origin --upload-pack=<cmd>` would run the command.
#[test]
fn issue_finish_refuses_option_like_refs() {
    let (tmp, pm, state, repo) = start_fx();
    let bare = tmp.path().join("remote.git");
    assert!(git(tmp.path(), &["init", "-q", "--bare", "remote.git"]).0);
    assert!(git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]).0);
    assert!(
        cli(
            &pm,
            &state,
            &["issue", "new", "Hostile", "--project", "demo"]
        )
        .0
    );
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let wt = repo.join(".cadence/wt/d-1-hostile");
    let wt_s = recorded_path(&pm, "D-1", &wt);
    let marker = tmp.path().join("upload-pack-ran");
    // The worktree ref closed, the branch ref rewritten: finish reads
    // the lone open branch ref as its target.
    let hostile = format!("--upload-pack=touch {}", marker.display());
    hand_edit(&pm, "D-1", |f| {
        f.replace(
            "path: cadence/d-1-hostile\n",
            &format!("path: '{hostile}'\n"),
        )
        .replace(
            &format!("path: {wt_s}\n"),
            &format!("path: {wt_s}\n  closed: true\n"),
        )
    });
    let before = commits(&pm);
    let (ok, err) = cli(
        &pm,
        &state,
        &["issue", "finish", "D-1", "--remote", "--force"],
    );
    assert!(!ok, "finish ran with '{hostile}': {err}");
    assert!(
        err["error"]
            .as_str()
            .unwrap_or_default()
            .contains("begins with '-'"),
        "{err}"
    );
    assert!(!marker.exists(), "--upload-pack reached git");
    assert_eq!(commits(&pm), before, "a refused finish commits nothing");
    // The same refusal for a plain `-x`, and for a worktree ref.
    hand_edit(&pm, "D-1", |f| f.replace(&format!("'{hostile}'"), "'-x'"));
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-1", "--force"]);
    assert!(
        !ok && err["error"].as_str().unwrap_or_default().contains("'-x'"),
        "{err}"
    );
    hand_edit(&pm, "D-1", |f| {
        f.replace(&format!("path: {wt_s}\n  closed: true\n"), "path: '-wt'\n")
    });
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-1", "--force"]);
    assert!(
        !ok && err["error"].as_str().unwrap_or_default().contains("'-wt'"),
        "{err}"
    );
    assert!(wt.is_dir(), "nothing was removed");
}

/// CAD-145: a branch kept with `--keep-branch` keeps its branch ref
/// open — the surviving work stays on the board and finishable — while
/// the worktree ref closes. A later finish deletes the merged branch
/// and closes the ref.
#[test]
fn issue_finish_keep_branch_leaves_its_ref_open() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(cli(&pm, &state, &["issue", "new", "Keep", "--project", "demo"]).0);
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let wt = repo.join(".cadence/wt/d-1-keep");
    land(&repo, &wt, "keep.txt");
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-1", "--keep-branch"]);
    assert!(ok, "{out}");
    assert_eq!(out["removed_worktree"], true, "{out}");
    assert_eq!(out["deleted_branch"], false, "{out}");
    assert!(
        git(
            &repo,
            &["rev-parse", "--verify", "--quiet", "cadence/d-1-keep"]
        )
        .0
    );
    let refs = refs_of(&pm, &state, "D-1");
    let closed = |kind: &str| {
        refs.iter()
            .find(|r| r["kind"] == kind)
            .map(|r| r["closed"] == true)
            .unwrap_or_else(|| panic!("no {kind} ref: {refs:?}"))
    };
    assert!(closed("worktree"), "the worktree ref closes: {refs:?}");
    assert!(
        !closed("branch"),
        "the kept branch's ref stays open: {refs:?}"
    );

    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(ok, "{out}");
    assert_eq!(out["finished"], true, "{out}");
    assert_eq!(out["deleted_branch"], true, "{out}");
    let refs = refs_of(&pm, &state, "D-1");
    assert!(
        refs.iter()
            .filter(|r| r["kind"] == "branch" || r["kind"] == "worktree")
            .all(|r| r["closed"] == true),
        "{refs:?}"
    );
}

/// CAD-265: the safety half of CAD-166's pairing — a worktree whose dir
/// differs from its branch, with a checked-out branch that is NOT a
/// recorded ref, finishes without deleting that branch, locally or on
/// the remote. It is foreign work (an adopted checkout, ADR-0003).
#[test]
fn issue_finish_never_deletes_an_unrecorded_checked_out_branch() {
    let (tmp, pm, state, repo) = start_fx();
    let bare = tmp.path().join("remote.git");
    assert!(git(tmp.path(), &["init", "-q", "--bare", "remote.git"]).0);
    assert!(git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]).0);
    assert!(cli(&pm, &state, &["issue", "new", "Moved", "--project", "demo"]).0);
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let old = repo.join(".cadence/wt/d-1-moved");
    let new = repo.join(".cadence/wt/elsewhere");
    let old_s = recorded_path(&pm, "D-1", &old);
    let moved = git(&repo, &["worktree", "move", &old_s, new.to_str().unwrap()]);
    assert!(moved.0, "{}", moved.1);
    hand_edit(&pm, "D-1", |f| f.replace(&old_s, new.to_str().unwrap()));
    // Foreign work: an unrecorded branch with a commit of its own,
    // merged and pushed — every rule would call it deletable.
    assert!(git(&new, &["checkout", "-q", "-b", "foreign"]).0);
    land(&repo, &new, "foreign.txt");
    assert!(git(&repo, &["push", "-q", "origin", "foreign"]).0);
    let tip = git(&repo, &["rev-parse", "foreign"]).1;

    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-1", "--remote"]);
    assert!(ok, "{out}");
    assert_eq!(out["branch"], "", "the foreign branch never pairs: {out}");
    assert_eq!(out["deleted_branch"], false, "{out}");
    assert_eq!(out["remote_deleted"], false, "{out}");
    assert_eq!(
        git(&repo, &["rev-parse", "--verify", "--quiet", "foreign"]).1,
        tip
    );
    assert_eq!(
        git(
            &bare,
            &["rev-parse", "--verify", "--quiet", "refs/heads/foreign"]
        )
        .1,
        tip,
        "the remote copy survives"
    );
}

/// `issue finish` without a reachable daemon refuses rather than
/// guesses; `--force` overrides and is recorded; a second finish is a
/// no-op; an issue without refs has nothing to finish.
#[test]
fn issue_finish_daemon_down_force_and_idempotent() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(cli(&pm, &state, &["issue", "new", "Done", "--project", "demo"]).0);
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let wt = repo.join(".cadence/wt/d-1-done");
    assert!(wt.is_dir());

    // A stale socket is a daemon that was there and stopped answering
    // — owner 'operator' can't be checked and finish refuses rather
    // than guessing.
    std::fs::write(state.join("cadence.sock"), "stale").unwrap();
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains("operator") && msg.contains("reachable"),
        "{msg}"
    );
    assert!(wt.is_dir(), "refused finish must not remove the worktree");

    // --force overrides and records it; the tracker commit carries
    // the Forced trailer and closes both refs as history.
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-1", "--force"]);
    assert!(ok, "{out}");
    assert_eq!(out["finished"], true);
    assert_eq!(out["removed_worktree"], true);
    assert_eq!(out["deleted_branch"], true);
    assert!(
        out["overrode"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o.as_str().unwrap().contains("unreachable")),
        "{out}"
    );
    assert!(!wt.exists());
    assert!(
        !git(
            &repo,
            &["rev-parse", "--verify", "--quiet", "cadence/d-1-done"]
        )
        .0
    );
    let body = git(&pm, &["log", "-1", "--format=%B"]).1;
    assert!(
        body.contains("D-1: finish cadence/d-1-done") && body.contains("Forced: true"),
        "{body}"
    );
    let front = std::fs::read_to_string(pm.join("demo/D-1/issue.md")).unwrap();
    assert!(front.contains("closed: true"), "{front}");
    assert!(front.contains("status: doing"), "status untouched: {front}");

    // A cleanly stopped daemon removes its socket — that means "no
    // agents", not "unreachable": the /proc and pane scans carry the
    // check and a clean merged worktree finishes without --force.
    std::fs::remove_file(state.join("cadence.sock")).unwrap();
    assert!(cli(&pm, &state, &["issue", "new", "Idle", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-2"]).0);
    land(&repo, &repo.join(".cadence/wt/d-2-idle"), "idle.txt");
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-2"]);
    assert!(
        ok && out["finished"] == true && out["overrode"] == json!([]),
        "no daemon at all must not block a clean finish: {out}"
    );

    // Second finish is a no-op, not an error.
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(ok, "{out}");
    assert_eq!(out["finished"], false);

    // An issue never started has nothing to finish.
    assert!(cli(&pm, &state, &["issue", "new", "Never", "--project", "demo"]).0);
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-3"]);
    assert!(!ok && err["error"].as_str().unwrap().contains("nothing to finish"));

    // --keep-branch leaves the local branch; the worktree ref closes.
    assert!(cli(&pm, &state, &["issue", "new", "Keep", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-4"]).0);
    let (ok, out) = cli(
        &pm,
        &state,
        &["issue", "finish", "D-4", "--force", "--keep-branch"],
    );
    assert!(
        ok && out["kept_branch"] == true && out["deleted_branch"] == false,
        "{out}"
    );
    assert!(
        git(
            &repo,
            &["rev-parse", "--verify", "--quiet", "cadence/d-4-keep"]
        )
        .0
    );
}

/// With no owner recorded (hand-edited history), the owner check is
/// skipped; the bound-message enumeration still needs a daemon that
/// answers (an unanswerable enumeration refuses — a task-bound
/// kickoff could hide anywhere). The worktree-side guards stay
/// observable: a dirty worktree refuses listing the files, an
/// unmerged+unpushed branch refuses, and finish succeeds once the
/// branch is merged.
#[test]
fn issue_finish_dirty_and_unmerged_refusals() {
    let (_tmp, pm, state, repo) = start_fx();
    let _d = UiDaemon::start_on(state.clone());
    assert!(
        cli(
            &pm,
            &state,
            &["issue", "new", "Guards", "--project", "demo"]
        )
        .0
    );
    assert!(cli(&pm, &state, &["issue", "start", "D-1"]).0);
    let wt = repo.join(".cadence/wt/d-1-guards");

    // Strip the owner so the daemon check is skipped — a tracker
    // commit of its own so finish commits nothing extra.
    let md = pm.join("demo/D-1/issue.md");
    let front = std::fs::read_to_string(&md).unwrap();
    std::fs::write(&md, front.replace("owner: operator\n", "")).unwrap();
    git(&pm, &["add", "-A"]);
    git(
        &pm,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "setup",
        ],
    );
    let before = commits(&pm);

    // Dirty: the refusal lists the uncommitted paths.
    std::fs::write(wt.join("scratch.txt"), "wip").unwrap();
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains("uncommitted") && msg.contains("scratch.txt"),
        "{msg}"
    );
    assert!(wt.is_dir());
    assert_eq!(commits(&pm), before);

    // Committed but unmerged and unpushed: the work would be lost.
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-qm", "wip"]);
    idle(&wt);
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(!ok, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains("neither merged"),
        "{err}"
    );
    assert!(wt.is_dir());

    // Merge into the repo's default branch → the guards pass and the
    // cleanup lands in one tracker commit.
    git(&repo, &["merge", "-q", "cadence/d-1-guards"]);
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(ok, "{out}");
    assert_eq!(out["finished"], true);
    assert_eq!(out["overrode"], json!([]));
    assert!(!wt.exists());
    assert!(
        !git(
            &repo,
            &["rev-parse", "--verify", "--quiet", "cadence/d-1-guards"]
        )
        .0
    );
    assert_eq!(commits(&pm), before + 1);

    // Unmerged but pushed: a plain finish (no --remote) leaves the
    // remote alone, so the pushed copy IS the survivability evidence
    // — the local branch is deleted, not kept.
    assert!(cli(&pm, &state, &["issue", "new", "Pushd", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-2"]).0);
    assert!(cli(&pm, &state, &["issue", "set", "D-2", "owner="]).0);
    let wt2 = repo.join(".cadence/wt/d-2-pushd");
    std::fs::write(wt2.join("p.txt"), "x").unwrap();
    git(&wt2, &["add", "-A"]);
    git(&wt2, &["commit", "-qm", "pushed work"]);
    idle(&wt2);
    let tip = git(&repo, &["rev-parse", "cadence/d-2-pushd"]).1;
    git(
        &repo,
        &[
            "update-ref",
            "refs/remotes/origin/cadence/d-2-pushd",
            tip.trim(),
        ],
    );
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-2"]);
    assert!(
        ok && out["finished"] == true && out["deleted_branch"] == true,
        "pushed evidence alone still deletes the local on a plain finish: {out}"
    );
    assert!(!wt2.exists());
}

/// Drop the recorded owner (and commit the edit) so `issue finish`
/// skips the daemon-owner check entirely — board tests run with no
/// daemon, and an owner would make finish refuse "unreachable".
fn strip_owner(pm: &Path, id: &str) {
    let md = pm.join(format!("demo/{id}/issue.md"));
    let front = std::fs::read_to_string(&md).unwrap();
    std::fs::write(&md, front.replace("owner: operator\n", "")).unwrap();
    git(pm, &["add", "-A"]);
    git(
        pm,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "strip owner",
        ],
    );
}

/// CAD-275: give a fresh lane real work — one commit, fast-forward
/// merged into the repo's default branch — then idle it, so finish
/// sees a started, merged, untouched lane.
fn land(repo: &Path, wt: &Path, file: &str) {
    std::fs::write(wt.join(file), format!("{file}\n")).unwrap();
    assert!(git(wt, &["add", "-A"]).0);
    assert!(git(wt, &["commit", "-qm", &format!("work {file}")]).0);
    let branch = git(wt, &["symbolic-ref", "--short", "HEAD"]).1;
    assert!(
        git(repo, &["merge", "-q", "--ff-only", &branch]).0,
        "merge {branch}"
    );
    idle(wt);
}

/// CAD-64: a branch is "merged" when its work is on the default
/// branch however it got there — a squash merge (combined patch) or
/// cherry-picked commits both count, while an extra unmerged commit
/// still refuses. Ignored paths (the ui/node_modules build symlink)
/// never count as dirty; real untracked files do and are listed.
#[test]
fn issue_finish_squash_cherry_and_ignored() {
    let (_tmp, pm, state, repo) = start_fx();
    let _d = UiDaemon::start_on(state.clone());
    // The build-symlink rule lives in the repo's .gitignore, like the
    // real cadence repo.
    std::fs::write(repo.join(".gitignore"), "/ui/node_modules\n").unwrap();
    git(&repo, &["add", ".gitignore"]);
    git(&repo, &["commit", "-qm", "ignore rules"]);

    // D-1: two branch commits squash-merged into one → "patch".
    assert!(cli(&pm, &state, &["issue", "new", "Sq", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-1"]).0);
    let wt = repo.join(".cadence/wt/d-1-sq");
    // Newline-terminated, multi-line, one file edited twice — the
    // shape of real code, whose diff ends in a newline.
    std::fs::write(wt.join("a.txt"), "1\n").unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-qm", "part a"]);
    std::fs::write(wt.join("a.txt"), "1\n2\n").unwrap();
    std::fs::write(wt.join("b.txt"), "b1\nb2\n").unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-qm", "part b"]);
    git(&repo, &["merge", "--squash", "-q", "cadence/d-1-sq"]);
    git(&repo, &["commit", "-qm", "D-1: sq (#1)"]);
    idle(&wt);
    strip_owner(&pm, "D-1");
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(ok, "{out}");
    assert_eq!(out["finished"], true);
    assert_eq!(out["merged_by"], "patch", "{out}");
    assert_eq!(out["overrode"], json!([]));
    assert!(!wt.exists());

    // D-2: the single branch commit cherry-picked onto main →
    // "cherry" (patch-equivalent, different sha).
    assert!(cli(&pm, &state, &["issue", "new", "Ch", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-2"]).0);
    let wt = repo.join(".cadence/wt/d-2-ch");
    std::fs::write(wt.join("c.txt"), "c1\nc2\n").unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-qm", "cherry work"]);
    // -x records the source sha in the message — a different commit
    // object with the same patch-id, like a real cherry-pick merge.
    git(&repo, &["cherry-pick", "-x", "cadence/d-2-ch"]);
    idle(&wt);
    strip_owner(&pm, "D-2");
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-2"]);
    assert!(ok && out["merged_by"] == "cherry", "{out}");

    // D-3: squash-merged, then an extra commit only on the branch —
    // the work isn't all upstream, so finish still refuses.
    assert!(cli(&pm, &state, &["issue", "new", "Ex", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-3"]).0);
    let wt = repo.join(".cadence/wt/d-3-ex");
    std::fs::write(wt.join("d.txt"), "d1\nd2\n").unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-qm", "part d"]);
    git(&repo, &["merge", "--squash", "-q", "cadence/d-3-ex"]);
    git(&repo, &["commit", "-qm", "D-3 part (#3)"]);
    std::fs::write(wt.join("late.txt"), "late\n").unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-qm", "unmerged late work"]);
    idle(&wt);
    strip_owner(&pm, "D-3");
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-3"]);
    assert!(!ok, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains("neither merged"),
        "{err}"
    );
    assert!(wt.is_dir());
    git(&repo, &["branch", "-D", "cadence/d-3-ex"]);
    let wts = wt.display().to_string();
    git(&repo, &["worktree", "remove", "--force", &wts]);

    // D-4: the ui/node_modules build symlink alone is ignored (`!!`)
    // and never blocks — the branch was fast-forwarded into main, so
    // "ancestry". (An unstarted lane is never merged — CAD-275 — so it
    // lands one commit first.)
    assert!(cli(&pm, &state, &["issue", "new", "Sy", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-4"]).0);
    let wt = repo.join(".cadence/wt/d-4-sy");
    land(&repo, &wt, "sy.txt");
    std::fs::create_dir_all(wt.join("ui")).unwrap();
    std::fs::create_dir_all(wt.join("real_nm")).unwrap();
    std::os::unix::fs::symlink("../real_nm", wt.join("ui/node_modules")).unwrap();
    idle(&wt);
    let status = git(&wt, &["status", "--porcelain", "--ignored"]).1;
    assert!(status.contains("!!"), "{status}");
    strip_owner(&pm, "D-4");
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-4"]);
    assert!(ok && out["finished"] == true, "{out}");
    assert_eq!(out["merged_by"], "ancestry");
    assert!(!wt.exists());

    // D-5: a real untracked file still refuses and is listed — while
    // the ignored symlink alongside it is not.
    assert!(cli(&pm, &state, &["issue", "new", "Re", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-5"]).0);
    let wt = repo.join(".cadence/wt/d-5-re");
    land(&repo, &wt, "re.txt");
    std::fs::create_dir_all(wt.join("ui")).unwrap();
    std::fs::create_dir_all(wt.join("real_nm")).unwrap();
    std::os::unix::fs::symlink("../real_nm", wt.join("ui/node_modules")).unwrap();
    std::fs::write(wt.join("real.txt"), "x").unwrap();
    strip_owner(&pm, "D-5");
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-5"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(msg.contains("real.txt"), "{msg}");
    assert!(
        !msg.contains("node_modules") && !msg.contains("!!"),
        "{msg}"
    );
    assert!(wt.is_dir());
    std::fs::remove_file(wt.join("real.txt")).unwrap();
    idle(&wt);
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-5"]);
    assert!(ok && out["finished"] == true, "{out}");

    // D-6 / D-7: two-commit squashes of a binary file and of a file
    // with no trailing newline — both diff shapes still apply → "patch".
    for (id, name, first, second) in [
        (
            "D-6",
            "Bi",
            &b"\x00\x01\xff\n\x00"[..],
            &b"\x00\x02\xfe\x00"[..],
        ),
        ("D-7", "Nn", &b"x\ny"[..], &b"x\ny\nz"[..]),
    ] {
        assert!(cli(&pm, &state, &["issue", "new", name, "--project", "demo"]).0);
        assert!(cli(&pm, &state, &["issue", "start", id]).0);
        let branch = format!("cadence/{}-{}", id.to_lowercase(), name.to_lowercase());
        let wt = repo.join(format!(
            ".cadence/wt/{}-{}",
            id.to_lowercase(),
            name.to_lowercase()
        ));
        std::fs::write(wt.join("f.bin"), first).unwrap();
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-qm", "first"]);
        std::fs::write(wt.join("f.bin"), second).unwrap();
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-qm", "second"]);
        git(&repo, &["merge", "--squash", "-q", &branch]);
        git(&repo, &["commit", "-qm", &format!("{id}: squash")]);
        idle(&wt);
        strip_owner(&pm, id);
        let (ok, out) = cli(&pm, &state, &["issue", "finish", id]);
        assert!(ok, "{id}: {out}");
        assert_eq!(out["merged_by"], "patch", "{id}: {out}");
        assert!(!wt.exists(), "{id}");
    }
}

/// CAD-64: with a GitHub origin and `gh` on PATH, a merged PR with
/// the branch as head counts as merged (`merged_by: "pr"`); a `gh`
/// that reports nothing merged leaves the branch refused.
#[test]
fn issue_finish_pr_merge_via_gh() {
    let (_tmp, pm, state, repo) = start_fx();
    let _d = UiDaemon::start_on(state.clone());
    git(
        &repo,
        &["remote", "add", "origin", "https://github.com/o/r.git"],
    );
    // Fake gh — controlled JSON on stdout, ignores its arguments.
    let fakebin = _tmp.path().join("fakebin");
    std::fs::create_dir_all(&fakebin).unwrap();
    let gh = fakebin.join("gh");
    let set_gh = |body: &str| {
        std::fs::write(&gh, format!("#!/bin/sh\nprintf '%s' '{body}'\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let path = format!(
        "{}:{}:{}",
        fakebin.display(),
        Path::new(bin()).parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    // D-1: branch commits not on main, nothing pushed — but gh says
    // a PR whose recorded head IS this tip is MERGED → finished,
    // merged_by "pr". A bare name match proves nothing (CAD-106):
    // first answer with a head oid that does not cover the tip.
    assert!(cli(&pm, &state, &["issue", "new", "Pr", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-1"]).0);
    let wt = repo.join(".cadence/wt/d-1-pr");
    std::fs::write(wt.join("p.txt"), "p").unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-qm", "pr work"]);
    idle(&wt);
    let tip = git(&repo, &["rev-parse", "cadence/d-1-pr"]).1;
    strip_owner(&pm, "D-1");
    set_gh("[{\"number\":7,\"headRefOid\":\"0000000000000000000000000000000000000000\",\"baseRefName\":\"main\"}]");
    let (ok, err) = cli_env(
        &pm,
        &state,
        &["issue", "finish", "D-1"],
        &[("PATH", path.as_str())],
    );
    assert!(!ok, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains("neither merged"),
        "a stale headRefOid must not prove the merge: {err}"
    );
    assert!(wt.is_dir());
    set_gh(&format!(
        "[{{\"number\":7,\"headRefOid\":\"{tip}\",\"baseRefName\":\"main\"}}]"
    ));
    let (ok, out) = cli_env(
        &pm,
        &state,
        &["issue", "finish", "D-1"],
        &[("PATH", path.as_str())],
    );
    assert!(ok, "{out}");
    assert_eq!(out["merged_by"], "pr", "{out}");
    assert!(!wt.exists());

    // D-2: gh reports nothing merged → the branch is still refused.
    assert!(cli(&pm, &state, &["issue", "new", "No", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-2"]).0);
    let wt = repo.join(".cadence/wt/d-2-no");
    std::fs::write(wt.join("n.txt"), "n").unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-qm", "unmerged"]);
    idle(&wt);
    strip_owner(&pm, "D-2");
    set_gh("[]");
    let (ok, err) = cli_env(
        &pm,
        &state,
        &["issue", "finish", "D-2"],
        &[("PATH", path.as_str())],
    );
    assert!(!ok, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains("neither merged"),
        "{err}"
    );
    assert!(wt.is_dir());
}

// ---- CAD-274: one issue, one open lane ----

/// The refs of `id` from `issue show --json`.
fn refs_of(pm: &Path, state: &Path, id: &str) -> Vec<Value> {
    let (ok, show) = cli(pm, state, &["issue", "show", id, "--json"]);
    assert!(ok, "{show}");
    show["refs"].as_array().cloned().unwrap_or_default()
}

/// The `cadence/*` branches in `repo`.
fn lane_branches(repo: &Path) -> Vec<String> {
    git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads/cadence/",
        ],
    )
    .1
    .lines()
    .map(str::to_string)
    .collect()
}

/// CAD-274: with exactly one open worktree ref, `issue start` reuses
/// that lane whether or not `--name` is given — even after the title
/// (and so the default slug) changed, the CAD-270 repro. It re-applies
/// the cargo target, fixes a stale recorded one in one commit, and
/// mints no worktree, branch or ref. A `--name` for a different slug
/// is refused, naming the open lane.
#[test]
fn issue_start_reuses_the_one_open_lane() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(
        cli(
            &pm,
            &state,
            &["issue", "new", "Skill Kickoff Lessons", "--project", "demo"]
        )
        .0
    );
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let wt = out["worktree"].as_str().unwrap().to_string();
    assert!(wt.ends_with("d-1-skill-kickoff-lessons"), "{out}");
    assert!(
        cli(
            &pm,
            &state,
            &[
                "issue",
                "set",
                "D-1",
                "title=Cadence skill kickoff checklist"
            ]
        )
        .0
    );
    // A stale recorded cargo target — the re-start's job is to fix it.
    let md = pm.join("demo/D-1/issue.md");
    let front = std::fs::read_to_string(&md).unwrap();
    let planted = front.replace(
        &format!("path: {wt}\n"),
        &format!("path: {wt}\n  cargo_target: /stale/target\n"),
    );
    assert_ne!(planted, front, "fixture: {front}");
    std::fs::write(&md, planted).unwrap();
    assert!(
        git(
            &pm,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qam",
                "stale target"
            ]
        )
        .0
    );
    let refs_before = refs_of(&pm, &state, "D-1").len();

    let before = commits(&pm);
    let (ok, again) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{again}");
    assert_eq!(again["created"], false, "{again}");
    assert_eq!(again["worktree"], out["worktree"], "{again}");
    assert_eq!(again["branch"], out["branch"], "{again}");
    assert_eq!(commits(&pm), before + 1, "one commit fixes the target");
    let (_, body) = git(&pm, &["log", "-1", "--format=%B"]);
    assert!(body.contains("(refs refreshed)"), "{body}");
    let refs = refs_of(&pm, &state, "D-1");
    assert_eq!(refs.len(), refs_before, "no new ref: {refs:?}");
    assert!(
        refs.iter().all(|r| r["cargo_target"].is_null()),
        "the stale target is corrected (not a cargo repo): {refs:?}"
    );
    assert_eq!(
        lane_branches(&repo),
        vec!["cadence/d-1-skill-kickoff-lessons"]
    );
    let lanes = std::fs::read_dir(repo.join(".cadence/wt")).unwrap().count();
    assert_eq!(lanes, 1, "no second worktree");

    // Idempotent now; `--name` naming the open slug reuses it too.
    let before = commits(&pm);
    for args in [
        &["issue", "start", "D-1"][..],
        &["issue", "start", "D-1", "--name", "skill-kickoff-lessons"][..],
    ] {
        let (ok, out) = cli(&pm, &state, args);
        assert!(ok && out["created"] == false, "{args:?}: {out}");
        assert_eq!(out["worktree"].as_str().unwrap(), wt, "{args:?}");
    }
    assert_eq!(commits(&pm), before);

    // A different --name would fork the work — refused, nothing made.
    let (ok, err) = cli(&pm, &state, &["issue", "start", "D-1", "--name", "other"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains("'skill-kickoff-lessons'") && msg.contains(&wt) && msg.contains("--worktree"),
        "{msg}"
    );
    assert!(!repo.join(".cadence/wt/d-1-other").exists());
    assert_eq!(
        lane_branches(&repo),
        vec!["cadence/d-1-skill-kickoff-lessons"]
    );
    assert_eq!(commits(&pm), before);
}

/// CAD-274: two or more open worktree refs make `issue start` and a
/// bare `issue finish` refuse, listing them; `issue finish <ID>
/// --worktree <path>` closes exactly one — for a lane whose dir (and
/// branch) are already gone it only marks the refs closed, in one
/// tracker commit, keeping any surviving branch. The remaining lane
/// is then reused by `issue start`.
#[test]
fn issue_several_open_lanes_refuse_and_finish_one_by_path() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(cli(&pm, &state, &["issue", "new", "Lanes", "--project", "demo"]).0);
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let live = out["worktree"].as_str().unwrap().to_string();
    // What the pre-CAD-274 start left behind: an extra pair whose dir
    // and branch were removed by hand, and one whose dir is gone but
    // whose branch still holds unmerged work.
    let ghost = repo.join(".cadence/wt/d-1-ghost");
    let ghost_s = ghost.to_str().unwrap().to_string();
    let stale = repo.join(".cadence/wt/d-1-stale");
    let stale_s = stale.to_str().unwrap().to_string();
    assert!(
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "cadence/d-1-stale", &stale_s]
        )
        .0
    );
    std::fs::write(stale.join("s.txt"), "s\n").unwrap();
    assert!(git(&stale, &["add", "-A"]).0);
    assert!(git(&stale, &["commit", "-qm", "stale work"]).0);
    std::fs::remove_dir_all(&stale).unwrap();
    assert!(git(&repo, &["worktree", "prune"]).0);
    for (kind, target) in [
        ("branch", "cadence/d-1-ghost"),
        ("worktree", ghost_s.as_str()),
        ("branch", "cadence/d-1-stale"),
        ("worktree", stale_s.as_str()),
    ] {
        let (ok, out) = cli(&pm, &state, &["issue", "ref", "D-1", kind, target]);
        assert!(ok, "{out}");
    }

    let before = commits(&pm);
    for args in [
        &["issue", "start", "D-1"][..],
        &["issue", "finish", "D-1"][..],
    ] {
        let (ok, err) = cli(&pm, &state, args);
        assert!(!ok, "{args:?}: {err}");
        let msg = err["error"].as_str().unwrap();
        assert!(
            msg.contains("3 open worktree refs")
                && msg.contains(&live)
                && msg.contains(&ghost_s)
                && msg.contains(&stale_s)
                && msg.contains("--worktree"),
            "{args:?}: {msg}"
        );
    }
    assert_eq!(commits(&pm), before);

    // Dir and branch both gone: the refs close, nothing else moves.
    let (ok, out) = cli(
        &pm,
        &state,
        &["issue", "finish", "D-1", "--worktree", &ghost_s],
    );
    assert!(ok, "{out}");
    assert!(
        out["finished"] == true
            && out["refs_only"] == true
            && out["removed_worktree"] == false
            && out["deleted_branch"] == false
            && out["overrode"] == json!([]),
        "{out}"
    );
    assert_eq!(commits(&pm), before + 1, "one tracker commit");
    let (_, body) = git(&pm, &["log", "-1", "--format=%B"]);
    assert!(body.contains("D-1: finish cadence/d-1-ghost"), "{body}");
    // Dir gone, branch alive with unmerged work: refs close, the
    // branch is kept — nothing is deleted, so nothing is lost.
    let (ok, out) = cli(
        &pm,
        &state,
        &["issue", "finish", "D-1", "--worktree", &stale_s],
    );
    assert!(ok, "{out}");
    assert!(
        out["finished"] == true && out["deleted_branch"] == false,
        "{out}"
    );
    assert!(
        out["branch_note"]
            .as_str()
            .unwrap_or_default()
            .contains("refs closed only"),
        "{out}"
    );
    assert!(
        git(
            &repo,
            &["rev-parse", "--verify", "--quiet", "cadence/d-1-stale"]
        )
        .0
    );
    assert_eq!(commits(&pm), before + 2);
    let refs = refs_of(&pm, &state, "D-1");
    for r in &refs {
        let path = r["path"].as_str().unwrap_or_default();
        let closed = r["closed"] == true;
        let extra = path.contains("d-1-ghost") || path.contains("d-1-stale");
        assert_eq!(closed, extra, "{path}: {refs:?}");
    }
    assert!(Path::new(&live).is_dir(), "the live lane is untouched");

    // Finishing a closed lane again is the idempotent no-op; a path
    // the issue never recorded refuses.
    let (ok, out) = cli(
        &pm,
        &state,
        &["issue", "finish", "D-1", "--worktree", &ghost_s],
    );
    assert!(ok && out["finished"] == false, "{out}");
    let (ok, err) = cli(
        &pm,
        &state,
        &["issue", "finish", "D-1", "--worktree", "/nope/wt"],
    );
    assert!(!ok, "{err}");
    assert!(
        err["error"]
            .as_str()
            .unwrap()
            .contains("records no open worktree"),
        "{err}"
    );

    // One open lane left — start reuses it.
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok && out["created"] == false, "{out}");
    assert_eq!(out["worktree"].as_str().unwrap(), live);
}

// ---- CAD-275: an unstarted or active lane is never finished as merged ----

/// A fresh `issue start` lane with no commits is `not started`, never
/// merged: the `--merged` sweep skips it (dry run and real, the text
/// output naming the reason), an explicit finish refuses — first as
/// recently active (the checkout itself is fresh), then, once idle, as
/// not started — and `--force` finishes an abandoned lane, recorded.
#[test]
fn issue_finish_unstarted_lane_is_never_merged() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(cli(&pm, &state, &["issue", "new", "Fresh", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-1"]).0);
    let wt = repo.join(".cadence/wt/d-1-fresh");

    let (ok, plan) = cli(
        &pm,
        &state,
        &["issue", "finish", "--merged", "--dry-run", "--json"],
    );
    assert!(ok, "{plan}");
    let row = &plan["rows"][0];
    assert!(
        row["outcome"] == "skipped" && row["reason"] == "not started" && row["merged_by"].is_null(),
        "{plan}"
    );
    let (ok, text) = cli_raw(&pm, &state, &["issue", "finish", "--merged", "--dry-run"]);
    assert!(ok, "{text}");
    assert!(text.contains("D-1: skipped(not started)"), "{text}");

    let before = commits(&pm);
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains("was modified") && msg.contains("s ago (") && msg.contains("idle 30m"),
        "a fresh checkout is recent activity, named: {msg}"
    );
    idle(&wt);
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(!ok, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains("has not started"),
        "{err}"
    );
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "--merged", "--json"]);
    assert!(ok, "{out}");
    assert_eq!(out["rows"][0]["reason"], "not started", "{out}");
    assert!(wt.is_dir());
    assert!(
        git(
            &repo,
            &["rev-parse", "--verify", "--quiet", "cadence/d-1-fresh"]
        )
        .0
    );
    assert_eq!(commits(&pm), before, "nothing finished");

    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-1", "--force"]);
    assert!(ok, "{out}");
    assert!(
        out["finished"] == true
            && out["not_started"] == true
            && out["merged_by"].is_null()
            && out["deleted_branch"] == true,
        "{out}"
    );
    assert!(
        out["overrode"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o == "not-started"),
        "{out}"
    );
    assert!(!wt.exists());
}

/// CAD-275: a merged, clean lane with a file modified inside the
/// active window is refused as in use — the refusal and the dry-run
/// row name the file and its age — and `--force` overrides, recorded.
#[test]
fn issue_finish_refuses_recent_activity() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(cli(&pm, &state, &["issue", "new", "Busy", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-1"]).0);
    let wt = repo.join(".cadence/wt/d-1-busy");
    land(&repo, &wt, "b.txt");
    // Touched, not changed: the tree stays clean.
    assert!(Command::new("touch")
        .arg(wt.join("f"))
        .status()
        .unwrap()
        .success());
    assert_eq!(git(&wt, &["status", "--porcelain"]).1, "");

    let (ok, text) = cli_raw(&pm, &state, &["issue", "finish", "--merged", "--dry-run"]);
    assert!(!ok, "a refused row exits 1: {text}");
    assert!(
        text.contains("D-1: refused(Worktree")
            && text.contains("was modified")
            && text.contains("(f)"),
        "{text}"
    );
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(msg.contains("(f)") && msg.contains("s ago"), "{msg}");
    assert!(wt.is_dir());

    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-1", "--force"]);
    assert!(ok, "{out}");
    assert!(
        out["merged_by"] == "ancestry" && out["not_started"] == false,
        "a landed lane is merged, not unstarted: {out}"
    );
    assert!(
        out["overrode"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o == "recent-activity"),
        "{out}"
    );
    let (_, body) = git(&pm, &["log", "-1", "--format=%B"]);
    assert!(body.contains("Forced: true"), "{body}");
}

/// Dispatch pre-flight refuses before anything is created: no return
/// address, an unreadable note, or an unreachable daemon each leave
/// no worktree, branch or tracker commit behind.
#[test]
fn dispatch_refusals_leave_nothing() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(cli(&pm, &state, &["issue", "new", "Disp", "--project", "demo"]).0);
    let before = commits(&pm);
    let wt = repo.join(".cadence/wt/d-1-disp");
    let note = _tmp.path().join("note.md");
    std::fs::write(&note, "# kickoff").unwrap();
    let note_s = note.to_str().unwrap().to_string();

    // No --reply-to and no CADENCE_ALIAS (the cli helper strips it).
    let (ok, err) = cli(
        &pm,
        &state,
        &["dispatch", "D-1", "--to", "w1", "--note", &note_s],
    );
    assert!(
        !ok && err["error"].as_str().unwrap().contains("return address"),
        "{err}"
    );

    // Unreadable note.
    let (ok, err) = cli(
        &pm,
        &state,
        &[
            "dispatch",
            "D-1",
            "--to",
            "w1",
            "--note",
            "/nonexistent/kickoff.md",
            "--reply-to",
            "pm",
        ],
    );
    assert!(
        !ok && err["error"].as_str().unwrap().contains("unreadable"),
        "{err}"
    );

    // Daemon down: the worker can't be verified — refused, nothing.
    let (ok, err) = cli(
        &pm,
        &state,
        &[
            "dispatch",
            "D-1",
            "--to",
            "w1",
            "--note",
            &note_s,
            "--reply-to",
            "pm",
        ],
    );
    assert!(!ok, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains("not reachable"),
        "{err}"
    );

    assert!(!wt.exists());
    assert!(
        !git(
            &repo,
            &["rev-parse", "--verify", "--quiet", "cadence/d-1-disp"]
        )
        .0
    );
    assert_eq!(commits(&pm), before);
}

// ---------- CAD-383: claims guard dispatch and issue start ----------

fn show_issue(pm: &Path, state: &Path, id: &str) -> Value {
    let (ok, out) = cli(pm, state, &["issue", "show", id, "--json"]);
    assert!(ok, "{out}");
    out
}

fn has_comment(issue: &Value, needles: &[&str]) -> bool {
    issue["comments"].as_array().unwrap().iter().any(|c| {
        let body = c["body"].as_str().unwrap_or_default();
        needles.iter().all(|n| body.contains(n))
    })
}

/// A claim recorded outside cadence (`issue claim`) guards `issue
/// start` on a doing issue: a foreign requester is refused naming the
/// holder and the claim age, the holder is let through, a take-over
/// needs a reason and is recorded, and backlog/unowned issues start
/// exactly as before (an owned backlog issue only warns).
#[test]
fn issue_claim_guards_start_and_take_over_is_recorded() {
    let (_tmp, pm, state, repo) = start_fx();
    for title in ["Claimed", "Taken", "Fresh", "Soft"] {
        assert!(cli(&pm, &state, &["issue", "new", title, "--project", "demo"]).0);
    }

    // A PM whose lane runs outside cadence records a claim: owner,
    // claim and a comment in one tracker commit; backlog → doing.
    let before = commits(&pm);
    let (ok, out) = cli(
        &pm,
        &state,
        &[
            "issue",
            "claim",
            "D-1",
            "--by",
            "pm-a",
            "--note",
            "claude subagent lane",
        ],
    );
    assert!(ok, "{out}");
    assert_eq!(out["claim"]["by"], "pm-a", "{out}");
    assert_eq!(commits(&pm), before + 1);
    let d1 = show_issue(&pm, &state, "D-1");
    assert_eq!(d1["status"], "doing", "{d1}");
    // The claim is the PM's; `owner` stays for the lane.
    assert!(d1["owner"].is_null(), "{d1}");
    assert_eq!(d1["claim"]["by"], "pm-a", "{d1}");
    assert!(d1["claim"]["at"].as_str().is_some(), "{d1}");
    assert!(d1["claim"]["age_secs"].as_i64().is_some(), "{d1}");
    assert!(
        has_comment(&d1, &["Claimed by pm-a", "claude subagent lane"]),
        "{d1}"
    );
    // The card view (issue ls / board) carries the claim too.
    let (_, ls) = cli(&pm, &state, &["issue", "ls", "--json"]);
    let card = ls["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "D-1")
        .unwrap()
        .clone();
    assert_eq!(card["claim"]["by"], "pm-a", "{card}");
    // `overview` lists the in-flight claim with its age.
    let (ok, ov) = cli(&pm, &state, &["overview", "--json"]);
    assert!(ok, "{ov}");
    let claim = ov["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["key"] == "demo")
        .and_then(|p| p["claims"].as_array())
        .and_then(|cs| cs.iter().find(|c| c["issue"] == "D-1"))
        .unwrap_or_else(|| panic!("no D-1 claim row: {ov}"))
        .clone();
    assert_eq!(claim["by"], "pm-a", "{claim}");
    assert!(claim["age_secs"].as_i64().is_some(), "{claim}");

    // A foreign requester is refused before anything is created: the
    // refusal names the holder, the claim age and the take-over flag.
    let before = commits(&pm);
    for args in [
        &["issue", "start", "D-1"][..],
        &["issue", "start", "D-1", "--by", "pm-b"][..],
    ] {
        let (ok, err) = cli(&pm, &state, args);
        assert!(!ok, "{err}");
        let msg = err["error"].as_str().unwrap();
        assert!(
            msg.contains("pm-a") && msg.contains("ago") && msg.contains("--take-over"),
            "{msg}"
        );
    }
    assert!(!repo.join(".cadence/wt/d-1-claimed").exists());
    assert_eq!(commits(&pm), before);
    // Another requester's claim is refused the same way.
    let (ok, err) = cli(&pm, &state, &["issue", "claim", "D-1", "--by", "pm-b"]);
    assert!(
        !ok && err["error"].as_str().unwrap().contains("pm-a"),
        "{err}"
    );
    assert_eq!(commits(&pm), before);

    // The holder is let through — by CADENCE_ALIAS or --by — and the
    // claim is kept as it was.
    let (ok, out) = cli_env(
        &pm,
        &state,
        &["issue", "start", "D-1"],
        &[("CADENCE_ALIAS", "pm-a")],
    );
    assert!(ok, "{out}");
    assert_eq!(out["created"], true);
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1", "--by", "pm-a"]);
    assert!(ok && out["created"] == false, "{out}");
    let d1 = show_issue(&pm, &state, "D-1");
    assert_eq!(d1["claim"]["by"], "pm-a", "{d1}");
    assert_eq!(d1["owner"], "pm-a", "{d1}");

    // Take-over: an empty reason is refused, a real one is recorded as
    // the new claim, a comment and a `claim` history entry.
    assert!(cli(&pm, &state, &["issue", "claim", "D-2", "--by", "pm-a"]).0);
    let before = commits(&pm);
    let (ok, err) = cli(
        &pm,
        &state,
        &["issue", "start", "D-2", "--by", "pm-b", "--take-over", "  "],
    );
    assert!(
        !ok && err["error"].as_str().unwrap().contains("reason"),
        "{err}"
    );
    assert_eq!(commits(&pm), before);
    let (ok, out) = cli(
        &pm,
        &state,
        &[
            "issue",
            "start",
            "D-2",
            "--by",
            "pm-b",
            "--take-over",
            "pm-a lane died at 09:00",
        ],
    );
    assert!(ok, "{out}");
    assert_eq!(out["claim"]["take_over"]["from"], "pm-a", "{out}");
    let d2 = show_issue(&pm, &state, "D-2");
    assert_eq!(d2["claim"]["by"], "pm-b", "{d2}");
    assert_eq!(d2["owner"], "pm-b", "{d2}");
    assert!(
        has_comment(
            &d2,
            &["Take-over by pm-b", "pm-a", "pm-a lane died at 09:00"]
        ),
        "{d2}"
    );
    let (ok, log) = cli(&pm, &state, &["issue", "log", "D-2"]);
    assert!(ok, "{log}");
    assert!(
        log["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "claim" && e["summary"].as_str().unwrap().contains("take-over")),
        "{log}"
    );

    // Backlog/unowned is unchanged: no warning, owner = the actor.
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-3"]);
    assert!(ok, "{out}");
    assert!(out["claim"]["warning"].is_null(), "{out}");
    let d3 = show_issue(&pm, &state, "D-3");
    assert_eq!(d3["owner"], "operator", "{d3}");
    assert_eq!(d3["claim"]["by"], "operator", "{d3}");
    // An owned backlog issue only warns, naming the owner, and keeps it.
    assert!(cli(&pm, &state, &["issue", "set", "D-4", "owner=bob"]).0);
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-4"]);
    assert!(ok, "{out}");
    assert!(
        out["claim"]["warning"].as_str().unwrap().contains("bob"),
        "{out}"
    );
    assert_eq!(show_issue(&pm, &state, "D-4")["owner"], "bob");

    // Release: only a holder may release; it clears the claim.
    let (ok, err) = cli(&pm, &state, &["issue", "release", "D-1", "--by", "pm-b"]);
    assert!(
        !ok && err["error"].as_str().unwrap().contains("pm-a"),
        "{err}"
    );
    let (ok, out) = cli(
        &pm,
        &state,
        &[
            "issue",
            "release",
            "D-1",
            "--by",
            "pm-a",
            "--note",
            "done here",
        ],
    );
    assert!(ok, "{out}");
    let d1 = show_issue(&pm, &state, "D-1");
    assert!(d1["claim"].is_null(), "{d1}");
    assert!(d1["owner"].is_null(), "{d1}");
    assert!(has_comment(&d1, &["Released by pm-a", "done here"]), "{d1}");
    // Unclaimed now: anyone may start it again.
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1", "--by", "pm-b"]);
    assert!(ok, "{out}");
}

/// `dispatch` (plain and `--job`) reads the claim: re-dispatching to
/// the same worker and the claiming PM dispatching another worker go
/// through; a different PM is refused before anything is created
/// unless it takes over with a reason; a claim recorded with `issue
/// claim` by a PM whose lanes run outside cadence is seen too.
#[test]
fn dispatch_respects_claims() {
    let (tmp, pm, state, repo) = start_fx();
    // `dispatch_send` reads the tracker daemon-side (claim check +
    // lane resolution) — the daemon must see the same pm dir the
    // cli calls do.
    let d = UiDaemon::start_on_pm(state.clone(), &pm);
    let cwd = pm.to_str().unwrap();
    for (alias, upstream) in [
        ("pm-a", None),
        ("pm-b", None),
        ("w1", Some("pm-a")),
        ("w2", Some("pm-a")),
        ("w3", Some("pm-b")),
    ] {
        // Mailboxes: kickoffs stay queued, so counts are exact.
        let mut req = json!({"alias": alias, "provider": "inbox",
                             "endpoint_kind": "inbox", "cwd": cwd});
        if let Some(up) = upstream {
            req["params"] = json!(format!("{{\"upstream\":\"{up}\"}}"));
        }
        let _ = d.operator_rpc("agent_register", req);
    }
    for title in ["Lane", "Outside"] {
        assert!(cli(&pm, &state, &["issue", "new", title, "--project", "demo"]).0);
    }
    let note = tmp.path().join("note.md");
    std::fs::write(&note, "# kickoff").unwrap();
    let note_s = note.to_str().unwrap().to_string();
    let spec = tmp.path().join("spec.md");
    std::fs::write(&spec, "# spec").unwrap();
    let spec_s = spec.to_str().unwrap().to_string();
    let dispatch = |id: &str, to: &str, by: &str, extra: &[&str]| {
        let mut args = vec![
            "dispatch",
            id,
            "--to",
            to,
            "--note",
            &note_s,
            "--reply-to",
            by,
        ];
        args.extend_from_slice(extra);
        cli_op(&pm, &state, &args)
    };
    let messages = |alias: &str| {
        d.rpc("agent_show", json!({"alias": alias}))["messages"]
            .as_array()
            .unwrap()
            .len()
    };

    // pm-a dispatches w1: the worker owns the lane, pm-a holds the claim.
    let (ok, out) = dispatch("D-1", "w1", "pm-a", &[]);
    assert!(ok && out["dispatched"] == true, "{out}");
    let d1 = show_issue(&pm, &state, "D-1");
    assert_eq!(d1["owner"], "w1", "{d1}");
    assert_eq!(d1["claim"]["by"], "pm-a", "{d1}");
    // `status` shows the claim, with its age, on the worker's row and
    // in the footer.
    let (ok, st) = cli(&pm, &state, &["status", "--json"]);
    assert!(ok, "{st}");
    let footer = st["footer"]["claims"].as_array().unwrap();
    let c = footer.iter().find(|c| c["issue"] == "D-1").unwrap();
    assert!(
        c["by"] == "pm-a" && c["owner"] == "w1" && c["age_secs"].as_i64().is_some(),
        "{c}"
    );
    let w1 = st["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["alias"] == "w1")
        .unwrap();
    assert!(
        w1["claims"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["issue"] == "D-1"),
        "{w1}"
    );
    // (a) re-dispatching to the same worker still works.
    let (ok, out) = dispatch("D-1", "w1", "pm-a", &[]);
    assert!(ok, "{out}");
    // (b) the claiming PM may hand the issue to another of its workers.
    let (ok, out) = dispatch("D-1", "w2", "pm-a", &[]);
    assert!(ok, "{out}");

    // (c) a different PM is refused, plain and --job, naming the holder
    // and the claim age — no commit, no message, no job.
    let before = commits(&pm);
    let (ok, err) = dispatch("D-1", "w3", "pm-b", &[]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains("pm-a") && msg.contains("ago") && msg.contains("--take-over"),
        "{msg}"
    );
    let (ok, err) = dispatch("D-1", "w3", "pm-b", &["--job", "--spec", &spec_s]);
    assert!(
        !ok && err["error"].as_str().unwrap().contains("pm-a"),
        "{err}"
    );
    assert_eq!(commits(&pm), before);
    assert_eq!(messages("w3"), 0);
    let jobs = d.rpc("job_list", json!({"all": true}));
    assert_eq!(jobs["jobs"].as_array().unwrap().len(), 0, "{jobs}");

    // A claim recorded outside cadence is seen by dispatch.
    assert!(cli(&pm, &state, &["issue", "claim", "D-2", "--by", "pm-x"]).0);
    let (ok, err) = dispatch("D-2", "w1", "pm-a", &[]);
    assert!(
        !ok && err["error"].as_str().unwrap().contains("pm-x"),
        "{err}"
    );
    assert!(!repo.join(".cadence/wt/d-2-outside").exists());
    assert_eq!(messages("w1"), 1);

    // Take-over with a reason goes through and is recorded: the new
    // claim, the new owner and a comment naming the old holder.
    let (ok, out) = dispatch(
        "D-2",
        "w1",
        "pm-a",
        &["--take-over", "pm-x asked pm-a to finish it"],
    );
    assert!(ok && out["dispatched"] == true, "{out}");
    let d2 = show_issue(&pm, &state, "D-2");
    assert_eq!(d2["claim"]["by"], "pm-a", "{d2}");
    assert_eq!(d2["owner"], "w1", "{d2}");
    assert!(
        has_comment(
            &d2,
            &["Take-over by pm-a", "pm-x", "pm-x asked pm-a to finish it"]
        ),
        "{d2}"
    );
    assert_eq!(messages("w1"), 2);
}
