//! Operator-only Works content RPCs (CAD-1649).
//!
//! Every method proves the operator connection, rejects unknown and
//! identity-shaped fields, proves the installation and live context on
//! core, then opens the installation's record file. The actor is
//! derived from the connection (`verified_owner_human` when the board's
//! public session proves a human, otherwise the literal `operator`);
//! a caller-supplied actor/on-behalf-of is never accepted. v0 is
//! operator-only: registered agents and detached callers are refused
//! before any file opens.

use super::*;
use crate::daemon::app_screen_services_rpc::ScreenServiceProof;
use crate::store::app_records::RecordStore;
use crate::store::app_works::WorksDraft;
use serde_json::Value;

fn required_i64(params: &Value, key: &str) -> Result<i64> {
    params
        .get(key)
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .ok_or_else(|| Error::invalid("invalid", format!("{key} must be a positive integer")))
}

/// The durable principal for a verified board human, scoped by the
/// proof's company, or the local operator. `verified_owner_human`
/// returns an object; reading it as a string would collapse every human
/// into `operator`, losing authorship and the self-review fence.
fn works_actor(human: Option<&Value>) -> Result<String> {
    let Some(human) = human else {
        return Ok("operator".to_string());
    };
    let company = human
        .get("company")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("invalid", "works identity is malformed"))?;
    let sub = human
        .get("sub")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("invalid", "works identity is malformed"))?;
    Ok(format!("human:{company}:{sub}"))
}

fn required_str<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::invalid("invalid", format!("{key} is required")))
}

impl Shared {
    pub(super) fn rpc_app_works(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
        proof: Option<&ScreenServiceProof>,
    ) -> Result<Value> {
        Self::screen_service_method(proof, method)?;
        self.operator_connection("works content management", params, peer_pid)?;
        let allowed: &[&str] = match method {
            "app_works_create" => &[
                "install_id",
                "context_id",
                "public_session",
                "kind",
                "title",
                "content",
                "tags",
                "idempotency_key",
            ],
            "app_works_get" => &["install_id", "context_id", "public_session", "id"],
            "app_works_query" => &[
                "install_id",
                "context_id",
                "public_session",
                "text",
                "kind",
                "review",
                "export",
                "tag",
                "owner",
                "sort",
            ],
            "app_works_patch" => &[
                "install_id",
                "context_id",
                "public_session",
                "id",
                "if_revision",
                "operations",
                "idempotency_key",
            ],
            "app_works_trash" | "app_works_restore" => &[
                "install_id",
                "context_id",
                "public_session",
                "id",
                "if_revision",
                "idempotency_key",
            ],
            "app_works_request_review" => &[
                "install_id",
                "context_id",
                "public_session",
                "id",
                "revision",
            ],
            "app_works_review" => &[
                "install_id",
                "context_id",
                "public_session",
                "id",
                "revision",
                "decision",
                "note",
            ],
            "app_works_export" => &[
                "install_id",
                "context_id",
                "public_session",
                "id",
                "revision",
                "format",
            ],
            _ => return Err(Error::invalid("invalid", "unknown Works content method")),
        };
        let fields = params
            .as_object()
            .ok_or_else(|| Error::invalid("invalid", "Works content payload must be an object"))?;
        for key in fields.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(Error::invalid(
                    "invalid",
                    "Works content payload has unsupported fields",
                ));
            }
        }
        // A screen call derives install/context from the private proof, never
        // from frame fields; a native call takes them from the operator caller.
        let install = match proof {
            Some(proof) => proof.install().to_string(),
            None => required_str(params, "install_id")?.to_string(),
        };
        let context = match proof {
            Some(proof) => proof.context()?.to_string(),
            None => required_str(params, "context_id")?.to_string(),
        };
        let human = self.verified_owner_human(params)?;
        // A review needs a proven reviewer identity; the unverified local
        // operator fallback must not approve or be approved. Refuse before
        // the installation/context or record file is touched.
        if method == "app_works_review" && human.is_none() {
            return Err(Error::invalid(
                "forbidden",
                "a review requires a verified reviewer",
            ));
        }
        let actor = works_actor(human.as_ref())?;
        let result = self.screen_service_scope(&install, &context, true, proof, || {
            let records = RecordStore::open(&self.state_dir, &install)?;
            match method {
                "app_works_create" => {
                    let draft = WorksDraft::parse(params)?;
                    let key = required_str(params, "idempotency_key")?;
                    records.app_works_create(&context, &draft, &actor, key, None)
                }
                "app_works_get" => records.app_works_get(&context, required_str(params, "id")?),
                "app_works_query" => records.app_works_query(&context, params),
                "app_works_patch" => {
                    let id = required_str(params, "id")?;
                    let if_revision = required_i64(params, "if_revision")?;
                    let operations = params
                        .get("operations")
                        .and_then(Value::as_array)
                        .ok_or_else(|| Error::invalid("invalid", "operations must be an array"))?;
                    let key = required_str(params, "idempotency_key")?;
                    records.app_works_patch(
                        &context,
                        id,
                        if_revision,
                        operations,
                        &actor,
                        key,
                        None,
                    )
                }
                "app_works_trash" | "app_works_restore" => {
                    let id = required_str(params, "id")?;
                    let if_revision = required_i64(params, "if_revision")?;
                    let key = required_str(params, "idempotency_key")?;
                    if method == "app_works_trash" {
                        records.app_works_trash(&context, id, if_revision, &actor, key, None)
                    } else {
                        records.app_works_restore(&context, id, if_revision, &actor, key, None)
                    }
                }
                "app_works_request_review" => {
                    let id = required_str(params, "id")?;
                    let revision = required_i64(params, "revision")?;
                    records.app_works_request_review(&context, id, revision, &actor, None)
                }
                "app_works_review" => {
                    let id = required_str(params, "id")?;
                    let revision = required_i64(params, "revision")?;
                    let decision = required_str(params, "decision")?;
                    let note = params.get("note").and_then(Value::as_str);
                    records.app_works_review(&context, id, revision, decision, note, &actor, None)
                }
                "app_works_export" => {
                    let id = required_str(params, "id")?;
                    let revision = required_i64(params, "revision")?;
                    let format = match params.get("format") {
                        None => "html",
                        Some(Value::String(format)) => format.as_str(),
                        Some(_) => {
                            return Err(Error::invalid("invalid", "format must be a string"))
                        }
                    };
                    records.app_works_export(&context, id, revision, format, &actor, None)
                }
                _ => Err(Error::invalid("invalid", "unknown Works content method")),
            }
        })?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "test-seam")]
    use crate::test_seam::{scoped, Asserted};
    use serde_json::json;

    #[test]
    fn cad1649_actor_derives_from_verified_human() {
        let alice = json!({"sub": "u-alice", "company": "acme"});
        let bob = json!({"sub": "u-bob", "company": "acme"});
        assert_eq!(works_actor(Some(&alice)).unwrap(), "human:acme:u-alice");
        assert_eq!(works_actor(Some(&bob)).unwrap(), "human:acme:u-bob");
        assert_ne!(
            works_actor(Some(&alice)).unwrap(),
            works_actor(Some(&bob)).unwrap()
        );
        assert_eq!(works_actor(None).unwrap(), "operator");
        assert!(works_actor(Some(&json!({"sub": "u"}))).is_err());
    }

    #[cfg(feature = "test-seam")]
    fn pid() -> u32 {
        std::process::id()
    }

    /// CAD-1649 ticket-authored acceptance: the real operator connection
    /// refuses a registered agent and an unproven caller before the
    /// installation/context is read or any record file is opened.
    #[cfg(feature = "test-seam")]
    #[test]
    fn cad1649_works_rpc_refuses_agent_and_unproven() {
        let dir = tempfile::Builder::new()
            .prefix("c1649-works")
            .tempdir()
            .unwrap();
        let pm = dir.path().join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let opts = crate::daemon::ServeOptions {
            test_seam: true,
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let shared = Shared::new(dir.path(), &opts).unwrap();
        let cwd = dir.path().to_str().unwrap().to_string();
        shared
            .store
            .register_agent(&crate::store::NewAgent {
                alias: "works-worker",
                provider: "fake",
                endpoint_kind: "fake",
                role: "worker",
                cwd: &cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        let source = format!("{}/tests/fixtures/apps/crm", env!("CARGO_MANIFEST_DIR"));
        scoped(Asserted::Operator, || {
            shared.dispatch("app_workspace_install", &json!({"source": source}), pid())
        })
        .unwrap();
        let params = json!({
            "install_id": "missing",
            "context_id": "missing",
            "kind": "doc",
            "title": "X",
            "content": {"blocks": [{"type": "h2", "text": "X"}]},
            "idempotency_key": "k",
        });
        for who in [
            Asserted::Agent("works-worker".to_string()),
            Asserted::Unproven,
        ] {
            let error =
                scoped(who, || shared.dispatch("app_works_create", &params, pid())).unwrap_err();
            let message = error.to_string();
            assert!(
                message.contains("operator action") || message.contains("works-worker"),
                "{message}"
            );
        }
        let review = json!({
            "install_id": "missing",
            "context_id": "missing",
            "id": "x",
            "revision": 1,
            "decision": "approve",
        });
        let error = scoped(Asserted::Operator, || {
            shared.dispatch("app_works_review", &review, pid())
        })
        .unwrap_err();
        assert!(error.to_string().contains("verified reviewer"), "{error}");
    }
}
