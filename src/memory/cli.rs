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
                json!({
                    "project": project,
                    "kind": kind,
                    "scope": scope,
                    "source": source,
                    "confidence": confidence,
                    "from": from_text,
                    "text": text,
                    "id": id,
                    "nature": nature,
                    "audience": audience,
                    "roles": roles,
                    "evidence_refs": evidence_refs,
                    "verification_condition": verification_condition,
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
        MemoryAction::Ls { .. }
        | MemoryAction::Show { .. }
        | MemoryAction::Match { .. }
        | MemoryAction::Lint { .. } => query::run(action),
    }
}
