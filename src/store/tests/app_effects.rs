use super::*;

#[test]
fn cad692_uncertain_outcome_and_resolution_preserve_exact_historical_authority() {
    let (_dir, s) = store();
    let (row, authority) = child_fixture(&s);
    let staged = s.app_effect_stage(&row, &authority).unwrap();
    assert_eq!(staged["effect"]["schema"], 1);
    assert_eq!(staged["effect"]["authorization_kind"], "app_artifact");
    assert_eq!(staged["effect"]["record"]["kind"], "app_artifact_effect");
    assert_eq!(staged["effect"]["needs_you"], false);
    let digest = staged["effect"]["digest"].as_str().unwrap();
    let outcome = json!({"kind":"uncertain","error":"directory sync failed","verified":true});
    assert!(s.app_effect_uncertain(&row.effect_id, &outcome).is_err());
    assert!(s
        .app_effect_resolve(&row.effect_id, digest, "close")
        .is_err());
    s.effect_decide(&row.request, true, &json!({"by":{"member":"operator","role":"operator","rule":"exact-app-artifact-release"},"at":"now"})).unwrap();
    s.app_effect_claim(&row.effect_id, digest, |_, _| Ok(true))
        .unwrap()
        .unwrap();
    let before = s.app_effect_show(&row.effect_id).unwrap();
    s.app_effect_uncertain(&row.effect_id, &outcome).unwrap();
    let uncertain = s.app_effect_show(&row.effect_id).unwrap();
    assert_eq!(uncertain["effect"]["state"], "reconcile");
    assert_eq!(uncertain["effect"]["needs_you"], true);
    assert_eq!(uncertain["effect"]["record"]["outcome"], outcome);
    assert!(s.effect_by_id(&row.effect_id).unwrap().unwrap().needs_you);
    assert!(s
        .app_effect_claim(&row.effect_id, digest, |_, _| Ok(true))
        .unwrap()
        .is_none());
    assert!(s.app_effect_uncertain(&row.effect_id, &outcome).is_err());
    assert!(s
        .app_effect_resolve(&row.effect_id, "forged", "close")
        .is_err());
    assert!(s
        .app_effect_resolve(&row.effect_id, digest, "acknowledge")
        .is_err());
    assert!(s
        .app_effect_resolve(&row.effect_id, digest, "retry")
        .is_err());
    assert_eq!(s.app_effect_show(&row.effect_id).unwrap(), uncertain);
    let closed = s
        .app_effect_resolve(&row.effect_id, digest, "close")
        .unwrap();
    assert_eq!(closed["effect"]["state"], "closed");
    assert_eq!(
        closed["effect"]["record"]["close_reason"],
        "operator_reconciled"
    );
    assert_eq!(closed["effect"]["record"]["outcome"], outcome);
    for field in ["authority", "digest", "request"] {
        assert_eq!(closed["effect"][field], before["effect"][field]);
    }
    for field in ["input", "preview", "decision", "source_hash"] {
        assert_eq!(
            closed["effect"]["record"][field],
            before["effect"]["record"][field]
        );
    }
    assert!(!s.effect_by_id(&row.effect_id).unwrap().unwrap().needs_you);
    assert!(s
        .app_effect_resolve(&row.effect_id, digest, "close")
        .is_err());
}

#[test]
fn cad692_terminal_acknowledgement_is_digest_pinned_and_never_erases_outcome() {
    let (_dir, s) = store();
    let (row, authority) = child_fixture(&s);
    let staged = s.app_effect_stage(&row, &authority).unwrap();
    let digest = staged["effect"]["digest"].as_str().unwrap();
    s.effect_decide(&row.request, true, &json!({"by":{"member":"operator","role":"operator","rule":"exact-app-artifact-release"},"at":"now"})).unwrap();
    s.app_effect_claim(&row.effect_id, digest, |_, _| Ok(true))
        .unwrap()
        .unwrap();
    let outcome = json!({"kind":"refused","error":"known refusal","verified":false});
    s.effect_outcome(&row.effect_id, false, &outcome, "known refusal")
        .unwrap();
    let before = s.app_effect_show(&row.effect_id).unwrap();
    assert!(s
        .app_effect_resolve(&row.effect_id, "forged", "acknowledge")
        .is_err());
    assert!(s
        .app_effect_resolve(&row.effect_id, digest, "close")
        .is_err());
    let acknowledged = s
        .app_effect_resolve(&row.effect_id, digest, "acknowledge")
        .unwrap();
    assert_eq!(acknowledged["effect"]["state"], "failed");
    assert_eq!(
        acknowledged["effect"]["authority"],
        before["effect"]["authority"]
    );
    assert_eq!(acknowledged["effect"]["record"], before["effect"]["record"]);
    assert_eq!(acknowledged["effect"]["digest"], before["effect"]["digest"]);
    assert!(!s.effect_by_id(&row.effect_id).unwrap().unwrap().needs_you);
    assert!(s
        .app_effect_resolve(&row.effect_id, digest, "acknowledge")
        .is_err());
}

// This is a storage receipt fixture, not an accepted-run lifecycle proof.
// The native suite independently produces and reviews the actual artifact.
fn child_fixture(s: &Store) -> (crate::store::EffectRow, Value) {
    let present = s
        .conn()
        .prepare("PRAGMA table_info(platform_effects)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .any(|column| column.unwrap() == "authorization_kind");
    if !present {
        s.conn().execute_batch("ALTER TABLE platform_effects ADD COLUMN authorization_kind TEXT NOT NULL DEFAULT 'agent_grant';").unwrap();
    }
    s.conn()
        .execute_batch(crate::store::app_effects::SCHEMA)
        .unwrap();
    let body = "server-owned body\n";
    let artifact_digest = crate::store::app_runs::artifact_digest(body.as_bytes());
    let mut provenance = json!({"install_id":"install-a","context_id":null,"run_id":"run-a",
        "artifact_id":"artifact-a","artifact_digest":artifact_digest,"binding_id":"binding-a",
        "binding_revision":1,"binding_digest":"binding-receipt","sink_registration":"sink-one"});
    let mut authority = json!({"schema":1,"install_id":"install-a","context":null,"run_id":"run-a",
        "artifact_id":"artifact-a","artifact_digest":artifact_digest,"provenance":provenance});
    provenance["effect_id"] = json!("effect-child");
    provenance["authorization_kind"] = json!("app_artifact");
    provenance["authority_digest"] = json!(crate::store::app_effects::authority_digest(&authority));
    authority["provenance"] = provenance.clone();
    let row = crate::store::EffectRow {
        effect_id: "effect-child".into(),
        request: "app-release-request".into(),
        agent: "routing-pm".into(),
        platform: "local".into(),
        account: "local".into(),
        tool: "publish_app_text".into(),
        label: None,
        input: json!({"schema":1,"title":"Title","body":body,"provenance":provenance}),
        input_summary: "Title".into(),
        preview: body.into(),
        source_name: None,
        source_hash: Some(artifact_digest),
        scopes: vec!["publish".into()],
        task: None,
        state: "waiting".into(),
        close_reason: None,
        decision: None,
        outcome: None,
        needs_you: false,
        staged_at: 0.0,
        updated_at: 0.0,
    };
    (row, authority)
}

#[test]
fn cad692_app_child_claim_is_exact_cas_and_readonly_permit_requires_integrity() {
    let (dir, s) = store();
    let (row, authority) = child_fixture(&s);
    for field in ["effect_id", "authority_digest", "authorization_kind"] {
        let mut forged = row.clone();
        let mut forged_authority = authority.clone();
        forged.input["provenance"][field] = json!("forged");
        forged_authority["provenance"] = forged.input["provenance"].clone();
        assert!(
            s.app_effect_stage(&forged, &forged_authority).is_err(),
            "{field}"
        );
    }
    let first = s.app_effect_stage(&row, &authority).unwrap();
    let digest = first["effect"]["digest"].as_str().unwrap();
    assert_eq!(s.app_effect_stage(&row, &authority).unwrap(), first);
    let mut changed = authority.clone();
    changed["artifact_id"] = json!("artifact-other");
    assert!(s.app_effect_stage(&row, &changed).is_err());
    let db = dir.path().join("t.sqlite3");
    assert!(crate::store::app_effects::read_execution_permit(&db, &row.effect_id).is_err());
    assert!(s
        .app_effect_claim(&row.effect_id, digest, |_, _| Ok(true))
        .unwrap()
        .is_none());
    s.effect_decide(
        &row.request,
        true,
        &json!({"by":{"member":"operator","role":"operator","rule":"operator"},"at":"now"}),
    )
    .unwrap();
    assert!(s
        .app_effect_claim(&row.effect_id, digest, |_, _| Ok(false))
        .unwrap()
        .is_none());
    assert_eq!(
        s.effect_by_id(&row.effect_id).unwrap().unwrap().state,
        "decided"
    );
    assert!(s
        .app_effect_claim(&row.effect_id, "forged", |_, _| Ok(true))
        .is_err());
    assert!(s
        .app_effect_claim(&row.effect_id, digest, |_, _| Ok(true))
        .unwrap()
        .is_some());
    assert!(s
        .app_effect_claim(&row.effect_id, digest, |_, _| Ok(true))
        .unwrap()
        .is_none());
    let permit = crate::store::app_effects::read_execution_permit(&db, &row.effect_id).unwrap();
    assert_eq!(permit.input, row.input);
    assert_eq!(permit.provenance, authority["provenance"]);
    assert_eq!(
        permit.authority_digest,
        authority["provenance"]["authority_digest"]
    );
    assert_eq!(permit.platform, "local");
    assert_eq!(permit.account, "local");
    assert_eq!(permit.tool, "publish_app_text");
    s.conn()
        .execute(
            "UPDATE platform_effects SET input='{}' WHERE effect_id=?",
            [&row.effect_id],
        )
        .unwrap();
    assert!(crate::store::app_effects::read_execution_permit(&db, &row.effect_id).is_err());
}

#[test]
fn cad692_legacy_or_missing_child_cannot_supply_app_execution_permit() {
    let (dir, s) = store();
    let (row, authority) = child_fixture(&s);
    s.app_effect_stage(&row, &authority).unwrap();
    s.conn().execute("UPDATE platform_effects SET state='executing',authorization_kind='agent_grant' WHERE effect_id=?",[&row.effect_id]).unwrap();
    let db = dir.path().join("t.sqlite3");
    assert!(crate::store::app_effects::read_execution_permit(&db, &row.effect_id).is_err());
    s.conn()
        .execute(
            "UPDATE platform_effects SET authorization_kind='app_artifact' WHERE effect_id=?",
            [&row.effect_id],
        )
        .unwrap();
    s.conn()
        .execute(
            "DELETE FROM app_effect_authorizations WHERE effect_id=?",
            [&row.effect_id],
        )
        .unwrap();
    assert!(crate::store::app_effects::read_execution_permit(&db, &row.effect_id).is_err());
}

#[test]
fn cad692_legacy_waiting_projection_excludes_child_with_populated_legacy_control() {
    let (_dir, s) = store();
    let (row, authority) = child_fixture(&s);
    let mut legacy = row.clone();
    legacy.effect_id = "legacy-waiting".into();
    legacy.request = "legacy-request".into();
    legacy.tool = "publish".into();
    legacy.input = json!({"project":"legacy-project","source":"draft.md"});
    s.effect_stage(&legacy).unwrap();
    s.app_effect_stage(&row, &authority).unwrap();
    let projected = s.waiting_effects().unwrap();
    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0].effect_id, legacy.effect_id);
    assert_eq!(projected[0].input, legacy.input);
    assert_eq!(s.platform_effects(None).unwrap().len(), 1);
    assert_eq!(
        s.app_effect_show(&row.effect_id).unwrap()["effect"]["state"],
        "waiting"
    );
}

#[test]
fn cad692_actual_v22_migration_is_atomic_and_preserves_context_and_legacy_receipts() {
    let (dir, s) = store();
    let (mut legacy, _) = child_fixture(&s);
    legacy.tool = "publish".into();
    legacy.input = json!({"project":"legacy-project","source":"draft.md"});
    s.effect_stage(&legacy).unwrap();
    let ctx = s
        .app_context_create(
            "install-a",
            &crate::store::app_contexts::ContextConfig::new(
                "Existing",
                std::collections::BTreeMap::new(),
            )
            .unwrap(),
            "existing-context",
        )
        .unwrap();
    let workspace = s.connection_workspace_id().unwrap();
    let before = s
        .effect_by_id(&legacy.effect_id)
        .unwrap()
        .unwrap()
        .to_record();
    drop(s);
    let db = dir.path().join("t.sqlite3");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch("DROP TABLE app_effect_authorizations; DROP TABLE IF EXISTS app_bindings; ALTER TABLE platform_effects DROP COLUMN authorization_kind; UPDATE schema_version SET version=22;").unwrap();
    conn.execute_batch("CREATE TRIGGER fail_release_migration BEFORE UPDATE ON schema_version BEGIN SELECT RAISE(ABORT,'forced release migration failure'); END;").unwrap();
    drop(conn);
    assert!(Store::open_for_schema_tests(&db).is_err());
    let conn = Connection::open(&db).unwrap();
    assert_eq!(
        conn.query_row("SELECT version FROM schema_version", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        22
    );
    assert_eq!(conn.query_row("SELECT count(*) FROM sqlite_master WHERE name IN ('app_bindings','app_effect_authorizations')",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    assert!(!conn
        .prepare("PRAGMA table_info(platform_effects)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .any(|c| c.unwrap() == "authorization_kind"));
    conn.execute_batch("DROP TRIGGER fail_release_migration;")
        .unwrap();
    drop(conn);
    let reopened = Store::open_for_schema_tests(&db).unwrap();
    assert_eq!(
        reopened
            .conn()
            .query_row("SELECT version FROM schema_version", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        crate::rollout::SCHEMA_VERSION
    );
    assert_eq!(reopened.connection_workspace_id().unwrap(), workspace);
    assert_eq!(
        reopened
            .app_context_show("install-a", ctx["context"]["id"].as_str().unwrap())
            .unwrap(),
        ctx
    );
    assert_eq!(
        reopened
            .effect_by_id(&legacy.effect_id)
            .unwrap()
            .unwrap()
            .to_record(),
        before
    );
    assert_eq!(
        reopened
            .conn()
            .query_row(
                "SELECT authorization_kind FROM platform_effects WHERE effect_id=?",
                [&legacy.effect_id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "agent_grant"
    );
}
