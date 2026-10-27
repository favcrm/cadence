//! Independent CAD-1365 refusal acceptance for the FIRST new media
//! service surface: daemon RPC `app_media_read` and its board peer
//! `POST /api/app-media/read`, authored from the frozen contract
//! (`design-plans/designer-media-contract-v1.md`) — NOT by the
//! implementer, who may not edit or weaken it.
//!
//! Frozen contract under test (the file, not the code, is the spec):
//! - RPC `app_media_read` params are CLOSED: exactly `action_token`,
//!   `token`, `key`, `origin`, `input`. `input` is the union
//!   `{kind:"catalog", limit?:1..20, cursor?:opaque-string}` or
//!   `{kind:"asset", asset_id:opaque-native-id}` — nothing else.
//! - Authority is the request's REAL native session (token/key/origin)
//!   plus the host-held mount `action_token`; the media handler
//!   re-proves the CURRENT installed bundle digest + approval + active
//!   context and the mounted tag's explicit media declaration — a bare
//!   `tools.invoke` alias map can never grant `media.read`.
//! - HTTP `POST /api/app-media/read` relays the request's own classified
//!   session credentials (Root-operator cookie `cadence_operator_{port}`
//!   + `X-Cadence-Session` key + classified Origin/Host), never
//!     body-selected credentials, caller, creator or scope.
//! - A native `media` tag declares `host_contract:"screen-media.v1"`,
//!   `may:["tools.invoke","media.read"]` and a nonempty provider-alias
//!   `tools` map: media permission is separate from provider-alias
//!   rights, and the nonempty-tools invariant is kept.
//! - An empty catalog is a valid POSITIVE control: the same real mount
//!   reads `{assets:[], next_cursor:null, current_actor:...}` over both
//!   RPC and HTTP. Asset-detail and foreign/provider custody checks are
//!   out of scope — no storage constructor, table or test-asset import
//!   is invented here.
//!
//! Baseline honesty: on the first admitted run every test below is
//! expected RED at a REAL cause — `app_media_read` is not in the daemon
//! method table ("Unknown method"), `POST /api/app-media/read` is not a
//! board route (404), and `screen-media.v1` is not a served
//! `host_contract` (install re-proof refused). A missing-method, 404 or
//! declaration refusal is a fixture/ABI RED, never a refusal PASS. The
//! accepted-positive pins are kept inside the SAME `assert_eq`/scope as
//! the negatives so an admission that never reached the real guard fails
//! the test, not just one clause of it.

#![cfg(all(test, feature = "test-seam"))]

use super::cad1365_mount_authority_acceptance::{Fx, Stop};
use crate::test_seam::{Asserted, Seam};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// Clone of the media-read input contract into the frozen parameter
/// envelope; credentials are the caller's REAL session fields the daemon
/// resolves, never an identity or scope selector.
fn rpc_params(action: &str, token: &str, key: &str, input: Value) -> Value {
    json!({"action_token": action, "token": token, "key": key,
        "origin": "loopback", "input": input})
}

fn refused(result: crate::error::Result<Value>, needle: &str, what: &str) {
    let text = result
        .expect_err(&format!("{what} must be refused"))
        .to_string();
    assert!(
        text.contains(needle),
        "{what} refused by the wrong guard: {text}"
    );
}

/// Rejects (never accept) the two contract-level non-refusals: an
/// accepted value is only the positive control when it also carries the
/// catalog shape the service owes (`assets` array, `next_cursor`,
/// `current_actor`). Anything else "succeeding" is a wrong contract,
/// not coverage.
fn assert_catalog_shape(v: &Value, what: &str) {
    assert!(
        v["assets"].is_array(),
        "{what}: reply lacks the assets array — wrong contract or a gate masquerading as success: {v}"
    );
    assert!(
        v.get("next_cursor").is_some(),
        "{what}: reply lacks next_cursor: {v}"
    );
    assert!(
        v.get("current_actor").is_some(),
        "{what}: reply lacks current_actor: {v}"
    );
}

/// Spin the test-seam board on a free 3110–3189 port under the fixture's
/// stop flag; returns `(port, session-key-request-helper)` — the SAME
/// pattern `cad1365_tool_invoke_http_peer_is_at_least_as_strict` proved.
/// The caller closes over `fx` state/pm for the board relay.
fn board(fx: &Fx) -> (u16, Stop) {
    let state = fx.state.clone();
    let pm = state.parent().unwrap().join("pm");
    let mut guard = Stop(fx._stop.0.clone(), Vec::new());
    let first = (std::process::id() % 80) as u16;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut offset = 0u16;
    loop {
        assert!(Instant::now() < deadline, "board startup deadline");
        let port = 3110 + (first + offset) % 80;
        let (startup, ready) = std::sync::mpsc::channel();
        let board = crate::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(guard.0.clone()),
            startup: Some(startup),
            test_seam: true,
            ..Default::default()
        };
        let (s, p) = (state.clone(), pm.clone());
        let thread = std::thread::spawn(move || drop(crate::ui::serve(&s, &p, &board)));
        let remaining = deadline.saturating_duration_since(Instant::now());
        match ready.recv_timeout(remaining) {
            Ok(Ok(())) => {
                guard.1.push(thread);
                return (port, guard);
            }
            Ok(Err(std::io::ErrorKind::AddrInUse)) => {
                assert!(thread.join().is_ok());
                offset += 1;
                assert!(offset < 80, "all fixture board candidates busy");
            }
            other => {
                guard.1.push(thread);
                panic!("fixture board startup failed on port {port}: {other:?}");
            }
        }
    }
}

/// POST the frozen body `{action_token, input}` to
/// `/api/app-media/read`; `who` drives the test-seam caller header and,
/// for the operator identity, the REAL per-port cookie +
/// `X-Cadence-Session` pair — never a caller header the body chooses.
fn http_call(
    port: u16,
    seam: &str,
    who: &str,
    session: Option<(&str, &str)>,
    body: Value,
) -> (u16, Value) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let host = format!("cadence-{port}.localhost:{port}");
    let mut req = agent
        .post(format!("http://127.0.0.1:{port}/api/app-media/read"))
        .header("Host", &host)
        .header("X-Cadence-Board", "1")
        .header("Origin", format!("http://{host}"))
        .header(crate::test_seam::AS_HEADER, who)
        .header(crate::test_seam::TOKEN_HEADER, seam)
        .header("Content-Type", "application/json");
    if let Some((token, key)) = session {
        req = req
            .header("Cookie", format!("cadence_operator_{port}={token}"))
            .header("X-Cadence-Session", key);
    }
    let mut r = req.send(body.to_string()).unwrap();
    (
        r.status().as_u16(),
        r.body_mut().read_json().unwrap_or(Value::Null),
    )
}

/// Positive authority spine: a media-declared mount's real operator
/// session reads the empty catalog over RPC AND HTTP; non-operator and
/// unproven callers stop at the connection guard before the method is
/// read; a body that carries credentials/scope selectors is refused
/// both at the HTTP closed body and the daemon's closed params.
#[test]
fn cad1365_media_read_positive_catalog_and_caller_gates() {
    // The media fixture keeps the nonempty provider-alias `tools` map
    // AND adds `media.read` next to `tools.invoke` — the declared
    // contract is the one the frozen contract names.
    let fx = Fx::with_declaration(Some((
        "screen-media.v1".to_string(),
        vec!["tools.invoke".to_string(), "media.read".to_string()],
    )));
    let (token, key) = fx.session();
    let (_nonce, action) = fx.mount(&token, &key, 1);

    let catalog = json!({"kind": "catalog"});

    // Non-operator callers stop at the operator-connection guard, the
    // same refusal class the mount/tool gates already return.
    for who in [Asserted::Agent("worker".into()), Asserted::Unproven] {
        refused(
            fx.call(
                who,
                "app_media_read",
                rpc_params(&action, &token, &key, catalog.clone()),
            ),
            "operator",
            "non-operator media read",
        );
    }

    // POSITIVE CONTROL (RPC): the real mount's session reads the empty
    // catalog and gets the catalog shape — a blanket denial cannot
    // satisfy this.
    let reply = fx
        .operator(
            "app_media_read",
            rpc_params(&action, &token, &key, catalog.clone()),
        )
        .expect("operator media read must reach the service");
    assert_catalog_shape(&reply, "rpc catalog");
    assert_eq!(reply["assets"], json!([]), "fresh catalog is empty");

    // POSITIVE CONTROL (HTTP): the same real mount relays through the
    // board's operator session cookie and returns the same catalog.
    let (port, _guard) = board(&fx);
    let seam = Seam::token_at(&fx.state).unwrap();
    let (status, reply) = http_call(
        port,
        &seam,
        "operator",
        Some((&token, &key)),
        json!({"action_token": action, "input": catalog}),
    );
    assert_eq!(status, 200, "operator media read over HTTP: {reply}");
    assert_catalog_shape(&reply, "http catalog");
    assert_eq!(reply["assets"], json!([]), "fresh catalog is empty");

    // HTTP caller gates: agent/unproven requests refuse at the board's
    // write gate (403), never reach the daemon.
    for who in ["agent:worker", "unproven"] {
        let (status, reply) = http_call(
            port,
            &seam,
            who,
            None,
            json!({"action_token": action, "input": {"kind": "catalog"}}),
        );
        assert_eq!(status, 403, "{who} relayed a media read: {reply}");
    }

    // A body-selected credential/scope/caller field is refused at the
    // board's closed body (400) before the daemon ever sees it — the
    // request's real session is the only credential the route relays.
    for field in ["token", "key", "origin", "workspace", "creator", "caller"] {
        let (status, reply) = http_call(
            port,
            &seam,
            "operator",
            Some((&token, &key)),
            json!({"action_token": action, "input": {"kind": "catalog"},
                field: "forged"}),
        );
        assert_eq!(
            status, 400,
            "body field {field} rode the media read: {reply}"
        );
    }

    // The daemon's own closed params refuse a caller-supplied scope or
    // credential selector even when the real session is valid — the
    // relayed credential is authority, never the body's.
    for field in [
        "workspace",
        "company",
        "creator",
        "caller",
        "installation",
        "context_id",
    ] {
        let mut p = rpc_params(&action, &token, &key, json!({"kind": "catalog"}));
        p[field] = json!("forged");
        refused(
            fx.operator("app_media_read", p),
            "unsupported fields",
            &format!("forged media-read field {field}"),
        );
    }
}

/// Mount authority: a forged action token, a foreign session borrowing
/// the mount's token, and a superseded mount are refused by the same
/// mount-context proof `app_tool_invoke` already runs — before any
/// catalog/storage work. The positive control rides the same mount.
#[test]
fn cad1365_media_read_mount_context_proofs() {
    let fx = Fx::with_declaration(Some((
        "screen-media.v1".to_string(),
        vec!["tools.invoke".to_string(), "media.read".to_string()],
    )));
    let (token_a, key_a) = fx.session();
    let (_nonce, action_a) = fx.mount(&token_a, &key_a, 1);
    let catalog = json!({"kind": "catalog"});

    // A well-formed forged action token under the real session is
    // refused by the mount-context proof, the same class as
    // `app_tool_invoke`'s "stale or unknown".
    refused(
        fx.operator(
            "app_media_read",
            rpc_params(&"c".repeat(64), &token_a, &key_a, catalog.clone()),
        ),
        "stale or unknown",
        "forged action token on media read",
    );

    // A second live operator session cannot spend mount A's context.
    let (token_b, key_b) = fx.session();
    refused(
        fx.operator(
            "app_media_read",
            rpc_params(&action_a, &token_b, &key_b, catalog.clone()),
        ),
        "does not belong to this session",
        "second session borrowing a media mount",
    );

    // Supersede mount A (generation 2 on the same session+install); its
    // action handle must die on the media read too.
    let (_n2, action_b) = fx.mount(&token_a, &key_a, 2);
    refused(
        fx.operator(
            "app_media_read",
            rpc_params(&action_a, &token_a, &key_a, catalog.clone()),
        ),
        "stale or unknown",
        "superseded mount on media read",
    );

    // POSITIVE CONTROL: the CURRENT mount's action token still reads
    // the empty catalog — proving the refusals above are authority,
    // not a blanket deny.
    let reply = fx
        .operator(
            "app_media_read",
            rpc_params(&action_b, &token_a, &key_a, catalog),
        )
        .expect("current media mount must read");
    assert_catalog_shape(&reply, "current-mount catalog");
    assert_eq!(reply["assets"], json!([]), "fresh catalog is empty");
}

/// Install-state re-proof: a mount minted against digest D is refused
/// once the install's live approval moves (revoked) or the digest moves
/// (an approved upgrade). The fresh-bundle/approval check rides the
/// SAME mount authority the tool invoke already re-proves — a bare
/// `tool_context_proven` alone is not enough, and this test pins the
/// refusal on the media surface itself.
#[test]
fn cad1365_media_read_install_state_reproof() {
    let fx = Fx::with_declaration(Some((
        "screen-media.v1".to_string(),
        vec!["tools.invoke".to_string(), "media.read".to_string()],
    )));
    let (token, key) = fx.session();
    let (_nonce, action) = fx.mount(&token, &key, 1);
    let catalog = json!({"kind": "catalog"});

    // POSITIVE: reads under the pinned approval/digest.
    let reply = fx
        .operator(
            "app_media_read",
            rpc_params(&action, &token, &key, catalog.clone()),
        )
        .expect("media read under approved digest");
    assert_catalog_shape(&reply, "approved-mount catalog");

    // Revoke the install's approval at its current digest: the same
    // mount's media read must refuse — the digest/approval pin is
    // re-proven live, never cached from the mount.
    let install = fx
        .operator("app_workspace_show", json!({"install_id": fx.install}))
        .expect("install show");
    let digest = install["digest"]
        .as_str()
        .or_else(|| install["install"]["digest"].as_str())
        .expect("install digest")
        .to_string();
    fx.operator(
        "app_local_install_revoke",
        json!({"install_id": fx.install, "digest": digest}),
    )
    .expect("operator local-install revoke must succeed");
    refused(
        fx.operator("app_media_read", rpc_params(&action, &token, &key, catalog)),
        "approved",
        "media read on a revoked install",
    );
}

/// Declared permission is the gate, not the tool map: an otherwise
/// identical mount whose screen kept `tools.invoke`/provider aliases
/// but did NOT declare `media.read` is refused on the media surface —
/// the nonempty provider-alias map cannot smuggle media rights. This
/// is the explicit-refusal fixture the frozen contract requires once
/// the service exists; on the current baseline it is expected RED at
/// "Unknown method 'app_media_read'" (the method is not implemented),
/// which is a MISSING-SERVICE red, not a passing negative.
#[test]
fn cad1365_media_read_missing_media_permission_refuses() {
    // The baseline screen declares tools.invoke + provider aliases but
    // NO media.read — media permission is never inferred from tools.
    let fx = Fx::new();
    let (token, key) = fx.session();
    let (_nonce, action) = fx.mount(&token, &key, 1);
    refused(
        fx.operator(
            "app_media_read",
            rpc_params(&action, &token, &key, json!({"kind": "catalog"})),
        ),
        "media.read",
        "media read without declared media.read",
    );
}
