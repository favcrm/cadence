//! CAD-606: operator-gated board kickoff. An agent, its detached
//! child, a forged identity field, an unknown provider, an off-policy
//! model and a path-shaped alias are refused before a lane exists.
//! One operator kickoff joins a worker and dispatches; a second call
//! and a racing pair leave exactly one lane. The board route is at
//! least as strict as the RPC.
#![allow(clippy::disallowed_methods)]
mod common;
use common::*;

use serde_json::{json, Value};
use std::path::Path;
use std::thread;

struct Lab {
    d: TestDaemon,
    tmp: tempfile::TempDir,
    pm_dir: std::path::PathBuf,
    repo: std::path::PathBuf,
    home: std::path::PathBuf,
}

impl Lab {
    fn new() -> Self {
        let (tmp, pm_dir, repo, home) = pm_lab_dirs();
        test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
        let git = git_ok();
        git_f_repo(&repo, &git, |_| {});
        let d = TestDaemon::start();
        d.register("pm");
        d.wait_agent("pm", "idle", 15);
        let lab = Self {
            d,
            tmp,
            pm_dir,
            repo,
            home,
        };
        let (ok, out, err) = lab.cli(&["issue", "init"]);
        assert!(ok, "{out} {err}");
        let repo_s = lab.repo.canonicalize().unwrap();
        let repo_s = repo_s.to_string_lossy().into_owned();
        let (ok, out, err) = lab.cli(&[
            "issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s,
        ]);
        assert!(ok, "{out} {err}");
        lab
    }

    fn cli(&self, args: &[&str]) -> (bool, Value, String) {
        let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence")).parent().unwrap();
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&self.d.state)
            .args(args)
            .env("CADENCE_PM_DIR", &self.pm_dir)
            .env("HOME", &self.home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .operator_output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let text = if out.stdout.is_empty() {
            stderr.clone()
        } else {
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        (
            out.status.success(),
            serde_json::from_str(text.trim()).unwrap_or(Value::Null),
            stderr,
        )
    }

    fn accept(&self, id: &str) {
        let criteria = self.tmp.path().join(format!("acc-{id}.md"));
        std::fs::write(&criteria, "- [ ] the worker lands in the lane\n").unwrap();
        let (ok, out, err) = self.cli(&[
            "issue",
            "acceptance",
            id,
            "--from",
            criteria.to_str().unwrap(),
        ]);
        assert!(ok, "{id}: {out} {err}");
    }

    fn lanes(&self) -> Vec<String> {
        let dir = self.repo.join(".cadence/wt");
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

fn msg(err: impl ToString) -> String {
    err.to_string()
}

/// Agent, detached child, and forged identity fields never open a lane.
#[test]
fn cad606_kickoff_refuses_agent_detached_and_forged_fields() {
    let lab = Lab::new();
    let (ok, _, err) = lab.cli(&["issue", "new", "Empty", "--project", "demo"]);
    assert!(ok, "{err}");
    lab.accept("D-1");
    let body = json!({"issue": "D-1", "group": "pm", "provider": "fake"});

    let mut wk = ManagedWorker::start(&lab.d, "wk");
    for how in ["self", "detached"] {
        let frame = wk.rpc(how, "issue_kickoff", body.clone());
        assert_eq!(frame["ok"], false, "{how}: {frame}");
        let text = frame["error"]["message"].as_str().unwrap_or("");
        assert!(
            text.contains("issue kickoff is an operator action"),
            "{how} must be refused as an operator action: {frame}"
        );
    }
    assert!(lab.lanes().is_empty(), "a refused caller opened a lane");

    for field in ["by", "actor", "operator"] {
        let mut forged = body.clone();
        forged[field] = json!("operator");
        let err = msg(lab.d.operator_rpc("issue_kickoff", forged).unwrap_err());
        assert!(
            err.contains("request field") && err.contains(field),
            "{field}: {err}"
        );
    }
    assert!(lab.lanes().is_empty(), "a forged field opened a lane");
}

/// Unknown provider, off-policy model, path-shaped names, empty
/// acceptance — each refuses, and none of them creates a worktree.
#[test]
fn cad606_kickoff_refuses_bad_launch_and_empty_acceptance() {
    let lab = Lab::new();
    let (ok, _, err) = lab.cli(&["issue", "new", "Empty", "--project", "demo"]);
    assert!(ok, "{err}");
    let (ok, _, err) = lab.cli(&["issue", "new", "Full", "--project", "demo"]);
    assert!(ok, "{err}");
    lab.accept("D-2");

    let err = msg(lab
        .d
        .operator_rpc(
            "issue_kickoff",
            json!({"issue": "D-2", "group": "pm", "provider": "nope"}),
        )
        .unwrap_err());
    assert!(err.contains("Unknown provider"), "{err}");

    let err = msg(lab
        .d
        .operator_rpc(
            "issue_kickoff",
            json!({
                "issue": "D-2", "group": "pm", "provider": "pi",
                "model": "devin/not-on-the-list"
            }),
        )
        .unwrap_err());
    assert!(err.contains("allowlist") || err.contains("not on"), "{err}");

    let err = msg(lab
        .d
        .operator_rpc(
            "issue_kickoff",
            json!({"issue": "D-2", "group": "pm", "provider": "fake", "alias": "../wt"}),
        )
        .unwrap_err());
    assert!(err.contains("not an alias"), "{err}");

    let err = msg(lab
        .d
        .operator_rpc(
            "issue_kickoff",
            json!({"issue": "D-2", "group": "../pm", "provider": "fake"}),
        )
        .unwrap_err());
    assert!(err.contains("not an alias"), "{err}");

    let err = msg(lab
        .d
        .operator_rpc(
            "issue_kickoff",
            json!({"issue": "D-2", "group": "pm", "provider": "fake", "note": "a\nb"}),
        )
        .unwrap_err());
    assert!(err.contains("one line"), "{err}");

    let err = msg(lab
        .d
        .operator_rpc(
            "issue_kickoff",
            json!({"issue": "D-1", "group": "pm", "provider": "fake", "alias": "lane-1"}),
        )
        .unwrap_err());
    assert!(err.contains("no acceptance"), "{err}");
    assert!(
        lab.lanes().is_empty(),
        "a refusal created a lane: {:?}",
        lab.lanes()
    );
    let err = msg(lab
        .d
        .operator_rpc("agent_show", json!({"alias": "lane-1"}))
        .unwrap_err());
    assert!(err.contains("Unknown") || err.contains("no such"), "{err}");
}

/// One kickoff joins the worker with the lane as cwd and appends the
/// note as a comment. A second call and two racers leave that one lane.
#[test]
fn cad606_kickoff_joins_once_and_returns_the_live_lane() {
    let lab = Lab::new();
    let (ok, _, err) = lab.cli(&["issue", "new", "Full", "--project", "demo"]);
    assert!(ok, "{err}");
    lab.accept("D-1");
    let note = "operator-note-cad606";
    let out = lab
        .d
        .operator_rpc(
            "issue_kickoff",
            json!({
                "issue": "D-1", "group": "pm", "provider": "fake",
                "alias": "w-kick", "note": note
            }),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(out["dispatched"], json!(true), "{out}");
    assert_eq!(out["worker"], "w-kick", "{out}");
    assert_eq!(out["group"], "pm", "{out}");
    let wt = out["worktree"].as_str().unwrap();
    assert!(wt.contains(".cadence/wt"), "{wt}");
    assert_eq!(lab.lanes().len(), 1, "{:?}", lab.lanes());

    let show = lab
        .d
        .operator_rpc("agent_show", json!({"alias": "w-kick"}))
        .unwrap();
    assert_eq!(show["agent"]["params"]["upstream"], "pm", "{show}");
    assert_eq!(show["agent"]["provider"], "fake", "{show}");
    let cwd = show["agent"]["cwd"].as_str().unwrap();
    assert_eq!(
        Path::new(cwd).canonicalize().unwrap(),
        Path::new(wt).canonicalize().unwrap(),
        "join cwd must be the issue-start worktree"
    );

    let issue_md = std::fs::read_to_string(lab.pm_dir.join("demo/D-1/issue.md")).unwrap();
    assert!(
        issue_md.contains("the worker lands in the lane"),
        "the note replaced the issue body: {issue_md}"
    );
    assert!(
        !issue_md.contains(note),
        "the operator note was written into issue.md: {issue_md}"
    );
    let comments = lab.pm_dir.join("demo/D-1/comments");
    let noted = std::fs::read_dir(&comments)
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| {
            std::fs::read_to_string(e.path())
                .unwrap_or_default()
                .contains(note)
        });
    assert!(noted, "the operator note was not appended as a comment");

    let again = lab
        .d
        .operator_rpc(
            "issue_kickoff",
            json!({
                "issue": "D-1", "group": "pm", "provider": "fake", "alias": "w-other"
            }),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(again["dispatched"], json!(false), "{again}");
    assert_eq!(again["duplicate_kind"], "lane", "{again}");
    assert_eq!(lab.lanes().len(), 1, "{:?}", lab.lanes());
    assert!(
        lab.d
            .operator_rpc("agent_show", json!({"alias": "w-other"}))
            .is_err(),
        "a second kickoff joined another worker"
    );

    let (ok, _, err) = lab.cli(&["issue", "new", "Race", "--project", "demo"]);
    assert!(ok, "{err}");
    lab.accept("D-2");
    thread::scope(|s| {
        for alias in ["race-a", "race-b"] {
            let d = &lab.d;
            s.spawn(move || {
                d.operator_rpc(
                    "issue_kickoff",
                    json!({
                        "issue": "D-2", "group": "pm", "provider": "fake", "alias": alias
                    }),
                )
                .unwrap_or_else(|e| panic!("{alias}: {e}"));
            });
        }
    });
    assert_eq!(
        lab.lanes().len(),
        2,
        "race opened a second lane: {:?}",
        lab.lanes()
    );
    let shown = ["race-a", "race-b"]
        .into_iter()
        .filter(|a| {
            lab.d
                .operator_rpc("agent_show", json!({"alias": a}))
                .is_ok()
        })
        .count();
    assert_eq!(shown, 1, "the race joined both workers");
}

/// The board refuses the same callers the RPC does, and an operator
/// POST joins one lane.
#[test]
fn cad606_board_kickoff_is_as_strict_as_the_rpc() {
    let lab = Lab::new();
    let (ok, _, err) = lab.cli(&["issue", "new", "Full", "--project", "demo"]);
    assert!(ok, "{err}");
    lab.accept("D-1");
    lab.d.register("w1");
    lab.d.wait_agent("w1", "idle", 15);
    let port = start_board(&lab.pm_dir, &lab.d.state);
    let session = op::sign_in(env!("CARGO_BIN_EXE_cadence"), &lab.d.state, port);
    let path = "/api/issues/D-1/kickoff";
    let body = r#"{"group":"pm","provider":"fake","alias":"board-w"}"#;

    // Options and the operator writes first. An agent presenting this
    // session is a stolen-session revocation, which would kill the
    // cookie the later reads need.
    let options = session.request("GET", path, "");
    let (status, _, resp) = op::raw(port, &options);
    assert_eq!(status, 200, "options: {resp}");
    assert!(resp.contains("\"fake\""), "{resp}");
    assert!(resp.contains("\"pm\""), "{resp}");

    let mut bare = session.request("POST", path, body);
    bare = bare.replace(&format!("Origin: {}\r\n", session.origin), "");
    let (status, _, resp) = op::raw(port, &bare);
    assert_eq!(status, 403, "missing origin: {resp}");

    let forged = session.request(
        "POST",
        path,
        r#"{"group":"pm","provider":"fake","by":"operator"}"#,
    );
    let (status, _, resp) = op::raw(port, &forged);
    assert_eq!(status, 400, "forged by: {resp}");
    assert!(resp.contains("by") || resp.contains("unknown"), "{resp}");

    let unknown = session.request("POST", path, r#"{"group":"pm","provider":"nope"}"#);
    let (status, _, resp) = op::raw(port, &unknown);
    assert_eq!(status, 400, "{resp}");
    assert!(resp.contains("Unknown provider"), "{resp}");
    assert!(lab.lanes().is_empty(), "{:?}", lab.lanes());

    let (status, _, resp) = op::raw(port, &session.request("POST", path, body));
    assert_eq!(status, 200, "{resp}");
    let v: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["kickoff"]["dispatched"], json!(true), "{v}");
    assert_eq!(v["card"]["id"], "D-1", "{v}");
    assert_eq!(lab.lanes().len(), 1, "{:?}", lab.lanes());

    let again = session.request(
        "POST",
        path,
        r#"{"group":"pm","provider":"nope","alias":"nope-w"}"#,
    );
    let (status, _, resp) = op::raw(port, &again);
    assert_eq!(status, 400, "{resp}");
    assert_eq!(lab.lanes().len(), 1, "{:?}", lab.lanes());

    let agent = session.request_as(
        "POST",
        path,
        body,
        &op::seam_headers(&lab.d.state, "agent:w1"),
    );
    let (status, _, resp) = op::raw(port, &agent);
    assert_eq!(status, 403, "agent session: {resp}");
    assert!(
        resp.contains("operator") || resp.contains("agent"),
        "{resp}"
    );

    let detached = session.request_as(
        "POST",
        path,
        body,
        &op::seam_headers(&lab.d.state, "unproven"),
    );
    let (status, _, resp) = op::raw(port, &detached);
    assert_eq!(status, 403, "unproven: {resp}");
}
