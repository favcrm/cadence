use super::*;

#[test]
fn cad631_explicit_local_schema_refuses_unsupported_execution_and_self_review() {
    use crate::store::app_runs::LocalWorkflow;
    let workflow = "---\ntitle: Local\ngoal: Reviewed text\n---\n## Write\nagent: writer\naction: local.text.produce\n\nWrite Markdown.\n\n### Acceptance\n- [ ] Markdown artifact exists\n\n## Review\nagent: reviewer\ndepends_on: 1\naction: local.text.review\n\nReview the exact artifact.\n\n### Acceptance\n- [ ] Exact artifact reviewed\n";
    let inputs = std::collections::BTreeMap::new();
    assert!(LocalWorkflow::parse(workflow, &inputs).is_ok());
    for changed in [
        workflow.replace("local.text.produce", "local.publish"),
        workflow.replace(
            "action: local.text.produce",
            "action: local.text.produce\ntries: 2",
        ),
        workflow.replace("agent: reviewer", "agent: writer"),
        workflow.replace("depends_on: 1", "depends_on: CAD-1"),
    ] {
        assert!(LocalWorkflow::parse(&changed, &inputs).is_err());
    }
}

/// CAD-1171: a host-execution capability run needs no agent at all — the
/// operator's own click executes it — while an agent run still refuses a
/// missing owner, and a capability step outside a host workflow refuses.
#[test]
fn cad1171_host_capability_run_needs_no_agents() {
    use crate::store::app_runs::{LocalRunRequest, LocalWorkflow};
    let (_dir, s) = store();
    let host_text = "---\ntitle: Read\ngoal: Retain a bounded provider receipt\nlabel: Find source\ncapability_slots: [source]\nexecution: host\ninputs:\n  profile_handle: { ask: Handle }\n---\n## Acquire: {{profile_handle}}\nsize: S\naction: local.capability.call\n\nCall the frozen source binding.\n\n### Acceptance\n- [ ] receipt retained\n";
    let mut inputs = std::collections::BTreeMap::new();
    inputs.insert("profile_handle".into(), "juicysuite_crm".into());
    let workflow = LocalWorkflow::parse(host_text, &inputs).unwrap();
    assert_eq!(workflow.execution, "host");
    assert_eq!(workflow.steps.len(), 1);
    assert_eq!(workflow.steps[0].kind, "capability");
    assert_eq!(workflow.steps[0].assignee, "");
    // A host workflow may not carry an agent step, a capability step may
    // not appear outside a host workflow, and host needs a capability slot.
    for refused in [
        host_text.replace(
            "action: local.capability.call",
            "agent: writer\naction: local.capability.call",
        ),
        host_text.replace("execution: host\n", ""),
        host_text.replace("capability_slots: [source]\n", ""),
    ] {
        assert!(
            LocalWorkflow::parse(&refused, &inputs).is_err(),
            "{refused}"
        );
    }
    // An agent run still needs its owner PM.
    let agent_text = "---\ntitle: Local\ngoal: Reviewed text\n---\n## Write\nagent: writer\naction: local.text.produce\n\nWrite Markdown.\n\n### Acceptance\n- [ ] Markdown artifact exists\n";
    let no_inputs = std::collections::BTreeMap::new();
    let agent = LocalWorkflow::parse(agent_text, &no_inputs).unwrap();
    s.app_capability_decide("install-1", "sha256:bundle", true)
        .unwrap();
    // The relaxation is host-only: an agent run still needs its owner PM.
    assert!(s
        .app_run_create(LocalRunRequest {
            install_id: "install-1",
            bundle_digest: "sha256:bundle",
            workflow: &agent,
            inputs: &no_inputs,
            request_id: "agent-without-owner",
            owner_pm: None,
            project_link: None,
        })
        .is_err());
    // A host run still freezes its bound, priced capability: without the
    // live binding proof the run is refused, owner or not.
    assert!(s
        .app_run_create(LocalRunRequest {
            install_id: "install-1",
            bundle_digest: "sha256:bundle",
            workflow: &workflow,
            inputs: &inputs,
            request_id: "host-request-1",
            owner_pm: None,
            project_link: None,
        })
        .is_err());
}

#[test]
fn cad631_material_digests_are_canonical_and_never_git_shas() {
    use crate::store::app_runs::{artifact_digest, material_digest};
    assert_eq!(
        material_digest(&json!({"b":2,"a":1})),
        material_digest(&json!({"a":1,"b":2}))
    );
    assert_ne!(
        material_digest(&json!({"a":1})),
        material_digest(&json!({"a":2}))
    );
    assert_eq!(
        artifact_digest(b"hello"),
        "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
}

pub(super) fn runtime_fixture() -> (TempDir, Store, Value) {
    let (dir, s) = store();
    for (alias, role) in [("lead", "pm"), ("writer", "worker"), ("reviewer", "worker")] {
        s.register_agent(&NewAgent {
            alias,
            provider: "claude",
            endpoint_kind: "managed",
            role,
            cwd: dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: Some("{\"upstream\":\"lead\"}"),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
        s.set_identity(alias, &endpoint_at(4242)).unwrap();
    }
    let text="---\ntitle: Local\ngoal: Reviewed text\n---\n## Write\nagent: writer\naction: local.text.produce\n\nWrite Markdown.\n\n### Acceptance\n- [ ] Markdown artifact exists\n\n## Review\nagent: reviewer\ndepends_on: 1\naction: local.text.review\n\nReview the exact artifact.\n\n### Acceptance\n- [ ] Exact artifact reviewed\n";
    let inputs = std::collections::BTreeMap::new();
    let workflow = crate::store::app_runs::LocalWorkflow::parse(text, &inputs).unwrap();
    s.app_capability_decide("install-1", "sha256:bundle", true)
        .unwrap();
    let run = s
        .app_run_create(crate::store::app_runs::LocalRunRequest {
            install_id: "install-1",
            bundle_digest: "sha256:bundle",
            workflow: &workflow,
            inputs: &inputs,
            request_id: "request-1",
            owner_pm: Some("lead"),
            project_link: None,
        })
        .unwrap();
    (dir, s, run)
}

#[test]
fn cad631_generic_dispatch_and_reopen_cannot_escape_app_approval() {
    let (_dir, s, run) = runtime_fixture();
    let task = run["steps"][0]["task_id"].as_str().unwrap();
    assert!(
        s.dispatch_task(task, None, None, "operator").is_err(),
        "generic dispatch bypassed run approval"
    );
    assert!(
        s.reopen_task(task, "operator").is_err(),
        "generic reopen erased immutable app association"
    );
    assert!(
        s.create_task(
            run["id"].as_str().unwrap(),
            "forged-task",
            None,
            Some("writer"),
            None,
            None,
            None,
            None,
            None
        )
        .is_err(),
        "generic task insertion escaped frozen graph"
    );
}

#[test]
fn cad631_execution_approval_is_separate_and_create_is_idempotent() {
    let (_dir, s, run) = runtime_fixture();
    let id = run["id"].as_str().unwrap();
    assert!(s.app_run_dispatch(id, "sha256:bundle").is_err());
    assert!(s
        .app_run_decide(id, Some("forged"), false, Some("sha256:bundle"))
        .is_err());
    s.app_run_decide(
        id,
        run["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    assert!(s
        .messages_for_task(run["steps"][0]["task_id"].as_str().unwrap())
        .unwrap()
        .is_empty());
    let first = s.app_run_dispatch(id, "sha256:bundle").unwrap();
    let second = s.app_run_dispatch(id, "sha256:bundle").unwrap();
    assert_eq!(
        first["steps"][0]["message_id"],
        second["steps"][0]["message_id"]
    );
    assert_eq!(
        s.messages_for_task(run["steps"][0]["task_id"].as_str().unwrap())
            .unwrap()
            .len(),
        1
    );
    s.app_capability_decide("install-1", "sha256:bundle", false)
        .unwrap();
    assert!(s.app_run_dispatch(id, "sha256:bundle").is_err());
}

#[test]
fn cad631_revoke_after_enqueue_prevents_claim_and_material_completion() {
    let (_dir, s, run) = runtime_fixture();
    let id = run["id"].as_str().unwrap();
    s.app_run_decide(
        id,
        run["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    let dispatched = s.app_run_dispatch(id, "sha256:bundle").unwrap();
    let message = dispatched["steps"][0]["message_id"].as_str().unwrap();
    s.app_capability_decide("install-1", "sha256:bundle", false)
        .unwrap();
    assert!(
        s.take_queued("writer").is_err(),
        "generic claim accepted an app kickoff without filesystem proof"
    );
    assert_eq!(s.message(message).unwrap().unwrap().state, "queued");
}

fn start_local_step(s: &Store, run: &Value, step: usize, alias: &str) -> Message {
    let message = run["steps"][step]["message_id"].as_str().unwrap();
    let Take::Message(taken) = s
        .take_queued_app_proven(alias, Some((message, "sha256:bundle")))
        .unwrap()
    else {
        panic!("app kickoff must be claimed")
    };
    let token = crate::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint("g1");
    s.mark_running(&taken.id, &token).unwrap();
    s.message(&taken.id).unwrap().unwrap()
}

#[test]
fn cad632_capability_result_is_bounded_durable_and_bound_to_active_turn() {
    let (dir, s, run) = runtime_fixture();
    let id = run["id"].as_str().unwrap();
    s.app_run_decide(
        id,
        run["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    let dispatched = s.app_run_dispatch(id, "sha256:bundle").unwrap();
    let writer = start_local_step(&s, &dispatched, 0, "writer");
    let turn = writer.turn_id.as_deref().unwrap();
    let input_digest = crate::store::app_runs::material_digest(&json!({"handle":"client_a"}));
    let result = json!({"posts":[{"id":"p1","caption":"First\npost"}]});
    s.app_capability_claim(super::super::app_capabilities::AppCapabilityClaim {
        run: id,
        step: "s1",
        message: &writer.id,
        turn,
        slot: "source",
        request: "req-1",
        binding_digest: "binding-a",
        input_digest: &input_digest,
        call_id: "call-1",
    })
    .unwrap();
    assert!(
        s.app_capability_claim(super::super::app_capabilities::AppCapabilityClaim {
            run: id,
            step: "s1",
            message: &writer.id,
            turn,
            slot: "source",
            request: "req-2",
            binding_digest: "binding-a",
            input_digest: &input_digest,
            call_id: "call-2",
        })
        .is_err(),
        "an unrecorded provider outcome must still reserve the approved slot"
    );
    assert!(
        s.app_capability_claim(super::super::app_capabilities::AppCapabilityClaim {
            run: id,
            step: "s1",
            message: &writer.id,
            turn,
            slot: "source",
            request: "req-1",
            binding_digest: "binding-a",
            input_digest: "forged",
            call_id: "call-1",
        })
        .is_err(),
        "an interrupted call cannot change its payload"
    );
    s.app_capability_claim(super::super::app_capabilities::AppCapabilityClaim {
        run: id,
        step: "s1",
        message: &writer.id,
        turn,
        slot: "source",
        request: "req-1",
        binding_digest: "binding-a",
        input_digest: &input_digest,
        call_id: "call-1",
    })
    .unwrap();
    let first = s
        .app_capability_record(super::super::app_capabilities::AppCapabilityRecord {
            id: "call-1",
            run: id,
            step: "s1",
            message: &writer.id,
            turn,
            slot: "source",
            request: "req-1",
            binding_digest: "binding-a",
            input_digest: &input_digest,
            result: &result,
            asset: None,
        })
        .unwrap();
    assert_eq!(first["result"], result);
    assert!(
        first.get("turn_id").is_none(),
        "active turn token leaked in receipt"
    );
    assert_eq!(
        s.app_capability_results(id).unwrap()["results"][0]["id"],
        "call-1"
    );
    assert_eq!(
        s.app_capability_record(super::super::app_capabilities::AppCapabilityRecord {
            id: "call-1",
            run: id,
            step: "s1",
            message: &writer.id,
            turn,
            slot: "source",
            request: "req-1",
            binding_digest: "binding-a",
            input_digest: &input_digest,
            result: &result,
            asset: None,
        })
        .unwrap(),
        first
    );
    assert!(s
        .app_capability_record(super::super::app_capabilities::AppCapabilityRecord {
            id: "call-2",
            run: id,
            step: "s1",
            message: &writer.id,
            turn,
            slot: "source",
            request: "req-1",
            binding_digest: "binding-a",
            input_digest: "different",
            result: &result,
            asset: None,
        })
        .is_err());
    assert!(
        s.app_capability_record(super::super::app_capabilities::AppCapabilityRecord {
            id: "call-3",
            run: id,
            step: "s1",
            message: &writer.id,
            turn,
            slot: "source",
            request: "req-2",
            binding_digest: "binding-a",
            input_digest: &input_digest,
            result: &result,
            asset: None,
        })
        .is_err(),
        "one approved slot must not permit a second charge"
    );
    s.app_run_decide(id, None, true, None).unwrap();
    let restarted = Store::open(&dir.path().join("t.sqlite3")).unwrap();
    assert_eq!(
        restarted
            .conn()
            .query_row(
                "SELECT call_id FROM app_capability_claims WHERE run_id=? AND slot='source'",
                [id],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "call-1",
        "durable slot claim must survive daemon restart"
    );
    assert!(s
        .app_capability_record(super::super::app_capabilities::AppCapabilityRecord {
            id: "call-3",
            run: id,
            step: "s1",
            message: &writer.id,
            turn,
            slot: "source",
            request: "req-2",
            binding_digest: "binding-a",
            input_digest: &input_digest,
            result: &result,
            asset: None,
        })
        .is_err());
}
fn producer_result(run: &str, message: &Message) -> Value {
    json!({"turn_id":message.turn_id,"text":json!({"schema":1,"kind":"produce_text","run_id":run,"step_id":"s1","revision":1,"outcome":"succeeded","artifacts":[{"media_type":"text/markdown","text":"# Local draft\nBased on the supplied source."}]}).to_string()})
}
#[test]
fn cad631_authenticated_text_and_independent_digest_review_complete_without_project() {
    let (dir, s, run) = runtime_fixture();
    let id = run["id"].as_str().unwrap();
    s.app_run_decide(
        id,
        run["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    let dispatched = s.app_run_dispatch(id, "sha256:bundle").unwrap();
    let writer = start_local_step(&s, &dispatched, 0, "writer");
    s.finish(&writer, "completed", &producer_result(id, &writer), None)
        .unwrap();
    let next = s.app_run_dispatch(id, "sha256:bundle").unwrap();
    assert_eq!(next["steps"][0]["state"], "succeeded");
    let reviewer = start_local_step(&s, &next, 1, "reviewer");
    let artifact = next["artifacts"][0]["id"].as_str().unwrap();
    let fetched = s
        .app_artifact_with_digest(
            artifact,
            Some((&reviewer.id, reviewer.turn_id.as_deref().unwrap())),
            "sha256:bundle",
        )
        .unwrap();
    assert_eq!(
        fetched["digest"],
        crate::store::app_runs::artifact_digest(fetched["text"].as_str().unwrap().as_bytes())
    );
    let response = json!({"schema":1,"kind":"review_text","run_id":id,"step_id":"s2","revision":1,"producer_step_id":"s1","producer_revision":1,"artifact_sha256":fetched["digest"],"decision":"approve","rationale":"Checked the exact supplied-source artifact."});
    s.finish(
        &reviewer,
        "completed",
        &json!({"turn_id":reviewer.turn_id,"text":response.to_string()}),
        None,
    )
    .unwrap();
    let final_run = s.app_run_show(id).unwrap();
    assert_eq!(final_run["state"], "succeeded");
    assert!(final_run["project_link"].is_null());
    assert_eq!(
        final_run["reviews"][0]["artifact_digest"],
        fetched["digest"]
    );
    assert_eq!(
        s.messages_for_task(final_run["steps"][0]["task_id"].as_str().unwrap())
            .unwrap()
            .len(),
        1
    );
    let owned_id = id.to_owned();
    drop(s);
    let reopened = Store::open(&dir.path().join("t.sqlite3")).unwrap();
    assert_eq!(
        reopened.app_run_show(&owned_id).unwrap()["state"],
        "succeeded"
    );
}

#[test]
fn cad631_operator_reconcile_cannot_create_material() {
    let (_dir, s, run) = runtime_fixture();
    let id = run["id"].as_str().unwrap();
    s.app_run_decide(
        id,
        run["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    let dispatched = s.app_run_dispatch(id, "sha256:bundle").unwrap();
    let writer = start_local_step(&s, &dispatched, 0, "writer");
    s.fixture_write(|c| {
        c.execute(
            "UPDATE messages SET state='unknown' WHERE id=?",
            [&writer.id],
        )
        .map_err(Into::into)
    })
    .unwrap();
    s.reconcile(
        &writer.id,
        "completed",
        Some(&producer_result(id, &writer).to_string()),
        "operator",
        None,
    )
    .unwrap();
    let after = s.app_run_show(id).unwrap();
    assert!(after["artifacts"].as_array().unwrap().is_empty());
    assert_eq!(after["state"], "failed");
    assert!(s.app_run_dispatch(id, "sha256:bundle").is_err());
    assert!(s
        .messages_for_task(after["steps"][1]["task_id"].as_str().unwrap())
        .unwrap()
        .is_empty());
}

#[test]
fn cad631_revoke_running_turn_refuses_material() {
    let (_dir, s, run) = runtime_fixture();
    let id = run["id"].as_str().unwrap();
    s.app_run_decide(
        id,
        run["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    let dispatched = s.app_run_dispatch(id, "sha256:bundle").unwrap();
    let writer = start_local_step(&s, &dispatched, 0, "writer");
    s.app_capability_decide("install-1", "sha256:bundle", false)
        .unwrap();
    s.finish(&writer, "completed", &producer_result(id, &writer), None)
        .unwrap();
    let after = s.app_run_show(id).unwrap();
    assert_eq!(after["state"], "failed");
    assert!(after["artifacts"].as_array().unwrap().is_empty());
}

#[test]
fn cad631_snapshot_identity_replacement_and_wrong_bundle_cannot_claim() {
    let (_dir, s, run) = runtime_fixture();
    let id = run["id"].as_str().unwrap();
    s.app_run_decide(
        id,
        run["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    let dispatched = s.app_run_dispatch(id, "sha256:bundle").unwrap();
    let message = dispatched["steps"][0]["message_id"].as_str().unwrap();
    assert!(s
        .take_queued_app_proven("writer", Some((message, "sha256:modified")))
        .is_err());
    s.fixture_write(|c| {
        c.execute(
            "UPDATE agents SET created=created+1 WHERE alias='writer'",
            [],
        )
        .map_err(Into::into)
    })
    .unwrap();
    assert!(s
        .take_queued_app_proven("writer", Some((message, "sha256:bundle")))
        .is_err());
    assert_eq!(s.message(message).unwrap().unwrap().state, "queued");
}

#[test]
fn cad631_concurrent_dispatch_mints_one_kickoff() {
    let (_dir, s, run) = runtime_fixture();
    let id = run["id"].as_str().unwrap().to_owned();
    s.app_run_decide(
        &id,
        run["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    let s = std::sync::Arc::new(s);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let handles = (0..2)
        .map(|_| {
            let s = s.clone();
            let barrier = barrier.clone();
            let id = id.clone();
            std::thread::spawn(move || {
                barrier.wait();
                s.app_run_dispatch(&id, "sha256:bundle").unwrap()
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let outputs = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        outputs[0]["steps"][0]["message_id"],
        outputs[1]["steps"][0]["message_id"]
    );
    assert_eq!(
        s.messages_for_task(run["steps"][0]["task_id"].as_str().unwrap())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn cad631_v19_additive_migration_is_atomic_and_preserves_legacy_queue() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("legacy.sqlite3");
    {
        let s = Store::open(&db).unwrap();
        reg(&s, "old-worker", dir.path());
        s.enqueue(
            "old-worker",
            "legacy pending",
            None,
            "legacy-message",
            "user",
        )
        .unwrap();
    }
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch("DROP TABLE app_run_reviews; DROP TABLE app_run_artifacts; DROP TABLE app_run_steps; DROP TABLE app_runs; DROP TABLE app_contexts; DROP TABLE app_install_capabilities; UPDATE schema_version SET version=19; CREATE TRIGGER fail_app_migration BEFORE UPDATE ON schema_version BEGIN SELECT RAISE(ABORT, 'forced app migration failure'); END;").unwrap();
    drop(conn);
    assert!(Store::open_for_schema_tests(&db)
        .err()
        .unwrap()
        .to_string()
        .contains("forced app migration failure"));
    let conn = Connection::open(&db).unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='app_runs'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row("SELECT version FROM schema_version", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        19
    );
    conn.execute_batch("DROP TRIGGER fail_app_migration")
        .unwrap();
    drop(conn);
    let migrated = Store::open_for_schema_tests(&db).unwrap();
    assert_eq!(
        migrated.message("legacy-message").unwrap().unwrap().body,
        "legacy pending"
    );
    assert_eq!(
        migrated.message("legacy-message").unwrap().unwrap().state,
        "queued"
    );
    assert!(migrated.app_run_list(None).unwrap()["runs"]
        .as_array()
        .unwrap()
        .is_empty());
    drop(migrated);
    assert!(Store::open(&db)
        .unwrap()
        .message("legacy-message")
        .unwrap()
        .is_some());
}

#[test]
fn cad631_app_turn_and_forged_task_cannot_borrow_account_union_for_effects() {
    let (_dir, s, run) = runtime_fixture();
    let id = run["id"].as_str().unwrap();
    s.app_run_decide(
        id,
        run["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    let dispatched = s.app_run_dispatch(id, "sha256:bundle").unwrap();
    let _writer = start_local_step(&s, &dispatched, 0, "writer");
    assert!(s.app_effect_guard("writer", None).is_err());
    let task = run["steps"][0]["task_id"].as_str().unwrap();
    assert!(s.app_effect_guard("unassigned", Some(task)).is_err());
    assert!(s.app_effect_guard("unassigned", None).is_ok());
}

#[test]
fn cad631_authority_loss_invalidates_once_and_keeps_material_receipts() {
    let (_dir, s, run) = runtime_fixture();
    let id = run["id"].as_str().unwrap();
    s.app_run_decide(
        id,
        run["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    s.app_run_dispatch(id, "sha256:bundle").unwrap();
    s.app_run_invalidate(id, "test authority loss").unwrap();
    s.app_run_invalidate(id, "test authority loss").unwrap();
    assert_eq!(s.app_run_show(id).unwrap()["state"], "failed");
    assert!(s.app_run_pending().unwrap().is_empty());
    assert!(s.app_run_dispatch(id, "sha256:bundle").is_err());
    let count: i64 = s
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE kind='app_run_invalidated'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn cad631_pm_in_owner_group_is_not_an_execution_worker() {
    let (_dir, s, run) = runtime_fixture();
    s.fixture_write(|c| {
        c.execute("UPDATE agents SET role='pm' WHERE alias='writer'", [])
            .map_err(Into::into)
    })
    .unwrap();
    let workflow: crate::store::app_runs::LocalWorkflow =
        serde_json::from_value(run["snapshot"]["workflow"].clone()).unwrap();
    let inputs = std::collections::BTreeMap::new();
    assert!(s
        .app_run_create(crate::store::app_runs::LocalRunRequest {
            install_id: "install-1",
            bundle_digest: "sha256:bundle",
            workflow: &workflow,
            inputs: &inputs,
            request_id: "pm-is-not-worker",
            owner_pm: Some("lead"),
            project_link: None,
        })
        .is_err());
}

#[test]
fn cad631_generic_job_reads_do_not_reveal_private_rendered_title() {
    let (_dir, s, run) = runtime_fixture();
    let mut workflow: crate::store::app_runs::LocalWorkflow =
        serde_json::from_value(run["snapshot"]["workflow"].clone()).unwrap();
    workflow.title = "Private subject sentinel CAD631".into();
    let inputs = std::collections::BTreeMap::new();
    let created = s
        .app_run_create(crate::store::app_runs::LocalRunRequest {
            install_id: "install-1",
            bundle_digest: "sha256:bundle",
            workflow: &workflow,
            inputs: &inputs,
            request_id: "private-title",
            owner_pm: Some("lead"),
            project_link: None,
        })
        .unwrap();
    let id = created["id"].as_str().unwrap();
    assert!(created.to_string().contains(&workflow.title));
    assert!(!format!("{:?}", s.job(id).unwrap()).contains(&workflow.title));
    assert!(!format!("{:?}", s.jobs(None, true).unwrap()).contains(&workflow.title));
    assert!(!format!("{:?}", s.tasks_for_job(id).unwrap()).contains(&workflow.title));
}

#[test]
fn cad631_public_app_message_projection_hides_provider_material() {
    let (_dir, s, run) = runtime_fixture();
    let id = run["id"].as_str().unwrap();
    s.app_run_decide(
        id,
        run["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    let dispatched = s.app_run_dispatch(id, "sha256:bundle").unwrap();
    let message = start_local_step(&s, &dispatched, 0, "writer");
    s.fixture_write(|c| {
        c.execute(
            "UPDATE messages SET error='private-provider-material-sentinel' WHERE id=?",
            [&message.id],
        )
        .map_err(Into::into)
    })
    .unwrap();
    let projection = s.message(&message.id).unwrap().unwrap().to_json();
    assert!(!projection
        .to_string()
        .contains("private-provider-material-sentinel"));
}

#[test]
fn cad631_pty_team_is_explicitly_unsupported_and_transcript_history_is_private() {
    let (_dir, s, run) = runtime_fixture();
    assert!(s.app_material_endpoint("writer").unwrap());
    let id = run["id"].as_str().unwrap();
    s.app_run_decide(id, None, true, None).unwrap();
    assert!(
        s.app_material_endpoint("writer").unwrap(),
        "terminal state must not expose retained transcript"
    );
    s.fixture_write(|c| {
        c.execute(
            "UPDATE agents SET endpoint_kind='pty' WHERE alias='writer'",
            [],
        )
        .map_err(Into::into)
    })
    .unwrap();
    let workflow: crate::store::app_runs::LocalWorkflow =
        serde_json::from_value(run["snapshot"]["workflow"].clone()).unwrap();
    let inputs = std::collections::BTreeMap::new();
    assert!(s
        .app_run_create(crate::store::app_runs::LocalRunRequest {
            install_id: "install-1",
            bundle_digest: "sha256:bundle",
            workflow: &workflow,
            inputs: &inputs,
            request_id: "unsupported-pty",
            owner_pm: Some("lead"),
            project_link: None,
        })
        .is_err());
}

#[test]
fn cad631_v18_upgrade_commits_only_completed_migrations_before_v20_failure() {
    let (dir, s) = store();
    drop(s);
    let db = dir.path().join("t.sqlite3");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch("DROP TABLE app_run_reviews; DROP TABLE app_run_artifacts; DROP TABLE app_run_steps; DROP TABLE app_runs; DROP TABLE app_contexts; DROP TABLE app_install_capabilities; DROP TABLE app_grants; UPDATE schema_version SET version=18; CREATE TRIGGER fail_v20 BEFORE UPDATE ON schema_version WHEN NEW.version=20 BEGIN SELECT RAISE(ABORT, 'forced v20 migration failure'); END;").unwrap();
    drop(conn);
    assert!(Store::open_for_schema_tests(&db)
        .err()
        .unwrap()
        .to_string()
        .contains("forced v20 migration failure"));
    let conn = Connection::open(&db).unwrap();
    assert_eq!(
        conn.query_row("SELECT version FROM schema_version", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        19
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='app_runs'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='app_grants'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    conn.execute_batch("DROP TRIGGER fail_v20").unwrap();
    drop(conn);
    let repaired = Store::open_for_schema_tests(&db).unwrap();
    assert!(repaired.app_run_list(None).unwrap()["runs"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        repaired
            .conn()
            .query_row("SELECT version FROM schema_version", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        crate::rollout::SCHEMA_VERSION
    );
}

#[test]
fn cad631_execution_approval_requires_current_bundle_not_snapshot_self_attestation() {
    let (_dir, s, run) = runtime_fixture();
    let id = run["id"].as_str().unwrap();
    assert!(s
        .app_run_decide(
            id,
            run["snapshot_digest"].as_str(),
            false,
            Some("sha256:changed-bundle")
        )
        .is_err());
    assert!(s
        .app_run_decide(id, run["snapshot_digest"].as_str(), false, None)
        .is_err());
    assert_eq!(s.app_run_show(id).unwrap()["state"], "awaiting_approval");
    assert_eq!(
        s.conn()
            .query_row(
                "SELECT COUNT(*) FROM events WHERE kind='app_run_execution_approved'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn cad631_operator_audit_survives_revoke_while_worker_fetch_and_corruption_refuse() {
    let (_dir, s, run) = runtime_fixture();
    let id = run["id"].as_str().unwrap();
    s.app_run_decide(
        id,
        run["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    let dispatched = s.app_run_dispatch(id, "sha256:bundle").unwrap();
    let writer = start_local_step(&s, &dispatched, 0, "writer");
    s.finish(&writer, "completed", &producer_result(id, &writer), None)
        .unwrap();
    let next = s.app_run_dispatch(id, "sha256:bundle").unwrap();
    let reviewer = start_local_step(&s, &next, 1, "reviewer");
    let artifact = next["artifacts"][0]["id"].as_str().unwrap();
    let before = s.app_artifact_for_operator(artifact).unwrap();
    assert!(s
        .app_artifact_with_digest(
            artifact,
            Some((&reviewer.id, reviewer.turn_id.as_deref().unwrap())),
            "sha256:bundle"
        )
        .is_ok());
    s.app_capability_decide("install-1", "sha256:bundle", false)
        .unwrap();
    assert_eq!(s.app_artifact_for_operator(artifact).unwrap(), before);
    assert!(s
        .app_artifact_with_digest(
            artifact,
            Some((&reviewer.id, reviewer.turn_id.as_deref().unwrap())),
            "sha256:bundle"
        )
        .is_err());
    s.fixture_write(|c| {
        c.execute(
            "UPDATE app_run_artifacts SET content=? WHERE id=?",
            params![b"corrupted material".as_slice(), artifact],
        )
        .map_err(Into::into)
    })
    .unwrap();
    assert!(s.app_artifact_for_operator(artifact).is_err());
}

const CAD749_PRODUCER: &str = r##"{"schema":1,"kind":"produce_text","run_id":"run-a","step_id":"s1","revision":1,"outcome":"succeeded","artifacts":[{"media_type":"text/markdown","text":"# Twelve retained posts"}]}"##;

fn cad749_parse(text: &str) -> bool {
    super::super::app_runs::parse_local_result_text(text).is_some()
}

#[test]
fn cad749_accepts_one_bounded_bare_or_fenced_envelope_amid_prose() {
    use super::super::app_runs::MAX_RESULT_TEXT_BYTES;
    assert!(cad749_parse(CAD749_PRODUCER));
    // Bare envelope with trailing whitespace is still one envelope.
    assert!(cad749_parse(&format!("{CAD749_PRODUCER}  \n")));
    // Observed Pi shape: prose / one json fence / prose.
    assert!(cad749_parse(&format!(
        "The source call succeeded; the broker receipt is authoritative.\n\n```json\n{CAD749_PRODUCER}\n```\n\n## Scope compliance\nNo outward action."
    )));
    // Prose plus one unfenced envelope is also exactly one candidate.
    assert!(cad749_parse(&format!(
        "The source call succeeded; the broker receipt is authoritative.\n\n{CAD749_PRODUCER}\n\n## Scope compliance\nNo outward action."
    )));
    // Fence body with surrounding blank lines still decodes.
    assert!(cad749_parse(&format!(
        "Notes.\n\n```json\n\n{CAD749_PRODUCER}\n\n```\n\nDone."
    )));
    // Braces inside JSON strings do not create a second candidate.
    let braces_in_string = CAD749_PRODUCER.replace(
        "# Twelve retained posts",
        "# Twelve retained posts {not a candidate}",
    );
    assert!(cad749_parse(&format!(
        "Done.\n\n{braces_in_string}\n\nNo outward action."
    )));

    let reject = [
        // Multiple fenced candidates.
        format!("```json\n{CAD749_PRODUCER}\n```\n```json\n{CAD749_PRODUCER}\n```"),
        // Bare candidate plus a fenced candidate.
        format!("{{\"another\":true}}\n```json\n{CAD749_PRODUCER}\n```"),
        // Two envelopes inside one fence.
        format!("```json\n{CAD749_PRODUCER}\n{CAD749_PRODUCER}\n```"),
        // Unclosed fence.
        format!("```json\n{CAD749_PRODUCER}"),
        // Wrong fence language.
        format!("```text\n{CAD749_PRODUCER}\n```"),
        // Fence plus trailing bare candidate.
        format!("```json\n{CAD749_PRODUCER}\n```\n{{\"another\":true}}"),
        // Unknown (forged identity) field.
        format!(
            "```json\n{}\n```",
            CAD749_PRODUCER.replace("\"schema\":1,", "\"schema\":1,\"turn_id\":\"forged\",")
        ),
        // Duplicate field.
        format!(
            "```json\n{}\n```",
            CAD749_PRODUCER.replace("\"revision\":1", "\"revision\":1,\"revision\":2")
        ),
        // Unknown field in an unfenced envelope.
        format!(
            "Done.\n\n{}\n\nDone.",
            CAD749_PRODUCER.replace("\"schema\":1,", "\"schema\":1,\"turn_id\":\"forged\",")
        ),
        // Two unfenced candidates amid prose.
        format!("Done.\n\n{CAD749_PRODUCER}\n\n{CAD749_PRODUCER}\n\nDone."),
        // Stray braces in prose around one unfenced envelope.
        format!("Done {{later}}.\n\n{CAD749_PRODUCER}\n\nDone."),
        // Truncated envelope amid prose.
        "Done.\n\n{\"schema\":1,\"kind\":\"produce_text\"\n\nDone.".to_string(),
        // Stray fence marker around one unfenced envelope.
        format!("Done.\n\n{CAD749_PRODUCER}\n\n```\nDone."),
        // Missing envelope entirely.
        "The source call succeeded; no envelope follows.".to_string(),
    ];
    for (n, case) in reject.iter().enumerate() {
        assert!(
            !cad749_parse(case),
            "accepted ambiguous, conflicting, malformed, or forged envelope case {n}"
        );
    }
    // Oversized input fails closed on both paths: bare with trailing
    // whitespace, and fenced with trailing prose.
    let oversized_bare = format!("{CAD749_PRODUCER} {}", "x".repeat(MAX_RESULT_TEXT_BYTES));
    assert!(!cad749_parse(&oversized_bare));
    assert!(!cad749_parse(&format!(
        "```json\n{CAD749_PRODUCER}\n```{}",
        "x".repeat(MAX_RESULT_TEXT_BYTES)
    )));
    // The bound itself still admits the largest allowed text.
    let room = MAX_RESULT_TEXT_BYTES - CAD749_PRODUCER.len();
    assert!(cad749_parse(&format!(
        "{CAD749_PRODUCER}{}",
        " ".repeat(room)
    )));
    assert!(!cad749_parse(&format!(
        "{CAD749_PRODUCER}{}",
        " ".repeat(room + 1)
    )));
}

// CAD-778 regression suite: frozen app-run snapshot digests must survive
// the store text round trip bit-identically, and the digest guard must stay
// strict.
//
// Masking warning (read before drawing conclusions from green runs): the
// dev-dependency `jsonschema` enables `serde_json/float_roundtrip`, and
// cargo feature unification turns that on for every in-lane `cargo test`
// binary. Parser-behavior assertions below therefore pass in-lane with or
// without the Cargo.toml fix; they pin the approve-time contract, not the
// production parser. The failing-first guards for the production parser
// are `cad778_production_json_graph_has_exact_float_parsing` (this file)
// and the no-dev-deps CLI freeze → approve rehearsal from the ticket.
//
// Proven 2026-09-29 against true default-feature serde_json 1.0.151 builds:
// the pilot float `1790647563.6348941` parses 1 ULP low without the flag
// (59,122 drifts / 200,000 shortest-repr samples) and exactly with it.

#[test]
fn cad778_production_json_graph_has_exact_float_parsing() {
    // The shipped binary builds without dev-dependencies, so dev-only
    // `jsonschema` must not be the only source of exact float parsing.
    // `--edges normal,build,features` excludes dev edges: this fails
    // pre-fix (feature count 0) and passes once Cargo.toml enables
    // `float_roundtrip` directly.
    let manifest = env!("CARGO_MANIFEST_DIR");
    let cargo = env!("CARGO");
    let out = std::process::Command::new(cargo)
        .args([
            "tree",
            "--locked",
            "--offline",
            "--edges",
            "normal,build,features",
            "--invert",
            "serde_json",
        ])
        .current_dir(manifest)
        .output()
        .expect("cargo tree must run for the production feature probe");
    assert!(
        out.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let tree = String::from_utf8(out.stdout).expect("cargo tree is UTF-8");
    assert!(
        tree.contains("serde_json feature \"float_roundtrip\""),
        "production JSON graph lacks exact float round-tripping;\n\
         frozen snapshot digests drift 1 ULP on reread without it"
    );
}

#[test]
fn cad778_float_snapshot_digest_survives_store_round_trip() {
    use crate::store::app_runs::material_digest;
    // Exact `assignments.identity.created` floats from the frozen pilot row.
    // Freeze serializes them exactly (Ryū shortest repr), but the default
    // float parser reads back a 1-ULP neighbor, so the approve-time
    // re-serialization hashes differently and the frozen digest is refused.
    for created in [1790647563.6348941f64, 1790648372.680178f64] {
        let identity = json!({
            "created": created,
            "created_bits": created.to_bits(),
            "provider": "pi",
            "endpoint_kind": "managed",
        });
        let digest = material_digest(&identity);
        // Freeze path: exact text into the store.
        let stored = identity.to_string();
        assert!(
            stored.contains("1790647") || stored.contains("1790648"),
            "freeze must keep the exact float text, got {stored}"
        );
        // Approve path: reparse the stored text, then rehash.
        let reparsed: Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(
            material_digest(&reparsed),
            digest,
            "float {created} did not survive the store round trip bit-identically"
        );
    }
}

#[test]
fn cad778_frozen_run_with_float_identity_approves() {
    // End-to-end freeze → approve on one build with hostile identity floats.
    let (dir, s) = store();
    for (alias, role) in [("lead", "pm"), ("writer", "worker"), ("reviewer", "worker")] {
        s.register_agent(&NewAgent {
            alias,
            provider: "claude",
            endpoint_kind: "managed",
            role,
            cwd: dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: Some("{\"upstream\":\"lead\"}"),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
        s.set_identity(alias, &endpoint_at(4242)).unwrap();
    }
    // Pin every agent's row timestamp to the exact pilot floats whose
    // shortest repr the default parser misreads by one ULP.
    for (alias, created) in [
        ("lead", 1790647563.6348941f64),
        ("writer", 1790648372.680178f64),
        ("reviewer", 1790647563.6348941f64),
    ] {
        s.fixture_write(|c| {
            c.execute(
                "UPDATE agents SET created=?1, updated=?1 WHERE alias=?2",
                rusqlite::params![created, alias],
            )
            .map_err(Into::into)
        })
        .unwrap();
    }
    let text="---\ntitle: Local\ngoal: Reviewed text\n---\n## Write\nagent: writer\naction: local.text.produce\n\nWrite Markdown.\n\n### Acceptance\n- [ ] Markdown artifact exists\n\n## Review\nagent: reviewer\ndepends_on: 1\naction: local.text.review\n\nReview the exact artifact.\n\n### Acceptance\n- [ ] Exact artifact reviewed\n";
    let inputs = std::collections::BTreeMap::new();
    let workflow = crate::store::app_runs::LocalWorkflow::parse(text, &inputs).unwrap();
    s.app_capability_decide("install-1", "sha256:bundle", true)
        .unwrap();
    let run = s
        .app_run_create(crate::store::app_runs::LocalRunRequest {
            install_id: "install-1",
            bundle_digest: "sha256:bundle",
            workflow: &workflow,
            inputs: &inputs,
            request_id: "request-1",
            owner_pm: Some("lead"),
            project_link: None,
        })
        .unwrap();
    let id = run["id"].as_str().unwrap();
    s.app_run_decide(
        id,
        run["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    assert_eq!(s.app_run_show(id).unwrap()["state"], "approved");
}

#[test]
fn cad778_tampered_snapshot_still_refused() {
    // The digest guard stays strict: stored text that no longer matches the
    // frozen digest must still be refused at approve time.
    let (_dir, s, run) = runtime_fixture();
    let id = run["id"].as_str().unwrap();
    let mut snapshot: Value = run["snapshot"].clone();
    snapshot["inputs"] = json!({"injected": "forged"});
    s.fixture_write(|c| {
        c.execute(
            "UPDATE app_runs SET snapshot=?1 WHERE id=?2",
            rusqlite::params![snapshot.to_string(), id],
        )
        .map_err(Into::into)
    })
    .unwrap();
    assert!(s
        .app_run_decide(
            id,
            run["snapshot_digest"].as_str(),
            false,
            Some("sha256:bundle")
        )
        .is_err());
    assert_eq!(s.app_run_show(id).unwrap()["state"], "awaiting_approval");
}

#[test]
fn cad778_benign_floats_round_trip_exactly() {
    // Historical rows whose floats already parsed exactly must hash
    // identically after the fix: exact parsing agrees with the default
    // parser wherever the default parser was already exact.
    use crate::store::app_runs::material_digest;
    for created in [0.0f64, 1.5, 4242.0, 1750000000.123, 1790648000.5] {
        let identity = json!({"created": created, "created_bits": created.to_bits()});
        let digest = material_digest(&identity);
        let reparsed: Value = serde_json::from_str(&identity.to_string()).unwrap();
        assert_eq!(
            reparsed["created"].as_f64().unwrap().to_bits(),
            created.to_bits()
        );
        assert_eq!(material_digest(&reparsed), digest);
    }
}

#[test]
fn cad1123_run_approval_is_recorded_durably_with_time_and_derived_actor() {
    let (_dir, s, run) = runtime_fixture();
    let id = run["id"].as_str().unwrap();
    assert!(
        run.get("approval").is_none(),
        "an unapproved run has no approval record"
    );
    assert!(run["created"].as_i64().unwrap() > 0 && run["updated"].as_i64().unwrap() > 0);
    // A refused approval leaves no record.
    assert!(s
        .app_run_decide(id, Some("forged"), false, Some("sha256:bundle"))
        .is_err());
    assert!(s.app_run_show(id).unwrap().get("approval").is_none());
    s.app_run_decide(
        id,
        run["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    // The pruned daemon stream does not hold it.
    s.prune_stream(Store::DAEMON_STREAM, 0).unwrap();
    let shown = s.app_run_show(id).unwrap();
    assert_eq!(shown["approval"]["by"], "operator");
    let at = shown["approval"]["at"].as_i64().unwrap();
    assert!(at >= run["created"].as_i64().unwrap() && at > 0);
    // Another run's approval never leaks onto this run.
    assert!(s.app_run_list(Some("install-1")).unwrap()["runs"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["id"] == id || r.get("approval").is_none()));
}

/// CAD-1142: a failed app worker turn's provider error text never
/// reaches the persisted step reason or the public daemon stream.
/// `steps[].reason` and `app_run_failed.payload.reason` are readable
/// via `agent_events` by any caller (Rule::Read), while a provider's
/// `errorMessage` may carry credentials — the step records a bounded
/// classification and the private detail stays on the message row.
#[test]
fn cad1142_failed_worker_turn_reason_is_classified_not_provider_prose() {
    let (_dir, s, run) = runtime_fixture();
    let id = run["id"].as_str().unwrap();
    s.app_run_decide(
        id,
        run["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    let dispatched = s.app_run_dispatch(id, "sha256:bundle").unwrap();
    let writer = start_local_step(&s, &dispatched, 0, "writer");
    let secret = "provider rejected request; Authorization: Bearer cad1142_SYNTHETIC_SECRET";
    s.finish(
        &writer,
        "failed",
        &json!({"status":"failed","turn_id":writer.turn_id,"error":secret}),
        Some(secret),
    )
    .unwrap();
    let shown = s.app_run_show(id).unwrap();
    assert_eq!(shown["state"], "failed", "{shown}");
    assert_eq!(shown["steps"][0]["state"], "failed", "{shown}");
    let reason = shown["steps"][0]["reason"].as_str().unwrap();
    assert!(
        reason.contains("provider error"),
        "classified reason names the cause class: {reason}"
    );
    assert!(
        !reason.contains("cad1142_SYNTHETIC_SECRET") && !reason.contains("Bearer"),
        "provider prose escaped into the step reason: {reason}"
    );
    // The same holds on the public daemon-stream event payload.
    let event = s
        .events(Store::DAEMON_STREAM, 0, 100)
        .unwrap()
        .into_iter()
        .find(|e| e.kind == "app_run_failed")
        .expect("app_run_failed event");
    let payload = event.payload.to_string();
    assert!(!payload.contains("cad1142_SYNTHETIC_SECRET"), "{payload}");
    assert!(!payload.contains("Bearer"), "{payload}");
    assert!(
        event.payload["reason"]
            .as_str()
            .unwrap()
            .contains("provider error"),
        "{payload}"
    );
    // The private detail is not erased — it stays on the message row
    // for operator-only inspection.
    let stored = s.message(&writer.id).unwrap().unwrap();
    assert_eq!(stored.error.as_deref(), Some(secret));
}

/// CAD-1142: invalidation reasons are classified onto the bounded
/// authority-loss vocabulary. Cadence-generated text keeps its class
/// (binding, price), while arbitrary interpolated text — including a
/// credential-bearing provider string — can never be persisted or
/// published verbatim.
#[test]
fn cad1142_invalidate_reason_classifies_and_never_echoes_secret_text() {
    let (_dir, s, run) = runtime_fixture();
    let id = run["id"].as_str().unwrap();
    s.app_run_decide(
        id,
        run["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    s.app_run_dispatch(id, "sha256:bundle").unwrap();
    // A genuine Cadence rejection keeps its readable class.
    s.app_run_invalidate(id, "publication binding is absent or revoked")
        .unwrap();
    let reason = s.app_run_show(id).unwrap()["steps"][0]["reason"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(reason.contains("binding"), "{reason}");
    // A credential-bearing provider string never echoes verbatim; it
    // classifies onto the bounded vocabulary instead.
    let (_dir2, s2, run2) = runtime_fixture();
    let id2 = run2["id"].as_str().unwrap();
    s2.app_run_decide(
        id2,
        run2["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    s2.app_run_dispatch(id2, "sha256:bundle").unwrap();
    s2.app_run_invalidate(
        id2,
        "quote door answered: Authorization: Bearer cad1142_LEAKED_TOKEN binding revoked",
    )
    .unwrap();
    let shown = s2.app_run_show(id2).unwrap();
    let reason = shown["steps"][0]["reason"].as_str().unwrap();
    assert!(!reason.contains("cad1142_LEAKED_TOKEN"), "{reason}");
    assert!(!reason.contains("Bearer"), "{reason}");
    assert!(reason.contains("binding"), "{reason}");
    for event in s2.events(Store::DAEMON_STREAM, 0, 200).unwrap() {
        assert!(
            !event.payload.to_string().contains("cad1142_LEAKED_TOKEN"),
            "secret in {} event: {}",
            event.kind,
            event.payload
        );
    }
}

/// CAD-1142: the classification helper is total — every input maps to a
/// bounded Cadence-authored class and never echoes the input.
#[test]
fn cad1142_reason_class_is_bounded_and_never_echoes() {
    use super::super::app_runs::app_reason_class;
    let cases = [
        ("publication binding is absent or revoked", "binding"),
        ("capability price changed since run creation", "price"),
        ("run context was updated or archived", "context"),
        ("connection is unavailable or stale", "connection"),
        (
            "registered assignment identity or group changed",
            "assignment",
        ),
        ("run execution approval is absent or stale", "approval"),
        (
            "Authorization: Bearer topsecret; unexpected prose",
            "authority",
        ),
    ];
    // A Cadence literal may classify onto its own canonical text;
    // free text never passes through verbatim unless it IS the class.
    const CLASSES: &[&str] = &[
        "run binding receipt is no longer current",
        "capability price changed since run approval",
        "run context was updated or archived",
        "capability connection receipt is stale or unavailable",
        "registered assignment identity or group changed",
        "run execution approval is absent or stale",
        "app authority or assignment is no longer current",
    ];
    for (input, needle) in cases {
        let class = app_reason_class(input);
        assert!(class.contains(needle), "{input} -> {class}");
        assert!(CLASSES.contains(&class), "{input} -> unlisted {class}");
    }
    // Arbitrary provider prose with credentials never echoes.
    for hostile in [
        "Authorization: Bearer leaked; request rejected",
        "upstream 500: secret=abc in body",
        "",
    ] {
        let class = app_reason_class(hostile);
        assert!(CLASSES.contains(&class), "{hostile} -> {class}");
        assert!(!class.contains("leaked") && !class.contains("abc"));
    }
}
