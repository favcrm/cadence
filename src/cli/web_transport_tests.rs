//! CAD-1429: CLIENT UNIT wire and recovery tests for `cadence web` against the
//! pinned `/v1/web-sites` wire. A loopback server replays the contract's
//! success and refusal envelopes and records each request. The real
//! `web_request` path, its credential load, the `Authorization` bearer, the
//! no-redirect rule and the envelope unwrap all run unchanged. Nothing here is
//! written to the real config dir: every test points `XDG_CONFIG_HOME` at a
//! private temp dir.
//!
//! Scope, stated plainly: these are CLIENT UNIT tests. The bearer is the saved
//! fixture credential, NOT a hosted-issued credential. They prove the client's
//! wire and its local recovery bookkeeping against the contract's shapes. They
//! are not full-stack, not a built-binary run, not TLS acceptance, and not a
//! separate-process restart. Recovery "fresh invocations" below are in-process
//! calls to `run` that reload the pending record from disk; a real separate
//! process restart remains unproven.
//!
//! Compiled into the library unit-test binary through `web_cli_test_harness`,
//! so the production `web.rs` functions run unchanged.

use super::web::{run, WebAction};
use cadence_agent::remote_cli::RemoteTarget;
use cadence_agent::web_publish;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

const ORG: &str = "acme";
const ORG_ID: &str = "ws_acme";
const BEARER: &str = "hct_WEBWEBWEBWEBWEBWEBWEBWEBWEBWEBWEBWEBWEBWEBW";
const SITE: &str = "site_11111111";
const UPLOAD: &str = "upl_22222222";
const RELEASE: &str = "rel_33333333";

/// One request the loopback server saw.
#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

/// A handler answers `(status, json)`. Status `0` drops the connection without
/// writing any reply: a real lost reply, not a synthesized error envelope.
type Handler = Arc<dyn Fn(&Seen) -> (u16, Value) + Send + Sync>;

/// Loopback server: replays `handler`'s `(status, json)` and records requests.
fn serve(handler: Handler) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let thread_log = Arc::clone(&log);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let seen = read_request(&mut stream);
            let (status, body) = handler(&seen);
            thread_log.lock().unwrap().push(seen);
            if status == 0 {
                // Lost reply: close without answering.
                continue;
            }
            let text = body.to_string();
            let reply = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    (addr, log)
}

fn read_request(stream: &mut std::net::TcpStream) -> Seen {
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
        if let Some(end) = find(&raw, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&raw[..end]).to_string();
            let length = head
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            let have = raw.len() - (end + 4);
            if have >= length {
                break;
            }
        }
    }
    let end = find(&raw, b"\r\n\r\n").unwrap_or(raw.len());
    let head = String::from_utf8_lossy(&raw[..end]).to_string();
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or_default().split(' ');
    let method = first.next().unwrap_or_default().to_string();
    let path = first.next().unwrap_or_default().to_string();
    let headers = lines
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            Some((k.trim().to_ascii_lowercase(), v.trim().to_string()))
        })
        .collect();
    let body = raw[(end + 4).min(raw.len())..].to_vec();
    Seen {
        method,
        path,
        headers,
        body,
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The board audience a saved login is bound to. It is never the issuer.
const BOARD_AUDIENCE: &str = "https://acme.cadencecloud.app";

/// A private config dir holding the owner-only `cli-acme.json` the login writes:
/// the verified public `issuer` (the platform origin web management uses), the
/// board `audience`, and the trusted-issuer pin the login requires.
fn credential_dir(issuer: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let auth = root.path().join("cadence/remote-auth");
    std::fs::create_dir_all(&auth).unwrap();
    std::fs::set_permissions(&auth, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let body = json!({
        "version": "hosted-cadence-auth.v1",
        "organization_id": ORG_ID,
        "organization_slug": ORG,
        "issuer": issuer,
        "audience": BOARD_AUDIENCE,
        "capabilities": ["cli.read", "cli.write", "web.read", "web.prepare"],
        "access_token": BEARER,
        "expires_at": 4_102_444_800u64,
    });
    write_private(
        &auth.join(format!("cli-{ORG}.json")),
        body.to_string().as_bytes(),
    );
    write_private(
        &auth.join("trusted-issuer"),
        format!("{issuer}\n").as_bytes(),
    );
    root
}

fn write_private(path: &Path, bytes: &[u8]) {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

fn target(_issuer: &str) -> RemoteTarget {
    RemoteTarget {
        org: ORG.into(),
        endpoint: BOARD_AUDIENCE.into(),
        org_id: ORG_ID.into(),
    }
}

/// Run `f` with `XDG_CONFIG_HOME` pointed at `config`, restoring it after.
/// Tests here are single-threaded per process section; the lock keeps the env
/// swap from racing a sibling test in this module.
fn with_config<T>(config: &Path, f: impl FnOnce() -> T) -> T {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prior = std::env::var_os("XDG_CONFIG_HOME");
    std::env::set_var("XDG_CONFIG_HOME", config);
    let out = f();
    match prior {
        Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
        None => std::env::remove_var("XDG_CONFIG_HOME"),
    }
    out
}

fn tiny_site(root: &Path) -> PathBuf {
    let site = root.join("site");
    std::fs::create_dir_all(site.join("assets")).unwrap();
    std::fs::write(site.join("index.html"), "<h1>hello</h1>").unwrap();
    std::fs::write(site.join("assets/app.css"), "body{}").unwrap();
    site
}

/// A built-React shaped dist: nested hashed assets, the shape the transport
/// guard admits as a canonical `:encodedPath` segment.
fn nested_site(root: &Path) -> PathBuf {
    let site = root.join("dist");
    std::fs::create_dir_all(site.join("assets/css")).unwrap();
    std::fs::write(site.join("index.html"), "<h1>built</h1>").unwrap();
    std::fs::write(site.join("assets/app-abc123.js"), "console.log(1)").unwrap();
    std::fs::write(site.join("assets/css/site.css"), "h1{margin:0}").unwrap();
    site
}

/// The server half of a successful prepare: an upload with the given missing
/// paths, accepted PUTs, and a ready release. The create echoes the manifest
/// digest the client sent (the host recomputes and returns it), and finalize
/// returns that same digest, as the host does.
fn prepare_handler(missing: Vec<&'static str>) -> Handler {
    let digest: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    Arc::new(
        move |seen: &Seen| match (seen.method.as_str(), seen.path.as_str()) {
            ("POST", "/v1/web-sites/uploads") => {
                let sent: Value = serde_json::from_slice(&seen.body).unwrap_or(Value::Null);
                let sent_digest = sent["manifestDigest"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                *digest.lock().unwrap() = sent_digest.clone();
                (
                    200,
                    json!({"ok": true, "data": {
                    "siteId": SITE, "uploadId": UPLOAD,
                    "manifestDigest": sent_digest, "missingPaths": missing,
                    "expiresAt": "2030-01-01T00:00:00.000Z"}}),
                )
            }
            ("PUT", p) if p.starts_with("/v1/web-sites/uploads/upl_22222222/files/") => (
                200,
                json!({"ok": true, "data": {"path": "x", "verified": true}}),
            ),
            ("POST", "/v1/web-sites/uploads/upl_22222222/finalize") => (
                200,
                json!({"ok": true, "data": {
                "releaseId": RELEASE,
                "manifestDigest": digest.lock().unwrap().clone(),
                "state": "ready"}}),
            ),
            _ => (
                404,
                json!({"ok": false, "error": {"code": "not_found", "message": "no"}}),
            ),
        },
    )
}

#[test]
fn prepare_uploads_then_finalizes_the_pinned_wire() {
    let root = tempfile::tempdir().unwrap();
    let site = tiny_site(root.path());
    // The server asks for both files; the client must PUT each with its digest.
    let (addr, log) = serve(prepare_handler(vec!["index.html", "assets/app.css"]));
    let creds = credential_dir(&addr);
    let action = WebAction::Prepare {
        input: site,
        name: "my-site".into(),
        spa: false,
    };
    let result = with_config(creds.path(), || run(&action, &target(&addr), 5));
    assert_eq!(result.expect("prepare failed"), 0);

    let seen = log.lock().unwrap().clone();
    // 1 create + 2 PUT + 1 finalize, in that order.
    assert_eq!(seen.len(), 4, "requests: {seen:?}");
    let create = &seen[0];
    assert_eq!(create.method, "POST");
    assert_eq!(create.path, "/v1/web-sites/uploads");
    let body: Value = serde_json::from_slice(&create.body).unwrap();
    assert_eq!(body["version"], "1");
    assert_eq!(body["workspaceId"], ORG_ID);
    assert_eq!(body["slug"], "my-site");
    assert!(body["manifestDigest"].as_str().unwrap().len() == 64);
    assert!(body["requestKey"]
        .as_str()
        .unwrap()
        .starts_with("web-prepare-"));
    assert!(
        body.get("spaFallback").is_none(),
        "default must omit spaFallback"
    );
    assert_eq!(body["manifest"]["version"], "1");
    assert_eq!(body["manifest"]["files"][0]["path"], "assets/app.css");
    assert_eq!(
        body["manifest"]["files"][1]["mime"],
        "text/html; charset=utf-8"
    );
    assert_eq!(create.headers["authorization"], format!("Bearer {BEARER}"));

    for put in &seen[1..3] {
        assert_eq!(put.method, "PUT");
        assert!(put
            .path
            .starts_with(&format!("/v1/web-sites/uploads/{UPLOAD}/files/")));
        assert_eq!(put.headers["x-workspace-id"], ORG_ID);
        assert_eq!(put.headers["x-content-sha256"].len(), 64);
        assert_eq!(put.headers["authorization"], format!("Bearer {BEARER}"));
    }
    // The host lists what is missing; the client uploads exactly those, in
    // the order it lists them, each with its own bytes.
    assert_eq!(
        seen[1].path,
        format!("/v1/web-sites/uploads/{UPLOAD}/files/index.html")
    );
    // A nested artifact is one canonical `:encodedPath` segment: `/` is %2F.
    assert_eq!(
        seen[2].path,
        format!("/v1/web-sites/uploads/{UPLOAD}/files/assets%2Fapp.css")
    );
    assert_eq!(seen[1].body, b"<h1>hello</h1>");
    assert_eq!(seen[2].body, b"body{}");

    let finalize = &seen[3];
    assert_eq!(finalize.method, "POST");
    assert_eq!(
        finalize.path,
        format!("/v1/web-sites/uploads/{UPLOAD}/finalize")
    );
    assert_eq!(finalize.headers["x-workspace-id"], ORG_ID);
}

#[test]
fn spa_flag_is_sent_only_when_opted_in() {
    let root = tempfile::tempdir().unwrap();
    let site = tiny_site(root.path());
    let (addr, log) = serve(prepare_handler(Vec::new()));
    let creds = credential_dir(&addr);
    let action = WebAction::Prepare {
        input: site,
        name: "my-site".into(),
        spa: true,
    };
    with_config(creds.path(), || run(&action, &target(&addr), 5)).unwrap();
    let body: Value = serde_json::from_slice(&log.lock().unwrap()[0].body).unwrap();
    assert_eq!(body["spaFallback"], true);
}

#[test]
fn a_refused_upload_stops_before_finalize_and_nothing_goes_live() {
    let root = tempfile::tempdir().unwrap();
    let site = tiny_site(root.path());
    let (addr, log) = serve(Arc::new(|seen: &Seen| {
        if seen.method == "POST" && seen.path == "/v1/web-sites/uploads" {
            return (
                200,
                json!({"ok": true, "data": {
                    "siteId": SITE, "uploadId": UPLOAD, "manifestDigest": "x",
                    "missingPaths": ["index.html"], "expiresAt": "2030-01-01T00:00:00.000Z"}}),
            );
        }
        (
            400,
            json!({"ok": false, "error": {"code": "file_digest_mismatch", "message": "bad"}}),
        )
    }));
    let creds = credential_dir(&addr);
    let action = WebAction::Prepare {
        input: site,
        name: "my-site".into(),
        spa: false,
    };
    let err = with_config(creds.path(), || run(&action, &target(&addr), 5)).unwrap_err();
    assert!(err.to_string().contains("file_digest_mismatch"), "{err}");
    let seen = log.lock().unwrap().clone();
    assert!(
        seen.iter().all(|s| !s.path.ends_with("/finalize")),
        "finalize was sent after a refused upload: {seen:?}"
    );
}

#[test]
fn a_bearer_never_appears_in_the_outcome_text() {
    let root = tempfile::tempdir().unwrap();
    let site = tiny_site(root.path());
    let (addr, _log) = serve(Arc::new(|_: &Seen| {
        (
            401,
            json!({"ok": false, "error": {"code": "unauthorized", "message": "no"}}),
        )
    }));
    let creds = credential_dir(&addr);
    let action = WebAction::Prepare {
        input: site,
        name: "my-site".into(),
        spa: false,
    };
    let err = with_config(creds.path(), || run(&action, &target(&addr), 5)).unwrap_err();
    let text = err.to_string();
    assert!(!text.contains(BEARER), "bearer leaked: {text}");
    assert!(text.contains("unauthorized"), "{text}");
}

/// The identity the fixture credential verifies as. A pending record is bound
/// to this exactly as `save_pending` binds the live identity.
fn fixture_identity(issuer: &str) -> Value {
    json!({
        "org": ORG,
        "organization_id": ORG_ID,
        "audience": BOARD_AUDIENCE,
        "issuer": issuer,
    })
}

fn fixture_issuer(creds: &Path) -> String {
    std::fs::read_to_string(creds.join("cadence/remote-auth/trusted-issuer"))
        .unwrap()
        .trim()
        .to_string()
}

/// One successful prepare's durable `done` intent for `digest`.
fn done_prepare(digest: &str) -> Value {
    json!({
        "kind": "prepare", "slug": "my-site", "digest": digest, "spa": false,
        "request_key": "web-prepare-seed0001",
        "site_id": SITE, "upload_id": UPLOAD, "state": "done"
    })
}

/// Write the durable `PendingRecord { identity, intents, releases }` for ORG.
/// `identity` is the binding to check; `intents` are the recorded intents.
fn write_record(creds: &Path, identity: Value, intents: Vec<Value>) {
    let record = json!({
        "identity": identity,
        "intents": intents,
        "releases": [{
            "slug": "my-site", "site_id": SITE,
            "release_id": RELEASE, "manifest_digest": "a".repeat(64)
        }]
    });
    write_private(
        &creds
            .join("cadence/remote-auth")
            .join(format!("web-{ORG}.json")),
        record.to_string().as_bytes(),
    );
}

/// A ready release must exist in the local pending record before publish can
/// name it. Seed the record the way a successful prepare leaves it.
fn seed_ready_release(creds: &Path) {
    let issuer = fixture_issuer(creds);
    write_record(
        creds,
        fixture_identity(&issuer),
        vec![done_prepare(&"a".repeat(64))],
    );
}

fn read_record(creds: &Path) -> Value {
    let raw = std::fs::read(
        creds
            .join("cadence/remote-auth")
            .join(format!("web-{ORG}.json")),
    )
    .unwrap();
    serde_json::from_slice(&raw).unwrap()
}

fn action_handler(state: &'static str, approval: Value) -> Handler {
    Arc::new(
        move |seen: &Seen| match (seen.method.as_str(), seen.path.as_str()) {
            ("GET", p) if p == format!("/v1/web-sites/sites/{SITE}") => (
                200,
                json!({"ok": true, "data": {"siteId": SITE, "slug": "my-site", "revision": 4}}),
            ),
            ("POST", p) if p == format!("/v1/web-sites/sites/{SITE}/actions") => {
                let mut action = json!({
                    "actionId": "act_44444444", "siteId": SITE, "operation": "publish",
                    "state": state, "releaseId": RELEASE, "expectedRevision": 4,
                    "resultRevision": null, "manifestDigest": "a".repeat(64),
                    "liveUrl": null, "approvalUrl": null,
                    "approvalExpiresAt": null, "updatedAt": "2030-01-01T00:00:00.000Z"
                });
                if !approval.is_null() {
                    action["approvalUrl"] = approval.clone();
                }
                (201, json!({"ok": true, "data": action}))
            }
            _ => (
                404,
                json!({"ok": false, "error": {"code": "not_found", "message": "no"}}),
            ),
        },
    )
}

#[test]
fn publish_creates_one_action_with_a_durable_key_and_no_approval_link() {
    let (addr, log) = serve(action_handler("awaiting_approval", Value::Null));
    let creds = credential_dir(&addr);
    seed_ready_release(creds.path());
    let action = WebAction::Publish {
        release: RELEASE.into(),
    };
    let code = with_config(creds.path(), || run(&action, &target(&addr), 5)).unwrap();
    assert_eq!(code, 0);
    let seen = log.lock().unwrap().clone();
    let create = seen
        .iter()
        .find(|s| s.method == "POST" && s.path.ends_with("/actions"))
        .expect("no action request was sent");
    let body: Value = serde_json::from_slice(&create.body).unwrap();
    assert_eq!(body["operation"], "publish");
    assert_eq!(body["releaseId"], RELEASE);
    assert_eq!(body["expectedRevision"], 4);
    assert_eq!(body["version"], "1");
    let key = body["requestKey"].as_str().unwrap().to_string();
    assert!(key.starts_with("web-publish-"));
    // The key was written to the pending record before the request left.
    let record = std::fs::read_to_string(
        creds
            .path()
            .join("cadence/remote-auth")
            .join(format!("web-{ORG}.json")),
    )
    .unwrap();
    assert!(record.contains(&key), "request key was not persisted first");
}

#[test]
fn a_foreign_release_is_refused_before_any_request() {
    let (addr, log) = serve(action_handler("awaiting_approval", Value::Null));
    let creds = credential_dir(&addr);
    seed_ready_release(creds.path());
    let action = WebAction::Publish {
        release: "rel_99999999".into(),
    };
    let err = with_config(creds.path(), || run(&action, &target(&addr), 5)).unwrap_err();
    assert!(err.to_string().contains("not a prepared release"), "{err}");
    assert!(
        log.lock().unwrap().is_empty(),
        "a request was sent for a foreign release"
    );
}

#[test]
fn null_approval_url_is_reported_as_unavailable_not_as_a_link() {
    let (addr, _log) = serve(action_handler("awaiting_approval", Value::Null));
    let creds = credential_dir(&addr);
    seed_ready_release(creds.path());
    let action = WebAction::Publish {
        release: RELEASE.into(),
    };
    // Captures stdout is not needed: the command returns Ok only while pending.
    let code = with_config(creds.path(), || run(&action, &target(&addr), 5)).unwrap();
    assert_eq!(code, 0);
    // The honest machine state: no fabricated URL. The record binds the issuer
    // legitimately, so only the intents are checked: they store no URL at all.
    let record = read_record(creds.path());
    let intents = record["intents"].to_string();
    assert!(
        !intents.contains("https://"),
        "a URL was fabricated in the intents: {intents}"
    );
    assert!(
        !intents.contains("liveUrl") && !intents.contains("approvalUrl"),
        "a live or approval URL field was recorded: {intents}"
    );
}

/// An unknown first reply (a 503) is reconciled by the same key and body. The
/// second answer succeeds, and both requests carry the identical request key.
#[test]
fn an_unknown_action_reply_reconciles_with_the_same_key_only() {
    let posts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&posts);
    let (addr, log) = serve(Arc::new(move |seen: &Seen| {
        match (seen.method.as_str(), seen.path.as_str()) {
            ("GET", p) if p == format!("/v1/web-sites/sites/{SITE}") => (
                200,
                json!({"ok": true, "data": {"siteId": SITE, "slug": "my-site", "revision": 4}}),
            ),
            ("POST", p) if p == format!("/v1/web-sites/sites/{SITE}/actions") => {
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    (
                        503,
                        json!({"ok": false, "error": {"code": "storage_unavailable", "message": "x"}}),
                    )
                } else {
                    (
                        201,
                        json!({"ok": true, "data": {
                        "actionId": "act_44444444", "siteId": SITE, "operation": "publish",
                        "state": "awaiting_approval", "releaseId": RELEASE,
                        "expectedRevision": 4, "resultRevision": null,
                        "manifestDigest": "a".repeat(64), "liveUrl": null,
                        "approvalUrl": null, "approvalExpiresAt": null,
                        "updatedAt": "2030-01-01T00:00:00.000Z"}}),
                    )
                }
            }
            _ => (
                404,
                json!({"ok": false, "error": {"code": "not_found", "message": "no"}}),
            ),
        }
    }));
    let creds = credential_dir(&addr);
    seed_ready_release(creds.path());
    let action = WebAction::Publish {
        release: RELEASE.into(),
    };
    let code = with_config(creds.path(), || run(&action, &target(&addr), 5)).unwrap();
    assert_eq!(code, 0);
    let actions: Vec<Value> = log
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.method == "POST" && s.path.ends_with("/actions"))
        .map(|s| serde_json::from_slice(&s.body).unwrap())
        .collect();
    assert_eq!(actions.len(), 2, "expected exactly one reconciling retry");
    assert_eq!(
        actions[0], actions[1],
        "the retry changed the body or the key"
    );
}

/// A status lookup goes through the pinned slug route, scoped to the workspace.
#[test]
fn status_reads_the_scoped_slug_route() {
    let (addr, log) = serve(Arc::new(|seen: &Seen| {
        if seen.method == "GET" && seen.path == "/v1/web-sites/sites/by-slug/my-site" {
            return (
                200,
                json!({"ok": true, "data": {
                "siteId": SITE, "slug": "my-site", "revision": 4,
                "liveReleaseId": null, "liveUrl": null, "suspended": false}}),
            );
        }
        (
            404,
            json!({"ok": false, "error": {"code": "not_found", "message": "no"}}),
        )
    }));
    let creds = credential_dir(&addr);
    let action = WebAction::Status {
        slug: "my-site".into(),
    };
    assert_eq!(
        with_config(creds.path(), || run(&action, &target(&addr), 5)).unwrap(),
        0
    );
    let seen = log.lock().unwrap().clone();
    assert_eq!(seen[0].path, "/v1/web-sites/sites/by-slug/my-site");
    assert_eq!(seen[0].headers["x-workspace-id"], ORG_ID);
}

// ---------------------------------------------------------------------------
// Focused client recovery and wire tests (CLIENT UNIT; in-process reloads).
// ---------------------------------------------------------------------------

/// Nested built assets stream through `run(prepare)` as canonical encoded
/// segments: `assets/app-abc123.js` and `assets/css/site.css` each travel as a
/// single `%2F`-encoded path, with their own bytes and digests.
#[test]
fn nested_dist_assets_stream_through_prepare_as_canonical_segments() {
    let root = tempfile::tempdir().unwrap();
    let site = nested_site(root.path());
    let (addr, log) = serve(prepare_handler(vec![
        "assets/app-abc123.js",
        "assets/css/site.css",
    ]));
    let creds = credential_dir(&addr);
    let action = WebAction::Prepare {
        input: site,
        name: "my-site".into(),
        spa: false,
    };
    with_config(creds.path(), || run(&action, &target(&addr), 5)).expect("prepare failed");

    let seen = log.lock().unwrap().clone();
    assert_eq!(seen.len(), 4, "requests: {seen:?}");
    assert_eq!(
        seen[1].path,
        format!("/v1/web-sites/uploads/{UPLOAD}/files/assets%2Fapp-abc123.js")
    );
    assert_eq!(
        seen[2].path,
        format!("/v1/web-sites/uploads/{UPLOAD}/files/assets%2Fcss%2Fsite.css")
    );
    assert_eq!(seen[1].body, b"console.log(1)");
    assert_eq!(seen[2].body, b"h1{margin:0}");
    // The create's manifest carries the nested contract paths verbatim.
    let body: Value = serde_json::from_slice(&seen[0].body).unwrap();
    let paths: Vec<&str> = body["manifest"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"assets/app-abc123.js"), "{paths:?}");
    assert!(paths.contains(&"assets/css/site.css"), "{paths:?}");
}

/// A lost first upload-create reply (connection closed, no answer): the
/// original request key is written to disk BEFORE the first POST leaves, and a
/// later invocation reuses that same key and digest for the same upload.
#[test]
fn a_lost_upload_create_reply_persists_the_key_before_send_and_resumes_it() {
    let root = tempfile::tempdir().unwrap();
    let site = tiny_site(root.path());
    let record_path: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
    let on_disk_at_first_post: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let first_post_dropped = Arc::new(AtomicBool::new(false));
    let inner = prepare_handler(vec!["index.html", "assets/app.css"]);
    let (rp, snap, dropped) = (
        Arc::clone(&record_path),
        Arc::clone(&on_disk_at_first_post),
        Arc::clone(&first_post_dropped),
    );
    let (addr, log) = serve(Arc::new(move |seen: &Seen| {
        if seen.method == "POST"
            && seen.path == "/v1/web-sites/uploads"
            && !dropped.swap(true, Ordering::SeqCst)
        {
            // Snapshot the record as it stands the instant the first create
            // arrives: it must already hold the key this request carries.
            if let Some(path) = rp.lock().unwrap().clone() {
                *snap.lock().unwrap() = std::fs::read_to_string(path).ok();
            }
            return (0, Value::Null);
        }
        inner(seen)
    }));
    let creds = credential_dir(&addr);
    *record_path.lock().unwrap() = Some(
        creds
            .path()
            .join("cadence/remote-auth")
            .join(format!("web-{ORG}.json")),
    );
    let action = WebAction::Prepare {
        input: site,
        name: "my-site".into(),
        spa: false,
    };

    // First invocation: the create reply is lost.
    let first = with_config(creds.path(), || run(&action, &target(&addr), 5));
    assert!(
        first.is_err(),
        "a lost create reply must not report success"
    );
    let first_create: Value = {
        let seen = log.lock().unwrap().clone();
        serde_json::from_slice(&seen[0].body).unwrap()
    };
    let key = first_create["requestKey"].as_str().unwrap().to_string();
    assert!(key.starts_with("web-prepare-"), "{key}");

    let snapshot = on_disk_at_first_post
        .lock()
        .unwrap()
        .clone()
        .expect("the record was not on disk when the first create arrived");
    assert!(
        snapshot.contains(&key),
        "the key was not persisted before the first send: {snapshot}"
    );
    let after_loss = read_record(creds.path());
    let open: Vec<&Value> = after_loss["intents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["state"] == "open" && i["kind"] == "prepare")
        .collect();
    assert_eq!(open.len(), 1, "{after_loss}");
    assert_eq!(open[0]["request_key"], key.as_str());
    assert!(open[0]["site_id"].is_null(), "no host id before a reply");

    // Second invocation (reloads the record from disk): same key, same digest.
    let second = with_config(creds.path(), || run(&action, &target(&addr), 5));
    assert_eq!(second.expect("resumed prepare failed"), 0);
    let creates: Vec<Value> = log
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.method == "POST" && s.path == "/v1/web-sites/uploads")
        .map(|s| serde_json::from_slice(&s.body).unwrap())
        .collect();
    let last = creates.last().unwrap();
    assert_eq!(
        last["requestKey"],
        key.as_str(),
        "the resume changed the key"
    );
    assert_eq!(
        last["manifestDigest"], first_create["manifestDigest"],
        "the resume changed the digest"
    );
}

/// An open prepare intent for the same slug and digest resumes its original
/// key on the next invocation. A different `--spa` setting is refused before any
/// request: the intent is never reused for a different request.
#[test]
fn an_open_prepare_resumes_its_key_and_refuses_a_different_spa_setting() {
    let root = tempfile::tempdir().unwrap();
    let site = tiny_site(root.path());
    let digest = web_publish::freeze(&site).unwrap().digest;
    let (addr, log) = serve(prepare_handler(Vec::new()));
    let creds = credential_dir(&addr);
    let issuer = fixture_issuer(creds.path());
    write_record(
        creds.path(),
        fixture_identity(&issuer),
        vec![json!({
            "kind": "prepare", "slug": "my-site", "digest": digest, "spa": false,
            "request_key": "web-prepare-openkey01", "state": "open"
        })],
    );

    // A different SPA setting is refused before any request is sent.
    let mismatch = WebAction::Prepare {
        input: site.clone(),
        name: "my-site".into(),
        spa: true,
    };
    let err = with_config(creds.path(), || run(&mismatch, &target(&addr), 5)).unwrap_err();
    assert!(err.to_string().contains("different --spa"), "{err}");
    assert!(
        log.lock().unwrap().is_empty(),
        "a request was sent for a mismatched open intent"
    );

    // The same setting resumes the original key.
    let resume = WebAction::Prepare {
        input: site,
        name: "my-site".into(),
        spa: false,
    };
    assert_eq!(
        with_config(creds.path(), || run(&resume, &target(&addr), 5)).unwrap(),
        0
    );
    let create: Value = serde_json::from_slice(&log.lock().unwrap()[0].body).unwrap();
    assert_eq!(create["requestKey"], "web-prepare-openkey01");
    assert_eq!(create["manifestDigest"], digest.as_str());
}

/// A pending record bound to a different issuer fails closed before any request.
#[test]
fn a_pending_record_from_another_issuer_fails_closed_before_any_request() {
    let (addr, log) = serve(action_handler("awaiting_approval", Value::Null));
    let creds = credential_dir(&addr);
    write_record(
        creds.path(),
        fixture_identity("https://other.example.test"),
        vec![done_prepare(&"a".repeat(64))],
    );
    let action = WebAction::Publish {
        release: RELEASE.into(),
    };
    let err = with_config(creds.path(), || run(&action, &target(&addr), 5)).unwrap_err();
    assert!(err.to_string().contains("belongs to another"), "{err}");
    assert!(log.lock().unwrap().is_empty(), "a request was sent");
}

/// A publish that loses both create replies stays open with its original key,
/// body and expectedRevision. A later invocation, after the server's revision
/// has moved on, resends that same body and does NOT re-read the revision.
#[test]
fn a_fresh_invocation_reuses_the_original_action_body_when_the_revision_moved() {
    let revision = Arc::new(AtomicI64::new(4));
    let lose_replies = Arc::new(AtomicBool::new(true));
    let (rev, lose) = (Arc::clone(&revision), Arc::clone(&lose_replies));
    let (addr, log) = serve(Arc::new(move |seen: &Seen| {
        match (seen.method.as_str(), seen.path.as_str()) {
            ("GET", p) if p == format!("/v1/web-sites/sites/{SITE}") => (
                200,
                json!({"ok": true, "data": {"siteId": SITE, "slug": "my-site",
                    "revision": rev.load(Ordering::SeqCst)}}),
            ),
            ("POST", p) if p == format!("/v1/web-sites/sites/{SITE}/actions") => {
                if lose.load(Ordering::SeqCst) {
                    return (0, Value::Null);
                }
                (
                    201,
                    json!({"ok": true, "data": {
                    "actionId": "act_44444444", "siteId": SITE, "operation": "publish",
                    "state": "awaiting_approval", "releaseId": RELEASE,
                    "expectedRevision": 4, "resultRevision": null,
                    "manifestDigest": "a".repeat(64), "liveUrl": null,
                    "approvalUrl": null, "approvalExpiresAt": null,
                    "updatedAt": "2030-01-01T00:00:00.000Z"}}),
                )
            }
            _ => (
                404,
                json!({"ok": false, "error": {"code": "not_found", "message": "no"}}),
            ),
        }
    }));
    let creds = credential_dir(&addr);
    seed_ready_release(creds.path());
    let action = WebAction::Publish {
        release: RELEASE.into(),
    };

    // First invocation: every create reply is lost, so the outcome is unknown.
    let first = with_config(creds.path(), || run(&action, &target(&addr), 5));
    assert!(first.is_err(), "an unknown outcome must not report success");
    let open = read_record(creds.path())["intents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "publish")
        .cloned()
        .expect("the publish intent was not kept");
    assert_eq!(open["state"], "open");
    assert_eq!(open["expected_revision"], 4);

    // The host moves on; the replies are answered now.
    revision.store(9, Ordering::SeqCst);
    lose_replies.store(false, Ordering::SeqCst);
    let second = with_config(creds.path(), || run(&action, &target(&addr), 5));
    assert_eq!(second.expect("resumed publish failed"), 0);

    let seen = log.lock().unwrap().clone();
    let bodies: Vec<Value> = seen
        .iter()
        .filter(|s| s.method == "POST" && s.path.ends_with("/actions"))
        .map(|s| serde_json::from_slice(&s.body).unwrap())
        .collect();
    assert!(bodies.len() >= 3, "expected resends: {bodies:?}");
    let first_body = &bodies[0];
    assert_eq!(first_body["expectedRevision"], 4);
    assert_eq!(first_body["requestKey"], open["request_key"]);
    for body in &bodies {
        assert_eq!(
            body, first_body,
            "a resend changed the key, revision or body"
        );
    }
    // The revision was read once, on the first invocation only.
    let revision_reads = seen
        .iter()
        .filter(|s| s.method == "GET" && s.path == format!("/v1/web-sites/sites/{SITE}"))
        .count();
    assert_eq!(revision_reads, 1, "the resume re-read the revision");
}

/// Rollback names a known release and sends its operation and id. Unpublish
/// sends no release id at all. An unknown site is refused before any request.
#[test]
fn rollback_and_unpublish_send_only_their_known_targets() {
    // Rollback of a known release.
    let (addr, log) = serve(action_handler("awaiting_approval", Value::Null));
    let creds = credential_dir(&addr);
    seed_ready_release(creds.path());
    let rollback = WebAction::Rollback {
        slug: "my-site".into(),
        release: RELEASE.into(),
    };
    assert_eq!(
        with_config(creds.path(), || run(&rollback, &target(&addr), 5)).unwrap(),
        0
    );
    let rb: Value = {
        let seen = log.lock().unwrap().clone();
        let post = seen
            .iter()
            .find(|s| s.method == "POST" && s.path.ends_with("/actions"))
            .expect("no rollback request");
        serde_json::from_slice(&post.body).unwrap()
    };
    assert_eq!(rb["operation"], "rollback");
    assert_eq!(rb["releaseId"], RELEASE);

    // Unpublish of the known site carries no release id.
    let (addr2, log2) = serve(action_handler("awaiting_approval", Value::Null));
    let creds2 = credential_dir(&addr2);
    seed_ready_release(creds2.path());
    let unpublish = WebAction::Unpublish {
        slug: "my-site".into(),
    };
    assert_eq!(
        with_config(creds2.path(), || run(&unpublish, &target(&addr2), 5)).unwrap(),
        0
    );
    let un: Value = {
        let seen = log2.lock().unwrap().clone();
        let post = seen
            .iter()
            .find(|s| s.method == "POST" && s.path.ends_with("/actions"))
            .expect("no unpublish request");
        serde_json::from_slice(&post.body).unwrap()
    };
    assert_eq!(un["operation"], "unpublish");
    assert!(un.get("releaseId").is_none(), "unpublish named a release");

    // An unknown site is refused before any request.
    let (addr3, log3) = serve(action_handler("awaiting_approval", Value::Null));
    let creds3 = credential_dir(&addr3);
    seed_ready_release(creds3.path());
    let unknown = WebAction::Unpublish {
        slug: "other-site".into(),
    };
    let err = with_config(creds3.path(), || run(&unknown, &target(&addr3), 5)).unwrap_err();
    assert!(err.to_string().contains("no prepared site"), "{err}");
    assert!(log3.lock().unwrap().is_empty(), "a request was sent");
}

/// Status reads back only an action whose id is known. An open intent whose
/// create reply was lost has no id, so it is reported unknown and nothing is
/// fetched for it. The read-back and the no-id case are both visible on the wire.
#[test]
fn status_reads_back_only_known_action_ids_and_never_invents_one() {
    let (addr, log) = serve(Arc::new(|seen: &Seen| {
        match (seen.method.as_str(), seen.path.as_str()) {
            ("GET", "/v1/web-sites/sites/by-slug/my-site") => (
                200,
                json!({"ok": true, "data": {"siteId": SITE, "slug": "my-site",
                    "revision": 4, "liveReleaseId": null, "liveUrl": null,
                    "suspended": false}}),
            ),
            ("GET", "/v1/web-sites/actions/act_44444444") => (
                200,
                json!({"ok": true, "data": {"actionId": "act_44444444",
                    "state": "awaiting_approval"}}),
            ),
            _ => (
                404,
                json!({"ok": false, "error": {"code": "not_found", "message": "no"}}),
            ),
        }
    }));
    let creds = credential_dir(&addr);
    let issuer = fixture_issuer(creds.path());
    write_record(
        creds.path(),
        fixture_identity(&issuer),
        vec![
            json!({
                "kind": "publish", "slug": "my-site", "release_id": RELEASE,
                "expected_revision": 4, "request_key": "web-publish-known0001",
                "site_id": SITE, "action_id": "act_44444444", "state": "open"
            }),
            json!({
                "kind": "unpublish", "slug": "my-site", "release_id": null,
                "expected_revision": 4, "request_key": "web-unpublish-lost0001",
                "site_id": SITE, "action_id": null, "state": "open"
            }),
        ],
    );
    let action = WebAction::Status {
        slug: "my-site".into(),
    };
    assert_eq!(
        with_config(creds.path(), || run(&action, &target(&addr), 5)).unwrap(),
        0
    );
    let seen = log.lock().unwrap().clone();
    let action_reads: Vec<&str> = seen
        .iter()
        .filter(|s| s.path.starts_with("/v1/web-sites/actions/"))
        .map(|s| s.path.as_str())
        .collect();
    assert_eq!(
        action_reads,
        vec!["/v1/web-sites/actions/act_44444444"],
        "only the known id may be read back: {seen:?}"
    );
    assert!(
        seen.iter().all(|s| !s.path.contains("unknown")),
        "an action id was invented for the lost intent"
    );
}
