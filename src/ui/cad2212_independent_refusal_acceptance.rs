//! CAD-2212 independently authored acceptance, written from the ticket text
//! and its listed surfaces only. The product implementer must not edit or
//! weaken this check.
//!
//! Rule under test: a caller the project's policy does not admit is served
//! nothing (not a row, not a count, not a title), and a refusal reads exactly
//! like an absent project. Signed-board HTTP only.

use super::*;

/// Raw GET that keeps refusals: `(status, body text)`.
fn raw_get(rig: &Rig, cookie: &str, path: &str) -> (u16, String) {
    let url = format!("http://127.0.0.1:{}{path}", rig.board_port);
    let mut response = http_agent()
        .get(&url)
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("Cookie", cookie)
        .call()
        .expect("board GET transport failed");
    let status = response.status().as_u16();
    let body = response.body_mut().read_to_string().unwrap_or_default();
    (status, body)
}

const WF_TITLE: &str = "CAD2212_SECRET_WORKFLOW_TITLE";
const WF_GOAL: &str = "CAD2212_SECRET_WORKFLOW_GOAL";
const WF_BODY: &str = "CAD2212_SECRET_PLAN_BODY";
const APP_TITLE: &str = "CAD2212_SECRET_APP_TITLE";
const APP: &str = "cad2212secretapp";
const WF: &str = "cad2212secretflow";

fn plant_private_fixtures(rig: &Rig) {
    let project = rig.pm.join("secret");
    std::fs::create_dir_all(project.join("workflows")).unwrap();
    std::fs::write(
        project.join("workflows").join(format!("{WF}.md")),
        format!("---\ntitle: {WF_TITLE}\ngoal: {WF_GOAL}\n---\n\n## Step\n\n{WF_BODY}\n"),
    )
    .unwrap();
    let apps = project.join("apps");
    let dir = apps.join(APP);
    std::fs::create_dir_all(dir.join("workflows")).unwrap();
    std::fs::write(
        dir.join("app.md"),
        format!("---\napp: {APP}\ntitle: {APP_TITLE}\nversion: 0.1.0\n---\n\n# Guide\n"),
    )
    .unwrap();
    std::fs::write(
        apps.join(format!("{APP}.yaml")),
        format!(
            "schema: 1\napp: {APP}\ninstall_id: inst-cad2212\n\
source:\n  kind: path\n  path: /tmp/src\n\
installed_at: '2026-09-27T00:00:00Z'\ninstalled_by: operator\n"
        ),
    )
    .unwrap();
}

#[test]
fn signed_board_serves_a_private_projects_apps_workflows_and_totals_to_no_member() {
    let _flag = AssistantFlagOn::arm();
    let (rig, _events) = queue_rig(&["ben"]);
    let _keys = initialize_queue_keyring(&rig);
    seed(&rig);
    plant_private_fixtures(&rig);
    let member = login(&rig, "ben", "member");
    let owner = login(&rig, "cad2212-owner", "owner");

    let markers = [
        WF_TITLE,
        WF_GOAL,
        WF_BODY,
        APP_TITLE,
        APP,
        WF,
        "SEC-1",
        "inst-cad2212",
    ];
    let per_project = |key: &str| {
        vec![
            format!("/api/projects/{key}/workflows"),
            format!("/api/projects/{key}/workflows/{WF}/preview"),
            format!("/api/apps/{key}/{APP}"),
            format!("/api/apps/{key}/{APP}/runs"),
            format!("/api/apps/{key}/{APP}/outputs"),
            format!("/api/apps?project={key}"),
        ]
    };

    // The owner can still recover the private content (control).
    let (s, owner_wf) = raw_get(&rig, &owner, &format!("/api/projects/secret/workflows"));
    assert_eq!(s, 200, "owner lost the private workflow list: {owner_wf}");
    assert!(owner_wf.contains(WF_TITLE), "owner cannot recover workflow");
    let (s, owner_apps) = raw_get(&rig, &owner, "/api/apps");
    assert_eq!(s, 200, "owner lost the app list");
    assert!(
        owner_apps.contains(APP),
        "owner cannot recover the private app: {owner_apps}"
    );

    // The member is refused every per-project surface for the private project,
    // in the same words and status as for a project that does not exist.
    for (secret_path, ghost_path) in per_project("secret")
        .into_iter()
        .zip(per_project("ghostproject"))
    {
        let (s_status, s_body) = raw_get(&rig, &member, &secret_path);
        let (g_status, g_body) = raw_get(&rig, &member, &ghost_path);
        for m in markers {
            assert!(
                !s_body.contains(m),
                "member {secret_path} leaked {m}: {s_body}"
            );
            assert!(
                !g_body.contains(m),
                "member {ghost_path} leaked {m}: {g_body}"
            );
        }
        assert_eq!(
            s_status, g_status,
            "{secret_path} refusal differs from an absent project"
        );
        assert_eq!(
            s_body.replace("secret", "ghostproject"),
            g_body,
            "{secret_path} body distinguishes a private project from an absent one"
        );
        // An absent project must not enumerate registered keys either.
        assert!(
            !g_body.contains("secret"),
            "absent-project answer names the private project: {g_body}"
        );
    }

    // Collections: no row for the private project in the member's app list.
    let (s, list) = raw_get(&rig, &member, "/api/apps");
    assert_eq!(s, 200, "member app list unavailable");
    for m in markers {
        assert!(!list.contains(m), "member app list leaked {m}: {list}");
    }

    // Workspace totals: the member counts only what their policy admits
    // (visible: VIS-1 and VIS-9), and is never handed the tracker path.
    let (s, health) = raw_get(&rig, &member, "/api/health");
    assert_eq!(s, 200);
    let health: Value = serde_json::from_str(&health).unwrap();
    assert!(
        health.get("pm_dir").is_none(),
        "member was handed the tracker path: {health}"
    );
    assert_eq!(health["projects"], 1, "member project total counts private");
    assert_eq!(health["issues"], 2, "member issue total counts private");
    let (_, owner_health) = raw_get(&rig, &owner, "/api/health");
    let owner_health: Value = serde_json::from_str(&owner_health).unwrap();
    assert_eq!(owner_health["projects"], 2, "owner lost the project total");
    assert_eq!(owner_health["issues"], 3, "owner lost the issue total");
}
