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
//! so the production `web_sites` workflow runs unchanged. The daemon adapter
//! and this suite share that workflow; the `cadence` binary is a thin staging
//! client in `cli/web.rs`.

use crate::remote_cli::{web_envelope, RemoteTarget, WebResponse};
use crate::web_publish;
use crate::web_sites::{run, WebAction};
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
        identity: None,
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

fn action_dto(
    action_id: &str,
    state: &str,
    site_id: &str,
    operation: &str,
    release_id: Option<&str>,
    expected_revision: i64,
) -> Value {
    json!({"actionId": action_id, "siteId": site_id, "operation": operation,
        "state": state, "releaseId": release_id, "expectedRevision": expected_revision,
        "resultRevision": null, "manifestDigest": "a".repeat(64), "liveUrl": null,
        "approvalUrl": null, "approvalExpiresAt": null,
        "updatedAt": "2030-01-01T00:00:00.000Z"})
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
                json!({"ok": true, "data": action_dto(
                    "act_44444444", "awaiting_approval", SITE, "publish", Some(RELEASE), 4
                )}),
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

/// CAD-1429 regression: the host refuses the first explicit publish with a typed
/// `revision_conflict` 409. That refusal is authoritative: exactly one create
/// is sent, the intent is closed as `failed` with its key and body kept
/// verbatim, nothing is re-sent in the same invocation, and the next explicit
/// invocation re-reads the revision and makes a new key and a new intent.
#[test]
fn a_typed_revision_conflict_closes_the_intent_and_the_next_explicit_run_makes_a_new_one() {
    let revision = Arc::new(AtomicI64::new(4));
    let refuse = Arc::new(AtomicBool::new(true));
    let (rev, refusing) = (Arc::clone(&revision), Arc::clone(&refuse));
    let (addr, log) = serve(Arc::new(move |seen: &Seen| {
        match (seen.method.as_str(), seen.path.as_str()) {
            ("GET", p) if p == format!("/v1/web-sites/sites/{SITE}") => (
                200,
                json!({"ok": true, "data": {"siteId": SITE, "slug": "my-site",
                    "revision": rev.load(Ordering::SeqCst)}}),
            ),
            ("POST", p) if p == format!("/v1/web-sites/sites/{SITE}/actions") => {
                if refusing.load(Ordering::SeqCst) {
                    return (
                        409,
                        json!({"ok": false, "error": {"code": "revision_conflict", "message": "moved"}}),
                    );
                }
                (
                    201,
                    json!({"ok": true, "data": {
                    "actionId": "act_44444444", "siteId": SITE, "operation": "publish",
                    "state": "awaiting_approval", "releaseId": RELEASE,
                    "expectedRevision": 5, "resultRevision": null,
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

    // First explicit run: the typed refusal surfaces and nothing is re-sent.
    let first = with_config(creds.path(), || run(&action, &target(&addr), 5));
    let err = first.expect_err("a typed revision conflict must not report success");
    assert_eq!(err.code(), Some("revision_conflict"), "{err}");
    let refused = read_record(creds.path())["intents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "publish")
        .cloned()
        .expect("the publish intent was not kept");
    assert_eq!(refused["state"], "failed");
    assert_eq!(refused["expected_revision"], 4);
    let first_key = refused["request_key"].as_str().unwrap().to_string();
    let first_body = refused["body"].clone();
    assert_eq!(first_body["requestKey"], first_key.as_str());
    assert_eq!(first_body["expectedRevision"], 4);
    let sent_after_first: Vec<Value> = log
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.method == "POST" && s.path.ends_with("/actions"))
        .map(|s| serde_json::from_slice(&s.body).unwrap())
        .collect();
    assert_eq!(
        sent_after_first,
        vec![first_body.clone()],
        "the refusal must not trigger a resend"
    );

    // The host moved on; the next explicit invocation starts over.
    revision.store(5, Ordering::SeqCst);
    refuse.store(false, Ordering::SeqCst);
    let second = with_config(creds.path(), || run(&action, &target(&addr), 5));
    assert_eq!(second.expect("the next explicit run failed"), 0);

    let seen = log.lock().unwrap().clone();
    let actions: Vec<Value> = seen
        .iter()
        .filter(|s| s.method == "POST" && s.path.ends_with("/actions"))
        .map(|s| serde_json::from_slice(&s.body).unwrap())
        .collect();
    assert_eq!(actions.len(), 2, "expected exactly one create per run");
    let fresh = &actions[1];
    assert_ne!(
        fresh["requestKey"],
        first_key.as_str(),
        "the key was reused"
    );
    assert!(fresh["requestKey"]
        .as_str()
        .unwrap()
        .starts_with("web-publish-"));
    assert_eq!(fresh["expectedRevision"], 5);
    assert_eq!(fresh["operation"], "publish");
    assert_eq!(fresh["releaseId"], RELEASE);
    let reads = seen
        .iter()
        .filter(|s| s.method == "GET" && s.path == format!("/v1/web-sites/sites/{SITE}"))
        .count();
    assert_eq!(reads, 2, "each explicit run reads the revision once");

    let record = read_record(creds.path());
    let publishes: Vec<&Value> = record["intents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "publish")
        .collect();
    assert_eq!(publishes.len(), 2, "the old failed record must be retained");
    assert_eq!(publishes[0]["state"], "failed");
    assert_eq!(publishes[0]["request_key"], first_key.as_str());
    assert_eq!(publishes[0]["body"], first_body);
    assert_eq!(publishes[1]["state"], "open");
    assert_eq!(publishes[1]["expected_revision"], 5);
    assert_eq!(publishes[1]["request_key"], fresh["requestKey"]);
    assert_eq!(publishes[1]["action_id"], "act_44444444");
}

/// A 201 whose data carries no action id or state is neither a success nor a
/// refusal. The intent stays open with its key and body verbatim, and the next
/// explicit invocation resends exactly that body.
#[test]
fn a_malformed_success_leaves_the_intent_open_with_the_same_key_and_body() {
    let count = Arc::new(AtomicUsize::new(0));
    let (addr, log) = serve(Arc::new(move |seen: &Seen| {
        match (seen.method.as_str(), seen.path.as_str()) {
            ("GET", p) if p == format!("/v1/web-sites/sites/{SITE}") => (
                200,
                json!({"ok": true, "data": {"siteId": SITE, "slug": "my-site", "revision": 4}}),
            ),
            ("POST", p) if p == format!("/v1/web-sites/sites/{SITE}/actions") => {
                if count.fetch_add(1, Ordering::SeqCst) == 0 {
                    return (201, json!({"ok": true}));
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

    let first = with_config(creds.path(), || run(&action, &target(&addr), 5));
    assert!(
        first.is_err(),
        "a malformed success must not report success"
    );
    let open = read_record(creds.path())["intents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "publish")
        .cloned()
        .expect("the publish intent was not kept");
    assert_eq!(open["state"], "open");
    assert!(open["action_id"].is_null(), "an id was invented: {open}");

    let second = with_config(creds.path(), || run(&action, &target(&addr), 5));
    assert_eq!(second.expect("the resumed publish failed"), 0);

    let seen = log.lock().unwrap().clone();
    let bodies: Vec<Value> = seen
        .iter()
        .filter(|s| s.method == "POST" && s.path.ends_with("/actions"))
        .map(|s| serde_json::from_slice(&s.body).unwrap())
        .collect();
    assert_eq!(bodies.len(), 2, "expected exactly one resend");
    assert_eq!(
        bodies[0], open["body"],
        "the first send was not the stored body"
    );
    assert_eq!(bodies[0], bodies[1], "the resend changed the key or body");
    assert_eq!(bodies[1]["requestKey"], open["request_key"]);
}

/// Negative unit for the typed-refusal mapping. Only a 409 whose body is
/// exactly `ok:false` with `error.code == "revision_conflict"` yields the typed
/// code. Any other status, any other or malformed shape, and any message text
/// that merely contains the words must not.
#[test]
fn web_envelope_types_only_a_409_with_the_exact_revision_conflict_shape() {
    fn refusal(status: u16, body: Option<Value>) -> crate::Error {
        web_envelope(WebResponse { status, body }).expect_err("a refusal was expected")
    }

    let typed = refusal(
        409,
        Some(json!({"ok": false, "error": {"code": "revision_conflict", "message": "moved"}})),
    );
    assert_eq!(typed.code(), Some("revision_conflict"), "{typed}");

    for status in [200u16, 400, 403, 412, 422, 500, 503] {
        let e = refusal(
            status,
            Some(json!({"ok": false, "error": {"code": "revision_conflict"}})),
        );
        assert_ne!(
            e.code(),
            Some("revision_conflict"),
            "status {status} was typed"
        );
    }

    let shapes = [
        json!({"ok": false, "error": {"code": "conflict"}}),
        json!({"ok": false, "error": {"code": "Revision_Conflict"}}),
        json!({"ok": false, "error": {"code": "revision_conflict "}}),
        json!({"ok": "false", "error": {"code": "revision_conflict"}}),
        json!({"error": {"code": "revision_conflict"}}),
        json!({"ok": false, "error": {"message": "revision_conflict"}}),
        json!({"ok": false, "message": "revision_conflict"}),
        json!({"ok": false, "error": {"code": ["revision_conflict"]}}),
        json!({"ok": false, "error": "revision_conflict"}),
        json!({"ok": false, "error": {"code": "conflict", "message": "revision_conflict"}}),
    ];
    for body in shapes {
        let e = refusal(409, Some(body.clone()));
        assert_ne!(e.code(), Some("revision_conflict"), "{body} was typed");
    }

    let not_json = refusal(409, None);
    assert_ne!(not_json.code(), Some("revision_conflict"));

    // A success envelope unwraps to its data.
    let data = web_envelope(WebResponse {
        status: 200,
        body: Some(json!({"ok": true, "data": {"revision": 7}})),
    })
    .expect("a success envelope must unwrap");
    assert_eq!(data["revision"], 7);
}

#[test]
fn a_terminal_known_action_is_saved_before_a_fresh_explicit_action() {
    for (operation, old_key, release_id) in [
        ("rollback", "web-rollback-original01", Some(RELEASE)),
        ("unpublish", "web-unpublish-original1", None),
    ] {
        let op = operation.to_string();
        let release = release_id.map(str::to_string);
        let (addr, log) = serve(Arc::new(move |seen: &Seen| {
            match (seen.method.as_str(), seen.path.as_str()) {
                ("GET", "/v1/web-sites/actions/act_44444444") => (
                    200,
                    json!({"ok": true, "data": action_dto(
                        "act_44444444", "completed", SITE, &op, release.as_deref(), 4
                    )}),
                ),
                ("GET", p) if p == format!("/v1/web-sites/sites/{SITE}") => (
                    200,
                    json!({"ok": true, "data": {"siteId": SITE, "slug": "my-site", "revision": 9}}),
                ),
                ("POST", p) if p == format!("/v1/web-sites/sites/{SITE}/actions") => (
                    201,
                    json!({"ok": true, "data": action_dto(
                        "act_55555555", "awaiting_approval", SITE, &op, release.as_deref(), 9
                    )}),
                ),
                _ => (
                    404,
                    json!({"ok": false, "error": {"code": "not_found", "message": "no"}}),
                ),
            }
        }));
        let creds = credential_dir(&addr);
        let issuer = fixture_issuer(creds.path());
        let old_body = if let Some(release_id) = release_id {
            json!({"version":"1","workspaceId":ORG_ID,"operation":operation,
                "releaseId":release_id,"expectedRevision":4,"requestKey":old_key})
        } else {
            json!({"version":"1","workspaceId":ORG_ID,"operation":operation,
                "expectedRevision":4,"requestKey":old_key})
        };
        write_record(
            creds.path(),
            fixture_identity(&issuer),
            vec![json!({
                "kind": operation, "slug": "my-site", "release_id": release_id,
                "expected_revision": 4, "request_key": old_key, "body": old_body,
                "site_id": SITE, "action_id": "act_44444444", "state": "open"
            })],
        );
        let action = match operation {
            "rollback" => WebAction::Rollback {
                slug: "my-site".into(),
                release: RELEASE.into(),
            },
            _ => WebAction::Unpublish {
                slug: "my-site".into(),
            },
        };
        assert_eq!(
            with_config(creds.path(), || run(&action, &target(&addr), 5)).unwrap(),
            0
        );
        let seen = log.lock().unwrap().clone();
        assert_eq!(
            seen.iter().map(|s| s.method.as_str()).collect::<Vec<_>>(),
            ["GET", "GET", "POST"]
        );
        assert_eq!(seen[0].path, "/v1/web-sites/actions/act_44444444");
        assert_eq!(seen[1].path, format!("/v1/web-sites/sites/{SITE}"));
        let created: Value = serde_json::from_slice(&seen[2].body).unwrap();
        assert_eq!(created["expectedRevision"], 9);
        assert_ne!(created["requestKey"], old_key);
        assert_eq!(created.get("releaseId").and_then(Value::as_str), release_id);
        let intents = read_record(creds.path())["intents"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(intents[0]["state"], "done");
        assert_eq!(intents[0]["body"]["requestKey"], old_key);
        assert_eq!(intents[1]["state"], "open");
    }
}

#[test]
fn an_awaiting_known_action_reuses_exact_key_and_body_without_revision_read() {
    let old_body = json!({"version":"1","workspaceId":ORG_ID,"operation":"publish",
        "releaseId":RELEASE,"expectedRevision":4,"requestKey":"web-publish-awaiting01"});
    let (addr, log) = serve(Arc::new(|seen: &Seen| {
        match (seen.method.as_str(), seen.path.as_str()) {
            ("GET", "/v1/web-sites/actions/act_44444444") => (
                200,
                json!({"ok":true,"data":action_dto(
                    "act_44444444","awaiting_approval",SITE,"publish",Some(RELEASE),4
                )}),
            ),
            ("POST", p) if p == format!("/v1/web-sites/sites/{SITE}/actions") => (
                201,
                json!({"ok":true,"data":action_dto(
                    "act_44444444","awaiting_approval",SITE,"publish",Some(RELEASE),4
                )}),
            ),
            _ => (
                404,
                json!({"ok":false,"error":{"code":"not_found","message":"no"}}),
            ),
        }
    }));
    let creds = credential_dir(&addr);
    let issuer = fixture_issuer(creds.path());
    write_record(
        creds.path(),
        fixture_identity(&issuer),
        vec![json!({
            "kind":"publish","slug":"my-site","release_id":RELEASE,"expected_revision":4,
            "request_key":"web-publish-awaiting01","body":old_body,"site_id":SITE,
            "action_id":"act_44444444","state":"open"
        })],
    );
    let action = WebAction::Publish {
        release: RELEASE.into(),
    };
    assert_eq!(
        with_config(creds.path(), || run(&action, &target(&addr), 5)).unwrap(),
        0
    );
    let seen = log.lock().unwrap().clone();
    assert_eq!(
        seen.iter().map(|s| s.method.as_str()).collect::<Vec<_>>(),
        ["GET", "POST"]
    );
    assert_eq!(seen[0].path, "/v1/web-sites/actions/act_44444444");
    assert_eq!(
        serde_json::from_slice::<Value>(&seen[1].body).unwrap(),
        old_body
    );
    assert_eq!(read_record(creds.path())["intents"][0]["state"], "open");
}

#[test]
fn a_known_action_with_any_binding_or_state_mismatch_fails_closed() {
    let base = action_dto(
        "act_44444444",
        "completed",
        SITE,
        "publish",
        Some(RELEASE),
        4,
    );
    let mut variants = Vec::new();
    for (field, value) in [
        ("actionId", json!("act_99999999")),
        ("siteId", json!("site_99999999")),
        ("operation", json!("rollback")),
        ("releaseId", json!("rel_99999999")),
        ("expectedRevision", json!(5)),
        ("state", json!("future_state")),
    ] {
        let mut action = base.clone();
        action[field] = value;
        variants.push((200, action));
    }
    let mut missing_state = base.clone();
    missing_state.as_object_mut().unwrap().remove("state");
    variants.push((200, missing_state));
    let mut missing_operation = base.clone();
    missing_operation
        .as_object_mut()
        .unwrap()
        .remove("operation");
    variants.push((200, missing_operation));
    variants.push((500, base.clone()));
    for (status, bad) in variants {
        let response = bad.clone();
        let (addr, log) = serve(Arc::new(move |seen: &Seen| {
            if seen.method == "GET" && seen.path == "/v1/web-sites/actions/act_44444444" {
                return (status, json!({"ok": true, "data": response}));
            }
            (
                404,
                json!({"ok": false, "error": {"code": "not_found", "message": "no"}}),
            )
        }));
        let creds = credential_dir(&addr);
        let issuer = fixture_issuer(creds.path());
        write_record(
            creds.path(),
            fixture_identity(&issuer),
            vec![json!({
                "kind": "publish", "slug": "my-site", "release_id": RELEASE,
                "expected_revision": 4, "request_key": "web-publish-known0001",
                "site_id": SITE, "action_id": "act_44444444", "state": "open"
            })],
        );
        let action = WebAction::Publish {
            release: RELEASE.into(),
        };
        assert!(
            with_config(creds.path(), || run(&action, &target(&addr), 5)).is_err(),
            "accepted {bad}"
        );
        assert!(
            log.lock().unwrap().iter().all(|s| s.method != "POST"),
            "sent for {bad}"
        );
        let saved = read_record(creds.path());
        assert_eq!(
            saved["intents"].as_array().unwrap().len(),
            1,
            "created a replacement intent for {bad}"
        );
        assert_eq!(saved["intents"][0]["state"], "open");
    }
}

#[test]
fn status_persists_first_terminal_receipt_when_a_later_read_fails_and_never_posts() {
    let (addr, log) = serve(Arc::new(|seen: &Seen| {
        match (seen.method.as_str(), seen.path.as_str()) {
            ("GET", "/v1/web-sites/sites/by-slug/my-site") => (
                200,
                json!({"ok": true, "data": {
            "siteId": SITE, "slug": "my-site", "revision": 4,
            "liveReleaseId": null, "liveUrl": null, "suspended": false}}),
            ),
            ("GET", "/v1/web-sites/actions/act_44444444") => (
                200,
                json!({"ok": true, "data": action_dto(
                    "act_44444444", "completed", SITE, "publish", Some(RELEASE), 4
                )}),
            ),
            ("GET", "/v1/web-sites/actions/act_55555555") => (
                503,
                json!({"ok": false, "error": {"code": "offline", "message": "no"}}),
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
            json!({"kind":"publish","slug":"my-site","release_id":RELEASE,"expected_revision":4,"request_key":"web-publish-first0001","site_id":SITE,"action_id":"act_44444444","state":"open"}),
            json!({"kind":"rollback","slug":"my-site","release_id":RELEASE,"expected_revision":4,"request_key":"web-rollback-second01","site_id":SITE,"action_id":"act_55555555","state":"open"}),
        ],
    );
    let action = WebAction::Status {
        slug: "my-site".into(),
    };
    assert!(with_config(creds.path(), || run(&action, &target(&addr), 5)).is_err());
    let intents = read_record(creds.path())["intents"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(intents[0]["state"], "done");
    assert_eq!(intents[1]["state"], "open");
    assert!(log.lock().unwrap().iter().all(|s| s.method != "POST"));
}

#[test]
fn typed_non_auth_4xx_refusal_retires_once_and_next_run_uses_fresh_intent() {
    for status in [400u16, 404, 410] {
        let refusals = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&refusals);
        let revision = Arc::new(AtomicI64::new(4));
        let rev = Arc::clone(&revision);
        let (addr, log) = serve(Arc::new(move |seen: &Seen| {
            match (seen.method.as_str(), seen.path.as_str()) {
                ("GET", p) if p == format!("/v1/web-sites/sites/{SITE}") => (
                    200,
                    json!({"ok":true,"data":{"siteId":SITE,"slug":"my-site","revision":rev.load(Ordering::SeqCst)}}),
                ),
                ("POST", p) if p == format!("/v1/web-sites/sites/{SITE}/actions") => {
                    if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                        (
                            status,
                            json!({"ok":false,"error":{"code":"invalid_action","message":"refused"}}),
                        )
                    } else {
                        (
                            201,
                            json!({"ok":true,"data":action_dto("act_55555555","awaiting_approval",SITE,"publish",Some(RELEASE),5)}),
                        )
                    }
                }
                _ => (
                    404,
                    json!({"ok":false,"error":{"code":"not_found","message":"no"}}),
                ),
            }
        }));
        let creds = credential_dir(&addr);
        seed_ready_release(creds.path());
        let action = WebAction::Publish {
            release: RELEASE.into(),
        };
        assert!(with_config(creds.path(), || run(&action, &target(&addr), 5)).is_err());
        let old = read_record(creds.path())["intents"][1].clone();
        assert_eq!(old["state"], "failed");
        let once = log
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.method == "POST")
            .count();
        assert_eq!(once, 1, "status={status}");
        revision.store(5, Ordering::SeqCst);
        assert_eq!(
            with_config(creds.path(), || run(&action, &target(&addr), 5)).unwrap(),
            0
        );
        let posts: Vec<Value> = log
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.method == "POST")
            .map(|s| serde_json::from_slice(&s.body).unwrap())
            .collect();
        assert_eq!(posts.len(), 2, "status={status}");
        assert_eq!(posts[0], old["body"]);
        assert_ne!(posts[1]["requestKey"], posts[0]["requestKey"]);
        assert_eq!(posts[1]["expectedRevision"], 5);
    }
}

#[test]
fn auth_and_server_errors_remain_unknown_and_retry_the_same_body() {
    for status in [401u16, 403, 500] {
        let count = Arc::new(AtomicUsize::new(0));
        let attempts = Arc::clone(&count);
        let (addr, log) = serve(Arc::new(move |seen: &Seen| {
            match (seen.method.as_str(), seen.path.as_str()) {
                ("GET", p) if p == format!("/v1/web-sites/sites/{SITE}") => (
                    200,
                    json!({"ok":true,"data":{"siteId":SITE,"slug":"my-site","revision":4}}),
                ),
                ("POST", p) if p == format!("/v1/web-sites/sites/{SITE}/actions") => {
                    if attempts.fetch_add(1, Ordering::SeqCst) < 2 {
                        (
                            status,
                            json!({"ok":false,"error":{"code":"unknown","message":"retry"}}),
                        )
                    } else {
                        (
                            201,
                            json!({"ok":true,"data":action_dto("act_55555555","awaiting_approval",SITE,"publish",Some(RELEASE),4)}),
                        )
                    }
                }
                _ => (
                    404,
                    json!({"ok":false,"error":{"code":"not_found","message":"no"}}),
                ),
            }
        }));
        let creds = credential_dir(&addr);
        seed_ready_release(creds.path());
        let action = WebAction::Publish {
            release: RELEASE.into(),
        };
        assert!(
            with_config(creds.path(), || run(&action, &target(&addr), 5)).is_err(),
            "status={status}"
        );
        let open = read_record(creds.path())["intents"][1].clone();
        assert_eq!(open["state"], "open");
        assert_eq!(open["body"]["requestKey"], open["request_key"]);
        assert_eq!(
            log.lock()
                .unwrap()
                .iter()
                .filter(|s| s.method == "POST")
                .count(),
            2
        );
        assert_eq!(
            with_config(creds.path(), || run(&action, &target(&addr), 5)).unwrap(),
            0
        );
        let posts: Vec<Value> = log
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.method == "POST")
            .map(|s| serde_json::from_slice(&s.body).unwrap())
            .collect();
        assert_eq!(posts.len(), 3, "status={status}");
        assert!(
            posts.iter().all(|body| body == &open["body"]),
            "status={status}: {posts:?}"
        );
    }
}

/// CAD-1665: the CLI's staging shape, written directly so the daemon adapter
/// can be exercised without the binary.
#[cfg(feature = "test-seam")]
fn stage_for_test(state: &Path, site: &web_publish::FrozenSite, slug: &str, spa: bool) -> String {
    let token = format!("stage-{}", "ab".repeat(12));
    let dir = state.join("web-staging").join(&token);
    std::fs::create_dir_all(dir.join("files")).unwrap();
    let files: Vec<Value> = site
        .files
        .iter()
        .map(|file| {
            json!({
                "path": file.path,
                "sha256": file.sha256,
                "mime": file.mime,
                "bytes": file.bytes.len(),
            })
        })
        .collect();
    let plan = json!({
        "version": "1",
        "slug": slug,
        "spa": spa,
        "digest": site.digest,
        "canonical": site.canonical,
        "files": files,
    });
    write_private(&dir.join("plan.json"), &serde_json::to_vec(&plan).unwrap());
    for file in &site.files {
        write_private(
            &dir.join("files")
                .join(web_publish::encode_artifact_path(&file.path)),
            &file.bytes,
        );
    }
    token
}

/// CAD-1665: the daemon seam — an operator `platform_call` reaches the
/// `agenticos-web` adapter, which resolves the saved login and uploads the
/// staged bundle over the real transport.
#[cfg(feature = "test-seam")]
#[test]
fn the_daemon_adapter_uploads_a_staged_site_through_the_gate() {
    use crate::daemon::{ServeOptions, Shared};
    use crate::test_seam::{scoped, Asserted};

    let (addr, seen) = serve(prepare_handler(vec!["index.html"]));
    let config = credential_dir(&addr);
    let registry = json!({"connections": [{
        "selection": {"org": ORG},
        "destination": {"mode": "remote", "endpoint": BOARD_AUDIENCE, "org_id": ORG_ID}
    }]});
    write_private(
        &config.path().join("cadence/orgs.json"),
        &serde_json::to_vec(&registry).unwrap(),
    );

    let state = tempfile::tempdir().unwrap();
    let pm = tempfile::tempdir().unwrap();
    crate::issue::Pm::init(pm.path()).unwrap();
    let mut opts = ServeOptions {
        provider_env: crate::adapter::ProviderEnv::refusing_providers(),
        lease: Some(Default::default()),
        test_seam: true,
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.path().to_str().unwrap());
    crate::platform::agenticos_web::attach(&mut opts, state.path()).unwrap();
    let shared = Shared::new(state.path(), &opts).unwrap();

    let root = tempfile::tempdir().unwrap();
    let site = web_publish::freeze(&tiny_site(root.path())).unwrap();
    let token = stage_for_test(state.path(), &site, "my-site", false);
    let params = json!({
        "platform": "agenticos-web",
        "account": ORG,
        "tool": "prepare_site",
        "input": {"org": ORG, "staging": token},
    });
    let value = with_config(config.path(), || {
        scoped(Asserted::Operator, || {
            shared.dispatch("platform_call", &params, std::process::id())
        })
    })
    .expect("the operator call must execute the adapter");

    assert_eq!(value["result"], json!("executed"));
    assert_eq!(value["platform_result"]["releaseId"], json!(RELEASE));
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter()
            .any(|request| request.method == "POST" && request.path == "/v1/web-sites/uploads"),
        "create missing"
    );
    assert!(
        seen.iter()
            .any(|request| request.method == "PUT" && request.path.contains("/files/index.html")),
        "artifact missing"
    );
    assert!(
        seen.iter()
            .any(|request| request.method == "POST" && request.path.ends_with("/finalize")),
        "finalize missing"
    );
}
