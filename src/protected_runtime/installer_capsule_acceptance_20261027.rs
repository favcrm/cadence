//! Independent CAD-1085 acceptance: the installer capsule parser refuses every
//! shape that is not the exact owner-written capsule for this fixed profile,
//! before any supervisor socket is touched.
//!
//! Child module of `protected_runtime::installer`; uses the in-memory profile
//! fixture and the real strict parser.

use serde_json::{json, Value};

use super::parse_capsule;
use crate::protected_runtime::profile::LocalProfile;

const RUNTIME_ID: &str = "01234567-89ab-4cde-8f01-23456789abcd";
const OPERATION_ID: &str = "fedcba98-7654-4321-8fed-cba987654321";
const NONCE: &str = "00112233-4455-4677-8899-aabbccddeeff";

fn profile() -> LocalProfile {
    LocalProfile::test_fixture([0x11; 32], 21000, 21000, "a".repeat(32))
}

fn capsule(profile: &LocalProfile) -> Value {
    json!({
        "v": 1,
        "kind": "owner_launched_install",
        "runtime_id": RUNTIME_ID,
        "operation_id": OPERATION_ID,
        "generation": 1,
        "installer_generation": profile.installer().generation.as_str(),
        "recipient_generation": "b".repeat(32),
        "recipient_nonce": NONCE,
        "profile_digest": profile.digest(),
        "grant_socket": profile.supervisor_grant_socket().to_string_lossy(),
        "challenge": {},
    })
}

fn frame(value: &Value) -> Vec<u8> {
    let mut body = serde_json::to_vec(value).expect("fixture capsule json");
    body.push(b'\n');
    body
}

#[test]
fn empty_capsule_is_refused() {
    let profile = profile();
    parse_capsule(b"", &profile)
        .err()
        .expect("an empty capsule must be refused");
}

#[test]
fn capsule_without_a_newline_is_refused() {
    let profile = profile();
    let mut body = frame(&capsule(&profile));
    body.pop();
    parse_capsule(&body, &profile)
        .err()
        .expect("a capsule without a newline must be refused");
}

#[test]
fn capsule_with_embedded_newlines_is_refused() {
    let profile = profile();
    let mut body = frame(&capsule(&profile));
    body.insert(0, b'\n');
    parse_capsule(&body, &profile)
        .err()
        .expect("a multi-line capsule must be refused");
}

#[test]
fn wrong_version_or_kind_is_refused() {
    let profile = profile();
    let mut value = capsule(&profile);
    value["kind"] = json!("owner_launched_install_extra");
    let error = parse_capsule(&frame(&value), &profile)
        .err()
        .expect("wrong kind must be refused");
    assert!(
        error.to_string().contains("version/kind"),
        "unexpected refusal: {error}"
    );
}

#[test]
fn a_foreign_profile_digest_is_refused() {
    let profile = profile();
    let mut value = capsule(&profile);
    value["profile_digest"] = json!(format!("sha256:{}", "f".repeat(64)));
    let error = parse_capsule(&frame(&value), &profile)
        .err()
        .expect("a foreign profile must be refused");
    assert!(
        error.to_string().contains("profile digest"),
        "unexpected refusal: {error}"
    );
}

#[test]
fn a_foreign_installer_generation_is_refused() {
    let profile = profile();
    let mut value = capsule(&profile);
    value["installer_generation"] = json!("c".repeat(32));
    let error = parse_capsule(&frame(&value), &profile)
        .err()
        .expect("a foreign installer generation must be refused");
    assert!(
        error.to_string().contains("generation is not pinned"),
        "unexpected refusal: {error}"
    );
}

#[test]
fn a_foreign_grant_socket_is_refused() {
    let profile = profile();
    let mut value = capsule(&profile);
    value["grant_socket"] = json!("/tmp/not-the-fixed-supervisor.sock");
    let error = parse_capsule(&frame(&value), &profile)
        .err()
        .expect("a foreign socket must be refused");
    assert!(
        error.to_string().contains("grant socket"),
        "unexpected refusal: {error}"
    );
}

/// Every earlier gate passes and the parse stops at the challenge, which is
/// the only value a capsule cannot omit or forge.
#[test]
fn a_capsule_without_a_real_challenge_is_refused() {
    let profile = profile();
    let error = parse_capsule(&frame(&capsule(&profile)), &profile)
        .err()
        .expect("an empty challenge must be refused");
    let text = error.to_string();
    assert!(
        !text.contains("profile digest")
            && !text.contains("generation is not pinned")
            && !text.contains("grant socket"),
        "the parse must reach the challenge, got: {text}"
    );
}
