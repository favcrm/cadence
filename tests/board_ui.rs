//! board_ui: area tests split from tests/integration.rs (CAD-426).
//! End-to-end tests: real socket daemon in-process, fake provider.
//! These exercise the observable contract — queue order, idempotency,
//! restart fencing, approval brokering, serialization — without model calls.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod common;
use common::*;

use cadence_agent::daemon;
use serde_json::json;
use serde_json::Value;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::path::Path;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tempfile::TempDir;

/// `capture-pane` calls recorded by the mock tmux — the probe's
/// signature invocation, counted for the one-probe-per-pane rule.
fn tmux_call_count(mock: &MockDevin, state: &Path, cmd: &str) -> usize {
    let log = mock
        .dir
        .join("tmux-state")
        .join(socket_for(state))
        .join("calls.log");
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.starts_with(cmd))
        .count()
}

#[test]
fn status_rows_probe_once_and_footer() {
    // The stall watch now samples idle panes too — park it far out so
    // a tick cannot land inside the capture-count window below.
    stall_sample(3600);
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    let _chatty = d.mock_claude("chatty", None);
    d.register_devin("dv1", None);
    d.register_devin("dv2", None);
    d.register_inbox("pm");
    d.register_claude("w1", json!({}));
    d.register("fx");
    d.wait_agent("dv1", "idle", 20);
    d.wait_agent("dv2", "idle", 20);
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 15);
    d.wait_agent("fx", "idle", 10);
    // dv2's pane shows the busy watermark — its row must read busy.
    std::fs::write(
        d.pane_file(&mock, "dv2", "tui-state"),
        "⠸ Thinking · 12s (esc twice to interrupt)\n❭ Guide Devin while it works\n",
    )
    .unwrap();
    // w1 mid-turn: chatty never completes, so m1 stays running.
    d.send(
        "w1",
        json!({"text": "draft the migration plan", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("w1", "m1", &["running"], 15);
    // pm holds an undrained message — unread inbox in the footer.
    d.send(
        "pm",
        json!({"text": "note for the operator", "message": "pm1"}),
    )
    .unwrap();
    // fx is fenced: one unknown message, attention state.
    fence_agent(&d, "fx", "mf1");
    // Tracker: one doing and one review issue owned by agents, plus a
    // backlog issue that must NOT appear.
    let home = d.dir.path().join("home");
    let pm_dir = d.dir.path().join("pm");
    std::fs::create_dir_all(&home).unwrap();
    issue_cli(&home, &d.state, &pm_dir, &["issue", "init"]);
    issue_cli(
        &home,
        &d.state,
        &pm_dir,
        &["issue", "project", "add", "cadence", "--prefix", "CAD"],
    );
    for title in ["one", "two", "three"] {
        issue_cli(
            &home,
            &d.state,
            &pm_dir,
            &["issue", "new", title, "--project", "cadence"],
        );
    }
    issue_cli(
        &home,
        &d.state,
        &pm_dir,
        &["issue", "set", "CAD-1", "status=doing", "owner=w1"],
    );
    issue_cli(
        &home,
        &d.state,
        &pm_dir,
        &["issue", "set", "CAD-2", "status=review", "owner=dv1"],
    );
    issue_cli(
        &home,
        &d.state,
        &pm_dir,
        &["issue", "set", "CAD-3", "owner=w1"],
    );
    // The stall watch samples each new pty pane once at registration —
    // the interval only gates REPEATS, so `stall_sample(3600)` cannot
    // hold that first capture back. Under suite load the watch's first
    // tick can land this late; wait it out so the window below counts
    // only the status probes.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while tmux_call_count(&mock, &d.state, "capture-pane") < 2 {
        assert!(
            deadline.elapsed() < Duration::from_secs(20),
            "first stall samples for dv1/dv2 never landed"
        );
        thread::sleep(Duration::from_millis(100));
    }
    let captures_before = tmux_call_count(&mock, &d.state, "capture-pane");
    let view = status_json(&d.state, &[], &[("CADENCE_PM_DIR", &pm_dir)]);
    let agents = view["agents"].as_array().unwrap();
    let row = |alias: &str| {
        agents
            .iter()
            .find(|a| a["alias"].as_str() == Some(alias))
            .unwrap_or_else(|| panic!("no row for {alias}: {view}"))
            .clone()
    };
    // w1: running message with age + head, owned doing issue.
    let w1 = row("w1");
    assert_eq!(w1["running"]["text"], "draft the migration plan", "{w1}");
    assert!(w1["running"]["age_secs"].as_u64().is_some(), "{w1}");
    assert_eq!(w1["issues"], json!(["CAD-1"]), "{w1}");
    // dv1 idle pane verdict + review issue; dv2 busy verdict.
    let dv1 = row("dv1");
    assert_eq!(dv1["pane"]["verdict"], "idle", "{dv1}");
    assert_eq!(dv1["issues"], json!(["CAD-2"]), "{dv1}");
    let dv2 = row("dv2");
    assert!(
        dv2["pane"]["verdict"]
            .as_str()
            .unwrap_or("")
            .starts_with("busy"),
        "{dv2}"
    );
    // fx fenced: attention state, one unknown.
    let fx = row("fx");
    assert_eq!(fx["state"], "attention", "{fx}");
    assert_eq!(fx["unknown"], 1, "{fx}");
    // pm inbox: no probe, unread lands in the footer.
    let pm = row("pm");
    assert!(pm["pane"].is_null(), "{pm}");
    assert!(view["footer"]["unread_inboxes"]
        .as_array()
        .unwrap()
        .contains(&json!("pm")));
    assert_eq!(view["footer"]["states"]["idle"], 3, "{view}");
    assert_eq!(view["footer"]["states"]["busy"], 1, "{view}");
    assert_eq!(view["footer"]["states"]["attention"], 1, "{view}");
    // Exactly one capture-pane per pty agent with a pane — no probe
    // for the managed, fake or inbox rows.
    let captures_after = tmux_call_count(&mock, &d.state, "capture-pane");
    assert_eq!(
        captures_after - captures_before,
        2,
        "status must probe each pty pane exactly once"
    );
    // Table form renders the same rows.
    let table = status_table(&d.state, &[("CADENCE_PM_DIR", &pm_dir)]);
    assert!(table.contains("w1"), "{table}");
    assert!(table.contains("draft the migration plan"), "{table}");
    assert!(table.contains("busy:"), "{table}");
    assert!(table.contains("unread: pm"), "{table}");
    // --group scopes to the root + its upstream members.
    d.operator_rpc("agent_set", json!({"alias": "w1", "patch": {}}))
        .ok();
}

/// --group filters to the named root plus agents naming it upstream.
#[test]
fn status_group_scopes_rows() {
    let d = TestDaemon::start();
    d.register_inbox("pm");
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.operator_rpc(
        "agent_register",
        json!({"alias": "w1", "provider": "fake", "endpoint_kind": "fake",
               "cwd": cwd, "params": "{\"upstream\":\"pm\"}"}),
    )
    .unwrap();
    d.register("other");
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    d.wait_agent("other", "idle", 10);
    let view = status_json(&d.state, &["--group", "pm"], &[]);
    let aliases: Vec<&str> = view["agents"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["alias"].as_str())
        .collect();
    assert_eq!(aliases, vec!["pm", "w1"], "{view}");
}

#[test]
fn overview_daemon_info_fenced_and_inbox_rows() {
    let d = TestDaemon::start();
    let home = TempDir::new().unwrap();

    // daemon_info carries the compiled-in build identity + start time.
    let info = d.rpc("daemon_info", json!({})).unwrap();
    assert_eq!(
        info["build_commit"].as_str().unwrap(),
        env!("CADENCE_BUILD_COMMIT")
    );
    assert_eq!(
        info["build_time"].as_str().unwrap(),
        env!("CADENCE_BUILD_TIME")
    );
    assert!(info["started_at"].as_f64().unwrap_or(0.0) > 0.0);

    // A fenced agent and unread inbox surface as needs-me rows with
    // their exact operator commands.
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    fence_agent(&d, "w1", "x1");
    d.register_inbox("pm");
    d.send("pm", json!({"text": "ping", "message": "n1"}))
        .unwrap();

    let view = overview_at(home.path(), &d.state, None, &[]);
    let needs = view["needs_me"].as_array().unwrap();
    let fenced = needs
        .iter()
        .find(|n| n["kind"] == "fenced")
        .expect("fenced row");
    assert_eq!(fenced["command"], "cadence agent unfence w1");
    let inbox = needs
        .iter()
        .find(|n| n["kind"] == "inbox_unread")
        .expect("inbox row");
    assert_eq!(inbox["command"], "cadence inbox pm");
    assert!(inbox["title"].as_str().unwrap().contains("pm"));
    // The daemon block carries identity through to the board payload.
    assert_eq!(view["daemon"]["reachable"], true);
    assert_eq!(
        view["daemon"]["build_commit"].as_str().unwrap(),
        env!("CADENCE_BUILD_COMMIT")
    );
    assert!(view["daemon"]["started_at"].as_f64().unwrap_or(0.0) > 0.0);
    // No tracker under the fake home → no repo can match the build.
    assert_eq!(view["drift"]["matched"], false);
}

#[test]
fn overview_approval_row_for_brokered_request() {
    let d = TestDaemon::start();
    let home = TempDir::new().unwrap();
    let _mock = d.mock_claude("permit", None);
    broker_command();
    d.register_inbox("pm");
    d.register_claude("w1", json!({"upstream": "pm", "broker_approvals": true}));
    d.wait_agent("w1", "idle", 15);
    d.send("w1", json!({"text": "run ls", "message": "m1"}))
        .unwrap();
    d.wait_agent("w1", "waiting_input", 15);
    let req = d.wait_request("w1", 15);
    let handle = req["request"].as_str().unwrap().to_string();

    let view = overview_at(home.path(), &d.state, None, &[]);
    let needs = view["needs_me"].as_array().unwrap();
    let approval = needs
        .iter()
        .find(|n| n["kind"] == "approval")
        .unwrap_or_else(|| panic!("approval row missing: {view}"));
    assert_eq!(
        approval["command"],
        format!("cadence agent respond w1 --request {handle} --decision accept")
    );
    assert!(approval["title"].as_str().unwrap().contains("w1"));
}

#[test]
fn overview_drift_reports_commits_after_build() {
    // Needs the compiled-in repo identity — absent only when the crate
    // was built outside a git checkout.
    if env!("CADENCE_BUILD_REMOTE") == "unknown" || env!("CADENCE_BUILD_COMMIT") == "unknown" {
        return;
    }
    // The build root can vanish between build and test (a removed
    // worktree) — skip rather than fail on the missing clone source.
    if !Path::new(env!("CADENCE_BUILD_ROOT")).join(".git").exists() {
        return;
    }
    let d = TestDaemon::start();
    let home = TempDir::new().unwrap();
    let pm = TempDir::new().unwrap();

    // A clone of the build repo carrying the same remote string (the
    // remote match) but refs we control, so the drift count is exact.
    let clone = TempDir::new().unwrap();
    git_at(
        clone.path(),
        &["clone", "-q", env!("CADENCE_BUILD_ROOT"), "."],
    );
    git_at(
        clone.path(),
        &["remote", "set-url", "origin", env!("CADENCE_BUILD_REMOTE")],
    );
    // Drop every remote-tracking ref so local `main` is the default.
    let refs = git_at(
        clone.path(),
        &["for-each-ref", "--format=%(refname)", "refs/remotes"],
    );
    for r in refs.lines() {
        git_at(clone.path(), &["update-ref", "-d", r]);
    }
    git_at(
        clone.path(),
        &["checkout", "-qB", "main", env!("CADENCE_BUILD_COMMIT")],
    );
    git_at(clone.path(), &["config", "user.email", "t@t"]);
    git_at(clone.path(), &["config", "user.name", "t"]);
    for (i, msg) in ["drift one (#11)", "drift two", "drift three (#13)"]
        .iter()
        .enumerate()
    {
        std::fs::write(clone.path().join("f"), format!("{i}")).unwrap();
        git_at(clone.path(), &["add", "f"]);
        git_at(clone.path(), &["commit", "-qm", msg]);
    }

    // The tracker declares that clone as cadence's repo.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["issue", "init"])
        .env("HOME", home.path())
        .env("CADENCE_PM_DIR", pm.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let repo = clone.path().to_str().unwrap().to_string();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "issue", "project", "add", "cadence", "--prefix", "CAD", "--repo", &repo,
        ])
        .env("HOME", home.path())
        .env("CADENCE_PM_DIR", pm.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A stub `gh` keeps the GitHub section off the network entirely.
    let ghbin = TempDir::new().unwrap();
    let gh = ghbin.path().join("gh");
    std::fs::write(
        &gh,
        "#!/bin/sh\nif [ \"$1\" = pr ]; then echo '[]'; else echo '{\"state\":\"success\"}'; fi\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!(
        "{}:{}",
        ghbin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let view = overview_at(home.path(), &d.state, Some(pm.path()), &[("PATH", path)]);
    let drift = &view["drift"];
    assert_eq!(drift["matched"], true, "{view}");
    assert_eq!(drift["project"], "cadence", "{view}");
    assert_eq!(drift["known"], true, "{view}");
    assert_eq!(drift["count"], 3, "{view}");
    let prs: Vec<Option<u64>> = drift["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["pr"].as_u64())
        .collect();
    assert_eq!(prs, vec![Some(13), None, Some(11)], "{view}");
    // All panes idle (no agents) → the drift row offers the verified
    // upgrade (CAD-334), which prints the lease-gated restart next.
    let needs = view["needs_me"].as_array().unwrap();
    let row = needs
        .iter()
        .find(|n| n["kind"] == "drift")
        .expect("drift row");
    assert_eq!(row["command"], "cadence upgrade --latest-main");
}

/// CAD-267: `cadence overview --json` reads the default branch's
/// `ci.yml` push runs through `gh` — never the legacy commit-status
/// API. Newest run failed → `ci_red`; the cancelled middle SHA has no
/// later pass → `ci_unverified` on the same subject, and it stays
/// labelled cancelled.
#[test]
fn overview_main_ci_red_and_unverified_from_actions_runs() {
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let pm = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    git_at(repo.path(), &["init", "-q", "-b", "main"]);
    git_at(repo.path(), &["config", "user.email", "t@t"]);
    git_at(repo.path(), &["config", "user.name", "t"]);
    git_at(
        repo.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widgets.git",
        ],
    );
    let mut shas = Vec::new();
    for i in 0..3 {
        std::fs::write(repo.path().join("f"), format!("{i}")).unwrap();
        git_at(repo.path(), &["add", "f"]);
        git_at(repo.path(), &["commit", "-qm", &format!("push {i}")]);
        shas.push(git_at(repo.path(), &["rev-parse", "HEAD"]));
    }
    let repo_s = repo.path().to_str().unwrap().to_string();
    issue_cli(home.path(), state.path(), pm.path(), &["issue", "init"]);
    issue_cli(
        home.path(),
        state.path(),
        pm.path(),
        &[
            "issue", "project", "add", "cadence", "--prefix", "CAD", "--repo", &repo_s,
        ],
    );
    let run = |id: u64, sha: &str, workflow: &str, conclusion: &str| {
        json!({
            "id": id, "name": workflow, "head_sha": sha, "head_branch": "main",
            "event": "push", "status": "completed", "conclusion": conclusion,
            "path": format!(".github/workflows/{workflow}.yml"),
            "html_url": format!("https://github.com/acme/widgets/actions/runs/{id}"),
            "created_at": format!("2026-09-23T01:{:02}:00Z", id),
        })
    };
    let runs = json!({"total_count": 4, "workflow_runs": [
        run(30, &shas[2], "ci", "failure"),
        // Handover passing on the cancelled SHA never counts.
        run(21, &shas[1], "handover", "success"),
        run(20, &shas[1], "ci", "cancelled"),
        run(10, &shas[0], "ci", "success"),
    ]});
    let gh = fake_gh();
    let envs = gh.envs(&[("FAKE_GH_RUNS".to_string(), runs.to_string())]);
    let envs: Vec<(&str, String)> = envs.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    let view = overview_at(home.path(), state.path(), Some(pm.path()), &envs);

    let block = &view["main_ci"][0];
    assert_eq!(block["slug"], "acme/widgets", "{view}");
    assert_eq!(block["branch"], "main", "{view}");
    assert_eq!(block["order"], "first_parent", "{view}");
    let got: Vec<(String, String)> = block["shas"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["sha"].as_str().unwrap().to_string(),
                s["state"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let want: Vec<(String, String)> = [(2, "failed"), (1, "cancelled"), (0, "passed")]
        .iter()
        .map(|(i, st)| (shas[*i].clone(), st.to_string()))
        .collect();
    assert_eq!(got, want, "{view}");
    assert!(block["shas"][1]["covered_by"].is_null(), "{view}");

    let row = view["needs_me"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["kind"] == "ci_red")
        .unwrap_or_else(|| panic!("no ci_red row: {view}"));
    assert_eq!(
        row["subject"],
        json!({"kind": "ci", "id": "acme/widgets@main"})
    );
    assert_eq!(row["project"], "cadence");
    assert_eq!(row["command"], "gh run view 30 --repo acme/widgets");
    assert!(
        row["title"].as_str().unwrap().contains(&shas[2][..7]),
        "{row}"
    );
    let causes: Vec<&str> = row["causes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["cause"].as_str().unwrap())
        .collect();
    assert_eq!(causes, ["ci_red", "ci_unverified"], "{row}");

    let calls = gh.calls();
    assert!(
        calls.iter().any(|c| c
            == "api\trepos/acme/widgets/actions/workflows/ci.yml/runs?branch=main&event=push&per_page=30"),
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.contains("/status")),
        "legacy status API read: {calls:?}"
    );
}

/// `cadence overview` with extra args — the raw output, success or not.
fn overview_cmd(home: &Path, state: &Path, pm: &Path, args: &[&str]) -> std::process::Output {
    // Operator-proven like `overview_at` — the probes it runs gate on
    // the caller (CAD-506).
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.arg("--state-dir")
        .arg(state)
        .args(["overview", "--json"])
        .args(args)
        .env("HOME", home)
        .env("CADENCE_PM_DIR", pm)
        .env_remove("CADENCE_ALIAS");
    cmd.operator_output().unwrap()
}

/// CAD-252: `--project` and `--group` scope the merged rows; a key the
/// tracker or the fleet does not know is an error naming the key.
#[test]
fn overview_scope_flags_filter_rows_and_reject_unknown_keys() {
    let d = TestDaemon::start();
    let home = TempDir::new().unwrap();
    let pm = TempDir::new().unwrap();
    // A git checkout the tracker declares as project `cadence`'s repo;
    // agents working inside it attribute to that project.
    let repo = TempDir::new().unwrap();
    git_at(repo.path(), &["init", "-q"]);
    let repo_s = repo.path().to_str().unwrap().to_string();
    issue_cli(home.path(), &d.state, pm.path(), &["issue", "init"]);
    issue_cli(
        home.path(),
        &d.state,
        pm.path(),
        &[
            "issue", "project", "add", "cadence", "--prefix", "CAD", "--repo", &repo_s,
        ],
    );
    // Group `pm`: an inbox root with worker w1 in the project repo.
    // w2 is its own root, outside every project.
    d.register_inbox("pm");
    d.operator_rpc(
        "agent_register",
        json!({"alias": "w1", "provider": "fake", "endpoint_kind": "fake",
               "cwd": repo_s, "params": json!({"upstream": "pm"}).to_string()}),
    )
    .unwrap();
    d.register("w2");
    for w in ["w1", "w2"] {
        d.wait_agent(w, "idle", 10);
        fence_agent(&d, w, &format!("x-{w}"));
    }
    let fenced = |v: &Value| -> Vec<String> {
        let mut out: Vec<String> = v["needs_me"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|n| n["kind"] == "fenced")
            .map(|n| n["subject"]["id"].as_str().unwrap().to_string())
            .collect();
        out.sort();
        out
    };
    let run = |args: &[&str]| -> Value {
        let out = overview_cmd(home.path(), &d.state, pm.path(), args);
        assert!(
            out.status.success(),
            "overview {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    };

    let all = run(&[]);
    assert_eq!(fenced(&all), ["w1", "w2"], "{all}");
    let w1 = all["needs_me"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["subject"]["id"] == "w1")
        .unwrap();
    assert_eq!(w1["subject"]["kind"], "agent", "{w1}");
    assert_eq!(w1["cause"], "fenced", "{w1}");
    assert_eq!(w1["project"], "cadence", "attributed by cwd: {w1}");

    let group = run(&["--group", "pm"]);
    assert_eq!(fenced(&group), ["w1"], "{group}");
    assert_eq!(group["scope"]["group"], "pm");
    let project = run(&["--project", "cadence"]);
    assert_eq!(fenced(&project), ["w1"], "{project}");
    assert_eq!(project["projects"].as_array().unwrap().len(), 1);

    for (flag, key) in [("--project", "nope-project"), ("--group", "nope-group")] {
        let out = overview_cmd(home.path(), &d.state, pm.path(), &[flag, key]);
        assert!(!out.status.success(), "{flag} {key} must fail");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(key), "{flag}: {err}");
    }
}

/// CAD-253: `cadence overview --json` carries each needs-me row's
/// server-resolved audience. A stalled worker whose PM is live stays
/// team work; one whose PM is fenced, and a stalled root agent with no
/// PM at all, are the operator's — and the plain render groups them the
/// same way. A fenced agent is the operator's whatever its PM (CAD-374:
/// only the operator may unfence or reconcile).
#[test]
fn overview_needs_me_audience_follows_owner_liveness() {
    let d = TestDaemon::start();
    let home = TempDir::new().unwrap();
    let pm = TempDir::new().unwrap();
    d.register_inbox("pm");
    d.register("lead");
    for (alias, upstream) in [("w1", "pm"), ("w2", "lead"), ("w4", "pm")] {
        register_fake_opts(&d, alias, json!({"upstream": upstream, "stall_secs": 2}));
    }
    register_fake_opts(&d, "w3", json!({"stall_secs": 2}));
    for w in ["lead", "w1", "w2", "w3", "w4"] {
        d.wait_agent(w, "idle", 10);
    }
    for w in ["lead", "w4"] {
        fence_agent(&d, w, &format!("x-{w}"));
    }
    // A silent turn stalls the worker: a row its PM owns.
    for w in ["w1", "w2", "w3"] {
        d.send(w, json!({"text": "SLEEP:20", "message": format!("s-{w}")}))
            .unwrap();
    }
    for w in ["w1", "w2", "w3"] {
        let id = format!("s-{w}");
        d.wait_event_where(
            w,
            "turn_stalled",
            |e| e["payload"]["message"].as_str() == Some(id.as_str()),
            20,
        );
    }
    let out = overview_cmd(home.path(), &d.state, pm.path(), &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let view: Value = serde_json::from_slice(&out.stdout).unwrap();
    let row = |alias: &str, kind: &str| -> (String, String) {
        let r = view["needs_me"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["subject"]["id"] == alias && n["kind"] == kind)
            .unwrap_or_else(|| panic!("no {kind} row for {alias}: {view}"));
        (
            r["audience"].as_str().unwrap().to_string(),
            r["audience_reason"].as_str().unwrap().to_string(),
        )
    };
    assert_eq!(
        row("w1", "stalled"),
        ("team".into(), "owner pm can act".into())
    );
    assert_eq!(
        row("w2", "stalled"),
        ("operator".into(), "owner lead is fenced".into())
    );
    assert_eq!(row("w3", "stalled"), ("operator".into(), "no owner".into()));
    // Fenced: the operator's, even with a live PM.
    assert_eq!(
        row("w4", "fenced"),
        ("operator".into(), "operator decision".into())
    );
    assert_eq!(
        row("lead", "fenced"),
        ("operator".into(), "operator decision".into())
    );

    // The plain render reads the same field: w2, w3, w4 and lead under
    // the decision, w1 under team handling.
    let plain = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .arg("overview")
        .env("HOME", home.path())
        .env("CADENCE_PM_DIR", pm.path())
        .env_remove("CADENCE_ALIAS")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&plain.stdout);
    let decision = text.find("needs your decision").expect("decision section");
    let team = text.find("team handling").expect("team section");
    assert!(decision < team, "{text}");
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle}: {text}"))
    };
    for op in [
        "agent lead fenced",
        "agent w4 fenced",
        "agent w2 turn silent",
        "agent w3 turn silent",
    ] {
        assert!(
            (decision..team).contains(&at(op)),
            "{op} not a decision: {text}"
        );
    }
    assert!(at("agent w1 turn silent") > team, "w1 is team work: {text}");
    assert!(!text.contains("nothing needs your decision"), "{text}");
}

/// CAD-253: the unhandled clock is when the issue entered its status —
/// the tracker's last `status:` change — never the issue's age. Two
/// issues created 30 days ago, owned by a live PM mailbox: one went to
/// review 10 minutes ago (team), the other 74 minutes ago (operator,
/// `unhandled 74m`).
#[test]
fn overview_tracker_row_clock_is_the_status_change() {
    let d = TestDaemon::start();
    let home = TempDir::new().unwrap();
    let pm = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    git_at(repo.path(), &["init", "-q"]);
    let repo_s = repo.path().to_str().unwrap().to_string();
    d.register_inbox("pm");
    issue_cli(home.path(), &d.state, pm.path(), &["issue", "init"]);
    issue_cli(
        home.path(),
        &d.state,
        pm.path(),
        &[
            "issue", "project", "add", "cadence", "--prefix", "CAD", "--repo", &repo_s,
        ],
    );
    for title in ["fresh review", "stale review"] {
        issue_cli(
            home.path(),
            &d.state,
            pm.path(),
            &["issue", "new", title, "--project", "cadence"],
        );
    }
    // Back-date both issues 30 days — a commit that never touches the
    // `status:` line.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let old = cadence_agent::issue::time::iso(now - 30 * 86_400);
    for id in ["CAD-1", "CAD-2"] {
        let file = pm.path().join("cadence").join(id).join("issue.md");
        let text = std::fs::read_to_string(&file).unwrap();
        let text: String = text
            .lines()
            .map(|l| {
                if l.starts_with("created:") {
                    format!("created: {old}\n")
                } else {
                    format!("{l}\n")
                }
            })
            .collect();
        std::fs::write(&file, text).unwrap();
    }
    git_at(
        pm.path(),
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "--no-verify",
            "-am",
            "backdate",
        ],
    );
    // Each status change is committed `ago` seconds in the past.
    let bin = env!("CARGO_BIN_EXE_cadence");
    for (id, ago) in [("CAD-1", 10 * 60), ("CAD-2", 74 * 60 + 30)] {
        let out = std::process::Command::new(bin)
            .arg("--state-dir")
            .arg(&d.state)
            .args(["issue", "set", id, "owner=pm", "status=review"])
            .env("HOME", home.path())
            .env("CADENCE_PM_DIR", pm.path())
            .env("GIT_AUTHOR_DATE", format!("@{} +0000", now - ago))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    Path::new(bin).parent().unwrap().display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "issue set {id}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let out = overview_cmd(home.path(), &d.state, pm.path(), &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let view: Value = serde_json::from_slice(&out.stdout).unwrap();
    let row = |id: &str| -> Value {
        view["needs_me"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["subject"]["id"] == id && n["kind"] == "review_no_pr")
            .unwrap_or_else(|| panic!("no review row for {id}: {view}"))
            .clone()
    };
    let (fresh, stale) = (row("CAD-1"), row("CAD-2"));
    for r in [&fresh, &stale] {
        assert!(r["age"].as_i64().unwrap() >= 29 * 86_400, "issue age: {r}");
    }
    assert_eq!(fresh["audience"], "team", "{fresh}");
    assert_eq!(fresh["audience_reason"], "owner pm can act", "{fresh}");
    let since = fresh["since"].as_i64().unwrap();
    assert!((now - 10 * 60 - since).abs() <= 5, "{fresh}");
    assert_eq!(stale["audience"], "operator", "{stale}");
    assert_eq!(stale["audience_reason"], "unhandled 74m", "{stale}");
}

/// `cadence status` carries the slot line — table and --json agree.
#[test]
fn status_footer_shows_slots() {
    let d = TestDaemon::start_opts(slot_opts(2, 1, 900, &[]));
    plant_self(&d);
    slot_acquire(&d, "build", SELF_LANE, "r1");
    slot_acquire(&d, "build", SELF_LANE, "r2");
    slot_acquire(&d, "build", SELF_LANE, "r3"); // the waiter
    let home = TempDir::new().unwrap();
    let out = cadence_at(home.path(), &d.state, &["status"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("slots: 2/2 build, 0/1 suite; waiting: 1"),
        "{text}"
    );
    let out = cadence_at(home.path(), &d.state, &["status", "--json"]);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["footer"]["slots"]["pools"]["build"]["held"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(v["footer"]["slots"]["waiting"].as_array().unwrap().len(), 1);
}

/// A resume that fails at open (a provider/session error) does not
/// fail silently: the agent is named with its waiting message on an
/// Overview needs-me row, it is not retried, and an operator resume
/// clears the row and delivers. A healthy auto-stopped peer resumes
/// and delivers in the same daemon.
#[test]
fn auto_resume_failure_raises_needs_me_row_naming_the_message() {
    let (d, offset) = auto_stop_daemon(daemon::AutoStopSetting::idle_after(3600));
    d.register_inbox("pm");
    let flag = d.dir.path().join("open-fails");
    register_fake_opts(
        &d,
        "w-fail",
        json!({"upstream": "pm", "fake_open_fail_if": flag.to_str().unwrap()}),
    );
    register_fake_opts(&d, "w-ok", json!({"upstream": "pm"}));
    for alias in ["w-fail", "w-ok"] {
        d.wait_agent(alias, "idle", 20);
    }
    offset.store(7200, std::sync::atomic::Ordering::SeqCst);
    wait_auto_stopped(&d, "w-fail");
    wait_auto_stopped(&d, "w-ok");
    offset.store(0, std::sync::atomic::Ordering::SeqCst);

    std::fs::write(&flag, "").unwrap();
    for (alias, id) in [("w-fail", "m-fail"), ("w-ok", "m-ok")] {
        d.send(alias, json!({"text": "work", "message": id}))
            .unwrap();
    }
    d.wait_message("w-ok", "m-ok", &["completed"], 20);
    let failed = d.wait_event("w-fail", "agent_auto_resume_failed", 20);
    assert_eq!(failed["payload"]["message"], "m-fail", "{failed}");
    assert!(
        failed["payload"]["reason"]
            .as_str()
            .unwrap()
            .contains("fake open refused"),
        "{failed}"
    );
    let agent = d.wait_agent("w-fail", "attention", 10);
    assert_eq!(agent["auto_resume_failed"]["message"], "m-fail", "{agent}");
    assert_eq!(d.message_state("w-fail", "m-fail"), "queued");

    let home = TempDir::new().unwrap();
    let view = overview_at(home.path(), &d.state, None, &[]);
    let needs = view["needs_me"].as_array().unwrap();
    let row = needs
        .iter()
        .find(|n| n["kind"] == "auto_resume_failed")
        .unwrap_or_else(|| panic!("no auto_resume_failed row: {needs:?}"));
    let title = row["title"].as_str().unwrap();
    assert!(
        title.contains("agent w-fail") && title.contains("message m-fail waiting"),
        "{row}"
    );
    assert_eq!(row["command"], "cadence agent resume w-fail", "{row}");
    assert!(
        !needs
            .iter()
            .any(|n| n["kind"] == "fenced" && n["title"].as_str().unwrap().contains("w-fail")),
        "{needs:?}"
    );
    // Not retried: one resume attempt, however many ticks passed.
    let attempts = event_kinds(&d, "w-fail")
        .iter()
        .filter(|k| *k == "agent_auto_resumed")
        .count();
    assert_eq!(attempts, 1);

    // The operator fixes the cause and resumes: the row clears and the
    // waiting message is delivered.
    std::fs::remove_file(&flag).unwrap();
    d.operator_rpc("agent_resume", json!({"alias": "w-fail"}))
        .unwrap();
    d.wait_message("w-fail", "m-fail", &["completed"], 20);
    let agent = d.wait_agent("w-fail", "idle", 10);
    assert!(agent["auto_resume_failed"].is_null(), "{agent}");
    let view = overview_at(home.path(), &d.state, None, &[]);
    assert!(
        !view["needs_me"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["kind"] == "auto_resume_failed"),
        "{view}"
    );
}

// ---- CAD-335: board writes from a managed endpoint's processes ----

/// Seed a tracker in `pm` for the board tests below: one project and
/// one issue, `CAD-1`. Runs the real CLI against the daemon's state.
fn seed_board(pm: &Path, state: &Path) {
    for args in [
        &["issue", "init"][..],
        &["issue", "project", "add", "cadence", "--prefix", "CAD"],
        &["issue", "new", "root task", "--project", "cadence"],
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(state)
            .args(args)
            .env("CADENCE_PM_DIR", pm)
            .env_remove("CADENCE_ALIAS")
            .operator_output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A raw board write — a comment `body` on CAD-1 — addressed to the
/// board on `port`, with every cross-site guard satisfied.
fn board_comment_request(port: u16, body: &str) -> String {
    let body = format!(r#"{{"body":"{body}"}}"#);
    format!(
        "POST /api/issues/CAD-1/comments HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\
         Content-Type: application/json\r\nX-Cadence-Board: 1\r\n\
         Origin: http://127.0.0.1:{port}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// The comment `body` in a board write's raw HTTP reply (asserting 200).
/// A reply relayed through the mock provider's text-mode capture has
/// its `\r\n` folded to `\n`, so either blank line ends the head.
fn board_replied_comment(response: &str, body: &str) -> Value {
    assert!(
        response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200"),
        "{response}"
    );
    let (_, json_body) = response
        .split_once("\r\n\r\n")
        .or_else(|| response.split_once("\n\n"))
        .unwrap_or_else(|| panic!("no header end in {response:?}"));
    let v: Value = serde_json::from_str(json_body).unwrap();
    v["issue"]["comments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["body"] == body)
        .cloned()
        .unwrap_or_else(|| panic!("no comment {body:?} in {v}"))
}

/// The tracker's newest commit message.
fn board_last_commit(pm: &Path) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(pm)
        .args(["log", "-1", "--format=%B"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// CAD-335 phase 1 ACCEPTANCE (item 2): a managed endpoint has no pane,
/// so before this fix its tool subprocess — a headless `claude -p`
/// running its Bash tool and curling the loopback board — was "tied to
/// no pane" and wrote as `operator (ui)`, author `operator`. The
/// daemon records the provider process it launched; a board write from
/// a process that descends from it is that agent's, never the
/// operator's.
#[test]
fn ui_write_caller_attributes_a_managed_endpoint_tool_process() {
    let d = TestDaemon::start();
    let pm = TempDir::new().unwrap();
    seed_board(pm.path(), &d.state);
    let port = start_board(pm.path(), &d.state);
    let mut wk = ManagedWorker::start(&d, "wk");
    let request = board_comment_request(port, "from a managed tool");
    let r = wk.exec(&[
        "bash",
        "-c",
        DEV_TCP_CLIENT,
        "_",
        &port.to_string(),
        &request,
    ]);
    assert_eq!(r["rc"], 0, "{r}");
    let comment = board_replied_comment(r["out"].as_str().unwrap(), "from a managed tool");
    assert_eq!(comment["author"], "wk", "{comment}");
    let last = board_last_commit(pm.path());
    assert!(last.contains("Actor: wk"), "{last}");
    assert!(!last.contains("operator"), "{last}");
}

/// CAD-390: the board ties a write to a managed endpoint only while the
/// provider pid still names the process the daemon recorded — pid AND
/// start time (`pid_start`, CAD-385). The daemon records the real
/// provider's start, so its tool process writes as the agent. With the
/// same pid but a different recorded start — what the row says once the
/// provider died and another process took its pid — the row ties
/// nothing, and the caller is placed exactly as a process tied to no
/// agent (CAD-313 caller rule): with no session it is refused
/// `operator_session_required`; with the operator's session it writes
/// as `operator (ui)`. Either way never as `wk` — a caller still tied
/// to `wk` would have written as `wk` without a session, and been
/// refused `session_from_agent` with one. A row with no recorded start
/// is unprovable: refused `caller_identity`, session or not.
#[test]
fn cad390_a_reused_managed_provider_pid_never_attributes_a_board_write() {
    let d = TestDaemon::start();
    let pm = TempDir::new().unwrap();
    seed_board(pm.path(), &d.state);
    let port = start_board(pm.path(), &d.state);
    let mut wk = ManagedWorker::start(&d, "wk");
    let provider = wk.pid;
    let op = sign_in(&d.state, port);
    let db = d.state.join("cadence.sqlite3");
    let set_start = |start: Option<i64>| {
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute(
                "UPDATE agents SET pid_start=?1 WHERE alias='wk'",
                rusqlite::params![start],
            )
            .unwrap();
    };
    // A comment write from wk's tool process, with the operator's
    // session (on the board's own Host) or with none.
    let mut write = |body: &str, session: bool| {
        let mut request = board_comment_request(port, body);
        if session {
            request = request
                .replace(
                    &format!("127.0.0.1:{port}"),
                    &format!("cadence-{port}.localhost:{port}"),
                )
                .replacen(
                    "\r\nContent-Type",
                    &format!(
                        "\r\nCookie: {}\r\n{}\r\nContent-Type",
                        op.cookie,
                        op.key_header()
                    ),
                    1,
                );
        }
        let r = wk.exec(&[
            "bash",
            "-c",
            DEV_TCP_CLIENT,
            "_",
            &port.to_string(),
            &request,
        ]);
        assert_eq!(r["rc"], 0, "{r}");
        r["out"].as_str().unwrap().to_string()
    };
    let refused = |response: &str, check: &str| {
        assert!(
            response.starts_with("HTTP/1.1 403") || response.starts_with("HTTP/1.0 403"),
            "{response}"
        );
        assert!(response.contains(&format!("\"{check}\"")), "{response}");
    };

    let agent = d.rpc("agent_show", json!({"alias": "wk"})).unwrap()["agent"].clone();
    assert_eq!(agent["pid"].as_u64(), Some(u64::from(provider)), "{agent}");
    let start = proc_start(provider).expect("the provider is alive");
    assert_eq!(agent["pid_start"].as_i64(), Some(start), "{agent}");

    // The real provider: its tool process is the agent.
    let body = "from the real provider";
    let comment = board_replied_comment(&write(body, false), body);
    assert_eq!(comment["author"], "wk", "{comment}");
    assert!(board_last_commit(pm.path()).contains("Actor: wk"));

    // The pid "reused": same number, a different recorded start. Tied
    // to no agent, so no session is no write at all ...
    set_start(Some(start - 1));
    let before = board_last_commit(pm.path());
    refused(
        &write("reused, no session", false),
        "operator_session_required",
    );
    assert_eq!(board_last_commit(pm.path()), before, "nothing is written");
    // ... and the operator's session writes as the operator.
    let body = "reused, with the session";
    let comment = board_replied_comment(&write(body, true), body);
    assert_eq!(comment["author"], "operator", "{comment}");
    let last = board_last_commit(pm.path());
    assert!(last.contains("(operator (ui))"), "{last}");
    assert!(!last.contains("wk"), "{last}");

    // No recorded start: unprovable, so the write is refused, session
    // or not, naming the row.
    set_start(None);
    let before = board_last_commit(pm.path());
    for session in [false, true] {
        let response = write("with no recorded start", session);
        refused(&response, "caller_identity");
        assert!(response.contains("'wk'"), "{response}");
    }
    assert_eq!(board_last_commit(pm.path()), before, "nothing is written");

    // The recorded start restored: the provider is the agent again.
    set_start(Some(start));
    let comment = board_replied_comment(&write("restored", false), "restored");
    assert_eq!(comment["author"], "wk", "{comment}");
}

/// CAD-335 phase 1 (item 4): attributing managed endpoints must not
/// cost the operator the board. With a managed agent live, a write
/// relayed by a process that is neither a pane nor a managed
/// provider's descendant — the operator's `socat` relay shape — writes
/// as `operator (ui)` when it carries the operator's session. CAD-428:
/// the same relay carrying no session is not the operator — the board
/// sees only the relay — and writes nothing.
#[test]
fn ui_write_caller_keeps_the_operator_relay_with_a_managed_agent_live() {
    use std::io::Read;
    let d = TestDaemon::start();
    let pm = TempDir::new().unwrap();
    seed_board(pm.path(), &d.state);
    let port = start_board(pm.path(), &d.state);
    let _wk = ManagedWorker::start(&d, "wk");
    let via_relay = |request: &str| -> String {
        let mut relay = std::process::Command::new("python3")
            .args(["-c", RELAY_PY, &port.to_string()])
            .env_remove("CADENCE_ALIAS")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut first = String::new();
        BufReader::new(relay.stdout.take().unwrap())
            .read_line(&mut first)
            .unwrap();
        let relay_port: u16 = first.trim().parse().unwrap();
        let mut s = std::net::TcpStream::connect(("127.0.0.1", relay_port)).unwrap();
        s.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        s.read_to_string(&mut response).unwrap();
        // Closing our end lets the relay's client-side pump finish.
        drop(s);
        assert!(relay.wait().unwrap().success());
        response
    };
    let before = board_last_commit(pm.path());
    let response = via_relay(&board_comment_request(port, "no session"));
    assert!(response.contains(" 403 "), "{response}");
    assert!(response.contains("operator_session_required"), "{response}");
    assert_eq!(board_last_commit(pm.path()), before, "nothing is written");

    let op = sign_in(&d.state, port);
    let request = board_comment_request(port, "via the relay")
        .replace(
            &format!("127.0.0.1:{port}"),
            &format!("cadence-{port}.localhost:{port}"),
        )
        .replacen(
            "\r\nContent-Type",
            &format!(
                "\r\nCookie: {}\r\n{}\r\nContent-Type",
                op.cookie,
                op.key_header()
            ),
            1,
        );
    let response = via_relay(&request);
    let comment = board_replied_comment(&response, "via the relay");
    assert_eq!(comment["author"], "operator", "{comment}");
    let last = board_last_commit(pm.path());
    assert!(last.contains("(operator (ui))"), "{last}");
    assert!(last.contains("Actor: operator"), "{last}");
    assert!(!last.contains("wk"), "{last}");
}

/// Percent-encode a query value (`?inputs=<json>`) — the board decodes
/// `%XX` itself, so the wire form must not rely on form semantics.
fn pct_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// CAD-496: the board's workflow endpoints serve Projects → Workflows.
/// `GET /api/projects/<key>/workflows` lists the stored templates with
/// the inputs their frontmatter declares (`ask`, `optional`) and the
/// gate approval; `…/<name>/preview?inputs=<json>` is the rendered plan
/// file `plan_propose` would get — a render refusal is data, not a
/// failed request, and carries the daemon's refusal `code` beside its
/// reason. `POST …/propose` relays `plan_propose` verbatim:
/// operator-only, daemon-gated (`workflow_unapproved` crosses the wire
/// unchanged), its result the ordinary proposed plan Needs you shows.
#[test]
fn board_workflow_list_preview_and_propose() {
    let f = PlanFixture::start();
    f.d.register("dev-1");
    f.d.register("qa-1");
    wf_add(&f, "two-step", WF_TWO_STEP);
    let port = start_board(&f.pm_dir, &f.d.state);
    let op = sign_in(&f.d.state, port);
    let guards = op_guards(&op);
    let before = f.commits();

    // The list: one row per stored workflow — inputs as the run form
    // renders them, in the file's declared order (`title`, then `note`;
    // the map is sorted, a form reads what the author wrote).
    let (status, body) = board_get(port, "/api/projects/demo/workflows");
    assert_eq!(status, 200, "{body}");
    let list: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(list["workflows"].as_array().unwrap().len(), 1, "{list}");
    let row = &list["workflows"][0];
    assert_eq!(row["name"], "two-step", "{row}");
    assert_eq!(row["title"], "Change: title", "{row}");
    assert_eq!(row["tickets"], 2, "{row}");
    assert_eq!(row["approved"], false, "not yet approved: {row}");
    let inputs = row["inputs"].as_array().unwrap();
    assert_eq!(
        (
            inputs[0]["name"].as_str().unwrap(),
            inputs[0]["optional"].as_bool().unwrap(),
            inputs[0]["ask"].as_str().unwrap(),
        ),
        ("title", false, "What change?"),
        "{inputs:?}"
    );
    assert_eq!(
        (
            inputs[1]["name"].as_str().unwrap(),
            inputs[1]["optional"].as_bool().unwrap(),
        ),
        ("note", true),
        "{inputs:?}"
    );
    // Unknown project, bad key, and a lookalike path.
    let (status, _) = board_get(port, "/api/projects/nope/workflows");
    assert_eq!(status, 404);
    let (status, _) = board_get(port, "/api/projects/Bad%20Key/workflows");
    assert_eq!(status, 400);
    let (status, _) = board_get(port, "/api/projects/demo/workflowsx");
    assert_eq!(status, 404);

    // The preview: the rendered plan file for the current inputs — a
    // refusal (a missing required input) is `{"error": …}` data, not a
    // failed GET.
    let prev = "/api/projects/demo/workflows/two-step/preview";
    let (status, body) = board_get(
        port,
        &format!("{prev}?inputs={}", pct_encode(r#"{"title":"login fix"}"#)),
    );
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    let rendered = v["rendered"].as_str().unwrap_or_default();
    assert!(rendered.contains("Change: login fix"), "{rendered}");
    assert!(rendered.contains("## Do login fix"), "{rendered}");
    assert!(!rendered.contains("inputs:"), "{rendered}");
    let (status, body) = board_get(port, prev);
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert!(
        v["error"]
            .as_str()
            .unwrap_or_default()
            .contains("missing required input 'title'"),
        "{v}"
    );
    // Malformed inputs and unknown names are HTTP errors.
    for (path, want) in [
        (format!("{prev}?inputs=%5B%5D"), 400),
        (format!("{prev}?inputs=%7B%22t%22%3A1%7D"), 400),
        (format!("{prev}?inputs=%7B"), 400),
        ("/api/projects/demo/workflows/nope/preview".to_string(), 404),
        ("/api/projects/demo/workflows/../preview".to_string(), 400),
        (
            "/api/projects/demo/workflows/Bad%20Name/preview".to_string(),
            400,
        ),
    ] {
        let (status, body) = board_get(port, &path);
        assert_eq!(status, want, "{path}: {body}");
    }

    // Propose is the operator's — refused without a session, refused
    // for an agent-attributed caller, and a forged attribution field is
    // refused by the body schema before the daemon sees it.
    let propose = "/api/projects/demo/workflows/two-step/propose";
    let (status, reply) = board_http(port, &cad328_post(port, propose, THREAD_GUARDS, "{}"));
    assert_eq!(status, 403, "{reply}");
    assert!(reply.contains("operator_session_required"), "{reply}");
    let mut wk = ManagedWorker::start(&f.d, "wk");
    let request = cad328_post(port, propose, THREAD_GUARDS, r#"{"inputs":{"title":"x"}}"#);
    let r = wk.exec(&[
        "bash",
        "-c",
        DEV_TCP_CLIENT,
        "_",
        &port.to_string(),
        &request,
    ]);
    let out = r["out"].as_str().unwrap();
    assert!(
        out.contains(" 403 ") && out.contains("operator_only"),
        "{out}"
    );
    for body in [
        r#"{"inputs":{"title":"x"},"actor":"wk"}"#,
        r#"{"inputs":{"title":"x"},"by":"operator"}"#,
        r#"{"inputs":{"title":"x"},"proposed_by":"wk"}"#,
    ] {
        let (status, reply) = board_http(port, &cad328_post(port, propose, &guards, body));
        assert_eq!(status, 400, "{body}: {reply}");
    }
    assert_eq!(f.commits(), before, "refusals write nothing");

    // The daemon's gate crosses the board unchanged.
    let (status, reply) = board_http(
        port,
        &cad328_post(port, propose, &guards, r#"{"inputs":{"title":"x"}}"#),
    );
    assert_eq!(status, 400, "{reply}");
    assert!(reply.contains("workflow_unapproved"), "{reply}");
    let (ok, out) = f.cli(&["workflow", "approve", "two-step", "--project", "demo"]);
    assert!(ok, "{out}");
    // Once approved the list row says so — the form may run.
    let (_, body) = board_get(port, "/api/projects/demo/workflows");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["workflows"][0]["approved"],
        true,
        "{body}"
    );
    // Render refusals are the daemon's own messages.
    for (body, want) in [
        ("{}", "missing required input"),
        (
            r#"{"inputs":{"title":"x","bogus":"y"}}"#,
            "unknown input 'bogus'",
        ),
        (r#"{"inputs":{"title":{"n":1}}}"#, "expected a string"),
    ] {
        let (status, reply) = board_http(port, &cad328_post(port, propose, &guards, body));
        assert_eq!(status, 400, "{body}: {reply}");
        assert!(reply.contains(want), "{want}: {reply}");
    }

    // The operator proposes a run — the same epic and tickets a
    // `plan propose --workflow` lands, waiting as a proposed plan.
    let (status, reply) = board_http(
        port,
        &cad328_post(
            port,
            propose,
            &guards,
            r#"{"inputs":{"title":"login fix","note":"look twice"}}"#,
        ),
    );
    assert_eq!(status, 200, "{reply}");
    let out: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(out["epic"], "D-1", "{out}");
    assert_eq!(out["tickets"], json!(["D-2", "D-3"]), "{out}");
    assert_eq!(f.commits(), before + 1, "one commit per proposal");
    let plan = f.front("D-1").plan.unwrap();
    assert_eq!(plan.state, "proposed");
    assert!(issue_body(&f, "D-1").contains("Ship login fix"));
    let events = f.daemon_events("plan_proposed");
    assert_eq!(events[0]["workflow"], "two-step", "{events:?}");
    // It waits in Needs you like every proposal: the overview's plan
    // row, and the board's issue card carries the proposed plan.
    let (status, body) = board_get(port, "/api/overview");
    assert_eq!(status, 200, "{body}");
    let overview: Value = serde_json::from_str(&body).unwrap();
    let needs = overview["needs_me"].as_array().unwrap();
    let row = needs
        .iter()
        .find(|n| n["plan"]["epic"] == "D-1")
        .unwrap_or_else(|| panic!("no plan row for D-1: {needs:?}"));
    assert_eq!(row["kind"], "plan", "{row}");
    let (status, body) = board_get(port, "/api/issues/D-1");
    assert_eq!(status, 200, "{body}");
    let card: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(card["plan"]["state"], "proposed", "{card}");
    // The plan gate still applies to the proposed tickets.
    let (ok, err) = f.cli(&["issue", "start", "D-2"]);
    assert!(!ok && err.to_string().contains("proposed"), "{err}");

    // A refused propose carries the daemon's named code in the error
    // body — `coded_response` keeps the Structured code the wire sent.
    let (status, reply) = board_http(
        port,
        &cad328_post(port, propose, &guards, r#"{"inputs":{"title":"a\nb"}}"#),
    );
    assert_eq!(status, 400, "{reply}");
    let v: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(v["code"], "one_line", "{v}");
}

/// CAD-496 (CAD-487's codes): a preview refusal is data carrying the
/// daemon's named `code` beside its reason — `one_line` on a value that
/// is no single visible line, `not_distinct` when `distinct:` inputs
/// render equal, `render_diverged` when a one-line value still breaks
/// the rendered plan (`has spaces` is no alias). The run form shows
/// each refusal by name.
#[test]
fn board_workflow_preview_names_render_refusals() {
    let f = PlanFixture::start();
    f.d.register("dev-1");
    f.d.register("qa-1");
    wf_add(&f, "two-step", WF_TWO_STEP);
    wf_add(&f, "pair", WF_PAIR);
    wf_add(&f, "alias", WF_ALIAS);
    wf_add(&f, "shaped", WF_SLUG);
    let port = start_board(&f.pm_dir, &f.d.state);
    let before = f.commits();

    let preview = |name: &str, inputs: &str| -> Value {
        let (status, body) = board_get(
            port,
            &format!(
                "/api/projects/demo/workflows/{name}/preview?inputs={}",
                pct_encode(inputs)
            ),
        );
        assert_eq!(status, 200, "{name}: {body}");
        serde_json::from_str(&body).unwrap()
    };

    let v = preview("two-step", r#"{"title":"a\nb"}"#);
    assert_eq!(v["code"], "one_line", "{v}");
    assert!(
        v["error"]
            .as_str()
            .unwrap_or_default()
            .contains("single line"),
        "{v}"
    );

    let v = preview(
        "pair",
        r#"{"title":"x","worker":"dev-1","reviewer":"dev-1"}"#,
    );
    assert_eq!(v["code"], "not_distinct", "{v}");
    assert!(
        v["error"]
            .as_str()
            .unwrap_or_default()
            .contains("must differ"),
        "{v}"
    );

    let v = preview("alias", r#"{"runner":"has spaces"}"#);
    assert_eq!(v["code"], "render_diverged", "{v}");
    assert!(v["error"].as_str().is_some(), "{v}");

    // CAD-571: a declared input shape (`kind: slug`) crosses the board
    // as the same named refusal — the preview relays the daemon's render
    // for every caller.
    let v = preview("shaped", r#"{"topic":"t","slug":"../x"}"#);
    assert_eq!(v["code"], "bad_shape", "{v}");
    assert!(
        v["error"]
            .as_str()
            .unwrap_or_default()
            .contains("folder name"),
        "{v}"
    );

    // A refusal is a render answer, never a write: nothing committed.
    assert_eq!(f.commits(), before, "previews write nothing");
}

/// CAD-547: the board lists an installed app's workflows beside the
/// stored ones, named `<app>/<wf>` and carrying the APP's approval
/// state (approval is whole-app). Preview reads the installed file;
/// propose relays `plan_propose` — operator-only like every write, and
/// the daemon's `app_unapproved` crosses the wire unchanged until
/// `app approve`, after which the run lands as an ordinary proposed
/// plan whose epic records the app-qualified workflow name.
#[test]
fn board_app_workflow_list_preview_and_propose() {
    let f = PlanFixture::start();
    f.d.register("dev-1");
    f.d.register("qa-1");
    app_install_studio(&f);
    let port = start_board(&f.pm_dir, &f.d.state);
    let op = sign_in(&f.d.state, port);
    let guards = op_guards(&op);
    let before = f.commits();

    // The list: the app workflow row names its app.
    let (status, body) = board_get(port, "/api/projects/demo/workflows");
    assert_eq!(status, 200, "{body}");
    let list: Value = serde_json::from_str(&body).unwrap();
    let row = list["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "studio/do-check")
        .unwrap_or_else(|| panic!("no app row: {list}"));
    assert_eq!(row["app"], "studio", "{row}");
    assert_eq!(row["title"], "Run: title", "{row}");
    assert_eq!(row["tickets"], 2, "{row}");
    assert_eq!(row["approved"], false, "not yet approved: {row}");

    // The preview renders the installed workflow — the `%2F` in the
    // wire path decodes to `<app>/<wf>` before routing.
    let prev = "/api/projects/demo/workflows/studio%2Fdo-check/preview";
    let (status, body) = board_get(
        port,
        &format!("{prev}?inputs={}", pct_encode(r#"{"title":"login fix"}"#)),
    );
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    let rendered = v["rendered"].as_str().unwrap_or_default();
    assert!(rendered.contains("Run: login fix"), "{rendered}");
    assert_eq!(v["approved"], false, "{v}");
    // A malformed app ref and an absent app are HTTP errors.
    for (path, want) in [
        (
            "/api/projects/demo/workflows/a%2Fb%2Fc/preview".to_string(),
            400,
        ),
        (
            "/api/projects/demo/workflows/nope%2Fwf/preview".to_string(),
            404,
        ),
    ] {
        let (status, body) = board_get(port, &path);
        assert_eq!(status, want, "{path}: {body}");
    }

    // Propose is the operator's — refused without a session, and the
    // daemon's app gate crosses the board unchanged.
    let propose = "/api/projects/demo/workflows/studio%2Fdo-check/propose";
    let (status, reply) = board_http(port, &cad328_post(port, propose, THREAD_GUARDS, "{}"));
    assert_eq!(status, 403, "{reply}");
    assert!(reply.contains("operator_session_required"), "{reply}");
    let (status, reply) = board_http(
        port,
        &cad328_post(port, propose, &guards, r#"{"inputs":{"title":"x"}}"#),
    );
    assert_eq!(status, 400, "{reply}");
    assert!(reply.contains("app_unapproved"), "{reply}");
    assert_eq!(f.commits(), before, "refusals write nothing");

    // Once the operator approves the app, the run proposes — the epic
    // records the app-qualified workflow it came from.
    let (ok, out) = f.cli(&["app", "approve", "studio", "--project", "demo"]);
    assert!(ok, "{out}");
    let (status, reply) = board_http(
        port,
        &cad328_post(
            port,
            propose,
            &guards,
            r#"{"inputs":{"title":"login fix"}}"#,
        ),
    );
    assert_eq!(status, 200, "{reply}");
    let out: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(out["epic"], "D-1", "{out}");
    let plan = f.front("D-1").plan.unwrap();
    assert_eq!(plan.state, "proposed");
    assert_eq!(plan.workflow.as_deref(), Some("studio/do-check"));
    // And the row now reports the app's approval.
    let (_, body) = board_get(port, "/api/projects/demo/workflows");
    let list: Value = serde_json::from_str(&body).unwrap();
    let row = list["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "studio/do-check")
        .unwrap();
    assert_eq!(row["approved"], true, "{row}");
}

/// CAD-557: the Apps page's reads — `GET /api/apps` answers one row per
/// installed app per project, from the daemon's real install data:
/// title, version, the declared slots with their effective bindings, the
/// bundle's workflows, the recorded source, the digest and the
/// three-state approval (`approved`/`changed`/`unapproved`/`unknown`).
/// `GET /api/apps/<project>/<name>` answers the detail: the guide, the
/// checked workflow summaries, the rubrics' bodies, the install record
/// and that app's doctor findings.
#[test]
fn board_apps_list_and_detail() {
    let f = PlanFixture::start();
    f.d.register("dev-1");
    f.d.register("qa-1");
    app_install_studio(&f);
    let port = start_board(&f.pm_dir, &f.d.state);

    let (status, body) = board_get(port, "/api/apps");
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    let row = v["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "studio")
        .unwrap_or_else(|| panic!("no studio row: {v}"));
    assert_eq!(row["project"], "demo", "{row}");
    assert_eq!(row["title"], "Content studio", "{row}");
    assert_eq!(row["version"], "0.1.0", "{row}");
    assert_eq!(row["summary"], "Run a checked change.", "{row}");
    assert_eq!(row["workflows"], json!(["do-check"]), "{row}");
    // The card's primary action: the first workflow and its label.
    assert_eq!(
        row["primary"],
        json!({"workflow": "do-check", "label": "New run"}),
        "{row}"
    );
    assert_eq!(
        row["connections"],
        json!([{"slot": "publish", "bound": "local"}]),
        "{row}"
    );
    assert_eq!(row["approved"], false, "{row}");
    assert_eq!(row["approval"], "unapproved", "{row}");
    // A path install names its source dir; a git install would carry
    // its pinned SHA in the same record field.
    assert_eq!(row["source"]["kind"], "path", "{row}");
    assert!(
        row["digest"]
            .as_str()
            .unwrap_or_default()
            .starts_with("sha256:"),
        "{row}"
    );

    // `?project=` scopes the same rows; a bad key is 400, an unknown
    // project 404, and a bare extra segment is no route.
    let (status, body) = board_get(port, "/api/apps?project=demo");
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["apps"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "{body}"
    );
    let (status, _) = board_get(port, "/api/apps?project=nope");
    assert_eq!(status, 404);
    let (status, _) = board_get(port, "/api/apps?project=Bad%20Key");
    assert_eq!(status, 400);
    let (status, _) = board_get(port, "/api/apps/demo");
    assert_eq!(status, 404);
    let (status, _) = board_get(port, "/api/appsx");
    assert_eq!(status, 404);

    // The detail: guide, app-qualified workflow summaries, rubric
    // bodies, the install record and this app's doctor row.
    let (status, body) = board_get(port, "/api/apps/demo/studio");
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert!(
        v["guide"]
            .as_str()
            .unwrap_or_default()
            .contains("How to run the studio"),
        "{v}"
    );
    assert_eq!(v["summary"], "Run a checked change.", "{v}");
    assert_eq!(v["workflows"][0]["name"], "studio/do-check", "{v}");
    assert_eq!(v["workflows"][0]["ok"], true, "{v}");
    assert_eq!(v["workflows"][0]["label"], "New run", "{v}");
    assert_eq!(v["workflows"][0]["uses"], json!(["publish"]), "{v}");
    // The inputs keep the file's order — the app page's primary field
    // (and the form's first field) is the first declared input.
    assert_eq!(
        v["workflows"][0]["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["title", "note"],
        "{v}"
    );
    // The steps, in order, from the canonical render — the app page's
    // stage row and its team mapping read these (CAD-563 r2).
    assert_eq!(
        v["workflows"][0]["steps"],
        json!([
            {"title": "Do title", "agent": "dev-1", "size": "S"},
            {"title": "Check title", "agent": "qa-1", "size": null},
        ]),
        "{v}"
    );
    assert_eq!(v["workflows"][0]["distinct"], json!([]), "{v}");
    assert_eq!(v["rubrics"][0]["name"], "review", "{v}");
    assert!(
        v["rubrics"][0]["body"]
            .as_str()
            .unwrap_or_default()
            .contains("check the work"),
        "{v}"
    );
    assert_eq!(v["record"]["app"], "studio", "{v}");
    assert_eq!(v["record"]["source"]["kind"], "path", "{v}");
    let doctor = &v["doctor"];
    assert_eq!(doctor["app"], "studio", "{doctor}");
    assert_eq!(doctor["unbound"], json!([]), "{doctor}");
    // `publish` fell back to the `local` default — a connection the
    // daemon knows, so it lands in slots_ok.
    assert_eq!(
        doctor["slots_ok"],
        json!([{"slot": "publish", "connection": "local"}]),
        "{doctor}"
    );

    // An app that is not installed is a 404 — not an error-shaped row;
    // bad names and unknown projects are refused by grammar and key.
    let (status, _) = board_get(port, "/api/apps/demo/nope");
    assert_eq!(status, 404);
    let (status, _) = board_get(port, "/api/apps/demo/Bad%20Name");
    assert_eq!(status, 400);
    let (status, _) = board_get(port, "/api/apps/nope/studio");
    assert_eq!(status, 404);
    let (status, _) = board_get(port, "/api/apps/demo/studio/extra");
    assert_eq!(status, 404);

    // An explicit unbind flags the slot — and the doctor row reports it.
    let (ok, out) = f.cli(&["app", "set", "studio", "publish=", "--project", "demo"]);
    assert!(ok, "{out}");
    let (status, body) = board_get(port, "/api/apps/demo/studio");
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        v["connections"],
        json!([{"slot": "publish", "bound": null}]),
        "{v}"
    );
    assert_eq!(v["doctor"]["unbound"], json!(["publish"]), "{v}");
}

/// CAD-557: `POST /api/apps/<project>/<name>/approve` relays the
/// daemon's `app_approve` — operator-only like the board's other
/// writes, so an unproven caller, an agent-attributed caller and a
/// forged attribution field are all refused before the daemon sees
/// them. Once admitted it approves the app's CURRENT digest: the rows
/// then read `approved`, and a later hand edit flips them to `changed`
/// until the operator approves again.
#[test]
fn board_app_approve_is_the_operators() {
    let f = PlanFixture::start();
    f.d.register("dev-1");
    f.d.register("qa-1");
    app_install_studio(&f);
    let port = start_board(&f.pm_dir, &f.d.state);
    let approve = "/api/apps/demo/studio/approve";

    // No session — refused before the handler runs.
    let (status, reply) = board_http(port, &cad328_post(port, approve, THREAD_GUARDS, "{}"));
    assert_eq!(status, 403, "{reply}");
    assert!(reply.contains("operator_session_required"), "{reply}");

    // An agent-attributed caller is refused — an approval is never a
    // pane's decision, even with the write guards in place.
    let mut wk = ManagedWorker::start(&f.d, "wk");
    let request = cad328_post(port, approve, THREAD_GUARDS, "{}");
    let r = wk.exec(&[
        "bash",
        "-c",
        DEV_TCP_CLIENT,
        "_",
        &port.to_string(),
        &request,
    ]);
    let out = r["out"].as_str().unwrap();
    assert!(
        out.contains(" 403 ") && out.contains("operator_only"),
        "{out}"
    );

    // Signed in: a forged attribution field dies in the body schema —
    // the daemon never sees it. (The member-role caller is refused in
    // board.rs's public-session test.)
    let op = sign_in(&f.d.state, port);
    let guards = op_guards(&op);
    for body in [
        r#"{"actor":"wk"}"#,
        r#"{"by":"operator"}"#,
        r#"{"name":"studio","project":"demo","peer_pid":1}"#,
    ] {
        let (status, reply) = board_http(port, &cad328_post(port, approve, &guards, body));
        assert_eq!(status, 400, "{body}: {reply}");
    }
    // And a name that is not an installed app refuses, never approves.
    let (status, reply) = board_http(
        port,
        &cad328_post(port, "/api/apps/demo/nope/approve", &guards, "{}"),
    );
    assert_eq!(status, 400, "{reply}");
    assert!(
        !cadence_agent::issue::app::fetch_approvals(&f.d.state)
            .unwrap_or_default()
            .contains_key("demo/studio"),
        "refusals approve nothing"
    );

    // The operator's POST approves — the daemon's own payload returns.
    let (status, reply) = board_http(port, &cad328_post(port, approve, &guards, "{}"));
    assert_eq!(status, 200, "{reply}");
    let v: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(v["project"], "demo", "{v}");
    assert_eq!(v["name"], "studio", "{v}");
    assert_eq!(v["by"], "operator", "{v}");
    assert!(
        v["digest"]
            .as_str()
            .unwrap_or_default()
            .starts_with("sha256:"),
        "{v}"
    );
    let approvals = cadence_agent::issue::app::fetch_approvals(&f.d.state).unwrap();
    assert_eq!(approvals["demo/studio"]["name"], "studio", "{approvals:?}");
    assert_eq!(approvals["demo/studio"]["by"], "operator", "{approvals:?}");

    // The rows then read approved — the gate a propose checks flips too.
    let (status, body) = board_get(port, "/api/apps/demo/studio");
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["approved"], true, "{v}");
    assert_eq!(v["approval"], "approved", "{v}");

    // A hand edit moves the digest — the rows then read `changed`,
    // never silently still-approved.
    let guide = f
        .pm_dir
        .join("demo")
        .join("apps")
        .join("studio")
        .join("app.md");
    std::fs::write(&guide, format!("{APP_MD}\nEdited.\n")).unwrap();
    let (status, body) = board_get(port, "/api/apps/demo/studio");
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["approved"], false, "{v}");
    assert_eq!(v["approval"], "changed", "{v}");
}

/// CAD-563: `GET /api/apps/<project>/<name>/runs` — the plans/epics
/// proposed from the app's workflows, by the recorded `plan.workflow`
/// provenance. Each row carries the epic, its derived status and the
/// plan block `plan show` renders (state, tickets, size-weighted
/// progress), and the list follows the tracker: a proposed plan reads
/// `proposed`, an approved one moves as its tickets do. A stored
/// workflow's plan — a bare name — is not the app's run, and an app
/// that is not installed is a 404 (a bad name 400, a deeper path no
/// route).
#[test]
fn board_app_runs_are_the_apps_plans() {
    let f = PlanFixture::start();
    f.d.register("dev-1");
    f.d.register("qa-1");
    app_install_studio(&f);
    let port = start_board(&f.pm_dir, &f.d.state);

    // Installed, nothing run yet: an empty list, never an error.
    let (status, body) = board_get(port, "/api/apps/demo/studio/runs");
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["project"], "demo", "{v}");
    assert_eq!(v["name"], "studio", "{v}");
    assert_eq!(v["runs"], json!([]), "{v}");

    // The operator approves the app, then proposes a run from its
    // workflow — the epic records `plan.workflow = studio/do-check`.
    f.d.operator_rpc("app_approve", json!({"project": "demo", "name": "studio"}))
        .unwrap();
    let out =
        f.d.operator_rpc(
            "plan_propose",
            json!({"project": "demo", "workflow": "studio/do-check",
                   "inputs": {"title": "login fix"}}),
        )
        .unwrap();
    let epic = out["epic"].as_str().unwrap().to_string();
    let tickets: Vec<String> = out["tickets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect();

    let (status, body) = board_get(port, "/api/apps/demo/studio/runs");
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    let runs = v["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1, "{v}");
    let run = &runs[0];
    assert_eq!(run["epic"], json!(epic), "{run}");
    assert_eq!(run["title"], "Run: login fix", "{run}");
    assert_eq!(run["workflow"], "studio/do-check", "{run}");
    assert_eq!(run["plan"]["state"], "proposed", "{run}");
    assert_eq!(
        run["plan"]["tickets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        tickets.iter().map(String::as_str).collect::<Vec<_>>(),
        "{run}"
    );
    // The workflow's S + unsized (M) tickets weigh 4; nothing is done
    // while the plan waits, and the epic reads its own backlog.
    assert_eq!(run["plan"]["progress"]["done_weight"], 0, "{run}");
    assert_eq!(run["plan"]["progress"]["total_weight"], 4, "{run}");
    assert_eq!(run["status"], "backlog", "{run}");

    // Approve the plan and finish one ticket: the state, the weighted
    // progress and the row follow the tracker.
    f.d.operator_rpc("plan_approve", json!({"epic": epic}))
        .unwrap();
    let (ok, out) = f.cli(&["issue", "set", &tickets[0], "status=done"]);
    assert!(ok, "{out}");
    let (status, body) = board_get(port, "/api/apps/demo/studio/runs");
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    let run = &v["runs"][0];
    assert_eq!(run["plan"]["state"], "approved", "{run}");
    assert_eq!(run["plan"]["progress"]["done_weight"], 1, "{run}");
    assert_eq!(run["plan"]["progress"]["ratio"], 0.25, "{run}");

    // A stored workflow that shares the app's name records a bare
    // `plan.workflow = "studio"` (CAD-547) — not the app's run.
    wf_add(&f, "studio", WF_TWO_STEP);
    f.d.operator_rpc(
        "workflow_approve",
        json!({"project": "demo", "name": "studio"}),
    )
    .unwrap();
    let (ok, out) = f.cli(&[
        "plan",
        "propose",
        "--project",
        "demo",
        "--workflow",
        "studio",
        "--input",
        "title=stored name",
    ]);
    assert!(ok, "{out}");
    let (status, body) = board_get(port, "/api/apps/demo/studio/runs");
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["runs"].as_array().unwrap().len(), 1, "{v}");

    // An app that is not installed is a 404; a bad name 400; a deeper
    // path is no route.
    let (status, _) = board_get(port, "/api/apps/demo/nope/runs");
    assert_eq!(status, 404);
    let (status, _) = board_get(port, "/api/apps/demo/Bad%20Name/runs");
    assert_eq!(status, 400);
    let (status, _) = board_get(port, "/api/apps/nope/studio/runs");
    assert_eq!(status, 404);
    let (status, _) = board_get(port, "/api/apps/demo/studio/runs/x");
    assert_eq!(status, 404);
}

/// One `local` outbox item in the adapter's on-disk shape —
/// `<outbox>/<project>/<effect_id>/{index.json,post.md}` — the shape
/// `platform_outbox` lists. The route's fixture: no platform, no press.
fn write_outbox_item(outbox: &Path, effect_id: &str, project: &str, title: &str, at: &str) {
    let dir = outbox.join(project).join(effect_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("post.md"), format!("# {title}\n\nbody\n")).unwrap();
    std::fs::write(
        dir.join("index.json"),
        json!({
            "effect_id": effect_id,
            "project": project,
            "title": title,
            "published_at": at,
            "post_sha256": "0".repeat(64),
            "content_sha256": "0".repeat(64),
            "input_sha256": "0".repeat(64),
            "attachments": [],
            "result": {"board_url": format!("http://127.0.0.1:3919/outbox?item={effect_id}")},
        })
        .to_string(),
    )
    .unwrap();
}

/// CAD-563: `GET /api/apps/<project>/<name>/outputs` — the outbox items
/// the app's runs produced, attributed by the effect's recorded `task`
/// (a ticket of one of the app's runs). An item with no task, one whose
/// task is no run's ticket, and one whose effect row is unknown all
/// stay out — a task-less send by an agent owning one of the tickets is
/// not the run's work (CAD-571 N7). The read is the operator's — the
/// same gate `/api/outbox` runs: no session and an agent-attributed
/// caller are refused before any ledger byte is read, a missing app is
/// a 404.
#[test]
fn board_app_outputs_are_the_runs_and_operator_only() {
    let outbox = TempDir::new().unwrap();
    let mut opts = daemon_opts();
    opts.outbox_dir = Some(outbox.path().to_path_buf());
    let f = PlanFixture::start_with(opts);
    f.d.register("dev-1");
    f.d.register("qa-1");
    app_install_studio(&f);
    let port = start_board(&f.pm_dir, &f.d.state);

    f.d.operator_rpc("app_approve", json!({"project": "demo", "name": "studio"}))
        .unwrap();
    let out =
        f.d.operator_rpc(
            "plan_propose",
            json!({"project": "demo", "workflow": "studio/do-check",
                   "inputs": {"title": "login fix"}}),
        )
        .unwrap();
    let epic = out["epic"].as_str().unwrap().to_string();
    let tickets: Vec<String> = out["tickets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect();
    // The workflow's `agent:` lines own the tickets: dev-1, then qa-1.
    assert_eq!(f.front(&tickets[0]).owner.as_deref(), Some("dev-1"));
    assert_eq!(f.front(&tickets[1]).owner.as_deref(), Some("qa-1"));

    // Three published items and their effect rows: one staged with the
    // ticket named as its task, one staged without a task by a ticket's
    // owner (not the run's work), one whose task is another run's. A
    // fourth row is still `waiting` on a run ticket — a staged send the
    // operator has not released — and a fifth is a waiting send that
    // names no ticket, so no run accounts for it.
    write_outbox_item(
        outbox.path(),
        "ef-task",
        "demo",
        "By task",
        "2026-09-26T01:00:00Z",
    );
    write_outbox_item(
        outbox.path(),
        "ef-owner",
        "demo",
        "By owner",
        "2026-09-26T02:00:00Z",
    );
    write_outbox_item(
        outbox.path(),
        "ef-other",
        "demo",
        "Not this app",
        "2026-09-26T03:00:00Z",
    );
    let conn = rusqlite::Connection::open(f.d.state.join("cadence.sqlite3")).unwrap();
    for (eid, agent, task, state, input) in [
        (
            "ef-task",
            "someone",
            Some(tickets[0].as_str()),
            "done",
            "{}",
        ),
        ("ef-owner", "qa-1", None, "done", "{}"),
        ("ef-other", "stranger", Some("D-9"), "done", "{}"),
        (
            "ef-waiting",
            "qa-1",
            Some(tickets[1].as_str()),
            "waiting",
            r#"{"title": "Ready to release"}"#,
        ),
        ("ef-stray", "stranger", None, "waiting", "{}"),
    ] {
        conn.execute(
            "INSERT INTO platform_effects (effect_id, request, agent, platform, account, \
             tool, input, input_summary, preview, scopes, task, state, needs_you, \
             staged_at, updated_at) \
             VALUES (?1, ?2, ?3, 'local', 'outbox', 'publish', ?5, 'publish', \
             'a post', '[\"publish\"]', ?4, ?6, 0, 1.0, 1.0)",
            rusqlite::params![eid, format!("req-{eid}"), agent, task, input, state],
        )
        .unwrap();
    }

    // No session: refused by the board's own caller rule, before the
    // handler runs.
    let (status, reply) = board_get(port, "/api/apps/demo/studio/outputs");
    assert_eq!(status, 403, "{reply}");
    assert!(reply.contains("operator_session_required"), "{reply}");

    // An agent-attributed caller is refused — the ledger is the
    // operator's read.
    let mut wk = ManagedWorker::start(&f.d, "wk");
    let request =
        format!("GET /api/apps/demo/studio/outputs HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n");
    let r = wk.exec(&[
        "bash",
        "-c",
        DEV_TCP_CLIENT,
        "_",
        &port.to_string(),
        &request,
    ]);
    let out = r["out"].as_str().unwrap();
    assert!(
        out.contains(" 403 ") && out.contains("operator_only"),
        "{out}"
    );

    // The gate runs before the tracker read: a missing app is a 403 to
    // a caller who may not read, a 404 to the operator.
    let (status, reply) = board_get(port, "/api/apps/demo/nope/outputs");
    assert_eq!(status, 403, "{reply}");
    let op = sign_in(&f.d.state, port);
    let (status, reply) = op_get(&op, port, "/api/apps/demo/nope/outputs");
    assert_eq!(status, 404, "{reply}");

    // The proven operator reads the app's items — the task-tied one
    // only: the task-less send by a ticket's owner is no run's work, and
    // neither is another run's task. Each item names the run it is
    // attributed to.
    let (status, reply) = op_get(&op, port, "/api/apps/demo/studio/outputs");
    assert_eq!(status, 200, "{reply}");
    let v: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(v["project"], "demo", "{v}");
    assert_eq!(v["name"], "studio", "{v}");
    let ids: Vec<&str> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["effect_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["ef-task"], "{v}");
    assert!(
        v["items"][0]["preview"]
            .as_str()
            .unwrap()
            .contains("By task"),
        "{v}"
    );
    assert_eq!(v["items"][0]["runs"], json!([epic]), "{v}");
    // The staged, unreleased send of one of the runs is the app's next
    // output — with its human title; a waiting send naming no ticket
    // stays out.
    let pending = v["pending"].as_array().unwrap();
    assert_eq!(pending.len(), 1, "{v}");
    assert_eq!(pending[0]["effect_id"], "ef-waiting", "{v}");
    assert_eq!(pending[0]["state"], "waiting", "{v}");
    assert_eq!(pending[0]["title"], "Ready to release", "{v}");
    assert_eq!(pending[0]["runs"], json!([epic]), "{v}");
    // A bad name is refused by grammar even for the operator.
    let (status, _) = op_get(&op, port, "/api/apps/demo/Bad%20Name/outputs");
    assert_eq!(status, 400);
}

/// The session `op` as presented to the board on `port` instead: that
/// board's own Host and Origin, its cookie name, the same token (a
/// session is the daemon's, not one board's). A session carried to a
/// board it wasn't minted on is a replay: it asserts no caller, so the
/// HTTP peer stands on its own identity (and an unarmed board doesn't
/// refuse the header outright).
fn op_on(op: &op::Session, port: u16) -> op::Session {
    let host = op::board_host(port);
    let token = op.cookie.split_once('=').unwrap().1;
    op::Session {
        host: host.clone(),
        origin: format!("http://{host}"),
        cookie: format!("cadence_operator_{port}={token}"),
        set_cookie: op.set_cookie.clone(),
        key: op.key.clone(),
        seam: String::new(),
    }
}

/// `GET path` on `port` as the signed-in operator (no `Origin`: a
/// browser sends none on a same-origin GET), asserted as `op.seam`.
fn op_get(op: &op::Session, port: u16, path: &str) -> (u16, String) {
    board_http(
        port,
        &format!(
            "GET {path} HTTP/1.0\r\nHost: {}\r\nCookie: {}\r\n{}\r\n{}\r\n",
            op.host,
            op.cookie,
            op.key_header(),
            op.seam
        ),
    )
}

// ---- CAD-432: the Projects screen's epic stages and milestones ----

/// A demo epic `D-1` (type=epic, milestone m1) with two sized children;
/// the epic has never moved, so it reads as `shape`.
fn cad432_epic(f: &PlanFixture) {
    assert!(f.cli(&["issue", "new", "Epic", "--project", "demo"]).0);
    for title in ["Big", "Small"] {
        let (ok, out) = f.cli(&["issue", "new", title, "--project", "demo", "--epic", "D-1"]);
        assert!(ok, "{out}");
    }
    assert!(f.cli(&["issue", "set", "D-2", "size=L", "status=done"]).0);
    assert!(f.cli(&["issue", "set", "D-3", "size=S"]).0);
    let (ok, out) = f.cli(&["issue", "set", "D-1", "type=epic", "milestone=m1"]);
    assert!(ok, "{out}");
}

fn cad432_move(port: u16, headers: &str, body: &str) -> (u16, String) {
    board_http(
        port,
        &cad328_post(port, "/api/epics/D-1/stage", headers, body),
    )
}

/// CAD-432: `POST /api/epics/<id>/stage` relays the daemon's `epic_stage`
/// behind the board's operator write path. The relay runs over the
/// board's own connection, which the daemon attributes to the operator,
/// so EVERY board move — the routine `build → verify` included, which a
/// pane may make over its own connection — is the operator's: an
/// agent-attributed caller gets 403, and so do a missing guard, a
/// read-only board and an identity-shaped field; each refusal writes
/// nothing. The operator's move lands as one commit by `operator`, the
/// history reads it as a `stage` entry, the card offers exactly the
/// legal next moves, `/api/meta` says who is the operator, and
/// `/api/milestones` rolls the epic up.
#[test]
fn cad432_board_stage_moves_are_operator_only_and_relayed() {
    let f = PlanFixture::start();
    cad432_epic(&f);
    let port = start_board(&f.pm_dir, &f.d.state);
    let op = sign_in(&f.d.state, port);
    let guards = op_guards(&op);
    let stage = || f.front("D-1").stage;
    let before = f.commits();
    let untouched = |what: &str, commits: usize, want: Option<&str>| {
        assert_eq!(f.commits(), commits, "{what}: a refusal writes nothing");
        assert_eq!(stage().as_deref(), want, "{what}");
    };
    let to_build = r#"{"stage":"build"}"#;

    // The board's write guards, then a read-only board.
    let (status, reply) = cad432_move(port, "Content-Type: application/json\r\n", to_build);
    assert_eq!(status, 403, "{reply}");
    assert!(reply.contains("x_cadence_board"), "{reply}");
    let ro = start_board_with(&f.pm_dir, &f.d.state, true);
    let (status, reply) = cad432_move(ro, THREAD_GUARDS, to_build);
    assert_eq!(status, 403, "{reply}");
    assert!(reply.contains("read_only"), "{reply}");
    let (_, meta) = board_get(ro, "/api/meta?operator=1");
    let meta: Value = serde_json::from_str(&meta).unwrap();
    assert_eq!(meta["operator"], false, "read-only is nobody's: {meta}");
    untouched("guards", before, None);

    // No identity-shaped field is read; a GET is not a move.
    for body in [
        r#"{"stage":"build","by":"operator"}"#,
        r#"{"stage":"build","actor":"operator"}"#,
        r#"{"stage":"build","operator":true}"#,
        r#"{"note":"x"}"#,
        r#"{"stage":"  "}"#,
    ] {
        let (status, reply) = cad432_move(port, &guards, body);
        assert_eq!(status, 400, "{body}: {reply}");
    }
    assert_eq!(board_get(port, "/api/epics/D-1/stage").0, 404);
    untouched("bad requests", before, None);

    // An agent-attributed caller is refused before any relay — for the
    // operator stage and, below, for a routine move too.
    let mut wk = ManagedWorker::start(&f.d, "wk");
    let mut agent_move = |body: &str| -> String {
        let request = cad328_post(port, "/api/epics/D-1/stage", THREAD_GUARDS, body);
        let r = wk.exec(&[
            "bash",
            "-c",
            DEV_TCP_CLIENT,
            "_",
            &port.to_string(),
            &request,
        ]);
        assert_eq!(r["rc"], 0, "{r}");
        r["out"].as_str().unwrap().to_string()
    };
    let out = agent_move(to_build);
    assert!(out.contains(" 403 "), "{out}");
    assert!(
        out.contains("operator_only") && out.contains("'wk'"),
        "{out}"
    );
    untouched("agent → build", before, None);
    assert!(f.daemon_events("epic_stage_moved").is_empty());

    // The operator: this test process, outside every agent — with its
    // session (CAD-313). The proof walks /proc, so `/api/meta` runs it
    // only when asked; without the session it is nobody's.
    let (_, meta) = op_get(&op, port, "/api/meta");
    let meta: Value = serde_json::from_str(&meta).unwrap();
    assert_eq!(
        meta["operator"],
        Value::Null,
        "not computed unasked: {meta}"
    );
    assert_eq!(meta["signed_in"], true, "{meta}");
    let (_, meta) = board_get(port, "/api/meta?operator=1");
    let meta: Value = serde_json::from_str(&meta).unwrap();
    assert_eq!(meta["operator"], false, "no session, no operator: {meta}");
    let (status, reply) = cad432_move(port, THREAD_GUARDS, to_build);
    assert_eq!(status, 403, "{reply}");
    assert!(reply.contains("operator_session_required"), "{reply}");
    untouched("no session", before, None);
    let (_, meta) = op_get(&op, port, "/api/meta?operator=1");
    let meta: Value = serde_json::from_str(&meta).unwrap();
    assert_eq!(meta["operator"], true, "{meta}");
    let (status, reply) = cad432_move(
        port,
        &guards,
        r#"{"stage":"build","note":"shaped (all 2 tasks)"}"#,
    );
    assert_eq!(status, 200, "{reply}");
    let moved: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(
        (&moved["from"], &moved["to"], &moved["by"]),
        (&json!("shape"), &json!("build"), &json!("operator")),
        "{moved}"
    );
    assert_eq!(f.commits(), before + 1);
    assert_eq!(stage().as_deref(), Some("build"));
    assert_eq!(f.daemon_events("epic_stage_moved").len(), 1);
    let after_build = f.commits();

    // A routine move is still the operator's on the board: the daemon
    // would take the board's relay as the operator's own.
    let out = agent_move(r#"{"stage":"verify"}"#);
    assert!(out.contains(" 403 "), "{out}");
    assert!(out.contains("operator_only"), "{out}");
    untouched("agent → verify", after_build, Some("build"));

    // The daemon's move rules hold through the relay.
    for (body, want) in [
        (r#"{"stage":"release"}"#, "skips a stage"),
        (r#"{"stage":"ship"}"#, "Unknown stage"),
        (r#"{"stage":"build"}"#, "already in stage"),
    ] {
        let (status, reply) = cad432_move(port, &guards, body);
        assert!(status == 400 || status == 409, "{body}: {status} {reply}");
        assert!(reply.contains(want), "{body}: {reply}");
    }
    untouched("illegal moves", after_build, Some("build"));

    // Stage history: who moved it, when, from and to.
    let (status, body) = board_get(port, "/api/issues/D-1/history?limit=50");
    assert_eq!(status, 200, "{body}");
    let hist: Value = serde_json::from_str(&body).unwrap();
    let moves: Vec<&Value> = hist["history"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "stage")
        .collect();
    assert_eq!(moves.len(), 1, "{hist}");
    assert_eq!(
        (
            &moves[0]["from"],
            &moves[0]["to"],
            &moves[0]["by"],
            &moves[0]["note"]
        ),
        (
            &json!("shape"),
            &json!("build"),
            &json!("operator"),
            &json!("shaped (all 2 tasks)")
        ),
        "{hist}"
    );
    assert!(moves[0]["at"].as_str().is_some_and(|a| !a.is_empty()));

    // The card offers exactly the legal moves, flagged for the operator.
    let (_, body) = board_get(port, "/api/issues?project=demo");
    let cards: Value = serde_json::from_str(&body).unwrap();
    let epic = cards["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "D-1")
        .unwrap();
    assert_eq!(
        epic["work"]["stage"]["moves"],
        json!([
            {"to": "shape", "forward": false, "needs_operator": false},
            {"to": "verify", "forward": true, "needs_operator": false},
        ]),
        "{epic}"
    );

    // Milestones: weighted progress (L done of L+S) and worst health.
    let (status, body) = board_get(port, "/api/milestones?project=demo");
    assert_eq!(status, 200, "{body}");
    let ms: Value = serde_json::from_str(&body).unwrap();
    let m1 = &ms["milestones"][0];
    assert_eq!(m1["id"], "m1", "{ms}");
    assert_eq!(
        (
            &m1["progress"]["done_weight"],
            &m1["progress"]["total_weight"]
        ),
        (&json!(8), &json!(9)),
        "{ms}"
    );
    assert_eq!(m1["health"]["state"], "on_track", "{ms}");
    assert_eq!(m1["epics"][0]["stage"], "build", "{ms}");
    assert_eq!(board_get(port, "/api/milestones?project=Bad!").0, 400);
}

/// CAD-432 adversarial: under a real `daemon run`, a detached,
/// env-scrubbed child of an enrolled managed worker's tool is tied to no
/// agent, so `write_caller` alone would read it as the operator. The
/// stage route runs the positive operator proof on its peer: the child
/// descends from the daemon and is refused `403 operator_proof`, writing
/// nothing, and `/api/meta` tells it it is not the operator. The
/// operator's own move still lands.
#[test]
fn cad432_stage_move_refuses_a_detached_managed_child_under_daemon_run() {
    let dir = TempDir::new().unwrap();
    let mock = ManagedWorker::install(dir.path(), dir.path(), "wk");
    let f = PlanFixture::start_on(|| TestDaemon::start_process_in(dir));
    let _reaper = DaemonReaper::new(&f.d.state);
    let daemon_pid = subreaper_daemon_pid(&f.d);
    let mut wk = mock.enroll(&f.d, "wk");
    cad432_epic(&f);
    let port = start_board(&f.pm_dir, &f.d.state);
    // CAD-313: the child presents the operator's session, as if stolen —
    // what refuses it is the process proof on the peer.
    let op = sign_in(&f.d.state, port);
    let guards = op_guards(&op);
    // The detached child presents the stolen session but stands on its
    // own (daemon-descendant) caller — no seam assertion rides along.
    let stolen_guards = op_guards_as(&op, "");
    let before = f.commits();

    const INNER: &str = r#"exec 3<>"/dev/tcp/127.0.0.1/$1"; printf '%s' "$2" >&3; cat <&3 > "$3.tmp"; mv "$3.tmp" "$3""#;
    const OUTER: &str =
        r#"setsid -f env -i /bin/bash -c "$1" _ "$2" "$3" "$4" </dev/null >/dev/null 2>&1"#;
    let work = TempDir::new().unwrap();
    let mut n = 0;
    let mut detached = |request: String| -> String {
        n += 1;
        let out = work.path().join(format!("reply-{n}"));
        let r = wk.exec(&[
            "bash",
            "-c",
            OUTER,
            "_",
            INNER,
            &port.to_string(),
            &request,
            out.to_str().unwrap(),
        ]);
        assert_eq!(r["rc"], 0, "{r}");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !out.exists() {
            assert!(
                Instant::now() < deadline,
                "the detached child never answered"
            );
            thread::sleep(Duration::from_millis(20));
        }
        std::fs::read_to_string(&out).unwrap()
    };
    let reply = detached(cad328_post(
        port,
        "/api/epics/D-1/stage",
        &stolen_guards,
        r#"{"stage":"build"}"#,
    ));
    assert!(reply.contains(" 403 "), "{reply}");
    assert!(reply.contains("operator_proof"), "{reply}");
    assert!(
        reply.contains(&format!("descends from the daemon (pid {daemon_pid})")),
        "{reply}"
    );
    assert_eq!(f.commits(), before, "a refusal writes nothing");
    assert_eq!(f.front("D-1").stage, None);
    assert!(f.daemon_events("epic_stage_moved").is_empty());
    let reply = detached(format!(
        "GET /api/meta?operator=1 HTTP/1.0\r\nHost: {}\r\nCookie: {}\r\n{}\r\n\r\n",
        op.host,
        op.cookie,
        op.key_header()
    ));
    assert!(reply.contains("\"operator\": false"), "{reply}");

    let (status, reply) = cad432_move(port, &guards, r#"{"stage":"build"}"#);
    assert_eq!(status, 200, "{reply}");
    assert_eq!(f.front("D-1").stage.as_deref(), Some("build"));
}

/// CAD-432 review round 1: the board relays a move over its OWN daemon
/// connection. A board an enrolled worker's tool started descends from
/// that worker, so without `operator_decision` the daemon attributed the
/// proven operator's ROUTINE move (`build → verify`, no operator stage)
/// to the worker: `200`, `by: wk`, `Actor: wk`. The board now marks
/// every relayed move as the operator's decision and the daemon demands
/// the operator on the board's connection for any target: the move is
/// refused and nothing is written. `/api/meta` reports `operator: false`
/// on that board, so it offers no move buttons.
#[test]
fn cad432_board_started_by_an_agent_cannot_relay_a_move() {
    let f = PlanFixture::start();
    cad432_epic(&f);
    f.d.operator_rpc("epic_stage", json!({"epic": "D-1", "stage": "build"}))
        .unwrap();
    let mut wk = ManagedWorker::start(&f.d, "wk");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let pidfile = f.tmp.path().join("agent-board.pid");
    // The worker's tool runs `cadence ui run` in the foreground (the
    // worker stays busy with it); the board is the tool's own child.
    // CAD-482: on a seam build the board asserts its agent's identity
    // on the process itself (`CADENCE_TEST_AS`), so `board_is_operator`
    // answers the same in a pane and in CI; a non-seam build keeps the
    // ambient ancestry path.
    let seam_env = if cfg!(feature = "test-seam") {
        format!(
            "{}=1 {}=agent:wk ",
            cadence_agent::test_seam::ARM_ENV,
            cadence_agent::test_seam::AS_ENV
        )
    } else {
        String::new()
    };
    let script = format!(
        "echo $$ > {pid}; exec env {seam_env}CADENCE_PM_DIR={pm} {bin} --state-dir {state} ui run --port {port}",
        pid = pidfile.display(),
        pm = f.pm_dir.display(),
        bin = env!("CARGO_BIN_EXE_cadence"),
        state = f.d.state.display(),
    );
    let n = wk.send(json!({"how": "exec", "argv": ["bash", "-c", script]}));
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        // `board_get` unwraps its connect; probe the bind first.
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
            && board_get(port, "/api/health").0 == 200
        {
            break;
        }
        assert!(Instant::now() < deadline, "the agent's board never came up");
        thread::sleep(Duration::from_millis(50));
    }
    let before = f.commits();

    // CAD-313: the agent's board cannot even open a session — the
    // daemon sees the worker on the board's connection and spends the
    // link. The operator signs in on its own board instead, and presents
    // that session to the agent's board.
    let link = op::login_link(env!("CARGO_BIN_EXE_cadence"), &f.d.state, port, &[]).unwrap();
    let (status, _, body) = op::exchange(port, &op::board_host(port), &op::nonce_of(&link));
    assert_eq!(status, 403, "{body}");
    assert!(
        body.contains("session_from_agent") || body.contains("'wk'"),
        "{body}"
    );
    let own = start_board(&f.pm_dir, &f.d.state);
    let op = op_on(&sign_in(&f.d.state, own), port);

    let (_, meta) = op_get(&op, port, "/api/meta?operator=1");
    let meta: Value = serde_json::from_str(&meta).unwrap();
    assert_eq!(
        meta["operator"], false,
        "an agent's board is nobody's: {meta}"
    );

    // The move is asked for by a second enrolled worker's tool: the
    // request's TCP peer descends from an enrolled endpoint, so the
    // board's operator proof refuses it as an agent's — the enrolled
    // hop sits below any CADENCE_ALIAS an ancestor carries, making the
    // refusal identical in a pane and in CI. No seam assertion rides
    // along: the agent's board is unarmed and would refuse one
    // outright. (wk's own exec channel is busy serving the board.)
    let mut wk2 = ManagedWorker::start(&f.d, "wk2");
    let request = cad328_post(
        port,
        "/api/epics/D-1/stage",
        &op_guards_as(&op, ""),
        r#"{"stage":"verify"}"#,
    );
    let r = wk2.exec(&[
        "bash",
        "-c",
        r#"exec 3<>"/dev/tcp/127.0.0.1/$1"; printf '%s' "$2" >&3; cat <&3"#,
        "_",
        &port.to_string(),
        &request,
    ]);
    assert_eq!(r["rc"], 0, "{r}");
    let reply = r["out"].as_str().unwrap().to_string();
    assert!(reply.contains(" 403 "), "{reply}");
    assert!(reply.contains("session_from_agent"), "{reply}");
    assert!(
        reply.contains("'wk2'"),
        "the stolen session names the presenting agent: {reply}"
    );
    assert_eq!(f.commits(), before, "a refusal writes nothing");
    assert_eq!(f.front("D-1").stage.as_deref(), Some("build"));
    assert!(
        !f.last_commit().contains("Actor: wk"),
        "{}",
        f.last_commit()
    );

    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let _ = wk.answer(n, "agent board exit");
}

/// Start [`SESSION_RELAY_PY`] to `target` in its own session; its port.
fn session_relay(dir: &Path, target: u16) -> u16 {
    let script = dir.join("relay.py");
    std::fs::write(&script, SESSION_RELAY_PY).unwrap();
    let portfile = dir.join(format!("relay-{}", uuid::Uuid::new_v4().simple()));
    let status = std::process::Command::new("setsid")
        .arg("-f")
        .arg("python3")
        .arg(&script)
        .arg(target.to_string())
        .arg(&portfile)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let deadline = Instant::now() + Duration::from_secs(20);
    while !portfile.exists() {
        assert!(Instant::now() < deadline, "the relay never listened");
        thread::sleep(Duration::from_millis(20));
    }
    std::fs::read_to_string(&portfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// CAD-428 ACCEPTANCE — the PR #229 round-2 probe. A TCP relay started
/// as its own session (like this host's nginx gateway) forwards an
/// ENROLLED managed agent's plan approve and reject. The board's TCP
/// peer is the relay, which is tied to no agent and passes process
/// proof — so before CAD-313 the approve landed as the operator's.
/// Now it holds no operator session and is refused
/// `operator_session_required`, with nothing written: no commit, the
/// plan still proposed, no decision event. The same relay carrying the
/// operator's session, and the operator directly on loopback with it,
/// succeed. (The tailnet-proven operator is the unit-tested
/// `ui::operator::decide` row: a test cannot own a socket as
/// tailscaled's uid.)
#[test]
fn cad428_a_relay_never_carries_an_agents_approve_as_the_operator() {
    let f = PlanFixture::start();
    let epic = f.propose(PLAN_MD).unwrap()["epic"]
        .as_str()
        .unwrap()
        .to_string();
    let later = f
        .propose("---\ntitle: Later\ngoal: g\n---\n## Only\n### Acceptance\n- [ ] a\n")
        .unwrap()["epic"]
        .as_str()
        .unwrap()
        .to_string();
    let port = start_board(&f.pm_dir, &f.d.state);
    let mut wk = ManagedWorker::start(&f.d, "wk");
    let work = TempDir::new().unwrap();
    let before = f.commits();
    let untouched = |what: &str| {
        assert_eq!(f.commits(), before, "{what}: a refusal writes nothing");
        assert_eq!(f.front(&epic).plan.unwrap().state, "proposed", "{what}");
        assert!(f.daemon_events("plan_approved").is_empty(), "{what}");
        assert!(f.daemon_events("plan_rejected").is_empty(), "{what}");
    };
    for (path, body) in [
        (format!("/api/plans/{epic}/approve"), "{}"),
        (
            format!("/api/plans/{epic}/reject"),
            r#"{"reason":"agent says no"}"#,
        ),
    ] {
        let relay = session_relay(work.path(), port);
        let request = cad328_post(
            port,
            &path,
            &format!("{THREAD_GUARDS}Origin: http://127.0.0.1:{port}\r\n"),
            body,
        );
        let r = wk.exec(&[
            "bash",
            "-c",
            DEV_TCP_CLIENT,
            "_",
            &relay.to_string(),
            &request,
        ]);
        assert_eq!(r["rc"], 0, "{r}");
        let out = r["out"].as_str().unwrap();
        assert!(out.contains(" 403 "), "{path}: {out}");
        assert!(out.contains("operator_session_required"), "{path}: {out}");
    }
    untouched("an agent through a relay");

    // The operator: signed in, through the same kind of relay and
    // directly on loopback.
    let op = sign_in(&f.d.state, port);
    let relay = session_relay(work.path(), port);
    let (status, reply) = board_http(
        relay,
        &cad328_post(
            port,
            &format!("/api/plans/{later}/reject"),
            &op_guards(&op),
            r#"{"reason":"not now"}"#,
        ),
    );
    assert_eq!(status, 200, "{reply}");
    assert_eq!(f.front(&later).plan.unwrap().state, "rejected");
    let (status, reply) = board_http(
        port,
        &cad328_post(
            port,
            &format!("/api/plans/{epic}/approve"),
            &op_guards(&op),
            "{}",
        ),
    );
    assert_eq!(status, 200, "{reply}");
    let plan = f.front(&epic).plan.unwrap();
    assert_eq!(
        (plan.state.as_str(), plan.decided_by.as_deref()),
        ("approved", Some("operator"))
    );
}
