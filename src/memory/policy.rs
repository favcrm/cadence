//! Revisioned memory policy. Native actors are not deserializable requests.

use crate::error::{Error, Result};

use super::{IdentityProof, NativeIdentity};
use serde::{Deserialize, Serialize};

pub const REVISION: u16 = 2;
pub const UNUSED_DAYS: i64 = 28;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Nature {
    Mechanical,
    #[default]
    Judgement,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Provenance {
    Operator,
    AgentClaimed,
    AgentVerified,
    ExternalDerived,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Audience {
    #[default]
    Project,
    Role,
    Company,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceKind {
    Git,
    Wiki,
    NativeRecord,
    External,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRef {
    pub kind: EvidenceKind,
    pub source: String,
    pub revision: String,
}

/// A condition names data, never an executable command or a claimed result.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerificationCondition {
    pub check_id: String,
    pub source_revision: Option<String>,
    pub target_revision: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ProposalOptions {
    pub nature: Nature,
    pub audience: Audience,
    pub roles: Vec<String>,
    pub evidence_refs: Vec<EvidenceRef>,
    pub verification_condition: Option<VerificationCondition>,
}

/// Matching native ready-event snapshot; owner generation lives in the actor proof.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModelEvidence {
    pub provider: String,
    pub model: String,
    pub vendor: Option<String>,
    pub ready_seq: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ActorSnapshot {
    Agent { identity: IdentityProof },
    Operator { uid: u32 },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Authorship {
    pub actor: ActorSnapshot,
    pub model: Option<ModelEvidence>,
}

/// Constructed from connection evidence; deliberately not Deserialize.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeActor {
    Agent {
        identity: NativeIdentity,
        model: Option<ModelEvidence>,
    },
    Operator {
        uid: u32,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProvenanceAttestation {
    pub digest_before: String,
    pub provenance: Provenance,
    pub operator_uid: u32,
    pub evidence: String,
    pub evidence_refs: Vec<EvidenceRef>,
    pub recorded_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CuratorReview {
    pub digest: String,
    pub cycle: u64,
    pub operation: String,
    pub verdict: String,
    pub evidence: String,
    pub reviewer: Authorship,
    pub recorded_at: String,
}

/// A single decision log also retains revalidation/invalidation history.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PolicyFinalization {
    pub digest: String,
    pub cycle: u64,
    pub operation: String,
    pub status: String,
    pub finalizer: ActorSnapshot,
    pub evidence: Option<String>,
    pub recorded_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FeedbackKind {
    Used,
    Wrong,
    Reread,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackReceipt {
    pub digest: String,
    pub actor: IdentityProof,
    pub message: String,
    pub task: Option<String>,
    pub recorded_at: String,
    pub evidence: Option<String>,
}

/// Bounded latest receipts plus report counts, not proven benefit metrics.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Feedback {
    pub used_reports: u64,
    pub wrong_reports: u64,
    pub reread_reports: u64,
    pub last_use: Option<FeedbackReceipt>,
    pub last_wrong: Option<FeedbackReceipt>,
    pub last_reread: Option<FeedbackReceipt>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PolicyV2 {
    pub revision: u16,
    pub nature: Nature,
    pub provenance: Provenance,
    pub audience: Audience,
    pub roles: Vec<String>,
    pub evidence_refs: Vec<EvidenceRef>,
    pub verification_condition: Option<VerificationCondition>,
    pub author: Authorship,
    pub contributors: Vec<Authorship>,
    pub attestations: Vec<ProvenanceAttestation>,
    pub reviews: Vec<CuratorReview>,
    pub finalizations: Vec<PolicyFinalization>,
    pub feedback: Feedback,
}

/// Minted after connection, active turn and trusted issue/task scope checks.
/// No raw turn token is retained in the policy or its feedback receipt.
#[derive(Clone, Debug)]
pub struct NativeFeedbackProof {
    pub project: String,
    pub digest: String,
    pub actor: IdentityProof,
    pub message: String,
    pub task: Option<String>,
    pub validated_at: String,
}

impl NativeActor {
    pub fn agent(&self) -> Option<&NativeIdentity> {
        match self {
            Self::Agent { identity, .. } => Some(identity),
            Self::Operator { .. } => None,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Agent { identity, .. } => &identity.proof.alias,
            Self::Operator { .. } => "operator",
        }
    }

    pub fn snapshot(&self) -> ActorSnapshot {
        match self {
            Self::Agent { identity, .. } => ActorSnapshot::Agent {
                identity: identity.proof.clone(),
            },
            Self::Operator { uid } => ActorSnapshot::Operator { uid: *uid },
        }
    }

    pub fn authorship(&self) -> Authorship {
        Authorship {
            actor: self.snapshot(),
            model: match self {
                Self::Agent { model, .. } => model.clone(),
                Self::Operator { .. } => None,
            },
        }
    }
}

/// Native confirmed model identifiers, not adapter/transport names.
pub fn model_vendor(model: &str) -> Option<&'static str> {
    fn namespace(value: &str) -> Option<&'static str> {
        [
            ("openai", "openai"),
            ("openai-codex", "openai"),
            ("azure-openai", "openai"),
            ("anthropic", "anthropic"),
            ("google", "google"),
            ("google-gemini-cli", "google"),
            ("google-vertex", "google"),
            ("deepseek", "deepseek"),
            ("xai", "xai"),
            ("x-ai", "xai"),
            ("mistral", "mistral"),
            ("meta", "meta"),
            ("cohere", "cohere"),
            ("amazon", "amazon"),
            ("alibaba", "alibaba"),
            ("qwen", "alibaba"),
            ("zai", "zhipu"),
            ("z-ai", "zhipu"),
            ("zhipu", "zhipu"),
            ("moonshot", "moonshot"),
            ("minimax", "minimax"),
        ]
        .into_iter()
        .find_map(|(name, vendor)| value.eq_ignore_ascii_case(name).then_some(vendor))
    }

    let model = model.trim();
    if let Some((prefix, rest)) = model.split_once('/') {
        if matches!(prefix, "openrouter" | "devin" | "pi") {
            return rest
                .split_once('/')
                .and_then(|(provider, _)| namespace(provider));
        }
        return namespace(prefix);
    }
    [
        ("gpt-", "openai"),
        ("chatgpt-", "openai"),
        ("o1", "openai"),
        ("o3", "openai"),
        ("o4", "openai"),
        ("claude-", "anthropic"),
        ("gemini-", "google"),
        ("deepseek-", "deepseek"),
        ("grok-", "xai"),
        ("mistral-", "mistral"),
        ("qwen-", "alibaba"),
        ("glm-", "zhipu"),
    ]
    .into_iter()
    .find_map(|(prefix, vendor)| model.starts_with(prefix).then_some(vendor))
}

fn bounded_text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn immutable_revision(value: &str, git: bool) -> bool {
    let value = value.strip_prefix("sha256:").unwrap_or(value);
    (value.len() == 64 || (git && value.len() == 40))
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn validate_options(options: &ProposalOptions) -> Result<()> {
    if options.roles.len() > 16
        || options.roles.iter().any(|role| !bounded_text(role, 64))
        || (options.audience == Audience::Role && options.roles.is_empty())
    {
        return Err(Error::rejected(
            "role scope requires bounded, nonempty role identifiers",
        ));
    }
    validate_refs(&options.evidence_refs)?;
    if let Some(condition) = &options.verification_condition {
        if condition.check_id.is_empty()
            || condition.check_id.len() > 64
            || !condition
                .check_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            || condition
                .source_revision
                .as_deref()
                .is_some_and(|revision| !immutable_revision(revision, true))
            || condition
                .target_revision
                .as_deref()
                .is_some_and(|revision| !immutable_revision(revision, true))
        {
            return Err(Error::rejected(
                "verification conditions name a check identifier and immutable revisions, never commands",
            ));
        }
    }
    Ok(())
}

pub fn validate_refs(refs: &[EvidenceRef]) -> Result<()> {
    if refs.len() > 16
        || refs.iter().any(|reference| {
            !bounded_text(&reference.source, 2_048)
                || !immutable_revision(&reference.revision, reference.kind == EvidenceKind::Git)
        })
    {
        return Err(Error::rejected(
            "evidence references require bounded sources and immutable revision digests",
        ));
    }
    Ok(())
}

fn valid_authorship(author: &Authorship) -> bool {
    match &author.actor {
        ActorSnapshot::Agent { identity } => {
            super::valid_identity_proof(identity)
                && identity
                    .uid
                    .as_deref()
                    .is_some_and(|uid| bounded_text(uid, 128))
                && author.model.as_ref().is_none_or(|model| {
                    bounded_text(&model.model, 256)
                        && bounded_text(&model.provider, 64)
                        && model.ready_seq > 0
                        && model.vendor.as_deref() == model_vendor(&model.model)
                })
        }
        ActorSnapshot::Operator { .. } => author.model.is_none(),
    }
}

pub fn new_policy(
    actor: &NativeActor,
    options: ProposalOptions,
    source: Option<&str>,
) -> Result<PolicyV2> {
    validate_options(&options)?;
    let author = actor.authorship();
    if !valid_authorship(&author) {
        return Err(Error::rejected(
            "native memory author evidence is incomplete",
        ));
    }
    let external = options
        .evidence_refs
        .iter()
        .any(|reference| reference.kind == EvidenceKind::External)
        || source.is_some_and(|source| {
            let source = source.trim_start();
            source.starts_with("https://") || source.starts_with("http://")
        });
    let provenance = if external {
        Provenance::ExternalDerived
    } else {
        match actor {
            NativeActor::Agent { .. } => Provenance::AgentClaimed,
            NativeActor::Operator { .. } => Provenance::Operator,
        }
    };
    Ok(PolicyV2 {
        revision: REVISION,
        nature: options.nature,
        provenance,
        audience: options.audience,
        roles: options.roles,
        evidence_refs: options.evidence_refs,
        verification_condition: options.verification_condition,
        author,
        contributors: Vec::new(),
        attestations: Vec::new(),
        reviews: Vec::new(),
        finalizations: Vec::new(),
        feedback: Feedback::default(),
    })
}

/// Only semantic data participates; reviews and usage cannot rotate a claim.
pub fn semantic_fields(policy: &PolicyV2) -> impl Serialize + '_ {
    #[derive(Serialize)]
    struct Semantic<'a> {
        revision: u16,
        nature: &'a Nature,
        provenance: &'a Provenance,
        audience: &'a Audience,
        roles: &'a [String],
        evidence_refs: &'a [EvidenceRef],
        verification_condition: &'a Option<VerificationCondition>,
        author: &'a Authorship,
        contributors: &'a [Authorship],
        attestations: &'a [ProvenanceAttestation],
    }
    Semantic {
        revision: policy.revision,
        nature: &policy.nature,
        provenance: &policy.provenance,
        audience: &policy.audience,
        roles: &policy.roles,
        evidence_refs: &policy.evidence_refs,
        verification_condition: &policy.verification_condition,
        author: &policy.author,
        contributors: &policy.contributors,
        attestations: &policy.attestations,
    }
}

/// A renamed registration is still the same accountable agent; recycling
/// an old alias cannot make its new registration independent of that author.
pub fn identity_conflicts(left: &IdentityProof, right: &IdentityProof) -> bool {
    (left.uid.is_some() && left.uid == right.uid) || left.alias == right.alias
}

pub fn is_contributor(policy: &PolicyV2, identity: &IdentityProof) -> bool {
    std::iter::once(&policy.author)
        .chain(&policy.contributors)
        .any(|author| {
            matches!(&author.actor, ActorSnapshot::Agent { identity: other }
                if identity_conflicts(identity, other))
        })
}

pub fn curator_vendor<'a>(
    policy: &PolicyV2,
    curator: &'a Authorship,
) -> std::result::Result<&'a str, &'static str> {
    let ActorSnapshot::Agent { identity } = &curator.actor else {
        return Err("a curator must have an independently accountable native agent identity");
    };
    if !valid_authorship(curator) || is_contributor(policy, identity) {
        return Err("a memory author or contributor cannot supply its curator approval");
    }
    let author_vendor = policy
        .author
        .model
        .as_ref()
        .and_then(|model| model.vendor.as_deref())
        .ok_or("the author's confirmed underlying model vendor is unavailable")?;
    let curator_vendor = curator
        .model
        .as_ref()
        .and_then(|model| model.vendor.as_deref())
        .ok_or("the curator's confirmed underlying model vendor is unavailable")?;
    if curator_vendor == author_vendor {
        return Err("curator and author require distinct underlying model vendors");
    }
    Ok(curator_vendor)
}

pub fn validate_policy(policy: &PolicyV2) -> std::result::Result<(), &'static str> {
    if policy.revision != REVISION {
        return Err("unsupported memory policy revision");
    }
    if !valid_authorship(&policy.author)
        || policy
            .contributors
            .iter()
            .any(|author| !valid_authorship(author))
    {
        return Err("native author or contributor evidence is malformed");
    }
    match (&policy.provenance, &policy.author.actor) {
        (Provenance::Operator, ActorSnapshot::Operator { .. })
        | (Provenance::AgentClaimed, ActorSnapshot::Agent { .. })
        | (Provenance::ExternalDerived, _) => {}
        (Provenance::AgentVerified, ActorSnapshot::Agent { .. })
            if policy.attestations.iter().any(|attestation| {
                attestation.provenance == Provenance::AgentVerified
                    && immutable_revision(&attestation.digest_before, false)
                    && !attestation.evidence.trim().is_empty()
                    && crate::issue::time::parse_iso(&attestation.recorded_at).is_some()
            }) => {}
        _ => return Err("source provenance has no matching native authority"),
    }
    Ok(())
}

fn has_operator(
    policy: &PolicyV2,
    operation: &str,
    cycle: u64,
    digest: &str,
    prospective: Option<&NativeActor>,
) -> bool {
    matches!(prospective, Some(NativeActor::Operator { .. }))
        || policy.finalizations.iter().any(|receipt| {
            receipt.operation == operation
                && receipt.cycle == cycle
                && receipt.digest == digest
                && receipt.status == "accepted"
                && matches!(receipt.finalizer, ActorSnapshot::Operator { .. })
                && crate::issue::time::parse_iso(&receipt.recorded_at).is_some()
        })
}

pub fn requires_curator(policy: &PolicyV2) -> bool {
    matches!(policy.provenance, Provenance::AgentVerified)
        || (policy.provenance == Provenance::AgentClaimed && policy.nature == Nature::Mechanical)
}

/// Evaluates the actual approved matrix, never a caller's asserted result.
/// CAD-348 owns the missing qualified check producer. A mechanical claim
/// therefore cannot be promoted by a vote, timestamp, hash or operator here.
pub fn qualify(
    policy: &PolicyV2,
    operation: &str,
    cycle: u64,
    digest: &str,
    prospective: Option<&NativeActor>,
) -> std::result::Result<(), &'static str> {
    validate_policy(policy)?;
    if cycle == 0 {
        return Err("a native policy decision requires a current review cycle");
    }
    if policy.nature == Nature::Mechanical {
        return Err("qualifying native logic-check evidence is unavailable (CAD-348)");
    }
    let operator = has_operator(policy, operation, cycle, digest, prospective);
    if matches!(
        policy.provenance,
        Provenance::Operator | Provenance::AgentClaimed | Provenance::ExternalDerived
    ) {
        return operator
            .then_some(())
            .ok_or("this judgement requires approval from a positively proven operator");
    }
    let mut first: Option<(&IdentityProof, &str)> = None;
    let mut second = false;
    for review in &policy.reviews {
        if review.digest != digest
            || review.cycle != cycle
            || review.operation != operation
            || review.evidence.trim().is_empty()
            || crate::issue::time::parse_iso(&review.recorded_at).is_none()
        {
            continue;
        }
        let Ok(vendor) = curator_vendor(policy, &review.reviewer) else {
            continue;
        };
        if review.verdict == "revise" {
            return Err("an independent curator requested revision in this policy cycle");
        }
        if review.verdict != "pass" {
            continue;
        }
        let ActorSnapshot::Agent { identity } = &review.reviewer.actor else {
            continue;
        };
        match first {
            None => first = Some((identity, vendor)),
            Some((prior, prior_vendor))
                if !identity_conflicts(prior, identity) && vendor != prior_vendor =>
            {
                second = true;
            }
            _ => {}
        }
    }
    if first.is_none() {
        return Err("an independent cross-vendor curator approval is required");
    }
    if policy.audience == Audience::Company && !second && !operator {
        return Err("company guidance needs a second distinct-vendor curator or operator approval");
    }
    Ok(())
}

/// Human-readable requirements; decisions still go through `qualify`.
pub(super) fn requirements(policy: &PolicyV2) -> impl Iterator<Item = &'static str> {
    let curator = requires_curator(policy);
    [
        (policy.nature == Nature::Mechanical).then_some("qualifying native logic-check evidence"),
        curator.then_some("independent cross-vendor curator"),
        if curator && policy.audience == Audience::Company && policy.nature == Nature::Judgement {
            Some("second distinct-vendor curator or operator")
        } else if !curator
            && (policy.nature == Nature::Judgement
                || policy.provenance == Provenance::ExternalDerived)
        {
            Some("positively proven operator approval")
        } else {
            None
        },
    ]
    .into_iter()
    .flatten()
}
