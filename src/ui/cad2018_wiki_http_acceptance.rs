//! CAD-2018: signed public-board sessions exercise recovery through the real
//! HTTP board and daemon, not a hand-inserted session or a mocked route.

use super::*;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tiny_http::{Header, Response, Server};

const ORG: &str = "cad2018-synthetic-org";
const KID: &str = "cad2018-synthetic-key";

struct Rig {
    _dir: tempfile::TempDir,
    board_port: u16,
    public_host: String,
    issuer: String,
    key: Ed25519KeyPair,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// Keep SIGCHLD from waking the client's socket reads. The daemon and board
/// threads this rig starts spawn `git`. A child that exits while its parent
/// thread has signals blocked (as `Command::spawn` does across the fork) has
/// the SIGCHLD queued on the process instead of dropped as ignored, and the
/// kernel then wakes any thread that does not block it. When that is the thread
/// in a timed `recv`, the call fails with EINTR: a socket with a receive
/// timeout is never restarted, whatever the signal's disposition, and ureq 3.4.2
/// does not retry it. Blocking it here is inherited by every thread `start`
/// spawns; `wait`/`waitpid` do not need it delivered.
fn block_sigchld() {
    // SAFETY: a local signal set and a mask change on the calling thread.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGCHLD);
        assert_eq!(
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()),
            0
        );
    }
}

fn start() -> Rig {
    block_sigchld();
    let dir = tempfile::Builder::new()
        .prefix("c2018-http")
        .tempdir()
        .unwrap();
    let state = dir.path().to_path_buf();
    let pm = state.join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();

    let opts = crate::daemon::ServeOptions {
        test_seam: true,
        stop: Some(stop.clone()),
        ..crate::daemon::ServeOptions::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let daemon_state = state.clone();
    threads.push(std::thread::spawn(move || {
        crate::daemon::serve_with(&daemon_state, opts).unwrap()
    }));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !state.join("cadence.sock").exists() || crate::test_seam::Seam::token_at(&state).is_none()
    {
        assert!(Instant::now() < deadline, "synthetic daemon did not start");
        std::thread::sleep(Duration::from_millis(50));
    }

    let key = Ed25519KeyPair::from_seed_unchecked(&[201; 32]).unwrap();
    let public_key = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
    let platform = (3110..=3199)
        .find_map(|port| Server::http(("127.0.0.1", port)).ok())
        .expect("no free synthetic platform port");
    let platform_port = match platform.server_addr() {
        tiny_http::ListenAddr::IP(addr) => addr.port(),
        _ => panic!("synthetic platform did not bind TCP"),
    };
    let public_host = format!("localhost:{platform_port}");
    let issuer = format!("http://{public_host}");
    let platform_stop = stop.clone();
    threads.push(std::thread::spawn(move || {
        while !platform_stop.load(Ordering::SeqCst) {
            if let Ok(Some(request)) = platform.recv_timeout(Duration::from_millis(100)) {
                let valid = request.url() == "/.well-known/agenticos-board-jwks.json";
                let body = if valid {
                    json!({"keys":[{"kty":"OKP","crv":"Ed25519","kid":KID,"x":public_key}]})
                } else {
                    json!({"error":"not_found"})
                };
                let response = Response::from_string(body.to_string())
                    .with_status_code(if valid { 200 } else { 404 })
                    .with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
                let _ = request.respond(response);
            }
        }
    }));

    let mut board_port = 0;
    for port in 3110..=3199 {
        if port == platform_port {
            continue;
        }
        let (startup, ready) = std::sync::mpsc::channel();
        let board = ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(stop.clone()),
            startup: Some(startup),
            test_seam: true,
            allow_hosts: vec![public_host.clone()],
            public: Some(PublicBoard {
                host: public_host.clone(),
                issuer: issuer.clone(),
                company: ORG.into(),
                authorize_url: format!("{issuer}/authorize"),
            }),
            ..ServeOpts::default()
        };
        let (board_state, board_pm) = (state.clone(), pm.clone());
        let thread =
            std::thread::spawn(move || drop(crate::ui::serve(&board_state, &board_pm, &board)));
        match ready.recv_timeout(Duration::from_secs(30)).unwrap() {
            Ok(()) => {
                board_port = port;
                threads.push(thread);
                break;
            }
            Err(_) => {
                thread.join().unwrap();
            }
        }
    }
    assert!(board_port > 0, "synthetic board could not bind");
    Rig {
        _dir: dir,
        board_port,
        public_host,
        issuer,
        key,
        stop,
        threads,
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            .build(),
    )
}

fn post(
    rig: &Rig,
    path: &str,
    cookie: Option<&str>,
    body: &Value,
) -> ureq::http::Response<ureq::Body> {
    let url = format!("http://127.0.0.1:{}{path}", rig.board_port);
    let mut request = agent()
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("X-Cadence-Board", "1");
    if let Some(cookie) = cookie {
        request = request.header("Cookie", cookie);
    }
    request.send_json(body).expect("real board POST failed")
}

fn get(rig: &Rig, path: &str, cookie: Option<&str>) -> ureq::http::Response<ureq::Body> {
    let url = format!("http://127.0.0.1:{}{path}", rig.board_port);
    let mut request = agent().get(&url).header("Host", &rig.public_host);
    if let Some(cookie) = cookie {
        request = request.header("Cookie", cookie);
    }
    request.call().expect("real board GET failed")
}

fn put(rig: &Rig, path: &str, cookie: &str, body: &Value) -> ureq::http::Response<ureq::Body> {
    let url = format!("http://127.0.0.1:{}{path}", rig.board_port);
    agent()
        .put(&url)
        .header("Content-Type", "application/json")
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("X-Cadence-Board", "1")
        .header("Cookie", cookie)
        .send_json(body)
        .expect("real board PUT failed")
}

fn session(rig: &Rig, sub: &str, role: &str) -> String {
    let now = crate::issue::time::now_epoch();
    let header = json!({"alg":"EdDSA","typ":"JWT","kid":KID});
    let claims = json!({
        "iss": rig.issuer, "aud": rig.public_host, "company": ORG,
        "sub": sub, "email": format!("{sub}@example.test"), "name": sub,
        "role": role, "iat": now, "exp": now + 60,
        "jti": format!("cad2018-{sub}-{}", uuid::Uuid::new_v4()),
    });
    let signed = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap()),
    );
    let assertion = format!(
        "{signed}.{}",
        URL_SAFE_NO_PAD.encode(rig.key.sign(signed.as_bytes()).as_ref()),
    );
    let response = post(
        rig,
        "/__platform/session",
        None,
        &json!({"assertion": assertion}),
    );
    assert_eq!(
        response.status().as_u16(),
        200,
        "signed fixture SSO was refused"
    );
    response
        .headers()
        .get("set-cookie")
        .expect("public session has no cookie")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

#[test]
fn signed_public_owner_restores_through_http_and_member_is_refused() {
    let rig = start();
    let owner = session(&rig, "owner", "owner");
    let member = session(&rig, "member", "member");
    let page = "global/http-recovery.md";

    let created = put(
        &rig,
        "/api/wiki/file",
        &owner,
        &json!({"path": page, "text": "synthetic retained text"}),
    );
    assert_eq!(
        created.status().as_u16(),
        200,
        "public owner could not create the control page"
    );
    let mut removed = post(&rig, "/api/wiki/rm", Some(&owner), &json!({"path": page}));
    assert_eq!(
        removed.status().as_u16(),
        200,
        "public owner could not remove the control page"
    );
    let removed: Value = removed.body_mut().read_json().unwrap();
    let from = removed["trash"]
        .as_str()
        .expect("delete returned no landing");

    assert_eq!(
        get(&rig, "/api/wiki/trash", Some(&member))
            .status()
            .as_u16(),
        403
    );
    assert_eq!(
        get(
            &rig,
            &format!("/api/wiki/trash/items?from={from}"),
            Some(&member)
        )
        .status()
        .as_u16(),
        403
    );
    assert_eq!(
        get(&rig, "/api/wiki/trash?wiki_as=operator", Some(&member))
            .status()
            .as_u16(),
        403
    );
    let denied = post(
        &rig,
        "/api/wiki/mv",
        Some(&member),
        &json!({"from": from, "to": page, "role": "operator"}),
    );
    assert!(
        !denied.status().is_success(),
        "member restored a landing by claiming an operator role"
    );

    let mut listed = get(&rig, "/api/wiki/trash", Some(&owner));
    assert_eq!(listed.status().as_u16(), 200);
    let listed: Value = listed.body_mut().read_json().unwrap();
    assert!(listed["landings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["from"] == from));
    let mut items = get(
        &rig,
        &format!("/api/wiki/trash/items?from={from}"),
        Some(&owner),
    );
    assert_eq!(items.status().as_u16(), 200);
    let items: Value = items.body_mut().read_json().unwrap();
    assert_eq!(items["entries"][0]["path"], page);
    assert_eq!(
        get(&rig, "/api/wiki/trash?wiki_as=operator", Some(&owner))
            .status()
            .as_u16(),
        400
    );

    let mut restored = post(
        &rig,
        "/api/wiki/mv",
        Some(&owner),
        &json!({"from": from, "to": page}),
    );
    assert_eq!(
        restored.status().as_u16(),
        200,
        "public owner restore was refused"
    );
    let restored: Value = restored.body_mut().read_json().unwrap();
    assert_eq!(restored["moved"], true);
    let mut read = get(&rig, &format!("/api/wiki/file?path={page}"), Some(&owner));
    assert_eq!(read.status().as_u16(), 200);
    let read: Value = read.body_mut().read_json().unwrap();
    assert_eq!(read["text"], "synthetic retained text");
}
