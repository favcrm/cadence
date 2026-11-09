//! CAD-2215 identity-collision acceptance: signed board sessions and separately
//! signed hosted CLI / wiki envelopes. The private policy is synthetic fixture
//! data, not an assertion that product private mode or OS isolation is enabled.
use super::*;

fn signed_envelope(rig: &Rig, sub: &str, owner: bool, wiki: bool) -> String {
    let now = crate::issue::time::now_epoch();
    let header = json!({"alg":"EdDSA", "typ":"agenticos-wiki-actor/1", "kid":KID});
    // Both distinct subjects genuinely sanitize to this legacy spelling.
    // The operator envelope uses the separately verified owner role.
    let actor = if owner { "operator" } else { "users/usr_safe" };
    let mut claims = json!({
        "iss": rig.issuer, "aud": format!("http://{}", rig.public_host),
        "sub": sub, "actor": actor, "principal_kind":"user",
        "role_at_issue": if owner { "owner" } else { "member" },
        "organization_id": ORG, "credential_id": format!("collision-{sub}"),
        "iat": now, "exp": now + 120, "jti": format!("collision-{}", uuid::Uuid::new_v4()),
    });
    if wiki {
        claims["scope"] = json!(["wiki.read"]);
        claims["wiki_prefixes"] = json!(["users/usr_safe/", "global/"]);
        claims["prefix_grants"] =
            json!([{"scope":"wiki.read", "prefixes":["users/usr_safe/", "global/"]}]);
    } else {
        claims["scope"] = json!(["cli.read"]);
    }
    let signed = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
    );
    format!(
        "{signed}.{}",
        URL_SAFE_NO_PAD.encode(rig.key.sign(signed.as_bytes()).as_ref())
    )
}

fn board_read(rig: &Rig, cookie: &str, path: &str) -> (u16, String) {
    let mut response = http_agent()
        .get(&format!("http://127.0.0.1:{}{path}", rig.board_port))
        .header("Host", &rig.public_host)
        .header("Cookie", cookie)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .call()
        .expect("board read failed");
    let status = response.status().as_u16();
    (status, response.body_mut().read_to_string().unwrap())
}

fn hosted_cli(rig: &Rig, sub: &str, owner: bool, verb: &str, args: Value) -> (u16, String) {
    let mut response = http_agent()
        .post(&format!(
            "http://127.0.0.1:{}/api/cli/{verb}",
            rig.board_port
        ))
        .header("Host", &rig.public_host)
        .header("Content-Type", "application/json")
        .header(
            "Authorization",
            &format!("Bearer wikienv_{}", signed_envelope(rig, sub, owner, false)),
        )
        .send_json(args)
        .expect("hosted CLI call failed");
    let status = response.status().as_u16();
    (status, response.body_mut().read_to_string().unwrap())
}

fn hosted_wiki(rig: &Rig, sub: &str, owner: bool) -> (u16, String) {
    let mut response = http_agent()
        .get(&format!(
            "http://127.0.0.1:{}/api/wiki/file?path=users/usr_safe/collision.md",
            rig.board_port
        ))
        .header("Host", &rig.public_host)
        .header(
            "Authorization",
            &format!("Bearer wikienv_{}", signed_envelope(rig, sub, owner, true)),
        )
        .call()
        .expect("hosted wiki call failed");
    let status = response.status().as_u16();
    (status, response.body_mut().read_to_string().unwrap())
}

#[test]
fn signed_board_and_hosted_identities_do_not_share_legacy_sanitized_authority() {
    assert_eq!(
        crate::ident::sanitized("usr_safe"),
        crate::ident::sanitized("usr|safe")
    );
    let rig = start(&[]);
    seed(&rig);
    // Synthetic retained private policy: only the canonical subject is named.
    std::fs::write(rig.pm.join("secret/project.yaml"),
        "key: secret\nprefix: SEC\npolicy:\n  version: 1\n  visibility: private\n  members:\n  - users/usr_safe\n").unwrap();
    std::fs::write(rig.pm.join("secret/SEC-1/issue.md"),
        "---\nid: SEC-1\ntitle: CAD2215_COLLISION_ISSUE_SECRET\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n").unwrap();
    let canonical = login(&rig, "usr_safe", "member");
    let collision = login(&rig, "usr|safe", "member");
    let operator = login(&rig, "cad2215-collision-owner", "owner");

    // Synthetic assignment fixtures are in the workspace-visible project:
    // project access alone must not imply a personal assignment match.
    std::fs::write(rig.pm.join("visible/VIS-1/issue.md"),
        "---\nid: VIS-1\ntitle: CAD2215_CANONICAL_MY_WORK\nstatus: backlog\npriority: P2\nowner: usr_safe\ncreated: '2026-01-01T00:00:00Z'\n---\n").unwrap();
    let operator_issue = rig.pm.join("visible/VIS-2");
    std::fs::create_dir_all(&operator_issue).unwrap();
    std::fs::write(operator_issue.join("issue.md"),
        "---\nid: VIS-2\ntitle: CAD2215_OPERATOR_MY_WORK\nstatus: backlog\npriority: P2\nowner: cad2215-collision-owner\ncreated: '2026-01-01T00:00:00Z'\n---\n").unwrap();

    // Create the canonical user's actual private wiki file through a signed
    // owner session; no forged session rows or fake wiki authorization.
    let mut written = http_agent()
        .put(&format!(
            "http://127.0.0.1:{}/api/wiki/file",
            rig.board_port
        ))
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("X-Cadence-Board", "1")
        .header("Cookie", &operator)
        .header("Content-Type", "application/json")
        .send_json(
            json!({"path":"users/usr_safe/collision.md", "text":"CAD2215_COLLISION_WIKI_SECRET"}),
        )
        .expect("signed operator wiki write failed");
    let written_status = written.status().as_u16();
    let written_body = written.body_mut().read_to_string().unwrap();
    assert_eq!(
        written_status, 200,
        "wiki fixture creation failed: {written_body}"
    );
    // Synthetic public wiki content, created by the real signed operator.
    let mut global_written = http_agent()
        .put(&format!(
            "http://127.0.0.1:{}/api/wiki/file",
            rig.board_port
        ))
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("X-Cadence-Board", "1")
        .header("Cookie", &operator)
        .header("Content-Type", "application/json")
        .send_json(
            json!({"path":"global/collision-public.md", "text":"CAD2215_COLLISION_GLOBAL_PUBLIC"}),
        )
        .expect("signed operator global wiki write failed");
    let global_status = global_written.status().as_u16();
    let global_body = global_written.body_mut().read_to_string().unwrap();
    assert_eq!(
        global_status, 200,
        "global wiki fixture creation failed: {global_body}"
    );

    for (path, marker) in [
        ("/api/projects", "secret"),
        ("/api/issues/SEC-1", "CAD2215_COLLISION_ISSUE_SECRET"),
        (
            "/api/wiki/file?path=users/usr_safe/collision.md",
            "CAD2215_COLLISION_WIKI_SECRET",
        ),
    ] {
        for (who, cookie) in [("canonical", &canonical), ("operator", &operator)] {
            let (status, body) = board_read(&rig, cookie, path);
            assert_eq!(status, 200, "{who} board {path} refused: {body}");
            assert!(
                body.contains(marker),
                "{who} board {path} lost {marker}: {body}"
            );
        }
        let (status, body) = board_read(&rig, &collision, path);
        assert!(
            matches!(status, 200 | 400 | 401 | 403 | 404),
            "unexpected colliding board status {status}: {body}"
        );
        assert!(
            !body.contains(marker),
            "colliding subject read {marker} on board {path}: {body}"
        );
    }

    // Both issue cards are project-readable, but personal attribution must
    // use the verified principal, not its colliding sanitized handle.
    for (who, cookie, expected) in [
        ("canonical", &canonical, "CAD2215_CANONICAL_MY_WORK"),
        ("operator", &operator, "CAD2215_OPERATOR_MY_WORK"),
    ] {
        let (status, body) = board_read(&rig, cookie, "/api/my-work");
        assert_eq!(status, 200, "{who} board my-work refused: {body}");
        assert!(
            body.contains(expected),
            "{who} lost own board assignment: {body}"
        );
    }
    let (status, body) = board_read(&rig, &collision, "/api/my-work");
    assert!(
        matches!(status, 200 | 400 | 401 | 403 | 404),
        "unexpected collision my-work status {status}: {body}"
    );
    assert!(
        !body.contains("CAD2215_CANONICAL_MY_WORK"),
        "collision inherited board assignment: {body}"
    );
    let (status, body) = board_read(&rig, &canonical, "/api/my-work/issues/VIS-1");
    assert_eq!(status, 200, "canonical assignment detail refused: {body}");
    let detail: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        detail["assigned"], true,
        "canonical assignment detail lost: {detail}"
    );
    let (status, body) = board_read(&rig, &collision, "/api/my-work/issues/VIS-1");
    if status == 200 {
        let detail: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            detail["assigned"], false,
            "collision inherited assignment: {detail}"
        );
        assert!(
            detail["assignment"].is_null(),
            "collision inherited assignment provenance: {detail}"
        );
    } else {
        assert!(
            matches!(status, 400 | 401 | 403 | 404),
            "unexpected collision detail status {status}: {body}"
        );
    }
    for (sub, owner, expected) in [
        ("usr_safe", false, "CAD2215_CANONICAL_MY_WORK"),
        ("cad2215-collision-owner", true, "CAD2215_OPERATOR_MY_WORK"),
    ] {
        let (status, body) = hosted_cli(&rig, sub, owner, "my_work", json!({}));
        assert_eq!(status, 200, "{sub} CLI my_work refused: {body}");
        assert!(
            body.contains(expected),
            "{sub} lost own CLI assignment: {body}"
        );
    }
    let (status, body) = hosted_cli(&rig, "usr|safe", false, "my_work", json!({}));
    assert!(
        matches!(status, 200 | 400 | 401 | 403 | 404),
        "unexpected collision CLI my_work status {status}: {body}"
    );
    assert!(
        !body.contains("CAD2215_CANONICAL_MY_WORK"),
        "collision inherited CLI assignment/provenance: {body}"
    );

    // The private project policy above is synthetic; this is the real signed
    // board issue-comment write route, not a forged caller or direct write.
    let issue_file = rig.pm.join("secret/SEC-1/issue.md");
    let before_write = std::fs::read(&issue_file).unwrap();
    let comments_dir = rig.pm.join("secret/SEC-1/comments");
    let comments = || {
        let entries = match std::fs::read_dir(&comments_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => panic!("cannot inspect comment directory: {error}"),
        };
        let mut snapshot: Vec<_> = entries
            .map(|entry| {
                let entry = entry.unwrap();
                assert!(
                    entry.file_type().unwrap().is_file(),
                    "unexpected non-file comment entry"
                );
                (entry.file_name(), std::fs::read(entry.path()).unwrap())
            })
            .collect();
        snapshot.sort_by(|left, right| left.0.cmp(&right.0));
        snapshot
    };
    let before_comments = comments();
    let comment = |cookie: &str, text: &str| {
        let mut response = http_agent()
            .post(&format!(
                "http://127.0.0.1:{}/api/issues/SEC-1/comments",
                rig.board_port
            ))
            .header("Host", &rig.public_host)
            .header("Origin", &format!("http://{}", rig.public_host))
            .header("Sec-Fetch-Site", "same-origin")
            .header("X-Cadence-Board", "1")
            .header("Cookie", cookie)
            .header("Content-Type", "application/json")
            .send_json(json!({"body": text}))
            .expect("signed board comment request failed");
        let status = response.status().as_u16();
        (status, response.body_mut().read_to_string().unwrap())
    };
    let (status, body) = comment(&collision, "CAD2215_COLLISION_FORBIDDEN_WRITE");
    assert!(
        matches!(status, 400 | 401 | 403 | 404),
        "colliding member wrote private issue: {status} {body}"
    );
    assert_eq!(
        std::fs::read(&issue_file).unwrap(),
        before_write,
        "refused collision write changed the issue"
    );
    assert_eq!(
        comments(),
        before_comments,
        "refused collision write changed comment files"
    );
    let (status, body) = comment(&operator, "CAD2215_OPERATOR_ALLOWED_WRITE");
    assert_eq!(
        status, 200,
        "operator could not use the same comment route: {body}"
    );
    let stored_comments = comments();
    assert!(
        stored_comments
            .iter()
            .any(|(_, bytes)| String::from_utf8_lossy(bytes)
                .contains("CAD2215_OPERATOR_ALLOWED_WRITE")),
        "operator comment was not persisted in comment files"
    );
    assert!(
        !stored_comments
            .iter()
            .any(|(_, bytes)| String::from_utf8_lossy(bytes)
                .contains("CAD2215_COLLISION_FORBIDDEN_WRITE")),
        "collision comment was persisted in comment files"
    );

    // issue_ls is the hosted CLI's project-filtered listing: there is no
    // project-list read verb. issue_show exercises the direct read separately.
    for (verb, args) in [
        ("issue_ls", json!({})),
        ("issue_show", json!({"id":"SEC-1"})),
    ] {
        for (sub, owner) in [("usr_safe", false), ("cad2215-collision-owner", true)] {
            let (status, body) = hosted_cli(&rig, sub, owner, verb, args.clone());
            assert_eq!(status, 200, "{sub} CLI {verb} refused: {body}");
            assert!(
                body.contains("CAD2215_COLLISION_ISSUE_SECRET"),
                "{sub} CLI {verb} omitted granted issue: {body}"
            );
        }
        let (status, body) = hosted_cli(&rig, "usr|safe", false, verb, args);
        assert!(
            matches!(status, 200 | 400 | 401 | 403 | 404),
            "unexpected collision CLI status {status}: {body}"
        );
        assert!(
            !body.contains("CAD2215_COLLISION_ISSUE_SECRET"),
            "collision CLI {verb} leaked: {body}"
        );
    }
    // Hosted wiki is a distinct signed wiki envelope and HTTP route, not a
    // CLI-scope envelope; both carry the same distinct verified subjects.
    for (sub, owner) in [("usr_safe", false), ("cad2215-collision-owner", true)] {
        let (status, body) = hosted_wiki(&rig, sub, owner);
        assert_eq!(status, 200, "{sub} hosted wiki refused: {body}");
        assert!(
            body.contains("CAD2215_COLLISION_WIKI_SECRET"),
            "{sub} hosted wiki omitted own page: {body}"
        );
    }
    let (status, body) = hosted_wiki(&rig, "usr|safe", false);
    assert!(
        matches!(status, 200 | 400 | 401 | 403 | 404),
        "unexpected collision wiki status {status}: {body}"
    );
    assert!(
        !body.contains("CAD2215_COLLISION_WIKI_SECRET"),
        "colliding subject read hosted wiki: {body}"
    );

    // An unresolvable subject still has the public wiki ceiling granted by
    // its own real signed wiki.read envelope; personal authority is separate.
    let mut public_response = http_agent()
        .get(&format!(
            "http://127.0.0.1:{}/api/wiki/file?path=global/collision-public.md",
            rig.board_port
        ))
        .header("Host", &rig.public_host)
        .header(
            "Authorization",
            &format!(
                "Bearer wikienv_{}",
                signed_envelope(&rig, "usr|safe", false, true)
            ),
        )
        .call()
        .expect("colliding subject public hosted wiki request failed");
    let public_status = public_response.status().as_u16();
    let public_body = public_response.body_mut().read_to_string().unwrap();
    assert_eq!(
        public_status, 200,
        "public hosted wiki became unavailable: {public_body}"
    );
    assert!(
        public_body.contains("CAD2215_COLLISION_GLOBAL_PUBLIC"),
        "public hosted wiki content missing: {public_body}"
    );
}
