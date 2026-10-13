//! Independent acceptance for CAD-1374's consumer-side membership-proof
//! cryptographic boundary. The issuer is simulated only by deterministic
//! Ed25519 signatures; the verifier and its real refusal path are production
//! code. This does not exercise transport, JWKS retrieval, or browser authority.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cadence_agent::board_identity::{PublicKey, Role};
use cadence_agent::board_membership::{self, Expected};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};

const KID: &str = "membership-acceptance-key";
const COMPANY: &str = "co_acceptance";
const INSTANCE: &str = "inst123";
const SUBJECT: &str = "user-acceptance";
const SESSION: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const NONCE: &str = "0123456789abcdef0123456789abcdef";
const NOW: i64 = 1_700_000_000;
const COOKIE_CANARY: &str = "raw-cookie-canary-not-for-errors";

fn signer() -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[0x37; 32]).expect("synthetic Ed25519 seed")
}

fn keypair_public(signer: &Ed25519KeyPair, kid: &str) -> PublicKey {
    PublicKey {
        kid: kid.to_owned(),
        x: signer
            .public_key()
            .as_ref()
            .try_into()
            .expect("32-byte Ed25519 key"),
    }
}

fn expected() -> Expected<'static> {
    Expected {
        company: COMPANY,
        instance: INSTANCE,
        sub: SUBJECT,
        session: SESSION,
        nonce: NONCE,
    }
}

fn claims(role: Value) -> Value {
    json!({
        "purpose": "cadence-board-membership/1",
        "iss": "http://api.internal",
        "aud": COMPANY,
        "instance": INSTANCE,
        "sub": SUBJECT,
        "session": SESSION,
        "nonce": NONCE,
        "role": role,
        "iat": NOW - 1,
        "exp": NOW + 4,
        "jti": "acceptance-proof-1"
    })
}

fn segment(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn signed_raw(signer: &Ed25519KeyPair, header: &str, payload: &str) -> String {
    let signing_input = format!(
        "{}.{}",
        segment(header.as_bytes()),
        segment(payload.as_bytes())
    );
    format!(
        "{signing_input}.{}",
        segment(signer.sign(signing_input.as_bytes()).as_ref())
    )
}

fn signed(signer: &Ed25519KeyPair, claims: &Value) -> String {
    signed_raw(
        signer,
        &json!({"alg":"EdDSA", "typ":"JWT", "kid":KID}).to_string(),
        &claims.to_string(),
    )
}

fn assert_refused(token: &str, expected: &Expected<'_>, key: &PublicKey) {
    assert!(
        board_membership::verify_proof(token, expected, key, NOW).is_err(),
        "proof unexpectedly accepted: {token}"
    );
}

#[test]
fn signed_membership_proofs_map_owner_member_and_authenticated_absence() {
    let signer = signer();
    let key = keypair_public(&signer, KID);
    for (role, want) in [
        (json!("owner"), Some(Role::Operator)),
        (json!("member"), Some(Role::Member)),
        (Value::Null, None),
    ] {
        let token = signed(&signer, &claims(role));
        assert_eq!(
            board_membership::verify_proof(&token, &expected(), &key, NOW).unwrap(),
            want
        );
    }
}

#[test]
fn signed_mismatches_and_malformed_claim_sets_are_refused() {
    let signer = signer();
    let key = keypair_public(&signer, KID);
    let mut invalid = Vec::new();
    for (field, value) in [
        ("aud", json!("other-company")),
        ("instance", json!("otherinst")),
        ("sub", json!("other-user")),
        ("session", json!("f".repeat(64))),
        ("nonce", json!("e".repeat(32))),
        ("purpose", json!("cadence-board-assertion/1")),
        ("iss", json!("https://untrusted.example")),
        ("role", json!("administrator")),
    ] {
        let mut c = claims(json!("member"));
        c[field] = value;
        invalid.push(signed(&signer, &c));
    }
    let mut missing_role = claims(json!("member"));
    missing_role.as_object_mut().unwrap().remove("role");
    invalid.push(signed(&signer, &missing_role));

    let mut unknown_claim = claims(json!("member"));
    unknown_claim["extra"] = json!(true);
    invalid.push(signed(&signer, &unknown_claim));
    // Duplicate JSON keys are signed as serialized bytes, not normalized through Value.
    invalid.push(signed_raw(
        &signer,
        &json!({"alg":"EdDSA", "typ":"JWT", "kid":KID}).to_string(),
        &format!(
            "{},\"role\":\"owner\"}}",
            claims(json!("member")).to_string().trim_end_matches('}')
        ),
    ));
    invalid.push(signed_raw(
        &signer,
        &json!({"alg":"EdDSA", "typ":"JWT", "kid":KID}).to_string(),
        "{malformed-json",
    ));

    for token in invalid {
        assert_refused(&token, &expected(), &key);
    }
}

#[test]
fn signed_time_bounds_and_overflow_are_refused() {
    let signer = signer();
    let key = keypair_public(&signer, KID);
    for (iat, exp) in [
        (NOW - 1, NOW),       // exact now == exp is expired
        (NOW + 1, NOW + 4),   // future iat
        (NOW - 1, NOW + 5),   // lifetime exceeds five seconds
        (i64::MIN, i64::MAX), // subtraction overflow
        (i64::MAX, i64::MIN), // subtraction overflow
    ] {
        let mut c = claims(json!("member"));
        c["iat"] = json!(iat);
        c["exp"] = json!(exp);
        assert_refused(&signed(&signer, &c), &expected(), &key);
    }
}

#[test]
fn jws_shape_signature_key_and_old_assertion_are_not_membership_proofs() {
    let signer = signer();
    let key = keypair_public(&signer, KID);
    let valid = signed(&signer, &claims(json!("member")));

    let wrong_kid = keypair_public(&signer, "different-kid");
    assert_refused(&valid, &expected(), &wrong_kid);
    let other_signer = Ed25519KeyPair::from_seed_unchecked(&[0x38; 32]).unwrap();
    assert_refused(&valid, &expected(), &keypair_public(&other_signer, KID));

    let mut pieces: Vec<_> = valid.split('.').map(str::to_owned).collect();
    pieces[2] = segment(&[0u8; 64]);
    assert_refused(&pieces.join("."), &expected(), &key);
    assert_refused(
        &signed_raw(
            &signer,
            r#"{"alg":"none","typ":"JWT","kid":"membership-acceptance-key"}"#,
            &claims(json!("member")).to_string(),
        ),
        &expected(),
        &key,
    );
    assert_refused(
        &signed_raw(
            &signer,
            r#"{"alg":"HS256","typ":"JWT","kid":"membership-acceptance-key"}"#,
            &claims(json!("member")).to_string(),
        ),
        &expected(),
        &key,
    );
    assert_refused(
        &signed_raw(
            &signer,
            r#"{"alg":"EdDSA","typ":"JWT","kid":"membership-acceptance-key","extra":true}"#,
            &claims(json!("member")).to_string(),
        ),
        &expected(),
        &key,
    );
    assert_refused(
        &signed_raw(
            &signer,
            r#"{"alg":"EdDSA","alg":"none","typ":"JWT","kid":"membership-acceptance-key"}"#,
            &claims(json!("member")).to_string(),
        ),
        &expected(),
        &key,
    );

    let old_board_assertion = json!({
        "iss":"http://api.internal", "aud":"board.example", "sub":SUBJECT,
        "email":"user@example.test", "name":"User", "company":COMPANY,
        "role":"owner", "iat":NOW-1, "exp":NOW+4, "jti":"old-assertion"
    });
    assert_refused(&signed(&signer, &old_board_assertion), &expected(), &key);

    assert_refused(&format!("{valid}A"), &expected(), &key);
    assert_refused(&format!("{}.x.y", "A".repeat(4097)), &expected(), &key);

    // Verification is a pure binding check, not a replay cache. A proof
    // bound to the previous nonce cannot satisfy a newly expected nonce.
    let proof = signed(&signer, &claims(json!("member")));
    assert_eq!(
        board_membership::verify_proof(&proof, &expected(), &key, NOW).unwrap(),
        Some(Role::Member)
    );
    assert_eq!(
        board_membership::verify_proof(&proof, &expected(), &key, NOW).unwrap(),
        Some(Role::Member)
    );
    let new_nonce = "fedcba9876543210fedcba9876543210";
    let changed = Expected {
        nonce: new_nonce,
        ..expected()
    };
    assert_refused(&proof, &changed, &key);
}

#[test]
fn refusal_text_never_discloses_proof_or_canary_material() {
    let signer = signer();
    let key = keypair_public(&signer, KID);
    let mut c = claims(json!("unexpected-role"));
    c["jti"] = json!(COOKIE_CANARY);
    let token = signed(&signer, &c);
    let error = board_membership::verify_proof(&token, &expected(), &key, NOW).unwrap_err();
    let text = format!("{error:?} {error}");
    assert!(!text.contains(&token));
    assert!(!text.contains(COOKIE_CANARY));
    assert!(!text.contains("unexpected-role"));
}
