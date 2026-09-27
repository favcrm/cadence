//! CAD-667 public workspace installation contract. Private state only.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, plant_member_pane, LaneShell, TestDaemon};
use serde_json::{json, Value};
use std::path::PathBuf;

struct Workspace {
    _root: tempfile::TempDir,
    pm: Pm,
    daemon: TestDaemon,
}
impl Workspace {
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
    fn source(&self) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("apps/blog-post")
    }
    fn install(&self) -> cadence_agent::Result<Value> {
        self.daemon
            .operator_rpc("app_workspace_install", json!({"source": self.source()}))
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
}

#[test]
fn cad667_operator_installs_lists_and_inspects_without_project_or_grants() {
    let w = Workspace::new();
    std::fs::write(w.pm.dir.join("foreign.txt"), "unrelated private draft").unwrap();
    let result = w
        .install()
        .expect("project-free operator install must exist");
    let id = result["install_id"]
        .as_str()
        .expect("stable installation ID");
    assert!(result["project"].is_null());
    assert_eq!(result["approved"], false);
    assert_eq!(result["committed"], true);
    assert!(result["foreign_files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value.as_str() == Some("foreign.txt")));
    let committed = std::process::Command::new("git")
        .arg("-C")
        .arg(&w.pm.dir)
        .args(["ls-files", "foreign.txt"])
        .output()
        .unwrap();
    assert!(committed.status.success());
    assert!(
        committed.stdout.is_empty(),
        "installation committed unrelated PM dirt"
    );
    assert!(w
        .pm
        .dir
        .join(".apps/installations")
        .join(id)
        .join("bundle/app.md")
        .is_file());
    assert_eq!(
        cadence_agent::issue::project::list(&w.pm.dir)
            .unwrap()
            .len(),
        0,
        "installation created a hidden project"
    );
    let list = w
        .daemon
        .operator_rpc("app_workspace_list", json!({}))
        .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["install_id"], id);
    let shown = w
        .daemon
        .operator_rpc("app_workspace_show", json!({"install_id":id}))
        .unwrap();
    assert_eq!(shown["name"], "blog-post");
    assert!(shown["guide"].as_str().unwrap().contains("Blog post"));
    let db = rusqlite::Connection::open(w.daemon.state.join("cadence.sqlite3")).unwrap();
    let grants: i64 = db
        .query_row("SELECT count(*) FROM app_grants", [], |r| r.get(0))
        .unwrap();
    assert_eq!(grants, 0);
    let first = w.head();
    let repeat = w.install();
    match repeat {
        Ok(row) => assert_eq!(row["install_id"], id),
        Err(error) => assert!(
            error.to_string().contains(id),
            "repeat refusal must identify existing installation: {error}"
        ),
    }
    assert_eq!(
        w.head(),
        first,
        "repeat silently duplicated or rewrote installation"
    );
}

#[test]
fn cad667_registered_agent_and_forged_operator_fields_cannot_install() {
    let w = Workspace::new();
    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "catalog-worker", "claude", None, lane.pid());
    let head = w.head();
    for params in [
        json!({"source":w.source()}),
        json!({"source":w.source(),"agent":"operator","by":"operator","approved":true}),
    ] {
        let frame = lane.rpc(&w.daemon.state, "app_workspace_install", params);
        assert_eq!(frame["ok"], false, "agent obtained installation authority");
    }
    assert_eq!(w.head(), head);
    assert!(!w.pm.dir.join(".apps").exists());
    // Prove this is an enrolled caller, not an unknown method-only negative.
    w.install()
        .expect("operator counterpart must be implemented");
}

#[test]
fn cad667_concurrent_installs_never_create_two_logical_copies() {
    let w = Workspace::new();
    let results = std::thread::scope(|scope| {
        let one = scope.spawn(|| w.install());
        let two = scope.spawn(|| w.install());
        (one.join().unwrap(), two.join().unwrap())
    });
    assert!(
        results.0.is_ok() || results.1.is_ok(),
        "neither operator install worked: {results:?}"
    );
    let rows = w
        .daemon
        .operator_rpc("app_workspace_list", json!({}))
        .unwrap();
    assert_eq!(
        rows.as_array().unwrap().len(),
        1,
        "concurrent calls created duplicate identities"
    );
    assert!(!w.pm.dir.join(".apps/pending.yaml").exists());
}

#[test]
fn cad667_detached_registered_descendant_cannot_claim_operator_install() {
    let w = Workspace::new();
    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "catalog-detached", "claude", None, lane.pid());
    let request = lane.dir.path().join("detached.json");
    std::fs::write(
        &request,
        cadence_agent::proto::request("app_workspace_install", json!({"source":w.source()}))
            .to_string(),
    )
    .unwrap();
    let head = w.head();
    let (rc, output) = lane.run(&format!("setsid python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);s.sendall(open(sys.argv[2],\"rb\").read()+b\"\\n\");print(s.makefile().readline())' {} {}", cadence_agent::client::socket_path(&w.daemon.state).display(), request.display()));
    assert_eq!(rc, 0);
    let frame: Value = serde_json::from_str(output.trim()).unwrap();
    assert_eq!(frame["ok"], false);
    assert!(
        frame.to_string().contains("operator"),
        "detached proof must reach caller authority, not missing method: {frame}"
    );
    assert_eq!(w.head(), head);
    assert!(!w.pm.dir.join(".apps").exists());
}

#[test]
fn cad667_http_install_and_reads_share_operator_authority() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};
    let w = Workspace::new();
    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "catalog-http", "claude", None, lane.pid());
    let port = (3110..3200)
        .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let opts = cadence_agent::ui::ServeOpts {
        host: "127.0.0.1".into(),
        port,
        stop: Some(Arc::clone(&stop)),
        test_seam: cfg!(feature = "test-seam"),
        ..Default::default()
    };
    let state = w.daemon.state.clone();
    let pm = w.pm.dir.clone();
    let board = std::thread::spawn(move || cadence_agent::ui::serve(&state, &pm, &opts));
    struct Cleanup(
        Arc<AtomicBool>,
        Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
    );
    impl Drop for Cleanup {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
            self.1.take().unwrap().join().unwrap().unwrap();
        }
    }
    let _cleanup = Cleanup(stop, Some(board));
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    let head = w.head();
    let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &w.daemon.state, port);
    let body = json!({"source":w.source()}).to_string();
    let (code, _, _) = common::op::raw(
        port,
        &session.request_as(
            "POST",
            "/api/app-installations",
            &body,
            &common::op::seam_headers(&w.daemon.state, "agent:catalog-http"),
        ),
    );
    assert!(matches!(code, 403 | 404));
    assert_eq!(w.head(), head);
    assert!(!w.pm.dir.join(".apps").exists());
    // Agent replay retires a session: a fresh real operator session is required.
    let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &w.daemon.state, port);
    let forged = json!({"source":w.source(),"by":"operator","approved":true}).to_string();
    let (code, _, _) = common::op::raw(
        port,
        &session.request("POST", "/api/app-installations", &forged),
    );
    assert_eq!(
        code, 400,
        "forged fields must be rejected by the installed route"
    );
    assert_eq!(w.head(), head);
    let (code, _, response) = common::op::raw(
        port,
        &session.request("POST", "/api/app-installations", &body),
    );
    assert_eq!(
        code, 200,
        "operator workspace HTTP install absent: {response}"
    );
    let row: Value = serde_json::from_str(&response).unwrap();
    let id = row["install_id"].as_str().unwrap();
    for path in [
        "/api/app-installations".to_string(),
        format!("/api/app-installations/{id}"),
    ] {
        let (code, _, _) = common::op::raw(port, &session.request("GET", &path, ""));
        assert_eq!(code, 200);
        let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &w.daemon.state, port);
        let (code, _, _) = common::op::raw(
            port,
            &session.request_as(
                "GET",
                &path,
                "",
                &common::op::seam_headers(&w.daemon.state, "agent:catalog-http"),
            ),
        );
        assert_eq!(
            code, 403,
            "agent read exposed workspace installation {path}"
        );
    }
    let installed_head = w.head();
    for prefix in ["", "setsid "] {
        for (method, path, body) in [
            ("POST", "/api/app-installations".to_string(), body.clone()),
            ("GET", "/api/app-installations".to_string(), String::new()),
            ("GET", format!("/api/app-installations/{id}"), String::new()),
        ] {
            let stolen = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &w.daemon.state, port);
            let request = lane.dir.path().join(format!("http-{}.txt", lane.seq));
            std::fs::write(&request, stolen.request(method, &path, &body)).unwrap();
            let (rc,response)=lane.run(&format!("{prefix}python3 -c 'import socket,sys; s=socket.create_connection((\"127.0.0.1\",int(sys.argv[1])));s.sendall(open(sys.argv[2],\"rb\").read());print(s.makefile().readline())' {port} {}",request.display()));
            assert_eq!(rc, 0);
            assert_eq!(
                response.split_whitespace().nth(1),
                Some("403"),
                "actual enrolled TCP peer accessed {method} {path}: {response}"
            );
            assert_eq!(w.head(), installed_head);
        }
    }
}

#[test]
fn cad667_cli_catalog_namespace_preserves_legacy_offline_reads() {
    let w = Workspace::new();
    let source = w.source();
    let installed = common::operator_cadence_at(
        w._root.path(),
        &w.daemon.state,
        &["app", "catalog", "install", source.to_str().unwrap()],
    );
    assert!(
        installed.status.success(),
        "catalog install CLI: {}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let row: Value = serde_json::from_slice(&installed.stdout).unwrap();
    let id = row["install_id"].as_str().unwrap();
    let inspected = common::operator_cadence_at(
        w._root.path(),
        &w.daemon.state,
        &["app", "catalog", "show", id],
    );
    assert!(inspected.status.success());
    let legacy = common::operator_cadence_at(w._root.path(), &w.daemon.state, &["app", "ls"]);
    assert!(
        legacy.status.success(),
        "legacy ls must retain its existing behavior"
    );
    let legacy: Value = serde_json::from_slice(&legacy.stdout).unwrap();
    assert!(legacy.as_array().is_some());
}

#[test]
fn cad667_git_failure_retains_pending_journal_and_explicit_recovery_delivers() {
    use std::os::unix::fs::PermissionsExt;
    let w = Workspace::new();
    let hook = w.pm.dir.join(".git/hooks/pre-commit");
    let original = std::fs::read(&hook).ok();
    std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    let head = w.head();
    let failed = w
        .install()
        .expect_err("failed Git delivery must not report committed success");
    assert!(
        failed.to_string().contains("git"),
        "wrong delivery boundary: {failed}"
    );
    assert_eq!(w.head(), head);
    let id = std::fs::read_to_string(w.pm.dir.join(".apps/install-pending.yaml")).unwrap();
    assert!(w
        .pm
        .dir
        .join(".apps/install-journals")
        .join(format!("{id}.yaml"))
        .is_file());
    assert!(
        w.daemon
            .operator_rpc("app_workspace_list", json!({}))
            .is_err(),
        "undelivered installation exposed as ready"
    );
    match original {
        Some(bytes) => std::fs::write(&hook, bytes).unwrap(),
        None => std::fs::remove_file(&hook).unwrap(),
    }
    let journal_path =
        w.pm.dir
            .join(".apps/install-journals")
            .join(format!("{id}.yaml"));
    let journal_bytes = std::fs::read(&journal_path).unwrap();
    for mode in ["extra-file", "record-id", "workflow"] {
        let mut journal: serde_yaml::Value = serde_yaml::from_slice(&journal_bytes).unwrap();
        match mode {
            "extra-file" => {
                journal["files"].as_mapping_mut().unwrap().insert(
                    serde_yaml::Value::String("../escape.md".into()),
                    serde_yaml::Value::String("forged".into()),
                );
            }
            "record-id" => {
                let mut record: serde_yaml::Value =
                    serde_yaml::from_str(journal["record"].as_str().unwrap()).unwrap();
                record["install_id"] = serde_yaml::Value::String("foreign-installation".into());
                journal["record"] =
                    serde_yaml::Value::String(serde_yaml::to_string(&record).unwrap());
            }
            _ => {
                journal["files"]["workflows/blog-post.md"] =
                    serde_yaml::Value::String("not a valid reviewed workflow".into());
            }
        }
        std::fs::write(&journal_path, serde_yaml::to_string(&journal).unwrap()).unwrap();
        assert!(
            w.daemon
                .operator_rpc("app_workspace_recover", json!({"install_id":id}))
                .is_err(),
            "altered journal accepted: {mode}"
        );
        assert_eq!(w.head(), head);
        assert!(!w
            .pm
            .dir
            .join(".apps/installations")
            .join(&id)
            .join("escape.md")
            .exists());
    }
    std::fs::write(&journal_path, journal_bytes).unwrap();
    let recovered = w
        .daemon
        .operator_rpc("app_workspace_recover", json!({"install_id":id}))
        .unwrap();
    assert_eq!(recovered["committed"], true);
    assert_eq!(recovered["approved"], false);
    assert!(!w.pm.dir.join(".apps/install-pending.yaml").exists());
    assert_ne!(w.head(), head);
    assert_eq!(
        w.daemon
            .operator_rpc("app_workspace_list", json!({}))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
