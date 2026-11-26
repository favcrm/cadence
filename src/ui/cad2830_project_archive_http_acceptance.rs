//! CAD-2830 signed-board acceptance for native project archive and restore:
//! 1. Only an operator/owner may archive or restore; members are refused with 403.
//! 2. Exact project-key confirmation and revision CAS are enforced.
//! 3. Archived projects disappear from `/api/projects`, `/api/issues`, `/api/overview`,
//!    and `/api/my-work`, appear under `?archived=1`, remain readable directly via
//!    `/api/issues/<id>`, and refuse mutations with `409 project_archived`.
//! 4. Restore re-enables writes and returns the project to active listings while
//!    preserving `archived_at` and `archived_by`.

use super::*;

const MEMBER: &str = "cad2830-member";
const OWNER: &str = "cad2830-owner";
const VISIBLE_TITLE: &str = "CAD2830_VISIBLE_MINE";

fn raw_get(rig: &Rig, cookie: &str, path: &str) -> (u16, String) {
    let mut response = http_agent()
        .get(&format!("http://127.0.0.1:{}{path}", rig.board_port))
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("Cookie", cookie)
        .call()
        .expect("board GET transport failed");
    let status = response.status().as_u16();
    (
        status,
        response.body_mut().read_to_string().unwrap_or_default(),
    )
}

fn send_json(rig: &Rig, method: &str, cookie: &str, path: &str, body: Value) -> (u16, String) {
    let url = format!("http://127.0.0.1:{}{path}", rig.board_port);
    let mut response = match method {
        "PATCH" => http_agent()
            .patch(&url)
            .header("Host", &rig.public_host)
            .header("Origin", &format!("http://{}", rig.public_host))
            .header("Sec-Fetch-Site", "same-origin")
            .header("X-Cadence-Board", "1")
            .header("Cookie", cookie)
            .header("Content-Type", "application/json")
            .send_json(body)
            .expect("board PATCH transport failed"),
        _ => http_agent()
            .post(&url)
            .header("Host", &rig.public_host)
            .header("Origin", &format!("http://{}", rig.public_host))
            .header("Sec-Fetch-Site", "same-origin")
            .header("X-Cadence-Board", "1")
            .header("Cookie", cookie)
            .header("Content-Type", "application/json")
            .send_json(body)
            .expect("board POST transport failed"),
    };
    let status = response.status().as_u16();
    (
        status,
        response.body_mut().read_to_string().unwrap_or_default(),
    )
}

fn run_git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git invocation failed");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn project_archive_hides_reads_refuses_mutations_and_restores_cleanly() {
    let rig = start(&[]);
    let _keys = initialize_queue_keyring(&rig);
    seed(&rig);
    let dir = rig.pm.join("visible").join("VIS-2");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("issue.md"),
        format!(
            "---\nid: VIS-2\ntitle: {VISIBLE_TITLE}\nstatus: backlog\npriority: P2\nowner: {MEMBER}\ncreated: '2026-01-01T00:00:00Z'\n---\n"
        ),
    )
    .unwrap();
    run_git(&rig.pm, &["add", "-A"]);
    run_git(
        &rig.pm,
        &[
            "-c",
            "user.name=cadence",
            "-c",
            "user.email=cadence@localhost",
            "commit",
            "-qm",
            "seed VIS-2",
        ],
    );

    let member = login(&rig, MEMBER, "member");
    let owner = login(&rig, OWNER, "owner");

    // Non-operator/member callers cannot read lifecycle or archive/restore.
    let (status, _) = raw_get(&rig, &member, "/api/projects/visible/lifecycle");
    assert_eq!(status, 403, "member must not read project lifecycle");
    let (status, _) = send_json(
        &rig,
        "POST",
        &member,
        "/api/projects/visible/archive",
        json!({"confirmation": "visible", "expected_revision": 0}),
    );
    assert_eq!(status, 403, "member must not archive");
    let (status, _) = send_json(
        &rig,
        "POST",
        &member,
        "/api/projects/visible/restore",
        json!({"confirmation": "visible", "expected_revision": 0}),
    );
    assert_eq!(status, 403, "member must not restore");

    // Exact confirmation is enforced.
    let (status, body) = send_json(
        &rig,
        "POST",
        &owner,
        "/api/projects/visible/archive",
        json!({"confirmation": "wrong", "expected_revision": 0}),
    );
    assert_eq!(status, 400, "wrong confirmation must return 400: {body}");
    let err: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(err["code"], "confirmation_mismatch");

    // Initial lifecycle read reports active fixture job blocker.
    let (status, body) = raw_get(&rig, &owner, "/api/projects/visible/lifecycle");
    assert_eq!(status, 200, "lifecycle read failed: {body}");
    let life: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(life["archived"], false);
    assert_eq!(life["revision"], 0);
    let blockers = life["blockers"].as_array().expect("blockers array");
    assert!(
        blockers.iter().any(|b| b
            .as_str()
            .unwrap_or_default()
            .contains("cad2215-visible-job")),
        "expected cad2215-visible-job blocker: {body}"
    );

    // Archive refuses while active work exists.
    let (status, body) = send_json(
        &rig,
        "POST",
        &owner,
        "/api/projects/visible/archive",
        json!({"confirmation": "visible", "expected_revision": 0}),
    );
    assert_eq!(status, 409, "active work should block archive: {body}");
    let err: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(err["code"], "blocked", "{body}");

    // Complete the seeded active job, verify blockers clear, and archive.
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    db.execute_batch(
        "UPDATE tasks SET state = 'done' WHERE job_id = 'cad2215-visible-job';
         UPDATE jobs SET state = 'done' WHERE id = 'cad2215-visible-job';",
    )
    .unwrap();

    let (status, body) = raw_get(&rig, &owner, "/api/projects/visible/lifecycle");
    assert_eq!(status, 200, "lifecycle re-read failed: {body}");
    let life: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        life["blockers"],
        json!([]),
        "expected blockers cleared: {body}"
    );

    // Archive succeeds with exact project key and matching revision.
    let (status, body) = send_json(
        &rig,
        "POST",
        &owner,
        "/api/projects/visible/archive",
        json!({"confirmation": "visible", "expected_revision": 0}),
    );
    assert_eq!(status, 200, "archive failed: {body}");
    let archived: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(archived["archived"], true);
    assert_eq!(archived["revision"], 1);
    let archived_at = archived["archived_at"]
        .as_str()
        .expect("archived_at")
        .to_string();
    let archived_by = archived["archived_by"]
        .as_str()
        .expect("archived_by")
        .to_string();

    // Hidden from default `/api/projects`, `/api/issues`, `/api/overview`, `/api/my-work`.
    let (status, active_projects) = raw_get(&rig, &owner, "/api/projects");
    assert_eq!(status, 200);
    assert!(
        !active_projects.contains("\"visible\""),
        "{active_projects}"
    );

    let (status, active_issues) = raw_get(&rig, &owner, "/api/issues");
    assert_eq!(status, 200);
    assert!(!active_issues.contains("VIS-1"), "{active_issues}");
    assert!(!active_issues.contains("VIS-2"), "{active_issues}");

    let (status, overview_body) = raw_get(&rig, &owner, "/api/overview");
    assert_eq!(status, 200);
    let overview_json: Value = serde_json::from_str(&overview_body).unwrap();
    let overview_keys: Vec<&str> = overview_json["projects"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|p| p["key"].as_str()).collect())
        .unwrap_or_default();
    assert!(!overview_keys.contains(&"visible"), "{overview_body}");

    let (status, my_work_body) = raw_get(&rig, &member, "/api/my-work");
    assert_eq!(status, 200);
    assert!(!my_work_body.contains("VIS-2"), "{my_work_body}");

    // Exposed via `?archived=1`, and direct issue reads continue working.
    let (status, archived_projects) = raw_get(&rig, &owner, "/api/projects?archived=1");
    assert_eq!(status, 200, "{archived_projects}");
    assert!(
        archived_projects.contains("\"archive_supported\": true"),
        "{archived_projects}"
    );
    assert!(
        archived_projects.contains("\"visible\""),
        "{archived_projects}"
    );

    let (status, archived_issues) = raw_get(&rig, &owner, "/api/issues?project=visible&archived=1");
    assert_eq!(status, 200, "{archived_issues}");
    assert!(archived_issues.contains("VIS-2"), "{archived_issues}");

    let (status, direct_issue) = raw_get(&rig, &owner, "/api/issues/VIS-2");
    assert_eq!(status, 200, "direct issue read failed: {direct_issue}");

    // Writes targeting the archived project refuse with 409 project_archived.
    for (method, path, payload) in [
        (
            "POST",
            "/api/issues",
            json!({"project": "visible", "title": "New on archived"}),
        ),
        ("PATCH", "/api/issues/VIS-2", json!({"priority": "P0"})),
        (
            "POST",
            "/api/issues/VIS-2/comments",
            json!({"body": "blocked comment"}),
        ),
        (
            "POST",
            "/api/projects/visible/policy",
            json!({"visibility": "workspace-visible", "members": []}),
        ),
    ] {
        let (status, body) = send_json(&rig, method, &owner, path, payload);
        assert_eq!(status, 409, "{method} {path} did not return 409: {body}");
        let err: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(err["code"], "project_archived", "{body}");
    }

    // Restore brings the project back and preserves archived_at / archived_by.
    let (status, body) = send_json(
        &rig,
        "POST",
        &owner,
        "/api/projects/visible/restore",
        json!({"confirmation": "visible", "expected_revision": 1}),
    );
    assert_eq!(status, 200, "restore failed: {body}");
    let restored: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(restored["archived"], false);
    assert_eq!(restored["revision"], 2);
    assert_eq!(restored["archived_at"], archived_at);
    assert_eq!(restored["archived_by"], archived_by);

    let (status, active_after) = raw_get(&rig, &owner, "/api/projects");
    assert_eq!(status, 200);
    assert!(active_after.contains("\"visible\""), "{active_after}");

    let (status, comment_after) = send_json(
        &rig,
        "POST",
        &owner,
        "/api/issues/VIS-2/comments",
        json!({"body": "restored comment"}),
    );
    assert_eq!(status, 200, "comment after restore failed: {comment_after}");
}
