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

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
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
    let mut member = rig.claims("native-member-acl");
    member["actor"] = json!("users/member");
    member["role_at_issue"] = json!("member");
    negatives.push((
        "signed grant cannot widen native member ACL".into(),
        member,
        put.clone(),
        None,
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
        forged
            .as_ref()
            .is_err_and(|e| e.to_string().contains("never acts as another"))
            && rig.page_snapshot() == before,
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
    assert!(
        failures.is_empty(),
        "independent boundary acceptance failures: {failures:?}"
    );
}
