//! CAD-2212 signed-board acceptance: a private project's apps and
//! workflows are served to nobody its policy does not admit, and an
//! unknown-project answer names no project.
//! Signed-board HTTP only; this does not prove local-agent isolation.

use super::*;

/// GET that returns the status and body instead of asserting 200.
fn get_status(rig: &Rig, cookie: &str, path: &str) -> (u16, String) {
    let url = format!("http://127.0.0.1:{}{path}", rig.board_port);
    let mut response = http_agent()
        .get(&url)
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("Cookie", cookie)
        .call()
        .expect("board GET failed");
    let status = response.status().as_u16();
    let body = response.body_mut().read_to_string().unwrap_or_default();
    (status, body)
}

fn put_workflow(rig: &Rig, project: &str, name: &str, title: &str) {
    let dir = rig.pm.join(project).join("workflows");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{name}.md")),
        format!(
            "---\ntitle: {title}\ngoal: {title} goal\n---\n\nWhy.\n\n## Work\nagent: acceptance-worker\nsize: S\n\nDo {title}.\n\n### Acceptance\n- [ ] {title} done\n"
        ),
    )
    .unwrap();
}

/// An entry under `apps/` that `app ls` reports as a row naming its
/// project, without needing a real install.
fn put_app_row(rig: &Rig, project: &str, name: &str) {
    let dir = rig.pm.join(project).join("apps");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), "not an app").unwrap();
}

/// A signed hosted-CLI member envelope with read and write scope.
fn member_cli_envelope(rig: &Rig) -> String {
    let now = crate::issue::time::now_epoch();
    let sub = "cad2212-cli-member";
    let header = json!({"alg": "EdDSA", "typ": "agenticos-wiki-actor/1", "kid": KID});
    let claims = json!({
        "iss": rig.issuer,
        "aud": format!("http://{}", rig.public_host),
        "sub": sub,
        "actor": format!("users/{sub}"),
        "principal_kind": "user",
        "role_at_issue": "member",
        "organization_id": ORG,
        "scope": ["cli.read", "cli.write"],
        "credential_id": format!("cad2212-{sub}"),
        "iat": now,
        "exp": now + 120,
        "jti": format!("cad2212-cli-{}", uuid::Uuid::new_v4()),
    });
    let signed = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap()),
    );
    format!(
        "{signed}.{}",
        URL_SAFE_NO_PAD.encode(rig.key.sign(signed.as_bytes()).as_ref())
    )
}

#[test]
fn hosted_cli_member_unknown_project_names_no_project() {
    let rig = start(&[]);
    seed(&rig);
    let call = |project: &str| {
        let url = format!("http://127.0.0.1:{}/api/cli/issue_new", rig.board_port);
        let mut response = http_agent()
            .post(&url)
            .header("Host", &rig.public_host)
            .header("Content-Type", "application/json")
            .header(
                "Authorization",
                &format!("Bearer wikienv_{}", member_cli_envelope(&rig)),
            )
            .send_json(json!({"project": project, "title": "CAD2212 probe"}))
            .expect("hosted CLI request failed");
        let status = response.status().as_u16();
        (
            status,
            response.body_mut().read_to_string().unwrap_or_default(),
        )
    };
    let (unknown_status, unknown_body) = call("nosuch");
    let (secret_status, secret_body) = call("secret");
    assert_eq!(
        unknown_status, 404,
        "member unknown project: {unknown_body}"
    );
    assert_eq!(secret_status, 404, "member private project: {secret_body}");
    assert_eq!(
        unknown_body, secret_body,
        "a private project answers differently from an unknown one"
    );
    for marker in ["secret", "visible", "known projects"] {
        assert!(
            !unknown_body.contains(marker),
            "unknown-project answer disclosed {marker}: {unknown_body}"
        );
    }
    assert!(
        !rig.pm.join("secret").read_dir().unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("SEC-2")),
        "a refused member write created an issue"
    );
}

#[test]
fn signed_member_gets_no_private_apps_or_workflows_and_no_project_names() {
    let _flag = AssistantFlagOn::arm();
    let (rig, _events) = queue_rig(&["ben"]);
    let _keys = initialize_queue_keyring(&rig);
    seed(&rig);
    put_workflow(
        &rig,
        "visible",
        "cad2212-visible",
        "CAD2212_VISIBLE_WORKFLOW",
    );
    put_workflow(&rig, "secret", "cad2212-secret", "CAD2212_PRIVATE_WORKFLOW");
    put_app_row(&rig, "visible", "cad2212-visible-stray");
    put_app_row(&rig, "secret", "cad2212-secret-stray");

    let ben = login(&rig, "ben", "member");
    let owner = login(&rig, "cad2212-owner", "owner");

    // The member reads the project they are admitted to.
    let (status, body) = get_status(&rig, &ben, "/api/projects/visible/workflows");
    assert_eq!(
        status, 200,
        "member lost a visible project's workflows: {body}"
    );
    assert!(
        body.contains("CAD2212_VISIBLE_WORKFLOW"),
        "visible workflow missing: {body}"
    );
    let (status, body) = get_status(&rig, &ben, "/api/apps?project=visible");
    assert_eq!(status, 200, "member lost a visible project's apps: {body}");
    assert!(
        body.contains("cad2212-visible-stray"),
        "visible app row missing: {body}"
    );

    // A restricted project answers exactly like one that does not exist,
    // and neither answer names any project.
    let (unknown_status, unknown_body) = get_status(&rig, &ben, "/api/projects/nosuch/workflows");
    assert_eq!(
        unknown_status, 404,
        "unknown project not 404: {unknown_body}"
    );
    for path in [
        "/api/projects/secret/workflows",
        "/api/projects/secret/workflows/",
        "/api/projects/secret/workflows/cad2212-secret/preview",
        "/api/projects/nosuch/workflows",
        "/api/projects/nosuch/workflows/x/preview",
        "/api/apps?project=secret",
        "/api/apps?project=nosuch",
        "/api/apps/secret/cad2212-secret-stray",
        "/api/apps/secret/cad2212-secret-stray/runs",
        "/api/apps/secret/cad2212-secret-stray/outputs",
        "/api/apps/nosuch/x",
        "/api/apps/nosuch/x/runs",
    ] {
        let (status, body) = get_status(&rig, &ben, path);
        assert_eq!(
            status, 404,
            "member {path}: expected 404, got {status}: {body}"
        );
        for marker in [
            "CAD2212_PRIVATE_WORKFLOW",
            "cad2212-secret-stray",
            "secret",
            "known projects",
        ] {
            // The path itself names `secret`; only the body is checked.
            assert!(
                !body.contains(marker),
                "member {path} disclosed {marker}: {body}"
            );
        }
    }
    let (_, secret_list) = get_status(&rig, &ben, "/api/projects/secret/workflows");
    assert_eq!(
        secret_list, unknown_body,
        "a restricted project answers differently from an unknown one"
    );

    // The all-projects app listing drops the restricted project's rows and
    // counts only what it serves.
    let (status, body) = get_status(&rig, &ben, "/api/apps");
    assert_eq!(status, 200, "member app listing failed: {body}");
    let listed: Value = serde_json::from_str(&body).unwrap();
    let rows = listed["apps"].as_array().expect("apps array");
    assert!(
        rows.iter().all(|r| r["project"].as_str() != Some("secret")),
        "member app listing included a restricted row: {body}"
    );
    assert!(
        rows.iter()
            .any(|r| r["project"].as_str() == Some("visible")),
        "member app listing lost the visible row: {body}"
    );
    assert_eq!(
        listed["count"],
        json!(rows.len()),
        "count disagrees with rows: {body}"
    );

    // The owner keeps administrative access to both.
    let (status, body) = get_status(&rig, &owner, "/api/projects/secret/workflows");
    assert_eq!(status, 200, "owner lost private workflows: {body}");
    assert!(
        body.contains("CAD2212_PRIVATE_WORKFLOW"),
        "owner private workflow missing"
    );
    let (status, body) = get_status(&rig, &owner, "/api/apps");
    assert_eq!(status, 200, "owner app listing failed: {body}");
    assert!(
        body.contains("cad2212-secret-stray"),
        "owner lost private app row: {body}"
    );
}
