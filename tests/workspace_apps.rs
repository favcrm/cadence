//! CAD-667 public workspace installation contract. Private state only.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, plant_member_pane, test_port, LaneShell, TestDaemon};
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
        let source = root.path().join("source");
        for name in [
            "app.md",
            "workflows/blog-post.md",
            "rubrics/blog.md",
            "templates/brief.md",
            "templates/post.md",
        ] {
            let destination = source.join(name);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::copy(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("apps/blog-post")
                    .join(name),
                destination,
            )
            .unwrap();
        }
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
        self._root.path().join("source")
    }
    fn install(&self) -> cadence_agent::Result<Value> {
        self.daemon
            .operator_rpc("app_workspace_install", json!({"source": self.source()}))
    }
    /// CAD-864: write an `app-views/v1` descriptor into the source bundle
    /// and declare it in the manifest (`needs.views.contract`). The
    /// descriptor is the CRM worked example with `app` renamed to match
    /// the bundle — a real descriptor exercising the seam, not a
    /// minimized stub.
    fn write_descriptor(&self, descriptor: &str, declare: bool) {
        let views = self.source().join("views");
        std::fs::create_dir_all(&views).unwrap();
        std::fs::write(views.join("app-views-v1.json"), descriptor).unwrap();
        if declare {
            let manifest = self.source().join("app.md");
            let text = std::fs::read_to_string(&manifest).unwrap();
            assert!(
                !text.contains("views:"),
                "source manifest already declares views"
            );
            std::fs::write(
                &manifest,
                text.replace(
                    "  connections: [publish]",
                    "  connections: [publish]\n  views:\n    contract: app-views/v1",
                ),
            )
            .unwrap();
        }
    }
    /// The descriptor bytes a descriptor-bearing source carries: the
    /// contracts crate's CRM example with `app` rebound to `blog-post`.
    fn descriptor_text() -> String {
        let text = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("contracts/app-views/v1/examples/crm.json"),
        )
        .unwrap();
        text.replace("\"app\": \"crm\"", "\"app\": \"blog-post\"")
    }
    /// CAD-867: write an `app-bindings/v1` companion into the source
    /// bundle and declare it in the manifest (`needs.bindings.contract`).
    /// The binding is the contracts crate's CRM example with `app`
    /// rebound — real CustomerProfile projections, not a stub.
    fn write_binding(&self, binding: &str, declare: bool) {
        let dir = self.source().join("bindings");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("app-bindings-v1.json"), binding).unwrap();
        if declare {
            let manifest = self.source().join("app.md");
            let text = std::fs::read_to_string(&manifest).unwrap();
            assert!(
                !text.contains("bindings:"),
                "source manifest already declares bindings"
            );
            // The declaration must sit under `needs:` beside any views
            // declaration — insert after the views block when present.
            let anchor = if text.contains("  views:") {
                "  views:\n    contract: app-views/v1"
            } else {
                "  connections: [publish]"
            };
            std::fs::write(
                &manifest,
                text.replace(
                    anchor,
                    &format!("{anchor}\n  bindings:\n    contract: app-bindings/v1"),
                ),
            )
            .unwrap();
        }
    }
    /// The binding bytes a companion-bearing source carries: the CRM
    /// binding example with `app` rebound to `blog-post`.
    fn binding_text() -> String {
        let text = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("contracts/app-bindings/v1/examples/crm.json"),
        )
        .unwrap();
        text.replace("\"app\": \"crm\"", "\"app\": \"blog-post\"")
    }
    fn show(&self, id: &str) -> Value {
        self.daemon
            .operator_rpc("app_workspace_show", json!({"install_id": id}))
            .unwrap()
    }
    fn upgrade_check(&self, installed: &Value) -> Value {
        self.daemon
            .operator_rpc(
                "app_workspace_upgrade_check",
                json!({
                    "install_id":installed["install_id"], "source":self.source(),
                    "expected_digest":installed["digest"],
                    "expected_generation":installed["catalog_generation"]
                }),
            )
            .unwrap()
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
fn cad743_upgrade_is_operator_only_race_checked_and_preserves_identity() {
    let w = Workspace::new();
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap();
    let old_digest = installed["digest"].as_str().unwrap();
    let generation = installed["catalog_generation"].as_str().unwrap();
    let old_bundle =
        w.pm.dir
            .join(".apps/installations")
            .join(id)
            .join("bundle/app.md");
    let old_text = std::fs::read_to_string(&old_bundle).unwrap();
    let source = w.source();
    let source_manifest = source.join("app.md");
    std::fs::write(
        &source_manifest,
        old_text.replace("version: 0.1.0", "version: 0.2.0"),
    )
    .unwrap();
    let head_before_check = w.head();
    let proposed = w.upgrade_check(&installed);
    assert_eq!(proposed["committed"], false);
    assert_eq!(proposed["version"], "0.2.0");
    assert!(proposed["structural_diff"]["changed"]
        .as_array()
        .unwrap()
        .contains(&json!("app.md")));
    assert_eq!(w.head(), head_before_check);
    assert!(!w.pm.dir.join(".apps/upgrade-journals").exists());
    let params = json!({"install_id":id,"source":source,"expected_digest":old_digest,
        "expected_generation":generation,"expected_new_digest":proposed["digest"],
        "request_id":"upgrade-once"});
    std::fs::write(
        &source_manifest,
        old_text.replace("version: 0.1.0", "version: 0.3.0"),
    )
    .unwrap();
    let changed = w
        .daemon
        .operator_rpc("app_workspace_upgrade", params.clone())
        .unwrap_err();
    assert!(changed
        .to_string()
        .contains("proposed workspace bundle digest changed"));
    assert_eq!(w.head(), head_before_check);
    assert!(!w.pm.dir.join(".apps/upgrade-journals").exists());
    std::fs::write(
        &source_manifest,
        old_text.replace("version: 0.1.0", "version: 0.2.0"),
    )
    .unwrap();
    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "upgrade-worker", "claude", None, lane.pid());
    for attack in [
        params.clone(),
        json!({"install_id":id,"source":source,
        "expected_digest":old_digest,"expected_generation":generation,
        "expected_new_digest":proposed["digest"],"request_id":"upgrade-once",
        "actor":"operator","approved":true}),
    ] {
        let frame = lane.rpc(&w.daemon.state, "app_workspace_upgrade", attack);
        assert_eq!(
            frame["ok"], false,
            "agent upgraded the installation: {frame}"
        );
        assert!(
            frame.to_string().contains("operator") || frame.to_string().contains("authority"),
            "agent denial missed the operator gate: {frame}"
        );
    }
    let detached_request = lane.dir.path().join("upgrade-detached.json");
    std::fs::write(
        &detached_request,
        cadence_agent::proto::request("app_workspace_upgrade", params.clone()).to_string(),
    )
    .unwrap();
    let (rc, output) = lane.run(&format!("setsid python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);s.sendall(open(sys.argv[2],\"rb\").read()+b\"\\n\");print(s.makefile().readline())' {} {}", cadence_agent::client::socket_path(&w.daemon.state).display(), detached_request.display()));
    assert_eq!(rc, 0);
    let frame: Value = serde_json::from_str(output.trim()).unwrap();
    assert_eq!(frame["ok"], false);
    assert!(
        frame.to_string().contains("operator"),
        "detached child denial missed the operator gate: {frame}"
    );
    let forged = w.daemon.operator_rpc(
        "app_workspace_upgrade",
        json!({"install_id":id,
        "source":source,"expected_digest":old_digest,"expected_generation":generation,
        "expected_new_digest":proposed["digest"],"request_id":"upgrade-once","approved":true}),
    );
    assert!(forged.is_err(), "forged approval field was accepted");
    let results = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            w.daemon
                .operator_rpc("app_workspace_upgrade", params.clone())
        });
        let b = scope.spawn(|| {
            w.daemon
                .operator_rpc("app_workspace_upgrade", params.clone())
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    let first = results.0.unwrap();
    let second = results.1.unwrap();
    assert_eq!(
        first["digest"], second["digest"],
        "repeat request was not idempotent"
    );
    assert_ne!(first["digest"], old_digest);
    assert_eq!(first["install_id"], id);
    assert_eq!(first["approved"], false);
    assert_eq!(std::fs::read_to_string(&old_bundle).unwrap(), old_text);
    let stale = w.daemon.operator_rpc(
        "app_workspace_upgrade",
        json!({"install_id":id,
        "source":source,"expected_digest":old_digest,"expected_generation":generation,
        "expected_new_digest":proposed["digest"],"request_id":"conflicting-upgrade"}),
    );
    assert!(stale.is_err(), "stale expected digest updated again");
    std::fs::write(&source_manifest, "the source is no longer a valid bundle").unwrap();
    let replay = w
        .daemon
        .operator_rpc("app_workspace_upgrade", params.clone())
        .unwrap();
    assert_eq!(
        replay["idempotent"], true,
        "committed replay fetched the mutable source"
    );
    let second_source = w._root.path().join("second-source");
    for name in [
        "app.md",
        "workflows/blog-post.md",
        "rubrics/blog.md",
        "templates/brief.md",
        "templates/post.md",
    ] {
        let destination = second_source.join(name);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("apps/blog-post")
                .join(name),
            destination,
        )
        .unwrap();
    }
    let second_manifest = second_source.join("app.md");
    let second_text = std::fs::read_to_string(&second_manifest).unwrap();
    std::fs::write(
        &second_manifest,
        second_text.replace("app: blog-post", "app: second-post"),
    )
    .unwrap();
    let second = w
        .daemon
        .operator_rpc("app_workspace_install", json!({"source":second_source}))
        .unwrap();
    assert_ne!(second["catalog_generation"], replay["catalog_generation"]);
    let replay_after_unrelated_install = w
        .daemon
        .operator_rpc("app_workspace_upgrade", params)
        .unwrap();
    assert_eq!(replay_after_unrelated_install["idempotent"], true);
    assert_eq!(replay_after_unrelated_install["digest"], proposed["digest"]);
    let shown = w
        .daemon
        .operator_rpc("app_workspace_show", json!({"install_id":id}))
        .unwrap();
    assert_eq!(shown["version"], "0.2.0");
}

#[test]
fn cad743_upgrade_refuses_full_binding_history_before_journal_mutation() {
    let w = Workspace::new();
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap();
    let source_manifest = w.source().join("app.md");
    let original = std::fs::read_to_string(&source_manifest).unwrap();
    std::fs::write(
        &source_manifest,
        original
            .replace("version: 0.1.0", "version: 0.2.0")
            .replace(
                "  connections: [publish]",
                "  connections: [publish]\n  capabilities:\n    publication:\n      schema: 1\n      capability: text.publish\n      version: 1\n      action: publish\n      resource_kind: connection_account\n      effect: send",
            ),
    )
    .unwrap();
    let proposed = w.upgrade_check(&installed);
    let mut conn = rusqlite::Connection::open(w.daemon.state.join("cadence.sqlite3")).unwrap();
    let old_config = json!({"schema":1,"install_id":id,"context":null,
        "bundle_digest":installed["digest"]});
    let old_receipt = cadence_agent::store::app_runs::material_digest(&json!({
        "kind":"app-binding-v1","install_id":id,"context_id":null,
        "slot":"publication","config":old_config,
    }));
    let tx = conn.transaction().unwrap();
    for index in 0..1000 {
        tx.execute(
            "INSERT INTO app_bindings
             (id,install_id,context_id,scope_key,slot,revision,state,config,digest,request_id,created,updated)
             VALUES(?1,?2,NULL,'installation','publication',1,'revoked',?3,?4,?5,1,1)",
            rusqlite::params![
                format!("old-binding-{index}"),
                id,
                old_config.to_string(),
                old_receipt,
                format!("old-request-{index}"),
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    drop(conn);
    let head = w.head();
    let refusal = w.daemon.operator_rpc(
        "app_workspace_upgrade",
        json!({"install_id":id,"source":w.source(),"expected_digest":installed["digest"],
            "expected_generation":installed["catalog_generation"],
            "expected_new_digest":proposed["digest"],"request_id":"capacity-blocked"}),
    );
    assert!(
        refusal
            .as_ref()
            .is_err_and(|error| error.to_string().contains("binding capacity")),
        "upgrade must refuse specifically for exhausted binding capacity: {refusal:?}"
    );
    assert_eq!(w.head(), head);
    assert!(!w.pm.dir.join(".apps/upgrade-pending.yaml").exists());
    assert!(!w.pm.dir.join(".apps/upgrade-journals").exists());
}

#[test]
fn cad743_upgrade_after_one_hundred_old_bindings_can_create_new_version_binding() {
    let w = Workspace::new();
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap();
    let source_manifest = w.source().join("app.md");
    let original = std::fs::read_to_string(&source_manifest).unwrap();
    std::fs::write(
        &source_manifest,
        original
            .replace("version: 0.1.0", "version: 0.2.0")
            .replace(
                "  connections: [publish]",
                "  connections: [publish]\n  capabilities:\n    publication:\n      schema: 1\n      capability: text.publish\n      version: 1\n      action: publish\n      resource_kind: connection_account\n      effect: send",
            ),
    )
    .unwrap();
    let proposed = w.upgrade_check(&installed);
    let mut conn = rusqlite::Connection::open(w.daemon.state.join("cadence.sqlite3")).unwrap();
    let old_config = json!({"schema":1,"install_id":id,"context":null,
        "bundle_digest":installed["digest"]});
    let old_receipt = cadence_agent::store::app_runs::material_digest(&json!({
        "kind":"app-binding-v1","install_id":id,"context_id":null,
        "slot":"publication","config":old_config,
    }));
    let tx = conn.transaction().unwrap();
    for index in 0..100 {
        tx.execute(
            "INSERT INTO app_bindings
             (id,install_id,context_id,scope_key,slot,revision,state,config,digest,request_id,created,updated)
             VALUES(?1,?2,NULL,'installation','publication',1,'revoked',?3,?4,?5,1,1)",
            rusqlite::params![
                format!("old-binding-{index}"), id, old_config.to_string(),
                old_receipt, format!("old-request-{index}"),
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    drop(conn);
    let upgraded = w
        .daemon
        .operator_rpc(
            "app_workspace_upgrade",
            json!({"install_id":id,"source":w.source(),"expected_digest":installed["digest"],
            "expected_generation":installed["catalog_generation"],
            "expected_new_digest":proposed["digest"],"request_id":"capacity-available"}),
        )
        .unwrap();
    assert_eq!(upgraded["digest"], proposed["digest"]);
    let store = cadence_agent::store::Store::open(&w.daemon.state.join("cadence.sqlite3")).unwrap();
    let binding = store
        .app_binding_create(
            id,
            None,
            "publication",
            &json!({"schema":1,"install_id":id,"context":null,"bundle_digest":proposed["digest"]}),
            "new-version-binding",
        )
        .unwrap();
    assert_eq!(
        binding["binding"]["config"]["bundle_digest"],
        proposed["digest"]
    );
    let listed = w
        .daemon
        .operator_rpc("app_binding_list", json!({"install_id":id}))
        .unwrap();
    assert_eq!(listed["bindings"][0]["id"], binding["binding"]["id"]);
    assert_eq!(listed["truncated"], true);
}

#[test]
fn cad743_upgrade_compatibility_inspects_configured_bindings_beyond_inventory_limit() {
    let w = Workspace::new();
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap();
    let manifest = w.source().join("app.md");
    let original = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, original
        .replace("version: 0.1.0", "version: 0.2.0")
        .replace("  connections: [publish]", "  connections: [publish]\n  capabilities:\n    publication:\n      schema: 1\n      capability: text.publish\n      version: 1\n      action: publish\n      resource_kind: connection_account\n      effect: send"))
        .unwrap();
    let proposed = w.upgrade_check(&installed);
    let mut conn = rusqlite::Connection::open(w.daemon.state.join("cadence.sqlite3")).unwrap();
    let tx = conn.transaction().unwrap();
    for index in 0..101 {
        let digest = if index == 100 {
            &proposed["digest"]
        } else {
            &installed["digest"]
        };
        let config = json!({"schema":1,"install_id":id,"context":null,
            "bundle_digest":digest,"declaration":null});
        let receipt = cadence_agent::store::app_runs::material_digest(&json!({
            "kind":"app-binding-v1","install_id":id,"context_id":null,
            "slot":"publication","config":config,
        }));
        tx.execute("INSERT INTO app_bindings
            (id,install_id,context_id,scope_key,slot,revision,state,config,digest,request_id,created,updated)
            VALUES(?1,?2,NULL,?3,'publication',1,'configured',?4,?5,?6,?7,?7)",
            rusqlite::params![format!("binding-{index:03}"),id,format!("context:fake-{index:03}"),
                config.to_string(),receipt,format!("request-{index:03}"),index as f64],
        ).unwrap();
    }
    tx.commit().unwrap();
    drop(conn);
    let listed = w
        .daemon
        .operator_rpc("app_binding_list", json!({"install_id":id}))
        .unwrap();
    assert_eq!(listed["bindings"].as_array().unwrap().len(), 100);
    assert_eq!(listed["truncated"], true);
    let upgraded = w
        .daemon
        .operator_rpc(
            "app_workspace_upgrade",
            json!({
                "install_id":id,"source":w.source(),"expected_digest":installed["digest"],
                "expected_generation":installed["catalog_generation"],
                "expected_new_digest":proposed["digest"],"request_id":"complete-binding-preflight"
            }),
        )
        .unwrap();
    let required = upgraded["compatibility"]["rebind_required"]
        .as_array()
        .unwrap();
    assert_eq!(required.len(), 101);
    assert!(required.contains(&json!("binding-000")));
    assert_eq!(
        upgraded["compatibility"]["incompatible_bindings"]
            .as_array()
            .unwrap()
            .len(),
        101
    );
}

#[test]
fn cad743_upgrade_refuses_nonterminal_run_and_retains_terminal_history() {
    let w = Workspace::new();
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap();
    let old_digest = installed["digest"].as_str().unwrap();
    let generation = installed["catalog_generation"].as_str().unwrap();
    let context = w.daemon.operator_rpc("app_context_create",
        json!({"install_id":id,"label":"Fav Limited","input_defaults":{},"request_id":"fav-limited"})).unwrap();
    let context_id = context["context"]["id"].as_str().unwrap();
    let db = rusqlite::Connection::open(w.daemon.state.join("cadence.sqlite3")).unwrap();
    for (run, state) in [
        ("kept-run", "succeeded"),
        ("pending-run", "awaiting_approval"),
    ] {
        db.execute("INSERT INTO app_runs(id,install_id,epoch,bundle_digest,snapshot,snapshot_digest,owner_pm,request_id,state,created,updated) VALUES(?,?,1,?,'{}','snapshot','owner',?,?,1,1)",
            rusqlite::params![run,id,old_digest,run,state]).unwrap();
    }
    let source = w.source();
    let manifest = source.join("app.md");
    let original = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        original.replace("version: 0.1.0", "version: 0.2.0"),
    )
    .unwrap();
    let proposed = w.upgrade_check(&installed);
    let params = json!({"install_id":id,"source":source,"expected_digest":old_digest,
        "expected_generation":generation,"expected_new_digest":proposed["digest"],
        "request_id":"after-pending"});
    let head = w.head();
    let blocked = w
        .daemon
        .operator_rpc("app_workspace_upgrade", params.clone())
        .unwrap_err();
    assert!(
        blocked.to_string().contains("nonterminal run"),
        "wrong gate: {blocked}"
    );
    assert_eq!(w.head(), head);
    assert!(!w.pm.dir.join(".apps/upgrade-pending.yaml").exists());
    assert!(!w.pm.dir.join(".apps/upgrade-journals").exists());
    db.execute(
        "UPDATE app_runs SET state='cancelled' WHERE id='pending-run'",
        [],
    )
    .unwrap();
    let upgraded = w
        .daemon
        .operator_rpc("app_workspace_upgrade", params)
        .unwrap();
    assert_eq!(upgraded["approved"], false);
    assert_eq!(upgraded["install_id"], id);
    assert_eq!(
        upgraded["compatibility"]["incompatible_contexts"],
        json!([])
    );
    let kept_context = w
        .daemon
        .operator_rpc(
            "app_context_show",
            json!({"install_id":id,"context_id":context_id}),
        )
        .unwrap();
    assert_eq!(kept_context["context"]["config"]["label"], "Fav Limited");
    let historical = w
        .daemon
        .operator_rpc("app_run_show", json!({"run_id":"kept-run"}))
        .unwrap();
    assert_eq!(historical["state"], "succeeded");
    assert_eq!(historical["snapshot"], json!({}));
    let cancelled = w
        .daemon
        .operator_rpc("app_run_show", json!({"run_id":"pending-run"}))
        .unwrap();
    assert_eq!(cancelled["state"], "cancelled");
    assert_eq!(
        std::fs::read_to_string(
            w.pm.dir
                .join(".apps/installations")
                .join(id)
                .join("bundle/app.md")
        )
        .unwrap(),
        original
    );
}

#[test]
fn cad743_invalid_new_bundle_never_stages_or_revokes_old_approval() {
    let w = Workspace::new();
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap();
    let old_digest = installed["digest"].as_str().unwrap();
    let generation = installed["catalog_generation"].as_str().unwrap();
    let source = w.source();
    std::fs::write(source.join("app.md"), "not a valid app manifest").unwrap();
    let head = w.head();
    assert!(w
        .daemon
        .operator_rpc(
            "app_workspace_upgrade",
            json!({"install_id":id,
        "source":source,"expected_digest":old_digest,"expected_generation":generation,
        "expected_new_digest":"sha256:unavailable","request_id":"invalid-bundle"})
        )
        .is_err());
    assert_eq!(w.head(), head);
    assert!(!w.pm.dir.join(".apps/upgrade-pending.yaml").exists());
    assert!(!w.pm.dir.join(".apps/upgrade-journals").exists());
    assert_eq!(
        w.daemon
            .operator_rpc("app_workspace_show", json!({"install_id":id}))
            .unwrap()["digest"],
        old_digest
    );
}

#[test]
fn cad743_interrupted_upgrade_requires_explicit_recovery() {
    use std::os::unix::fs::PermissionsExt;
    let w = Workspace::new();
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap();
    let old_digest = installed["digest"].as_str().unwrap();
    let generation = installed["catalog_generation"].as_str().unwrap();
    let old_bundle =
        w.pm.dir
            .join(".apps/installations")
            .join(id)
            .join("bundle/app.md");
    let old_text = std::fs::read_to_string(&old_bundle).unwrap();
    let source = w.source();
    std::fs::write(
        source.join("app.md"),
        old_text.replace("version: 0.1.0", "version: 0.2.0"),
    )
    .unwrap();
    let proposed = w.upgrade_check(&installed);
    let hook = w.pm.dir.join(".git/hooks/pre-commit");
    let original_hook = std::fs::read(&hook).ok();
    std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    let head = w.head();
    let failure = w
        .daemon
        .operator_rpc(
            "app_workspace_upgrade",
            json!({"install_id":id,
        "source":source,"expected_digest":old_digest,"expected_generation":generation,
        "expected_new_digest":proposed["digest"],"request_id":"recover-once"}),
        )
        .unwrap_err();
    assert!(failure.to_string().contains("upgrade-recover"));
    assert_eq!(w.head(), head);
    assert!(w.pm.dir.join(".apps/upgrade-pending.yaml").is_file());
    assert_eq!(std::fs::read_to_string(&old_bundle).unwrap(), old_text);
    assert!(w
        .daemon
        .operator_rpc("app_workspace_show", json!({"install_id":id}))
        .is_err());
    match original_hook {
        Some(bytes) => std::fs::write(&hook, bytes).unwrap(),
        None => std::fs::remove_file(&hook).unwrap(),
    }
    let journal_path =
        w.pm.dir
            .join(".apps/upgrade-journals")
            .join(id)
            .join("recover-once.yaml");
    let journal_bytes = std::fs::read(&journal_path).unwrap();
    let mut forged: serde_yaml::Value = serde_yaml::from_slice(&journal_bytes).unwrap();
    forged["files"].as_mapping_mut().unwrap().insert(
        serde_yaml::Value::String("../escape.md".into()),
        serde_yaml::Value::String("forged".into()),
    );
    std::fs::write(&journal_path, serde_yaml::to_string(&forged).unwrap()).unwrap();
    assert!(
        w.daemon
            .operator_rpc(
                "app_workspace_upgrade_recover",
                json!({"install_id":id,"request_id":"recover-once"})
            )
            .is_err(),
        "forged upgrade journal escaped the confined bundle"
    );
    assert_eq!(w.head(), head);
    std::fs::write(&journal_path, journal_bytes).unwrap();
    std::fs::remove_file(w.pm.dir.join(".apps/upgrade-pending.yaml")).unwrap();
    // Reconstruct the instant after the durable journal write but before the
    // pending marker write. Recovery must work even when nothing was applied.
    let staged: serde_yaml::Value =
        serde_yaml::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
    std::fs::write(
        w.pm.dir.join(".apps/catalog.yaml"),
        staged["before_catalog"].as_str().unwrap(),
    )
    .unwrap();
    std::fs::write(
        w.pm.dir
            .join(".apps/installations")
            .join(id)
            .join("record.yaml"),
        staged["before_record"].as_str().unwrap(),
    )
    .unwrap();
    let staged_replay = w.daemon.operator_rpc(
        "app_workspace_upgrade",
        json!({"install_id":id,"source":source,"expected_digest":old_digest,
        "expected_generation":generation,"expected_new_digest":proposed["digest"],
        "request_id":"recover-once"}),
    );
    assert!(
        staged_replay
            .as_ref()
            .is_err_and(|error| error.to_string().contains("upgrade-recover")),
        "uncommitted journal was mistaken for a committed replay: {staged_replay:?}"
    );
    let recovered = w
        .daemon
        .operator_rpc(
            "app_workspace_upgrade_recover",
            json!({"install_id":id,"request_id":"recover-once"}),
        )
        .unwrap();
    assert_eq!(recovered["version"], "0.2.0");
    assert_eq!(recovered["install_id"], id);
    assert!(!w.pm.dir.join(".apps/upgrade-pending.yaml").exists());
    assert_eq!(std::fs::read_to_string(&old_bundle).unwrap(), old_text);
}

#[test]
fn cad743_http_upgrade_has_same_operator_and_field_gate() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};
    let w = Workspace::new();
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap();
    let original = std::fs::read_to_string(w.source().join("app.md")).unwrap();
    std::fs::write(
        w.source().join("app.md"),
        original.replace("version: 0.1.0", "version: 0.2.0"),
    )
    .unwrap();
    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "upgrade-http-agent", "claude", None, lane.pid());
    let lease = test_port();
    let port = lease.port;
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
    let path = format!("/api/app-installations/{id}/upgrade");
    let check_path = format!("/api/app-installations/{id}/upgrade/check");
    let check_body = json!({"source":w.source(),"expected_digest":installed["digest"],
        "expected_generation":installed["catalog_generation"]})
    .to_string();
    let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &w.daemon.state, port);
    let (code, _, response) =
        common::op::raw(port, &session.request("POST", &check_path, &check_body));
    assert_eq!(code, 200, "operator HTTP upgrade check: {response}");
    let proposed: Value = serde_json::from_str(&response).unwrap();
    let body = json!({"source":w.source(),"expected_digest":installed["digest"],
        "expected_generation":installed["catalog_generation"],
        "expected_new_digest":proposed["digest"],"request_id":"http-upgrade"})
    .to_string();
    let head = w.head();
    for prefix in ["", "setsid "] {
        for (path, body) in [(&check_path, &check_body), (&path, &body)] {
            let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &w.daemon.state, port);
            let request = lane.dir.path().join(format!(
                "upgrade-http-stolen-{}-{}.txt",
                prefix.len(),
                path.len()
            ));
            std::fs::write(&request, session.request_as("POST", path, body, "")).unwrap();
            let (rc, response) = lane.run(&format!("{prefix}python3 -c 'import socket,sys; s=socket.create_connection((\"127.0.0.1\",int(sys.argv[1])));s.sendall(open(sys.argv[2],\"rb\").read());print(s.makefile().readline())' {port} {}", request.display()));
            assert_eq!(rc, 0);
            assert_eq!(
                response.split_whitespace().nth(1),
                Some("403"),
                "agent HTTP peer accessed {path}: {response}"
            );
        }
    }
    assert_eq!(w.head(), head);
    let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &w.daemon.state, port);
    let forged = json!({"source":w.source(),"expected_digest":installed["digest"],
        "expected_generation":installed["catalog_generation"],
        "expected_new_digest":proposed["digest"],"request_id":"http-upgrade",
        "approved":true})
    .to_string();
    let (code, _, _) = common::op::raw(port, &session.request("POST", &path, &forged));
    assert_eq!(code, 400);
    assert_eq!(w.head(), head);
    let (code, _, response) = common::op::raw(port, &session.request("POST", &path, &body));
    assert_eq!(code, 200, "operator HTTP upgrade: {response}");
    let row: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(row["version"], "0.2.0");
    assert_eq!(row["approved"], false);
}

#[test]
fn cad743_cli_upgrade_uses_expected_digest_and_generation() {
    let w = Workspace::new();
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap();
    let original = std::fs::read_to_string(w.source().join("app.md")).unwrap();
    std::fs::write(
        w.source().join("app.md"),
        original.replace("version: 0.1.0", "version: 0.2.0"),
    )
    .unwrap();
    let check = common::operator_cadence_at(
        w._root.path(),
        &w.daemon.state,
        &[
            "app",
            "catalog",
            "upgrade-check",
            id,
            w.source().to_str().unwrap(),
            "--expected-digest",
            installed["digest"].as_str().unwrap(),
            "--expected-generation",
            installed["catalog_generation"].as_str().unwrap(),
        ],
    );
    assert!(
        check.status.success(),
        "CLI upgrade check: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    let proposed: Value = serde_json::from_slice(&check.stdout).unwrap();
    assert_eq!(proposed["committed"], false);
    let command = common::operator_cadence_at(
        w._root.path(),
        &w.daemon.state,
        &[
            "app",
            "catalog",
            "upgrade",
            id,
            w.source().to_str().unwrap(),
            "--expected-digest",
            installed["digest"].as_str().unwrap(),
            "--expected-generation",
            installed["catalog_generation"].as_str().unwrap(),
            "--expected-new-digest",
            proposed["digest"].as_str().unwrap(),
            "--request-id",
            "cli-upgrade",
        ],
    );
    assert!(
        command.status.success(),
        "CLI upgrade: {}",
        String::from_utf8_lossy(&command.stderr)
    );
    let row: Value = serde_json::from_slice(&command.stdout).unwrap();
    assert_eq!(row["install_id"], id);
    assert_eq!(row["version"], "0.2.0");
    assert_eq!(row["approved"], false);
    std::fs::remove_dir_all(w.source()).unwrap();
    let repeated = common::operator_cadence_at(
        w._root.path(),
        &w.daemon.state,
        &[
            "app",
            "catalog",
            "upgrade",
            id,
            w.source().to_str().unwrap(),
            "--expected-digest",
            installed["digest"].as_str().unwrap(),
            "--expected-generation",
            installed["catalog_generation"].as_str().unwrap(),
            "--expected-new-digest",
            proposed["digest"].as_str().unwrap(),
            "--request-id",
            "cli-upgrade",
        ],
    );
    assert!(
        repeated.status.success(),
        "CLI replay after source removal: {}",
        String::from_utf8_lossy(&repeated.stderr)
    );
    let repeated_row: Value = serde_json::from_slice(&repeated.stdout).unwrap();
    assert_eq!(repeated_row["idempotent"], true);
}

/// Operator-run evidence against a private *copy*. Never point these variables at live state.
#[test]
#[ignore = "requires an isolated pilot PM/SQLite copy and a proposed Social Content source"]
fn cad743_isolated_social_content_pilot_upgrade() {
    let pm_dir = PathBuf::from(std::env::var("CAD743_COPY_PM").unwrap());
    let state = PathBuf::from(std::env::var("CAD743_COPY_STATE").unwrap());
    let source = PathBuf::from(std::env::var("CAD743_PROPOSED_SOURCE").unwrap());
    let id = std::env::var("CAD743_COPY_INSTALL_ID").unwrap();
    assert!(pm_dir.starts_with("/tmp/cad743-pilot-proof/"));
    assert!(state.starts_with("/tmp/cad743-pilot-proof/"));
    assert!(source.is_dir());
    let old_bundle = pm_dir
        .join(".apps/installations")
        .join(&id)
        .join("bundle/app.md");
    let old_bytes = std::fs::read(&old_bundle).unwrap();
    let before = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    assert_eq!(
        before
            .query_row("SELECT version FROM schema_version", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        27
    );
    let old_approval: (i64, String, String) = before
        .query_row(
            "SELECT epoch,digest,state FROM app_install_capabilities WHERE install_id=?",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    drop(before);
    let opts = daemon_opts();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let daemon = TestDaemon::start_on_opts(state.clone(), opts);
    let installed = daemon
        .operator_rpc("app_workspace_show", json!({"install_id":id}))
        .unwrap();
    assert_eq!(installed["version"], "0.4.0");
    let contexts_before = daemon
        .operator_rpc("app_context_list", json!({"install_id":id}))
        .unwrap();
    let bindings_before = daemon
        .operator_rpc("app_binding_list", json!({"install_id":id}))
        .unwrap();
    assert_eq!(contexts_before["contexts"].as_array().unwrap().len(), 1);
    assert_eq!(bindings_before["bindings"].as_array().unwrap().len(), 3);
    let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    assert_eq!(
        db.query_row("SELECT version FROM schema_version", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        28
    );
    let retained_approval: (i64, String, String) = db
        .query_row(
            "SELECT epoch,digest,state FROM app_capability_epochs WHERE install_id=?",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(retained_approval, old_approval);
    let runs: Vec<String> = db
        .prepare("SELECT id FROM app_runs WHERE install_id=? ORDER BY id")
        .unwrap()
        .query_map([&id], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(runs.len() >= 2);
    let old_runs: Vec<Value> = runs
        .iter()
        .map(|run| {
            daemon
                .operator_rpc("app_run_show", json!({"run_id":run}))
                .unwrap()
        })
        .collect();
    assert!(old_runs.iter().all(|run| run["state"] == "failed"));
    let check = daemon
        .operator_rpc(
            "app_workspace_upgrade_check",
            json!({
        "install_id":id,"source":source,"expected_digest":installed["digest"],
        "expected_generation":installed["catalog_generation"]}),
        )
        .unwrap();
    assert_eq!(check["version"], "0.5.0");
    let apply = json!({"install_id":id,"source":source,"expected_digest":installed["digest"],
        "expected_generation":installed["catalog_generation"],
        "expected_new_digest":check["digest"],"request_id":"isolated-social-content-v05"});
    db.execute(
        "UPDATE app_runs SET state='awaiting_approval' WHERE id=?",
        [&runs[0]],
    )
    .unwrap();
    let blocked = daemon
        .operator_rpc("app_workspace_upgrade", apply.clone())
        .unwrap_err();
    assert!(blocked.to_string().contains("nonterminal run"), "{blocked}");
    assert!(!pm_dir.join(".apps/upgrade-pending.yaml").exists());
    db.execute("UPDATE app_runs SET state='failed' WHERE id=?", [&runs[0]])
        .unwrap();
    let upgraded = daemon.operator_rpc("app_workspace_upgrade", apply).unwrap();
    assert_eq!(upgraded["install_id"], id);
    assert_eq!(upgraded["version"], "0.5.0");
    assert_eq!(upgraded["approved"], false);
    assert_eq!(std::fs::read(&old_bundle).unwrap(), old_bytes);
    assert_eq!(
        daemon
            .operator_rpc("app_context_list", json!({"install_id":id}))
            .unwrap(),
        contexts_before
    );
    assert_eq!(
        daemon
            .operator_rpc("app_binding_list", json!({"install_id":id}))
            .unwrap(),
        bindings_before
    );
    for (run, old) in runs.iter().zip(old_runs.iter()) {
        assert_eq!(
            &daemon
                .operator_rpc("app_run_show", json!({"run_id":run}))
                .unwrap(),
            old
        );
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
    let lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "catalog-http", "claude", None, lane.pid());
    let lease = test_port();
    let port = lease.port;
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
}

#[test]
fn cad667_actual_enrolled_http_peers_and_detached_children_cannot_install_or_read() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};
    let w = Workspace::new();
    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "catalog-http", "claude", None, lane.pid());
    let lease = test_port();
    let port = lease.port;
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
    let row = w.install().unwrap();
    let id = row["install_id"].as_str().unwrap();
    let body = json!({"source":w.source()}).to_string();
    let installed_head = w.head();
    for prefix in ["", "setsid "] {
        for (method, path, body) in [
            ("POST", "/api/app-installations".to_string(), body.clone()),
            ("GET", "/api/app-installations".to_string(), String::new()),
            ("GET", format!("/api/app-installations/{id}"), String::new()),
        ] {
            let stolen = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &w.daemon.state, port);
            let request = lane.dir.path().join(format!("http-{}.txt", lane.seq));
            let wire = stolen.request_as(method, &path, &body, "");
            assert!(!wire.contains(cadence_agent::test_seam::AS_HEADER));
            assert!(!wire.contains(cadence_agent::test_seam::TOKEN_HEADER));
            assert!(wire.contains(&stolen.cookie));
            assert!(wire.contains(&stolen.key));
            std::fs::write(&request, wire).unwrap();
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
    assert!(legacy["apps"].as_array().is_some());
    assert_eq!(legacy["count"], 0);
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
    assert!(
        failed
            .to_string()
            .contains(&format!("cadence app catalog recover {id}")),
        "failed install did not identify its explicit recovery: {failed}"
    );
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

#[test]
fn cad667_workspace_source_credentials_refuse_before_clone_or_persistence() {
    let w = Workspace::new();
    let head = w.head();
    let marker = ["cad667", "test", "fixture"].join("-");
    for source in [
        format!("https://user:{marker}@example.invalid/app.git"),
        format!("ssh://git:{marker}@example.invalid/app.git"),
        format!("https://example.invalid/app.git?token={marker}"),
        format!("https://example.invalid/app.git#{marker}"),
    ] {
        let error = w
            .daemon
            .operator_rpc("app_workspace_install", json!({"source":source}))
            .unwrap_err()
            .to_string();
        assert!(
            !error.contains(&marker),
            "credential leaked in source refusal"
        );
        assert!(
            error.contains("credential"),
            "wrong source boundary: {error}"
        );
        assert_eq!(w.head(), head);
        assert!(!w.pm.dir.join(".apps").exists());
    }
}

#[test]
fn cad667_explicit_migration_recovery_has_operator_proof_and_preserves_legacy_identity() {
    let w = Workspace::new();
    let base = w.pm.dir.join("client/apps/legacy");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(
        w.pm.dir.join("client/project.yaml"),
        "key: client\nprefix: C\n",
    )
    .unwrap();
    std::fs::write(
        base.join("app.md"),
        "---\napp: legacy\ntitle: Legacy\nversion: '1'\n---\nGuide\n",
    )
    .unwrap();
    let record = w.pm.dir.join("client/apps/legacy.yaml");
    std::fs::write(&record,"schema: 1\napp: legacy\ninstall_id: stable-legacy\nsource:\n  kind: path\n  path: /legacy\ninstalled_at: yesterday\ninstalled_by: operator\n").unwrap();
    let before = std::fs::read(&record).unwrap();
    let migrated = w
        .daemon
        .operator_rpc("app_workspace_migrate", json!({}))
        .expect("explicit delivered migration");
    assert_eq!(migrated["committed"], true);
    let catalog: serde_yaml::Value =
        serde_yaml::from_slice(&std::fs::read(w.pm.dir.join(".apps/catalog.yaml")).unwrap())
            .unwrap();
    let journal = catalog["last_migration"].as_str().unwrap();
    std::fs::write(
        w.pm.dir.join(".apps/pending.yaml"),
        format!("schema: 1\njournal: {journal}\n"),
    )
    .unwrap();
    let head = w.head();
    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "migration-worker", "claude", None, lane.pid());
    let denied = lane.rpc(
        &w.daemon.state,
        "app_workspace_migration_recover",
        json!({"journal_id":journal,"rollback":false}),
    );
    assert_eq!(denied["ok"], false);
    assert!(
        denied.to_string().contains("operator"),
        "wrong migration caller boundary: {denied}"
    );
    assert_eq!(w.head(), head);
    let recovered = w
        .daemon
        .operator_rpc(
            "app_workspace_migration_recover",
            json!({"journal_id":journal,"rollback":false}),
        )
        .expect("public migration recovery must exist");
    assert_eq!(recovered["committed"], true);
    assert!(!w.pm.dir.join(".apps/pending.yaml").exists());
    assert_eq!(std::fs::read(record).unwrap(), before);
    assert_eq!(
        w.daemon
            .operator_rpc("app_workspace_show", json!({"install_id":"stable-legacy"}))
            .unwrap()["install_id"],
        "stable-legacy"
    );
}

#[test]
fn cad667_installed_manifest_cannot_relabel_a_stable_installation() {
    let w = Workspace::new();
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap();
    let manifest =
        w.pm.dir
            .join(".apps/installations")
            .join(id)
            .join("bundle/app.md");
    let changed = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("app: blog-post", "app: substituted-app");
    std::fs::write(manifest, changed).unwrap();
    assert!(
        w.daemon
            .operator_rpc("app_workspace_show", json!({"install_id":id}))
            .is_err(),
        "manifest name substituted the catalog identity"
    );
    assert!(w
        .daemon
        .operator_rpc("app_workspace_list", json!({}))
        .is_err());
}

#[test]
fn cad667_flat_text_rubrics_and_templates_keep_legacy_bundle_compatibility() {
    let w = Workspace::new();
    std::fs::write(w.source().join("templates/outline.txt"), "An outline").unwrap();
    std::fs::write(
        w.source().join("rubrics/check.html"),
        "<p>Review carefully</p>",
    )
    .unwrap();
    let installed = w
        .install()
        .expect("existing flat text bundle grammar must remain accepted");
    let id = installed["install_id"].as_str().unwrap();
    let shown = w
        .daemon
        .operator_rpc("app_workspace_show", json!({"install_id":id}))
        .unwrap();
    assert!(shown["files"]
        .as_array()
        .unwrap()
        .contains(&json!("templates/outline.txt")));
    let legacy = w.pm.dir.join("client/apps/legacy");
    std::fs::create_dir_all(legacy.join("templates")).unwrap();
    std::fs::write(
        w.pm.dir.join("client/project.yaml"),
        "key: client\nprefix: C\n",
    )
    .unwrap();
    std::fs::write(
        legacy.join("app.md"),
        "---\napp: legacy\ntitle: Legacy\nversion: '1'\n---\nGuide\n",
    )
    .unwrap();
    std::fs::write(
        legacy.join("templates/outline.html"),
        "<p>Legacy outline</p>",
    )
    .unwrap();
    std::fs::write(w.pm.dir.join("client/apps/legacy.yaml"),"schema: 1\napp: legacy\ninstall_id: legacy-text\nsource:\n  kind: path\n  path: /legacy\ninstalled_at: yesterday\ninstalled_by: operator\n").unwrap();
    w.daemon
        .operator_rpc("app_workspace_migrate", json!({}))
        .unwrap();
    let shown = w
        .daemon
        .operator_rpc("app_workspace_show", json!({"install_id":"legacy-text"}))
        .unwrap();
    assert!(shown["files"]
        .as_array()
        .unwrap()
        .contains(&json!("templates/outline.html")));
    assert_eq!(
        w.daemon
            .operator_rpc("app_workspace_list", json!({}))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn cad667_workspace_snapshot_preserves_existing_aggregate_bundle_limit() {
    let w = Workspace::new();
    for index in 0..9 {
        std::fs::write(
            w.source()
                .join("templates")
                .join(format!("large-{index}.md")),
            "x".repeat(256 * 1024),
        )
        .unwrap();
    }
    let before = w.head();
    let refused = w
        .install()
        .expect_err("individually bounded files must not widen the A1 aggregate limit");
    assert!(refused.to_string().contains("limit") || refused.to_string().contains("large"));
    assert_eq!(w.head(), before);
    assert!(!w.pm.dir.join(".apps").exists());
}

#[test]
fn cad667_migration_delivery_failure_keeps_reads_and_installs_closed_until_explicit_retry() {
    use std::os::unix::fs::PermissionsExt;
    for rollback in [false, true] {
        let w = Workspace::new();
        let base = w.pm.dir.join("client/apps/legacy");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(
            w.pm.dir.join("client/project.yaml"),
            "key: client\nprefix: C\n",
        )
        .unwrap();
        std::fs::write(
            base.join("app.md"),
            "---\napp: legacy\ntitle: Legacy\nversion: '1'\n---\nGuide\n",
        )
        .unwrap();
        let record = w.pm.dir.join("client/apps/legacy.yaml");
        std::fs::write(&record,"schema: 1\napp: legacy\ninstall_id: stable-migration\nsource:\n  kind: path\n  path: /legacy\ninstalled_at: yesterday\ninstalled_by: operator\n").unwrap();
        let original_record = std::fs::read(&record).unwrap();
        let hook = w.pm.dir.join(".git/hooks/pre-commit");
        let original_hook = std::fs::read(&hook).ok();
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
        let head = w.head();
        let failed = w
            .daemon
            .operator_rpc("app_workspace_migrate", json!({}))
            .unwrap_err();
        assert_eq!(w.head(), head);
        let pending = w.pm.dir.join(".apps/pending.yaml");
        assert!(
            pending.exists(),
            "migration publication was exposed before failed Git delivery could be retried"
        );
        let pending: serde_yaml::Value =
            serde_yaml::from_slice(&std::fs::read(&pending).unwrap()).unwrap();
        let id = pending["journal"].as_str().unwrap();
        assert!(
            failed
                .to_string()
                .contains(&format!("cadence app catalog migration-recover {id}")),
            "failed migration did not identify its explicit recovery: {failed}"
        );
        assert!(w
            .daemon
            .operator_rpc("app_workspace_list", json!({}))
            .is_err());
        assert!(w
            .daemon
            .operator_rpc(
                "app_workspace_show",
                json!({"install_id":"stable-migration"})
            )
            .is_err());
        assert!(
            w.install().is_err(),
            "new install swallowed an undelivered migration"
        );
        assert!(w
            .daemon
            .operator_rpc(
                "app_workspace_migration_recover",
                json!({"journal_id":id,"rollback":rollback})
            )
            .is_err());
        assert!(
            w.pm.dir.join(".apps/pending.yaml").exists(),
            "failed recovery Git delivery removed its refusal gate"
        );
        match original_hook {
            Some(bytes) => std::fs::write(&hook, bytes).unwrap(),
            None => std::fs::remove_file(&hook).unwrap(),
        }
        let delivered = w
            .daemon
            .operator_rpc(
                "app_workspace_migration_recover",
                json!({"journal_id":id,"rollback":rollback}),
            )
            .unwrap();
        assert_eq!(delivered["committed"], true);
        assert!(!w.pm.dir.join(".apps/pending.yaml").exists());
        assert_eq!(std::fs::read(record).unwrap(), original_record);
        if rollback {
            assert!(!w.pm.dir.join(".apps/catalog.yaml").exists());
        } else {
            assert_eq!(
                w.daemon
                    .operator_rpc(
                        "app_workspace_show",
                        json!({"install_id":"stable-migration"})
                    )
                    .unwrap()["install_id"],
                "stable-migration"
            );
            let before_rollback = w.head();
            let result = w
                .daemon
                .operator_rpc(
                    "app_workspace_migration_recover",
                    json!({"journal_id":id,"rollback":true}),
                )
                .unwrap();
            assert_eq!(result["committed"], true);
            assert_ne!(w.head(), before_rollback);
            assert!(!w.pm.dir.join(".apps/catalog.yaml").exists());
            let tracked = std::process::Command::new("git")
                .arg("-C")
                .arg(&w.pm.dir)
                .args(["ls-files", "--", ".apps/catalog.yaml"])
                .output()
                .unwrap();
            assert!(tracked.status.success());
            assert!(
                tracked.stdout.is_empty(),
                "rollback omitted the tracked catalog deletion"
            );
        }
    }
}

/* ------------------------------------------------------------------ */
/* CAD-864: the installable app-views/v1 descriptor seam.              */
/*                                                                     */
/* Adversarial-first: these tests name the guard before the new        */
/* validation exists. A descriptor-bearing bundle installs only when   */
/* `views/app-views-v1.json` parses under the contract's exact rules   */
/* AND `needs.views.contract` declares it; malformed, undeclared,      */
/* over-bound, forbidden-key, traversal-named or swapped descriptors   */
/* refuse before any journal/pending write or catalog mutation. The    */
/* descriptor rides the verified `describe` receipt — daemon           */
/* `app_workspace_show` and the HTTP GET peer — never a                */
/* descriptor-supplied scope.                                          */
/* ------------------------------------------------------------------ */

/// A descriptor-bearing bundle installs and the verified receipt serves
/// the validated descriptor plus its content digest; a legacy bundle
/// without one keeps `view_descriptor: null`.
#[test]
fn cad864_descriptor_installs_and_rides_the_verified_receipt() {
    let w = Workspace::new();
    // Legacy bundle: no views/ dir, no declaration — unchanged contract.
    let plain = w.install().unwrap();
    assert_eq!(plain["view_descriptor"], Value::Null);
    assert_eq!(plain["view_descriptor_digest"], Value::Null);
    let id = plain["install_id"].as_str().unwrap().to_string();
    assert_eq!(w.show(&id)["view_descriptor"], Value::Null);

    // Fresh workspace so the descriptor-bearing bundle is not "already
    // installed".
    let w2 = Workspace::new();
    w2.write_descriptor(&Workspace::descriptor_text(), true);
    let installed = w2.install().unwrap();
    let id2 = installed["install_id"].as_str().unwrap().to_string();
    let shown = w2.show(&id2);
    let descriptor = &shown["view_descriptor"];
    assert_eq!(descriptor["contract"], "app-views/v1");
    assert_eq!(descriptor["app"], "blog-post");
    assert_eq!(
        descriptor["views"].as_array().unwrap().len(),
        3,
        "descriptor serves its declared views through the receipt"
    );
    // The receipt's descriptor digest matches the bytes in the
    // installation's bundle digest (content-covered, so a descriptor
    // byte change is a structural change).
    assert!(shown["view_descriptor_digest"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(
        shown["digest"], installed["digest"],
        "receipt digest is the installed bundle digest"
    );
    // Descriptor bytes are part of the installed file inventory.
    assert!(shown["files"]
        .as_array()
        .unwrap()
        .contains(&json!("views/app-views-v1.json")));
}

/// Every malformed/undeclared/forbidden descriptor refuses install
/// before publication — no `.apps/` tree is created and git HEAD does
/// not move.
#[test]
fn cad864_malformed_and_forbidden_descriptors_refuse_before_install() {
    let good = Workspace::descriptor_text();
    let nested_forbidden = r#"{"contract":"app-views/v1","app":"blog-post","title":"T","views":[{"id":"v","title":"t","kind":"detail","fields":[{"id":"f","label":"l","url":"https://evil.test"}]}]}"#
        .to_string();
    // Column naming an undeclared field.
    let bad_column = r#"{"contract":"app-views/v1","app":"blog-post","title":"T","views":[{"id":"v","title":"t","kind":"table","fields":[{"id":"f","label":"l"}],"columns":[{"field":"ghost"}]}]}"#
        .to_string();
    // createView naming no declared form view.
    let bad_create_view = r#"{"contract":"app-views/v1","app":"blog-post","title":"T","views":[{"id":"v","title":"t","kind":"detail","fields":[{"id":"f","label":"l","createView":"nope"}]}]}"#
        .to_string();
    // Each case: (descriptor bytes, declare flag) that must refuse.
    let cases: Vec<(String, bool)> = vec![
        // Undeclared file — present on disk, never declared in app.md.
        (good.clone(), false),
        // Declaration without the file is covered in the next test; here
        // the file exists but is malformed.
        ("not json".to_string(), true),
        (good.replace("\"app-views/v1\"", "\"app-views/v2\""), true),
        // Forbidden keys at the root and nested inside a field.
        (
            good.replace("\"summary\":", "\"install_id\": \"forged\", \"summary\":"),
            true,
        ),
        (nested_forbidden, true),
        (bad_column, true),
        (bad_create_view, true),
        // `app` provenance must match the manifest.
        (
            good.replace("\"app\": \"blog-post\"", "\"app\": \"other-app\""),
            true,
        ),
    ];

    for (i, (descriptor, declare)) in cases.into_iter().enumerate() {
        let w = Workspace::new();
        w.write_descriptor(&descriptor, declare);
        let head = w.head();
        let err = w
            .install()
            .expect_err(&format!("case {i} must refuse install"));
        let _ = err; // refusal itself is the assertion — no partial state
        assert_eq!(w.head(), head, "case {i} moved HEAD on refusal");
        assert!(
            !w.pm.dir.join(".apps").exists(),
            "case {i} published catalog state on refusal"
        );
    }
}

/// `needs.views` declared without `views/app-views-v1.json` refuses;
/// a `views/` file under any other name refuses; a symlinked descriptor
/// refuses.
#[test]
fn cad864_declaration_file_pairing_and_layout_are_exact() {
    // Declaration without the file.
    let w = Workspace::new();
    let manifest = w.source().join("app.md");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        text.replace(
            "  connections: [publish]",
            "  connections: [publish]\n  views:\n    contract: app-views/v1",
        ),
    )
    .unwrap();
    let head = w.head();
    assert!(
        w.install().is_err(),
        "declared views with no descriptor file installed"
    );
    assert_eq!(w.head(), head);
    assert!(!w.pm.dir.join(".apps").exists());

    // Wrong filename under views/.
    for leaf in ["other.json", "app-views-v2.json", "APP-VIEWS-V1.JSON"] {
        let w = Workspace::new();
        let views = w.source().join("views");
        std::fs::create_dir_all(&views).unwrap();
        std::fs::write(views.join(leaf), Workspace::descriptor_text()).unwrap();
        // Declaration present but the file name is wrong — the contract
        // pins exactly `views/app-views-v1.json`.
        let manifest = w.source().join("app.md");
        let text = std::fs::read_to_string(&manifest).unwrap();
        std::fs::write(
            &manifest,
            text.replace(
                "  connections: [publish]",
                "  connections: [publish]\n  views:\n    contract: app-views/v1",
            ),
        )
        .unwrap();
        let head = w.head();
        assert!(
            w.install().is_err(),
            "views/{leaf} installed under the v1 declaration"
        );
        assert_eq!(w.head(), head);
        assert!(!w.pm.dir.join(".apps").exists());
    }

    // A nested dir under views/ refuses (flat dirs only).
    let w = Workspace::new();
    std::fs::create_dir_all(w.source().join("views/nested")).unwrap();
    std::fs::write(
        w.source().join("views/app-views-v1.json"),
        Workspace::descriptor_text(),
    )
    .unwrap();
    let manifest = w.source().join("app.md");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        text.replace(
            "  connections: [publish]",
            "  connections: [publish]\n  views:\n    contract: app-views/v1",
        ),
    )
    .unwrap();
    assert!(w.install().is_err(), "nested views/ dir installed");

    // A symlinked descriptor refuses.
    #[cfg(unix)]
    {
        let w = Workspace::new();
        let views = w.source().join("views");
        std::fs::create_dir_all(&views).unwrap();
        let outside = w._root.path().join("outside.json");
        std::fs::write(&outside, Workspace::descriptor_text()).unwrap();
        std::os::unix::fs::symlink(&outside, views.join("app-views-v1.json")).unwrap();
        let manifest = w.source().join("app.md");
        let text = std::fs::read_to_string(&manifest).unwrap();
        std::fs::write(
            &manifest,
            text.replace(
                "  connections: [publish]",
                "  connections: [publish]\n  views:\n    contract: app-views/v1",
            ),
        )
        .unwrap();
        assert!(w.install().is_err(), "symlinked descriptor installed");
        assert!(!w.pm.dir.join(".apps").exists());
    }
}

/// A descriptor byte change is a structural change: it moves the bundle
/// digest, an upgrade must pin it, and the upgraded receipt serves the
/// new descriptor. The old revision keeps its own descriptor bytes.
#[test]
fn cad864_descriptor_bytes_are_identity_and_upgrade_pinned() {
    let w = Workspace::new();
    w.write_descriptor(&Workspace::descriptor_text(), true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let old_digest = installed["digest"].as_str().unwrap().to_string();
    let generation = installed["catalog_generation"]
        .as_str()
        .unwrap()
        .to_string();

    // Change one label inside the descriptor (still valid) and one
    // version byte in the manifest.
    let descriptor = w.source().join("views/app-views-v1.json");
    let text = std::fs::read_to_string(&descriptor).unwrap();
    std::fs::write(&descriptor, text.replace("\"Customers\"", "\"Clients\"")).unwrap();
    let manifest = w.source().join("app.md");
    let mtext = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, mtext.replace("version: 0.1.0", "version: 0.2.0")).unwrap();

    let proposed = w.upgrade_check(&installed);
    let new_digest = proposed["digest"].as_str().unwrap().to_string();
    assert_ne!(
        new_digest, old_digest,
        "descriptor edit did not move the digest"
    );
    assert!(proposed["structural_diff"]["changed"]
        .as_array()
        .unwrap()
        .contains(&json!("views/app-views-v1.json")));

    let upgraded = w
        .daemon
        .operator_rpc(
            "app_workspace_upgrade",
            json!({"install_id": id, "source": w.source(),
                "expected_digest": old_digest, "expected_generation": generation,
                "expected_new_digest": new_digest, "request_id": "desc-upgrade"}),
        )
        .unwrap();
    assert_eq!(upgraded["digest"], json!(new_digest));
    assert_eq!(upgraded["approved"], json!(false));
    let shown = w.show(&id);
    assert_eq!(
        shown["view_descriptor"]["views"][0]["title"],
        json!("Clients")
    );
    // The descriptor's own digest moved with the bytes.
    assert_ne!(
        shown["view_descriptor_digest"],
        json!(installed["view_descriptor_digest"])
    );

    // A stale upgrade (pre-change digest) refuses without mutation.
    let stale = w.daemon.operator_rpc(
        "app_workspace_upgrade",
        json!({"install_id": id, "source": w.source(),
            "expected_digest": old_digest, "expected_generation": generation,
            "expected_new_digest": new_digest, "request_id": "desc-stale"}),
    );
    assert!(
        stale.is_err(),
        "stale descriptor-bearing digest upgraded again"
    );
    assert_eq!(w.show(&id)["digest"], json!(new_digest));
}

/// A descriptor-bearing install refuses midway when the descriptor is
/// invalid, and the retained-journal recovery path also re-validates —
/// a forged journal carrying a bad descriptor cannot apply.
#[test]
fn cad864_recovery_revalidates_a_forged_descriptor_journal() {
    use std::os::unix::fs::PermissionsExt;
    let w = Workspace::new();
    w.write_descriptor(&Workspace::descriptor_text(), true);
    let hook = w.pm.dir.join(".git/hooks/pre-commit");
    let original_hook = std::fs::read(&hook).ok();
    std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    let failed = w.install().unwrap_err();
    assert!(failed.to_string().contains("git"));
    let id = std::fs::read_to_string(w.pm.dir.join(".apps/install-pending.yaml")).unwrap();
    let journal_path =
        w.pm.dir
            .join(".apps/install-journals")
            .join(format!("{}.yaml", id.trim()));
    let journal_bytes = std::fs::read(&journal_path).unwrap();
    // Forge the journal's descriptor entry to invalid bytes — the
    // journal still parses as YAML but the bundle it carries is bad.
    let mut forged: serde_yaml::Value = serde_yaml::from_slice(&journal_bytes).unwrap();
    forged["files"].as_mapping_mut().unwrap().insert(
        serde_yaml::Value::String("views/app-views-v1.json".into()),
        serde_yaml::Value::String("{\"contract\":\"app-views/v2\"}".into()),
    );
    std::fs::write(&journal_path, serde_yaml::to_string(&forged).unwrap()).unwrap();
    match original_hook {
        Some(bytes) => std::fs::write(&hook, bytes).unwrap(),
        None => std::fs::remove_file(&hook).unwrap(),
    }
    assert!(
        w.daemon
            .operator_rpc("app_workspace_recover", json!({"install_id": id.trim()}))
            .is_err(),
        "a journal carrying an invalid descriptor applied"
    );
    // Restore and recover cleanly.
    std::fs::write(&journal_path, &journal_bytes).unwrap();
    let recovered = w
        .daemon
        .operator_rpc("app_workspace_recover", json!({"install_id": id.trim()}))
        .unwrap();
    assert_eq!(recovered["committed"], true);
    assert_eq!(
        recovered["view_descriptor"]["contract"],
        json!("app-views/v1")
    );
}

/// The receipt is served only over the operator gate — an agent caller
/// (daemon RPC) and an HTTP peer carrying a stolen session both refuse;
/// the descriptor never names its own scope.
#[test]
fn cad864_descriptor_read_stays_operator_only_and_installation_bound() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};
    let w = Workspace::new();
    w.write_descriptor(&Workspace::descriptor_text(), true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();

    // Daemon RPC: an agent caller cannot read the receipt.
    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "desc-reader", "claude", None, lane.pid());
    for forged in [
        json!({"install_id": id}),
        json!({"install_id": id, "actor": "operator"}),
        json!({"install_id": "../other"}),
    ] {
        let frame = lane.rpc(&w.daemon.state, "app_workspace_show", forged);
        assert_eq!(frame["ok"], false, "agent read descriptor receipt: {frame}");
    }

    // HTTP peer: the descriptor rides GET /api/app-installations/<id>
    // under the same operator read gate — a stolen session from an
    // enrolled agent's pane is refused.
    let lease = test_port();
    let port = lease.port;
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
    // Operator GET serves the descriptor.
    let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &w.daemon.state, port);
    let path = format!("/api/app-installations/{id}");
    let (code, _, body) = common::op::raw(port, &session.request("GET", &path, ""));
    assert_eq!(code, 200, "operator descriptor read: {body}");
    let row: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(row["view_descriptor"]["app"], json!("blog-post"));

    // Agent-peered HTTP read refuses (stolen session replayed through
    // the agent's pane — the existing operator-read gate's proof).
    for prefix in ["", "setsid "] {
        let stolen = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &w.daemon.state, port);
        let wire = stolen.request_as("GET", &path, "", "");
        let request_file = lane.dir.path().join(format!("desc-http-{}.txt", lane.seq));
        std::fs::write(&request_file, wire).unwrap();
        let (rc, response) = lane.run(&format!("{prefix}python3 -c 'import socket,sys; s=socket.create_connection((\"127.0.0.1\",int(sys.argv[1])));s.sendall(open(sys.argv[2],\"rb\").read());print(s.makefile().readline())' {port} {}", request_file.display()));
        assert_eq!(rc, 0);
        assert_eq!(
            response.split_whitespace().nth(1),
            Some("403"),
            "agent-peered HTTP descriptor read reached {path}: {response}"
        );
    }
    // A cross-install/guessed read — an id that is not a live
    // installation — refuses on both peers.
    let fake = "0123456789abcdef0123456789abcdef";
    assert!(w
        .daemon
        .operator_rpc("app_workspace_show", json!({"install_id": fake}))
        .is_err());
    let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &w.daemon.state, port);
    let (code, _, _) = common::op::raw(
        port,
        &session.request("GET", &format!("/api/app-installations/{fake}"), ""),
    );
    assert!(code >= 400, "unknown installation read served: {code}");
}

/* ------------------------------------------------------------------ */
/* CAD-867: the app-bindings/v1 companion contract seam.               */
/*                                                                     */
/* A bundle carrying `needs.bindings.contract` +                       */
/* `bindings/app-bindings-v1.json` installs only when the file parses, */
/* its `app` matches the manifest's, every bound view/field is one the */
/* SAME bundle's descriptor declares, and every mapping is honest      */
/* about the produced shape. Undeclared/orphaned bindings, forged      */
/* authority keys, tampered installed bytes and descriptor-less        */
/* bindings all refuse before any journal/pending write. The verified  */
/* receipt serves `view_binding` + `view_binding_digest` from the same */
/* snapshot as the descriptor — never a stale pair.                    */
/* ------------------------------------------------------------------ */

/// A descriptor+binding bundle installs and the receipt serves both
/// validated documents plus their digests — descriptor and binding
/// come from the same verified snapshot, bound by `app` and view ids.
#[test]
fn cad867_descriptor_and_binding_install_and_ride_the_verified_receipt() {
    let w = Workspace::new();
    w.write_descriptor(&Workspace::descriptor_text(), true);
    w.write_binding(&Workspace::binding_text(), true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let shown = w.show(&id);
    // Both documents ride the receipt together.
    let descriptor = &shown["view_descriptor"];
    let binding = &shown["view_binding"];
    assert_eq!(descriptor["contract"], "app-views/v1");
    assert_eq!(binding["contract"], "app-bindings/v1");
    assert_eq!(binding["app"], "blog-post");
    assert!(binding["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["view"] == "customers" && b["source"] == "customers"));
    for key in ["view_descriptor_digest", "view_binding_digest"] {
        assert!(shown[key].as_str().unwrap().starts_with("sha256:"));
    }
    assert_eq!(shown["digest"], installed["digest"]);
    assert!(shown["files"]
        .as_array()
        .unwrap()
        .contains(&json!("bindings/app-bindings-v1.json")));
}

/// A descriptor-only `app-views/v1` bundle still installs byte-identical
/// — the companion is optional, never required, and the receipt's
/// binding fields stay null.
#[test]
fn cad867_descriptor_only_package_still_installs_unchanged() {
    let w = Workspace::new();
    w.write_descriptor(&Workspace::descriptor_text(), true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let shown = w.show(&id);
    assert_eq!(shown["view_descriptor"]["contract"], "app-views/v1");
    assert_eq!(shown["view_binding"], Value::Null);
    assert_eq!(shown["view_binding_digest"], Value::Null);
}

/// The binding file and its declaration pair up exactly like the
/// descriptor's: either alone refuses before any catalog write.
#[test]
fn cad867_binding_declaration_file_pairing_is_exact() {
    // Declared without the file.
    let w = Workspace::new();
    w.write_descriptor(&Workspace::descriptor_text(), true);
    let manifest = w.source().join("app.md");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        text.replace(
            "  views:\n    contract: app-views/v1",
            "  views:\n    contract: app-views/v1\n  bindings:\n    contract: app-bindings/v1",
        ),
    )
    .unwrap();
    let head = w.head();
    assert!(
        w.install().is_err(),
        "declared bindings with no binding file installed"
    );
    assert_eq!(w.head(), head);
    assert!(!w.pm.dir.join(".apps").exists());

    // File present but undeclared.
    let w = Workspace::new();
    w.write_descriptor(&Workspace::descriptor_text(), true);
    w.write_binding(&Workspace::binding_text(), false);
    let head = w.head();
    assert!(w.install().is_err(), "undeclared binding file installed");
    assert_eq!(w.head(), head);
    assert!(!w.pm.dir.join(".apps").exists());
}

/// A binding requires its descriptor: `needs.bindings` without
/// `needs.views` refuses — the companion can never stand alone.
#[test]
fn cad867_binding_requires_its_descriptor() {
    let w = Workspace::new();
    w.write_binding(&Workspace::binding_text(), true);
    let m = w.source().join("app.md");
    let mtext = std::fs::read_to_string(&m).unwrap();
    assert!(mtext.contains("bindings:"), "write_binding must declare");
    assert!(
        !mtext.contains("views:"),
        "this bundle must not declare views"
    );
    let head = w.head();
    assert!(
        w.install().is_err(),
        "needs.bindings without needs.views installed"
    );
    assert_eq!(w.head(), head);
    assert!(!w.pm.dir.join(".apps").exists());
}

/// Malformed, forbidden-key, undeclared-view/field and forged
/// cross-reference bindings all refuse install before publication.
#[test]
fn cad867_malformed_and_forged_bindings_refuse_before_install() {
    let good = Workspace::binding_text();
    let cases: Vec<String> = vec![
        "not json".to_string(),
        good.replace("\"app-bindings/v1\"", "\"app-bindings/v2\""),
        good.replace("\"app\": \"blog-post\"", "\"app\": \"other\""),
        // Forbidden authority/invocation keys at root and nested.
        good.replace("\"title\":", "\"install_id\": \"forged\", \"title\":"),
        good.replace(
            "\"view\": \"customers\"",
            "\"view\": \"customers\", \"method\": \"app_record_delete\"",
        ),
        good.replace(
            "\"field\": \"name\", \"key\": \"display_name\"",
            "\"field\": \"name\", \"key\": \"display_name\", \"token\": \"x\"",
        ),
        // Undeclared view id the descriptor never declared.
        good.replace("\"view\": \"customers\"", "\"view\": \"ghosts\""),
        // A bound form view — disabled previews can never be bound.
        good.replace(
            "\"view\": \"customer-detail\"",
            "\"view\": \"customer-form\"",
        ),
        // A field the view does not declare.
        good.replace("\"field\": \"tags\"", "\"field\": \"ghost\""),
        // A key outside the source's reviewed projection.
        good.replace("\"key\": \"email\"", "\"key\": \"password\""),
        // Format lying about the descriptor's declared format.
        good.replace(
            "\"field\": \"tags\", \"key\": \"tags\", \"format\": \"tags\"",
            "\"field\": \"tags\", \"key\": \"tags\", \"format\": \"number\"",
        ),
        // Unknown source name.
        good.replace("\"source\": \"customers\"", "\"source\": \"orders\""),
        // An op the view kind cannot use (detail cannot list).
        good.replace(
            "\"view\": \"customer-detail\",\n      \"source\": \"customers\",\n      \"ops\": [\"show\"]",
            "\"view\": \"customer-detail\",\n      \"source\": \"customers\",\n      \"ops\": [\"list\"]",
        ),
    ];
    for (i, binding) in cases.into_iter().enumerate() {
        let w = Workspace::new();
        w.write_descriptor(&Workspace::descriptor_text(), true);
        w.write_binding(&binding, true);
        let head = w.head();
        let err = w.install().expect_err(&format!("case {i} must refuse"));
        let _ = err;
        assert_eq!(w.head(), head, "case {i} moved HEAD on refusal");
        assert!(
            !w.pm.dir.join(".apps").exists(),
            "case {i} published catalog state on refusal"
        );
    }
}

/// Binding bytes are identity: a byte change moves the bundle digest,
/// the upgraded receipt serves the new pair from one snapshot, and a
/// stale pre-change digest can never upgrade again.
#[test]
fn cad867_binding_bytes_are_identity_and_upgrade_pinned() {
    let w = Workspace::new();
    w.write_descriptor(&Workspace::descriptor_text(), true);
    w.write_binding(&Workspace::binding_text(), true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let old_digest = installed["digest"].as_str().unwrap().to_string();
    let generation = installed["catalog_generation"]
        .as_str()
        .unwrap()
        .to_string();
    // Change one label inside the binding (still valid) + version bump.
    let file = w.source().join("bindings/app-bindings-v1.json");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(
        &file,
        text.replace("\"CRM bindings — customers\"", "\"CRM bindings v2\""),
    )
    .unwrap();
    let manifest = w.source().join("app.md");
    let mtext = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, mtext.replace("version: 0.1.0", "version: 0.2.0")).unwrap();
    let proposed = w.upgrade_check(&installed);
    let new_digest = proposed["digest"].as_str().unwrap().to_string();
    assert_ne!(new_digest, old_digest);
    assert!(proposed["structural_diff"]["changed"]
        .as_array()
        .unwrap()
        .contains(&json!("bindings/app-bindings-v1.json")));
    let upgraded = w
        .daemon
        .operator_rpc(
            "app_workspace_upgrade",
            json!({"install_id": id, "source": w.source(),
                "expected_digest": old_digest, "expected_generation": generation,
                "expected_new_digest": new_digest, "request_id": "bind-upgrade"}),
        )
        .unwrap();
    assert_eq!(upgraded["digest"], json!(new_digest));
    let shown = w.show(&id);
    assert_eq!(shown["view_binding"]["title"], json!("CRM bindings v2"));
    // Stale pre-change digest can never upgrade again.
    assert!(w
        .daemon
        .operator_rpc(
            "app_workspace_upgrade",
            json!({"install_id": id, "source": w.source(),
                "expected_digest": old_digest, "expected_generation": generation,
                "expected_new_digest": new_digest, "request_id": "bind-stale"}),
        )
        .is_err());
}

/// Tampered installed bytes refuse on read: a hand-edited installed
/// binding, a dropped declaration or a descriptor/binding that no
/// longer pair all fail `describe` rather than serve stale bytes.
#[test]
fn cad867_tampered_installed_binding_refuses_on_readback() {
    let w = Workspace::new();
    w.write_descriptor(&Workspace::descriptor_text(), true);
    w.write_binding(&Workspace::binding_text(), true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let bundle =
        w.pm.dir
            .join(".apps/installations")
            .join(&id)
            .join("bundle");
    // Tamper the installed binding's key — readback must refuse.
    let bfile = bundle.join("bindings/app-bindings-v1.json");
    let original = std::fs::read_to_string(&bfile).unwrap();
    std::fs::write(&bfile, original.replace("display_name", "password")).unwrap();
    assert!(
        w.daemon
            .operator_rpc("app_workspace_show", json!({"install_id": id}))
            .is_err(),
        "tampered installed binding served"
    );
    std::fs::write(&bfile, &original).unwrap();
    // Drop the manifest's bindings declaration — undeclared file refuses.
    let mfile = bundle.join("app.md");
    let mtext = std::fs::read_to_string(&mfile).unwrap();
    std::fs::write(
        &mfile,
        mtext.replace("  bindings:\n    contract: app-bindings/v1\n", ""),
    )
    .unwrap();
    assert!(
        w.daemon
            .operator_rpc("app_workspace_show", json!({"install_id": id}))
            .is_err(),
        "undeclared installed binding served"
    );
    std::fs::write(&mfile, &mtext).unwrap();
}

/// The binding receipt is served only over the operator gate — an agent
/// caller and an agent-peered HTTP read both refuse; the binding never
/// names its own scope, actor, or installation.
#[test]
fn cad867_binding_read_stays_operator_only_and_installation_bound() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};
    let w = Workspace::new();
    w.write_descriptor(&Workspace::descriptor_text(), true);
    w.write_binding(&Workspace::binding_text(), true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "bind-reader", "claude", None, lane.pid());
    for forged in [
        json!({"install_id": id}),
        json!({"install_id": id, "actor": "operator"}),
    ] {
        let frame = lane.rpc(&w.daemon.state, "app_workspace_show", forged);
        assert_eq!(frame["ok"], false, "agent read binding receipt: {frame}");
    }
    let lease = test_port();
    let port = lease.port;
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
    let path = format!("/api/app-installations/{id}");
    let (code, _, body) = common::op::raw(port, &session.request("GET", &path, ""));
    assert_eq!(code, 200, "operator binding read: {body}");
    let row: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(row["view_binding"]["app"], json!("blog-post"));
    for prefix in ["", "setsid "] {
        let stolen = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &w.daemon.state, port);
        let wire = stolen.request_as("GET", &path, "", "");
        let request_file = lane.dir.path().join(format!("bind-http-{}.txt", lane.seq));
        std::fs::write(&request_file, wire).unwrap();
        let (rc, response) = lane.run(&format!("{prefix}python3 -c 'import socket,sys; s=socket.create_connection((\"127.0.0.1\",int(sys.argv[1])));s.sendall(open(sys.argv[2],\"rb\").read());print(s.makefile().readline())' {port} {}", request_file.display()));
        assert_eq!(rc, 0);
        assert_eq!(
            response.split_whitespace().nth(1),
            Some("403"),
            "agent-peered HTTP binding read reached {path}: {response}"
        );
    }
}

/* ------------------------------------------------------------------ */
/* CAD-867 live bound reads: app_view_read + its HTTP peer.            */
/*                                                                     */
/* The descriptor-driven surface reads live rows ONLY through the      */
/* bound read — never the raw per-source endpoints. The request names  */
/* the installation, descriptor view id, op and all three expected     */
/* digests; the host derives source + allowed op from the installed    */
/* binding and re-proves every digest against ONE verified snapshot    */
/* under the PM lock, then reads typed producer rows under the same    */
/* lock — an upgrade can never slip between authorization and read.    */
/*                                                                     */
/* These are tests-first: `app_view_read`/`/views/…` do not exist yet,  */
/* so the suite will not compile until the slice lands. Dynamic-RPC    */
/* tests can compile without the method; an "unknown method" refusal  */
/* is an absence, never the behavioral guard RED these target.        */
/* ------------------------------------------------------------------ */

/// Read the three digest pins off the verified receipt — the request
/// only ever asserts the install's current digest triple, never one it
/// invented. Returned as a ready-made param block.
fn view_read_digests(w: &Workspace, id: &str) -> Value {
    let shown = w.show(id);
    json!({
        "digest": shown["digest"],
        "view_descriptor_digest": shown["view_descriptor_digest"],
        "view_binding_digest": shown["view_binding_digest"],
    })
}

/// The shared RPC param block: install, descriptor view id, op and all
/// three digest pins — every caller then adds its own context/record.
fn view_read_params(install: &str, view: &str, op: &str, digests: &Value) -> Value {
    let mut p = digests.clone();
    p["install_id"] = json!(install);
    p["view_id"] = json!(view);
    p["op"] = json!(op);
    p
}

/// Assert the response echoes the very digest triple it was authorized
/// under — bundle + descriptor + binding, all three on every read.
fn assert_view_pins(response: &Value, digests: &Value) {
    assert_eq!(response["digest"], digests["digest"]);
    assert_eq!(
        response["view_descriptor_digest"], digests["view_descriptor_digest"],
        "response did not echo its descriptor pin"
    );
    assert_eq!(
        response["view_binding_digest"], digests["view_binding_digest"],
        "response did not echo its binding pin"
    );
}

/// A live context under `install` plus one fully-populated record —
/// created through the real operator write path so the projection test
/// reads genuine `CustomerProfile` bytes, not a stub. `record_id` is
/// caller-chosen so two installs can carry same-id or distinct ids.
fn seed_customer(
    w: &Workspace,
    install: &str,
    ctx_label: &str,
    ctx_request: &str,
    record_id: &str,
) -> (String, String) {
    let context = w
        .daemon
        .operator_rpc(
            "app_context_create",
            json!({"install_id":install,"label":ctx_label,"input_defaults":{},"request_id":ctx_request}),
        )
        .unwrap();
    let context_id = context["context"]["id"].as_str().unwrap().to_string();
    w.daemon
        .operator_rpc(
            "app_record_create",
            json!({"install_id":install,"context_id":context_id,"record_id":record_id,
                "profile":{"schema":1,"display_name":"Ada Lovelace","email":"ada@example.com",
                    "phone":"+1 555 0100","tags":["vip","beta"],"source":"import",
                    "consent":{"email":"granted","sms":"denied"}}}),
        )
        .unwrap();
    (context_id, record_id.to_string())
}

/// A descriptor+binding that binds the FULL customer projection —
/// name/email/phone/tags/source/consent — so the test can assert every
/// produced value and the consent enum domain, not just a subset.
fn full_customer_descriptor() -> String {
    json!({
        "contract":"app-views/v1","app":"blog-post","title":"views",
        "views":[
            {"id":"customers","title":"Customers","kind":"table",
             "fields":[
                {"id":"ref","label":"Ref","format":"text"},
                {"id":"name","label":"Name","format":"text"},
                {"id":"email","label":"Email","format":"text"},
                {"id":"phone","label":"Phone","format":"text"},
                {"id":"tags","label":"Tags","format":"tags","kind":"list"},
                {"id":"source","label":"Source","format":"text"},
                {"id":"consent_email","label":"Email consent","format":"enum","values":["granted","denied","unknown"]},
                {"id":"consent_sms","label":"SMS consent","format":"enum","values":["granted","denied","unknown"]},
                {"id":"visits","label":"Visits","format":"number"},
                {"id":"tier","label":"Tier","format":"enum","values":["member","vip","vip_plus"]}
             ],
             "columns":[{"field":"ref"},{"field":"name"},{"field":"email"}]},
            {"id":"customer-detail","title":"Customer","kind":"detail",
             "fields":[{"id":"ref","label":"Ref","format":"text"},
                       {"id":"name","label":"Name","format":"text"}]},
            {"id":"customer-form","title":"New customer","kind":"form",
             "previewOf":[{"id":"name","label":"Name","format":"text"}]}
        ]
    })
    .to_string()
}

fn full_customer_binding() -> String {
    json!({
        "contract":"app-bindings/v1","app":"blog-post","title":"b",
        "bindings":[
            {"view":"customers","source":"customers","ops":["list"],
             "fields":[
                {"field":"ref","key":"record_id","format":"text"},
                {"field":"name","key":"display_name","format":"text"},
                {"field":"email","key":"email","format":"text"},
                {"field":"phone","key":"phone","format":"text"},
                {"field":"tags","key":"tags","format":"tags"},
                {"field":"source","key":"source","format":"text"},
                {"field":"consent_email","key":"consent.email","format":"enum"},
                {"field":"consent_sms","key":"consent.sms","format":"enum"}
             ]},
            {"view":"customer-detail","source":"customers","ops":["show"],
             "fields":[{"field":"ref","key":"record_id","format":"text"},
                       {"field":"name","key":"display_name","format":"text"}]}
        ]
    })
    .to_string()
}

/// The binding variant whose `name` field maps a different produced
/// key (`email` instead of `display_name`): a read authorized under
/// the post-upgrade triple returns a different `name` cell, which is
/// what makes the concurrent-upgrade assertions distinguishable
/// (old "Ada Lovelace" vs new "ada@example.com"), never vacuous.
fn v2_customer_binding() -> String {
    // Structural edit, never a serialization-order-dependent string
    // replace: `json!` sorts object keys, so a `field,key,format`
    // literal replace is a silent no-op. Rewrite every `name` mapping's
    // `key` and assert the exact rename count (2 — `customers` and
    // `customer-detail` both bind `name`).
    let mut binding: Value = serde_json::from_str(&full_customer_binding()).unwrap();
    let mut renamed = 0usize;
    for b in binding["bindings"].as_array_mut().unwrap() {
        for f in b["fields"].as_array_mut().unwrap() {
            if f["field"] == "name" {
                assert_eq!(
                    f["key"],
                    json!("display_name"),
                    "v2 fixture expected `name` to map `display_name`"
                );
                f["key"] = json!("email");
                renamed += 1;
            }
        }
    }
    assert_eq!(renamed, 2, "v2 fixture must rename `name` in both bindings");
    binding.to_string()
}

/// A caption-runs descriptor/binding on `app` blog-post that binds
/// `subject` AND a stable row id (`ref` <- the run's `id` key) so a
/// corrupt/absent row can be identified without relying on order.
fn caption_descriptor() -> String {
    json!({
        "contract":"app-views/v1","app":"blog-post","title":"caption views",
        "views":[
            {"id":"caption-runs","title":"Caption runs","kind":"table",
             "fields":[
                {"id":"ref","label":"Ref","format":"text"},
                {"id":"subject","label":"Subject","format":"text"}
             ],
             "columns":[{"field":"ref"},{"field":"subject"}]},
            {"id":"caption-detail","title":"Caption run","kind":"detail",
             "fields":[{"id":"ref","label":"Ref","format":"text"},
                       {"id":"subject","label":"Subject","format":"text"}]}
        ]
    })
    .to_string()
}

fn caption_binding() -> String {
    json!({
        "contract":"app-bindings/v1","app":"blog-post","title":"cb",
        "bindings":[
            {"view":"caption-runs","source":"caption-runs","ops":["list"],
             "fields":[{"field":"ref","key":"id","format":"text"},
                       {"field":"subject","key":"snapshot.inputs.subject","format":"text"}]},
            {"view":"caption-detail","source":"caption-runs","ops":["show"],
             "fields":[{"field":"ref","key":"id","format":"text"},
                       {"field":"subject","key":"snapshot.inputs.subject","format":"text"}]}
        ]
    })
    .to_string()
}

/// Insert a run row whose `snapshot` is a well-formed schema-1/2
/// object with a REAL `material_digest` — valid canonical data through
/// owned test SQL, so a later read reaches the intended guard instead
/// of failing in setup. `context` supplies (id, revision, digest) for
/// schema-2 context-bound runs; `None` writes a contextless schema-1.
fn seed_run(
    w: &Workspace,
    install: &str,
    run_id: &str,
    subject: Option<&str>,
    context: Option<(&str, i64, &str)>,
) {
    let bundle = w.show(install)["digest"].as_str().unwrap().to_string();
    let mut inputs = serde_json::Map::new();
    if let Some(s) = subject {
        inputs.insert("subject".to_string(), json!(s));
    }
    let snapshot = match context {
        Some((cid, rev, cdigest)) => json!({
            "schema":2,"install_id":install,"bundle_digest":bundle,"epoch":1,
            "workflow":{"title":"Instagram post"},"inputs":Value::Object(inputs),
            "context":{"id":cid,"revision":rev,"digest":cdigest}}),
        None => json!({
            "schema":1,"install_id":install,"bundle_digest":bundle,"epoch":1,
            "workflow":{"title":"Manual post"},"inputs":Value::Object(inputs)}),
    };
    seed_run_snapshot(w, install, run_id, snapshot, context.map(|(c, _, _)| c));
}

/// Insert a run row carrying an arbitrary `snapshot` `Value` — used to
/// seed canonical objects with a deliberately wrong-typed optional
/// field (`subject`/`workflow.title`/`context.id`), still with a real
/// `material_digest` so the row is honestly stored. Owned test SQL
/// only; never touches production.
fn seed_run_snapshot(
    w: &Workspace,
    install: &str,
    run_id: &str,
    snapshot: Value,
    context_id: Option<&str>,
) {
    let db = rusqlite::Connection::open(w.daemon.state.join("cadence.sqlite3")).unwrap();
    let bundle = w.show(install)["digest"].as_str().unwrap().to_string();
    let digest = cadence_agent::store::app_runs::material_digest(&snapshot);
    db.execute(
        "INSERT INTO app_runs(id,install_id,epoch,bundle_digest,snapshot,snapshot_digest,owner_pm,request_id,state,context_id,created,updated) VALUES(?,?,1,?,?,?,'owner',?,'succeeded',?,1,1)",
        rusqlite::params![run_id, install, bundle, snapshot.to_string(), digest,
            format!("req-{run_id}"), context_id],
    )
    .unwrap();
}

/// Build the HTTP path for a view read structurally — list is
/// `/views/<view>/rows`, show adds `/<record>` — with query pairs
/// joined whole. Never string-replace out a param: that leaves an
/// empty `k=`/`&&` span which refuses as malformed grammar, not as a
/// genuinely absent digest.
fn view_http_path(
    install: &str,
    view: &str,
    record: Option<&str>,
    params: &[(&str, &str)],
) -> String {
    let mut path = format!("/api/app-installations/{install}/views/{view}/rows");
    if let Some(r) = record {
        path.push('/');
        path.push_str(r);
    }
    if !params.is_empty() {
        let q: Vec<String> = params.iter().map(|(k, v)| format!("{k}={v}")).collect();
        path.push('?');
        path.push_str(&q.join("&"));
    }
    path
}

/// Digest triple as HTTP query pairs (digest/descriptor/binding names).
fn http_digests(d: &Value) -> Vec<(String, String)> {
    vec![
        (
            "digest".to_string(),
            d["digest"].as_str().unwrap().to_string(),
        ),
        (
            "descriptor".to_string(),
            d["view_descriptor_digest"].as_str().unwrap().to_string(),
        ),
        (
            "binding".to_string(),
            d["view_binding_digest"].as_str().unwrap().to_string(),
        ),
    ]
}

/// Write a second descriptor+binding source bundle rebinding `app` to
/// `name`, ready for a real `app_workspace_install` on the SAME
/// daemon/PM — the cross-install fixture.
fn second_source_with(
    w: &Workspace,
    dir_name: &str,
    name: &str,
    descriptor: &str,
    binding: &str,
) -> PathBuf {
    let second_source = w._root.path().join(dir_name);
    for f in [
        "app.md",
        "workflows/blog-post.md",
        "rubrics/blog.md",
        "templates/brief.md",
        "templates/post.md",
    ] {
        let dest = second_source.join(f);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("apps/blog-post")
                .join(f),
            &dest,
        )
        .unwrap();
    }
    let manifest = second_source.join("app.md");
    let mtext = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        mtext.replace("app: blog-post", &format!("app: {name}")),
    )
    .unwrap();
    let views = second_source.join("views");
    std::fs::create_dir_all(&views).unwrap();
    std::fs::write(
        views.join("app-views-v1.json"),
        descriptor.replace("blog-post", name),
    )
    .unwrap();
    let bindings = second_source.join("bindings");
    std::fs::create_dir_all(&bindings).unwrap();
    std::fs::write(
        bindings.join("app-bindings-v1.json"),
        binding.replace("blog-post", name),
    )
    .unwrap();
    let m2 = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        m2.replace(
            "  connections: [publish]",
            "  connections: [publish]\n  views:\n    contract: app-views/v1\n  bindings:\n    contract: app-bindings/v1",
        ),
    )
    .unwrap();
    second_source
}

/// The `customers` list projects every bound field's REAL produced
/// value — including the consent enum domain — keyed by descriptor
/// field id; `record_id` renames the row's `id` (non-identity control).
/// Absent optionals omit the key, never `null`. Uniform `rows`
/// envelope + all three digest pins on both ops.
#[test]
fn cad867_view_read_customers_projects_real_values() {
    let w = Workspace::new();
    w.write_descriptor(&full_customer_descriptor(), true);
    w.write_binding(&full_customer_binding(), true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let (context_id, _) = seed_customer(&w, &id, "Fav Limited", "ctx-1", "cust-1");
    // Sparse record: no email/phone/source/consent.sms (Option fields).
    w.daemon
        .operator_rpc(
            "app_record_create",
            json!({"install_id":id,"context_id":context_id,"record_id":"cust-2",
                "profile":{"schema":1,"display_name":"Sparse","tags":[],
                    "consent":{"email":"unknown"}}}),
        )
        .unwrap();
    let digests = view_read_digests(&w, &id);

    let mut p = view_read_params(&id, "customers", "list", &digests);
    p["context_id"] = json!(context_id);
    let listed = w.daemon.operator_rpc("app_view_read", p).unwrap();
    assert_eq!(listed["view_id"], json!("customers"));
    assert_eq!(listed["op"], json!("list"));
    assert_view_pins(&listed, &digests);

    let rows = listed["rows"].as_array().unwrap();
    let full = rows.iter().find(|r| r["name"] == "Ada Lovelace").unwrap();
    // Real produced values (typed extraction, never raw row keys).
    assert_eq!(full["ref"], json!("cust-1")); // record_id rename -> row id
    assert_eq!(full["email"], json!("ada@example.com"));
    assert_eq!(full["phone"], json!("+1 555 0100"));
    assert_eq!(full["tags"], json!(["vip", "beta"]));
    assert_eq!(full["source"], json!("import"));
    assert_eq!(full["consent_email"], json!("granted"));
    assert_eq!(full["consent_sms"], json!("denied"));
    let sparse = rows.iter().find(|r| r["name"] == "Sparse").unwrap();
    for absent in ["email", "phone", "source", "consent_sms"] {
        assert!(
            !sparse.as_object().unwrap().contains_key(absent),
            "absent optional '{absent}' emitted a cell"
        );
    }
    // The sparse record DID supply consent.email=unknown: the bound
    // enum renders a set value (omission is only for ABSENT optionals),
    // so `consent_email` is present and equals "unknown".
    assert_eq!(sparse["consent_email"], json!("unknown"));
    // `visits`/`tier` are genuinely declared on the `customers` view
    // yet bound to no source key — declared-but-unbound cells never
    // appear in a produced row.
    for unbound in ["visits", "tier"] {
        assert!(
            !full.as_object().unwrap().contains_key(unbound),
            "unbound field '{unbound}' emitted"
        );
    }

    // Filtered query contents: `query` is a real bounded substring
    // search (app_record_list_paged) — a list naming only the matching
    // record's text returns exactly that row, never a silent-ignore
    // all-rows dump.
    let mut p = view_read_params(&id, "customers", "list", &digests);
    p["context_id"] = json!(context_id);
    p["query"] = json!("Ada");
    let filtered = w.daemon.operator_rpc("app_view_read", p).unwrap();
    let frows = filtered["rows"].as_array().unwrap();
    assert_eq!(frows.len(), 1, "query must filter to the matching row");
    assert_eq!(frows[0]["ref"], json!("cust-1"));

    // show: the detail binding projects `ref`+`name` under the same
    // one-row `rows` envelope and echoes all three pins.
    let mut p = view_read_params(&id, "customer-detail", "show", &digests);
    p["context_id"] = json!(context_id);
    p["record_id"] = json!("cust-1");
    let shown = w.daemon.operator_rpc("app_view_read", p).unwrap();
    assert_eq!(shown["op"], json!("show"));
    assert_view_pins(&shown, &digests);
    let rows = shown["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["ref"], json!("cust-1"));
    assert_eq!(rows[0]["name"], json!("Ada Lovelace"));
}

/// `customers` pagination is genuinely supported: `limit`/`cursor`
/// page forward without duplicates; every `show` refuses
/// query/cursor/limit — including a NUMERIC limit, so a typed (not
/// only string-type) refusal is proven; a `record_id` on list refuses.
#[test]
fn cad867_view_read_customers_paginates_and_shows_refuse_paging() {
    let w = Workspace::new();
    w.write_descriptor(&full_customer_descriptor(), true);
    w.write_binding(&full_customer_binding(), true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let (context_id, _) = seed_customer(&w, &id, "Fav Limited", "ctx-1", "cust-1");
    for (rec, name) in [("cust-2", "Two"), ("cust-3", "Three")] {
        w.daemon
            .operator_rpc(
                "app_record_create",
                json!({"install_id":id,"context_id":context_id,"record_id":rec,
                    "profile":{"schema":1,"display_name":name,"tags":[],
                        "consent":{"email":"unknown"}}}),
            )
            .unwrap();
    }
    let digests = view_read_digests(&w, &id);

    // Page 1: numeric limit=1 returns the first row + a cursor.
    let mut p = view_read_params(&id, "customers", "list", &digests);
    p["context_id"] = json!(context_id);
    p["limit"] = json!(1);
    let page1 = w.daemon.operator_rpc("app_view_read", p).unwrap();
    assert_eq!(page1["rows"].as_array().unwrap().len(), 1);
    assert_view_pins(&page1, &digests);
    assert_eq!(page1["truncated"], json!(true));
    let cursor = page1["next_cursor"].clone();
    assert!(cursor.is_string());
    assert!(
        !cursor.as_str().unwrap().is_empty(),
        "next_cursor must be a non-empty string"
    );
    let first_id = page1["rows"][0]["ref"].clone();

    // Page 2: the cursor advances without duplicating page 1.
    let mut p = view_read_params(&id, "customers", "list", &digests);
    p["context_id"] = json!(context_id);
    p["limit"] = json!(1);
    p["cursor"] = cursor;
    let page2 = w.daemon.operator_rpc("app_view_read", p).unwrap();
    assert_eq!(page2["rows"].as_array().unwrap().len(), 1);
    assert_ne!(page2["rows"][0]["ref"], first_id);

    // Every customer `show` refuses query/cursor/limit — string AND
    // numeric — so a typed refusal is proven, not only a string-type one.
    for (bad, val) in [
        ("query", json!("x")),
        ("cursor", json!("x")),
        ("limit", json!("x")),
        ("limit", json!(1)), // numeric show limit — a typed refusal
    ] {
        let mut p = view_read_params(&id, "customer-detail", "show", &digests);
        p["context_id"] = json!(context_id);
        p["record_id"] = json!("cust-1");
        p[bad] = val;
        assert!(
            w.daemon.operator_rpc("app_view_read", p).is_err(),
            "customer-detail show admitted {bad}"
        );
    }
    // `record_id` on a `customers` LIST refuses — a list never names
    // one record.
    let mut p = view_read_params(&id, "customers", "list", &digests);
    p["context_id"] = json!(context_id);
    p["record_id"] = json!("cust-1");
    assert!(w.daemon.operator_rpc("app_view_read", p).is_err());
    // Limit bounds: RECORD_LIMIT is 1..=100 — 0 and 101 both refuse.
    for bound in [0, 101] {
        let mut p = view_read_params(&id, "customers", "list", &digests);
        p["context_id"] = json!(context_id);
        p["limit"] = json!(bound);
        assert!(
            w.daemon.operator_rpc("app_view_read", p).is_err(),
            "customers list admitted limit={bound}"
        );
    }
}

/// Every digest the request asserts is re-proven: wrong, stale (a real
/// upgrade moved it), wrong-typed or missing all refuse before a row
/// is read.
#[test]
fn cad867_view_read_refuses_stale_wrong_and_missing_digests() {
    let w = Workspace::new();
    w.write_descriptor(&full_customer_descriptor(), true);
    w.write_binding(&full_customer_binding(), true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let (context_id, _) = seed_customer(&w, &id, "Fav Limited", "ctx-1", "cust-1");
    let digests = view_read_digests(&w, &id);
    let fake = format!("sha256:{}", "f".repeat(64));

    for field in ["digest", "view_descriptor_digest", "view_binding_digest"] {
        for (label, v) in [
            ("wrong", json!(fake)),
            ("typed", json!(42)),
            ("null", Value::Null),
        ] {
            let mut p = view_read_params(&id, "customers", "list", &digests);
            p["context_id"] = json!(context_id);
            p[field] = v;
            assert!(
                w.daemon.operator_rpc("app_view_read", p).is_err(),
                "{field} {label} admitted"
            );
        }
        let mut p = view_read_params(&id, "customers", "list", &digests);
        p["context_id"] = json!(context_id);
        p.as_object_mut().unwrap().remove(field);
        assert!(
            w.daemon.operator_rpc("app_view_read", p).is_err(),
            "{field} missing admitted"
        );
    }

    // STALE: a real upgrade moves the bundle digest; the pre-upgrade
    // triple is then refused, never authorized against the old snapshot.
    let manifest = w.source().join("app.md");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, text.replace("version: 0.1.0", "version: 0.2.0")).unwrap();
    let proposed = w.upgrade_check(&installed);
    let new_digest = proposed["digest"].as_str().unwrap().to_string();
    let generation = installed["catalog_generation"]
        .as_str()
        .unwrap()
        .to_string();
    w.daemon
        .operator_rpc(
            "app_workspace_upgrade",
            json!({"install_id":id,"source":w.source(),"expected_digest":digests["digest"],
                "expected_generation":generation,"expected_new_digest":new_digest,
                "request_id":"view-stale"}),
        )
        .unwrap();
    let mut p = view_read_params(&id, "customers", "list", &digests);
    p["context_id"] = json!(context_id);
    assert!(
        w.daemon.operator_rpc("app_view_read", p).is_err(),
        "pre-upgrade digest triple still authorized after upgrade"
    );
}

/// Three distinct "no live rows" refusals: descriptor-only install
/// (binding absent → receipt `view_binding` is null), a declared-but-
/// unbound view, and a truly nonexistent view — plus a DECLARED form
/// view (`customer-form`, actually present in the descriptor) can never
/// carry a binding or serve a read. Each request uses a valid customer
/// context and a well-formed asserted binding digest where required,
/// so the refusal reason is the missing binding — never a type or
/// context failure.
#[test]
fn cad867_view_read_distinguishes_absent_binding_unbound_and_missing_view() {
    // (a) descriptor-only install → view_binding null → any read refuses.
    //    A REAL context and a WELL-FORMED binding digest are passed so the
    //    refusal is specifically "no bound binding", not a type/context one.
    let w = Workspace::new();
    w.write_descriptor(&full_customer_descriptor(), true); // no binding
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let (context_id, _) = seed_customer(&w, &id, "Fav Limited", "ctx-1", "cust-1");
    let shown = w.show(&id);
    assert_eq!(shown["view_binding"], Value::Null);
    let well_formed_binding = format!("sha256:{}", "a".repeat(64));
    let digests = json!({
        "digest": shown["digest"],
        "view_descriptor_digest": shown["view_descriptor_digest"],
        "view_binding_digest": well_formed_binding,
    });
    let mut p = view_read_params(&id, "customers", "list", &digests);
    p["context_id"] = json!(context_id);
    assert!(
        w.daemon.operator_rpc("app_view_read", p).is_err(),
        "descriptor-only install served a bound read"
    );

    // (b) declared-but-unbound view: descriptor declares `customers`
    //     AND `customer-detail`; the binding maps only `customers`.
    let w = Workspace::new();
    let partial = json!({
        "contract":"app-bindings/v1","app":"blog-post","title":"b",
        "bindings":[{"view":"customers","source":"customers","ops":["list"],
            "fields":[{"field":"name","key":"display_name","format":"text"}]}]
    })
    .to_string();
    w.write_descriptor(&full_customer_descriptor(), true);
    w.write_binding(&partial, true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let (context_id, _) = seed_customer(&w, &id, "Fav Limited", "ctx-1", "cust-1");
    let digests = view_read_digests(&w, &id);
    // Declared but unbound.
    let mut p = view_read_params(&id, "customer-detail", "show", &digests);
    p["context_id"] = json!(context_id);
    p["record_id"] = json!("cust-1");
    assert!(
        w.daemon.operator_rpc("app_view_read", p).is_err(),
        "declared-but-unbound view served a read"
    );
    // Truly nonexistent view id.
    let mut p = view_read_params(&id, "ghost", "list", &digests);
    p["context_id"] = json!(context_id);
    assert!(
        w.daemon.operator_rpc("app_view_read", p).is_err(),
        "nonexistent view served a read"
    );

    // (c) declared FORM view — `customer-form` is a real `kind:"form"`
    //     view the descriptor actually declares; a read on it refuses
    //     even with a valid context and digest triple (a form can never
    //     carry a binding, and the request would be one anyway).
    let w = Workspace::new();
    w.write_descriptor(&full_customer_descriptor(), true);
    w.write_binding(&full_customer_binding(), true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let (context_id, _) = seed_customer(&w, &id, "Fav Limited", "ctx-1", "cust-1");
    let digests = view_read_digests(&w, &id);
    let mut p = view_read_params(&id, "customer-form", "list", &digests);
    p["context_id"] = json!(context_id);
    assert!(
        w.daemon.operator_rpc("app_view_read", p).is_err(),
        "declared form view served a read"
    );
    let mut p = view_read_params(&id, "customer-form", "show", &digests);
    p["context_id"] = json!(context_id);
    p["record_id"] = json!("cust-1");
    assert!(
        w.daemon.operator_rpc("app_view_read", p).is_err(),
        "declared form view served a show"
    );
}

/// Forged scope/authority fields and source-overrides refuse; op/kind
/// mismatches refuse; `customers` requires a live context; a cross-
/// context and a cross-INSTALL record/show refuse. The foreign install
/// owns a B-ONLY record id that never exists in A, a positive B bound
/// show proves that id is genuinely seeded, and a same-id control
/// proves A/B return their own rows independently.
#[test]
fn cad867_view_read_refuses_forged_scope_and_cross_install() {
    let w = Workspace::new();
    w.write_descriptor(&full_customer_descriptor(), true);
    w.write_binding(&full_customer_binding(), true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let (context_id, record_id) = seed_customer(&w, &id, "Fav Limited", "ctx-1", "cust-1");
    let digests = view_read_digests(&w, &id);

    for extra in ["actor", "source", "sql", "where", "method", "by", "install"] {
        let mut p = view_read_params(&id, "customers", "list", &digests);
        p["context_id"] = json!(context_id);
        p[extra] = json!("forged");
        assert!(
            w.daemon.operator_rpc("app_view_read", p).is_err(),
            "{extra} admitted"
        );
    }
    // op/kind mismatch both directions.
    let mut p = view_read_params(&id, "customers", "show", &digests);
    p["context_id"] = json!(context_id);
    p["record_id"] = json!(record_id);
    assert!(w.daemon.operator_rpc("app_view_read", p).is_err());
    let mut p = view_read_params(&id, "customer-detail", "list", &digests);
    p["context_id"] = json!(context_id);
    assert!(w.daemon.operator_rpc("app_view_read", p).is_err());
    // contextless customers refuses.
    let p = view_read_params(&id, "customers", "list", &digests);
    assert!(w.daemon.operator_rpc("app_view_read", p).is_err());
    // Cross-CONTEXT: a real second context on the SAME install that does
    // not own cust-1 refuses the show.
    let other = w
        .daemon
        .operator_rpc(
            "app_context_create",
            json!({"install_id":id,"label":"Other","input_defaults":{},"request_id":"ctx-2"}),
        )
        .unwrap();
    let mut p = view_read_params(&id, "customer-detail", "show", &digests);
    p["context_id"] = json!(other["context"]["id"]);
    p["record_id"] = json!(record_id);
    assert!(
        w.daemon.operator_rpc("app_view_read", p).is_err(),
        "cross-context record show admitted"
    );

    // Cross-INSTALL on the SAME daemon/PM: a second install owns a
    // B-ONLY record id. A's valid context+pins can never read it, and a
    // same-id control proves A/B return their own rows independently.
    let second_source = second_source_with(
        &w,
        "second-src",
        "second-post",
        &full_customer_descriptor(),
        &full_customer_binding(),
    );
    let second = w
        .daemon
        .operator_rpc("app_workspace_install", json!({"source":second_source}))
        .unwrap();
    let bid = second["install_id"].as_str().unwrap().to_string();
    // B-only record id — never created in A.
    let (bctx, bonly) = seed_customer(&w, &bid, "B Client", "ctx-b1", "cust-b-only");
    let bdigests = view_read_digests(&w, &bid);
    // Positive control: B's own bound show of its foreign-only id works.
    let mut pb = view_read_params(&bid, "customer-detail", "show", &bdigests);
    pb["context_id"] = json!(bctx);
    pb["record_id"] = json!(bonly);
    let bshown = w.daemon.operator_rpc("app_view_read", pb).unwrap();
    assert_eq!(bshown["rows"][0]["ref"], json!("cust-b-only"));
    // Negative: A's valid context+pins can never read the B-only id —
    // A simply has no such record under its own context.
    let mut p = view_read_params(&id, "customer-detail", "show", &digests);
    p["context_id"] = json!(context_id);
    p["record_id"] = json!(bonly);
    assert!(
        w.daemon.operator_rpc("app_view_read", p).is_err(),
        "cross-install show of a B-only id under A's context admitted"
    );
    // Same-id control: seed cust-1 in B too and prove A/B return their
    // own rows independently (no shared store leak).
    w.daemon
        .operator_rpc(
            "app_record_create",
            json!({"install_id":bid,"context_id":bctx,"record_id":"cust-1",
                "profile":{"schema":1,"display_name":"B Ada","tags":[],
                    "consent":{"email":"unknown"}}}),
        )
        .unwrap();
    let mut pb = view_read_params(&bid, "customer-detail", "show", &bdigests);
    pb["context_id"] = json!(bctx);
    pb["record_id"] = json!("cust-1");
    let b_own = w.daemon.operator_rpc("app_view_read", pb).unwrap();
    assert_eq!(b_own["rows"][0]["name"], json!("B Ada"));
    let mut pa = view_read_params(&id, "customer-detail", "show", &digests);
    pa["context_id"] = json!(context_id);
    pa["record_id"] = json!("cust-1");
    let a_own = w.daemon.operator_rpc("app_view_read", pa).unwrap();
    assert_eq!(a_own["rows"][0]["name"], json!("Ada Lovelace"));
    // Foreign context id under A's digests refuses (context not owned by A).
    let mut p = view_read_params(&id, "customers", "list", &digests);
    p["context_id"] = json!(bctx);
    assert!(
        w.daemon.operator_rpc("app_view_read", p).is_err(),
        "foreign context under install A admitted"
    );
}

/// `caption-runs` bound to a caption descriptor: a contextless run
/// lists under the install only; an absent `subject` omits the cell; a
/// supplied context scopes; a real cross-install run `show` on the SAME
/// daemon refuses; run reads reject numeric pagination/query on both
/// ops.
#[test]
fn cad867_view_read_caption_runs_real_rows_and_scope() {
    let w = Workspace::new();
    w.write_descriptor(&caption_descriptor(), true);
    w.write_binding(&caption_binding(), true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let digests = view_read_digests(&w, &id);

    // A real live context for the scoped run.
    let context = w
        .daemon
        .operator_rpc(
            "app_context_create",
            json!({"install_id":id,"label":"Ctx","input_defaults":{},"request_id":"ctx-c"}),
        )
        .unwrap();
    let ctx_id = context["context"]["id"].as_str().unwrap().to_string();
    let ctx_rev = context["context"]["revision"].as_i64().unwrap();
    let ctx_digest = context["context"]["digest"].as_str().unwrap().to_string();
    // run-full: context-bound with a subject (schema 2, real digest).
    // run-bare: contextless, no subject input — `subject` cell omitted.
    seed_run(
        &w,
        &id,
        "run-full",
        Some("New menu launch"),
        Some((&ctx_id, ctx_rev, &ctx_digest)),
    );
    seed_run(&w, &id, "run-bare", None, None);

    // list (contextless allowed): both rows present; the bare run's row
    // omits `subject`; the full run carries the real subject. Rows are
    // identified by the bound `ref` (run id), never by position.
    let p = view_read_params(&id, "caption-runs", "list", &digests);
    let listed = w.daemon.operator_rpc("app_view_read", p).unwrap();
    assert_eq!(listed["view_id"], json!("caption-runs"));
    assert_eq!(listed["op"], json!("list"));
    assert_view_pins(&listed, &digests);
    let rows = listed["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let full = rows
        .iter()
        .find(|r| r["ref"] == "run-full")
        .expect("the context-bound run's row");
    assert_eq!(full["subject"], json!("New menu launch"));
    let bare = rows
        .iter()
        .find(|r| r["ref"] == "run-bare")
        .expect("the contextless run's row");
    assert!(!bare.as_object().unwrap().contains_key("subject"));
    // Only bound fields appear — `state`/`context_id`/`snapshot.*` are
    // not bound in this descriptor, so absent.
    for unbound in ["id", "state", "context_id"] {
        assert!(
            !full.as_object().unwrap().contains_key(unbound),
            "unbound run field '{unbound}' emitted"
        );
    }
    // A supplied context scopes the list to the one context-bound run.
    let mut p = view_read_params(&id, "caption-runs", "list", &digests);
    p["context_id"] = json!(ctx_id);
    let scoped = w.daemon.operator_rpc("app_view_read", p).unwrap();
    assert_eq!(scoped["rows"].as_array().unwrap().len(), 1);
    assert_eq!(scoped["rows"][0]["ref"], json!("run-full"));
    // show the context-bound run under its own context.
    let mut p = view_read_params(&id, "caption-detail", "show", &digests);
    p["record_id"] = json!("run-full");
    p["context_id"] = json!(ctx_id);
    let shown = w.daemon.operator_rpc("app_view_read", p).unwrap();
    assert_view_pins(&shown, &digests);
    assert_eq!(shown["rows"][0]["subject"], json!("New menu launch"));
    // A show naming a valid context but the contextless run refuses —
    // the run does not belong to that context.
    let mut p = view_read_params(&id, "caption-detail", "show", &digests);
    p["record_id"] = json!("run-bare");
    p["context_id"] = json!(ctx_id);
    assert!(
        w.daemon.operator_rpc("app_view_read", p).is_err(),
        "contextless run shown under a context"
    );
    // run reads reject pagination/query on BOTH ops — with a NUMERIC
    // limit so a typed refusal is proven, not only a string-type one.
    for bad in ["limit", "cursor", "query"] {
        for (view, op, rec) in [
            ("caption-runs", "list", None),
            ("caption-detail", "show", Some("run-full")),
        ] {
            let mut p = view_read_params(&id, view, op, &digests);
            if let Some(r) = rec {
                p["record_id"] = json!(r);
            }
            p[bad] = json!(1); // numeric — a typed refusal
            assert!(
                w.daemon.operator_rpc("app_view_read", p).is_err(),
                "{view} {op} admitted numeric {bad}"
            );
        }
    }

    // Cross-INSTALL run on the SAME daemon: a real second install owns
    // run-b-only; a caption-detail show via install A's digests refuses.
    let second_source = second_source_with(
        &w,
        "second-src",
        "second-post",
        &caption_descriptor(),
        &caption_binding(),
    );
    let second = w
        .daemon
        .operator_rpc("app_workspace_install", json!({"source":second_source}))
        .unwrap();
    let bid = second["install_id"].as_str().unwrap().to_string();
    let bctx = w
        .daemon
        .operator_rpc(
            "app_context_create",
            json!({"install_id":bid,"label":"B Ctx","input_defaults":{},"request_id":"ctx-b"}),
        )
        .unwrap();
    let bctx_id = bctx["context"]["id"].as_str().unwrap().to_string();
    let bctx_rev = bctx["context"]["revision"].as_i64().unwrap();
    let bctx_digest = bctx["context"]["digest"].as_str().unwrap().to_string();
    seed_run(
        &w,
        &bid,
        "run-b-only",
        Some("B subject"),
        Some((&bctx_id, bctx_rev, &bctx_digest)),
    );
    // Positive: B's own bound show of run-b-only works.
    let bdigests = view_read_digests(&w, &bid);
    let mut pb = view_read_params(&bid, "caption-detail", "show", &bdigests);
    pb["record_id"] = json!("run-b-only");
    pb["context_id"] = json!(bctx_id);
    let bshown = w.daemon.operator_rpc("app_view_read", pb).unwrap();
    assert_eq!(bshown["rows"][0]["subject"], json!("B subject"));
    // Negative: A's digests/context can never read B's run id.
    let mut p = view_read_params(&id, "caption-detail", "show", &digests);
    p["record_id"] = json!("run-b-only");
    p["context_id"] = json!(ctx_id);
    assert!(
        w.daemon.operator_rpc("app_view_read", p).is_err(),
        "cross-install run show admitted"
    );
}

/// Producer corruption: a record body that no longer parses refuses
/// the whole read (never a partial row); a run whose `snapshot` is
/// well-formed JSON but semantically NOT the produced object refuses
/// its show, and a list containing it must never emit a forged/partial
/// row for the corrupt run — while a valid sibling run still reads.
#[test]
fn cad867_view_read_refuses_corrupt_producer_rows() {
    // (a) corrupt customer record body.
    let w = Workspace::new();
    w.write_descriptor(&full_customer_descriptor(), true);
    w.write_binding(&full_customer_binding(), true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let (context_id, _) = seed_customer(&w, &id, "Fav Limited", "ctx-1", "cust-1");
    let digests = view_read_digests(&w, &id);
    let recdb =
        rusqlite::Connection::open(w.daemon.state.join(format!("app-records/{id}.sqlite3")))
            .unwrap();
    recdb
        .execute(
            "UPDATE app_records SET body='not-json' WHERE context_id=? AND id='cust-1'",
            rusqlite::params![context_id],
        )
        .unwrap();
    let mut p = view_read_params(&id, "customers", "list", &digests);
    p["context_id"] = json!(context_id);
    assert!(
        w.daemon.operator_rpc("app_view_read", p).is_err(),
        "corrupt record row served"
    );

    // (b) corrupt run snapshot: a VALID run row exists as the ordinary
    //     control, plus a row whose `snapshot` column is syntactically
    //     valid JSON but the WRONG produced shape — a bare string, not
    //     the snapshot object the schema requires. Its own digest is
    //     honest, so the row itself is well-formed; only the producer
    //     content is corrupt, so the failure lands at the read guard.
    let w = Workspace::new();
    w.write_descriptor(&caption_descriptor(), true);
    w.write_binding(&caption_binding(), true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let digests = view_read_digests(&w, &id);
    // Valid ordinary run control (real canonical snapshot+digest).
    seed_run(&w, &id, "run-ok", Some("ok"), None);
    // Corrupt run: valid row shape, invalid producer content.
    let db = rusqlite::Connection::open(w.daemon.state.join("cadence.sqlite3")).unwrap();
    let bundle = installed["digest"].as_str().unwrap();
    let snap_bad = json!("not-a-snapshot-object");
    db.execute(
        "INSERT INTO app_runs(id,install_id,epoch,bundle_digest,snapshot,snapshot_digest,owner_pm,request_id,state,context_id,created,updated) VALUES(?,?,1,?,?,?,'owner','req-bad','succeeded',NULL,1,1)",
        rusqlite::params![
            "run-corrupt", id, bundle, snap_bad.to_string(),
            cadence_agent::store::app_runs::material_digest(&snap_bad)],
    ).unwrap();
    drop(db);
    // The corrupt run's own show refuses — bounded, no partial row.
    let mut show_bad = view_read_params(&id, "caption-detail", "show", &digests);
    show_bad["record_id"] = json!("run-corrupt");
    assert!(
        w.daemon.operator_rpc("app_view_read", show_bad).is_err(),
        "corrupt run snapshot served a show"
    );
    // A list that includes the corrupt run must REFUSE wholesale — a
    // producer-integrity error is never a partial-success skip that
    // still returns the good row. The refusal is a bounded `rejected`
    // error, not an internal crash.
    let p = view_read_params(&id, "caption-runs", "list", &digests);
    let listed = w
        .daemon
        .operator_rpc("app_view_read", p)
        .expect_err("list over a corrupt run must refuse, never skip it");
    assert_eq!(
        listed.kind(),
        "rejected",
        "corrupt-run list refusal was not a bounded rejection: {listed}"
    );
    // A valid ordinary run still shows.
    let mut show_ok = view_read_params(&id, "caption-detail", "show", &digests);
    show_ok["record_id"] = json!("run-ok");
    let ok = w.daemon.operator_rpc("app_view_read", show_ok).unwrap();
    assert_eq!(ok["rows"][0]["subject"], json!("ok"));
}

/// An optional run field that is PRESENT but the wrong type must
/// refuse — `subject`/`workflow.title`/`context.id`/`context_id` are
/// optional cells only when ABSENT or null; a present number/object
/// is a corrupt producer shape, never a silent omission. Each case
/// uses a real canonical snapshot object with an honest
/// `material_digest`, so the refusal lands at the read guard, not in
/// setup. A contextless run and an absent-subject run remain valid
/// positive controls.
#[test]
fn cad867_view_read_refuses_wrong_typed_optional_run_fields() {
    let w = Workspace::new();
    w.write_descriptor(&caption_descriptor(), true);
    w.write_binding(&caption_binding(), true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let bundle = installed["digest"].as_str().unwrap().to_string();
    let digests = view_read_digests(&w, &id);

    // Positive controls: an absent-subject contextless run (subject
    // omitted) and a subject-bearing run both read fine.
    seed_run(&w, &id, "run-ok", Some("ok"), None);
    seed_run(&w, &id, "run-nosubj", None, None);

    // (1) inputs.subject present but a NUMBER.
    let snap = json!({"schema":1,"install_id":id,"bundle_digest":bundle,"epoch":1,
        "workflow":{"title":"x"},"inputs":{"subject":42}});
    seed_run_snapshot(&w, &id, "run-subj-num", snap, None);
    // (2) workflow.title present but a NUMBER.
    let snap = json!({"schema":1,"install_id":id,"bundle_digest":bundle,"epoch":1,
        "workflow":{"title":42},"inputs":{"subject":"s"}});
    seed_run_snapshot(&w, &id, "run-title-num", snap, None);
    // (3) snapshot.context.id present but a NUMBER (schema-2 run needs
    //     a real context; use a fresh owned context's shape).
    let ctx = w
        .daemon
        .operator_rpc(
            "app_context_create",
            json!({"install_id":id,"label":"C","input_defaults":{},"request_id":"ctx-wt"}),
        )
        .unwrap();
    let cid = ctx["context"]["id"].as_str().unwrap();
    let snap = json!({"schema":2,"install_id":id,"bundle_digest":bundle,"epoch":1,
        "workflow":{"title":"x"},"inputs":{"subject":"s"},
        "context":{"id":42,"revision":ctx["context"]["revision"],"digest":ctx["context"]["digest"]}});
    seed_run_snapshot(&w, &id, "run-ctxid-num", snap, Some(cid));

    // Each wrong-typed run refuses on both list and show.
    for run_id in ["run-subj-num", "run-title-num", "run-ctxid-num"] {
        let mut p = view_read_params(&id, "caption-detail", "show", &digests);
        p["record_id"] = json!(run_id);
        let err = w
            .daemon
            .operator_rpc("app_view_read", p)
            .expect_err("a present-but-wrong-typed optional field must refuse");
        assert_eq!(err.kind(), "rejected", "{run_id} non-bounded error: {err}");
    }
    // The list containing a wrong-typed run refuses wholesale.
    let p = view_read_params(&id, "caption-runs", "list", &digests);
    let err = w
        .daemon
        .operator_rpc("app_view_read", p)
        .expect_err("list over a wrong-typed run must refuse");
    assert_eq!(err.kind(), "rejected", "list non-bounded error: {err}");

    // Valid controls still read: subject-bearing and subject-absent.
    let mut p = view_read_params(&id, "caption-detail", "show", &digests);
    p["record_id"] = json!("run-ok");
    let ok = w.daemon.operator_rpc("app_view_read", p).unwrap();
    assert_eq!(ok["rows"][0]["subject"], json!("ok"));
    let mut p = view_read_params(&id, "caption-detail", "show", &digests);
    p["record_id"] = json!("run-nosubj");
    let ok = w.daemon.operator_rpc("app_view_read", p).unwrap();
    assert!(
        !ok["rows"][0].as_object().unwrap().contains_key("subject"),
        "absent subject emitted a cell"
    );
}

/// Concurrent reads stay consistent and a read never observes a torn
/// descriptor/binding. `std::thread::scope` borrows the daemon so each
/// thread uses `TestDaemon::operator_rpc` — the explicit operator
/// assertion — never a bare `client::rpc`. The upgrade changes BOTH a
/// bound projection (`name` -> `email`) and the version, so old/new
/// row values are genuinely distinguishable.
///
/// This is stress/race coverage, NOT deterministic guard-removal RED:
/// a `Barrier` aligns thread entry, but no test seam forces an exact
/// interleave inside the critical section, so a racing read may
/// complete under its (old) pins or refuse as stale — both consistent.
/// It must never return pins from one revision with rows from another.
#[test]
fn cad867_view_read_is_consistent_under_concurrent_reads_and_upgrade() {
    use std::sync::{Arc, Barrier};
    let w = Workspace::new();
    w.write_descriptor(&full_customer_descriptor(), true);
    w.write_binding(&full_customer_binding(), true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let (context_id, _) = seed_customer(&w, &id, "Fav Limited", "ctx-1", "cust-1");
    let old_digests = view_read_digests(&w, &id);

    // N parallel identical reads all succeed identically (operator
    // calls under a barrier, scoped so they borrow `w.daemon`).
    let barrier = Arc::new(Barrier::new(4));
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let b = Arc::clone(&barrier);
                let dg = old_digests.clone();
                let ctx = context_id.clone();
                let iid = id.clone();
                let d = &w.daemon;
                scope.spawn(move || {
                    b.wait();
                    let mut p = view_read_params(&iid, "customer-detail", "show", &dg);
                    p["context_id"] = json!(ctx);
                    p["record_id"] = json!("cust-1");
                    d.operator_rpc("app_view_read", p)
                })
            })
            .collect();
        for h in handles {
            let v = h.join().unwrap().unwrap();
            assert_eq!(v["rows"][0]["name"], json!("Ada Lovelace"));
            assert_view_pins(&v, &old_digests);
        }
    });

    // Prepare the upgrade: version bump + a binding that renames the
    // `name` produced key to `email` — post-upgrade rows differ, so a
    // torn read is detectable rather than vacuously equal.
    let manifest = w.source().join("app.md");
    let mtext = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, mtext.replace("version: 0.1.0", "version: 0.2.0")).unwrap();
    std::fs::write(
        w.source().join("bindings/app-bindings-v1.json"),
        v2_customer_binding(),
    )
    .unwrap();
    let proposed = w.upgrade_check(&installed);
    let new_digest = proposed["digest"].as_str().unwrap().to_string();
    let generation = installed["catalog_generation"]
        .as_str()
        .unwrap()
        .to_string();
    let upgrade_params = json!({"install_id":id,"source":w.source(),
        "expected_digest":old_digests["digest"],"expected_generation":generation,
        "expected_new_digest":new_digest,"request_id":"view-race-upg"});

    // Barrier-synchronized read-vs-upgrade: both threads enter together;
    // the read pins the OLD triple. It either completes with the OLD
    // consistent rows+pins or refuses — never torn.
    let barrier = Arc::new(Barrier::new(2));
    let read_res = std::thread::scope(|scope| {
        let read_handle = {
            let b = Arc::clone(&barrier);
            let dg = old_digests.clone();
            let ctx = context_id.clone();
            let iid = id.clone();
            let d = &w.daemon;
            scope.spawn(move || {
                b.wait();
                let mut p = view_read_params(&iid, "customer-detail", "show", &dg);
                p["context_id"] = json!(ctx);
                p["record_id"] = json!("cust-1");
                d.operator_rpc("app_view_read", p)
            })
        };
        let up_handle = {
            let b = Arc::clone(&barrier);
            let d = &w.daemon;
            let up = upgrade_params.clone();
            scope.spawn(move || {
                b.wait();
                d.operator_rpc("app_workspace_upgrade", up)
            })
        };
        let r = read_handle.join().unwrap();
        // The upgrade must actually succeed — `join().unwrap().unwrap()`
        // propagates a refusal so a failed upgrade can never leave the
        // post-upgrade new-pinned assertions vacuously satisfied.
        let upgrade = up_handle.join().unwrap().unwrap();
        assert_eq!(upgrade["digest"], json!(new_digest));
        r
    });
    match read_res {
        Ok(v) => {
            // Completed under its pins: rows+pins are ONE revision — the
            // OLD one (the request pinned old digests). A torn impl that
            // authorized pre-commit then read new bytes returns the new
            // `name` cell (the email) under old pins — caught here.
            assert_view_pins(&v, &old_digests);
            assert_eq!(
                v["rows"][0]["name"],
                json!("Ada Lovelace"),
                "racing read returned a torn old-pins/new-projection row"
            );
        }
        Err(e) => {
            // A racing read may only refuse as a bounded `rejected`
            // (stale-pins) error — never an internal/timeout silently
            // reclassified as consistency.
            assert_eq!(
                e.kind(),
                "rejected",
                "racing read failed with a non-bounded error: {e}"
            );
        }
    }
    // Post-upgrade the receipt pins genuinely moved: the bundle digest
    // and (the binding bytes changed) the binding digest differ.
    let new_digests = view_read_digests(&w, &id);
    assert_ne!(
        new_digests["digest"], old_digests["digest"],
        "upgrade did not move the bundle pin"
    );
    assert_ne!(
        new_digests["view_binding_digest"], old_digests["view_binding_digest"],
        "upgrade did not move the binding pin"
    );
    // A new-pinned read succeeds and now projects the renamed `name`
    // cell — `email` under the v2 binding — proving the projection
    // actually changed under the new pins.
    let mut p = view_read_params(&id, "customer-detail", "show", &new_digests);
    p["context_id"] = json!(context_id);
    p["record_id"] = json!("cust-1");
    let new_read = w.daemon.operator_rpc("app_view_read", p).unwrap();
    assert_view_pins(&new_read, &new_digests);
    assert_eq!(
        new_read["rows"][0]["name"],
        json!("ada@example.com"),
        "post-upgrade `name` did not project the renamed `email` key"
    );
    let mut p = view_read_params(&id, "customer-detail", "show", &old_digests);
    p["context_id"] = json!(context_id);
    p["record_id"] = json!("cust-1");
    assert!(
        w.daemon.operator_rpc("app_view_read", p).is_err(),
        "post-upgrade read under old pins authorized"
    );
}

/// Operator-only parity on the DAEMON: a planted-pane agent caller, a
/// `setsid -f`-detached agent-shaped child (carrying the enrolled
/// lane's `CADENCE_ALIAS`, never a bare unmarked residual), and an
/// unproven caller each refuse a fully-valid bound read (valid context
/// and all digests) — while the operator's identical request succeeds.
/// The detached child hands its response back through a durable
/// response-file (the `operator_rpc.py` script shape), never a
/// parent-exited stdout race.
#[test]
fn cad867_view_read_daemon_actor_gate() {
    let w = Workspace::new();
    w.write_descriptor(&full_customer_descriptor(), true);
    w.write_binding(&full_customer_binding(), true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let (context_id, _) = seed_customer(&w, &id, "Fav Limited", "ctx-1", "cust-1");
    let digests = view_read_digests(&w, &id);

    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "view-reader", "claude", None, lane.pid());
    // Agent caller (planted pane ancestry) with a fully-valid request —
    // a missing-context error can never masquerade as the operator gate.
    let mut valid = view_read_params(&id, "customers", "list", &digests);
    valid["context_id"] = json!(context_id);
    let frame = lane.rpc(&w.daemon.state, "app_view_read", valid.clone());
    assert_eq!(frame["ok"], false, "agent bound read: {frame}");
    // A forged `actor` field from the same pane refuses.
    let mut forged = valid.clone();
    forged["actor"] = json!("operator");
    let frame = lane.rpc(&w.daemon.state, "app_view_read", forged);
    assert_eq!(frame["ok"], false, "agent forged actor: {frame}");

    // `setsid -f`-detached agent-shaped child: the SAME valid request
    // over the daemon socket. `CADENCE_ALIAS=view-reader` marks it as
    // the enrolled lane's own env-derived identity — agent-shaped, not
    // the bare-setsid residual `peer::operator_proof` accepts as OS
    // operator (documented on `TestDaemon::operator_rpc`). The response
    // lands at a durable file the test polls, not a raced stdout.
    let req = lane.dir.path().join("viewread-detached.json");
    std::fs::write(
        &req,
        cadence_agent::proto::request("app_view_read", valid.clone()).to_string(),
    )
    .unwrap();
    let out = lane.dir.path().join("viewread-detached.out");
    let script = lane.dir.path().join("detached-rpc.py");
    std::fs::write(&script, common::op::rpc_script_from_file()).unwrap();
    let sock = cadence_agent::client::socket_path(&w.daemon.state);
    let (rc, runout) = lane.run(&format!(
        "setsid -f env CADENCE_ALIAS=view-reader python3 {} {} {} {} {}",
        script.display(),
        sock.display(),
        req.display(),
        out.display(),
        std::process::id()
    ));
    assert_eq!(rc, 0, "detached rpc invocation failed: {runout}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !out.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "detached bound read never answered"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let frame: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(
        frame["ok"], false,
        "detached agent-shaped bound read admitted: {frame}"
    );

    // Unproven caller (deterministically `Who::Unproven`) also refuses.
    assert!(
        w.daemon
            .unproven_rpc("app_view_read", valid.clone())
            .is_err(),
        "unproven bound read admitted"
    );

    // Operator control: the identical request over RPC succeeds.
    let ok = w.daemon.operator_rpc("app_view_read", valid).unwrap();
    assert_eq!(ok["rows"][0]["name"], json!("Ada Lovelace"));
}

/// Operator-only parity on the HTTP peer: a real operator session reads
/// customers AND caption rows (list + show for both sources); each
/// digest missing/wrong/stale, a duplicate descriptor/binding param,
/// `source`/`op`/show-paging selectors and forged fields each refuse
/// with a meaningful 400/403/404 — never a `>=400` that would accept a
/// crash-500; an agent-peered replay refuses exactly 403.
#[test]
fn cad867_view_read_http_actor_gate_and_parity() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};
    let w = Workspace::new();
    // One bundle binding BOTH customer and caption views so a single
    // install exercises both sources' routes.
    let descriptor = {
        let mut d: Value = serde_json::from_str(&full_customer_descriptor()).unwrap();
        let cap: Value = serde_json::from_str(&caption_descriptor()).unwrap();
        d["views"]
            .as_array_mut()
            .unwrap()
            .extend(cap["views"].as_array().unwrap().iter().cloned());
        d.to_string()
    };
    let binding = {
        let mut b: Value = serde_json::from_str(&full_customer_binding()).unwrap();
        let cap: Value = serde_json::from_str(&caption_binding()).unwrap();
        b["bindings"]
            .as_array_mut()
            .unwrap()
            .extend(cap["bindings"].as_array().unwrap().iter().cloned());
        b.to_string()
    };
    w.write_descriptor(&descriptor, true);
    w.write_binding(&binding, true);
    let installed = w.install().unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let (context_id, _) = seed_customer(&w, &id, "Fav Limited", "ctx-1", "cust-1");
    // A second customer so `limit=1` + cursor continuation is provable
    // over HTTP with a real no-duplicate `ref` control.
    w.daemon
        .operator_rpc(
            "app_record_create",
            json!({"install_id":id,"context_id":context_id,"record_id":"cust-2",
                "profile":{"schema":1,"display_name":"Second","tags":[],
                    "consent":{"email":"unknown"}}}),
        )
        .unwrap();
    // A context-bound caption run for the caption list/show controls.
    let ctx = w
        .daemon
        .operator_rpc(
            "app_context_create",
            json!({"install_id":id,"label":"RCtx","input_defaults":{},"request_id":"ctx-r"}),
        )
        .unwrap();
    let rctx = ctx["context"]["id"].as_str().unwrap().to_string();
    let rctx_rev = ctx["context"]["revision"].as_i64().unwrap();
    let rctx_digest = ctx["context"]["digest"].as_str().unwrap().to_string();
    seed_run(
        &w,
        &id,
        "run-h",
        Some("hi"),
        Some((&rctx, rctx_rev, &rctx_digest)),
    );
    let digests = view_read_digests(&w, &id);
    let d = &digests;

    // HTTP board on the same daemon.
    let lease = test_port();
    let port = lease.port;
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

    // Positive operator reads: customers list AND show, caption list
    // AND show — controls proving the route serves both ops for both
    // sources. `limit`/`query` on `customers` list are legitimate
    // selectors and return 200.
    let dig = http_digests(d);
    let dig_pairs: Vec<(&str, &str)> = dig.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let cust_list = |extra: &str| {
        let mut p = dig_pairs.clone();
        p.push(("context_id", context_id.as_str()));
        let mut path = view_http_path(&id, "customers", None, &p);
        if !extra.is_empty() {
            path.push_str(&format!("&{extra}"));
        }
        path
    };
    let (code, _, body) = common::op::raw(port, &session.request("GET", &cust_list(""), ""));
    assert_eq!(code, 200, "operator customers list: {body}");
    let (code, _, body) = common::op::raw(port, &session.request("GET", &cust_list("limit=1"), ""));
    assert_eq!(code, 200, "operator customers list limit=1: {body}");
    let (code, _, body) =
        common::op::raw(port, &session.request("GET", &cust_list("query=Ada"), ""));
    assert_eq!(code, 200, "operator customers list query: {body}");
    // Filtered query must actually FILTER — a 200 that ignores `query`
    // and returns all rows is a silent-ignore hole.
    let filtered: Value = serde_json::from_str(&body).unwrap();
    let frows = filtered["rows"].as_array().unwrap();
    assert_eq!(frows.len(), 1, "HTTP query must filter to the match");
    assert_eq!(frows[0]["ref"], json!("cust-1"));
    // Limit bounds over HTTP: 0 and 101 refuse (RECORD_LIMIT 1..=100).
    for bound in ["limit=0", "limit=101"] {
        let (code, _, body) = common::op::raw(port, &session.request("GET", &cust_list(bound), ""));
        assert_eq!(code, 400, "HTTP customers list admitted {bound}: {body}");
    }
    // Cursor continuation over HTTP: `limit=1` returns one row plus a
    // `next_cursor`; following it returns a DIFFERENT `ref` (no
    // duplicate of the first page).
    let (code, _, body) = common::op::raw(port, &session.request("GET", &cust_list("limit=1"), ""));
    assert_eq!(code, 200, "customers list page1: {body}");
    let page1: Value = serde_json::from_str(&body).unwrap();
    let p1_rows = page1["rows"].as_array().unwrap();
    assert_eq!(p1_rows.len(), 1, "limit=1 must return one row");
    let first_ref = p1_rows[0]["ref"].as_str().unwrap().to_string();
    let cursor = page1["next_cursor"]
        .as_str()
        .expect("page1 must emit a next_cursor")
        .to_string();
    let next = cust_list(&format!("limit=1&cursor={cursor}"));
    let (code, _, body) = common::op::raw(port, &session.request("GET", &next, ""));
    assert_eq!(code, 200, "customers list page2: {body}");
    let page2: Value = serde_json::from_str(&body).unwrap();
    let p2_rows = page2["rows"].as_array().unwrap();
    assert_eq!(p2_rows.len(), 1);
    assert_ne!(
        p2_rows[0]["ref"].as_str().unwrap(),
        first_ref,
        "cursor page duplicated the first row"
    );
    // customers show (record id in path).
    let mut sp = dig_pairs.clone();
    sp.push(("context_id", context_id.as_str()));
    let show_path = view_http_path(&id, "customer-detail", Some("cust-1"), &sp);
    let (code, _, body) = common::op::raw(port, &session.request("GET", &show_path, ""));
    assert_eq!(code, 200, "operator customers show: {body}");
    // caption list + show (context-bound run under its context).
    let mut cp = dig_pairs.clone();
    cp.push(("context_id", rctx.as_str()));
    let cap_list = view_http_path(&id, "caption-runs", None, &cp);
    let (code, _, body) = common::op::raw(port, &session.request("GET", &cap_list, ""));
    assert_eq!(code, 200, "operator caption list: {body}");
    let cap_show = view_http_path(&id, "caption-detail", Some("run-h"), &cp);
    let (code, _, body) = common::op::raw(port, &session.request("GET", &cap_show, ""));
    assert_eq!(code, 200, "operator caption show: {body}");

    // Each digest missing individually — built structurally so the pair
    // is wholly absent (never an empty `k=`/`&&` span).
    for (label, params) in [
        (
            "missing digest",
            vec![
                ("descriptor", d["view_descriptor_digest"].as_str().unwrap()),
                ("binding", d["view_binding_digest"].as_str().unwrap()),
                ("context_id", context_id.as_str()),
            ],
        ),
        (
            "missing descriptor",
            vec![
                ("digest", d["digest"].as_str().unwrap()),
                ("binding", d["view_binding_digest"].as_str().unwrap()),
                ("context_id", context_id.as_str()),
            ],
        ),
        (
            "missing binding",
            vec![
                ("digest", d["digest"].as_str().unwrap()),
                ("descriptor", d["view_descriptor_digest"].as_str().unwrap()),
                ("context_id", context_id.as_str()),
            ],
        ),
    ] {
        let path = view_http_path(&id, "customers", None, &params);
        let (code, _, body) = common::op::raw(port, &session.request("GET", &path, ""));
        assert_eq!(code, 400, "{label} bound read served: {code} {body}");
    }
    // Wrong digests (each individually).
    let fake = format!("sha256:{}", "f".repeat(64));
    for (label, params) in [
        (
            "wrong digest",
            vec![
                ("digest", fake.as_str()),
                ("descriptor", d["view_descriptor_digest"].as_str().unwrap()),
                ("binding", d["view_binding_digest"].as_str().unwrap()),
                ("context_id", context_id.as_str()),
            ],
        ),
        (
            "wrong descriptor",
            vec![
                ("digest", d["digest"].as_str().unwrap()),
                ("descriptor", fake.as_str()),
                ("binding", d["view_binding_digest"].as_str().unwrap()),
                ("context_id", context_id.as_str()),
            ],
        ),
        (
            "wrong binding",
            vec![
                ("digest", d["digest"].as_str().unwrap()),
                ("descriptor", d["view_descriptor_digest"].as_str().unwrap()),
                ("binding", fake.as_str()),
                ("context_id", context_id.as_str()),
            ],
        ),
    ] {
        let path = view_http_path(&id, "customers", None, &params);
        let (code, _, body) = common::op::raw(port, &session.request("GET", &path, ""));
        assert_eq!(code, 400, "{label} bound read served: {code} {body}");
    }
    // Duplicate descriptor and duplicate binding params.
    for dup in ["descriptor", "binding"] {
        let mut params = dig_pairs.clone();
        params.push(("context_id", context_id.as_str()));
        params.push((dup, d["view_descriptor_digest"].as_str().unwrap()));
        let path = view_http_path(&id, "customers", None, &params);
        let (code, _, body) = common::op::raw(port, &session.request("GET", &path, ""));
        assert_eq!(code, 400, "duplicate {dup} served: {code} {body}");
    }
    // Forged actor/source/op/scope params on the list route.
    for (label, key) in [
        ("actor", "actor"),
        ("source", "source"),
        ("op", "op"),
        ("view_id scalar", "view_id"),
    ] {
        let mut params = dig_pairs.clone();
        params.push(("context_id", context_id.as_str()));
        params.push((key, "forged"));
        let path = view_http_path(&id, "customers", None, &params);
        let (code, _, body) = common::op::raw(port, &session.request("GET", &path, ""));
        assert_eq!(code, 400, "{label} bound read served: {code} {body}");
    }
    // Paging/show-only selectors refused on shows and caption reads —
    // numeric `limit` text included so a typed refusal is proven.
    for (label, view, record, extra) in [
        (
            "query on customers show",
            "customer-detail",
            Some("cust-1"),
            "query=x",
        ),
        (
            "cursor on customers show",
            "customer-detail",
            Some("cust-1"),
            "cursor=abc",
        ),
        (
            "limit on customers show",
            "customer-detail",
            Some("cust-1"),
            "limit=1",
        ),
        ("cursor on caption list", "caption-runs", None, "cursor=abc"),
        ("limit on caption list", "caption-runs", None, "limit=1"),
        ("query on caption list", "caption-runs", None, "query=x"),
        (
            "limit on caption show",
            "caption-detail",
            Some("run-h"),
            "limit=1",
        ),
    ] {
        let mut params = dig_pairs.clone();
        params.push(("context_id", context_id.as_str()));
        let mut path = view_http_path(&id, view, record, &params);
        path.push_str(&format!("&{extra}"));
        let (code, _, body) = common::op::raw(port, &session.request("GET", &path, ""));
        assert_eq!(code, 400, "{label} served: {code} {body}");
    }

    // Cross-install HTTP show: install A's digests/context never serve
    // a B-only record over HTTP either.
    let second_source = second_source_with(
        &w,
        "second-src",
        "second-post",
        &full_customer_descriptor(),
        &full_customer_binding(),
    );
    let second = w
        .daemon
        .operator_rpc("app_workspace_install", json!({"source":second_source}))
        .unwrap();
    let bid = second["install_id"].as_str().unwrap().to_string();
    let (bctx, _) = seed_customer(&w, &bid, "B Client", "ctx-b1", "cust-b-only");
    let _ = bctx;
    let cross = view_http_path(&id, "customer-detail", Some("cust-b-only"), &dig_pairs);
    let (code, _, body) = common::op::raw(
        port,
        &session.request("GET", &format!("{cross}&context_id={context_id}"), ""),
    );
    assert!(
        matches!(code, 400 | 403 | 404),
        "cross-install HTTP show served: {code} {body}"
    );

    // STALE digest on HTTP: after a real upgrade the old digest triple
    // refuses with 400.
    let manifest = w.source().join("app.md");
    let mtext = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, mtext.replace("version: 0.1.0", "version: 0.2.0")).unwrap();
    // The second install above advanced the catalog, so the generation
    // captured before it is now stale: upgrade_check must reject it —
    // asserted on the raw RPC error, not a helper unwrap.
    let stale_check = w.daemon.operator_rpc(
        "app_workspace_upgrade_check",
        json!({
            "install_id":installed["install_id"], "source":w.source(),
            "expected_digest":installed["digest"],
            "expected_generation":installed["catalog_generation"]
        }),
    );
    assert!(
        stale_check
            .as_ref()
            .is_err_and(|error| error.to_string().contains("catalog generation is stale")),
        "pre-second-install generation must reject as stale: {stale_check:?}"
    );
    // Re-read the install: the catalog generation moved under it, but
    // its bundle digest is untouched — the upgrade pins the fresh
    // generation against the original installed digest.
    let refreshed = w.show(&id);
    assert_eq!(
        refreshed["digest"], installed["digest"],
        "cross-install changed the target bundle"
    );
    let generation = refreshed["catalog_generation"]
        .as_str()
        .unwrap()
        .to_string();
    let proposed = w.upgrade_check(&refreshed);
    let new_digest = proposed["digest"].as_str().unwrap().to_string();
    w.daemon
        .operator_rpc(
            "app_workspace_upgrade",
            json!({"install_id":id,"source":w.source(),"expected_digest":d["digest"],
                "expected_generation":generation,"expected_new_digest":new_digest,
                "request_id":"http-stale"}),
        )
        .unwrap();
    let stale_path = view_http_path(&id, "customers", None, &dig_pairs);
    let (code, _, body) = common::op::raw(
        port,
        &session.request("GET", &format!("{stale_path}&context_id={context_id}"), ""),
    );
    assert_eq!(code, 400, "stale-digest bound read served: {code} {body}");

    // Agent-peered replay of the signed operator request: a session's
    // wire replayed under a planted pane's ancestry refuses exactly 403.
    // Re-read fresh digests so the replay isn't the stale one above.
    let fresh = view_read_digests(&w, &id);
    let fresh_pairs: Vec<(String, String)> = http_digests(&fresh);
    let fp: Vec<(&str, &str)> = fresh_pairs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let mut replay_params = fp.clone();
    replay_params.push(("context_id", context_id.as_str()));
    let replay_path = view_http_path(&id, "customers", None, &replay_params);
    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "view-http", "claude", None, lane.pid());
    for prefix in ["", "setsid "] {
        let stolen = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &w.daemon.state, port);
        let wire = stolen.request_as("GET", &replay_path, "", "");
        let request_file = lane
            .dir
            .path()
            .join(format!("viewread-http-{}.txt", lane.seq));
        std::fs::write(&request_file, wire).unwrap();
        let (rc, response) = lane.run(&format!("{prefix}python3 -c 'import socket,sys; s=socket.create_connection((\"127.0.0.1\",int(sys.argv[1])));s.sendall(open(sys.argv[2],\"rb\").read());print(s.makefile().readline())' {port} {}", request_file.display()));
        assert_eq!(rc, 0);
        assert_eq!(
            response.split_whitespace().nth(1),
            Some("403"),
            "agent-peered bound read reached rows: {response}"
        );
    }
}
