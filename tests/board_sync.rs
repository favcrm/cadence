//! board_sync: area tests split from tests/board.rs (CAD-537).
//! Board e2e: the `cadence issue` CLI against a temp PM dir, and the
//! `cadence ui` HTTP server in-process.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod board_common;
use board_common::*;

use serde_json::json;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use std::time::Instant;
use tempfile::TempDir;

fn porcelain(dir: &Path) -> String {
    git(dir, &["status", "--porcelain"]).1
}

/// The remote's current tip for `branch`, or empty when unreadable.
fn remote_tip(remote: &Path, branch: &str) -> String {
    Command::new("git")
        .arg("ls-remote")
        .arg(remote)
        .arg(format!("refs/heads/{branch}"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .next()
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// Fail if the remote's branch tip leaves `tip` inside `window` — a
/// leaked post-commit push lands within a couple of seconds of the
/// replayed commit, so watching briefly after sync returns covers the
/// hook's in-flight window.
fn assert_remote_stable(remote: &Path, branch: &str, tip: &str, window: Duration) {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        assert_eq!(
            remote_tip(remote, branch),
            tip,
            "remote moved during the post-commit window"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// A writes, B writes a different issue (in its own project — same-id
/// minting is inherent while behind), B syncs → rebased + pushed;
/// A syncs → level, both trees identical, no conflict.
#[test]
fn sync_rebases_and_levels_two_clones() {
    let t = TrackerPair::new();
    // A: project + first issue, pushed.
    let (ok, out) = t.cli(&t.a, &["issue", "project", "add", "cad", "--prefix", "CAD"]);
    assert!(ok, "{out}");
    t.publish(&t.a);
    // B picks the project up via a strict-behind sync first — a pure
    // fast-forward replays and pushes nothing.
    let (ok, out) = t.cli(&t.b, &["issue", "sync"]);
    assert!(ok, "B first sync: {out}");
    assert_eq!(out["rebased"], false);
    assert_eq!(out["pushed"], false);
    assert_eq!(out["behind"], 0);
    // A moves the remote again — B is behind from here.
    let (ok, out) = t.cli(&t.a, &["issue", "new", "alpha", "--project", "cad"]);
    assert!(ok, "{out}");
    t.publish(&t.a);
    // B writes a different issue in its own project — disjoint paths,
    // so the rebase merges cleanly. B's hook push fails non-ff.
    let (ok, out) = t.cli(&t.b, &["issue", "project", "add", "ops", "--prefix", "OPS"]);
    assert!(ok, "{out}");
    let (ok, out) = t.cli(&t.b, &["issue", "new", "beta", "--project", "ops"]);
    assert!(ok, "{out}");
    assert_eq!(out["id"].as_str().unwrap(), "OPS-1");
    let (ok, out) = t.cli(&t.b, &["issue", "sync"]);
    assert!(ok, "B sync: {out}");
    assert_eq!(out["rebased"], true);
    assert_eq!(out["pushed"], true);
    // A syncs: strictly behind now → rebase fast-forwards.
    let (ok, out) = t.cli(&t.a, &["issue", "sync"]);
    assert!(ok, "A sync: {out}");
    // Identical tracked trees: same HEAD, same file list.
    assert_eq!(head(&t.a), head(&t.b));
    assert_eq!(
        git(&t.a, &["ls-files"]).1,
        git(&t.b, &["ls-files"]).1,
        "tracked trees differ"
    );
}

/// Same `issue.md` edited on both hosts: sync without `--resolve`
/// aborts naming the path and both subjects, and the tree is exactly
/// as found; `--resolve theirs` then `--resolve ours` on a fresh
/// divergence take the named side whole.
#[test]
fn sync_conflict_aborts_then_resolves() {
    let t = TrackerPair::new();
    let (ok, out) = t.cli(&t.a, &["issue", "project", "add", "cad", "--prefix", "CAD"]);
    assert!(ok, "{out}");
    let (ok, out) = t.cli(&t.a, &["issue", "new", "alpha", "--project", "cad"]);
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap().to_string();
    t.publish(&t.a);
    let (ok, out) = t.cli(&t.b, &["issue", "sync"]);
    assert!(ok, "B level sync: {out}");

    // B edits the issue first and lands it on the remote.
    let (ok, out) = t.cli(&t.b, &["issue", "set", &id, "status=doing"]);
    assert!(ok, "{out}");
    t.publish(&t.b);
    // A edits the same file — its hook push fails non-ff.
    let (ok, out) = t.cli(&t.a, &["issue", "set", &id, "status=review"]);
    assert!(ok, "{out}");
    let pre_head = head(&t.a);

    let (ok, out) = t.cli(&t.a, &["issue", "sync"]);
    assert!(!ok, "conflicting sync must fail: {out}");
    assert_eq!(out["ok"], false);
    assert_eq!(out["aborted"], "conflict");
    let conflicts = out["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    assert_eq!(
        conflicts[0]["path"].as_str().unwrap(),
        format!("cad/{id}/issue.md")
    );
    assert!(conflicts[0]["local"].as_str().unwrap().contains("set"));
    assert!(conflicts[0]["remote"].as_str().unwrap().contains("set"));
    assert_eq!(out["tree_restored"], true);
    // The tree is exactly as found: same HEAD, clean status, no
    // rebase marker left behind.
    assert_eq!(head(&t.a), pre_head);
    assert_eq!(porcelain(&t.a), "", "sync left the tree dirty");
    assert!(!t.a.join(".git/rebase-merge").exists());

    // --resolve theirs: the remote's value wins wholesale.
    let (ok, out) = t.cli(&t.a, &["issue", "sync", "--resolve", "theirs"]);
    assert!(ok, "resolve theirs: {out}");
    let body = std::fs::read_to_string(t.a.join(format!("cad/{id}/issue.md"))).unwrap();
    assert!(body.contains("status: doing"), "{body}");
    assert_eq!(porcelain(&t.a), "");

    // Fresh divergence — --resolve ours keeps the local value. B must
    // publish first: whichever `issue set` commits first wins the
    // remote via its own post-commit hook, so A writes only after B's
    // push has landed (A's hook push then fails non-ff, as intended).
    let (ok, out) = t.cli(&t.b, &["issue", "set", &id, "status=done"]);
    assert!(ok, "{out}");
    t.publish(&t.b);
    let (ok, out) = t.cli(&t.a, &["issue", "set", &id, "status=backlog"]);
    assert!(ok, "{out}");
    let (ok, out) = t.cli(&t.a, &["issue", "sync", "--resolve", "ours"]);
    assert!(ok, "resolve ours: {out}");
    let body = std::fs::read_to_string(t.a.join(format!("cad/{id}/issue.md"))).unwrap();
    assert!(body.contains("status: backlog"), "{body}");
}

/// A clean rebase into a lint failure aborts the same way: the local
/// host added an issue on a component the remote side removed — the
/// merge is disjoint, lint rejects it, the tree is restored and
/// nothing is pushed.
#[test]
fn sync_lint_failure_aborts() {
    let t = TrackerPair::new();
    let (ok, out) = t.cli(
        &t.a,
        &[
            "issue",
            "project",
            "add",
            "cad",
            "--prefix",
            "CAD",
            "--component",
            "board",
            "--component",
            "gone",
        ],
    );
    assert!(ok, "{out}");
    t.publish(&t.a);
    let (ok, out) = t.cli(&t.b, &["issue", "sync"]);
    assert!(ok, "B level sync: {out}");

    // B removes `gone` from project.yaml by hand and pushes it.
    let py = t.b.join("cad/project.yaml");
    std::fs::write(
        &py,
        std::fs::read_to_string(&py)
            .unwrap()
            .replace("\n- gone", ""),
    )
    .unwrap();
    let (ok, _) = git(&t.b, &["add", "-A"]);
    assert!(ok);
    let (ok, _) = git(&t.b, &["commit", "-qm", "drop gone component"]);
    assert!(ok);
    t.publish(&t.b);

    // A's local project.yaml still declares `gone` — the write is
    // valid there and its background push fails non-ff.
    let (ok, out) = t.cli(
        &t.a,
        &[
            "issue",
            "new",
            "uses gone",
            "--project",
            "cad",
            "--component",
            "gone",
        ],
    );
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap().to_string();
    let pre_head = head(&t.a);
    // Baseline before sync — a mid-rebase hook leak lands inside the
    // sync call itself, so the remote tip must be captured first.
    let tip = remote_tip(&t.remote, "main");
    assert!(!tip.is_empty());

    let (ok, out) = t.cli(&t.a, &["issue", "sync"]);
    assert!(!ok, "lint-broken merge must fail: {out}");
    assert_eq!(out["aborted"], "lint");
    assert_eq!(out["rebased"], true);
    assert_eq!(out["pushed"], false);
    assert_eq!(out["tree_restored"], true);
    let errors = out["lint"]["errors"].as_array().unwrap();
    assert!(
        errors
            .iter()
            .any(|e| e.as_str().unwrap().contains(&id) && e.as_str().unwrap().contains("gone")),
        "lint errors: {errors:?}"
    );
    assert_eq!(head(&t.a), pre_head);
    assert_eq!(porcelain(&t.a), "");
    // Nothing was pushed — the remote tip still lacks A's commit, and
    // it must stay that way through the post-commit hook's window: the
    // rebase's replayed commits once leaked out mid-abort.
    assert_remote_stable(&t.remote, "main", &tip, Duration::from_secs(6));
    // A fresh clone of the remote still lints clean — the bad commit
    // never escaped this clone's rebase.
    let fresh = TempDir::new().unwrap();
    let (ok, _) = {
        let out = Command::new("git")
            .args(["clone", "-q"])
            .arg(&t.remote)
            .arg(fresh.path())
            .output()
            .unwrap();
        (out.status.success(), ())
    };
    assert!(ok, "fresh clone failed");
    let (ok, out) = cli(fresh.path(), t.state.path(), &["issue", "lint"]);
    assert!(ok && out["ok"] == true, "fresh clone lint: {out}");
}

/// `--dry-run` fetches and reports divergence + would-conflict paths
/// without touching the tree; `--no-push` rebases and lints but
/// leaves the remote alone.
#[test]
fn sync_dry_run_and_no_push() {
    let t = TrackerPair::new();
    let (ok, out) = t.cli(&t.a, &["issue", "project", "add", "cad", "--prefix", "CAD"]);
    assert!(ok, "{out}");
    let (ok, out) = t.cli(&t.a, &["issue", "new", "alpha", "--project", "cad"]);
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap().to_string();
    t.publish(&t.a);
    let (ok, out) = t.cli(&t.b, &["issue", "sync"]);
    assert!(ok, "{out}");

    // Same-file divergence: B pushes its edit, A's two edits stay
    // local — ahead=2/behind=1 is asymmetric, so a swapped count
    // would fail these asserts.
    let (ok, out) = t.cli(&t.b, &["issue", "set", &id, "status=doing"]);
    assert!(ok, "{out}");
    t.publish(&t.b);
    let (ok, out) = t.cli(&t.a, &["issue", "set", &id, "status=review"]);
    assert!(ok, "{out}");
    let (ok, out) = t.cli(&t.a, &["issue", "set", &id, "priority=P1"]);
    assert!(ok, "{out}");
    let pre_head = head(&t.a);

    let (ok, out) = t.cli(&t.a, &["issue", "sync", "--dry-run"]);
    assert!(ok, "dry-run: {out}");
    assert_eq!(out["dry_run"], true);
    assert_eq!(out["changed"], false);
    assert_eq!(out["behind"], 1);
    assert_eq!(out["ahead"], 2);
    assert_eq!(
        out["would_conflict"].as_array().unwrap(),
        &vec![json!(format!("cad/{id}/issue.md"))]
    );
    assert_eq!(head(&t.a), pre_head);
    assert_eq!(porcelain(&t.a), "");

    // --no-push rebases and lints but leaves the remote untouched —
    // and stays untouched through the post-commit hook's window: the
    // rebase's replayed commits once leaked out mid-sync. `--resolve
    // ours` keeps A's two commits so the follow-up push has real work.
    let remote_before = git(&t.a, &["rev-parse", "origin/main"]).1;
    let (ok, out) = t.cli(&t.a, &["issue", "sync", "--no-push", "--resolve", "ours"]);
    assert!(ok, "no-push: {out}");
    assert_eq!(out["rebased"], true);
    assert_eq!(out["pushed"], false);
    assert_eq!(git(&t.a, &["rev-parse", "origin/main"]).1, remote_before);
    assert_remote_stable(&t.remote, "main", &remote_before, Duration::from_secs(6));
    // The rebased commits exist locally — a later plain sync pushes
    // them, and `pushed` reports the remote actually moving.
    let (ok, out) = t.cli(&t.a, &["issue", "sync"]);
    assert!(ok, "follow-up push: {out}");
    assert_eq!(out["pushed"], true);
}

/// Preconditions refuse with the offending paths: a dirty tree, no
/// `origin`, and a rebase or merge already in progress.
#[test]
fn sync_precondition_refusals() {
    let t = TrackerPair::new();

    // Dirty tree — the porcelain listing names the path.
    std::fs::write(t.a.join("scratch.txt"), "x").unwrap();
    let (ok, out) = t.cli(&t.a, &["issue", "sync"]);
    assert!(!ok);
    assert!(
        out["error"].as_str().unwrap().contains("scratch.txt"),
        "{out}"
    );
    std::fs::remove_file(t.a.join("scratch.txt")).unwrap();

    // A rebase in progress — the marker path is named.
    std::fs::create_dir(t.a.join(".git/rebase-merge")).unwrap();
    let (ok, out) = t.cli(&t.a, &["issue", "sync"]);
    assert!(!ok);
    assert!(
        out["error"].as_str().unwrap().contains("rebase-merge"),
        "{out}"
    );
    std::fs::remove_dir(t.a.join(".git/rebase-merge")).unwrap();

    // A merge in progress — MERGE_HEAD is named.
    std::fs::write(t.a.join(".git/MERGE_HEAD"), "0".repeat(40)).unwrap();
    let (ok, out) = t.cli(&t.a, &["issue", "sync"]);
    assert!(!ok);
    assert!(
        out["error"].as_str().unwrap().contains("MERGE_HEAD"),
        "{out}"
    );
    std::fs::remove_file(t.a.join(".git/MERGE_HEAD")).unwrap();

    // No origin: an init'd tracker without a remote.
    let solo = TempDir::new().unwrap();
    let (ok, out) = cli(solo.path(), t.state.path(), &["issue", "init"]);
    assert!(ok, "{out}");
    let (ok, out) = cli(solo.path(), t.state.path(), &["issue", "sync"]);
    assert!(!ok);
    assert!(out["error"].as_str().unwrap().contains("origin"), "{out}");

    // Not a git repo: pm.yaml with no .git at all.
    let bare = TempDir::new().unwrap();
    std::fs::write(
        bare.path().join("pm.yaml"),
        "schema: 1\nnotes_dir: /var/www/agent-notes\n",
    )
    .unwrap();
    let (ok, out) = cli(bare.path(), t.state.path(), &["issue", "sync"]);
    assert!(!ok);
    assert!(
        out["error"]
            .as_str()
            .unwrap()
            .contains("not a git repository"),
        "{out}"
    );
}

/// Doctor reports ahead/behind against `origin/<branch>` and points
/// at `issue sync` while the local side is behind.
#[test]
fn doctor_reports_divergence_and_sync_hint() {
    let t = TrackerPair::new();
    let (ok, out) = t.cli(&t.a, &["issue", "project", "add", "cad", "--prefix", "CAD"]);
    assert!(ok, "{out}");
    t.publish(&t.a);
    // A is level — doctor is healthy and carries no sync hint.
    let (ok, out) = t.cli(&t.a, &["issue", "doctor"]);
    assert!(ok, "level doctor: {out}");
    assert_eq!(out["push"]["ahead"], 0);
    assert_eq!(out["push"]["behind"], 0);
    assert!(out["push"]["sync"].is_null());

    // B falls behind: fetch alone moves `origin/<branch>` forward.
    let (ok, _) = git(&t.b, &["fetch", "origin"]);
    assert!(ok);
    let (_, report) = t.cli(&t.b, &["issue", "doctor"]);
    assert!(report["push"]["behind"].as_u64().unwrap() >= 1, "{report}");
    assert_eq!(
        report["push"]["sync"].as_str().unwrap(),
        "cadence issue sync"
    );
}

/// The hardened post-commit hook itself: a commit made while a rebase
/// marker stands is not pushed, and neither is one on a detached HEAD —
/// even when the hook is invoked directly. Commits under the marker
/// stay local, which is what makes the whole class safe.
#[test]
fn post_commit_hook_refuses_mid_sequence_and_detached() {
    let t = TrackerPair::new();
    let tip0 = remote_tip(&t.remote, "main");
    assert!(!tip0.is_empty());

    // A rebase marker: commits created now (which fire the hook) must
    // stay local — replayed commits are not settled state.
    std::fs::create_dir(t.a.join(".git/rebase-merge")).unwrap();
    let (ok, out) = t.cli(&t.a, &["issue", "project", "add", "cad", "--prefix", "CAD"]);
    assert!(ok, "{out}");
    let (ok, out) = t.cli(&t.a, &["issue", "new", "alpha", "--project", "cad"]);
    assert!(ok, "{out}");
    assert_ne!(head(&t.a), tip0, "commits did not happen locally");
    // Invoke the hook directly — it still exits without pushing.
    let rc = Command::new("sh")
        .arg(".git/hooks/post-commit")
        .current_dir(&t.a)
        .status()
        .unwrap();
    assert!(rc.success());
    assert_remote_stable(&t.remote, "main", &tip0, Duration::from_secs(3));

    // A merge marker covers the same branch of the guard.
    std::fs::remove_dir(t.a.join(".git/rebase-merge")).unwrap();
    std::fs::write(t.a.join(".git/MERGE_HEAD"), "0".repeat(40)).unwrap();
    let rc = Command::new("sh")
        .arg(".git/hooks/post-commit")
        .current_dir(&t.a)
        .status()
        .unwrap();
    assert!(rc.success());
    std::fs::remove_file(t.a.join(".git/MERGE_HEAD")).unwrap();

    // Detached HEAD — no branch means nothing to push.
    let (ok, _) = git(&t.a, &["checkout", "-q", "--detach", "HEAD"]);
    assert!(ok);
    let rc = Command::new("sh")
        .arg(".git/hooks/post-commit")
        .current_dir(&t.a)
        .status()
        .unwrap();
    assert!(rc.success());
    assert_remote_stable(&t.remote, "main", &tip0, Duration::from_secs(3));
}
