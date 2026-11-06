//! CAD-2207 — independent lifecycle-guard acceptance (bounded).
//!
//! A member's personal assistant is visible and controllable only by its
//! owner through the member's own signed route. This module pins the OTHER
//! half of that statement on the REAL dispatcher and the REAL store: a
//! generic operator connection — unbound by construction — cannot capture,
//! probe, retune, stop, remove, ack, answer or recover a member-bound
//! assistant; the durable store paths (remove, gc, archive, restore) refuse
//! it even when a candidate/plan snapshot was taken while the row looked
//! disposable; and the binding's event/message fence is frozen in the same
//! transaction as the binding, so work already in flight at bind time is
//! ordered behind it.
//!
//! Scope, stated narrowly: this is the LOCAL runtime/lifecycle boundary. It
//! makes NO claim about OS-user custody, about the remote platform's
//! membership/credential decisions, or about an owner-authorized signed
//! lifecycle route — none of those is exercised or implied here.
//!
//! Ordinary controls cover every path where a genuine ordinary success is
//! reachable in this fixture (`agent set` with `gc_keep`, `agent stop`,
//! `agent remove --force`, and the archive/restore/gc store paths). For the
//! endpoint-dependent verbs (`agent capture`, `agent probe`, `agent
//! inbox_ack`, `agent answer`, `agent recover_submit`) an ordinary managed
//! agent with no live provider actor only fails for its own precondition;
//! the ordinary call there must NOT answer the lifecycle refusal, which is
//! asserted, and no fake ordinary success is staged.
//!
//! The fixture (`Fx`) is the parent module's already-real in-process daemon
//! (real store, real dispatcher, real operator/board identity); this child
//! module adds no production seam.
//!
//! One narrow health acceptance is included because the same local work must
//! never read as idle: when the work table is genuinely unreadable, `health`
//! must answer `update_waiting` as null/unknown with a generic
//! `update_waiting_unavailable: true`, never `[]` — while a healthy store
//! keeps the real array.

use super::*;

/// The store's generic refusal for a restricted lifecycle target. Equality
/// is asserted so a target-keyed refusal cannot pass as a leaked alias.
const RESTRICTED: &str = crate::store::RESTRICTED_LIFECYCLE;

/// Register a stopped, disabled ordinary agent — a disposable row every
/// lifecycle path is allowed to touch.
fn ordinary_agent(fx: &Fx, alias: &str) {
    let cwd = fx.dir.path().join(alias);
    std::fs::create_dir_all(&cwd).unwrap();
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias,
            provider: "pi",
            endpoint_kind: "managed",
            role: "worker",
            cwd: cwd.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    fx.shared.store.set_enabled(alias, false).unwrap();
    fx.shared
        .store
        .set_agent_state(alias, "stopped", None)
        .unwrap();
}

fn assert_generic(error: &crate::error::Error, what: &str, alias: &str) {
    let text = error.to_string();
    assert_eq!(
        text, RESTRICTED,
        "{what} did not answer with the generic lifecycle refusal: {text}"
    );
    assert!(
        !text.contains(alias),
        "{what} named the restricted alias: {text}"
    );
    assert!(
        !text.contains("sub-"),
        "{what} named the member principal: {text}"
    );
}

#[test]
fn unbound_operator_lifecycle_verbs_refuse_a_member_assistant() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");

    let control = "cad2207-lifecycle-control";
    let control_stop = "cad2207-lifecycle-stop";
    let control_remove = "cad2207-lifecycle-remove";
    ordinary_agent(&fx, control);
    ordinary_agent(&fx, control_stop);
    ordinary_agent(&fx, control_remove);

    let before_agent = fx.shared.store.agent(&alice.alias).unwrap().to_json();
    let before_binding = fx
        .shared
        .store
        .assistant_binding(ORG, "sub-alice")
        .unwrap()
        .expect("the fixture binding vanished")
        .to_json();
    let before_tree = fx.tree();

    for (method, params) in [
        ("agent_capture", json!({"alias": alice.alias})),
        ("agent_probe", json!({"alias": alice.alias})),
        (
            "agent_set",
            json!({"alias": alice.alias, "patch": {"gc_keep": true}}),
        ),
        ("agent_stop", json!({"alias": alice.alias})),
        ("agent_remove", json!({"alias": alice.alias, "force": true})),
        (
            "agent_inbox_ack",
            json!({"alias": alice.alias, "reader": "cad2207-reader"}),
        ),
        ("agent_answer", json!({"alias": alice.alias, "choice": "1"})),
        (
            "agent_recover_submit",
            json!({"alias": alice.alias, "message": "cad2207-missing"}),
        ),
        (
            "agent_instructions_set",
            json!({"alias": alice.alias, "instructions": "cad2207-private-instructions"}),
        ),
        (
            "agent_unfence",
            json!({"alias": alice.alias, "status": "failed"}),
        ),
    ] {
        let error = fx
            .call(Asserted::Operator, method, params)
            .expect_err("an unbound operator reached a member assistant's lifecycle");
        assert_generic(&error, method, &alice.alias);
    }

    // The instructions INPUT is private too: the generic refusal names the
    // alias neither, and never echoes the text an operator tried to install.
    let error = fx
        .call(
            Asserted::Operator,
            "agent_instructions_set",
            json!({"alias": alice.alias, "instructions": "cad2207-private-instructions"}),
        )
        .expect_err("an unbound operator changed a member assistant's instructions");
    assert_generic(&error, "agent_instructions_set", &alice.alias);
    assert!(
        !error.to_string().contains("cad2207-private-instructions"),
        "the refusal echoed the private instructions input: {error}"
    );

    // Nothing was captured, probed, retuned, stopped, removed, acked,
    // answered, retuned by instructions or unfenced.
    assert_eq!(
        fx.shared.store.agent(&alice.alias).unwrap().to_json(),
        before_agent,
        "a refused lifecycle verb changed the assistant's runtime row"
    );
    assert_eq!(
        fx.shared
            .store
            .assistant_binding(ORG, "sub-alice")
            .unwrap()
            .expect("the fixture binding vanished")
            .to_json(),
        before_binding,
        "a refused lifecycle verb changed the member binding"
    );
    assert_eq!(
        fx.tree(),
        before_tree,
        "a refused lifecycle verb changed the tracker"
    );

    // ---- ordinary controls -------------------------------------------------
    // Endpoint-dependent verbs: the ordinary managed agent has no live actor,
    // so it fails for its own precondition — never with the lifecycle refusal.
    for (method, params) in [
        ("agent_capture", json!({"alias": control})),
        ("agent_probe", json!({"alias": control})),
        (
            "agent_inbox_ack",
            json!({"alias": control, "reader": "cad2207-reader"}),
        ),
        ("agent_answer", json!({"alias": control, "choice": "1"})),
        (
            "agent_recover_submit",
            json!({"alias": control, "message": "cad2207-missing"}),
        ),
    ] {
        if let Err(error) = fx.call(Asserted::Operator, method, params) {
            assert_ne!(
                error.to_string(),
                RESTRICTED,
                "{method} refused an ordinary target with the lifecycle refusal: {error}"
            );
        }
    }

    // ... and the ordinary set/stop/remove paths really execute.
    fx.call(
        Asserted::Operator,
        "agent_set",
        json!({"alias": control_stop, "patch": {"gc_keep": true}}),
    )
    .expect("an ordinary agent lost the operator set path");
    assert!(
        fx.shared.store.agent(control_stop).unwrap().gc_keep,
        "the ordinary agent_set did not record gc_keep"
    );
    let stopped = fx
        .call(
            Asserted::Operator,
            "agent_stop",
            json!({"alias": control_stop}),
        )
        .expect("an ordinary agent lost the operator stop path");
    assert_eq!(stopped["state"], "stopped");
    fx.call(
        Asserted::Operator,
        "agent_remove",
        json!({"alias": control_remove, "force": true}),
    )
    .expect("an ordinary agent lost the operator remove path");
    assert!(
        fx.shared.store.agent_opt(control_remove).unwrap().is_none(),
        "the ordinary agent_remove did not delete the row"
    );

    // ... and the ordinary instructions path still installs the text.
    let instructions = "cad2207 ordinary instructions";
    let updated = fx
        .call(
            Asserted::Operator,
            "agent_instructions_set",
            json!({"alias": control, "instructions": instructions}),
        )
        .expect("an ordinary agent lost the operator instructions path");
    assert_eq!(updated["state"], "updated", "{updated}");
    assert_eq!(
        fx.shared
            .store
            .agent(control)
            .unwrap()
            .instructions
            .as_deref(),
        Some(instructions),
        "the ordinary agent_instructions_set did not store the text"
    );
}

#[test]
fn store_lifecycle_and_gc_refuse_a_bound_assistant_even_when_disposable() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");

    // Make the bound assistant's runtime row look genuinely disposable: a
    // stopped, disabled, endpoint-less row with no open work. A candidate
    // snapshot taken now IS the stale snapshot a generic sweep would act on.
    fx.shared.store.set_enabled(&alice.alias, false).unwrap();
    fx.shared
        .store
        .set_agent_state(&alice.alias, "stopped", None)
        .unwrap();
    let candidates = fx.shared.store.gc_candidates(None).unwrap();
    assert!(
        candidates.iter().any(|agent| agent.alias == alice.alias),
        "the fixture could not make the bound assistant a gc candidate: {candidates:?}"
    );

    let by = json!({"by": "operator"});
    let error = fx
        .shared
        .store
        .remove_agent(&alice.alias, true, &by)
        .expect_err("remove_agent removed a bound assistant");
    assert_generic(&error, "remove_agent", &alice.alias);
    let error = fx
        .shared
        .store
        .gc_remove_agent(&alice.alias, &by)
        .expect_err("gc_remove_agent removed a bound assistant");
    assert_generic(&error, "gc_remove_agent", &alice.alias);
    let error = fx
        .shared
        .store
        .archive_plan(&alice.alias)
        .expect_err("archive_plan disclosed a bound assistant's plan");
    assert_generic(&error, "archive_plan", &alice.alias);
    let error = fx
        .shared
        .store
        .archive_agents(
            &[(alice.alias.clone(), "stale-preview-digest".into())],
            "cad2207 stale snapshot",
            &by,
        )
        .expect_err("archive_agents archived a bound assistant");
    assert_generic(&error, "archive_agents", &alice.alias);
    let error = fx
        .shared
        .store
        .restore_agents(&[(alice.alias.clone(), 0)], &by)
        .expect_err("restore_agents restored/interrogated a bound assistant");
    assert_generic(&error, "restore_agents", &alice.alias);

    let after = fx.shared.store.agent(&alice.alias).unwrap();
    assert!(after.archived_at.is_none(), "{after:?}");
    assert_eq!(after.state, "stopped", "{after:?}");
    assert_eq!(
        fx.shared
            .store
            .assistant_binding(ORG, "sub-alice")
            .unwrap()
            .expect("the binding vanished")
            .state,
        "active",
        "a refused lifecycle call changed the member binding"
    );

    // ---- ordinary controls: the same store paths still work ---------------
    let archive = "cad2207-archive-control";
    ordinary_agent(&fx, archive);
    let plan = fx
        .shared
        .store
        .archive_plan(archive)
        .expect("an ordinary agent lost its archive plan");
    assert!(plan.reasons.is_empty(), "{plan:?}");
    fx.shared
        .store
        .archive_agents(
            &[(archive.to_string(), plan.digest.clone())],
            "cad2207 ordinary control",
            &by,
        )
        .expect("an ordinary agent lost the archive path");
    let archived = fx.shared.store.agent(archive).unwrap();
    assert!(archived.archived_at.is_some(), "{archived:?}");
    fx.shared
        .store
        .restore_agents(&[(archive.to_string(), archived.lifecycle_rev)], &by)
        .expect("an ordinary agent lost the restore path");
    assert!(
        fx.shared
            .store
            .agent(archive)
            .unwrap()
            .archived_at
            .is_none(),
        "the ordinary restore did not clear archived_at"
    );

    let gc = "cad2207-gc-control";
    ordinary_agent(&fx, gc);
    assert!(
        fx.shared
            .store
            .gc_candidates(None)
            .unwrap()
            .iter()
            .any(|agent| agent.alias == gc),
        "the ordinary gc control is not a candidate"
    );
    fx.shared
        .store
        .gc_remove_agent(gc, &by)
        .expect("an ordinary agent lost the gc remove path");
    assert!(
        fx.shared.store.agent_opt(gc).unwrap().is_none(),
        "the ordinary gc remove did not delete the row"
    );
}

#[test]
fn binding_fence_orders_work_already_in_flight_at_bind_time() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);

    // Pre-bind history for the alias the binding is about to claim: one
    // RUNNING (in-flight) message and one event. The alias has no agents row,
    // so the new-binding path is used and these rows are exactly the history
    // a stale read could otherwise attribute to the member's assistant.
    let alias = "cad2207-fence-assistant";
    let sub = "sub-fence";
    let pre_message_seq: i64 = fx
        .shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT INTO messages(id,alias,body,reply_to,source,state,created) \
                 VALUES(?,?,?,NULL,'user','running',?)",
                rusqlite::params![
                    "cad2207-fence-pre-message",
                    alias,
                    "pre-bind in-flight turn",
                    crate::issue::time::now_epoch() as f64
                ],
            )?;
            tx.query_row(
                "SELECT seq FROM messages WHERE id='cad2207-fence-pre-message'",
                [],
                |row| row.get(0),
            )
        })
        .expect("seed the pre-bind in-flight message");
    let pre_event_seq: i64 = fx
        .shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT INTO events(alias,kind,payload,at) VALUES(?,?,?,?)",
                rusqlite::params![
                    alias,
                    "cad2207_pre_bind",
                    "{}",
                    crate::issue::time::now_epoch() as f64
                ],
            )?;
            tx.query_row(
                "SELECT MAX(seq) FROM events WHERE alias=?",
                [alias],
                |row| row.get(0),
            )
        })
        .expect("seed the pre-bind event");

    let binding = fx
        .shared
        .store
        .assistant_bind(ORG, sub, "assist_sub-fence", alias, Some("rev0"))
        .expect("a fresh binding over pre-existing history must succeed");
    assert_eq!(binding.state, "active");
    let fence = fx
        .shared
        .store
        .assistant_event_fence(ORG, sub)
        .unwrap()
        .expect("the new binding has no event fence");
    assert!(
        fence.message_cutoff_seq >= pre_message_seq,
        "the binding fence missed the in-flight message: {fence:?} < {pre_message_seq}"
    );
    assert!(
        fence.cutoff_seq >= pre_event_seq,
        "the binding fence missed the pre-bind event: {fence:?} < {pre_event_seq}"
    );

    // Work created AFTER the binding is above the immutable cutoff: the
    // ordering boundary is the binding transaction, not wall-clock or a
    // later re-bind.
    let post_message_seq: i64 = fx
        .shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT INTO messages(id,alias,body,reply_to,source,state,created) \
                 VALUES(?,?,?,NULL,'user','running',?)",
                rusqlite::params![
                    "cad2207-fence-post-message",
                    alias,
                    "post-bind in-flight turn",
                    crate::issue::time::now_epoch() as f64
                ],
            )?;
            tx.query_row(
                "SELECT seq FROM messages WHERE id='cad2207-fence-post-message'",
                [],
                |row| row.get(0),
            )
        })
        .expect("seed the post-bind message");
    assert!(
        post_message_seq > fence.message_cutoff_seq,
        "the post-bind message landed under the frozen cutoff: {post_message_seq} <= {:?}",
        fence.message_cutoff_seq
    );
    assert_eq!(
        fx.shared
            .store
            .message("cad2207-fence-pre-message")
            .unwrap()
            .expect("the pre-bind message vanished")
            .state,
        "running",
        "the fixture did not leave the pre-bind work in flight"
    );
}

/// CAD-2207 health liveness: an unreadable local work table must never read
/// as an idle fleet. The daemon's `health` is the CLI/board liveness probe;
/// when its real in-flight read fails it must answer `update_waiting` as
/// null/unknown plus one generic `update_waiting_unavailable: true`, while a
/// healthy store keeps the REAL array and does not set the flag. The failure
/// is a real store error (the local work table is renamed away for every
/// reader), not a mocked result and not a private-only boolean.
#[test]
fn health_reports_unavailable_when_local_inflight_work_cannot_be_read() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);

    // One ordinary actor with one in-flight turn: the real array a healthy
    // answer must retain.
    let worker = "cad2207-health-worker";
    ordinary_agent(&fx, worker);
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT INTO messages(id,alias,body,reply_to,source,state,created) \
                 VALUES(?,?,?,NULL,'user','running',?)",
                rusqlite::params![
                    "cad2207-health-turn",
                    worker,
                    "in-flight ordinary turn",
                    crate::issue::time::now_epoch() as f64
                ],
            )?;
            Ok(())
        })
        .expect("the fixture cannot seed the ordinary in-flight turn");

    let healthy = fx
        .call(Asserted::Operator, "health", json!({}))
        .expect("the daemon's health was refused while the store was healthy");
    assert_eq!(healthy["state"], "ready", "{healthy}");
    assert!(
        healthy["pid"].as_u64().is_some_and(|pid| pid > 0),
        "health lost its liveness identity: {healthy}"
    );
    let waiting = healthy["update_waiting"].as_array().unwrap_or_else(|| {
        panic!("a healthy health did not carry the real waiting array: {healthy}")
    });
    assert_eq!(
        waiting.len(),
        1,
        "the ordinary in-flight turn is not reported as waiting: {healthy}"
    );
    assert_ne!(
        healthy
            .get("update_waiting_unavailable")
            .and_then(Value::as_bool),
        Some(true),
        "a healthy health carried the generic unavailable flag: {healthy}"
    );

    // Deterministic store failure: the local work table is gone for every
    // reader. This is the real query error the daemon must not flatten to
    // `[]` (which reads as an idle fleet).
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "ALTER TABLE messages RENAME TO cad2207_messages_unreadable",
                [],
            )?;
            Ok(())
        })
        .expect("the fixture cannot make the local work table unreadable");

    let broken = fx
        .call(Asserted::Operator, "health", json!({}))
        .expect("health itself was refused; only the work read should be unavailable");
    assert_eq!(
        broken["state"], "ready",
        "health liveness changed with the unreadable work table: {broken}"
    );
    assert!(
        broken["pid"].as_u64().is_some_and(|pid| pid > 0),
        "health lost its liveness identity: {broken}"
    );
    let waiting = &broken["update_waiting"];
    assert!(
        waiting.is_null() || waiting == "unknown",
        "an unreadable work table still answered a waiting value — `[]` reads as an \
         idle fleet: {broken}"
    );
    assert_eq!(
        broken
            .get("update_waiting_unavailable")
            .and_then(Value::as_bool),
        Some(true),
        "health did not flag the unreadable local work: {broken}"
    );
}

/// CAD-2207: the CLI restart's record verdict now comes from the daemon's
/// aggregate `drain` (the dead CLI-only helper's cases ported here). The
/// successor's `last-recovery.json` is validated strictly: absent,
/// unparseable, bound to another run, malformed fields, a failed predecessor
/// drain, or any fenced/unevidenced turn are never `clean`; only a record
/// bound to THIS run with a clean drain and no rows is. The aggregate answer
/// never echoes the private identifiers the record carries.
#[test]
fn fleet_drain_verdict_rejects_bad_recovery_records_and_keeps_them_opaque() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");
    let state = fx.dir.path();
    let record_path = state.join(crate::daemon::LAST_RECOVERY_FILE);

    let instance = fx
        .call(Asserted::Operator, "health", json!({}))
        .expect("the fixture daemon's health was refused")["instance"]
        .as_str()
        .expect("the fixture daemon named no instance")
        .to_string();

    let record = |value: &Value| {
        std::fs::write(&record_path, value.to_string())
            .expect("the fixture cannot write the recovery record");
    };
    let drain = || {
        fx.call(Asserted::Operator, "agent_fleet_state", json!({}))
            .expect("the operator's fleet-state gate was refused")["drain"]
            .as_str()
            .map(str::to_string)
    };
    let clean = json!({
        "instance": instance,
        "consumed": {"instance": "cad2207-predecessor", "stale": null, "failed": null},
        "fenced": [],
        "unevidenced": []
    });

    // No record at all — the predecessor's drain is unverifiable.
    let _ = std::fs::remove_file(&record_path);
    assert_eq!(drain().as_deref(), Some("unverified"));

    // Unparseable JSON is evidence that never reached the writer.
    std::fs::write(&record_path, "{not json").expect("the fixture cannot corrupt the record");
    assert_eq!(drain().as_deref(), Some("unverified"));

    // A record naming another run is not this restart's evidence.
    let mut other = clean.clone();
    other["instance"] = json!("cad2207-not-this-run");
    record(&other);
    assert_eq!(drain().as_deref(), Some("unverified"));

    // Missing members, wrong types and non-strict rows: each is unknown
    // evidence, never clean.
    for bad in [
        json!({"instance": instance}),
        json!({"instance": instance, "consumed": null,
               "fenced": "unreadable", "unevidenced": []}),
        json!({"instance": instance, "consumed": null,
               "fenced": [{"alias": "cad2207-worker"}], "unevidenced": []}),
        json!({"instance": instance,
               "consumed": {"instance": "cad2207-predecessor"},
               "fenced": [], "unevidenced": []}),
        json!({"instance": instance,
               "consumed": {"instance": "cad2207-predecessor", "stale": null, "failed": true},
               "fenced": [], "unevidenced": []}),
        json!({"instance": instance, "consumed": "gone",
               "fenced": [], "unevidenced": []}),
    ] {
        record(&bad);
        assert_eq!(drain().as_deref(), Some("unverified"), "{bad}");
    }

    // Bound to this run but the predecessor drain failed.
    record(&json!({
        "instance": instance,
        "consumed": {"instance": "cad2207-predecessor", "stale": null, "failed": "drain exploded"},
        "fenced": [],
        "unevidenced": []
    }));
    assert_eq!(drain().as_deref(), Some("failed"));

    // Any fenced/unevidenced turn fails the verdict; the aggregate answer
    // must not echo the private identifiers the record carries.
    let private_message = "cad2207-private-recovery-turn";
    record(&json!({
        "instance": instance,
        "consumed": {"instance": "cad2207-predecessor", "stale": null, "failed": null},
        "fenced": [{"alias": alice.alias, "message_id": private_message}],
        "unevidenced": [{"alias": alice.alias, "message_id": private_message}]
    }));
    let fenced = fx
        .call(Asserted::Operator, "agent_fleet_state", json!({}))
        .expect("the operator's fleet-state gate was refused");
    assert_eq!(fenced["drain"].as_str(), Some("failed"), "{fenced}");
    assert!(
        !fenced.to_string().contains(alice.alias.as_str())
            && !fenced.to_string().contains(private_message),
        "the fleet aggregate echoed a private recovery identifier: {fenced}"
    );

    // The only clean shape: this run, a clean predecessor drain, nothing
    // fenced or unevidenced.
    record(&clean);
    let settled = fx
        .call(Asserted::Operator, "agent_fleet_state", json!({}))
        .expect("the operator's fleet-state gate was refused");
    assert_eq!(settled["drain"].as_str(), Some("clean"), "{settled}");
    assert!(
        !settled.to_string().contains(alice.alias.as_str()),
        "the clean fleet aggregate named the member's private assistant: {settled}"
    );
}
