//! CAD-1156 independent assertions, fresh-main 4e3f729c API baseline.
//! Mount as daemon::app_runs_rpc::cad1156_tests under the cfg below.
//! The in-process Shared::new + dispatch rig owns the real custody mutex,
//! Store and backend; post-dispatch inspection holds THAT mutex. No second
//! Shared, new seam, encrypted manager or archived CAD-1128 rig is used.
//!
//! SIMULATED claude/managed g1 identities and guarded Store turn completion
//! produce/review real publication material. No actors or providers launch.
//! SMTP custody/record setup uses existing canonical custody_bytes, put and
//! Store::platform_enroll under C, not network connection verification.
//! The adapter's text.publish capability/mapping is TEST-ONLY metadata, not
//! shipping SMTP publication support. Five checks only; short-password
//! outcome/error, malformed SMTP, opaque-provider and native/live-provider
//! acceptance remain outside this package. No secret-bearing failure output.
#![cfg(all(test, unix, feature = "test-seam"))]

use crate::daemon::{ServeOptions, Shared};
use crate::platform::smtp::test_artifact_adapter::SmtpArtifactAdapter;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::process::id as pid;
use std::sync::Arc;

const A: &str = "cad1156-smtp-a";

/// Unlike Result::expect/unwrap this never Debug-renders an error.
fn checked<T, E>(result: std::result::Result<T, E>, message: &'static str) -> T {
    match result {
        Ok(value) => value,
        Err(_) => panic!("{message}"),
    }
}

struct Fx {
    dir: tempfile::TempDir,
    shared: Arc<Shared>,
    smtp_adapter: Arc<SmtpArtifactAdapter>,
}

impl Fx {
    fn new(preview_leak: Option<Vec<u8>>) -> Self {
        let dir = checked(
            tempfile::Builder::new().prefix("c1156").tempdir(),
            "isolated directory creation failed",
        );
        checked(
            crate::issue::Pm::init(&dir.path().join("pm")),
            "PM initialization failed",
        );
        let mut opts = ServeOptions {
            provider_env: crate::adapter::ProviderEnv::refusing_providers(),
            test_seam: true,
            ..ServeOptions::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", dir.path().join("pm").to_str().unwrap());
        crate::platform::smtp::attach(&mut opts);
        let mut adapter = SmtpArtifactAdapter::default();
        if let Some(bytes) = preview_leak {
            adapter = adapter.with_preview_leak(bytes);
        }
        let smtp_adapter = Arc::new(adapter);
        opts.platforms.insert("smtp".into(), smtp_adapter.clone());
        let shared = checked(
            Shared::new(dir.path(), &opts),
            "Shared initialization failed",
        );
        assert!(
            shared.platform_custody.tag() == crate::platform::custody::FILE_TAG,
            "SETUP/ISOLATION: SMTP fixture requires private FILE custody before custody operations"
        );
        let cwd = dir.path().to_str().unwrap();
        // Explicitly SIMULATED managed identities consistent with the g1
        // tokens below. set_identity records fixture state only; no launch.
        for (alias, role) in [("lead", "pm"), ("writer", "worker"), ("reviewer", "worker")] {
            checked(
                shared.store.register_agent(&NewAgent {
                    alias,
                    provider: "claude",
                    endpoint_kind: "managed",
                    role,
                    cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params: Some(r#"{"upstream":"lead"}"#),
                    team_role: None,
                    model_policy: None,
                }),
                "simulated agent registration failed",
            );
            let identity = crate::adapter::Identity {
                thread_id: "fixture-thread".into(),
                session_id: "fixture-session".into(),
                model: None,
                effort: None,
                pid: pid(),
                endpoint: None,
                generation: Some("g1".into()),
                attach: None,
            };
            checked(
                shared.store.set_identity(alias, &identity),
                "simulated managed identity recording failed",
            );
        }
        Self {
            dir,
            shared,
            smtp_adapter,
        }
    }

    fn call(&self, method: &str, params: Value) -> crate::Result<Value> {
        scoped(Asserted::Operator, || {
            self.shared.dispatch(method, &params, pid())
        })
    }

    fn op(&self, method: &str, params: Value) -> Value {
        checked(
            self.call(method, params),
            "operator setup/observation failed",
        )
    }

    /// Canonical legacy custody fixture; no claim of live SMTP enrollment.
    /// Plain platform_enroll RPC refuses shape:smtp; connection_create
    /// verifies a relay. Here existing backend + Store APIs establish the
    /// actual record/bytes the real stage handler and default loader read.
    fn enroll_smtp(&self, account: &str) -> String {
        let enrollment = crate::platform::smtp::SmtpEnrollment {
            host: "localhost".into(),
            port: 465,
            tls_mode: "implicit".into(),
            username: "fixture-user".into(),
            secret: A.as_bytes().to_vec(),
            sender: "fixture@fixture.cadence".into(),
            sender_name: "Fixture".into(),
        };
        let bytes = zeroize::Zeroizing::new(checked(
            crate::platform::smtp::custody_bytes(&enrollment),
            "canonical smtp custody encoding failed",
        ));
        let connection_id = format!("conn-{}", uuid::Uuid::new_v4().simple());
        {
            let _guard = self
                .shared
                .platform_custody_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let record = crate::store::CredentialRecord {
                connection_id: connection_id.clone(),
                credential_revision: 1,
                platform: "smtp".into(),
                account: account.into(),
                scopes: vec!["email:send".into()],
                fingerprint: crate::secret::fingerprint(&bytes),
                custody: self.shared.platform_custody.tag().into(),
                exchange: "smtp".into(),
                enrolled_at: 1.0,
                by: "operator".into(),
            };
            checked(
                self.shared.platform_custody.put(
                    &crate::platform::custody::Key {
                        platform: "smtp",
                        account,
                    },
                    &bytes,
                ),
                "fixture custody write failed",
            );
            checked(
                self.shared.store.platform_enroll(&record, false, None),
                "fixture Store enrollment failed",
            );
        }
        let rows = self.op("connection_list", json!({}));
        let listed = rows["connections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["provider"] == "smtp" && row["account"] == account)
            .unwrap_or_else(|| panic!("enrolled smtp connection unavailable"));
        assert!(
            listed["id"] == connection_id,
            "listed connection must name the enrolled record"
        );
        connection_id
    }

    fn install_social(&self, connection: &str) -> (String, String) {
        let source = format!(
            "{}/workspace-apps/social-content",
            env!("CARGO_MANIFEST_DIR")
        );
        let installed = self.op("app_workspace_install", json!({"source": source}));
        let install = installed["install_id"].as_str().unwrap().to_string();
        let bundle = installed["digest"].as_str().unwrap().to_string();
        self.op(
            "app_local_install_approve",
            json!({"install_id": install, "digest": bundle}),
        );
        self.op(
            "app_binding_create",
            json!({"install_id": install, "slot": "publication",
            "connection_id": connection, "request_id": "bind-smtp"}),
        );
        (install, bundle)
    }

    /// Guarded take/mark/reread/finish typed material driver from the current
    /// approved_intent reference. Dispatch QUEUES; it does not finish turns.
    fn publication_run(&self, install: &str, bundle: &str, tag: &str) -> (String, String) {
        let created = self.op(
            "app_run_create",
            json!({"install_id": install, "workflow": "facebook",
            "request_id": format!("run-{tag}"), "owner_pm": "lead",
            "inputs": {"subject": "Fixture caption", "source": "fixture source facts",
                "writer": "writer", "reviewer": "reviewer"}}),
        );
        let run_id = created["id"].as_str().unwrap().to_string();
        self.op(
            "app_run_approve",
            json!({"run_id": run_id, "digest": created["snapshot_digest"]}),
        );
        let store = &self.shared.store;
        let body = "# Post\nReviewed fixture copy.";
        let turn = |run: &Value, step: usize, alias: &str, reply: Value| {
            let message = run["steps"][step]["message_id"].as_str().unwrap();
            let taken = match checked(
                store.take_queued_app_proven(alias, Some((message, bundle))),
                "guarded app turn claim failed",
            ) {
                crate::store::Take::Message(taken) => taken,
                _ => panic!("app turn was not claimed"),
            };
            let token = crate::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint("g1");
            checked(
                store.mark_running(&taken.id, &token),
                "managed turn start failed",
            );
            let taken = checked(store.message(&taken.id), "turn reread failed")
                .unwrap_or_else(|| panic!("claimed turn missing"));
            let reply = json!({"turn_id": taken.turn_id, "text": reply.to_string()});
            checked(
                store.finish(&taken, "completed", &reply, None),
                "typed turn finish failed",
            );
        };
        let run = checked(
            store.app_run_dispatch(&run_id, bundle),
            "producer dispatch failed",
        );
        turn(
            &run,
            0,
            "writer",
            json!({"schema":1,"kind":"produce_text","run_id":run_id,
            "step_id":"s1","revision":1,"outcome":"succeeded",
            "artifacts":[{"media_type":"text/markdown","text":body}]}),
        );
        let run = checked(
            store.app_run_dispatch(&run_id, bundle),
            "reviewer dispatch failed",
        );
        let artifact_id = run["artifacts"][0]["id"].as_str().unwrap().to_string();
        let digest = crate::store::app_runs::artifact_digest(body.as_bytes());
        turn(
            &run,
            1,
            "reviewer",
            json!({"schema":1,"kind":"review_text","run_id":run_id,
            "step_id":"s2","revision":1,"producer_step_id":"s1","producer_revision":1,
            "artifact_sha256":digest,"decision":"approve","rationale":"Checked the exact artifact."}),
        );
        let completed = checked(
            store.app_run_show(&run_id),
            "completed run observation failed",
        );
        assert!(
            completed["state"] == "succeeded",
            "Store must compute run succeeded"
        );
        let material = checked(
            store.app_publication_material(&run_id, &artifact_id, bundle, "publication"),
            "completed publication material refused",
        );
        assert!(
            material["artifact"]["id"] == artifact_id
                && material["artifact"]["text"] == body
                && material["producer_receipt"]["material"]["outcome"] == "succeeded"
                && material["review_receipt"]["material"]["decision"] == "approve",
            "Store must authenticate the completed producer and independent review material"
        );
        (run_id, artifact_id)
    }

    fn effect_count(&self, install: &str) -> usize {
        self.op("app_effect_list", json!({"install_id": install}))["effects"]
            .as_array()
            .unwrap()
            .len()
    }

    /// Stage emits on PLATFORM_STREAM (audit:platforms), NOT lead. Public
    /// Store::events requires an agent alias, so observe the actual audit
    /// stream via a read-only SQLite handle, without a second Store::open
    /// (which clears generation state), fixture rows or rendered payloads.
    fn release_event_count(&self, request: &str) -> i64 {
        let conn = checked(
            rusqlite::Connection::open_with_flags(
                self.dir.path().join("cadence.sqlite3"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            ),
            "event observation connection failed",
        );
        checked(conn.query_row(
            "SELECT COUNT(*) FROM events WHERE alias=?1 AND json_extract(payload,'$.request')=?2",
            rusqlite::params![crate::store::PLATFORM_STREAM, request],
            |row| row.get(0),
        ), "request event observation failed")
    }
}

fn derived_request(install: &str, request_id: &str) -> String {
    format!(
        "app-release-{}",
        uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            format!("{install}:{request_id}").as_bytes(),
        )
        .simple()
    )
}

/// Outcome is JSON (null/object), never coerced through as_str. Only
/// boolean assertions consume this private observation; never Debug it.
fn effect_state(fx: &Fx, install: &str, request: &str) -> Option<(String, Value)> {
    fx.op("app_effect_list", json!({"install_id": install}))["effects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|effect| effect["request"] == request)
        .map(|effect| {
            (
                effect["state"].as_str().unwrap().to_string(),
                effect["record"]["outcome"].clone(),
            )
        })
}

/// Called BEFORE matching/refusal classification. All failures render only
/// fixed messages or counts, even if an omitted guard persisted a secret.
fn assert_no_release(
    fx: &Fx,
    install: &str,
    request: &str,
    effects_before: usize,
    events_before: i64,
) {
    assert!(
        effect_state(fx, install, request).is_none(),
        "refusal must leave no release row"
    );
    assert_eq!(
        fx.smtp_adapter.execute_count(),
        0,
        "refusal must not execute"
    );
    assert_eq!(
        fx.effect_count(install),
        effects_before,
        "refusal must not add an effect"
    );
    assert_eq!(
        fx.release_event_count(request),
        events_before,
        "refusal must not add a request event"
    );
    assert!(
        fx.smtp_adapter.prepare_count() > 0 && fx.smtp_adapter.preview_count() > 0,
        "the real stage must reach prepare and preview"
    );
}

fn assert_leak_refusal(result: crate::Result<Value>, preview_case: bool, forbidden: &[&str]) {
    match result {
        Err(error) => {
            // Evaluate no-echo first; neither this private text nor any
            // unexpected Ok(Value) is ever rendered in a panic.
            let text = zeroize::Zeroizing::new(error.to_string());
            let echoes_secret = forbidden.iter().any(|secret| text.contains(*secret));
            assert!(!echoes_secret, "refusal must not echo enrolled material");
            let input_screen =
                text.contains("app release input would carry the enrolled credential");
            let preview_screen =
                text.contains("app release preview would carry the enrolled credential");
            assert!(
                input_screen || (preview_case && preview_screen),
                "the appropriate leak screen must refuse"
            );
        }
        Ok(_) => panic!("leaky publication unexpectedly succeeded"),
    }
}

/// Intended-success anchor, NOT a test of the current bug as desired behavior.
#[test]
fn smtp_publication_public_metadata_input_should_stage_waiting() {
    let fx = Fx::new(None);
    let connection = fx.enroll_smtp("leg-clean");
    let (install, bundle) = fx.install_social(&connection);
    let (run, artifact_id) = fx.publication_run(&install, &bundle, "r-clean");
    let request = derived_request(&install, "rel-clean");
    let result = fx.call(
        "app_effect_stage",
        json!({"run_id":run,"artifact_id":artifact_id,
        "slot":"publication","request_id":"rel-clean","title":"Sprint notes — public"}),
    );
    assert!(
        fx.smtp_adapter.prepare_count() > 0 && fx.smtp_adapter.preview_count() > 0,
        "the real stage must reach prepare and preview"
    );
    assert_eq!(fx.smtp_adapter.execute_count(), 0, "stage must not execute");
    let staged = match result {
        Ok(staged) => staged,
        Err(_) => {
            panic!("clean public metadata must stage waiting without canonical-framing over-match")
        }
    };
    assert!(
        staged["effect"]["state"] == "waiting",
        "clean stage must reach waiting"
    );
    assert!(
        staged["effect"]["request"] == request,
        "derived release request must match"
    );
    assert!(
        staged["effect"]["record"]["outcome"].is_null(),
        "stage must have JSON-null outcome"
    );
    let waiting_null = match effect_state(&fx, &install, &request) {
        Some((state, outcome)) => state == "waiting" && outcome.is_null(),
        None => false,
    };
    assert!(
        waiting_null,
        "durable stage must be waiting with JSON-null outcome"
    );
}

#[test]
fn smtp_publication_refuses_password_input() {
    let fx = Fx::new(None);
    let connection = fx.enroll_smtp("leg-in");
    let (install, bundle) = fx.install_social(&connection);
    let (run, artifact_id) = fx.publication_run(&install, &bundle, "r-in");
    let request = derived_request(&install, "rel-in");
    let effects_before = fx.effect_count(&install);
    let events_before = fx.release_event_count(&request);
    let result = fx.call(
        "app_effect_stage",
        json!({"run_id":run,"artifact_id":artifact_id,
        "slot":"publication","request_id":"rel-in","title":A}),
    );
    assert_no_release(&fx, &install, &request, effects_before, events_before);
    assert_leak_refusal(result, false, &[A]);
}

#[test]
fn smtp_publication_refuses_password_fragment() {
    let fx = Fx::new(None);
    let connection = fx.enroll_smtp("leg-frag");
    let (install, bundle) = fx.install_social(&connection);
    let (run, artifact_id) = fx.publication_run(&install, &bundle, "r-frag");
    let request = derived_request(&install, "rel-frag");
    let fragment: String = A.chars().take(8).collect();
    let effects_before = fx.effect_count(&install);
    let events_before = fx.release_event_count(&request);
    let result = fx.call(
        "app_effect_stage",
        json!({"run_id":run,"artifact_id":artifact_id,
        "slot":"publication","request_id":"rel-frag","title":format!("notes {fragment}")}),
    );
    assert_no_release(&fx, &install, &request, effects_before, events_before);
    assert_leak_refusal(result, false, &[A, &fragment]);
}

/// Before repair the raw input screen masks the preview screen. This case
/// does NOT qualify preview-guard mutation until the clean input can pass.
#[test]
fn smtp_publication_refuses_password_in_preview_only() {
    let fx = Fx::new(Some(A.as_bytes().to_vec()));
    let connection = fx.enroll_smtp("leg-prev");
    let (install, bundle) = fx.install_social(&connection);
    let (run, artifact_id) = fx.publication_run(&install, &bundle, "r-prev");
    let request = derived_request(&install, "rel-prev");
    let effects_before = fx.effect_count(&install);
    let events_before = fx.release_event_count(&request);
    let result = fx.call(
        "app_effect_stage",
        json!({"run_id":run,"artifact_id":artifact_id,
        "slot":"publication","request_id":"rel-prev","title":"Clean public caption"}),
    );
    assert_no_release(&fx, &install, &request, effects_before, events_before);
    let (input, preview) = fx.smtp_adapter.last_prepared();
    let clean_input_leaky_preview = match (input, preview) {
        (Some(input), Some(preview)) => {
            crate::platform::refuse_leak("diagnostic", &input.to_string(), A.as_bytes()).is_ok()
                && preview == A
        }
        _ => false,
    };
    assert!(
        clean_input_leaky_preview,
        "preview control must have clean input and only a leaky preview"
    );
    assert_leak_refusal(result, true, &[A]);
}

/// Real dispatch capture, live default loader+fingerprint under the SAME C
/// guard AFTER dispatch returns, canonical borrowed password, booleans only.
#[test]
fn smtp_leak_screen_diagnostic_boolean_only() {
    let fx = Fx::new(None);
    let connection = fx.enroll_smtp("leg-diag");
    let (install, bundle) = fx.install_social(&connection);
    let (run, artifact_id) = fx.publication_run(&install, &bundle, "r-diag");
    let result = fx.call(
        "app_effect_stage",
        json!({"run_id":run,"artifact_id":artifact_id,
        "slot":"publication","request_id":"rel-diag","title":"Diagnostic caption"}),
    );
    let (input, preview) = match fx.smtp_adapter.last_prepared() {
        (Some(input), Some(preview)) => (input, preview),
        _ => panic!("real stage must capture prepared input and preview"),
    };
    let stage_is_qualified = match result {
        Err(error) => {
            let text = zeroize::Zeroizing::new(error.to_string());
            assert!(
                !text.contains(A),
                "diagnostic refusal must not echo the enrolled password"
            );
            text.contains("app release input would carry the enrolled credential")
        }
        Ok(staged) => staged["effect"]["state"] == "waiting",
    };
    assert!(
        stage_is_qualified,
        "clean stage must be input-screen refusal before repair or waiting after repair"
    );
    let (canonical_decode_ok, raw_input, raw_preview, secret_input, secret_preview) = {
        let shared = &fx.shared;
        let guard = shared
            .platform_custody_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = checked(
            shared.store.connection_credential(&connection),
            "live record read failed",
        )
        .unwrap_or_else(|| panic!("live enrolled record missing"));
        let legacy_smtp = record.platform == "smtp"
            && record.account == "leg-diag"
            && record.exchange == "smtp"
            && matches!(
                record.custody.as_str(),
                crate::platform::custody::FILE_TAG | crate::platform::custody::LIBSECRET_TAG
            );
        assert!(
            legacy_smtp,
            "provider classification must establish typed legacy SMTP before decoding"
        );
        let bytes = zeroize::Zeroizing::new(checked(
            crate::platform::load_credential(
                &shared.store,
                &shared.platform_custody,
                &record.platform,
                &record.account,
            ),
            "authenticated live credential load failed",
        ));
        let (envelope, _projection) = checked(
            crate::platform::smtp::custody_decode(&bytes),
            "canonical SMTP decode failed",
        );
        assert!(
            envelope.secret() == A.as_bytes(),
            "live credential must contain the actually enrolled password"
        );
        let observations = (
            true,
            crate::platform::refuse_leak("diagnostic", &input.to_string(), &bytes).is_err(),
            crate::platform::refuse_leak("diagnostic", &preview, &bytes).is_err(),
            crate::platform::refuse_leak("diagnostic", &input.to_string(), envelope.secret())
                .is_err(),
            crate::platform::refuse_leak("diagnostic", &preview, envelope.secret()).is_err(),
        );
        drop(envelope);
        drop(bytes);
        drop(guard);
        observations
    };
    assert!(canonical_decode_ok, "canonical SMTP decode must succeed");
    assert!(
        raw_input,
        "raw custody document must over-match this clean input"
    );
    assert!(
        raw_preview,
        "raw custody document must over-match this preview which embeds the input"
    );
    assert!(
        !secret_input,
        "password-only screen must permit this clean input"
    );
    assert!(
        !secret_preview,
        "password-only screen must permit this clean preview"
    );
}
