//! CAD-996 operator-only bounded manual app bundle upload — adversarial
//! tests. `POST /api/app-installations/upload` admits ONLY a verified-owner
//! `{files: {path→utf8}}` map, staged to a server-derived temp dir OUTSIDE the
//! PM tracker, then installed through the unchanged `app_workspace_install`
//! path (lands UNAPPROVED). Written tests-first (RED); the route/handler are
//! added by the source commit. No production/deploy/approval actions.
#![allow(clippy::disallowed_methods)]
mod common;

use cadence_agent::issue::Pm;
use common::{daemon_opts, plant_member_pane, test_port, LaneShell, TestDaemon};
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Barrier,
};
use std::time::{Duration, Instant};

const UPLOAD_PATH: &str = "/api/app-installations/upload";

/// The workspace fixture: a PM tracker + daemon. The bundle travels as a
/// `{files}` map built from the canonical on-disk `apps/blog-post` fixture.
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

    /// The canonical blog-post bundle as a `{files}` map (rel path → UTF-8).
    /// Always a VALID bundle — admission/bounds tests must be refused for the
    /// right reason, not because the manifest happened to be invalid.
    fn files_map() -> Value {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("apps/blog-post");
        let mut files = Map::new();
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

    /// The valid map with one extra `key: value` file inserted.
    fn files_map_plus(key: &str, value: &str) -> String {
        let mut m = Self::files_map();
        m["files"]
            .as_object_mut()
            .unwrap()
            .insert(key.to_string(), json!(value));
        m.to_string()
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

    /// Live workspace installation count via the daemon (the list verb
    /// answers a bare array of installation rows).
    fn install_count(&self) -> usize {
        let out = self
            .daemon
            .operator_rpc("app_workspace_list", json!({}))
            .unwrap();
        out.as_array().map_or(0, Vec::len)
    }
}

/// Spin up an in-process board over the workspace's daemon.
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

/// A verified-owner upload: sign in as the operator and POST the map.
fn operator_upload(port: u16, u: &Upload, body: &str) -> (u16, String, String) {
    let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &u.daemon.state, port);
    common::op::raw(port, &session.request("POST", UPLOAD_PATH, body))
}

// ---------------------------------------------------------------------------
// I1 — operator-only admission: agent / detached / setsid peers refused before
// body is read. The route now exists, so the gate must be 403 (not the
// fail-closed 404 an unlisted write also returns).
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
    //     planted agent; refused (403) before body read.
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
    assert_eq!(code, 403, "agent upload must be refused 403 (got {code})");
    assert_eq!(u.head(), head, "agent upload mutated the catalog");
    assert!(!u.catalog_exists(), "agent upload created .apps/");

    // (b) actual enrolled TCP peer + detached setsid child replaying the
    //     operator session wire — refused 403.
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
// I1/I2 — forged authority fields + unknown/duplicate keys refused before any
// catalog mutation; the body admits ONLY `{files}` (custom deserializer — a
// `serde_json::Value` parse would silently keep the last duplicate key).
// ---------------------------------------------------------------------------
#[test]
fn cad996_upload_body_is_strict_files_map() {
    let u = Upload::new();
    let (port, _cleanup) = serve_upload_board(&u);
    let head = u.head();
    let files = Upload::files_map()["files"].clone();

    // Forged authority fields alongside a VALID map → 400, no mutation.
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

    // DUPLICATE top-level `files` keys — a `Value` parse collapses to the last;
    // the strict visitor must reject the raw duplicate outright.
    let dup = format!(
        "{{\"files\":{{\"app.md\":\"x\"}},\"files\":{}}}",
        Upload::files_map()["files"]
    );
    let (code, _, resp) = operator_upload(port, &u, &dup);
    assert_eq!(code, 400, "duplicate `files` key accepted: {resp}");

    // DUPLICATE INNER path key that a weak decoder would silently collapse and
    // then INSTALL — build raw JSON where `app.md` appears twice, the SECOND a
    // VALID manifest, so a Value-overwrites decoder would actually install.
    let files_obj = Upload::files_map()["files"].clone();
    let mut raw_inner = String::new();
    for (k, v) in files_obj.as_object().unwrap() {
        if k != "app.md" {
            raw_inner.push_str(&format!("{}:{},", json!(k), v));
        }
    }
    // app.md first (a junk value), then all others, then app.md AGAIN (valid).
    let dup_inner = format!(
        "{{\"files\":{{\"app.md\":\"JUNK\",{}\"app.md\":{}}}}}",
        raw_inner, files_obj["app.md"]
    );
    let (code, _, resp) = operator_upload(port, &u, &dup_inner);
    assert_eq!(
        code, 400,
        "inner duplicate app.md (would install on weak decode) accepted: {resp}"
    );

    assert_eq!(u.head(), head, "a strict-map refusal still mutated catalog");
    assert!(!u.catalog_exists());
}

// ---------------------------------------------------------------------------
// I3 — wire + decoded bounds and the EXACT-lexical flat path allowlist.
// ---------------------------------------------------------------------------
#[test]
fn cad996_upload_enforces_bounds_and_allowlist() {
    let u = Upload::new();
    let (port, _cleanup) = serve_upload_board(&u);
    let head = u.head();

    // Path smuggling — each on top of a VALID bundle so the refusal is the
    // path grammar, not an invalid manifest. Interior `.`, doubled `//`,
    // trailing `/`, backslash, NUL, `..`, abs, dotfiles, extra top-level dir.
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
        "templates//foo.md",  // doubled slash — normalization alias
        "templates/./foo.md", // interior dot — normalization alias
        "templates/foo/",     // trailing slash
        "templates\\foo.md",  // backslash alias (Windows separator)
        "workflows/up .md",   // space — invalid tag stem
    ] {
        let body = Upload::files_map_plus(key, "x");
        let (code, _, resp) = operator_upload(port, &u, &body);
        assert_eq!(code, 400, "unsafe path admitted {key}: {resp}");
        assert_eq!(u.head(), head, "unsafe path {key} mutated the catalog");
    }

    // Distinct raw keys that COLLIDE to one staged file under normalization —
    // all three must be refused as a set (any one alone is also bad grammar,
    // but together they prove no alias lands).
    let mut m = Upload::files_map();
    let inner = m["files"].as_object_mut().unwrap();
    inner.insert("templates/foo.md".into(), json!("a"));
    inner.insert("templates//foo.md".into(), json!("b"));
    inner.insert("templates/./foo.md".into(), json!("c"));
    let (code, _, resp) = operator_upload(port, &u, &m.to_string());
    assert_eq!(code, 400, "normalization-alias collision admitted: {resp}");
    assert_eq!(u.head(), head);

    // Per-file bound: a single file over 256 KiB refused (valid base).
    let big = "x".repeat(257 * 1024);
    let body = Upload::files_map_plus("workflows/big.md", &big);
    let (code, _, _) = operator_upload(port, &u, &body);
    assert_eq!(code, 400, "per-file oversize admitted");
    assert_eq!(u.head(), head);

    // Count bound: >128 files refused (valid base + extra workflow files).
    let mut m = Upload::files_map();
    let inner = m["files"].as_object_mut().unwrap();
    for i in 0..124 {
        inner.insert(format!("workflows/w{i}.md"), json!("x"));
    }
    // 124 extra + 5 canonical = 129 > 128.
    let (code, _, _) = operator_upload(port, &u, &m.to_string());
    assert_eq!(code, 400, ">128 files admitted");
    assert_eq!(u.head(), head);

    // Decoded aggregate > 2 MiB with every file < 256 KiB → 400.
    // Use valid base + several ~200 KiB rubric leaves.
    let mut m = Upload::files_map();
    let inner = m["files"].as_object_mut().unwrap();
    let chunk = "y".repeat(200 * 1024);
    for i in 0..12 {
        inner.insert(format!("rubrics/r{i}.md"), json!(chunk.clone()));
    }
    let (code, _, resp) = operator_upload(port, &u, &m.to_string());
    assert_eq!(code, 400, "decoded >2MiB aggregate admitted: {resp}");
    assert_eq!(u.head(), head);

    // Wire cap boundary: a JSON body > 8 MiB must 413 (read_body stops at cap).
    let huge_val = "z".repeat(8 * 1024 * 1024 + 4096);
    let body = json!({"files": {"app.md": "a", "rubrics/huge.md": huge_val}}).to_string();
    let (code, _, _) = operator_upload(port, &u, &body);
    assert_eq!(code, 413, "over-8MiB wire body not refused: {code}");
    assert_eq!(u.head(), head);

    assert!(!u.catalog_exists(), "a refused upload still created .apps/");
}

// ---------------------------------------------------------------------------
// I3 regression — the OLD {source}-install route keeps its 4 KiB BODY_CAP.
// A >4KiB body to /api/app-installations must still be refused.
// ---------------------------------------------------------------------------
#[test]
fn cad996_existing_install_route_keeps_4kib_cap() {
    let u = Upload::new();
    let (port, _cleanup) = serve_upload_board(&u);
    let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &u.daemon.state, port);
    // A valid {source} field padded past 4 KiB — refused by the unchanged cap.
    let padded = "x".repeat(8 * 1024);
    let body = json!({"source": format!("/tmp/{padded}")}).to_string();
    assert!(body.len() > 4096);
    let (code, _, _) = common::op::raw(
        port,
        &session.request("POST", "/api/app-installations", &body),
    );
    assert_eq!(code, 413, "source-install 4KiB cap changed: {code}");
}

// ---------------------------------------------------------------------------
// I1→I5 happy path: a verified-owner upload installs exactly the allowed
// bundle, UNAPPROVED, returns install_id + real host digest, and the staged
// temp is gone — its Source::Path records a non-existent path (one-shot).
// ---------------------------------------------------------------------------
#[test]
fn cad996_upload_installs_unapproved_and_cleans_temp() {
    let u = Upload::new();
    let (port, _cleanup) = serve_upload_board(&u);
    let body = Upload::files_map().to_string();
    let (code, _, response) = operator_upload(port, &u, &body);
    assert_eq!(code, 200, "operator upload failed: {response}");
    let row: Value = serde_json::from_str(&response).unwrap();
    let id = row["install_id"].as_str().expect("no install_id");
    assert_eq!(row["committed"], true, "install not committed: {row}");
    assert_eq!(row["approved"], false, "upload auto-approved: {row}");
    let digest = row["digest"].as_str().unwrap();
    assert!(digest.starts_with("sha256:"), "no host digest: {row}");

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

    // The recorded source is a Source::Path pointing at the staged temp — which
    // is now deleted, so it cannot be a reusable update URL. Parse the actual
    // record and assert the recorded path no longer exists.
    let record = std::fs::read_to_string(
        u.pm.dir
            .join(format!(".apps/installations/{id}/record.yaml")),
    )
    .expect("record.yaml missing");
    let recorded: serde_yaml::Value = serde_yaml::from_str(&record).unwrap();
    let src_path = recorded["source"]["path"]
        .as_str()
        .expect("record.source.path missing — expected Source::Path");
    assert!(
        !std::path::Path::new(src_path).exists(),
        "staged source path still exists — temp was not cleaned: {src_path}"
    );
}

// ---------------------------------------------------------------------------
// I5 — two TRULY concurrent same-app uploads: exactly one 200, the other a
// refusal (pm.lock + same-app identity rule), and exactly one installation.
// ---------------------------------------------------------------------------
#[test]
fn cad996_upload_concurrent_same_app_refuses() {
    let u = Upload::new();
    let (port, _cleanup) = serve_upload_board(&u);
    let body = Upload::files_map().to_string();
    let head = u.head();

    // Fire both requests simultaneously on a barrier, each on its own operator
    // session on its own thread — not sequential.
    let barrier = Arc::new(Barrier::new(2));
    let state = u.daemon.state.clone();
    let body_c = body.clone();
    let b2 = Arc::clone(&barrier);
    let handle = std::thread::spawn(move || {
        let s = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &state, port);
        b2.wait();
        common::op::raw(port, &s.request("POST", UPLOAD_PATH, &body_c))
    });
    let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &u.daemon.state, port);
    barrier.wait();
    let (code_a, _, _) = common::op::raw(port, &session.request("POST", UPLOAD_PATH, &body));
    let (code_b, _, _) = handle.join().unwrap();

    let mut ok = 0;
    let mut refused = 0;
    for code in [code_a, code_b] {
        match code {
            200 => ok += 1,
            400 | 409 => refused += 1,
            other => panic!("unexpected status {other}"),
        }
    }
    assert_eq!(
        ok, 1,
        "both concurrent uploads installed (a={code_a} b={code_b})"
    );
    assert_eq!(
        refused, 1,
        "duplicate upload not refused (a={code_a} b={code_b})"
    );
    assert_eq!(
        u.install_count(),
        1,
        "concurrent uploads produced != 1 installation"
    );
    assert_ne!(u.head(), head, "install must have committed (head moved)");
}
