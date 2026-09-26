//! CAD-608: lane reassign and the issue-scoped board relays.
//! Operator-only, same worktree, exactly one live lane. Guards are
//! proved by the tests in this file (agent, detached child, forged
//! fields, off-policy model, cross-issue `from`, concurrent reassign).
#![allow(clippy::disallowed_methods)]
mod common;
use common::*;

use std::path::{Path, PathBuf};
use std::thread;

use serde_json::{json, Value};

struct Lab {
    _tmp: tempfile::TempDir,
    d: TestDaemon,
    pm_dir: PathBuf,
    home: PathBuf,
    state: PathBuf,
}

fn lab() -> Lab {
    let (tmp, pm_dir, repo, home) = pm_lab_dirs();
    let git = git_ok();
    git_f_repo(&repo, &git, |_| {});
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let d = TestDaemon::start();
    let state = d.state.clone();
    let cli = cadence_cli_json(&state, &pm_dir, &home);
    let repo_s = repo.canonicalize().unwrap().to_string_lossy().into_owned();
    demo_project_init(&cli, &repo_s);
    demo_issue_news(&cli, &["Lane one", "Lane two"]);
    let acc = tmp.path().join("acc.md");
    std::fs::write(&acc, "- [ ] keep the worktree\n").unwrap();
    let acc_s = acc.to_str().unwrap().to_string();
    assert!(cli(&["issue", "acceptance", "D-1", "--from", &acc_s]).0);
    assert!(cli(&["issue", "acceptance", "D-2", "--from", &acc_s]).0);
    let pm_cwd = pm_dir
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    d.operator_rpc(
        "agent_register",
        json!({
            "alias": "pm",
            "provider": "fake",
            "endpoint_kind": "fake",
            "role": "pm",
            "cwd": pm_cwd,
        }),
    )
    .unwrap();
    Lab {
        _tmp: tmp,
        d,
        pm_dir,
        home,
        state,
    }
}

fn cli(lab: &Lab) -> impl Fn(&[&str]) -> (bool, Value) + '_ {
    cadence_cli_json(&lab.state, &lab.pm_dir, &lab.home)
}

fn show(lab: &Lab, id: &str) -> Value {
    let (ok, out) = cli(lab)(&["issue", "show", id, "--json"]);
    assert!(ok, "{id} show: {out}");
    out
}

fn open_worktrees(issue: &Value) -> Vec<String> {
    let Some(refs) = issue["refs"].as_array() else {
        return Vec::new();
    };
    refs.iter()
        .filter(|r| r["kind"] == "worktree" && r["closed"] != true)
        .filter_map(|r| r["path"].as_str().map(str::to_string))
        .collect()
}

fn start_lane(lab: &Lab, id: &str, alias: &str) -> PathBuf {
    let (ok, out) = cli(lab)(&["issue", "start", id, "--by", "pm", "--owner", alias]);
    assert!(ok, "start {id}: {out}");
    let trees = open_worktrees(&show(lab, id));
    assert_eq!(trees.len(), 1, "{id} worktrees: {trees:?}");
    let wt = PathBuf::from(&trees[0]).canonicalize().unwrap();
    lab.d
        .operator_rpc(
            "agent_register",
            json!({
                "alias": alias,
                "provider": "fake",
                "endpoint_kind": "fake",
                "role": "worker",
                "cwd": wt,
                "params": "{\"upstream\":\"pm\"}",
            }),
        )
        .unwrap();
    lab.d.wait_agent(alias, "idle", 20);
    let (ok, dispatched) = cli(lab)(&["dispatch", id, "--to", alias, "--reply-to", "pm"]);
    assert!(
        ok && dispatched["dispatched"] == true,
        "dispatch {id}: {dispatched}"
    );
    wt
}

fn agent(lab: &Lab, alias: &str) -> Value {
    lab.d
        .operator_rpc("agent_show", json!({"alias": alias}))
        .unwrap()["agent"]
        .clone()
}

fn enabled_workers(lab: &Lab, wt: &Path) -> Vec<String> {
    let want = wt.canonicalize().unwrap();
    let list = lab.d.operator_rpc("agent_list", json!({})).unwrap();
    list["agents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["role"] == "worker" && a["enabled"] == true)
        .filter(|a| {
            a["cwd"]
                .as_str()
                .and_then(|p| Path::new(p).canonicalize().ok())
                .as_ref()
                == Some(&want)
        })
        .filter_map(|a| a["alias"].as_str().map(str::to_string))
        .collect()
}

fn op(state: &Path, method: &str, params: Value) -> cadence_agent::Result<Value> {
    cadence_agent::test_seam::scoped(cadence_agent::test_seam::Asserted::Operator, || {
        cadence_agent::client::rpc(state, method, params)
    })
}

#[test]
fn reassign_refuses_agent_detached_and_forged_fields() {
    let lab = lab();
    let wt = start_lane(&lab, "D-1", "w-old");
    let worker = ManagedWorker::start(&lab.d, "intruder");
    let params = json!({"issue": "D-1", "provider": "fake", "alias": "w-sneak"});
    let mut worker = worker;
    let as_self = worker.rpc("self", "lane_reassign", params.clone());
    assert_refused(&as_self, "lane reassign", "operator action", "agent self");
    let detached = worker.rpc("detached", "lane_reassign", params.clone());
    assert_refused(
        &detached,
        "lane reassign",
        "operator action",
        "detached child",
    );
    for field in ["by", "actor", "operator"] {
        let mut forged = params.clone();
        forged[field] = json!("operator");
        let err = lab
            .d
            .operator_rpc("lane_reassign", forged)
            .unwrap_err()
            .to_string();
        assert!(err.contains("request field"), "forged {field}: {err}");
    }
    assert!(
        lab.d
            .operator_rpc("agent_show", json!({"alias": "w-sneak"}))
            .is_err(),
        "a refused reassign registered a worker"
    );
    assert_eq!(enabled_workers(&lab, &wt), vec!["w-old".to_string()]);
    assert!(agent(&lab, "w-old")["enabled"] == true);
}

#[test]
fn reassign_refuses_an_off_policy_model_before_stopping() {
    let lab = lab();
    let wt = start_lane(&lab, "D-1", "w-old");
    let err = lab
        .d
        .operator_rpc(
            "lane_reassign",
            json!({
                "issue": "D-1",
                "provider": "pi",
                "model": "devin/not-on-the-list",
                "alias": "w-off",
            }),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("allowlist"), "{err}");
    assert!(agent(&lab, "w-old")["enabled"] == true);
    assert!(lab
        .d
        .operator_rpc("agent_show", json!({"alias": "w-off"}))
        .is_err());
    assert_eq!(enabled_workers(&lab, &wt), vec!["w-old".to_string()]);
}

#[test]
fn reassign_refuses_another_issues_lane() {
    let lab = lab();
    let wt1 = start_lane(&lab, "D-1", "w-old");
    let wt2 = start_lane(&lab, "D-2", "w-other");
    let err = lab
        .d
        .operator_rpc(
            "lane_reassign",
            json!({
                "issue": "D-1",
                "provider": "fake",
                "alias": "w-new",
                "from": "w-other",
            }),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("another issue"), "{err}");
    assert!(agent(&lab, "w-old")["enabled"] == true);
    assert!(agent(&lab, "w-other")["enabled"] == true);
    assert_eq!(enabled_workers(&lab, &wt1), vec!["w-old".to_string()]);
    assert_eq!(enabled_workers(&lab, &wt2), vec!["w-other".to_string()]);
}

#[test]
fn reassign_keeps_the_worktree_and_one_lane() {
    let lab = lab();
    let wt = start_lane(&lab, "D-1", "w-old");
    let out = lab
        .d
        .operator_rpc(
            "lane_reassign",
            json!({
                "issue": "D-1",
                "provider": "fake",
                "alias": "w-new",
                "note": "pick up the open diff",
            }),
        )
        .unwrap();
    assert_eq!(out["dispatched"], true, "{out}");
    assert!(
        out["continuity"].as_str().unwrap_or("").contains("w-old"),
        "{out}"
    );
    let shown = show(&lab, "D-1");
    let trees = open_worktrees(&shown);
    assert_eq!(trees.len(), 1, "{trees:?}");
    assert_eq!(PathBuf::from(&trees[0]).canonicalize().unwrap(), wt);
    let comments = shown["comments"].to_string();
    assert!(comments.contains("Previous agent was w-old"), "{comments}");
    assert!(
        agent(&lab, "w-old")["enabled"] == false,
        "old lane still enabled"
    );
    assert!(agent(&lab, "w-new")["enabled"] == true);
    assert_eq!(enabled_workers(&lab, &wt), vec!["w-new".to_string()]);
    let card = lab
        .d
        .operator_rpc("lane_show", json!({"issue": "D-1"}))
        .unwrap();
    assert_eq!(card["lane"]["agent"], "w-new", "{card}");
    assert_eq!(card["lane"]["provider"], "fake");
    assert!(
        matches!(card["lane"]["state"].as_str(), Some("busy") | Some("idle")),
        "{card}"
    );
}

struct ClearStopFail;

impl Drop for ClearStopFail {
    fn drop(&mut self) {
        test_env().remove("CADENCE_TEST_STOP_FAILS");
    }
}

/// No stop-failure seam existed. This uses the same `ProviderEnv::own`
/// shape as `CADENCE_TEST_INTERRUPT_PAUSE_MS`: the stop of the previous
/// agent fails before it is disabled, after the new one has dispatched.
#[test]
fn reassign_names_both_aliases_when_the_previous_stop_fails() {
    let lab = lab();
    let wt = start_lane(&lab, "D-1", "w-old");
    test_env().set("CADENCE_TEST_STOP_FAILS", "w-old");
    let _clear = ClearStopFail;
    let err = lab
        .d
        .operator_rpc(
            "lane_reassign",
            json!({
                "issue": "D-1",
                "provider": "fake",
                "alias": "w-new",
            }),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("w-old"), "{err}");
    assert!(err.contains("w-new"), "{err}");
    assert!(err.contains("still live"), "{err}");
    assert!(agent(&lab, "w-old")["enabled"] == true, "{err}");
    assert!(agent(&lab, "w-new")["enabled"] == true, "{err}");
    let mut live = enabled_workers(&lab, &wt);
    live.sort();
    assert_eq!(live, vec!["w-new".to_string(), "w-old".to_string()]);
    assert_eq!(open_worktrees(&show(&lab, "D-1")).len(), 1);
}

#[test]
fn two_reassigns_leave_one_enabled_worker() {
    let lab = lab();
    let wt = start_lane(&lab, "D-1", "w-old");
    let state = lab.state.clone();
    let results = thread::scope(|scope| {
        let left = {
            let state = state.clone();
            scope.spawn(move || {
                op(
                    &state,
                    "lane_reassign",
                    json!({"issue": "D-1", "provider": "fake", "alias": "w-a"}),
                )
            })
        };
        let right = {
            let state = state.clone();
            scope.spawn(move || {
                op(
                    &state,
                    "lane_reassign",
                    json!({"issue": "D-1", "provider": "fake", "alias": "w-b"}),
                )
            })
        };
        vec![left.join().unwrap(), right.join().unwrap()]
    });
    assert!(
        results
            .iter()
            .any(|r| r.as_ref().ok().is_some_and(|v| v["dispatched"] == true)),
        "neither reassign dispatched: {results:?}"
    );
    let live = enabled_workers(&lab, &wt);
    assert_eq!(
        live.len(),
        1,
        "enabled on the worktree: {live:?} results {results:?}"
    );
    assert_eq!(open_worktrees(&show(&lab, "D-1")).len(), 1);
}

#[test]
fn board_lane_routes_are_operator_only() {
    let lab = lab();
    let _wt = start_lane(&lab, "D-1", "w-old");
    let port = start_board(&lab.pm_dir, &lab.state);
    let session = sign_in(&lab.state, port);

    let bare = format!(
        "POST /api/issues/D-1/lane/reassign HTTP/1.0\r\nHost: {}\r\nContent-Type: application/json\r\n\
         X-Cadence-Board: 1\r\nCookie: {}\r\n{}\r\n{}\r\nContent-Length: 19\r\n\r\n{{\"provider\":\"fake\"}}",
        session.host,
        session.cookie,
        session.key_header(),
        session.seam,
    );
    let (status, body) = board_http(port, &bare);
    assert_eq!(status, 403, "missing origin: {body}");
    assert!(body.contains("Origin"), "{body}");

    let forged = session.request(
        "POST",
        "/api/issues/D-1/lane/reassign",
        r#"{"provider":"fake","by":"operator"}"#,
    );
    let (status, body) = board_http(port, &forged);
    assert_eq!(status, 400, "forged by: {body}");
    assert!(body.contains("request field"), "{body}");

    let ok = session.request(
        "POST",
        "/api/issues/D-1/lane/reassign",
        r#"{"provider":"fake","alias":"w-board"}"#,
    );
    let (status, body) = board_http(port, &ok);
    assert_eq!(status, 200, "operator reassign: {body}");
    assert!(agent(&lab, "w-old")["enabled"] == false);
    assert!(agent(&lab, "w-board")["enabled"] == true);

    let cross = session.request(
        "POST",
        "/api/issues/D-1/lane/interrupt",
        r#"{"from":"w-old"}"#,
    );
    let (status, body) = board_http(port, &cross);
    assert_eq!(status, 400, "cross-issue: {body}");
    assert!(body.contains("another issue"), "{body}");

    let ask = session.request("POST", "/api/issues/D-1/lane/ask", r#"{"text":"the diff"}"#);
    let (status, body) = board_http(port, &ask);
    assert_eq!(status, 200, "ask: {body}");
    let thread = lab
        .d
        .operator_rpc("thread_read", json!({"alias": "w-board", "tail": true}))
        .unwrap();
    let texts: Vec<&str> = thread["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["role"] == "operator")
        .filter_map(|e| e["text"].as_str())
        .collect();
    assert!(
        texts.iter().any(|t| t.starts_with("status?")),
        "operator status text: {texts:?}"
    );

    let instruct = session.request(
        "POST",
        "/api/issues/D-1/lane/instruct",
        r#"{"text":"land the card"}"#,
    );
    let (status, body) = board_http(port, &instruct);
    assert_eq!(status, 200, "instruct: {body}");
    let thread = lab
        .d
        .operator_rpc("thread_read", json!({"alias": "w-board", "tail": true}))
        .unwrap();
    let texts: Vec<&str> = thread["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["role"] == "operator")
        .filter_map(|e| e["text"].as_str())
        .collect();
    assert!(texts.contains(&"land the card"), "{texts:?}");

    let unfence = session.request("POST", "/api/issues/D-1/lane/unfence", "{}");
    let (status, body) = board_http(port, &unfence);
    assert_eq!(status, 400, "unfence without status: {body}");

    let interrupt = session.request("POST", "/api/issues/D-1/lane/interrupt", "{}");
    let (status, body) = board_http(port, &interrupt);
    assert_eq!(status, 200, "interrupt: {body}");

    // After the operator path: an agent-asserted replay and an unproven
    // one are refused. The agent replay revokes the session, so it is last.
    let unproven = op::seam_headers(&lab.state, "unproven");
    let as_unproven = session.request_as("POST", "/api/issues/D-1/lane/interrupt", "{}", &unproven);
    let (status, body) = board_http(port, &as_unproven);
    assert_eq!(status, 403, "unproven seam: {body}");

    let fresh = sign_in(&lab.state, port);
    let agent_seam = op::seam_headers(&lab.state, "agent:w-board");
    let as_agent = fresh.request_as(
        "POST",
        "/api/issues/D-1/lane/reassign",
        r#"{"provider":"fake","alias":"w-sneak"}"#,
        &agent_seam,
    );
    let (status, body) = board_http(port, &as_agent);
    assert_eq!(status, 403, "agent seam: {body}");
    assert!(
        lab.d
            .operator_rpc("agent_show", json!({"alias": "w-sneak"}))
            .is_err(),
        "agent seam registered a worker"
    );
}
