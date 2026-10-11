//! Independent CAD-1337 bad-case acceptance, authored from the revised ticket
//! and filesystem design, not from implementation helpers. This module must
//! only be registered under `cfg(all(test, feature = "test-seam"))`.
//!
//! `test_seam::scoped(Asserted::Operator)` below is the existing isolated
//! fixture premise for the unchanged kernel-attribution layer. It is NOT
//! evidence of an actual operator connection, kernel identity, provider
//! success, production runtime, or Browser QA. Account/session identities,
//! owner/member assertions, and conversations are minted/verified by the
//! production dispatcher and auth verifier against this fixture's ephemeral
//! Ed25519 issuer. No test-only identity/account hook is added.

#![cfg(feature = "test-seam")]

use super::*;
use crate::adapter::Identity;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const COMPANY: &str = "cad1337-fixture-workspace";
const PUBLIC_HOST: &str = "cad1337.board.localhost";
const KEY_ID: &str = "cad1337-ephemeral";
const KEY_SEED: [u8; 32] = [0x37; 32];
const GENERATION: &str = "1337aabbccddeeff1337aabbccddeeff";
const SECRET_BYTES: &[u8] = b"ticket-derived private upload; not knowledge\n";

struct Issuer {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    origin: String,
}

impl Issuer {
    fn start() -> Self {
        let server = (3110..=3199)
            .find_map(|port| tiny_http::Server::http(("127.0.0.1", port)).ok())
            .expect("no free isolated issuer port in 3110..3199");
        let origin = format!("http://{}", server.server_addr());
        let key = Ed25519KeyPair::from_seed_unchecked(&KEY_SEED).unwrap();
        let x = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let thread = std::thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                let Ok(Some(request)) = server.recv_timeout(Duration::from_millis(100)) else {
                    continue;
                };
                let body = json!({"keys": [{"kty":"OKP","crv":"Ed25519","kid":KEY_ID,"x":x}]});
                let response = tiny_http::Response::from_string(body.to_string()).with_header(
                    tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
                );
                let _ = request.respond(response);
            }
        });
        Self {
            stop,
            thread: Some(thread),
            origin,
        }
    }

    fn assertion(&self, subject: &str, role: &str) -> String {
        let key = Ed25519KeyPair::from_seed_unchecked(&KEY_SEED).unwrap();
        let now = crate::issue::time::now_epoch();
        let part = |v: Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(&v).unwrap());
        let signed = format!(
            "{}.{}",
            part(json!({"alg":"EdDSA","typ":"JWT","kid":KEY_ID})),
            part(json!({
                "iss": self.origin,
                "aud": PUBLIC_HOST,
                "sub": subject,
                "email": format!("{subject}@example.test"),
                "name": format!("Fixture {subject}"),
                "company": COMPANY,
                "role": role,
                "iat": now,
                "exp": now + 30,
                "jti": format!("cad1337-{subject}-{}", uuid::Uuid::new_v4()),
            }))
        );
        format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(key.sign(signed.as_bytes()).as_ref())
        )
    }
}

impl Drop for Issuer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Fx {
    _dir: tempfile::TempDir,
    shared: Arc<Shared>,
    issuer: Issuer,
    master_home: String,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("cad1337-native-")
            .tempdir()
            .unwrap();
        let pm = dir.path().join("tracker");
        crate::issue::Pm::init(&pm).unwrap();
        let issuer = Issuer::start();
        crate::board_identity::write_config(
            dir.path(),
            &crate::board_identity::Config {
                host: PUBLIC_HOST.into(),
                issuer: issuer.origin.clone(),
                company: COMPANY.into(),
            },
        )
        .unwrap();
        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        opts.provider_env
            .set("CADENCE_HOME", dir.path().to_str().unwrap());
        let shared = Shared::new(dir.path(), &opts).unwrap();
        let cwd = dir.path().to_str().unwrap().to_string();
        for (alias, provider, endpoint_kind) in
            [("master", "pi", "managed"), ("worker", "fake", "fake")]
        {
            shared
                .store
                .register_agent(&NewAgent {
                    alias,
                    provider,
                    endpoint_kind,
                    role: "worker",
                    cwd: &cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params: None,
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
        }
        shared
            .store
            .set_identity_with_quota(
                "master",
                &Identity {
                    thread_id: "fixture-thread".into(),
                    session_id: "fixture-session".into(),
                    model: None,
                    effort: None,
                    pid: std::process::id(),
                    endpoint: None,
                    generation: Some(GENERATION.into()),
                    attach: None,
                },
                None,
            )
            .unwrap();
        let pid = std::process::id();
        let made = scoped(Asserted::Operator, || shared.dispatch(
            "thread_send", &json!({"alias":"master","text":"create genuine Home thread","message":"cad1337-home-seed"}), pid,
        )).unwrap();
        assert_eq!(
            made["state"],
            json!("queued"),
            "Home-thread setup must use real dispatcher: {made}"
        );
        let master_home = shared
            .store
            .thread("master")
            .unwrap()
            .expect("dispatcher-created Home thread")
            .id;
        Self {
            _dir: dir,
            shared,
            issuer,
            master_home,
        }
    }

    fn call(&self, who: Asserted, method: &str, params: Value) -> Result<Value> {
        scoped(who, || {
            self.shared.dispatch(method, &params, std::process::id())
        })
    }

    fn public_session(&self, subject: &str, role: &str) -> Value {
        self.call(
            Asserted::Operator,
            "board_session_open",
            json!({"assertion": self.issuer.assertion(subject, role)}),
        )
        .unwrap()
    }

    fn stage(&self, bytes: &[u8]) -> String {
        let dir = self._dir.path().join(crate::wiki::UPLOAD_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("upload-{}", uuid::Uuid::new_v4().simple()));
        std::fs::write(&path, bytes).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn upload(&self, session: &Value, bytes: &[u8], extra: Value) -> Result<Value> {
        let mut params = json!({
            "name":"ticket.txt",
            "tmp":self.stage(bytes),
            "session":{"token":session["token"],"key":"","origin":"public"},
            "conversation":self.master_home,
        });
        for (key, value) in extra.as_object().unwrap() {
            params[key] = value.clone();
        }
        self.call(Asserted::Operator, "chat_file_upload", params)
    }

    fn row_count(&self) -> i64 {
        rusqlite::Connection::open(crate::rollout::db_file(self._dir.path()))
            .unwrap()
            .query_row("SELECT count(*) FROM chat_files", [], |r| r.get(0))
            .unwrap()
    }
}

fn assert_refusal(result: Result<Value>, marker: &str, case: &str) -> String {
    let error = result
        .expect_err(&format!("{case} unexpectedly succeeded"))
        .to_string();
    assert!(
        error.contains(marker),
        "{case} refused at the wrong layer: {error}"
    );
    error
}

/// The one ticket-derived acceptance matrix. It proves a positive control
/// through production assertion verification and account/session issuance,
/// then checks false/missing proof and targets at native dispatcher guards,
/// including persistence/no-byte effects. HTTP route/peer assertions live in
/// `crate::ui::cad1337_acceptance` and use the same genuine auth contract.
#[test]
fn cad1337_native_account_home_and_turn_refusals() {
    let fx = Fx::new();
    let before = fx.row_count();

    // A genuine owner assertion is verified by board_session_open, and its
    // returned session is bound by the production auth/store issuance path.
    let owner_a = fx.public_session("usr-cad1337-a", "owner");
    let owner_b = fx.public_session("usr-cad1337-b", "owner");
    let member = fx.public_session("usr-cad1337-member", "member");
    let account_a = owner_a["session"]["account_id"]
        .as_str()
        .expect("issued owner account")
        .to_owned();
    let account_b = owner_b["session"]["account_id"]
        .as_str()
        .expect("issued second owner account")
        .to_owned();
    assert_ne!(
        account_a, account_b,
        "distinct verified issuer/workspace/subject principals need distinct accounts"
    );

    // No client account/uploader label is authority, and an unbound/missing
    // account proof must fail before a row or retained byte appears.
    assert_refusal(
        fx.call(
            Asserted::Operator,
            "chat_file_upload",
            json!({"name":"ticket.txt","tmp":fx.stage(SECRET_BYTES),"conversation":fx.master_home}),
        ),
        "account",
        "upload with no session and no local admin account",
    );
    assert_eq!(
        fx.row_count(),
        before,
        "missing account proof retained metadata"
    );
    for (label, proof) in [
        (
            "forged token",
            json!({"token":"not-a-session","key":"","origin":"public"}),
        ),
        (
            "wrong origin",
            json!({"token":owner_a["token"],"key":"","origin":"loopback"}),
        ),
    ] {
        let row_before = fx.row_count();
        let failure = assert_refusal(
            fx.upload(&owner_a, SECRET_BYTES, json!({"session":proof})),
            "session",
            label,
        );
        assert!(
            !failure.contains("not provably the operator"),
            "test seam premise must pass the unchanged caller layer: {failure}"
        );
        assert_eq!(
            fx.row_count(),
            row_before,
            "{label} left attachment metadata"
        );
    }
    let row_before = fx.row_count();
    assert_refusal(
        fx.upload(&member, SECRET_BYTES, json!({})),
        "authenticated operator",
        "verified ordinary member cannot upload as owner",
    );
    assert_eq!(
        fx.row_count(),
        row_before,
        "ordinary member upload retained metadata"
    );
    for field in ["account_id", "user_id", "uploader", "scope", "path"] {
        let mut claims = serde_json::Map::new();
        claims.insert(field.into(), json!(account_b));
        let row_before = fx.row_count();
        let error = assert_refusal(
            fx.upload(&owner_a, SECRET_BYTES, Value::Object(claims)),
            "not accepted",
            "browser-supplied identity/custody claim",
        );
        assert!(
            !error.contains("not provably the operator"),
            "caller premise did not reach the parameter guard: {error}"
        );
        assert_eq!(
            fx.row_count(),
            row_before,
            "forged {field} retained metadata"
        );
    }

    // A real authenticated account uploads to the actual Home thread. The
    // original bytes must be in the daemon-authoritative per-user/thread
    // workspace, not the vault or a duplicate sidecar.
    let uploaded_a = fx.upload(&owner_a, SECRET_BYTES, json!({})).unwrap();
    let id_a = uploaded_a["id"]
        .as_str()
        .expect("daemon-minted id")
        .to_owned();
    let workspace = fx._dir.path().join("workspace");
    let original = workspace
        .join("users")
        .join(&account_a)
        .join("threads")
        .join(&fx.master_home)
        .join("uploads")
        .join(&id_a)
        .join("ticket.txt");
    assert_eq!(
        std::fs::read(&original).unwrap(),
        SECRET_BYTES,
        "new bytes must be the actual immutable workspace original"
    );
    let tracker = fx._dir.path().join("tracker");
    assert!(
        !workspace.starts_with(fx._dir.path().join("vault")),
        "upload entered the knowledge vault"
    );
    assert!(
        !workspace.starts_with(&tracker) && !tracker.starts_with(&workspace),
        "workspace and tracker roots overlap"
    );
    let uploaded_b = fx.upload(&owner_b, SECRET_BYTES, json!({})).unwrap();
    let id_b = uploaded_b["id"]
        .as_str()
        .expect("daemon-minted id")
        .to_owned();
    assert_ne!(
        id_a, id_b,
        "identical target scope cannot collapse distinct accounts"
    );
    let original_b = workspace
        .join("users")
        .join(&account_b)
        .join("threads")
        .join(&fx.master_home)
        .join("uploads")
        .join(&id_b)
        .join("ticket.txt");
    assert_eq!(std::fs::read(&original_b).unwrap(), SECRET_BYTES);
    let retained_entries = std::fs::read_dir(original_b.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(
        retained_entries,
        vec![std::ffi::OsString::from("ticket.txt")],
        "attachment dir must contain only immutable original bytes, no editable sidecar"
    );
    assert!(
        !workspace.join(".blobs").exists(),
        "operational upload bytes must not create a wiki blob store"
    );

    // An actual thread belonging to another native alias is not a Home
    // conversation for Master; malformed/foreign Home selectors refuse.
    let made = fx
        .call(
            Asserted::Operator,
            "thread_send",
            json!({"alias":"worker","text":"seed other Home","message":"cad1337-other-home"}),
        )
        .unwrap();
    assert_eq!(made["state"], json!("queued"));
    let other_home = fx.shared.store.thread("worker").unwrap().unwrap().id;
    let row_before = fx.row_count();
    assert_refusal(
        fx.upload(&owner_a, SECRET_BYTES, json!({"conversation":other_home})),
        "conversation",
        "wrong Home conversation selector",
    );
    assert_eq!(
        fx.row_count(),
        row_before,
        "wrong Home selector retained metadata"
    );

    // Exact active turn plus persisted attachment envelope is the only
    // Master read control. A live token alone or a different account's id
    // learned outside that envelope cannot widen reads.
    let sent = fx.call(Asserted::Operator, "thread_send", json!({
        "alias":"master","text":"read only attached file","message":"cad1337-envelope-control",
        "attachments":[{"id":id_a}],
    })).unwrap();
    assert_eq!(sent["state"], json!("queued"));
    let token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(GENERATION);
    let db = rusqlite::Connection::open(crate::rollout::db_file(fx._dir.path())).unwrap();
    db.execute(
        "UPDATE messages SET state='submitting', started=1.0 WHERE id=?",
        ["cad1337-envelope-control"],
    )
    .unwrap();
    fx.shared
        .store
        .mark_running("cad1337-envelope-control", &token)
        .unwrap();
    let read = fx
        .call(
            Asserted::Agent("master".into()),
            "chat_file_read",
            json!({"id":id_a,"message":"cad1337-envelope-control","token":token}),
        )
        .unwrap();
    assert_eq!(
        read["text"],
        json!(String::from_utf8_lossy(SECRET_BYTES).as_ref()),
        "valid exact-envelope control"
    );
    let no_envelope = assert_refusal(
        fx.call(
            Asserted::Agent("master".into()),
            "chat_file_read",
            json!({"id":id_b,"message":"cad1337-envelope-control","token":token}),
        ),
        "not on your turn's envelope",
        "cross-account/off-envelope Master read",
    );
    assert!(
        !no_envelope.contains("ticket-derived private upload"),
        "refusal leaked private bytes"
    );
    assert_refusal(
        fx.call(
            Asserted::Agent("master".into()),
            "chat_file_read",
            json!({"id":id_a,"message":"cad1337-envelope-control","token":"forged-turn-token"}),
        ),
        "active assigned turn",
        "forged/out-of-turn Master read",
    );
    let worker_read = assert_refusal(
        fx.call(
            Asserted::Agent("worker".into()),
            "chat_file_read",
            json!({"id":id_a,"message":"cad1337-envelope-control","token":token}),
        ),
        "only the master",
        "ordinary agent attempting private upload read",
    );
    assert!(
        !worker_read.contains(String::from_utf8_lossy(SECRET_BYTES).as_ref()),
        "worker refusal leaked bytes"
    );

    // Explicit operator administration is intentionally retained: the
    // operator fixture may read either account's valid row through the same
    // checked custody path; no ordinary-agent allowance is inferred.
    let admin_read = fx
        .call(Asserted::Operator, "chat_file_read", json!({"id":id_b}))
        .unwrap();
    assert_eq!(
        admin_read["text"],
        json!(String::from_utf8_lossy(SECRET_BYTES).as_ref())
    );

    // Ownership/location metadata stays authoritative in SQLite; no
    // per-upload editable sidecar or vault blob is created.
    let columns = {
        let mut stmt = db.prepare("PRAGMA table_info(chat_files)").unwrap();
        stmt.query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    for required in ["user_id", "thread_id", "location_version"] {
        assert!(
            columns.iter().any(|column| column == required),
            "SQLite attachment authority lacks {required}: {columns:?}"
        );
    }
    let persisted: (String, String, i64) = db
        .query_row(
            "SELECT user_id,thread_id,location_version FROM chat_files WHERE id=?",
            [&id_a],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        persisted,
        (account_a, fx.master_home.clone(), 1),
        "SQLite ownership/location must record the actual account, Home thread and version"
    );
    assert!(
        !workspace.join("vault").exists(),
        "workspace fixture unexpectedly contains a vault copy"
    );
    assert_eq!(
        fx.row_count(),
        before + 2,
        "refused cases or dedup altered the two successful account rows"
    );
}
