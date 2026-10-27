//! CAD-1485 independent HTTP refusal acceptance. Implementers must not edit or
//! weaken this check. Reuses the existing real daemon/board/JWKS rig as a child
//! module; no production registration or native session check is mocked.
//!
//! The platform's response is synthetic signed authority, not proof of real AOS
//! original-session custody or a provider launch. The native session, signature,
//! HTTP routing and cross-member guard are the real implementations.

use super::*;

const VERSION: &str = "hosted-cadence-board-assistant.v1";

struct AuthorityRequest {
    method: String,
    authorization: String,
    cookie_present: bool,
    body: Value,
}

/// Opt-in platform transport response. It deliberately can return a VALID Bob
/// envelope to Alice: the actual native guard, not this fixture, must refuse it.
#[derive(Default)]
pub(super) struct BoardAuthorize {
    envelope: String,
    seen: Vec<AuthorityRequest>,
}

impl BoardAuthorize {
    pub(super) fn answer(&mut self, mut request: tiny_http::Request) {
        let mut bytes = Vec::new();
        request
            .as_reader()
            .take(64 * 1024)
            .read_to_end(&mut bytes)
            .unwrap();
        self.seen.push(AuthorityRequest {
            method: request.method().as_str().to_string(),
            authorization: request
                .headers()
                .iter()
                .find(|h| h.field.equiv("Authorization"))
                .map(|h| h.value.as_str().to_string())
                .unwrap_or_default(),
            cookie_present: request.headers().iter().any(|h| h.field.equiv("Cookie")),
            body: serde_json::from_slice(&bytes).expect("authority request must be JSON"),
        });
        let response = Response::from_string(
            json!({
                "version": VERSION,
                "envelope": self.envelope,
                "expires_at": crate::issue::time::now_epoch() + 120,
            })
            .to_string(),
        )
        .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
        .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
        request.respond(response).unwrap();
    }
}

impl Rig {
    /// Existing signed board SSO protocol, not the assistant envelope format.
    fn alice_sso(&self) -> String {
        let now = crate::issue::time::now_epoch();
        let header = json!({"alg":"EdDSA","typ":"JWT","kid":KID});
        let claims = json!({
            "iss": self.issuer, "aud": self.public_host,
            "sub": "alice", "email": "alice@cad1485.example.test", "name": "Alice",
            "company": ORG, "role": "member", "iat": now, "exp": now + 60,
            "jti": format!("cad1485-sso-{}", uuid::Uuid::new_v4()),
        });
        let signed = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap()),
        );
        format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(self.key.sign(signed.as_bytes()).as_ref()),
        )
    }

    /// Browser-shaped HTTP request to the real board. Its Host/Origin name the
    /// public fixture identity; the loopback URL is only the socket destination.
    fn member_post(
        &self,
        path: &str,
        cookie: Option<&str>,
        body: &Value,
    ) -> ureq::http::Response<ureq::Body> {
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .proxy(None)
                .max_redirects(0)
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(20)))
                .build(),
        );
        let url = format!("http://127.0.0.1:{}{path}", self.board_port);
        let mut request = agent
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Host", &self.public_host)
            .header("Origin", &format!("http://{}", self.public_host))
            .header("Sec-Fetch-Site", "same-origin")
            .header("X-Cadence-Board", "1");
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        request
            .send_json(body)
            .expect("real board HTTP request failed")
    }

    fn authorize_as(&self, sub: &str) -> String {
        let envelope = self.envelope(sub, json!({}), json!({}));
        let mut authority = self.authority.lock().unwrap();
        authority.board_authorize = Some(BoardAuthorize {
            envelope: envelope.clone(),
            seen: Vec::new(),
        });
        envelope
    }

    fn assert_authority_wire(&self, token: &str, verb: &str) {
        let authority = self.authority.lock().unwrap();
        let fixture = authority.board_authorize.as_ref().unwrap();
        assert_eq!(
            fixture.seen.len(),
            1,
            "new native route did not request authority exactly once"
        );
        let seen = &fixture.seen[0];
        assert_eq!(seen.method, "POST");
        // Do not print session-cookie values, even on an assertion failure.
        assert!(
            seen.authorization == format!("BoardSession {token}"),
            "wrong authority possession proof"
        );
        assert!(
            !seen.cookie_present,
            "browser Cookie header reached the server issuer"
        );
        assert_eq!(
            seen.body,
            json!({"version": VERSION, "verb": verb}),
            "frozen paired wire changed"
        );
    }
}

/// Canonical effects, not auth bookkeeping or asynchronous diagnostic events.
/// Includes ALL native messages (hence Master too), bindings and agent state.
fn effects(shared: &crate::daemon::Shared) -> BTreeMap<String, Vec<Vec<rusqlite::types::Value>>> {
    let connection = shared.store.try_conn().unwrap();
    ["messages", "assistants", "assist_requests", "agents"]
        .into_iter()
        .map(|table| {
            let mut statement = connection
                .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .unwrap();
            let count = statement.column_count();
            let rows = statement
                .query_map([], |row| {
                    (0..count)
                        .map(|index| row.get::<_, rusqlite::types::Value>(index))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            (table.to_string(), rows)
        })
        .collect()
}

#[test]
fn member_chat_refuses_another_members_signed_authority_before_effect() {
    let _flag = AssistantFlagOn::arm();
    let rig = start(&["alice", "bob"]);

    // REAL signed SSO -> real daemon verification/JTI consumption -> actual
    // public cookie. Never directly insert an Auth row or fake session checks.
    let mut opened = rig.member_post(
        "/__platform/session",
        None,
        &json!({"assertion": rig.alice_sso()}),
    );
    assert_eq!(
        opened.status().as_u16(),
        200,
        "real Alice SSO exchange refused: {}",
        {
            let error: Value = opened.body_mut().read_json().unwrap_or(Value::Null);
            let code = match error["error"]["code"].as_str().unwrap_or_default() {
                "assertion_invalid" => "assertion_invalid",
                "assertion_replayed" => "assertion_replayed",
                "assertion_expired" => "assertion_expired",
                "audience_mismatch" => "audience_mismatch",
                "issuer_mismatch" => "issuer_mismatch",
                "not_a_member" => "not_a_member",
                "role_unmapped" => "role_unmapped",
                "capability_unavailable" => "capability_unavailable",
                _ => "unrecognized error (redacted)",
            };
            // Only fixed protocol diagnostics may enter logs; never echo a
            // response body, assertion, envelope or Set-Cookie value.
            let reason = match error["error"]["message"].as_str().unwrap_or_default() {
                "request body must be application/json" => "request body must be application/json",
                "request body is not valid JSON" => "request body is not valid JSON",
                _ => "detail redacted",
            };
            format!("{code}: {reason}")
        }
    );
    let cookie = opened
        .headers()
        .get("set-cookie")
        .expect("SSO returned no public cookie")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    assert!(cookie.starts_with("__Host-aos-board-session="));
    let token = cookie.split_once('=').unwrap().1;
    drop(opened);
    let checked = crate::client::rpc(
        rig._dir.path(),
        "board_session_check",
        json!({"token": token}),
    )
    .unwrap();
    assert_eq!(checked["valid"], true, "real public session is not live");
    assert_eq!(checked["session"]["user"]["sub"], "alice");
    assert_eq!(checked["session"]["origin"], "public");

    // Bob is a genuinely bound, live member too, not a bad signature or stale
    // credential. This existing real HTTP+daemon control uses a DIFFERENT JTI.
    let (status, body) = rig.call(
        "assist_state",
        &rig.envelope("bob", json!({}), json!({})),
        &json!({}),
    );
    assert_eq!(
        status, 200,
        "Bob's actual bound-member control refused: {body}"
    );

    // The NEW cookie-facing route must exist and reach Alice's own assistant.
    rig.authorize_as("alice");
    let mut own = rig.member_post("/api/member-assistant/state", Some(&cookie), &json!({}));
    assert_eq!(
        own.status().as_u16(),
        200,
        "own-state control did not reach the new route"
    );
    let own: Value = own.body_mut().read_json().unwrap();
    assert_eq!(
        own["assistant"]["alias"], "assist-agent-alice",
        "Alice state resolved another actor or Master: {own}"
    );
    rig.assert_authority_wire(token, "state");

    // A fresh Bob envelope independently passes the ACTUAL native signature,
    // issuer/audience/org/time/actor verifier, so only cross-member binding may
    // explain this denial. The fixture does not implement that guard.
    let bob = rig.authorize_as("bob");
    let key = crate::board_identity::PublicKey {
        kid: KID.to_string(),
        x: rig.key.public_key().as_ref().try_into().unwrap(),
    };
    let config = crate::board_identity::Config {
        host: rig.public_host.clone(),
        issuer: rig.issuer.clone(),
        company: ORG.to_string(),
    };
    let verified =
        crate::assistant_actor::verify(&bob, &config, &key, crate::issue::time::now_epoch())
            .expect("Bob fixture authority is not cryptographically valid");
    assert_eq!(verified.sub, "bob");

    let options = ServeOptions::default();
    options
        .provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &options).unwrap();
    // A regressed guard must fail the test without launching a real provider.
    // An operator pause retains sends with HTTP 200 (the existing parent-suite
    // acceptance pins that behavior), so it cannot explain our 401/403 result.
    for agent in shared.store.agents().unwrap() {
        shared.store.set_enabled(&agent.alias, false).unwrap();
        shared
            .store
            .event_public(&agent.alias, "stop_requested", json!({}))
            .unwrap();
    }
    let before = effects(&shared);
    let files_before = tree(&rig.pm);
    let mut refused = rig.member_post(
        "/api/member-assistant/send",
        Some(&cookie),
        &json!({"text":"cad1485-cross-member-refusal-marker"}),
    );
    assert!(
        matches!(refused.status().as_u16(), 401 | 403),
        "cross-member send was not an authorization refusal"
    );
    let refusal = refused.body_mut().read_to_string().unwrap();
    assert!(
        !refusal.contains(&bob),
        "signed member authority leaked to the browser"
    );
    assert!(
        !refusal.contains(token),
        "public session bearer leaked to the browser"
    );
    rig.assert_authority_wire(token, "send");
    assert_eq!(
        effects(&shared),
        before,
        "refused send changed a member, agent or Master delivery"
    );
    assert_eq!(
        tree(&rig.pm),
        files_before,
        "refused send changed member/Master files"
    );
}
