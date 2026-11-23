//! The shared strict role/provider/model allowlist decision.
//!
//! Native `src/pi_policy.rs` delegates this decision and keeps its own
//! operator-facing refusal text; no unrelated Pi fallback behaviour lives
//! here. An absent policy or an empty applicable list allows nothing.
use crate::connection_dispatch::dto::{RoleModelPolicyV1, RoleV1};
use crate::connection_dispatch::primitives::ModelId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiModelRefusalV1 {
    /// No `[pi]` policy is configured: nothing is allowed.
    AbsentPolicy,
    /// The applicable allowlist is present but empty: nothing is allowed.
    EmptyAllowList,
    /// The exact model id is not listed for this role.
    NotAllowed,
    /// A master model must name `provider/id`.
    MasterNeedsProvider,
    /// An agentic provider runs its own tools and bypasses guardrails.
    AgenticProviderForMaster,
}

impl PiModelRefusalV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            PiModelRefusalV1::AbsentPolicy => "no pi policy allows a model",
            PiModelRefusalV1::EmptyAllowList => "the applicable allowlist is empty",
            PiModelRefusalV1::NotAllowed => "the model is not on the operator's allowlist",
            PiModelRefusalV1::MasterNeedsProvider => {
                "a master model must name provider/id so its execution class is known"
            }
            PiModelRefusalV1::AgenticProviderForMaster => {
                "a master model uses an agentic provider that bypasses the tool allowlist"
            }
        }
    }
}

/// The exact allowlist `role` launches under: the role's own list when
/// present (even empty), else the common list.
pub fn role_allowlist(policy: &RoleModelPolicyV1, role: RoleV1) -> &[ModelId] {
    match role {
        RoleV1::Master => policy.master_allow.as_deref().unwrap_or(&policy.allow),
        RoleV1::Worker => policy.worker_allow.as_deref().unwrap_or(&policy.allow),
    }
}

pub fn pi_model_decision(
    role: RoleV1,
    model: &ModelId,
    policy: Option<&RoleModelPolicyV1>,
) -> Result<(), PiModelRefusalV1> {
    if role == RoleV1::Master && model.provider().is_none() {
        return Err(PiModelRefusalV1::MasterNeedsProvider);
    }
    let provider = model.provider().unwrap_or(model.as_str());
    if role == RoleV1::Master
        && (provider == "cursor"
            || policy.is_some_and(|policy| {
                policy
                    .agentic_providers
                    .iter()
                    .any(|agentic| agentic.as_str() == provider)
            }))
    {
        return Err(PiModelRefusalV1::AgenticProviderForMaster);
    }
    let Some(policy) = policy else {
        return Err(PiModelRefusalV1::AbsentPolicy);
    };
    let allow = role_allowlist(policy, role);
    if allow.is_empty() {
        return Err(PiModelRefusalV1::EmptyAllowList);
    }
    if allow.iter().any(|allowed| allowed == model) {
        Ok(())
    } else {
        Err(PiModelRefusalV1::NotAllowed)
    }
}
