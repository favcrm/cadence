//! Independent CAD-1751 daemon-cap refusal acceptance, cc13-rev-sol.
//! Install unchanged as src/gh_guard/cad1751_call_cap_refusal_acceptance.rs
//! and register in src/gh_guard.rs with:
//! #[cfg(test)] mod cad1751_call_cap_refusal_acceptance;
//!
//! Uses the real guard and the reachable polling entry, delivery::sync_pr.
//! No production visibility change or replacement runner is required.

use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn cad1751_exhausted_call_cap_spawns_nothing_and_polling_fails_closed() {
    const CHILD: &str = "CADENCE_CAD1751_CALL_CAP_PROOF_CHILD_V2";
    if std::env::var_os(CHILD).is_none() {
        // BOUND is process-global; never change another test's state.
        let exact = format!(
            "{}::cad1751_exhausted_call_cap_spawns_nothing_and_polling_fails_closed",
            module_path!().split_once("::").unwrap().1
        );
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &exact, "--nocapture", "--test-threads", "1"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "child failed: {stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stdout.contains("running 1 test"),
            "wrong child filter: {stdout}"
        );
        assert!(
            stdout.contains("cap-refusal-proof-complete"),
            "proof did not finish"
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let calls = root.path().join("calls");
    let gh = root.path().join("gh");
    // If polling escapes the cap, this executable both logs that process and
    // offers fresh green/merged evidence. Neither may happen at the limit.
    let pr = serde_json::json!({
        "headRefOid": "1751175117511751175117511751175117511751",
        "baseRefOid": "1751175117511751175117511751175117511752",
        "baseRefName": "main",
        "state": "MERGED",
        "statusCheckRollup": [{
            "__typename": "CheckRun", "name": "test",
            "status": "COMPLETED", "conclusion": "SUCCESS"
        }],
        "additions": 1, "deletions": 0, "changedFiles": 1,
        "autoMergeRequest": null
    });
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nprintf '%s\\n' '{pr}'\n",
            calls.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    bind(&state);
    let count = || {
        std::fs::read_to_string(&calls)
            .unwrap_or_default()
            .lines()
            .count()
    };
    with_call_cap(150, || {
        for _ in 0..150 {
            let out = run(&mut Command::new(&gh), Duration::from_secs(2)).unwrap();
            assert!(out.status.success());
            assert_eq!(
                serde_json::from_slice::<Value>(&out.stdout).unwrap()["state"],
                "MERGED"
            );
        }
        assert_eq!(count(), 150);
        assert!(run(&mut Command::new(&gh), Duration::from_secs(2))
            .unwrap_err()
            .contains("cap"));
        assert_eq!(count(), 150, "151st call spawned a process");
        let row = crate::delivery::sync_pr(
            &state,
            "CAP-1751",
            "https://github.com/acme/widget/pull/1751",
            gh.to_str().unwrap(),
        );
        assert!(row["error"].as_str().unwrap().contains("cap"), "{row}");
        assert!(
            row.get("head").is_none(),
            "cap refusal returned head evidence: {row}"
        );
        assert!(
            row.get("ci_green").is_none(),
            "cap refusal returned CI evidence: {row}"
        );
        assert!(
            row.get("pr_state").is_none(),
            "cap refusal returned PR state: {row}"
        );
        assert_eq!(count(), 150, "polling escaped the cap");
    });
    assert!(run(&mut Command::new(&gh), Duration::from_secs(2))
        .unwrap()
        .status
        .success());
    assert_eq!(count(), 151, "cap leaked beyond its scope");
    println!("cap-refusal-proof-complete");
}
