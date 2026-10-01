//! CAD-887: `cadence issue edit` — several single-purpose writes as one
//! tracker commit, all-or-nothing. A temp PM dir only; the real tracker
//! is never touched.
#![allow(clippy::disallowed_methods)]
mod board_common;
use board_common::*;

use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use tempfile::TempDir;

fn git(pm: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(pm)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn head(pm: &Path) -> String {
    git(pm, &["rev-parse", "HEAD"]).trim().to_string()
}

/// Everything under the PM dir except `.git`, as path -> bytes.
fn tree(pm: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p.strip_prefix(root).unwrap().display().to_string();
                out.push((rel, std::fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(pm, pm, &mut out);
    out.sort();
    out
}

struct Fx {
    pm: TempDir,
    state: TempDir,
    files: TempDir,
}

fn fx() -> Fx {
    let f = Fx {
        pm: TempDir::new().unwrap(),
        state: TempDir::new().unwrap(),
        files: TempDir::new().unwrap(),
    };
    seed(f.pm.path(), f.state.path());
    f
}

impl Fx {
    fn cli(&self, args: &[&str]) -> (bool, Value) {
        cli(self.pm.path(), self.state.path(), args)
    }
    fn file(&self, name: &str, text: &str) -> String {
        let p = self.files.path().join(name);
        std::fs::write(&p, text).unwrap();
        p.display().to_string()
    }
    /// `issue edit` with `body` piped to stdin.
    fn stdin(&self, body: &str, args: &[&str]) -> (bool, Value) {
        let mut cmd = Command::new(bin());
        cmd.arg("--state-dir")
            .arg(self.state.path())
            .args(args)
            .env("CADENCE_PM_DIR", self.pm.path())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    Path::new(bin()).parent().unwrap().display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("HOME", self.state.path())
            .env_remove("CADENCE_ALIAS")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        // A capped reader may close the pipe early.
        let _ = child.stdin.take().unwrap().write_all(body.as_bytes());
        let out = child.wait_with_output().unwrap();
        let text = if out.stdout.is_empty() {
            String::from_utf8_lossy(&out.stderr).to_string()
        } else {
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        let json: Value =
            serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}"));
        (out.status.success(), json)
    }
}

#[test]
fn multi_field_edit_is_one_commit() {
    let f = fx();
    let pm = f.pm.path();
    let acc = f.file("acc.md", "- [ ] first\n- [x] second\n");
    let comment = f.file("c.md", "done: `backticks` stay literal $(x)\n");
    let attach = f.file("shot.txt", "proof");
    let before = commits(pm);
    let (ok, out) = f.cli(&[
        "issue",
        "edit",
        "CAD-3",
        "--set",
        "priority=P1",
        "--set",
        "owner=me",
        "--tag",
        "+ui,+api",
        "--link",
        "blocked_by:CAD-2",
        "--ref",
        "pr:https://example.com/pr/1",
        "--attach",
        &attach,
        "--acceptance",
        &acc,
        "--comment-file",
        &comment,
        "--author",
        "rev-bot",
    ]);
    assert!(ok, "{out}");
    assert_eq!(commits(pm), before + 1, "exactly one commit");
    assert_eq!(out["id"], "CAD-3");
    let changed: Vec<String> = out["changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    for want in [
        "set:priority=P1",
        "set:owner=me",
        "tags=api,ui",
        "link:blocked_by:CAD-2",
        "ref:pr",
        "attach:shot.txt",
        "acceptance",
        "comment",
    ] {
        assert!(changed.iter().any(|c| c == want), "{want} in {changed:?}");
    }
    // rev is the issue.md revision.
    let (ok, shown) = f.cli(&["issue", "show", "CAD-3", "--json"]);
    assert!(ok, "{shown}");
    assert_eq!(shown["priority"], "P1");
    assert_eq!(shown["owner"], "me");
    let files = git(pm, &["show", "--pretty=format:", "--name-only", "HEAD"]);
    let mut paths: Vec<&str> = files.lines().filter(|l| !l.is_empty()).collect();
    paths.sort();
    assert_eq!(paths.len(), 3, "{paths:?}");
    assert!(paths.iter().any(|p| p.ends_with("CAD-3/issue.md")));
    assert!(paths
        .iter()
        .any(|p| p.ends_with("CAD-3/artifacts/shot.txt")));
    assert!(paths
        .iter()
        .any(|p| p.contains("CAD-3/comments/") && p.ends_with("-rev-bot.md")));
    let msg = git(pm, &["log", "-1", "--format=%B"]);
    assert!(msg.contains("Issue: CAD-3"), "{msg}");
    assert!(msg.contains("Issue: CAD-2"), "link target trailer: {msg}");
    assert!(msg.contains("Actor: rev-bot"), "{msg}");
    let issue = std::fs::read_to_string(pm.join("cadence/CAD-3/issue.md")).unwrap();
    assert!(issue.contains("- [x] second"), "{issue}");
    assert!(git(pm, &["status", "--porcelain"]).trim().is_empty());
    assert_eq!(
        out["rev"],
        shown["rev"].clone(),
        "edit prints the rev `show` reports"
    );
}

#[test]
fn any_bad_part_refuses_the_whole_edit_and_writes_nothing() {
    let f = fx();
    let pm = f.pm.path();
    let good_comment = f.file("ok.md", "a fine comment\n");
    let good_attach = f.file("ok.txt", "x");
    let bad_acc = f.file("bad.md", "not a checkbox\n");
    let secret = format!("ghp_{}", "a".repeat(36));
    let secret_comment = f.file("secret.md", &format!("key {secret} here\n"));
    let empty = f.file("empty.md", "  \n");
    let missing = f.files.path().join("nope.bin").display().to_string();

    // (flag named in the error, the bad part's args)
    let bad: Vec<(&str, Vec<&str>)> = vec![
        ("--set", vec!["--set", "status=bogus"]),
        ("--set", vec!["--set", "nokey=1"]),
        ("--set", vec!["--set", "no-equals"]),
        ("--tag", vec!["--tag", "+Bad Tag"]),
        ("--link", vec!["--link", "bogus:CAD-2"]),
        ("--link", vec!["--link", "blocked_by:CAD-999"]),
        ("--link", vec!["--link", "blocked_by:CAD-3"]),
        ("--unlink", vec!["--unlink", "blocked_by:CAD-2"]),
        ("--ref", vec!["--ref", "bogus:x"]),
        ("--ref", vec!["--ref", "pr:--upload-pack=x"]),
        ("--attach", vec!["--attach", &missing]),
        ("--acceptance", vec!["--acceptance", &bad_acc]),
        ("--comment-file", vec!["--comment-file", &secret_comment]),
        ("--comment-file", vec!["--comment-file", &empty]),
        (
            "--comment-file",
            vec!["--comment-file", &good_comment, "--author", "bad author!"],
        ),
        // The gate `issue set` runs: done needs evidence.
        ("--set", vec!["--set", "status=done"]),
    ];
    for (flag, part) in bad {
        let tree_before = tree(pm);
        let head_before = head(pm);
        let n = commits(pm);
        let mut args = vec![
            "issue",
            "edit",
            "CAD-3",
            "--set",
            "priority=P0",
            "--tag",
            "+ok",
            "--ref",
            "note:fine",
            "--attach",
            &good_attach,
        ];
        args.extend(part.iter().copied());
        let (ok, out) = f.cli(&args);
        assert!(!ok, "{part:?} was accepted: {out}");
        assert_eq!(out["kind"], "rejected", "{part:?}: {out}");
        let msg = out["error"].as_str().unwrap();
        assert!(
            msg.contains(flag),
            "{part:?}: error must name {flag}: {msg}"
        );
        assert_eq!(head(pm), head_before, "{part:?}: HEAD moved");
        assert_eq!(commits(pm), n, "{part:?}: commit count moved");
        assert_eq!(tree(pm), tree_before, "{part:?}: files changed");
        assert!(
            git(pm, &["status", "--porcelain"]).trim().is_empty(),
            "{part:?}: dirty tracker"
        );
    }
    // And an empty edit is refused too.
    let (ok, out) = f.cli(&["issue", "edit", "CAD-3", "--tag", "-nonexistent"]);
    assert!(!ok && out["kind"] == "rejected", "{out}");
}

#[test]
fn a_ref_in_the_same_edit_is_evidence_for_done() {
    let f = fx();
    let (ok, out) = f.cli(&[
        "issue",
        "edit",
        "CAD-3",
        "--ref",
        "pr:https://example.com/pr/9",
        "--set",
        "status=done",
    ]);
    assert!(ok, "{out}");
    let (_, shown) = f.cli(&["issue", "show", "CAD-3", "--json"]);
    assert_eq!(shown["status"], "done");
}

#[test]
fn stdin_bodies_work_for_edit_new_and_comment() {
    let f = fx();
    let pm = f.pm.path();
    let (ok, out) = f.stdin(
        "- [ ] from stdin\n",
        &["issue", "edit", "CAD-3", "--acceptance", "-"],
    );
    assert!(ok, "{out}");
    assert!(std::fs::read_to_string(pm.join("cadence/CAD-3/issue.md"))
        .unwrap()
        .contains("- [ ] from stdin"));

    let n = commits(pm);
    let (ok, out) = f.stdin(
        "piped `comment` $HOME\n",
        &[
            "issue",
            "edit",
            "CAD-3",
            "--comment-file",
            "-",
            "--set",
            "size=S",
        ],
    );
    assert!(ok, "{out}");
    assert_eq!(commits(pm), n + 1);
    let name = out["comment"].as_str().unwrap();
    let text = std::fs::read_to_string(pm.join("cadence/CAD-3/comments").join(name)).unwrap();
    assert!(text.contains("piped `comment` $HOME"), "{text}");

    // Stdin can feed one body only.
    let n = commits(pm);
    let (ok, out) = f.stdin(
        "x",
        &[
            "issue",
            "edit",
            "CAD-3",
            "--comment-file",
            "-",
            "--acceptance",
            "-",
        ],
    );
    assert!(!ok && out["kind"] == "rejected", "{out}");
    assert_eq!(commits(pm), n);

    // `issue comment --file -` and `issue new --file -`.
    let (ok, out) = f.stdin(
        "comment via stdin\n",
        &["issue", "comment", "CAD-3", "--file", "-"],
    );
    assert!(ok, "{out}");
    let (ok, out) = f.stdin(
        "## Body\nfrom stdin\n",
        &[
            "issue",
            "new",
            "stdin ticket",
            "--project",
            "cadence",
            "--file",
            "-",
        ],
    );
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap();
    let issue = std::fs::read_to_string(pm.join(format!("cadence/{id}/issue.md"))).unwrap();
    assert!(issue.contains("from stdin"), "{issue}");
}

#[test]
fn author_needs_a_comment_and_follows_the_comment_derivation() {
    let f = fx();
    let pm = f.pm.path();
    // --author alone would only forge the Actor trailer: refused.
    let n = commits(pm);
    let (ok, out) = f.cli(&["issue", "edit", "CAD-3", "--set", "size=S", "--author", "x"]);
    assert!(!ok && out["kind"] == "rejected", "{out}");
    assert_eq!(commits(pm), n);

    // Without --author the comment is the ambient alias, as `issue comment`.
    let c = f.file("c.md", "hello\n");
    let (ok, out) = cli_env(
        pm,
        f.state.path(),
        &["issue", "edit", "CAD-3", "--comment-file", &c],
        &[("CADENCE_ALIAS", "worker-1")],
    );
    assert!(ok, "{out}");
    assert!(out["comment"].as_str().unwrap().ends_with("-worker-1.md"));
    let msg = git(pm, &["log", "-1", "--format=%B"]);
    assert!(msg.contains("Actor: worker-1"), "{msg}");

    // A set-only edit takes its Actor from the alias like `issue set`.
    let (ok, out) = cli_env(
        pm,
        f.state.path(),
        &["issue", "edit", "CAD-3", "--set", "size=M"],
        &[("CADENCE_ALIAS", "worker-2")],
    );
    assert!(ok, "{out}");
    assert!(git(pm, &["log", "-1", "--format=%B"]).contains("Actor: worker-2"));
}

#[test]
fn concurrent_edits_each_commit_once_and_lose_nothing() {
    let f = fx();
    let pm = f.pm.path();
    let n = commits(pm);
    let mut kids = Vec::new();
    for i in 0..4 {
        let mut cmd = Command::new(bin());
        cmd.arg("--state-dir")
            .arg(f.state.path())
            .args([
                "issue",
                "edit",
                "CAD-3",
                "--tag",
                &format!("+t{i}"),
                "--ref",
            ])
            .arg(format!("note:n{i}"))
            .env("CADENCE_PM_DIR", pm)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    Path::new(bin()).parent().unwrap().display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("HOME", f.state.path())
            .env_remove("CADENCE_ALIAS")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        kids.push(cmd.spawn().unwrap());
    }
    for k in kids {
        let out = k.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert_eq!(commits(pm), n + 4);
    let issue = std::fs::read_to_string(pm.join("cadence/CAD-3/issue.md")).unwrap();
    for i in 0..4 {
        assert!(issue.contains(&format!("n{i}")), "{issue}");
        assert!(issue.contains(&format!("t{i}")), "{issue}");
    }
}

#[test]
fn single_purpose_verbs_still_make_one_commit_each() {
    let f = fx();
    let pm = f.pm.path();
    let verbs: [&[&str]; 6] = [
        &["issue", "set", "CAD-3", "priority=P0"],
        &["issue", "tag", "CAD-3", "add", "solo"],
        &["issue", "link", "CAD-3", "relates", "CAD-2"],
        &["issue", "unlink", "CAD-3", "relates", "CAD-2"],
        &["issue", "ref", "CAD-3", "note", "x"],
        &["issue", "comment", "CAD-3", "-m", "hi"],
    ];
    for args in verbs {
        let n = commits(pm);
        let (ok, out) = f.cli(args);
        assert!(ok, "{args:?}: {out}");
        assert_eq!(commits(pm), n + 1, "{args:?}: one commit");
    }
}

#[test]
fn blocked_by_link_warns_and_notifies_holders_like_issue_link() {
    let f = fx();
    // seed leaves CAD-2 `doing`, owned by `me`.
    let (ok, link) = f.cli(&["issue", "link", "CAD-2", "blocked_by", "CAD-3"]);
    assert!(ok, "{link}");
    assert!(
        f.cli(&["issue", "unlink", "CAD-2", "blocked_by", "CAD-3"])
            .0
    );
    let (ok, out) = f.cli(&[
        "issue",
        "edit",
        "CAD-2",
        "--link",
        "blocked_by:CAD-3",
        "--set",
        "priority=P0",
    ]);
    assert!(ok, "{out}");
    let notice = &out["blocker_notices"][0];
    assert_eq!(notice["target"], "CAD-3", "{out}");
    assert_eq!(
        notice["notified"][0]["to"], link["blocker_notice"]["notified"][0]["to"],
        "edit notifies the same holders `issue link` does: {out}"
    );
    assert_eq!(notice["notified"][0]["to"], "me", "{out}");
    assert_eq!(out["warnings"], link["warnings"], "{out}");
    assert!(
        out["warnings"][0].as_str().unwrap().contains("CAD-3"),
        "{out}"
    );
}

#[test]
fn bare_comment_stdin_is_capped() {
    let f = fx();
    let pm = f.pm.path();
    let (ok, out) = f.stdin("piped bare\n", &["issue", "comment", "CAD-3"]);
    assert!(ok, "{out}");
    let n = commits(pm);
    let big = "x".repeat((4 << 20) + 10);
    let (ok, out) = f.stdin(&big, &["issue", "comment", "CAD-3"]);
    assert!(
        !ok && out["kind"] == "rejected",
        "{}",
        out.to_string().len()
    );
    assert_eq!(commits(pm), n);
}

/// Run the CLI with explicit env, keeping stdout and stderr apart.
fn run_split(f: &Fx, env: &[(&str, &Path)], args: &[&str]) -> (bool, Value, String) {
    let mut cmd = Command::new(bin());
    cmd.arg("--state-dir")
        .arg(f.state.path())
        .args(args)
        .env("CADENCE_PM_DIR", f.pm.path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                Path::new(bin()).parent().unwrap().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env_remove("CADENCE_ALIAS");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let text = if out.stdout.is_empty() {
        String::from_utf8_lossy(&out.stderr).to_string()
    } else {
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let json: Value =
        serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}"));
    (
        out.status.success(),
        json,
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// A post-commit hook that corrupts a second project's `project.yaml`:
/// the edit commits cleanly, then every board load fails.
fn break_the_board_after_commit(pm: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let hook = pm.join(".git/hooks/post-commit");
    // Prepend: the installed hook exits early when no remote exists.
    let text = format!(
        "#!/bin/sh\nmkdir -p BAD && printf 'key: [unclosed\\n' > BAD/project.yaml\n{}",
        std::fs::read_to_string(&hook)
            .unwrap()
            .trim_start_matches("#!/bin/sh\n")
    );
    std::fs::write(&hook, text).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn comment_files(pm: &Path, id: &str) -> usize {
    std::fs::read_dir(pm.join("cadence").join(id).join("comments"))
        .map(|rd| rd.flatten().count())
        .unwrap_or(0)
}

#[test]
fn a_post_commit_failure_never_reports_a_landed_edit_as_failed() {
    let f = fx();
    let pm = f.pm.path();
    let body = f.file("c.md", "landed once\n");
    break_the_board_after_commit(pm);
    let n = commits(pm);
    let (ok, out, stderr) = run_split(
        &f,
        &[],
        &[
            "issue",
            "edit",
            "CAD-3",
            "--set",
            "priority=P1",
            "--comment-file",
            &body,
        ],
    );
    assert!(ok, "a landed edit reported failure: {out} / {stderr}");
    assert_eq!(out["committed"], true, "{out}");
    assert_eq!(commits(pm), n + 1, "one commit");
    assert_eq!(comment_files(pm, "CAD-3"), 1, "exactly one comment");
    let warns = out["post_commit_warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("{out}"));
    assert!(
        warns.iter().any(|w| w
            .as_str()
            .unwrap()
            .contains("blocked-by warnings unavailable")),
        "{out}"
    );
    assert!(stderr.contains("warning:"), "stderr names it: {stderr}");
    assert_eq!(out["warnings"], serde_json::json!([]), "{out}");

    // `issue link` has the same shape: committed, exit 0, warned.
    let f = fx();
    break_the_board_after_commit(f.pm.path());
    let n = commits(f.pm.path());
    let (ok, out, stderr) = run_split(&f, &[], &["issue", "link", "CAD-3", "relates", "CAD-1"]);
    assert!(ok, "a landed link reported failure: {out} / {stderr}");
    assert_eq!(commits(f.pm.path()), n + 1);
    assert!(out["post_commit_warnings"][0]
        .as_str()
        .unwrap()
        .contains("unavailable"));
}

#[test]
fn link_then_unlink_in_one_edit_notifies_nobody() {
    let f = fx();
    // seed leaves CAD-2 `doing`, owned by `me` — a holder to notify.
    let (ok, out) = f.cli(&[
        "issue",
        "edit",
        "CAD-2",
        "--link",
        "blocked_by:CAD-3",
        "--unlink",
        "blocked_by:CAD-3",
        "--set",
        "priority=P0",
    ]);
    assert!(ok, "{out}");
    assert!(
        out.get("blocker_notices").is_none(),
        "the target is not in the final blocked_by: {out}"
    );
    let (_, shown) = f.cli(&["issue", "show", "CAD-2", "--json"]);
    assert!(
        shown["blocked_by"].as_array().is_none_or(|a| a.is_empty()),
        "{shown}"
    );
}

/// A daemon stand-in on `<state>/cadence.sock` that logs each method.
fn fake_daemon(state: &Path) -> std::sync::mpsc::Receiver<String> {
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;
    let listener = UnixListener::bind(state.join("cadence.sock")).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            let _ = BufReader::new(&stream).read_line(&mut line);
            if let Ok(req) = serde_json::from_str::<Value>(&line) {
                let _ = tx.send(req["method"].as_str().unwrap_or("").to_string());
            }
            let mut w = &stream;
            let _ = writeln!(
                w,
                r#"{{"ok":true,"result":{{"jobs":[],"duplicate":false}}}}"#
            );
        }
    });
    rx
}

#[test]
fn link_unlink_and_edit_use_the_state_dir_they_were_given() {
    let f = fx();
    let rx = fake_daemon(f.state.path());
    // The ambient default points somewhere with no daemon at all.
    let other = TempDir::new().unwrap();
    let env: [(&str, &Path); 2] = [("HOME", other.path()), ("CADENCE_STATE_DIR", other.path())];
    let seen = |rx: &std::sync::mpsc::Receiver<String>| -> Vec<String> { rx.try_iter().collect() };

    let (ok, out, _) = run_split(&f, &env, &["issue", "link", "CAD-2", "blocked_by", "CAD-3"]);
    assert!(ok, "{out}");
    assert_eq!(
        out["blocker_notice"]["notified"][0]["sent"], true,
        "the notice reached --state-dir's daemon: {out}"
    );
    assert!(seen(&rx).contains(&"agent_send".to_string()));

    let (ok, out, _) = run_split(
        &f,
        &env,
        &["issue", "unlink", "CAD-2", "blocked_by", "CAD-3"],
    );
    assert!(ok, "{out}");
    assert!(
        seen(&rx).contains(&"job_list".to_string()),
        "unlink looked jobs up on --state-dir's daemon"
    );

    let (ok, out, _) = run_split(
        &f,
        &env,
        &[
            "issue",
            "edit",
            "CAD-2",
            "--link",
            "blocked_by:CAD-3",
            "--set",
            "priority=P0",
        ],
    );
    assert!(ok, "{out}");
    assert_eq!(
        out["blocker_notices"][0]["notified"][0]["sent"], true,
        "{out}"
    );
    let methods = seen(&rx);
    assert!(
        methods.contains(&"agent_send".to_string()) && methods.contains(&"job_list".to_string()),
        "{methods:?}"
    );
}

#[test]
fn a_verdict_attachment_in_the_same_edit_is_evidence_for_done() {
    let f = fx();
    let pm = f.pm.path();
    let plain = f.file("notes.txt", "n\n");
    let n = commits(pm);
    let (ok, out) = f.cli(&[
        "issue",
        "edit",
        "CAD-3",
        "--set",
        "status=done",
        "--attach",
        &plain,
    ]);
    assert!(
        !ok && out["error"].as_str().unwrap().contains("needs evidence"),
        "{out}"
    );
    assert_eq!(commits(pm), n, "a refused edit commits nothing");
    let verdict = f.file("review-r1.md", "pass\n");
    let (ok, out) = f.cli(&[
        "issue",
        "edit",
        "CAD-3",
        "--set",
        "status=done",
        "--attach",
        &verdict,
    ]);
    assert!(ok, "{out}");
    let (_, shown) = f.cli(&["issue", "show", "CAD-3", "--json"]);
    assert_eq!(shown["status"], "done");
}

#[test]
fn the_worktree_hint_follows_any_status_done_like_issue_set() {
    let f = fx();
    let (ok, out) = f.cli(&[
        "issue",
        "edit",
        "CAD-3",
        "--ref",
        "worktree:/tmp/cad908-wt",
        "--ref",
        "pr:https://example.com/pr/1",
        "--set",
        "status=done",
    ]);
    assert!(ok, "{out}");
    assert_eq!(out["worktree_open"][0], "CAD-3", "{out}");
    // Already done: `issue set status=done` still prints the hint, so
    // an edit that restates it does too.
    let (ok, set) = f.cli(&["issue", "set", "CAD-3", "status=done", "priority=P2"]);
    assert!(ok, "{set}");
    assert_eq!(set["worktree_open"][0], "CAD-3", "{set}");
    let (ok, out) = f.cli(&[
        "issue",
        "edit",
        "CAD-3",
        "--set",
        "status=done",
        "--set",
        "priority=P1",
    ]);
    assert!(ok, "{out}");
    assert_eq!(out["worktree_open"], set["worktree_open"], "{out}");
    // Without a status=done the hint stays quiet.
    let (ok, out) = f.cli(&["issue", "edit", "CAD-3", "--set", "priority=P0"]);
    assert!(ok, "{out}");
    assert!(out.get("worktree_open").is_none(), "{out}");
}

/// `edit --comment-file -` on a terminal refuses instead of hanging.
#[test]
fn edit_comment_file_stdin_on_a_terminal_is_refused() {
    use std::os::fd::{FromRawFd, OwnedFd};
    let f = fx();
    let pm = f.pm.path();
    let (mut m, mut s) = (0, 0);
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut m,
                &mut s,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        },
        0
    );
    let (_master, slave) = unsafe { (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s)) };
    let n = commits(pm);
    let out = Command::new(bin())
        .arg("--state-dir")
        .arg(f.state.path())
        .args(["issue", "edit", "CAD-3", "--comment-file", "-"])
        .env("CADENCE_PM_DIR", pm)
        .env_remove("CADENCE_ALIAS")
        .stdin(Stdio::from(slave))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("stdin is a terminal"), "{text}");
    assert_eq!(commits(pm), n);
}

#[test]
fn edit_comment_file_stdin_is_capped_at_4_mib() {
    let f = fx();
    let pm = f.pm.path();
    let n = commits(pm);
    let big = "x".repeat((4 << 20) + 10);
    let (ok, out) = f.stdin(&big, &["issue", "edit", "CAD-3", "--comment-file", "-"]);
    assert!(
        !ok && out["kind"] == "rejected" && out["error"].as_str().unwrap().contains("4 MiB"),
        "{out}"
    );
    assert_eq!(commits(pm), n);
    assert_eq!(comment_files(pm, "CAD-3"), 0);
    // Exactly at the cap is fine.
    let at_cap = "y".repeat(4 << 20);
    let (ok, out) = f.stdin(&at_cap, &["issue", "edit", "CAD-3", "--comment-file", "-"]);
    assert!(ok, "{}", out.to_string().len());
}
