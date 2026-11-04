//! Canonical v2 transitions. Peer/turn proofs are minted by the daemon;
//! file reads, digest checks and decision writes share the tracker lock.

use super::*;
use crate::issue::PmLock;
use policy::{
    ActorSnapshot, Audience, CuratorReview, FeedbackKind, FeedbackReceipt, NativeActor,
    NativeFeedbackProof, PolicyFinalization, ProposalOptions, Provenance, ProvenanceAttestation,
};

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ScopeInput {
    components: Vec<String>,
    paths: Vec<String>,
    providers: Vec<String>,
    tags: Vec<String>,
    project: bool,
}

impl From<ScopeInput> for Scope {
    fn from(scope: ScopeInput) -> Self {
        Self {
            components: scope.components,
            paths: scope.paths,
            providers: scope.providers,
            tags: scope.tags,
            project: scope.project,
        }
    }
}

pub(crate) fn scope_input(value: &Value) -> Result<Scope> {
    ScopeInput::deserialize(value)
        .map(Into::into)
        .map_err(|error| Error::rejected(format!("invalid proposal scope: {error}")))
}

/// Draft data is not a previously approved record or an adoption operation.
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Draft {
    id: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    status: Option<String>,
    scope: Option<ScopeInput>,
    source: Option<String>,
    confidence: Option<String>,
    #[serde(rename = "created")]
    _created: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn propose(
    pm: &Pm,
    key: &str,
    kind: &str,
    scope: Scope,
    source: Option<&str>,
    confidence: Option<&str>,
    from_text: Option<&str>,
    text: Option<&str>,
    slug: Option<&str>,
    options: ProposalOptions,
    actor: &NativeActor,
) -> Result<Value> {
    let projects = project::list(&pm.dir)?;
    let proj = projects
        .iter()
        .find(|project| project.key == key)
        .ok_or_else(|| project::unknown_project(key, &pm.dir))?;
    let (draft, body) = match (from_text, text) {
        (Some(raw), None) if raw.starts_with("---\n") || raw.starts_with("---\r\n") => {
            let (yaml, body) = parse::split_front(raw)
                .map_err(|error| Error::rejected(format!("memory draft {error}")))?;
            let draft: Draft = serde_yaml::from_str(yaml).map_err(|error| {
                Error::rejected(format!(
                    "proposal accepts draft data, not authority-bearing frontmatter: {error}"
                ))
            })?;
            if draft
                .status
                .as_deref()
                .is_some_and(|status| status != "proposed")
            {
                return Err(Error::rejected(
                    "an imported draft cannot supply a lifecycle decision",
                ));
            }
            (draft, body.to_owned())
        }
        (Some(raw), None) => (Draft::default(), raw.to_owned()),
        (None, Some(body)) => (Draft::default(), body.to_owned()),
        _ => {
            return Err(Error::rejected(
                "propose requires exactly one content source",
            ))
        }
    };
    let metadata = serde_json::to_string(&options)
        .map_err(|error| Error::internal(format!("cannot inspect proposal metadata: {error}")))?;
    let scanned = [
        from_text.or(text).unwrap_or_default(),
        source.unwrap_or_default(),
        draft.source.as_deref().unwrap_or_default(),
        &metadata,
    ]
    .join("\n");
    let warnings = crate::secret::guard("memory propose", &scanned)?;
    let slug = match slug.or(draft.id.as_deref()) {
        Some(slug) => check_slug(slug)?,
        None => {
            let derived = crate::worktree::layout::slugify(&fact_line(&body));
            check_slug(if derived.is_empty() || derived == "work" {
                "lesson"
            } else {
                &derived
            })?
        }
    };
    let source = draft.source.or_else(|| source.map(str::to_owned));
    let policy = policy::new_policy(actor, options, source.as_deref())?;
    let status = if policy.provenance == Provenance::ExternalDerived {
        "quarantined"
    } else {
        "proposed"
    };
    let path = memory_dir(pm, key).join(format!("{slug}.md"));
    let mem = Memory {
        project: key.to_owned(),
        front: Front {
            id: slug.clone(),
            kind: draft.kind.unwrap_or_else(|| kind.to_owned()),
            status: status.to_owned(),
            scope: draft.scope.map(Into::into).unwrap_or(scope),
            source,
            confidence: draft
                .confidence
                .unwrap_or_else(|| confidence.unwrap_or("medium").to_owned()),
            created: time::iso(time::now_epoch()),
            verified_at: None,
            stale: None,
            supersedes: None,
            author: Some(actor.label().to_owned()),
            author_proof: None,
            contributors: Vec::new(),
            review_cycle: 0,
            active_operation: None,
            reviews: Vec::new(),
            finalizations: Vec::new(),
            policy: Some(policy),
        },
        body,
        path,
    };
    check_front(&mem, &proj.components)?;
    let lock = pm.lock_for_paths(std::slice::from_ref(&mem.path))?;
    if mem.path.exists() {
        return Err(Error::rejected(
            "memory already exists; submit a new proposal",
        ));
    }
    std::fs::create_dir_all(memory_dir(pm, key))?;
    let committed = save_and_commit_mem(
        pm,
        &lock,
        &mem,
        &format!("{key}/memory/{slug}: v2 proposed"),
        actor.label(),
    )?;
    Ok(json!({
        "project": key, "slug": slug, "status": mem.front.status,
        "policy_revision": policy::REVISION, "digest": semantic_digest(&mem),
        "quorum": {"eligible": false, "reason": "native policy approval required"},
        "path": mem.path, "committed": committed,
        "secret_warnings": crate::secret::warnings_json(&warnings),
    }))
}

fn digest_matches(mem: &Memory, expected: &str) -> Result<String> {
    let actual = semantic_digest(mem);
    if actual != expected {
        return Err(Error::rejected(
            "memory revision changed; obtain the current candidate digest",
        ));
    }
    Ok(actual)
}

fn bounded_evidence(evidence: &str) -> Result<&str> {
    let evidence = evidence.trim();
    if evidence.is_empty() || evidence.len() > 16_384 {
        return Err(Error::rejected(
            "policy evidence requires nonempty bounded text",
        ));
    }
    crate::secret::guard("memory policy evidence", evidence)?;
    Ok(evidence)
}

fn start_cycle(front: &mut Front, operation: &str) -> Result<u64> {
    match front.active_operation.as_deref() {
        Some(active) if active != operation => {
            return Err(Error::rejected("another memory review operation is active"))
        }
        Some(_) => {}
        None => {
            front.review_cycle = front
                .review_cycle
                .checked_add(1)
                .ok_or_else(|| Error::rejected("memory review cycle exhausted"))?;
            front.active_operation = Some(operation.to_owned());
        }
    }
    Ok(front.review_cycle)
}

pub(super) fn review_locked(
    pm: &Pm,
    lock: &PmLock,
    proj: &project::Project,
    mem: &mut Memory,
    request: &ReviewRequest<'_>,
    actor: &NativeActor,
) -> Result<Value> {
    let digest = digest_matches(mem, request.expected_digest)?;
    let evidence = bounded_evidence(request.evidence)?;
    let reviewer = actor.authorship();
    let policy = mem.front.policy.as_ref().expect("v2 record");
    policy::validate_policy(policy).map_err(Error::rejected)?;
    policy::curator_vendor(policy, &reviewer).map_err(Error::rejected)?;
    match request.operation {
        "accept" if matches!(mem.front.status.as_str(), "proposed" | "quarantined") => {}
        "verify" if mem.front.status == "pending-revalidation" => {}
        "verify" if mem.front.status == "accepted" && policy::requires_curator(policy) => {
            mem.front.status = "pending-revalidation".to_owned();
        }
        _ => {
            return Err(Error::rejected(
                "this policy state does not admit that review operation",
            ))
        }
    }
    let cycle = start_cycle(&mut mem.front, request.operation)?;
    let policy = mem.front.policy.as_mut().expect("v2 record");
    let ActorSnapshot::Agent { identity } = &reviewer.actor else {
        return Err(Error::rejected(
            "operators approve natively, not as model curators",
        ));
    };
    if policy.reviews.iter().any(|review| {
        review.operation == request.operation
            && review.cycle == cycle
            && matches!(&review.reviewer.actor, ActorSnapshot::Agent { identity: prior }
                if policy::identity_conflicts(prior, identity))
    }) {
        return Err(Error::rejected(
            "this accountable curator already reviewed the current cycle",
        ));
    }
    policy.reviews.push(CuratorReview {
        digest: digest.clone(),
        cycle,
        operation: request.operation.to_owned(),
        verdict: request.verdict.to_owned(),
        evidence: evidence.to_owned(),
        reviewer,
        recorded_at: time::iso(time::now_epoch()),
    });
    check_front(mem, &proj.components)?;
    let committed = save_and_commit_mem(
        pm,
        lock,
        mem,
        &format!(
            "{}/memory/{}: v2 {} review",
            proj.key, mem.front.id, request.operation
        ),
        actor.label(),
    )?;
    let (eligible, reason) = quorum_status(mem, request.operation);
    Ok(
        json!({"project": proj.key, "slug": mem.front.id, "status": mem.front.status,
        "operation": request.operation, "cycle": cycle, "digest": digest,
        "quorum": {"eligible": eligible, "reason": reason}, "committed": committed}),
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn finalize_locked(
    pm: &Pm,
    lock: &PmLock,
    proj: &project::Project,
    mem: &mut Memory,
    operation: &str,
    expected: &str,
    evidence: Option<&str>,
    actor: &NativeActor,
) -> Result<Value> {
    let digest = digest_matches(mem, expected)?;
    if let Some(evidence) = evidence {
        bounded_evidence(evidence)?;
    }
    match operation {
        "accept" if matches!(mem.front.status.as_str(), "proposed" | "quarantined") => {}
        "verify"
            if matches!(
                mem.front.status.as_str(),
                "accepted" | "pending-revalidation"
            ) => {}
        _ => {
            return Err(Error::rejected(
                "this memory cannot be finalized in the requested state",
            ))
        }
    }
    let cycle = start_cycle(&mut mem.front, operation)?;
    let policy = mem.front.policy.as_ref().expect("v2 record");
    policy::qualify(policy, operation, cycle, &digest, Some(actor)).map_err(Error::rejected)?;
    if let NativeActor::Agent { identity, .. } = actor {
        let authorship = actor.authorship();
        policy::curator_vendor(policy, &authorship).map_err(Error::rejected)?;
        if !policy.reviews.iter().any(|review| {
            review.digest == digest
                && review.cycle == cycle
                && review.operation == operation
                && review.verdict == "pass"
                && matches!(&review.reviewer.actor, ActorSnapshot::Agent { identity: prior }
                    if prior.uid.is_some() && prior.uid == identity.proof.uid)
        }) {
            return Err(Error::rejected(
                "a native agent finalizer must have its own qualifying curator receipt",
            ));
        }
    }
    let recorded_at = time::iso(time::now_epoch());
    mem.front
        .policy
        .as_mut()
        .expect("v2 record")
        .finalizations
        .push(PolicyFinalization {
            digest: digest.clone(),
            cycle,
            operation: operation.to_owned(),
            status: "accepted".to_owned(),
            finalizer: actor.snapshot(),
            evidence: evidence.map(str::to_owned),
            recorded_at,
        });
    mem.front.status = "accepted".to_owned();
    mem.front.active_operation = None;
    // A policy review is not a qualifying at-SHA mechanical check.
    mem.front.verified_at = None;
    if operation == "verify" {
        mem.front.stale = None;
    }
    check_front(mem, &proj.components)?;
    let committed = save_and_commit_mem(
        pm,
        lock,
        mem,
        &format!(
            "{}/memory/{}: v2 {operation} finalized",
            proj.key, mem.front.id
        ),
        actor.label(),
    )?;
    Ok(
        json!({"project": proj.key, "slug": mem.front.id, "status": mem.front.status,
        "operation": operation, "cycle": cycle, "digest": digest, "finalized": true,
        "quorum": {"eligible": true, "reason": "memory policy v2 requirements satisfied"},
        "committed": committed}),
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn transition_locked(
    pm: &Pm,
    lock: &PmLock,
    proj: &project::Project,
    mem: &mut Memory,
    operation: &str,
    expected: &str,
    evidence: &str,
    actor: &NativeActor,
) -> Result<Value> {
    if !matches!(actor, NativeActor::Operator { .. }) {
        return Err(Error::rejected(
            "this lifecycle decision requires a positively proven operator",
        ));
    }
    let digest = digest_matches(mem, expected)?;
    let evidence = bounded_evidence(evidence)?;
    if matches!(
        mem.front.status.as_str(),
        "superseded" | "rejected" | "invalidated"
    ) {
        return Err(Error::rejected(
            "a terminal memory decision cannot be reopened in place",
        ));
    }
    let status = match operation {
        "revalidate" if mem.front.status == "accepted" => "pending-revalidation",
        "invalidate"
            if matches!(
                mem.front.status.as_str(),
                "accepted" | "pending-revalidation"
            ) =>
        {
            "invalidated"
        }
        "reject" => "rejected",
        _ => return Err(Error::rejected("unsupported memory lifecycle transition")),
    };
    mem.front.active_operation = None;
    let cycle = start_cycle(
        &mut mem.front,
        if operation == "revalidate" {
            "verify"
        } else {
            operation
        },
    )?;
    mem.front
        .policy
        .as_mut()
        .expect("v2 record")
        .finalizations
        .push(PolicyFinalization {
            digest: digest.clone(),
            cycle,
            operation: operation.to_owned(),
            status: status.to_owned(),
            finalizer: actor.snapshot(),
            evidence: Some(evidence.to_owned()),
            recorded_at: time::iso(time::now_epoch()),
        });
    mem.front.status = status.to_owned();
    mem.front.verified_at = None;
    if operation != "revalidate" {
        mem.front.active_operation = None;
    }
    check_front(mem, &proj.components)?;
    let committed = save_and_commit_mem(
        pm,
        lock,
        mem,
        &format!("{}/memory/{}: v2 {operation}", proj.key, mem.front.id),
        actor.label(),
    )?;
    Ok(
        json!({"project": proj.key, "slug": mem.front.id, "status": status,
        "digest": digest, "operation": operation, "committed": committed}),
    )
}

pub(crate) fn classify(
    pm: &Pm,
    project: Option<&str>,
    slug: &str,
    expected: &str,
    evidence: &str,
    refs: Vec<policy::EvidenceRef>,
    actor: &NativeActor,
) -> Result<Value> {
    let NativeActor::Operator { uid } = actor else {
        return Err(Error::rejected(
            "source classification requires a positively proven operator",
        ));
    };
    let evidence = bounded_evidence(evidence)?;
    policy::validate_refs(&refs)?;
    for reference in &refs {
        crate::secret::guard("memory provenance evidence reference", &reference.source)?;
    }
    if refs
        .iter()
        .any(|reference| reference.kind == policy::EvidenceKind::External)
    {
        return Err(Error::rejected(
            "external-derived evidence cannot be reclassified as native agent verification",
        ));
    }
    let (proj, initial) = find(pm, project, slug)?;
    let lock = pm.lock_for_paths(std::slice::from_ref(&initial.path))?;
    let (_, mut mem) = find(pm, Some(&proj.key), slug)?;
    let old_digest = digest_matches(&mem, expected)?;
    let policy = mem
        .front
        .policy
        .as_mut()
        .ok_or_else(|| Error::rejected("legacy adoption is a separate operation (CAD-382)"))?;
    policy::validate_policy(policy).map_err(Error::rejected)?;
    if policy.provenance != Provenance::AgentClaimed
        || !matches!(policy.author.actor, ActorSnapshot::Agent { .. })
        || !matches!(mem.front.status.as_str(), "proposed" | "accepted")
    {
        return Err(Error::rejected(
            "only a current agent-claimed candidate can receive a source attestation",
        ));
    }
    policy.attestations.push(ProvenanceAttestation {
        digest_before: old_digest,
        provenance: Provenance::AgentVerified,
        operator_uid: *uid,
        evidence: evidence.to_owned(),
        evidence_refs: refs,
        recorded_at: time::iso(time::now_epoch()),
    });
    policy.provenance = Provenance::AgentVerified;
    mem.front.review_cycle = mem
        .front
        .review_cycle
        .checked_add(1)
        .ok_or_else(|| Error::rejected("memory review cycle exhausted"))?;
    mem.front.active_operation = None;
    if mem.front.status == "accepted" {
        mem.front.status = "pending-revalidation".to_owned();
    }
    check_front(&mem, &proj.components)?;
    let committed = save_and_commit_mem(
        pm,
        &lock,
        &mem,
        &format!(
            "{}/memory/{slug}: native source provenance attested",
            proj.key
        ),
        actor.label(),
    )?;
    Ok(
        json!({"project": proj.key, "slug": slug, "status": mem.front.status,
        "digest": semantic_digest(&mem), "policy_revision": policy::REVISION, "committed": committed}),
    )
}

#[cfg(all(test, feature = "test-seam"))]
thread_local! {
    static FEEDBACK_AFTER_SAVE_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}
#[cfg(all(test, feature = "test-seam"))]
fn feedback_after_save_for_test() {
    let hook = FEEDBACK_AFTER_SAVE_HOOK.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

/// Holds the tracker lock before native ownership is fenced. Only the atomic
/// file save runs under Store's writer; Git hooks run afterwards, without that
/// mutex. A failed native transaction restores the file before any Git commit.
pub(crate) struct FeedbackWrite<'a> {
    pm: &'a Pm,
    lock: PmLock,
    mem: Memory,
    digest: String,
    previous: Vec<u8>,
    kind: FeedbackKind,
    saved: bool,
}

impl<'a> FeedbackWrite<'a> {
    pub(crate) fn lock(
        pm: &'a Pm,
        project: &str,
        slug: &str,
        expected: &str,
        kind: FeedbackKind,
    ) -> Result<Self> {
        let (proj, initial) = find(pm, Some(project), slug)?;
        let lock = pm.lock_for_paths(std::slice::from_ref(&initial.path))?;
        let (_, mem) = find(pm, Some(&proj.key), slug)?;
        let digest = digest_matches(&mem, expected)?;
        if mem.front.policy.is_none() || !retrieval_status(&mem).0 {
            return Err(Error::rejected(
                "feedback requires a policy-qualified accepted v2 memory",
            ));
        }
        let previous = std::fs::read(&mem.path)?;
        Ok(Self {
            pm,
            lock,
            mem,
            digest,
            previous,
            kind,
            saved: false,
        })
    }

    pub(crate) fn save(
        &mut self,
        evidence: Option<&str>,
        proof: NativeFeedbackProof,
    ) -> Result<Value> {
        if proof.project != self.mem.project || proof.digest != self.digest {
            return Err(Error::rejected(
                "feedback proof does not match the locked memory revision and project",
            ));
        }
        if !valid_identity_proof(&proof.actor)
            || proof.actor.uid.as_deref().is_none_or(str::is_empty)
            || proof.message.is_empty()
            || time::parse_iso(&proof.validated_at).is_none()
        {
            return Err(Error::rejected(
                "authenticated active-turn feedback proof is incomplete",
            ));
        }
        if let Some(evidence) = evidence {
            bounded_evidence(evidence)?;
        }
        let receipt = FeedbackReceipt {
            digest: proof.digest,
            actor: proof.actor,
            message: proof.message,
            task: proof.task,
            recorded_at: proof.validated_at,
            evidence: evidence.map(str::to_owned),
        };
        let feedback = &mut self.mem.front.policy.as_mut().expect("v2 record").feedback;
        let (count, latest) = match self.kind {
            FeedbackKind::Used => (&mut feedback.used_reports, &mut feedback.last_use),
            FeedbackKind::Wrong => (&mut feedback.wrong_reports, &mut feedback.last_wrong),
            FeedbackKind::Reread => (&mut feedback.reread_reports, &mut feedback.last_reread),
        };
        *count = count
            .checked_add(1)
            .ok_or_else(|| Error::rejected("memory feedback counter exhausted"))?;
        *latest = Some(receipt);
        self.saved = true;
        save_mem(&self.mem)?;
        #[cfg(all(test, feature = "test-seam"))]
        feedback_after_save_for_test();
        Ok(
            json!({"project": self.mem.project, "slug": self.mem.front.id, "kind": self.kind,
            "digest": self.digest, "feedback": self.mem.front.policy.as_ref().expect("v2 record").feedback}),
        )
    }

    pub(crate) fn finish(mut self, outcome: Result<Value>) -> Result<Value> {
        let mut response = match outcome {
            Ok(response) => response,
            Err(error) => return Err(self.abort(error)),
        };
        let feedback = &self.mem.front.policy.as_ref().expect("v2 record").feedback;
        let receipt = match self.kind {
            FeedbackKind::Used => feedback.last_use.as_ref(),
            FeedbackKind::Wrong => feedback.last_wrong.as_ref(),
            FeedbackKind::Reread => feedback.last_reread.as_ref(),
        }
        .expect("saved feedback receipt");
        let committed = match commit_mem(
            self.pm,
            &self.lock,
            &self.mem,
            &format!(
                "{}/memory/{}: authenticated feedback",
                self.mem.project, self.mem.front.id
            ),
            &receipt.actor.alias,
        ) {
            Ok(committed) => committed,
            Err(error) => return Err(self.abort(error)),
        };
        self.saved = false;
        response["committed"] = json!(committed);
        Ok(response)
    }

    fn restore(&mut self) -> Result<()> {
        if self.saved {
            let tmp = self.mem.path.with_extension("md.tmp");
            std::fs::write(&tmp, &self.previous)?;
            std::fs::rename(&tmp, &self.mem.path)?;
            self.saved = false;
        }
        Ok(())
    }

    fn abort(&mut self, error: Error) -> Error {
        match self.restore() {
            Ok(()) => error,
            Err(rollback) => Error::internal(format!(
                "feedback rollback failed after {error}: {rollback}"
            )),
        }
    }
}

impl Drop for FeedbackWrite<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            eprintln!("memory feedback rollback failed: {error}");
        }
    }
}

pub(super) fn quorum_status(mem: &Memory, operation: &str) -> (bool, String) {
    let policy = mem.front.policy.as_ref().expect("v2 record");
    let digest = semantic_digest(mem);
    let cycle = mem.front.review_cycle.max(1);
    match policy::qualify(policy, operation, cycle, &digest, None) {
        Ok(()) => (true, "memory policy v2 requirements satisfied".to_owned()),
        Err(reason) => (false, reason.to_owned()),
    }
}

pub(super) fn retrieval_status(mem: &Memory) -> (bool, String) {
    let policy = mem.front.policy.as_ref().expect("v2 record");
    let blocked = |reason: &str| (false, reason.to_owned());
    if mem.front.status != "accepted" || mem.front.active_operation.is_some() {
        return blocked("memory policy v2 is not in a finalized accepted state");
    }
    let Some(receipt) = policy.finalizations.last() else {
        return blocked("accepted status has no native v2 decision receipt");
    };
    let digest = semantic_digest(mem);
    if receipt.status != "accepted"
        || !matches!(receipt.operation.as_str(), "accept" | "verify")
        || receipt.cycle != mem.front.review_cycle
        || receipt.digest != digest
        || time::parse_iso(&receipt.recorded_at).is_none()
    {
        return blocked("accepted state does not match the current native v2 decision");
    }
    if let ActorSnapshot::Agent { identity } = &receipt.finalizer {
        if !valid_identity_proof(identity)
            || identity.uid.as_deref().is_none_or(str::is_empty)
            || policy::is_contributor(policy, identity)
            || !policy.reviews.iter().any(|review| {
                review.digest == digest
                    && review.cycle == receipt.cycle
                    && review.operation == receipt.operation
                    && review.verdict == "pass"
                    && policy::curator_vendor(policy, &review.reviewer).is_ok()
                    && matches!(&review.reviewer.actor, ActorSnapshot::Agent { identity: curator }
                        if curator.uid.is_some() && curator.uid == identity.uid)
            })
        {
            return blocked("native finalizer has no qualifying independent curator receipt");
        }
    }
    match policy::qualify(policy, &receipt.operation, receipt.cycle, &digest, None) {
        Ok(()) => (true, "memory policy v2 decision is qualified".to_owned()),
        Err(reason) => blocked(reason),
    }
}

/// Validity is distinct from review age and never deletes the claim.
pub(super) fn unused_reason(mem: &Memory, now: i64) -> Option<String> {
    let policy = mem.front.policy.as_ref()?;
    if mem.front.status != "accepted" {
        return None;
    }
    let decision = policy.finalizations.last()?;
    // Source classification rotates the digest, not this retained claim's
    // inactivity lifetime. Revalidation is not a new acceptance or reported use.
    let accepted_at = policy
        .finalizations
        .iter()
        .filter(|receipt| receipt.operation == "accept" && receipt.status == "accepted")
        .filter_map(|receipt| time::parse_iso(&receipt.recorded_at))
        .min();
    let Some(accepted_at) = accepted_at else {
        return Some("v2 inactivity anchor is unavailable".to_owned());
    };
    let last_use = policy
        .feedback
        .last_use
        .as_ref()
        .filter(|receipt| {
            receipt.digest == decision.digest
                && valid_identity_proof(&receipt.actor)
                && receipt
                    .actor
                    .uid
                    .as_deref()
                    .is_some_and(|uid| !uid.is_empty())
                && !receipt.message.is_empty()
        })
        .and_then(|receipt| time::parse_iso(&receipt.recorded_at));
    let anchor = last_use.unwrap_or(accepted_at).max(accepted_at);
    if anchor > now {
        return Some("v2 inactivity timestamp is in the future".to_owned());
    }
    (now.saturating_sub(anchor) >= policy::UNUSED_DAYS * 86_400)
        .then(|| "expired-unused: no authenticated reported use for 28 days".to_owned())
}

pub(super) fn role_matches<'a>(mem: &Memory, mut roles: impl Iterator<Item = &'a str>) -> bool {
    mem.front.policy.as_ref().is_none_or(|policy| {
        (policy.audience != Audience::Role || !policy.roles.is_empty())
            && (policy.roles.is_empty()
                || roles.any(|role| policy.roles.iter().any(|selected| selected == role)))
    })
}
