//! CAD-2357 signed hosted-CLI acceptance: an issue id in a private project
//! answers a member exactly as an id that does not exist, on every verb
//! that takes an issue id, and the Owner keeps the verbs' own answers.
//! Signed hosted-CLI HTTP only; this does not prove local-agent isolation.

use super::*;

fn envelope(rig: &Rig, role: &str) -> String {
    let now = crate::issue::time::now_epoch();
    let (sub, actor) = match role {
        "owner" => ("cad2357-cli-owner", "operator"),
        "member" => ("cad2357-cli-member", "users/cad2357-cli-member"),
        _ => panic!("unsupported fixture role"),
    };
    let header = json!({"alg": "EdDSA", "typ": "agenticos-wiki-actor/1", "kid": KID});
    let claims = json!({
        "iss": rig.issuer,
        "aud": format!("http://{}", rig.public_host),
        "sub": sub,
        "actor": actor,
        "principal_kind": "user",
        "role_at_issue": role,
        "organization_id": ORG,
        "scope": ["cli.read", "cli.write"],
        "credential_id": format!("cad2357-{sub}"),
        "iat": now,
        "exp": now + 120,
        "jti": format!("cad2357-cli-{}", uuid::Uuid::new_v4()),
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

fn call(rig: &Rig, role: &str, verb: &str, args: Value) -> (u16, String) {
    let url = format!("http://127.0.0.1:{}/api/cli/{verb}", rig.board_port);
    let mut response = http_agent()
        .post(&url)
        .header("Host", &rig.public_host)
        .header("Content-Type", "application/json")
        .header(
            "Authorization",
            &format!("Bearer wikienv_{}", envelope(rig, role)),
        )
        .send_json(args)
        .expect("hosted CLI request failed");
    let status = response.status().as_u16();
    (
        status,
        response.body_mut().read_to_string().unwrap_or_default(),
    )
}

/// Every file under both fixture projects, so a refused call that wrote
/// anything (a comment, a field, a new issue) shows up as a difference.
fn projects_state(rig: &Rig) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().display().to_string();
                out.push((rel, std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    for key in ["visible", "secret"] {
        walk(&rig.pm, &rig.pm.join(key), &mut out);
    }
    out
}

/// The request each issue-id verb makes for `id`.
fn verb_calls(id: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("issue_show", json!({"id": id})),
        ("issue_history", json!({"id": id})),
        ("issue_comment", json!({"id": id, "body": "CAD2357 probe"})),
        (
            "issue_set",
            json!({"ids": [id], "set": ["priority=P1"], "if_rev": "cad2357-probe"}),
        ),
        (
            "issue_new",
            json!({"project": "visible", "title": "CAD2357 probe", "parent": id}),
        ),
        (
            "issue_new",
            json!({"project": "visible", "title": "CAD2357 probe", "blocked_by": [id]}),
        ),
    ]
}

#[test]
fn member_gets_the_same_answer_for_a_private_issue_id_as_for_a_missing_one() {
    let rig = start(&[]);
    seed(&rig);

    // Positive control: the member reaches the verbs, and SEC-1 exists.
    let (status, body) = call(&rig, "member", "issue_show", json!({"id": "VIS-1"}));
    assert_eq!(status, 200, "member refused a visible issue: {body}");
    let (status, body) = call(&rig, "owner", "issue_show", json!({"id": "SEC-1"}));
    assert_eq!(status, 200, "owner cannot read the private fixture: {body}");

    let before = projects_state(&rig);
    for ((verb, private), ((_, absent), (_, absent_visible))) in verb_calls("SEC-1")
        .into_iter()
        .zip(verb_calls("SEC-404").into_iter().zip(verb_calls("VIS-404")))
    {
        let (private_status, private_body) = call(&rig, "member", verb, private.clone());
        let (absent_status, absent_body) = call(&rig, "member", verb, absent);
        let (visible_status, visible_body) = call(&rig, "member", verb, absent_visible);
        assert_eq!(
            private_status, 404,
            "member {verb} {private}: {private_body}"
        );
        assert_eq!(
            (absent_status, absent_body.as_str()),
            (private_status, private_body.as_str()),
            "member {verb}: a private id answers differently from an absent one"
        );
        assert_eq!(
            (visible_status, visible_body.as_str()),
            (private_status, private_body.as_str()),
            "member {verb}: a private id answers differently from an absent id in a readable project"
        );
        for marker in ["secret", "SEC-1", "SEC-404", "known projects"] {
            assert!(
                !private_body.contains(marker),
                "member {verb} answer disclosed {marker}: {private_body}"
            );
        }
        assert_eq!(
            projects_state(&rig),
            before,
            "a refused member {verb} changed the tracker"
        );
    }

    // The Owner keeps the verbs' own answers: the private issue is served,
    // and an absent id still gets the verb's own not-found text.
    let (status, body) = call(&rig, "owner", "issue_show", json!({"id": "SEC-1"}));
    assert_eq!(status, 200, "owner issue_show SEC-1: {body}");
    // The fixture tracker is not a git repository, so history itself may be
    // unavailable; what matters is that the Owner is not refused.
    let (status, body) = call(&rig, "owner", "issue_history", json!({"id": "SEC-1"}));
    assert_ne!(status, 404, "owner issue_history SEC-1 refused: {body}");
    let (status, body) = call(&rig, "owner", "issue_history", json!({"id": "SEC-404"}));
    assert_eq!(status, 404, "owner issue_history SEC-404: {body}");
    assert!(
        body.contains("SEC-404"),
        "owner lost the verb's own not-found answer: {body}"
    );
}
