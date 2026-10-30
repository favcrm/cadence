#[cfg(feature = "test-seam")]
mod native_chain_acceptance {
    include!("../remote_cli_native_chain_acceptance.rs");
}

use super::*;
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::thread;

const ORG: &str = "acme";
const ORG_ID: &str = "ws_real";
const BRIDGE: &str = "hct_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

fn target(listener: &TcpListener) -> RemoteTarget {
    RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint: format!("http://{}", listener.local_addr().unwrap()),
        org_id: ORG_ID.into(),
    }
}

/// Write the credential the login slice writes — the same on-disk
/// layout (`cli-<slug>.json`, the `hct_` bound to org id + audience).
fn store_credential_at(dir: &Path, org: &str, org_id: &str, endpoint: &str, expires_at: u64) {
    let body = json!({"version": "hosted-cadence-auth.v1",
        "organization_id": org_id, "audience": endpoint,
        "access_token": BRIDGE, "expires_at": expires_at});
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join(format!("cli-{org}.json")))
        .unwrap();
    file.write_all(body.to_string().as_bytes()).unwrap();
}
fn store_credential(dir: &Path, endpoint: &str) {
    store_credential_at(dir, ORG, ORG_ID, endpoint, now().unwrap() + 300);
}

#[test]
fn assignment_identity_survives_saved_login_reloads() {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut frozen = target(&listener);
    frozen.identity = Some(CliIdentityPin {
        subject_id: "user_original".into(),
        issuer: "https://original.invalid".into(),
    });
    store_credential(dir.path(), &frozen.endpoint);
    let path = dir.path().join(format!("cli-{ORG}.json"));
    let mut original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    original["subject_id"] = json!("user_original");
    original["issuer"] = json!("https://original.invalid");
    fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(
        cli_material(dir.path(), &frozen).unwrap().access_token,
        BRIDGE
    );

    for (subject, issuer) in [
        (json!("user_replacement"), json!("https://original.invalid")),
        (json!("user_original"), json!("https://replacement.invalid")),
        (Value::Null, Value::Null),
        (json!("user_original"), Value::Null),
    ] {
        let mut replacement = original.clone();
        replacement["subject_id"] = subject;
        replacement["issuer"] = issuer;
        fs::write(&path, serde_json::to_vec(&replacement).unwrap()).unwrap();
        assert!(cli_material(dir.path(), &frozen).is_err());
        assert_eq!(
            listener.accept().err().unwrap().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
    fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(
        cli_material(dir.path(), &frozen).unwrap().access_token,
        BRIDGE
    );
}

fn read_request(socket: std::net::TcpStream) -> (std::net::TcpStream, Request) {
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut parts = line.trim_end().split(' ');
    let (method, path) = (
        parts.next().unwrap().to_string(),
        parts.next().unwrap().to_string(),
    );
    assert_eq!(method, "POST");
    let mut bearer = String::new();
    let mut length = 0_usize;
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        let (key, value) = line.split_once(':').unwrap();
        if key.eq_ignore_ascii_case("authorization") {
            bearer = value.trim().trim_start_matches("Bearer ").to_string();
        }
        if key.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse().unwrap();
        }
    }
    let mut raw = vec![0; length];
    reader.read_exact(&mut raw).unwrap();
    (
        socket,
        Request {
            path,
            bearer,
            body: serde_json::from_slice(&raw).unwrap(),
        },
    )
}

/// One request the fake accepted — path, bearer and parsed body.
struct Request {
    path: String,
    bearer: String,
    body: Value,
}

fn answer(mut socket: std::net::TcpStream, status: &str, extra: &str, body: &Value) {
    let bytes = serde_json::to_vec(body).unwrap();
    write!(
        socket,
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n{extra}\r\n",
        bytes.len()
    )
    .unwrap();
    socket.write_all(&bytes).unwrap();
}

/// The strict `/__platform/cli/authorize` gate (mirrors
/// `hostedCadenceCliAuthorizeRequestSchema`): every key required, no key
/// extra, org and audience pinned to this board, scopes a subset of
/// `cli.*`, ttl within 1..=300.
fn check_authorize(req: &Request) -> bool {
    req.path == "/__platform/cli/authorize"
        && req.bearer == BRIDGE
        && req.body.as_object().map(Map::len) == Some(5)
        && req.body["version"] == json!(CLI_VERSION)
        && req.body["organization_id"] == json!(ORG_ID)
        && req.body["audience"].as_str().is_some()
        && req.body["requested_scopes"].as_array().is_some_and(|s| {
            !s.is_empty()
                && s.iter()
                    .all(|v| v.as_str().is_some_and(|s| s.starts_with("cli.")))
        })
        && req.body["ttl_seconds"]
            .as_u64()
            .is_some_and(|t| (1..=300).contains(&t))
}

/// The strict `/__platform/cli/call` gate (mirrors
/// `hostedCadenceCliCallRequestSchema` + the forbidden-key rule):
/// version, a compact-JWS envelope, an allowlisted verb, optional
/// arguments object — and no authority-shaped top-level key.
fn check_call(req: &Request) -> bool {
    const FORBIDDEN: &[&str] = &["wiki_as", "actor", "role", "org", "user", "as", "principal"];
    let keys_ok = req
        .body
        .as_object()
        .is_some_and(|o| o.keys().all(|k| !FORBIDDEN.contains(&k.as_str())));
    let envelope_ok = req.body["envelope"].as_str().is_some_and(|e| {
        e.split('.').count() == 3
            && e.split('.').all(|p| {
                !p.is_empty()
                    && p.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            })
    });
    req.path == "/__platform/cli/call"
        && req.bearer == BRIDGE
        && keys_ok
        && req.body["version"] == json!(CLI_VERSION)
        && envelope_ok
        && REMOTE_VERBS.contains(&req.body["verb"].as_str().unwrap_or_default())
        && req.body.get("arguments").is_none_or(|a| a.is_object())
}

/// A loopback fake worker: one authorize (strict schema → mint a fixed
/// envelope shape), then `calls` call-attempts answered in order.
/// Returns the bound listener plus a `Vec` the server records every
/// request path + body into — so a test can prove nothing was sent.
fn fake_worker(
    answers: Vec<(u16, &'static str, Value)>,
) -> (
    TcpListener,
    std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = listener.try_clone().unwrap();
    let seen: std::sync::Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
    let requests = seen.clone();
    // The container (#744) consumes each cli envelope's `jti` once —
    // the fake does the same so a reused envelope is refused, not
    // silently accepted.
    let spent: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let consumed = spent.clone();
    let handle = thread::spawn(move || {
        loop {
            let Ok((socket, _)) = server.accept() else {
                return;
            };
            let req = read_request(socket);
            let (socket, req) = (req.0, req.1);
            requests.lock().unwrap().push(json!({
                "path": req.path, "body": req.body,
            }));
            if req.path == "/__platform/cli/authorize" {
                if !check_authorize(&req) {
                    answer(
                        socket,
                        "400 Bad Request",
                        "",
                        &json!({"code": "invalid_request"}),
                    );
                    continue;
                }
                // Envelope shape only — the worker signs the real one.
                // A distinct envelope per mint lets a test prove the
                // client never reuses one across commands/retries (#744:
                // the container treats each cli envelope as single-use).
                let mint = requests
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|r| r["path"] == json!("/__platform/cli/authorize"))
                    .count();
                let envelope = format!("h.e.{mint}");
                answer(
                    socket,
                    "200 OK",
                    "",
                    &json!({"version": CLI_VERSION, "envelope": envelope,
                        "bearer": format!("wikienv_{envelope}"), "expires_at": 0,
                        "actor": "users/o", "scope": ["cli.read"], "issuer": "i"}),
                );
                continue;
            }
            if req.path == "/__platform/cli/call" {
                let n = requests
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|r| r["path"] == json!("/__platform/cli/call"))
                    .count();
                if !check_call(&req) {
                    answer(
                        socket,
                        "400 Bad Request",
                        "",
                        &json!({"code": "invalid_request"}),
                    );
                    continue;
                }
                // Single-use: a replayed envelope is unauthorized —
                // the jti was already spent by an earlier call.
                let env = req.body["envelope"].as_str().unwrap().to_string();
                {
                    let mut spent = consumed.lock().unwrap();
                    if spent.contains(&env) {
                        answer(
                            socket,
                            "401 Unauthorized",
                            "",
                            &json!({"code": "envelope_reused"}),
                        );
                        continue;
                    }
                    spent.push(env);
                }
                let Some((status, extra, body)) = answers.get(n - 1) else {
                    // More attempts than answers — a test bug or an
                    // unbounded retry loop; refuse loudly.
                    answer(socket, "500", "", &json!({"code": "unexpected_call"}));
                    continue;
                };
                let line = match status {
                    200 => "200 OK",
                    302 => "302 Found",
                    401 => "401 Unauthorized",
                    403 => "403 Forbidden",
                    409 => "409 Conflict",
                    503 => "503 Service Unavailable",
                    _ => "400 Bad Request",
                };
                answer(socket, line, extra, &body.clone());
                continue;
            }
            answer(socket, "404 Not Found", "", &json!({"code": "not_found"}));
        }
    });
    (listener, seen, handle)
}

/// Run one command against the strict fake: returns the call result
/// and every request the worker saw (to count mints / reads).
fn run(
    answers: Vec<(u16, &'static str, Value)>,
    verb: &str,
    args: Map<String, Value>,
    wake_timeout: u64,
) -> (Result<Value>, Vec<Value>) {
    let (listener, calls, _server) = fake_worker(answers);
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    let out = call_verb_in(&target, verb, args, wake_timeout, &dir, |_| {}, |_| {});
    let calls_out = calls.lock().unwrap().clone();
    // Tests assert on the verb's JSON body — a conflict answer
    // still carries one, so unwrap it the same way.
    let out = out.map(|a| match a {
        RemoteAnswer::Ok(b) | RemoteAnswer::Conflict(b) => b,
    });
    (out, calls_out)
}

/// Number of requests the fake accepted at `path` — 0 proves no byte
/// for that route ever left the client.
fn hits(requests: &[Value], path: &str) -> usize {
    requests.iter().filter(|r| r["path"] == json!(path)).count()
}

#[test]
fn cad1434_blocker_authorize_refusals_never_forward_or_retry_user_writes() {
    let request_id = "12345678-1234-4234-8234-123456789abc";
    let unavailable = json!({"ok": false, "error": {
        "code": "runtime_unavailable", "hop": "board", "request_id": request_id,
        "message": "do-not-print-server-text"}})
    .to_string();
    let unsafe_code = json!({"code": "do-not-print-server-text /private/token",
        "message": "do-not-print-server-text"})
    .to_string();
    for (status, body, expected) in [
        (
            401,
            "<html>do-not-print-server-text</html>",
            "uncertain, HTTP 401",
        ),
        (
            403,
            "<html>do-not-print-server-text</html>",
            "authorization, HTTP 403",
        ),
        (503, unavailable.as_str(), "availability, HTTP 503"),
        (400, unsafe_code.as_str(), "unknown, HTTP 400"),
        (
            302,
            "<html>do-not-print-server-text</html>",
            "remote redirect refused",
        ),
        (
            200,
            "<html>do-not-print-server-text</html>",
            "remote authorize answered non-JSON",
        ),
    ] {
        for operation in ["issue_new", "write", "upload"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let target = target(&listener);
            let server = listener.try_clone().unwrap();
            let response = body.to_owned();
            let (stop, stopped) = std::sync::mpsc::channel();
            let worker = thread::spawn(move || {
                let mut paths = Vec::new();
                let mut stopping = false;
                let deadline = Instant::now() + Duration::from_secs(10);
                while Instant::now() < deadline {
                    stopping |= stopped.try_recv().is_ok();
                    let (mut socket, _) = match server.accept() {
                        Ok(connection) => connection,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            if stopping {
                                break;
                            }
                            thread::sleep(Duration::from_millis(2));
                            continue;
                        }
                        Err(e) => panic!("refusal worker accept: {e}"),
                    };
                    socket.set_nonblocking(false).unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(10)))
                        .unwrap();
                    let mut reader = BufReader::new(socket.try_clone().unwrap());
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    paths.push(line.split(' ').nth(1).unwrap().to_owned());
                    let mut length = 0;
                    loop {
                        line.clear();
                        reader.read_line(&mut line).unwrap();
                        if line == "\r\n" {
                            break;
                        }
                        if let Some((key, value)) = line.split_once(':') {
                            if key.eq_ignore_ascii_case("content-length") {
                                length = value.trim().parse::<usize>().unwrap();
                            }
                        }
                    }
                    // Read bytes, not JSON: an incorrectly forwarded multipart
                    // upload must also be recorded and fail the path assertion.
                    let mut raw = vec![0; length];
                    reader.read_exact(&mut raw).unwrap();
                    write!(socket, "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}", response.len()).unwrap();
                }
                paths
            });
            let root = tempfile::tempdir().unwrap();
            let dir = root.path();
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
            store_credential(dir, &target.endpoint);
            let saved = fs::read(dir.join(format!("cli-{ORG}.json"))).unwrap();
            let result = if operation == "upload" {
                upload_file_in(
                    dir,
                    &target,
                    "global/test.txt",
                    "test.txt",
                    b"test",
                    true,
                    5,
                    |_| panic!("authorize refusal retried"),
                )
            } else {
                let arguments = if operation == "write" {
                    json!({"path": "global/test", "text": "test"})
                } else {
                    json!({"project": "test", "title": "test"})
                };
                call_verb_in(
                    &target,
                    operation,
                    arguments.as_object().unwrap().clone(),
                    5,
                    dir,
                    |_| panic!("authorize refusal retried"),
                    |_| panic!("authorize refusal slept"),
                )
                .map(|_| json!(null))
            };
            stop.send(()).unwrap();
            let paths = worker.join().unwrap();
            let relay = if operation == "issue_new" {
                "cli"
            } else {
                "wiki"
            };
            assert_eq!(
                paths,
                [format!("/__platform/{relay}/authorize")],
                "{operation}, HTTP {status}"
            );
            let text = result.unwrap_err().to_string();
            assert!(text.contains(expected), "{operation}: {text}");
            assert!(!text.contains("do-not-print-server-text"), "{text}");
            assert!(!text.contains("/private/token"), "{text}");
            assert!(!text.contains("cadence login"), "{text}");
            assert!(!text.contains(BRIDGE), "{text}");
            if status == 503 {
                assert!(
                    text.contains(&format!("hop board, request {request_id}")),
                    "{text}"
                );
            }
            assert_eq!(
                fs::read(dir.join(format!("cli-{ORG}.json"))).unwrap(),
                saved
            );
        }
    }
}

#[test]
fn wiki_refuses_forged_authority_before_mint_and_never_uses_cli_relay() {
    // CAD-1401 independent refusal acceptance. The worker deliberately
    // accepts every payload: only the real client guard can refuse it.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let target = target(&listener);
    let server = listener.try_clone().unwrap();
    let seen: std::sync::Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
    let requests = seen.clone();
    let (stop, stopped) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        while stopped.try_recv().is_err() {
            let socket = match server.accept() {
                Ok((socket, _)) => socket,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(e) => panic!("fake worker accept: {e}"),
            };
            let (socket, req) = read_request(socket);
            let version = if req.path.starts_with("/__platform/wiki/") {
                "hosted-cadence-wiki.v1"
            } else {
                "hosted-cadence-cli.v1"
            };
            let response = if req.path.ends_with("/authorize") {
                json!({"version": version, "envelope": "h.e.1"})
            } else {
                json!({"ok": true})
            };
            requests.lock().unwrap().push(json!({
                "path": req.path, "bearer": req.bearer, "body": req.body,
            }));
            answer(socket, "200 OK", "", &response);
        }
    });
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(
        root.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    store_credential(root.path(), &target.endpoint);
    let mut failures = Vec::new();

    for dir in [root.path().to_path_buf(), root.path().join("missing")] {
        let before = seen.lock().unwrap().len();
        let out = call_verb_in(&target, "delete", Map::new(), 10, &dir, |_| {}, |_| {});
        if !matches!(out, Err(ref e) if e.kind() == "rejected" && e.to_string().contains("allowlist"))
            || seen.lock().unwrap().len() != before
        {
            failures.push("delete was not refused before credential access/request".to_string());
        }
    }
    // Observe successful calls too, so a missing route or a wrong schema
    // cannot masquerade as a local refusal. Include an empty arguments map.
    for (action, args) in [
        ("enumerate", json!({"path": "global/", "q": "acceptance"})),
        ("read", json!({"path": "global/acceptance.md"})),
        (
            "write",
            json!({"path": "global/acceptance.md", "text": "acceptance", "if_rev": "none"}),
        ),
        ("enumerate", json!({})),
    ] {
        let args = args.as_object().unwrap().clone();
        let before = seen.lock().unwrap().len();
        let out = call_verb_in(
            &target,
            action,
            args.clone(),
            10,
            root.path(),
            |_| {},
            |_| {},
        );
        let observed = seen.lock().unwrap();
        let pair = &observed[before..];
        let scope = if action == "write" {
            "wiki.write"
        } else {
            "wiki.read"
        };
        if !matches!(out, Ok(RemoteAnswer::Ok(_)))
            || pair.len() != 2
            || pair[0]["path"] != "/__platform/wiki/authorize"
            || pair[0]["bearer"] != BRIDGE
            || pair[0]["body"]["version"] != "hosted-cadence-wiki.v1"
            || pair[0]["body"]["organization_id"] != ORG_ID
            || pair[0]["body"]["audience"] != target.endpoint
            || pair[0]["body"]["requested_scopes"] != json!([scope])
            || pair[1]["path"] != "/__platform/wiki/call"
            || pair[1]["bearer"] != BRIDGE
            || !exact_object(
                &pair[1]["body"],
                &["version", "envelope", "action", "arguments"],
            )
            || pair[1]["body"]["version"] != "hosted-cadence-wiki.v1"
            || pair[1]["body"]["envelope"] != "h.e.1"
            || pair[1]["body"]["action"] != action
            || pair[1]["body"]["arguments"] != Value::Object(args)
        {
            failures.push(format!(
                "{action}: wrong wiki route, scope, or exact call body"
            ));
        }
    }
    for action in ["enumerate", "read", "write"] {
        for field in ["actor", "role", "org", "user", "wiki_as", "as", "principal"] {
            let mut args = json!({"path": "global/acceptance.md", "text": "acceptance"})
                .as_object()
                .unwrap()
                .clone();
            args.insert(field.into(), json!("forged"));
            let before = seen.lock().unwrap().len();
            let out = call_verb_in(&target, action, args, 10, root.path(), |_| {}, |_| {});
            if !matches!(out, Err(ref e) if e.kind() == "rejected")
                || seen.lock().unwrap().len() != before
            {
                failures.push(format!(
                    "{action}: {field} was not refused before mint/request"
                ));
            }
        }
    }
    stop.send(()).unwrap();
    worker.join().unwrap();
    let requests = seen.lock().unwrap();
    if requests.iter().any(|r| r["path"] == "/__platform/cli/call") {
        failures.push("a wiki action reached /__platform/cli/call".into());
    }
    for request in requests
        .iter()
        .filter(|r| r["path"] == "/__platform/wiki/call")
    {
        if request["body"]["arguments"]
            .as_object()
            .is_some_and(|args| {
                args.keys().any(|key| {
                    ["actor", "role", "org", "user", "wiki_as", "as", "principal"]
                        .contains(&key.as_str())
                })
            })
        {
            failures.push("a wiki call carried authority in arguments".into());
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn credential_for_another_org_is_never_sent_to_this_host() {
    // The stored file names ws_real/acme's endpoint; the resolved org is
    // beta — the token must not ride to a foreign host (contract I1).
    let (listener, _c, _s) = fake_worker(vec![]);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, "https://acme.cadencecloud.app");
    let foreign = RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint: format!("http://{}", listener.local_addr().unwrap()),
        org_id: "ws_other".into(),
    };
    let out = call_verb_in(&foreign, "status", Map::new(), 10, &dir, |_| {}, |_| {});
    assert!(out.unwrap_err().to_string().contains("different workspace"));
}

/// Envelopes the calls presented, in order — the value the container
/// would consume as single-use `jti`.
fn envelopes(requests: &[Value]) -> Vec<String> {
    requests
        .iter()
        .filter(|r| r["path"] == json!("/__platform/cli/call"))
        .map(|r| r["body"]["envelope"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn every_command_mints_a_fresh_envelope() {
    // Single-use contract (#744): the container consumes the envelope's
    // `jti`, so two commands must present two different mints — an
    // envelope is never reused across calls.
    let (listener, requests, _s) =
        fake_worker(vec![(200, "", json!({"a": 1})), (200, "", json!({"b": 2}))]);
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    for verb in ["status", "agent_list"] {
        call_verb_in(&target, verb, Map::new(), 10, &dir, |_| {}, |_| {}).unwrap();
    }
    let envs = envelopes(&requests.lock().unwrap());
    assert_eq!(envs.len(), 2);
    assert_ne!(envs[0], envs[1], "two commands reused one envelope");
}

#[test]
fn a_wake_retry_remints_the_envelope() {
    // The waking 503 came from the worker before any forward — the
    // envelope's jti was verified there but never consumed by a
    // container. Retrying still mints a fresh one: a 503 that fired
    // later in the pipeline would have burned the first.
    let (out, requests) = run(
        vec![
            (503, "", json!({"state": "waking", "retry_after_s": 1})),
            (200, "", json!({"ok": true})),
        ],
        "status",
        Map::new(),
        120,
    );
    assert!(out.is_ok());
    assert_eq!(hits(&requests, "/__platform/cli/authorize"), 2);
    let envs = envelopes(&requests);
    assert_eq!(envs.len(), 2);
    assert_ne!(envs[0], envs[1], "a wake retry reused the envelope");
}

/// The complete correlated receipt a `message_send` 2xx must carry
/// (CAD-1568 revision): the request's stable `message` id and canonical
/// `alias` echoed, a known lifecycle `state`, a real `duplicate` bool,
/// the immutable `workspace_id`, the sender's canonical
/// `sender_subject` and the persisted `conversation` Home thread id.
fn send_receipt(id: &str) -> Value {
    json!({
        "message": id,
        "alias": "master",
        "state": "queued",
        "duplicate": false,
        "workspace_id": ORG_ID,
        "sender_subject": "sub_owner_1",
        "sender": "cli-owner",
        "conversation": "conv_home_1",
    })
}

#[test]
fn successful_send_without_a_receipt_is_unknown_and_not_retried() {
    for receipt in [
        Value::Null,
        json!({"message":"lost-receipt","state":"waking","duplicate":false}),
        json!({"message":"another-request","state":"queued","duplicate":false}),
    ] {
        let (out, requests) = run(
            vec![(200, "", receipt)],
            "message_send",
            json!({"alias": "master", "text": "coordinate", "message": "lost-receipt"})
                .as_object()
                .unwrap()
                .clone(),
            10,
        );
        assert_eq!(out.unwrap_err().kind(), "unknown");
        assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
    }
}

#[test]
fn send_with_a_missing_or_malformed_binding_is_unknown_and_not_retried() {
    // Each corruption removes or falsifies one correlation field of the
    // complete receipt: a 2xx carrying it cannot prove THIS send landed
    // as this caller's message in this workspace's conversation, so the
    // outcome is `unknown` — and the send is still never replayed.
    let tampered = |damage: &dyn Fn(&mut Map<String, Value>)| {
        let mut receipt = send_receipt("lost-receipt");
        damage(receipt.as_object_mut().unwrap());
        receipt
    };
    let receipts = [
        tampered(&|r| {
            r.remove("workspace_id");
        }),
        tampered(&|r| {
            r["workspace_id"] = json!("ws_foreign");
        }),
        tampered(&|r| {
            r["workspace_id"] = json!("");
        }),
        tampered(&|r| {
            r.remove("sender_subject");
        }),
        tampered(&|r| {
            r["sender_subject"] = json!("");
        }),
        tampered(&|r| {
            r.remove("conversation");
        }),
        tampered(&|r| {
            r["conversation"] = json!("");
        }),
        tampered(&|r| {
            r["alias"] = json!("assistant-personal");
        }),
        tampered(&|r| {
            r.remove("sender");
        }),
        tampered(&|r| {
            r["sender"] = json!("");
        }),
        tampered(&|r| {
            r["duplicate"] = json!("yes");
        }),
        tampered(&|r| {
            r["state"] = json!("delivered");
        }),
    ];
    for receipt in receipts {
        let (out, requests) = run(
            vec![(200, "", receipt)],
            "message_send",
            json!({"alias": "master", "text": "coordinate", "message": "lost-receipt"})
                .as_object()
                .unwrap()
                .clone(),
            10,
        );
        assert_eq!(out.unwrap_err().kind(), "unknown");
        assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
    }
}

#[test]
fn send_without_expected_message_identity_is_unknown_and_not_retried() {
    let mut receipt = send_receipt("lost-receipt");
    receipt.as_object_mut().unwrap().remove("message");
    let (out, requests) = run(
        vec![(200, "", receipt)],
        "message_send",
        json!({"alias": "master", "text": "coordinate"})
            .as_object()
            .unwrap()
            .clone(),
        10,
    );
    assert_eq!(out.unwrap_err().kind(), "unknown");
    assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
}

#[test]
fn send_receipt_for_another_subject_is_unknown_and_not_retried() {
    // A login that recorded its canonical subject binds the receipt to
    // it: a 2xx answering a different `sender_subject` cannot be this
    // send's enqueue proof.
    let (listener, requests, _s) = fake_worker(vec![(200, "", send_receipt("lost-receipt"))]);
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    let path = dir.join(format!("cli-{ORG}.json"));
    let mut cred: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    cred["subject_id"] = json!("sub_the_login");
    fs::write(&path, serde_json::to_vec(&cred).unwrap()).unwrap();
    let out = call_verb_in(
        &target,
        "message_send",
        json!({"alias": "master", "text": "coordinate", "message": "lost-receipt"})
            .as_object()
            .unwrap()
            .clone(),
        10,
        &dir,
        |_| {},
        |_| {},
    );
    assert_eq!(out.unwrap_err().kind(), "unknown");
    assert_eq!(
        hits(&requests.lock().unwrap(), "/__platform/cli/call"),
        1,
        "an unknown send outcome was replayed"
    );
}

#[test]
fn a_send_wake_retry_keeps_its_durable_message_id() {
    // A pre-forward wake retry needs a fresh envelope but must not
    // mint another durable message id.
    let args: Map<String, Value> = json!({
        "alias": "master",
        "text": "please coordinate",
        "message": "cad1568-proof-1",
    })
    .as_object()
    .unwrap()
    .clone();
    let (out, requests) = run(
        vec![
            (503, "", json!({"state": "waking", "retry_after_s": 1})),
            (200, "", send_receipt("cad1568-proof-1")),
        ],
        "message_send",
        args,
        120,
    );
    out.unwrap();
    let mut calls = 0;
    for request in requests
        .iter()
        .filter(|r| r["path"] == json!("/__platform/cli/call"))
    {
        calls += 1;
        assert_eq!(request["body"]["arguments"]["message"], "cad1568-proof-1");
    }
    assert_eq!(calls, 2);
    let envs = envelopes(&requests);
    assert_ne!(envs[0], envs[1], "a send retry reused the envelope");
}

#[test]
fn cli_message_refusals_map_bounded_codes_never_server_text() {
    // CAD-1568: the container's `cli_fail` body is
    // `{ok:false,error:{code,message}}` and `message` is arbitrary
    // server text — the client answers the bounded code with its own
    // actionable text, and the server string never surfaces.
    let poisonous = |code: &str| {
        json!({"ok": false, "error": {
            "code": code,
            "message": "internal-server-text /private/path"}})
    };
    for (verb, status, code) in [
        ("message_send", 400u16, "recipient_class_unsupported"),
        ("message_send", 400, "message_id_conflict"),
        ("message_send", 403, "not_permitted"),
        ("message_read", 404, "not_found"),
    ] {
        let (out, requests) = run(vec![(status, "", poisonous(code))], verb, Map::new(), 10);
        let err = out.unwrap_err();
        let shown = err.to_string();
        assert_eq!(
            err.code(),
            Some(code),
            "{verb}: machine-readable refusal code changed"
        );
        assert!(
            !shown.contains("internal-server-text") && !shown.contains("/private/path"),
            "{verb} {code} leaked server text: {shown}"
        );
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "a refusal was retried"
        );
    }
    // An unlisted code on a message verb keeps the generic refusal —
    // never a fabricated action.
    let (out, _) = run(
        vec![(400, "", poisonous("some_future_code"))],
        "message_send",
        Map::new(),
        10,
    );
    let shown = out.unwrap_err().to_string();
    assert!(shown.contains("unknown"), "{shown}");
    assert!(!shown.contains("internal-server-text"), "{shown}");
}

#[test]
fn a_checked_conflict_rides_up_as_the_conflict_answer() {
    // A 409 `{conflict, current_rev, card}` is the remote's checked
    // "not written" verdict — the body must reach the caller (the
    // CLI prints it and exits 5), not collapse into a generic
    // refusal. One attempt only: a conflict is never retried.
    let body = json!({"conflict": "if_rev",
        "current_rev": "fnv1a:829ba77079943a03",
        "card": {"id": "CAD-1"},
        "error": "if_rev does not match issue.md — re-read and retry"});
    let (listener, requests, _s) = fake_worker(vec![(409, "", body)]);
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    let out = call_verb_in(&target, "issue_set", Map::new(), 10, &dir, |_| {}, |_| {}).unwrap();
    let RemoteAnswer::Conflict(body) = out else {
        panic!("a 409 conflict answer must surface as Conflict, got {out:?}");
    };
    assert_eq!(body["current_rev"], json!("fnv1a:829ba77079943a03"));
    assert_eq!(body["card"]["id"], json!("CAD-1"));
    assert_eq!(
        hits(&requests.lock().unwrap(), "/__platform/cli/call"),
        1,
        "a conflict was retried"
    );
}

#[test]
fn a_bare_409_without_the_conflict_shape_is_a_generic_refusal() {
    // Only the contract's `{conflict: …}` payload is a verdict; a
    // 409 carrying anything else stays the generic refusal.
    let (out, requests) = run(
        vec![(409, "", json!({"error": "midway collision"}))],
        "issue_set",
        Map::new(),
        10,
    );
    // CAD-1434: decoded, not a bare generic message. It shows the safe
    // category, never the raw body, and is never retried.
    let shown = out.unwrap_err().to_string();
    assert!(shown.contains("unknown"), "{shown}");
    assert!(!shown.contains("midway collision"), "{shown}");
    assert!(!shown.contains("cadence login"), "{shown}");
    assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
}

#[test]
fn a_bare_503_is_availability_and_is_not_retried_or_login() {
    // A 503 without a waking state is terminal for this command: retry
    // later, keep the saved login, never ask for a fresh one.
    let (out, requests) = run(
        vec![(503, "", json!({"state": "board_unreachable"}))],
        "issue_set",
        Map::new(),
        10,
    );
    let shown = out.unwrap_err().to_string();
    assert!(shown.contains("unavailable"), "{shown}");
    assert!(shown.contains("retry later"), "{shown}");
    assert!(!shown.contains("cadence login"), "{shown}");
    assert_eq!(
        hits(&requests, "/__platform/cli/call"),
        1,
        "a bare 503 was retried"
    );
}

/// The exact Cadence container refusal (`src/ui/wiki_envelope.rs`): a path
/// outside the signed grant is a 403 `insufficient_scope` with a message.
/// On the call path it is authorization, shows its safe code, asks for no
/// login, is sent once, and never echoes the body.
#[test]
fn a_container_403_insufficient_scope_is_authorization_on_the_call_path() {
    let body = json!({"ok": false, "error": {"code": "insufficient_scope",
        "message": "the path is outside this envelope's wiki write prefix do-not-print"}});
    let (out, requests) = run(vec![(403, "", body)], "issue_set", Map::new(), 10);
    let shown = out.unwrap_err().to_string();
    assert!(shown.contains("authorization"), "{shown}");
    assert!(shown.contains("insufficient_scope"), "{shown}");
    assert!(!shown.contains("cadence login"), "{shown}");
    assert!(!shown.contains("do-not-print"), "{shown}");
    assert_eq!(
        hits(&requests, "/__platform/cli/call"),
        1,
        "a container 403 was retried"
    );
}

/// A generic 403 (no safe code) is still an access denial: conservative
/// authorization guidance, never a login claim or an unknown-shape answer.
#[test]
fn a_generic_403_is_conservative_access_denial_not_login() {
    let (refusal, reason) = decode_refusal(403, Some(&json!({"error": {"code": "forbidden"}})));
    assert_eq!(refusal, Refusal::Authorization);
    assert_eq!(reason, None);
    let text = refusal_message(refusal, reason.as_deref(), 403);
    assert!(text.contains("not permitted"), "{text}");
    assert!(!text.contains("cadence login"), "{text}");
    let bare = decode_refusal(403, None);
    assert_eq!(bare.0, Refusal::Authorization);
    assert!(!refusal_message(bare.0, bare.1.as_deref(), 403).contains("cadence login"));
}

/// The container's `insufficient_scope` is authorization only on 403. The same
/// code on a 401 is not trusted as a policy proof, so it stays uncertain.
#[test]
fn insufficient_scope_is_authorization_only_on_403() {
    let body = json!({"error": {"code": "insufficient_scope"}});
    assert_eq!(decode_refusal(403, Some(&body)).0, Refusal::Authorization);
    assert_eq!(decode_refusal(401, Some(&body)).0, Refusal::Unknown);
}

/// The same container refusal on the blob upload path: one upload request,
/// safe code shown, no login advice, the raw body never printed.
#[test]
fn a_container_403_insufficient_scope_is_authorization_on_the_upload_path() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();
    std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let (listener, handle) = upload_fake(
        403,
        r#"{"ok":false,"error":{"code":"insufficient_scope","message":"outside prefix do-not-print"}}"#,
    );
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    store_credential(dir, &endpoint);
    let target = RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint,
        org_id: ORG_ID.into(),
    };
    let err = upload_file_in(
        dir,
        &target,
        "projects/x/a.pdf",
        "a.pdf",
        b"%PDF-bytes",
        true,
        5,
        |_| {},
    )
    .unwrap_err();
    let _ = handle.join().unwrap();
    let shown = err.to_string();
    assert!(shown.contains("authorization"), "{shown}");
    assert!(shown.contains("insufficient_scope"), "{shown}");
    assert!(!shown.contains("cadence login"), "{shown}");
    assert!(!shown.contains("do-not-print"), "{shown}");
}

#[test]
fn a_400_shows_its_safe_code_and_is_not_retried() {
    // A payload/validation refusal keeps its safe code so the caller sees
    // what was refused. It is not a login or availability claim.
    let (out, requests) = run(
        vec![(
            400,
            "",
            json!({"ok": false, "error": {"code": "payload_too_large"}}),
        )],
        "issue_set",
        Map::new(),
        10,
    );
    let shown = out.unwrap_err().to_string();
    assert!(shown.contains("payload_too_large"), "{shown}");
    assert!(!shown.contains("cadence login"), "{shown}");
    assert_eq!(
        hits(&requests, "/__platform/cli/call"),
        1,
        "a 400 was retried"
    );
}

/// The exact AOS gateway refusal (`routes.ts` `gateRefusal`): 401, code
/// `unauthorized`, plus the proven policy reason. Each is driven through the
/// real call path (`call_verb_in` -> `call_once` -> fake relay), not only the
/// decoder.
fn aos_refusal(reason: &str) -> Value {
    json!({"ok": false, "error": {"code": "unauthorized",
        "message": "Wiki call refused.", "reason": reason}})
}

#[test]
fn a_policy_401_scope_not_granted_never_recommends_login_and_is_not_retried() {
    // Paired AOS shape: 401 unauthorized + reason scope_not_granted is a
    // policy outcome. It must not say "cadence login", and exactly one
    // attempt is sent (no retry, no mutation replay).
    let (out, requests) = run(
        vec![(401, "", aos_refusal("scope_not_granted"))],
        "issue_set",
        Map::new(),
        10,
    );
    let shown = out.unwrap_err().to_string();
    assert!(!shown.contains("cadence login"), "{shown}");
    assert!(shown.contains("authorization"), "{shown}");
    assert!(shown.contains("scope_not_granted"), "{shown}");
    assert_eq!(
        hits(&requests, "/__platform/cli/call"),
        1,
        "a policy 401 was retried"
    );
}

#[test]
fn every_allowlisted_policy_reason_is_authorization_on_the_real_path() {
    for reason in ["scope_not_granted", "grant_narrowed", "binding_mismatch"] {
        let (out, requests) = run(
            vec![(401, "", aos_refusal(reason))],
            "issue_set",
            Map::new(),
            10,
        );
        let shown = out.unwrap_err().to_string();
        assert!(!shown.contains("cadence login"), "{reason}: {shown}");
        assert!(shown.contains(reason), "{reason}: {shown}");
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "{reason} retried"
        );
    }
}

#[test]
fn envelope_invalid_is_not_terminal_session_loss_and_is_not_retried() {
    // envelope_invalid (expired/legacy envelope) is neither policy nor a
    // proven session loss. It must not claim expiry or tell the user to log in.
    let (out, requests) = run(
        vec![(401, "", aos_refusal("envelope_invalid"))],
        "status",
        Map::new(),
        10,
    );
    let shown = out.unwrap_err().to_string();
    assert!(!shown.contains("cadence login"), "{shown}");
    assert!(shown.contains("did not confirm"), "{shown}");
    assert_eq!(
        hits(&requests, "/__platform/cli/call"),
        1,
        "an envelope refusal was retried"
    );
}

#[test]
fn a_generic_unauthorized_without_reason_is_uncertain_not_confirmed_expiry() {
    // A 401 unauthorized with no reason means the bearer did not resolve, but
    // the server cannot tell expiry from revocation. It is uncertain: no
    // confirmed-expiry claim, one attempt, read-only diagnosis advised.
    let (out, requests) = run(
        vec![(
            401,
            "",
            json!({"ok": false, "error": {"code": "unauthorized"}}),
        )],
        "status",
        Map::new(),
        10,
    );
    let shown = out.unwrap_err().to_string();
    assert!(shown.contains("did not confirm"), "{shown}");
    assert!(shown.contains("read-only diagnosis"), "{shown}");
    assert!(!shown.contains("has expired"), "{shown}");
    assert_eq!(
        hits(&requests, "/__platform/cli/call"),
        1,
        "a generic 401 was retried"
    );
}

#[test]
fn a_non_json_401_is_uncertain_and_never_echoes_the_body() {
    // A gateway or ingress 401 that is not the envelope (HTML, empty) must not
    // be read as confirmed expiry, and its body must not be echoed.
    let (refusal, reason) = decode_refusal(401, None);
    assert_eq!(refusal, Refusal::Uncertain);
    assert_eq!(reason, None);
    let text = refusal_message(refusal, reason.as_deref(), 401);
    assert!(text.contains("did not confirm"), "{text}");
    assert!(!text.contains("<html"), "{text}");
}

#[test]
fn only_terminal_issuer_and_session_codes_ask_for_login() {
    for code in [
        "invalid_token",
        "invalid_grant",
        "session_expired",
        "session_revoked",
        "lineage_terminal",
    ] {
        let (refusal, _) = decode_refusal(401, Some(&json!({"error": {"code": code}})));
        assert_eq!(refusal, Refusal::Authentication, "{code}");
        let text = refusal_message(refusal, None, 401);
        assert!(text.contains("cadence login"), "{code}: {text}");
    }
}

#[test]
fn an_unknown_policy_looking_reason_is_not_trusted() {
    // Only the allowlisted reasons count. A free-form or unknown reason is
    // dropped, so it can neither assert policy nor leak into the message.
    let (refusal, reason) = decode_refusal(
        401,
        Some(&json!({"error": {"code": "unauthorized", "reason": "some raw server text"}})),
    );
    assert_eq!(reason, None);
    assert_eq!(refusal, Refusal::Uncertain);
}

#[test]
fn a_board_refusal_shows_its_hop_and_request_id_and_never_the_raw_body() {
    // The AOS board names its hop and a fresh UUID. Both reach the message;
    // the raw body and any extra server text never do.
    let id = "0b2e1f4a-7c3d-4e5f-8a9b-1c2d3e4f5a6b";
    let body = json!({"ok": false, "error": {"code": "unauthorized",
        "reason": "scope_not_granted", "hop": "board", "request_id": id,
        "message": "raw server text do-not-print"}});
    let (out, requests) = run(vec![(401, "", body)], "issue_set", Map::new(), 10);
    let shown = out.unwrap_err().to_string();
    assert!(shown.contains("hop board"), "{shown}");
    assert!(shown.contains(id), "{shown}");
    assert!(!shown.contains("do-not-print"), "{shown}");
    assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
}

#[test]
fn an_unlisted_hop_or_a_non_uuid_request_id_is_dropped() {
    for body in [
        json!({"error": {"code": "unauthorized", "hop": "evil.example",
            "request_id": "0b2e1f4a-7c3d-4e5f-8a9b-1c2d3e4f5a6b"}}),
        json!({"error": {"code": "unauthorized", "hop": "board",
            "request_id": "not a uuid <script>"}}),
    ] {
        assert_eq!(refusal_correlation(Some(&body)), None, "{body}");
    }
    // A legacy envelope with no correlation still decodes exactly as before.
    assert_eq!(
        refusal_correlation(Some(&json!({"error": {"code": "unauthorized"}}))),
        None
    );
}

#[test]
fn a_renewal_refusal_uses_the_issuer_shape_not_the_gateway_envelope() {
    // Renewal reads the issuer's {error:"<code>"}. Only its terminal
    // refresh-grant refusals ask for login; a transient 503 keeps the login.
    let terminal = renewal_refusal(401, &json!({"error": "invalid_token"}), None);
    assert!(terminal.to_string().contains("cadence login"), "{terminal}");
    let transient = renewal_refusal(503, &json!({"error": "authority_unavailable"}), None);
    assert!(
        !transient.to_string().contains("cadence login"),
        "{transient}"
    );
    assert!(transient.to_string().contains("kept"), "{transient}");
}

#[test]
fn a_team_commit_uncertainty_is_an_unknown_outcome_not_a_refusal() {
    let answer = classify(
        503,
        None,
        Some(json!({"error": {"code": "mutation_outcome_uncertain"}})),
    );
    assert!(matches!(
        answer,
        Verdict::Done(Err(Error::OutcomeUnknown(_)))
    ));
}

/// A relay that answers the authorize mint, then drops the connection
/// without answering the call — the response is lost after the request
/// was dispatched.
fn dropping_worker() -> (TcpListener, std::sync::Arc<std::sync::Mutex<Vec<Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = listener.try_clone().unwrap();
    let seen: std::sync::Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
    let requests = seen.clone();
    thread::spawn(move || {
        loop {
            let Ok((socket, _)) = server.accept() else {
                return;
            };
            let (socket, req) = read_request(socket);
            requests.lock().unwrap().push(json!({
                "path": req.path, "body": req.body,
            }));
            if req.path == "/__platform/cli/authorize" {
                answer(
                    socket,
                    "200 OK",
                    "",
                    &json!({"version": CLI_VERSION, "envelope": "h.e.1",
                        "bearer": "wikienv_h.e.1", "expires_at": 0,
                        "actor": "users/o", "scope": ["cli.read"], "issuer": "i"}),
                );
                continue;
            }
            // The call was dispatched; the answer never comes.
            drop(socket);
        }
    });
    (listener, seen)
}

/// CAD-1504: a lost response to a team mutation is an unknown outcome,
/// not a rejection. The Worker may have committed the write, so the CLI
/// must not report it as refused and must not invite a blind retry.
#[test]
fn a_lost_response_to_a_team_mutation_is_unknown_not_a_refusal() {
    let (listener, _seen) = dropping_worker();
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    let err = call_verb_in(
        &target,
        "team_invite",
        json!({"email": "new@example.test", "role": "member"})
            .as_object()
            .cloned()
            .unwrap(),
        10,
        &dir,
        |_| {},
        |_| {},
    )
    .unwrap_err();
    assert_eq!(err.kind(), "unknown", "{err}");
    let text = err.to_string();
    assert!(text.contains("team list"), "{text}");
    assert!(!text.contains("retry later"), "{text}");
}

/// A generic 5xx with no worker `state` and no board hop follows a
/// possible commit: the platform may have written the membership change
/// and then failed to answer. It is an unknown outcome, never an
/// availability error that says "retry later".
#[test]
fn a_bare_5xx_to_a_team_mutation_is_unknown_not_availability() {
    let body = json!({"ok": false, "error": {"code": "internal_error"}});
    let (listener, _seen, _server) = fake_worker(vec![(503, "", body)]);
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    let err = call_verb_in(
        &target,
        "team_remove",
        json!({"user_id": "usr_x"}).as_object().cloned().unwrap(),
        10,
        &dir,
        |_| {},
        |_| {},
    )
    .unwrap_err();
    assert_eq!(err.kind(), "unknown", "{err}");
    let text = err.to_string();
    assert!(text.contains("team list"), "{text}");
    assert!(!text.contains("retry later"), "{text}");
}

/// The board's own gate refusal is decided before the verb ran, so it
/// stays a definite refusal — it must not be blurred into "may have
/// committed".
#[test]
fn a_board_gate_refusal_to_a_team_mutation_stays_a_refusal() {
    let request_id = "12345678-1234-4234-8234-123456789abc";
    let body = json!({"ok": false, "error": {
        "code": "runtime_unavailable", "hop": "board", "request_id": request_id,
        "message": "do-not-print-server-text"}});
    let (listener, _seen, _server) = fake_worker(vec![(503, "", body)]);
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    let err = call_verb_in(
        &target,
        "team_set_role",
        json!({"user_id": "usr_x", "role": "member"})
            .as_object()
            .cloned()
            .unwrap(),
        10,
        &dir,
        |_| {},
        |_| {},
    )
    .unwrap_err();
    assert_eq!(err.kind(), "rejected", "{err}");
    let text = err.to_string();
    assert!(!text.contains("may already have committed"), "{text}");
}

/// A read-only team verb commits nothing, so a 5xx is plain
/// availability — the unknown-outcome treatment is for writes only.
#[test]
fn a_5xx_to_a_team_read_stays_availability() {
    let body = json!({"ok": false, "error": {"code": "internal_error"}});
    let (listener, _seen, _server) = fake_worker(vec![(503, "", body)]);
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    let err = call_verb_in(&target, "team_list", Map::new(), 10, &dir, |_| {}, |_| {}).unwrap_err();
    assert_ne!(err.kind(), "unknown", "{err}");
}

/// The platform's own explicit uncertainty code keeps its decoded
/// message — it names the code the Worker returned, which is more
/// precise than the generic ambiguous-outcome text.
#[test]
fn an_explicit_uncertainty_code_to_a_team_mutation_keeps_its_code() {
    let body = json!({"ok": false, "error": {"code": "invite_email_uncertain"}});
    let (listener, _seen, _server) = fake_worker(vec![(503, "", body)]);
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    let err = call_verb_in(
        &target,
        "team_invite",
        json!({"email": "new@example.test", "role": "member"})
            .as_object()
            .cloned()
            .unwrap(),
        10,
        &dir,
        |_| {},
        |_| {},
    )
    .unwrap_err();
    assert_eq!(err.kind(), "unknown", "{err}");
    let text = err.to_string();
    assert!(text.contains("invite_email_uncertain"), "{text}");
}

#[test]
fn local_status_is_offline_redacted_and_never_claims_live_verification() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let endpoint = "https://acme.cadencecloud.app";
    let target = RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint: endpoint.into(),
        org_id: ORG_ID.into(),
    };
    let at = now().unwrap();
    let body = json!({"version": "hosted-cadence-auth.v1", "organization_id": ORG_ID,
        "audience": endpoint, "access_token": BRIDGE, "expires_at": at + 300,
        "capabilities": ["cli.read", "cli.write"],
        "cli_session": {"session_id": "hcs_12345678-1234-4234-8234-123456789abc",
            "refresh_token": "hcr_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "generation": 1, "device_label": "cadence CLI",
            "idle_expires_at": at + 1000, "absolute_expires_at": at + 10_000}});
    std::fs::write(dir.join(format!("cli-{ORG}.json")), body.to_string()).unwrap();
    std::fs::set_permissions(
        dir.join(format!("cli-{ORG}.json")),
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    let status = local_status_at(&dir, &target);
    assert_eq!(status["state"], "renewable");
    assert_eq!(status["live_verified"], false);
    assert_eq!(status["scopes"], json!(["cli.read", "cli.write"]));
    let text = status.to_string();
    assert!(!text.contains(BRIDGE), "{text}");
    assert!(!text.contains("hcr_AAAA"), "{text}");
    assert!(!text.contains("hct_"), "{text}");
    // Freshness is a local observation, not renewal health.
    assert!(status["observed_at"].as_u64().is_some());
    assert!(status["stored_at"].as_u64().is_some());
    assert!(status.get("renewal_health").is_none(), "{text}");
}

#[test]
fn cad1434_blocker_local_status_legacy_expiry_keeps_command_refusal() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.join(format!("cli-{ORG}.json"));
    let at = now().unwrap();
    store_credential_at(dir, ORG, ORG_ID, &target.endpoint, at + 300);
    assert_eq!(
        local_status_at(dir, &target)["state"],
        "access_valid_until_expiry"
    );
    assert!(load_credential(dir, &target).is_ok());

    store_credential_at(dir, ORG, ORG_ID, &target.endpoint, at - 1);
    let mut body: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    body["capabilities"] = json!(["cli.read", "cli.write"]);
    fs::write(&path, body.to_string()).unwrap();
    let status = local_status_at(dir, &target);
    assert_eq!(status["state"], "reauthentication_required");
    assert_eq!(status["access_expires_at"], at - 1);
    assert_eq!(status["scopes"], json!(["cli.read", "cli.write"]));
    assert_eq!(status["live_verified"], false);
    assert!(status["observed_at"].as_u64().is_some());
    let text = status.to_string();
    assert!(!text.contains("hct_"), "{text}");
    assert!(!text.contains("hcr_"), "{text}");
    assert!(!text.contains(dir.to_string_lossy().as_ref()), "{text}");
    assert_eq!(
        load_credential(dir, &target).err().unwrap().to_string(),
        "the remote credential has expired — run `cadence login` again"
    );

    for (key, value) in [
        ("version", json!("invalid")),
        ("organization_id", json!("ws_other")),
        ("audience", json!("https://other.cadencecloud.app")),
        ("access_token", json!("invalid")),
        ("unexpected", json!(true)),
    ] {
        let mut invalid = body.clone();
        invalid[key] = value;
        fs::write(&path, invalid.to_string()).unwrap();
        assert_eq!(
            local_status_at(dir, &target)["state"],
            "invalid_local_credential",
            "{key}"
        );
        assert!(load_credential(dir, &target).is_err());
    }
    fs::write(&path, "not JSON").unwrap();
    assert_eq!(
        local_status_at(dir, &target)["state"],
        "invalid_local_credential"
    );
    fs::write(&path, body.to_string()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        local_status_at(dir, &target)["state"],
        "invalid_local_credential"
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        local_status_at(dir, &target)["state"],
        "invalid_local_credential"
    );
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
    let original = dir.join("saved.json");
    fs::rename(&path, &original).unwrap();
    std::os::unix::fs::symlink(&original, &path).unwrap();
    assert_eq!(
        local_status_at(dir, &target)["state"],
        "invalid_local_credential"
    );
    assert!(load_credential(dir, &target).is_err());
    assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
}

#[test]
fn a_permission_error_is_unavailable_never_no_credential() {
    // Only a missing file is absence. A stat failure (here: the credential
    // directory is unreadable) is unavailable, not proof that nothing is stored.
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
    let target = RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint: "https://acme.cadencecloud.app".into(),
        org_id: ORG_ID.into(),
    };
    let status = local_status_at(&dir, &target);
    // Restore before asserting so the tempdir can be removed either way.
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    assert_ne!(status["state"], "no_credential", "{status}");
    assert!(
        matches!(
            status["state"].as_str(),
            Some("unavailable" | "invalid_local_credential")
        ),
        "{status}"
    );
}

#[test]
fn an_expired_session_keeps_its_expiry_fields() {
    // The expired branch must still report the session's idle/absolute
    // expiry, so a reader sees why renewal is no longer possible.
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let endpoint = "https://acme.cadencecloud.app";
    let target = RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint: endpoint.into(),
        org_id: ORG_ID.into(),
    };
    let at = now().unwrap();
    let body = json!({"version": "hosted-cadence-auth.v1", "organization_id": ORG_ID,
        "audience": endpoint, "access_token": BRIDGE, "expires_at": at - 10,
        "capabilities": ["cli.read", "cli.write"],
        "cli_session": {"session_id": "hcs_12345678-1234-4234-8234-123456789abc",
            "refresh_token": "hcr_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "generation": 1, "device_label": "cadence CLI",
            "idle_expires_at": at - 5, "absolute_expires_at": at - 1}});
    let path = dir.join(format!("cli-{ORG}.json"));
    std::fs::write(&path, body.to_string()).unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let status = local_status_at(&dir, &target);
    assert_eq!(status["state"], "reauthentication_required");
    assert_eq!(status["session_idle_expires_at"], json!(at - 5));
    assert_eq!(status["session_absolute_expires_at"], json!(at - 1));
    assert_eq!(status["live_verified"], false);
}

/// CAD-1853: cloud-purpose admission must not be added to the default CLI
/// renewal entrypoint. This drives its real lock/load/due/authority guard;
/// the transport closure is an observable boundary, never a refusal oracle.
#[test]
fn default_cli_renewal_refuses_cloud_or_missing_cli_consent_before_any_post() {
    for scopes in [
        json!(["bridge.enroll", "cloud_agent.read", "cloud_agent.write"]),
        json!(["cli.read", "cli.write", "cloud_agent.write"]),
        json!(["cli.read"]),
    ] {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let endpoint = "https://acme.cadencecloud.app";
        let issuer = "https://api-v2.agenticos.hk";
        let target = RemoteTarget {
            identity: Some(CliIdentityPin {
                issuer: issuer.into(),
                subject_id: "user_owner".into(),
            }),
            org: ORG.into(),
            org_id: ORG_ID.into(),
            endpoint: endpoint.into(),
        };
        let at = now().unwrap();
        store_credential_at(root.path(), ORG, ORG_ID, endpoint, at - 1);
        let path = root.path().join(format!("cli-{ORG}.json"));
        let mut saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        saved["issuer"] = json!(issuer);
        saved["subject_id"] = json!("user_owner");
        saved["organization_slug"] = json!(ORG);
        saved["capabilities"] = scopes.clone();
        saved["cli_session"] = json!({
            "session_id": "hcs_12345678-1234-4234-8234-123456789abc",
            "refresh_token": format!("hcr_{}", "A".repeat(43)),
            "generation": 1,
            "device_label": "cadence CLI",
            "idle_expires_at": at + 1000,
            "absolute_expires_at": at + 10_000
        });
        fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
        fs::write(root.path().join("trusted-issuer"), format!("{issuer}\n")).unwrap();
        fs::set_permissions(
            root.path().join("trusted-issuer"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let before = fs::read(&path).unwrap();
        let posts = std::cell::Cell::new(0);
        let result = renewed_credential_with(
            root.path(),
            &target,
            Instant::now() + Duration::from_secs(5),
            |_dir, _issuer, _path, _bearer, _body, _deadline| {
                posts.set(posts.get() + 1);
                Err(Error::unknown("default CLI must not send this consent"))
            },
        );
        assert!(result.is_err(), "default CLI renewed {scopes}");
        assert_eq!(
            posts.get(),
            0,
            "default CLI sent cloud or incomplete consent"
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            before,
            "refusal published intent or authority"
        );
    }
}

#[test]
fn a_missing_audit_scope_is_refused_locally_with_the_login_that_grants_it() {
    // A default login carries cli.* only. The owner's merge approval and the
    // delivery record both need `audit.approve`, so the client refuses before a
    // request is built and names the opt-in login instead of leaving the caller
    // with a gateway 401 they cannot act on.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    let path = dir.join(format!("cli-{ORG}.json"));
    let mut stored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    stored["capabilities"] = json!(["cli.read", "cli.write", "wiki.read", "wiki.write"]);
    fs::write(&path, serde_json::to_vec(&stored).unwrap()).unwrap();

    for (verb, args) in [
        (
            "audit_approve",
            json!({"pr": 1445, "head": "e".repeat(40), "source": "probe"}),
        ),
        (
            "delivery_policy_set",
            json!({"project": "cadence", "repos": ["favcrm/cadence"]}),
        ),
    ] {
        let args = args.as_object().cloned().unwrap_or_default();
        let out = call_verb_in(&target, verb, args, 10, &dir, |_| {}, |_| {});
        let err = out.unwrap_err();
        let shown = err.to_string();
        assert_eq!(err.kind(), "rejected", "{verb}: {shown}");
        assert!(shown.contains("audit.approve"), "{verb}: {shown}");
        assert!(shown.contains("--audit-approve"), "{verb}: {shown}");
        assert!(shown.contains("nothing was sent"), "{verb}: {shown}");
    }
}

#[test]
fn a_review_refusal_is_definite_and_an_unserved_verb_is_not_retryable() {
    // The three codes a hosted review uses to say "fix the request" are
    // rejections, not unknown answers: nothing was written.
    for code in ["digest_mismatch", "self_review", "review_exists"] {
        let body = json!({"error": {"code": code}});
        let (refusal, shown) = decode_refusal(409, Some(&body));
        assert_eq!(refusal, Refusal::Rejected, "{code}");
        assert_eq!(shown.as_deref(), Some(code), "{code}");
        let text = refusal_message(refusal, shown.as_deref(), 409);
        assert!(text.contains("wrote nothing"), "{code}: {text}");
        assert!(!text.contains("cadence login"), "{code}: {text}");
    }
    // A 501 is the deployed build lacking the verb: no retry, no re-login.
    let (refusal, shown) =
        decode_refusal(501, Some(&json!({"error": {"code": "not_implemented"}})));
    assert_eq!(refusal, Refusal::Unsupported);
    let text = refusal_message(refusal, shown.as_deref(), 501);
    assert!(text.contains("does not serve"), "{text}");
    assert!(!text.contains("retry later"), "{text}");
    // The tenant board names a structured refusal with `state`, not
    // `error.code`: a delivery record naming an unauthorized repo.
    let (refusal, shown) = decode_refusal(
        409,
        Some(&json!({"state": "not_authorized", "repos": ["a/b"],
                     "reason": "these repositories are not authorized"})),
    );
    assert_eq!(refusal, Refusal::Rejected);
    assert_eq!(shown.as_deref(), Some("not_authorized"));
    let text = refusal_message(refusal, shown.as_deref(), 409);
    assert!(text.contains("not_authorized"), "{text}");
    assert!(
        !text.contains("these repositories"),
        "free text leaked: {text}"
    );
    // An unlisted 409 code keeps the conservative generic refusal.
    let (refusal, _) = decode_refusal(409, Some(&json!({"error": {"code": "midway_collision"}})));
    assert_eq!(refusal, Refusal::Unknown);
}

#[test]
fn an_unsafe_server_code_is_dropped_from_the_decoded_reason() {
    // Free text or wrong case from a server never becomes the shown reason.
    let (refusal, reason) = decode_refusal(
        418,
        Some(&json!({"error": {"code": "Bad Code With Spaces"}})),
    );
    assert_eq!(refusal, Refusal::Unknown);
    assert_eq!(reason, None);
    let text = refusal_message(refusal, reason.as_deref(), 418);
    assert!(!text.contains("Bad Code"), "{text}");
}

#[test]
fn a_cli_only_login_stays_valid_and_a_wiki_verb_needs_its_scope() {
    // CAD-1401 repair: the stored CLI pair is a valid login and its renewal
    // keeps exactly that set; a wiki verb on it is refused before any request.
    let cli: Vec<String> = vec!["cli.read".into(), "cli.write".into()];
    assert!(valid_capabilities(&cli));
    assert!(!valid_capabilities(&["cli.read".into()]));
    assert!(same_capabilities_json(
        &json!(["cli.read", "cli.write"]),
        &cli
    ));
    assert!(!same_capabilities_json(
        &json!(["cli.read", "cli.write", "wiki.read"]),
        &cli
    ));

    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(
        root.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let target = RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint: "http://127.0.0.1:9".into(),
        org_id: ORG_ID.into(),
    };
    let body = json!({"version": "hosted-cadence-auth.v1", "organization_id": ORG_ID,
        "audience": target.endpoint, "capabilities": cli,
        "access_token": BRIDGE, "expires_at": now().unwrap() + 300});
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.path().join(format!("cli-{ORG}.json")))
        .unwrap();
    file.write_all(body.to_string().as_bytes()).unwrap();
    let mut args = Map::new();
    args.insert("path".into(), json!("global/"));
    let err =
        call_verb_in(&target, "enumerate", args, 10, root.path(), |_| {}, |_| {}).unwrap_err();
    assert!(err.to_string().contains("wiki.read capability"), "{err}");
    assert!(err.to_string().contains("nothing was sent"), "{err}");
}

/// CAD-913: a fake relay that answers one authorize and one upload. It
/// records the upload's raw multipart bytes and bearer so the test checks
/// the wire form itself: the contract fields, one `file` part, the bearer,
/// and a refusal that prints only status and code.
fn upload_fake(
    upload_status: u16,
    upload_body: &'static str,
) -> (TcpListener, thread::JoinHandle<(Vec<u8>, String, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = listener.try_clone().unwrap();
    server.set_nonblocking(true).unwrap();
    let handle = thread::spawn(move || {
        let mut seen_upload = (Vec::new(), String::new(), String::new());
        // Bounded: the client may refuse before the second request, so the
        // fake gives up on its own instead of hanging the test.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut answered = 0;
        while answered < 2 && Instant::now() < deadline {
            let socket = match server.accept() {
                Ok((socket, _)) => socket,
                Err(_) => {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let path = line.split(' ').nth(1).unwrap().to_string();
            let mut bearer = String::new();
            let mut envelope_header = String::new();
            let mut length = 0_usize;
            let mut content_type = String::new();
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let (key, value) = line.split_once(':').unwrap();
                if key.eq_ignore_ascii_case("authorization") {
                    bearer = value.trim().trim_start_matches("Bearer ").to_string();
                }
                if key.eq_ignore_ascii_case("x-cadence-wiki-envelope") {
                    envelope_header = value.trim().to_string();
                }
                if key.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap();
                }
                if key.eq_ignore_ascii_case("content-type") {
                    content_type = value.trim().to_string();
                }
            }
            let mut raw = vec![0; length];
            reader.read_exact(&mut raw).unwrap();
            answered += 1;
            if path == "/__platform/wiki/authorize" {
                let env = "eyJhIjoxfQ.eyJiIjoyfQ.c2ln";
                answer(
                    socket,
                    "200 OK",
                    "",
                    &json!({"version": WIKI_VERSION, "envelope": env}),
                );
            } else {
                assert_eq!(path, "/__platform/wiki/upload");
                assert!(content_type.starts_with("multipart/form-data; boundary="));
                seen_upload = (raw, bearer, envelope_header);
                let body: Value = serde_json::from_str(upload_body).unwrap_or(json!({}));
                answer(socket, &format!("{upload_status} X"), "", &body);
            }
        }
        seen_upload
    });
    (listener, handle)
}

#[test]
fn upload_sends_one_multipart_body_and_a_refusal_shows_only_the_code() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();
    std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    // The overwrite pre-check is off here: the test drives the upload
    // request itself, and the listing path is covered by `overwrite`.
    let (listener, handle) = upload_fake(409, r#"{"code":"too_large","secret":"do-not-print"}"#);
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    store_credential(dir, &endpoint);
    let target = RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint,
        org_id: ORG_ID.into(),
    };
    let err = upload_file_in(
        dir,
        &target,
        "sources/a.pdf",
        "a.pdf",
        b"%PDF-bytes",
        true,
        5,
        |_| {},
    )
    .unwrap_err();
    let (raw, bearer, envelope_header) = handle.join().unwrap();
    // The bearer rides the Authorization header and the envelope rides
    // X-Cadence-Wiki-Envelope. The body carries the contract fields and
    // the single file part with the raw bytes, and no envelope field.
    assert_eq!(bearer, BRIDGE);
    assert_eq!(envelope_header, "eyJhIjoxfQ.eyJiIjoyfQ.c2ln");
    let text = String::from_utf8_lossy(&raw);
    for needle in [
        "name=\"version\"\r\n\r\nhosted-cadence-wiki.v1",
        "name=\"action\"\r\n\r\nwrite_blob",
        "name=\"path\"\r\n\r\nsources/a.pdf",
        "filename=\"a.pdf\"",
    ] {
        assert!(text.contains(needle), "missing {needle}");
    }
    assert!(!text.contains("name=\"envelope\""), "envelope in body");
    assert!(!text.contains("eyJhIjoxfQ"), "envelope in body");
    assert_eq!(text.matches("name=\"file\"").count(), 1);
    // A refusal names the status and code and never the raw body.
    let shown = err.to_string();
    assert!(
        shown.contains("409") && shown.contains("too_large"),
        "{shown}"
    );
    assert!(!shown.contains("do-not-print"), "{shown}");
}

/// CAD-913: the overwrite check fails closed on any listing that may be
/// partial, and reads a plain listing exactly.
#[test]
fn overwrite_check_fails_closed_on_a_possibly_partial_listing() {
    let plain = json!({"path": "sources", "entries": [{"name": "a.pdf"}]});
    assert_eq!(listing_taken(&plain, "a.pdf"), Ok(true));
    assert_eq!(listing_taken(&plain, "b.pdf"), Ok(false));
    for paged in [
        json!({"path": "sources", "entries": [], "truncated": true}),
        json!({"path": "sources", "entries": [], "next_cursor": "c2"}),
        json!({"path": "sources", "entries": [], "has_more": true}),
    ] {
        assert!(listing_taken(&paged, "a.pdf").is_err(), "{paged}");
    }
    // A missing or non-array `entries` cannot prove the name is free.
    assert!(listing_taken(&json!({"path": "sources"}), "a.pdf").is_err());
    // Any extra key is refused, even one that reads as "complete".
    let done = json!({"path": "sources", "entries": [], "truncated": false});
    assert!(listing_taken(&done, "a.pdf").is_err());
    // An unknown paging key that carries an existing name is refused too.
    let token = json!({"path": "sources", "entries": [], "next_token": "t2"});
    assert!(listing_taken(&token, "a.pdf").is_err());
    // A non-object body cannot prove the name is free.
    assert!(listing_taken(&json!([{"name": "a.pdf"}]), "a.pdf").is_err());
}

/// CAD-913 overwrite guard: an existing destination without `--overwrite`
/// is refused from the listing alone. The fake records every request path.
/// Authorize and the listing are reads and may be sent; the test asserts
/// the upload endpoint is never reached.
#[test]
fn overwrite_refusal_sends_no_upload_request() {
    let (message, order) = refused_listing_fixture(json!({
        "path": "sources",
        "entries": [{"name": "a.pdf"}]
    }));
    assert!(message.contains("already exists"), "{message}");
    assert!(message.contains("Nothing was sent"), "{message}");
    // Order: authorize, then the listing call. No upload request at all.
    assert_eq!(
        order.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(),
        ["/__platform/wiki/authorize", "/__platform/wiki/call"]
    );
    assert_eq!(order[1].1, "enumerate");
}

/// CAD-913: an unknown paging key on the listing refuses the upload even
/// when the existing name is on that page, and sends no upload request.
#[test]
fn unknown_paging_key_refuses_without_sending_upload() {
    let (message, order) = refused_listing_fixture(json!({
        "path": "sources",
        "entries": [{"name": "a.pdf"}],
        "next_token": "page-2"
    }));
    assert!(message.contains("may be incomplete"), "{message}");
    assert!(message.contains("Nothing was sent"), "{message}");
    assert!(!message.contains("page-2"), "{message}");
    assert!(
        !order.iter().any(|(p, _)| p.ends_with("/upload")),
        "upload reached: {order:?}"
    );
}

/// Runs `upload_file_in` without `--overwrite` against a fake whose listing
/// answer is `listing`. Returns the refusal message and every request seen.
fn refused_listing_fixture(listing: Value) -> (String, Vec<(String, String)>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();
    std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = listener.try_clone().unwrap();
    server.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_fake = Arc::clone(&stop);
    let fake = thread::spawn(move || {
        let mut paths: Vec<(String, String)> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !stop_fake.load(Ordering::SeqCst) && Instant::now() < deadline {
            let socket = match server.accept() {
                Ok((socket, _)) => socket,
                Err(_) => {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let path = line.split(' ').nth(1).unwrap().to_string();
            let mut length = 0_usize;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let (key, value) = line.split_once(':').unwrap();
                if key.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut raw = vec![0; length];
            reader.read_exact(&mut raw).unwrap();
            let action = serde_json::from_slice::<Value>(&raw)
                .ok()
                .and_then(|b| b["action"].as_str().map(str::to_string))
                .unwrap_or_default();
            paths.push((path.clone(), action));
            if path == "/__platform/wiki/authorize" {
                answer(
                    socket,
                    "200 OK",
                    "",
                    &json!({"version": WIKI_VERSION, "envelope": "eyJhIjoxfQ.eyJiIjoyfQ.c2ln"}),
                );
            } else if path == "/__platform/wiki/call" {
                answer(socket, "200 OK", "", &listing);
            } else {
                answer(socket, "500 X", "", &json!({}));
            }
        }
        paths
    });
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    store_credential(dir, &endpoint);
    let target = RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint,
        org_id: ORG_ID.into(),
    };
    let err = upload_file_in(
        dir,
        &target,
        "sources/a.pdf",
        "a.pdf",
        b"%PDF-bytes",
        false,
        5,
        |_| {},
    )
    .unwrap_err();
    stop.store(true, Ordering::SeqCst);
    let paths = fake.join().unwrap();
    (err.to_string(), paths)
}
fn credentialed(listener: &TcpListener) -> (RemoteTarget, tempfile::TempDir, PathBuf) {
    let target = target(listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    (target, root, dir)
}

fn write_answer(socket: &mut std::net::TcpStream, status: &str, extra: &str, body: &Value) {
    let bytes = serde_json::to_vec(body).unwrap();
    let _ = write!(
        socket,
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n{extra}\r\n",
        bytes.len()
    );
    let _ = socket.write_all(&bytes);
    let _ = socket.flush();
}

fn finite_worker(
    n: usize,
    serve: impl Fn(usize, &mut std::net::TcpStream, &Request) + Send + 'static,
) -> (
    TcpListener,
    std::sync::Arc<std::sync::Mutex<Vec<Request>>>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = listener.try_clone().unwrap();
    let seen: std::sync::Arc<std::sync::Mutex<Vec<Request>>> = Default::default();
    let requests = seen.clone();
    let handle = thread::spawn(move || {
        for i in 0..n {
            let Ok((socket, _)) = server.accept() else {
                return;
            };
            let (mut socket, req) = {
                let (s, r) = read_request(socket);
                (s, r)
            };
            requests.lock().unwrap().push(clone_request(&req));
            serve(i, &mut socket, &req);
        }
    });
    (listener, seen, handle)
}

fn clone_request(req: &Request) -> Request {
    Request {
        path: req.path.clone(),
        bearer: req.bearer.clone(),
        body: req.body.clone(),
    }
}

/// Read an HTTP request without assuming its body is JSON; uploads are
/// multipart and the server fixture must observe the bytes as sent.
fn read_raw_request(socket: std::net::TcpStream) -> (std::net::TcpStream, String, Vec<u8>) {
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let path = line.split(' ').nth(1).unwrap().to_string();
    let mut length = 0_usize;
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap();
            }
        }
    }
    let mut raw = vec![0; length];
    reader.read_exact(&mut raw).unwrap();
    (socket, path, raw)
}

/// Bounded loopback fixture for upload deadline tests. A quiet half-second
/// after the last request ends the fixture, including when the client
/// correctly refuses before sending a later phase.
fn upload_deadline_worker(
    serve: impl Fn(&str, &mut std::net::TcpStream, &[u8]) + Send + 'static,
) -> (
    TcpListener,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = listener.try_clone().unwrap();
    server.set_nonblocking(true).unwrap();
    let paths = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = paths.clone();
    let handle = thread::spawn(move || {
        let overall = Instant::now() + Duration::from_secs(8);
        let mut last_request = Instant::now();
        while Instant::now() < overall {
            match server.accept() {
                Ok((socket, _)) => {
                    let (mut socket, path, raw) = read_raw_request(socket);
                    observed.lock().unwrap().push(path.clone());
                    serve(&path, &mut socket, &raw);
                    last_request = Instant::now();
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if !observed.lock().unwrap().is_empty()
                        && last_request.elapsed() >= Duration::from_millis(500)
                    {
                        break;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });
    (listener, paths, handle)
}

fn upload_authorize_answer(socket: &mut std::net::TcpStream) {
    write_answer(
        socket,
        "200 OK",
        "",
        &json!({"version": WIKI_VERSION, "envelope": "h.e.1"}),
    );
}

#[test]
fn upload_total_deadline_covers_authorization() {
    let (listener, paths, server) = upload_deadline_worker(|path, socket, _raw| {
        if path == "/__platform/wiki/authorize" {
            thread::sleep(Duration::from_secs(3));
            upload_authorize_answer(socket);
        } else {
            write_answer(socket, "200 OK", "", &json!({"ok": true}));
        }
    });
    let (target, _root, dir) = credentialed(&listener);
    let start = Instant::now();
    let out = upload_file_in(&dir, &target, "a.pdf", "a.pdf", b"pdf", true, 1, |_| {});
    let elapsed = start.elapsed();
    assert!(
        out.is_err(),
        "slow upload authorization unexpectedly succeeded"
    );
    assert!(
        elapsed < Duration::from_millis(2300),
        "1s upload budget took {elapsed:?}"
    );
    server.join().unwrap();
    assert_eq!(*paths.lock().unwrap(), ["/__platform/wiki/authorize"]);
}

#[test]
fn upload_total_deadline_covers_response_body() {
    let (listener, paths, server) = upload_deadline_worker(|path, socket, _raw| {
        if path == "/__platform/wiki/authorize" {
            upload_authorize_answer(socket);
        } else {
            let _ = write!(socket, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{{");
            let _ = socket.flush();
            thread::sleep(Duration::from_secs(3));
            let _ = socket.write_all(b"\"ok\":true}");
        }
    });
    let (target, _root, dir) = credentialed(&listener);
    let start = Instant::now();
    let out = upload_file_in(&dir, &target, "a.pdf", "a.pdf", b"pdf", true, 1, |_| {});
    let elapsed = start.elapsed();
    assert!(
        out.is_err(),
        "slow upload response body unexpectedly succeeded"
    );
    assert!(
        elapsed < Duration::from_millis(2300),
        "1s upload budget took {elapsed:?}"
    );
    server.join().unwrap();
    assert_eq!(
        *paths.lock().unwrap(),
        ["/__platform/wiki/authorize", "/__platform/wiki/upload"]
    );
}

#[test]
fn upload_total_deadline_includes_overwrite_preflight() {
    let wiki_authorizes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let authorizes = wiki_authorizes.clone();
    let (listener, paths, server) = upload_deadline_worker(move |path, socket, _raw| match path {
        "/__platform/wiki/authorize"
            if authorizes.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 =>
        {
            upload_authorize_answer(socket);
        }
        "/__platform/wiki/authorize" => {
            thread::sleep(Duration::from_secs(3));
            upload_authorize_answer(socket);
        }
        "/__platform/wiki/call" => {
            thread::sleep(Duration::from_millis(1200));
            write_answer(socket, "200 OK", "", &json!({"path":"a", "entries":[]}));
        }
        _ => write_answer(socket, "200 OK", "", &json!({"ok": true})),
    });
    let (target, _root, dir) = credentialed(&listener);
    let start = Instant::now();
    let out = upload_file_in(&dir, &target, "a/a.pdf", "a.pdf", b"pdf", false, 2, |_| {});
    let elapsed = start.elapsed();
    assert!(out.is_err(), "upload exceeded the shared preflight budget");
    assert!(
        elapsed < Duration::from_millis(2700),
        "2s total upload budget took {elapsed:?}"
    );
    server.join().unwrap();
    let paths = paths.lock().unwrap();
    assert_eq!(paths[0], "/__platform/wiki/authorize");
    assert_eq!(paths[1], "/__platform/wiki/call");
    assert_eq!(paths[2], "/__platform/wiki/authorize");
    assert_eq!(paths.len(), 3, "timeout must not send or replay an upload");
}

#[test]
fn a_slow_authorize_is_capped_by_the_wake_budget() {
    let (listener, seen, server) = finite_worker(1, |_i, socket, _req| {
        thread::sleep(Duration::from_secs(3));
        write_answer(
            socket,
            "200 OK",
            "",
            &json!({"version": CLI_VERSION, "envelope": "h.e.1"}),
        );
    });
    let (target, _root, dir) = credentialed(&listener);
    let start = Instant::now();
    let out = call_verb_in(&target, "status", Map::new(), 1, &dir, |_| {}, |_| {});
    let elapsed = start.elapsed();
    assert!(out.is_err());
    assert!(
        elapsed < Duration::from_secs(2),
        "a 1s budget let authorize run {elapsed:?}"
    );
    server.join().unwrap();
    let reqs = seen.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].path, "/__platform/cli/authorize");
}

#[test]
fn authorize_and_call_share_one_deadline() {
    let (listener, seen, server) = finite_worker(2, |i, socket, _req| {
        if i == 0 {
            thread::sleep(Duration::from_secs(1));
            write_answer(
                socket,
                "200 OK",
                "",
                &json!({"version": CLI_VERSION, "envelope": "h.e.1"}),
            );
        } else {
            thread::sleep(Duration::from_secs(3));
            write_answer(socket, "200 OK", "", &json!({"ok": true}));
        }
    });
    let (target, _root, dir) = credentialed(&listener);
    let start = Instant::now();
    let out = call_verb_in(&target, "status", Map::new(), 2, &dir, |_| {}, |_| {});
    let elapsed = start.elapsed();
    assert!(out.is_err());
    assert!(
        elapsed < Duration::from_millis(2750),
        "authorize+call did not share the 2s deadline: {elapsed:?}"
    );
    server.join().unwrap();
    let reqs = seen.lock().unwrap();
    assert_eq!(reqs.len(), 2);
    assert!(check_authorize(&reqs[0]), "authorize schema");
    assert!(check_call(&reqs[1]), "call schema");
}

#[test]
fn call_response_body_is_bounded_by_remaining_deadline() {
    let (listener, _seen, server) = finite_worker(2, |i, socket, _req| {
        if i == 0 {
            write_answer(
                socket,
                "200 OK",
                "",
                &json!({"version": CLI_VERSION, "envelope": "h.e.1"}),
            );
        } else {
            let body = serde_json::to_vec(&json!({"ok": true})).unwrap();
            let _ = write!(
                socket,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(&body[..1]);
            let _ = socket.flush();
            thread::sleep(Duration::from_secs(3));
            let _ = socket.write_all(&body[1..]);
        }
    });
    let (target, _root, dir) = credentialed(&listener);
    let start = Instant::now();
    let out = call_verb_in(&target, "status", Map::new(), 1, &dir, |_| {}, |_| {});
    let elapsed = start.elapsed();
    assert!(out.is_err());
    assert!(
        elapsed < Duration::from_secs(2),
        "a 1s budget let the body read run {elapsed:?}"
    );
    server.join().unwrap();
}

#[test]
fn a_zero_budget_never_sends_a_byte() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let (target, _root, dir) = credentialed(&listener);
    let out = call_verb_in(&target, "status", Map::new(), 0, &dir, |_| {}, |_| {});
    assert!(out.is_err());
    assert!(
        matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "a dead budget opened a socket"
    );
}

#[test]
fn a_shared_expired_deadline_cannot_restart_the_wake_budget() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let (target, _root, dir) = credentialed(&listener);
    let deadline = Instant::now() - Duration::from_secs(1);
    let error = call_verb_until(
        &target,
        "read",
        Map::new(),
        deadline,
        60,
        &dir,
        |_| {},
        |_| {},
    )
    .unwrap_err();
    assert_eq!(error.kind(), "busy");
    assert_eq!(error.code(), Some("waking"));
    assert!(
        matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "a continuation restarted its expired budget"
    );
}

#[test]
fn a_wake_sleep_that_would_pass_the_deadline_is_the_retryable_busy() {
    let (out, _requests) = run(
        vec![
            (503, "", json!({"state": "waking", "retry_after_s": 5})),
            (503, "", json!({"state": "waking", "retry_after_s": 5})),
        ],
        "status",
        Map::new(),
        2,
    );
    let err = out.unwrap_err();
    assert_eq!(err.kind(), "busy");
    assert_eq!(err.code(), Some("waking"));
}

#[test]
fn transfer_ops_stay_on_write_or_job_status_and_refuse_authority() {
    let begin = json!({
        "path": "projects/example",
        "transfer": {
            "op": "begin",
            "manifest": {"version": 1, "root": "projects/example", "entries": []}
        }
    });
    assert!(wiki_transfer_shape("write", begin.as_object().unwrap()).is_ok());
    let mut mixed = begin.as_object().unwrap().clone();
    mixed.insert("text".into(), json!("no"));
    let err = wiki_transfer_shape("write", &mixed).unwrap_err();
    assert!(err.to_string().contains("never sent"), "{err}");
    let export = json!({
        "path": "projects/example/a.md",
        "transfer": {"op": "export", "path": "projects/example/a.md"}
    });
    assert!(wiki_transfer_shape("job_status", export.as_object().unwrap()).is_ok());
    let outside = json!({
        "path": "projects/example",
        "transfer": {"op": "export", "path": "projects/example/a.md"}
    });
    let err = wiki_transfer_shape("job_status", outside.as_object().unwrap()).unwrap_err();
    assert!(err.to_string().contains("outer path"), "{err}");
    let binding = json!({
        "path": "projects/example",
        "transfer": {"op": "status", "root": "projects/example", "_transfer_binding": {}}
    });
    let err = wiki_transfer_shape("job_status", binding.as_object().unwrap()).unwrap_err();
    assert!(err.to_string().contains("never sent"), "{err}");
    let forged = json!({
        "path": "projects/example",
        "transfer": {"op": "status", "actor": "operator"}
    });
    let err = wiki_transfer_shape("job_status", forged.as_object().unwrap()).unwrap_err();
    assert!(err.to_string().contains("never sent"), "{err}");
    let err = wiki_transfer_shape(
        "job_status",
        json!({"path": "projects/example", "transfer": {"op": "begin"}})
            .as_object()
            .unwrap(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("not allowed"), "{err}");
}
