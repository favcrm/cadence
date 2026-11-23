//! `cadence memory` reads the tracker directly, but all authority-bearing
//! writes are daemon RPCs. The daemon derives the native endpoint identity;
//! request aliases and operator fallbacks are never accepted.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

mod args;
mod query;

use serde_json::json;

use crate::client;
use crate::error::{Error, Result};
use crate::memory::{self, Scope};

pub use args::MemoryAction;

/// One `memory_propose` request, as the CLI and the agent MCP tool both
/// send it (CAD-2662). `project: None` leaves the project to the daemon,
/// which takes it from the calling agent's running issue turn.
pub struct Proposal<'a> {
    pub project: Option<&'a str>,
    pub kind: &'a str,
    pub scope: &'a Scope,
    pub source: Option<&'a str>,
    pub confidence: Option<&'a str>,
    pub from: Option<&'a str>,
    pub text: Option<&'a str>,
    pub id: Option<&'a str>,
    pub nature: &'a str,
    pub audience: &'a str,
    pub roles: &'a [String],
    pub evidence_refs: &'a [memory::policy::EvidenceRef],
    pub verification_condition: Option<&'a memory::policy::VerificationCondition>,
}

/// The `memory_propose` RPC params for `proposal`.
pub fn propose_params(proposal: &Proposal<'_>) -> serde_json::Value {
    let mut params = json!({
        "kind": proposal.kind,
        "scope": proposal.scope,
        "source": proposal.source,
        "confidence": proposal.confidence,
        "from": proposal.from,
        "text": proposal.text,
        "id": proposal.id,
        "nature": proposal.nature,
        "audience": proposal.audience,
        "roles": proposal.roles,
        "evidence_refs": proposal.evidence_refs,
        "verification_condition": proposal.verification_condition,
    });
    if let Some(project) = proposal.project {
        params["project"] = json!(project);
    }
    params
}

/// The `memory_match` RPC params for a turn lookup — one shape for the CLI's
/// `memory match --turn` and the agent MCP tool (CAD-2662).
pub fn turn_match_params(
    components: &[String],
    paths: &[String],
    tags: &[String],
) -> serde_json::Value {
    json!({"components": components, "paths": paths, "tags": tags})
}

/// `cadence memory …` — returns the process exit code.
pub fn run(action: &MemoryAction, state_dir: &std::path::Path) -> Result<i32> {
    match action {
        MemoryAction::Propose {
            project,
            kind,
            components,
            paths,
            providers,
            tags,
            project_wide,
            source,
            confidence,
            from,
            text,
            id,
            nature,
            audience,
            roles,
            evidence_refs,
            verification_condition,
        } => {
            let scope = Scope {
                components: components.clone(),
                paths: paths.clone(),
                providers: providers.clone(),
                tags: tags.clone(),
                project: *project_wide,
            };
            if from.is_some() && text.is_some() {
                return Err(Error::rejected("propose takes --from or -m, not both"));
            }
            let from_text = from
                .as_ref()
                .map(std::fs::read_to_string)
                .transpose()
                .map_err(|e| Error::rejected(format!("cannot read proposal source: {e}")))?;
            let evidence_refs: Vec<memory::policy::EvidenceRef> = evidence_refs
                .iter()
                .map(|reference| {
                    serde_json::from_str(reference).map_err(|error| {
                        Error::rejected(format!("invalid evidence reference: {error}"))
                    })
                })
                .collect::<Result<_>>()?;
            let verification_condition: Option<memory::policy::VerificationCondition> =
                verification_condition
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()
                    .map_err(|error| {
                        Error::rejected(format!("invalid verification condition: {error}"))
                    })?;
            let out = client::rpc(
                state_dir,
                "memory_propose",
                propose_params(&Proposal {
                    project: project.as_deref(),
                    kind,
                    scope: &scope,
                    source: source.as_deref(),
                    confidence: confidence.as_deref(),
                    from: from_text.as_deref(),
                    text: text.as_deref(),
                    id: id.as_deref(),
                    nature,
                    audience,
                    roles,
                    evidence_refs: &evidence_refs,
                    verification_condition: verification_condition.as_ref(),
                }),
            )?;
            crate::issue::cli::print_json(&out);
            Ok(0)
        }
        MemoryAction::Accept {
            slug,
            project,
            digest,
            evidence,
        }
        | MemoryAction::Verify {
            slug,
            project,
            digest,
            evidence,
        } => {
            let operation = if matches!(action, MemoryAction::Accept { .. }) {
                "accept"
            } else {
                "verify"
            };
            let out = client::rpc(
                state_dir,
                "memory_finalize",
                json!({
                    "project": project, "slug": slug, "operation": operation,
                    "digest": digest, "evidence": evidence,
                }),
            )?;
            crate::issue::cli::print_json(&out);
            Ok(0)
        }
        MemoryAction::Reject {
            slug,
            project,
            digest,
            evidence,
        }
        | MemoryAction::Revalidate {
            slug,
            project,
            digest,
            evidence,
        }
        | MemoryAction::Invalidate {
            slug,
            project,
            digest,
            evidence,
        } => {
            let operation = match action {
                MemoryAction::Reject { .. } => "reject",
                MemoryAction::Revalidate { .. } => "revalidate",
                _ => "invalidate",
            };
            let out = client::rpc(
                state_dir,
                "memory_finalize",
                json!({
                    "project": project, "slug": slug, "operation": operation,
                    "digest": digest, "evidence": evidence,
                }),
            )?;
            crate::issue::cli::print_json(&out);
            Ok(0)
        }
        MemoryAction::Supersede { old, new, project } => {
            let out = client::rpc(
                state_dir,
                "memory_finalize",
                json!({
                    "project": project,
                    "old": old,
                    "new": new,
                    "operation": "supersede",
                }),
            )?;
            crate::issue::cli::print_json(&out);
            Ok(0)
        }
        MemoryAction::Classify {
            slug,
            project,
            digest,
            evidence,
            evidence_refs,
        } => {
            let refs: Vec<memory::policy::EvidenceRef> = evidence_refs
                .iter()
                .map(|reference| {
                    serde_json::from_str(reference).map_err(|error| {
                        Error::rejected(format!("invalid evidence reference: {error}"))
                    })
                })
                .collect::<Result<_>>()?;
            let out = client::rpc(
                state_dir,
                "memory_classify",
                json!({
                    "project": project, "slug": slug, "digest": digest,
                    "provenance": "agent-verified", "evidence": evidence, "evidence_refs": refs,
                }),
            )?;
            crate::issue::cli::print_json(&out);
            Ok(0)
        }
        MemoryAction::Feedback {
            slug,
            project,
            digest,
            message,
            token,
            kind,
            evidence,
        } => {
            let out = client::rpc(
                state_dir,
                "memory_feedback",
                json!({
                    "project": project, "slug": slug, "digest": digest,
                    "message": message, "token": token, "kind": kind, "evidence": evidence,
                }),
            )?;
            crate::issue::cli::print_json(&out);
            Ok(0)
        }
        MemoryAction::Review {
            slug,
            project,
            operation,
            verdict,
            evidence,
            digest,
        } => {
            let out = client::rpc(
                state_dir,
                "memory_review",
                json!({
                    "project": project,
                    "slug": slug,
                    "operation": operation,
                    "verdict": verdict,
                    "evidence": evidence,
                    "digest": digest,
                }),
            )?;
            crate::issue::cli::print_json(&out);
            Ok(0)
        }
        MemoryAction::Match {
            turn: true,
            component,
            path,
            tag,
            json: as_json,
            ..
        } => {
            let out = client::rpc(
                state_dir,
                "memory_match",
                turn_match_params(component, path, tag),
            )?;
            if *as_json {
                crate::issue::cli::print_json(&out);
            } else {
                match out["lessons"].as_str().filter(|text| !text.is_empty()) {
                    Some(text) => print!("{text}"),
                    None => println!("No project lessons apply to this task."),
                }
            }
            Ok(0)
        }
        MemoryAction::Ls { .. }
        | MemoryAction::Show { .. }
        | MemoryAction::Match { turn: false, .. }
        | MemoryAction::Lint { .. } => query::run(action),
    }
}
