//! CAD-1186 acceptance check (reviewer-written, from the ticket): the install
//! digest pin is a gate. A real in-process daemon and board (CAD-482 test
//! seam). Each case proves the bad input is refused by the real guard and
//! that nothing on disk moved: catalog bytes, generation, journals and
//! install records are compared as a whole-tree byte snapshot.
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

const BAD_PINS: [&str; 7] = [
    "",
    "sha256:",
    "sha256:abc",
    "SHA256:0000000000000000000000000000000000000000000000000000000000000000",
    "sha256:ZZZZ000000000000000000000000000000000000000000000000000000000000",
    "sha256:0000000000000000000000000000000000000000000000000000000000000000",
    "md5:00000000000000000000000000000000",
];

/// install-check in a fresh workspace writes nothing and creates no catalog;
/// its digest is the one install then records.
#[test]
fn install_check_is_read_only_and_digest_matches_install() {
    let fx = Fx::start();
    let src = fx.bundle("a", "pin-a", "1");
    let before = fx.tree();
    let check = fx.op("app_workspace_install_check", json!({"source": src}));
    assert_eq!(fx.tree(), before, "install-check wrote to the tracker");
    assert!(
        !catalog_exists(&fx),
        "install-check created the catalog: {:?}",
        fx.tree().keys().collect::<Vec<_>>()
    );
    let digest = check["digest"].as_str().unwrap().to_string();
    let out = fx.op(
        "app_workspace_install",
        json!({"source": src, "expected_digest": digest}),
    );
    let shown = fx.op(
        "app_workspace_show",
        json!({"install_id": out["install_id"]}),
    );
    assert_eq!(
        shown["digest"],
        json!(digest),
        "install-check digest differs from the recorded one"
    );
}

/// A mismatched or malformed pin is refused with no change at all, on a fresh
/// workspace and on a populated one; the pin is not consumed by a refusal.
#[test]
fn mismatched_and_malformed_pins_change_nothing_over_rpc() {
    let fx = Fx::start();
    let a = fx.bundle("a", "pin-a", "1");
    let b = fx.bundle("b", "pin-b", "1");
    let good_b = fx.op("app_workspace_install_check", json!({"source": b}))["digest"]
        .as_str()
        .unwrap()
        .to_string();
    // fresh workspace
    let before = fx.tree();
    for pin in BAD_PINS {
        let r = fx.rpc(
            Asserted::Operator,
            "app_workspace_install",
            json!({"source": b, "expected_digest": pin}),
        );
        assert!(r.is_err(), "pin {pin:?} accepted: {r:?}");
    }
    let r = fx.rpc(
        Asserted::Operator,
        "app_workspace_install",
        json!({"source": b, "expected_digest": 7}),
    );
    assert!(r.is_err(), "non-string pin accepted");
    assert_eq!(fx.tree(), before, "refused pin changed the fresh tracker");
    // populated workspace
    fx.op("app_workspace_install", json!({"source": a}));
    let before = fx.tree();
    for pin in BAD_PINS {
        let r = fx.rpc(
            Asserted::Operator,
            "app_workspace_install",
            json!({"source": b, "expected_digest": pin}),
        );
        assert!(r.is_err(), "pin {pin:?} accepted: {r:?}");
    }
    // bundle edited after the check: the old digest is refused.
    std::fs::write(b.join("app.md"), manifest("pin-b", "2")).unwrap();
    let r = fx.rpc(
        Asserted::Operator,
        "app_workspace_install",
        json!({"source": b, "expected_digest": good_b}),
    );
    assert!(r.is_err(), "stale pin accepted after bundle edit: {r:?}");
    assert_eq!(
        fx.tree(),
        before,
        "refused pin changed catalog/journals/records"
    );
    // the new digest installs.
    let fresh = fx.op("app_workspace_install_check", json!({"source": b}))["digest"].clone();
    assert_ne!(fresh, json!(good_b));
    fx.op(
        "app_workspace_install",
        json!({"source": b, "expected_digest": fresh}),
    );
}

/// The HTTP peer is at least as strict: both install routes refuse the same
/// pins and changed bytes with no change; unknown/duplicate/case-variant keys
/// are refused; the matching pin succeeds.
#[test]
fn http_install_routes_refuse_like_the_rpc() {
    let fx = Fx::start();
    let a = fx.bundle("a", "pin-a", "1");
    let b = fx.bundle("b", "pin-b", "1");
    fx.op("app_workspace_install", json!({"source": a}));
    let before = fx.tree();
    let wrong = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
    let files = |v: &str| json!({"app.md": manifest("pin-b", v), "workflows/do.md": WORKFLOW});
    let mut bodies: Vec<(&str, String)> = vec![];
    for pin in BAD_PINS.iter().copied().chain([wrong]) {
        bodies.push((
            "/api/app-installations",
            json!({"source": b, "expected_digest": pin}).to_string(),
        ));
        bodies.push((
            "/api/app-installations/upload",
            json!({"files": files("1"), "expected_digest": pin}).to_string(),
        ));
    }
    // non-string, case variant, unknown, duplicate keys
    bodies.push((
        "/api/app-installations",
        json!({"source": b, "expected_digest": 5}).to_string(),
    ));
    bodies.push((
        "/api/app-installations/upload",
        json!({"files": files("1"), "expected_digest": 5}).to_string(),
    ));
    bodies.push((
        "/api/app-installations",
        json!({"source": b, "Expected_Digest": wrong}).to_string(),
    ));
    bodies.push((
        "/api/app-installations/upload",
        json!({"files": files("1"), "Expected_Digest": wrong}).to_string(),
    ));
    bodies.push((
        "/api/app-installations/upload",
        format!(
            "{{\"files\":{},\"expected_digest\":\"{wrong}\",\"expected_digest\":\"{wrong}\"}}",
            files("1")
        ),
    ));
    for (route, body) in &bodies {
        let (status, text) = fx.http("operator", route, body);
        assert!(status >= 400, "{route} accepted {body}: {status} {text}");
        assert!(
            !text.contains("operator_session_required"),
            "vacuous refusal (no session): {text}"
        );
    }
    assert_eq!(
        fx.tree(),
        before,
        "refused HTTP install changed the tracker"
    );
    // Agent callers are refused by the route class even with a right pin.
    let good = fx.op("app_workspace_install_check", json!({"source": b}))["digest"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, _) = fx.http(
        "agent:writer",
        "/api/app-installations",
        &json!({"source": b, "expected_digest": good}).to_string(),
    );
    assert_eq!(status, 403);
    assert_eq!(fx.tree(), before);
    // The matching pin works on both routes (upload bytes == dir bytes).
    let (status, text) = fx.http(
        "operator",
        "/api/app-installations",
        &json!({"source": b, "expected_digest": good}).to_string(),
    );
    assert_eq!(status, 200, "{text}");
    let c = fx.bundle("c", "pin-c", "1");
    let good_c = fx.op("app_workspace_install_check", json!({"source": c}))["digest"]
        .as_str()
        .unwrap()
        .to_string();
    let up = json!({"files": {"app.md": manifest("pin-c", "1"), "workflows/do.md": WORKFLOW}, "expected_digest": good_c});
    let (status, text) = fx.http("operator", "/api/app-installations/upload", &up.to_string());
    assert_eq!(status, 200, "upload with matching pin: {text}");
}

/// install-check is operator-only: an agent and a detached child (unproven)
/// are refused over the RPC, and nothing is created.
#[test]
fn install_check_refuses_agents_and_unproven_callers() {
    let fx = Fx::start();
    let src = fx.bundle("a", "pin-a", "1");
    let before = fx.tree();
    for who in [Asserted::Agent("writer".into()), Asserted::Unproven] {
        let r = fx.rpc(
            who.clone(),
            "app_workspace_install_check",
            json!({"source": src}),
        );
        assert!(r.is_err(), "{who:?} ran install-check: {r:?}");
        let r = fx.rpc(
            who.clone(),
            "app_workspace_install",
            json!({"source": src, "expected_digest": "sha256:x"}),
        );
        assert!(r.is_err(), "{who:?} ran install: {r:?}");
    }
    // forged identity/approval fields never ride install-check either
    for forged in [
        json!({"source": src, "approved": true}),
        json!({"source": src, "actor": "operator"}),
    ] {
        let r = fx.rpc(
            Asserted::Operator,
            "app_workspace_install_check",
            forged.clone(),
        );
        assert!(r.is_err(), "{forged} accepted: {r:?}");
    }
    assert_eq!(fx.tree(), before);
}
