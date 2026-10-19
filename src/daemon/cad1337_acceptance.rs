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
                    params: (alias == "master").then_some(r#"{"model":"fixture/model"}"#),
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
        }
        crate::test_seam::seed_pi_fixture_policy(&shared.store, "fixture/model").unwrap();
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

    // A valid session for account A cannot read account B's real row; this
    // reaches the account-ownership guard, not Master's envelope guard.
    let cross_account = assert_refusal(
        fx.call(
            Asserted::Operator,
            "chat_file_read",
            json!({"id":id_a,"session":{"token":owner_b["token"],"key":"","origin":"public"}}),
        ),
        "attachment is not bound to this authenticated account and conversation",
        "owner B session attempting to read owner A attachment",
    );
    assert!(
        !cross_account.contains(String::from_utf8_lossy(SECRET_BYTES).as_ref()),
        "cross-account read refusal disclosed retained bytes"
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

/// A symlink at the operational account component must be refused by the
/// actual upload custody path, after a positive upload control. The outside
/// target is fixture-owned and its sentinel must remain the only entry.
#[cfg(unix)]
#[test]
fn cad1337_native_upload_refuses_symlinked_account_path() {
    let fx = Fx::new();
    let owner = fx.public_session("usr-cad1337-symlink", "owner");
    let account = owner["session"]["account_id"].as_str().unwrap().to_owned();
    let positive = fx
        .upload(&owner, b"symlink positive control\n", json!({}))
        .unwrap();
    assert!(positive["id"].as_str().is_some());
    let users = fx._dir.path().join("workspace/users");
    let account_path = users.join(&account);
    let saved = users.join(format!("{account}.saved"));
    let outside = fx._dir.path().join("outside-sentinel");
    std::fs::create_dir(&outside).unwrap();
    let sentinel = outside.join("sentinel.txt");
    std::fs::write(&sentinel, b"outside fixture sentinel\n").unwrap();
    std::fs::rename(&account_path, &saved).unwrap();
    std::os::unix::fs::symlink(&outside, &account_path).unwrap();
    let before = fx.row_count();
    assert_refusal(
        fx.upload(&owner, b"must not escape\n", json!({})),
        "quota accounting",
        "upload through symlinked operational account path",
    );
    assert_eq!(fx.row_count(), before, "symlink refusal retained a row");
    assert_eq!(
        std::fs::read(&sentinel).unwrap(),
        b"outside fixture sentinel\n"
    );
    assert_eq!(
        std::fs::read_dir(&outside).unwrap().count(),
        1,
        "outside path gained upload bytes"
    );
}

/// Only `users/.gitignore` is control metadata; metadata-named residue in the
/// users tree is charged. Files are sparse, but stat size exceeds the actual
/// per-scope cap. This is expected to fail on the unfixed implementation.
#[test]
fn cad1337_quota_charges_nested_metadata_named_residue() {
    let fx = Fx::new();
    let owner = fx.public_session("usr-cad1337-quota", "owner");
    let account = owner["session"]["account_id"].as_str().unwrap().to_owned();
    let uploaded = fx
        .upload(&owner, b"positive quota control\n", json!({}))
        .unwrap();
    let id = uploaded["id"].as_str().unwrap();
    let thread = &fx.master_home;
    let users = fx._dir.path().join("workspace/users");
    assert_eq!(std::fs::read(users.join(".gitignore")).unwrap(), b"*\n");
    let nested = users
        .join(&account)
        .join("threads")
        .join(thread)
        .join("uploads/residue");
    std::fs::create_dir_all(&nested).unwrap();
    let file_size = 257 * 1024 * 1024u64 / 3 + 1;
    for path in [
        users.join(".quota.lock"),
        nested.join(".gitignore"),
        nested.join(".quota.lock"),
    ] {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        file.set_len(file_size).unwrap();
    }
    let before = fx.row_count();
    assert_refusal(
        fx.upload(&owner, b"quota refusal attempt\n", json!({})),
        "quota",
        "metadata-named residue exceeding actual upload quota",
    );
    assert_eq!(fx.row_count(), before, "quota refusal retained metadata");
    assert!(
        !nested.join("ticket.txt").exists(),
        "quota refusal published bytes"
    );
    assert!(
        users
            .join(&account)
            .join("threads")
            .join(thread)
            .join("uploads")
            .join(id)
            .join("ticket.txt")
            .exists(),
        "positive upload control disappeared"
    );
}

/// Independent CAD-1337 root-admission acceptance: private originals must not
/// inherit any recursive Master filesystem grant. Only the actual Shared boot
/// constructor runs on empty disposable stores; no agent, actor, provider,
/// listener, authentication or upload is started. The first control proves
/// ordinary separate roots and harmless extra grants remain usable. On the
/// unfixed baseline the negative boots succeed, and the final assertion lists
/// every admitted unsafe configuration rather than stopping at the first one.
#[cfg(unix)]
#[test]
fn cad1337_master_grant_overlap_refuses_private_workspace_boot() {
    fn options(state: &Path, tracker: &Path, home: &Path, cadence_home: &Path) -> ServeOptions {
        std::fs::create_dir_all(state).unwrap();
        std::fs::create_dir_all(home).unwrap();
        std::fs::create_dir_all(cadence_home).unwrap();
        crate::issue::Pm::init(tracker).unwrap();
        let opts = ServeOptions::default();
        opts.provider_env.set("HOME", home.to_str().unwrap());
        opts.provider_env
            .set("CADENCE_HOME", cadence_home.to_str().unwrap());
        opts.provider_env
            .set("CADENCE_PM_DIR", tracker.to_str().unwrap());
        // Override inherited extras only in this instance's captured env.
        // These are the real configuration names consumed by both adapters.
        opts.provider_env
            .set(crate::master::CONFINE_EXTRA_READ_ENV, "");
        opts.provider_env
            .set(crate::master::CONFINE_EXTRA_WRITE_ENV, "");
        opts
    }

    let safe = tempfile::Builder::new()
        .prefix("c1337-g-")
        .tempdir()
        .unwrap();
    let safe_state = safe.path().join("s");
    let safe_tracker = safe.path().join("t");
    let safe_home = safe.path().join("h");
    let safe_private = safe.path().join("p");
    let safe_opts = options(&safe_state, &safe_tracker, &safe_home, &safe_private);
    for (env, leaf) in [
        (crate::master::CONFINE_EXTRA_READ_ENV, "r"),
        (crate::master::CONFINE_EXTRA_WRITE_ENV, "w"),
    ] {
        let grant = safe.path().join(leaf);
        std::fs::create_dir(&grant).unwrap();
        safe_opts.provider_env.set(env, grant.to_str().unwrap());
    }
    let shared = Shared::new(&safe_state, &safe_opts)
        .unwrap_or_else(|error| panic!("separate private workspace boot control refused: {error}"));
    assert_eq!(shared.upload_workspace_dir, safe_private.join("workspace"));
    drop(shared);

    let mut admitted = Vec::new();
    for case in [
        "master-cwd",
        "master-tmp",
        "master-pi",
        "master-claude",
        "extra-read-ancestor",
        "extra-write-ancestor",
        "extra-read-alias",
        "extra-write-alias",
    ] {
        let fixture = tempfile::Builder::new()
            .prefix("c1337-g-")
            .tempdir()
            .unwrap();
        let state = fixture.path().join("s");
        let tracker = fixture.path().join("t");
        let home = fixture.path().join("h");
        let grant_ancestor = fixture.path().join("p");
        // Use production state-derived policy paths, not a test-side copy
        // of a proposed isolation guard or a pretend provider command.
        let private = match case {
            "master-cwd" => crate::master::workdir(&state).join("private"),
            "master-tmp" => crate::master::tmpdir(&state).join("private"),
            "master-pi" => crate::master::provider_config_dir("pi", &state).join("private"),
            "master-claude" => crate::master::provider_config_dir("claude", &state).join("private"),
            _ => grant_ancestor.join("private"),
        };
        let opts = options(&state, &tracker, &home, &private);
        let workspace = private.join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let sentinel = workspace.join("sentinel.txt");
        std::fs::write(&sentinel, b"fixture-owned private sentinel\n").unwrap();
        let extra = match case {
            "extra-read-ancestor" | "extra-read-alias" => {
                Some(crate::master::CONFINE_EXTRA_READ_ENV)
            }
            "extra-write-ancestor" | "extra-write-alias" => {
                Some(crate::master::CONFINE_EXTRA_WRITE_ENV)
            }
            _ => None,
        };
        if let Some(env) = extra {
            let grant = if case.ends_with("alias") {
                let alias = fixture.path().join("grant-alias");
                // The authority workspace path is entirely real. Only the
                // existing Landlock grant is a symlink, so rejection cannot
                // be credited to the old workspace-path symlink check.
                std::os::unix::fs::symlink(&workspace, &alias).unwrap();
                assert_eq!(alias.canonicalize().unwrap(), workspace);
                alias
            } else {
                grant_ancestor
            };
            opts.provider_env.set(env, grant.to_str().unwrap());
        }
        match Shared::new(&state, &opts) {
            Ok(shared) => {
                assert_eq!(shared.upload_workspace_dir, workspace);
                admitted.push(case);
                drop(shared);
            }
            Err(error) => {
                let reason = error.to_string().to_ascii_lowercase();
                assert!(
                    (reason.contains("workspace") || reason.contains("upload"))
                        && (reason.contains("grant")
                            || reason.contains("confin")
                            || reason.contains("isolation"))
                        && (reason.contains("overlap")
                            || reason.contains("expos")
                            || reason.contains("unsafe")),
                    "{case} refused for an unrelated boot error, not private-root isolation: {error}"
                );
            }
        }
        assert_eq!(
            std::fs::read(&sentinel).unwrap(),
            b"fixture-owned private sentinel\n",
            "{case} boot changed private fixture bytes"
        );
    }
    assert!(
        admitted.is_empty(),
        "private workspace boot admitted recursive Master grant overlap: {admitted:?}"
    );
}

/// Independently authored exact-launch-policy acceptance. A literal process
/// alias belongs to the future confined child, not the validating daemon;
/// traversal or ordinary symlink spellings must not turn it into daemon
/// process authority. The checked private root remains authoritative after
/// mutable env changes. Only the real guard consumed by both adapters runs;
/// no actor, provider, process lookup, Landlock exec or service is started.
#[cfg(target_os = "linux")]
#[test]
fn cad1337_master_launch_policy_process_alias_and_bound_root_refusals() {
    use crate::adapter::ProviderEnv;
    use crate::confine::Policy;
    use std::path::PathBuf;

    let fixture = tempfile::Builder::new()
        .prefix("c1337-p-")
        .tempdir()
        .unwrap();
    let tracker = fixture.path().join("t");
    let home = fixture.path().join("h");
    let private = fixture.path().join("p");
    let sibling = fixture.path().join("r");
    let sibling_write = fixture.path().join("w");
    for dir in [&tracker, &home, &private, &sibling, &sibling_write] {
        std::fs::create_dir(dir).unwrap();
    }
    let workspace = crate::home::workspace_for_instance(
        &tracker,
        Some(private.to_str().unwrap()),
        Some(home.to_str().unwrap()),
    )
    .expect("separate private authority must resolve through the real root checker");
    std::fs::create_dir(&workspace).unwrap();
    let marker = workspace.join("marker.txt");
    std::fs::write(&marker, b"original private authority marker\n").unwrap();
    let mutable_env = ProviderEnv::default();
    mutable_env.set("HOME", home.to_str().unwrap());
    mutable_env.set("CADENCE_HOME", private.to_str().unwrap());
    let bound_env = mutable_env.with_workspace(workspace.clone());

    // Ordinary aliases normalize to their real sibling authority; literal
    // child-relative /proc spellings must reach the encoded policy unchanged.
    let harmless_alias = fixture.path().join("safe-alias");
    std::os::unix::fs::symlink(&sibling, &harmless_alias).unwrap();
    let policy = Policy {
        read: vec![
            harmless_alias,
            PathBuf::from("/proc/self"),
            PathBuf::from("/proc/thread-self/status"),
        ],
        write: vec![sibling_write.clone()],
    };
    let checked = crate::home::validate_policy_for_env(policy, &bound_env)
        .expect("harmless sibling and literal child-relative policy must validate");
    assert_eq!(
        checked.read,
        vec![
            sibling,
            PathBuf::from("/proc/self"),
            PathBuf::from("/proc/thread-self/status"),
        ],
        "validation changed child-relative spelling or expanded sibling authority"
    );
    assert_eq!(checked.write, vec![sibling_write]);

    let proc_alias = fixture.path().join("proc-alias");
    std::os::unix::fs::symlink("/proc/self", &proc_alias).unwrap();
    let chained_alias = fixture.path().join("proc-chain");
    std::os::unix::fs::symlink("proc-alias", &chained_alias).unwrap();
    let thread_alias = fixture.path().join("thread-alias");
    std::os::unix::fs::symlink("/proc/thread-self", &thread_alias).unwrap();
    for (case, path) in [
        ("raw-parent-self", PathBuf::from("/tmp/../proc/self")),
        (
            "raw-parent-thread-status",
            PathBuf::from("/tmp/../proc/thread-self/status"),
        ),
        ("ordinary-proc-symlink", proc_alias.clone()),
        ("chained-proc-symlink", chained_alias),
        ("qualified-proc-symlink", proc_alias.join("status")),
        ("qualified-thread-symlink", thread_alias.join("status")),
        (
            "literal-process-parent-traversal",
            PathBuf::from("/proc/self/../thread-self/status"),
        ),
    ] {
        for kind in ["read", "write"] {
            let policy = if kind == "read" {
                Policy {
                    read: vec![path.clone()],
                    write: Vec::new(),
                }
            } else {
                Policy {
                    read: Vec::new(),
                    write: vec![path.clone()],
                }
            };
            let error = crate::home::validate_policy_for_env(policy, &bound_env)
                .expect_err(&format!("{case} became accepted {kind} process authority"));
            let reason = error.to_string().to_ascii_lowercase();
            assert!(
                reason.contains("process-relative") && reason.contains("workspace isolation"),
                "{case} {kind} refused for an unrelated reason: {error}"
            );
        }
    }

    // `set` and `remove` affect the real shared map, including clones. Neither
    // may replace the already-bound custody root with a recomputed location.
    let later_home = fixture.path().join("later-h");
    let later_private = fixture.path().join("later-p");
    std::fs::create_dir(&later_home).unwrap();
    std::fs::create_dir(&later_private).unwrap();
    for change in ["replace", "remove"] {
        if change == "replace" {
            mutable_env.set("HOME", later_home.to_str().unwrap());
            mutable_env.set("CADENCE_HOME", later_private.to_str().unwrap());
        } else {
            mutable_env.remove("HOME");
            mutable_env.remove("CADENCE_HOME");
        }
        for kind in ["read", "write"] {
            let policy = if kind == "read" {
                Policy {
                    read: vec![workspace.clone()],
                    write: Vec::new(),
                }
            } else {
                Policy {
                    read: Vec::new(),
                    write: vec![workspace.clone()],
                }
            };
            let error = crate::home::validate_policy_for_env(policy, &bound_env.clone())
                .expect_err(&format!(
                    "{change} mutable env lost original private {kind} root"
                ));
            let reason = error.to_string().to_ascii_lowercase();
            assert!(
                reason.contains("workspace")
                    && reason.contains("overlap")
                    && reason.contains("filesystem grant"),
                "{change} mutable env {kind} refused for an unrelated reason: {error}"
            );
        }
    }
    assert_eq!(
        std::fs::read(marker).unwrap(),
        b"original private authority marker\n",
        "policy validation altered original private marker bytes"
    );
}

/// Independent custody-parent acceptance: private children must not be
/// replaceable through a non-sticky writable workspace or its owned parent.
/// Only disposable fixture directories change mode. The existing signed
/// session and native dispatcher exercise real upload custody; no actor,
/// provider, invented UID or malicious relocation is involved.
#[cfg(unix)]
#[test]
fn cad1337_upload_refuses_writable_workspace_and_parent() {
    use std::os::unix::fs::PermissionsExt;

    let safe = Fx::new();
    let safe_owner = safe.public_session("usr-cad1337-parent-safe", "owner");
    let safe_workspace = safe._dir.path().join("workspace");
    std::fs::create_dir(&safe_workspace).unwrap();
    std::fs::set_permissions(&safe_workspace, std::fs::Permissions::from_mode(0o755)).unwrap();
    let safe_bytes = b"safe existing workspace custody control\n";
    let safe_row = safe.upload(&safe_owner, safe_bytes, json!({})).unwrap();
    let safe_original = safe_workspace
        .join("users")
        .join(safe_row["user_id"].as_str().unwrap())
        .join("threads")
        .join(&safe.master_home)
        .join("uploads")
        .join(safe_row["id"].as_str().unwrap())
        .join("ticket.txt");
    assert_eq!(std::fs::read(&safe_original).unwrap(), safe_bytes);
    assert_eq!(
        safe.row_count(),
        1,
        "safe upload did not retain exactly one row"
    );
    assert_eq!(
        std::fs::metadata(safe_workspace.join("users"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o700,
        "safe workspace must retain the existing private users boundary"
    );

    let mut admitted = Vec::new();
    for case in ["workspace", "parent"] {
        let fx = Fx::new();
        let owner = fx.public_session("usr-cad1337-parent-unsafe", "owner");
        let workspace = fx._dir.path().join("workspace");
        assert_eq!(fx.shared.upload_workspace_dir, workspace);
        std::fs::create_dir(&workspace).unwrap();
        std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o755)).unwrap();
        let marker = workspace.join("sentinel.txt");
        let marker_bytes = b"preexisting fixture custody marker\n";
        std::fs::write(&marker, marker_bytes).unwrap();
        let writable = if case == "workspace" {
            workspace.as_path()
        } else {
            fx._dir.path()
        };
        // Non-sticky mode: another UID could rename a private child even
        // without access to its bytes. Never chmod /tmp or another fixture.
        std::fs::set_permissions(writable, std::fs::Permissions::from_mode(0o777)).unwrap();
        let before = fx.row_count();
        match fx.upload(
            &owner,
            b"unsafe parent must not retain this original\n",
            json!({}),
        ) {
            Ok(_) => admitted.push(case),
            Err(error) => {
                let reason = error.to_string().to_ascii_lowercase();
                assert!(
                    (reason.contains("workspace") || reason.contains("custody"))
                        && (reason.contains("writable")
                            || reason.contains("mode")
                            || reason.contains("ownership")
                            || reason.contains("permissions")
                            || reason.contains("daemon-owned")),
                    "{case} refused for an unrelated account, context or storage error: {error}"
                );
                assert_eq!(fx.row_count(), before, "{case} refusal retained metadata");
                assert!(
                    !workspace.join("users").exists(),
                    "{case} refusal created a users tree or custody residue"
                );
                let entries = std::fs::read_dir(&workspace)
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name())
                    .collect::<Vec<_>>();
                assert_eq!(
                    entries,
                    vec![std::ffi::OsString::from("sentinel.txt")],
                    "{case} refusal retained operational files"
                );
                let legacy = fx._dir.path().join("tracker/.cadence/chat-files");
                if legacy.exists() {
                    for entry in std::fs::read_dir(legacy).unwrap() {
                        let name = entry.unwrap().file_name();
                        assert!(
                            name == ".gitignore" || name == ".quota.lock",
                            "{case} refusal retained a legacy original or quota residue: {name:?}"
                        );
                    }
                }
            }
        }
        assert_eq!(std::fs::read(marker).unwrap(), marker_bytes);
        assert_eq!(
            std::fs::metadata(writable).unwrap().permissions().mode() & 0o7777,
            0o777,
            "{case} custody silently changed the unsafe directory mode"
        );
    }
    assert_eq!(std::fs::read(safe_original).unwrap(), safe_bytes);
    assert_eq!(
        safe.row_count(),
        1,
        "unsafe cases altered the safe control metadata"
    );
    assert_eq!(
        std::fs::metadata(safe_workspace)
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o755,
        "custody must not change the acceptable safe workspace mode"
    );
    assert!(
        admitted.is_empty(),
        "production upload custody admitted non-sticky writable parents: {admitted:?}"
    );
}

#[cfg(unix)]
#[test]
fn cad1553_image_company_authority_is_strict_and_bound_at_shared_boot() {
    use crate::platform::deployments::DeploymentMetadata;

    const EXACT: &str = r#"{"schema":1,"master_trust_boundary":"company-container@1","providers":[{"provider":"agenticos","origin":"http://api.internal","manifest_pin":"agenticos-manifest@1/publish_post@2"}]}"#;
    const ORDINARY: &str = r#"{"schema":1,"providers":[{"provider":"agenticos","origin":"http://api.internal","manifest_pin":"agenticos-manifest@1/publish_post@2"}]}"#;
    let valid: Value = serde_json::from_str(EXACT).unwrap();
    let trusted = DeploymentMetadata::parse(EXACT.as_bytes())
        .expect("exact versioned image assertion must be accepted");
    let ordinary = DeploymentMetadata::parse(ORDINARY.as_bytes()).unwrap();
    assert!(trusted.hosted_company_master().is_some());
    assert!(
        ordinary.hosted_company_master().is_none(),
        "an ordinary provider pin is not company-wide file authority"
    );

    let mut bad = Vec::new();
    for value in [
        json!(null),
        json!(true),
        json!(1),
        json!(""),
        json!("company-container@2"),
        json!("COMPANY-CONTAINER@1"),
        json!("company-container@1 "),
        json!(["company-container@1"]),
        json!({"mode":"company-container@1"}),
    ] {
        let mut candidate = valid.clone();
        candidate["master_trust_boundary"] = value;
        bad.push(candidate.to_string());
    }
    for (field, value) in [
        ("provider", "other"),
        ("origin", "https://api.internal"),
        ("origin", "http://api.internal:80"),
        ("origin", "http://api.internal/"),
        ("origin", "http://other.internal"),
        ("manifest_pin", "agenticos-manifest@1/publish_post@1"),
        ("manifest_pin", "agenticos-manifest@1/publish_post@3"),
    ] {
        let mut candidate = valid.clone();
        candidate["providers"][0][field] = json!(value);
        bad.push(candidate.to_string());
    }
    let mut no_provider = valid.clone();
    no_provider["providers"] = json!([]);
    bad.push(no_provider.to_string());
    let mut unknown_field = valid.clone();
    unknown_field["company_master"] = json!(true);
    bad.push(unknown_field.to_string());
    let mut unknown_schema = valid.clone();
    unknown_schema["schema"] = json!(2);
    bad.push(unknown_schema.to_string());
    bad.push(EXACT.replacen("\"schema\":1", "\"schema\":1,\"schema\":1", 1));
    bad.push(EXACT.replacen(
        r#""master_trust_boundary":"company-container@1""#,
        r#""master_trust_boundary":"company-container@1","master_trust_boundary":"company-container@1""#,
        1,
    ));
    bad.push("{".to_string());
    for candidate in bad {
        let error = DeploymentMetadata::parse(candidate.as_bytes())
            .expect_err("malformed, unknown or mismatched image authority was accepted");
        assert!(
            error
                .to_string()
                .contains("trusted provider deployment metadata refused"),
            "wrong metadata refusal: {error}; candidate={candidate}"
        );
    }

    // Exercise actual Shared composition, not only the proof accessor.
    // The pre-bound stale proof is deliberately supplied in the mutable opts
    // for negative cases: only the current metadata may authorize this boot.
    for case in ["absent", "ordinary-provider", "exact"] {
        let root = tempfile::Builder::new()
            .prefix("c1553-boot-")
            .tempdir()
            .unwrap();
        let state = root.path().join("s");
        let tracker = root.path().join("t");
        let home = root.path().join("h");
        let private = root.path().join("p");
        for path in [&state, &home, &private] {
            std::fs::create_dir(path).unwrap();
        }
        crate::issue::Pm::init(&tracker).unwrap();
        let mut opts = ServeOptions {
            provider_env: crate::adapter::ProviderEnv::refusing_providers()
                .with_company_master(trusted.hosted_company_master()),
            ..ServeOptions::default()
        };
        opts.provider_env.set("HOME", home.to_str().unwrap());
        opts.provider_env
            .set("CADENCE_HOME", private.to_str().unwrap());
        opts.provider_env
            .set("CADENCE_PM_DIR", tracker.to_str().unwrap());
        opts.provider_env
            .set(crate::master::CONFINE_EXTRA_READ_ENV, "");
        opts.provider_env
            .set(crate::master::CONFINE_EXTRA_WRITE_ENV, "");
        opts.provider_env
            .set("master_trust_boundary", "company-container@1");
        opts.provider_env
            .set("CADENCE_MASTER_TRUST_BOUNDARY", "company-container@1");
        opts.provider_env.set("CADENCE_COMPANY_MASTER", "true");
        opts.provider_env.set("CADENCE_PROVIDER_DEPLOYMENTS", EXACT);
        opts.provider_deployments = match case {
            "absent" => None,
            "ordinary-provider" => Some(ordinary.clone()),
            "exact" => Some(trusted.clone()),
            _ => unreachable!(),
        };
        let shared = Shared::new(&state, &opts)
            .unwrap_or_else(|error| panic!("{case} disposable Shared boot failed: {error}"));
        assert_eq!(shared.upload_workspace_dir, private.join("workspace"));
        assert_eq!(
            shared.provider_env.company_master(),
            case == "exact",
            "{case}: mutable env or a stale proof replaced current image authority"
        );
        // Mutation through the original shared map and a clone must neither
        // manufacture authority on negative boots nor erase the positive one.
        let mutable = opts.provider_env.clone();
        mutable.set("master_trust_boundary", "company-container@2");
        mutable.remove("CADENCE_PROVIDER_DEPLOYMENTS");
        assert_eq!(
            shared.provider_env.clone().company_master(),
            case == "exact"
        );
        assert_eq!(
            shared.provider_env.workspace(),
            Some(shared.upload_workspace_dir.as_path())
        );
    }
}
include!("cad1337_repair_acceptance.rs");
