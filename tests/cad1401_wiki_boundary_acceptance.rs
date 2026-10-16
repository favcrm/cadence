//! CAD-1401 / AOS-225: independently authored from the tickets by
//! wiki-boundary-codex-sol61 (active openai-codex/gpt-6.1-sol).
//! Implementation authors must not edit or weaken this check.
//!
//! Runs the REAL public-host HTTP dispatcher, Ed25519/JWKS verifier, wiki
//! handler and daemon RPC/store. Only the native board-to-daemon connection
//! uses the supported test-seam operator-peer fixture; signed actor, scope,
//! prefixes, signature and replay authorization are NEVER mocked. Local
//! agent RPC checks use that same native caller harness, not HTTP actor JSON.
//! This is container-handler acceptance, not a Docker image or hosted proxy
//! deployment test. No CLI -> AgenticOS issuer/proxy integration is claimed.
//! Blob/PDF upload and corpus ingestion are explicitly outside this check.
//!
//! Run under an admitted test slot:
//! cadence build-slot run test --org local -- env CARGO_BUILD_JOBS=4 cargo test
//!   --features test-seam --test cad1401_wiki_boundary_acceptance
//!   -- --nocapture --test-threads 2
#![cfg(feature = "test-seam")]

use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use cadence_agent::test_seam::{scoped, Asserted, Seam};
use cadence_agent::{adapter::ProviderEnv, client, daemon, issue::Pm, ui, wiki};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering::SeqCst},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tiny_http::{Header, Response, Server};

const HOST: &str = "cad1401.cadencecloud.test";
const ORG: &str = "ws_cad1401_fixture";
const KID: &str = "cad1401-independent-fixture";
const PAGE: &str = "global/covered/ordinary text + page.md";
const TEXT: &str = "# Independent wiki boundary\n\nquartzboundary acceptance needle\n";

fn port() -> u16 {
    (3110..=3199)
        .find(|p| TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .expect("no isolated port in 3110..3199")
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
        Self::start_with_read_only(false)
    }

    fn start_with_read_only(read_only: bool) -> Self {
        let root = tempfile::Builder::new()
            .prefix("c1401-")
            .tempdir_in("/tmp")
            .unwrap();
        for dir in ["home", "xdg", "tmp", "locks", "s"] {
            std::fs::create_dir(root.path().join(dir)).unwrap();
        }
        // Test process only: never clear managed identity or caller variables.
        std::env::set_var("HOME", root.path().join("home"));
        std::env::set_var("XDG_STATE_HOME", root.path().join("xdg"));
        std::env::set_var("XDG_CONFIG_HOME", root.path().join("home/config"));
        std::env::set_var("XDG_DATA_HOME", root.path().join("home/data"));
        std::env::set_var("TMPDIR", root.path());
        std::env::set_var("CADENCE_SUITE_LOCK", root.path().join("locks/suite"));
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let state = root.path().join("s");
        let stop = Arc::new(AtomicBool::new(false));
        let mut rig = Self {
            root,
            stop,
            threads: Vec::new(),
            port: 0,
            issuer: String::new(),
            key: Ed25519KeyPair::from_seed_unchecked(&[140; 32]).unwrap(),
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
        let run_state = state.clone();
        rig.threads
            .push(thread::spawn(move || daemon::serve_with(&run_state, opts)));
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            if client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(1)).is_ok() {
                if let Some(token) = Seam::token_at(&state) {
                    rig.seam = token;
                    break;
                }
            }
            if rig.threads[0].is_finished() {
                let outcome = rig.threads.remove(0).join();
                panic!("isolated daemon startup failed: {outcome:?}");
            }
            assert!(Instant::now() < until, "isolated daemon startup timed out");
            thread::sleep(Duration::from_millis(50));
        }
        let jwks = Server::http(("127.0.0.1", port())).unwrap();
        rig.issuer = format!("http://{}", jwks.server_addr());
        let public_key = URL_SAFE_NO_PAD.encode(rig.key.public_key().as_ref());
        let jwks_stop = rig.stop.clone();
        rig.threads.push(thread::spawn(move || {
            while !jwks_stop.load(SeqCst) {
                if let Some(request) = jwks.recv_timeout(Duration::from_millis(100)).unwrap() {
                    assert_eq!(request.url(), "/.well-known/agenticos-board-jwks.json");
                    let response = Response::from_string(
                        json!({"keys": [{
                            "kty": "OKP", "crv": "Ed25519", "kid": KID, "x": public_key
                        }]})
                        .to_string(),
                    )
                    .with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
                    request.respond(response).unwrap();
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
            read_only,
            public: Some(ui::PublicBoard {
                host: HOST.into(),
                issuer: rig.issuer.clone(),
                company: ORG.into(),
                authorize_url: format!("{}/authorize", rig.issuer),
            }),
            test_seam: true,
            stop: Some(rig.stop.clone()),
            startup: Some(ready),
            // Any unrelated delivery scan stays in this temp tracker, with
            // an inert gh executable rather than the operator's credentials.
            gh: Some("/bin/false".into()),
            ..Default::default()
        };
        let pm_dir = pm.dir.clone();
        rig.threads
            .push(thread::spawn(move || ui::serve(&state, &pm_dir, &opts)));
        wait.recv_timeout(Duration::from_secs(30))
            .expect("board startup disconnected")
            .unwrap();
        rig
    }

    fn op(&self, method: &str, params: Value) -> Value {
        scoped(Asserted::Operator, || {
            client::rpc(&self.root.path().join("s"), method, params)
        })
        .unwrap()
    }

    // Exact hostedCadenceWikiActorClaimsSchema/header names. The origin is
    // HTTPS even though the isolated upstream TCP hop is loopback HTTP.
    fn claims(&self, jti: &str) -> Value {
        let now = cadence_agent::issue::time::now_epoch();
        json!({
            "iss": self.issuer, "aud": format!("https://{HOST}"), "sub": "usr_fixture",
            "actor": "operator", "principal_kind": "user", "role_at_issue": "owner",
            "organization_id": ORG, "scope": ["wiki.read", "wiki.write"],
            "wiki_prefixes": ["global/covered/"],
            "prefix_grants": [
                {"scope": "wiki.read", "prefixes": ["global/covered/"]},
                {"scope": "wiki.write", "prefixes": ["global/covered/"]}
            ],
            "credential_id": "hcp_fixture", "iat": now, "exp": now + 120, "jti": jti
        })
    }

    fn sign(&self, claims: &Value) -> String {
        let h = URL_SAFE_NO_PAD.encode(
            json!({"alg": "EdDSA", "typ": "agenticos-wiki-actor/1", "kid": KID}).to_string(),
        );
        let p = URL_SAFE_NO_PAD.encode(claims.to_string());
        let signed = format!("{h}.{p}");
        format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(self.key.sign(signed.as_bytes()).as_ref())
        )
    }

    fn call(
        &self,
        method: &str,
        path: &str,
        env: Option<&str>,
        body: &Value,
    ) -> (u16, String, Value) {
        let bytes = if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(body).unwrap()
        };
        let mut socket = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        write!(socket, "{method} {path} HTTP/1.1\r\nHost: {HOST}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n", bytes.len()).unwrap();
        // Supported fixture for the board's native relay connection ONLY.
        // It does not carry the remote actor, scopes, grants or signature.
        write!(
            socket,
            "X-Cadence-Test-As: operator\r\nX-Cadence-Test-Token: {}\r\n",
            self.seam
        )
        .unwrap();
        if let Some(env) = env {
            write!(socket, "Authorization: Bearer wikienv_{env}\r\n").unwrap();
        }
        socket.write_all(b"\r\n").unwrap();
        socket.write_all(&bytes).unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).unwrap();
        let at = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let headers = String::from_utf8_lossy(&response[..at]).into_owned();
        let status = headers
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let value = serde_json::from_slice(&response[at + 4..])
            .unwrap_or_else(|_| json!({"raw": String::from_utf8_lossy(&response[at + 4..])}));
        (status, headers, value)
    }

    fn page_snapshot(&self) -> (String, Vec<(String, Vec<u8>)>) {
        fn walk(at: &std::path::Path, files: &mut Vec<(String, Vec<u8>)>) {
            if !at.exists() {
                return;
            }
            for entry in std::fs::read_dir(at).unwrap() {
                let p = entry.unwrap().path();
                if p.is_dir() {
                    walk(&p, files);
                } else {
                    files.push((p.to_string_lossy().into_owned(), std::fs::read(p).unwrap()));
                }
            }
        }
        let pm = self.root.path().join("pm");
        let output = cadence_agent::proc::run_bounded(
            std::process::Command::new("git").args([
                "-C",
                pm.to_str().unwrap(),
                "rev-parse",
                "HEAD",
            ]),
            Duration::from_secs(15),
        )
        .unwrap();
        assert!(output.status.success());
        let mut files = Vec::new();
        walk(&wiki::vault_dir(&Pm::at(&pm).unwrap()).unwrap(), &mut files);
        files.sort();
        (String::from_utf8(output.stdout).unwrap(), files)
    }
}

fn check(failures: &mut Vec<String>, name: &str, ok: bool, detail: impl std::fmt::Debug) {
    println!(
        "{} {name}: {detail:?}",
        if ok { "OBSERVED_OK" } else { "OBSERVED_FAIL" }
    );
    if !ok {
        failures.push(name.into());
    }
}

#[test]
fn signed_wiki_http_boundary_intersects_native_acl_scope_prefix_and_replay() {
    let rig = Rig::start();
    let mut failures = Vec::new();
    let put = json!({"path": PAGE, "text": TEXT, "if_rev": "none"});
    let owner = rig.sign(&rig.claims("owner-text-put"));
    let response = rig.call("PUT", "/api/wiki/file", Some(&owner), &put);
    check(
        &mut failures,
        "verified owner covered text PUT",
        response.0 == 200 && response.2["committed"] == true,
        &response,
    );

    let escaped = "global/outside/secret.md";
    rig.op(
        "wiki_write",
        json!({"path": escaped, "text": TEXT, "if_rev": "none"}),
    );
    let readonly = |id: &str| {
        let mut c = rig.claims(id);
        c["scope"] = json!(["wiki.read"]);
        c["prefix_grants"] = json!([{"scope":"wiki.read", "prefixes":["global/covered/"]}]);
        c
    };
    let read = rig.sign(&readonly("read-page"));
    let response = rig.call(
        "GET",
        "/api/wiki/file?path=global%2Fcovered%2Fordinary%20text%20%2B%20page.md",
        Some(&read),
        &Value::Null,
    );
    check(
        &mut failures,
        "covered GET file preserves spaces and plus",
        response.0 == 200 && response.2["text"] == TEXT,
        &response,
    );
    let response = rig.call(
        "GET",
        "/api/wiki/ls?path=global%2Fcovered",
        Some(&rig.sign(&readonly("listing"))),
        &Value::Null,
    );
    check(
        &mut failures,
        "covered GET ls",
        response.0 == 200
            && response.2["entries"]
                .as_array()
                .is_some_and(|es| es.iter().any(|e| e["path"] == PAGE)),
        &response,
    );
    let response = rig.call(
        "GET",
        "/api/wiki/search?q=quartzboundary&path=global",
        Some(&rig.sign(&readonly("search"))),
        &Value::Null,
    );
    let hits = response.2["matches"].as_array();
    check(
        &mut failures,
        "automatic search indexing and result prefix filtering",
        response.0 == 200
            && hits.is_some_and(|hs| {
                hs.iter().any(|h| h["path"] == PAGE)
                    && hs.iter().all(|h| {
                        h["path"]
                            .as_str()
                            .is_some_and(|p| p.starts_with("global/covered/"))
                    })
            }),
        &response,
    );
    let host_hits = rig.op(
        "wiki_search",
        json!({"q":"quartzboundary", "path":"global/covered"}),
    );
    check(
        &mut failures,
        "remote search equals covered host hits",
        response.2["matches"] == host_hits["matches"],
        &host_hits,
    );

    type NegativeScenario = (String, Value, Value, Option<(&'static str, u16)>);
    let mut negatives: Vec<NegativeScenario> = Vec::new();
    let mut agent = rig.claims("signed-remote-agent-put");
    agent["sub"] = json!("hca_fixture");
    agent["actor"] = json!("agents/hca_fixture");
    agent["principal_kind"] = json!("agent");
    agent["role_at_issue"] = json!("owner"); // owner parent never elevates its agent
    agent["credential_id"] = json!("hcc_fixture");
    negatives.push((
        "signed remote-agent PUT".into(),
        agent.clone(),
        put.clone(),
        Some(("insufficient_scope", 403)),
    ));
    negatives.push((
        "read-only PUT".into(),
        readonly("readonly-put"),
        put.clone(),
        Some(("insufficient_scope", 403)),
    ));
    for field in ["actor", "role", "org", "user", "wiki_as", "principal", "as"] {
        let mut body = put.clone();
        body[field] = json!("operator");
        negatives.push((
            format!("forged body {field}"),
            rig.claims(&format!("forged-{field}")),
            body,
            Some(("invalid_request", 400)),
        ));
    }
    let mut expired = rig.claims("expired");
    let now = cadence_agent::issue::time::now_epoch();
    expired["iat"] = json!(now - 180);
    expired["exp"] = json!(now - 60);
    negatives.push((
        "expired signed envelope".into(),
        expired,
        put.clone(),
        Some(("assertion_expired", 401)),
    ));
    let mut wrong_org = rig.claims("wrong-org");
    wrong_org["organization_id"] = json!("ws_foreign");
    negatives.push((
        "wrong organization".into(),
        wrong_org,
        put.clone(),
        Some(("not_a_member", 401)),
    ));
    let mut wrong_aud = rig.claims("wrong-audience");
    wrong_aud["aud"] = json!("https://other.test");
    negatives.push((
        "wrong audience".into(),
        wrong_aud,
        put.clone(),
        Some(("audience_mismatch", 401)),
    ));
    let mut wrong_issuer = rig.claims("wrong-issuer");
    wrong_issuer["iss"] = json!("https://untrusted.test");
    negatives.push((
        "wrong issuer".into(),
        wrong_issuer,
        put.clone(),
        Some(("issuer_mismatch", 401)),
    ));
    for path in [
        "global/outside/secret.md",
        "global/covered-evil/escape.md",
        "global/covered/../outside/escape.md",
        "/global/covered/absolute.md",
    ] {
        let body = json!({"path": path, "text": "escape attempted", "if_rev":"none"});
        negatives.push((
            format!("write prefix/normalization escape {path}"),
            rig.claims(&format!("escape-{}", negatives.len())),
            body,
            None,
        ));
    }
    let mut split = rig.claims("per-scope-write-prefix");
    split["wiki_prefixes"] = json!(["global/covered/", "global/outside/"]);
    split["prefix_grants"] = json!([
        {"scope":"wiki.read", "prefixes":["global/covered/", "global/outside/"]},
        {"scope":"wiki.write", "prefixes":["global/covered/"]}
    ]);
    negatives.push((
        "read prefix cannot substitute write prefix".into(),
        split,
        json!({"path":escaped, "text":"elevated"}),
        Some(("insufficient_scope", 403)),
    ));
    let mut agent_operator = agent.clone();
    agent_operator["jti"] = json!("agent-as-operator");
    agent_operator["actor"] = json!("operator");
    negatives.push((
        "signed agent cannot carry operator actor".into(),
        agent_operator,
        put.clone(),
        Some(("bad_claims", 401)),
    ));
    for (name, claims, body, exact) in negatives {
        let before = rig.page_snapshot();
        let response = rig.call("PUT", "/api/wiki/file", Some(&rig.sign(&claims)), &body);
        let refused = exact.map_or((400..500).contains(&response.0), |(code, status)| {
            response.0 == status && response.2["error"]["code"] == code
        });
        check(
            &mut failures,
            &name,
            refused && rig.page_snapshot() == before,
            &response,
        );
    }
    let private_path = "users/other/private.md";
    let private_text = "cad1401 other member private needle\n";
    let seeded_private = rig.op(
        "wiki_write",
        json!({"path": private_path, "text": private_text, "if_rev": "none"}),
    );
    check(
        &mut failures,
        "operator seeds another member's private page",
        seeded_private["committed"] == true,
        &seeded_private,
    );
    let mut member = rig.claims("native-member-acl");
    member["actor"] = json!("users/member");
    member["role_at_issue"] = json!("member");
    member["wiki_prefixes"] = json!(["users/other/"]);
    member["prefix_grants"] = json!([
        {"scope": "wiki.read", "prefixes": ["users/other/"]},
        {"scope": "wiki.write", "prefixes": ["users/other/"]}
    ]);
    let before_private = rig.page_snapshot();
    let cross_text = "cad1401 member cross-write must not land\n";
    let cross = rig.call(
        "PUT",
        "/api/wiki/file",
        Some(&rig.sign(&member)),
        &json!({"path": private_path, "text": cross_text, "if_rev": "none"}),
    );
    let holds_private = |files: &[(String, Vec<u8>)]| {
        files.iter().any(|(path, bytes)| {
            path.ends_with("/users/other/private.md") && bytes.as_slice() == private_text.as_bytes()
        })
    };
    check(
        &mut failures,
        "signed grant cannot widen native member ACL",
        cross.0 == 400
            && cross.2["error"].as_str().is_some()
            && cross.2.get("conflict").is_none()
            && cross.2["committed"] != true
            && rig.page_snapshot() == before_private
            && holds_private(&before_private.1)
            && holds_private(&rig.page_snapshot().1)
            && hides(&cross.2.to_string(), &[private_text, cross_text]),
        &cross,
    );
    // Cryptographic authenticity control: modify a real agent's signed actor
    // without re-signing. A 403 alone could otherwise be a blanket denial.
    let real_agent = rig.sign(&agent);
    let mut parts: Vec<String> = real_agent.split('.').map(str::to_owned).collect();
    agent["actor"] = json!("operator");
    agent["principal_kind"] = json!("user");
    parts[1] = URL_SAFE_NO_PAD.encode(agent.to_string());
    let before = rig.page_snapshot();
    let response = rig.call("PUT", "/api/wiki/file", Some(&parts.join(".")), &put);
    check(
        &mut failures,
        "tampered signed agent identity",
        response.0 == 401
            && response.2["error"]["code"] == "signature_invalid"
            && rig.page_snapshot() == before,
        &response,
    );
    let response = rig.call(
        "PUT",
        "/api/wiki/file",
        Some(&owner),
        &json!({"path":PAGE,"text":"replayed mutation"}),
    );
    check(
        &mut failures,
        "replay of successful text PUT",
        response.0 == 401
            && response.2["error"]["code"] == "assertion_replayed"
            && rig.page_snapshot() == before,
        &response,
    );
    let response = rig.call(
        "GET",
        "/api/wiki/file?path=global%2Foutside%2Fsecret.md",
        Some(&rig.sign(&readonly("read-escape"))),
        &Value::Null,
    );
    check(
        &mut failures,
        "read outside prefix",
        response.0 == 403 && response.2["error"]["code"] == "insufficient_scope",
        &response,
    );

    // No cookie or browser authority is minted by a wiki envelope; removing
    // it returns to the pre-existing browser session/CSRF gates.
    let response = rig.call(
        "GET",
        "/api/wiki/file?path=global%2Fcovered",
        None,
        &Value::Null,
    );
    check(
        &mut failures,
        "no-bearer browser read stays gated",
        response.0 == 401 && response.2["error"]["code"] == "board_session_required",
        &response,
    );
    let response = rig.call("PUT", "/api/wiki/file", None, &put);
    check(
        &mut failures,
        "no-bearer browser PUT stays gated",
        response.0 == 403
            && response.2["check"] == "x_cadence_board"
            && rig.page_snapshot() == before,
        &response,
    );
    let response = rig.call(
        "POST",
        "/api/cli/status",
        Some(&rig.sign(&readonly("no-wiki-cli-substitution"))),
        &json!({}),
    );
    check(
        &mut failures,
        "wiki envelope cannot substitute CLI scope",
        (400..500).contains(&response.0),
        &response,
    );

    let state = rig.root.path().join("s");
    let local_path = "agents/local_fixture/knowledge/ordinary.md";
    let local = scoped(Asserted::Agent("local_fixture".into()), || {
        client::rpc(
            &state,
            "wiki_write",
            json!({"path":local_path,"text":"local agent rights remain"}),
        )
    });
    check(
        &mut failures,
        "LOCAL agent own knowledge write unchanged",
        local.as_ref().is_ok_and(|v| v["committed"] == true),
        &local,
    );
    let before = rig.page_snapshot();
    let forged = scoped(Asserted::Agent("local_fixture".into()), || {
        client::rpc(
            &state,
            "wiki_write",
            json!({"path":PAGE,"text":"impersonated", "wiki_as":"operator"}),
        )
    });
    check(
        &mut failures,
        "LOCAL agent cannot forge wiki_as operator",
        forged.as_ref().is_err() && rig.page_snapshot() == before,
        &forged,
    );
    let local_read = scoped(Asserted::Agent("local_fixture".into()), || {
        client::rpc(&state, "wiki_read", json!({"path":local_path}))
    });
    check(
        &mut failures,
        "LOCAL agent read unchanged",
        local_read
            .as_ref()
            .is_ok_and(|v| v["text"] == "local agent rights remain"),
        &local_read,
    );
    // A separate board uses the supported ui --read-only option. Seed its
    // page through native operator setup, not a bypass of the HTTP guard.
    drop(rig);
    let rig = Rig::start_with_read_only(true);
    let seeded = rig.op(
        "wiki_write",
        json!({"path": PAGE, "text": TEXT, "if_rev": "none"}),
    );
    assert_eq!(seeded["committed"], true);
    let before = rig.page_snapshot();
    let owner = rig.sign(&rig.claims("board-read-only-owner-put"));
    let response = rig.call(
        "PUT",
        "/api/wiki/file",
        Some(&owner),
        &json!({"path": PAGE, "text": "read-only mutation attempted"}),
    );
    check(
        &mut failures,
        "read-only board refuses verified owner covered text PUT without side effects",
        response.0 == 403 && rig.page_snapshot() == before,
        &response,
    );
    let response = rig.call(
        "GET",
        "/api/wiki/file?path=global%2Fcovered%2Fordinary%20text%20%2B%20page.md",
        Some(&rig.sign(&rig.claims("board-read-only-get"))),
        &Value::Null,
    );
    check(
        &mut failures,
        "read-only board permits verified covered GET file",
        response.0 == 200 && response.2["text"] == TEXT,
        &response,
    );
    for (id, path, field) in [
        ("ls", "/api/wiki/ls?path=global%2Fcovered", "entries"),
        (
            "search",
            "/api/wiki/search?q=quartzboundary&path=global%2Fcovered",
            "matches",
        ),
    ] {
        let response = rig.call(
            "GET",
            path,
            Some(&rig.sign(&rig.claims(&format!("board-read-only-{id}")))),
            &Value::Null,
        );
        check(
            &mut failures,
            &format!("read-only board permits verified covered GET {id}"),
            response.0 == 200
                && response.2[field]
                    .as_array()
                    .is_some_and(|items| items.iter().any(|item| item["path"] == PAGE)),
            &response,
        );
    }
    check(
        &mut failures,
        "read-only board reads preserve wiki bytes and tracker HEAD",
        rig.page_snapshot() == before,
        (),
    );
    assert!(
        failures.is_empty(),
        "independent boundary acceptance failures: {failures:?}"
    );
}

fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn is_uuid(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    parts.len() == 5
        && parts[0].len() == 8
        && parts[1].len() == 4
        && parts[2].len() == 4
        && parts[3].len() == 4
        && parts[4].len() == 12
        && parts
            .iter()
            .all(|part| part.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn err_text(result: &cadence_agent::Result<Value>) -> String {
    match result {
        Ok(value) => value.to_string(),
        Err(error) => error.to_string(),
    }
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn manifest_entry(id: &str, path: &str, bytes: &[u8]) -> Value {
    json!({
        "id": id,
        "path": path,
        "kind": "text",
        "size": bytes.len() as u64,
        "sha256": sha256_hex(bytes),
        "if_rev": "none",
    })
}

fn transfer_manifest(root: &str, source: &str, entries: Vec<Value>) -> Value {
    json!({
        "version": 1,
        "source_id": source,
        "root": root,
        "entries": entries,
    })
}

fn staged_transfer_files(rig: &Rig) -> Vec<(String, Vec<u8>)> {
    fn walk(at: &std::path::Path, files: &mut Vec<(String, Vec<u8>)>) {
        if !at.exists() {
            return;
        }
        for entry in std::fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, files);
            } else {
                files.push((
                    path.to_string_lossy().into_owned(),
                    std::fs::read(&path).unwrap(),
                ));
            }
        }
    }
    let mut files = Vec::new();
    walk(&rig.root.path().join("s/wiki-transfers"), &mut files);
    files.sort();
    files
}

type TransferEffects = ((String, Vec<(String, Vec<u8>)>), Vec<(String, Vec<u8>)>);

fn transfer_effects(rig: &Rig) -> TransferEffects {
    (rig.page_snapshot(), staged_transfer_files(rig))
}

fn transfer_job_ids(rig: &Rig) -> Vec<String> {
    let conn = rusqlite::Connection::open_with_flags(
        rig.root.path().join("s/cadence.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    let mut stmt = conn
        .prepare("SELECT job_id FROM wiki_transfer_jobs ORDER BY job_id")
        .unwrap();
    stmt.query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn rpc_transfer(rig: &Rig, params: Value) -> cadence_agent::Result<Value> {
    let state = rig.root.path().join("s");
    scoped(Asserted::Operator, || {
        client::rpc(&state, "wiki_transfer", params)
    })
}

fn rpc_transfer_as(rig: &Rig, actor: &str, params: Value) -> cadence_agent::Result<Value> {
    let state = rig.root.path().join("s");
    scoped(Asserted::Agent(actor.into()), || {
        client::rpc(&state, "wiki_transfer", params)
    })
}

fn hides(text: &str, secrets: &[&str]) -> bool {
    secrets
        .iter()
        .all(|secret| secret.is_empty() || !text.contains(secret))
}

fn begin_receipt(value: &Value, root: &str, source: &str) -> bool {
    value["root"].as_str() == Some(root)
        && value["source_id"].as_str() == Some(source)
        && value["state"].as_str() == Some("open")
        && value["entries"].as_u64() == Some(1)
        && value["job_id"].as_str().is_some_and(is_uuid)
        && value["digest"].as_str().is_some_and(is_digest)
        && hides(&value.to_string(), &[KEEP_TEXT])
}

fn status_receipt(value: &Value, job_id: &str, digest: &str, root: &str, relative: &str) -> bool {
    let full = format!("{root}/{relative}");
    value["job_id"].as_str() == Some(job_id)
        && value["digest"].as_str() == Some(digest)
        && value["root"].as_str() == Some(root)
        && value["state"].as_str() == Some("open")
        && value["index"].as_str() == Some("pending")
        && value
            .get("next_offset")
            .is_some_and(|offset| offset.is_null())
        && value["entries"].as_array().is_some_and(|rows| {
            rows.len() == 1
                && rows[0]["entry_id"].as_str() == Some("fresh")
                && rows[0]["path"].as_str() == Some(relative)
                && rows[0]["full_path"].as_str() == Some(full.as_str())
                && rows[0]["outcome"].as_str() == Some("pending")
                && rows[0]["received"].as_u64() == Some(0)
        })
        && hides(&value.to_string(), &[KEEP_TEXT, FRESH])
}

const ROOT: &str = "global/covered/cad914-guard";
const KEEP: &str = "global/covered/cad914-guard/keep.md";
const KEEP_TEXT: &str = "cad914 keep needle must survive\n";
const FRESH: &str = "cad914 fresh synthetic page\n";
const SOURCE: &str = "cad914-synthetic-source";

/// CAD-914: real daemon `wiki_transfer` and signed HTTP. Implementation
/// authors must not edit or weaken this check. Invalid manifests leave no
/// job and no wiki/staging bytes. A changed job, root, digest, or caller
/// cannot resume, read, or retarget the owned job.
#[test]
fn cad914_transfer_guard_creates_owned_job_and_refuses_invalid_manifest_retarget_and_readonly_smuggle(
) {
    let rig = Rig::start();
    let mut failures = Vec::new();
    let seeded = rig.op(
        "wiki_write",
        json!({"path": KEEP, "text": KEEP_TEXT, "if_rev": "none"}),
    );
    assert_eq!(seeded["committed"], true);
    let baseline = transfer_effects(&rig);
    let fresh = FRESH.as_bytes();
    let manifest = transfer_manifest(
        ROOT,
        SOURCE,
        vec![manifest_entry("fresh", "fresh.md", fresh)],
    );
    let begun = rpc_transfer(
        &rig,
        json!({"op": "begin", "root": ROOT, "manifest": manifest}),
    );
    let owned = begun
        .as_ref()
        .ok()
        .filter(|value| begin_receipt(value, ROOT, SOURCE));
    check(
        &mut failures,
        "CAD914 daemon begin creates an owned unpublished job",
        owned.is_some() && transfer_effects(&rig) == baseline,
        &begun,
    );
    let (job_id, digest) = match owned {
        Some(value) => (
            value["job_id"].as_str().unwrap_or("").to_string(),
            value["digest"].as_str().unwrap_or("").to_string(),
        ),
        None => (String::new(), String::new()),
    };
    let secrets = [SOURCE, digest.as_str(), "fresh.md", KEEP_TEXT, FRESH];

    let retry = rpc_transfer(
        &rig,
        json!({"op": "begin", "root": ROOT, "job_id": job_id, "manifest": manifest}),
    );
    check(
        &mut failures,
        "CAD914 identical job manifest and actor retries the same receipt",
        retry.as_ref().is_ok_and(|value| {
            value["job_id"].as_str() == Some(job_id.as_str())
                && value["digest"].as_str() == Some(digest.as_str())
                && value["state"].as_str() == Some("open")
        }) && transfer_effects(&rig) == baseline,
        &retry,
    );
    let status = rpc_transfer(
        &rig,
        json!({"op": "status", "job_id": job_id, "root": ROOT}),
    );
    check(
        &mut failures,
        "CAD914 owner status returns the pinned job without file bytes",
        status
            .as_ref()
            .is_ok_and(|value| status_receipt(value, &job_id, &digest, ROOT, "fresh.md"))
            && transfer_effects(&rig) == baseline,
        &status,
    );

    let other_bytes = b"cad914 other synthetic page\n";
    let other_manifest = transfer_manifest(
        ROOT,
        SOURCE,
        vec![manifest_entry("fresh", "fresh.md", other_bytes)],
    );
    let retarget_digest = rpc_transfer(
        &rig,
        json!({"op": "begin", "root": ROOT, "job_id": job_id, "manifest": other_manifest}),
    );
    let still = rpc_transfer(
        &rig,
        json!({"op": "status", "job_id": job_id, "root": ROOT}),
    );
    check(
        &mut failures,
        "CAD914 changed manifest digest cannot retarget the job",
        retarget_digest.is_err()
            && hides(&err_text(&retarget_digest), &secrets)
            && still
                .as_ref()
                .is_ok_and(|value| value["digest"].as_str() == Some(digest.as_str()))
            && transfer_effects(&rig) == baseline,
        &retarget_digest,
    );

    let other_root = "global/covered/cad914-other";
    let other_root_manifest = transfer_manifest(
        other_root,
        SOURCE,
        vec![manifest_entry("fresh", "fresh.md", fresh)],
    );
    let retarget_root = rpc_transfer(
        &rig,
        json!({
            "op": "begin",
            "root": other_root,
            "job_id": job_id,
            "manifest": other_root_manifest,
        }),
    );
    let wrong_root_status = rpc_transfer(
        &rig,
        json!({"op": "status", "job_id": job_id, "root": other_root}),
    );
    let still = rpc_transfer(
        &rig,
        json!({"op": "status", "job_id": job_id, "root": ROOT}),
    );
    check(
        &mut failures,
        "CAD914 changed root cannot resume or read the job",
        retarget_root.is_err()
            && wrong_root_status.is_err()
            && hides(&err_text(&wrong_root_status), &secrets)
            && still.as_ref().is_ok_and(|value| {
                value["digest"].as_str() == Some(digest.as_str())
                    && value["root"].as_str() == Some(ROOT)
            })
            && transfer_effects(&rig) == baseline,
        (&retarget_root, &wrong_root_status),
    );

    let missing_job = "66666666-6666-4666-8666-666666666666";
    let missing = rpc_transfer(
        &rig,
        json!({"op": "status", "job_id": missing_job, "root": ROOT}),
    );
    check(
        &mut failures,
        "CAD914 unknown job cannot read the owned manifest",
        missing.is_err()
            && hides(&err_text(&missing), &secrets)
            && transfer_effects(&rig) == baseline,
        &missing,
    );

    let forged_actor = rpc_transfer(
        &rig,
        json!({"op": "status", "job_id": job_id, "root": ROOT, "actor": "users/member"}),
    );
    let still = rpc_transfer(
        &rig,
        json!({"op": "status", "job_id": job_id, "root": ROOT}),
    );
    check(
        &mut failures,
        "CAD914 client actor field cannot retarget the caller",
        forged_actor.is_err()
            && hides(&err_text(&forged_actor), &secrets)
            && still
                .as_ref()
                .is_ok_and(|value| value["digest"].as_str() == Some(digest.as_str()))
            && transfer_effects(&rig) == baseline,
        &forged_actor,
    );

    let foreign_binding = json!({
        "issuer": "https://untrusted.test",
        "organization_id": "ws_foreign",
        "audience": "https://other.test",
        "subject": "usr_other",
    });
    let rebound = rpc_transfer(
        &rig,
        json!({
            "op": "status",
            "job_id": job_id,
            "root": ROOT,
            "_transfer_binding": foreign_binding,
        }),
    );
    let still = rpc_transfer(
        &rig,
        json!({"op": "status", "job_id": job_id, "root": ROOT}),
    );
    check(
        &mut failures,
        "CAD914 forged authority binding cannot read or retarget the job",
        rebound.is_err()
            && hides(&err_text(&rebound), &secrets)
            && still
                .as_ref()
                .is_ok_and(|value| value["digest"].as_str() == Some(digest.as_str()))
            && transfer_effects(&rig) == baseline,
        &rebound,
    );

    let agent_params = json!({"op": "status", "job_id": job_id, "root": ROOT});
    let agent_status = rpc_transfer_as(&rig, "local_fixture", agent_params.clone());
    let agent_apply = rpc_transfer_as(
        &rig,
        "local_fixture",
        json!({"op": "apply", "job_id": job_id, "root": ROOT, "entry_id": "fresh"}),
    );
    let agent_chunk = rpc_transfer_as(
        &rig,
        "local_fixture",
        json!({
            "op": "chunk",
            "job_id": job_id,
            "root": ROOT,
            "entry_id": "fresh",
            "offset": 0,
            "data_base64": STANDARD.encode(fresh),
        }),
    );
    let agent_resume = rpc_transfer_as(
        &rig,
        "local_fixture",
        json!({"op": "begin", "root": ROOT, "job_id": job_id, "manifest": manifest}),
    );
    let agent_binding = rpc_transfer_as(
        &rig,
        "local_fixture",
        json!({
            "op": "status",
            "job_id": job_id,
            "root": ROOT,
            "_transfer_binding": {
                "issuer": rig.issuer,
                "organization_id": ORG,
                "audience": format!("https://{HOST}"),
                "subject": "usr_fixture",
            },
        }),
    );
    let agent_wiki_as = rpc_transfer_as(
        &rig,
        "local_fixture",
        json!({"op": "apply", "job_id": job_id, "root": ROOT, "entry_id": "fresh", "wiki_as": "operator"}),
    );
    let still = rpc_transfer(
        &rig,
        json!({"op": "status", "job_id": job_id, "root": ROOT}),
    );
    let agent_results = [
        &agent_status,
        &agent_apply,
        &agent_chunk,
        &agent_resume,
        &agent_binding,
        &agent_wiki_as,
    ];
    check(
        &mut failures,
        "CAD914 caller-seam actor cannot resume read or write the owned job",
        agent_results
            .iter()
            .all(|result| result.is_err() && hides(&err_text(result), &secrets))
            && still
                .as_ref()
                .is_ok_and(|value| status_receipt(value, &job_id, &digest, ROOT, "fresh.md"))
            && transfer_effects(&rig) == baseline,
        (
            &agent_status,
            &agent_apply,
            &agent_chunk,
            &agent_resume,
            &agent_binding,
            &agent_wiki_as,
        ),
    );

    let early_apply = rpc_transfer(
        &rig,
        json!({"op": "apply", "job_id": job_id, "root": ROOT, "entry_id": "fresh"}),
    );
    let still = rpc_transfer(
        &rig,
        json!({"op": "status", "job_id": job_id, "root": ROOT}),
    );
    check(
        &mut failures,
        "CAD914 apply before staged bytes does not publish or overwrite",
        early_apply.is_err()
            && still
                .as_ref()
                .is_ok_and(|value| status_receipt(value, &job_id, &digest, ROOT, "fresh.md"))
            && transfer_effects(&rig) == baseline,
        &early_apply,
    );

    let duplicate_id = "22222222-2222-4222-8222-222222222222";
    let duplicate_source = "cad914-duplicate-source";
    let duplicate = transfer_manifest(
        ROOT,
        duplicate_source,
        vec![
            manifest_entry("one", "dup.md", fresh),
            manifest_entry("two", "dup.md", fresh),
        ],
    );
    let duplicate_begin = rpc_transfer(
        &rig,
        json!({"op": "begin", "root": ROOT, "job_id": duplicate_id, "manifest": duplicate}),
    );
    let duplicate_status = rpc_transfer(
        &rig,
        json!({"op": "status", "job_id": duplicate_id, "root": ROOT}),
    );
    check(
        &mut failures,
        "CAD914 duplicate manifest is refused before any job or wiki write",
        duplicate_begin.as_ref().is_err()
            && duplicate_status.is_err()
            && hides(&err_text(&duplicate_status), &[duplicate_source, "dup.md"])
            && transfer_effects(&rig) == baseline,
        &duplicate_begin,
    );

    let traversal_id = "33333333-3333-4333-8333-333333333333";
    let traversal_source = "cad914-traversal-source";
    let traversal = transfer_manifest(
        ROOT,
        traversal_source,
        vec![manifest_entry("escape", "../outside/secret.md", fresh)],
    );
    let traversal_begin = rpc_transfer(
        &rig,
        json!({"op": "begin", "root": ROOT, "job_id": traversal_id, "manifest": traversal}),
    );
    let traversal_status = rpc_transfer(
        &rig,
        json!({"op": "status", "job_id": traversal_id, "root": ROOT}),
    );
    check(
        &mut failures,
        "CAD914 traversal manifest is refused before any job or wiki write",
        traversal_begin.as_ref().is_err()
            && traversal_status.is_err()
            && hides(&err_text(&traversal_status), &[traversal_source])
            && transfer_effects(&rig) == baseline,
        &traversal_begin,
    );

    let collision_id = "44444444-4444-4444-8444-444444444444";
    let collision_source = "cad914-collision-source";
    let collision = transfer_manifest(
        ROOT,
        collision_source,
        vec![
            manifest_entry("file", "notes.md", fresh),
            manifest_entry("child", "notes.md/child.txt", fresh),
        ],
    );
    let collision_begin = rpc_transfer(
        &rig,
        json!({"op": "begin", "root": ROOT, "job_id": collision_id, "manifest": collision}),
    );
    let collision_status = rpc_transfer(
        &rig,
        json!({"op": "status", "job_id": collision_id, "root": ROOT}),
    );
    check(
        &mut failures,
        "CAD914 file/child collision manifest is refused before any write",
        collision_begin.as_ref().is_err()
            && collision_status.is_err()
            && transfer_effects(&rig) == baseline,
        &collision_begin,
    );

    let http_source = "cad914-http-source";
    let http_manifest = transfer_manifest(
        ROOT,
        http_source,
        vec![manifest_entry("fresh", "http.md", fresh)],
    );
    let http_body = json!({"path": ROOT, "transfer": {"op": "begin", "manifest": http_manifest}});
    let http_begin = rig.call(
        "PUT",
        "/api/wiki/transfer",
        Some(&rig.sign(&rig.claims("cad914-http-begin"))),
        &http_body,
    );
    let http_job = http_begin.2["job_id"].as_str().unwrap_or("").to_string();
    let http_digest = http_begin.2["digest"].as_str().unwrap_or("").to_string();
    check(
        &mut failures,
        "CAD914 signed HTTP begin creates an owned unpublished job",
        http_begin.0 == 200
            && begin_receipt(&http_begin.2, ROOT, http_source)
            && http_digest != digest
            && transfer_effects(&rig) == baseline,
        &http_begin,
    );
    let client_proof = json!({
        "issuer": "https://untrusted.test",
        "organization_id": "ws_foreign",
        "audience": "https://other.test",
        "subject": "usr_other",
    });
    let proof_job = "77777777-7777-4777-8777-777777777777";
    for (name, body) in [
        (
            "wrapper binding",
            json!({
                "path": ROOT,
                "_transfer_binding": client_proof,
                "transfer": {"op": "begin", "job_id": proof_job, "manifest": http_manifest},
            }),
        ),
        (
            "transfer binding",
            json!({
                "path": ROOT,
                "transfer": {
                    "op": "begin",
                    "job_id": proof_job,
                    "_transfer_binding": client_proof,
                    "manifest": http_manifest,
                },
            }),
        ),
        (
            "public session",
            json!({
                "path": ROOT,
                "public_session": "client-supplied-proof",
                "transfer": {"op": "begin", "job_id": proof_job, "manifest": http_manifest},
            }),
        ),
    ] {
        let response = rig.call(
            "PUT",
            "/api/wiki/transfer",
            Some(&rig.sign(&rig.claims(&format!("cad914-client-proof-{name}")))),
            &body,
        );
        check(
            &mut failures,
            &format!("CAD914 signed transfer refuses a client {name} before any write"),
            response.0 == 400 && transfer_effects(&rig) == baseline,
            &response,
        );
    }
    let client_status_proof = rig.call(
        "POST",
        "/api/wiki/job_status",
        Some(&rig.sign(&rig.claims("cad914-client-status-proof"))),
        &json!({
            "path": ROOT,
            "_transfer_binding": client_proof,
            "transfer": {"op": "status", "job_id": http_job, "root": ROOT},
        }),
    );
    let proof_absent = rpc_transfer(
        &rig,
        json!({"op": "status", "job_id": proof_job, "root": ROOT}),
    );
    check(
        &mut failures,
        "CAD914 a client proof cannot bind a job or replace the verified binding",
        client_status_proof.0 == 400
            && proof_absent.is_err()
            && hides(
                &client_status_proof.2.to_string(),
                &[http_source, http_digest.as_str()],
            )
            && transfer_effects(&rig) == baseline,
        &client_status_proof,
    );
    let http_secrets = [
        http_source,
        http_digest.as_str(),
        "http.md",
        KEEP_TEXT,
        FRESH,
    ];
    let readonly = |id: &str| {
        let mut claims = rig.claims(id);
        claims["scope"] = json!(["wiki.read"]);
        claims["prefix_grants"] = json!([{"scope": "wiki.read", "prefixes": ["global/covered/"]}]);
        claims
    };
    let http_status_body = json!({
        "path": ROOT,
        "transfer": {"op": "status", "job_id": http_job, "root": ROOT},
    });
    let http_status = rig.call(
        "POST",
        "/api/wiki/job_status",
        Some(&rig.sign(&readonly("cad914-http-status"))),
        &http_status_body,
    );
    check(
        &mut failures,
        "CAD914 signed owner read status pages the HTTP job without writing",
        http_status.0 == 200
            && status_receipt(&http_status.2, &http_job, &http_digest, ROOT, "http.md")
            && transfer_effects(&rig) == baseline,
        &http_status,
    );
    let http_retry_body = json!({
        "path": ROOT,
        "transfer": {"op": "begin", "job_id": http_job, "manifest": http_manifest},
    });
    let http_retry = rig.call(
        "PUT",
        "/api/wiki/transfer",
        Some(&rig.sign(&rig.claims("cad914-http-retry"))),
        &http_retry_body,
    );
    check(
        &mut failures,
        "CAD914 signed retry of the same HTTP job keeps its digest",
        http_retry.0 == 200
            && http_retry.2["job_id"].as_str() == Some(http_job.as_str())
            && http_retry.2["digest"].as_str() == Some(http_digest.as_str())
            && transfer_effects(&rig) == baseline,
        &http_retry,
    );

    let local_reads_http = rpc_transfer(
        &rig,
        json!({"op": "status", "job_id": http_job, "root": ROOT}),
    );
    let http_reads_local = rig.call(
        "POST",
        "/api/wiki/job_status",
        Some(&rig.sign(&readonly("cad914-http-reads-local"))),
        &json!({"path": ROOT, "transfer": {"op": "status", "job_id": job_id, "root": ROOT}}),
    );
    check(
        &mut failures,
        "CAD914 signed authority and the local caller cannot read each other's jobs",
        local_reads_http.is_err()
            && hides(&err_text(&local_reads_http), &http_secrets)
            && (400..500).contains(&http_reads_local.0)
            && hides(&http_reads_local.2.to_string(), &secrets)
            && transfer_effects(&rig) == baseline,
        (&local_reads_http, &http_reads_local),
    );

    for (name, op) in [
        (
            "apply",
            json!({"op": "apply", "job_id": http_job, "root": ROOT, "entry_id": "fresh"}),
        ),
        (
            "chunk",
            json!({"op": "chunk", "job_id": http_job, "root": ROOT, "entry_id": "fresh", "offset": 0, "data_base64": STANDARD.encode(fresh)}),
        ),
        (
            "finish",
            json!({"op": "finish", "job_id": http_job, "root": ROOT}),
        ),
        (
            "begin",
            json!({"op": "begin", "job_id": http_job, "root": ROOT, "manifest": http_manifest}),
        ),
    ] {
        let response = rig.call(
            "POST",
            "/api/wiki/job_status",
            Some(&rig.sign(&readonly(&format!("cad914-smuggle-{name}")))),
            &json!({"path": ROOT, "transfer": op}),
        );
        check(
            &mut failures,
            &format!("CAD914 read-only job_status cannot smuggle {name}"),
            (400..500).contains(&response.0)
                && hides(&response.2.to_string(), &http_secrets)
                && transfer_effects(&rig) == baseline,
            &response,
        );
    }
    let after_smuggle = rig.call(
        "POST",
        "/api/wiki/job_status",
        Some(&rig.sign(&readonly("cad914-after-smuggle"))),
        &http_status_body,
    );
    check(
        &mut failures,
        "CAD914 smuggled writes leave the HTTP job unpublished",
        after_smuggle.0 == 200
            && status_receipt(&after_smuggle.2, &http_job, &http_digest, ROOT, "http.md"),
        &after_smuggle,
    );

    let read_only_put = rig.call(
        "PUT",
        "/api/wiki/transfer",
        Some(&rig.sign(&readonly("cad914-readonly-put"))),
        &json!({"path": ROOT, "transfer": {"op": "apply", "job_id": http_job, "root": ROOT, "entry_id": "fresh"}}),
    );
    check(
        &mut failures,
        "CAD914 read-only signed grant cannot PUT transfer",
        read_only_put.0 == 403
            && read_only_put.2["error"]["code"] == "insufficient_scope"
            && transfer_effects(&rig) == baseline,
        &read_only_put,
    );

    let mut agent = rig.claims("cad914-signed-agent");
    agent["sub"] = json!("hca_fixture");
    agent["actor"] = json!("agents/hca_fixture");
    agent["principal_kind"] = json!("agent");
    agent["role_at_issue"] = json!("owner");
    agent["credential_id"] = json!("hcc_fixture");
    let signed_agent = rig.call(
        "PUT",
        "/api/wiki/transfer",
        Some(&rig.sign(&agent)),
        &http_body,
    );
    check(
        &mut failures,
        "CAD914 signed agent actor cannot begin a transfer",
        signed_agent.0 == 403
            && signed_agent.2["error"]["code"] == "insufficient_scope"
            && transfer_effects(&rig) == baseline,
        &signed_agent,
    );
    let mut agent_operator = agent.clone();
    agent_operator["jti"] = json!("cad914-agent-as-operator");
    agent_operator["actor"] = json!("operator");
    let signed_agent_operator = rig.call(
        "PUT",
        "/api/wiki/transfer",
        Some(&rig.sign(&agent_operator)),
        &http_body,
    );
    check(
        &mut failures,
        "CAD914 signed agent cannot carry the operator actor",
        signed_agent_operator.0 == 401
            && signed_agent_operator.2["error"]["code"] == "bad_claims"
            && transfer_effects(&rig) == baseline,
        &signed_agent_operator,
    );

    let member_text = "cad914 delegated member page\n";
    let member_manifest = transfer_manifest(
        ROOT,
        "cad914-member-source",
        vec![manifest_entry(
            "member",
            "member.md",
            member_text.as_bytes(),
        )],
    );
    let jobs_at_member = transfer_job_ids(&rig);
    let mut member_read = rig.claims("cad914-member-readonly");
    member_read["actor"] = json!("users/member");
    member_read["role_at_issue"] = json!("member");
    member_read["scope"] = json!(["wiki.read"]);
    member_read["wiki_prefixes"] = json!(["global/covered/"]);
    member_read["prefix_grants"] = json!([{"scope": "wiki.read", "prefixes": ["global/covered/"]}]);
    let member_readonly = rig.call(
        "PUT",
        "/api/wiki/transfer",
        Some(&rig.sign(&member_read)),
        &json!({
            "path": ROOT,
            "transfer": {"op": "begin", "manifest": member_manifest},
        }),
    );
    check(
        &mut failures,
        "CAD914 read-only member grant cannot begin a transfer",
        member_readonly.0 == 403
            && member_readonly.2["error"]["code"] == "insufficient_scope"
            && member_readonly.2["job_id"].as_str().is_none()
            && transfer_job_ids(&rig) == jobs_at_member
            && transfer_effects(&rig) == baseline,
        &member_readonly,
    );
    let mut member_write = rig.claims("cad914-member-delegated");
    member_write["actor"] = json!("users/member");
    member_write["role_at_issue"] = json!("member");
    let member_begin = rig.call(
        "PUT",
        "/api/wiki/transfer",
        Some(&rig.sign(&member_write)),
        &json!({
            "path": ROOT,
            "transfer": {"op": "begin", "manifest": member_manifest},
        }),
    );
    let member_job = member_begin.2["job_id"].as_str().unwrap_or("").to_string();
    let member_digest = member_begin.2["digest"].as_str().unwrap_or("").to_string();
    let mut member_jobs = jobs_at_member.clone();
    member_jobs.push(member_job.clone());
    member_jobs.sort();
    check(
        &mut failures,
        "CAD914 delegated member begin creates a distinct unpublished job",
        member_begin.0 == 200
            && begin_receipt(&member_begin.2, ROOT, "cad914-member-source")
            && member_job != http_job
            && member_job != job_id
            && !member_digest.is_empty()
            && member_digest != http_digest
            && transfer_job_ids(&rig) == member_jobs
            && transfer_effects(&rig) == baseline,
        &member_begin,
    );
    member_write["jti"] = json!("cad914-member-status-operator");
    let member_status = rig.call(
        "POST",
        "/api/wiki/job_status",
        Some(&rig.sign(&member_write)),
        &http_status_body,
    );
    member_write["jti"] = json!("cad914-member-apply-operator");
    let member_apply = rig.call(
        "PUT",
        "/api/wiki/transfer",
        Some(&rig.sign(&member_write)),
        &json!({
            "path": ROOT,
            "transfer": {
                "op": "apply",
                "job_id": http_job,
                "root": ROOT,
                "entry_id": "fresh",
            },
        }),
    );
    let operator_still = rig.call(
        "POST",
        "/api/wiki/job_status",
        Some(&rig.sign(&readonly("cad914-member-operator-intact"))),
        &http_status_body,
    );
    check(
        &mut failures,
        "CAD914 delegated member cannot status or apply the operator job",
        member_status.0 == 400
            && member_status.2["job_id"].as_str().is_none()
            && member_apply.0 == 400
            && member_apply.2["outcome"].as_str() != Some("applied")
            && hides(&member_status.2.to_string(), &http_secrets)
            && hides(&member_apply.2.to_string(), &http_secrets)
            && operator_still.0 == 200
            && status_receipt(&operator_still.2, &http_job, &http_digest, ROOT, "http.md")
            && transfer_job_ids(&rig) == member_jobs
            && transfer_effects(&rig) == baseline,
        (&member_status, &member_apply),
    );

    for (name, path) in [
        ("sibling prefix", "global/covered-evil/cad914"),
        ("outside prefix", "global/outside/cad914"),
    ] {
        let ghost = if name.starts_with("sibling") {
            "88888888-8888-4888-8888-888888888888"
        } else {
            "99999999-9999-4999-8999-999999999999"
        };
        let body_manifest = transfer_manifest(
            path,
            "cad914-prefix-source",
            vec![manifest_entry("fresh", "fresh.md", fresh)],
        );
        let response = rig.call(
            "PUT",
            "/api/wiki/transfer",
            Some(&rig.sign(&rig.claims(&format!("cad914-prefix-{ghost}")))),
            &json!({"path": path, "transfer": {"op": "begin", "job_id": ghost, "manifest": body_manifest}}),
        );
        let ghost_status =
            rpc_transfer(&rig, json!({"op": "status", "job_id": ghost, "root": path}));
        check(
            &mut failures,
            &format!("CAD914 signed prefix refuses a job root {name}"),
            response.0 == 403
                && response.2["error"]["code"] == "insufficient_scope"
                && ghost_status.is_err()
                && transfer_effects(&rig) == baseline,
            &response,
        );
    }

    let entry_escape_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let entry_escape = transfer_manifest(
        ROOT,
        "cad914-entry-escape",
        vec![manifest_entry("escape", "../outside/secret.md", fresh)],
    );
    let entry_response = rig.call(
        "PUT",
        "/api/wiki/transfer",
        Some(&rig.sign(&rig.claims("cad914-entry-escape"))),
        &json!({"path": ROOT, "transfer": {"op": "begin", "job_id": entry_escape_id, "manifest": entry_escape}}),
    );
    let entry_status = rpc_transfer(
        &rig,
        json!({"op": "status", "job_id": entry_escape_id, "root": ROOT}),
    );
    check(
        &mut failures,
        "CAD914 signed transfer refuses an entry path outside the job root",
        (400..500).contains(&entry_response.0)
            && entry_status.is_err()
            && hides(&entry_response.2.to_string(), &["cad914-entry-escape"])
            && transfer_effects(&rig) == baseline,
        &entry_response,
    );

    let mut narrow = rig.claims("cad914-narrow-prefix");
    narrow["wiki_prefixes"] = json!(["global/covered/cad914-guard/nested/"]);
    narrow["prefix_grants"] = json!([
        {"scope": "wiki.read", "prefixes": ["global/covered/cad914-guard/nested/"]},
        {"scope": "wiki.write", "prefixes": ["global/covered/cad914-guard/nested/"]}
    ]);
    let narrow_response = rig.call(
        "PUT",
        "/api/wiki/transfer",
        Some(&rig.sign(&narrow)),
        &http_retry_body,
    );
    check(
        &mut failures,
        "CAD914 narrower signed prefix refuses the job root and its entry",
        narrow_response.0 == 403
            && narrow_response.2["error"]["code"] == "insufficient_scope"
            && transfer_effects(&rig) == baseline,
        &narrow_response,
    );
    let outside_follow = rig.call(
        "POST",
        "/api/wiki/job_status",
        Some(&rig.sign(&readonly("cad914-outside-follow"))),
        &json!({"path": "global/outside/cad914", "transfer": {"op": "status", "job_id": http_job, "root": "global/outside/cad914"}}),
    );
    check(
        &mut failures,
        "CAD914 signed status refuses a root outside the grant even with a real job id",
        outside_follow.0 == 403
            && outside_follow.2["error"]["code"] == "insufficient_scope"
            && hides(&outside_follow.2.to_string(), &http_secrets)
            && transfer_effects(&rig) == baseline,
        &outside_follow,
    );

    let still = rpc_transfer(
        &rig,
        json!({"op": "status", "job_id": job_id, "root": ROOT}),
    );
    check(
        &mut failures,
        "CAD914 owned job digest and keep page survive every refusal",
        still
            .as_ref()
            .is_ok_and(|value| status_receipt(value, &job_id, &digest, ROOT, "fresh.md"))
            && transfer_effects(&rig) == baseline,
        &still,
    );

    let page = format!("{ROOT}/http.md");
    let fresh_hash = sha256_hex(fresh);
    let head_before = rig.page_snapshot().0;
    let chunk = rig.call(
        "PUT",
        "/api/wiki/transfer",
        Some(&rig.sign(&rig.claims("cad914-http-chunk"))),
        &json!({
            "path": ROOT,
            "transfer": {
                "op": "chunk",
                "job_id": http_job,
                "root": ROOT,
                "entry_id": "fresh",
                "offset": 0,
                "data_base64": STANDARD.encode(fresh),
            },
        }),
    );
    let staged_only = rig.page_snapshot();
    check(
        &mut failures,
        "CAD914 signed HTTP chunk accepts the fresh bytes without publishing",
        chunk.0 == 200
            && chunk.2["job_id"].as_str() == Some(http_job.as_str())
            && chunk.2["entry_id"].as_str() == Some("fresh")
            && chunk.2["received"].as_u64() == Some(fresh.len() as u64)
            && chunk.2["size"].as_u64() == Some(fresh.len() as u64)
            && staged_only.0 == head_before,
        &chunk,
    );
    let apply = rig.call(
        "PUT",
        "/api/wiki/transfer",
        Some(&rig.sign(&rig.claims("cad914-http-apply"))),
        &json!({
            "path": ROOT,
            "transfer": {
                "op": "apply",
                "job_id": http_job,
                "root": ROOT,
                "entry_id": "fresh",
            },
        }),
    );
    let published = rig.page_snapshot();
    let apply_rev = apply.2["rev"].as_str().unwrap_or("").to_string();
    let holds = |files: &[(String, Vec<u8>)], suffix: &str, bytes: &[u8]| {
        files
            .iter()
            .any(|(path, body)| path.ends_with(suffix) && body.as_slice() == bytes)
    };
    check(
        &mut failures,
        "CAD914 signed HTTP apply publishes the fresh page once",
        apply.0 == 200
            && apply.2["job_id"].as_str() == Some(http_job.as_str())
            && apply.2["outcome"].as_str() == Some("applied")
            && apply.2["path"].as_str() == Some(page.as_str())
            && !apply_rev.is_empty()
            && published.0 != head_before
            && holds(&published.1, "/http.md", fresh)
            && holds(&published.1, "/keep.md", KEEP_TEXT.as_bytes()),
        &apply,
    );
    let reapply = rig.call(
        "PUT",
        "/api/wiki/transfer",
        Some(&rig.sign(&rig.claims("cad914-http-reapply"))),
        &json!({
            "path": ROOT,
            "transfer": {
                "op": "apply",
                "job_id": http_job,
                "root": ROOT,
                "entry_id": "fresh",
            },
        }),
    );
    let republished = rig.page_snapshot();
    check(
        &mut failures,
        "CAD914 signed HTTP reapply is idempotent and does not publish again",
        reapply.0 == 200
            && reapply.2["outcome"].as_str() == Some("applied")
            && reapply.2["rev"].as_str() == Some(apply_rev.as_str())
            && reapply.2["full_path"].as_str() == Some(page.as_str())
            && republished.0 == published.0
            && republished.1 == published.1,
        &reapply,
    );
    let finish = rig.call(
        "PUT",
        "/api/wiki/transfer",
        Some(&rig.sign(&rig.claims("cad914-http-finish"))),
        &json!({
            "path": ROOT,
            "transfer": {"op": "finish", "job_id": http_job, "root": ROOT},
        }),
    );
    check(
        &mut failures,
        "CAD914 signed HTTP finish completes the same job and digest",
        finish.0 == 200
            && finish.2["job_id"].as_str() == Some(http_job.as_str())
            && finish.2["digest"].as_str() == Some(http_digest.as_str())
            && finish.2["root"].as_str() == Some(ROOT)
            && finish.2["state"].as_str() == Some("complete"),
        &finish,
    );
    let done = rig.call(
        "POST",
        "/api/wiki/job_status",
        Some(&rig.sign(&readonly("cad914-http-complete"))),
        &http_status_body,
    );
    let done_entry = &done.2["entries"][0];
    check(
        &mut failures,
        "CAD914 signed HTTP job_status reads the completed applied entry",
        done.0 == 200
            && done.2["job_id"].as_str() == Some(http_job.as_str())
            && done.2["digest"].as_str() == Some(http_digest.as_str())
            && done.2["root"].as_str() == Some(ROOT)
            && done.2["state"].as_str() == Some("complete")
            && done_entry["outcome"].as_str() == Some("applied")
            && done_entry["path"].as_str() == Some("http.md")
            && done_entry["full_path"].as_str() == Some(page.as_str())
            && done_entry["sha256"].as_str() == Some(fresh_hash.as_str()),
        &done,
    );
    let export = rig.call(
        "POST",
        "/api/wiki/job_status",
        Some(&rig.sign(&readonly("cad914-http-export"))),
        &json!({
            "path": page,
            "transfer": {"op": "export", "path": page},
        }),
    );
    let decoded = export.2["data_base64"]
        .as_str()
        .and_then(|text| STANDARD.decode(text).ok());
    check(
        &mut failures,
        "CAD914 signed HTTP job_status export returns the exact fresh bytes",
        export.0 == 200
            && export.2["path"].as_str() == Some(page.as_str())
            && export.2["kind"].as_str() == Some("text")
            && export.2["sha256"].as_str() == Some(fresh_hash.as_str())
            && export.2["size"].as_u64() == Some(fresh.len() as u64)
            && decoded.as_deref() == Some(fresh),
        &export,
    );
    let owned_still = rpc_transfer(
        &rig,
        json!({"op": "status", "job_id": job_id, "root": ROOT}),
    );
    check(
        &mut failures,
        "CAD914 HTTP publication leaves the other owned job unpublished",
        owned_still
            .as_ref()
            .is_ok_and(|value| status_receipt(value, &job_id, &digest, ROOT, "fresh.md")),
        &owned_still,
    );

    let jobs_before = transfer_job_ids(&rig);
    let effects_before = transfer_effects(&rig);
    let numeric_job = rig.call(
        "PUT",
        "/api/wiki/transfer",
        Some(&rig.sign(&rig.claims("cad914-http-numeric-job"))),
        &json!({
            "path": ROOT,
            "transfer": {"op": "begin", "job_id": 123, "manifest": http_manifest},
        }),
    );
    check(
        &mut failures,
        "CAD914 signed HTTP begin refuses a numeric job id before any job or publication",
        numeric_job.0 == 400
            && numeric_job.2["job_id"].as_str().is_none()
            && transfer_job_ids(&rig) == jobs_before
            && transfer_effects(&rig) == effects_before
            && hides(
                &numeric_job.2.to_string(),
                &[http_source, http_digest.as_str(), FRESH, KEEP_TEXT],
            ),
        &numeric_job,
    );

    let member_page = format!("{ROOT}/member.md");
    member_write["jti"] = json!("cad914-member-chunk");
    let member_chunk = rig.call(
        "PUT",
        "/api/wiki/transfer",
        Some(&rig.sign(&member_write)),
        &json!({
            "path": ROOT,
            "transfer": {
                "op": "chunk",
                "job_id": member_job,
                "root": ROOT,
                "entry_id": "member",
                "offset": 0,
                "data_base64": STANDARD.encode(member_text.as_bytes()),
            },
        }),
    );
    check(
        &mut failures,
        "CAD914 delegated member chunk stores its own page",
        member_chunk.0 == 200
            && member_chunk.2["job_id"].as_str() == Some(member_job.as_str())
            && member_chunk.2["entry_id"].as_str() == Some("member")
            && member_chunk.2["received"].as_u64() == Some(member_text.len() as u64),
        &member_chunk,
    );
    member_write["jti"] = json!("cad914-member-apply");
    let member_publish = rig.call(
        "PUT",
        "/api/wiki/transfer",
        Some(&rig.sign(&member_write)),
        &json!({
            "path": ROOT,
            "transfer": {
                "op": "apply",
                "job_id": member_job,
                "root": ROOT,
                "entry_id": "member",
            },
        }),
    );
    let member_files = rig.page_snapshot();
    let holds = |files: &[(String, Vec<u8>)], suffix: &str, bytes: &[u8]| {
        files
            .iter()
            .any(|(path, body)| path.ends_with(suffix) && body.as_slice() == bytes)
    };
    check(
        &mut failures,
        "CAD914 delegated member apply publishes only member.md",
        member_publish.0 == 200
            && member_publish.2["job_id"].as_str() == Some(member_job.as_str())
            && member_publish.2["outcome"].as_str() == Some("applied")
            && member_publish.2["path"].as_str() == Some(member_page.as_str())
            && holds(&member_files.1, "/member.md", member_text.as_bytes())
            && holds(&member_files.1, "/http.md", fresh)
            && holds(&member_files.1, "/keep.md", KEEP_TEXT.as_bytes()),
        &member_publish,
    );
    member_write["jti"] = json!("cad914-member-finish");
    let member_finish = rig.call(
        "PUT",
        "/api/wiki/transfer",
        Some(&rig.sign(&member_write)),
        &json!({
            "path": ROOT,
            "transfer": {"op": "finish", "job_id": member_job, "root": ROOT},
        }),
    );
    check(
        &mut failures,
        "CAD914 delegated member finish completes its own job and digest",
        member_finish.0 == 200
            && member_finish.2["job_id"].as_str() == Some(member_job.as_str())
            && member_finish.2["digest"].as_str() == Some(member_digest.as_str())
            && member_finish.2["root"].as_str() == Some(ROOT)
            && member_finish.2["state"].as_str() == Some("complete"),
        &member_finish,
    );
    member_write["jti"] = json!("cad914-member-export");
    let member_export = rig.call(
        "POST",
        "/api/wiki/job_status",
        Some(&rig.sign(&member_write)),
        &json!({
            "path": member_page,
            "transfer": {"op": "export", "path": member_page},
        }),
    );
    let member_decoded = member_export.2["data_base64"]
        .as_str()
        .and_then(|text| STANDARD.decode(text).ok());
    let operator_after = rig.call(
        "POST",
        "/api/wiki/job_status",
        Some(&rig.sign(&readonly("cad914-operator-after-member"))),
        &http_status_body,
    );
    let owned_after = rpc_transfer(
        &rig,
        json!({"op": "status", "job_id": job_id, "root": ROOT}),
    );
    check(
        &mut failures,
        "CAD914 delegated member export leaves the operator job and keep page intact",
        member_export.0 == 200
            && member_export.2["path"].as_str() == Some(member_page.as_str())
            && member_export.2["sha256"].as_str()
                == Some(sha256_hex(member_text.as_bytes()).as_str())
            && member_decoded.as_deref() == Some(member_text.as_bytes())
            && operator_after.0 == 200
            && operator_after.2["job_id"].as_str() == Some(http_job.as_str())
            && operator_after.2["digest"].as_str() == Some(http_digest.as_str())
            && operator_after.2["state"].as_str() == Some("complete")
            && owned_after
                .as_ref()
                .is_ok_and(|value| status_receipt(value, &job_id, &digest, ROOT, "fresh.md"))
            && holds(&rig.page_snapshot().1, "/keep.md", KEEP_TEXT.as_bytes())
            && holds(&rig.page_snapshot().1, "/http.md", fresh),
        &member_export,
    );
    assert!(
        failures.is_empty(),
        "CAD914 transfer guard failures: {failures:?}"
    );
}
