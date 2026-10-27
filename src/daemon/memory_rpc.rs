//! Native memory curation: connection evidence, never caller identity labels.

use super::*;
use crate::memory::{self, policy, IdentityProof, NativeIdentity};

impl Shared {
    fn memory_actor(&self, peer_pid: u32, capture_model: bool) -> Result<policy::NativeActor> {
        let verified = match self.caller_identity(peer_pid)? {
            Caller::Agent(verified) => *verified,
            Caller::NoAgentIdentity => {
                self.proven_operator("memory curation", peer_pid)?;
                return Ok(policy::NativeActor::Operator {
                    uid: unsafe { libc::geteuid() },
                });
            }
        };
        if !matches!(verified.agent.role.as_str(), "pm" | "worker") {
            return Err(Error::rejected(
                "memory curation requires a native PM/worker endpoint or proven operator",
            ));
        }
        let model = if capture_model {
            self.memory_model_evidence(&verified.agent)?
        } else {
            None
        };
        Ok(policy::NativeActor::Agent {
            identity: Self::memory_identity(verified),
            model,
        })
    }

    fn memory_identity(verified: super::identity::VerifiedAgent) -> NativeIdentity {
        NativeIdentity {
            proof: IdentityProof {
                alias: verified.agent.alias,
                registration: verified.agent.created.to_bits(),
                generation: verified.generation,
                process_start: verified.process_start,
                role: verified.agent.role,
                uid: Some(verified.agent.uid),
            },
        }
    }

    /// `set_identity` commits this model and generation with its native
    /// ready event. Params/configuration and a row-only substitution do not
    /// provide the matching launch/open evidence.
    fn memory_model_evidence(
        &self,
        agent: &crate::store::Agent,
    ) -> Result<Option<policy::ModelEvidence>> {
        let Some(model) = agent
            .model
            .as_deref()
            .filter(|model| !model.trim().is_empty())
        else {
            return Ok(None);
        };
        let Some(endpoint_generation) = agent
            .generation
            .as_deref()
            .filter(|generation| !generation.trim().is_empty())
        else {
            return Ok(None);
        };
        let Some(ready) = self.store.last_event_of(&agent.alias, &["ready"])? else {
            return Ok(None);
        };
        // Managed native proof binds a composite owner generation. The ready
        // event carries the endpoint generation, not that enrollment binding.
        if ready.at < agent.created
            || ready.payload.get("generation").and_then(Value::as_str) != Some(endpoint_generation)
            || ready.payload.get("model").and_then(Value::as_str) != Some(model)
        {
            return Ok(None);
        }
        Ok(Some(policy::ModelEvidence {
            provider: agent.provider.clone(),
            model: model.to_owned(),
            vendor: policy::model_vendor(model).map(str::to_owned),
            ready_seq: ready.seq,
        }))
    }

    fn memory_fields(params: &Value, allowed: &[&str]) -> Result<()> {
        let object = params
            .as_object()
            .ok_or_else(|| Error::rejected("memory request must be an object"))?;
        if let Some(field) = object
            .keys()
            .find(|field| !allowed.contains(&field.as_str()))
        {
            return Err(Error::rejected(format!(
                "memory request field '{field}' is not accepted"
            )));
        }
        Ok(())
    }

    fn memory_value<T: serde::de::DeserializeOwned>(
        params: &Value,
        field: &str,
    ) -> Result<Option<T>> {
        params
            .get(field)
            .filter(|value| !value.is_null())
            .map(|value| {
                serde::Deserialize::deserialize(value)
                    .map_err(|error| Error::rejected(format!("invalid memory {field}: {error}")))
            })
            .transpose()
    }

    pub(super) fn rpc_memory_propose(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::memory_fields(
            params,
            &[
                "project",
                "kind",
                "scope",
                "source",
                "confidence",
                "from",
                "text",
                "id",
                "nature",
                "audience",
                "roles",
                "evidence_refs",
                "verification_condition",
            ],
        )?;
        let actor = self.memory_actor(peer_pid, true)?;
        let pm = self.pm()?;
        let scope = params
            .get("scope")
            .filter(|scope| !scope.is_null())
            .map(memory::v2::scope_input)
            .transpose()?
            .unwrap_or_default();
        let options = policy::ProposalOptions {
            nature: Self::memory_value(params, "nature")?.unwrap_or_default(),
            audience: Self::memory_value(params, "audience")?.unwrap_or_default(),
            roles: Self::memory_value(params, "roles")?.unwrap_or_default(),
            evidence_refs: Self::memory_value(params, "evidence_refs")?.unwrap_or_default(),
            verification_condition: Self::memory_value(params, "verification_condition")?,
        };
        memory::propose_native(
            &pm,
            required_str(params, "project")?,
            required_str(params, "kind")?,
            scope,
            optional_str(params, "source"),
            optional_str(params, "confidence"),
            optional_str(params, "from"),
            optional_str(params, "text"),
            optional_str(params, "id"),
            options,
            &actor,
        )
    }

    pub(super) fn rpc_memory_review(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::memory_fields(
            params,
            &[
                "project",
                "slug",
                "operation",
                "verdict",
                "evidence",
                "digest",
            ],
        )?;
        let actor = self.memory_actor(peer_pid, true)?;
        let pm = self.pm()?;
        let request = memory::ReviewRequest {
            operation: required_str(params, "operation")?,
            verdict: required_str(params, "verdict")?,
            evidence: required_str(params, "evidence")?,
            expected_digest: required_str(params, "digest")?,
        };
        memory::submit_review(
            &pm,
            optional_str(params, "project"),
            required_str(params, "slug")?,
            &request,
            &actor,
        )
    }

    pub(super) fn rpc_memory_finalize(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::memory_fields(
            params,
            &[
                "project",
                "slug",
                "operation",
                "digest",
                "evidence",
                "old",
                "new",
            ],
        )?;
        let actor = self.memory_actor(peer_pid, true)?;
        let pm = self.pm()?;
        let project = optional_str(params, "project");
        match required_str(params, "operation")? {
            "supersede" => memory::supersede_native(
                &pm,
                project,
                required_str(params, "old")?,
                required_str(params, "new")?,
                &actor,
            ),
            "reject" => memory::reject_native(
                &pm,
                project,
                required_str(params, "slug")?,
                required_str(params, "digest")?,
                optional_str(params, "evidence"),
                &actor,
            ),
            operation @ ("accept" | "verify" | "revalidate" | "invalidate") => {
                memory::finalize_native(
                    &pm,
                    project,
                    required_str(params, "slug")?,
                    operation,
                    required_str(params, "digest")?,
                    optional_str(params, "evidence"),
                    &actor,
                )
            }
            _ => Err(Error::rejected("unsupported memory finalization operation")),
        }
    }

    pub(super) fn rpc_memory_classify(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::memory_fields(
            params,
            &[
                "project",
                "slug",
                "digest",
                "provenance",
                "evidence",
                "evidence_refs",
            ],
        )?;
        let actor = self.memory_actor(peer_pid, false)?;
        if !matches!(actor, policy::NativeActor::Operator { .. }) {
            return Err(Error::rejected(
                "source provenance classification requires the operator",
            ));
        }
        if required_str(params, "provenance")? != "agent-verified" {
            return Err(Error::rejected(
                "classification accepts only operator-attested agent-verified provenance",
            ));
        }
        let pm = self.pm()?;
        memory::v2::classify(
            &pm,
            optional_str(params, "project"),
            required_str(params, "slug")?,
            required_str(params, "digest")?,
            required_str(params, "evidence")?,
            Self::memory_value(params, "evidence_refs")?.unwrap_or_default(),
            &actor,
        )
    }

    pub(super) fn rpc_memory_feedback(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::memory_fields(
            params,
            &[
                "project", "slug", "digest", "message", "token", "kind", "evidence",
            ],
        )?;
        let Caller::Agent(verified) = self.caller_identity(peer_pid)? else {
            return Err(Error::rejected(
                "usage reports require the assigned native agent",
            ));
        };
        if !matches!(verified.agent.role.as_str(), "pm" | "worker") {
            return Err(Error::rejected(
                "usage reports require a native PM/worker endpoint",
            ));
        }
        let id = required_str(params, "message")?;
        let token = required_str(params, "token")?;
        let evidence = optional_str(params, "evidence");
        if evidence.is_some_and(|evidence| evidence.contains(token)) {
            return Err(Error::rejected(
                "feedback evidence must not contain the raw turn token",
            ));
        }
        let kind = Self::memory_value(params, "kind")?
            .ok_or_else(|| Error::rejected("feedback kind is required"))?;
        let project = required_str(params, "project")?;
        let digest = required_str(params, "digest")?;
        let pm = self.pm()?;
        let mut write = memory::v2::FeedbackWrite::lock(
            &pm,
            project,
            required_str(params, "slug")?,
            digest,
            kind,
        )?;
        let super::identity::VerifiedAgent {
            agent: expected,
            generation,
            process_start,
        } = *verified;
        let outcome =
            self.store
                .with_memory_feedback(&expected, id, token, |agent, message, bound_issue| {
                    if self.adoption_pending(&message.alias, id, token) {
                        return Err(Error::rejected(
                            "feedback requires a settled native turn adoption",
                        ));
                    }
                    let issue = crate::issue::board::find_issue(&pm.dir, &bound_issue)?;
                    if issue.project != project {
                        return Err(Error::rejected(
                            "feedback memory is outside the active native issue project",
                        ));
                    }
                    let proof = policy::NativeFeedbackProof {
                        project: issue.project,
                        digest: digest.to_owned(),
                        actor: Self::memory_identity(super::identity::VerifiedAgent {
                            agent,
                            generation,
                            process_start,
                        })
                        .proof,
                        message: message.id,
                        task: message.task_id,
                        validated_at: crate::issue::time::iso(crate::issue::time::now_epoch()),
                    };
                    write.save(evidence, proof)
                });
        write.finish(outcome)
    }
}
