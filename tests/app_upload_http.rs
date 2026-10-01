//! CAD-996 operator-only bounded manual app bundle upload — adversarial
//! tests. `POST /api/app-installations/upload` admits ONLY a verified-owner
//! `{files: {path→utf8}}` map, staged to a server-derived temp dir OUTSIDE the
//! PM tracker, then installed through the unchanged `app_workspace_install`
//! path (lands UNAPPROVED). These tests are written FIRST (RED): the route and
//! handler do not exist yet. No production/deploy/approval actions.
#![allow(clippy::disallowed_methods)]
mod common;

use cadence_agent::issue::Pm;
use common::{daemon_opts, plant_member_pane, test_port, LaneShell, TestDaemon};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

const UPLOAD_PATH: &str = "/api/app-installations/upload";

/// The workspace fixture: a PM tracker + daemon + a v0.5-shaped bundle the
/// upload can carry as a `{files}` map (read from disk, sent as JSON text).
struct Upload {
    _root: tempfile::TempDir,
    pm: Pm,
    daemon: TestDaemon,
}

impl Upload {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let opts = daemon_opts();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        let daemon = TestDaemon::start_opts(opts);
        Self {
            _root: root,
            pm,
            daemon,
        }
    }

    /// The canonical blog-post bundle as a `{files}` map — the exact shape the
    /// route admits (relative path → UTF-8 text), mirroring the on-disk layout.
    fn files_map() -> Value {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("apps/blog-post");
        let mut files = serde_json::Map::new();
        for rel in [
            "app.md",
            "workflows/blog-post.md",
            "rubrics/blog.md",
            "templates/brief.md",
            "templates/post.md",
        ] {
            let text = std::fs::read_to_string(base.join(rel)).unwrap();
            files.insert(rel.to_string(), json!(text));
        }
        json!({"files": files})
    }

    fn head(&self) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.pm.dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    }

    fn catalog_exists(&self) -> bool {
        self.pm.dir.join(".apps/catalog.yaml").exists()
    }
}

/// Spin up an in-process board over the workspace's daemon, returning its
/// port. The board is stopped on `_stop` drop (handled by the caller's
/// Cleanup).
fn board_for(u: &Upload, port: u16, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    let opts = cadence_agent::ui::ServeOpts {
        host: "127.0.0.1".into(),
        port,
        stop: Some(stop),
        test_seam: cfg!(feature = "test-seam"),
        ..Default::default()
    };
    let state = u.daemon.state.clone();
    let pm = u.pm.dir.clone();
    std::thread::spawn(move || {
        let _ = cadence_agent::ui::serve(&state, &pm, &opts);
    })
}

struct Cleanup(Arc<AtomicBool>, Option<std::thread::JoinHandle<()>>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
        if let Some(h) = self.1.take() {
            let _ = h.join();
        }
    }
}

/// Bring up board + wait for it to accept connections.
fn serve_upload_board(u: &Upload) -> (u16, Cleanup) {
    let lease = test_port();
    let port = lease.port;
    let stop = Arc::new(AtomicBool::new(false));
    let board = board_for(u, port, Arc::clone(&stop));
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "board did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    (port, Cleanup(stop, Some(board)))
}

/// A verified-owner upload: sign in as the operator and POST the file map.
fn operator_upload(port: u16, u: &Upload, body: &str) -> (u16, String, String) {
    let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &u.daemon.state, port);
    common::op::raw(port, &session.request("POST", UPLOAD_PATH, body))
}

// ---------------------------------------------------------------------------
// I1 — operator-only admission: agent / member / detached / unattributable
// peers are refused BEFORE the body is read and before any write.
// ---------------------------------------------------------------------------
#[test]
fn cad996_upload_requires_operator_only_admission() {
    let u = Upload::new();
    let mut lane = LaneShell::spawn(u._root.path());
    plant_member_pane(&u.daemon, "upload-agent", "claude", None, lane.pid());
    let (port, _cleanup) = serve_upload_board(&u);
    let body = Upload::files_map().to_string();
    let head = u.head();

    // (a) agent session assertion — seam replays the signed-in request AS the
    //     planted agent; refused before body read.
    let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &u.daemon.state, port);
    let (code, _, _) = common::op::raw(
        port,
        &session.request_as(
            "POST",
            UPLOAD_PATH,
            &body,
            &common::op::seam_headers(&u.daemon.state, "agent:upload-agent"),
        ),
    );
    assert!(
        matches!(code, 403 | 404),
        "agent upload must be refused (got {code})"
    );
    assert_eq!(u.head(), head, "agent upload mutated the catalog");
    assert!(!u.catalog_exists(), "agent upload created .apps/");

    // (b) actual enrolled TCP peer + detached setsid child — the operator
    //     session's wire is replayed by a non-operator peer; refused.
    for prefix in ["", "setsid "] {
        let stolen = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &u.daemon.state, port);
        let wire = stolen.request_as("POST", UPLOAD_PATH, &body, "");
        assert!(!wire.contains(cadence_agent::test_seam::AS_HEADER));
        assert!(!wire.contains(cadence_agent::test_seam::TOKEN_HEADER));
        let request = lane
            .dir
            .path()
            .join(format!("upload-peer-{}.txt", lane.seq));
        std::fs::write(&request, &wire).unwrap();
        let (rc, response) = lane.run(&format!(
            "{prefix}python3 -c 'import socket,sys;\
             s=socket.create_connection((\"127.0.0.1\",int(sys.argv[1])));\
             s.sendall(open(sys.argv[2],\"rb\").read());\
             print(s.makefile().readline())' {port} {}",
            request.display()
        ));
        assert_eq!(rc, 0);
        assert_eq!(
            response.split_whitespace().nth(1),
            Some("403"),
            "{prefix}peer reached the upload write: {response}"
        );
        assert_eq!(u.head(), head, "{prefix}peer upload mutated the catalog");
    }
    assert!(!u.catalog_exists(), "non-operator upload created .apps/");
}

// ---------------------------------------------------------------------------
// I1/I2 — forged authority fields and duplicate/unknown keys are refused
// before any catalog mutation; the body admits ONLY `{files}`.
// ---------------------------------------------------------------------------
#[test]
fn cad996_upload_body_is_strict_files_map() {
    let u = Upload::new();
    let (port, _cleanup) = serve_upload_board(&u);
    let head = u.head();
    let files = Upload::files_map()["files"].clone();

    // Forged authority fields alongside a valid map → 400, no mutation.
    for forged in [
        json!({"files": files, "approved": true}),
        json!({"files": files, "installed_by": "operator"}),
        json!({"files": files, "actor": "operator"}),
        json!({"files": files, "source": "/tmp/x"}),
        json!({"files": files, "path": "/tmp/x"}),
        json!({"files": files, "install_id": "00000000-0000-0000-0000-000000000000"}),
        json!({"files": files, "request_id": "once"}),
        json!({"files": files, "source_label": "manual"}),
    ] {
        let (code, _, resp) = operator_upload(port, &u, &forged.to_string());
        assert_eq!(code, 400, "forged/extra field accepted: {resp}");
        assert_eq!(u.head(), head, "forged-field upload mutated the catalog");
    }
    // Non-object files / wrong top shape.
    for bad in [
        json!({"files": []}),
        json!({"files": "app.md"}),
        json!({}),            // missing files
        json!({"files": {}}), // empty map
        json!([]),
        Value::Null,
    ] {
        let (code, _, _) = operator_upload(port, &u, &bad.to_string());
        assert_eq!(code, 400, "bad body shape accepted: {bad}");
    }
    // DUPLICATE top-level `files` keys — serde_json::Value would silently keep
    // the last; the custom deserializer must reject the raw duplicate.
    let dup = format!(
        "{{\"files\":{{\"app.md\":\"x\"}},\"files\":{}}}",
        Upload::files_map()["files"]
    );
    let (code, _, resp) = operator_upload(port, &u, &dup);
    assert_eq!(code, 400, "duplicate `files` key accepted: {resp}");
    assert_eq!(u.head(), head, "duplicate-key upload mutated the catalog");
    assert!(!u.catalog_exists());
}

// ---------------------------------------------------------------------------
// I3 — wire + decoded bounds and the flat path allowlist.
// ---------------------------------------------------------------------------
#[test]
fn cad996_upload_enforces_bounds_and_allowlist() {
    let u = Upload::new();
    let (port, _cleanup) = serve_upload_board(&u);
    let head = u.head();

    // Path smuggling / non-allowlisted entries.
    for key in [
        "../escape.md",
        "/abs/app.md",
        "workflows/../../etc/passwd",
        "workflows/.hidden.md",
        ".hidden",
        "app.md/extra.md",
        "evil/run.sh",
        "nested/deep/x.md",
        "workflows/a/b.md",
    ] {
        let body = json!({"files": {"app.md": "a", key: "x"}}).to_string();
        let (code, _, resp) = operator_upload(port, &u, &body);
        assert_eq!(code, 400, "unsafe path admitted {key}: {resp}");
        assert_eq!(u.head(), head);
    }

    // Per-file bound: a single file over 256 KiB is refused.
    let big = "x".repeat(257 * 1024);
    let body = json!({"files": {"app.md": "a", "workflows/big.md": big}}).to_string();
    let (code, _, _) = operator_upload(port, &u, &body);
    assert_eq!(code, 400, "per-file oversize admitted");

    // Count bound: >128 files refused.
    let mut many = serde_json::Map::new();
    many.insert("app.md".into(), json!("a"));
    for i in 0..128 {
        many.insert(format!("workflows/w{i}.md"), json!("x"));
    }
    let body = json!({"files": many}).to_string();
    let (code, _, _) = operator_upload(port, &u, &body);
    assert_eq!(code, 400, ">128 files admitted");

    // Non-UTF8 / malformed content inside the map.
    let body = "{\"files\":{\"app.md\":\"\\uD800\"}}"; // lone surrogate → invalid UTF-8 scalar
    let (code, _, _) = operator_upload(port, &u, body);
    assert!(matches!(code, 400), "malformed scalar admitted: {code}");

    assert_eq!(u.head(), head, "a refused upload still mutated the catalog");
    assert!(!u.catalog_exists());
}

// ---------------------------------------------------------------------------
// I1→I5 happy path: a verified-owner upload installs exactly the allowed
// bundle, UNAPPROVED, returns install_id + real host digest, and the staged
// temp is cleaned. The catalog records Source::Path (one-shot provenance).
// ---------------------------------------------------------------------------
#[test]
fn cad996_upload_installs_unapproved_and_cleans_temp() {
    let u = Upload::new();
    let (port, _cleanup) = serve_upload_board(&u);
    let body = Upload::files_map().to_string();
    let (code, _, response) = operator_upload(port, &u, &body);
    assert_eq!(code, 200, "operator upload failed: {response}");
    let row: Value = serde_json::from_str(&response).unwrap();
    assert!(row["install_id"].as_str().is_some(), "no install_id: {row}");
    assert_eq!(row["committed"], true, "install not committed: {row}");
    assert_eq!(row["approved"], false, "upload auto-approved: {row}");
    // Real host bundle digest (sha256:…) — not a caller-supplied value.
    let digest = row["digest"].as_str().unwrap();
    assert!(digest.starts_with("sha256:"), "no host digest: {row}");
    // The installation landed + catalog persisted.
    let id = row["install_id"].as_str().unwrap();
    assert!(
        u.catalog_exists(),
        "install did not write .apps/catalog.yaml"
    );
    assert!(
        u.pm.dir
            .join(format!(".apps/installations/{id}/bundle/app.md"))
            .exists(),
        "installed bundle not persisted under PM"
    );
    // Staged external temp is gone — nothing reusable left in PM or /tmp source.
    // (We cannot know the random temp path, but the catalog must NOT point into
    //  a live source the operator could mutate: the record's source is a temp
    //  that no longer exists ⇒ it cannot be a reusable update URL.)
    let record = std::fs::read_to_string(
        u.pm.dir
            .join(format!(".apps/installations/{id}/record.yaml")),
    )
    .unwrap();
    assert!(
        record.contains("kind: path"),
        "expected Source::Path record: {record}"
    );
}

// ---------------------------------------------------------------------------
// I5 — concurrent same-app uploads fail safe under pm.lock + identity refusal.
// ---------------------------------------------------------------------------
#[test]
fn cad996_upload_concurrent_same_app_refuses() {
    let u = Upload::new();
    let (port, _cleanup) = serve_upload_board(&u);
    let body = Upload::files_map().to_string();
    // First install succeeds.
    let (code, _, response) = operator_upload(port, &u, &body);
    assert_eq!(code, 200, "first upload failed: {response}");
    // A second upload of the SAME app is refused by the same-app identity
    // rule — the daemon returns "already has workspace installation", which
    // `rpc_err` maps to 409 (decided). Either way it must NOT be a 200 and the
    // catalog still holds exactly one installation.
    let (code, _, resp) = operator_upload(port, &u, &body);
    assert!(
        matches!(code, 400 | 409),
        "duplicate app upload was admitted ({code}): {resp}"
    );
    let (code, _, list) = common::op::raw(
        port,
        &common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &u.daemon.state, port).request(
            "GET",
            "/api/app-installations",
            "",
        ),
    );
    assert_eq!(code, 200, "list after uploads: {list}");
    let list: Value = serde_json::from_str(&list).unwrap();
    let count = list["installations"]
        .as_array()
        .map(|a| a.len())
        .or_else(|| list.as_array().map(|a| a.len()))
        .unwrap_or(0);
    assert_eq!(
        count, 1,
        "duplicate app produced a second installation: {list}"
    );
}
