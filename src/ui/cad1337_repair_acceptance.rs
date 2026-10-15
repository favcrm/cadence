// Appended to the existing HTTP CAD-1337 acceptance namespace. The relay
// must not make a saved public attachment proof fresher than the daemon.

#[test]
fn cad1337_http_attachment_relay_refuses_stale_public_realm() {
    let fx = Fx::new();
    let (token, account) = fx.session("usr-cad1337-http-stale-realm", "owner");
    let home = fx._home_id("master");

    // A live, verified owner reaches the real HTTP relay and native storage
    // before the current board realm changes.
    let (status, body) = fx.post_multipart(&token, &[("conversation", &home)], None);
    assert_eq!(status, 200, "valid HTTP owner upload control: {body}");
    let receipt: Value = serde_json::from_str(&body).unwrap();
    let id = receipt["id"].as_str().unwrap();
    let original = fx
        .root
        .path()
        .join("workspace/users")
        .join(&account)
        .join("threads")
        .join(&home)
        .join("uploads")
        .join(id)
        .join("http.txt");
    assert_eq!(std::fs::read(&original).unwrap(), FILE_BYTES);
    let before = fx.row_count();

    // The HTTP server keeps its admitted public host, but the daemon's
    // current realm no longer matches the signed cookie's saved realm.
    crate::board_identity::write_config(
        &fx.state(),
        &crate::board_identity::Config {
            host: "cad1337-http-changed.board.localhost".into(),
            issuer: fx.issuer.clone(),
            company: COMPANY.into(),
        },
    )
    .unwrap();
    let (status, body) = fx.post_multipart(&token, &[("conversation", &home)], None);
    assert!(
        !(200..300).contains(&status),
        "HTTP relay accepted an owner cookie from a stale realm: {body}"
    );
    assert!(
        !body.contains(String::from_utf8_lossy(FILE_BYTES).as_ref()),
        "HTTP stale-realm refusal disclosed submitted attachment bytes"
    );
    assert_eq!(
        fx.row_count(),
        before,
        "stale HTTP proof changed attachment metadata"
    );
    assert_eq!(std::fs::read(&original).unwrap(), FILE_BYTES);
    assert_eq!(
        std::fs::read_dir(fx.state().join(crate::wiki::UPLOAD_DIR))
            .unwrap()
            .count(),
        0,
        "HTTP relay left staging bytes after stale-proof refusal"
    );
}
