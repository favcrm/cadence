// CAD-1128 app publication-stage + historical-accept boundary — REAL
// `Rig` socket daemon via `Rig::start_with_platforms`, REAL `social-content`
// `publication`/`text.publish` slot, REAL `app_*`/`platform_effect_*` RPC
// over the real socket. TEST-ONLY. Mount as
// `src/daemon/app_effects_rpc/cad1128_boundaries_tests.rs` (a `#[cfg(test)]`
// `mod` under `app_effects_rpc`'s test cfg). `Rig`/`fx::*`/
// `client::rpc_private_timeout`/`scoped`/`Asserted`/`register_agent`/
// `launch_actor`/`DeploymentMetadata`/`PlatformMap`/
// `store::app_effect_stage`/`app_publication_material`/`app_binding_create`/
// `BindingProof`/`authority_digest`/`material_digest`/`EffectRow` are
// `pub`/`pub(crate)`/`pub(super)`/`#[cfg(test)]`-reachable.
// UNCOMPILED/UNEXECUTED.
//
// PROVIDER FIXTURE (separately assigned independent-author authorship):
// the legacy-SMTP positive needs a REAL `execute_app_artifact` callback and
// a `released` outcome, but the merged CAD-1156
// `platform::smtp::test_artifact_adapter` helper is intentionally
// stage-only (it does not implement that hook, so the inherited default
// refuses). This successor therefore injects its OWN private, distinctly
// named TEST-ONLY execution-capable fixture
// (`cad1128_exec_smtp_fixture::Cad1128ExecSmtpArtifactAdapter`), which
// preserves the historical fixture's counted callback + deterministic
// in-memory receipt behavior. The merged helper is untouched and unwrapped;
// the synthetic provider compatibility is labeled in that fixture file. No
// generic compatibility facade exists and no real SMTP delivery occurs.
//
// ENCRYPTED RUN CREATION (current main): `app_run_create` resolves the
// publication binding and calls `refuse_encrypted_binding` before price
// discovery and before `app_run_create_attributed`. The encrypted test
// therefore proves the REAL RPC refusal (`unsupported_custody_use`) leaves no
// run/job/task/step/message row and no provider callback, then seeds a
// HISTORICAL succeeded run through the current private Store API
// (`app_run_create_attributed` + `app_run_decide`) with the encrypted binding
// frozen in its snapshot, and completes it with the real producer/reviewer
// turns. The stage-before-prepare/preview/persistence oracles and the
// historical waiting accept/decline oracles are unchanged.
//
// ENCRYPTED BINDING READINESS (current main): native `app_binding_create`
// refuses an encrypted connection because the projection derives
// `custody_available` through the default loader, which refuses encrypted
// references. The encrypted tests therefore seed the publication binding
// through the private Store API (metadata only, no custody read), exactly as
// they seed the historical run; the consumer-guard oracles are unchanged.
//
// THE BOUNDARY (app_effects_rpc.rs:155 + :252): `stage_app_artifact` and
// `app_release_current` (run before `app_effect_claim` on `decide accept`
// and `execute_app_artifact`) call
// `refuse_encrypted_binding(&proof.config)` — a binding whose
// `config.provider`/`account`/`connection_id` resolves to an ENCRYPTED
// credential refuses `unsupported_custody_use` BEFORE
// `prepare_app_artifact`/`preview`/new-effect/request/input/event/claim.
// A historical `waiting` row accepts via `app_release_current` — the same
// check runs; `decline`/`close` never reach it. The private execution
// fixture's `Counters` (`prepare`/`preview`/`execute`) observe ZERO
// callbacks on the refusal — a guarded-out late loader can't mask the
// screen.
//
// The historical encrypted `waiting` row is seeded through
// `store.app_effect_stage(&row, &authority)` — the Store-level write that
// inserts `platform_effects` + `app_effect_authorizations` directly — with
// the `authority`/`provenance`/`binding`/`material`/`artifact` computed from
// the REAL `social-content` `publication_run` + `app_binding_create`
// (encrypted smtp). `app_effect_stage` is the seam the writer owns: it does
// NOT run `refuse_encrypted_binding` (that's the daemon RPC's job), so the
// seed lands the `waiting` row the RPC can't produce. The `decide accept`
// then runs `app_release_current` → `refuse_encrypted_binding` →
// `unsupported_custody_use`, the row stays `waiting`, `decision` `NULL`.

#![cfg(all(unix, feature = "test-seam"))]

// PRIVATE execution-capable SMTP fixture (distinct from the merged,
// stage-only `platform::smtp::test_artifact_adapter` helper).
mod cad1128_exec_smtp_fixture;

use crate::daemon::cad1128_canary_fx as fx;
use crate::daemon::custody_acceptance_fixture::Rig;
use crate::platform::adapter::PlatformAdapter;
use crate::platform::deployments::DeploymentMetadata;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};
use std::time::Duration;

const A: &str = "cad1128-pub-a";
const B: &str = "cad1128-pub-b";

/// Isolated `DeploymentMetadata` `start_with_platforms` requires —
/// schema-1 with an EMPTY `providers` list: an isolated deployment set,
/// not a real provider origin/pin (no admission/lineage claim).
fn fixture_deployments() -> DeploymentMetadata {
    DeploymentMetadata::parse(br#"{"schema":1,"providers":[]}"#).unwrap()
}

/// `Rig::start_with_platforms` + `extra_platforms["smtp"]` (the private
/// execution-capable fixture) + `Some(DeploymentMetadata::parse(schema1))`.
/// Returns the concrete adapter Arc for the callback-counter oracle.
fn rig(
    encrypted: bool,
) -> (
    tempfile::TempDir,
    Rig,
    std::sync::Arc<cad1128_exec_smtp_fixture::Cad1128ExecSmtpArtifactAdapter>,
) {
    let dir = fx::new_root();
    let adapter =
        std::sync::Arc::new(cad1128_exec_smtp_fixture::Cad1128ExecSmtpArtifactAdapter::new());
    let mut platforms: super::effect_rpc::PlatformMap = Default::default();
    platforms.insert(
        "smtp".into(),
        adapter.clone() as std::sync::Arc<dyn crate::platform::adapter::PlatformAdapter>,
    );
    // Bound the `local` adapter to an OWNED outbox inside `dir` — a temporary
    // `ServeOptions` + `local::register_at(state,outbox,loopback board_url)`
    // builds a real `LocalAdapter` whose `outbox` is `dir/outbox`; moving its
    // `platforms["local"]` into `extra_platforms` overrides the `Rig`'s
    // default `XDG_DATA_HOME` `local` (`opts.platforms.extend` runs after
    // `local::register`), so the published `app-items/<effect_id>` lands under
    // `dir` — the canonical owned-path confinement assert stays exact.
    let mut opts = crate::daemon::ServeOptions::default();
    crate::platform::local::register_at(
        &dir.path().join("state"),
        &mut opts,
        dir.path().join("outbox"),
        "http://127.0.0.1:0".into(),
    );
    let local = opts
        .platforms
        .remove("local")
        .expect("register_at must install the local adapter");
    platforms.insert("local".into(), local);
    let rig = Rig::start_with_platforms(
        dir.path(),
        A.as_bytes(),
        B.as_bytes(),
        encrypted,
        platforms,
        Some(fixture_deployments()),
    )
    .expect("rig start_with_platforms failed");
    (dir, rig, adapter)
}

/// Operator RPC over the real socket — `state_dir` cloned, never `rig`.
fn op(rig: &Rig, method: &str, params: Value) -> Value {
    let state = rig.state_dir.clone();
    crate::test_seam::scoped(crate::test_seam::Asserted::Operator, move || {
        crate::client::rpc_private_timeout(&state, method, params, Duration::from_secs(30))
    })
    .unwrap()
}

/// Operator RPC returning `Result` — for refusals.
fn op_result(rig: &Rig, method: &str, params: Value) -> crate::error::Result<Value> {
    let state = rig.state_dir.clone();
    crate::test_seam::scoped(crate::test_seam::Asserted::Operator, move || {
        crate::client::rpc_private_timeout(&state, method, params, Duration::from_secs(30))
    })
}

/// Install `social-content` and return `(install_id, bundle_digest)` — the
/// digest the run snapshot, binding config and material all pin.
fn social_install_bundle(rig: &Rig) -> (String, String) {
    let out = op(
        rig,
        "app_workspace_install",
        json!({"source": format!("{}/tests/fixtures/apps/social-content", env!("CARGO_MANIFEST_DIR"))}),
    );
    op(
        rig,
        "app_local_install_approve",
        json!({"install_id": out["install_id"], "digest": out["digest"]}),
    );
    (
        out["install_id"].as_str().unwrap().to_string(),
        out["digest"].as_str().unwrap().to_string(),
    )
}

/// Install `social-content` (the real `text.publish` app).
fn social_install(rig: &Rig) -> String {
    social_install_bundle(rig).0
}

/// `claude`/`managed` lead/writer/reviewer — SIMULATED managed identities
/// (`generation:"g1"`, the `approved_intent` scheme) set via `set_identity`,
/// NO `launch_actor`/`fake_generation` — the fixture drives the turns itself
/// (`take_queued_app_proven`/`mark_running`/`finish`), so no actors compete
/// with the fixture-owned claims. `lead` is the owner-pm.
fn register_agents(rig: &Rig) {
    let shared = fx::shared(rig);
    for (alias, role) in [("lead", "pm"), ("writer", "worker"), ("reviewer", "worker")] {
        if shared.store.agent_opt(alias).unwrap().is_some() {
            continue;
        }
        shared
            .store
            .register_agent(&crate::store::NewAgent {
                alias,
                provider: "claude",
                endpoint_kind: "managed",
                role,
                cwd: "/tmp",
                sandbox: "read-only",
                instructions: None,
                params: Some("{\"upstream\":\"lead\"}"),
                team_role: None,
                model_policy: None,
            })
            .expect("register agent failed");
        // SIMULATED managed identity — `generation:"g1"` matches the token
        // `drive_turn` mints (`CLAUDE_MANAGED_TURN_TOKENS.mint("g1")`), so
        // `turn_token_current`/`local_token_current` validate the active
        // endpoint generation. A fixture identity, not a provider launch.
        let identity = crate::adapter::Identity {
            thread_id: "t".into(),
            session_id: "s".into(),
            model: None,
            effort: None,
            pid: 4242,
            endpoint: None,
            generation: Some("g1".into()),
            attach: None,
        };
        shared.store.set_identity(alias, &identity).unwrap();
    }
}

/// Drive one step's assigned turn to completion — the real producer/reviewer
/// path: `app_run_dispatch` queues the step's `message_id`;
/// `take_queued_app_proven` claims it (admission + current identity proof);
/// `mark_running` mints the simulated turn token; `store.finish` with
/// `status="completed"` + `{turn_id, text:typed LocalResult envelope}` runs
/// `app_run_finished_in` → `produce_text`/`review_text` — the REAL Store
/// material path, not a fake-provider result or fabricated succeeded row.
fn drive_turn(
    shared: &crate::daemon::Shared,
    run: &Value,
    step_idx: usize,
    alias: &str,
    bundle: &str,
    reply: Value,
    artifact: Option<&str>,
) {
    let message_id = run["steps"][step_idx]["message_id"].as_str().unwrap();
    let crate::store::Take::Message(taken) = shared
        .store
        .take_queued_app_proven(alias, Some((message_id, bundle)))
        .expect("app turn must be claimed")
    else {
        panic!("app turn must be claimed")
    };
    // SIMULATED turn token — the managed endpoint-generation scheme the
    // approved_intent test uses; a test fixture identity, not a real provider
    // launch or native/outside-agent result.
    let token = crate::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint("g1");
    shared
        .store
        .mark_running(&taken.id, &token)
        .expect("mark_running failed");
    let taken = shared.store.message(&taken.id).unwrap().unwrap();
    // A review `approve` is only material when the reviewer fetched THIS
    // artifact on the same assigned turn/token (`app_run_finished_in` checks
    // the fetch receipt). The producer step passes None.
    if let Some(artifact) = artifact {
        shared
            .store
            .app_artifact_with_digest(
                artifact,
                Some((&taken.id, taken.turn_id.as_deref().unwrap())),
                bundle,
            )
            .expect("guarded dependency fetch failed");
    }
    let reply = json!({"turn_id": taken.turn_id, "text": reply.to_string()});
    shared
        .store
        .finish(&taken, "completed", &reply, None)
        .expect("step completion failed");
}

/// `facebook` run → `succeeded` via real producer+reviewer turns → the ROOT
/// `artifacts` row — real RPC create/approve plus real `produce_text`/
/// `review_text` completions. Only for a rig whose publication binding is not
/// encrypted: current main refuses encrypted run creation.
fn publication_run(rig: &Rig, install: &str, request: &str, bundle: &str) -> (String, Value) {
    let run = op(
        rig,
        "app_run_create",
        json!({"install_id": install, "workflow": "facebook",
            "request_id": request, "owner_pm": "lead",
            "inputs": {"subject":"Ramen","source":"Kura Summer Ramen HK$88.",
                "writer":"writer","reviewer":"reviewer"}}),
    );
    let run_id = run["id"].as_str().unwrap().to_string();
    op(
        rig,
        "app_run_approve",
        json!({"run_id": run_id, "digest": run["snapshot_digest"]}),
    );
    let artifact = publication_turns(rig, &run_id, bundle, request);
    (run_id, artifact)
}

/// The real producer+reviewer turns for an approved `facebook` run, driven
/// through the Store exactly as the daemon dispatcher does (`produce_text`
/// then `review_text` on the run's own assigned messages). Returns the ROOT
/// `artifacts` row.
fn publication_turns(rig: &Rig, run_id: &str, bundle: &str, request: &str) -> Value {
    let shared = fx::shared(rig);
    let body = format!("# Post {request}\nReviewed copy.");
    // Produce turn — writer claims + completes with a typed `produce_text`
    // LocalResult envelope (`schema:1`/`kind`/`run_id`/`step_id`/`revision`/
    // `outcome=succeeded`/`artifacts`).
    let run = shared.store.app_run_dispatch(run_id, bundle).unwrap();
    drive_turn(
        &shared,
        &run,
        0,
        "writer",
        bundle,
        json!({"schema":1,"kind":"produce_text","run_id":run_id,"step_id":"s1","revision":1,
            "outcome":"succeeded","artifacts":[{"media_type":"text/markdown","text":body}]}),
        None,
    );
    // Review turn — reviewer claims + completes with a typed `review_text`
    // envelope (`producer_step_id`/`artifact_sha256`/`decision=approve`).
    let run = shared.store.app_run_dispatch(run_id, bundle).unwrap();
    let artifact_id = run["artifacts"][0]["id"].as_str().unwrap();
    let digest = crate::store::app_runs::artifact_digest(body.as_bytes());
    drive_turn(
        &shared,
        &run,
        1,
        "reviewer",
        bundle,
        json!({"schema":1,"kind":"review_text","run_id":run_id,"step_id":"s2","revision":1,
            "producer_step_id":"s1","producer_revision":1,"artifact_sha256":digest,
            "decision":"approve","rationale":"Checked the exact artifact."}),
        Some(artifact_id),
    );
    // After both turns the Store's `app_run_finished_in`/`produce_text`/
    // `review_text` completes the run — read the now-succeeded `artifacts`.
    let shown = op(rig, "app_run_show", json!({"run_id": run_id}));
    assert_eq!(shown["state"], "succeeded", "run must complete: {shown}");
    shown["artifacts"][0].clone()
}

/// A HISTORICAL encrypted publication run, seeded through the CURRENT
/// private Store API (`app_run_create_attributed`, not the RPC that now
/// refuses encrypted bindings) and then completed by the REAL
/// producer+reviewer turns. The frozen snapshot still carries the encrypted
/// publication binding, so the real stage/decide custody guards are what the
/// boundary tests observe. Test-only setup, not a production seam.
fn seeded_encrypted_publication_run(
    rig: &Rig,
    install: &str,
    bound: &Value,
    request: &str,
) -> (String, Value) {
    let shared = fx::shared(rig);
    let bundle = bound["config"]["bundle_digest"].as_str().unwrap();
    let proof: crate::store::app_bindings::BindingProof =
        serde_json::from_value(bound.clone()).unwrap();
    // The same effective inputs the RPC freezes for this workflow (the
    // optional `content_prompt` default included); optional inputs left
    // absent render empty.
    let inputs: std::collections::BTreeMap<String, String> = [
        ("subject", "Ramen"),
        ("source", "Kura Summer Ramen HK$88."),
        ("writer", "writer"),
        ("reviewer", "reviewer"),
        (
            "content_prompt",
            "Write a concise zh-HK caption grounded only in the source facts.",
        ),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect();
    let workflow_text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/apps/social-content/workflows/facebook.md"),
    )
    .expect("facebook workflow fixture readable");
    let workflow = crate::store::app_runs::LocalWorkflow::parse(&workflow_text, &inputs)
        .expect("facebook workflow parses for the historical seed");
    let created = shared
        .store
        .app_run_create_attributed(
            crate::store::app_runs::LocalRunRequest {
                install_id: install,
                bundle_digest: bundle,
                workflow: &workflow,
                inputs: &inputs,
                request_id: request,
                owner_pm: Some("lead"),
                project_link: None,
            },
            None,
            Some(&proof),
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
            crate::store::app_runs::LocalRunProvenance {
                selected_source: None,
                input_origins: &std::collections::BTreeMap::new(),
                human: None,
            },
            None,
        )
        .expect("historical encrypted run seed");
    let run_id = created["id"].as_str().unwrap().to_string();
    shared
        .store
        .app_run_decide(
            &run_id,
            Some(created["snapshot_digest"].as_str().unwrap()),
            false,
            Some(bundle),
        )
        .expect("historical encrypted run approval");
    let artifact = publication_turns(rig, &run_id, bundle, request);
    (run_id, artifact)
}

/// `app_binding_create` → the binding (`{"binding":{...,"config":{...}}}`).
fn bind(rig: &Rig, install: &str, connection_id: &str) -> Value {
    op(
        rig,
        "app_binding_create",
        json!({"install_id": install, "slot": "publication",
            "connection_id": connection_id, "request_id": "bind-1"}),
    )["binding"]
        .clone()
}

/// Seed the publication binding through the CURRENT private Store API for an
/// encrypted connection. Current main's native `app_binding_create` refuses
/// encrypted readiness (the connection projection marks `custody_available`
/// via the default loader), so the historical encrypted tests use this
/// metadata binding: `binding_current_in`/material validate it and the
/// consumer guards still classify the live encrypted target.
fn seed_encrypted_binding(
    rig: &Rig,
    adapter: &cad1128_exec_smtp_fixture::Cad1128ExecSmtpArtifactAdapter,
    install: &str,
    bundle: &str,
    connection_id: &str,
) -> Value {
    let shared = fx::shared(rig);
    let record = rig.core_record(connection_id).expect("encrypted record");
    let descriptor = adapter.connection_descriptor().expect("fixture descriptor");
    let mapping = descriptor
        .resolve_action("text.publish", 1, "publish", "connection_account")
        .expect("fixture publication mapping");
    let declaration = {
        let manifest_text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/apps/social-content/app.md"),
        )
        .expect("social app manifest readable");
        let manifest = crate::issue::app::parse_manifest(&manifest_text).expect("manifest parses");
        serde_json::to_value(&manifest.capabilities["publication"]).expect("declaration")
    };
    let sink_registration = adapter.connection_registration().unwrap_or_default();
    let registration = crate::platform::connections::registration_digest(&format!(
        "{sink_registration}:{}",
        serde_json::to_string(&descriptor).unwrap_or_default()
    ));
    let config = json!({
        "schema": 1,
        "install_id": install,
        "context": Value::Null,
        "bundle_digest": bundle,
        "workspace_id": shared.store.connection_workspace_id().expect("workspace id"),
        "connection_id": record.connection_id,
        "provider": record.platform,
        "account": record.account,
        "connection_kind": "enrolled",
        "connection_revision": record.credential_revision,
        "registration_digest": registration,
        "sink_registration": sink_registration,
        "descriptor_revision": descriptor.revision,
        "reviewed_pin": adapter.table().manifest_version.clone().unwrap_or_default(),
        "reported_pin": adapter.reported_manifest_version().unwrap_or_default(),
        "mapping": mapping,
        "declaration": declaration,
    });
    shared
        .store
        .app_binding_create(install, None, "publication", &config, "bind-1")
        .expect("historical encrypted binding seed");
    let proof = shared
        .store
        .app_binding_for_slot(install, None, "publication", bundle)
        .expect("binding read")
        .expect("seeded binding");
    serde_json::to_value(proof).expect("binding proof")
}

/// `local`/`local` builtin connection id.
fn local_builtin(rig: &Rig) -> String {
    let rows = op(rig, "connection_list", json!({}))["connections"].clone();
    rows.as_array()
        .unwrap()
        .iter()
        .find(|row| row["provider"] == "local" && row["account"] == "local")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Enroll a `custody:"file"` smtp connection — `fx::enroll_smtp`.
fn enroll_legacy_smtp(rig: &Rig, account: &str) -> String {
    fx::enroll_smtp(rig, account, A)
}

/// Enroll an encrypted smtp connection — `fx::unlock_vault` ONCE (the vault
/// is then open), then `connection_create` → `encrypted-sqlite-v1:` record.
/// `custody` is `encrypted-sqlite-v1:` at create (the binding's
/// `binding_encrypted` check reads the record `custody`, not the lock
/// state), so no `connection_custody_lock` is needed — the connection stays
/// `custody_available=true` through the binding/stage/decide boundary.
fn enroll_encrypted_smtp(rig: &Rig, account: &str) -> String {
    fx::unlock_vault(rig); // initialize + unlock once — vault is open after this.
    let created = op(
        rig,
        "connection_create",
        json!({"provider":"smtp","account":account,"shape":"smtp",
            "host":"smtp.example","port":465,"tls_mode":"implicit",
            "username":"u","secret":"app-password",
            "sender":"sender@example.test","scopes":["email:send"],
            "accept_same_uid_risk":true}),
    );
    created["connection"]["id"].as_str().unwrap().to_string()
}

/// `app_publication_material` — the run's publication material (binding,
/// artifact, producer/review receipts) bound to `bundle_digest`/`slot`.
fn publication_material(
    rig: &Rig,
    run_id: &str,
    artifact_id: &str,
    bundle: &Value,
    slot: &str,
) -> Value {
    fx::shared(rig)
        .store
        .app_publication_material(
            run_id,
            artifact_id,
            bundle["digest"].as_str().unwrap(),
            slot,
        )
        .unwrap()
}

/// The `platform_effects` `(state, close_reason)` for `request` — a real
/// read; `Ok`/`Err` never a disguised `0`. `None` = no row.
fn effect_state(rig: &Rig, request: &str) -> Option<(String, Option<String>)> {
    Connection::open_with_flags(&rig.core_db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("core db")
        .query_row(
            "SELECT state, close_reason FROM platform_effects WHERE request=?",
            [request],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .expect("effect_state read failed")
}

/// Row count for a Store table — a real read used by the no-residue oracle.
fn table_count(rig: &Rig, table: &str) -> i64 {
    Connection::open_with_flags(&rig.core_db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("core db")
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .expect("table count read failed")
}

/// `app_runs` rows for one installation request — must stay ZERO after a
/// refused encrypted run creation.
fn app_run_rows(rig: &Rig, install: &str, request: &str) -> i64 {
    Connection::open_with_flags(&rig.core_db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("core db")
        .query_row(
            "SELECT COUNT(*) FROM app_runs WHERE install_id=? AND request_id=?",
            [install, request],
            |r| r.get(0),
        )
        .expect("app_runs read failed")
}

/// `platform_effects` `decision` for `request` — the durable-decision slot.
fn effect_decision(rig: &Rig, request: &str) -> Option<Value> {
    Connection::open_with_flags(&rig.core_db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("core db")
        .query_row(
            "SELECT decision FROM platform_effects WHERE request=?",
            [request],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .expect("effect_decision read failed")
        .flatten()
        .map(|s| serde_json::from_str(&s).expect("decision column is not valid json"))
}

/// `events` rows for `request` — `request_closed`/`platform`-event residue.
fn request_event_count(rig: &Rig, request: &str) -> i64 {
    Connection::open_with_flags(&rig.core_db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("core db")
        .query_row(
            "SELECT COUNT(*) FROM events WHERE json_extract(payload,'$.request')=?",
            [request],
            |r| r.get(0),
        )
        .expect("event count read failed")
}

/// The derived `request` `app_effect_stage` writes — `app-release-` +
/// `Uuid::new_v5(NAMESPACE_OID, "{install}:{request_id}")`. A real
/// `Uuid` computation — the request the stage persists.
fn derived_request(install: &str, request_id: &str) -> String {
    format!(
        "app-release-{}",
        uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            format!("{install}:{request_id}").as_bytes()
        )
        .simple()
    )
}

// ==================== the boundaries ====================

/// LEGACY `local`/`local` positive — `social-content` `publication`/
/// `text.publish` on a NON-encrypted rig: real `Local` `prepare`/`preview`/
/// `stage`→`waiting`, `decide accept` → `app_release_current` →
/// `app_effect_claim` → `execute_app_artifact` → the real `local`
/// `publish_app_text` outbox receipt.
#[test]
fn local_builtin_artifact_stage_and_accept_publishes() {
    let (dir, rig, _adapter) = rig(false);
    register_agents(&rig);
    let install = social_install(&rig);
    let conn = local_builtin(&rig);
    let bound = bind(&rig, &install, &conn);
    assert_eq!(bound["config"]["provider"], "local");
    let (run, artifact) = publication_run(
        &rig,
        &install,
        "r-legacy",
        bound["config"]["bundle_digest"].as_str().unwrap(),
    );
    let artifact_id = artifact["id"].as_str().unwrap().to_string();
    let staged = op(
        &rig,
        "app_effect_stage",
        json!({"run_id": run, "artifact_id": artifact_id,
            "slot": "publication", "request_id": "rel-1",
            "title": "Facebook caption"}),
    );
    let effect_id = staged["effect"]["effect_id"].as_str().unwrap().to_string();
    let shown = op(&rig, "app_effect_show", json!({"effect_id": effect_id}));
    assert_eq!(shown["effect"]["state"], "waiting", "{shown}");
    assert_eq!(shown["effect"]["authorization_kind"], "app_artifact");
    let decided = op(
        &rig,
        "app_effect_decide",
        json!({"effect_id": effect_id,
            "digest": shown["effect"]["digest"], "decision": "accept"}),
    );
    // `app_release` stores `{kind:"released",result,verified}` — assert the
    // released kind + the real `local` `execute_app_artifact` result.
    let outcome = &decided["effect"]["record"]["outcome"];
    assert_eq!(
        outcome["kind"], "released",
        "legacy accept must release: {decided}"
    );
    // The real `local` `app_text::execute` returns `result.path` =
    // `<outbox>/app-items/<effect_id>` — canonicalize it and require it sits
    // inside the canonical OWNED outbox root (`dir/outbox`, bound via
    // `local::register_at` into `extra_platforms["local"]`) BEFORE any file
    // read — the real owned-path confinement, not a mere suffix check.
    let result = &outcome["result"];
    assert!(
        result["platform_ref"]
            .as_str()
            .unwrap()
            .starts_with("outbox/app-items/"),
        "local publish must record the app-items platform_ref: {result}"
    );
    let canonical = std::fs::canonicalize(result["path"].as_str().unwrap())
        .expect("result.path must exist on disk");
    let outbox = std::fs::canonicalize(dir.path().join("outbox")).expect("owned outbox root");
    assert!(
        canonical.starts_with(&outbox),
        "local publish path {canonical:?} escapes the owned outbox root {outbox:?}"
    );
    assert!(
        canonical.ends_with(format!("app-items/{effect_id}")),
        "local publish path is not the effect's app-items dir: {canonical:?}"
    );
    // The authoritative approved material — the staged effect's record.input
    // (`schema:1`/`title`/`body`/provenance). `post.md` renders exactly
    // `# {title}\n\n{body}`; `index.json` carries the effect id + provenance.
    let record_input = &shown["effect"]["record"]["input"];
    let title = record_input["title"].as_str().unwrap();
    let body = record_input["body"].as_str().unwrap();
    let post = std::fs::read_to_string(canonical.join("post.md")).unwrap();
    assert_eq!(
        post,
        format!("# {title}\n\n{body}"),
        "post.md does not render the exact approved title+body"
    );
    let index: Value =
        serde_json::from_str(&std::fs::read_to_string(canonical.join("index.json")).unwrap())
            .unwrap();
    assert_eq!(index["effect_id"], effect_id, "outbox index effect_id");
    assert_eq!(
        index["provenance"], record_input["provenance"],
        "outbox index provenance differs from the staged input's provenance"
    );
    rig.finish().unwrap();
}

/// ELIGIBLE LEGACY smtp twin — `custody:"file"` smtp bound to `publication`/
/// `text.publish` on a NON-encrypted rig via the PRIVATE execution-capable
/// fixture `Cad1128ExecSmtpArtifactAdapter` (the merged CAD-1156 helper is
/// stage-only and is not used here): `resolve_action` passes (fixture
/// declares `text.publish`), `prepare`/`preview`/`execute` OBSERVED
/// (`counters` advance), `decide accept` → real `execute_app_artifact` →
/// the fixture's deterministic in-memory receipt (no real SMTP delivery).
#[test]
fn legacy_smtp_artifact_stage_and_accept_observes_callbacks() {
    let (_dir, rig, adapter) = rig(false);
    register_agents(&rig);
    let install = social_install(&rig);
    let conn = enroll_legacy_smtp(&rig, "leg-acct");
    assert!(
        !fx::is_encrypted(&rig, &conn).unwrap(),
        "legacy must be file custody"
    );
    let bound = bind(&rig, &install, &conn);
    assert_eq!(bound["config"]["provider"], "smtp");
    let (run, artifact) = publication_run(
        &rig,
        &install,
        "r-leg-smtp",
        bound["config"]["bundle_digest"].as_str().unwrap(),
    );
    let artifact_id = artifact["id"].as_str().unwrap().to_string();
    let staged = op(
        &rig,
        "app_effect_stage",
        json!({"run_id": run, "artifact_id": artifact_id,
            "slot": "publication", "request_id": "rel-leg-smtp",
            "title": "Legacy smtp caption"}),
    );
    let effect_id = staged["effect"]["effect_id"].as_str().unwrap().to_string();
    let (p, v, _e) = adapter.counters().snapshot();
    assert!(p > 0 && v > 0, "prepare/preview callbacks not observed");
    let shown = op(&rig, "app_effect_show", json!({"effect_id": effect_id}));
    assert_eq!(shown["effect"]["state"], "waiting");
    let decided = op(
        &rig,
        "app_effect_decide",
        json!({"effect_id": effect_id,
            "digest": shown["effect"]["digest"], "decision": "accept"}),
    );
    let (_p2, _v2, e2) = adapter.counters().snapshot();
    assert!(e2 > 0, "execute callback not observed");
    let outcome = &decided["effect"]["record"]["outcome"];
    assert_eq!(
        outcome["kind"], "released",
        "legacy smtp accept must release: {decided}"
    );
    // The fixture's `execute_app_artifact` simulated receipt is
    // `outcome.result` — the original released-receipt oracle, unchanged.
    assert!(
        !outcome["result"].is_null(),
        "legacy smtp accept has no result: {decided}"
    );
    rig.finish().unwrap();
}

/// AUTHENTIC negative — `encrypted-sqlite-v1:` smtp bound to the SAME
/// `publication`/`text.publish` slot on an ENCRYPTED rig:
/// `refuse_encrypted_binding` fires `unsupported_custody_use` BEFORE
/// prepare/preview/new-effect/request/input/event/claim. `Counters` stay
/// `(0,0,0)`; `platform_effects`/`events` residue stays absent under the
/// DERIVED `app-release-{uuid5}` request the stage persists.
#[test]
fn encrypted_smtp_artifact_binding_refuses_unsupported_custody_use() {
    let (_dir, rig, adapter) = rig(true);
    register_agents(&rig);
    let (install, bundle) = social_install_bundle(&rig);
    let conn = enroll_encrypted_smtp(&rig, "enc-acct");
    let bound = seed_encrypted_binding(&rig, &adapter, &install, &bundle, &conn);
    assert_eq!(bound["config"]["provider"], "smtp");

    // CURRENT guard: a real encrypted `app_run_create` must refuse
    // `unsupported_custody_use` BEFORE any provider callback and BEFORE the
    // run/input/job/task/message persistence. Snapshot every count first.
    let (p0, v0, e0) = adapter.counters().snapshot();
    let runs0 = table_count(&rig, "app_runs");
    let jobs0 = table_count(&rig, "jobs");
    let tasks0 = table_count(&rig, "tasks");
    let steps0 = table_count(&rig, "app_run_steps");
    let messages0 = table_count(&rig, "messages");
    let refusal = op_result(
        &rig,
        "app_run_create",
        json!({"install_id": install, "workflow": "facebook",
            "request_id": "r-enc", "owner_pm": "lead",
            "inputs": {"subject":"Ramen","source":"Kura Summer Ramen HK$88.",
                "writer":"writer","reviewer":"reviewer"}}),
    );
    // Assert the forbidden work/state BEFORE extracting the refusal.
    let (p1, v1, e1) = adapter.counters().snapshot();
    assert_eq!(
        (p1, v1, e1),
        (p0, v0, e0),
        "encrypted run creation ran a provider callback"
    );
    assert_eq!(
        table_count(&rig, "app_runs"),
        runs0,
        "refused encrypted run creation persisted a run"
    );
    assert_eq!(
        table_count(&rig, "jobs"),
        jobs0,
        "refused encrypted run creation persisted a job"
    );
    assert_eq!(
        table_count(&rig, "tasks"),
        tasks0,
        "refused encrypted run creation persisted a task"
    );
    assert_eq!(
        table_count(&rig, "app_run_steps"),
        steps0,
        "refused encrypted run creation persisted a step"
    );
    assert_eq!(
        table_count(&rig, "messages"),
        messages0,
        "refused encrypted run creation queued a turn"
    );
    assert_eq!(
        app_run_rows(&rig, &install, "r-enc"),
        0,
        "refused encrypted run creation left its run row"
    );
    let err = refusal.expect_err("encrypted binding must refuse app_run_create");
    assert_eq!(
        err.code(),
        Some("unsupported_custody_use"),
        "encrypted run creation must refuse unsupported_custody_use: {err}"
    );

    // HISTORICAL run seeded through the current private Store API (the real
    // RPC now correctly refuses earlier), completed by the real producer and
    // reviewer turns. Then retain the ORIGINAL encrypted stage oracle:
    // prepare/preview/persistence must not run.
    let (run, artifact) = seeded_encrypted_publication_run(&rig, &install, &bound, "r-enc-hist");
    let artifact_id = artifact["id"].as_str().unwrap().to_string();
    let request_id = "rel-enc";
    let derived = derived_request(&install, request_id);
    let (p0, v0, e0) = adapter.counters().snapshot();
    let events0 = request_event_count(&rig, &derived);
    let refusal = op_result(
        &rig,
        "app_effect_stage",
        json!({"run_id": run, "artifact_id": artifact_id,
            "slot": "publication", "request_id": request_id,
            "title": "Encrypted caption"}),
    );
    // Assert the forbidden work/state BEFORE extracting the refusal — a
    // source mutation must fail at the real forbidden-work/persistence
    // assertion, not an unrelated error shape or a premature expect_err.
    let (p1, v1, e1) = adapter.counters().snapshot();
    let row_present = effect_state(&rig, &derived).is_some();
    let events_after = request_event_count(&rig, &derived);
    assert_eq!(
        (p1, v1, e1),
        (p0, v0, e0),
        "encrypted refusal ran a prepare/preview/execute callback"
    );
    assert!(!row_present, "a refused stage left a platform_effects row");
    assert_eq!(events_after, events0, "encrypted refusal emitted an event");
    let err = refusal.expect_err("encrypted binding must refuse stage");
    assert_eq!(
        err.code(),
        Some("unsupported_custody_use"),
        "encrypted stage must refuse unsupported_custody_use: {err}"
    );
    rig.finish().unwrap();
}

/// Seed a historical `waiting` `platform_effects` row bound to the encrypted
/// smtp binding — `store.app_effect_stage(&row, &authority)` (the Store-level
/// write, which does NOT run `refuse_encrypted_binding`). `authority`/
/// `provenance`/`row` computed exactly as `stage_app_artifact` builds them —
/// real material-bound values from `publication_run` + `app_binding_create` +
/// `app_publication_material`. Returns `(derived_request, authority)`.
fn seed_encrypted_waiting(
    rig: &Rig,
    install: &str,
    bound: &Value,
    run: &str,
    artifact_id: &str,
    effect_id: &str,
    request_id: &str,
) -> (String, Value) {
    let bundle = json!({"digest": bound["config"]["bundle_digest"]});
    let material = publication_material(rig, run, artifact_id, &bundle, "publication");
    let proof: crate::store::app_bindings::BindingProof =
        serde_json::from_value(bound.clone()).unwrap();
    let run_shown = op(rig, "app_run_show", json!({"run_id": run}));
    let artifact_digest = material["artifact"]["digest"].as_str().unwrap().to_string();
    let mut authority = json!({"schema":1,"install_id":install,"context":run_shown["snapshot"]["context"],
        "run_id":run,"run_snapshot_digest":run_shown["snapshot_digest"],"epoch":run_shown["epoch"],
        "bundle_digest":bundle["digest"],"artifact_id":artifact_id,"artifact_digest":artifact_digest,
        "slot":"publication","binding":proof,"material_digest":crate::store::app_runs::material_digest(&material),
        "producer_receipt_digest":crate::store::app_runs::material_digest(&material["producer_receipt"]),
        "review_receipt_digest":crate::store::app_runs::material_digest(&material["review_receipt"])});
    if let Some(asset) = material.get("asset") {
        authority["asset"] = asset.clone();
    }
    let core_digest = crate::store::app_effects::authority_digest(&authority);
    let provenance = json!({"schema":1,"authorization_kind":"app_artifact","effect_id":effect_id,
        "authority_digest":core_digest,"install_id":install,"context_id":run_shown["context_id"],
        "context_revision":run_shown["snapshot"]["context"]["revision"],
        "context_digest":run_shown["snapshot"]["context"]["digest"],"run_id":run,
        "run_snapshot_digest":run_shown["snapshot_digest"],
        "artifact_id":artifact_id,"artifact_digest":artifact_digest,"binding_id":proof.id,
        "binding_revision":proof.revision,"binding_digest":proof.digest,
        "connection_id":proof.config["connection_id"],"connection_kind":proof.config["connection_kind"],
        "connection_revision":proof.config["connection_revision"],"registration_digest":proof.config["registration_digest"],
        "sink_registration":proof.config["sink_registration"],"mapping":proof.config["mapping"],
        "review_receipt_digest":authority["review_receipt_digest"]});
    if let Some(asset) = material.get("asset") {
        authority["provenance"] = provenance.clone();
        authority["provenance"]["asset"] = asset.clone();
    } else {
        authority["provenance"] = provenance;
    }
    let derived = derived_request(install, request_id);
    let row = crate::store::EffectRow {
        effect_id: effect_id.into(),
        request: derived.clone(),
        agent: run_shown["snapshot"]["owner_pm"].as_str().unwrap().into(),
        platform: "smtp".into(),
        account: proof.config["account"].as_str().unwrap().into(),
        tool: proof.config["mapping"]["tool"].as_str().unwrap().into(),
        label: None,
        input: json!({"schema":1,"title":"Historical caption",
            "tool":proof.config["mapping"]["tool"],
            "body":material["artifact"]["text"],"provenance":authority["provenance"]}),
        input_summary: "Historical caption".into(),
        preview: "preview text".into(),
        source_name: None,
        source_hash: Some(artifact_digest.clone()),
        scopes: serde_json::from_value(proof.config["mapping"]["scopes"].clone()).unwrap(),
        task: None,
        state: "waiting".into(),
        close_reason: None,
        decision: None,
        outcome: None,
        needs_you: false,
        staged_at: 0.0,
        updated_at: 0.0,
    };
    fx::shared(rig)
        .store
        .app_effect_stage(&row, &authority)
        .unwrap();
    (derived, authority)
}

/// HISTORICAL encrypted `waiting` row — `app_effect_stage` (the Store-level
/// write, which does NOT run `refuse_encrypted_binding`) seeds a `waiting`
/// row bound to the encrypted smtp connection with REAL `authority`/
/// `provenance`/`binding`/`material`/`artifact` (computed from the real
/// `publication_run` + `app_binding_create` + `app_publication_material`).
/// `decide accept` → `app_release_current` → `refuse_encrypted_binding` →
/// `unsupported_custody_use`; the row stays `waiting`, `decision` `NULL`.
/// `decline`/`close` stay allowed — the guard never runs on those paths.
#[test]
fn historical_encrypted_waiting_row_refuses_accept_waiting_null() {
    let (_dir, rig, adapter) = rig(true);
    register_agents(&rig);
    let (install, bundle) = social_install_bundle(&rig);
    let conn = enroll_encrypted_smtp(&rig, "enc-hist");
    let bound = seed_encrypted_binding(&rig, &adapter, &install, &bundle, &conn);
    assert_eq!(bound["config"]["provider"], "smtp");
    let (run, artifact) = seeded_encrypted_publication_run(&rig, &install, &bound, "r-hist-enc");
    let artifact_id = artifact["id"].as_str().unwrap().to_string();
    let (derived, _authority) = seed_encrypted_waiting(
        &rig,
        &install,
        &bound,
        &run,
        &artifact_id,
        "eff-hist-enc",
        "rel-hist-enc",
    );
    // `app_effect_show` recomputes + validates the child's `release_digest` —
    // use the envelope's real `effect.digest` for the decide CAS.
    let shown = op(
        &rig,
        "app_effect_show",
        json!({"effect_id": "eff-hist-enc"}),
    );
    // Baseline: the Store `app_effect_stage` seed already emitted the
    // legitimate `effect_requested` event for `derived` — capture the count
    // and require it exists. The refused `decide accept` must add NO new
    // request-scoped event.
    let events_before = request_event_count(&rig, &derived);
    assert!(
        events_before > 0,
        "the seeded stage emitted no request-scoped event — baseline uncalibrated"
    );
    let (p0, v0, e0) = adapter.counters().snapshot();
    let refusal = op_result(
        &rig,
        "app_effect_decide",
        json!({"effect_id": "eff-hist-enc",
            "digest": shown["effect"]["digest"], "decision": "accept"}),
    );
    // Assert the forbidden state/decision/outcome/event/callback BEFORE
    // extracting the refusal — a mutation must fail at the real forbidden
    // work/state, not an unrelated error shape or a premature expect_err.
    let (state, reason) = effect_state(&rig, &derived).unwrap_or_default();
    let decision_present = effect_decision(&rig, &derived).is_some();
    let outcome_present =
        Connection::open_with_flags(&rig.core_db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("core db")
            .query_row(
                "SELECT outcome IS NOT NULL FROM platform_effects WHERE request=?",
                [derived.as_str()],
                |r| r.get::<_, bool>(0),
            )
            .optional()
            .expect("outcome read failed")
            .unwrap_or(false);
    let events_after = request_event_count(&rig, &derived);
    let (p1, v1, e1) = adapter.counters().snapshot();
    assert_eq!(state, "waiting", "refused accept changed the row state");
    assert!(
        reason.is_none(),
        "refused accept wrote a close_reason: {reason:?}"
    );
    assert!(!decision_present, "a refused accept wrote a decision");
    assert!(!outcome_present, "a refused accept wrote an outcome");
    assert_eq!(
        (p1, v1, e1),
        (p0, v0, e0),
        "encrypted refusal ran a prepare/preview/execute callback"
    );
    assert_eq!(
        events_after, events_before,
        "a refused accept emitted a request-scoped event beyond the seed's own"
    );
    let err = refusal.expect_err("encrypted historical accept must refuse");
    assert_eq!(
        err.code(),
        Some("unsupported_custody_use"),
        "historical encrypted accept must refuse unsupported_custody_use: {err}"
    );
    rig.finish().unwrap();
}

/// DECLINE on the historical ENCRYPTED smtp `waiting` row stays allowed —
/// `decline`/`close` never reach `refuse_encrypted_binding`. Reuses the real
/// historical seed (valid `authority`/`provenance`/material bound to the
/// encrypted connection).
#[test]
fn decline_on_historical_encrypted_waiting_row_allowed() {
    let (_dir, rig, adapter) = rig(true);
    register_agents(&rig);
    let (install, bundle) = social_install_bundle(&rig);
    let conn = enroll_encrypted_smtp(&rig, "enc-dec");
    let bound = seed_encrypted_binding(&rig, &adapter, &install, &bundle, &conn);
    assert_eq!(bound["config"]["provider"], "smtp");
    let (run, artifact) = seeded_encrypted_publication_run(&rig, &install, &bound, "r-dec-enc");
    let artifact_id = artifact["id"].as_str().unwrap().to_string();
    let (derived, _authority) = seed_encrypted_waiting(
        &rig,
        &install,
        &bound,
        &run,
        &artifact_id,
        "eff-dec-enc",
        "rel-dec-enc",
    );
    let shown = op(&rig, "app_effect_show", json!({"effect_id": "eff-dec-enc"}));
    // Snapshot the execute counter BEFORE the decline — `decline`/`close` must
    // not run `execute_app_artifact` on the adapter.
    let (p0, v0, e0) = adapter.counters().snapshot();
    op(
        &rig,
        "app_effect_decide",
        json!({"effect_id": "eff-dec-enc",
            "digest": shown["effect"]["digest"], "decision": "decline"}),
    );
    let (p1, v1, e1) = adapter.counters().snapshot();
    // Decline is a durable terminal transition, not the legacy generic
    // `closed` close: `Store::effect_decide(accept=false)` writes
    // `state='declined'` plus the decision column and emits the decided
    // event; `app_effect_decide` then returns the envelope with no
    // release/claim/execute. Assert that durable decline + decision, and NO
    // execution residue.
    let (state, _reason) = effect_state(&rig, &derived).unwrap_or_default();
    let outcome_present =
        Connection::open_with_flags(&rig.core_db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("core db")
            .query_row(
                "SELECT outcome IS NOT NULL FROM platform_effects WHERE request=?",
                [derived.as_str()],
                |r| r.get::<_, bool>(0),
            )
            .optional()
            .expect("outcome read failed")
            .unwrap_or(false);
    assert_eq!(
        state, "declined",
        "decline must land the durable declined transition"
    );
    assert!(
        effect_decision(&rig, &derived).is_some(),
        "decline must record a durable decision"
    );
    assert!(
        !outcome_present,
        "a declined effect wrote an outcome — execution ran"
    );
    assert_eq!(
        (p1, v1, e1),
        (p0, v0, e0),
        "decline ran a prepare/preview/execute callback"
    );
    rig.finish().unwrap();
}
