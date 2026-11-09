//! CAD-2215 signed-board wiki memory project-policy acceptance.
//! Lessons, private policy and FTS shadow documents are synthetic fixture data;
//! this does not enable private mode or establish local-agent OS isolation.
use super::*;

fn memory_get(rig: &Rig, cookie: &str, path: &str) -> (u16, Value) {
    let mut response = http_agent()
        .get(&format!("http://127.0.0.1:{}{path}", rig.board_port))
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("Cookie", cookie)
        .call()
        .expect("wiki GET failed");
    let status = response.status().as_u16();
    (
        status,
        response
            .body_mut()
            .read_json()
            .expect("wiki GET was not JSON"),
    )
}
fn memory_export(rig: &Rig, cookie: &str, path: &str) -> (u16, Value) {
    let mut response = http_agent()
        .post(&format!(
            "http://127.0.0.1:{}/api/wiki/job_status",
            rig.board_port
        ))
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("X-Cadence-Board", "1")
        .header("Cookie", cookie)
        .header("Content-Type", "application/json")
        .send_json(json!({"path":path,"transfer":{"op":"export","offset":0}}))
        .expect("wiki export request failed");
    let status = response.status().as_u16();
    (
        status,
        response
            .body_mut()
            .read_json()
            .expect("wiki export was not JSON"),
    )
}
fn lesson(id: &str, marker: &str) -> String {
    format!("---\nid: {id}\ntype: fact\nstatus: accepted\nscope:\n  project: true\nconfidence: high\ncreated: '2026-01-01T00:00:00Z'\nauthor: cad2215-worker\n---\n\n{marker} memoryneedle\n\n**Why:** fixture\n\n**How to apply:** fixture\n")
}
fn run_git(pm: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(pm)
        .args([
            "-c",
            "user.name=CAD2215 Fixture",
            "-c",
            "user.email=cad2215@example.test",
        ])
        .args(args)
        .output()
        .expect("git fixture command failed");
    assert!(
        output.status.success(),
        "git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn wiki_memory_views_require_the_resolved_project_for_each_caller() {
    let rig = start(&[]);
    seed(&rig);
    // Synthetic retained policy; no private-mode activation is attempted.
    std::fs::write(rig.pm.join("secret/project.yaml"),
        "key: secret\nprefix: SEC\npolicy:\n  version: 1\n  visibility: private\n  members:\n  - users/ana\n").unwrap();
    let pm = crate::issue::Pm::at(&rig.pm).unwrap();
    let vault = crate::wiki::vault_dir(&pm).unwrap();
    for (project, slug, marker) in [
        ("visible", "open-lesson", "CAD2215_MEMORY_OPEN"),
        ("secret", "secret-lesson", "CAD2215_MEMORY_PRIVATE"),
    ] {
        let body = lesson(slug, marker);
        let directory = rig.pm.join(project).join("memory");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(format!("{slug}.md")), &body).unwrap();
        // Wiki FTS indexes actual committed vault documents, not the virtual
        // memory store. A same-path synthetic shadow document exercises the
        // search candidate ACL; direct reads/exports still resolve the real
        // project memory file and do not use this shadow as their source.
        let shadow = vault.join("agents/cad2215-worker/memory").join(project);
        std::fs::create_dir_all(&shadow).unwrap();
        std::fs::write(shadow.join(format!("{slug}.md")), &body).unwrap();
    }
    assert!(crate::memory::find(&pm, Some("visible"), "open-lesson").is_ok());
    assert!(crate::memory::find(&pm, Some("secret"), "secret-lesson").is_ok());
    let relative_vault = vault
        .strip_prefix(&rig.pm)
        .expect("fixture wiki vault must be tracker-local");
    let shadow_path = relative_vault.join("agents/cad2215-worker/memory");
    let shadow_path = shadow_path.to_str().unwrap();
    run_git(&rig.pm, &["add", "--", shadow_path]);
    run_git(
        &rig.pm,
        &[
            "commit",
            "-m",
            "CAD2215 synthetic memory wiki search candidates",
            "--",
            shadow_path,
        ],
    );
    let permitted = login(&rig, "ana", "member");
    let other = login(&rig, "ben", "member");
    let owner = login(&rig, "cad2215-memory-owner", "owner");
    let base = "agents/cad2215-worker/memory";
    let open = format!("{base}/visible/open-lesson.md");
    let private = format!("{base}/secret/secret-lesson.md");

    for (who, cookie) in [("permitted", &permitted), ("operator", &owner)] {
        for (path, marker) in [
            (&open, "CAD2215_MEMORY_OPEN"),
            (&private, "CAD2215_MEMORY_PRIVATE"),
        ] {
            let (status, body) = memory_get(&rig, cookie, &format!("/api/wiki/file?path={path}"));
            assert_eq!(status, 200, "{who} direct read failed: {body}");
            assert!(
                body.to_string().contains(marker),
                "{who} direct read lost {marker}: {body}"
            );
            let (status, body) = memory_export(&rig, cookie, path);
            assert_eq!(status, 200, "{who} export failed: {body}");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(body["data_base64"].as_str().expect("export data"))
                .unwrap();
            assert!(
                String::from_utf8_lossy(&bytes).contains(marker),
                "{who} export lost {marker}"
            );
        }
        let (status, listing) = memory_get(&rig, cookie, &format!("/api/wiki/ls?path={base}"));
        assert_eq!(status, 200, "{who} listing failed: {listing}");
        assert!(
            listing.to_string().contains("visible") && listing.to_string().contains("secret"),
            "{who} lost project listing: {listing}"
        );
        let (status, hits) = memory_get(
            &rig,
            cookie,
            "/api/wiki/search?q=memoryneedle&path=agents/cad2215-worker/memory",
        );
        assert_eq!(status, 200, "{who} search failed: {hits}");
        let text = hits.to_string();
        assert!(
            text.contains("open-lesson") && text.contains("secret-lesson"),
            "{who} lost indexed candidates: {hits}"
        );
    }
    let (status, body) = memory_get(&rig, &other, &format!("/api/wiki/file?path={open}"));
    assert_eq!(
        status, 200,
        "other member lost workspace-visible lesson: {body}"
    );
    assert!(body.to_string().contains("CAD2215_MEMORY_OPEN"));
    let (status, body) = memory_get(&rig, &other, &format!("/api/wiki/file?path={private}"));
    assert!(
        matches!(status, 400 | 401 | 403 | 404),
        "private memory file admitted: {status} {body}"
    );
    assert!(!body.to_string().contains("CAD2215_MEMORY_PRIVATE"));
    let (status, listing) = memory_get(&rig, &other, &format!("/api/wiki/ls?path={base}"));
    assert_eq!(status, 200, "other member lost memory listing: {listing}");
    assert!(listing.to_string().contains("visible"));
    assert!(
        !listing.to_string().contains("secret"),
        "memory listing disclosed private project: {listing}"
    );
    let (status, hits) = memory_get(
        &rig,
        &other,
        "/api/wiki/search?q=memoryneedle&path=agents/cad2215-worker/memory",
    );
    assert_eq!(status, 200, "other member lost search: {hits}");
    assert!(
        hits.to_string().contains("open-lesson"),
        "visible search candidate missing: {hits}"
    );
    assert!(
        !hits.to_string().contains("secret-lesson")
            && !hits.to_string().contains("CAD2215_MEMORY_PRIVATE"),
        "search leaked private candidate: {hits}"
    );
    let (status, open_export) = memory_export(&rig, &other, &open);
    assert_eq!(
        status, 200,
        "other member lost visible export: {open_export}"
    );
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(open_export["data_base64"].as_str().unwrap())
        .unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("CAD2215_MEMORY_OPEN"));
    let (status, private_export) = memory_export(&rig, &other, &private);
    assert!(
        matches!(status, 400 | 401 | 403 | 404),
        "private export admitted: {status} {private_export}"
    );
    assert!(
        private_export.get("data_base64").is_none(),
        "private export returned bytes: {private_export}"
    );
    let (status, listing_export) = memory_export(&rig, &other, base);
    assert_eq!(
        status, 200,
        "visible export listing refused: {listing_export}"
    );
    assert!(listing_export.to_string().contains("visible"));
    assert!(
        !listing_export.to_string().contains("secret"),
        "export listing leaked private project: {listing_export}"
    );
}
