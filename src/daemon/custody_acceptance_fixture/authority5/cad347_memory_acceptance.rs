//! Independent CAD-347 memory authority through actual managed peers.
//! Model/turn rows are explicit server-side fixtures, not vendor launches
//! or observations that a provider received, read, or applied a lesson.
//! Proposal, reviews, finalization and feedback use raw real sockets.

use super::*;

fn cad347_process_ready(rig: &Rig, alias: &str, model: &str, probe: &Probe) {
    // Match the native generation/token grammar, without claiming that
    // a model provider actually launched. Persist through the real
    // set_identity transaction and its native ready event.
    let generation: String = Sha256::digest(format!("cad347-owned-process:{alias}").as_bytes())
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    rig.shared
        .store
        .set_identity(
            alias,
            &crate::adapter::Identity {
                thread_id: format!("{alias}-thread"),
                session_id: format!("{alias}-session"),
                model: Some(model.to_string()),
                effort: None,
                pid: probe.pid,
                endpoint: None,
                generation: Some(generation),
                attach: None,
            },
        )
        .expect("native ready snapshot for owned process fixture");
}

fn cad347_register_process(rig: &Rig, alias: &str, role: &str, model: &str, probe: &Probe) {
    rig.shared
        .store
        .register_agent(&NewAgent {
            alias,
            provider: "pi",
            endpoint_kind: "managed",
            role,
            cwd: rig.state_dir.to_str().expect("fixture path encoding"),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .expect("register owned managed-process fixture");
    cad347_process_ready(rig, alias, model, probe);
    rig.shared.enroll_endpoint(alias);
    assert!(crate::test_seam::asserted().is_none());
    match rig
        .shared
        .caller_identity(probe.pid)
        .unwrap_or_else(|error| {
            panic!("native managed-process fixture identity failed for {alias}: {error}")
        }) {
        crate::daemon::Caller::Agent(verified) => {
            assert_eq!(verified.agent.alias, alias);
            assert_eq!(verified.agent.role, role);
            assert_eq!(verified.agent.pid, Some(i64::from(probe.pid)));
            assert_ne!(verified.process_start, 0);
            assert!(!verified.generation.is_empty());
        }
        _ => panic!("owned managed-process fixture has no native agent identity"),
    }
    assert!(matches!(
        &rig.shared.connection_caller(probe.pid).unwrap(),
        Who::Agent(actual) if actual == alias
    ));
}

fn cad347_raw_success(probe: &mut Probe, rig: &Rig, method: &str, params: Value) -> Value {
    assert!(crate::test_seam::asserted().is_none());
    let answer = probe.rpc(rig, method, params);
    assert_eq!(
        answer["ok"], true,
        "raw native request failed for method {method}; returned error: {}",
        answer["error"]
    );
    answer["result"].clone()
}

fn cad347_raw_memory_refusal(
    probe: &mut Probe,
    rig: &Rig,
    pm_dir: &Path,
    candidate_path: &Path,
    method: &str,
    params: Value,
) {
    assert!(crate::test_seam::asserted().is_none());
    let before_bytes = std::fs::read(candidate_path).unwrap();
    let before_head = crate::issue::git(pm_dir, &["rev-parse", "HEAD"]).unwrap();
    let answer = probe.rpc(rig, method, params);
    assert_eq!(
        answer["ok"], false,
        "real native caller acquired forbidden memory authority"
    );
    assert_eq!(
        answer["error"]["kind"], "rejected",
        "transport/setup/internal failure is not authority-denial evidence"
    );
    assert_eq!(std::fs::read(candidate_path).unwrap(), before_bytes);
    assert_eq!(
        crate::issue::git(pm_dir, &["rev-parse", "HEAD"]).unwrap(),
        before_head
    );
}

#[test]
fn cad347_real_native_pm_unregistered_and_setsid_callers_cannot_mint_operator_authority() {
    assert!(
        std::env::var_os("CADENCE_SOCKET").is_none(),
        "isolated runner required; never clear an inherited socket override"
    );
    assert!(
        std::env::var_os("CADENCE_TEST_AS").is_none(),
        "isolated runner required; never clear an inherited caller assertion"
    );
    assert!(crate::test_seam::asserted().is_none());
    let root = tempfile::Builder::new()
        .prefix("cad347-native-")
        .tempdir_in(std::env::temp_dir())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let rig = Rig::start(root.path(), A.as_bytes(), B.as_bytes()).unwrap();
    assert!(rig
        .state_dir
        .canonicalize()
        .unwrap()
        .starts_with(root.path()));
    let pm_dir = root.path().join("pm");
    let pm = crate::issue::Pm::at(&pm_dir).unwrap();
    crate::issue::write::project_add(&pm, "cad347-native", "CNA", &[], &[], &[], None).unwrap();

    let mut actors = [Probe::start(false), Probe::start(false), Probe::start(true)];
    cad347_register_process(
        &rig,
        "authority5-native-agent",
        "pm",
        "openai/gpt-5",
        &actors[0],
    );
    let mut author = Probe::start(false);
    let mut reviewers = [Probe::start(false), Probe::start(false)];
    cad347_register_process(
        &rig,
        "cad347-source-author",
        "worker",
        "openai/gpt-5",
        &author,
    );
    cad347_register_process(
        &rig,
        "cad347-review-one",
        "worker",
        "anthropic/claude-sonnet-4-5",
        &reviewers[0],
    );
    cad347_register_process(
        &rig,
        "cad347-review-two",
        "worker",
        "google/gemini-2.5-pro",
        &reviewers[1],
    );
    assert_eq!(actors[2].pid, actors[2].sid);
    let slug = "cad347-native-operator-required";
    let proposal = cad347_raw_success(
        &mut author,
        &rig,
        "memory_propose",
        json!({
            "project": "cad347-native",
            "id": slug,
            "kind": "decision",
            "scope": {"project": true},
            "source": "CAD-347 independent raw-native authority acceptance",
            "confidence": "high",
            "text": "Require operator authority for this judgement\n\n**Why:** two agent reviews do not authorize agent-claimed judgement\n\n**How to apply:** derive approval from the native connection\n",
        }),
    );
    let pinned_digest = proposal["digest"].as_str().unwrap().to_string();
    for reviewer in &mut reviewers {
        cad347_raw_success(
            reviewer,
            &rig,
            "memory_review",
            json!({
                "project": "cad347-native",
                "slug": slug,
                "operation": "accept",
                "verdict": "pass",
                "evidence": "Independent evidence review of the unchanged judgement",
                "digest": pinned_digest,
            }),
        );
    }
    let candidate_path = crate::memory::memory_dir(&pm, "cad347-native").join(format!("{slug}.md"));
    for (index, actor) in actors.iter_mut().enumerate() {
        assert_native_class(&rig, actor, index == 0);
        assert_eq!(actor.rpc(&rig, "health", json!({}))["ok"], true);
        if index == 0 {
            match rig.shared.caller_identity(actor.pid).unwrap() {
                crate::daemon::Caller::Agent(verified) => assert_eq!(verified.agent.role, "pm"),
                _ => panic!("registered real socket peer is not a native PM"),
            }
        }
        for (method, params) in [
            (
                "memory_finalize",
                json!({
                    "project": "cad347-native",
                    "slug": slug,
                    "operation": "accept",
                    "digest": pinned_digest,
                }),
            ),
            (
                "memory_classify",
                json!({
                    "project": "cad347-native",
                    "slug": slug,
                    "digest": pinned_digest,
                    "provenance": "agent-verified",
                    "evidence": "A native agent or detached process cannot impersonate an operator attestation",
                }),
            ),
        ] {
            assert_native_class(&rig, actor, index == 0);
            cad347_raw_memory_refusal(actor, &rig, &pm_dir, &candidate_path, method, params);
        }
    }

    // Existing explicitly synthetic operator control. No actual probe
    // is represented as having positive operator process ancestry.
    let classification = setup_rpc(
        &rig,
        "memory_classify",
        json!({
            "project": "cad347-native",
            "slug": slug,
            "digest": pinned_digest,
            "provenance": "agent-verified",
            "evidence": "Explicit simulated operator provenance control",
        }),
    );
    let current_digest = classification["digest"].as_str().unwrap().to_string();
    assert_ne!(current_digest, pinned_digest);
    {
        let memories = crate::memory::load_project(&pm_dir, "cad347-native").unwrap();
        let memory = memories
            .iter()
            .find(|memory| memory.front.id == slug)
            .unwrap();
        assert!(!crate::memory::retrieval_status(memory).0);
        assert!(crate::memory::last_verified(memory).is_none());
    }
    // The old reviews do not cover classification's changed digest.
    // One actual managed curator now reviews and finalizes the current
    // project judgement using its native ready model/vendor evidence.
    cad347_raw_success(
        &mut reviewers[0],
        &rig,
        "memory_review",
        json!({
            "project": "cad347-native",
            "slug": slug,
            "operation": "accept",
            "verdict": "pass",
            "evidence": "Current-digest independent cross-vendor curator review",
            "digest": current_digest,
        }),
    );
    cad347_raw_success(
        &mut reviewers[0],
        &rig,
        "memory_finalize",
        json!({
            "project": "cad347-native",
            "slug": slug,
            "operation": "accept",
            "digest": current_digest,
        }),
    );
    {
        let memories = crate::memory::load_project(&pm_dir, "cad347-native").unwrap();
        let memory = memories
            .iter()
            .find(|memory| memory.front.id == slug)
            .unwrap();
        assert!(crate::memory::retrieval_status(memory).0);
        assert_eq!(crate::memory::semantic_digest(memory), current_digest);
        assert!(crate::memory::last_verified(memory).is_none());
    }

    // Explicit server-side current-turn fixture, using the SAME native
    // enqueue, provider token scheme and mark_running producer APIs as
    // existing feedback acceptance. This is not a claim that Pi ran a
    // model turn, nor a caller-constructed NativeFeedbackProof/receipt.
    let issue = crate::issue::write::new_issue(
        &pm,
        root.path(),
        Some("cad347-native"),
        "Real managed memory-feedback scope",
        None,
        None,
        &[],
        None,
        None,
        &[],
        None,
        None,
        "pm",
    )
    .unwrap();
    let issue_id = issue["id"].as_str().unwrap();
    let message_id = "cad347-native-feedback-turn";
    let row = rig.shared.store.agent("cad347-review-one").unwrap();
    let generation = row.generation.as_deref().unwrap();
    let token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(generation);
    assert!(crate::adapter::registry::turn_token_current(
        &row.provider,
        &row.endpoint_kind,
        Some(generation),
        &token,
    ));
    let (duplicate, _) = rig
        .shared
        .store
        .enqueue_steered(
            "cad347-review-one",
            "Native active-turn feedback fixture",
            None,
            message_id,
            "dispatch",
            None,
            Some(issue_id),
            None,
            &crate::store::Sender::Unattributed,
            &crate::store::Steer::NONE,
            None,
            None,
            None,
        )
        .unwrap();
    assert!(!duplicate);
    rig.shared.store.mark_running(message_id, &token).unwrap();
    let active = rig.shared.store.message(message_id).unwrap().unwrap();
    assert_eq!(active.alias, "cad347-review-one");
    assert_eq!(active.recipient_uid.as_deref(), Some(row.uid.as_str()));
    assert_eq!(active.state, "running");
    assert!(active.turn_id.as_deref() == Some(token.as_str()));
    let feedback = json!({
        "project": "cad347-native",
        "slug": slug,
        "digest": current_digest,
        "message": message_id,
        "token": token,
        "kind": "used",
        "evidence": "Explicit native report of manual use, not proven model compliance or benefit",
    });
    // A correct current token cannot authorize another real agent,
    // the native PM, or either genuinely unproven process.
    cad347_raw_memory_refusal(
        &mut reviewers[1],
        &rig,
        &pm_dir,
        &candidate_path,
        "memory_feedback",
        feedback.clone(),
    );
    for actor in &mut actors {
        cad347_raw_memory_refusal(
            actor,
            &rig,
            &pm_dir,
            &candidate_path,
            "memory_feedback",
            feedback.clone(),
        );
    }
    let different_token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(generation);
    assert!(different_token != token);
    let mut wrong_token = feedback.clone();
    wrong_token["token"] = json!(different_token);
    cad347_raw_memory_refusal(
        &mut reviewers[0],
        &rig,
        &pm_dir,
        &candidate_path,
        "memory_feedback",
        wrong_token,
    );
    cad347_raw_success(&mut reviewers[0], &rig, "memory_feedback", feedback.clone());
    {
        let memories = crate::memory::load_project(&pm_dir, "cad347-native").unwrap();
        let memory = memories
            .iter()
            .find(|memory| memory.front.id == slug)
            .unwrap();
        let front = serde_json::to_value(&memory.front).unwrap();
        let reports = &front["policy"]["feedback"];
        assert_eq!(reports["used_reports"].as_u64(), Some(1));
        assert_eq!(reports["last_use"]["message"].as_str(), Some(message_id));
        assert_eq!(
            reports["last_use"]["actor"]["alias"].as_str(),
            Some("cad347-review-one")
        );
        assert_eq!(
            reports["last_use"]["actor"]["uid"].as_str(),
            Some(row.uid.as_str())
        );
        assert_eq!(crate::memory::semantic_digest(memory), current_digest);
        assert!(crate::memory::retrieval_status(memory).0);
        assert!(crate::memory::last_verified(memory).is_none());
        assert!(!std::fs::read_to_string(&candidate_path)
            .unwrap()
            .contains(&token));
    }
    let active = rig.shared.store.message(message_id).unwrap().unwrap();
    rig.shared
        .store
        .finish(
            &active,
            "completed",
            &json!({"text": "native turn fixture completed"}),
            None,
        )
        .unwrap();
    assert!(crate::adapter::registry::turn_token_current(
        &row.provider,
        &row.endpoint_kind,
        Some(generation),
        &token,
    ));
    cad347_raw_memory_refusal(
        &mut reviewers[0],
        &rig,
        &pm_dir,
        &candidate_path,
        "memory_feedback",
        feedback,
    );

    author.finish();
    for reviewer in &mut reviewers {
        reviewer.finish();
    }
    for actor in &mut actors {
        actor.finish();
    }
    rig.finish().unwrap();
}
