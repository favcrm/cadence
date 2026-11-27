//! CAD-2859 independent signed-board HTTP acceptance for native `POST /api/projects`:
//! 1. Enforces board write guards (`Content-Type: application/json`,
//!    `X-Cadence-Board: 1`, same-origin `Origin` / `Sec-Fetch-Site`).
//! 2. Refuses unauthenticated and non-operator / workspace member callers with
//!    HTTP 403 and writes nothing on disk or in `GET /api/projects`.
//! 3. Validates input fields, rejects unknown/forged identity fields (`actor`,
//!    `operator`, etc.), blank `name`, invalid `key`, invalid `prefix`,
//!    invalid `preset`, and invalid `visibility` with HTTP 400 structured JSON
//!    errors and no side effects.
//! 4. Creates projects for operator/owner callers with `201 Created`, makes them
//!    visible via `GET /api/projects`, and refuses duplicate `key`
//!    (`409 project_exists`) and conflicting `prefix` (`409 prefix_conflict`).

#![cfg(all(test, feature = "test-seam"))]

use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tiny_http::{Header, Response, Server};

const ORG: &str = "cad2859-synthetic-org";
const KID: &str = "cad2859-synthetic-key";
const OWNER: &str = "cad2859-owner";
const MEMBER: &str = "cad2859-member";

struct Rig {
    _dir: tempfile::TempDir,
    pm: PathBuf,
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

fn start() -> Rig {
    block_sigchld();
    let dir = tempfile::Builder::new()
        .prefix("c2859-project-create")
        .tempdir()
        .unwrap();
    let state = dir.path().to_path_buf();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    let pm = state.join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    std::fs::set_permissions(&pm, std::fs::Permissions::from_mode(0o700)).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();

    let key = Ed25519KeyPair::from_seed_unchecked(&[241; 32]).unwrap();
    let public_key = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
    let platform = (3110..=3199)
        .find_map(|port| Server::http(("127.0.0.1", port)).ok())
        .expect("no free synthetic platform port in 3110..=3199");
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
                let path = request.url().split('?').next().unwrap_or("");
                let (status_code, body) = if path == "/.well-known/agenticos-board-jwks.json" {
                    (
                        200,
                        json!({"keys":[{"kty":"OKP","crv":"Ed25519","kid":KID,"x":public_key}]}),
                    )
                } else {
                    (404, json!({"ok": false, "error": {"code": "not_found"}}))
                };
                let response = Response::from_string(body.to_string())
                    .with_status_code(status_code)
                    .with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
                let _ = request.respond(response);
            }
        }
    }));

    let opts = crate::daemon::ServeOptions {
        test_seam: true,
        stop: Some(stop.clone()),
        checkup: Some(0),
        provider_deployments: Some(
            crate::platform::deployments::DeploymentMetadata::parse(
                br#"{"schema":1,"providers":[]}"#,
            )
            .expect("isolated deployment metadata"),
        ),
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
        pm,
        board_port,
        public_host,
        issuer,
        key,
        stop,
        threads,
    }
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            .build(),
    )
}

fn login(rig: &Rig, sub: &str, role: &str) -> String {
    let now = crate::issue::time::now_epoch();
    let header = json!({"alg": "EdDSA", "typ": "JWT", "kid": KID});
    let claims = json!({
        "iss": rig.issuer,
        "aud": rig.public_host,
        "company": ORG,
        "sub": sub,
        "email": format!("{sub}@cad2859.example.test"),
        "name": sub,
        "role": role,
        "iat": now,
        "exp": now + 60,
        "jti": format!("cad2859-{sub}-{}", uuid::Uuid::new_v4()),
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
    let url = format!("http://127.0.0.1:{}/__platform/session", rig.board_port);
    let response = http_agent()
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("X-Cadence-Board", "1")
        .send_json(json!({"assertion": assertion}))
        .expect("board session exchange failed");
    assert_eq!(
        response.status().as_u16(),
        200,
        "signed {role} login refused"
    );
    response
        .headers()
        .get("set-cookie")
        .expect("session exchange did not set a cookie")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

fn get_json(rig: &Rig, cookie: &str, path: &str) -> (u16, Value) {
    let url = format!("http://127.0.0.1:{}{path}", rig.board_port);
    let mut response = http_agent()
        .get(&url)
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("Cookie", cookie)
        .call()
        .expect("board GET failed");
    let status = response.status().as_u16();
    let raw = response.body_mut().read_to_string().unwrap_or_default();
    let parsed = serde_json::from_str(&raw).unwrap_or_else(|_| json!({"raw": raw}));
    (status, parsed)
}

struct PostHeaders<'a> {
    cookie: Option<&'a str>,
    content_type: Option<&'a str>,
    board_header: Option<&'a str>,
    origin: Option<&'a str>,
    sec_fetch_site: Option<&'a str>,
}

impl<'a> PostHeaders<'a> {
    fn valid(rig: &'a Rig, cookie: Option<&'a str>) -> (Self, String) {
        let same_origin = format!("http://{}", rig.public_host);
        (
            Self {
                cookie,
                content_type: Some("application/json"),
                board_header: Some("1"),
                origin: None,
                sec_fetch_site: Some("same-origin"),
            },
            same_origin,
        )
    }
}

fn post_raw(
    rig: &Rig,
    path: &str,
    headers: &PostHeaders<'_>,
    default_origin: &str,
    body: &str,
) -> (u16, Value) {
    let url = format!("http://127.0.0.1:{}{path}", rig.board_port);
    let mut req = http_agent().post(&url).header("Host", &rig.public_host);
    if let Some(ct) = headers.content_type {
        req = req.header("Content-Type", ct);
    }
    if let Some(board) = headers.board_header {
        req = req.header("X-Cadence-Board", board);
    }
    req = req.header("Origin", headers.origin.unwrap_or(default_origin));
    if let Some(sfs) = headers.sec_fetch_site {
        req = req.header("Sec-Fetch-Site", sfs);
    }
    if let Some(cookie) = headers.cookie {
        req = req.header("Cookie", cookie);
    }
    let mut response = req.send(body).expect("board POST failed");
    let status = response.status().as_u16();
    let raw = response.body_mut().read_to_string().unwrap_or_default();
    let parsed = serde_json::from_str(&raw).unwrap_or_else(|_| json!({"raw": raw}));
    (status, parsed)
}

fn post_json(rig: &Rig, cookie: Option<&str>, body: &Value) -> (u16, Value) {
    let (headers, origin) = PostHeaders::valid(rig, cookie);
    post_raw(rig, "/api/projects", &headers, &origin, &body.to_string())
}

fn tracker_snapshot(root: &std::path::Path) -> BTreeMap<String, Vec<u8>> {
    let pm = crate::issue::Pm::at(root).unwrap();
    let guard = pm
        .lock_for(Duration::from_secs(30))
        .expect("tracker write lock never quiesced");
    fn walk(base: &std::path::Path, dir: &std::path::Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if rel == ".git"
                || rel == ".write.lock"
                || rel.starts_with(".index/")
                || rel == ".index"
                || rel.starts_with(".cadence/")
                || rel == ".cadence"
            {
                continue;
            }
            if path.is_dir() {
                out.insert(format!("{rel}/"), vec![]);
                walk(base, &path, out);
            } else {
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    drop(guard);
    out
}

fn project_keys(rig: &Rig, cookie: &str) -> Vec<String> {
    let (status, body) = get_json(rig, cookie, "/api/projects");
    assert_eq!(status, 200, "GET /api/projects failed: {body}");
    body["projects"]
        .as_array()
        .expect("GET /api/projects missing projects array")
        .iter()
        .filter_map(|p| p["key"].as_str().map(str::to_string))
        .collect()
}

fn assert_structured_error(body: &Value, context: &str) {
    let obj = body
        .as_object()
        .unwrap_or_else(|| panic!("{context}: expected JSON object error, got {body}"));
    let has_message = obj
        .get("error")
        .and_then(|v| {
            v.as_str()
                .or_else(|| v.get("message").and_then(Value::as_str))
        })
        .is_some_and(|s| !s.is_empty());
    let has_code = obj
        .get("code")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    assert!(
        has_message || has_code,
        "{context}: expected structured error JSON with error/code, got {body}"
    );
}

#[test]
fn cad2859_post_projects_enforces_write_guards_and_operator_auth() {
    let rig = start();
    let owner = login(&rig, OWNER, "owner");
    let member = login(&rig, MEMBER, "member");

    let before_disk = tracker_snapshot(&rig.pm);
    assert!(
        project_keys(&rig, &owner).is_empty(),
        "expected clean initial project list"
    );

    let payload = json!({
        "name": "Guarded Project",
        "key": "guarded-proj",
        "prefix": "GRD",
    })
    .to_string();

    // Missing X-Cadence-Board: 1 must be refused with 403.
    {
        let (mut headers, origin) = PostHeaders::valid(&rig, Some(&owner));
        headers.board_header = None;
        let (status, body) = post_raw(&rig, "/api/projects", &headers, &origin, &payload);
        assert_eq!(
            status, 403,
            "missing X-Cadence-Board must return 403: {body}"
        );
        assert_eq!(tracker_snapshot(&rig.pm), before_disk);
        assert!(project_keys(&rig, &owner).is_empty());
    }

    // Cross-site Origin must be refused with 403.
    {
        let (mut headers, origin) = PostHeaders::valid(&rig, Some(&owner));
        headers.origin = Some("https://evil.example");
        let (status, body) = post_raw(&rig, "/api/projects", &headers, &origin, &payload);
        assert_eq!(status, 403, "cross-site Origin must return 403: {body}");
        assert_eq!(tracker_snapshot(&rig.pm), before_disk);
        assert!(project_keys(&rig, &owner).is_empty());
    }

    // Cross-site Sec-Fetch-Site must be refused with 403.
    {
        let (mut headers, origin) = PostHeaders::valid(&rig, Some(&owner));
        headers.sec_fetch_site = Some("cross-site");
        let (status, body) = post_raw(&rig, "/api/projects", &headers, &origin, &payload);
        assert_eq!(
            status, 403,
            "cross-site Sec-Fetch-Site must return 403: {body}"
        );
        assert_eq!(tracker_snapshot(&rig.pm), before_disk);
        assert!(project_keys(&rig, &owner).is_empty());
    }

    // Non-JSON Content-Type must be refused by the write guard.
    {
        let (mut headers, origin) = PostHeaders::valid(&rig, Some(&owner));
        headers.content_type = Some("text/plain");
        let (status, body) = post_raw(&rig, "/api/projects", &headers, &origin, &payload);
        assert!(
            matches!(status, 403 | 415),
            "non-JSON Content-Type must be refused (403/415), got {status}: {body}"
        );
        assert_eq!(tracker_snapshot(&rig.pm), before_disk);
        assert!(project_keys(&rig, &owner).is_empty());
    }

    // Unauthenticated caller must be refused with 403.
    {
        let (status, body) = post_json(
            &rig,
            None,
            &json!({"name": "Anon Proj", "key": "anon-proj", "prefix": "ANON"}),
        );
        assert_eq!(
            status, 403,
            "unauthenticated caller must return 403: {body}"
        );
        assert_eq!(tracker_snapshot(&rig.pm), before_disk);
        assert!(project_keys(&rig, &owner).is_empty());
    }

    // Non-operator workspace member caller must be refused with 403.
    {
        let (status, body) = post_json(
            &rig,
            Some(&member),
            &json!({"name": "Member Proj", "key": "member-proj", "prefix": "MEM"}),
        );
        assert_eq!(status, 403, "member caller must return 403: {body}");
        assert_eq!(tracker_snapshot(&rig.pm), before_disk);
        assert!(project_keys(&rig, &owner).is_empty());
    }
}

#[test]
fn cad2859_post_projects_validates_inputs_and_rejects_unknown_fields() {
    let rig = start();
    let owner = login(&rig, OWNER, "owner");
    let before_disk = tracker_snapshot(&rig.pm);

    let invalid_cases = [
        (
            "forged actor field",
            json!({"name": "Demo", "actor": "operator"}),
        ),
        (
            "forged operator bool",
            json!({"name": "Demo", "operator": true}),
        ),
        (
            "forged principal field",
            json!({"name": "Demo", "principal": "operator"}),
        ),
        (
            "unknown extra field",
            json!({"name": "Demo", "unexpected": "value"}),
        ),
        ("blank empty name", json!({"name": ""})),
        ("blank whitespace name", json!({"name": "   \t  "})),
        (
            "invalid key uppercase/symbols",
            json!({"name": "Demo", "key": "Bad_Key!"}),
        ),
        (
            "reserved key agents",
            json!({"name": "Demo", "key": "agents"}),
        ),
        (
            "invalid prefix leading digit",
            json!({"name": "Demo", "key": "demo-prefix", "prefix": "1BAD"}),
        ),
        (
            "invalid prefix lowercase",
            json!({"name": "Demo", "key": "demo-prefix2", "prefix": "abc"}),
        ),
        (
            "invalid prefix too long",
            json!({"name": "Demo", "key": "demo-prefix3", "prefix": "ABCDEFGHI"}),
        ),
        (
            "invalid preset",
            json!({"name": "Demo", "key": "demo-preset", "preset": "not_a_preset"}),
        ),
        (
            "invalid visibility",
            json!({"name": "Demo", "key": "demo-vis", "visibility": "public"}),
        ),
    ];

    for (label, body) in invalid_cases {
        let (status, resp) = post_json(&rig, Some(&owner), &body);
        assert_eq!(status, 400, "{label} must return 400, got {status}: {resp}");
        assert_structured_error(&resp, label);
        assert_eq!(
            tracker_snapshot(&rig.pm),
            before_disk,
            "{label} mutated tracker disk state"
        );
        assert!(
            project_keys(&rig, &owner).is_empty(),
            "{label} appeared in GET /api/projects"
        );
    }
}

#[test]
fn cad2859_post_projects_creates_projects_and_refuses_conflicts() {
    let rig = start();
    let owner = login(&rig, OWNER, "owner");
    let member = login(&rig, MEMBER, "member");

    // Valid repo-less creation with explicit key, prefix, preset, and visibility.
    let (status, created) = post_json(
        &rig,
        Some(&owner),
        &json!({
            "name": "Alpha Launch",
            "key": "alpha-launch",
            "prefix": "ALPH",
            "preset": "software",
            "visibility": "workspace-visible",
        }),
    );
    assert_eq!(status, 201, "owner create failed: {created}");
    assert_eq!(created["project"]["key"], "alpha-launch", "{created}");
    assert_eq!(created["project"]["prefix"], "ALPH", "{created}");
    assert_eq!(created["project"]["preset"], "software", "{created}");
    assert!(
        rig.pm.join("alpha-launch/project.yaml").is_file(),
        "project.yaml missing on disk"
    );

    assert!(
        project_keys(&rig, &owner).contains(&"alpha-launch".to_string()),
        "created project missing from owner GET /api/projects"
    );
    assert!(
        project_keys(&rig, &member).contains(&"alpha-launch".to_string()),
        "workspace-visible created project missing from member GET /api/projects"
    );

    // Valid repo-backed creation with goal and preset seeds PROJECT.md.
    let repo_dir = tempfile::Builder::new()
        .prefix("c2859-repo")
        .tempdir()
        .unwrap();
    let init_status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo_dir.path())
        .status()
        .expect("git init failed");
    assert!(init_status.success(), "git init failed");
    let (status, repo_created) = post_json(
        &rig,
        Some(&owner),
        &json!({
            "name": "Operations Desk",
            "key": "ops-desk",
            "prefix": "OPSD",
            "goal": "Run reliable operations.",
            "preset": "operations",
            "repo": repo_dir.path().to_str().unwrap(),
        }),
    );
    assert_eq!(status, 201, "repo-backed create failed: {repo_created}");
    assert_eq!(repo_created["project"]["key"], "ops-desk", "{repo_created}");
    assert!(
        rig.pm.join("ops-desk/project.yaml").is_file(),
        "repo-backed project.yaml missing on disk"
    );
    assert!(
        rig.pm.join("ops-desk/PROJECT.md").is_file(),
        "repo-backed PROJECT.md missing on disk"
    );
    assert!(
        project_keys(&rig, &owner).contains(&"ops-desk".to_string()),
        "repo-backed project missing from GET /api/projects"
    );

    // Valid creations exercising all remaining allowed presets and optional key/prefix derivation.
    for (name, preset) in [
        ("General Desk", "general"),
        ("Client Delivery", "client_delivery"),
        ("Marketing Hub", "marketing"),
    ] {
        let (status, body) =
            post_json(&rig, Some(&owner), &json!({"name": name, "preset": preset}));
        assert_eq!(
            status, 201,
            "preset {preset} creation failed: {status} {body}"
        );
        let key = body["project"]["key"]
            .as_str()
            .expect("created response missing project.key")
            .to_string();
        assert!(
            project_keys(&rig, &owner).contains(&key),
            "created project {key} missing from GET /api/projects"
        );
    }

    let snapshot_before_conflicts = tracker_snapshot(&rig.pm);

    // Conflicting key -> 409 project_exists.
    let (status, dup_key) = post_json(
        &rig,
        Some(&owner),
        &json!({
            "name": "Alpha Duplicate",
            "key": "alpha-launch",
            "prefix": "ADUP",
        }),
    );
    assert_eq!(status, 409, "duplicate key must return 409: {dup_key}");
    assert_eq!(dup_key["code"], "project_exists", "{dup_key}");
    assert_eq!(tracker_snapshot(&rig.pm), snapshot_before_conflicts);

    // Conflicting prefix -> 409 prefix_conflict.
    let (status, dup_prefix) = post_json(
        &rig,
        Some(&owner),
        &json!({
            "name": "Beta Conflicting Prefix",
            "key": "beta-conflict",
            "prefix": "ALPH",
        }),
    );
    assert_eq!(
        status, 409,
        "conflicting prefix must return 409: {dup_prefix}"
    );
    assert_eq!(dup_prefix["code"], "prefix_conflict", "{dup_prefix}");
    assert_eq!(tracker_snapshot(&rig.pm), snapshot_before_conflicts);
    assert!(
        !project_keys(&rig, &owner).contains(&"beta-conflict".to_string()),
        "conflicting project leaked into GET /api/projects"
    );
}
