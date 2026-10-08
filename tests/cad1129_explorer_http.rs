//! CAD-1129 / CAD-1194 HTTP acceptance checks (reviewer-written from the
//! tickets, cc13-sonnet-spec794): the board peers of the explorer RPCs are at
//! least as strict as the RPC they relay. A real in-process daemon and board
//! (CAD-482 test seam); each refusal is compared against a whole-tracker byte
//! snapshot, and every case has a positive control.
//!
//! Note on the seam: the test seam forwards the HTTP caller's asserted
//! identity into the relayed RPC, so here an agent is refused by BOTH the
//! board route class and the daemon. In production the board relays over its
//! own operator connection and carries no agent identity, so the route class
//! is the only board-side guard; it is pinned on its own by
//! `ui::operator::tests::explorer_routes_are_operator_only_except_the_member_verbs`.
#![cfg(all(feature = "test-seam", target_os = "linux"))]

use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
use cadence_agent::{client, daemon, issue};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::{Duration, Instant};

const WORKFLOW: &str = "---\ntitle: \"Post: {{topic}}\"\ngoal: \"Publish {{topic}}\"\ninputs:\n  topic: { ask: \"About what?\" }\n---\n\nWhy.\n\n## Research {{topic}}\nagent: dev-1\nsize: S\n\nDo it.\n\n### Acceptance\n- [ ] brief written\n";

fn manifest(app: &str, version: &str) -> String {
    format!("---\napp: {app}\ntitle: Pin\nversion: '{version}'\nneeds:\n  connections: []\n---\n\nGuide.\n")
}

struct Fx {
    root: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
    port: u16,
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        for t in self.threads.drain(..).rev() {
            let _ = t.join();
        }
    }
}

impl Fx {
    fn start() -> Self {
        let root = tempfile::Builder::new().prefix("c1186").tempdir().unwrap();
        let mut fx = Self {
            root,
            stop: Arc::new(AtomicBool::new(false)),
            threads: vec![],
            port: 0,
        };
        issue::Pm::init(&fx.pm()).unwrap();
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", fx.pm().to_str().unwrap());
        let opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(fx.stop.clone()),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        let state = fx.state();
        fx.threads.push(std::thread::spawn(move || {
            daemon::serve_with(&state, opts).unwrap()
        }));
        let deadline = Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&fx.state(), "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&fx.state()).is_none()
        {
            assert!(Instant::now() < deadline, "daemon never started");
            std::thread::sleep(Duration::from_millis(50));
        }
        for port in 3110..3200 {
            let (tx, rx) = std::sync::mpsc::channel();
            let opts = cadence_agent::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                stop: Some(fx.stop.clone()),
                startup: Some(tx),
                test_seam: true,
                ..Default::default()
            };
            let (state, pm) = (fx.state(), fx.pm());
            let thread = std::thread::spawn(move || {
                let _ = cadence_agent::ui::serve(&state, &pm, &opts);
            });
            match rx.recv_timeout(Duration::from_secs(20)).unwrap() {
                Ok(()) => {
                    fx.port = port;
                    fx.threads.push(thread);
                    break;
                }
                Err(_) => thread.join().unwrap(),
            }
        }
        assert_ne!(fx.port, 0, "no board port in 3110..3200");
        fx
    }
    fn state(&self) -> PathBuf {
        self.root.path().join("s")
    }
    fn pm(&self) -> PathBuf {
        self.root.path().join("pm")
    }
    /// A bundle directory outside the tracker.
    fn bundle(&self, name: &str, app: &str, version: &str) -> PathBuf {
        let dir = self.root.path().join("src").join(name);
        std::fs::create_dir_all(dir.join("workflows")).unwrap();
        std::fs::write(dir.join("app.md"), manifest(app, version)).unwrap();
        std::fs::write(dir.join("workflows/do.md"), WORKFLOW).unwrap();
        dir
    }
    fn rpc(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let state = self.state();
        scoped(who, || client::rpc(&state, method, params))
    }
    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("{method}: {e}"))
    }
    /// Every file under the tracker (outside .git) with its bytes, plus HEAD.
    fn tree(&self) -> BTreeMap<String, Vec<u8>> {
        fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                if rel == ".git" {
                    continue;
                }
                if p.is_dir() {
                    out.insert(format!("{rel}/"), vec![]);
                    walk(base, &p, out);
                } else {
                    out.insert(rel, std::fs::read(&p).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&self.pm(), &self.pm(), &mut out);
        let head = cadence_agent::reaper::output(
            std::process::Command::new("git")
                .arg("-C")
                .arg(self.pm())
                .args(["rev-parse", "HEAD"]),
        )
        .unwrap();
        out.insert("<git HEAD>".into(), head.stdout);
        out
    }
    fn session(&self) -> (String, String) {
        cadence_agent::operator_auth::ensure_secret(&self.state()).unwrap();
        let secret = cadence_agent::operator_auth::read_secret(&self.state()).unwrap();
        let nonce = self.op(
            "operator_link_mint",
            json!({"secret": secret, "origin": "loopback"}),
        )["nonce"]
            .clone();
        let (status, text, cookie) = self.http_with(
            "operator",
            "/api/session",
            &json!({"nonce": nonce}).to_string(),
            None,
        );
        assert_eq!(status, 200, "operator session exchange: {text}");
        let body: Value = serde_json::from_str(&text).unwrap();
        (
            cookie.unwrap(),
            body["session_key"].as_str().unwrap().to_string(),
        )
    }
    fn http(&self, who: &str, route: &str, body: &str) -> (u16, String) {
        let s = if who == "operator" {
            Some(self.session())
        } else {
            None
        };
        let (a, b, _) = self.http_with(who, route, body, s.as_ref());
        (a, b)
    }
    fn get(&self, who: &str, route: &str) -> (u16, String) {
        let host = format!("cadence-{}.localhost:{}", self.port, self.port);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let token = Seam::token_at(&self.state()).unwrap();
        let mut req = agent
            .get(format!("http://127.0.0.1:{}{route}", self.port))
            .header("Host", &host)
            .header("X-Cadence-Board", "1")
            .header(AS_HEADER, who)
            .header(TOKEN_HEADER, token);
        if who == "operator" {
            let (cookie, key) = self.session();
            req = req
                .header("Cookie", cookie)
                .header("X-Cadence-Session", key);
        }
        let mut r = req.call().unwrap();
        (r.status().as_u16(), r.body_mut().read_to_string().unwrap())
    }
    fn http_with(
        &self,
        who: &str,
        route: &str,
        body: &str,
        session: Option<&(String, String)>,
    ) -> (u16, String, Option<String>) {
        let host = format!("cadence-{}.localhost:{}", self.port, self.port);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let token = Seam::token_at(&self.state()).unwrap();
        let mut req = agent
            .post(format!("http://127.0.0.1:{}{route}", self.port))
            .header("Host", &host)
            .header("Origin", format!("http://{host}"))
            .header("X-Cadence-Board", "1")
            .header("Content-Type", "application/json")
            .header(AS_HEADER, who)
            .header(TOKEN_HEADER, token);
        if let Some((cookie, key)) = session {
            req = req
                .header("Cookie", cookie)
                .header("X-Cadence-Session", key);
        }
        let mut r = req.send(body.to_string()).unwrap();
        let cookie = r
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap().to_string());
        (
            r.status().as_u16(),
            r.body_mut().read_to_string().unwrap(),
            cookie,
        )
    }
}

fn catalog_exists(fx: &Fx) -> bool {
    fx.pm().join(".apps/catalog.yaml").exists() || fx.tree().keys().any(|k| k.contains("catalog"))
}

const STALE: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

/// `POST /api/app-installations/check`: operator-only, body exactly
/// `{source}`, read-only. An agent, an unauthenticated caller, a forged,
/// unknown, duplicated or mistyped field is refused; the tracker never moves
/// and no catalog is created.
#[test]
fn check_route_refuses_everything_but_a_clean_operator_source() {
    let fx = Fx::start();
    let src = fx.bundle("a", "chk-a", "1");
    let route = "/api/app-installations/check";
    let before = fx.tree();
    let good = json!({"source": src}).to_string();

    // agent and unauthenticated callers, even with a right body.
    let (status, text) = fx.http("agent:writer", route, &good);
    assert_eq!(status, 403, "agent: {text}");
    let (status, text) = fx.http("unproven", route, &good);
    assert!(
        status == 401 || status == 403,
        "unauthenticated caller got {status}: {text}"
    );
    assert!(!text.contains("digest"), "refusal leaked a digest: {text}");

    // operator session, bad bodies.
    let dup = format!("{{\"source\":{},\"source\":{}}}", json!(src), json!(src));
    let bodies = [
        json!({"source": src, "approved": true}).to_string(),
        json!({"source": src, "actor": "operator"}).to_string(),
        json!({"source": src, "expected_digest": STALE}).to_string(),
        json!({"source": src, "member_as": "alice"}).to_string(),
        json!({"Source": src}).to_string(),
        json!({"source": src, "source2": 1}).to_string(),
        json!({}).to_string(),
        json!({"source": ""}).to_string(),
        json!({"source": 5}).to_string(),
        json!({"source": null}).to_string(),
        json!([src, "extra"]).to_string(),
        json!(src).to_string(),
        "not json".to_string(),
        String::new(),
        dup,
    ];
    for body in &bodies {
        let (status, text) = fx.http("operator", route, body);
        assert_eq!(status, 400, "accepted {body}: {status} {text}");
        assert!(
            !text.contains("operator_session_required"),
            "vacuous refusal (no session): {text}"
        );
    }
    // a bad source reaches the RPC and is refused there, writing nothing.
    for source in [
        "relative/path",
        "/nonexistent/dir",
        "ssh://git@host/x",
        "https://u:p@h/x",
    ] {
        let (status, text) = fx.http("operator", route, &json!({"source": source}).to_string());
        assert!(status >= 400, "{source}: {status} {text}");
    }
    // only POST.
    let (status, _) = fx.get("operator", route);
    assert_ne!(status, 200);
    assert_eq!(fx.tree(), before, "a refused check changed the tracker");
    assert!(!catalog_exists(&fx), "a refused check created the catalog");

    // positive controls: the clean operator body answers the install digest
    // and still writes nothing; a built-in answers too.
    let (status, text) = fx.http("operator", route, &good);
    assert_eq!(status, 200, "{text}");
    let body: Value = serde_json::from_str(&text).unwrap();
    let rpc = fx.op("app_workspace_install_check", json!({"source": src}));
    assert_eq!(body["digest"], rpc["digest"]);
    let (status, text) = fx.http(
        "operator",
        route,
        &json!({"source": "builtin:crm"}).to_string(),
    );
    assert_eq!(status, 200, "{text}");
    assert!(serde_json::from_str::<Value>(&text).unwrap()["digest"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(fx.tree(), before, "the check wrote");
    assert!(!catalog_exists(&fx), "the check created the catalog");
}

/// `builtin:` is a check-only source: the generic install RPC and route do
/// not resolve it, and it never smuggles a path.
#[test]
fn builtin_source_form_is_check_only() {
    let fx = Fx::start();
    let before = fx.tree();
    for source in [
        "builtin:crm",
        "builtin:",
        "builtin:../../etc",
        "builtin:nope",
    ] {
        let r = fx.rpc(
            Asserted::Operator,
            "app_workspace_install",
            json!({"source": source}),
        );
        assert!(r.is_err(), "install accepted {source}: {r:?}");
    }
    for source in [
        "builtin:",
        "builtin:../../etc",
        "builtin:nope",
        "builtin:CRM",
        "builtin:crm/x",
    ] {
        let r = fx.rpc(
            Asserted::Operator,
            "app_workspace_install_check",
            json!({"source": source}),
        );
        assert!(r.is_err(), "check accepted {source}: {r:?}");
    }
    assert_eq!(fx.tree(), before);
}

/// The built-in install route needs the checked `expected_digest`, over the
/// RPC and over HTTP. Missing, stale, malformed, wrong-type and
/// other-bundle digests are refused with nothing written; agents and
/// unauthenticated callers are refused by the route class.
#[test]
fn builtin_install_route_requires_the_checked_digest() {
    let fx = Fx::start();
    let route = "/api/app-catalog/install";
    let check = fx.op(
        "app_workspace_install_check",
        json!({"source": "builtin:crm"}),
    );
    let good = check["digest"].as_str().unwrap().to_string();
    let other = fx.op(
        "app_workspace_install_check",
        json!({"source": "builtin:social-content"}),
    )["digest"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(good, other);
    let before = fx.tree();

    let bad: Vec<Value> = vec![
        json!({"catalog_id": "crm"}),
        json!({"catalog_id": "crm", "expected_digest": ""}),
        json!({"catalog_id": "crm", "expected_digest": "sha256:"}),
        json!({"catalog_id": "crm", "expected_digest": STALE}),
        json!({"catalog_id": "crm", "expected_digest": other}),
        json!({"catalog_id": "crm", "expected_digest": good.to_uppercase()}),
        json!({"catalog_id": "crm", "expected_digest": 5}),
        json!({"catalog_id": "crm", "expected_digest": null}),
        json!({"expected_digest": good}),
        json!({"catalog_id": "nope", "expected_digest": good}),
        json!({"catalog_id": "../crm", "expected_digest": good}),
    ];
    for body in &bad {
        let (status, text) = fx.http("operator", route, &body.to_string());
        assert!(status >= 400, "HTTP accepted {body}: {status} {text}");
        assert!(
            !text.contains("operator_session_required"),
            "vacuous refusal (no session): {text}"
        );
        let r = fx.rpc(
            Asserted::Operator,
            "app_workspace_install_entry",
            body.clone(),
        );
        assert!(r.is_err(), "RPC accepted {body}: {r:?}");
    }
    // forged fields on the RPC are refused outright.
    for forged in [
        json!({"catalog_id": "crm", "expected_digest": good, "source": "/tmp/x"}),
        json!({"catalog_id": "crm", "expected_digest": good, "actor": "operator"}),
        json!({"catalog_id": "crm", "expected_digest": good, "member_as": "alice"}),
    ] {
        let r = fx.rpc(
            Asserted::Operator,
            "app_workspace_install_entry",
            forged.clone(),
        );
        assert!(r.is_err(), "RPC accepted {forged}: {r:?}");
    }
    assert_eq!(fx.tree(), before, "a refused built-in install wrote");
    assert!(!catalog_exists(&fx));

    // agent and unauthenticated callers, with the right digest.
    let body = json!({"catalog_id": "crm", "expected_digest": good}).to_string();
    let (status, text) = fx.http("agent:writer", route, &body);
    assert_eq!(status, 403, "{text}");
    let (status, text) = fx.http("unproven", route, &body);
    assert!(status == 401 || status == 403, "{status} {text}");
    for who in [Asserted::Agent("writer".into()), Asserted::Unproven] {
        let r = fx.rpc(
            who,
            "app_workspace_install_entry",
            serde_json::from_str(&body).unwrap(),
        );
        assert!(r.is_err());
    }
    assert_eq!(fx.tree(), before);

    // positive control, then replay and a second install refuse.
    let (status, text) = fx.http("operator", route, &body);
    assert_eq!(status, 200, "{text}");
    let out: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(out["digest"], json!(good));
    let installed = fx.tree();
    let (status, _) = fx.http("operator", route, &body);
    assert!(status >= 400, "a second install of one app was accepted");
    assert_eq!(fx.tree(), installed);
}

/// Every operator-only explorer write refuses an agent over HTTP with no
/// change; the daemon RPC refuses it too.
#[test]
fn operator_only_explorer_writes_refuse_agents() {
    let fx = Fx::start();
    let id = fx.op(
        "app_workspace_install",
        json!({"source": fx.bundle("a", "ex-a", "1")}),
    )["install_id"]
        .as_str()
        .unwrap()
        .to_string();
    let before = fx.tree();
    let preview = fx.op("app_workspace_remove_preview", json!({"install_id": id}));
    let remove = json!({"expected_generation": preview["generation"],
        "expected_digest": preview["digest"], "request_id": "rm-http"})
    .to_string();
    let routes: Vec<(String, String)> = vec![
        (
            "/api/app-catalog/git-check".into(),
            json!({"url": "https://example.invalid/o/r"}).to_string(),
        ),
        (
            "/api/app-favorites/default".into(),
            json!({"install_ids": []}).to_string(),
        ),
        (
            "/api/app-requests/dismiss".into(),
            json!({"id": "req-x"}).to_string(),
        ),
        (
            format!("/api/app-installations/{id}/update-check"),
            "{}".into(),
        ),
        (
            format!("/api/app-installations/{id}/remove-preview"),
            "{}".into(),
        ),
        (
            format!("/api/app-installations/{id}/remove"),
            remove.clone(),
        ),
        (format!("/api/app-installations/{id}/restore"), "{}".into()),
    ];
    for (route, body) in &routes {
        let (status, text) = fx.http("agent:writer", route, body);
        assert_eq!(status, 403, "agent {route}: {status} {text}");
        let (status, text) = fx.http("unproven", route, body);
        assert!(
            status == 401 || status == 403,
            "unauth {route}: {status} {text}"
        );
    }
    assert_eq!(fx.tree(), before, "an agent changed the tracker");
    // positive control: the operator removes and restores over HTTP.
    let (status, text) = fx.http(
        "operator",
        &format!("/api/app-installations/{id}/remove"),
        &remove,
    );
    assert_eq!(status, 200, "{text}");
    let (status, text) = fx.http(
        "operator",
        &format!("/api/app-installations/{id}/restore"),
        "{}",
    );
    assert_eq!(status, 200, "{text}");
}

/// CAD-1129 F3 at the real board layer: an agent peer is refused by the
/// board's own `principal` gate on every explorer route (read or write),
/// with the board's message. The daemon would refuse the same agent with a
/// different message ("operator action") because the seam forwards the
/// identity, so asserting the board's text proves the board gate itself
/// fired: with `principal` letting an agent through, this test fails on the
/// message, even though the daemon still refuses in the seam.
#[test]
fn board_refuses_agent_peers_on_the_explorer_routes_itself() {
    let fx = Fx::start();
    let id = fx.op(
        "app_workspace_install",
        json!({"source": fx.bundle("a", "ag-a", "1")}),
    )["install_id"]
        .as_str()
        .unwrap()
        .to_string();
    fx.op("app_favorites_put", json!({"install_ids": [id]}));
    let before = fx.op("app_favorites_get", json!({}));
    let board = "agents use the CLI";
    for route in [
        "/api/app-catalog",
        "/api/app-catalog/crm",
        "/api/app-home",
        "/api/app-favorites",
        "/api/app-requests",
    ] {
        let (status, text) = fx.get("agent:writer", route);
        assert_eq!(status, 403, "GET {route}: {text}");
        assert!(
            text.contains(board),
            "GET {route} not refused by the board gate: {text}"
        );
    }
    for (route, body) in [
        ("/api/app-favorites", json!({"install_ids": []})),
        ("/api/app-favorites/opened", json!({"install_id": id})),
        ("/api/app-catalog/request", json!({"catalog_id": "crm"})),
    ] {
        let (status, text) = fx.http("agent:writer", route, &body.to_string());
        assert_eq!(status, 403, "POST {route}: {text}");
        assert!(
            text.contains(board),
            "POST {route} not refused by the board gate: {text}"
        );
    }
    assert_eq!(
        fx.op("app_favorites_get", json!({})),
        before,
        "an agent changed the operator's favourites"
    );
    // positive control: the operator reads and writes the same routes.
    let (status, text) = fx.get("operator", "/api/app-requests");
    assert_eq!(status, 200, "{text}");
    let (status, text) = fx.http(
        "operator",
        "/api/app-favorites",
        &json!({"install_ids": [id]}).to_string(),
    );
    assert_eq!(status, 200, "{text}");
}
