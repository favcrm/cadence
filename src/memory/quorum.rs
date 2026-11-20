//! Semantic revision digests, review-cycle quorum checks, and retrieval eligibility.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::HashSet;

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{
    policy, v2, valid_identity_proof, FinalizationReceipt, Front, IdentityProof, Memory, Scope,
};

/// Stable semantic revision digest. This deliberately excludes lifecycle
/// status, review timestamps and receipt history: changing any claim or its
/// applicability invalidates the old quorum, while a timestamp-only update
/// does not create a new claim.
pub fn semantic_digest(mem: &Memory) -> String {
    #[derive(Serialize)]
    struct Revision<'a> {
        project: &'a str,
        id: &'a str,
        kind: &'a str,
        body: &'a str,
        author: &'a Option<String>,
        author_proof: &'a Option<IdentityProof>,
        contributors: &'a [IdentityProof],
        source: &'a Option<String>,
        confidence: &'a str,
        scope: &'a Scope,
        supersedes: &'a Option<String>,
    }
    let revision = Revision {
        project: &mem.project,
        id: &mem.front.id,
        kind: &mem.front.kind,
        body: &mem.body,
        author: &mem.front.author,
        author_proof: &mem.front.author_proof,
        contributors: &mem.front.contributors,
        source: &mem.front.source,
        confidence: &mem.front.confidence,
        scope: &mem.front.scope,
        supersedes: &mem.front.supersedes,
    };
    let bytes = match &mem.front.policy {
        Some(policy) => serde_json::to_vec(&(
            "memory-policy-v2",
            &revision,
            policy::semantic_fields(policy),
        )),
        // Do not serialize even a null policy field into old claim digests.
        None => serde_json::to_vec(&revision),
    }
    .expect("memory semantic revision is serializable");
    format!("{:x}", Sha256::digest(bytes))
}

/// Return the exact current review cycle for an operation. A cycle is only
/// live while its operation is recorded in `active_operation`; finalization
/// clears that marker so an old quorum cannot be submitted twice.
pub(super) fn operation_cycle(front: &Front, operation: &str) -> Option<u64> {
    if front.active_operation.as_deref() != Some(operation) {
        return None;
    }
    match operation {
        "accept" if front.status == "proposed" && front.review_cycle > 0 => {
            Some(front.review_cycle)
        }
        "verify" if front.status == "accepted" && front.review_cycle > 1 => {
            Some(front.review_cycle)
        }
        _ => None,
    }
}

fn finalization_for<'a>(
    front: &'a Front,
    operation: &str,
    cycle: Option<u64>,
    digest: &str,
) -> Option<&'a FinalizationReceipt> {
    let receipt = front
        .finalizations
        .iter()
        .filter(|receipt| {
            receipt.operation == operation && cycle.is_none_or(|expected| receipt.cycle == expected)
        })
        .max_by_key(|receipt| receipt.cycle)?;
    (receipt.digest == digest
        && valid_identity_proof(&receipt.finalizer)
        && receipt.finalizer.role == "pm"
        && !proposal_identity_matches(front, &receipt.finalizer)
        && !receipt.finalized_at.is_empty())
    .then_some(receipt)
}

fn same_identity_or_alias(left: &IdentityProof, right: &IdentityProof) -> bool {
    left.stable_id() == right.stable_id() || left.alias == right.alias
}

pub(super) fn proposal_identity_matches(front: &Front, identity: &IdentityProof) -> bool {
    front
        .author_proof
        .as_ref()
        .is_some_and(|author| same_identity_or_alias(author, identity))
        || front
            .contributors
            .iter()
            .any(|contributor| same_identity_or_alias(contributor, identity))
}

pub(super) fn next_verify_cycle(front: &Front) -> u64 {
    let last = front
        .finalizations
        .iter()
        .filter(|receipt| receipt.operation == "verify")
        .map(|receipt| receipt.cycle)
        .max()
        .unwrap_or(1);
    last.max(front.review_cycle).saturating_add(1).max(2)
}

/// Whether a memory has the authenticated quorum needed for operation, and
/// a precise reason when it does not. This is used both by finalization and
/// by all retrieval paths; status alone is never enough.
fn quorum_status_for_cycle(mem: &Memory, operation: &str, cycle: u64) -> (bool, String) {
    let Some(author) = mem.front.author_proof.as_ref() else {
        return (
            false,
            "review blocked: proposer has no authenticated native identity".to_string(),
        );
    };
    if !valid_identity_proof(author) {
        return (
            false,
            "review blocked: proposer identity proof is incomplete".to_string(),
        );
    }
    if cycle == 0 {
        return (
            false,
            format!("review blocked: invalid {operation} review cycle"),
        );
    }
    let digest = semantic_digest(mem);
    let author_id = author.stable_id();
    let contributor_ids: HashSet<String> = mem
        .front
        .contributors
        .iter()
        .map(IdentityProof::stable_id)
        .collect();
    let contributor_aliases: HashSet<String> = mem
        .front
        .contributors
        .iter()
        .map(|c| c.alias.clone())
        .collect();
    let mut identities = HashSet::new();
    let mut aliases = HashSet::new();
    let mut passes = 0usize;
    let mut disagreement = false;
    for receipt in &mem.front.reviews {
        if receipt.operation != operation || receipt.cycle != cycle {
            continue;
        }
        let identity_prefix = format!("{}#", receipt.reviewer);
        let valid_registration = receipt
            .identity
            .strip_prefix(&identity_prefix)
            .and_then(|registration| registration.parse::<u64>().ok())
            .is_some_and(|registration| registration != 0);
        if receipt.digest != digest
            || receipt.evidence.trim().is_empty()
            || !matches!(receipt.role.as_str(), "pm" | "worker")
            || receipt.reviewer.is_empty()
            || !valid_registration
            || receipt.generation.is_empty()
            || receipt.process_start == 0
        {
            continue;
        }
        if receipt.verdict == "revise" {
            disagreement = true;
            continue;
        }
        if receipt.verdict != "pass"
            || receipt.identity == author_id
            || receipt.reviewer == author.alias
            || contributor_ids.contains(&receipt.identity)
            || contributor_aliases.contains(&receipt.reviewer)
        {
            continue;
        }
        if !identities.insert(receipt.identity.clone()) || !aliases.insert(receipt.reviewer.clone())
        {
            continue;
        }
        passes += 1;
    }
    if disagreement {
        return (
            false,
            format!(
                "review blocked: an authenticated reviewer requested revision in {operation} cycle {cycle}"
            ),
        );
    }
    if passes < 2 {
        return (
            false,
            format!(
                "review blocked: {passes}/2 distinct authenticated reviewers passed {operation} cycle {cycle}"
            ),
        );
    }
    (
        true,
        format!("{passes}/2 authenticated reviewers passed {operation} cycle {cycle}"),
    )
}

pub fn quorum_status(mem: &Memory, operation: &str) -> (bool, String) {
    if mem.front.policy.is_some() {
        return v2::quorum_status(mem, operation);
    }
    let Some(author) = mem.front.author_proof.as_ref() else {
        return (
            false,
            "review blocked: proposer has no authenticated native identity".to_string(),
        );
    };
    if !valid_identity_proof(author) {
        return (
            false,
            "review blocked: proposer identity proof is incomplete".to_string(),
        );
    }
    let Some(cycle) = operation_cycle(&mem.front, operation) else {
        return (
            false,
            format!("review blocked: no active {operation} review cycle"),
        );
    };
    quorum_status_for_cycle(mem, operation, cycle)
}

/// Retrieval eligibility is stricter than a lifecycle status. Accepted
/// records must retain their PM acceptance finalization; once a fresh verify
/// cycle is opened, native reviewer receipts do not make the lesson eligible
/// until a PM records a matching finalization for that cycle.
pub fn retrieval_status(mem: &Memory) -> (bool, String) {
    if mem.front.policy.is_some() {
        return v2::retrieval_status(mem);
    }
    if mem.front.status != "accepted" {
        return (
            false,
            format!("review blocked: memory status is {}", mem.front.status),
        );
    }
    if mem.front.author_proof.is_none() {
        return (
            false,
            "review blocked: accepted record has no authenticated proposer".to_string(),
        );
    }
    let digest = semantic_digest(mem);
    let Some(accept_finalization) = finalization_for(&mem.front, "accept", None, &digest) else {
        return (
            false,
            "review blocked: accepted record has no matching PM acceptance finalization"
                .to_string(),
        );
    };
    if accept_finalization.cycle != 1 {
        return (
            false,
            "review blocked: acceptance finalization is bound to an invalid cycle".to_string(),
        );
    }
    if mem.front.review_cycle == 0 {
        return (
            false,
            "review blocked: accepted record has no current review cycle".to_string(),
        );
    }
    let (accept_quorum, accept_reason) =
        quorum_status_for_cycle(mem, "accept", accept_finalization.cycle);
    if !accept_quorum {
        return (
            false,
            format!(
                "review blocked: acceptance finalization is not backed by a current quorum: {accept_reason}"
            ),
        );
    }
    if let Some(operation) = mem.front.active_operation.as_deref() {
        return (
            false,
            format!(
                "review blocked: {operation} cycle {} awaits PM finalization",
                mem.front.review_cycle
            ),
        );
    }
    if mem.front.review_cycle > 1 {
        let Some(receipt) = finalization_for(&mem.front, "verify", None, &digest) else {
            return (
                false,
                "review blocked: latest verify finalization is invalid or has a different digest"
                    .to_string(),
            );
        };
        if receipt.cycle != mem.front.review_cycle {
            return (
                false,
                format!(
                    "review blocked: latest verify finalization cycle {} does not match current cycle {}",
                    receipt.cycle, mem.front.review_cycle
                ),
            );
        }
        let (verify_quorum, verify_reason) = quorum_status_for_cycle(mem, "verify", receipt.cycle);
        if !verify_quorum {
            return (
                false,
                format!(
                    "review blocked: verify finalization is not backed by a current quorum: {verify_reason}"
                ),
            );
        }
        return (
            true,
            format!(
                "accepted with PM verify finalization cycle {}",
                receipt.cycle
            ),
        );
    }
    if mem
        .front
        .finalizations
        .iter()
        .any(|receipt| receipt.operation == "verify")
    {
        return (
            false,
            "review blocked: verify finalization exists without a current verify cycle".to_string(),
        );
    }
    (true, "accepted with PM acceptance finalization".to_string())
}
