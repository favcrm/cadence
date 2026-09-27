use super::*;

// This is a storage receipt fixture, not an accepted-run lifecycle proof.
// The native suite independently produces and reviews the actual artifact.
fn child_fixture(s: &Store) -> (crate::store::EffectRow, Value) {
    s.conn().execute_batch("ALTER TABLE platform_effects ADD COLUMN authorization_kind TEXT NOT NULL DEFAULT 'agent_grant';").unwrap();
    s.conn()
        .execute_batch(crate::store::app_effects::SCHEMA)
        .unwrap();
    let body = "server-owned body\n";
    let artifact_digest = crate::store::app_runs::artifact_digest(body.as_bytes());
    let provenance = json!({"install_id":"install-a","context_id":null,"run_id":"run-a",
        "artifact_id":"artifact-a","artifact_digest":artifact_digest,"binding_id":"binding-a",
        "binding_revision":1,"binding_digest":"binding-receipt","sink_registration":"sink-one"});
    let authority = json!({"schema":1,"install_id":"install-a","context":null,"run_id":"run-a",
        "artifact_id":"artifact-a","artifact_digest":artifact_digest,"provenance":provenance});
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
