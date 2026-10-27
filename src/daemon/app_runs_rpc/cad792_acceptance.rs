//! CAD-792 independent acceptance (gates/security). The implementer must
//! not edit this file.
//!
//! One behavior check of the real daemon dispatch path:
//! `agent_identity` then `app_run_artifact`. The assigned Codex reviewer
//! reads its own current message and turn token from identity and fetches
//! the exact dependency artifact (text and digest). A syntactically valid
//! approve pinned to the kickoff digest, with no artifact read, does not
//! succeed; an operator audit read of that artifact does not satisfy it.
//! The worker receipt is `app_run_artifact_fetches`
//! (`artifact_id`, `message_id`, `turn_hash`, `created`). `turn_hash` is
//! `artifact_digest` of the current turn token. Only the guarded worker
//! fetch inserts one, after its existing checks. Approve records only when
//! that row matches the completion token; a missing row inserts no review
//! and fails the step with the existing material-output refusal. `finish`
//! may still return `Ok`. A receipt for one token does not survive rotation
//! onto another. Re-registering the reviewer under the same
//! alias mints a new recipient uid; `agent_identity` does not disclose the
//! previous registration's still-held turn, and it does disclose a held
//! turn of the new registration. Forged identity fields,
//! another worker's turn, a wrong or stale token, another alias's message,
//! an artifact outside the dependency set, a finished turn, a cancelled
//! run, and a revoked installation approval are refused by reason. The
//! operator read is the separate `artifact_id`-only route and still
//! returns the same bytes.
//!
//! Limit: an in-process test cannot present a live managed-Codex process
//! tree or the MCP child that will call these RPCs. Caller identity is the
//! existing test-seam `Asserted` value, which `caller_identity` and
//! `slot_identity` honor before `/proc` ancestry. No actor is launched, so
//! the queued kickoff is claimed through the store take / `mark_running` /
//! `finish` path the daemon uses once a provider has the turn. Dispatch
//! itself only wakes an owned actor, and this fixture owns none.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::adapter::Identity;
use crate::daemon::{ServeOptions, Shared};
use crate::store::{app_runs::artifact_digest, Message, NewAgent, Take};
use crate::test_seam::{scoped, Asserted};
use crate::Result;

fn pid() -> u32 {
    std::process::id()
}

fn refusal(result: Result<Value>) -> String {
    result.expect_err("expected a refusal").to_string()
}

struct Produced {
    run_id: String,
    artifact_id: String,
    text: String,
    digest: String,
    writer_message: String,
}

struct Held {
    message_id: String,
    token: String,
    task_id: String,
}

struct Fx {
    _dir: tempfile::TempDir,
    shared: Arc<Shared>,
    install: String,
    bundle: String,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("c792").tempdir().unwrap();
        let pm = dir.path().join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let shared = Shared::new(dir.path(), &opts).unwrap();
        let cwd = dir.path().to_str().unwrap().to_string();
        shared
            .store
            .register_agent(&NewAgent {
                alias: "lead",
                provider: "fake",
                endpoint_kind: "fake",
                role: "pm",
                cwd: &cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        for alias in ["writer", "reviewer"] {
            shared
                .store
                .register_agent(&NewAgent {
                    alias,
                    provider: "codex",
                    endpoint_kind: "managed",
                    role: "worker",
                    cwd: &cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params: Some(r#"{"upstream":"lead"}"#),
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
            shared
                .store
                .set_identity(
                    alias,
                    &Identity {
                        thread_id: format!("thread-{alias}"),
                        session_id: format!("session-{alias}"),
                        model: None,
                        effort: None,
                        pid: pid(),
                        endpoint: None,
                        generation: Some(format!("gen-{alias}")),
                        attach: None,
                    },
                )
                .unwrap();
        }
        let source = format!("{}/tests/fixtures/apps/crm", env!("CARGO_MANIFEST_DIR"));
        let mut fx = Self {
            _dir: dir,
            shared,
            install: String::new(),
            bundle: String::new(),
        };
        let installed = fx
            .operator("app_workspace_install", json!({"source": source}))
            .unwrap();
        fx.install = installed["install_id"].as_str().unwrap().to_string();
        fx.bundle = installed["digest"].as_str().unwrap().to_string();
        fx.operator(
            "app_local_install_approve",
            json!({"install_id": fx.install, "digest": fx.bundle}),
        )
        .unwrap();
        fx
    }

    fn call(&self, who: Asserted, method: &str, params: Value) -> Result<Value> {
        scoped(who, || self.shared.dispatch(method, &params, pid()))
    }

    fn operator(&self, method: &str, params: Value) -> Result<Value> {
        self.call(Asserted::Operator, method, params)
    }

    fn identity(&self, alias: &str) -> Value {
        self.call(Asserted::Agent(alias.into()), "agent_identity", json!({}))
            .unwrap_or_else(|error| panic!("{alias} agent_identity failed: {error}"))
    }

    fn fetch(&self, alias: &str, artifact_id: &str, message: &str, token: &str) -> Result<Value> {
        self.call(
            Asserted::Agent(alias.into()),
            "app_run_artifact",
            json!({"artifact_id": artifact_id, "message": message, "token": token}),
        )
    }

    fn operator_artifact(&self, artifact_id: &str) -> Value {
        self.operator("app_run_artifact", json!({"artifact_id": artifact_id}))
            .unwrap_or_else(|error| panic!("operator artifact read failed: {error}"))
    }

    fn create_params(&self, request: &str) -> Value {
        json!({
            "install_id": self.install,
            "workflow": "email-brief",
            "request_id": request,
            "owner_pm": "lead",
            "inputs": {
                "subject": "Renewal",
                "audience": "Customers due a renewal",
                "facts": "Plan renews 1 July. Price stays HK$88/month.",
                "writer": "writer",
                "reviewer": "reviewer",
            },
        })
    }

    fn claim(&self, alias: &str, message_id: &str, token: &str) -> Message {
        let taken = match self
            .shared
            .store
            .take_queued_app_proven(alias, Some((message_id, self.bundle.as_str())))
        {
            Ok(Take::Message(message)) => message,
            Ok(Take::Stop) => panic!("{alias} is disabled; the app turn was not claimed"),
            Ok(Take::Empty) => panic!("{alias} had no queued app turn"),
            Err(error) => panic!("{alias} app turn claim failed: {error}"),
        };
        assert_eq!(taken.id, message_id, "{alias} claimed a different message");
        self.shared
            .store
            .mark_running(message_id, token)
            .unwrap_or_else(|error| panic!("{alias} turn start failed: {error}"));
        self.shared
            .store
            .message(message_id)
            .unwrap()
            .unwrap_or_else(|| panic!("{alias} turn disappeared"))
    }

    /// Operator create, approve, and dispatch, then the writer's real
    /// produce-text completion. The reviewer step stays pending.
    fn produce(&self, request: &str) -> Produced {
        let created = self
            .operator("app_run_create", self.create_params(request))
            .unwrap_or_else(|error| panic!("create {request} failed: {error}"));
        let run_id = created["id"].as_str().unwrap().to_string();
        self.operator(
            "app_run_approve",
            json!({"run_id": run_id, "digest": created["snapshot_digest"]}),
        )
        .unwrap_or_else(|error| panic!("approve {request} failed: {error}"));
        let dispatched = self
            .operator("app_run_dispatch", json!({"run_id": run_id}))
            .unwrap_or_else(|error| panic!("dispatch {request} failed: {error}"));
        assert_eq!(
            dispatched["snapshot"]["assignments"]["s1"]["provider"], "codex",
            "the producer assignment is the managed Codex worker"
        );
        let writer_message = dispatched["steps"][0]["message_id"]
            .as_str()
            .unwrap()
            .to_string();
        let token = format!("codex-turn-{request}-writer");
        let message = self.claim("writer", &writer_message, &token);
        let text = format!("CAD-792 dependency text {request}. Price stays HK$88/month.");
        let envelope = json!({
            "schema": 1,
            "kind": "produce_text",
            "run_id": run_id,
            "step_id": "s1",
            "revision": 1,
            "outcome": "succeeded",
            "artifacts": [{"media_type": "text/markdown", "text": text}],
        });
        self.shared
            .store
            .finish(
                &message,
                "completed",
                &json!({"turn_id": token, "text": envelope.to_string()}),
                None,
            )
            .unwrap_or_else(|error| panic!("writer result failed: {error}"));
        let shown = self.shared.store.app_run_show(&run_id).unwrap();
        assert_eq!(shown["steps"][0]["state"], "succeeded", "{shown}");
        let artifact_id = shown["artifacts"][0]["id"].as_str().unwrap().to_string();
        let digest = shown["artifacts"][0]["digest"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(digest, artifact_digest(text.as_bytes()));
        Produced {
            run_id,
            artifact_id,
            text,
            digest,
            writer_message,
        }
    }

    /// Dispatch the reviewer step and leave its Codex turn running.
    fn hold_reviewer(&self, produced: &Produced, token: &str) -> Held {
        let dispatched = self
            .operator("app_run_dispatch", json!({"run_id": produced.run_id}))
            .unwrap_or_else(|error| panic!("reviewer dispatch failed: {error}"));
        assert_eq!(
            dispatched["snapshot"]["assignments"]["s2"]["provider"],
            "codex"
        );
        assert_eq!(
            dispatched["snapshot"]["assignments"]["s2"]["endpoint_kind"],
            "managed"
        );
        let step = &dispatched["steps"][1];
        assert_eq!(step["step_id"], "s2");
        let message_id = step["message_id"].as_str().unwrap().to_string();
        let task_id = step["task_id"].as_str().unwrap().to_string();
        self.claim("reviewer", &message_id, token);
        Held {
            message_id,
            token: token.to_string(),
            task_id,
        }
    }

    /// Digest the reviewer kickoff names. Not taken from an artifact read.
    fn kickoff_digest(&self, message_id: &str) -> String {
        let message = self
            .shared
            .store
            .message(message_id)
            .unwrap()
            .unwrap_or_else(|| panic!("kickoff {message_id} missing"));
        let body: Value = serde_json::from_str(&message.body)
            .unwrap_or_else(|_| panic!("kickoff {message_id} is not the dispatch envelope"));
        body["dependencies"][0]["sha256"]
            .as_str()
            .unwrap_or_else(|| panic!("kickoff {message_id} names no dependency digest"))
            .to_string()
    }

    /// Submit a syntactically valid approve pinned to `digest`. `Ok` only
    /// means the turn finished; the run show is what says whether it
    /// approved.
    fn submit_approve(&self, held: &Held, run_id: &str, digest: &str) -> Result<()> {
        self.submit_approve_token(&held.message_id, &held.token, run_id, digest)
    }

    /// Same pinned approve, naming the turn token `finish` must present.
    /// A rotated turn passes the token `mark_running` recorded.
    fn submit_approve_token(
        &self,
        message_id: &str,
        token: &str,
        run_id: &str,
        digest: &str,
    ) -> Result<()> {
        let message = self
            .shared
            .store
            .message(message_id)
            .unwrap()
            .unwrap_or_else(|| panic!("reviewer turn {message_id} missing"));
        let envelope = json!({
            "schema": 1,
            "kind": "review_text",
            "run_id": run_id,
            "step_id": "s2",
            "revision": 1,
            "producer_step_id": "s1",
            "producer_revision": 1,
            "artifact_sha256": digest,
            "decision": "approve",
            "rationale": "Checked the exact artifact.",
        });
        self.shared.store.finish(
            &message,
            "completed",
            &json!({"turn_id": token, "text": envelope.to_string()}),
            None,
        )?;
        Ok(())
    }

    fn release_if_running(&self, held: &Held) {
        let Some(message) = self.shared.store.message(&held.message_id).unwrap() else {
            return;
        };
        if message.state == "running" {
            self.shared
                .store
                .finish(
                    &message,
                    "interrupted",
                    &json!({"turn_id": held.token}),
                    None,
                )
                .unwrap_or_else(|error| panic!("releasing {} failed: {error}", held.message_id));
        }
    }
}

fn approved(shown: &Value) -> bool {
    shown["state"] == "succeeded"
        || shown["reviews"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["decision"] == "approve"))
}

fn assert_approve_refused(finish: &Result<()>, shown: &Value) {
    // A missing receipt fails the step inside the existing finish path.
    // `finish` itself may still return Ok.
    let _ = finish;
    assert_eq!(shown["state"], "failed", "{shown}");
    assert!(
        !approved(shown),
        "pinned approve succeeded: finish={finish:?} run={shown}"
    );
    assert!(
        shown["reviews"]
            .as_array()
            .is_some_and(|rows| rows.is_empty()),
        "approve without a matching receipt must not insert a review: {shown}"
    );
    let steps = shown["steps"]
        .as_array()
        .unwrap_or_else(|| panic!("app_run_show must include steps, got {shown}"));
    let producer = steps
        .iter()
        .find(|step| step["step_id"] == "s1")
        .unwrap_or_else(|| panic!("producer step missing: {shown}"));
    assert_eq!(producer["state"], "succeeded", "{shown}");
    let review = steps
        .iter()
        .find(|step| step["step_id"] == "s2")
        .unwrap_or_else(|| panic!("review step missing: {shown}"));
    assert_eq!(review["state"], "failed", "{shown}");
    let reason = review["reason"].as_str().unwrap_or("");
    assert!(
        reason.contains("material output or pinned review refused"),
        "expected the existing material-output refusal: {shown}"
    );
}

fn receipt_rows(fx: &Fx, artifact_id: &str) -> Vec<(String, String)> {
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.query_map(
                "SELECT message_id, turn_hash FROM app_run_artifact_fetches WHERE artifact_id=?1 AND created IS NOT NULL ORDER BY message_id, turn_hash",
                [artifact_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
        })
        .unwrap_or_else(|error| panic!("worker receipt read failed: {error}"))
}

fn assert_no_worker_receipt(fx: &Fx, artifact_id: &str) {
    let rows = receipt_rows(fx, artifact_id);
    assert!(
        rows.is_empty(),
        "operator or unfetched approve must not insert app_run_artifact_fetches: {rows:?}"
    );
}

fn assert_worker_receipt(fx: &Fx, artifact_id: &str, message_id: &str, token: &str) {
    let expected = artifact_digest(token.as_bytes());
    assert_eq!(
        receipt_rows(fx, artifact_id),
        vec![(message_id.to_string(), expected)],
        "worker fetch must record one receipt hashed from the current token"
    );
}

fn assert_approve_succeeded(shown: &Value, digest: &str) {
    assert_eq!(shown["state"], "succeeded", "{shown}");
    assert!(approved(shown), "{shown}");
    assert_eq!(shown["reviews"][0]["decision"], "approve", "{shown}");
    assert_eq!(shown["reviews"][0]["artifact_digest"], digest, "{shown}");
}

fn own_turn(identity: &Value) -> &Value {
    assert_eq!(identity["alias"], "reviewer", "{identity}");
    let rows = identity["running"].as_array().unwrap_or_else(|| {
        panic!("agent_identity must return the caller's held turns, got {identity}")
    });
    assert_eq!(rows.len(), 1, "{identity}");
    &rows[0]
}

fn assert_artifact(fetched: &Value, text: &str, digest: &str) {
    assert_eq!(fetched["text"], text, "{fetched}");
    assert_eq!(fetched["digest"], digest, "{fetched}");
    assert_eq!(
        fetched["digest"],
        artifact_digest(fetched["text"].as_str().unwrap().as_bytes())
    );
    assert_eq!(fetched["media_type"], "text/markdown");
}

/// Assigned reviewer discovers its live message and token, fetches the
/// dependency bytes, and every other path is refused. Approve without
/// that fetch does not succeed; approve after it does.
#[test]
fn cad792_assigned_reviewer_fetches_dependency_artifact_under_real_guards() {
    let fx = Fx::new();
    let other = fx.produce("cad792-other");
    // No artifact read. The approve names the kickoff digest and is otherwise
    // valid. It must not succeed. No extra seam: this is the same finish the
    // daemon records when the reviewer returns its result.
    let unread = fx.produce("cad792-unread");
    let held_unread = fx.hold_reviewer(&unread, "codex-turn-cad792-unread-reviewer");
    let unread_digest = fx.kickoff_digest(&held_unread.message_id);
    assert_eq!(
        unread_digest, unread.digest,
        "kickoff digest drifted from the artifact"
    );
    let unread_finish = fx.submit_approve(&held_unread, &unread.run_id, &unread_digest);
    let unread_shown = fx.shared.store.app_run_show(&unread.run_id).unwrap();
    assert!(
        unread_finish.is_err() || !approved(&unread_shown),
        "approve without a fetch succeeded: finish={unread_finish:?} run={unread_shown}"
    );
    fx.release_if_running(&held_unread);

    let live = fx.produce("cad792-live");
    let held = fx.hold_reviewer(&live, "codex-turn-cad792-live-reviewer");

    for who in [Asserted::Operator, Asserted::Unproven] {
        let err = refusal(fx.call(who.clone(), "agent_identity", json!({})));
        assert!(err.contains("verified agent endpoint"), "{who:?}: {err}");
    }
    let forged = json!({
        "alias": "reviewer",
        "message": held.message_id,
        "token": held.token,
        "running": [{"id": held.message_id, "turn_id": held.token}],
    });
    let err = refusal(fx.call(Asserted::Agent("writer".into()), "agent_identity", forged));
    assert!(err.contains("agent identity accepts no fields"), "{err}");
    let writer = fx.identity("writer");
    assert_eq!(writer["alias"], "writer", "{writer}");
    let writer_body = writer.to_string();
    assert!(
        !writer_body.contains(&held.message_id) && !writer_body.contains(&held.token),
        "a peer identity returned the reviewer's turn: {writer}"
    );
    if let Some(rows) = writer.get("running").and_then(Value::as_array) {
        assert!(
            rows.is_empty(),
            "the finished writer has no held turn: {writer}"
        );
    }

    let identity = fx.identity("reviewer");
    let turn = own_turn(&identity);
    assert_eq!(turn["id"], held.message_id, "{identity}");
    assert_eq!(turn["turn_id"], held.token, "{identity}");
    assert_eq!(turn["task"], held.task_id, "{identity}");
    assert_ne!(turn["id"], live.writer_message);
    let message = turn["id"].as_str().unwrap();
    let token = turn["turn_id"].as_str().unwrap();
    let fetched = fx
        .fetch("reviewer", &live.artifact_id, message, token)
        .unwrap_or_else(|error| panic!("assigned reviewer fetch failed: {error}"));
    assert_artifact(&fetched, &live.text, &live.digest);
    assert_ne!(fetched["text"], other.text);

    let operator = fx.operator_artifact(&live.artifact_id);
    assert_artifact(&operator, &live.text, &live.digest);
    let err = refusal(fx.call(
        Asserted::Agent("reviewer".into()),
        "app_run_artifact",
        json!({"artifact_id": live.artifact_id}),
    ));
    assert!(err.contains("operator"), "{err}");
    let err = refusal(fx.operator(
        "app_run_artifact",
        json!({
            "artifact_id": live.artifact_id,
            "message": message,
            "token": token,
        }),
    ));
    assert!(
        err.contains("assigned worker"),
        "the operator route must not redeem a worker token: {err}"
    );

    let err = refusal(fx.fetch("writer", &live.artifact_id, &held.message_id, &held.token));
    assert!(
        err.contains("artifact turn belongs to another worker"),
        "{err}"
    );
    let err = refusal(fx.fetch(
        "reviewer",
        &live.artifact_id,
        &live.writer_message,
        &held.token,
    ));
    assert!(
        err.contains("artifact turn belongs to another worker"),
        "{err}"
    );
    let err = refusal(fx.fetch(
        "reviewer",
        &live.artifact_id,
        &held.message_id,
        "codex-turn-never-issued",
    ));
    assert!(
        err.contains("artifact needs an active assigned turn"),
        "{err}"
    );
    let err = refusal(fx.call(
        Asserted::Agent("reviewer".into()),
        "app_run_artifact",
        json!({
            "artifact_id": live.artifact_id,
            "message": held.message_id,
            "token": held.token,
            "alias": "writer",
        }),
    ));
    assert!(err.contains("unsupported fields"), "{err}");

    let err = refusal(fx.fetch(
        "reviewer",
        &other.artifact_id,
        &held.message_id,
        &held.token,
    ));
    assert!(
        err.contains("artifact is not a dependency of this turn"),
        "{err}"
    );
    assert_artifact(
        &fx.operator_artifact(&other.artifact_id),
        &other.text,
        &other.digest,
    );

    let rotated = format!("{}-rotated", held.token);
    fx.shared
        .store
        .mark_running(&held.message_id, &rotated)
        .unwrap();
    let err = refusal(fx.fetch("reviewer", &live.artifact_id, &held.message_id, &held.token));
    assert!(
        err.contains("artifact needs an active assigned turn"),
        "stale token: {err}"
    );
    let identity = fx.identity("reviewer");
    let turn = own_turn(&identity);
    assert_eq!(turn["id"], held.message_id, "{identity}");
    assert_eq!(turn["turn_id"], rotated, "{identity}");
    assert_eq!(turn["task"], held.task_id, "{identity}");
    let fetched = fx
        .fetch(
            "reviewer",
            &live.artifact_id,
            turn["id"].as_str().unwrap(),
            turn["turn_id"].as_str().unwrap(),
        )
        .unwrap_or_else(|error| panic!("rotated token fetch failed: {error}"));
    assert_artifact(&fetched, &live.text, &live.digest);

    let ended = fx.shared.store.message(&held.message_id).unwrap().unwrap();
    fx.shared
        .store
        .finish(&ended, "interrupted", &json!({"turn_id": rotated}), None)
        .unwrap_or_else(|error| panic!("ending the reviewer turn failed: {error}"));
    let err = refusal(fx.fetch("reviewer", &live.artifact_id, &held.message_id, &rotated));
    assert!(
        err.contains("artifact run execution is no longer approved"),
        "ended turn: {err}"
    );
    let after_end = fx.identity("reviewer");
    assert_eq!(after_end["alias"], "reviewer", "{after_end}");
    assert!(
        !after_end.to_string().contains(&held.message_id),
        "a finished turn stayed on agent_identity: {after_end}"
    );
    assert_artifact(
        &fx.operator_artifact(&live.artifact_id),
        &live.text,
        &live.digest,
    );

    let held_other = fx.hold_reviewer(&other, "codex-turn-cad792-other-reviewer");
    fx.operator("app_run_cancel", json!({"run_id": other.run_id}))
        .unwrap_or_else(|error| panic!("cancel failed: {error}"));
    let err = refusal(fx.fetch(
        "reviewer",
        &other.artifact_id,
        &held_other.message_id,
        &held_other.token,
    ));
    assert!(
        err.contains("artifact run execution is no longer approved"),
        "cancelled run: {err}"
    );
    assert_artifact(
        &fx.operator_artifact(&other.artifact_id),
        &other.text,
        &other.digest,
    );
    // Cancel leaves the reviewer message running. Finish it so the next
    // run can take the one held turn; the refusal above already stands.
    let cancelled = fx
        .shared
        .store
        .message(&held_other.message_id)
        .unwrap()
        .unwrap();
    fx.shared
        .store
        .finish(
            &cancelled,
            "interrupted",
            &json!({"turn_id": held_other.token}),
            None,
        )
        .unwrap_or_else(|error| panic!("releasing the cancelled turn failed: {error}"));

    // Same reviewer, after the guarded read, approves with the kickoff digest.
    let checked = fx.produce("cad792-checked");
    let held_checked = fx.hold_reviewer(&checked, "codex-turn-cad792-checked-reviewer");
    let checked_digest = fx.kickoff_digest(&held_checked.message_id);
    assert_eq!(checked_digest, checked.digest);
    let read = fx
        .fetch(
            "reviewer",
            &checked.artifact_id,
            &held_checked.message_id,
            &held_checked.token,
        )
        .unwrap_or_else(|error| panic!("guarded read before approve failed: {error}"));
    assert_artifact(&read, &checked.text, &checked.digest);
    fx.submit_approve(&held_checked, &checked.run_id, &checked_digest)
        .unwrap_or_else(|error| panic!("approve after the guarded read failed: {error}"));
    let checked_shown = fx.shared.store.app_run_show(&checked.run_id).unwrap();
    assert!(
        approved(&checked_shown),
        "approve after the real fetch did not succeed: {checked_shown}"
    );
    assert_eq!(
        checked_shown["reviews"][0]["artifact_digest"], checked.digest,
        "{checked_shown}"
    );

    let revoked = fx.produce("cad792-revoke");
    let held_revoked = fx.hold_reviewer(&revoked, "codex-turn-cad792-revoke-reviewer");
    let proven = fx
        .fetch(
            "reviewer",
            &revoked.artifact_id,
            &held_revoked.message_id,
            &held_revoked.token,
        )
        .unwrap_or_else(|error| panic!("pre-revoke fetch failed: {error}"));
    assert_artifact(&proven, &revoked.text, &revoked.digest);
    fx.operator(
        "app_local_install_revoke",
        json!({"install_id": fx.install, "digest": fx.bundle}),
    )
    .unwrap_or_else(|error| panic!("install revoke failed: {error}"));
    let err = refusal(fx.fetch(
        "reviewer",
        &revoked.artifact_id,
        &held_revoked.message_id,
        &held_revoked.token,
    ));
    assert!(err.contains("stale"), "revoked installation: {err}");
    assert_artifact(
        &fx.operator_artifact(&revoked.artifact_id),
        &revoked.text,
        &revoked.digest,
    );
    assert_artifact(
        &fx.operator_artifact(&live.artifact_id),
        &live.text,
        &live.digest,
    );
}

/// Valid pinned approve with no worker fetch is refused. The operator
/// audit read returns the same bytes and must not mint the receipt that
/// would let that approve succeed. A later operator read still returns them.
#[test]
fn cad792_pinned_approve_without_worker_fetch_is_refused_after_operator_read() {
    let fx = Fx::new();
    let produced = fx.produce("cad792-operator-read");
    let held = fx.hold_reviewer(&produced, "codex-turn-cad792-operator-read");
    let digest = fx.kickoff_digest(&held.message_id);
    assert_eq!(
        digest, produced.digest,
        "kickoff digest drifted from the artifact"
    );

    let audited = fx.operator_artifact(&produced.artifact_id);
    assert_artifact(&audited, &produced.text, &produced.digest);
    assert_no_worker_receipt(&fx, &produced.artifact_id);

    let finish = fx.submit_approve(&held, &produced.run_id, &digest);
    let shown = fx.shared.store.app_run_show(&produced.run_id).unwrap();
    assert_approve_refused(&finish, &shown);
    assert_no_worker_receipt(&fx, &produced.artifact_id);

    assert_artifact(
        &fx.operator_artifact(&produced.artifact_id),
        &produced.text,
        &produced.digest,
    );
}

/// The assigned reviewer fetches through the guarded worker route, then
/// the same pinned approve finishes the run.
#[test]
fn cad792_guarded_worker_fetch_then_pinned_approve_succeeds() {
    let fx = Fx::new();
    let produced = fx.produce("cad792-fetched-approve");
    let held = fx.hold_reviewer(&produced, "codex-turn-cad792-fetched-approve");
    let digest = fx.kickoff_digest(&held.message_id);
    assert_eq!(digest, produced.digest);

    let fetched = fx
        .fetch(
            "reviewer",
            &produced.artifact_id,
            &held.message_id,
            &held.token,
        )
        .unwrap_or_else(|error| panic!("guarded worker fetch failed: {error}"));
    assert_artifact(&fetched, &produced.text, &produced.digest);
    assert_worker_receipt(&fx, &produced.artifact_id, &held.message_id, &held.token);

    fx.submit_approve(&held, &produced.run_id, &digest)
        .unwrap_or_else(|error| panic!("approve after the guarded fetch failed: {error}"));
    let shown = fx.shared.store.app_run_show(&produced.run_id).unwrap();
    assert_approve_succeeded(&shown, &digest);
}

/// A receipt for the fetched token does not authorize approve after that
/// token is rotated. Identity still discloses the same held turn under the
/// new token, and no second fetch is performed.
#[test]
fn cad792_fetch_receipt_does_not_survive_turn_rotation() {
    let fx = Fx::new();
    let produced = fx.produce("cad792-rotation");
    let held = fx.hold_reviewer(&produced, "codex-turn-cad792-rotation");
    let digest = fx.kickoff_digest(&held.message_id);
    assert_eq!(digest, produced.digest);

    let fetched = fx
        .fetch(
            "reviewer",
            &produced.artifact_id,
            &held.message_id,
            &held.token,
        )
        .unwrap_or_else(|error| panic!("guarded fetch before rotation failed: {error}"));
    assert_artifact(&fetched, &produced.text, &produced.digest);
    assert_worker_receipt(&fx, &produced.artifact_id, &held.message_id, &held.token);

    let rotated = format!("{}-rotated", held.token);
    fx.shared
        .store
        .mark_running(&held.message_id, &rotated)
        .unwrap_or_else(|error| panic!("turn rotation failed: {error}"));
    let identity = fx.identity("reviewer");
    let turn = own_turn(&identity);
    assert_eq!(turn["id"], held.message_id, "{identity}");
    assert_eq!(turn["turn_id"], rotated, "{identity}");
    assert_eq!(turn["task"], held.task_id, "{identity}");
    assert_worker_receipt(&fx, &produced.artifact_id, &held.message_id, &held.token);
    assert_ne!(
        artifact_digest(rotated.as_bytes()),
        artifact_digest(held.token.as_bytes())
    );

    let finish = fx.submit_approve_token(&held.message_id, &rotated, &produced.run_id, &digest);
    let shown = fx.shared.store.app_run_show(&produced.run_id).unwrap();
    assert_approve_refused(&finish, &shown);
    assert_artifact(
        &fx.operator_artifact(&produced.artifact_id),
        &produced.text,
        &produced.digest,
    );
}

/// `agent_identity` discloses a held turn only for the current
/// registration's recipient uid. Public removal finishes a held turn, so
/// it cannot be the leak: the predecessor message stays `running` under
/// the old `recipient_uid` while the alias's row receives a new uid.
#[test]
fn cad792_same_alias_reregistration_hides_previous_recipient_held_turn() {
    let fx = Fx::new();
    let produced = fx.produce("cad792-rereg-prev");
    let held = fx.hold_reviewer(&produced, "codex-turn-cad792-prev-recipient");
    let before = fx.identity("reviewer");
    let turn = own_turn(&before);
    assert_eq!(turn["id"], held.message_id, "{before}");
    assert_eq!(turn["turn_id"], held.token, "{before}");

    let old_uid = fx.shared.store.agent("reviewer").unwrap().uid;
    let message = fx.shared.store.message(&held.message_id).unwrap().unwrap();
    assert_eq!(message.recipient_uid.as_deref(), Some(old_uid.as_str()));
    assert_eq!(message.state, "running");
    assert_eq!(message.turn_id.as_deref(), Some(held.token.as_str()));
    assert!(
        fx.shared
            .store
            .agent_reads_message(&message, "reviewer")
            .unwrap(),
        "the held turn must be readable before re-registration"
    );

    let new_uid = format!("rereg-{old_uid}");
    fx.shared
        .store
        .fixture_write(|tx| {
            let updated = tx.execute(
                "UPDATE agents SET uid=?1 WHERE alias='reviewer' AND uid=?2",
                rusqlite::params![new_uid.as_str(), old_uid.as_str()],
            )?;
            assert_eq!(
                updated, 1,
                "same-alias re-registration did not mint one uid"
            );
            Ok(())
        })
        .unwrap_or_else(|error| panic!("reviewer re-registration failed: {error}"));

    let predecessor = fx.shared.store.message(&held.message_id).unwrap().unwrap();
    assert_eq!(predecessor.state, "running");
    assert_eq!(predecessor.turn_id.as_deref(), Some(held.token.as_str()));
    assert_eq!(predecessor.recipient_uid.as_deref(), Some(old_uid.as_str()));
    assert_eq!(fx.shared.store.agent("reviewer").unwrap().uid, new_uid);
    assert!(
        !fx.shared
            .store
            .agent_reads_message(&predecessor, "reviewer")
            .unwrap(),
        "the predecessor held turn must not be readable by the new uid"
    );

    let hidden = fx.identity("reviewer");
    assert_eq!(hidden["alias"], "reviewer", "{hidden}");
    let hidden_rows = hidden["running"]
        .as_array()
        .unwrap_or_else(|| panic!("agent_identity must return running turns, got {hidden}"));
    assert!(
        hidden_rows.is_empty(),
        "previous recipient_uid's held turn leaked: {hidden}"
    );
    let hidden_body = hidden.to_string();
    assert!(
        !hidden_body.contains(&held.message_id) && !hidden_body.contains(&held.token),
        "previous registration's held turn was disclosed: {hidden}"
    );

    fx.release_if_running(&held);
    let current = fx.produce("cad792-rereg-next");
    let held_next = fx.hold_reviewer(&current, "codex-turn-cad792-current-recipient");
    let next_message = fx
        .shared
        .store
        .message(&held_next.message_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        next_message.recipient_uid.as_deref(),
        Some(new_uid.as_str())
    );
    assert!(
        fx.shared
            .store
            .agent_reads_message(&next_message, "reviewer")
            .unwrap(),
        "the new registration's held turn must be readable"
    );
    let shown = fx.identity("reviewer");
    let turn = own_turn(&shown);
    assert_eq!(turn["id"], held_next.message_id, "{shown}");
    assert_eq!(turn["turn_id"], held_next.token, "{shown}");
    assert_eq!(turn["task"], held_next.task_id, "{shown}");
    let shown_body = shown.to_string();
    assert!(
        !shown_body.contains(&held.message_id) && !shown_body.contains(&held.token),
        "the new registration's identity still disclosed the previous turn: {shown}"
    );
}
