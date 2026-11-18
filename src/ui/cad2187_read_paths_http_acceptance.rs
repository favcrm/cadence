//! CAD-2187 signed-board proof that a member the project's policy does not
//! admit is served nothing on the board read and write paths that already
//! consult `project_policy`. Local fixture authority only: private policy is
//! stored by the fixture, and this does not enable private mode.

use super::*;
use std::collections::BTreeMap;

const MEMBER: &str = "cad2187-member";
const PRIVATE_TITLE: &str = "CAD2187_PRIVATE_MINE";
const VISIBLE_TITLE: &str = "CAD2187_VISIBLE_MINE";
const PRIVATE_MEMORY: &str = "CAD2187_PRIVATE_MEMORY";
const VISIBLE_MEMORY: &str = "CAD2187_VISIBLE_MEMORY";

fn raw(rig: &Rig, cookie: &str, path: &str) -> (u16, String) {
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

fn post_comment(rig: &Rig, cookie: &str, id: &str, text: &str) -> (u16, String) {
    let mut response = http_agent()
        .post(&format!(
            "http://127.0.0.1:{}/api/issues/{id}/comments",
            rig.board_port
        ))
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("X-Cadence-Board", "1")
        .header("Cookie", cookie)
        .header("Content-Type", "application/json")
        .send_json(json!({"body": text}))
        .expect("board comment transport failed");
    let status = response.status().as_u16();
    (
        status,
        response.body_mut().read_to_string().unwrap_or_default(),
    )
}

fn issue(rig: &Rig, project: &str, id: &str, title: &str, owner: &str) {
    let dir = rig.pm.join(project).join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("issue.md"),
        format!(
            "---\nid: {id}\ntitle: {title}\nstatus: backlog\npriority: P2\nowner: {owner}\ncreated: '2026-01-01T00:00:00Z'\n---\n"
        ),
    )
    .unwrap();
}

fn memory(rig: &Rig, project: &str, slug: &str, marker: &str) {
    let dir = rig.pm.join(project).join("memory");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{slug}.md")),
        format!("---\nid: {slug}\ntype: fact\nstatus: accepted\nscope:\n  project: true\nconfidence: high\ncreated: '2026-01-01T00:00:00Z'\nauthor: cad2187-worker\n---\n\n{marker}\n\n**Why:** fixture\n\n**How to apply:** fixture\n"),
    )
    .unwrap();
}

/// Every file under the tracker, so a refused write is shown to change none.
fn tracker(rig: &Rig) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == ".git") {
                    continue;
                }
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().display().to_string();
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(&rig.pm, &rig.pm, &mut out);
    out
}

#[test]
fn signed_member_is_served_nothing_of_a_private_project_on_the_board_read_and_write_paths() {
    let rig = start(&[]);
    let _keys = initialize_queue_keyring(&rig);
    seed(&rig);
    issue(&rig, "secret", "SEC-2", PRIVATE_TITLE, MEMBER);
    issue(&rig, "visible", "VIS-2", VISIBLE_TITLE, MEMBER);
    memory(&rig, "secret", "secret-lesson", PRIVATE_MEMORY);
    memory(&rig, "visible", "open-lesson", VISIBLE_MEMORY);
    let member = login(&rig, MEMBER, "member");
    let owner = login(&rig, "cad2187-owner", "owner");

    // The project's recorded policy and its context bundle: a private project
    // answers a member exactly like a project that does not exist.
    for leaf in ["policy", "context"] {
        let (status, body) = raw(&rig, &member, &format!("/api/projects/ghost/{leaf}"));
        assert_eq!(status, 404, "absent project {leaf} not 404: {body}");
        let (status, private) = raw(&rig, &member, &format!("/api/projects/secret/{leaf}"));
        assert_eq!(status, 404, "member {leaf} of a private project: {private}");
        assert_eq!(
            private.replace("secret", "ghost"),
            body,
            "{leaf} distinguishes a private project from an absent one"
        );
        let (status, visible) = raw(&rig, &member, &format!("/api/projects/visible/{leaf}"));
        assert_eq!(
            status, 200,
            "member lost the visible project's {leaf}: {visible}"
        );
        let (status, owner_body) = raw(&rig, &owner, &format!("/api/projects/secret/{leaf}"));
        assert_eq!(
            status, 200,
            "owner lost the private project's {leaf}: {owner_body}"
        );
    }

    // My work: the member's own private-project assignment is not listed, and
    // the per-issue answer matches the one for an issue that does not exist.
    let (status, body) = raw(&rig, &member, "/api/my-work");
    assert_eq!(status, 200, "member my-work refused: {body}");
    assert!(
        body.contains(VISIBLE_TITLE),
        "member lost own visible work: {body}"
    );
    for marker in [PRIVATE_TITLE, "SEC-2", "SEC-1", "\"secret\""] {
        assert!(
            !body.contains(marker),
            "member my-work leaked {marker}: {body}"
        );
    }
    let (status, private) = raw(&rig, &member, "/api/my-work/issues/SEC-2");
    assert_eq!(status, 200, "member my-work detail failed: {private}");
    let (_, absent) = raw(&rig, &member, "/api/my-work/issues/SEC-77");
    assert_eq!(
        private.replace("SEC-2", "SEC-77"),
        absent,
        "my-work detail distinguishes a private issue from an absent one"
    );

    // The overview payload carries no row, key, id or title of the project.
    let keys = |overview: &Value| -> Vec<String> {
        overview["projects"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| r["key"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let (status, text) = raw(&rig, &member, "/api/overview");
    assert_eq!(status, 200, "member overview refused: {text}");
    let member_overview: Value = serde_json::from_str(&text).unwrap();
    let (status, owner_text) = raw(&rig, &owner, "/api/overview");
    assert_eq!(status, 200, "owner overview refused: {owner_text}");
    let owner_overview: Value = serde_json::from_str(&owner_text).unwrap();
    assert!(
        keys(&owner_overview).contains(&"secret".to_string()),
        "owner overview lost the private project: {owner_text}"
    );
    assert!(
        keys(&member_overview).contains(&"visible".to_string()),
        "member overview lost the visible project: {text}"
    );
    assert!(
        !keys(&member_overview).contains(&"secret".to_string()),
        "member overview lists the private project: {text}"
    );
    for marker in [PRIVATE_TITLE, "SEC-1", "SEC-2", "\"secret\""] {
        assert!(
            !text.contains(marker),
            "member overview leaked {marker}: {text}"
        );
    }

    // The activity feed and its export are the owner's.
    for path in ["/api/activity", "/api/activity/export"] {
        let (status, body) = raw(&rig, &member, path);
        assert!(
            (400..500).contains(&status),
            "member reached {path}: {status} {body}"
        );
        for marker in [PRIVATE_TITLE, "SEC-"] {
            assert!(
                !body.contains(marker),
                "member {path} leaked {marker}: {body}"
            );
        }
        let (status, body) = raw(&rig, &owner, path);
        assert_eq!(status, 200, "owner lost {path}: {body}");
    }

    // A memory of a private project answers like one in a project that does
    // not exist.
    let (status, absent) = raw(&rig, &member, "/api/memories/ghost/secret-lesson");
    assert_eq!(status, 404, "absent project memory not 404: {absent}");
    let (status, private) = raw(&rig, &member, "/api/memories/secret/secret-lesson");
    assert_eq!(status, 404, "member read a private memory: {private}");
    assert_eq!(
        private, absent,
        "memory detail distinguishes a private project from an absent one"
    );
    assert!(
        !private.contains(PRIVATE_MEMORY),
        "private memory leaked: {private}"
    );
    let (status, body) = raw(&rig, &member, "/api/memories/visible/open-lesson");
    assert_eq!(status, 200, "member lost a visible memory: {body}");
    assert!(
        body.contains(VISIBLE_MEMORY),
        "visible memory missing: {body}"
    );
    let (status, body) = raw(&rig, &owner, "/api/memories/secret/secret-lesson");
    assert_eq!(status, 200, "owner lost the private memory: {body}");
    assert!(
        body.contains(PRIVATE_MEMORY),
        "private memory missing for owner: {body}"
    );

    // A write to a private project's issue is refused and changes no file.
    let before = tracker(&rig);
    let (status, body) = post_comment(&rig, &member, "SEC-1", "CAD2187_FORBIDDEN_WRITE");
    assert_eq!(status, 404, "member wrote a private issue: {body}");
    assert!(
        !body.contains(PRIVATE_TITLE) && !body.contains("secret"),
        "refused write named the private project: {body}"
    );
    assert_eq!(
        tracker(&rig),
        before,
        "a refused write to a private issue changed the tracker"
    );
}

/// An unreadable tracker is not an empty one: the member's counts are omitted
/// rather than reported as zero. Returns early where the mode cannot express
/// it (root, ACLs), as the policy unit tests do.
#[test]
fn signed_member_health_omits_counts_when_the_tracker_cannot_be_checked() {
    use std::os::unix::fs::PermissionsExt;
    struct Restore(std::path::PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }
    let rig = start(&[]);
    seed(&rig);
    let member = login(&rig, MEMBER, "member");
    let (status, body) = raw(&rig, &member, "/api/health");
    assert_eq!(status, 200, "member health refused: {body}");
    let health: Value = serde_json::from_str(&body).unwrap();
    assert!(
        health.get("pm_dir").is_none(),
        "member was handed pm_dir: {body}"
    );
    assert!(
        health["projects"].is_number(),
        "readable tracker lost counts: {body}"
    );

    let hidden = rig.pm.join("secret");
    std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _restore = Restore(hidden.clone());
    if std::fs::symlink_metadata(hidden.join("project.yaml")).is_ok() {
        return;
    }
    let (status, body) = raw(&rig, &member, "/api/health");
    assert_eq!(status, 200, "member health refused: {body}");
    let health: Value = serde_json::from_str(&body).unwrap();
    assert!(
        health.get("pm_dir").is_none(),
        "member was handed pm_dir: {body}"
    );
    assert!(
        health.get("projects").is_none() && health.get("issues").is_none(),
        "an unreadable tracker still reported counts: {body}"
    );
}
