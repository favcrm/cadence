//! CAD-1751: the overview's `gh` call rate stays inside its budget, and a
//! rate-limit reply from `gh` stops every call until GitHub's reset.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{json, Value};

use super::github_cache::{
    cache_file, github_throttled_with, read_cache, refresh_floor_secs, reset_repo_memo,
    write_cache, GH_BOARD_BUDGET_PER_HOUR, GH_CALLS_PER_REPO,
};
use super::now_epoch;

/// Both checks share the process-wide repo memo and fetch counter.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
static FETCHES: AtomicUsize = AtomicUsize::new(0);
static ONE_REPO_FAILS: AtomicBool = AtomicBool::new(false);

fn counting(slug: &str) -> Result<Value, String> {
    FETCHES.fetch_add(1, Ordering::SeqCst);
    if slug == "acme/repo07" && ONE_REPO_FAILS.load(Ordering::SeqCst) {
        return Err("gh: Not Found (HTTP 404)".to_string());
    }
    Ok(json!({"prs": []}))
}

fn expire(dir: &Path, slugs: &[String]) {
    write_cache(
        &cache_file(dir),
        slugs,
        &HashMap::new(),
        now_epoch() - 10_000,
    );
}

fn fake_gh(dir: &Path) -> std::path::PathBuf {
    let gh = dir.join("gh");
    let reset = now_epoch() + 600;
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\necho \"$*\" >> {log}\nif [ \"$*\" = \"api rate_limit\" ]; then\n  echo '{{\"resources\":{{\"core\":{{\"remaining\":0,\"reset\":{reset}}}}}}}'; exit 0\nfi\necho 'gh: API rate limit exceeded for user ID 1. (HTTP 403)' >&2\nexit 1\n",
            log = dir.join("gh-calls").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    gh
}

#[test]
fn cad1751_overview_gh_rate_is_bounded_and_backs_off_at_the_limit() {
    let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let slugs: Vec<String> = (1..=15).map(|n| format!("acme/repo{n:02}")).collect();
    let wait = Duration::from_secs(5);
    reset_repo_memo();

    // Any number of tabs inside one refresh window cost one refresh.
    for _ in 0..300 {
        github_throttled_with(dir.path(), &slugs, wait, 60, counting);
    }
    assert_eq!(FETCHES.swap(0, Ordering::SeqCst), 15, "one fetch per repo");

    // The floor keeps a refresh every `floor` seconds inside the budget:
    // 2 gh calls per repo per refresh, 15 repos, at most 1,000 an hour.
    let floor = refresh_floor_secs(slugs.len());
    let per_hour = slugs.len() as i64 * GH_CALLS_PER_REPO * 3600 / floor;
    assert!(
        per_hour <= GH_BOARD_BUDGET_PER_HOUR,
        "{per_hour} gh calls/h"
    );

    // One repo that always fails keeps the cache stale, so every request
    // refreshes — but only the failing repo's first read spawns anything.
    ONE_REPO_FAILS.store(true, Ordering::SeqCst);
    reset_repo_memo();
    expire(dir.path(), &slugs);
    for _ in 0..100 {
        let (_, state) = github_throttled_with(dir.path(), &slugs, wait, 60, counting);
        assert_eq!(state["state"], "stale", "{state}");
    }
    assert_eq!(
        FETCHES.swap(0, Ordering::SeqCst),
        15,
        "failing repo backed off"
    );

    // A rate-limit reply from `gh` pauses every later call until the reset,
    // read from gh's exit and stderr.
    let gh = fake_gh(dir.path());
    let calls = || {
        std::fs::read_to_string(dir.path().join("gh-calls"))
            .unwrap_or_default()
            .lines()
            .count()
    };
    let first = crate::gh_guard::run_in(
        Some(dir.path()),
        Command::new(&gh).args(["pr", "list"]),
        Duration::from_secs(5),
    )
    .expect("the first call runs");
    assert!(!first.status.success());
    let until = crate::gh_guard::paused_until(dir.path()).expect("paused after the reply");
    assert!(until > now_epoch() + 500, "pause lasts until the reset");
    let spent = calls();
    for _ in 0..50 {
        let again = crate::gh_guard::run_in(
            Some(dir.path()),
            Command::new(&gh).args(["pr", "list"]),
            Duration::from_secs(5),
        );
        assert!(again.is_err_and(|e| e.contains("paused until")));
    }
    assert_eq!(calls(), spent, "no gh call while paused");

    // The board shows the pause as a degraded note and fetches nothing.
    reset_repo_memo();
    expire(dir.path(), &slugs);
    let (_, state) = github_throttled_with(dir.path(), &slugs, wait, 60, counting);
    assert_eq!(FETCHES.load(Ordering::SeqCst), 0, "no fetch while paused");
    assert!(
        state["error"]
            .as_str()
            .is_some_and(|e| e.contains("rate limit")),
        "{state}"
    );
    assert!(read_cache(&cache_file(dir.path())).is_some());
}

/// A board on an explicit state dir keeps its pause there: written there
/// when `gh` reports the limit, read from there, and not taken from (or
/// leaked into) the environment's or the default state dir.
#[test]
fn cad1751_pause_lives_in_the_boards_own_state_dir() {
    let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let slugs = vec!["acme/repo01".to_string()];
    let gh = fake_gh(dir.path());
    let fetch = move |_: &str| -> Result<Value, String> {
        let out = crate::gh_guard::run(
            Command::new(&gh).args(["pr", "list"]),
            Duration::from_secs(5),
        )?;
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    };
    let wait = Duration::from_secs(5);
    reset_repo_memo();
    github_throttled_with(dir.path(), &slugs, wait, 60, fetch.clone());
    assert!(
        crate::gh_guard::paused_until(dir.path()).is_some(),
        "the pause is written in the board's state dir"
    );
    if let Ok(env_dir) = crate::home::state_dir() {
        if env_dir != dir.path() {
            assert!(!env_dir.join("gh-rate-limit.json").exists());
        }
    }
    let spent = std::fs::read_to_string(dir.path().join("gh-calls"))
        .unwrap()
        .lines()
        .count();
    reset_repo_memo();
    expire(dir.path(), &slugs);
    let (_, state) = github_throttled_with(dir.path(), &slugs, wait, 60, fetch);
    assert!(
        state["error"]
            .as_str()
            .is_some_and(|e| e.contains("paused until")),
        "{state}"
    );
    let after = std::fs::read_to_string(dir.path().join("gh-calls"))
        .unwrap()
        .lines()
        .count();
    assert_eq!(after, spent, "no gh call while paused");
}

/// The delivery sync is a polling caller too: it stays inside the board's
/// budget with the overview, and while `gh` is paused it reads nothing and
/// reports an error row (so it records no evidence).
#[test]
fn cad1751_delivery_sync_is_bounded_and_stops_while_paused() {
    let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // Timer passes (60/h) plus page-view passes (4 per 10 min = 24/h).
    let sync_per_hour = crate::ui::delivery_sync::MAX_PER_PASS as i64 * (60 + 24);
    assert_eq!(sync_per_hour, 336);
    assert!(
        GH_BOARD_BUDGET_PER_HOUR + sync_per_hour <= 900,
        "daemon keeps the rest of 1,000"
    );

    let dir = tempfile::tempdir().unwrap();
    let gh = fake_gh(dir.path());
    crate::gh_guard::bind(dir.path());
    let url = "https://github.com/acme/repo01/pull/1";
    let first = crate::delivery::sync_pr(dir.path(), "CAD-1", url, gh.to_str().unwrap());
    assert!(first["error"].as_str().is_some(), "{first}");
    assert!(
        crate::gh_guard::paused_until(dir.path()).is_some(),
        "the reply paused gh"
    );
    let spent = std::fs::read_to_string(dir.path().join("gh-calls"))
        .unwrap()
        .lines()
        .count();
    for _ in 0..20 {
        let row = crate::delivery::sync_pr(dir.path(), "CAD-1", url, gh.to_str().unwrap());
        assert!(
            row["error"]
                .as_str()
                .is_some_and(|e| e.contains("paused until")),
            "{row}"
        );
        assert!(row.get("head").is_none(), "no evidence while paused");
    }
    let after = std::fs::read_to_string(dir.path().join("gh-calls"))
        .unwrap()
        .lines()
        .count();
    assert_eq!(after, spent, "no gh call while paused");
}
