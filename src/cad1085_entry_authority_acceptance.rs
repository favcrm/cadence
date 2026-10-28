//! CAD-1085 retained acceptance check for the credential-entry auth exchange.
//!
//! Written by the independent acceptance lane (not by the auth implementer) and
//! must not be edited or weakened by the implementer. It exercises the real
//! typed guard in [`crate::operator_auth`] — no test assertion, fixture daemon
//! or caller field substitutes for it:
//!
//! * an exchanged entry nonce whose submitted request selector differs from the
//!   stored one is refused, creates no session, and is spent (single use);
//! * replaying that nonce with the original selector is refused afterwards;
//! * the only successful exchange is the exact stored selector, the resulting
//!   session is entry-scope only (never an ordinary board session), a board
//!   link cannot become entry authority, and an entry link cannot become a
//!   board session.
//!
//! The bad case is the security property, so this check fails if any refused
//! exchange leaves a session behind or the spent nonce can authorize again.

use crate::operator_auth::{Auth, LinkRefusal, LinkScope, Origin};

const NOW: i64 = 1_800_000_000;
const ENTRY_ORIGIN: &str = "https://entry.acceptance.invalid:3143";
const OTHER_ENTRY_ORIGIN: &str = "https://entry-two.acceptance.invalid:3143";
const REQUEST_A: &str = "11111111-1111-4111-8111-111111111111";
const REQUEST_B: &str = "22222222-2222-4222-8222-222222222222";

fn entry(origin: &str, request_id: &str) -> LinkScope {
    LinkScope::Entry {
        origin: origin.to_string(),
        request_id: request_id.to_string(),
    }
}

fn session_file(root: &std::path::Path) -> std::path::PathBuf {
    root.join("operator").join("sessions.json")
}

#[test]
fn altered_request_selector_cannot_mint_entry_authority_and_spends_the_nonce() {
    let root = tempfile::TempDir::new().unwrap();
    let mut auth = Auth::load(root.path());
    let stored = entry(ENTRY_ORIGIN, REQUEST_A);
    let nonce = auth
        .mint_scoped(Origin::Loopback, stored.clone(), NOW)
        .unwrap();

    // Correct origin and LinkScope kind, but a different (real) request id.
    let altered = auth.open_scoped(
        &nonce,
        Origin::Loopback,
        entry(ENTRY_ORIGIN, REQUEST_B),
        "acceptance",
        NOW,
    );
    assert!(matches!(altered, Err(LinkRefusal::WrongOrigin)));
    assert!(
        !session_file(root.path()).exists(),
        "a refused altered-selector exchange persisted a session"
    );

    // The mismatch spent the nonce: the stored selector can no longer be used.
    let replay = auth.open_scoped(&nonce, Origin::Loopback, stored, "acceptance", NOW);
    assert!(matches!(replay, Err(LinkRefusal::AlreadyUsed)));
    assert!(
        !session_file(root.path()).exists(),
        "a spent nonce minted a session on replay"
    );
}

#[test]
fn entry_scope_is_distinct_and_only_the_exact_stored_audience_mints() {
    let root = tempfile::TempDir::new().unwrap();
    let mut auth = Auth::load(root.path());
    let scope = entry(ENTRY_ORIGIN, REQUEST_A);
    let nonce = auth
        .mint_scoped(Origin::Loopback, scope.clone(), NOW)
        .unwrap();
    let (opened, stored) = auth
        .open_scoped(&nonce, Origin::Loopback, scope.clone(), "acceptance", NOW)
        .unwrap()
        .unwrap();
    assert_eq!(stored, scope);
    assert!(auth
        .check_entry(&opened.token, &opened.key, ENTRY_ORIGIN, NOW)
        .unwrap()
        .is_some());
    // A copied entry-origin token is not an ordinary board session, and it is
    // not a session for a different pinned entry origin either.
    assert!(auth
        .check(&opened.token, &opened.key, Origin::Loopback, NOW)
        .unwrap()
        .is_none());
    assert!(auth
        .check_entry(&opened.token, &opened.key, OTHER_ENTRY_ORIGIN, NOW)
        .unwrap()
        .is_none());

    // An ordinary board link can never be exchanged as entry authority.
    let board = auth.mint(Origin::Loopback, NOW).unwrap();
    let mixed = auth.open_scoped(
        &board,
        Origin::Loopback,
        entry(ENTRY_ORIGIN, REQUEST_A),
        "acceptance",
        NOW,
    );
    assert!(matches!(mixed, Err(LinkRefusal::WrongOrigin)));

    // An entry link can never be exchanged as ordinary board authority.
    let entry_nonce = auth
        .mint_scoped(Origin::Loopback, entry(ENTRY_ORIGIN, REQUEST_A), NOW)
        .unwrap();
    let board_attempt = auth.open_scoped(
        &entry_nonce,
        Origin::Loopback,
        LinkScope::Board,
        "acceptance",
        NOW,
    );
    assert!(matches!(board_attempt, Err(LinkRefusal::WrongOrigin)));
}
