//! Independent CAD-1365 refusal acceptance for the SECOND new media
//! service surface: daemon RPC `app_media_publish` and its board peer
//! `POST /api/app-media/publish`, authored from the frozen contract
//! (`design-plans/designer-media-publish-contract-v1.md`, supplementing
//! `designer-media-contract-v1.md`) — NOT by the implementer, who may
//! not edit or weaken it.
//!
//! Frozen contract under test (the file, not the code, is the spec):
//! - RPC `app_media_publish` params are CLOSED: exactly `action_token`,
//!   `token`, `key`, `origin`, `asset_id`, `expected_revision`,
//!   `request_id`. No frame scope, creator, URL, path, workspace,
//!   caller or actor selector is accepted — the mount's own
//!   server-bound context decides every boundary and the request's real
//!   session supplies the credential.
//! - HTTP `POST /api/app-media/publish` body is CLOSED: exactly
//!   `action_token`, `asset_id`, `expected_revision`, `request_id`;
//!   the route supplies the request's classified session credentials
//!   (operator cookie `cadence_operator_{port}` + `X-Cadence-Session` +
//!   classified Origin/Host) host-side, never from the body.
//! - Authority re-proves the SAME operator/session/consumed mount/
//!   current installed digest/approval/active context chain as
//!   `media.read`, but requires the declared `screen-media.v1`
//!   `media.publish` permission. `tools.invoke` or `media.read` alone
//!   never grants mutation.
//! - Initial publication requires an existing valid draft row at
//!   `expected_revision`; atomically changes only `lifecycle` to
//!   `published`, `revision` by one and `updated`; preserves id,
//!   `created`, `creator`, producing `install_id`, `title` and `kind`;
//!   and returns the exact two-key object `{asset, receipt:{request_id,
//!   asset_id, expected_revision, revision, lifecycle:"published"}}`.
//!   Identical `(asset_id,expected_revision)` under the same request id
//!   replays the exact accepted result with NO second revision; changed
//!   intent under that id refuses; a fresh publish of an
//!   already-published or stale-revision asset refuses without a new
//!   receipt; a failed attempt never reserves its request id. A
//!   different permitted session in the same workspace is NOT refused
//!   as a private owner — workspace curation is shared.
//!
//! Baseline honesty: on the first admitted run the POSITIVE/replay
//! fixtures are expected RED at a REAL cause — `media.publish` is not
//! an admitted `may` member (`app_workspace_install` refuses the
//! declaration "not a method this contract grants"), `app_media_publish`
//! is not in the daemon method table ("Unknown method") and
//! `POST /api/app-media/publish` is not a board route (404). An
//! unsupported-declaration, missing-method or 404 outcome is a
//! fixture/ABI RED, never a refusal PASS. The positive pins stay inside
//! the SAME scope as the negatives so a gate that never reached the
//! real service fails the test, not just one clause. Custody honesty:
//! this seeds and reads only the metadata `app_media_assets` columns;
//! it does NOT prove catalog-to-original lineage or byte custody, which
//! remain separate acceptance. No original descriptor or
//! capability-result column is fabricated.

#![cfg(all(test, feature = "test-seam"))]

use super::cad1365_mount_authority_acceptance::{Fx, Stop};
use crate::error::Result;
use crate::store::StoreConn;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// Daemon-minted id grammar — `med-` + 32 lowercase hex.
fn media_id(n: u8) -> String {
    format!("med-{:0>32x}", n)
}

/// A canonical tagged creator tag the producing writer records — the
/// `{kind:"user",id,label?}`/`{kind:"operator"}`/`{kind:"unknown"}`
/// union, never a bare id.
fn creator_tag() -> String {
    json!({"kind": "operator"}).to_string()
}

/// A second valid creator tag — a structured named-user attribution a
/// TRUSTED writer recorded, used to prove publication preserves an
/// existing creator rather than rewriting it to the caller. It is
/// seeded metadata, never a caller-supplied or invented identity.
fn named_creator_tag() -> String {
    json!({"kind": "user", "id": "usr-seeded-editor", "label": "Seeded Editor"}).to_string()
}

/// The `may` set a publish-capable media mount declares per the frozen
/// contract: the tools bridge plus BOTH media permissions. The current
/// native parser does not admit `media.publish` — install of this
/// declaration is the honest baseline RED, not a refusal pass.
fn publish_declaration() -> Option<(String, Vec<String>)> {
    Some((
        "screen-media.v1".to_string(),
        vec![
            "tools.invoke".to_string(),
            "media.read".to_string(),
            "media.publish".to_string(),
        ],
    ))
}

/// Seed one valid DRAFT row into the workspace media catalog through a
/// trusted side connection (`Store::open_side` while the daemon holds
/// the store — the same lane `app remove` uses, never a public import
/// API). Returns `(asset_id, revision, created)`.
fn seed_draft(fx: &Fx, seq: u8, title: &str, creator: &str) -> (String, i64, f64) {
    let store = crate::store::Store::open_side(&fx.state.join("cadence.sqlite3")).unwrap();
    let id = media_id(seq);
    let created = 1_700_000_000.0 + seq as f64;
    store
        .write_tx(|tx| {
            tx.execute(
                "INSERT INTO app_media_assets(id,install_id,title,kind,lifecycle,revision,creator,created,updated)
                 VALUES(?1,?2,?3,'image','draft',1,?4,?5,?5)",
                rusqlite::params![id, fx.install, title, creator, created],
            )?;
            Ok(())
        })
        .expect("seed a valid draft media row");
    (id, 1, created)
}

/// The raw persisted row, read back through a side connection — the
/// preservation evidence. Only the metadata projection columns are
/// read; no locator/byte column exists in this table.
fn stored_row(fx: &Fx, id: &str) -> Value {
    let store = crate::store::Store::open_side(&fx.state.join("cadence.sqlite3")).unwrap();
    store
        .write_tx(|tx| {
            Ok(tx.query_opt(
                "SELECT id,install_id,title,kind,lifecycle,revision,creator,created,updated
                 FROM app_media_assets WHERE id=?1",
                [id],
                |r| {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "install_id": r.get::<_, String>(1)?,
                        "title": r.get::<_, String>(2)?,
                        "kind": r.get::<_, String>(3)?,
                        "lifecycle": r.get::<_, String>(4)?,
                        "revision": r.get::<_, i64>(5)?,
                        "creator": r.get::<_, String>(6)?,
                        "created": r.get::<_, f64>(7)?,
                        "updated": r.get::<_, f64>(8)?,
                    }))
                },
            )?)
        })
        .unwrap()
        .unwrap_or(Value::Null)
}

/// Clone the publish params into the frozen closed envelope; the
/// credentials are the caller's REAL session fields the daemon resolves,
/// never an identity or scope selector.
fn publish_params(
    action: &str,
    token: &str,
    key: &str,
    asset_id: &str,
    expected_revision: i64,
    request_id: &str,
) -> Value {
    json!({"action_token": action, "token": token, "key": key,
        "origin": "loopback", "asset_id": asset_id,
        "expected_revision": expected_revision, "request_id": request_id})
}

fn refused(result: Result<Value>, needle: &str, what: &str) {
    let text = result
        .expect_err(&format!("{what} must be refused"))
        .to_string();
    assert!(
        text.contains(needle),
        "{what} refused by the wrong guard: {text}"
    );
}

/// The publish reply must be EXACTLY the contract's two-key object
/// `{asset, receipt}`: the canonical asset metadata DTO plus the
/// five-field receipt. A blanket success lacking the closed shape fails
/// as a wrong contract, not a pass.
fn assert_publish_shape(
    v: &Value,
    asset_id: &str,
    request_id: &str,
    expected: i64,
    revision: i64,
    what: &str,
) {
    let keys: std::collections::BTreeSet<&str> = v
        .as_object()
        .unwrap_or_else(|| panic!("{what}: reply is not an object: {v}"))
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["asset", "receipt"].into_iter().collect(),
        "{what}: reply must be exactly {{asset, receipt}}: {v}"
    );
    let asset = &v["asset"];
    for field in [
        "id",
        "title",
        "kind",
        "lifecycle",
        "revision",
        "creator",
        "created",
        "updated",
    ] {
        assert!(
            asset.get(field).is_some(),
            "{what}: canonical asset DTO lacks {field}: {v}"
        );
    }
    let r = &v["receipt"];
    let rkeys: std::collections::BTreeSet<&str> = r
        .as_object()
        .unwrap_or_else(|| panic!("{what}: receipt is not an object: {v}"))
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        rkeys,
        [
            "request_id",
            "asset_id",
            "expected_revision",
            "revision",
            "lifecycle"
        ]
        .into_iter()
        .collect(),
        "{what}: receipt must be the contract's five fields: {v}"
    );
    assert_eq!(
        r["request_id"],
        json!(request_id),
        "{what} receipt.request_id: {v}"
    );
    assert_eq!(
        r["asset_id"],
        json!(asset_id),
        "{what} receipt.asset_id: {v}"
    );
    assert_eq!(
        r["expected_revision"],
        json!(expected),
        "{what} receipt.expected_revision: {v}"
    );
    assert_eq!(
        r["revision"],
        json!(revision),
        "{what} receipt.revision: {v}"
    );
    assert_eq!(
        r["lifecycle"],
        json!("published"),
        "{what} receipt.lifecycle: {v}"
    );
}

/// Spin the test-seam board on a free 3110–3189 port under the fixture's
/// stop flag — the same pattern the read acceptance proved.
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

/// POST the frozen closed publish body `{action_token, asset_id,
/// expected_revision, request_id}` to `/api/app-media/publish`.
fn http_publish(
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
        .post(format!("http://127.0.0.1:{port}/api/app-media/publish"))
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

/// Positive publication + preservation evidence over RPC and HTTP under
/// a `media.publish`-declared mount: a valid seeded draft at its
/// expected revision publishes atomically (lifecycle→published,
/// revision+1, updated moves; id/title/kind/install/creator/created
/// unchanged) and returns the exact two-key contract shape. A second
/// permitted SESSION publishes a trusted row carrying a pre-existing
/// named creator tag without rewriting it — proving workspace curation
/// is shared, not owner-private (this proves session/mount
/// independence, NOT a creator-producing pipeline). On the current
/// baseline the whole fixture is RED at install: `media.publish` is
/// not an admitted `may` member.
#[test]
fn cad1365_media_publish_positive_atomic_and_preserves_metadata() {
    let fx = Fx::with_declaration(publish_declaration());
    let (token, key) = fx.session();
    let (_nonce, action) = fx.mount(&token, &key, 1);
    let (asset_id, expected_rev, created) = seed_draft(&fx, 1, "Studio hero", &creator_tag());
    let request_id = "pub-req-1";

    // Non-operator callers stop at the operator-connection guard before
    // the publish method is read.
    for who in [
        crate::test_seam::Asserted::Agent("worker".into()),
        crate::test_seam::Asserted::Unproven,
    ] {
        refused(
            fx.call(
                who,
                "app_media_publish",
                publish_params(&action, &token, &key, &asset_id, expected_rev, request_id),
            ),
            "operator",
            "non-operator media publish",
        );
    }

    // POSITIVE CONTROL (RPC): the real mount publishes the draft and
    // gets the contract's exact {asset, receipt} object.
    let reply = fx
        .operator(
            "app_media_publish",
            publish_params(&action, &token, &key, &asset_id, expected_rev, request_id),
        )
        .expect("operator media publish must reach the service");
    assert_publish_shape(
        &reply,
        &asset_id,
        request_id,
        expected_rev,
        expected_rev + 1,
        "rpc publish",
    );
    assert_eq!(reply["asset"]["id"], json!(asset_id), "asset id unchanged");
    assert_eq!(
        reply["asset"]["title"],
        json!("Studio hero"),
        "title preserved"
    );
    assert_eq!(reply["asset"]["kind"], json!("image"), "kind preserved");
    assert_eq!(
        reply["asset"]["revision"],
        json!(expected_rev + 1),
        "revision bumped once"
    );
    assert_eq!(
        reply["asset"]["lifecycle"],
        json!("published"),
        "lifecycle now published"
    );
    assert_eq!(
        reply["asset"]["created"],
        json!(created),
        "created timestamp preserved"
    );
    assert_eq!(
        reply["asset"]["creator"],
        json!({"kind": "operator"}),
        "creator projection preserved"
    );

    // Preservation evidence: the raw seeded columns survive; ONLY
    // lifecycle/revision/updated may change — install/creator/created
    // are frozen provenance the mutation must not rewrite.
    let row = stored_row(&fx, &asset_id);
    assert_eq!(row["id"], json!(asset_id), "stored id preserved");
    assert_eq!(
        row["install_id"],
        json!(fx.install),
        "producing install preserved"
    );
    assert_eq!(row["title"], json!("Studio hero"), "stored title preserved");
    assert_eq!(row["kind"], json!("image"), "stored kind preserved");
    assert_eq!(
        row["creator"],
        json!(creator_tag()),
        "creator tag preserved"
    );
    assert_eq!(
        row["created"],
        json!(created),
        "created timestamp preserved"
    );
    assert_eq!(row["lifecycle"], json!("published"), "lifecycle flipped");
    assert_eq!(
        row["revision"],
        json!(expected_rev + 1),
        "revision CAS applied"
    );
    assert!(
        row["updated"].as_f64().unwrap() > created,
        "updated advanced"
    );

    // POSITIVE CONTROL (HTTP): a second permitted SESSION publishes a
    // DIFFERENT trusted row — one seeded with a pre-existing named-user
    // creator tag — over the board route. This proves a separate
    // session/mount can curate the shared workspace catalog and that
    // publication does NOT rewrite the stored creator to the caller;
    // it is NOT a private-owner refusal and verifies no creator pipeline.
    let (asset_b, rev_b, created_b) = seed_draft(&fx, 2, "Collab draft", &named_creator_tag());
    let (token_b, key_b) = fx.session();
    let (_n2, action_b) = fx.mount(&token_b, &key_b, 1);
    let (port, _guard) = board(&fx);
    let seam = crate::test_seam::Seam::token_at(&fx.state).unwrap();
    let (status, reply) = http_publish(
        port,
        &seam,
        "operator",
        Some((&token_b, &key_b)),
        json!({"action_token": action_b, "asset_id": asset_b,
            "expected_revision": rev_b, "request_id": "pub-req-collab"}),
    );
    assert_eq!(
        status, 200,
        "second-session media publish over HTTP: {reply}"
    );
    assert_publish_shape(
        &reply,
        &asset_b,
        "pub-req-collab",
        rev_b,
        rev_b + 1,
        "http publish",
    );
    let row_b = stored_row(&fx, &asset_b);
    assert_eq!(
        row_b["lifecycle"],
        json!("published"),
        "second session's draft published"
    );
    assert_eq!(
        row_b["created"],
        json!(created_b),
        "collab created preserved"
    );
    assert_eq!(
        row_b["creator"],
        json!(named_creator_tag()),
        "publish must not rewrite the stored named creator to the caller"
    );

    // HTTP caller gates: agent/unproven requests refuse at the board's
    // write gate (403), never reach the daemon.
    for who in ["agent:worker", "unproven"] {
        let (status, reply) = http_publish(
            port,
            &seam,
            who,
            None,
            json!({"action_token": action_b, "asset_id": asset_b,
                "expected_revision": rev_b + 1, "request_id": "pub-x"}),
        );
        assert_eq!(status, 403, "{who} relayed a media publish: {reply}");
    }

    // A body-selected credential/scope/caller field is refused at the
    // board's closed body (400) before the daemon ever sees it.
    for field in [
        "token",
        "key",
        "origin",
        "workspace",
        "creator",
        "caller",
        "input",
    ] {
        let (status, reply) = http_publish(
            port,
            &seam,
            "operator",
            Some((&token_b, &key_b)),
            json!({"action_token": action_b, "asset_id": asset_b,
                "expected_revision": rev_b + 1, "request_id": "pub-y",
                field: "forged"}),
        );
        assert_eq!(status, 400, "body field {field} rode the publish: {reply}");
    }

    // The daemon's closed params refuse caller-supplied selectors with
    // TWO distinct guards: connection-bound identity names
    // (OPERATOR_FIELDS: actor/sub/operator/…) refuse at
    // `reject_operator_fields` — "request field 'X' is not accepted" —
    // while ordinary scope selectors (creator/caller/workspace/company/
    // installation/context_id) refuse at `strict_fields` —
    // "unsupported fields". Asserting the actual guard, not one uniform
    // needle.
    for field in ["actor", "sub", "operator", "as"] {
        let mut p = publish_params(&action, &token, &key, &asset_id, expected_rev + 1, "pub-z");
        p[field] = json!("forged");
        refused(
            fx.operator("app_media_publish", p),
            &format!("request field '{field}' is not accepted"),
            &format!("forged publish identity field {field}"),
        );
    }
    for field in [
        "workspace",
        "company",
        "creator",
        "caller",
        "installation",
        "context_id",
    ] {
        let mut p = publish_params(&action, &token, &key, &asset_id, expected_rev + 1, "pub-z");
        p[field] = json!("forged");
        refused(
            fx.operator("app_media_publish", p),
            "unsupported fields",
            &format!("forged publish scope field {field}"),
        );
    }
}

/// Idempotent replay + CAS refusals under a `media.publish`-declared
/// mount. Identical `(asset_id,expected_revision)` under the same
/// request id returns the exact prior accepted result with no second
/// revision; changed intent under that id refuses without mutating the
/// other row; a stale revision refuses; an already-published asset
/// refuses a fresh publish; and a failed attempt never reserves its
/// request id — the same id on the CORRECT intent afterwards succeeds
/// and records exactly one accepted receipt.
#[test]
fn cad1365_media_publish_replay_and_cas_refusals() {
    let fx = Fx::with_declaration(publish_declaration());
    let (token, key) = fx.session();
    let (_nonce, action) = fx.mount(&token, &key, 1);
    let (asset_id, expected_rev, created) = seed_draft(&fx, 10, "Replay asset", &creator_tag());
    let request_id = "pub-replay-1";

    // POSITIVE: initial publish.
    let first = fx
        .operator(
            "app_media_publish",
            publish_params(&action, &token, &key, &asset_id, expected_rev, request_id),
        )
        .expect("initial publish");
    assert_publish_shape(
        &first,
        &asset_id,
        request_id,
        expected_rev,
        expected_rev + 1,
        "first",
    );

    // REPLAY: the identical accepted request returns the exact prior
    // result — no second revision, no new mutation.
    let replay = fx
        .operator(
            "app_media_publish",
            publish_params(&action, &token, &key, &asset_id, expected_rev, request_id),
        )
        .expect("identical replay returns the accepted result");
    assert_eq!(
        replay, first,
        "replay must return the exact accepted receipt"
    );
    let row = stored_row(&fx, &asset_id);
    assert_eq!(
        row["revision"],
        json!(expected_rev + 1),
        "replay must not bump revision again"
    );
    assert_eq!(row["created"], json!(created), "replay preserved created");

    // CHANGED INTENT under the accepted request id refuses — a
    // different asset_id under `pub-replay-1` must not ride it, and the
    // refusal must not mutate or reserve anything against that row.
    let (asset_other, _, _) = seed_draft(&fx, 11, "Other draft", &creator_tag());
    refused(
        fx.operator(
            "app_media_publish",
            publish_params(&action, &token, &key, &asset_other, 1, request_id),
        ),
        "request",
        "changed asset_id under an accepted request id",
    );
    let other = stored_row(&fx, &asset_other);
    assert_eq!(
        other["lifecycle"],
        json!("draft"),
        "changed intent must not publish the other row"
    );
    assert_eq!(
        other["revision"],
        json!(1),
        "changed intent must not touch other revision"
    );

    // Changed expected_revision under the accepted id also refuses.
    refused(
        fx.operator(
            "app_media_publish",
            publish_params(&action, &token, &key, &asset_id, 99, request_id),
        ),
        "request",
        "changed expected_revision under an accepted request id",
    );

    // STALE expected_revision: the asset is now at revision 2; a fresh
    // request expecting revision 1 is stale and refuses, no mutation.
    refused(
        fx.operator(
            "app_media_publish",
            publish_params(&action, &token, &key, &asset_id, 1, "pub-stale-1"),
        ),
        "revision",
        "stale expected_revision on a fresh publish",
    );
    let row2 = stored_row(&fx, &asset_id);
    assert_eq!(
        row2["revision"],
        json!(expected_rev + 1),
        "stale refusal must not mutate"
    );

    // ALREADY PUBLISHED: a fresh request at the CURRENT revision (2) on
    // a published asset refuses — publication is a draft→published move,
    // not an idempotent re-publish that mints a second receipt.
    refused(
        fx.operator(
            "app_media_publish",
            publish_params(
                &action,
                &token,
                &key,
                &asset_id,
                expected_rev + 1,
                "pub-already-1",
            ),
        ),
        "draft",
        "fresh publish of an already-published asset",
    );
    let row3 = stored_row(&fx, &asset_id);
    assert_eq!(
        row3["revision"],
        json!(expected_rev + 1),
        "already-published refusal must not bump"
    );

    // A FAILED attempt does not reserve its request id: `pub-stale-1`
    // was refused above on a stale revision; the SAME id on the
    // now-correct intent (a still-draft asset at its real revision)
    // succeeds and records exactly one accepted receipt — a reserved-id
    // scheme would refuse it as changed intent.
    let (asset_c, rev_c, _) = seed_draft(&fx, 12, "Retry draft", &creator_tag());
    refused(
        fx.operator(
            "app_media_publish",
            publish_params(&action, &token, &key, &asset_c, 99, "pub-retry-1"),
        ),
        "revision",
        "stale expected_revision must fail before reserving the id",
    );
    let retried = fx
        .operator(
            "app_media_publish",
            publish_params(&action, &token, &key, &asset_c, rev_c, "pub-retry-1"),
        )
        .expect("same request id on the correct intent succeeds — failed attempts never reserve");
    assert_publish_shape(
        &retried,
        &asset_c,
        "pub-retry-1",
        rev_c,
        rev_c + 1,
        "retry after failure",
    );
    let row_c = stored_row(&fx, &asset_c);
    assert_eq!(
        row_c["lifecycle"],
        json!("published"),
        "retried publish applied once"
    );
    assert_eq!(
        row_c["revision"],
        json!(rev_c + 1),
        "retried publish bumped exactly once"
    );
}

/// Permission and fresh-authority refusals, split by the declaration
/// they prove.
///
/// PART A — missing permission: a real session+live mount whose screen
/// declares `tools.invoke` + `media.read` but NOT `media.publish` is
/// refused on the publish surface — read rights never carry mutation —
/// and the draft stays untouched. On the current baseline the service
/// is absent, so this is an expected ABI RED ("Unknown method"), not a
/// refusal pass.
///
/// PART B — mount/session/install proofs under a `media.publish`
/// mount: forged action token, second session borrowing a mount,
/// superseded mount, and an approval revoked AFTER an accepted publish
/// all refuse — a replayed accepted intent can never bypass the fresh
/// approval/digest re-proof.
#[test]
fn cad1365_media_publish_authority_and_permission_refusals() {
    // PART A: the read-only media mount must be refused on publish.
    {
        let fx = Fx::with_declaration(Some((
            "screen-media.v1".to_string(),
            vec!["tools.invoke".to_string(), "media.read".to_string()],
        )));
        let (token, key) = fx.session();
        let (_nonce, action) = fx.mount(&token, &key, 1);
        let (asset_id, expected_rev, _) = seed_draft(&fx, 30, "Read-only asset", &creator_tag());
        refused(
            fx.operator(
                "app_media_publish",
                publish_params(&action, &token, &key, &asset_id, expected_rev, "pub-ro-1"),
            ),
            "media.publish",
            "publish under a media.read-only mount",
        );
        let row = stored_row(&fx, &asset_id);
        assert_eq!(
            row["lifecycle"],
            json!("draft"),
            "permission refusal must not mutate"
        );
        assert_eq!(
            row["revision"],
            json!(expected_rev),
            "permission refusal must not bump"
        );
    }

    // PART B: a publish-declared mount exercises the fresh authority
    // proofs. Baseline: install itself is RED (media.publish
    // unadmitted); below is the contract the implementation must meet.
    let fx = Fx::with_declaration(publish_declaration());
    let (token_a, key_a) = fx.session();
    let (_nonce, action_a) = fx.mount(&token_a, &key_a, 1);
    let (asset_id, expected_rev, _) = seed_draft(&fx, 20, "Authority asset", &creator_tag());
    let request_id = "pub-auth-1";

    // A well-formed forged action token under the real session refuses.
    refused(
        fx.operator(
            "app_media_publish",
            publish_params(
                &"e".repeat(64),
                &token_a,
                &key_a,
                &asset_id,
                expected_rev,
                request_id,
            ),
        ),
        "stale or unknown",
        "forged action token on publish",
    );

    // A second live operator session cannot spend mount A's context.
    let (token_b, key_b) = fx.session();
    refused(
        fx.operator(
            "app_media_publish",
            publish_params(
                &action_a,
                &token_b,
                &key_b,
                &asset_id,
                expected_rev,
                request_id,
            ),
        ),
        "does not belong to this session",
        "second session borrowing a publish mount",
    );

    // ACCEPTED intent on a SEPARATE asset, so the post-revoke replay
    // below is a real accepted-intent replay, not a fresh first publish.
    let (asset_published, rev_pub, _) = seed_draft(&fx, 21, "Accepted asset", &creator_tag());
    let accepted = fx
        .operator(
            "app_media_publish",
            publish_params(
                &action_a,
                &token_a,
                &key_a,
                &asset_published,
                rev_pub,
                "pub-accepted-1",
            ),
        )
        .expect("pre-revoke publish must be accepted");
    assert_publish_shape(
        &accepted,
        &asset_published,
        "pub-accepted-1",
        rev_pub,
        rev_pub + 1,
        "accepted",
    );

    // Supersede mount A; its action handle must die on publish too.
    let (_n2, action_b) = fx.mount(&token_a, &key_a, 2);
    refused(
        fx.operator(
            "app_media_publish",
            publish_params(
                &action_a,
                &token_a,
                &key_a,
                &asset_id,
                expected_rev,
                request_id,
            ),
        ),
        "stale or unknown",
        "superseded mount on publish",
    );

    // Revoke the install's live approval, then REPLAY the SAME accepted
    // intent under the current session+mount — the fresh proof must
    // refuse; the accepted receipt can never bypass re-verification.
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
        fx.operator(
            "app_media_publish",
            publish_params(
                &action_b,
                &token_a,
                &key_a,
                &asset_published,
                rev_pub,
                "pub-accepted-1",
            ),
        ),
        "approved",
        "accepted-intent replay after approval revoke",
    );

    // The accepted row stays at its published revision; the refused
    // draft was never touched.
    let row_pub = stored_row(&fx, &asset_published);
    assert_eq!(
        row_pub["revision"],
        json!(rev_pub + 1),
        "accepted row stays at its published revision"
    );
    assert_eq!(
        row_pub["lifecycle"],
        json!("published"),
        "accepted row stays published"
    );
    let row = stored_row(&fx, &asset_id);
    assert_eq!(
        row["lifecycle"],
        json!("draft"),
        "refused publish must not mutate"
    );
    assert_eq!(
        row["revision"],
        json!(expected_rev),
        "refused publish must not bump revision"
    );
}
