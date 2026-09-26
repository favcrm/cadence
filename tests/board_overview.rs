//! board_overview: area tests split from tests/board.rs (CAD-537).
//! Board e2e: the `cadence issue` CLI against a temp PM dir, and the
//! `cadence ui` HTTP server in-process.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod board_common;
use board_common::*;

use serde_json::json;
use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

fn fake_gh() -> FakeGh {
    let tmp = TempDir::new().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let gh = bin.join("gh");
    std::fs::write(&gh, FAKE_GH).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let log = tmp.path().join("gh.log");
    std::fs::write(&log, "").unwrap();
    FakeGh {
        _tmp: tmp,
        bin,
        log,
    }
}

/// PATH overlay: fake gh first, the just-built cadence second (the
/// tracker's pre-commit hook resolves `cadence` from PATH).
fn gh_path(gh: &FakeGh) -> String {
    format!(
        "{}:{}:{}",
        gh.bin.display(),
        Path::new(bin()).parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// A temp git repo carrying `remote.origin.url` — `issue project add`
/// records the remote for cwd/drift matching.
fn repo_with_remote(remote: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    assert!(git(dir.path(), &["init", "-q"]).0);
    assert!(git(dir.path(), &["config", "user.email", "t@t"]).0);
    assert!(git(dir.path(), &["config", "user.name", "t"]).0);
    std::fs::write(dir.path().join("f"), "x").unwrap();
    assert!(git(dir.path(), &["add", "f"]).0);
    assert!(git(dir.path(), &["commit", "-qm", "init"]).0);
    assert!(git(dir.path(), &["remote", "add", "origin", remote]).0);
    dir
}

/// ISO `<n> seconds ago` — for `updatedAt` fixtures.
fn iso_ago(secs: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    cadence_agent::issue::time::iso(now - secs)
}

#[test]
fn overview_meta_and_shell_routes() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");

    // /api/meta carries the serving binary's build identity.
    let (code, body) = http(port, "GET", "/api/meta", &host);
    assert_eq!(code, 200, "{body}");
    let meta: Value = serde_json::from_str(&body).unwrap();
    assert!(!meta["build_commit"].as_str().unwrap_or("").is_empty());
    assert!(!meta["build_time"].as_str().unwrap_or("").is_empty());
    assert!(!meta["version"].as_str().unwrap_or("").is_empty());

    // /api/overview — the whole derived screen, daemon unreachable is
    // honest but never fatal.
    let (code, body) = http(port, "GET", "/api/overview", &host);
    assert_eq!(code, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert!(v["needs_me"].is_array(), "{v}");
    assert!(v["drift"].is_object(), "{v}");
    assert!(v["projects"].is_array(), "{v}");
    assert!(v["generated_at"].as_i64().unwrap_or(0) > 0, "{v}");
    assert_eq!(v["daemon"]["reachable"], false, "{v}");
    assert_eq!(v["drift"]["matched"], false, "{v}");
    assert!(
        v["drift"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("unreachable"),
        "{v}"
    );
}

#[test]
fn overview_merge_ready_pr_first_with_exact_command() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let repo = repo_with_remote("https://github.com/acme/widgets.git");
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    let repo_s = repo.path().to_str().unwrap().to_string();
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "project", "add", "cadence", "--prefix", "CAD", "--repo", &repo_s]
        )
        .0
    );
    let gh = fake_gh();
    let prs = format!(
        r#"[
        {{"number": 7, "title": "widgets: the fix",
          "url": "https://github.com/acme/widgets/pull/7",
          "headRefOid": "abc", "headRefName": "fix",
          "updatedAt": "{}",
          "statusCheckRollup": [
            {{"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"SUCCESS"}},
            {{"__typename":"StatusContext","context":"qa-verdict","state":"SUCCESS"}}
          ]}},
        {{"number": 9, "title": "wip thing",
          "url": "https://github.com/acme/widgets/pull/9",
          "headRefOid": "def", "headRefName": "wip",
          "updatedAt": "{}",
          "statusCheckRollup": [
            {{"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"SUCCESS"}}
          ]}}
        ]"#,
        iso_ago(7200),
        iso_ago(3 * 3600)
    );
    let path = gh_path(&gh);
    let log = gh.log.to_str().unwrap().to_string();
    let (ok, v) = cli_env(
        pm.path(),
        state.path(),
        &["overview", "--json"],
        &[
            ("PATH", path.as_str()),
            ("FAKE_GH_LOG", log.as_str()),
            ("FAKE_GH_PRS", prs.as_str()),
        ],
    );
    assert!(ok, "{v}");
    let needs = v["needs_me"].as_array().unwrap();
    // The merge-ready PR leads: exact command, link, project, age.
    assert_eq!(needs[0]["kind"], "merge", "{needs:?}");
    assert_eq!(
        needs[0]["command"],
        "gh pr merge 7 --repo acme/widgets --squash --admin --match-head-commit abc",
        "{needs:?}"
    );
    assert_eq!(needs[0]["link"], "https://github.com/acme/widgets/pull/7");
    assert_eq!(needs[0]["project"], "cadence");
    assert!(needs[0]["age"].as_i64().unwrap_or(0) >= 7000);
    // The verdict-less PR follows with its age and the view command.
    assert_eq!(needs[1]["kind"], "pr_no_verdict", "{needs:?}");
    assert_eq!(needs[1]["command"], "gh pr view 9 --repo acme/widgets");
    assert!(needs[1]["age"].as_i64().unwrap_or(0) >= 10000);
    assert_eq!(v["github"]["state"], "ok");
    // gh was asked exactly once per repo for each of the three queries
    // (PRs, default branch, ci.yml runs) — never the legacy status API.
    let calls = std::fs::read_to_string(&gh.log).unwrap();
    assert_eq!(calls.lines().count(), 3, "{calls}");
    assert!(
        calls.lines().any(|c| c
            == "api repos/acme/widgets/actions/workflows/ci.yml/runs?branch=main&event=push&per_page=30"),
        "{calls}"
    );
    assert!(!calls.contains("/status"), "{calls}");
}

#[test]
fn overview_github_failure_degrades_not_fails() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let repo = repo_with_remote("https://github.com/acme/widgets.git");
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    let repo_s = repo.path().to_str().unwrap().to_string();
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "project", "add", "cadence", "--prefix", "CAD", "--repo", &repo_s]
        )
        .0
    );
    // A review issue still surfaces when GitHub is out.
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "new", "review me", "--project", "cadence"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "set", "CAD-1", "status=review"]
        )
        .0
    );
    let gh = fake_gh();
    let path = gh_path(&gh);
    let log = gh.log.to_str().unwrap().to_string();
    let (ok, v) = cli_env(
        pm.path(),
        state.path(),
        &["overview", "--json"],
        &[
            ("PATH", path.as_str()),
            ("FAKE_GH_LOG", log.as_str()),
            ("FAKE_GH_FAIL", "1"),
            ("FAKE_GH_PRS", "[]"),
        ],
    );
    assert!(ok, "{v}");
    assert_eq!(v["github"]["state"], "unavailable", "{v}");
    let needs = v["needs_me"].as_array().unwrap();
    assert!(!needs.iter().any(|n| n["kind"] == "merge"));
    assert!(
        needs.iter().any(|n| n["kind"] == "review_no_pr"),
        "{needs:?}"
    );
}

#[test]
fn overview_tracker_items_and_tracker_behind() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    // seed: CAD-1 container (derived doing via child CAD-2 doing),
    // CAD-3 sibling leaf. Review must go on a leaf — container status
    // rolls up from children.
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "set", "CAD-3", "status=review"]
        )
        .0
    );
    // blocked_ready: CAD-5 blocked_by CAD-4, and CAD-4 is done.
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "new", "blocker", "--project", "cadence"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "new", "blocked work", "--project", "cadence"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "link", "CAD-5", "blocked_by", "CAD-4"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "set", "CAD-4", "status=done"]
        )
        .0
    );
    // tracker_behind: an upstream clone with one extra commit.
    let upstream = TempDir::new().unwrap();
    let (ok, _) = git(
        upstream.path(),
        &["clone", "-q", pm.path().to_str().unwrap(), "."],
    );
    assert!(ok);
    assert!(git(upstream.path(), &["config", "user.email", "t@t"]).0);
    assert!(git(upstream.path(), &["config", "user.name", "t"]).0);
    std::fs::write(upstream.path().join("extra.md"), "x").unwrap();
    assert!(git(upstream.path(), &["add", "extra.md"]).0);
    assert!(git(upstream.path(), &["commit", "-qm", "upstream commit"]).0);
    let branch = git(pm.path(), &["rev-parse", "--abbrev-ref", "HEAD"]).1;
    assert!(
        git(
            pm.path(),
            &["remote", "add", "origin", upstream.path().to_str().unwrap()]
        )
        .0
    );
    assert!(git(pm.path(), &["fetch", "-q", "origin"]).0);
    assert!(
        git(
            pm.path(),
            &[
                "branch",
                "--set-upstream-to",
                &format!("origin/{branch}"),
                &branch
            ]
        )
        .0
    );

    let (ok, v) = cli(pm.path(), state.path(), &["overview", "--json"]);
    assert!(ok, "{v}");
    let needs = v["needs_me"].as_array().unwrap();
    let kind = |k: &str| needs.iter().find(|n| n["kind"] == k);
    let review = kind("review_no_pr").expect("review item");
    assert_eq!(review["command"], "cadence issue show CAD-3");
    let unblocked = kind("blocked_ready").expect("unblocked item");
    assert_eq!(unblocked["command"], "cadence issue set CAD-5 status=ready");
    let behind = kind("tracker_behind").expect("behind item");
    assert_eq!(behind["command"], "cadence issue sync");
    // No github remotes declared → no gh work attempted, state "ok".
    assert_eq!(v["github"]["state"], "ok");
    // projects summary counts + oldest review age.
    let proj = v["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["key"] == "cadence")
        .expect("cadence project row");
    assert!(proj["oldest_review_age"].as_i64().is_some(), "{proj}");
    assert_eq!(proj["open_by_status"]["review"], 1, "{proj}");
}

#[test]
fn overview_plain_render_shows_sections() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "set", "CAD-3", "status=review"]
        )
        .0
    );
    let (ok, out) = cli_raw(pm.path(), state.path(), &["overview"]);
    assert!(ok, "{out}");
    assert!(out.contains("NEEDS ME"), "{out}");
    assert!(out.contains("DRIFT"), "{out}");
    assert!(out.contains("PROJECTS"), "{out}");
    assert!(out.contains("review_no_pr"), "{out}");
    assert!(out.contains("cadence issue show CAD-3"), "{out}");
}

#[test]
fn version_reports_build_identity() {
    let out = Command::new(bin()).arg("--version").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(
        text,
        format!(
            "cadence {}+{}",
            env!("CARGO_PKG_VERSION"),
            env!("CADENCE_BUILD_COMMIT")
        ),
        "{text}"
    );
}

/// A fake daemon on `<state>/cadence.sock` — answers `health`,
/// `agent_list`, `agent_show`, `agent_requests`, `agent_probe`, and
/// rejects `daemon_info` like a build that predates the RPC. Returns
/// the listener thread's stop flag + join handle.
fn fake_daemon_no_info(
    state: &Path,
    agents: Value,
) -> (
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread::JoinHandle<()>,
) {
    use std::io::BufRead;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    std::fs::create_dir_all(state).unwrap();
    let listener = UnixListener::bind(state.join("cadence.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let handle = thread::spawn(move || {
        while !flag.load(Ordering::Relaxed) {
            let Ok((mut conn, _)) = listener.accept() else {
                thread::sleep(Duration::from_millis(5));
                continue;
            };
            let mut line = String::new();
            std::io::BufReader::new(&conn)
                .read_line(&mut line)
                .unwrap_or_default();
            let method = serde_json::from_str::<Value>(&line)
                .ok()
                .and_then(|r| r["method"].as_str().map(str::to_string))
                .unwrap_or_default();
            let result = match method.as_str() {
                "health" => json!({"ok": true, "result": {"pid": 1}}),
                "daemon_info" => json!({
                    "ok": false,
                    "error": {"kind": "rejected", "message": "Unknown method 'daemon_info'"}
                }),
                "agent_list" => {
                    json!({"ok": true, "result": {"agents": agents.clone()}})
                }
                "agent_show" => json!({"ok": true, "result": {"messages": [], "queued": 0}}),
                "agent_requests" => json!({"ok": true, "result": {"requests": []}}),
                "agent_probe" => json!({"ok": true, "result": {"idle": true}}),
                _ => json!({
                    "ok": false,
                    "error": {"kind": "rejected", "message": "Unknown method"}
                }),
            };
            let _ = writeln!(conn, "{result}");
        }
    });
    (stop, handle)
}

#[test]
fn overview_daemon_without_daemon_info_stays_reachable() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    // A fenced worker agent — reachable daemon rows must still appear
    // even though `daemon_info` is unknown to this old build.
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        - 300.0;
    let agents = json!([{
        "alias": "w1", "state": "attention", "provider": "devin",
        "endpoint_kind": "ws", "updated": epoch
    }]);
    let (stop, handle) = fake_daemon_no_info(state.path(), agents);
    let (ok, v) = cli(pm.path(), state.path(), &["overview", "--json"]);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = handle.join();
    assert!(ok, "{v}");
    assert_eq!(v["daemon"]["reachable"], true, "{v}");
    let needs = v["needs_me"].as_array().unwrap();
    let fenced = needs
        .iter()
        .find(|n| n["kind"] == "fenced")
        .expect("fenced row from a reachable old daemon");
    assert_eq!(fenced["command"], "cadence agent unfence w1");
    // Build identity unreadable → drift explains instead of guessing.
    let reason = v["drift"]["reason"].as_str().unwrap_or("");
    assert!(reason.contains("predates daemon_info"), "{v}");
    assert_eq!(v["drift"]["matched"], false, "{v}");
}

#[test]
fn overview_review_suppressed_by_branch_match() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let repo = repo_with_remote("https://github.com/acme/widgets.git");
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    let repo_s = repo.path().to_str().unwrap().to_string();
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "project", "add", "cadence", "--prefix", "CAD", "--repo", &repo_s]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "new", "review me", "--project", "cadence"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "set", "CAD-1", "status=review"]
        )
        .0
    );
    // CAD-1 has no `pr` ref — but an open PR on `cadence/cad-1-…`
    // branch counts as its PR.
    let gh = fake_gh();
    let prs = format!(
        r#"[{{"number": 9, "title": "cad-1 work",
           "url": "https://github.com/acme/widgets/pull/9",
           "headRefOid": "def", "headRefName": "cadence/cad-1-review",
           "updatedAt": "{}", "statusCheckRollup": []}}]"#,
        iso_ago(300)
    );
    let path = gh_path(&gh);
    let log = gh.log.to_str().unwrap().to_string();
    let (ok, v) = cli_env(
        pm.path(),
        state.path(),
        &["overview", "--json"],
        &[
            ("PATH", path.as_str()),
            ("FAKE_GH_LOG", log.as_str()),
            ("FAKE_GH_PRS", prs.as_str()),
        ],
    );
    assert!(ok, "{v}");
    let needs = v["needs_me"].as_array().unwrap();
    assert!(
        !needs.iter().any(|n| n["kind"] == "review_no_pr"),
        "{needs:?}"
    );
}

#[test]
fn overview_empty_slug_set_keeps_cached_rows() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let repo = repo_with_remote("https://github.com/acme/widgets.git");
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    let repo_s = repo.path().to_str().unwrap().to_string();
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "project", "add", "cadence", "--prefix", "CAD", "--repo", &repo_s]
        )
        .0
    );
    // Run 1: a good fetch writes the slug-keyed cache.
    let gh = fake_gh();
    let prs = format!(
        r#"[{{"number": 9, "title": "wip thing",
           "url": "https://github.com/acme/widgets/pull/9",
           "headRefOid": "def", "headRefName": "wip",
           "updatedAt": "{}", "statusCheckRollup": []}}]"#,
        iso_ago(300)
    );
    let path = gh_path(&gh);
    let log = gh.log.to_str().unwrap().to_string();
    let (ok, v) = cli_env(
        pm.path(),
        state.path(),
        &["overview", "--json"],
        &[
            ("PATH", path.as_str()),
            ("FAKE_GH_LOG", log.as_str()),
            ("FAKE_GH_PRS", prs.as_str()),
        ],
    );
    assert!(ok, "{v}");
    assert_eq!(v["github"]["state"], "ok");
    let cache = state.path().join("overview-gh.json");
    let body: Value = serde_json::from_str(&std::fs::read_to_string(&cache).unwrap()).unwrap();
    assert_eq!(body["slugs"], json!(["acme/widgets"]), "{body}");
    // Run 2: a tracker with no remotes must not stamp over the cache.
    let empty_pm = TempDir::new().unwrap();
    assert!(cli(empty_pm.path(), state.path(), &["issue", "init"]).0);
    let (ok, v) = cli_env(
        empty_pm.path(),
        state.path(),
        &["overview", "--json"],
        &[("PATH", path.as_str()), ("FAKE_GH_LOG", log.as_str())],
    );
    assert!(ok, "{v}");
    assert_eq!(v["github"]["state"], "ok", "{v}");
    let body2: Value = serde_json::from_str(&std::fs::read_to_string(&cache).unwrap()).unwrap();
    assert_eq!(body2["slugs"], json!(["acme/widgets"]), "{body2}");
    assert!(body2["repos"]["acme/widgets"]["prs"].is_array(), "{body2}");
}
