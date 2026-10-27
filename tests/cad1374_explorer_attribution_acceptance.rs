//! Independent CAD-1374 bounded Explorer attribution acceptance.
//! Exercises signed hosted sessions through the real board and daemon, and
//! checks the same SQLite transaction owns favorite state and its receipt.
#![cfg(feature = "test-seam")]

use base64::Engine;
use cadence_agent::test_seam::{scoped, Asserted};
use cadence_agent::{client, daemon, master};
use ring::signature::KeyPair;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tempfile::{Builder, TempDir};

const COMPANY: &str = "co_cad1374";
const KID: &str = "cad1374-explorer-key";
type Session = String;

struct EnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl EnvRestore {
    fn install(root: &Path) -> Self {
        let values = [
            ("HOME", root.join("home")),
            ("XDG_CONFIG_HOME", root.join("xdg-config")),
            ("XDG_CACHE_HOME", root.join("xdg-cache")),
            ("TMPDIR", root.to_path_buf()),
        ];
        for (_, path) in &values {
            std::fs::create_dir_all(path).unwrap();
        }
        let saved = values
            .iter()
            .map(|(key, _)| (*key, std::env::var_os(key)))
            .collect();
        for (key, value) in values {
            std::env::set_var(key, value);
        }
        Self(saved)
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (key, value) in self.0.drain(..) {
            if let Some(value) = value {
                std::env::set_var(key, value);
            } else {
                std::env::remove_var(key);
            }
        }
    }
}

struct Fx {
    root: TempDir,
    _env: EnvRestore,
    issuer: String,
    signer: ring::signature::Ed25519KeyPair,
    daemon_stop: Arc<AtomicBool>,
    daemon: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
    board_stop: Arc<AtomicBool>,
    board: Option<std::thread::JoinHandle<()>>,
    port: u16,
    jwks: Option<(Arc<tiny_http::Server>, std::thread::JoinHandle<()>)>,
}

impl Fx {
    fn new() -> Self {
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let root = Builder::new()
            .prefix("c1374expl-")
            .tempdir_in(base)
            .unwrap();
        let env = EnvRestore::install(root.path());
        let state = root.path().join("state");
        let pm = root.path().join("pm");
        std::fs::create_dir_all(&state).unwrap();
        cadence_agent::issue::Pm::init(&pm).unwrap();
        let mut yaml = std::fs::read_to_string(pm.join("pm.yaml")).unwrap();
        yaml.push_str("\npi:\n  models:\n    allow: [\"fake/model-1\"]\n    default: {master: \"fake/model-1\", worker: \"fake/model-1\"}\n");
        std::fs::write(pm.join("pm.yaml"), yaml).unwrap();

        let signer = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[0x74; 32]).unwrap();
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let jwks = json!({"keys":[{"kty":"OKP","crv":"Ed25519","kid":KID,"x":b64.encode(signer.public_key().as_ref())}]}).to_string();
        let (jwks_port, jwks_server) = (3110..3200)
            .find_map(|port| {
                tiny_http::Server::http(format!("127.0.0.1:{port}"))
                    .ok()
                    .map(|server| (port, Arc::new(server)))
            })
            .expect("free isolated JWKS loopback port");
        let issuer = format!("http://127.0.0.1:{jwks_port}");
        let jwks_thread_server = Arc::clone(&jwks_server);
        let jwks_thread = std::thread::spawn(move || {
            for request in jwks_thread_server.incoming_requests() {
                let _ = request.respond(tiny_http::Response::from_string(jwks.clone()));
            }
        });
        let port = (3110..3200)
            .find(|p| *p != jwks_port && TcpListener::bind(("127.0.0.1", *p)).is_ok())
            .expect("free isolated board loopback port");
        let public_host = format!("explorer.board.localhost:{port}");
        cadence_agent::board_identity::write_config(
            &state,
            &cadence_agent::board_identity::Config {
                host: public_host.clone(),
                issuer: issuer.clone(),
                company: COMPANY.into(),
            },
        )
        .unwrap();

        let provider_env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        provider_env.set("CADENCE_PM_DIR", pm.to_string_lossy().to_string());
        provider_env.set(
            "HOME",
            root.path().join("home").to_string_lossy().to_string(),
        );
        provider_env.set(
            "CADENCE_PI_COMMAND",
            format!(
                "python3 {}",
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/pi-gateway.py")
                    .display()
            ),
        );
        provider_env.set(master::TEST_NO_LANDLOCK, "1");
        let daemon_stop = Arc::new(AtomicBool::new(false));
        let mut opts = daemon::ServeOptions {
            provider_env,
            stop: Some(daemon_stop.clone()),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        cadence_agent::platform::local::register_at(
            &state,
            &mut opts,
            state.join("outbox"),
            format!("http://127.0.0.1:{port}"),
        );
        let daemon_dir = state.clone();
        let daemon = std::thread::spawn(move || daemon::serve_with(&daemon_dir, opts));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(1)).is_err()
            || cadence_agent::test_seam::Seam::token_at(&state).is_none()
        {
            if daemon.is_finished() {
                panic!("daemon exited during startup: {:?}", daemon.join().unwrap());
            }
            assert!(
                std::time::Instant::now() < deadline,
                "daemon startup timeout"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        scoped(Asserted::Operator, || {
            client::rpc(
                &state,
                "master_start",
                json!({"provider":"pi","unconfined":true}),
            )
        })
        .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let board_stop = Arc::new(AtomicBool::new(false));
        let board_opts = cadence_agent::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(board_stop.clone()),
            startup: Some(tx),
            test_seam: true,
            allow_hosts: vec![public_host.clone()],
            allow_origins: vec![format!("http://{public_host}")],
            public: Some(cadence_agent::ui::PublicBoard {
                host: public_host.clone(),
                authorize_url: format!("{issuer}/v2/board/authorize"),
                issuer: issuer.clone(),
                company: COMPANY.into(),
            }),
            ..Default::default()
        };
        let (dir, board_pm) = (state, pm);
        let board = std::thread::spawn(move || {
            let _ = cadence_agent::ui::serve(&dir, &board_pm, &board_opts);
        });
        rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
        Self {
            root,
            _env: env,
            issuer,
            signer,
            daemon_stop,
            daemon: Some(daemon),
            board_stop,
            board: Some(board),
            port,
            jwks: Some((jwks_server, jwks_thread)),
        }
    }

    fn state(&self) -> PathBuf {
        self.root.path().join("state")
    }
    fn host(&self) -> String {
        format!("explorer.board.localhost:{}", self.port)
    }
    fn rpc(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(Asserted::Operator, || {
            client::rpc(&self.state(), method, params)
        })
    }
    fn http(&self, method: &str, path: &str, cookie: Option<&str>, body: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: {}\r\nOrigin: http://{}\r\nSec-Fetch-Site: same-origin\r\nX-Cadence-Board: 1\r\nContent-Type: application/json\r\nConnection: close\r\n{}Content-Length: {}\r\n\r\n{body}", self.host(), self.host(), cookie.map(|c| format!("Cookie: {c}\r\n")).unwrap_or_default(), body.len()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        (status, response)
    }
    fn assertion(&self, sub: &str, role: &str, jti: &str) -> String {
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let claims = json!({"iss":self.issuer,"aud":self.host(),"sub":sub,"email":format!("{sub}@example.test"),"name":format!("label-{sub}"),"company":COMPANY,"role":role,"iat":now-1,"exp":now+59,"jti":jti});
        let signed = format!(
            "{}.{}",
            b64.encode(json!({"alg":"EdDSA","typ":"JWT","kid":KID}).to_string()),
            b64.encode(claims.to_string())
        );
        format!(
            "{signed}.{}",
            b64.encode(self.signer.sign(signed.as_bytes()))
        )
    }
    fn login(&self, sub: &str, role: &str) -> Session {
        let assertion = self.assertion(sub, role, &format!("jti-{sub}"));
        let (status, response) = self.http(
            "POST",
            "/__platform/session",
            None,
            &json!({"assertion":assertion}).to_string(),
        );
        assert_eq!(status, 200, "signed session open: {response}");
        response
            .lines()
            .find_map(|line| {
                line.strip_prefix("Set-Cookie: ")
                    .or_else(|| line.strip_prefix("set-cookie: "))
                    .map(|s| s.split(';').next().unwrap().to_string())
            })
            .expect("session cookie")
    }
    fn proof(cookie: &str) -> String {
        cookie.split_once('=').unwrap().1.to_string()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        if let Some(board) = self.board.take() {
            self.board_stop.store(true, SeqCst);
            let _ = board.join();
        }
        self.daemon_stop.store(true, SeqCst);
        if let Some(daemon) = self.daemon.take() {
            let _ = daemon.join();
        }
        if let Some((server, thread)) = self.jwks.take() {
            server.unblock();
            let _ = thread.join();
        }
    }
}

fn app_source(root: &Path) -> PathBuf {
    let source = root.join("synthetic-blog-app");
    for name in [
        "app.md",
        "workflows/blog-post.md",
        "rubrics/blog.md",
        "templates/brief.md",
        "templates/post.md",
    ] {
        let from = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/apps/blog-post")
            .join(name);
        let to = source.join(name);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(from, to).unwrap();
    }
    source
}

fn sql_favorites(fx: &Fx, owner: &str) -> Vec<String> {
    let conn = Connection::open(fx.state().join("cadence.sqlite3")).unwrap();
    let mut stmt = conn
        .prepare("SELECT install_id FROM app_favorites WHERE owner=? ORDER BY position")
        .unwrap();
    let rows = stmt
        .query_map([owner], |row| row.get::<_, String>(0))
        .unwrap();
    rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
}

fn activity(fx: &Fx, proof: &str, action: &str, sub: &str) -> Vec<Value> {
    fx.rpc(
        "activity_query",
        json!({"public_session":proof,"action":action,"sub":sub,"limit":200}),
    )
    .unwrap()["events"]
        .as_array()
        .unwrap()
        .clone()
}

fn raw_receipts(fx: &Fx, kind: &str) -> Vec<Value> {
    let conn = Connection::open(fx.state().join("cadence.sqlite3")).unwrap();
    let mut stmt = conn
        .prepare("SELECT payload FROM events WHERE kind=? ORDER BY seq")
        .unwrap();
    let rows = stmt
        .query_map([kind], |row| row.get::<_, String>(0))
        .unwrap();
    rows.map(|payload| serde_json::from_str(&payload.unwrap()).unwrap())
        .collect()
}

#[test]
fn explorer_writes_are_session_attributed_and_receipts_share_the_state_transaction() {
    let fx = Fx::new();
    let owner_a = fx.login("user-owner-a", "owner");
    let owner_b = fx.login("user-owner-b", "owner");
    let member = fx.login("user-member", "member");
    // A second live member lets the daemon prove that a valid member cookie
    // cannot be relabelled as another otherwise-authorized member handle.
    let _other_member = fx.login("user-member-b", "member");
    let proof_a = Fx::proof(&owner_a);
    let proof_b = Fx::proof(&owner_b);
    let proof_member = Fx::proof(&member);

    // A real, local synthetic workspace install makes the favorites RPC
    // validate a live catalog id; no external installation or customer data.
    let source = app_source(&fx.root.path().join("owned-fixtures"));
    let installed = fx
        .rpc("app_workspace_install", json!({"source":source}))
        .unwrap();
    let install_id = installed["install_id"]
        .as_str()
        .expect("synthetic install id")
        .to_string();

    // Public writes derive proof from the actual Host/cookie relay. Body
    // identity, role, company and bearer claims are not forwarded.
    for (cookie, expected_sub, forged_sub) in [
        (&owner_a, "user-owner-a", "forged-user"),
        (&owner_b, "user-owner-b", "user-owner-a"),
    ] {
        let (status, response) = fx.http("POST", "/api/app-favorites", Some(cookie), &json!({"install_ids":[install_id],"actor":forged_sub,"role":"member","company":"forged-company","public_session":"forged-cookie-canary","member_as":"user-member"}).to_string());
        assert_eq!(status, 200, "verified owner favorite save: {response}");
        let rows = activity(&fx, &proof_a, "favorites_saved", expected_sub);
        assert!(
            rows.iter().any(|row| row["human"]["sub"] == expected_sub
                && row["human"]["company"] == COMPANY
                && row["human"]["role"] == "operator"
                && row["human"]["session"].as_str().is_some()),
            "missing verified actor for {expected_sub}: {rows:?}"
        );
        let actor = rows
            .iter()
            .find(|row| row["human"]["sub"] == expected_sub)
            .unwrap();
        assert_eq!(actor["target"], "favorites:operator");
        assert_eq!(actor["outcome"], "saved");
        assert!(!actor.to_string().contains("forged-cookie-canary"));
        let receipt = raw_receipts(&fx, "app_favorites_saved")
            .into_iter()
            .find(|receipt| receipt["human"]["sub"] == expected_sub)
            .expect("raw core receipt");
        assert_eq!(receipt["schema_version"], 1);
        assert_eq!(receipt["action"], "favorites_saved");
        assert_eq!(receipt["outcome"], "saved");
        assert_eq!(receipt["resource"], "favorites:operator");
        assert!(receipt["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()));
        assert!(receipt["request_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()));
        assert_eq!(receipt["request_id"], receipt["correlation_id"]);
        assert!(receipt["digest"]
            .as_str()
            .is_some_and(|d| d.starts_with("sha256:")));
        assert!(!receipt.to_string().contains(&proof_a));
    }
    let a_events = activity(&fx, &proof_a, "favorites_saved", "user-owner-a");
    let b_events = activity(&fx, &proof_a, "favorites_saved", "user-owner-b");
    assert_ne!(a_events[0]["human"]["sub"], b_events[0]["human"]["sub"]);
    assert_ne!(
        a_events[0]["human"]["session"],
        b_events[0]["human"]["session"]
    );
    assert!(!a_events[0].to_string().contains(&proof_a));
    assert!(!b_events[0].to_string().contains(&proof_b));

    let (member_favorite, member_favorite_response) = fx.http(
        "POST",
        "/api/app-favorites",
        Some(&member),
        &json!({"install_ids":[install_id]}).to_string(),
    );
    assert_eq!(
        member_favorite, 200,
        "member favorite capability remains available: {member_favorite_response}"
    );
    let member_favorites = activity(&fx, &proof_a, "favorites_saved", "user-member");
    assert!(member_favorites
        .iter()
        .any(|row| row["human"]["sub"] == "user-member"
            && row["human"]["role"] == "member"
            && row["target"] == "favorites:user-member"));
    assert!(!member_favorites
        .iter()
        .any(|row| row.to_string().contains(&proof_member)));

    let (default_status, default_response) = fx.http(
        "POST",
        "/api/app-favorites/default",
        Some(&owner_a),
        &json!({"install_ids":[install_id]}).to_string(),
    );
    assert_eq!(
        default_status, 200,
        "owner default save: {default_response}"
    );
    let default_rows = activity(&fx, &proof_a, "favorites_default_saved", "user-owner-a");
    assert!(default_rows
        .iter()
        .any(|row| row["human"]["sub"] == "user-owner-a"
            && row["human"]["role"] == "operator"
            && row["target"] == "favorites:default"));

    // Wrong member claim paired with a valid, different member proof cannot
    // mutate shared favorites. The member proof cannot save as an owner.
    let before = sql_favorites(&fx, "operator");
    assert!(!before.is_empty());
    assert!(fx
        .rpc(
            "app_favorites_put",
            json!({"install_ids":[],"member_as":"user-member","public_session":proof_a})
        )
        .is_err());
    assert_eq!(sql_favorites(&fx, "operator"), before);
    assert!(fx
        .rpc(
            "app_favorites_put",
            json!({"install_ids":[],"public_session":proof_member})
        )
        .is_err());
    assert_eq!(sql_favorites(&fx, "operator"), before);
    assert!(
        fx.rpc(
            "app_favorites_put",
            json!({"install_ids":[],"member_as":"user-member-b","public_session":proof_member})
        )
        .is_err(),
        "valid member proof cannot be relabelled as a different live member"
    );
    assert_eq!(sql_favorites(&fx, "operator"), before);
    assert!(fx
        .rpc(
            "app_favorites_put_default",
            json!({"install_ids":[install_id],"public_session":proof_member})
        )
        .is_err());
    assert!(
        fx.rpc(
            "app_favorites_put",
            json!({"install_ids":[],"public_session":"not-a-live-session"})
        )
        .is_err(),
        "present invalid proof must not downgrade to local unknown-human write"
    );
    for operation in ["activity_query", "activity_export"] {
        assert!(
            fx.rpc(operation, json!({"public_session":proof_member,"limit":10}))
                .is_err(),
            "member cannot access owner trail via {operation}"
        );
    }
    let (missing_status, _) = fx.http(
        "POST",
        "/api/app-favorites",
        None,
        &json!({"install_ids":[]}).to_string(),
    );
    assert_ne!(
        missing_status, 200,
        "public write without cookie proof must not fall back to local operator"
    );
    let (member_default, _) = fx.http(
        "POST",
        "/api/app-favorites/default",
        Some(&member),
        &json!({"install_ids":[install_id]}).to_string(),
    );
    assert_ne!(member_default, 200, "member cannot save workspace defaults");

    // A real SQLite trigger aborts the event INSERT after the favorite DELETE;
    // transaction rollback must restore the nonempty state established above.
    let conn = Connection::open(fx.state().join("cadence.sqlite3")).unwrap();
    conn.execute_batch("CREATE TRIGGER cad1374_fail_favorite_receipt BEFORE INSERT ON events WHEN NEW.kind='app_favorites_saved' BEGIN SELECT RAISE(ABORT,'cad1374 receipt fault'); END;").unwrap();
    drop(conn);
    assert!(fx
        .rpc(
            "app_favorites_put",
            json!({"install_ids":[],"public_session":proof_a})
        )
        .is_err());
    assert_eq!(
        sql_favorites(&fx, "operator"),
        before,
        "favorite rows changed despite receipt abort"
    );
    Connection::open(fx.state().join("cadence.sqlite3"))
        .unwrap()
        .execute_batch("DROP TRIGGER cad1374_fail_favorite_receipt;")
        .unwrap();
    assert!(
        fx.rpc(
            "app_favorites_put",
            json!({"install_ids":[],"public_session":proof_a})
        )
        .is_ok(),
        "allowed positive control after removing fault"
    );
    assert!(sql_favorites(&fx, "operator").is_empty());

    // CAD-1422: the built-in catalog is retired, so an install request
    // resolves against a tombstone — a catalog id a historical
    // `Source::Builtin` install still names. Rewrite the synthetic
    // install's record to builtin provenance, exactly as a pre-retire
    // install stored it, then exercise the existing member-only request
    // capability and state transitions through the real HTTP routes.
    {
        let record = fx
            .root
            .path()
            .join("pm/.apps/installations")
            .join(&install_id)
            .join("record.yaml");
        let mut value: Value =
            serde_yaml::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
        assert_eq!(value["source"]["kind"].as_str(), Some("path"));
        value["source"] = json!({
            "kind": "builtin",
            "id": "retired-cat-app",
            "digest": installed["digest"].as_str().unwrap(),
        });
        std::fs::write(&record, serde_yaml::to_string(&value).unwrap()).unwrap();
    }
    let (catalog_status, catalog_response) = fx.http("GET", "/api/app-catalog", Some(&member), "");
    assert_eq!(
        catalog_status, 200,
        "member catalog read: {catalog_response}"
    );
    let catalog: Value =
        serde_json::from_str(catalog_response.split_once("\r\n\r\n").unwrap().1).unwrap();
    let catalog_id = catalog["catalog"].as_array().unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(catalog_id, "retired-cat-app", "tombstone id: {catalog}");
    assert_eq!(
        catalog["catalog"][0]["retired"],
        json!(true),
        "the resolved entry is the retired tombstone: {catalog}"
    );
    let (owner_request, _) = fx.http(
        "POST",
        "/api/app-catalog/request",
        Some(&owner_a),
        &json!({"catalog_id":catalog_id}).to_string(),
    );
    assert_ne!(
        owner_request, 200,
        "do not expand install-request permission to owners"
    );
    let (request_status, request_response) = fx.http("POST", "/api/app-catalog/request", Some(&member), &json!({"catalog_id":catalog_id,"actor":"user-owner-a","company":"other","role":"owner","public_session":"forged-cookie-canary"}).to_string());
    assert_eq!(request_status, 200, "member request: {request_response}");
    let request: Value =
        serde_json::from_str(request_response.split_once("\r\n\r\n").unwrap().1).unwrap();
    let request_id = request["id"].as_str().expect("request id").to_string();
    let created = activity(&fx, &proof_a, "install_request_created", "user-member");
    assert!(
        created
            .iter()
            .any(|row| row["human"]["sub"] == "user-member"
                && row["human"]["role"] == "member"
                && row["target"].as_str().unwrap().contains(&request_id)
                && row["outcome"] == "open"),
        "member request attribution missing: {created:?}"
    );
    let request_receipt = raw_receipts(&fx, "app_install_requested")
        .into_iter()
        .find(|receipt| receipt["id"] == request_id)
        .expect("durable request receipt");
    assert_eq!(request_receipt["human"]["sub"], "user-member");
    assert_eq!(request_receipt["human"]["role"], "member");
    assert_eq!(
        request_receipt["request_id"],
        request_receipt["correlation_id"]
    );
    assert!(!request_receipt.to_string().contains(&proof_member));
    let (repeat_status, repeat_response) = fx.http(
        "POST",
        "/api/app-catalog/request",
        Some(&member),
        &json!({"catalog_id":catalog_id}).to_string(),
    );
    assert_eq!(
        repeat_status, 200,
        "idempotent member request: {repeat_response}"
    );
    assert_eq!(
        serde_json::from_str::<Value>(repeat_response.split_once("\r\n\r\n").unwrap().1).unwrap()
            ["id"],
        request_id
    );
    assert_eq!(
        activity(&fx, &proof_a, "install_request_created", "user-member").len(),
        1,
        "open idempotent retry fabricated a receipt"
    );
    let (member_dismiss, _) = fx.http(
        "POST",
        "/api/app-requests/dismiss",
        Some(&member),
        &json!({"id":request_id}).to_string(),
    );
    assert_ne!(
        member_dismiss, 200,
        "member cannot dismiss an owner's request"
    );
    let (dismiss_status, dismiss_response) = fx.http(
        "POST",
        "/api/app-requests/dismiss",
        Some(&owner_a),
        &json!({"id":request_id,"actor":"forged"}).to_string(),
    );
    assert_eq!(dismiss_status, 200, "owner dismiss: {dismiss_response}");
    let dismissed = activity(&fx, &proof_a, "install_request_dismissed", "user-owner-a");
    assert!(dismissed
        .iter()
        .any(|row| row["human"]["sub"] == "user-owner-a"
            && row["target"].as_str().unwrap().contains(&request_id)
            && row["outcome"] == "dismissed"));
    let (reopen_status, reopen_response) = fx.http(
        "POST",
        "/api/app-catalog/request",
        Some(&member),
        &json!({"catalog_id":catalog_id}).to_string(),
    );
    assert_eq!(
        reopen_status, 200,
        "reopen member request: {reopen_response}"
    );
    assert_eq!(
        serde_json::from_str::<Value>(reopen_response.split_once("\r\n\r\n").unwrap().1).unwrap()
            ["id"],
        request_id
    );
    let reopened = activity(&fx, &proof_a, "install_request_reopened", "user-member");
    assert!(reopened
        .iter()
        .any(|row| row["human"]["sub"] == "user-member"
            && row["target"].as_str().unwrap().contains(&request_id)
            && row["outcome"] == "open"));
    assert!(!reopened
        .iter()
        .any(|row| row.to_string().contains("forged-cookie-canary")));
    for kind in [
        "app_favorites_saved",
        "app_favorites_default_saved",
        "app_install_requested",
        "app_install_request_dismissed",
        "app_install_request_reopened",
    ] {
        for receipt in raw_receipts(&fx, kind) {
            let stored = receipt.to_string();
            assert!(
                !stored.contains(&proof_a)
                    && !stored.contains(&proof_b)
                    && !stored.contains(&proof_member),
                "raw bearer leaked to {kind}"
            );
            assert!(
                !stored.contains("forged-cookie-canary")
                    && !stored.contains("private-payload-canary"),
                "untrusted browser payload leaked to {kind}"
            );
        }
    }
}
