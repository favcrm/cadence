//! CAD-753 host-managed per-installation app records with scoped revisions.
//!
//! Adversarial-first: these tests name the guard before the
//! implementation exists. An operator caller gets typed
//! create/get/list/update with revision CAS and history; an agent
//! caller, a detached child, forged install/context/project fields,
//! cross-install and cross-context probes, and stale/concurrent
//! writes are all refused without mutation.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, plant_member_pane, LaneShell, TestDaemon};
use serde_json::{json, Value};
use std::path::PathBuf;

const PROFILE_A: &str = r#"{"schema":1,"display_name":"Amina Diallo","email":"amina@example.com","tags":["vip"],"consent":{"email":"granted"}}"#;
const PROFILE_B: &str = r#"{"schema":1,"display_name":"Boris Feld","email":"boris@example.com","tags":[],"consent":{"email":"denied"}}"#;

struct Records {
    _root: tempfile::TempDir,
    pm: Pm,
    daemon: TestDaemon,
}

impl Records {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        Self::copy_source(&root.path().join("source"), "blog-post");
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

    fn copy_source(into: &std::path::Path, app: &str) {
        for name in [
            "app.md",
            "workflows/blog-post.md",
            "rubrics/blog.md",
            "templates/brief.md",
            "templates/post.md",
        ] {
            let destination = into.join(name);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::copy(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("apps/blog-post")
                    .join(name),
                &destination,
            )
            .unwrap();
        }
        if app != "blog-post" {
            let manifest = into.join("app.md");
            let text = std::fs::read_to_string(&manifest).unwrap();
            std::fs::write(
                manifest,
                text.replace("app: blog-post", &format!("app: {app}")),
            )
            .unwrap();
        }
    }

    fn install(&self) -> Value {
        self.daemon
            .operator_rpc(
                "app_workspace_install",
                json!({"source": self._root.path().join("source")}),
            )
            .unwrap()
    }

    fn install_second(&self) -> Value {
        let second = self._root.path().join("second");
        Self::copy_source(&second, "blog-post-two");
        self.daemon
            .operator_rpc("app_workspace_install", json!({"source": second}))
            .unwrap()
    }

    fn context(&self, install: &str, label: &str, request: &str) -> Value {
        self.daemon
            .operator_rpc(
                "app_context_create",
                json!({"install_id": install, "label": label, "input_defaults": {}, "request_id": request}),
            )
            .unwrap()["context"]
            .clone()
    }

    fn profile(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    fn create(&self, install: &str, context: &str, record: &str, profile: Value) -> Value {
        self.daemon
            .operator_rpc(
                "app_record_create",
                json!({"install_id": install, "context_id": context, "record_id": record, "profile": profile}),
            )
            .unwrap()
    }

    fn show(&self, install: &str, context: &str, record: &str) -> Value {
        self.daemon
            .operator_rpc(
                "app_record_show",
                json!({"install_id": install, "context_id": context, "record_id": record}),
            )
            .unwrap()
    }
}

#[test]
fn cad753_operator_record_cas_roundtrip_with_history() {
    let w = Records::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap();

    let created = w.create(
        install,
        context_id,
        "customer-1",
        Records::profile(PROFILE_A),
    );
    assert_eq!(created["record"]["revision"], 1);
    assert_eq!(created["record"]["profile"]["display_name"], "Amina Diallo");
    assert!(created["record"]["digest"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(created["record"]["history"].as_array().unwrap().len(), 1);
    assert_eq!(created["record"]["history"][0]["actor"], "operator");

    let shown = w.show(install, context_id, "customer-1");
    assert_eq!(shown["record"], created["record"]);

    let listed = w
        .daemon
        .operator_rpc(
            "app_record_list",
            json!({"install_id": install, "context_id": context_id}),
        )
        .unwrap();
    assert_eq!(listed["records"].as_array().unwrap().len(), 1);

    let updated = w
        .daemon
        .operator_rpc(
            "app_record_update",
            json!({"install_id": install, "context_id": context_id, "record_id": "customer-1", "expected_revision": 1, "profile": Records::profile(PROFILE_B)}),
        )
        .unwrap();
    assert_eq!(updated["record"]["revision"], 2);
    assert_ne!(updated["record"]["digest"], created["record"]["digest"]);
    assert_eq!(updated["record"]["history"].as_array().unwrap().len(), 2);
    assert_eq!(updated["record"]["profile"]["display_name"], "Boris Feld");
}

#[test]
fn cad753_same_record_id_is_independent_across_installations() {
    let w = Records::new();
    let first = w.install();
    let second = w.install_second();
    let a = first["install_id"].as_str().unwrap();
    let b = second["install_id"].as_str().unwrap();
    assert_ne!(a, b);
    let ctx_a = w.context(a, "Client", "ctx-a")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let ctx_b = w.context(b, "Client", "ctx-b")["id"]
        .as_str()
        .unwrap()
        .to_string();

    w.create(a, &ctx_a, "customer-1", Records::profile(PROFILE_A));
    w.create(b, &ctx_b, "customer-1", Records::profile(PROFILE_B));

    assert_eq!(
        w.show(a, &ctx_a, "customer-1")["record"]["profile"]["display_name"],
        "Amina Diallo"
    );
    assert_eq!(
        w.show(b, &ctx_b, "customer-1")["record"]["profile"]["display_name"],
        "Boris Feld"
    );

    // An update in one installation never touches its sibling.
    w.daemon
        .operator_rpc(
            "app_record_update",
            json!({"install_id": a, "context_id": ctx_a, "record_id": "customer-1", "expected_revision": 1, "profile": Records::profile(PROFILE_B)}),
        )
        .unwrap();
    assert_eq!(w.show(a, &ctx_a, "customer-1")["record"]["revision"], 2);
    assert_eq!(w.show(b, &ctx_b, "customer-1")["record"]["revision"], 1);
    assert_eq!(
        w.show(b, &ctx_b, "customer-1")["record"]["profile"]["display_name"],
        "Boris Feld"
    );
}

#[test]
fn cad753_cross_install_and_cross_context_access_fails() {
    let w = Records::new();
    let first = w.install();
    let second = w.install_second();
    let a = first["install_id"].as_str().unwrap();
    let b = second["install_id"].as_str().unwrap();
    let ctx_a = w.context(a, "Client A", "ctx-a")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let ctx_b = w.context(b, "Client B", "ctx-b")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let ctx_a2 = w.context(a, "Client A2", "ctx-a2")["id"]
        .as_str()
        .unwrap()
        .to_string();
    w.create(a, &ctx_a, "customer-1", Records::profile(PROFILE_A));

    // Cross-install reads and writes fail, both directions.
    for params in [
        json!({"install_id": b, "context_id": ctx_b, "record_id": "customer-1"}),
        json!({"install_id": b, "context_id": ctx_a, "record_id": "customer-1"}),
        json!({"install_id": a, "context_id": ctx_b, "record_id": "customer-1"}),
    ] {
        assert!(
            w.daemon
                .operator_rpc("app_record_show", params.clone())
                .is_err(),
            "cross-install read reached a record: {params}"
        );
    }
    assert!(
        w.daemon
            .operator_rpc(
                "app_record_update",
                json!({"install_id": b, "context_id": ctx_b, "record_id": "customer-1", "expected_revision": 1, "profile": Records::profile(PROFILE_B)}),
            )
            .is_err(),
        "cross-install write reached a record"
    );
    // Cross-context reads, writes and lists fail inside one installation.
    assert!(
        w.daemon
            .operator_rpc(
                "app_record_show",
                json!({"install_id": a, "context_id": ctx_a2, "record_id": "customer-1"}),
            )
            .is_err(),
        "cross-context read reached a record"
    );
    assert!(
        w.daemon
            .operator_rpc(
                "app_record_update",
                json!({"install_id": a, "context_id": ctx_a2, "record_id": "customer-1", "expected_revision": 1, "profile": Records::profile(PROFILE_B)}),
            )
            .is_err(),
        "cross-context write reached a record"
    );
    let listed = w
        .daemon
        .operator_rpc(
            "app_record_list",
            json!({"install_id": a, "context_id": ctx_a2}),
        )
        .unwrap();
    assert!(listed["records"].as_array().unwrap().is_empty());
    // Forged installation and context IDs fail closed.
    for params in [
        json!({"install_id": "no-such-install", "context_id": ctx_a, "record_id": "customer-1"}),
        json!({"install_id": a, "context_id": "ctx-no-such-context", "record_id": "customer-1"}),
    ] {
        assert!(
            w.daemon.operator_rpc("app_record_show", params).is_err(),
            "forged scope reached a record"
        );
    }
    // Nothing above mutated the record.
    assert_eq!(w.show(a, &ctx_a, "customer-1")["record"]["revision"], 1);
}

#[test]
fn cad753_stale_and_concurrent_writes_refuse_without_mutation() {
    let w = Records::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap();
    let created = w.create(
        install,
        context_id,
        "customer-1",
        Records::profile(PROFILE_A),
    );

    // A stale expected revision is refused and changes nothing.
    let stale = w.daemon.operator_rpc(
        "app_record_update",
        json!({"install_id": install, "context_id": context_id, "record_id": "customer-1", "expected_revision": 7, "profile": Records::profile(PROFILE_B)}),
    );
    assert!(stale.is_err(), "stale write was accepted");
    assert_eq!(
        w.show(install, context_id, "customer-1")["record"],
        created["record"]
    );

    // Concurrent updates at the same expected revision: exactly one wins.
    let attempts = 6;
    let results = std::thread::scope(|scope| {
        (0..attempts)
            .map(|_| {
                scope.spawn(|| {
                    w.daemon.operator_rpc(
                        "app_record_update",
                        json!({"install_id": install, "context_id": context_id, "record_id": "customer-1", "expected_revision": 1, "profile": Records::profile(PROFILE_B)}),
                    )
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap().is_ok())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        results.iter().filter(|ok| **ok).count(),
        1,
        "concurrent CAS admitted {results:?}"
    );
    let shown = w.show(install, context_id, "customer-1");
    assert_eq!(shown["record"]["revision"], 2);
    assert_eq!(shown["record"]["history"].as_array().unwrap().len(), 2);
}

#[test]
fn cad753_agent_forged_and_detached_callers_cannot_touch_records() {
    let w = Records::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap();
    w.create(
        install,
        context_id,
        "customer-1",
        Records::profile(PROFILE_A),
    );
    let before = w.show(install, context_id, "customer-1");

    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "record-worker", "claude", None, lane.pid());
    for (method, params) in [
        (
            "app_record_create",
            json!({"install_id": install, "context_id": context_id, "record_id": "customer-2", "profile": Records::profile(PROFILE_A)}),
        ),
        (
            "app_record_show",
            json!({"install_id": install, "context_id": context_id, "record_id": "customer-1"}),
        ),
        (
            "app_record_list",
            json!({"install_id": install, "context_id": context_id}),
        ),
        (
            "app_record_update",
            json!({"install_id": install, "context_id": context_id, "record_id": "customer-1", "expected_revision": 1, "profile": Records::profile(PROFILE_B)}),
        ),
    ] {
        let frame = lane.rpc(&w.daemon.state, method, params);
        assert_eq!(frame["ok"], false, "agent reached {method}");
        assert!(
            frame.to_string().contains("operator"),
            "agent refusal missed caller authority for {method}: {frame}"
        );
    }
    // Forged identity and discovery-link fields never confer access:
    // the connection-bound authority check (for `by`/`actor`) and
    // the exact payload grammar (for `project`/`project_link`)
    // refuse before any record is touched.
    for params in [
        json!({"install_id": install, "context_id": context_id, "record_id": "customer-1", "by": "operator", "actor": "operator"}),
        json!({"install_id": install, "context_id": context_id, "record_id": "customer-1", "project": "client", "project_link": "client"}),
    ] {
        let frame = lane.rpc(&w.daemon.state, "app_record_show", params);
        assert_eq!(
            frame["ok"], false,
            "forged fields reached a record: {frame}"
        );
    }
    // A detached child of the agent — no provable identity — is refused too.
    let request = lane.dir.path().join("detached.json");
    std::fs::write(
        &request,
        cadence_agent::proto::request(
            "app_record_show",
            json!({"install_id": install, "context_id": context_id, "record_id": "customer-1"}),
        )
        .to_string(),
    )
    .unwrap();
    let (rc, output) = lane.run(&format!("setsid python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);s.sendall(open(sys.argv[2],\"rb\").read()+b\"\\n\");print(s.makefile().readline())' {} {}", cadence_agent::client::socket_path(&w.daemon.state).display(), request.display()));
    assert_eq!(rc, 0);
    let frame: Value = serde_json::from_str(output.trim()).unwrap();
    assert_eq!(frame["ok"], false);
    assert!(frame.to_string().contains("operator"));

    // Prove the lane is an enrolled caller, not an unknown-method negative.
    w.show(install, context_id, "customer-1");
    assert_eq!(
        w.show(install, context_id, "customer-1")["record"],
        before["record"]
    );
    assert!(
        w.daemon
            .operator_rpc(
                "app_record_show",
                json!({"install_id": install, "context_id": context_id, "record_id": "customer-2"}),
            )
            .is_err(),
        "agent created a record"
    );
}

#[test]
fn cad753_record_profile_validation_refuses_without_mutation_or_leak() {
    let w = Records::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap();
    let marker = "cad753-private-profile-marker";
    for profile in [
        json!({"schema": 1, "display_name": "", "consent": {"email": "granted"}}),
        json!({"schema": 1, "display_name": marker, "email": "not-an-email", "consent": {"email": "granted"}}),
        json!({"schema": 1, "display_name": marker, "consent": {"email": "maybe"}}),
        json!({"display_name": marker, "consent": {"email": "granted"}}),
        json!({"schema": 1, "display_name": "x".repeat(200), "consent": {"email": "granted"}}),
        json!({"schema": 1, "display_name": marker, "consent": {"email": "granted"}, "database_path": "/tmp/x"}),
    ] {
        let error = w
            .daemon
            .operator_rpc(
                "app_record_create",
                json!({"install_id": install, "context_id": context_id, "record_id": "customer-bad", "profile": profile}),
            )
            .unwrap_err()
            .to_string();
        assert!(!error.contains(marker), "profile content leaked in refusal");
    }
    assert!(w
        .daemon
        .operator_rpc(
            "app_record_show",
            json!({"install_id": install, "context_id": context_id, "record_id": "customer-bad"}),
        )
        .is_err());
}

#[test]
fn cad753_records_stay_out_of_git_and_have_no_http_route() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};
    let w = Records::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap();
    // A distinctive body that must never reach the tracker Git tree.
    let profile = json!({"schema": 1, "display_name": "cad753-record-git-marker Quill", "email": "quill@example.com", "tags": [], "consent": {"email": "granted"}});
    w.create(install, context_id, "customer-1", profile);

    let output = std::process::Command::new("grep")
        .args(["-r", "cad753-record-git-marker", "--exclude-dir=.git", "."])
        .current_dir(&w.pm.dir)
        .output()
        .unwrap();
    assert!(
        output.stdout.is_empty(),
        "record body reached the tracker tree"
    );
    let tracked = std::process::Command::new("git")
        .arg("-C")
        .arg(&w.pm.dir)
        .args(["grep", "-l", "cad753-record-git-marker", "HEAD"])
        .output()
        .unwrap();
    assert!(tracked.stdout.is_empty(), "record body reached Git history");

    // No HTTP route exposes records in this slice; the board peer is a
    // named follow-up, so record-shaped paths must 404 for the operator.
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
    let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &w.daemon.state, port);
    for (method, path, body) in [
        (
            "GET",
            format!("/api/app-installations/{install}/records"),
            String::new(),
        ),
        (
            "POST",
            format!("/api/app-installations/{install}/records"),
            "{}".to_string(),
        ),
        (
            "GET",
            format!("/api/app-installations/{install}/contexts/{context_id}/records"),
            String::new(),
        ),
        ("GET", "/api/app-records".to_string(), String::new()),
    ] {
        let (code, _, _) = common::op::raw(port, &session.request(method, &path, &body));
        assert_eq!(code, 404, "HTTP surface exposed records at {method} {path}");
    }
}

#[test]
fn cad753_cli_record_namespace_roundtrip() {
    let w = Records::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap();
    let profile = w._root.path().join("profile.json");
    std::fs::write(&profile, PROFILE_A).unwrap();
    let updated = w._root.path().join("updated.json");
    std::fs::write(&updated, PROFILE_B).unwrap();

    let created: Value = serde_json::from_slice(
        &common::operator_cadence_at(
            w._root.path(),
            &w.daemon.state,
            &[
                "app",
                "record",
                "create",
                install,
                "--context-id",
                context_id,
                "--record-id",
                "customer-1",
                "--profile",
                profile.to_str().unwrap(),
            ],
        )
        .stdout,
    )
    .unwrap();
    assert_eq!(created["record"]["revision"], 1);
    let shown: Value = serde_json::from_slice(
        &common::operator_cadence_at(
            w._root.path(),
            &w.daemon.state,
            &[
                "app",
                "record",
                "show",
                install,
                "--context-id",
                context_id,
                "--record-id",
                "customer-1",
            ],
        )
        .stdout,
    )
    .unwrap();
    assert_eq!(shown["record"], created["record"]);
    let listed: Value = serde_json::from_slice(
        &common::operator_cadence_at(
            w._root.path(),
            &w.daemon.state,
            &["app", "record", "ls", install, "--context-id", context_id],
        )
        .stdout,
    )
    .unwrap();
    assert_eq!(listed["records"].as_array().unwrap().len(), 1);
    let output = common::operator_cadence_at(
        w._root.path(),
        &w.daemon.state,
        &[
            "app",
            "record",
            "set",
            install,
            "--context-id",
            context_id,
            "--record-id",
            "customer-1",
            "--expected-revision",
            "1",
            "--profile",
            updated.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "record set CLI: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let settled: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(settled["record"]["revision"], 2);
    let stale = common::operator_cadence_at(
        w._root.path(),
        &w.daemon.state,
        &[
            "app",
            "record",
            "set",
            install,
            "--context-id",
            context_id,
            "--record-id",
            "customer-1",
            "--expected-revision",
            "1",
            "--profile",
            updated.to_str().unwrap(),
        ],
    );
    assert!(!stale.status.success(), "stale CLI write was accepted");
}
