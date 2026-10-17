//! Verified consumer for `POST http://api.internal/v1/runtime/cadence/member/work`.
//!
//! The request is the existing board-membership check body: schema version,
//! subject, session digest and a fresh nonce. Company and instance come from
//! the daemon's provisioned context and are never sent. The proof is a compact
//! Ed25519 JWS. This module trusts a claim only after the signature matches
//! the board keyring and every binding matches this call.
//!
//! A missing membership (`role: null`), a bad signature, a stale window, a
//! malformed assignment or any transport failure is a refusal. An empty
//! assignment list is returned only from a verified owner or member proof.

use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::board_identity::{JwksCache, PublicKey};
use crate::board_membership::session_digest;
use crate::error::{Error, Result};

pub const PURPOSE: &str = "cadence-member-work/1";
const ISSUER: &str = "http://api.internal";
const WORK_URL: &str = "http://api.internal/v1/runtime/cadence/member/work";
const TOKEN_MAX: usize = 65_536;
const RESPONSE_MAX: u64 = 65_536;
const DEADLINE: Duration = Duration::from_secs(2);
const MAX_LIFE_SECS: i64 = 5;
const MAX_ASSIGNMENTS: usize = 50;
const VERSION: &str = "hosted-cadence-local-assignment.v1";

const STATES: &[&str] = &[
    "queued",
    "running",
    "blocked",
    "result",
    "failed",
    "cancelled",
];

/// Bindings the proof must repeat exactly. Company and instance are trusted
/// configuration. Subject and session come from the authenticated board session.
pub struct Expected<'a> {
    pub company: &'a str,
    pub instance: &'a str,
    pub sub: &'a str,
    pub session: &'a str,
    pub nonce: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorkItem {
    pub id: String,
    pub issue_id: String,
    pub project: String,
    pub action: String,
    pub state: String,
    pub generation: u64,
    pub created_by_you: bool,
    pub assigned_to_you: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedWork {
    pub role: &'static str,
    pub assigned: Vec<WorkItem>,
    pub created: Vec<WorkItem>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    alg: String,
    typ: String,
    kid: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    purpose: String,
    iss: String,
    aud: String,
    instance: String,
    sub: String,
    session: String,
    nonce: String,
    role: Value,
    iat: i64,
    exp: i64,
    jti: String,
    assignments: Vec<Assignment>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Assignment {
    version: String,
    assignment_id: String,
    organization_id: String,
    issue_id: String,
    project: String,
    action: String,
    execution: String,
    state: String,
    generation: u64,
    claim_id: Option<String>,
    target: Option<String>,
    created_by_subject_id: String,
    #[serde(default)]
    claim_subject_id: Option<String>,
    client_agent_id: Option<String>,
    #[serde(default)]
    guidance: Option<Value>,
    reported_head_sha: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    ok: bool,
    data: EnvelopeData,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvelopeData {
    proof: String,
}

#[derive(Serialize)]
struct CheckRequest {
    schema_version: u8,
    sub: String,
    session: String,
    nonce: String,
}

fn refused() -> Error {
    Error::invalid(
        "capability_unavailable",
        "hosted work could not be confirmed",
    )
}

fn bounded(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}

fn optional_bounded(s: &Option<String>, max: usize) -> bool {
    match s {
        None => true,
        Some(value) => value.is_empty() || bounded(value, max),
    }
}

fn lower_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn company_ok(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn instance_ok(s: &str) -> bool {
    (1..=12).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
}

fn expected_ok(e: &Expected<'_>) -> bool {
    company_ok(e.company)
        && instance_ok(e.instance)
        && bounded(e.sub, 128)
        && lower_hex(e.session, 64)
        && lower_hex(e.nonce, 32)
}

fn segment(part: &str) -> Option<Vec<u8>> {
    if part.is_empty()
        || !part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    URL_SAFE_NO_PAD.decode(part).ok()
}

struct Parts<'a> {
    signed: &'a str,
    header: Header,
    payload: Vec<u8>,
    signature: Vec<u8>,
}

fn split(token: &str) -> Option<Parts<'_>> {
    if token.is_empty() || token.len() > TOKEN_MAX {
        return None;
    }
    let mut it = token.split('.');
    let (h, p, s) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some() {
        return None;
    }
    let header: Header = serde_json::from_slice(&segment(h)?).ok()?;
    if header.alg != "EdDSA" || header.typ != "JWT" || !bounded(&header.kid, 128) {
        return None;
    }
    let payload = segment(p)?;
    let signature = segment(s)?;
    if signature.len() != 64 {
        return None;
    }
    Some(Parts {
        signed: &token[..h.len() + 1 + p.len()],
        header,
        payload,
        signature,
    })
}

fn project_ok(s: &str) -> bool {
    (1..=80).contains(&s.len())
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.'
        })
        && !s.starts_with('.')
}

fn assignment_ok(row: &Assignment, sub: &str) -> bool {
    if row.version != VERSION
        || row.execution != "local"
        || !STATES.contains(&row.state.as_str())
        || !bounded(&row.assignment_id, 128)
        || !bounded(&row.organization_id, 128)
        || !bounded(&row.issue_id, 128)
        || !project_ok(&row.project)
        || !bounded(&row.action, 64)
        || !bounded(&row.created_by_subject_id, 128)
        || !optional_bounded(&row.claim_id, 128)
        || !optional_bounded(&row.target, 128)
        || !optional_bounded(&row.claim_subject_id, 128)
        || !optional_bounded(&row.client_agent_id, 128)
        || !optional_bounded(&row.reported_head_sha, 64)
        || row.guidance.is_some()
    {
        return false;
    }
    if let Some(text) = &row.text {
        if text.len() > 4_000 || text.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
            return false;
        }
    }
    let assigned = row.target.as_deref() == Some(sub) || row.claim_subject_id.as_deref() == Some(sub);
    let created = row.created_by_subject_id == sub;
    assigned || created
}

fn item_from(row: &Assignment, sub: &str) -> WorkItem {
    let assigned = row.target.as_deref() == Some(sub) || row.claim_subject_id.as_deref() == Some(sub);
    let created = row.created_by_subject_id == sub;
    WorkItem {
        id: row.assignment_id.clone(),
        issue_id: row.issue_id.clone(),
        project: row.project.clone(),
        action: row.action.clone(),
        state: row.state.clone(),
        generation: row.generation,
        created_by_you: created,
        assigned_to_you: assigned,
    }
}

/// Verify one work proof. `Ok` is a verified owner or member list, which may
/// be empty. Absence, a bad signature and a malformed row are all `Err`.
pub fn verify_work(
    token: &str,
    expected: &Expected<'_>,
    key: &PublicKey,
    now: i64,
) -> Result<VerifiedWork> {
    if !expected_ok(expected) {
        return Err(refused());
    }
    let parts = split(token).ok_or_else(refused)?;
    if parts.header.kid != key.kid {
        return Err(refused());
    }
    UnparsedPublicKey::new(&ED25519, &key.x)
        .verify(parts.signed.as_bytes(), &parts.signature)
        .map_err(|_| refused())?;
    let claims: Claims = serde_json::from_slice(&parts.payload).map_err(|_| refused())?;
    if claims.purpose != PURPOSE
        || claims.iss != ISSUER
        || claims.aud != expected.company
        || claims.instance != expected.instance
        || claims.sub != expected.sub
        || claims.session != expected.session
        || claims.nonce != expected.nonce
        || !bounded(&claims.jti, 128)
        || claims.assignments.len() > MAX_ASSIGNMENTS
    {
        return Err(refused());
    }
    let life = claims.exp.checked_sub(claims.iat).ok_or_else(refused)?;
    if life <= 0 || life > MAX_LIFE_SECS || claims.iat > now || now >= claims.exp {
        return Err(refused());
    }
    let role = match claims.role.as_str() {
        Some("owner") => "owner",
        Some("member") => "member",
        _ => return Err(refused()),
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut assigned = Vec::new();
    let mut created = Vec::new();
    for row in &claims.assignments {
        if !assignment_ok(row, expected.sub) || !seen.insert(row.assignment_id.clone()) {
            return Err(refused());
        }
        let item = item_from(row, expected.sub);
        if item.assigned_to_you {
            assigned.push(item.clone());
        }
        if item.created_by_you {
            created.push(item);
        }
    }
    Ok(VerifiedWork {
        role,
        assigned,
        created,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn now_unix() -> Result<i64> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| refused())?
        .as_secs();
    i64::try_from(secs).map_err(|_| refused())
}

fn check_request(session: &str, sub: &str) -> Result<CheckRequest> {
    if !lower_hex(session, 64) || !bounded(sub, 128) {
        return Err(refused());
    }
    let mut nonce = [0u8; 16];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| refused())?;
    Ok(CheckRequest {
        schema_version: 1,
        sub: sub.to_string(),
        session: session.to_string(),
        nonce: hex(&nonce),
    })
}

/// Fresh POST and strict verification for the authenticated subject.
/// The raw cookie is hashed here and is not sent.
pub fn lookup_member_work(
    raw_cookie: &str,
    authenticated_sub: &str,
    trusted_company: &str,
    trusted_instance: &str,
    jwks: &Mutex<JwksCache>,
) -> Result<VerifiedWork> {
    if raw_cookie.is_empty() {
        return Err(refused());
    }
    let request = check_request(&session_digest(raw_cookie), authenticated_sub)?;
    let expected = Expected {
        company: trusted_company,
        instance: trusted_instance,
        sub: authenticated_sub,
        session: &request.session,
        nonce: &request.nonce,
    };
    if !expected_ok(&expected) {
        return Err(refused());
    }
    let agent = crate::issuer_transport::agent(
        ureq::Agent::config_builder()
            .timeout_global(Some(DEADLINE))
            .http_status_as_error(false),
    );
    let mut resp = agent
        .post(WORK_URL)
        .send_json(&request)
        .map_err(|_| refused())?;
    if resp.status() != 200 {
        return Err(refused());
    }
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(RESPONSE_MAX)
        .read_to_vec()
        .map_err(|_| refused())?;
    let env: Envelope = serde_json::from_slice(&bytes).map_err(|_| refused())?;
    if !env.ok {
        return Err(refused());
    }
    let token = env.data.proof;
    let kid = split(&token).ok_or_else(refused)?.header.kid;
    let key = {
        let mut cache = jwks.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_unix()?;
        cache.key(ISSUER, &kid, now).map_err(|_| refused())?
    };
    let now = now_unix()?;
    verify_work(&token, &expected, &key, now)
}

/// Company and instance captured the same way as native membership.
/// Absent or partial placement means this board has no hosted work channel.
pub fn hosted_binding(read: &dyn Fn(&str) -> Option<String>) -> Option<(String, String)> {
    let company = read("AGENTICOS_COMPANY").filter(|s| !s.is_empty())?;
    let instance = read("AGENTICOS_INSTANCE").filter(|s| !s.is_empty())?;
    if company_ok(&company) && instance_ok(&instance) {
        Some((company, instance))
    } else {
        None
    }
}

pub fn unavailable_body() -> Value {
    json!({
        "ok": false,
        "code": "hosted_work_unavailable",
        "message": "Hosted work is unavailable for this board."
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    fn keypair() -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(&[7u8; 32]).unwrap()
    }

    fn public(kp: &Ed25519KeyPair) -> PublicKey {
        let bytes = kp.public_key().as_ref();
        let mut x = [0u8; 32];
        x.copy_from_slice(bytes);
        PublicKey {
            kid: "work-kid".into(),
            x,
        }
    }

    fn sign(kp: &Ed25519KeyPair, claims: &Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"EdDSA","typ":"JWT","kid":"work-kid"}"#);
        let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
        let signed = format!("{header}.{payload}");
        let signature = URL_SAFE_NO_PAD.encode(kp.sign(signed.as_bytes()).as_ref());
        format!("{signed}.{signature}")
    }

    fn sample<'a>(session: &'a str, nonce: &'a str) -> Expected<'a> {
        Expected {
            company: "acme",
            instance: "ab12",
            sub: "user-1",
            session,
            nonce,
        }
    }
    fn expected() -> Expected<'static> {
        use std::sync::LazyLock;
        static SESSION: LazyLock<String> = LazyLock::new(|| "ab".repeat(32));
        static NONCE: LazyLock<String> = LazyLock::new(|| "cd".repeat(16));
        sample(&SESSION, &NONCE)
    }
    fn row() -> Value {
        json!({
            "version": VERSION,
            "assignment_id": "hla-1",
            "organization_id": "org-1",
            "issue_id": "CAD-1",
            "project": "cadence",
            "action": "dispatch",
            "execution": "local",
            "state": "queued",
            "generation": 1,
            "claim_id": null,
            "target": "user-1",
            "created_by_subject_id": "user-2",
            "claim_subject_id": null,
            "client_agent_id": null,
            "guidance": null,
            "reported_head_sha": null,
            "text": null
        })
    }

    fn claims(role: Value, assignments: Value) -> Value {
        json!({
            "purpose": PURPOSE,
            "iss": ISSUER,
            "aud": "acme",
            "instance": "ab12",
            "sub": "user-1",
            "session": "ab".repeat(32),
            "nonce": "cd".repeat(16),
            "role": role,
            "iat": 1_700_000_000,
            "exp": 1_700_000_004,
            "jti": "jti-1",
            "assignments": assignments
        })
    }

    #[test]
    fn verified_member_sees_only_their_assignment() {
        let kp = keypair();
        let token = sign(&kp, &claims(json!("member"), json!([row()])));
        let work = verify_work(&token, &expected(), &public(&kp), 1_700_000_001).unwrap();
        assert_eq!(work.role, "member");
        assert_eq!(work.assigned.len(), 1);
        assert!(work.created.is_empty());
        assert_eq!(work.assigned[0].state, "queued");
        assert!(work.assigned[0].assigned_to_you);
        assert!(!work.assigned[0].created_by_you);
    }

    #[test]
    fn unsigned_wrong_purpose_expired_null_role_and_stranger_fail() {
        let kp = keypair();
        let key = public(&kp);
        let now = 1_700_000_001;
        let good = sign(&kp, &claims(json!("owner"), json!([])));
        assert!(verify_work("not-a-proof", &expected(), &key, now).is_err());
        let mut flipped = good.clone().into_bytes();
        let last = flipped.len() - 1;
        flipped[last] ^= 0x1;
        let bad_sig = String::from_utf8(flipped).unwrap();
        assert!(verify_work(&bad_sig, &expected(), &key, now).is_err());

        let mut wrong = claims(json!("member"), json!([]));
        wrong["purpose"] = json!("cadence-board-membership/1");
        assert!(verify_work(&sign(&kp, &wrong), &expected(), &key, now).is_err());

        let mut stale = claims(json!("member"), json!([]));
        stale["iat"] = json!(1_700_000_000);
        stale["exp"] = json!(1_700_000_001);
        assert!(verify_work(&sign(&kp, &stale), &expected(), &key, 1_700_000_001).is_err());

        assert!(verify_work(
            &sign(&kp, &claims(Value::Null, json!([]))),
            &expected(),
            &key,
            now
        )
        .is_err());

        let mut foreign = row();
        foreign["target"] = json!("user-9");
        foreign["created_by_subject_id"] = json!("user-8");
        assert!(verify_work(
            &sign(&kp, &claims(json!("owner"), json!([foreign]))),
            &expected(),
            &key,
            now
        )
        .is_err());
    }

    #[test]
    fn guidance_and_unknown_fields_fail_closed() {
        let kp = keypair();
        let key = public(&kp);
        let mut guided = row();
        guided["guidance"] = json!({"token": "secret"});
        assert!(verify_work(
            &sign(&kp, &claims(json!("member"), json!([guided]))),
            &expected(),
            &key,
            1_700_000_001
        )
        .is_err());
        let mut extra = claims(json!("member"), json!([]));
        extra["bearer"] = json!("nope");
        assert!(verify_work(&sign(&kp, &extra), &expected(), &key, 1_700_000_001).is_err());
    }
}
