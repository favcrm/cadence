//! Independent CAD-1751 refusal acceptance for the delivery evidence path.
//!
//! Written by the reviewer from the ticket and the operator decision on
//! CAD-1751 (comment 20261022T163855Z), not by the implementer. The board's
//! polling delivery sync (`ui::delivery_sync` -> [`crate::delivery::sync_pr`])
//! now reads GitHub through the rate-limit pause. The bad case is a paused
//! sync that still produces, refreshes or upgrades delivery evidence.
//!
//! | case | real guard |
//! |---|---|
//! | a rate-limit reply from `gh` | `gh_guard::run` records the pause in the bound state dir; no observation reaches the daemon |
//! | paused, while GitHub would now say green and merged | `sync_pr` spawns no `gh`, sends no `delivery_observe` (so no head, no CI-green, no merged), and the stored not-green observation is neither replaced nor refreshed |
//! | a pause file alone, with no reply in this process | the same refusal, read from the file |
//! | the operator's gate read (`delivery::pr_view`, used by `rpc_delivery_approve`) while paused | reads live every time, or fails — never served from a cache |
//!
//! The daemon end is a recording socket at `<state>/cadence.sock`: the
//! daemon can only learn delivery evidence from the sync through
//! `delivery_observe`, so an observation that never arrives cannot mark
//! anything fresh or green. A control call with no pause proves the
//! socket does record a real observation.
//!
//! The bound state dir is process-wide, and other tests in this binary
//! bind their own. The check therefore re-runs itself in a child process
//! of the same test binary, where nothing else binds.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

const CHILD_ENV: &str = "CADENCE_CAD1751_REFUSAL_CHILD";
const URL: &str = "https://github.com/acme/widgets/pull/1751";
const SLUG: &str = "acme/widgets";
const NUMBER: u64 = 1751;
const RED_HEAD: &str = "1751aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const GREEN_HEAD: &str = "1751bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
/// What `gh` prints when the GraphQL quota is gone (copied from a real
/// reply on this host, 2026-10-22), with `gh`'s real exit status 1.
const LIMIT_STDERR: &str = "GraphQL: API rate limit already exceeded for user ID 26186634.";
const OUTAGE_STDERR: &str = "HTTP 502: Bad Gateway (https://api.github.com/graphql)";

fn now() -> i64 {
    crate::issue::time::now_epoch()
}

fn pr_json(head: &str, state: &str, conclusion: &str) -> String {
    json!({
        "headRefOid": head,
        "baseRefOid": "1751cccccccccccccccccccccccccccccccccccc",
        "baseRefName": "main",
        "state": state,
        "statusCheckRollup": [
            {"__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": conclusion}
        ],
        "additions": 3,
        "deletions": 1,
        "changedFiles": 1,
        "autoMergeRequest": null,
    })
    .to_string()
}

/// A scripted `gh` with real exit codes and stderr. `<dir>/gh-mode` picks
/// the answer: `red` / `green` print the PR, `limit` is the rate-limit
/// refusal, `fail` an outage. Every invocation is logged to `<dir>/gh-calls`.
fn fake_gh(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(
        dir.join("gh-red.json"),
        pr_json(RED_HEAD, "OPEN", "FAILURE"),
    )
    .unwrap();
    std::fs::write(
        dir.join("gh-green.json"),
        pr_json(GREEN_HEAD, "MERGED", "SUCCESS"),
    )
    .unwrap();
    let reset = now() + 900;
    let script = format!(
        "#!/bin/sh\n\
         d=\"{dir}\"\n\
         echo \"$*\" >> \"$d/gh-calls\"\n\
         if [ \"$*\" = \"api rate_limit\" ]; then\n\
         \x20 echo '{{\"resources\":{{\"core\":{{\"remaining\":4000,\"reset\":{reset}}},\"graphql\":{{\"remaining\":0,\"reset\":{reset}}}}}}}'\n\
         \x20 exit 0\n\
         fi\n\
         mode=$(cat \"$d/gh-mode\")\n\
         if [ \"$mode\" = limit ]; then echo '{LIMIT_STDERR}' >&2; exit 1; fi\n\
         if [ \"$mode\" = fail ]; then echo '{OUTAGE_STDERR}' >&2; exit 1; fi\n\
         cat \"$d/gh-$mode.json\"\n",
        dir = dir.display(),
    );
    let gh = bin.join("gh");
    std::fs::write(&gh, script).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    gh
}

fn set_mode(dir: &Path, mode: &str) {
    std::fs::write(dir.join("gh-mode"), mode).unwrap();
}

fn gh_calls(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("gh-calls"))
        .unwrap_or_default()
        .lines()
        .count()
}

/// A stand-in daemon on `<state>/cadence.sock` that records every request
/// and answers `ok`.
fn recording_daemon(state_dir: &Path) -> Arc<Mutex<Vec<Value>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let listener = UnixListener::bind(crate::client::socket_path(state_dir)).unwrap();
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).is_err() {
                continue;
            }
            let request: Value = serde_json::from_str(&line).unwrap_or(json!({"raw": line}));
            log.lock().unwrap().push(request);
            let _ = writeln!(stream, "{}", crate::proto::ok(json!({})));
        }
    });
    seen
}

fn observations(seen: &Arc<Mutex<Vec<Value>>>) -> Vec<Value> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|r| r["method"] == "delivery_observe")
        .map(|r| r["params"].clone())
        .collect()
}

#[test]
fn cad1751_paused_delivery_sync_records_no_evidence_and_gate_read_stays_live() {
    if std::env::var_os(CHILD_ENV).is_none() {
        let name = format!(
            "{}::cad1751_paused_delivery_sync_records_no_evidence_and_gate_read_stays_live",
            module_path!().split_once("::").map_or("", |(_, rest)| rest)
        );
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                name.as_str(),
                "--exact",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(CHILD_ENV, "1")
            .env_remove("CADENCE_SOCKET")
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "child check failed:\n{text}");
        assert!(
            text.contains("1 passed"),
            "child did not run the check:\n{text}"
        );
        return;
    }
    paused_sync_is_refused();
}

fn paused_sync_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let gh = fake_gh(root.path());
    let gh_bin = gh.to_str().unwrap();
    let daemon = recording_daemon(&state);
    crate::gh_guard::bind(&state);

    // Control: with no pause the sync reads GitHub and reports a not-green
    // observation — the recorder sees real evidence when there is some.
    set_mode(root.path(), "red");
    let row = crate::delivery::sync_pr(&state, "CAD-1751", URL, gh_bin);
    assert!(row.get("error").is_none(), "control sync failed: {row}");
    let stored = observations(&daemon);
    assert_eq!(stored.len(), 1, "control observation: {stored:?}");
    assert_eq!(stored[0]["head"], RED_HEAD);
    assert_eq!(stored[0]["ci_green"], false);
    assert_eq!(stored[0]["pr_state"], "OPEN");
    assert_eq!(gh_calls(root.path()), 1);
    assert!(crate::gh_guard::paused_until(&state).is_none());

    // GitHub answers with its rate limit: the sync fails, records no
    // evidence, and the pause is written in the bound state dir.
    set_mode(root.path(), "limit");
    let row = crate::delivery::sync_pr(&state, "CAD-1751", URL, gh_bin);
    let err = row["error"].as_str().unwrap_or_default();
    assert!(err.contains("rate limit already exceeded"), "{row}");
    for key in ["head", "ci_green", "pr_state", "auto_merge_disabled"] {
        assert!(row.get(key).is_none(), "{key} on a refused sync: {row}");
    }
    let until = crate::gh_guard::paused_until(&state).expect("the limit reply pauses gh");
    assert!(until > now() + 600, "pause runs to GitHub's reset: {until}");
    assert!(state.join("gh-rate-limit.json").is_file());
    assert_eq!(
        observations(&daemon).len(),
        1,
        "no observation on a limit reply"
    );
    let spent = gh_calls(root.path());

    // Paused, while GitHub would now report a new head, green CI and a
    // merge: every sync is refused, spawns no gh, sends no observation.
    set_mode(root.path(), "green");
    for _ in 0..10 {
        let row = crate::delivery::sync_pr(&state, "CAD-1751", URL, gh_bin);
        let err = row["error"].as_str().unwrap_or_default();
        assert!(err.contains("paused until"), "{row}");
        for key in ["head", "ci_green", "pr_state", "auto_merge_disabled"] {
            assert!(row.get(key).is_none(), "{key} while paused: {row}");
        }
    }
    assert_eq!(
        gh_calls(root.path()),
        spent,
        "a gh process ran while paused"
    );
    let after = observations(&daemon);
    assert_eq!(
        after, stored,
        "the stored not-green observation was replaced or refreshed while paused"
    );
    assert_eq!(after[0]["ci_green"], false, "not-green stays not-green");
    assert_eq!(
        after[0]["read_at"], stored[0]["read_at"],
        "not marked fresh"
    );

    // A pause file alone (another process hit the limit) refuses the same way.
    let other = root.path().join("other-state");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(
        other.join("gh-rate-limit.json"),
        json!({"until": now() + 600}).to_string(),
    )
    .unwrap();
    let other_daemon = recording_daemon(&other);
    crate::gh_guard::bind(&other);
    let row = crate::delivery::sync_pr(&other, "CAD-1751", URL, gh_bin);
    assert!(
        row["error"]
            .as_str()
            .is_some_and(|e| e.contains("paused until")),
        "{row}"
    );
    assert_eq!(
        gh_calls(root.path()),
        spent,
        "a gh process ran on a pause file"
    );
    assert!(observations(&other_daemon).is_empty());

    // The gate read the approve path uses is not the polling read: with the
    // pause in force it reads GitHub live each time, or fails. It never
    // answers from an earlier read.
    crate::gh_guard::bind(&state);
    assert!(crate::gh_guard::paused_until(&state).is_some());
    let live = crate::delivery::pr_view(gh_bin, SLUG, NUMBER).expect("live gate read");
    assert_eq!(live["headRefOid"], GREEN_HEAD);
    assert_eq!(live["state"], "MERGED");
    assert_eq!(gh_calls(root.path()), spent + 1, "the gate read ran gh");

    set_mode(root.path(), "fail");
    let failed = crate::delivery::pr_view(gh_bin, SLUG, NUMBER)
        .expect_err("an outage must fail the gate read, not serve the last answer");
    assert!(failed.to_string().contains("HTTP 502"), "{failed}");
    assert_eq!(gh_calls(root.path()), spent + 2);

    set_mode(root.path(), "red");
    let again = crate::delivery::pr_view(gh_bin, SLUG, NUMBER).expect("live gate read");
    assert_eq!(
        again["headRefOid"], RED_HEAD,
        "the gate read is live, not cached"
    );
    assert_eq!(gh_calls(root.path()), spent + 3);
    assert_eq!(
        observations(&daemon),
        stored,
        "the gate read reports nothing"
    );
}
