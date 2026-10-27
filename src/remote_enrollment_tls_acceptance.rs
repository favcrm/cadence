//! CAD-1434 / AOS-243: TLS slice of the issuer renewal path.
//!
//! The renewal POST runs through the real `cli_session_post_detailed`: origin
//! and HTTPS-only check, operator pin, Access ingress, deadline, redirect
//! refusal, response-size cap, JSON parse and access re-confirm, then the real
//! `send_issuer_request` over `ureq` TLS. Only the agent's trust roots and the
//! loopback resolver come from the test fixture (`issuer_transport`). Server
//! certificates are verified normally against the ephemeral CA.
//!
//! Negative cases use a wrong CA, a wrong hostname and an unpinned issuer, and
//! must fail before any mutation. The fixture records requests so a refused
//! case can show that no request reached the issuer.

use super::*;
use crate::issuer_tls_fixture::{TlsIssuer, HOST};
use crate::issuer_transport::test_fixture::{self, Fixture};

const CORRELATION: &str = "0b2e1f4a-7c3d-4e5f-8a9b-1c2d3e4f5a6b";

fn trust(dir: &Path, issuer: &str) {
    fs::create_dir(dir).unwrap();
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(dir.join(TRUSTED_ISSUER), format!("{issuer}\n")).unwrap();
    fs::set_permissions(dir.join(TRUSTED_ISSUER), fs::Permissions::from_mode(0o600)).unwrap();
}

fn access_file(dir: &Path, issuer: &str) {
    fs::write(
        dir.join(ACCESS_INGRESS),
        json!({"issuer":issuer,"client_id":"test-id.access","client_secret":"test-secret"})
            .to_string(),
    )
    .unwrap();
    fs::set_permissions(dir.join(ACCESS_INGRESS), fs::Permissions::from_mode(0o600)).unwrap();
}

/// A renewal answer the issuer would give for a live session (the real
/// success shape is checked by the renewal state machine, not here).
fn renewed_answer() -> (u16, String) {
    (
        200,
        json!({"version":"hosted-cadence-auth.v1","organization_id":"ws_tls",
               "audience":"https://tls.board.example.test","principal":{"kind":"user",
               "current_role":"owner","subject_id":"usr_tls"},"capabilities":["cli.read"],
               "credential":{"token_type":"Bearer","access_token":"hct_x","credential_id":"hcp_x",
               "issued_at":1,"expires_at":2,"renewal":"reexchange"},"cli_session":{}})
        .to_string(),
    )
}

fn fresh_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("enrollment");
    (root, dir)
}

/// Run the real renewal POST with the fixture trust installed for this thread.
fn post_renewal(
    dir: &Path,
    origin: &str,
    server: &TlsIssuer,
    trusted_ca: &TlsIssuer,
) -> Result<IssuerAnswer> {
    // The loopback is the server the origin must reach; the CA is the one the
    // client trusts. They are separate so a wrong-CA case really reaches `server`.
    let _guard = test_fixture::install(Fixture {
        ca_der: trusted_ca.ca_der().to_vec(),
        loopback: server.addr(),
    });
    cli_session_post_detailed(
        dir,
        origin,
        "/v1/hosted-cadence/cli-session/renew",
        "hcr_TESTREFRESHTOKEN",
        json!({"version":"hosted-cadence-cli-session.v1"}),
        Instant::now() + Duration::from_secs(10),
    )
}

#[test]
fn renewal_over_verified_tls_carries_the_issuer_correlation_and_strips_nothing() {
    // The success answer carries the issuer's correlation headers; the real
    // send path must read them before the body is consumed.
    let issuer = TlsIssuer::start(|_| renewed_answer());
    let (_root, dir) = fresh_dir();
    trust(&dir, &issuer.origin());
    access_file(&dir, &issuer.origin());

    let answer =
        post_renewal(&dir, &issuer.origin(), &issuer, &issuer).expect("verified TLS renewal");
    assert_eq!(answer.status, 200);
    assert_eq!(answer.body["version"], "hosted-cadence-auth.v1");
    let seen = issuer.seen();
    assert_eq!(seen.len(), 1, "exactly one request reached the issuer");
    assert_eq!(seen[0].path, "/v1/hosted-cadence/cli-session/renew");
    assert!(
        seen[0].header_names.iter().any(|n| n == "authorization"),
        "bearer rode the TLS request"
    );
    assert!(
        seen[0]
            .header_names
            .iter()
            .any(|n| n == "cf-access-client-id"),
        "Access ingress header rode the TLS request"
    );
}

#[test]
fn a_refusal_over_tls_keeps_the_validated_issuer_correlation() {
    // A refusal with the exact issuer correlation headers, sent over TLS.
    let extra = format!("x-cadence-hop: issuer\r\nx-cadence-request-id: {CORRELATION}\r\n");
    let issuer = TlsIssuer::start_with_headers(move |_| {
        (
            400,
            extra.clone(),
            json!({"error":"invalid_grant"}).to_string(),
        )
    });
    let (_root, dir) = fresh_dir();
    trust(&dir, &issuer.origin());
    access_file(&dir, &issuer.origin());
    let answer =
        post_renewal(&dir, &issuer.origin(), &issuer, &issuer).expect("verified TLS refusal");
    assert_eq!(answer.status, 400);
    assert_eq!(answer.body["error"], "invalid_grant");
    assert_eq!(issuer.seen().len(), 1);
    // The validated correlation read off the real TLS response headers.
    let correlation = answer
        .correlation
        .expect("issuer hop and uuid read over TLS");
    assert_eq!(correlation.request_id, CORRELATION);
}

#[test]
fn a_wrong_ca_is_refused_by_normal_verification_before_any_request_is_answered() {
    // The server presents a chain from issuer A; the client trusts CA B only.
    let served = TlsIssuer::start(|_| renewed_answer());
    let other = TlsIssuer::start(|_| renewed_answer());
    let (_root, dir) = fresh_dir();
    trust(&dir, &served.origin());
    access_file(&dir, &served.origin());
    // Trust the OTHER fixture's CA while talking to `served`.
    let err = post_renewal(&dir, &served.origin(), &served, &other);
    assert!(
        err.is_err(),
        "a certificate from an untrusted CA must be refused"
    );
    assert!(
        served.seen().is_empty(),
        "no request body or bearer may reach an untrusted-CA peer"
    );
}

#[test]
fn an_unpinned_issuer_is_refused_before_the_wire() {
    let issuer = TlsIssuer::start(|_| renewed_answer());
    let (_root, dir) = fresh_dir();
    // Pin a different origin than the one we request.
    trust(&dir, "https://other.board.localhost:9");
    access_file(&dir, "https://other.board.localhost:9");
    let err = post_renewal(&dir, &issuer.origin(), &issuer, &issuer);
    assert!(err.is_err(), "an unpinned issuer must be refused");
    assert!(
        issuer.seen().is_empty(),
        "unpinned renewal must not reach the wire"
    );
}

#[test]
fn a_plain_http_issuer_is_refused_before_the_wire() {
    let issuer = TlsIssuer::start(|_| renewed_answer());
    let (_root, dir) = fresh_dir();
    let http = format!("http://{HOST}:{}", issuer.addr().port());
    trust(&dir, &http);
    access_file(&dir, &http);
    let err = post_renewal(&dir, &http, &issuer, &issuer);
    assert!(err.is_err(), "http must never be sent");
    assert!(issuer.seen().is_empty());
}

#[test]
fn a_wrong_hostname_is_refused_by_name_verification() {
    // The fixture certificate is valid only for HOST. Request a different
    // name that resolves to the same loopback socket.
    let issuer = TlsIssuer::start(|_| renewed_answer());
    let (_root, dir) = fresh_dir();
    let wrong = format!("https://wrong.board.localhost:{}", issuer.addr().port());
    trust(&dir, &wrong);
    access_file(&dir, &wrong);
    let err = post_renewal(&dir, &wrong, &issuer, &issuer);
    assert!(
        err.is_err(),
        "a certificate for another name must be refused"
    );
    assert!(issuer.seen().is_empty());
}
