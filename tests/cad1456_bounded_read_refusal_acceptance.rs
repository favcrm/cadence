//! Independent CAD-1456 acceptance: real daemon RPC and verified-envelope HTTP.
//! The test seam supplies only the native relay peer. Ed25519 verification,
//! signed prefix ceilings, native ACLs and revision refusal run unmodified.
//! Implementation authors must not edit or weaken this check.
#![cfg(feature = "test-seam")]

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use cadence_agent::test_seam::{scoped, Asserted, Seam};
use cadence_agent::{adapter::ProviderEnv, client, daemon, issue::Pm, ui, wiki};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering::SeqCst},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tiny_http::{Header, Response, Server};

const HOST: &str = "cad1456.cadencecloud.test";
const ORG: &str = "ws_cad1456_independent";
const KID: &str = "cad1456-independent-key";
const PAGE: &str = "global/covered/original.md";
const SECRET: &str = "users/other/secret.md";
const SECRET_BODY: &str = "PRIVATE-SOURCE-SENTINEL";

fn port() -> u16 {
    (3110..=3199)
        .find(|p| TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .expect("no isolated acceptance port")
}

struct Rig {
    root: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<cadence_agent::Result<()>>>,
    port: u16,
    issuer: String,
    key: Ed25519KeyPair,
    seam: String,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}

impl Rig {
    fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("c1456-")
            .tempdir_in("/tmp")
            .unwrap();
        for dir in ["home", "xdg", "locks", "s"] {
            std::fs::create_dir(root.path().join(dir)).unwrap();
        }
        // These settings affect this isolated integration-test process only.
        // Managed identity variables are never removed or rewritten.
        std::env::set_var("HOME", root.path().join("home"));
        std::env::set_var("XDG_STATE_HOME", root.path().join("xdg"));
        std::env::set_var("XDG_CONFIG_HOME", root.path().join("home/config"));
        std::env::set_var("XDG_DATA_HOME", root.path().join("home/data"));
        std::env::set_var("TMPDIR", root.path());
        std::env::set_var("CADENCE_SUITE_LOCK", root.path().join("locks/suite"));
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let mut rig = Self {
            root,
            stop,
            threads: Vec::new(),
            port: 0,
            issuer: String::new(),
            key: Ed25519KeyPair::from_seed_unchecked(&[146; 32]).unwrap(),
            seam: String::new(),
        };
        let provider_env = ProviderEnv::refusing_providers();
        provider_env.set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        let opts = daemon::ServeOptions {
            provider_env,
            stop: Some(rig.stop.clone()),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        let state = rig.state();
        rig.threads
            .push(thread::spawn(move || daemon::serve_with(&state, opts)));
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if client::rpc_timeout(&rig.state(), "health", json!({}), Duration::from_secs(1))
                .is_ok()
            {
                if let Some(token) = Seam::token_at(&rig.state()) {
                    rig.seam = token;
                    break;
                }
            }
            assert!(!rig.threads[0].is_finished(), "fixture daemon exited");
            assert!(Instant::now() < deadline, "fixture daemon startup timeout");
            thread::sleep(Duration::from_millis(50));
        }
        let jwks = Server::http(("127.0.0.1", port())).unwrap();
        rig.issuer = format!("http://{}", jwks.server_addr());
        let public_key = URL_SAFE_NO_PAD.encode(rig.key.public_key().as_ref());
        let stop = rig.stop.clone();
        rig.threads.push(thread::spawn(move || {
            while !stop.load(SeqCst) {
                if let Some(request) = jwks.recv_timeout(Duration::from_millis(100)).unwrap() {
                    assert_eq!(request.url(), "/.well-known/agenticos-board-jwks.json");
                    request
                        .respond(
                            Response::from_string(
                                json!({"keys": [{
                                    "kty":"OKP", "crv":"Ed25519", "kid":KID, "x":public_key
                                }]})
                                .to_string(),
                            )
                            .with_header(
                                Header::from_bytes("Content-Type", "application/json").unwrap(),
                            ),
                        )
                        .unwrap();
                }
            }
            Ok(())
        }));
        rig.port = port();
        let (ready, wait) = std::sync::mpsc::channel();
        let opts = ui::ServeOpts {
            host: "127.0.0.1".into(),
            port: rig.port,
            allow_hosts: vec![HOST.into()],
            board_public_only: true,
            public: Some(ui::PublicBoard {
                host: HOST.into(),
                issuer: rig.issuer.clone(),
                company: ORG.into(),
                authorize_url: format!("{}/authorize", rig.issuer),
            }),
            test_seam: true,
            stop: Some(rig.stop.clone()),
            startup: Some(ready),
            gh: Some("/bin/false".into()),
            ..Default::default()
        };
        let state = rig.state();
        rig.threads
            .push(thread::spawn(move || ui::serve(&state, &pm.dir, &opts)));
        wait.recv_timeout(Duration::from_secs(30)).unwrap().unwrap();
        rig
    }

    fn state(&self) -> PathBuf {
        self.root.path().join("s")
    }

    fn rpc(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(who, || client::rpc(&self.state(), method, params))
    }

    fn put(&self, path: &str, text: &str) -> Value {
        self.rpc(
            Asserted::Operator,
            "wiki_write",
            json!({"path":path, "text":text}),
        )
        .unwrap()
    }

    fn sign(&self, jti: &str, member: bool, prefix: &str) -> String {
        let now = cadence_agent::issue::time::now_epoch();
        let claims = json!({
            "iss":self.issuer, "aud":format!("https://{HOST}"), "sub":"usr_fixture",
            "actor":if member { "users/member" } else { "operator" },
            "principal_kind":"user", "role_at_issue":if member { "member" } else { "owner" },
            "organization_id":ORG, "scope":["wiki.read"], "wiki_prefixes":[prefix],
            "prefix_grants":[{"scope":"wiki.read", "prefixes":[prefix]}],
            "credential_id":"hcp_independent", "iat":now, "exp":now+120, "jti":jti
        });
        let head = URL_SAFE_NO_PAD.encode(
            json!({
                "alg":"EdDSA", "typ":"agenticos-wiki-actor/1", "kid":KID
            })
            .to_string(),
        );
        let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
        let signed = format!("{head}.{payload}");
        format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(self.key.sign(signed.as_bytes()).as_ref())
        )
    }

    fn get(&self, path: &str, envelope: &str) -> (u16, Vec<u8>, Value) {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(15)))
            .max_redirects(0)
            .build()
            .into();
        let mut response = agent
            .get(format!("http://127.0.0.1:{}{path}", self.port))
            .header("Host", HOST)
            .header("Authorization", format!("Bearer wikienv_{envelope}"))
            .header("X-Cadence-Test-As", "operator")
            .header("X-Cadence-Test-Token", &self.seam)
            .call()
            .expect("real board HTTP response");
        let status = response.status().as_u16();
        // Decode HTTP framing, but retain the exact JSON payload bytes for
        // the native line-bound assertion rather than reserializing a Value.
        let bytes = response
            .body_mut()
            .with_config()
            .limit(1_048_576)
            .read_to_vec()
            .expect("complete decoded board response body");
        let value = serde_json::from_slice(&bytes).expect("JSON response from real board");
        (status, bytes, value)
    }
}

fn assert_range(value: &Value, path: &str, rev: &str, original: &str, start: usize, end: usize) {
    assert_eq!(value["kind"], "text");
    assert_eq!(value["path"], path);
    assert_eq!(value["rev"], rev);
    assert_eq!(
        value["range"],
        json!({"start":start, "end":end, "total":original.len()})
    );
    assert_eq!(
        value["text"].as_str().unwrap().as_bytes(),
        &original.as_bytes()[start..end]
    );
}

fn assert_refusal(status: u16, body: &[u8], value: &Value, sentinel: &str) {
    assert!(
        (400..600).contains(&status),
        "unexpected status {status}: {value}"
    );
    assert!(value.get("text").is_none(), "refusal exposed text: {value}");
    assert!(
        value.get("range").is_none(),
        "refusal exposed a range: {value}"
    );
    assert!(
        !String::from_utf8_lossy(body).contains(sentinel),
        "refusal exposed source bytes"
    );
}

#[test]
fn verified_http_and_rpc_refuse_private_sources_and_stale_ranges() {
    let rig = Rig::start();
    let original = format!(
        "AAéBODY-SENTINEL\n{}",
        "quote\" slash\\ tab\t line\n".repeat(4_000)
    );
    rig.put(PAGE, &original);
    rig.put(SECRET, SECRET_BODY);
    rig.put("global/outside/secret.md", SECRET_BODY);
    let full = rig
        .rpc(Asserted::Operator, "wiki_read", json!({"path":PAGE}))
        .unwrap();
    assert_eq!(full["text"], original);
    assert!(full.get("range").is_none(), "unbounded read changed");
    let rev = full["rev"].as_str().unwrap();
    let exact = rig
        .rpc(
            Asserted::Agent("range-fixture".into()),
            "wiki_read",
            json!({
                "path":PAGE, "offset":2, "bytes":2, "rev":rev
            }),
        )
        .unwrap();
    assert_range(&exact, PAGE, rev, &original, 2, 4);
    let url = format!("/api/wiki/file?path={PAGE}&offset=2&bytes=2&rev={rev}");
    let (status, _, body) = rig.get(&url, &rig.sign("exact", false, "global/covered/"));
    assert_eq!(status, 200, "{body}");
    assert_range(&body, PAGE, rev, &original, 2, 4);

    // Assert the ACTUAL serialized HTTP response, including JSON escapes.
    let (status, bytes, body) = rig.get(
        &format!("/api/wiki/file?path={PAGE}&offset=0&rev={rev}"),
        &rig.sign("fitting", false, "global/covered/"),
    );
    assert_eq!(status, 200, "{body}");
    assert!(
        bytes.len() + 1 < 50_000,
        "HTTP JSON was {} bytes",
        bytes.len()
    );
    let end = body["range"]["end"].as_u64().unwrap() as usize;
    assert!(end > 0 && end < original.len());
    assert_range(&body, PAGE, rev, &original, 0, end);

    // The real CLI stdout serializer must preserve the same bound and slice.
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    command
        .arg("--state-dir")
        .arg(rig.state())
        .args(["wiki", "cat", PAGE, "--offset", "0", "--rev", rev])
        .current_dir(rig.root.path())
        .env("HOME", rig.root.path().join("home"))
        .env_remove("CADENCE_HOME")
        .env("CADENCE_PM_DIR", rig.root.path().join("pm"))
        .env("CADENCE_STATE_DIR", rig.state())
        .env("XDG_STATE_HOME", rig.root.path().join("xdg"))
        .env("XDG_CONFIG_HOME", rig.root.path().join("home/config"))
        .env("XDG_DATA_HOME", rig.root.path().join("home/data"))
        .env("XDG_CACHE_HOME", rig.root.path().join("home/cache"))
        .env("TMPDIR", rig.root.path())
        .env("CADENCE_SUITE_LOCK", rig.root.path().join("locks/suite"))
        .env("CADENCE_TEST_AS", "operator");
    let output = cadence_agent::proc::run_bounded(&mut command, Duration::from_secs(30)).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len() < 50_000);
    assert_eq!(output.stdout.iter().filter(|b| **b == 10).count(), 1);
    let cli: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_range(
        &cli,
        PAGE,
        rev,
        &original,
        0,
        cli["range"]["end"].as_u64().unwrap() as usize,
    );

    let denied = rig
        .rpc(
            Asserted::Agent("range-fixture".into()),
            "wiki_read",
            json!({
                "path":SECRET, "offset":0, "bytes":8, "rev":rev
            }),
        )
        .unwrap_err()
        .to_string();
    assert!(denied.contains("private"), "wrong refusal: {denied}");
    assert!(!denied.contains(SECRET_BODY));
    // This signed prefix covers the target: the native member ACL must deny.
    let (status, bytes, body) = rig.get(
        &format!("/api/wiki/file?path={SECRET}&offset=0&bytes=8&rev={rev}"),
        &rig.sign("member-native-acl", true, "users/other/"),
    );
    assert_refusal(status, &bytes, &body, SECRET_BODY);
    assert!(
        String::from_utf8_lossy(&bytes).contains("private"),
        "wrong refusal: {body}"
    );
    // Prefix refusal takes precedence over even malformed range arguments.
    let (status, bytes, body) = rig.get(
        "/api/wiki/file?path=global/outside/secret.md&offset=bad&bytes=8",
        &rig.sign("prefix-before-range", false, "global/covered/"),
    );
    assert_eq!(status, 403);
    assert_eq!(body["error"]["code"], "insufficient_scope");
    assert_refusal(status, &bytes, &body, SECRET_BODY);

    for (i, query) in [
        "offset=-1",
        "offset=1.5",
        "offset=18446744073709551616",
        "bytes=-1",
        "bytes=1.5",
        "bytes=18446744073709551616",
        "rev=",
    ]
    .iter()
    .enumerate()
    {
        let (status, bytes, body) = rig.get(
            &format!("/api/wiki/file?path={PAGE}&{query}"),
            &rig.sign(&format!("invalid-{i}"), false, "global/covered/"),
        );
        assert_eq!(status, 400, "{query}: {body}");
        assert_refusal(status, &bytes, &body, "BODY-SENTINEL");
    }
    for fields in [
        json!({"offset":-1}),
        json!({"bytes":1.5}),
        json!({"offset":"1"}),
        json!({"rev":false}),
    ] {
        let mut params = fields;
        params["path"] = json!(PAGE);
        let refused = rig
            .rpc(Asserted::Operator, "wiki_read", params)
            .unwrap_err()
            .to_string();
        assert!(!refused.contains("BODY-SENTINEL"));
    }

    let current = "CHANGED-SOURCE-SENTINEL";
    rig.put(PAGE, current);
    let stale = rig
        .rpc(
            Asserted::Operator,
            "wiki_read",
            json!({
                "path":PAGE, "offset":0, "bytes":8, "rev":rev
            }),
        )
        .unwrap_err()
        .to_string();
    assert!(stale.contains("revision"));
    assert!(!stale.contains(current));
    assert!(!stale.contains("BODY-SENTINEL"));
    let (status, bytes, body) = rig.get(&url, &rig.sign("stale", false, "global/covered/"));
    assert_refusal(status, &bytes, &body, current);
    assert!(
        String::from_utf8_lossy(&bytes).contains("revision"),
        "wrong refusal: {body}"
    );
    assert!(!String::from_utf8_lossy(&bytes).contains("BODY-SENTINEL"));
    let full = rig
        .rpc(Asserted::Operator, "wiki_read", json!({"path":PAGE}))
        .unwrap();
    let current_rev = full["rev"].as_str().unwrap();
    let (status, _, body) = rig.get(
        &format!("/api/wiki/file?path={PAGE}&offset=0&bytes=7&rev={current_rev}"),
        &rig.sign("current-revision-control", false, "global/covered/"),
    );
    assert_eq!(status, 200, "{body}");
    assert_range(&body, PAGE, current_rev, current, 0, 7);
    let vault = wiki::vault_dir(&Pm::at(&rig.root.path().join("pm")).unwrap()).unwrap();
    assert_eq!(
        std::fs::read(vault.join(SECRET)).unwrap(),
        SECRET_BODY.as_bytes()
    );
    assert_eq!(std::fs::read(vault.join(PAGE)).unwrap(), current.as_bytes());
}
