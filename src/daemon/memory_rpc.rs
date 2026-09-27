//! CAD-534: `cadence daemon` memory RPC handlers — moved verbatim from src/daemon.rs.

use super::*;

use crate::memory;
use crate::memory::IdentityProof;
use crate::memory::NativeIdentity;

impl Shared {
    /// Resolve a memory actor through [`Self::caller_identity`]: only a
    /// verified pm or worker endpoint may author, review or finalize.
    fn memory_actor(&self, peer_pid: u32) -> Result<NativeIdentity> {
        let verified = match self.caller_identity(peer_pid)? {
            Caller::Agent(v) => *v,
            Caller::NoAgentIdentity => {
                return Err(Error::rejected(format!(
                    "Memory caller pid {peer_pid} has no agent identity — it descends \
                     from no registered pane and no enrolled managed endpoint; memory \
                     actions are agent-authenticated and such a caller is never given \
                     an agent's identity"
                )))
            }
        };
        if !matches!(verified.agent.role.as_str(), "pm" | "worker") {
            return Err(Error::rejected(format!(
                "Memory caller '{}' has role '{}' — only pm and worker endpoints \
                 author or review memory",
                verified.agent.alias, verified.agent.role
            )));
        }
        Ok(NativeIdentity {
            proof: IdentityProof {
                alias: verified.agent.alias,
                registration: verified.agent.created.to_bits(),
                generation: verified.generation,
                process_start: verified.process_start,
                role: verified.agent.role,
            },
        })
    }

    /// The tracker memory RPCs read and write — [`Self::pm_dir`], so an
    /// in-process test daemon pins its own and never races another
    /// test over the process-wide `CADENCE_PM_DIR`.
    fn memory_pm(&self) -> Result<crate::issue::Pm> {
        self.pm()
    }

    fn reject_memory_identity_claims(params: &Value) -> Result<()> {
        for field in [
            "actor",
            "alias",
            "by",
            "generation",
            "identity",
            "pane",
            "pid",
            "process_start",
            "reviewer",
        ] {
            if params.get(field).is_some() {
                return Err(Error::rejected(format!(
                    "memory identity is connection-bound; request field '{field}' is not accepted"
                )));
            }
        }
        Ok(())
    }

    pub(super) fn rpc_memory_propose(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_memory_identity_claims(params)?;
        let actor = self.memory_actor(peer_pid)?;
        let pm = self.memory_pm()?;
        let key = required_str(params, "project")?;
        let kind = required_str(params, "kind")?;
        let scope = params
            .get("scope")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| Error::rejected(format!("invalid memory scope: {e}")))?
            .unwrap_or_default();
        memory::propose_native(
            &pm,
            key,
            kind,
            &scope,
            optional_str(params, "source"),
            optional_str(params, "confidence"),
            optional_str(params, "from"),
            optional_str(params, "text"),
            optional_str(params, "id"),
            &actor,
        )
    }

    pub(super) fn rpc_memory_review(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_memory_identity_claims(params)?;
        let actor = self.memory_actor(peer_pid)?;
        let pm = self.memory_pm()?;
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
        Self::reject_memory_identity_claims(params)?;
        if params.get("body").is_some() || params.get("edit").is_some() {
            return Err(Error::rejected(
                "editing memory content during finalization is refused; submit a new proposal",
            ));
        }
        let actor = self.memory_actor(peer_pid)?;
        let pm = self.memory_pm()?;
        let operation = required_str(params, "operation")?;
        match operation {
            "reject" => memory::reject_native(
                &pm,
                optional_str(params, "project"),
                required_str(params, "slug")?,
                &actor,
            ),
            "supersede" => memory::supersede_native(
                &pm,
                optional_str(params, "project"),
                required_str(params, "old")?,
                required_str(params, "new")?,
                &actor,
            ),
            "accept" | "verify" => memory::finalize_native(
                &pm,
                optional_str(params, "project"),
                required_str(params, "slug")?,
                operation,
                required_str(params, "digest")?,
                &actor,
            ),
            _ => Err(Error::rejected("unsupported memory finalization operation")),
        }
    }
}
