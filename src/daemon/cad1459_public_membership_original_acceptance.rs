//! Independent SEC-003 acceptance for a public session on a natively hosted
//! daemon. Membership HTTP is the only stub; the proof is a real Ed25519 JWS
//! the production verifier accepts. Authority, Store, custody and the original
//! guards are the real ones. No membership lookup seam and no fabricated
//! original receipt.

use super::*;
use crate::issuer_transport::test_fixture::{self, Fixture};
use crate::operator_auth::BoardUser;
use crate::remote_cli::cad1459_account_image_acceptance as login;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::sync::{mpsc, Condvar};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

const TEST: &str = "daemon::cad1365_original_transfer_acceptance::cad1459_public_membership_original_acceptance::cad1459_public_membership_original_refuses_without_holding_guards";
const CHILD: &str = "CAD1459_PUBLIC_MEMBER_ROOT";
const COMPANY: &str = "cad1365-test-company";
const INSTANCE: &str = "inst0001";
const PUBLIC_HOST: &str = "cad1365.example.test";
const KID: &str = "cad1459-mem";
static PROOF_SEQ: AtomicU64 = AtomicU64::new(1);

struct Gate {
    pause_at: AtomicUsize,
    seen: AtomicUsize,
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}

impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            pause_at: AtomicUsize::new(0),
            seen: AtomicUsize::new(0),
            state: Mutex::new((false, false)),
            changed: Condvar::new(),
        })
    }

    fn arm(&self, pause_at: usize) {
        self.pause_at.store(pause_at, Ordering::SeqCst);
        self.seen.store(0, Ordering::SeqCst);
        *self.state.lock().unwrap() = (false, false);
    }

    fn arrive(&self) {
        let n = self.seen.fetch_add(1, Ordering::SeqCst) + 1;
        let pause_at = self.pause_at.load(Ordering::SeqCst);
        if pause_at == 0 || n < pause_at {
            return;
        }
        let mut state = self.state.lock().unwrap();
        state.0 = true;
        self.changed.notify_all();
        while !state.1 {
            state = self.changed.wait(state).unwrap();
        }
    }

    fn reached(&self) -> bool {
        let state = self.state.lock().unwrap();
        let (state, _) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(8), |s| !s.0)
            .unwrap();
        state.0
    }

    fn release(&self) {
        self.state.lock().unwrap().1 = true;
        self.changed.notify_all();
    }
}

struct Pending {
    gate: Arc<Gate>,
    threads: Vec<JoinHandle<()>>,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.gate.release();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

struct Membership {
    gate: Arc<Gate>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    address: std::net::SocketAddr,
}

impl Membership {
    fn start(gate: Arc<Gate>) -> Self {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let signer = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let x = URL_SAFE_NO_PAD.encode(signer.public_key().as_ref());
        let jwks = json!({"keys":[{"kty":"OKP","crv":"Ed25519","kid":KID,"x":x}]})
            .to_string()
            .into_bytes();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (gate_thread, stop_thread) = (gate.clone(), stop.clone());
        let thread = thread::spawn(move || {
            serve_membership(listener, signer, jwks, gate_thread, stop_thread)
        });
        Self {
            gate,
            stop,
            thread: Some(thread),
            address,
        }
    }
}

impl Drop for Membership {
    fn drop(&mut self) {
        self.gate.release();
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve_membership(
    listener: TcpListener,
    signer: Ed25519KeyPair,
    jwks: Vec<u8>,
    gate: Arc<Gate>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::SeqCst) {
        let (mut stream, _) = match listener.accept() {
            Ok(pair) => pair,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(2));
                continue;
            }
            Err(_) => break,
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request = String::new();
        if reader.read_line(&mut request).is_err() {
            continue;
        }
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                if key.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
        }
        let mut body = vec![0; length];
        if length > 0 && reader.read_exact(&mut body).is_err() {
            continue;
        }
        let bytes = if request.starts_with("POST /v1/runtime/cadence/member/check ") {
            gate.arrive();
            proof_envelope(&signer, &body)
        } else if request.starts_with("GET /.well-known/agenticos-board-jwks.json ") {
            jwks.clone()
        } else {
            continue;
        };
        let header = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            bytes.len()
        );
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.write_all(&bytes);
    }
}

fn proof_envelope(signer: &Ed25519KeyPair, body: &[u8]) -> Vec<u8> {
    let request: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let claims = json!({
        "purpose": "cadence-board-membership/1",
        "iss": "http://api.internal",
        "aud": COMPANY,
        "instance": INSTANCE,
        "sub": request["sub"].as_str().unwrap_or(""),
        "session": request["session"].as_str().unwrap_or(""),
        "nonce": request["nonce"].as_str().unwrap_or(""),
        "role": "owner",
        "iat": now,
        "exp": now + 4,
        "jti": format!("cad1459-{now}-{}", PROOF_SEQ.fetch_add(1, Ordering::SeqCst))
    });
    let header = URL_SAFE_NO_PAD.encode(format!(r#"{{"alg":"EdDSA","typ":"JWT","kid":"{KID}"}}"#));
    let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
    let signed = format!("{header}.{payload}");
    let signature = URL_SAFE_NO_PAD.encode(signer.sign(signed.as_bytes()).as_ref());
    json!({"ok":true,"data":{"proof":format!("{signed}.{signature}")}})
        .to_string()
        .into_bytes()
}

fn public_token(fx: &Fx, case: usize) -> String {
    let now = fx.shared.operator_now();
    let config = crate::board_identity::read_config(&fx.state).unwrap();
    fx.shared
        .operator_auth()
        .open_public_bound(
            BoardUser {
                sub: format!("usr-cad1459-{case}"),
                email: format!("usr-cad1459-{case}@example.test"),
                name: "Cadence Public".into(),
                role: "operator".into(),
                handle: format!("cad1459{case}"),
            },
            &format!("jti-cad1459-{case}-{}", std::process::id()),
            now + 60,
            "cad1459-public-membership",
            crate::operator_auth::PublicRealm {
                issuer: config.issuer,
                company: config.company,
                host: config.host,
            },
            now,
        )
        .unwrap()
        .expect("fresh public session")
        .token
}

fn mount_public(fx: &Fx, token: &str) -> String {
    let mint = fx
        .op(
            "app_screen_mint",
            json!({"install_id": fx.install, "context_id": fx.context,
                "tag": "media", "token": token, "key": "",
                "origin": "public", "generation": 1}),
        )
        .expect("public operator mint");
    let nonce = mint["mount"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();
    fx.op("app_screen_consume", json!({"nonce": nonce}))
        .unwrap();
    mint["action_token"].as_str().unwrap().to_string()
}

fn public_board(fx: &Fx) -> (u16, JoinHandle<()>) {
    let state = fx.state.clone();
    let pm = state.parent().unwrap().join("pm");
    let stop = fx.stop.clone();
    let first = (std::process::id() % 80) as u16;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut offset = 0u16;
    loop {
        assert!(Instant::now() < deadline, "public board startup deadline");
        let port = 3110 + (first + offset) % 80;
        let (startup, ready) = mpsc::channel();
        let board = crate::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(stop.clone()),
            startup: Some(startup),
            test_seam: true,
            public: Some(crate::ui::PublicBoard {
                host: PUBLIC_HOST.into(),
                issuer: "https://platform.example.test".into(),
                company: COMPANY.into(),
                authorize_url: "https://platform.example.test/authorize".into(),
            }),
            allow_hosts: vec![PUBLIC_HOST.into()],
            allow_origins: vec![format!("https://{PUBLIC_HOST}")],
            ..Default::default()
        };
        let (s, p) = (state.clone(), pm.clone());
        let thread = thread::spawn(move || drop(crate::ui::serve(&s, &p, &board)));
        match ready.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Ok(())) => return (port, thread),
            Ok(Err(std::io::ErrorKind::AddrInUse)) => {
                assert!(thread.join().is_ok());
                offset += 1;
            }
            other => panic!("public board did not start: {other:?}"),
        }
    }
}

fn http_public(
    port: u16,
    token: &str,
    action: &str,
    asset: &str,
) -> (u16, String, String, Vec<u8>) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = agent
        .post(format!("http://127.0.0.1:{port}/api/app-media/original"))
        .header("Host", PUBLIC_HOST)
        .header("Origin", format!("https://{PUBLIC_HOST}"))
        .header("X-Cadence-Board", "1")
        .header("Cookie", format!("aos-board-session={token}"))
        .header("Content-Type", "application/json")
        .send(json!({"action_token": action, "asset_id": asset}).to_string())
        .unwrap();
    let status = response.status().as_u16();
    let ctype = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let disposition = response
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(ORIGINAL_LIMIT as u64 + 4096)
        .read_to_end(&mut bytes)
        .unwrap();
    (status, ctype, disposition, bytes)
}

struct Seen {
    success: bool,
    bytes: Vec<u8>,
}

fn fetch_original(
    fx: &Fx,
    mode: &str,
    port: u16,
    action: &str,
    token: &str,
    trust: Option<Fixture>,
) -> Seen {
    let params = json!({"action_token": action, "token": token, "key": "",
        "origin": "public", "asset_id": fx.asset_id});
    match mode {
        "callback" => {
            let _tls = trust.map(test_fixture::install);
            let mut seen = Seen {
                success: false,
                bytes: Vec::new(),
            };
            let result = scoped(Asserted::Operator, || {
                fx.shared.rpc_app_media_original_stream(
                    &params,
                    std::process::id(),
                    |header, bytes, _| {
                        seen.success = header["transfer"] == "app-media-original/1";
                        seen.bytes = bytes.to_vec();
                        Ok(())
                    },
                )
            });
            if result.is_err() {
                seen.success = false;
                seen.bytes.clear();
            }
            seen
        }
        "native" => {
            let (reader, line) =
                socket_call(fx, Asserted::Operator, "app_media_original", params).unwrap();
            let frame: Value = serde_json::from_str(&line).unwrap();
            let header = frame["result"]["transfer"] == "app-media-original/1";
            reader
                .get_ref()
                .set_read_timeout(Some(Duration::from_secs(8)))
                .unwrap();
            let mut bytes = Vec::new();
            let _ = reader
                .take(ORIGINAL_LIMIT as u64 + 1)
                .read_to_end(&mut bytes);
            Seen {
                success: header && bytes == fx.original,
                bytes: if header { bytes } else { Vec::new() },
            }
        }
        "http" => {
            let (status, ctype, disposition, bytes) =
                http_public(port, token, action, &fx.asset_id);
            let success = status == 200;
            if success {
                assert_eq!(ctype, "image/png");
                assert!(disposition.contains("attachment"));
            } else {
                assert!((400..500).contains(&status), "HTTP refusal status {status}");
                assert!(!ctype.starts_with("image/"));
                assert!(!disposition.contains("attachment"));
                let error: Value = serde_json::from_slice(&bytes).expect("HTTP error JSON");
                assert!(error.is_object());
            }
            Seen {
                success,
                bytes: if success { bytes } else { Vec::new() },
            }
        }
        _ => unreachable!(),
    }
}

fn control_while_paused(fx: &Fx) -> bool {
    let (done, reply) = mpsc::channel();
    let control = fx.shared.clone();
    let install = fx.install.clone();
    let state = fx.state.clone();
    thread::spawn(move || {
        let pm = control.pm_at(&control.pm_dir().unwrap()).unwrap();
        let locked =
            crate::issue::app_catalog::workspace::with_runtime_snapshot(&pm, &install, |_, _| {
                let _custody = control.platform_custody_lock.lock().unwrap();
                let _release = control.app_release_lock.lock().unwrap();
                assert_eq!(
                    crate::platform::account_media::identities().unwrap().len(),
                    1
                );
                Ok(())
            });
        if locked.is_ok() {
            let shown = scoped(Asserted::Operator, || {
                crate::client::rpc(
                    &state,
                    "app_workspace_show",
                    json!({"install_id": &install}),
                )
            })
            .unwrap();
            let digest = shown["digest"]
                .as_str()
                .or_else(|| shown["install"]["digest"].as_str())
                .unwrap()
                .to_string();
            let result = scoped(Asserted::Operator, || {
                crate::client::rpc(
                    &state,
                    "app_local_install_revoke",
                    json!({"install_id": &install, "digest": digest}),
                )
            })
            .unwrap();
            assert_eq!(result["approved"], false);
        }
        let _ = done.send(locked.is_ok());
    });
    reply
        .recv_timeout(Duration::from_millis(1500))
        .unwrap_or(false)
}

#[test]
fn cad1459_public_membership_original_refuses_without_holding_guards() {
    let Some(root) = std::env::var_os(CHILD).map(PathBuf::from) else {
        let root = tempfile::Builder::new()
            .prefix("c1459p-")
            .tempdir()
            .unwrap();
        for name in ["home", "config", "data", "state", "tmp", "locks"] {
            login::private_dir(&root.path().join(name));
        }
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--test-threads", "1"])
            .env(CHILD, root.path())
            .env("HOME", root.path().join("home"))
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("XDG_STATE_HOME", root.path().join("state"))
            .env("TMPDIR", root.path().join("tmp"))
            .env("CADENCE_SUITE_LOCK", root.path().join("locks/suite"))
            .stdin(Stdio::null())
            .status()
            .unwrap();
        assert!(
            status.success(),
            "isolated public-membership original acceptance failed"
        );
        return;
    };
    let auth = root.join("config/cadence/remote-auth");
    login::private_dir(&auth);
    login::private_write(
        &auth.join("trusted-issuer"),
        b"https://issuer.board.localhost",
    );
    login::save_login(&auth, &login::normal_login());
    login::select(&root, "lc");
    let gate = Gate::new();
    let membership = Membership::start(gate.clone());

    // Membership POSTs on one fresh public original, counted only for
    // POST /v1/runtime/cadence/member/check and reset by each arm():
    // 1 tool_session current_public_session, before the artifact GET.
    // 2 tool_context_proven public_row_hash, before the artifact GET.
    // 3 and 4 are both post-GET original_public_fresh: current_public_session,
    //    then public_row_hash. Neither is the chunk path.
    // 5 is the first OriginalDelivery::with_current current_public_session,
    //    the header chunk. Later body chunks repeat that pair.
    for (case, (mode, pause_at)) in [
        ("callback", 3usize),
        ("native", 3usize),
        ("http", 5usize),
        ("native", 5usize),
    ]
    .into_iter()
    .enumerate()
    {
        let original_bytes = noisy_square_png();
        let digest = format!("{:x}", Sha256::digest(&original_bytes));
        let artifact = json!({"ref": format!("{JOB_ID}.{digest}"), "digest": digest,
            "bytes": original_bytes.len(), "mime": "image/png"});
        let price = json!({"slug":"generate_image","chargeMinor":31_500,"currency":"USD","version":"2026-09-29T00:00:00.000Z"});
        let gets = Arc::new(AtomicUsize::new(0));
        let gets_reply = gets.clone();
        let provider_bytes = original_bytes.clone();
        let pki = root.join(format!("wire-public-{case}"));
        login::private_dir(&pki);
        let wire = login::Wire::media(
            &pki,
            Arc::new(move |request| {
                let body = if request == "GET /__platform/media/price/image HTTP/1.1\r\n" {
                    json!({"ok":true,"data":{"kind":"image","model":"openai/gpt-image-2.5","price":price}})
                } else if request == "POST /__platform/media/image HTTP/1.1\r\n" {
                    json!({"ok":true,"data":{"job":{"id":JOB_ID,"kind":"image","status":"succeeded",
                        "model":"openai/gpt-image-2.5","provider":"upstream-fixture","providerTaskId":null,
                        "artifacts":[artifact],"artifactError":null,"usage":{"providerCredits":null},
                        "price":price,"repeated":false,"cached":false,"stale":false}}})
                } else if request.starts_with("GET /__platform/media/artifacts/") {
                    gets_reply.fetch_add(1, Ordering::SeqCst);
                    return ("image/png".into(), provider_bytes.clone());
                } else {
                    panic!("unexpected synthetic account media operation");
                };
                ("application/json".into(), body.to_string().into_bytes())
            }),
        );
        // Membership POST and JWKS both use issuer_transport. The daemon thread
        // has no thread-local TLS fixture, so this plain route is what it resolves.
        let _plain = test_fixture::route_plain_http(
            wire.trust().loopback,
            "api.internal",
            membership.address,
        );
        let fx = Arc::new(Fx::boot_configured(
            original_bytes,
            None,
            true,
            Some(wire.trust()),
            true,
            false,
        ));
        assert!(
            matches!(
                fx.shared.membership_mode,
                crate::daemon::public_membership::MembershipMode::Native(_)
            ),
            "daemon must be natively hosted"
        );
        let token = public_token(&fx, case);
        let action = mount_public(&fx, &token);
        let (port, board) = if mode == "http" {
            let (port, board) = public_board(&fx);
            (port, Some(board))
        } else {
            (0, None)
        };
        gate.arm(0);
        let positive = fetch_original(&fx, mode, port, &action, &token, Some(wire.trust()));
        assert!(
            positive.success,
            "{mode}/{pause_at}: positive public original"
        );
        assert_eq!(positive.bytes, fx.original);
        let before_gets = gets.load(Ordering::SeqCst);

        gate.arm(pause_at);
        let mut pending = Pending {
            gate: gate.clone(),
            threads: Vec::new(),
        };
        let (done, reply) = mpsc::channel();
        let (client, client_action, client_token, client_trust) =
            (fx.clone(), action.clone(), token.clone(), wire.trust());
        pending.threads.push(thread::spawn(move || {
            let seen = fetch_original(
                &client,
                mode,
                port,
                &client_action,
                &client_token,
                Some(client_trust),
            );
            let _ = done.send(seen);
        }));
        let paused = gate.reached();
        if pause_at > 1 && !paused {
            drop(pending);
            let finished = reply.recv_timeout(Duration::from_secs(5)).ok();
            assert!(
                finished.is_some_and(|seen| seen.success && seen.bytes == fx.original),
                "{mode}: delivery made no further membership HTTP, so the approved original still completes"
            );
            stop_board(&fx, board);
            continue;
        }
        assert!(
            paused,
            "{mode}/{pause_at}: membership HTTP must reach the pre-response pause"
        );
        assert!(
            control_while_paused(&fx),
            "{mode}/{pause_at}: runtime/custody/release and revocation must finish while membership HTTP is paused"
        );
        gate.release();
        drop(pending);
        let refused = reply
            .recv_timeout(Duration::from_secs(8))
            .expect("original request completes after membership resume");
        assert!(
            !refused.success,
            "{mode}/{pause_at}: revoked public original must refuse"
        );
        assert!(
            refused.bytes != fx.original,
            "{mode}/{pause_at}: no full original bytes after revocation"
        );
        assert_eq!(
            gets.load(Ordering::SeqCst),
            before_gets + 1,
            "{mode}/{pause_at}: one artifact GET and no retry"
        );
        stop_board(&fx, board);
    }
}

fn stop_board(fx: &Fx, board: Option<JoinHandle<()>>) {
    fx.stop.store(true, Ordering::SeqCst);
    if let Some(board) = board {
        let _ = board.join();
    }
}
