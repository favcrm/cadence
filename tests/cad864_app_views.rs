//! CAD-864 workspace-app `views/app-views-v1.json` descriptor checks,
//! ported to the CAD-482 `test_seam` harness (the CAD-1073 clean-slate
//! retired `tests/common/`): a real in-process daemon
//! (`daemon::ServeOptions{test_seam:true, ..}` + `daemon::serve_with`)
//! and, for the operator-read gate, a real board on a loopback port.
//! Callers assert identity in-band — `scoped`/`Asserted` over
//! `client::rpc`, `AS_HEADER`/`TOKEN_HEADER` over HTTP — so the suite
//! decides identically in an agent pane and in CI. Each test names the
//! outcome it proves: descriptor-bearing installs ride the verified
//! receipt, malformed/undeclared/forbidden descriptors refuse before
//! publication, the declaration↔file pairing is exact, descriptor
//! bytes are identity and upgrade-pinned, a forged install journal is
//! re-validated at recovery, and the descriptor read stays
//! operator-only and installation-bound; CAD-867's live-read check also
//! refuses an authentic pre-upgrade descriptor pin through both peers.
#![cfg(feature = "test-seam")]

use cadence_agent::issue::Pm;
use cadence_agent::store::Store;
use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
use cadence_agent::{client, daemon};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

/// A descriptor-bearing (or plain) `apps/blog-post` source tree, a
/// private PM dir and one seam-armed in-process daemon serving it.
struct Workspace {
    _root: tempfile::TempDir,
    pm: Pm,
    state: PathBuf,
    stop: Arc<AtomicBool>,
    daemon: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
}

impl Workspace {
    fn new() -> Self {
        let root = tempfile::Builder::new().prefix("c864").tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let source = root.path().join("source");
        for name in [
            "app.md",
            "workflows/blog-post.md",
            "rubrics/blog.md",
            "templates/brief.md",
            "templates/post.md",
        ] {
            let destination = source.join(name);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::copy(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("apps/blog-post")
                    .join(name),
                destination,
            )
            .unwrap();
        }
        let state = root.path().join("s");
        std::fs::create_dir_all(&state).unwrap();
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(Arc::clone(&stop)),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        let dir = state.clone();
        let handle = std::thread::spawn(move || daemon::serve_with(&dir, opts));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&state).is_none()
        {
            assert!(
                !handle.is_finished() && std::time::Instant::now() < deadline,
                "daemon never started"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        Self {
            _root: root,
            pm,
            state,
            stop,
            daemon: Some(handle),
        }
    }

    fn source(&self) -> PathBuf {
        self._root.path().join("source")
    }

    /// One daemon RPC under the asserted caller `who` — the seam's
    /// `test_caller` carries the identity; the dispatch resolves it.
    fn rpc(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let state = self.state.clone();
        scoped(who, || client::rpc(&state, method, params))
    }

    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }

    /// `app_workspace_install` asserted as the operator.
    fn install(&self) -> cadence_agent::Result<Value> {
        self.rpc(
            Asserted::Operator,
            "app_workspace_install",
            json!({"source": self.source()}),
        )
    }

    /// CAD-864: write an `app-views/v1` descriptor into the source bundle
    /// and declare it in the manifest (`needs.views.contract`). The
    /// descriptor is the CRM worked example with `app` renamed to match
    /// the bundle — a real descriptor exercising the seam, not a
    /// minimized stub.
    fn write_descriptor(&self, descriptor: &str, declare: bool) {
        let views = self.source().join("views");
        std::fs::create_dir_all(&views).unwrap();
        std::fs::write(views.join("app-views-v1.json"), descriptor).unwrap();
        if declare {
            self.declare_views();
        }
    }

    /// Add the shipped CRM binding example to this fixture package and
    /// declare the companion contract in its manifest.
    fn write_binding(&self) {
        let bindings = self.source().join("bindings");
        std::fs::create_dir_all(&bindings).unwrap();
        std::fs::write(bindings.join("app-bindings-v1.json"), Self::binding_text()).unwrap();
        let manifest = self.source().join("app.md");
        let text = std::fs::read_to_string(&manifest).unwrap();
        assert!(
            !text.contains("bindings:"),
            "source manifest already declares bindings"
        );
        std::fs::write(
            &manifest,
            text.replace(
                "  connections: [publish]",
                "  connections: [publish]\n  bindings:\n    contract: app-bindings/v1",
            ),
        )
        .unwrap();
    }

    /// The bundle as the board's `{files}` wire map — every source file
    /// as a `{path: utf8-text}` entry, the exact shape
    /// `workspace_upload` stages and `UpgradeWire`'s `FilesMap` decodes
    /// (duplicate-rejecting, before one byte touches the filesystem).
    fn files_body(&self) -> Value {
        let source = self.source();
        let mut files = serde_json::Map::new();
        for name in [
            "app.md",
            "workflows/blog-post.md",
            "rubrics/blog.md",
            "templates/brief.md",
            "templates/post.md",
            "views/app-views-v1.json",
            "bindings/app-bindings-v1.json",
        ] {
            if let Ok(text) = std::fs::read_to_string(source.join(name)) {
                files.insert(name.to_string(), json!(text));
            }
        }
        json!({"files": Value::Object(files)})
    }

    /// `files_body` with the descriptor entry re-keyed to `bad` — the
    /// map a refused upload carries.
    fn files_body_rekeyed(&self, bad: &str) -> Value {
        let mut files = self.files_body()["files"].as_object().unwrap().clone();
        let text = files
            .remove("views/app-views-v1.json")
            .expect("descriptor entry in the files map");
        files.insert(bad.to_string(), text);
        json!({"files": Value::Object(files)})
    }

    /// `needs.views.contract: app-views/v1` declared on the bundle's
    /// `app.md` — the manifest edit every declared-descriptor case makes.,
    fn declare_views(&self) {
        let manifest = self.source().join("app.md");
        let text = std::fs::read_to_string(&manifest).unwrap();
        assert!(
            !text.contains("views:"),
            "source manifest already declares views"
        );
        std::fs::write(
            &manifest,
            text.replace(
                "  connections: [publish]",
                "  connections: [publish]\n  views:\n    contract: app-views/v1",
            ),
        )
        .unwrap();
    }

    /// The descriptor bytes a descriptor-bearing source carries: the
    /// contracts crate's CRM example with `app` rebound to `blog-post`.
    fn descriptor_text() -> String {
        let text = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("contracts/app-views/v1/examples/crm.json"),
        )
        .unwrap();
        text.replace("\"app\": \"crm\"", "\"app\": \"blog-post\"")
    }

    fn binding_text() -> String {
        let text = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("contracts/app-bindings/v1/examples/crm.json"),
        )
        .unwrap();
        text.replace("\"app\": \"crm\"", "\"app\": \"blog-post\"")
    }

    fn show(&self, id: &str) -> Value {
        self.op("app_workspace_show", json!({"install_id": id}))
    }

    fn upgrade_check(&self, installed: &Value) -> Value {
        self.op(
            "app_workspace_upgrade_check",
            json!({
                "install_id":installed["install_id"], "source":self.source(),
                "expected_digest":installed["digest"],
                "expected_generation":installed["catalog_generation"]
            }),
        )
    }

    fn head(&self) -> String {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C").arg(&self.pm.dir).args(["rev-parse", "HEAD"]);
        let output = cadence_agent::reaper::output(&mut cmd).unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    }

    /// One registered agent row the seam asserts against — the registry
    /// resolves `agent:<alias>` only for real agents (an unregistered
    /// name refuses), so this stands in for `plant_member_pane`'s row.
    fn plant_agent(&self, alias: &str) {
        let store = Store::open(&self.state.join("cadence.sqlite3")).unwrap();
        store
            .register_agent(&cadence_agent::store::NewAgent {
                alias,
                provider: "claude",
                endpoint_kind: "managed",
                role: "worker",
                cwd: "/tmp",
                sandbox: "read-only",
                instructions: None,
                params: Some("{\"upstream\":\"lead\"}"),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
    }

    fn stop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(handle) = self.daemon.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A board port in 3110-3199 — never production's 3010 — held for the
/// test's lifetime by an exclusive `flock`, as the retired
/// `common::test_port` did: it excludes other threads and processes
/// alike, and the kernel releases it however the test ends.
struct PortLease {
    port: u16,
    _lock: std::fs::File,
}

fn test_port() -> PortLease {
    use std::os::fd::AsRawFd;
    let dir = std::path::Path::new("/tmp/cadence-test-ports");
    std::fs::create_dir_all(dir).unwrap();
    let span = 90;
    let start = std::process::id() as usize * 31 % span;
    for i in 0..span {
        let port = 3110 + ((start + i) % span) as u16;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!("{port}.lock")))
            .unwrap();
        // SAFETY: plain syscall on a descriptor this function owns.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            continue;
        }
        lock.set_len(0).unwrap();
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return PortLease { port, _lock: lock };
        }
    }
    panic!("no free port in 3110-3199");
}

/// A seam-armed board on `state`/`pm` bound to `port`, with its own
/// stop flag; the returned thread joins on drop through `Cleanup`.
fn board_on(
    state: &std::path::Path,
    pm: &std::path::Path,
    port: u16,
    stop: &Arc<AtomicBool>,
) -> std::thread::JoinHandle<cadence_agent::Result<()>> {
    let (startup, ready) = std::sync::mpsc::channel();
    let opts = cadence_agent::ui::ServeOpts {
        host: "127.0.0.1".into(),
        port,
        stop: Some(Arc::clone(stop)),
        startup: Some(startup),
        test_seam: true,
        ..Default::default()
    };
    let (state, pm) = (state.to_path_buf(), pm.to_path_buf());
    let board = std::thread::spawn(move || cadence_agent::ui::serve(&state, &pm, &opts));
    ready
        .recv_timeout(Duration::from_secs(20))
        .unwrap()
        .unwrap();
    board
}

/// One raw HTTP/1.0 exchange on `port`: `(status, head, body)`.
fn raw(port: u16, request: &str) -> (u16, String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(60))).ok();
    s.write_all(request.as_bytes()).unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).unwrap();
    let text = String::from_utf8_lossy(&buf).to_string();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (status, head.to_string(), body.to_string())
}

/// The operator's sign-in over the real login-link exchange: the mint
/// RPC (operator-asserted, carrying the fixture's operator secret) and
/// `POST /api/session` under the seam's operator headers. Answers
/// `(cookie, session key)`.
fn sign_in(state: &std::path::Path, port: u16) -> (String, String) {
    cadence_agent::operator_auth::ensure_secret(state).unwrap();
    let secret = cadence_agent::operator_auth::read_secret(state).unwrap();
    let mint = scoped(Asserted::Operator, || {
        client::rpc(
            state,
            "operator_link_mint",
            json!({"secret": secret, "origin": "loopback"}),
        )
    })
    .unwrap();
    let nonce = mint["nonce"].as_str().unwrap();
    let host = format!("cadence-{port}.localhost:{port}");
    let token = Seam::token_at(state).unwrap();
    let request = format!(
        "POST /api/session HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         X-Cadence-Board: 1\r\nOrigin: http://{host}\r\nSec-Fetch-Site: same-origin\r\n\
         {AS_HEADER}: operator\r\n{TOKEN_HEADER}: {token}\r\nContent-Length: {}\r\n\r\n{}",
        json!({"nonce": nonce}).to_string().len(),
        json!({"nonce": nonce})
    );
    let (status, head, body) = raw(port, &request);
    assert_eq!(status, 200, "{head}\n{body}");
    let cookie = head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()))
        .expect("no Set-Cookie");
    let cookie = cookie.split(';').next().unwrap().trim().to_string();
    let key = serde_json::from_str::<Value>(&body).unwrap()["session_key"]
        .as_str()
        .unwrap()
        .to_string();
    (cookie, key)
}

/// A GET on `path` over the board on `port`, optionally carrying an
/// operator `(cookie, key)` session, asserted as `who` through the seam
/// headers (`Some("")` sends no assertion — the ambient unattributed
/// caller).
fn get_as(
    state: &std::path::Path,
    port: u16,
    who: Option<&str>,
    session: Option<(&str, &str)>,
    path: &str,
) -> (u16, String, String) {
    let host = format!("cadence-{port}.localhost:{port}");
    let seam = match who {
        Some(who) => {
            let token = Seam::token_at(state).unwrap();
            format!("{AS_HEADER}: {who}\r\n{TOKEN_HEADER}: {token}\r\n")
        }
        None => String::new(),
    };
    let session_headers = match session {
        Some((cookie, key)) => format!("Cookie: {cookie}\r\nX-Cadence-Session: {key}\r\n"),
        None => String::new(),
    };
    let request = format!(
        "GET {path} HTTP/1.0\r\nHost: {host}\r\nX-Cadence-Board: 1\r\n\
         Origin: http://{host}\r\nSec-Fetch-Site: same-origin\r\n{seam}{session_headers}\r\n"
    );
    raw(port, &request)
}

/// A POST on `path` over the board on `port`, asserted as `who` through
/// the seam headers and carrying a JSON body plus an optional operator
/// `(cookie, key)` session. Answers `(status, head, body)`.
fn post_as(
    state: &std::path::Path,
    port: u16,
    who: &str,
    session: Option<(&str, &str)>,
    path: &str,
    body: &Value,
) -> (u16, String, String) {
    let host = format!("cadence-{port}.localhost:{port}");
    let token = Seam::token_at(state).unwrap();
    let session_headers = match session {
        Some((cookie, key)) => format!("Cookie: {cookie}\r\nX-Cadence-Session: {key}\r\n"),
        None => String::new(),
    };
    let body_text = body.to_string();
    let request = format!(
        "POST {path} HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         X-Cadence-Board: 1\r\nOrigin: http://{host}\r\nSec-Fetch-Site: same-origin\r\n\
         {AS_HEADER}: {who}\r\n{TOKEN_HEADER}: {token}\r\n{session_headers}\
         Content-Length: {}\r\n\r\n{body_text}",
        body_text.len()
    );
    raw(port, &request)
}

/* ------------------------------------------------------------------ */

/// A descriptor-bearing bundle installs and the verified receipt serves
/// the validated descriptor plus its content digest; a legacy bundle
/// without one keeps `view_descriptor: null`.
#[test]
fn cad864_descriptor_installs_and_rides_the_verified_receipt() {
    let w = Workspace::new();
    // Legacy bundle: no views/ dir, no declaration — unchanged contract.
    let plain = w.install().unwrap();
    assert_eq!(plain["view_descriptor"], Value::Null);
    assert_eq!(plain["view_descriptor_digest"], Value::Null);
    let id = plain["install_id"].as_str().unwrap().to_string();
    assert_eq!(w.show(&id)["view_descriptor"], Value::Null);

    // Fresh workspace so the descriptor-bearing bundle is not "already
    // installed".
    let w2 = Workspace::new();
    w2.write_descriptor(&Workspace::descriptor_text(), true);
    let installed = w2.install().unwrap();
    let id2 = installed["install_id"].as_str().unwrap().to_string();
    let shown = w2.show(&id2);
    let descriptor = &shown["view_descriptor"];
    assert_eq!(descriptor["contract"], "app-views/v1");
    assert_eq!(descriptor["app"], "blog-post");
    let expected_descriptor: Value = serde_json::from_str(&Workspace::descriptor_text()).unwrap();
    assert_eq!(
        descriptor, &expected_descriptor,
        "receipt serves the full source descriptor, including every declared view and field"
    );
    // The receipt's descriptor digest matches the bytes in the
    // installation's bundle digest (content-covered, so a descriptor
    // byte change is a structural change).
    assert!(shown["view_descriptor_digest"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(
        shown["digest"], installed["digest"],
        "receipt digest is the installed bundle digest"
    );
    // Descriptor bytes are part of the installed file inventory.
    assert!(shown["files"]
        .as_array()
        .unwrap()
        .contains(&json!("views/app-views-v1.json")));
}

/// Every malformed/undeclared/forbidden descriptor refuses install
/// before publication — no `.apps/` tree is created and git HEAD does
/// not move.
#[test]
fn cad864_malformed_and_forbidden_descriptors_refuse_before_install() {
    let good = Workspace::descriptor_text();
    let nested_forbidden = r#"{"contract":"app-views/v1","app":"blog-post","title":"T","views":[{"id":"v","title":"t","kind":"detail","fields":[{"id":"f","label":"l","url":"https://evil.test"}]}]}"#
        .to_string();
    // Column naming an undeclared field.
    let bad_column = r#"{"contract":"app-views/v1","app":"blog-post","title":"T","views":[{"id":"v","title":"t","kind":"table","fields":[{"id":"f","label":"l"}],"columns":[{"field":"ghost"}]}]}"#
        .to_string();
    // createView naming no declared form view.
    let bad_create_view = r#"{"contract":"app-views/v1","app":"blog-post","title":"T","views":[{"id":"v","title":"t","kind":"detail","fields":[{"id":"f","label":"l","createView":"nope"}]}]}"#
        .to_string();
    // Each case: (descriptor bytes, declare flag) that must refuse.
    let cases: Vec<(String, bool)> = vec![
        // Undeclared file — present on disk, never declared in app.md.
        (good.clone(), false),
        // Declaration without the file is covered in the next test; here
        // the file exists but is malformed.
        ("not json".to_string(), true),
        (good.replace("\"app-views/v1\"", "\"app-views/v2\""), true),
        // Forbidden keys at the root and nested inside a field.
        (
            good.replace("\"summary\":", "\"install_id\": \"forged\", \"summary\":"),
            true,
        ),
        (nested_forbidden, true),
        (bad_column, true),
        (bad_create_view, true),
        // `app` provenance must match the manifest.
        (
            good.replace("\"app\": \"blog-post\"", "\"app\": \"other-app\""),
            true,
        ),
    ];

    for (i, (descriptor, declare)) in cases.into_iter().enumerate() {
        let w = Workspace::new();
        w.write_descriptor(&descriptor, declare);
        let head = w.head();
        let err = w
            .install()
            .expect_err(&format!("case {i} must refuse install"));
        let _ = err; // refusal itself is the assertion — no partial state
        assert_eq!(w.head(), head, "case {i} moved HEAD on refusal");
        assert!(
            !w.pm.dir.join(".apps").exists(),
            "case {i} published catalog state on refusal"
        );
    }
}

/// `needs.views` declared without `views/app-views-v1.json` refuses;
/// a `views/` file under any other name refuses; a symlinked descriptor
/// refuses.
#[test]
fn cad864_declaration_file_pairing_and_layout_are_exact() {
    // Declaration without the file.
    let w = Workspace::new();
    w.declare_views();
    let head = w.head();
    assert!(
        w.install().is_err(),
        "declared views with no descriptor file installed"
    );
    assert_eq!(w.head(), head);
    assert!(!w.pm.dir.join(".apps").exists());

    // Wrong filename under views/.
    for leaf in ["other.json", "app-views-v2.json", "APP-VIEWS-V1.JSON"] {
        let w = Workspace::new();
        let views = w.source().join("views");
        std::fs::create_dir_all(&views).unwrap();
        std::fs::write(views.join(leaf), Workspace::descriptor_text()).unwrap();
        // Declaration present but the file name is wrong — the contract
        // pins exactly `views/app-views-v1.json`.
        w.declare_views();
        let head = w.head();
        assert!(
            w.install().is_err(),
            "views/{leaf} installed under the v1 declaration"
        );
        assert_eq!(w.head(), head);
        assert!(!w.pm.dir.join(".apps").exists());
    }

    // A nested dir under views/ refuses (flat dirs only).
    let w = Workspace::new();
    std::fs::create_dir_all(w.source().join("views/nested")).unwrap();
    std::fs::write(
        w.source().join("views/app-views-v1.json"),
        Workspace::descriptor_text(),
    )
    .unwrap();
    w.declare_views();
    assert!(w.install().is_err(), "nested views/ dir installed");

    // A symlinked descriptor refuses.
    #[cfg(unix)]
    {
        let w = Workspace::new();
        let views = w.source().join("views");
        std::fs::create_dir_all(&views).unwrap();
        let outside = w._root.path().join("outside.json");
        std::fs::write(&outside, Workspace::descriptor_text()).unwrap();
        std::os::unix::fs::symlink(&outside, views.join("app-views-v1.json")).unwrap();
        w.declare_views();
        assert!(w.install().is_err(), "symlinked descriptor installed");
        assert!(!w.pm.dir.join(".apps").exists());
    }
}

/// A descriptor byte change is a structural change: it moves the bundle
/// digest, an upgrade must pin it, and the upgraded receipt serves the
/// new descriptor. The old revision keeps its own descriptor bytes.
#[test]
fn cad864_descriptor_bytes_are_identity_and_upgrade_pinned() {
    let w = Workspace::new();
    w.write_descriptor(&Workspace::descriptor_text(), true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let old_digest = installed["digest"].as_str().unwrap().to_string();
    let generation = installed["catalog_generation"]
        .as_str()
        .unwrap()
        .to_string();

    // Change one label inside the descriptor (still valid) and one
    // version byte in the manifest.
    let descriptor = w.source().join("views/app-views-v1.json");
    let text = std::fs::read_to_string(&descriptor).unwrap();
    std::fs::write(&descriptor, text.replace("\"Customers\"", "\"Clients\"")).unwrap();
    let manifest = w.source().join("app.md");
    let mtext = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, mtext.replace("version: 0.1.0", "version: 0.2.0")).unwrap();

    let proposed = w.upgrade_check(&installed);
    let new_digest = proposed["digest"].as_str().unwrap().to_string();
    assert_ne!(
        new_digest, old_digest,
        "descriptor edit did not move the digest"
    );
    assert!(proposed["structural_diff"]["changed"]
        .as_array()
        .unwrap()
        .contains(&json!("views/app-views-v1.json")));

    let upgraded = w.op(
        "app_workspace_upgrade",
        json!({"install_id": id, "source": w.source(),
                "expected_digest": old_digest, "expected_generation": generation,
                "expected_new_digest": new_digest, "request_id": "desc-upgrade"}),
    );
    assert_eq!(upgraded["digest"], json!(new_digest));
    assert_eq!(upgraded["approved"], json!(false));
    let shown = w.show(&id);
    assert_eq!(
        shown["view_descriptor"]["views"][0]["title"],
        json!("Clients")
    );
    // The descriptor's own digest moved with the bytes.
    assert_ne!(
        shown["view_descriptor_digest"],
        json!(installed["view_descriptor_digest"])
    );

    // A stale upgrade (pre-change digest) refuses without mutation.
    let stale = w.rpc(
        Asserted::Operator,
        "app_workspace_upgrade",
        json!({"install_id": id, "source": w.source(),
            "expected_digest": old_digest, "expected_generation": generation,
            "expected_new_digest": new_digest, "request_id": "desc-stale"}),
    );
    assert!(
        stale.is_err(),
        "stale descriptor-bearing digest upgraded again"
    );
    assert_eq!(w.show(&id)["digest"], json!(new_digest));
}

/// A descriptor-bearing install refuses midway when the descriptor is
/// invalid, and the retained-journal recovery path also re-validates —
/// a forged journal carrying a bad descriptor cannot apply.
#[test]
fn cad864_recovery_revalidates_a_forged_descriptor_journal() {
    use std::os::unix::fs::PermissionsExt;
    let w = Workspace::new();
    w.write_descriptor(&Workspace::descriptor_text(), true);
    let hook = w.pm.dir.join(".git/hooks/pre-commit");
    let original_hook = std::fs::read(&hook).ok();
    std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    let failed = w.install().unwrap_err();
    assert!(failed.to_string().contains("git"));
    let id = std::fs::read_to_string(w.pm.dir.join(".apps/install-pending.yaml")).unwrap();
    let journal_path =
        w.pm.dir
            .join(".apps/install-journals")
            .join(format!("{}.yaml", id.trim()));
    let journal_bytes = std::fs::read(&journal_path).unwrap();
    // Forge the journal's descriptor entry to invalid bytes — the
    // journal still parses as YAML but the bundle it carries is bad.
    let mut forged: serde_yaml::Value = serde_yaml::from_slice(&journal_bytes).unwrap();
    forged["files"].as_mapping_mut().unwrap().insert(
        serde_yaml::Value::String("views/app-views-v1.json".into()),
        serde_yaml::Value::String("{\"contract\":\"app-views/v2\"}".into()),
    );
    std::fs::write(&journal_path, serde_yaml::to_string(&forged).unwrap()).unwrap();
    match original_hook {
        Some(bytes) => std::fs::write(&hook, bytes).unwrap(),
        None => std::fs::remove_file(&hook).unwrap(),
    }
    assert!(
        w.rpc(
            Asserted::Operator,
            "app_workspace_recover",
            json!({"install_id": id.trim()})
        )
        .is_err(),
        "a journal carrying an invalid descriptor applied"
    );
    // Restore and recover cleanly.
    std::fs::write(&journal_path, &journal_bytes).unwrap();
    let recovered = w.op("app_workspace_recover", json!({"install_id": id.trim()}));
    assert_eq!(recovered["committed"], true);
    assert_eq!(
        recovered["view_descriptor"]["contract"],
        json!("app-views/v1")
    );
}

/// The receipt is served only over the operator gate — an agent caller
/// (daemon RPC) and an HTTP peer carrying a stolen session both refuse;
/// the descriptor never names its own scope.
#[test]
fn cad864_descriptor_read_stays_operator_only_and_installation_bound() {
    let w = Workspace::new();
    w.write_descriptor(&Workspace::descriptor_text(), true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    w.plant_agent("desc-reader");

    // Daemon RPC: an agent caller cannot read the receipt, and an
    // unproven caller cannot either — plus a forged `actor` field and a
    // traversal id, all refused by the same operator gate.
    for forged in [
        json!({"install_id": id}),
        json!({"install_id": id, "actor": "operator"}),
        json!({"install_id": "../other"}),
    ] {
        for who in [Asserted::Agent("desc-reader".into()), Asserted::Unproven] {
            assert!(
                w.rpc(who.clone(), "app_workspace_show", forged.clone())
                    .is_err(),
                "{who:?} read descriptor receipt: {forged}"
            );
        }
    }

    // HTTP peer: the descriptor rides GET /api/app-installations/<id>
    // under the same operator read gate — a stolen session asserted
    // through an agent's caller is refused.
    let lease = test_port();
    let port = lease.port;
    let stop = Arc::new(AtomicBool::new(false));
    let board = board_on(&w.state, &w.pm.dir, port, &stop);
    struct Cleanup(
        Arc<AtomicBool>,
        Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
    );
    impl Drop for Cleanup {
        fn drop(&mut self) {
            self.0.store(true, SeqCst);
            if let Some(board) = self.1.take() {
                let _ = board.join();
            }
        }
    }
    let _cleanup = Cleanup(stop, Some(board));

    // Operator GET serves the descriptor.
    let path = format!("/api/app-installations/{id}");
    let (cookie, key) = sign_in(&w.state, port);
    let (code, _, body) = get_as(
        &w.state,
        port,
        Some("operator"),
        Some((&cookie, &key)),
        &path,
    );
    assert_eq!(code, 200, "operator descriptor read: {body}");
    let row: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(row["view_descriptor"]["app"], json!("blog-post"));

    // Agent-peered HTTP read refuses (a stolen session replayed under an
    // agent assertion — the existing operator-read gate's proof), and so
    // does an agent with no session at all.
    for held in [true, false] {
        let stolen = if held {
            Some((cookie.as_str(), key.as_str()))
        } else {
            None
        };
        for who in ["agent:desc-reader", "unproven"] {
            let (code, _, body) = get_as(&w.state, port, Some(who), stolen, &path);
            assert_eq!(
                code, 403,
                "agent-peered HTTP descriptor read ({who}, session={held}) reached {path}: {body}"
            );
        }
    }
    // A cross-install/guessed read — an id that is not a live
    // installation — refuses on both peers.
    let fake = "0123456789abcdef0123456789abcdef";
    assert!(w
        .rpc(
            Asserted::Operator,
            "app_workspace_show",
            json!({"install_id": fake})
        )
        .is_err());
    let (code, _, _) = get_as(
        &w.state,
        port,
        Some("operator"),
        Some((&cookie, &key)),
        &format!("/api/app-installations/{fake}"),
    );
    assert!(code >= 400, "unknown installation read served: {code}");
}

/// The board's `{files}` transports carry the descriptor over the SAME
/// bundle the CLI path-source install admits: `POST
/// /api/app-installations/upload` (CAD-996) and files-mode `POST
/// /api/app-installations/<id>/upgrade` (CAD-1006) both run every
/// bundle path through the one `upload_path_ok` grammar before a byte
/// is staged, so `views/app-views-v1.json` — the exact entry
/// `app_view::FILE` names — must be on it. A `views/` entry under any
/// other leaf, a nested dir, or a traversal key refuses at that gate
/// before staging: 400, HEAD unmoved, no `.apps/` and no install
/// mutation. Independent of the CLI acceptance: this drives the real
/// HTTP routes, not `app_workspace_install` with a path source.
#[test]
fn cad864_board_files_transports_admit_the_descriptor() {
    let w = Workspace::new();
    w.write_descriptor(&Workspace::descriptor_text(), true);
    let lease = test_port();
    let port = lease.port;
    let stop = Arc::new(AtomicBool::new(false));
    let board = board_on(&w.state, &w.pm.dir, port, &stop);
    struct Cleanup(
        Arc<AtomicBool>,
        Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
    );
    impl Drop for Cleanup {
        fn drop(&mut self) {
            self.0.store(true, SeqCst);
            if let Some(board) = self.1.take() {
                let _ = board.join();
            }
        }
    }
    let _cleanup = Cleanup(stop, Some(board));
    let (cookie, key) = sign_in(&w.state, port);
    let session = Some((cookie.as_str(), key.as_str()));

    // Bad cases first, on a clean tracker: a `views/` entry under a
    // wrong leaf, a traversal key and a nested dir refuse at the path
    // grammar — before staging, so no install mutation of any kind.
    let head = w.head();
    for (label, bad) in [
        ("wrong views leaf", "views/app-views-v2.json"),
        ("traversal under views/", "views/../app.md"),
        ("nested views dir", "views/sub/app-views-v1.json"),
    ] {
        let (status, _, body) = post_as(
            &w.state,
            port,
            "operator",
            session,
            "/api/app-installations/upload",
            &w.files_body_rekeyed(bad),
        );
        assert_eq!(status, 400, "{label} upload admitted: {body}");
        assert_eq!(w.head(), head, "{label} upload moved HEAD");
        assert!(
            !w.pm.dir.join(".apps").exists(),
            "{label} upload published install state"
        );
    }

    // The good bundle: the descriptor at its exact path installs through
    // the board upload, and the verified receipt serves it.
    let (status, _, body) = post_as(
        &w.state,
        port,
        "operator",
        session,
        "/api/app-installations/upload",
        &w.files_body(),
    );
    assert_eq!(status, 200, "descriptor bundle upload: {body}");
    let receipt: Value = serde_json::from_str(&body).unwrap();
    let install_id = receipt["install_id"].as_str().unwrap().to_string();
    let old_digest = receipt["digest"].as_str().unwrap().to_string();
    let generation = receipt["catalog_generation"].as_str().unwrap().to_string();
    let shown = w.show(&install_id);
    assert_eq!(shown["view_descriptor"]["contract"], json!("app-views/v1"));
    assert_eq!(shown["view_descriptor"]["app"], json!("blog-post"));
    assert!(shown["files"]
        .as_array()
        .unwrap()
        .contains(&json!("views/app-views-v1.json")));

    // Files-mode upgrade: one descriptor byte and the manifest version
    // move the digest; `upgrade_check` (path source, already exact)
    // names the proposal the files-mode upgrade must land.
    let descriptor = w.source().join("views/app-views-v1.json");
    let text = std::fs::read_to_string(&descriptor).unwrap();
    std::fs::write(&descriptor, text.replace("\"Customers\"", "\"Clients\"")).unwrap();
    let manifest = w.source().join("app.md");
    let mtext = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, mtext.replace("version: 0.1.0", "version: 0.2.0")).unwrap();
    let proposed = w.upgrade_check(&receipt);
    let new_digest = proposed["digest"].as_str().unwrap().to_string();
    assert_ne!(new_digest, old_digest);

    let upgrade_path = format!("/api/app-installations/{install_id}/upgrade");
    // A files-mode upgrade carrying a path the grammar refuses — wrong
    // views leaf, traversal — is refused before staging, and the
    // installed digest does not move.
    for (label, bad) in [
        ("wrong views leaf", "views/app-views-v2.json"),
        ("traversal under views/", "views/../app.md"),
    ] {
        let mut fields = w.files_body_rekeyed(bad);
        fields["expected_digest"] = json!(old_digest);
        fields["expected_generation"] = json!(generation);
        fields["expected_new_digest"] = json!(new_digest);
        fields["request_id"] = json!(format!("files-upgrade-bad-{label}"));
        let (status, _, body) =
            post_as(&w.state, port, "operator", session, &upgrade_path, &fields);
        assert_eq!(status, 400, "{label} files-upgrade admitted: {body}");
        assert_eq!(
            w.show(&install_id)["digest"],
            json!(old_digest),
            "{label} files-upgrade moved the install"
        );
    }

    // The good bundle upgrades over `{files}` — the staged descriptor
    // passes the board grammar and the daemon's own re-validation.
    let mut fields = w.files_body();
    fields["expected_digest"] = json!(old_digest);
    fields["expected_generation"] = json!(generation);
    fields["expected_new_digest"] = json!(new_digest);
    fields["request_id"] = json!("files-upgrade");
    let (status, _, body) = post_as(&w.state, port, "operator", session, &upgrade_path, &fields);
    assert_eq!(status, 200, "files-mode descriptor upgrade: {body}");
    let upgraded: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(upgraded["digest"], json!(new_digest));
    assert_eq!(
        w.show(&install_id)["view_descriptor"]["views"][0]["title"],
        json!("Clients")
    );
}

/// A genuine prior descriptor receipt must not authorize a live read
/// after upgrade. The same real customer view succeeds on both peers
/// with current pins, then one stale pin is refused at RPC and HTTP.
/// The test-seam identity is asserted, not native-auth proof or full
/// CAD-867 acceptance.
#[test]
fn cad867_stale_descriptor_pin_refuses_on_rpc_and_http() {
    const CANARY_NAME: &str = "CAD867 guard canary";
    const CANARY_EMAIL: &str = "cad867-private@example.invalid";

    let w = Workspace::new();
    w.write_descriptor(&Workspace::descriptor_text(), true);
    w.write_binding();
    let installed = w.install().unwrap();
    let install_id = installed["install_id"].as_str().unwrap().to_string();
    let stale_descriptor_digest = installed["view_descriptor_digest"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(stale_descriptor_digest.starts_with("sha256:"));

    let context = w.op(
        "app_context_create",
        json!({
            "install_id": install_id,
            "label": "CAD-867 acceptance",
            "input_defaults": {},
            "request_id": "cad867-view-context"
        }),
    );
    let context_id = context["context"]["id"].as_str().unwrap().to_string();
    let record_params = json!({
        "install_id": install_id,
        "context_id": context_id,
        "record_id": "cad867-customer",
        "profile": {
            "schema": 1,
            "display_name": CANARY_NAME,
            "email": CANARY_EMAIL,
            "tags": ["vip"],
            "consent": {"email": "unknown"}
        }
    });
    let created = w.op("app_record_create", record_params.clone());
    assert_eq!(created["record"]["id"], json!("cad867-customer"));
    let before = w.op(
        "app_record_show",
        record_params_without_profile(&record_params),
    );

    // Upgrade only the descriptor's display label (plus manifest
    // version), leaving the live binding and its source mapping intact.
    let descriptor_path = w.source().join("views/app-views-v1.json");
    let descriptor = std::fs::read_to_string(&descriptor_path).unwrap();
    assert!(descriptor.contains("\"Customers\""));
    std::fs::write(
        &descriptor_path,
        descriptor.replace("\"Customers\"", "\"Clients\""),
    )
    .unwrap();
    let manifest_path = w.source().join("app.md");
    let manifest = std::fs::read_to_string(&manifest_path).unwrap();
    assert!(manifest.contains("version: 0.1.0"));
    std::fs::write(
        &manifest_path,
        manifest.replace("version: 0.1.0", "version: 0.2.0"),
    )
    .unwrap();
    let proposed = w.upgrade_check(&installed);
    let new_digest = proposed["digest"].as_str().unwrap().to_string();
    assert_ne!(new_digest, installed["digest"]);
    let generation = installed["catalog_generation"].as_str().unwrap();
    w.op(
        "app_workspace_upgrade",
        json!({
            "install_id": install_id,
            "source": w.source(),
            "expected_digest": installed["digest"],
            "expected_generation": generation,
            "expected_new_digest": new_digest,
            "request_id": "cad867-view-upgrade"
        }),
    );
    let current = w.show(&install_id);
    assert_eq!(
        current["view_descriptor"]["views"][0]["title"],
        json!("Clients")
    );
    assert_ne!(
        current["view_descriptor_digest"],
        json!(stale_descriptor_digest)
    );
    assert_ne!(current["digest"], installed["digest"]);
    assert_eq!(
        current["view_binding_digest"],
        installed["view_binding_digest"]
    );

    let current_params = json!({
        "install_id": install_id,
        "view_id": "customers",
        "op": "list",
        "digest": current["digest"],
        "view_descriptor_digest": current["view_descriptor_digest"],
        "view_binding_digest": current["view_binding_digest"],
        "context_id": context_id
    });
    let rpc_positive = w
        .rpc(Asserted::Operator, "app_view_read", current_params.clone())
        .unwrap_or_else(|error| panic!("current-pin RPC control failed: {error}"));
    assert_eq!(rpc_positive["rows"].as_array().unwrap().len(), 1);
    assert_eq!(rpc_positive["rows"][0]["name"], json!(CANARY_NAME));
    assert_eq!(rpc_positive["rows"][0]["email"], json!(CANARY_EMAIL));
    assert_eq!(rpc_positive["rows"][0]["tags"], json!(["vip"]));
    for field in ["digest", "view_descriptor_digest", "view_binding_digest"] {
        assert_eq!(
            rpc_positive[field], current[field],
            "RPC did not echo {field}"
        );
    }

    let lease = test_port();
    let port = lease.port;
    let board_stop = Arc::new(AtomicBool::new(false));
    let board = board_on(&w.state, &w.pm.dir, port, &board_stop);
    struct BoardCleanup(
        Arc<AtomicBool>,
        Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
    );
    impl Drop for BoardCleanup {
        fn drop(&mut self) {
            self.0.store(true, SeqCst);
            if let Some(board) = self.1.take() {
                let _ = board.join();
            }
        }
    }
    let _board_cleanup = BoardCleanup(board_stop, Some(board));
    let (cookie, session_key) = sign_in(&w.state, port);
    let list_path = |descriptor_pin: &str| {
        format!(
            "/api/app-installations/{install_id}/views/customers/rows?digest={}&descriptor={descriptor_pin}&binding={}&context_id={context_id}",
            current["digest"].as_str().unwrap(),
            current["view_binding_digest"].as_str().unwrap()
        )
    };
    let current_path = list_path(current["view_descriptor_digest"].as_str().unwrap());
    let (http_status, _, http_body) = get_as(
        &w.state,
        port,
        Some("operator"),
        Some((&cookie, &session_key)),
        &current_path,
    );
    assert_eq!(http_status, 200, "current-pin HTTP control: {http_body}");
    let http_positive: Value = serde_json::from_str(&http_body).unwrap();
    assert_eq!(http_positive["rows"].as_array().unwrap().len(), 1);
    assert_eq!(http_positive["rows"][0]["name"], json!(CANARY_NAME));
    assert_eq!(http_positive["rows"][0]["email"], json!(CANARY_EMAIL));
    assert_eq!(http_positive["rows"][0]["tags"], json!(["vip"]));
    for field in ["digest", "view_descriptor_digest", "view_binding_digest"] {
        assert_eq!(
            http_positive[field], current[field],
            "HTTP did not echo {field}"
        );
    }

    // All other claims remain current and valid; only the descriptor
    // pin is replayed from the authentic pre-upgrade receipt.
    let mut stale_params = current_params;
    stale_params["view_descriptor_digest"] = json!(stale_descriptor_digest);
    let rpc_error = w
        .rpc(Asserted::Operator, "app_view_read", stale_params)
        .expect_err("a stale descriptor receipt authorized an RPC read");
    assert!(
        rpc_error
            .to_string()
            .contains("view descriptor digest is stale"),
        "stale RPC refused for an unrelated reason: {rpc_error}"
    );
    assert!(!rpc_error.to_string().contains(CANARY_NAME));
    assert!(!rpc_error.to_string().contains(CANARY_EMAIL));

    let stale_path = list_path(&stale_descriptor_digest);
    let (http_status, _, http_body) = get_as(
        &w.state,
        port,
        Some("operator"),
        Some((&cookie, &session_key)),
        &stale_path,
    );
    assert_eq!(http_status, 400, "stale HTTP descriptor pin: {http_body}");
    assert!(http_body.contains("app view read refused or unavailable"));
    assert!(!http_body.contains(CANARY_NAME));
    assert!(!http_body.contains(CANARY_EMAIL));

    assert_eq!(
        w.op(
            "app_record_show",
            record_params_without_profile(&record_params)
        ),
        before,
        "a refused read mutated the customer record"
    );
    assert_eq!(w.show(&install_id)["digest"], current["digest"]);
}

fn record_params_without_profile(params: &Value) -> Value {
    json!({
        "install_id": params["install_id"],
        "context_id": params["context_id"],
        "record_id": params["record_id"]
    })
}

const CAPTION_CONTEXT_REQUIRED: &str = "caption-runs read needs its live context";

fn write_caption_context_package(w: &Workspace) {
    let source = w.source();
    std::fs::remove_dir_all(&source).unwrap();
    std::fs::create_dir_all(source.join("views")).unwrap();
    std::fs::create_dir_all(source.join("bindings")).unwrap();
    std::fs::create_dir_all(source.join("workflows")).unwrap();
    std::fs::write(
        source.join("app.md"),
        r#"---
app: social-content
title: Caption context test
version: 1.0.0
summary: Test-seam fixture for scoped caption-run reads.
needs:
  connections: []
  views:
    contract: app-views/v1
  bindings:
    contract: app-bindings/v1
---

A local-only test package exposing Social Content caption-run metadata.
"#,
    )
    .unwrap();
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::fs::copy(
        manifest_dir.join("apps/social-content/views/app-views-v1.json"),
        source.join("views/app-views-v1.json"),
    )
    .unwrap();
    // The versioned contract example includes both list and show bindings;
    // the shipped app file currently enables list only.
    std::fs::copy(
        manifest_dir.join("contracts/app-bindings/v1/examples/social-content.json"),
        source.join("bindings/app-bindings-v1.json"),
    )
    .unwrap();
    std::fs::write(
        source.join("workflows/caption-context-probe.md"),
        r#"---
title: "Caption context probe: {{subject}}"
goal: "Persist a host-produced row for the scoped read acceptance"
label: Caption context probe
inputs:
  subject: { ask: "Metadata label for this test run" }
  writer: { ask: "Registered local worker" }
---

## Persist caption metadata: {{subject}}
agent: {{writer}}
size: S
action: local.text.produce

Create only the local host run record used by the context-scope check.

### Acceptance
- [ ] The run row retains this subject and its installation context.
"#,
    )
    .unwrap();
}

fn plant_caption_context_team(w: &Workspace) {
    let store = Store::open(&w.state.join("cadence.sqlite3")).unwrap();
    let cwd = w._root.path().to_str().unwrap();
    for (alias, role, params) in [
        ("cad867-caption-owner", "pm", None),
        (
            "cad867-caption-writer",
            "worker",
            Some("{\"upstream\":\"cad867-caption-owner\"}"),
        ),
    ] {
        store
            .register_agent(&cadence_agent::store::NewAgent {
                alias,
                provider: "claude",
                endpoint_kind: "managed",
                role,
                cwd,
                sandbox: "read-only",
                instructions: None,
                params,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        store
            .set_identity(
                alias,
                &cadence_agent::adapter::Identity {
                    thread_id: format!("test-{alias}"),
                    session_id: format!("session-{alias}"),
                    model: None,
                    effort: None,
                    pid: std::process::id(),
                    endpoint: None,
                    generation: Some("cad867-caption-context-fixture".into()),
                    attach: None,
                },
            )
            .unwrap();
    }
}

fn caption_read_params(
    receipt: &Value,
    view: &str,
    op: &str,
    context_id: Option<&str>,
    record_id: Option<&str>,
) -> Value {
    let mut params = json!({
        "install_id": receipt["install_id"],
        "view_id": view,
        "op": op,
        "digest": receipt["digest"],
        "view_descriptor_digest": receipt["view_descriptor_digest"],
        "view_binding_digest": receipt["view_binding_digest"]
    });
    if let Some(context_id) = context_id {
        params["context_id"] = json!(context_id);
    }
    if let Some(record_id) = record_id {
        params["record_id"] = json!(record_id);
    }
    params
}

fn caption_read_path(
    receipt: &Value,
    view: &str,
    context_id: Option<&str>,
    record_id: Option<&str>,
) -> String {
    let install_id = receipt["install_id"].as_str().unwrap();
    let route = record_id
        .map(|record_id| format!("rows/{record_id}"))
        .unwrap_or_else(|| "rows".to_string());
    let context = context_id
        .map(|context_id| format!("&context_id={context_id}"))
        .unwrap_or_default();
    format!(
        "/api/app-installations/{install_id}/views/{view}/{route}?digest={}&descriptor={}&binding={}{context}",
        receipt["digest"].as_str().unwrap(),
        receipt["view_descriptor_digest"].as_str().unwrap(),
        receipt["view_binding_digest"].as_str().unwrap()
    )
}

#[test]
fn cad867_caption_runs_require_live_context_on_rpc_and_http() {
    const SUBJECT_A: &str = "CAD867_CONTEXT_A_CANARY";
    const SUBJECT_B: &str = "CAD867_CONTEXT_B_CANARY";

    let w = Workspace::new();
    write_caption_context_package(&w);
    let installed = w.install().unwrap_or_else(|error| {
        panic!("caption-run scope fixture did not install: {error}");
    });
    let install_id = installed["install_id"].as_str().unwrap().to_string();
    let receipt = w.show(&install_id);
    for pin in ["digest", "view_descriptor_digest", "view_binding_digest"] {
        assert!(receipt[pin]
            .as_str()
            .is_some_and(|value| value.starts_with("sha256:")));
    }
    let approved = w.op(
        "app_local_install_approve",
        json!({"install_id": install_id, "digest": receipt["digest"]}),
    );
    assert_eq!(approved["approved"], true);
    plant_caption_context_team(&w);

    let make_context = |request_id: &str| {
        w.op(
            "app_context_create",
            json!({
                "install_id": install_id,
                "label": "CAD-867 caption scope acceptance",
                "input_defaults": {},
                "request_id": request_id
            }),
        )["context"]["id"]
            .as_str()
            .expect("host context creation returned an id")
            .to_string()
    };
    let context_a = make_context("cad867-caption-context-a");
    let context_b = make_context("cad867-caption-context-b");
    let make_run = |context: &str, subject: &str, request_id: &str| {
        w.op(
            "app_run_create",
            json!({
                "install_id": install_id,
                "context_id": context,
                "workflow": "caption-context-probe",
                "inputs": {
                    "subject": subject,
                    "writer": "cad867-caption-writer"
                },
                "request_id": request_id,
                "owner_pm": "cad867-caption-owner"
            }),
        )
    };
    let run_a = make_run(&context_a, SUBJECT_A, "cad867-caption-run-context-a");
    let run_b = make_run(&context_b, SUBJECT_B, "cad867-caption-run-context-b");
    assert_ne!(run_a["id"], run_b["id"]);
    assert_eq!(run_a["context_id"], json!(context_a));
    assert_eq!(run_b["context_id"], json!(context_b));

    // These valid same-install producer rows prove context filtering and
    // show authorization before any omitted-context request is judged.
    for (context, expected_id, other_id, subject) in [
        (&context_a, &run_a["id"], &run_b["id"], SUBJECT_A),
        (&context_b, &run_b["id"], &run_a["id"], SUBJECT_B),
    ] {
        let list = w
            .rpc(
                Asserted::Operator,
                "app_view_read",
                caption_read_params(&receipt, "caption-runs", "list", Some(context), None),
            )
            .unwrap_or_else(|error| panic!("valid scoped RPC list refused: {error}"));
        assert_eq!(list["rows"].as_array().unwrap().len(), 1);
        assert_eq!(list["rows"][0]["id"], *expected_id);
        assert_ne!(list["rows"][0]["id"], *other_id);
        assert_eq!(list["rows"][0]["subject"], json!(subject));
        for pin in ["digest", "view_descriptor_digest", "view_binding_digest"] {
            assert_eq!(list[pin], receipt[pin]);
        }
    }
    let rpc_show_a = w
        .rpc(
            Asserted::Operator,
            "app_view_read",
            caption_read_params(
                &receipt,
                "caption-detail",
                "show",
                Some(&context_a),
                run_a["id"].as_str(),
            ),
        )
        .unwrap_or_else(|error| panic!("valid scoped RPC show refused: {error}"));
    assert_eq!(rpc_show_a["rows"].as_array().unwrap().len(), 1);
    assert_eq!(rpc_show_a["rows"][0]["id"], run_a["id"]);
    assert_eq!(rpc_show_a["rows"][0]["subject"], json!(SUBJECT_A));
    let rpc_cross_context_show = w.rpc(
        Asserted::Operator,
        "app_view_read",
        caption_read_params(
            &receipt,
            "caption-detail",
            "show",
            Some(&context_a),
            run_b["id"].as_str(),
        ),
    );
    let rpc_cross_context_error = rpc_cross_context_show
        .as_ref()
        .err()
        .map(ToString::to_string)
        .unwrap_or_default();
    assert!(
        rpc_cross_context_error.contains("run does not belong to this context"),
        "explicit-context RPC show did not enforce context ownership"
    );
    for marker in [run_b["id"].as_str().unwrap(), SUBJECT_B] {
        assert!(!rpc_cross_context_error.contains(marker));
    }

    let lease = test_port();
    let port = lease.port;
    let board_stop = Arc::new(AtomicBool::new(false));
    let board = board_on(&w.state, &w.pm.dir, port, &board_stop);
    struct CaptionBoardCleanup(
        Arc<AtomicBool>,
        Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
    );
    impl Drop for CaptionBoardCleanup {
        fn drop(&mut self) {
            self.0.store(true, SeqCst);
            if let Some(board) = self.1.take() {
                let _ = board.join();
            }
        }
    }
    let _board_cleanup = CaptionBoardCleanup(board_stop, Some(board));
    let (cookie, session_key) = sign_in(&w.state, port);
    let session = Some((cookie.as_str(), session_key.as_str()));

    for (context, expected_id, other_id, subject) in [
        (&context_a, &run_a["id"], &run_b["id"], SUBJECT_A),
        (&context_b, &run_b["id"], &run_a["id"], SUBJECT_B),
    ] {
        let path = caption_read_path(&receipt, "caption-runs", Some(context), None);
        let (status, _, body) = get_as(&w.state, port, Some("operator"), session, &path);
        assert_eq!(status, 200, "valid scoped HTTP list refused");
        let list: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(list["rows"].as_array().unwrap().len(), 1);
        assert_eq!(list["rows"][0]["id"], *expected_id);
        assert_ne!(list["rows"][0]["id"], *other_id);
        assert_eq!(list["rows"][0]["subject"], json!(subject));
        for pin in ["digest", "view_descriptor_digest", "view_binding_digest"] {
            assert_eq!(list[pin], receipt[pin]);
        }
    }
    let path = caption_read_path(
        &receipt,
        "caption-detail",
        Some(&context_a),
        run_a["id"].as_str(),
    );
    let (status, _, body) = get_as(&w.state, port, Some("operator"), session, &path);
    assert_eq!(status, 200, "valid scoped HTTP show refused");
    let http_show_a: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(http_show_a["rows"].as_array().unwrap().len(), 1);
    assert_eq!(http_show_a["rows"][0]["id"], run_a["id"]);
    assert_eq!(http_show_a["rows"][0]["subject"], json!(SUBJECT_A));
    let path = caption_read_path(
        &receipt,
        "caption-detail",
        Some(&context_a),
        run_b["id"].as_str(),
    );
    let (status, _, body) = get_as(&w.state, port, Some("operator"), session, &path);
    assert_eq!(status, 400, "cross-context HTTP show was not refused");
    assert!(body.contains("app view read refused or unavailable"));
    assert!(!body.contains(run_b["id"].as_str().unwrap()));
    assert!(!body.contains(SUBJECT_B));

    let omitted_rpc_list = w.rpc(
        Asserted::Operator,
        "app_view_read",
        caption_read_params(&receipt, "caption-runs", "list", None, None),
    );
    let omitted_rpc_show = w.rpc(
        Asserted::Operator,
        "app_view_read",
        caption_read_params(
            &receipt,
            "caption-detail",
            "show",
            None,
            run_b["id"].as_str(),
        ),
    );
    let omitted_http_list_path = caption_read_path(&receipt, "caption-runs", None, None);
    let omitted_http_show_path =
        caption_read_path(&receipt, "caption-detail", None, run_b["id"].as_str());
    let (omitted_http_list_status, _, omitted_http_list_body) = get_as(
        &w.state,
        port,
        Some("operator"),
        session,
        &omitted_http_list_path,
    );
    let (omitted_http_show_status, _, omitted_http_show_body) = get_as(
        &w.state,
        port,
        Some("operator"),
        session,
        &omitted_http_show_path,
    );

    // Gather every omitted-context outcome before failing, so one RED
    // run reports both peers and both operations without printing rows.
    let mut failures = Vec::new();
    for (label, result) in [
        ("RPC list", &omitted_rpc_list),
        ("RPC show", &omitted_rpc_show),
    ] {
        match result {
            Err(error) => {
                let text = error.to_string();
                if !text.contains(CAPTION_CONTEXT_REQUIRED) {
                    failures.push(format!("{label} refused for an unrelated reason"));
                }
                if [
                    run_a["id"].as_str().unwrap(),
                    run_b["id"].as_str().unwrap(),
                    SUBJECT_A,
                    SUBJECT_B,
                ]
                .iter()
                .any(|marker| text.contains(marker))
                {
                    failures.push(format!("{label} refusal disclosed a run marker"));
                }
            }
            Ok(value) => {
                let count = value["rows"].as_array().map_or(0, Vec::len);
                failures.push(format!(
                    "{label} accepted omitted context and returned {count} row(s)"
                ));
            }
        }
    }
    for (label, status, body) in [
        (
            "HTTP list",
            omitted_http_list_status,
            &omitted_http_list_body,
        ),
        (
            "HTTP show",
            omitted_http_show_status,
            &omitted_http_show_body,
        ),
    ] {
        if status != 400 || !body.contains("app view read refused or unavailable") {
            failures.push(format!(
                "{label} omitted-context request did not return generic 400"
            ));
        }
        if [
            run_a["id"].as_str().unwrap(),
            run_b["id"].as_str().unwrap(),
            SUBJECT_A,
            SUBJECT_B,
        ]
        .iter()
        .any(|marker| body.contains(marker))
        {
            failures.push(format!("{label} refusal disclosed a run marker"));
        }
    }
    assert!(
        failures.is_empty(),
        "caption-runs omitted-context guard failed: {}",
        failures.join("; ")
    );
}
