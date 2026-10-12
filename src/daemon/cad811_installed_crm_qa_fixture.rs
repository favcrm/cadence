//! CAD-811 opt-in installed-CRM browser fixture.
//!
//! This is an interactive test-only fixture, not routine CI coverage and not
//! browser-QA evidence by itself. Run only with the exact ignored test after
//! an isolated build/runtime admission. It installs the caller-pinned archive
//! through the real workspace check/install path and serves the real board.
#![cfg(feature = "test-seam")]

use crate::test_seam::{scoped, Asserted, Seam};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const ARCHIVE_SHA256: &str = "ec3cb02f3ad40126617d1ed24a20d11b5de53a13ae77ee07b3f127fe5c187752";
const FORM_ADAPTER_ARCHIVE_SHA256: &str =
    "e392d841d2a03cb0e1edd4035da1c39b02345314be95c467651126dd9e805349";
const TABLE_FORM_ADAPTER_ARCHIVE_SHA256: &str =
    "921291fc4d4ea0ea9f527463ae3fc13b993b661b553f91211e089dd030e20ad9";
const CAMPAIGN_DIALOG_ARCHIVE_SHA256: &str =
    "02b5a91fdb8d393ea67a4ccbaafb6bc23e1d0b4bbeebb0088c4ccdc3a91cb46b";
const FRAME_CAP: usize = 786_432;
const HOLD_MAX_SECS: u64 = 2_700;

struct Servers {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Drop for Servers {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for thread in self.threads.drain(..).rev() {
            assert!(
                thread.join().is_ok(),
                "owned CAD-811 fixture server panicked"
            );
        }
    }
}

// Deliberately no Debug: browser credentials must not reach failure output.
struct Session {
    cookie: String,
    key: String,
    token: String,
}

fn hash_file(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("read pinned CRM archive");
    format!("{:x}", Sha256::digest(bytes))
}

fn hash_tree(root: &Path) -> String {
    fn collect(root: &Path, dir: &Path, paths: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read fresh UI dist directory") {
            let entry = entry.expect("read fresh UI dist entry");
            let path = entry.path();
            let kind = entry.file_type().expect("inspect fresh UI dist entry");
            if kind.is_symlink() {
                panic!("symlink in fresh UI dist refuses fixture");
            } else if kind.is_dir() {
                collect(root, &path, paths);
            } else if kind.is_file() {
                paths.push(path.strip_prefix(root).unwrap().to_path_buf());
            } else {
                panic!("non-regular entry in fresh UI dist refuses fixture");
            }
        }
    }

    let mut paths = Vec::new();
    collect(root, root, &mut paths);
    paths.sort();
    let mut digest = Sha256::new();
    for path in paths {
        let name = path.to_str().expect("non-UTF-8 fresh UI dist path refused");
        digest.update((name.len() as u64).to_be_bytes());
        digest.update(name.as_bytes());
        let bytes = std::fs::read(root.join(&path)).expect("read fresh UI asset");
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    format!("{:x}", digest.finalize())
}

/// Safely extracts the pinned source archive: reject links/special files,
/// traversal, duplicate names, oversized members and non-UTF-8 source. The
/// package's one top-level directory is stripped to form app source.
fn extract_archive(archive: &Path, destination: &Path) {
    const SCRIPT: &str = r#"
import os, pathlib, sys, tarfile
archive, destination = sys.argv[1:]
root = pathlib.Path(destination)
seen = set(); files = []; total = 0; prefix = 'crm-0.1.0/'
with tarfile.open(archive, 'r:gz') as tf:
    members = tf.getmembers()
    if not members or len(members) > 256: raise SystemExit('archive member count refused')
    for m in members:
        name = m.name
        if name.startswith('./') or name.startswith('/') or '\\' in name or '\x00' in name:
            raise SystemExit('archive path refused')
        parts = pathlib.PurePosixPath(name).parts
        if not parts or any(p in ('', '.', '..') for p in parts) or parts[0] != 'crm-0.1.0':
            raise SystemExit('archive path refused')
        if name in seen: raise SystemExit('duplicate archive member refused')
        seen.add(name)
        if m.isdir(): continue
        if not m.isfile() or m.issym() or m.islnk(): raise SystemExit('non-regular archive member refused')
        if not name.startswith(prefix): raise SystemExit('archive root refused')
        rel = name[len(prefix):]
        if not rel or '/' not in rel and rel in ('.', '..'): raise SystemExit('empty archive member refused')
        if m.size > 16 * 1024 * 1024: raise SystemExit('archive member size refused')
        total += m.size
        if total > 32 * 1024 * 1024: raise SystemExit('archive total size refused')
        stream = tf.extractfile(m)
        if stream is None: raise SystemExit('archive member unreadable')
        data = stream.read(m.size + 1)
        if len(data) != m.size: raise SystemExit('archive member length mismatch')
        text = data.decode('utf-8')
        files.append((rel, text))
if not any(name == 'app.md' for name, _ in files): raise SystemExit('app.md missing')
for name, text in files:
    target = root.joinpath(*pathlib.PurePosixPath(name).parts)
    target.parent.mkdir(parents=True, exist_ok=True)
    with target.open('x', encoding='utf-8', newline='') as f: f.write(text)
"#;
    let status = Command::new("python3")
        .arg("-c")
        .arg(SCRIPT)
        .arg(archive)
        .arg(destination)
        .status()
        .expect("run standard-library archive verifier");
    assert!(
        status.success(),
        "pinned CRM archive failed safe extraction"
    );
}

fn rpc(state: &Path, who: Asserted, method: &str, params: Value) -> crate::Result<Value> {
    scoped(who, || {
        crate::client::rpc_timeout(state, method, params, Duration::from_secs(20))
    })
}

fn operator(state: &Path, method: &str, params: Value) -> Value {
    rpc(state, Asserted::Operator, method, params)
        .unwrap_or_else(|error| panic!("native CRM fixture RPC refused: {method}: {error}"))
}

fn board_post(
    agent: &ureq::Agent,
    base: &str,
    host: &str,
    seam: &str,
    path: &str,
    body: &Value,
) -> ureq::http::Response<ureq::Body> {
    agent
        .post(format!("{base}{path}"))
        .header("Host", host)
        .header("Origin", format!("http://{host}"))
        .header("X-Cadence-Board", "1")
        .header(crate::test_seam::AS_HEADER, "operator")
        .header(crate::test_seam::TOKEN_HEADER, seam)
        .header("Content-Type", "application/json")
        .send(body.to_string())
        .expect("owned fixture HTTP request failed")
}

fn session(state: &Path, agent: &ureq::Agent, base: &str, host: &str, seam: &str) -> Session {
    crate::operator_auth::ensure_secret(state).expect("create fixture-only operator secret");
    let secret = crate::operator_auth::read_secret(state).expect("read fixture-only secret");
    let nonce = operator(
        state,
        "operator_link_mint",
        json!({"secret": secret, "origin": "loopback"}),
    )["nonce"]
        .clone();
    let response = board_post(
        agent,
        base,
        host,
        seam,
        "/api/session",
        &json!({"nonce": nonce}),
    );
    assert_eq!(
        response.status().as_u16(),
        200,
        "native board session bootstrap refused"
    );
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .expect("session cookie header")
        .split(';')
        .next()
        .expect("cookie pair")
        .to_owned();
    let token = cookie.split_once('=').expect("cookie token").1.to_owned();
    let body: Value = response
        .into_body()
        .read_json()
        .expect("session response JSON");
    let key = body["session_key"]
        .as_str()
        .expect("page session key")
        .to_owned();
    Session { cookie, key, token }
}

fn seed_customer(state: &Path, install: &str, context: &str, id: &str, name: &str, email: &str) {
    operator(
        state,
        "app_record_create",
        json!({"install_id":install,"context_id":context,"record_id":id,
            "profile":{"schema":1,"display_name":name,"email":email,"tags":[],
                "consent":{"email":"unknown"}}}),
    );
}

fn write_private(path: &Path, bytes: &[u8]) {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .expect("create private browser bootstrap file");
    std::io::Write::write_all(&mut file, bytes).expect("write private fixture bootstrap");
}

/// Starts an actual installed CRM on an isolated native daemon and board,
/// then holds them for a human/browser QA run. This never launches a browser
/// and its successful exit is not a Browser QA PASS.
#[test]
#[ignore = "interactive installed CRM browser fixture; run only after isolated runtime admission"]
fn cad811_installed_crm_qa_fixture() {
    assert!(
        std::env::var_os("CADENCE_SOCKET").is_none(),
        "inherited CADENCE_SOCKET refuses fixture; do not clear identity/environment"
    );
    let archive_sha256 = match std::env::var("CAD811_APP_ARTIFACT") {
        Err(std::env::VarError::NotPresent) => ARCHIVE_SHA256,
        Ok(profile) if profile == "vA" => ARCHIVE_SHA256,
        Ok(profile) if profile == "form-adapter" => FORM_ADAPTER_ARCHIVE_SHA256,
        Ok(profile) if profile == "form-adapter-table" => TABLE_FORM_ADAPTER_ARCHIVE_SHA256,
        Ok(profile) if profile == "form-adapter-campaign-dialog" => CAMPAIGN_DIALOG_ARCHIVE_SHA256,
        _ => panic!(
            "CAD811_APP_ARTIFACT must be vA, form-adapter, form-adapter-table or form-adapter-campaign-dialog"
        ),
    };
    let archive = PathBuf::from(
        std::env::var_os("CAD811_APP_ARCHIVE")
            .expect("set CAD811_APP_ARCHIVE to the pinned development CRM archive"),
    );
    assert_eq!(
        hash_file(&archive),
        archive_sha256,
        "CRM archive SHA-256 differs from ticket pin"
    );
    let dist = PathBuf::from(
        std::env::var_os("CAD811_UI_DIST")
            .expect("set CAD811_UI_DIST to the fresh official UI build"),
    );
    assert!(
        dist.join("index.html").is_file(),
        "fresh UI dist has no index.html"
    );
    let ui_dist_sha256 = hash_tree(&dist);
    let binary_sha256 = std::env::current_exe()
        .map(|path| hash_file(&path))
        .expect("hash running fixture binary");
    let hold = std::env::var("CAD811_HOLD_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(2_700);
    assert!(
        (1..=HOLD_MAX_SECS).contains(&hold),
        "fixture hold duration must be 1..=2700 seconds"
    );

    let root = tempfile::Builder::new()
        .prefix("c811crm")
        .tempdir()
        .expect("isolated short fixture root");
    let state = root.path().join("state");
    let pm = root.path().join("pm");
    let source = root.path().join("verified-app");
    std::fs::create_dir_all(&state).unwrap();
    crate::issue::Pm::init(&pm).expect("initialize fixture PM");
    extract_archive(&archive, &source);
    let store = crate::store::Store::open(&crate::rollout::db_file(&state))
        .expect("open fixture rollout store");
    store
        .register_agent(&crate::store::NewAgent {
            alias: "synthetic-worker",
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: state.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .expect("register provider-refusing fixture worker");
    drop(store);

    let mut servers = Servers {
        stop: Arc::new(AtomicBool::new(false)),
        threads: Vec::new(),
    };
    let daemon = crate::daemon::ServeOptions {
        provider_env: crate::adapter::ProviderEnv::refusing_providers(),
        test_seam: true,
        stop: Some(servers.stop.clone()),
        report_router: Some(0),
        checkup: Some(0),
        ..Default::default()
    };
    daemon
        .provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let daemon_state = state.clone();
    servers.threads.push(std::thread::spawn(move || {
        crate::daemon::serve_with(&daemon_state, daemon).unwrap()
    }));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !state.join("cadence.sock").exists() || Seam::token_at(&state).is_none() {
        assert!(Instant::now() < deadline, "fixture daemon startup deadline");
        assert!(!servers.threads[0].is_finished(), "fixture daemon exited");
        std::thread::sleep(Duration::from_millis(25));
    }
    let seam = Seam::token_at(&state).expect("owned native seam token");

    let checked = operator(
        &state,
        "app_workspace_install_check",
        json!({"source":source.to_str().unwrap()}),
    );
    assert_eq!(
        checked["name"], "crm",
        "pinned source is not the actual CRM app"
    );
    for member in [
        "screens/crm/runtime.js",
        "screens/crm/client.js",
        "screens/crm/crm.css",
    ] {
        assert!(
            checked["files"]
                .as_array()
                .is_some_and(|files| files.iter().any(|file| file.as_str() == Some(member))),
            "native install check omitted CRM member {member}"
        );
    }
    let checked_digest = checked["digest"]
        .as_str()
        .expect("install check digest")
        .to_owned();
    let installed = operator(
        &state,
        "app_workspace_install",
        json!({"source":source.to_str().unwrap(),"expected_digest":checked_digest}),
    );
    assert_eq!(
        installed["approved"], true,
        "normal CRM install consent did not approve checked bytes"
    );
    let install = installed["install_id"]
        .as_str()
        .expect("install ID")
        .to_owned();
    let digest = installed["digest"]
        .as_str()
        .expect("installed bundle digest")
        .to_owned();
    assert_eq!(
        digest, checked_digest,
        "normal installer receipt differs from check"
    );
    let context = operator(&state, "app_context_create", json!({"install_id":install,
        "label":"Synthetic CRM browser QA", "input_defaults":{}, "request_id":"cad811-installed-browser-context"}))["context"]["id"]
        .as_str().expect("context ID").to_owned();
    seed_customer(
        &state,
        &install,
        &context,
        "qa-customer-1",
        "CRM QA Customer",
        "crm-qa-customer@example.test",
    );
    seed_customer(
        &state,
        &install,
        &context,
        "qa-customer-2",
        "CRM QA Prospect",
        "crm-qa-prospect@example.test",
    );

    let (port, board) = {
        let first = (std::process::id() % 90) as u16;
        let mut chosen = None;
        for offset in 0..90 {
            let candidate = 3110 + (first + offset) % 90;
            let (ready, rx) = std::sync::mpsc::channel();
            let opts = crate::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port: candidate,
                dist: Some(dist.clone()),
                stop: Some(servers.stop.clone()),
                startup: Some(ready),
                test_seam: true,
                ..Default::default()
            };
            let (state, pm) = (state.clone(), pm.clone());
            let thread = std::thread::spawn(move || drop(crate::ui::serve(&state, &pm, &opts)));
            match rx.recv_timeout(Duration::from_secs(20)) {
                Ok(Ok(())) => {
                    chosen = Some((candidate, thread));
                    break;
                }
                Ok(Err(std::io::ErrorKind::AddrInUse)) => {
                    assert!(thread.join().is_ok(), "owned bind retry thread panicked")
                }
                other => {
                    servers.threads.push(thread);
                    panic!("owned board startup failed: {other:?}");
                }
            }
        }
        chosen.expect("no owned board port available in 3110-3199")
    };
    servers.threads.push(board);
    let base = format!("http://127.0.0.1:{port}");
    let host = format!("cadence-{port}.localhost:{port}");
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .into();
    let session = session(&state, &agent, &base, &host, &seam);

    // Exercise the real screen capability and frame HTTP route once as a
    // native document probe. The browser receives a separate fresh mount.
    let mint = operator(
        &state,
        "app_screen_mint",
        json!({"install_id":install,"context_id":context,
        "tag":"crm","generation":1,"token":session.token,"key":session.key,"origin":"loopback"}),
    );
    let mount = mint["mount"].as_str().expect("native screen mint mount");
    let frame = agent
        .get(format!("{base}{mount}"))
        .header("Host", &host)
        .call()
        .expect("real frame GET failed");
    assert_eq!(frame.status().as_u16(), 200, "real CRM frame route refused");
    let csp = frame
        .headers()
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        csp.contains("default-src 'none'")
            && csp.contains("connect-src 'none'")
            && csp.contains("frame-src 'none'"),
        "CRM frame CSP is not restrictive"
    );
    let frame_html = frame
        .into_body()
        .read_to_string()
        .expect("read real frame HTML");
    assert!(
        frame_html.len() < FRAME_CAP,
        "real frame exceeds 786432-byte cap"
    );
    assert!(
        frame_html.contains("var CadenceCrmRuntime=")
            && frame_html.matches("<script nonce=").count() >= 4,
        "installed CRM startup scripts are absent from served frame"
    );
    assert!(
        !frame_html.contains(&session.token) && !frame_html.contains(&session.key),
        "session authority leaked into app frame"
    );

    let private = root.path().join("browser-private.json");
    let bootstrap = json!({"origin":format!("http://{host}"),"cookie":session.cookie,"session_key":session.key,
        "url":format!("http://{host}/app-installations/{install}?ctx={context}&crm=customers"),
        "install_id":install,"context_id":context});
    write_private(&private, bootstrap.to_string().as_bytes());
    let receipt = json!({"base_url":format!("http://{host}"),"launch_url":format!("http://{host}/app-installations/{install}?ctx={context}&crm=customers"),
        "install_id":install,"context_id":context,"bundle_digest":digest,"archive_sha256":archive_sha256,
        "binary_sha256":binary_sha256,"ui_dist_sha256":ui_dist_sha256,
        "screen_tag":"crm","ui_dist":dist,"bootstrap_file":"browser-private.json","stop_file":"STOP",
        "hold_seconds":hold,"server_port":port});
    std::fs::write(
        root.path().join("fixture-receipt.json"),
        serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();
    println!("CAD811_INSTALLED_FIXTURE_READY root={} receipt=fixture-receipt.json; browser credentials are only in browser-private.json (0600); write STOP in this root to stop owned servers", root.path().display());

    let stop_file = root.path().join("STOP");
    let expires = Instant::now() + Duration::from_secs(hold);
    while !stop_file.exists() && Instant::now() < expires {
        std::thread::sleep(Duration::from_millis(250));
    }
    drop(servers);
    // Explicitly drop the secret-bearing value before the private temp root.
    drop(session);
    println!(
        "CAD811_INSTALLED_FIXTURE_STOPPED owned servers joined; fixture state removed on return"
    );
}
