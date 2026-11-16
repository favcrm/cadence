//! CAD-2357 independently authored refusal acceptance for the hosted CLI.
//! The product implementer must not edit or weaken this check.
//! Drives the real signed POST /api/cli/<verb> route; the tracker fixture is
//! a visible project (VIS) and a private one (SEC) the member cannot read.

use super::*;
use std::collections::BTreeMap;

fn indep_envelope(rig: &Rig, role: &str) -> String {
    let now = crate::issue::time::now_epoch();
    let (sub, actor) = match role {
        "owner" => ("cad2357-indep-owner", "operator"),
        "member" => ("cad2357-indep-member", "users/cad2357-indep-member"),
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
        "credential_id": format!("cad2357-indep-{sub}"),
        "iat": now,
        "exp": now + 120,
        "jti": format!("cad2357-indep-{}", uuid::Uuid::new_v4()),
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

/// Status and raw body: a refusal is compared byte for byte.
fn post(rig: &Rig, role: &str, verb: &str, args: Value) -> (u16, String) {
    let url = format!("http://127.0.0.1:{}/api/cli/{verb}", rig.board_port);
    let mut response = http_agent()
        .post(&url)
        .header("Host", &rig.public_host)
        .header("Content-Type", "application/json")
        .header(
            "Authorization",
            &format!("Bearer wikienv_{}", indep_envelope(rig, role)),
        )
        .send_json(args)
        .expect("hosted CLI request failed");
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .expect("hosted CLI body unreadable");
    (status, body)
}

/// Every file under the whole tracker directory.
fn tracker(rig: &Rig) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
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

fn id_verbs(id: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("issue_show", json!({"id": id})),
        ("issue_history", json!({"id": id})),
        (
            "issue_comment",
            json!({"id": id, "body": "CAD2357 indep probe"}),
        ),
        (
            "issue_set",
            json!({"ids": [id], "set": ["priority=P1"], "if_rev": "cad2357-indep"}),
        ),
    ]
}

fn indirect_verbs(id: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "parent",
            json!({"project": "visible", "title": "CAD2357 indep", "parent": id}),
        ),
        (
            "blocked_by",
            json!({"project": "visible", "title": "CAD2357 indep", "blocked_by": [id]}),
        ),
        (
            "blocked_by after a readable id",
            json!({"project": "visible", "title": "CAD2357 indep", "blocked_by": ["VIS-1", id]}),
        ),
    ]
}

fn assert_hidden_body(what: &str, id: &str, body: &str) {
    for marker in [id, "SEC-", "secret", "private"] {
        assert!(!body.contains(marker), "{what} disclosed {marker}: {body}");
    }
}

#[test]
fn member_issue_verbs_answer_a_private_id_byte_identically_to_a_missing_id() {
    let rig = start(&[]);
    seed(&rig);

    // Positive control: the member's credential reaches these verbs and the
    // visible issue is served; the Owner reads the private issue. A refusal
    // below therefore comes from the privacy rule, not a broken fixture.
    let (status, body) = post(&rig, "member", "issue_show", json!({"id": "VIS-1"}));
    assert_eq!(status, 200, "member cannot read a visible issue: {body}");
    assert!(body.contains("VIS-1"), "visible issue not served: {body}");
    let (status, body) = post(&rig, "owner", "issue_show", json!({"id": "SEC-1"}));
    assert_eq!(status, 200, "owner cannot read the private issue: {body}");
    assert!(body.contains("SEC-1"), "private issue not served: {body}");
    let (status, body) = post(&rig, "member", "issue_history", json!({"id": "VIS-1"}));
    assert_ne!(status, 404, "member history of a visible issue: {body}");

    let before = tracker(&rig);
    // Absent ids: in the private project's prefix, in the visible one, and
    // under a prefix no project has.
    let absent = ["SEC-404", "VIS-404", "ZZZ-1"];
    for (verb, private_args) in id_verbs("SEC-1") {
        let (private_status, private_body) = post(&rig, "member", verb, private_args.clone());
        assert_eq!(private_status, 404, "member {verb}: {private_body}");
        assert_hidden_body(&format!("member {verb}"), "SEC-1", &private_body);
        for id in absent {
            let args = id_verbs(id)
                .into_iter()
                .find(|(v, _)| *v == verb)
                .unwrap()
                .1;
            let got = post(&rig, "member", verb, args);
            assert_eq!(
                got,
                (private_status, private_body.clone()),
                "member {verb}: private SEC-1 differs from missing {id}"
            );
        }
        assert_eq!(tracker(&rig), before, "refused member {verb} wrote");
    }
}

#[test]
fn member_ids_passed_indirectly_do_not_distinguish_a_private_issue() {
    let rig = start(&[]);
    seed(&rig);
    let (status, body) = post(&rig, "member", "issue_show", json!({"id": "VIS-1"}));
    assert_eq!(status, 200, "member cannot read a visible issue: {body}");

    let before = tracker(&rig);
    for (via, private_args) in indirect_verbs("SEC-1") {
        let private = post(&rig, "member", "issue_new", private_args);
        assert_eq!(private.0, 404, "member issue_new {via}: {}", private.1);
        assert_hidden_body(&format!("member issue_new {via}"), "SEC-1", &private.1);
        for id in ["SEC-404", "VIS-404", "ZZZ-1"] {
            let args = indirect_verbs(id)
                .into_iter()
                .find(|(v, _)| *v == via)
                .unwrap()
                .1;
            assert_eq!(
                post(&rig, "member", "issue_new", args),
                private,
                "member issue_new {via}: private SEC-1 differs from missing {id}"
            );
        }
        assert_eq!(tracker(&rig), before, "refused issue_new {via} wrote");
    }

    // The same request with a readable parent works, so the refusals above
    // are about the linked id and not the title, project or credential.
    let (status, body) = post(
        &rig,
        "member",
        "issue_new",
        json!({"project": "visible", "title": "CAD2357 control", "parent": "VIS-1"}),
    );
    assert_eq!(
        status, 200,
        "member issue_new with a readable parent: {body}"
    );
    assert_ne!(tracker(&rig), before, "control issue_new wrote nothing");
}

#[test]
fn member_malformed_and_path_like_ids_do_not_distinguish_existence() {
    let rig = start(&[]);
    seed(&rig);
    let (status, body) = post(&rig, "member", "issue_show", json!({"id": "VIS-1"}));
    assert_eq!(status, 200, "member cannot read a visible issue: {body}");

    // (an id built on something that exists, the same shape on something
    // that does not). Each pair must answer identically for every verb.
    let pairs: Vec<(String, String)> = [
        ("../secret/SEC-1", "../secret/SEC-404"),
        ("../secret/SEC-1", "../nokey/SEC-1"),
        ("../secret/../visible/VIS-1", "../nokey/../visible/VIS-1"),
        ("../secret/../visible/VIS-9", "../nokey/../visible/VIS-9"),
        ("secret/SEC-1", "nokey/SEC-1"),
        ("secret/SEC-1", "secret/SEC-404"),
        ("./SEC-1", "./SEC-404"),
        ("SEC-1/", "SEC-404/"),
        ("SEC-1/../SEC-1", "SEC-404/../SEC-404"),
        (" SEC-1", " SEC-404"),
        ("sec-1", "sec-404"),
        ("SEC-1\u{0}", "SEC-404\u{0}"),
        ("..%2Fsecret%2FSEC-1", "..%2Fnokey%2FSEC-1"),
        ("/SEC-1", "/SEC-404"),
    ]
    .into_iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();

    let before = tracker(&rig);
    for (on_private, on_absent) in &pairs {
        for ((verb, a), (_, b)) in id_verbs(on_private).into_iter().zip(id_verbs(on_absent)) {
            let left = post(&rig, "member", verb, a);
            let right = post(&rig, "member", verb, b);
            assert_eq!(
                left, right,
                "member {verb}: {on_private:?} vs {on_absent:?}"
            );
            if verb == "issue_show" {
                assert_ne!(left.0, 200, "member {verb} admitted {on_private:?}");
            }
        }
        for ((via, a), (_, b)) in indirect_verbs(on_private)
            .into_iter()
            .zip(indirect_verbs(on_absent))
        {
            let left = post(&rig, "member", "issue_new", a);
            let right = post(&rig, "member", "issue_new", b);
            assert_eq!(
                left, right,
                "member issue_new {via}: {on_private:?} vs {on_absent:?}"
            );
        }
        assert_eq!(
            tracker(&rig),
            before,
            "a malformed id {on_private:?} changed the tracker"
        );
    }
}

#[test]
fn owner_keeps_each_verbs_own_answer_for_a_private_and_a_missing_id() {
    let rig = start(&[]);
    seed(&rig);
    let hidden = post(&rig, "member", "issue_show", json!({"id": "SEC-1"}));
    assert_eq!(hidden.0, 404, "member was not refused: {}", hidden.1);

    for (verb, args) in id_verbs("SEC-1") {
        let before = tracker(&rig);
        let (status, body) = post(&rig, "owner", verb, args);
        assert_ne!(
            body, hidden.1,
            "owner {verb} on a private id got the member refusal: {status} {body}"
        );
        assert!(
            !body.contains("unknown project"),
            "owner {verb} treated its own project as unknown: {status} {body}"
        );
        if verb == "issue_show" {
            assert_eq!(status, 200, "owner {verb}: {body}");
        }
        if verb == "issue_comment" {
            assert_eq!(status, 200, "owner {verb}: {body}");
            assert_ne!(tracker(&rig), before, "owner comment was not recorded");
        }
    }

    // A missing id still gets the verb's own answer, never the member's.
    for (verb, args) in id_verbs("SEC-404") {
        let (status, body) = post(&rig, "owner", verb, args);
        if verb == "issue_show" || verb == "issue_history" {
            assert_eq!(status, 404, "owner {verb} on a missing id: {body}");
        }
        assert_ne!(body, hidden.1, "owner {verb} got the member refusal");
    }

    // The Owner may still link a private issue.
    let (status, body) = post(
        &rig,
        "owner",
        "issue_new",
        json!({"project": "visible", "title": "CAD2357 owner", "parent": "SEC-1"}),
    );
    assert_eq!(status, 200, "owner issue_new with a private parent: {body}");
}
