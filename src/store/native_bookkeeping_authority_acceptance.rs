// Independently authored CAD-1085 acceptance for migration-installed triggers.
use super::Store;
use rusqlite::{params, Connection};

fn permission_revision(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT authorization_revision FROM connection_metadata WHERE singleton=1",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn queued_rows(conn: &Connection, table: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM connection_authority_changes WHERE table_name=?1",
        [table],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn native_bookkeeping_is_journaled_without_invalidating_permission_snapshots() {
    let cases = [
        ("ordinary unselected store", 0_i64, 0_i64, false),
        ("protected selection before first enable", 0, 1, true),
        ("historically enabled but currently disabled", 1, 0, true),
    ];

    for (case, ever_enabled, protection_required, must_journal) in cases {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("trigger-acceptance.sqlite3");
        let store = Store::open(&db).unwrap();
        drop(store);
        // Store's connection-local raw-DML authorizer remains unchanged.
        // This fixture connection observes persistent migration-installed SQL
        // triggers, not the production writer/owner-ACK admission boundary.
        let conn = Connection::open(&db).unwrap();
        // These are fixture inputs to the real migration-installed triggers.
        // No trigger is replaced, disabled or synthesized by this acceptance.
        conn.execute(
            "UPDATE connection_dispatch_config SET enabled=0,ever_enabled=?1,protection_required=?2 WHERE singleton=1",
            params![ever_enabled, protection_required],
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO native_model_sources(
                source_id,workspace_id,provider,account,revision,origin,origin_kind,
                catalog_revision,model_uid_map,vault_reference,key_id,state,created_ms,updated_ms)
             VALUES('source-acceptance','workspace-acceptance','devin','account-acceptance',1,
                'https://example.com','operator_pinned',1,'{}','opaque-vault-reference',X'00','active',1,1);",
        )
        .unwrap();

        let snapshot_revision = permission_revision(&conn);
        let changes = [
            (
                "native_model_tasks",
                "INSERT INTO native_model_tasks(
                    durable_task_id,workspace_id,agent_uid,session_binding,client_message_id,
                    server_message_id,turn_token,state,created_ms)
                 VALUES('task-acceptance','workspace-acceptance','agent-acceptance','session-acceptance',
                    'client-acceptance','server-acceptance','turn-acceptance','active',1);",
                "UPDATE native_model_tasks SET state='fenced',fenced_ms=2 WHERE durable_task_id='task-acceptance';",
                "DELETE FROM native_model_tasks WHERE durable_task_id='task-acceptance';",
            ),
            (
                "native_model_configurations",
                "INSERT INTO native_model_configurations(
                    configuration_id,revision,workspace_id,agent_uid,session_binding,runtime_id,
                    runtime_generation,owner_epoch,role,model,model_uid,effort,provider,source_id,
                    source_revision,account,origin,destination,billing_route,sdk_graph_digest,
                    approved_tools_json,manifest_digest,snapshot_id,snapshot_expires_ms,
                    loaded_receipt_id,max_output_tokens,created_ms)
                 VALUES('configuration-acceptance',1,'workspace-acceptance','agent-acceptance',
                    'session-acceptance','runtime-acceptance',1,7,'worker','devin/model-acceptance',
                    'model-acceptance','high','devin','source-acceptance',1,'account-acceptance',
                    'https://example.com','example.com','subscription','graph-acceptance','[]',
                    'manifest-acceptance','snapshot-acceptance',60000,'receipt-acceptance',128,1);",
                "UPDATE native_model_configurations SET revision=2 WHERE configuration_id='configuration-acceptance';",
                "DELETE FROM native_model_configurations WHERE configuration_id='configuration-acceptance';",
            ),
            (
                "native_model_requests",
                "INSERT INTO native_model_requests(
                    request_id,workspace_id,agent_uid,durable_task_id,request_index,configuration_id,
                    configuration_revision,snapshot_id,loaded_receipt_id,source_id,account,state,
                    billing_json,created_ms)
                 VALUES('request-acceptance','workspace-acceptance','agent-acceptance','task-acceptance',0,
                    'configuration-acceptance',1,'snapshot-acceptance','receipt-acceptance',
                    'source-acceptance','account-acceptance','prepared','{}',1);",
                "UPDATE native_model_requests SET state='committed',input_digest='input-acceptance',committed_ms=2 WHERE request_id='request-acceptance';",
                "DELETE FROM native_model_requests WHERE request_id='request-acceptance';",
            ),
        ];

        for (table, insert, update, delete) in changes {
            for (event, sql) in [("insert", insert), ("update", update), ("delete", delete)] {
                let before = queued_rows(&conn, table);
                conn.execute_batch(sql).unwrap();
                let after = queued_rows(&conn, table);
                if must_journal {
                    assert!(
                        after > before,
                        "{case}: {table} {event} lost its journal entry"
                    );
                } else {
                    assert_eq!(
                        after, before,
                        "{case}: ordinary store unexpectedly journaled {table} {event}"
                    );
                }
                assert_eq!(
                    permission_revision(&conn),
                    snapshot_revision,
                    "{case}: derived {table} {event} invalidated its own permission snapshot",
                );
            }
        }

        let source_queue_before = queued_rows(&conn, "native_model_sources");
        conn.execute(
            "UPDATE native_model_sources SET state='locked' WHERE source_id='source-acceptance'",
            [],
        )
        .unwrap();
        let source_revision = permission_revision(&conn);
        if must_journal {
            assert!(
                queued_rows(&conn, "native_model_sources") > source_queue_before,
                "{case}: a genuine source change lost its journal entry",
            );
            assert!(
                source_revision > snapshot_revision,
                "{case}: genuine source revocation left stale permission authority current",
            );
        } else {
            assert_eq!(source_revision, snapshot_revision);
            assert_eq!(
                queued_rows(&conn, "native_model_sources"),
                source_queue_before
            );
        }

        if protection_required == 1 {
            assert!(
                conn.execute(
                    "UPDATE connection_dispatch_config SET protection_required=0 WHERE singleton=1",
                    []
                )
                .is_err(),
                "protected authority selection must remain monotone",
            );
        }
        if ever_enabled == 1 {
            assert!(
                conn.execute(
                    "UPDATE connection_dispatch_config SET ever_enabled=0 WHERE singleton=1",
                    []
                )
                .is_err(),
                "historical protected enablement must remain monotone",
            );
        }
    }
}
