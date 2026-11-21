//! Native project-memory mutation lifecycle: proposal, review, finalization, and rejection.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

#[cfg(test)]
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::time::Duration;

use serde_json::{json, Value};

#[cfg(test)]
use super::memory_dir;
use super::policy;
use super::quorum::{next_verify_cycle, operation_cycle, proposal_identity_matches};
use super::{
    check_front, find, quorum_status, save_and_commit_mem, semantic_digest, v2,
    valid_identity_proof, FinalizationReceipt, ReviewReceipt, Scope,
};
use crate::error::{Error, Result};
use crate::issue::{time, Pm};

/// Build and persist a proposal from content supplied by the authenticated
/// daemon caller. The caller supplies raw file content rather than a path so
/// the daemon, not a client-controlled path, owns the write decision.
#[allow(clippy::too_many_arguments)]
pub fn propose_native(
    pm: &Pm,
    key: &str,
    kind: &str,
    scope: Scope,
    source: Option<&str>,
    confidence: Option<&str>,
    from_text: Option<&str>,
    text: Option<&str>,
    slug: Option<&str>,
    options: policy::ProposalOptions,
    actor: &policy::NativeActor,
) -> Result<Value> {
    v2::propose(
        pm, key, kind, scope, source, confidence, from_text, text, slug, options, actor,
    )
}

/// Submit one native review receipt. All reads that decide the revision are
/// repeated under the existing PM lock, so a concurrent edit cannot leave a
/// receipt attached to a different claim.
pub struct ReviewRequest<'a> {
    pub operation: &'a str,
    pub verdict: &'a str,
    pub evidence: &'a str,
    pub expected_digest: &'a str,
}

#[cfg(test)]
pub(super) struct AfterInitialFindHook {
    pub(super) path: PathBuf,
    pub(super) reached: std::sync::mpsc::Sender<()>,
    pub(super) proceed: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
pub(super) const TEST_COORDINATION_TIMEOUT: Duration = Duration::from_secs(15);

#[cfg(test)]
thread_local! {
    pub(super) static AFTER_INITIAL_FIND_HOOK: std::cell::RefCell<Option<AfterInitialFindHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn after_initial_find_for_test(path: &Path) {
    let hook = AFTER_INITIAL_FIND_HOOK.with(|slot| slot.borrow_mut().take());
    let Some(hook) = hook else {
        return;
    };
    if hook.path == path {
        hook.reached
            .send(())
            .expect("writer did not reach memory lock");
        hook.proceed
            .recv_timeout(TEST_COORDINATION_TIMEOUT)
            .unwrap_or_else(|error| {
                panic!("writer did not release memory lock within 15s: {error}")
            });
    } else {
        AFTER_INITIAL_FIND_HOOK.with(|slot| *slot.borrow_mut() = Some(hook));
    }
}
pub fn submit_review(
    pm: &Pm,
    flag: Option<&str>,
    slug: &str,
    request: &ReviewRequest<'_>,
    actor: &policy::NativeActor,
) -> Result<Value> {
    let operation = request.operation;
    let verdict = request.verdict;
    let evidence = request.evidence;
    let expected_digest = request.expected_digest;
    let native_actor = actor;
    let actor = actor.agent().ok_or_else(|| {
        Error::rejected("native operators approve decisions, not curator reviews")
    })?;
    if !matches!(operation, "accept" | "verify") {
        return Err(Error::rejected(
            "memory review operation must be accept or verify",
        ));
    }
    if !matches!(verdict, "pass" | "revise") {
        return Err(Error::rejected(
            "memory review verdict must be pass or revise",
        ));
    }
    if !valid_identity_proof(&actor.proof) {
        return Err(Error::rejected(
            "authenticated PM or worker identity is incomplete",
        ));
    }
    let evidence = evidence.trim();
    if evidence.is_empty() {
        return Err(Error::rejected("memory review evidence must be nonempty"));
    }
    if evidence.len() > 16_384 {
        return Err(Error::rejected("memory review evidence is too long"));
    }
    if expected_digest.len() != 64 || !expected_digest.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::rejected(
            "memory review requires a SHA-256 content digest",
        ));
    }
    let (proj, initial) = find(pm, flag, slug)?;
    #[cfg(test)]
    after_initial_find_for_test(&memory_dir(pm, &proj.key).join(format!("{slug}.md")));
    let lock = pm.lock_for_paths(std::slice::from_ref(&initial.path))?;
    let (_, mut mem) = find(pm, Some(&proj.key), slug)?;
    if mem.front.policy.is_some() {
        return v2::review_locked(pm, &lock, &proj, &mut mem, request, native_actor);
    }
    let digest = semantic_digest(&mem);
    if digest != expected_digest {
        return Err(Error::rejected(format!(
            "memory revision changed — expected {expected_digest}, current {digest}"
        )));
    }
    let Some(author) = mem.front.author_proof.as_ref() else {
        return Err(Error::rejected(
            "legacy memory has no authenticated proposer; create a corrected native proposal",
        ));
    };
    if actor.stable_id() == author.stable_id() || actor.proof.alias == author.alias {
        return Err(Error::rejected(
            "memory author cannot review its own proposal",
        ));
    }
    let cycle = match operation {
        "accept" => {
            if mem.front.status != "proposed" {
                return Err(Error::rejected("accept reviews require a proposed memory"));
            }
            match mem.front.active_operation.as_deref() {
                None => {
                    mem.front.active_operation = Some("accept".to_string());
                    mem.front.review_cycle = 1;
                }
                Some("accept") => {}
                Some(other) => {
                    return Err(Error::rejected(format!(
                        "memory has an active {other} review cycle"
                    )))
                }
            }
            mem.front.review_cycle
        }
        "verify" => {
            if mem.front.status != "accepted" {
                return Err(Error::rejected("verify reviews require an accepted memory"));
            }
            match mem.front.active_operation.as_deref() {
                None => {
                    mem.front.active_operation = Some("verify".to_string());
                    let next_cycle = next_verify_cycle(&mem.front);
                    mem.front.review_cycle = next_cycle;
                }
                Some("verify") => {}
                Some(other) => {
                    return Err(Error::rejected(format!(
                        "memory has an active {other} review cycle"
                    )))
                }
            }
            mem.front.review_cycle
        }
        _ => unreachable!(),
    };
    if mem.front.reviews.iter().any(|r| {
        r.operation == operation
            && r.cycle == cycle
            && (r.identity == actor.stable_id() || r.reviewer == actor.proof.alias)
    }) {
        return Err(Error::rejected(
            "this native agent already reviewed the current memory cycle",
        ));
    }
    mem.front.reviews.push(ReviewReceipt {
        reviewer: actor.proof.alias.clone(),
        identity: actor.stable_id(),
        generation: actor.proof.generation.clone(),
        process_start: actor.proof.process_start,
        role: actor.proof.role.clone(),
        operation: operation.to_string(),
        cycle,
        digest,
        verdict: verdict.to_string(),
        evidence: evidence.to_string(),
        recorded_at: time::iso(time::now_epoch()),
    });
    check_front(&mem, &proj.components)?;
    let committed = save_and_commit_mem(
        pm,
        &lock,
        &mem,
        &format!("{}/memory/{slug}: review {operation} {verdict}", proj.key),
        &actor.proof.alias,
    )?;
    let (eligible, reason) = quorum_status(&mem, operation);
    Ok(json!({
        "project": proj.key,
        "slug": slug,
        "status": mem.front.status,
        "operation": operation,
        "cycle": cycle,
        "verdict": verdict,
        "digest": semantic_digest(&mem),
        "quorum": {"eligible": eligible, "reason": reason},
        "committed": committed
    }))
}

/// Schema-bound finalization. Existing v1 records retain their PM quorum;
/// v2 decisions consume the approved native authority matrix.
pub fn finalize_native(
    pm: &Pm,
    flag: Option<&str>,
    slug: &str,
    operation: &str,
    expected_digest: &str,
    evidence: Option<&str>,
    actor: &policy::NativeActor,
) -> Result<Value> {
    if !matches!(operation, "accept" | "verify" | "revalidate" | "invalidate") {
        return Err(Error::rejected(
            "memory finalization operation is unsupported",
        ));
    }
    let (proj, initial) = find(pm, flag, slug)?;
    let lock = pm.lock_for_paths(std::slice::from_ref(&initial.path))?;
    let (_, mut mem) = find(pm, Some(&proj.key), slug)?;
    if mem.front.policy.is_some() {
        return match operation {
            "accept" | "verify" => v2::finalize_locked(
                pm,
                &lock,
                &proj,
                &mut mem,
                operation,
                expected_digest,
                evidence,
                actor,
            ),
            _ => v2::transition_locked(
                pm,
                &lock,
                &proj,
                &mut mem,
                operation,
                expected_digest,
                evidence.ok_or_else(|| Error::rejected("lifecycle evidence is required"))?,
                actor,
            ),
        };
    }
    let actor = actor.agent().ok_or_else(|| {
        Error::rejected("legacy finalization requires its existing native PM role")
    })?;
    if !valid_identity_proof(&actor.proof) || actor.proof.role != "pm" {
        return Err(Error::rejected(
            "memory finalization is restricted to an authenticated PM endpoint",
        ));
    }
    if !matches!(operation, "accept" | "verify") {
        return Err(Error::rejected(
            "legacy lifecycle adoption is a separate operation (CAD-382)",
        ));
    }
    let digest = semantic_digest(&mem);
    if digest != expected_digest {
        return Err(Error::rejected(format!(
            "memory revision changed — expected {expected_digest}, current {digest}"
        )));
    }
    if proposal_identity_matches(&mem.front, &actor.proof) {
        return Err(Error::rejected(
            "memory proposer or contributor cannot finalize its own acceptance or verification",
        ));
    }
    let Some(cycle) = operation_cycle(&mem.front, operation) else {
        if mem
            .front
            .finalizations
            .iter()
            .any(|receipt| receipt.operation == operation)
        {
            return Err(Error::rejected(format!(
                "memory {operation} review cycle was already finalized; submit a fresh review cycle"
            )));
        }
        return Err(Error::rejected(format!(
            "memory has no active {operation} review cycle"
        )));
    };
    if mem
        .front
        .finalizations
        .iter()
        .any(|receipt| receipt.operation == operation && receipt.cycle == cycle)
    {
        return Err(Error::rejected(format!(
            "memory {operation} review cycle {cycle} was already finalized"
        )));
    }
    let (eligible, reason) = quorum_status(&mem, operation);
    if !eligible {
        return Err(Error::rejected(reason));
    }
    match operation {
        "accept" => {
            if mem.front.status != "proposed" {
                return Err(Error::rejected("only proposed memories can be accepted"));
            }
            mem.front.status = "accepted".to_string();
        }
        "verify" => {
            if mem.front.status != "accepted" {
                return Err(Error::rejected("only accepted memories can be verified"));
            }
        }
        _ => unreachable!(),
    }
    let finalized_at = time::iso(time::now_epoch());
    // Acceptance records the review; only a verify re-checks the evidence,
    // so only a verify writes the evidence fields (CAD-203).
    if operation == "verify" {
        mem.front.verified_at = Some(finalized_at.clone());
        mem.front.stale = None;
    }
    mem.front.finalizations.push(FinalizationReceipt {
        operation: operation.to_string(),
        cycle,
        digest: digest.clone(),
        finalizer: actor.proof.clone(),
        finalized_at,
    });
    mem.front.active_operation = None;
    check_front(&mem, &proj.components)?;
    let committed = save_and_commit_mem(
        pm,
        &lock,
        &mem,
        &format!("{}/memory/{slug}: {operation} finalized", proj.key),
        &actor.proof.alias,
    )?;
    Ok(json!({
        "project": proj.key,
        "slug": slug,
        "status": mem.front.status,
        "operation": operation,
        "cycle": cycle,
        "digest": semantic_digest(&mem),
        "quorum": {"eligible": true, "reason": reason},
        "finalized": true,
        "committed": committed
    }))
}

/// PM-only rejection. Rejection is a lifecycle decision, not a positive
/// review, and never upgrades legacy records or clears their history.
pub fn reject_native(
    pm: &Pm,
    flag: Option<&str>,
    slug: &str,
    expected_digest: &str,
    evidence: Option<&str>,
    actor: &policy::NativeActor,
) -> Result<Value> {
    let (proj, initial) = find(pm, flag, slug)?;
    let lock = pm.lock_for_paths(std::slice::from_ref(&initial.path))?;
    let (_, mut mem) = find(pm, Some(&proj.key), slug)?;
    if mem.front.policy.is_some() {
        return v2::transition_locked(
            pm,
            &lock,
            &proj,
            &mut mem,
            "reject",
            expected_digest,
            evidence.ok_or_else(|| Error::rejected("rejection evidence is required"))?,
            actor,
        );
    }
    let actor = actor
        .agent()
        .ok_or_else(|| Error::rejected("legacy rejection requires its existing native PM role"))?;
    if !valid_identity_proof(&actor.proof) || actor.proof.role != "pm" {
        return Err(Error::rejected(
            "memory rejection is restricted to an authenticated PM endpoint",
        ));
    }
    if semantic_digest(&mem) != expected_digest {
        return Err(Error::rejected("memory revision changed before rejection"));
    }
    if mem.front.status == "superseded" {
        return Err(Error::rejected("a superseded memory cannot be rejected"));
    }
    mem.front.status = "rejected".to_string();
    mem.front.verified_at = None;
    mem.front.active_operation = None;
    check_front(&mem, &proj.components)?;
    let committed = save_and_commit_mem(
        pm,
        &lock,
        &mem,
        &format!("{}/memory/{slug}: rejected", proj.key),
        &actor.proof.alias,
    )?;
    Ok(json!({
        "project": proj.key,
        "slug": slug,
        "status": "rejected",
        "digest": semantic_digest(&mem),
        "committed": committed
    }))
}

/// Pairwise supersede needs a crash-atomic transaction/recovery primitive
/// that this file writer does not have. Refuse it until CAD-464 supplies
/// that primitive rather than performing two sequential writes.
pub fn supersede_native(
    _pm: &Pm,
    _flag: Option<&str>,
    _old: &str,
    _new: &str,
    _actor: &policy::NativeActor,
) -> Result<Value> {
    Err(Error::rejected(
        "memory supersede is unsupported until crash-atomic pair recovery is available",
    ))
}
