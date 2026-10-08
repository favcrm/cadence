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

const VET: &str = "git check";

/// Sources the check route refuses. The install-side vet must refuse the
/// same ones with its own message (not a later parse/transport error).
const BAD: &[&str] = &[
    "https://127.1/x",
    "https://localhost/x",
    "https://2130706433/x",
    "https://0x7f000001/x",
    "https://10.0.0.5/x",
    "https://foo.internal/x",
    "https://u:p@example.com/x",
    "https://example.com/x?y=1",
    "http://example.com/x",
    "ssh://host/x",
    "ssh://git@github.com/o/r",
    "git@host:x",
    "git@github.com:o/r.git",
    "git://example.com/x",
    "file:///etc",
    "relative/path",
    "--upload-pack=touch",
];

/// Remote-looking strings dressed as the two pass-through forms.
const SMUGGLE: &[&str] = &[
    "builtin:https://127.1/x",
    "builtin:https://example.com/x",
    "builtin:ssh://host/x",
    "builtin:git@host:x",
    "builtin:../..",
    "builtin:",
    "builtin:crm/../../x",
];

const METHODS: &[&str] = &[
    "app_workspace_install",
    "app_workspace_install_check",
    "app_workspace_upgrade",
    "app_workspace_upgrade_check",
];

fn params(method: &str, source: &str) -> Value {
    let digest = format!("sha256:{}", "0".repeat(64));
    match method {
        "app_workspace_install" | "app_workspace_install_check" => json!({"source": source}),
        "app_workspace_upgrade" => json!({"install_id":"inst-x","source":source,
            "expected_digest":digest,"expected_generation":"g",
            "expected_new_digest":digest,"request_id":"req-1"}),
        _ => json!({"install_id":"inst-x","source":source,
            "expected_digest":digest,"expected_generation":"g"}),
    }
}

/// CAD-1209 item 2, RPC: install, install-check, upgrade and upgrade-check
/// refuse every URL the check route refuses, by the vet itself, and write
/// nothing. Positive controls: a local absolute path and `builtin:` pass
/// the vet.
#[test]
fn rpc_routes_refuse_what_the_check_route_refuses() {
    let fx = Fx::start();
    let before = fx.tree();
    for method in METHODS {
        for url in BAD {
            let r = fx.rpc(Asserted::Operator, method, params(method, url));
            let e = r
                .err()
                .unwrap_or_else(|| panic!("{method} accepted {url}"))
                .to_string();
            assert!(
                e.contains(VET),
                "{method} {url}: not refused by the vet: {e}"
            );
            // the check route agrees on every https/other form it can name
            if url.starts_with("https://") {
                let c = fx.rpc(
                    Asserted::Operator,
                    "app_catalog_git_check",
                    json!({"url": url}),
                );
                assert!(c.is_err(), "check accepted {url}");
            }
        }
    }
    for method in METHODS {
        for src in SMUGGLE {
            let r = fx.rpc(Asserted::Operator, method, params(method, src));
            assert!(r.is_err(), "{method} accepted {src}: {r:?}");
        }
    }
    assert_eq!(fx.tree(), before, "a refused source wrote");
    assert!(!catalog_exists(&fx));

    // positive controls: past the vet (a later, different refusal or success).
    let src = fx.bundle("a", "vet-a", "1");
    for method in ["app_workspace_upgrade", "app_workspace_upgrade_check"] {
        let p = params(method, src.to_str().unwrap());
        let e = fx
            .rpc(Asserted::Operator, method, p)
            .err()
            .unwrap()
            .to_string();
        assert!(!e.contains(VET), "{method} vet refused a local path: {e}");
    }
    let chk = fx.op("app_workspace_install_check", json!({"source": src}));
    assert!(chk["digest"].is_string());
    let b = fx.op(
        "app_workspace_install_check",
        json!({"source": "builtin:crm"}),
    );
    assert!(b["digest"].is_string());
    let inst = fx.op("app_workspace_install", json!({"source": src}));
    assert!(inst["install_id"].is_string(), "{inst}");
}

/// CAD-1209 item 2, board: `POST /api/app-installations`, `/check` and the
/// per-installation upgrade routes refuse the same URLs, byte-identical
/// tracker before and after.
#[test]
fn board_routes_refuse_what_the_check_route_refuses() {
    let fx = Fx::start();
    let before = fx.tree();
    let digest = format!("sha256:{}", "0".repeat(64));
    let upg = json!({"source":"@","expected_digest":digest,"expected_generation":"g",
        "expected_new_digest":digest,"request_id":"req-1"});
    let upc = json!({"source":"@","expected_digest":digest,"expected_generation":"g"});
    for url in BAD {
        let mk = |t: &Value| {
            let mut v = t.clone();
            v["source"] = json!(url);
            v.to_string()
        };
        let cases: Vec<(String, String)> = vec![
            (
                "/api/app-installations".into(),
                json!({"source": url}).to_string(),
            ),
            (
                "/api/app-installations/check".into(),
                json!({"source": url}).to_string(),
            ),
            ("/api/app-installations/inst-x/upgrade".into(), mk(&upg)),
            (
                "/api/app-installations/inst-x/upgrade/check".into(),
                mk(&upc),
            ),
        ];
        for (route, body) in cases {
            let (status, text) = fx.http("operator", &route, &body);
            assert!(status >= 400, "{route} accepted {url}: {status} {text}");
            assert!(
                !text.contains("operator_session_required"),
                "vacuous refusal (no session) {route}: {text}"
            );
            assert!(
                text.contains(VET),
                "{route} {url}: not refused by the vet: {text}"
            );
        }
    }
    for src in SMUGGLE {
        for route in ["/api/app-installations", "/api/app-installations/check"] {
            let (status, text) = fx.http("operator", route, &json!({"source": src}).to_string());
            if route.ends_with("check") && *src == "builtin:crm" {
                continue;
            }
            assert!(status >= 400, "{route} accepted {src}: {status} {text}");
        }
    }
    assert_eq!(fx.tree(), before, "a refused board source wrote");
    assert!(!catalog_exists(&fx));

    // positive controls
    let src = fx.bundle("b", "vet-b", "1");
    let (status, text) = fx.http(
        "operator",
        "/api/app-installations/check",
        &json!({"source": src}).to_string(),
    );
    assert_eq!(status, 200, "{text}");
    let (status, text) = fx.http(
        "operator",
        "/api/app-installations/check",
        &json!({"source": "builtin:crm"}).to_string(),
    );
    assert_eq!(status, 200, "{text}");
    let (status, text) = fx.http(
        "operator",
        "/api/app-installations",
        &json!({"source": src}).to_string(),
    );
    assert_eq!(status, 200, "{text}");
}
