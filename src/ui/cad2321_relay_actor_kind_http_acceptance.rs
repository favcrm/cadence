//! CAD-2321 independently authored refusal acceptance for the daemon relay.
//! The product implementer must not edit or weaken this check.
//!
//! The rule (operator decision of 2026-11-15 on CAD-2294) applies to every
//! project read of a CLI envelope, including the reads the board relays to
//! the daemon with `cli_envelope` (`status`, `agent_list`, `agent_show`): a
//! principal kind whose actor prefix disagrees names no project caller, so
//! the relayed read is refused rather than scoped to anyone.
//!
//! The fixture is the real daemon and the real board, with a real signed
//! envelope on the public CLI route. `mia` is the only member the private
//! project grants, and every mismatched caller carries `sub = mia`, so a
//! relay that fell back to `sub` or to the actor spelling would show it the
//! private project's task and this check would fail. The positive control
//! proves the matching caller does see that task.

use super::*;

const SECRET_TASK: &str = "cad2215-secret-task";

fn envelope(rig: &Rig, kind: &str, actor: &str, sub: &str) -> String {
    let now = crate::issue::time::now_epoch();
    let header = json!({"alg": "EdDSA", "typ": "agenticos-wiki-actor/1", "kid": KID});
    let claims = json!({
        "iss": rig.issuer,
        "aud": format!("http://{}", rig.public_host),
        "sub": sub,
        "actor": actor,
        "principal_kind": kind,
        "role_at_issue": "member",
        "organization_id": ORG,
        "scope": ["cli.read"],
        "credential_id": format!("cad2321-{sub}"),
        "iat": now,
        "exp": now + 120,
        "jti": format!("cad2321-relay-{}", uuid::Uuid::new_v4()),
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

fn cli(rig: &Rig, kind: &str, actor: &str, sub: &str, verb: &str, args: Value) -> (u16, Value) {
    let url = format!("http://127.0.0.1:{}/api/cli/{verb}", rig.board_port);
    let mut response = http_agent()
        .post(&url)
        .header("Host", &rig.public_host)
        .header("Content-Type", "application/json")
        .header(
            "Authorization",
            &format!("Bearer wikienv_{}", envelope(rig, kind, actor, sub)),
        )
        .send_json(args)
        .expect("hosted CLI request failed");
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_json()
        .expect("hosted CLI response is not JSON");
    (status, body)
}

/// The tracker's content: [`tree`] without the entries the tracker's own
/// `.gitignore` names as caches (`.write.lock`, `.index/`, `.cadence/`),
/// which the daemon's watcher refreshes on its own schedule.
fn tracker(pm: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut out = tree(pm);
    out.retain(|path, _| {
        !(path == ".write.lock" || path.starts_with(".index/") || path.starts_with(".cadence/"))
    });
    out
}

/// The CAD-2215 fixture, with its private project granting exactly
/// `users/mia`.
fn mia_rig() -> Rig {
    let rig = start(&[]);
    seed(&rig);
    std::fs::write(
        rig.pm.join("secret").join("project.yaml"),
        "key: secret\nprefix: SEC\npolicy:\n  version: 1\n  visibility: private\n  members:\n  - users/mia\n",
    )
    .unwrap();
    // Settle the daemon's own tracker writes before any snapshot. Its
    // periodic checkup grooms dormant backlog work (stamping
    // `last_groomed_at`), and the fixture issues are dormant. One pass here
    // stamps every candidate under the same per-issue lock, and the groom's
    // cooldown then makes every later daemon pass skip them. So a later
    // tracker change can only come from the call under test.
    let pm = crate::issue::Pm::at(&rig.pm).unwrap();
    crate::issue::groom::groom(
        &pm,
        None,
        crate::issue::groom::GROOM_GRACE_SECS,
        false,
        "cad2321-fixture",
    )
    .unwrap();
    rig
}

const RELAYED: [(&str, fn() -> Value); 3] = [
    ("agent_list", || json!({})),
    ("agent_show", || json!({"alias": "cad2215-worker"})),
    ("status", || json!({})),
];

fn assert_relay_refused(kind: &str, actor: &str) {
    let rig = mia_rig();
    let before = tracker(&rig.pm);
    let who = format!("{kind} principal with actor {actor} (sub mia)");
    for (verb, args) in RELAYED {
        let (status, body) = cli(&rig, kind, actor, "mia", verb, args());
        let text = body.to_string();
        assert_eq!(status, 400, "{who} {verb} was not refused: {text}");
        assert_eq!(body["ok"], false, "{who} {verb}: {text}");
        assert!(
            body["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("invalid read identity")),
            "{who} {verb} refused for another reason: {text}"
        );
        assert!(!text.contains(SECRET_TASK), "{who} {verb} leaked: {text}");
        assert_eq!(tracker(&rig.pm), before, "{who} {verb} changed the tracker");
    }
}

#[test]
fn relayed_reads_refuse_a_user_principal_with_an_agent_actor() {
    assert_relay_refused("user", "agents/x");
}

#[test]
fn relayed_reads_refuse_an_agent_principal_with_a_user_actor() {
    assert_relay_refused("agent", "users/mia");
}

#[test]
fn relayed_reads_refuse_an_agent_principal_with_a_service_actor() {
    assert_relay_refused("agent", "service/mia");
}

/// Positive control: the matching caller's relayed read is scoped to its
/// grant and does see the private project's task.
#[test]
fn relayed_agent_list_shows_the_granted_member_its_private_task() {
    let rig = mia_rig();
    let before = tracker(&rig.pm);
    let (status, body) = cli(&rig, "user", "users/mia", "mia", "agent_list", json!({}));
    let text = body.to_string();
    assert_eq!(status, 200, "the granted member was refused: {text}");
    assert!(text.contains("cad2215-worker"), "no fixture agent: {text}");
    assert!(
        text.contains(SECRET_TASK),
        "the granted member lost its private task: {text}"
    );
    assert_eq!(
        tracker(&rig.pm),
        before,
        "the admitted read changed the tracker"
    );
}
