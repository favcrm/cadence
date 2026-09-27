use super::app_runs::runtime_fixture;
use super::*;

#[test]
fn cad690_required_context_content_default_is_nonempty_but_optional_may_be_empty() {
    let text = include_str!("../../../apps/local-content/workflows/draft.md")
        .replace("source: { ask:", "source: { context_default: true, ask:");
    let defaults = std::collections::BTreeMap::from([("source".to_string(), "facts".to_string())]);
    assert!(crate::issue::workflow::check_context_defaults(&text, &defaults).is_ok());
    let empty = std::collections::BTreeMap::from([("source".to_string(), String::new())]);
    assert!(crate::issue::workflow::check_context_defaults(&text, &empty).is_err());
    let optional = text.replace(
        "context_default: true",
        "context_default: true, optional: true",
    );
    assert!(crate::issue::workflow::check_context_defaults(&optional, &empty).is_ok());
}

#[test]
fn cad690_context_receipt_is_rechecked_in_create_and_execution_transactions() {
    use crate::store::app_contexts::ContextConfig;
    use crate::store::app_runs::{LocalRunRequest, LocalWorkflow};
    let (_dir, s, legacy) = runtime_fixture();
    let config = ContextConfig::new("Client", std::collections::BTreeMap::new()).unwrap();
    let context = s
        .app_context_create("install-1", &config, "context-1")
        .unwrap();
    let id = context["context"]["id"].as_str().unwrap();
    let (_, proof) = s.app_context_proof("install-1", id).unwrap();
    let workflow: LocalWorkflow =
        serde_json::from_value(legacy["snapshot"]["workflow"].clone()).unwrap();
    let inputs = std::collections::BTreeMap::new();
    let request = |key: &'static str| LocalRunRequest {
        install_id: "install-1",
        bundle_digest: "sha256:bundle",
        workflow: &workflow,
        inputs: &inputs,
        request_id: key,
        owner_pm: "lead",
        project_link: None,
    };
    let contextual = s
        .app_run_create_with_context(request("contextual"), Some(&proof))
        .unwrap();
    assert_eq!(contextual["snapshot"]["context"]["id"], id);
    s.app_run_decide(
        contextual["id"].as_str().unwrap(),
        contextual["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    let pending = s
        .app_run_create_with_context(request("pending"), Some(&proof))
        .unwrap();
    // Corrupt only the revision fixture, deliberately bypassing proactive
    // invalidation, so this assertion isolates the current-receipt gate.
    s.conn()
        .execute(
            "UPDATE app_contexts SET revision=revision+1 WHERE id=?",
            [id],
        )
        .unwrap();
    assert!(
        s.app_run_create_with_context(request("stale-create"), Some(&proof))
            .is_err(),
        "stale context proof reached a run commit"
    );
    assert!(
        s.app_run_decide(
            pending["id"].as_str().unwrap(),
            pending["snapshot_digest"].as_str(),
            false,
            Some("sha256:bundle")
        )
        .is_err(),
        "stale context receipt reached execution approval"
    );
    assert_eq!(
        s.app_run_show(pending["id"].as_str().unwrap()).unwrap()["state"],
        "awaiting_approval"
    );
    assert_eq!(
        s.app_run_show(legacy["id"].as_str().unwrap()).unwrap()["snapshot"],
        legacy["snapshot"]
    );
}

#[test]
fn cad690_context_migration_is_atomic_and_preserves_legacy_snapshots_and_authority() {
    let (dir, s, run) = runtime_fixture();
    s.app_run_decide(
        run["id"].as_str().unwrap(),
        run["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    let before = s.app_run_show(run["id"].as_str().unwrap()).unwrap();
    s.conn().execute_batch("INSERT INTO platform_grants(agent,platform,account,scopes,granted_at,by) VALUES('writer','legacy','account','[\"read\"]',1,'operator'); INSERT INTO platform_effects(effect_id,request,agent,platform,account,tool,input,input_summary,preview,scopes,state,staged_at,updated_at) VALUES('legacy-effect','legacy-request','writer','legacy','account','publish','{}','legacy summary','legacy preview','[\"publish\"]','pending',1,1);").unwrap();
    s.conn().execute_batch("INSERT INTO platform_credentials(platform,account,scopes,fingerprint,custody,exchange,enrolled_at,by,connection_id,credential_revision) VALUES('legacy','account','[\"read\"]','fingerprint','file','token',1,'operator','conn-legacy',3);").unwrap();
    let workspace: String = s
        .conn()
        .query_row(
            "SELECT workspace_id FROM connection_metadata WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(s);
    let db = dir.path().join("t.sqlite3");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch("DROP INDEX app_runs_context; ALTER TABLE app_runs DROP COLUMN context_id; DROP TABLE app_contexts;").unwrap();
    // This fixture retains the authentic v21 connection objects while removing
    // only this increment's additive objects. It must follow merged CAD688.
    let previous = 21;
    conn.execute("UPDATE schema_version SET version=?", [previous])
        .unwrap();
    conn.execute_batch("CREATE TRIGGER fail_context_migration BEFORE UPDATE ON schema_version BEGIN SELECT RAISE(ABORT,'forced context migration failure'); END;").unwrap();
    drop(conn);
    assert!(Store::open_for_schema_tests(&db).is_err());
    let conn = Connection::open(&db).unwrap();
    assert_eq!(
        conn.query_row("SELECT version FROM schema_version", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        previous
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name IN ('app_contexts','app_runs_context')",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.prepare("PRAGMA table_info(app_runs)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(|value| value.unwrap())
            .filter(|name| name == "context_id")
            .count(),
        0
    );
    conn.execute_batch("DROP TRIGGER fail_context_migration;")
        .unwrap();
    drop(conn);
    let migrated = Store::open_for_schema_tests(&db).unwrap();
    assert_eq!(
        migrated
            .conn()
            .query_row(
                "SELECT workspace_id FROM connection_metadata WHERE singleton=1",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        workspace
    );
    assert_eq!(migrated.conn().query_row("SELECT connection_id,credential_revision FROM platform_credentials WHERE platform='legacy' AND account='account'",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?))).unwrap(),("conn-legacy".to_string(),3));
    assert_eq!(
        migrated.app_run_show(run["id"].as_str().unwrap()).unwrap(),
        before
    );
    for table in ["platform_grants", "platform_effects"] {
        assert_eq!(
            migrated
                .conn()
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    assert!(migrated.app_context_list("install-1").unwrap()["contexts"]
        .as_array()
        .unwrap()
        .is_empty());
    drop(migrated);
    let reopened = Store::open(&db).unwrap();
    assert_eq!(
        reopened.app_run_show(run["id"].as_str().unwrap()).unwrap(),
        before
    );
}
