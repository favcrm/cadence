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
    let result = w
        .install()
        .expect("project-free operator install must exist");
    let id = result["install_id"]
        .as_str()
        .expect("stable installation ID");
    assert!(result["project"].is_null());
    assert_eq!(result["approved"], false);
    assert_eq!(result["committed"], true);
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
