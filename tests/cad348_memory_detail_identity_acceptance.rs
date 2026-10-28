//! CAD-348 memory-detail identity acceptance (draft PR #1397).
//!
//! Exercises the real served `GET /api/memories/<project>/<slug>` against an
//! isolated tracker. A caller whose board identity is absent, forged, expired,
//! stolen (a live session presented by an agent) or underivable is refused and
//! never handed the record. An operator and a named public member do get the
//! record, and the named member's Wiki citation resolves as that session's
//! **handle**, never its display actor — the identity the Wiki route itself
//! relays (`user:<handle>`).
//!
//! Each refusal is pinned to the guard that produced it, so an unrelated 4xx
//! cannot satisfy the assertion, and every refusal is also checked for the
//! record's body marker so a served record cannot pass as a refusal.
//!
//! `identity_unavailable` (503) is a bounded metadata read's answer; this route
//! calls `board_caller(.., budget = None)`, so an identity dependency that
//! cannot answer surfaces as the `caller_identity` refusal covered here.
#![cfg(feature = "test-seam")]

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cadence_agent::memory::policy::{self, EvidenceKind, EvidenceRef};
use cadence_agent::test_seam::{Asserted, Seam, AS_HEADER, TOKEN_HEADER};
use cadence_agent::{client, daemon, operator_auth};
use ring::signature::KeyPair;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

const LOCAL_PROJECT: &str = "cad348-memory-detail";
const LOCAL_SLUG: &str = "cad348-detail-fixture";
const PUBLIC_PROJECT: &str = "cad348-named-detail";
const PUBLIC_SLUG: &str = "cad348-named-fixture";
const MEMORY_BODY_MARKER: &str = "cad348-memory-detail-body-marker";

const PUBLIC_HOST: &str = "cad348.board.localhost";
const COMPANY: &str = "cad348-fixture-company";
const PUBLIC_KEY_SEED: [u8; 32] = [34; 32];
const PUBLIC_SUB: &str = "usr_cad348_member";
const PUBLIC_NAME: &str = "CAD-348 Member";

type Session = (String, String);

/// The fixture's HEAD commit, which contains the Wiki page the record cites.
fn head(pm_dir: &Path) -> String {
    let output = cadence_agent::reaper::output(
        std::process::Command::new("git")
            .arg("-C")
            .arg(pm_dir)
            .args(["rev-parse", "HEAD"]),
    )
    .unwrap();
    assert!(output.status.success(), "fixture git rev-parse failed");
    let sha = String::from_utf8(output.stdout).expect("git rev-parse HEAD is UTF-8");
    sha.trim().to_string()
}

/// Store one v2 record exactly as the tracker keeps it, with a Wiki citation
/// at an immutable commit. The record is fixture DATA: `memory::find` loads it
/// without re-validating policy evidence, and the served read must resolve the
/// citation through the reader's own ACL.
fn write_memory_record(
    pm_dir: &Path,
    key: &str,
    slug: &str,
    source: &str,
    revision: &str,
) -> PathBuf {
    let dir = pm_dir.join(key).join("memory");
    std::fs::create_dir_all(&dir).unwrap();
    let minimal = format!(
        r#"id: {slug}
type: decision
status: accepted
scope:
  project: true
confidence: high
created: '2026-10-28T00:00:00Z'
"#
    );
    let mut front: cadence_agent::memory::Front = serde_yaml::from_str(&minimal).unwrap();
    front.policy = Some(policy::PolicyV2 {
        revision: policy::REVISION,
        nature: policy::Nature::Judgement,
        provenance: policy::Provenance::Operator,
        audience: policy::Audience::Project,
        roles: Vec::new(),
        evidence_refs: vec![EvidenceRef {
            kind: EvidenceKind::Wiki,
            source: source.to_string(),
            revision: revision.to_string(),
        }],
        verification_condition: None,
        author: policy::Authorship {
            actor: policy::ActorSnapshot::Operator { uid: 1000 },
            model: None,
        },
        contributors: Vec::new(),
        attestations: Vec::new(),
        reviews: Vec::new(),
        finalizations: Vec::new(),
        feedback: Default::default(),
    });
    let yaml = serde_yaml::to_string(&front).unwrap();
    let body = format!(
        "{MEMORY_BODY_MARKER}\n\n\
         **Why:** the board read resolves citations as its own reader\n\n\
         **How to apply:** assert the reader's own access on GET\n"
    );
    let path = dir.join(format!("{slug}.md"));
    std::fs::write(&path, format!("---\n{yaml}---\n\n{body}")).unwrap();
    path
}

/// The refusal code a body names. The local admission answers
/// `{"code": …}`; the public host's platform failure nests it under
/// `error.code`. Both are the same guard answer, pinned by its marker.
fn marker_at<'a>(body: &'a Value, field: &str) -> Option<&'a str> {
    match field {
        "code" => body["code"]
            .as_str()
            .or_else(|| body["error"]["code"].as_str()),
        other => body[other].as_str(),
    }
}

/// One refusal must name the guard that produced it, and must not carry the
/// record: no `slug`, no body marker.
fn refused(
    label: &str,
    (status, reply): (u16, String),
    want_status: u16,
    field: &str,
    want_marker: &str,
) {
    assert_eq!(status, want_status, "{label}: {reply}");
    let body: Value = serde_json::from_str(&reply)
        .unwrap_or_else(|error| panic!("{label}: refusal is not JSON ({error}): {reply}"));
    assert_eq!(
        marker_at(&body, field),
        Some(want_marker),
        "{label} refused for the wrong reason: {reply}"
    );
    assert!(
        body["slug"].is_null(),
        "{label} answered a memory record: {reply}"
    );
    assert!(
        !reply.contains(MEMORY_BODY_MARKER),
        "{label} leaked the memory body: {reply}"
    );
}

/// The served detail's Wiki citation resolved through the reader's own access.
fn assert_citation_resolved(detail: &Value, path: &str, revision: &str) {
    let analysis = &detail["evidence_analysis"];
    let checked = analysis["checked"]
        .as_array()
        .unwrap_or_else(|| panic!("no evidence receipt: {analysis}"));
    assert!(
        checked.iter().any(|entry| entry
            .as_str()
            .is_some_and(|entry| entry.starts_with(&format!("wiki:{path}@{revision} ")))),
        "the reader's own Wiki citation did not resolve: {analysis}"
    );
    let skipped = analysis["skipped"].as_array().cloned().unwrap_or_default();
    assert!(
        !skipped.iter().any(|entry| entry
            .as_str()
            .is_some_and(|entry| entry.contains("not readable by this reader"))),
        "the citation was refused to this reader: {analysis}"
    );
}

/// Serve one fixture board on a free port in 3110-3199. The probe listener
/// is dropped before `ui::serve` binds, so a concurrent fixture can take the
/// port in between; a bind failure reports `AddrInUse` on the startup channel,
/// and the next free port is tried instead of failing the check.
fn start_board(
    state: PathBuf,
    pm: PathBuf,
    opts: cadence_agent::ui::ServeOpts,
) -> (u16, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let mut candidates = 3110..=3199;
    loop {
        let port = candidates
            .find(|port| std::net::TcpListener::bind(("127.0.0.1", *port)).is_ok())
            .expect("free board port in 3110-3199");
        let (tx, rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let mut attempt = opts.clone();
        attempt.port = port;
        attempt.startup = Some(tx);
        attempt.stop = Some(Arc::clone(&stop));
        let (state, pm) = (state.clone(), pm.clone());
        let board = std::thread::spawn(move || {
            let _ = cadence_agent::ui::serve(&state, &pm, &attempt);
        });
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => return (port, stop, board),
            Ok(Err(std::io::ErrorKind::AddrInUse)) => {
                let _ = board.join();
            }
            Ok(Err(kind)) => panic!("fixture board could not bind a port: {kind:?}"),
            Err(error) => panic!("fixture board did not report startup: {error}"),
        }
    }
}

struct LocalFixture {
    root: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    clock: Arc<AtomicI64>,
    daemon: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
    board: Option<(Arc<AtomicBool>, std::thread::JoinHandle<()>)>,
    port: u16,
    revision: String,
}

impl LocalFixture {
    fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("cad348memory")
            .tempdir()
            .unwrap();
        let pm = root.path().join("pm");
        let pm = cadence_agent::issue::Pm::init(&pm).unwrap();
        let state = root.path().join("state");
        std::fs::create_dir_all(&state).unwrap();

        // The served tracker follows the isolated legacy HOME/wiki layout.
        let wiki = pm.dir.join("wiki/global");
        std::fs::create_dir_all(&wiki).unwrap();
        let page = wiki.join("acceptance.md");
        std::fs::write(&page, "# CAD-348 Memory Detail Wiki Fixture\n").unwrap();
        pm.commit(std::slice::from_ref(&page), "CAD-348 wiki fixture\n")
            .unwrap();
        let revision = head(&pm.dir);

        cadence_agent::issue::write::project_add(&pm, LOCAL_PROJECT, "CDM", &[], &[], &[], None)
            .expect("isolated project for the memory-detail check");
        let record = write_memory_record(
            &pm.dir,
            LOCAL_PROJECT,
            LOCAL_SLUG,
            "global/acceptance.md",
            &revision,
        );
        pm.commit(
            std::slice::from_ref(&record),
            "CAD-348 memory record fixture\n",
        )
        .unwrap();

        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let clock = Arc::new(AtomicI64::new(1_800_000_000));
        let opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(Arc::clone(&stop)),
            test_seam: true,
            operator_clock: Some({
                let clock = Arc::clone(&clock);
                Arc::new(move || clock.load(SeqCst))
            }),
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        let daemon_state = state.clone();
        let daemon = std::thread::spawn(move || daemon::serve_with(&daemon_state, opts));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&state).is_none()
        {
            assert!(
                !daemon.is_finished() && std::time::Instant::now() < deadline,
                "daemon down"
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        let board_opts = cadence_agent::ui::ServeOpts {
            host: "127.0.0.1".into(),
            test_seam: true,
            ..Default::default()
        };
        let (port, board_stop, board) = start_board(state, pm.dir.clone(), board_opts);
        Self {
            root,
            stop,
            clock,
            daemon: Some(daemon),
            board: Some((board_stop, board)),
            port,
            revision,
        }
    }

    fn state(&self) -> PathBuf {
        self.root.path().join("state")
    }

    fn path(&self) -> String {
        format!("/api/memories/{LOCAL_PROJECT}/{LOCAL_SLUG}")
    }

    fn request(&self, path: &str, session: Option<&Session>, asserted: Asserted) -> (u16, String) {
        let host = format!("cadence-{}.localhost:{}", self.port, self.port);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let mut request = agent
            .get(format!("http://127.0.0.1:{}{path}", self.port))
            .header("Host", &host)
            .header("Origin", format!("http://{host}"))
            .header("X-Cadence-Board", "1")
            .header(AS_HEADER, asserted.as_str())
            .header(TOKEN_HEADER, Seam::token_at(&self.state()).unwrap());
        if let Some((cookie, key)) = session {
            request = request
                .header("Cookie", cookie)
                .header("X-Cadence-Session", key);
        }
        // Deliberately no Authorization: this is the browser's
        // cookie-authenticated GET form.
        let mut response = request.call().unwrap();
        let status = response.status().as_u16();
        (status, response.body_mut().read_to_string().unwrap())
    }

    fn operator_session(&self) -> Session {
        operator_auth::ensure_secret(&self.state()).unwrap();
        let secret = operator_auth::read_secret(&self.state()).unwrap();
        let nonce = cadence_agent::test_seam::scoped(Asserted::Operator, || {
            client::rpc(
                &self.state(),
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )
        })
        .unwrap()["nonce"]
            .as_str()
            .unwrap()
            .to_owned();
        let body = json!({"nonce": nonce}).to_string();
        let host = format!("cadence-{}.localhost:{}", self.port, self.port);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let mut response = agent
            .post(format!("http://127.0.0.1:{}/api/session", self.port))
            .header("Host", &host)
            .header("Origin", format!("http://{host}"))
            .header("X-Cadence-Board", "1")
            .header("Content-Type", "application/json")
            .header(AS_HEADER, "operator")
            .header(TOKEN_HEADER, Seam::token_at(&self.state()).unwrap())
            .send(body)
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let key = response.body_mut().read_to_string().unwrap();
        let key = serde_json::from_str::<Value>(&key).unwrap()["session_key"]
            .as_str()
            .unwrap()
            .to_owned();
        (cookie, key)
    }
}

impl Drop for LocalFixture {
    fn drop(&mut self) {
        if let Some((stop, thread)) = self.board.take() {
            stop.store(true, SeqCst);
            let _ = thread.join();
        }
        self.stop.store(true, SeqCst);
        if let Some(thread) = self.daemon.take() {
            let _ = thread.join();
        }
    }
}

#[test]
fn unproven_or_stolen_board_identity_never_reads_a_memory_detail() {
    let fx = LocalFixture::start();
    let path = fx.path();

    // The fixture is readable at all: the marker assertion below is not
    // vacuous, and the operator's own Wiki access resolves the citation.
    let session = fx.operator_session();
    let (status, detail) = fx.request(&path, Some(&session), Asserted::Operator);
    assert_eq!(status, 200, "operator detail read: {detail}");
    let detail: Value = serde_json::from_str(&detail).unwrap();
    assert_eq!(detail["slug"].as_str(), Some(LOCAL_SLUG));
    assert!(
        detail["body"]
            .as_str()
            .is_some_and(|body| body.contains(MEMORY_BODY_MARKER)),
        "operator detail omitted the record body: {detail}"
    );
    assert_citation_resolved(&detail, "global/acceptance.md", &fx.revision);

    let other = fx.operator_session();
    for (label, presented, want_status, field, marker) in [
        (
            "absent session",
            None,
            401,
            "code",
            "operator_session_required",
        ),
        (
            "forged cookie",
            Some((
                format!("cadence_operator_{}=forged", fx.port),
                session.1.clone(),
            )),
            401,
            "code",
            "operator_session_required",
        ),
        // A real cookie paired with another live session's key.
        (
            "mismatched cookie and key",
            Some((session.0.clone(), other.1.clone())),
            401,
            "code",
            "operator_session_required",
        ),
    ] {
        let reply = fx.request(&path, presented.as_ref(), Asserted::Operator);
        refused(label, reply, want_status, field, marker);
    }

    // A live operator session presented by an agent: the guard refuses and
    // revokes it. The old read fell back to anonymous here and served the
    // record.
    let reply = fx.request(
        &path,
        Some(&other),
        Asserted::Agent("cad348-fixture".to_string()),
    );
    refused("stolen session", reply, 403, "check", "session_from_agent");

    // A live session the board cannot attribute: no identity, no read.
    let reply = fx.request(&path, Some(&session), Asserted::Unproven);
    refused(
        "underivable identity",
        reply,
        403,
        "check",
        "caller_identity",
    );

    // Advance the daemon's deterministic auth clock beyond this session's
    // absolute lifetime; this exercises expiry, not just bad syntax.
    fx.clock
        .store(1_800_000_000 + operator_auth::ABSOLUTE_SECS + 1, SeqCst);
    let reply = fx.request(&path, Some(&session), Asserted::Operator);
    refused(
        "expired session",
        reply,
        401,
        "code",
        "operator_session_required",
    );
}

struct PublicFixture {
    root: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    board_stop: Arc<AtomicBool>,
    clock: Arc<AtomicI64>,
    daemon: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
    board: Option<std::thread::JoinHandle<()>>,
    jwks_stop: Arc<AtomicBool>,
    jwks: Option<std::thread::JoinHandle<()>>,
    port: u16,
    issuer: String,
    agent: ureq::Agent,
    revision: String,
}

impl PublicFixture {
    fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("cad348public")
            .tempdir()
            .unwrap();
        let pm = root.path().join("pm");
        let pm = cadence_agent::issue::Pm::init(&pm).unwrap();
        let state = root.path().join("state");
        std::fs::create_dir_all(&state).unwrap();

        // The member's private workspace is both read and citable.
        let wiki = pm.dir.join(format!("wiki/users/{PUBLIC_SUB}"));
        std::fs::create_dir_all(&wiki).unwrap();
        let page = wiki.join("acceptance.md");
        std::fs::write(
            &page,
            "# CAD-348 Named Memory Detail Fixture\n\ncad348-named-wiki-marker\n",
        )
        .unwrap();
        pm.commit(std::slice::from_ref(&page), "CAD-348 named wiki fixture\n")
            .unwrap();
        let revision = head(&pm.dir);

        cadence_agent::issue::write::project_add(&pm, PUBLIC_PROJECT, "CND", &[], &[], &[], None)
            .expect("isolated project for the named memory-detail check");
        let record = write_memory_record(
            &pm.dir,
            PUBLIC_PROJECT,
            PUBLIC_SLUG,
            &format!("users/{PUBLIC_SUB}/acceptance.md"),
            &revision,
        );
        pm.commit(
            std::slice::from_ref(&record),
            "CAD-348 named memory record fixture\n",
        )
        .unwrap();

        let signer =
            ring::signature::Ed25519KeyPair::from_seed_unchecked(&PUBLIC_KEY_SEED).unwrap();
        let jwks_body = json!({"keys": [{
            "kty": "OKP", "crv": "Ed25519", "kid": "cad348",
            "x": URL_SAFE_NO_PAD.encode(signer.public_key().as_ref())
        }]})
        .to_string();
        let (jwks_server, issuer) = (3110..=3199)
            .find_map(|port| {
                tiny_http::Server::http(("127.0.0.1", port))
                    .ok()
                    .map(|server| {
                        let issuer = format!("http://{}", server.server_addr());
                        (server, issuer)
                    })
            })
            .expect("free local fixture port for platform JWKS");
        let jwks_stop = Arc::new(AtomicBool::new(false));
        let jwks_flag = Arc::clone(&jwks_stop);
        let jwks = std::thread::spawn(move || {
            while !jwks_flag.load(SeqCst) {
                let Ok(Some(request)) = jwks_server.recv_timeout(Duration::from_millis(100)) else {
                    continue;
                };
                let response = tiny_http::Response::from_string(jwks_body.clone()).with_header(
                    tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
                );
                let _ = request.respond(response);
            }
        });

        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let now = cadence_agent::issue::time::now_epoch();
        let clock = Arc::new(AtomicI64::new(now));
        let daemon_opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(Arc::clone(&stop)),
            test_seam: true,
            operator_clock: Some({
                let clock = Arc::clone(&clock);
                Arc::new(move || clock.load(SeqCst))
            }),
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        let daemon_state = state.clone();
        let daemon = std::thread::spawn(move || daemon::serve_with(&daemon_state, daemon_opts));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&state).is_none()
        {
            assert!(
                !daemon.is_finished() && std::time::Instant::now() < deadline,
                "public fixture daemon down"
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        let host = PUBLIC_HOST.to_string();
        let board_opts = cadence_agent::ui::ServeOpts {
            host: "127.0.0.1".into(),
            test_seam: true,
            public: Some(cadence_agent::ui::PublicBoard {
                host: host.clone(),
                issuer: issuer.clone(),
                company: COMPANY.into(),
                authorize_url: format!("{issuer}/authorize"),
            }),
            allow_hosts: vec![host.clone()],
            allow_origins: vec![format!("http://{host}")],
            ..Default::default()
        };
        let (port, board_stop, board) = start_board(state, pm.dir.clone(), board_opts);

        Self {
            root,
            stop,
            board_stop,
            clock,
            daemon: Some(daemon),
            board: Some(board),
            jwks_stop,
            jwks: Some(jwks),
            port,
            issuer,
            agent: ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .into(),
            revision,
        }
    }

    fn state(&self) -> PathBuf {
        self.root.path().join("state")
    }

    fn path(&self) -> String {
        format!("/api/memories/{PUBLIC_PROJECT}/{PUBLIC_SLUG}")
    }

    fn assertion(&self, jti: &str) -> String {
        let key = ring::signature::Ed25519KeyPair::from_seed_unchecked(&PUBLIC_KEY_SEED).unwrap();
        let now = self.clock.load(SeqCst);
        let part = |value: Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).unwrap());
        let signed = format!(
            "{}.{}",
            part(json!({"alg": "EdDSA", "typ": "JWT", "kid": "cad348"})),
            part(json!({
                "iss": self.issuer, "aud": PUBLIC_HOST, "sub": PUBLIC_SUB,
                "email": "member@example.invalid", "name": PUBLIC_NAME,
                "company": COMPANY, "role": "member", "iat": now, "exp": now + 60,
                "jti": jti
            }))
        );
        format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(key.sign(signed.as_bytes()).as_ref())
        )
    }

    fn open_public_session(&self, jti: &str) -> String {
        let response = self
            .agent
            .post(format!("http://127.0.0.1:{}/__platform/session", self.port))
            .header("Host", PUBLIC_HOST)
            .header("Origin", format!("http://{PUBLIC_HOST}"))
            .header("Sec-Fetch-Site", "same-origin")
            .header("Content-Type", "application/json")
            .send(json!({"assertion": self.assertion(jti)}).to_string())
            .unwrap();
        assert_eq!(response.status().as_u16(), 200, "public session exchange");
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    }

    fn get(&self, path: &str, cookie: Option<&str>, asserted: Option<&Asserted>) -> (u16, String) {
        let mut request = self
            .agent
            .get(format!("http://127.0.0.1:{}{path}", self.port))
            .header("Host", PUBLIC_HOST);
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        if let Some(asserted) = asserted {
            request = request
                .header(AS_HEADER, asserted.as_str())
                .header(TOKEN_HEADER, Seam::token_at(&self.state()).unwrap());
        }
        let mut response = request.call().unwrap();
        let status = response.status().as_u16();
        (status, response.body_mut().read_to_string().unwrap())
    }
}

impl Drop for PublicFixture {
    fn drop(&mut self) {
        self.board_stop.store(true, SeqCst);
        if let Some(thread) = self.board.take() {
            let _ = thread.join();
        }
        self.stop.store(true, SeqCst);
        if let Some(thread) = self.daemon.take() {
            let _ = thread.join();
        }
        self.jwks_stop.store(true, SeqCst);
        if let Some(thread) = self.jwks.take() {
            let _ = thread.join();
        }
        let _ = self.root.path();
    }
}

#[test]
fn named_public_session_reads_as_its_handle_and_unproven_identity_is_refused() {
    let fx = PublicFixture::start();
    let path = fx.path();
    let cookie = fx.open_public_session("cad348-named-session");

    // The named member's own session reads the record, and the private Wiki
    // citation resolves — which it can only do under the session's handle,
    // `usr_cad348_member`. The display actor (`CAD-348 Member <…> (board)`)
    // would be refused `users/<u>/ is private to its member`.
    let (status, detail) = fx.get(&path, Some(&cookie), None);
    assert_eq!(status, 200, "named detail read: {detail}");
    let detail: Value = serde_json::from_str(&detail).unwrap();
    assert_eq!(detail["slug"].as_str(), Some(PUBLIC_SLUG));
    assert!(
        detail["body"]
            .as_str()
            .is_some_and(|body| body.contains(MEMORY_BODY_MARKER)),
        "named detail omitted the record body: {detail}"
    );
    assert_citation_resolved(
        &detail,
        &format!("users/{PUBLIC_SUB}/acceptance.md"),
        &fx.revision,
    );
    let analysis = &detail["evidence_analysis"];
    assert!(
        !analysis["skipped"]
            .as_array()
            .is_some_and(|skipped| skipped.iter().any(|entry| entry
                .as_str()
                .is_some_and(|entry| entry.contains("private to its member")))),
        "the display actor, not the handle, was used as the wiki identity: {analysis}"
    );

    // No session, a forged one, or a live one presented by an agent: the
    // record is never served.
    for (label, presented, want_status, field, marker) in [
        (
            "absent public session",
            None,
            401,
            "code",
            "board_session_required",
        ),
        (
            "forged public session",
            Some("__Host-aos-board-session=forged"),
            401,
            "code",
            "board_session_required",
        ),
    ] {
        let reply = fx.get(&path, presented, None);
        refused(label, reply, want_status, field, marker);
    }

    // One session per `sub` (contract §9): a fresh public sign-in ends this
    // member's earlier one, so the stolen case signs in afresh and the
    // underivable case below signs in afresh again. Each refusal therefore
    // presents a genuinely live session, never a cookie an earlier step ended.
    let stolen = fx.open_public_session("cad348-stolen-session");
    let reply = fx.get(
        &path,
        Some(&stolen),
        Some(&Asserted::Agent("cad348-fixture".to_string())),
    );
    refused(
        "stolen public session",
        reply,
        403,
        "check",
        "session_from_agent",
    );

    let underivable = fx.open_public_session("cad348-underivable-session");
    let reply = fx.get(&path, Some(&underivable), Some(&Asserted::Unproven));
    refused(
        "underivable public identity",
        reply,
        403,
        "check",
        "caller_identity",
    );
}
