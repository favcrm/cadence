//! CAD-786 e2e harness: a real browser driving the board UI against an
//! in-process daemon + the CAD-785 loopback SMTP rig. Runs only when
//! `CADENCE_E2E_HOLD=1`; starts a fenced board on 3110-3199 serving a
//! fresh `pnpm build` dist, seeds a synthetic CRM install (5 customers:
//! one in an exclusion list, one without consent; approved content; a
//! frozen audience; a bound SMTP connection), prints
//! `{url, login_link, rig_log_path}` as JSON, then blocks on stdin.
//!
//! A separate Node/Chromium script drives the flow and asserts the
//! counts. This file is `#[ignore]` — the suite never runs it.
#![cfg(feature = "test-seam")]
#![allow(clippy::disallowed_methods)]

mod common;

use common::daemon_opts;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SECRET: &str = "9fK2-qW7z-Xm4p-Lv8t-63";
const USERNAME: &str = "smtp-user";
const SENDER: &str = "news@example.com";

fn openssl(args: &[&str], dir: &Path) {
    let status = Command::new("openssl")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("openssl is required for the isolated SMTP rig");
    assert!(
        status.status.success(),
        "openssl {args:?}: {}",
        String::from_utf8_lossy(&status.stderr)
    );
}

fn mint_ca(dir: &Path) {
    openssl(
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "ca-key.pem",
            "-out",
            "ca.pem",
            "-days",
            "2",
            "-subj",
            "/CN=cadence-smtp-test-ca",
        ],
        dir,
    );
    openssl(
        &[
            "req",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "srv-key.pem",
            "-out",
            "srv.csr",
            "-subj",
            "/CN=localhost",
        ],
        dir,
    );
    std::fs::write(
        dir.join("srv-ext.cnf"),
        "subjectAltName=DNS:localhost,IP:127.0.0.1\n",
    )
    .unwrap();
    openssl(
        &[
            "x509",
            "-req",
            "-in",
            "srv.csr",
            "-CA",
            "ca.pem",
            "-CAkey",
            "ca-key.pem",
            "-CAcreateserial",
            "-days",
            "2",
            "-out",
            "srv-cert.pem",
            "-extfile",
            "srv-ext.cnf",
        ],
        dir,
    );
}

/// The CAD-785 loopback SMTP rig as a subprocess: accepts AUTH for the
/// enrolled credential, 250s every RCPT/DATA, and appends each captured
/// message (MAIL FROM, RCPT TO, and the raw DATA block) to a log file
/// the driver diffs. The cert/key come from the fixture CA.
/// Spawn the rig subprocess. Its process outlives the harness — the
/// daemon will keep dialling it while the browser drives, so we never
/// `wait()` it; the fixture's tempdir cleans up with the suite.
#[allow(clippy::zombie_processes)]
fn start_rig(ca_dir: &Path, log: &Path) -> u16 {
    let script = ca_dir.join("rig.py");
    std::fs::write(
        &script,
        r#"import base64, socket, ssl, sys, threading

port_file, log_path, cert, key, user, pw = sys.argv[1:7]
log = open(log_path, "a", buffering=1)
ls = socket.socket()
ls.bind(("127.0.0.1", 0))
ls.listen(8)
port = ls.getsockname()[1]
with open(port_file + ".tmp", "w") as f:
    f.write(str(port))
import os
os.rename(port_file + ".tmp", port_file)
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(cert, key)

def serve(conn, peer):
    try:
        conn = ctx.wrap_socket(conn, server_side=True)
        conn.sendall(b"220 localhost cadence-test-smtp\r\n")
        auth_ok = False
        mail_from = None
        rcpts = []
        data = []
        in_data = False
        buf = b""
        while True:
            chunk = conn.recv(65536)
            if not chunk:
                break
            buf += chunk
            while b"\r\n" in buf or b"\n" in buf:
                i = buf.find(b"\r\n")
                j = buf.find(b"\n")
                cut = i if (i != -1 and (j == -1 or i < j)) else j
                line = buf[:cut].decode(errors="replace").rstrip("\r\n")
                buf = buf[cut + 2 if (i != -1 and (j == -1 or i < j)) else cut + 1:]
                if in_data:
                    if line == ".":
                        in_data = False
                        log.write("=== MESSAGE ===\n")
                        log.write("mail_from=" + str(mail_from) + "\n")
                        for r in rcpts:
                            log.write("rcpt=" + r + "\n")
                        log.write("data<<EOF\n" + "\n".join(data) + "\nEOF\n")
                        conn.sendall(b"250 accepted\r\n")
                    else:
                        data.append(line)
                    continue
                head = line.split(" ", 1)[0].upper()
                if head in ("EHLO", "HELO"):
                    conn.sendall(b"250-localhost\r\n250 AUTH LOGIN PLAIN\r\n")
                elif head == "AUTH":
                    # AUTH LOGIN: user/pass on following lines.
                    parts = line.split(" ", 2)
                    conn.sendall(b"334 VXNlcm5hbWU6\r\n")
                    while b"\r\n" not in buf and b"\n" not in buf:
                        more = conn.recv(65536)
                        if not more:
                            break
                        buf += more
                    uline, buf = buf.split(b"\n", 1)
                    conn.sendall(b"334 UGFzc3dvcmQ6\r\n")
                    while b"\r\n" not in buf and b"\n" not in buf:
                        more = conn.recv(65536)
                        if not more:
                            break
                        buf += more
                    pline, buf = buf.split(b"\n", 1)
                    u = base64.b64decode(uline.strip()).decode(errors="replace")
                    p = base64.b64decode(pline.strip()).decode(errors="replace")
                    if u == user and p == pw:
                        auth_ok = True
                        conn.sendall(b"235 ok\r\n")
                    else:
                        conn.sendall(b"535 refused\r\n")
                elif head == "MAIL":
                    if not auth_ok:
                        conn.sendall(b"530 authentication required\r\n")
                    else:
                        mail_from = line
                        conn.sendall(b"250 ok\r\n")
                elif head == "RCPT":
                    if not auth_ok:
                        conn.sendall(b"530 authentication required\r\n")
                    else:
                        rcpts.append(line)
                        conn.sendall(b"250 ok\r\n")
                elif head == "DATA":
                    if not auth_ok or not rcpts:
                        conn.sendall(b"503 bad sequence\r\n")
                    else:
                        conn.sendall(b"354 go\r\n")
                        in_data = True
                        data = []
                elif head in ("RSET", "NOOP"):
                    conn.sendall(b"250 ok\r\n")
                elif head == "QUIT":
                    conn.sendall(b"221 bye\r\n")
                    return
                else:
                    conn.sendall(b"502 unimplemented\r\n")
    except Exception as exc:
        try:
            log.write("rig error: " + str(exc) + "\n")
        except Exception:
            pass
    finally:
        try:
            conn.close()
        except Exception:
            pass

while True:
    c, p = ls.accept()
    threading.Thread(target=serve, args=(c, p), daemon=True).start()
"#,
    )
    .unwrap();
    let port_file = ca_dir.join("rig.port");
    // A stale port file from an earlier run must not answer the wait —
    // the rig writes its real port after bind.
    let _ = std::fs::remove_file(&port_file);
    let log_path = log.to_path_buf();
    let cert = ca_dir.join("srv-cert.pem");
    let key = ca_dir.join("srv-key.pem");
    Command::new("python3")
        .args([
            script.to_str().unwrap(),
            port_file.to_str().unwrap(),
            log_path.to_str().unwrap(),
            cert.to_str().unwrap(),
            key.to_str().unwrap(),
            USERNAME,
            SECRET,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start rig");
    for _ in 0..200 {
        if port_file.exists() {
            return std::fs::read_to_string(&port_file)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("rig did not bind");
}

/// `pnpm build` the UI dist — the board serves it from `--dist`.
fn build_dist(_root: &Path) -> PathBuf {
    let ui = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    let status = Command::new("pnpm")
        .args(["build"])
        .current_dir(&ui)
        .status()
        .expect("pnpm build the board dist");
    assert!(status.success(), "pnpm build failed");
    let dist = ui.join("dist");
    assert!(dist.join("index.html").exists(), "no dist index.html");
    dist
}

/// Seed a synthetic CRM install: 5 customers (one in an exclusion
/// list, one without consent), approved content, a frozen audience
/// and a bound SMTP connection. Returns (install_id, context_id).
fn seed(daemon: &common::TestDaemon, rig_port: u16) -> (String, String) {
    let install = daemon
        .operator_rpc(
            "app_workspace_install",
            json!({"source": source_dir().join("source")}),
        )
        .unwrap()["install_id"]
        .as_str()
        .unwrap()
        .to_string();
    let context = daemon
        .operator_rpc(
            "app_context_create",
            json!({"install_id": install, "label": "brand", "input_defaults": {}, "request_id": "ctx-1"}),
        )
        .unwrap()["context"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    // 5 customers: A (consent, excluded), B (no consent), C/D/E ok.
    for (id, profile) in [
        (
            "customer-a",
            r#"{"schema":1,"display_name":"Amina Diallo","email":"amina@example.com","tags":[],"consent":{"email":"granted"}}"#,
        ),
        (
            "customer-b",
            r#"{"schema":1,"display_name":"Boris Feld","email":"boris@example.com","tags":[],"consent":{"email":"unknown"}}"#,
        ),
        (
            "customer-c",
            r#"{"schema":1,"display_name":"Cleo Boone","email":"cleo@example.com","tags":[],"consent":{"email":"granted"}}"#,
        ),
        (
            "customer-d",
            r#"{"schema":1,"display_name":"Dana Cole","email":"dana@example.com","tags":[],"consent":{"email":"granted"}}"#,
        ),
        (
            "customer-e",
            r#"{"schema":1,"display_name":"Erin Vale","email":"erin@example.com","tags":[],"consent":{"email":"granted"}}"#,
        ),
    ] {
        daemon
            .operator_rpc(
                "app_record_create",
                json!({"install_id": install, "context_id": context, "record_id": id,
                    "profile": serde_json::from_str::<Value>(profile).unwrap()}),
            )
            .unwrap();
    }
    // Exclusion list carrying customer-a; the freeze applies it.
    daemon
        .operator_rpc(
            "app_exclusion_save",
            json!({"install_id": install, "context_id": context, "list_id": "skip-1",
                "name": "Skip", "member_ids": ["customer-a"]}),
        )
        .unwrap();
    // Approved content.
    daemon
        .operator_rpc(
            "app_content_save",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
            "subject": "Spring launch", "preheader": "News", "blocks": [
                {"type":"heading","text":"Hello {{first_name|Friend}}"},
                {"type":"paragraph","text":"A calm first line."},
                {"type":"button","label":"Read more","url":"https://example.com/posts/welcome"}
            ]}),
        )
        .unwrap();
    let revision = daemon
        .operator_rpc(
            "app_content_show",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1"}),
        )
        .unwrap()["content"]["revision"]
        .as_u64()
        .unwrap();
    daemon
        .operator_rpc(
            "app_content_approve",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                "expected_revision": revision}),
        )
        .unwrap();
    // Freeze: base all, exclusion skip-1, ceiling 50 → A excluded,
    // B no-consent, so the frozen audience is C/D/E (3 members). The
    // id matches the board's `<campaign>-freeze-1` default.
    daemon
        .operator_rpc(
            "app_audience_prepare",
            json!({"install_id": install, "context_id": context, "freeze_id": "launch-1-freeze-1",
                "base": {"mode":"all"}, "exclusion_list_id": "skip-1", "max_recipients": 50}),
        )
        .unwrap();
    // Enroll + bind the SMTP connection.
    let connection = daemon
        .operator_rpc(
            "connection_create",
            json!({"provider": "smtp", "account": "send-rig", "shape": "smtp",
                "host": "localhost", "port": rig_port, "tls_mode": "implicit",
                "username": USERNAME, "secret": SECRET,
                "sender": SENDER, "sender_name": "CRM News",
                "scopes": ["email:send"], "accept_same_uid_risk": true}),
        )
        .unwrap()["connection"]
        .clone();
    daemon
        .operator_rpc(
            "crm_smtp_bind",
            json!({"install_id": install, "context_id": context,
                "connection_id": connection["id"], "request_id": "bind-1"}),
        )
        .unwrap();
    (install, context)
}

fn source_dir() -> PathBuf {
    let root = PathBuf::from(std::env::var("CADENCE_E2E_DIR").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("cad786-e2e")
            .to_string_lossy()
            .to_string()
    }));
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// `cadence ui login --json` for the fixture board — a real link the
/// driver opens once to mint the operator session. The login proves
/// the operator through the same test seam the daemon itself carries:
/// `CADENCE_TEST_AS=operator` on the login child asserts the identity
/// the board's session binds.
fn login_link(bin: &str, state: &Path, port: u16) -> String {
    let out = Command::new(bin)
        .args([
            "--state-dir",
            state.to_str().unwrap(),
            "ui",
            "login",
            "--json",
            "--port",
            &port.to_string(),
        ])
        .env(cadence_agent::test_seam::AS_ENV, "operator")
        .output()
        .expect("ui login");
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    serde_json::from_str::<Value>(text.trim())
        .unwrap_or_else(|_| panic!("ui login did not answer JSON: {text}"))["link"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
#[ignore = "e2e harness — CADENCE_E2E_HOLD=1 only"]
fn cad786_ui_send_controls_e2e() {
    if std::env::var("CADENCE_E2E_HOLD").ok().as_deref() != Some("1") {
        eprintln!("skipped: CADENCE_E2E_HOLD=1 not set");
        return;
    }
    let work = source_dir();
    let ca_dir = work.join("ca");
    std::fs::create_dir_all(&ca_dir).unwrap();
    mint_ca(&ca_dir);
    let rig_log = work.join("rig.log");
    let rig_port = start_rig(&ca_dir, &rig_log);

    let pm_dir = work.join("pm");
    let pm = cadence_agent::issue::Pm::init(&pm_dir).unwrap();
    copy_source(&work.join("source"));

    let mut opts = daemon_opts();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
    cadence_agent::platform::smtp::attach(&mut opts);
    opts.smtp_test_ca_pem = Some(std::fs::read(ca_dir.join("ca.pem")).unwrap());
    opts.crm_send_interval_ms = 50;
    // The unsubscribe origin is the board's own loopback — the
    // recipient's browser opens the GET page and the POST redeems.
    let daemon = common::TestDaemon::start_opts(opts);

    let dist = build_dist(&work);
    let lease = common::test_port();
    let port = lease.port;
    let state = daemon.state.clone();
    let pm_path = pm.dir.clone();
    std::thread::spawn(move || {
        let opts = cadence_agent::ui::ServeOpts {
            host: "127.0.0.1".to_string(),
            port,
            dist: Some(dist),
            test_seam: cfg!(feature = "test-seam"),
            ..Default::default()
        };
        let _ = cadence_agent::ui::serve(&state, &pm_path, &opts);
    });
    // Wait for health.
    for _ in 0..200 {
        if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            use std::io::Write;
            let _ = s.write_all(b"GET /api/health HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n");
            let mut buf = String::new();
            use std::io::Read;
            if s.read_to_string(&mut buf).is_ok() && buf.contains("200") {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let (install, context) = seed(&daemon, rig_port);
    // Point the unsubscribe origin at this board so minted links open
    // the same-page unsubscribe flow.
    daemon
        .operator_rpc(
            "crm_send_origin_set",
            json!({"unsubscribe_origin": format!("http://127.0.0.1:{port}")}),
        )
        .unwrap();

    let link = login_link(env!("CARGO_BIN_EXE_cadence"), &daemon.state, port);
    let out = json!({
        "url": format!("http://cadence-{port}.localhost:{port}/app-installations/{install}?ctx={context}&crm=campaigns&record=launch-1"),
        "login_link": link,
        "rig_log_path": rig_log.to_string_lossy(),
        "board_port": port,
        "install_id": install,
        "context_id": context,
        "campaign_id": "launch-1",
        "freeze_id": "launch-1-freeze-1",
    });
    println!("CADENCE_E2E_JSON={}", out);
    eprintln!("e2e harness up on :{port} — holding for the driver");
    // Hold until killed — the driver (or the suite teardown) signals
    // the process; a closed stdin must not drop the rig mid-flight.
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

fn copy_source(into: &Path) {
    // The board renders the CRM screens for the installation whose app
    // is named `crm`; the seeded source is the real CRM bundle.
    for name in ["app.md", "workflows/email-brief.md", "rubrics/email.md"] {
        let destination = into.join(name);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("workspace-apps/crm")
                .join(name),
            &destination,
        )
        .unwrap();
    }
}
