//! Assignment never grants project access, including through a readable epic's card.
//! Private policies are synthetic tracker fixtures, not product API activation.

use super::*;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

fn fixture_issue(
    pm: &Pm,
    project: &str,
    title: &str,
    owner: &str,
    parent: Option<&str>,
    blocked_by: &[String],
) -> String {
    let created = issue_write::new_issue(
        pm,
        &pm.dir,
        Some(project),
        title,
        None,
        parent,
        blocked_by,
        Some(owner),
        None,
        &[],
        None,
        None,
        "operator",
    )
    .unwrap();
    created["id"].as_str().unwrap().to_string()
}

fn fixture_fields(pm: &Pm, id: &str, pairs: &[&str]) {
    let pairs: Vec<String> = pairs.iter().map(|pair| (*pair).to_string()).collect();
    issue_write::set_fields(pm, &[id.to_string()], &pairs, "operator", None).unwrap();
}

fn fixture_private_policy(pm: &Pm, project: &str, version: u64, members: &[&str]) {
    use crate::issue::project::{Grant, ProjectPolicy, Visibility};

    // Match project_policy's synthetic-policy fixtures without activating Private
    // through the product API or changing its isolation refusal.
    let path = pm.dir.join(project).join("project.yaml");
    let mut recorded = crate::issue::project::load(&path).unwrap();
    recorded.policy = Some(ProjectPolicy {
        version,
        visibility: Visibility::Private,
        members: members
            .iter()
            .map(|member| Grant::parse(member).unwrap())
            .collect(),
    });
    std::fs::write(path, serde_yaml::to_string(&recorded).unwrap()).unwrap();
}

fn assert_visible_ids(body: &Value, expected: &[&str]) {
    assert_eq!(body["owner"], "alpha", "{body}");
    let rows = body["issues"].as_array().expect("my_work issues array");
    let actual: BTreeSet<&str> = rows
        .iter()
        .map(|row| row["id"].as_str().expect("issue card id"))
        .collect();
    let expected: BTreeSet<&str> = expected.iter().copied().collect();
    assert_eq!(
        rows.len(),
        expected.len(),
        "unexpected or duplicate card: {body}"
    );
    assert_eq!(
        actual, expected,
        "assignment must not grant project access: {body}"
    );
}

fn assert_no_hidden_values(body: &Value, hidden: &[&str]) {
    let encoded = body.to_string();
    for value in hidden {
        assert!(
            !encoded.contains(value),
            "restricted issue data {value:?} escaped through My work: {body}"
        );
    }
}

#[test]
fn cad1500_assignment_does_not_grant_access_and_revocation_fences_my_work() {
    let root = tempfile::tempdir().unwrap();
    let pm = Pm::init(&root.path().join("pm")).unwrap();
    for (project, prefix) in [
        ("workspace", "WSP"),
        ("included", "INC"),
        ("excluded", "EXC"),
    ] {
        issue_write::project_add(&pm, project, prefix, &[], &[], &[], None).unwrap();
    }

    let parent = fixture_issue(
        &pm,
        "workspace",
        "Readable personal epic",
        "alpha",
        None,
        &[],
    );
    fixture_fields(&pm, &parent, &["type=epic"]);
    // This child is readable but not personally owned: project access, not
    // ownership, determines the context used for the parent's progress.
    let public_child = fixture_issue(
        &pm,
        "workspace",
        "Another human's readable child",
        "beta",
        Some(&parent),
        &[],
    );
    fixture_fields(&pm, &public_child, &["size=S"]);
    let included_title = "INCLUDED_CONFIDENTIAL_ASSIGNED_CHILD";
    let included = fixture_issue(
        &pm,
        "included",
        included_title,
        "alpha",
        Some(&parent),
        std::slice::from_ref(&public_child),
    );
    fixture_fields(&pm, &included, &["size=S", "status=doing"]);
    let excluded_title = "EXCLUDED_CONFIDENTIAL_ASSIGNED_CHILD";
    let excluded = fixture_issue(
        &pm,
        "excluded",
        excluded_title,
        "alpha",
        Some(&parent),
        std::slice::from_ref(&public_child),
    );
    fixture_fields(&pm, &excluded, &["size=L", "status=review"]);
    fixture_private_policy(&pm, "included", 2, &["users/alpha"]);
    fixture_private_policy(&pm, "excluded", 2, &["users/someone-else"]);

    let state = root.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let key = Ed25519KeyPair::from_seed_unchecked(&[150; 32]).unwrap();
    let public = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
    let jwks = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let issuer = format!("http://{}", jwks.server_addr());
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();
    let opts = ServeOpts {
        public: Some(crate::ui::PublicBoard {
            host: "cad1500-policy.board.localhost".into(),
            issuer: issuer.clone(),
            company: "cad1500-policy".into(),
            authorize_url: format!("{issuer}/authorize"),
        }),
        ..ServeOpts::default()
    };
    let stop = Arc::new(AtomicBool::new(false));
    let jwks_stop = stop.clone();
    let jwks_thread = std::thread::spawn(move || {
        while !jwks_stop.load(Ordering::SeqCst) {
            if let Some(request) = jwks.recv_timeout(Duration::from_millis(100)).unwrap() {
                request
                    .respond(Response::from_string(
                        json!({"keys": [{
                            "kty":"OKP", "crv":"Ed25519", "kid":"cad1500-policy", "x":public
                        }]})
                        .to_string(),
                    ))
                    .unwrap();
            }
        }
    });
    let route_stop = stop.clone();
    let pm_dir = pm.dir.clone();
    let route_thread = std::thread::spawn(move || {
        while !route_stop.load(Ordering::SeqCst) {
            if let Some(mut request) = server.recv_timeout(Duration::from_millis(100)).unwrap() {
                assert_eq!(request.url(), "/api/cli/my_work");
                let method = request.method().clone();
                let response = post(&mut request, &method, "my_work", &state, &pm_dir, &opts);
                request.respond(response).unwrap();
            }
        }
    });
    // Stop and join the fixture listeners even when the refusal assertion fails.
    struct Cleanup(Arc<AtomicBool>, Vec<std::thread::JoinHandle<()>>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
            for thread in self.1.drain(..) {
                let _ = thread.join();
            }
        }
    }
    let _cleanup = Cleanup(stop, vec![route_thread, jwks_thread]);
    let mut serial = 0;
    let mut read_my_work = || {
        serial += 1;
        let now = crate::issue::time::now_epoch();
        let header = URL_SAFE_NO_PAD.encode(
            json!({"alg":"EdDSA", "typ":"agenticos-wiki-actor/1", "kid":"cad1500-policy"})
                .to_string(),
        );
        let claims = URL_SAFE_NO_PAD.encode(
            json!({
                "iss":issuer, "aud":"http://cad1500-policy.board.localhost", "sub":"alpha",
                "actor":"users/alpha", "principal_kind":"user", "role_at_issue":"member",
                "organization_id":"cad1500-policy", "scope":["cli.read"],
                "credential_id":"fixture-alpha", "iat":now, "exp":now+120,
                "jti":format!("cad1500-policy-{serial}")
            })
            .to_string(),
        );
        let signed = format!("{header}.{claims}");
        let envelope = format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(key.sign(signed.as_bytes()).as_ref())
        );
        let mut socket = TcpStream::connect(("127.0.0.1", port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        write!(socket, "POST /api/cli/my_work HTTP/1.1\r\nHost: cad1500-policy.board.localhost\r\nAuthorization: Bearer wikienv_{envelope}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").unwrap();
        let mut bytes = Vec::new();
        socket.read_to_end(&mut bytes).unwrap();
        let split = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let header = std::str::from_utf8(&bytes[..split]).unwrap();
        let status: u16 = header
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes[split + 4..]).unwrap();
        assert_eq!(status, 200, "{body}");
        body
    };

    let initial = read_my_work();
    assert_visible_ids(&initial, &[&parent, &included]);
    assert_no_hidden_values(&initial, &[&excluded, excluded_title]);
    let epic = initial["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"].as_str() == Some(parent.as_str()))
        .unwrap();
    assert_eq!(epic["status"], "doing", "{initial}");
    assert_eq!(epic["work"]["progress"]["total_weight"], 2, "{initial}");
    assert_eq!(
        epic["work"]["progress"]["counts"],
        json!({
            "open":1, "doing":1, "review":0, "blocked":1,
            "done":0, "dropped":0, "total":2
        }),
        "{initial}"
    );
    assert_eq!(epic["work"]["health"]["state"], "at_risk", "{initial}");
    let reasons = epic["work"]["health"]["reasons"].as_array().unwrap();
    assert_eq!(reasons.len(), 1, "{initial}");
    assert_eq!(reasons[0]["issue"], included.as_str(), "{initial}");
    assert_eq!(reasons[0]["owner"], "alpha", "{initial}");

    // Revoke only project membership. Leave the issue, ownership, dependency,
    // parent relationship, server and already-used read-model cache intact.
    fixture_private_policy(&pm, "included", 3, &[]);
    for id in [&included, &excluded] {
        let issue = crate::issue::board::find_issue(&pm.dir, id).unwrap();
        assert_eq!(issue.front.owner.as_deref(), Some("alpha"));
        assert_eq!(issue.front.parent.as_deref(), Some(parent.as_str()));
    }
    let revoked = read_my_work();
    assert_visible_ids(&revoked, &[&parent]);
    assert_no_hidden_values(
        &revoked,
        &[&included, included_title, &excluded, excluded_title],
    );
    let epic = &revoked["issues"][0];
    assert_eq!(
        epic["status"], "backlog",
        "hidden children changed roll-up: {revoked}"
    );
    assert_eq!(epic["work"]["progress"]["total_weight"], 1, "{revoked}");
    assert_eq!(
        epic["work"]["progress"]["counts"],
        json!({
            "open":1, "doing":0, "review":0, "blocked":0,
            "done":0, "dropped":0, "total":1
        }),
        "readable beta-owned child must still count: {revoked}"
    );
    assert_eq!(epic["work"]["health"]["state"], "on_track", "{revoked}");
    assert_eq!(epic["work"]["health"]["reasons"], json!([]), "{revoked}");
}
