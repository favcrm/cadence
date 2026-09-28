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
