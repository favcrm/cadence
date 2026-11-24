//! CAD-1809: `SessionInfo` must match its live producer exactly.

use crate::board_schema_test::assert_matches;

#[test]
fn schema_only_board_types_match_their_producers() {
    let dir = tempfile::tempdir().unwrap();
    let mut auth = crate::operator_auth::Auth::load(dir.path());
    let user = crate::operator_auth::BoardUser {
        sub: "s".into(),
        email: "e@example.test".into(),
        name: "n".into(),
        role: "member".into(),
        handle: "h".into(),
    };
    let realm = crate::operator_auth::PublicRealm {
        issuer: "https://issuer.example.test".into(),
        company: "co".into(),
        host: "h.example.test".into(),
    };
    let opened = auth
        .open_public_bound_with_intent(
            user,
            ("jti", 4_000_000_000),
            "ua",
            (realm, None),
            1_000,
            |_| Ok(()),
        )
        .unwrap()
        .unwrap();
    assert_matches(
        "meta.schema.json",
        "SessionInfo",
        serde_json::to_value(&opened.session).unwrap(),
    );
    // A login-link session has no user and no account: both keys stay `null`.
    let nonce = auth
        .mint(crate::operator_auth::Origin::Loopback, 1_000)
        .unwrap();
    let link = auth
        .open(&nonce, crate::operator_auth::Origin::Loopback, "ua", 1_000)
        .unwrap()
        .unwrap();
    assert!(link.session.user.is_none() && link.session.account_id.is_none());
    assert_matches(
        "meta.schema.json",
        "SessionInfo",
        serde_json::to_value(&link.session).unwrap(),
    );
}
