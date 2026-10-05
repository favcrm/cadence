//! Test-only copy of the sealed REV3 material/enrollment driver, separately
//! owned for downstream execution. No mutation or visibility change to REV3.
use super::{checked, required, ClaimObservation, OutcomeAdapter};
use crate::daemon::{ServeOptions, Shared};
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::process::id as pid;
use std::sync::{Arc, Mutex};

pub(super) struct Fx {
    pub(super) dir: tempfile::TempDir,
    pub(super) shared: Arc<Shared>,
    pub(super) adapter: Arc<OutcomeAdapter>,
    pub(super) claims: Arc<Mutex<ClaimObservation>>,
}

impl Fx {
    pub(super) fn new(adapter: OutcomeAdapter) -> Self {
        let dir = checked(
            tempfile::Builder::new().prefix("d1156").tempdir(),
            "isolated directory creation failed",
        );
        checked(
            crate::issue::Pm::init(&dir.path().join("pm")),
            "PM initialization failed",
        );
        let claims = Arc::new(Mutex::new(ClaimObservation::default()));
        let decide_observation = claims.clone();
        let claim_observation = claims.clone();
        let mut opts = ServeOptions {
            provider_env: crate::adapter::ProviderEnv::refusing_providers(),
            test_seam: true,
            effect_execute_gate: Some(Arc::new(move |row| {
                assert!(
                    row.state == "decided" && row.decision.is_some() && row.outcome.is_none(),
                    "decision callback must observe the real persisted acceptance before execution"
                );
                let mut observed = decide_observation.lock().unwrap_or_else(|p| p.into_inner());
                observed.decisions += 1;
                observed.decided = row.decision.clone();
                true
            })),
            app_release_claim_gate: Some(Arc::new(move |row| {
                assert!(
                    row.state == "executing" && row.decision.is_some() && row.outcome.is_none(),
                    "claim callback must observe executing with the prior decision and no outcome"
                );
                let mut observed = claim_observation.lock().unwrap_or_else(|p| p.into_inner());
                observed.claims += 1;
                observed.claimed = row.decision.clone();
                true
            })),
            ..ServeOptions::default()
        };
        opts.provider_env.set(
            "CADENCE_PM_DIR",
            required(dir.path().join("pm").to_str(), "PM path invalid"),
        );
        crate::platform::smtp::attach(&mut opts);
        let adapter = Arc::new(adapter);
        opts.platforms.insert("smtp".into(), adapter.clone());
        let shared = checked(
            Shared::new(dir.path(), &opts),
            "Shared initialization failed",
        );
        let cwd = required(dir.path().to_str(), "fixture path invalid");
        // SIMULATED managed g1 identities, consistent with the minted tokens.
        // No actor/provider launch or network SMTP verification occurs.
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
            checked(
                shared.store.set_identity(
                    alias,
                    &crate::adapter::Identity {
                        thread_id: "fixture-thread".into(),
                        session_id: "fixture-session".into(),
                        model: None,
                        effort: None,
                        pid: pid(),
                        endpoint: None,
                        generation: Some("g1".into()),
                        attach: None,
                    },
                ),
                "simulated identity recording failed",
            );
        }
        Self {
            dir,
            shared,
            adapter,
            claims,
        }
    }

    pub(super) fn call(&self, method: &str, params: Value) -> crate::Result<Value> {
        scoped(Asserted::Operator, || {
            self.shared.dispatch(method, &params, pid())
        })
    }
    pub(super) fn op(&self, method: &str, params: Value) -> Value {
        checked(
            self.call(method, params),
            "operator setup/observation failed",
        )
    }

    /// Existing canonical custody + durable record setup under the serving C
    /// guard, not connection_create/network enrollment or a new custody seam.
    pub(super) fn enroll_smtp(&self, account: &str, secret: &str) -> String {
        let enrollment = crate::platform::smtp::SmtpEnrollment {
            host: "localhost".into(),
            port: 465,
            tls_mode: "implicit".into(),
            username: "fixture-user".into(),
            secret: secret.as_bytes().to_vec(),
            sender: "fixture@fixture.cadence".into(),
            sender_name: "Fixture".into(),
        };
        let bytes = zeroize::Zeroizing::new(checked(
            crate::platform::smtp::custody_bytes(&enrollment),
            "canonical SMTP encoding failed",
        ));
        let connection = format!("conn-{}", uuid::Uuid::new_v4().simple());
        {
            let _guard = self
                .shared
                .platform_custody_lock
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let record = crate::store::CredentialRecord {
                connection_id: connection.clone(),
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
                "fixture enrollment failed",
            );
        }
        let listed = self.op("connection_list", json!({}));
        assert!(
            required(listed["connections"].as_array(), "connection list invalid")
                .iter()
                .any(|row| row["id"] == connection
                    && row["provider"] == "smtp"
                    && row["account"] == account),
            "connection list must name the actually enrolled record"
        );
        connection
    }

    pub(super) fn install_social(&self, connection: &str) -> (String, String) {
        let source = format!(
            "{}/workspace-apps/social-content",
            env!("CARGO_MANIFEST_DIR")
        );
        let installed = self.op("app_workspace_install", json!({"source": source}));
        let install =
            required(installed["install_id"].as_str(), "install handle missing").to_string();
        let bundle = required(installed["digest"].as_str(), "bundle handle missing").to_string();
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

    pub(super) fn publication_run(&self, install: &str, bundle: &str) -> (String, String) {
        let created = self.op(
            "app_run_create",
            json!({"install_id": install, "workflow": "facebook",
            "request_id": "run-downstream", "owner_pm": "lead",
            "inputs": {"subject": "Fixture caption", "source": "fixture source facts",
                "writer": "writer", "reviewer": "reviewer"}}),
        );
        let run_id = required(created["id"].as_str(), "run handle missing").to_string();
        self.op(
            "app_run_approve",
            json!({"run_id": run_id, "digest": created["snapshot_digest"]}),
        );
        let store = &self.shared.store;
        let body = "# Post\nReviewed fixture copy.";
        let turn = |run: &Value, step: usize, alias: &str, reply: Value| {
            let message = required(
                run["steps"][step]["message_id"].as_str(),
                "turn handle missing",
            );
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
            let taken = required(
                checked(store.message(&taken.id), "turn reread failed"),
                "claimed turn missing",
            );
            checked(
                store.finish(
                    &taken,
                    "completed",
                    &json!({"turn_id": taken.turn_id,
                "text": reply.to_string()}),
                    None,
                ),
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
        let artifact_id = required(
            run["artifacts"][0]["id"].as_str(),
            "artifact handle missing",
        )
        .to_string();
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
            "Store must authenticate exact producer and independent review material"
        );
        (run_id, artifact_id)
    }

    /// Read authenticated bytes privately under THIS dispatch target's C guard.
    /// The borrowed secret cannot escape the decode/guard scope.
    pub(super) fn with_secret<T>(
        &self,
        connection: &str,
        expected: &str,
        f: impl FnOnce(&[u8]) -> T,
    ) -> T {
        let guard = self
            .shared
            .platform_custody_lock
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let record = required(
            checked(
                self.shared.store.connection_credential(connection),
                "live credential record read failed",
            ),
            "live enrolled record missing",
        );
        assert!(
            record.platform == "smtp"
                && record.exchange == "smtp"
                && matches!(
                    record.custody.as_str(),
                    crate::platform::custody::FILE_TAG | crate::platform::custody::LIBSECRET_TAG
                ),
            "live record must establish legacy SMTP classification"
        );
        let bytes = zeroize::Zeroizing::new(checked(
            crate::platform::load_credential(
                &self.shared.store,
                &self.shared.platform_custody,
                &record.platform,
                &record.account,
            ),
            "authenticated live credential load failed",
        ));
        let (envelope, projection) = checked(
            crate::platform::smtp::custody_decode(&bytes),
            "canonical SMTP decode failed",
        );
        assert!(
            envelope.secret() == expected.as_bytes(),
            "loaded secret must be the actually enrolled password"
        );
        assert!(
            crate::platform::refuse_leak(
                "diagnostic",
                &projection.to_json().to_string(),
                envelope.secret()
            )
            .is_ok(),
            "downstream credential public projection must be otherwise eligible"
        );
        let result = f(envelope.secret());
        drop(envelope);
        drop(bytes);
        drop(guard);
        result
    }
}
