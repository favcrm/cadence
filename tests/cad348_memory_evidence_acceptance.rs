//! CAD-348 independent acceptance check (written by the reviewer, not the
//! implementer; AGENTS.md "Gates and security work").
//!
//! The memory evidence receipt is the record's only claim about its own
//! citations. This check drives the public analysis surface directly, with
//! isolated tracker/repository/wiki fixtures, and pins what a receipt must
//! never get wrong:
//!
//! 1. a citation that is missing, unreadable or of an unsupported kind is
//!    never reported as a pass, even when the record names a verification
//!    condition;
//! 2. a credential in the citation reference is refused and never echoed;
//! 3. cited bytes that are not UTF-8 are never treated as scanned: a
//!    credential-free blob is unknown, and a blob carrying a token is
//!    refused or reported incomplete — never a pass, never echoed;
//! 4. a valid Wiki revision both passes proposal validation and resolves to
//!    the immutable page it names;
//! 5. attestation evidence references are analysed exactly like the record's
//!    own citations, including the reference secret scan;
//! 6. Wiki historical text and blob originals are read only inside the cited
//!    byte bound: an over-bound original is unknown, never checked;
//! 7. a condition's separate Wiki target revision is a retained tracker
//!    commit, checked against the cited page's own bytes with the same caller
//!    ACL: identical bytes pass and name the checked target, changed or absent
//!    page bytes revise, and a target this reader cannot resolve is reported
//!    incomplete rather than passed;
//! 8. an absent Wiki target is byte-for-byte the previously recorded
//!    condition — the same three keys, so old claim digests do not move — and
//!    a selector that is not a full lowercase commit is refused;
//! 9. an existing target commit that no longer carries the cited path is a
//!    revision, while a target commit that does not exist is an unanswered
//!    check — unknown and skipped, never reported as an absent path;
//! 10. an unreadable sibling record makes the project pool unknown, with a
//!    sanitized scope in `skipped` and no name or content echoed;
//! 11. reordered or repeated scope selectors still describe one scope, while
//!    different selector sets stay distinct;
//! 12. a Git replacement ref cannot swap the bytes or the size bound of a
//!    cited commit: the analysis reads the object it names, not the
//!    replacement;
//! 13. a valid Git citation whose project is not registered is an unavailable
//!    repository — unsupported and unknown, never a missing citation.
//!
//! Every fixture is a tempdir; no production tracker, vault or state is read
//! or written.

use cadence_agent::issue::write::project_add;
use cadence_agent::issue::Pm;
use cadence_agent::memory::evidence::{
    analyse, analyse_as, resolve, scan_content, Outcome, Receipt, Resolution,
};
use cadence_agent::memory::policy::{
    self, ActorSnapshot, Audience, Authorship, EvidenceKind, EvidenceRef, Nature, PolicyV2,
    ProposalOptions, Provenance, VerificationCondition,
};
use cadence_agent::memory::{Front, Memory, Scope};
use cadence_agent::wiki::Caller;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One isolated tracker, one Git repository registered as `demo`, and the
/// legacy `<tracker>/wiki` vault the tracker resolves its citations against.
struct Rig {
    /// Kept alive for the duration of the check; nothing reads it directly.
    _root: tempfile::TempDir,
    pm: Pm,
    repo: PathBuf,
    /// The first repository commit, which contains `evidence.txt`.
    commit: String,
}

impl Rig {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("cad348evidence-")
            .tempdir()
            .unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("evidence.txt"), "original\n").unwrap();
        git(&repo, &["add", "--", "evidence.txt"]);
        let commit = commit_tree(&repo, "evidence");
        project_add(
            &pm,
            "demo",
            "DEMO",
            &[repo.display().to_string()],
            &[],
            &[],
            None,
        )
        .expect("isolated project with one registered repository");
        Self {
            _root: root,
            pm,
            repo,
            commit,
        }
    }

    /// Write and commit one repository file; returns its commit.
    fn commit_bytes(&self, name: &str, bytes: &[u8]) -> String {
        std::fs::write(self.repo.join(name), bytes).unwrap();
        git(&self.repo, &["add", "--", name]);
        commit_tree(&self.repo, name)
    }

    /// Write and commit one wiki page; returns its commit. The vault is the
    /// legacy `<tracker>/wiki` root, so the tracker's own git sees the page.
    fn commit_wiki(&self, rel: &str, text: &str) -> String {
        let path = self.pm.dir.join("wiki").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        self.pm
            .commit(std::slice::from_ref(&path), "CAD-348 evidence fixture\n")
            .unwrap();
        git(&self.pm.dir, &["rev-parse", "HEAD"])
    }

    /// Commit a content-addressed Wiki blob page the way `put_blob` lands it:
    /// the bytes live in the vault's gitignored `.blobs/<sha256>` store and the
    /// committed `<path>.blob` pointer records the digest and declared size.
    fn commit_wiki_blob(&self, rel: &str, bytes: &[u8], declared_size: Option<u64>) -> String {
        let vault = self.pm.dir.join("wiki");
        let sha = sha256_hex(bytes);
        let blobs = vault.join(".blobs");
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::write(blobs.join(&sha), bytes).unwrap();
        let pointer = serde_json::json!({
            "sha256": sha,
            "size": declared_size.unwrap_or(bytes.len() as u64),
            "mime": "application/octet-stream",
            "name": rel.rsplit('/').next().unwrap_or(rel),
        });
        let pointer_path = vault.join(format!("{rel}.blob"));
        std::fs::create_dir_all(pointer_path.parent().unwrap()).unwrap();
        std::fs::write(&pointer_path, pointer.to_string()).unwrap();
        self.pm
            .commit(
                std::slice::from_ref(&pointer_path),
                "CAD-348 blob fixture\n",
            )
            .unwrap();
        git(&self.pm.dir, &["rev-parse", "HEAD"])
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("`git` is required by this fixture");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit_tree(dir: &Path, message: &str) -> String {
    git(
        dir,
        &[
            "-c",
            "user.name=CAD-348 fixture",
            "-c",
            "user.email=cad348@fixture.invalid",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
    git(dir, &["rev-parse", "HEAD"])
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A credential shape assembled at runtime: a literal one in this file would
/// trip the repository's own secret scan, which must stay clean.
fn fixture_token() -> String {
    format!("figd_{}", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dE3fG5h")
}

fn condition(source: Option<&str>, target: Option<&str>) -> VerificationCondition {
    condition_with_wiki_target(source, target, None)
}

/// A condition that also names the separate Wiki target revision: the retained
/// tracker commit a Wiki citation must still agree with. A project target and a
/// Wiki target are different repositories, so neither stands in for the other.
fn condition_with_wiki_target(
    source: Option<&str>,
    target: Option<&str>,
    wiki_target: Option<&str>,
) -> VerificationCondition {
    VerificationCondition {
        check_id: "evidence-analysis".into(),
        source_revision: source.map(str::to_owned),
        target_revision: target.map(str::to_owned),
        wiki_target_revision: wiki_target.map(str::to_owned),
    }
}

fn policy_with(
    refs: Vec<EvidenceRef>,
    verification_condition: Option<VerificationCondition>,
) -> PolicyV2 {
    PolicyV2 {
        revision: policy::REVISION,
        nature: Nature::Judgement,
        provenance: Provenance::Operator,
        audience: Audience::Project,
        roles: Vec::new(),
        evidence_refs: refs,
        verification_condition,
        author: Authorship {
            actor: ActorSnapshot::Operator { uid: 1000 },
            model: None,
        },
        contributors: Vec::new(),
        attestations: Vec::new(),
        reviews: Vec::new(),
        finalizations: Vec::new(),
        feedback: Default::default(),
    }
}

fn memory(
    id: &str,
    body: &str,
    refs: Vec<EvidenceRef>,
    verification_condition: Option<VerificationCondition>,
) -> Memory {
    Memory {
        project: "demo".into(),
        front: Front {
            id: id.into(),
            kind: "rule".into(),
            status: "proposed".into(),
            scope: Scope::default(),
            source: None,
            confidence: "medium".into(),
            created: "2026-10-28T00:00:00Z".into(),
            verified_at: None,
            stale: None,
            supersedes: None,
            author: Some("fixture".into()),
            author_proof: None,
            contributors: Vec::new(),
            review_cycle: 0,
            active_operation: None,
            reviews: Vec::new(),
            finalizations: Vec::new(),
            policy: Some(policy_with(refs, verification_condition)),
        },
        body: body.into(),
        path: PathBuf::from("/nonexistent"),
    }
}

fn git_ref(source: &str, revision: &str) -> EvidenceRef {
    EvidenceRef {
        kind: EvidenceKind::Git,
        source: source.into(),
        revision: revision.into(),
    }
}

fn wiki_ref(source: &str, revision: &str) -> EvidenceRef {
    EvidenceRef {
        kind: EvidenceKind::Wiki,
        source: source.into(),
        revision: revision.into(),
    }
}

/// A receipt that reports a pass must have resolved something and skipped
/// nothing; this is the inverse every negative check asserts.
fn assert_not_pass(label: &str, receipt: &Receipt) {
    assert_ne!(
        receipt.outcome,
        Outcome::Pass,
        "{label}: an unresolved citation was reported as a pass \
         (checked {:?}, skipped {:?}, findings {:?})",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_ne!(
        receipt.to_json()["result"],
        "pass",
        "{label}: the machine-readable receipt claimed a pass"
    );
}

/// Attach one operator source attestation carrying the given citations, so a
/// record's attestation refs are analysed alongside its own.
fn with_attestation(mut mem: Memory, refs: Vec<EvidenceRef>) -> Memory {
    let front_policy = mem.front.policy.as_mut().expect("fixture policy");
    front_policy
        .attestations
        .push(policy::ProvenanceAttestation {
            digest_before: format!("sha256:{}", "1".repeat(64)),
            provenance: Provenance::Operator,
            operator_uid: 1000,
            evidence: "operator source attestation".into(),
            evidence_refs: refs,
            recorded_at: "2026-10-28T00:00:00Z".into(),
        });
    mem
}

/// Give a fixture record an explicit component and path selector set.
fn with_scope(mut mem: Memory, components: &[&str], paths: &[&str]) -> Memory {
    mem.front.scope.components = components.iter().map(|c| c.to_string()).collect();
    mem.front.scope.paths = paths.iter().map(|p| p.to_string()).collect();
    mem
}

/// Store one fixture record exactly as the tracker keeps it, so the project
/// pool the analysis reads contains it.
fn persist(rig: &Rig, mem: &Memory) {
    let dir = rig.pm.dir.join(&mem.project).join("memory");
    std::fs::create_dir_all(&dir).unwrap();
    let yaml = serde_yaml::to_string(&mem.front).unwrap();
    std::fs::write(
        dir.join(format!("{}.md", mem.front.id)),
        format!("---\n{yaml}---\n\n{}", mem.body),
    )
    .unwrap();
}

#[test]
fn a_missing_cited_source_with_a_condition_is_blocked() {
    let rig = Rig::new();
    let mem = memory(
        "missing",
        "A claim whose source is not in the repository",
        vec![git_ref("not-there.txt", &rig.commit)],
        Some(condition(Some(&rig.commit), Some(&rig.commit))),
    );
    let receipt = analyse(&rig.pm, &mem).unwrap();

    assert_eq!(
        receipt.outcome,
        Outcome::Blocked,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(receipt.to_json()["result"], "blocked");
    assert!(
        receipt.checked.is_empty(),
        "a missing source was reported as checked: {:?}",
        receipt.checked
    );
    assert!(
        receipt
            .findings
            .iter()
            .any(|finding| finding.contains("is missing")),
        "the receipt did not name the missing citation: {:?}",
        receipt.findings
    );
    assert_not_pass("missing source", &receipt);
}

#[test]
fn a_revision_that_is_not_an_immutable_commit_is_blocked() {
    let rig = Rig::new();
    // `HEAD` and a 64-hex digest are both refused: the analysis must not
    // resolve a moving name, and must not substitute a different revision.
    let zeros = "0".repeat(64);
    let mem = memory(
        "moving",
        "A claim pinned to something other than a commit",
        vec![
            git_ref("evidence.txt", "HEAD"),
            git_ref("evidence.txt", &zeros),
        ],
        Some(condition(Some(&rig.commit), None)),
    );
    let receipt = analyse(&rig.pm, &mem).unwrap();

    assert_eq!(
        receipt.outcome,
        Outcome::Blocked,
        "checked {:?}, findings {:?}",
        receipt.checked,
        receipt.findings
    );
    assert!(
        receipt.checked.is_empty(),
        "a non-commit revision resolved: {:?}",
        receipt.checked
    );
    assert!(
        receipt
            .findings
            .iter()
            .all(|finding| finding.contains("40-hex commit")),
        "the receipt did not name the revision rule: {:?}",
        receipt.findings
    );
    assert_not_pass("non-commit revision", &receipt);
}

#[test]
fn an_unreadable_wiki_citation_with_a_condition_is_unknown() {
    let rig = Rig::new();
    let revision = rig.commit_wiki("users/other/private.md", "# Another member's page\n");
    let reference = wiki_ref("users/other/private.md", &revision);

    let (resolution, content) = resolve(&rig.pm, "demo", &Caller::Public, &reference).unwrap();
    assert!(
        matches!(&resolution, Resolution::Unsupported(_)),
        "an unreadable citation did not report itself as unsupported: {resolution:?}"
    );
    assert!(
        content.is_none(),
        "an unreadable citation returned bytes to scan"
    );

    let mem = memory(
        "unreadable",
        "A claim citing a page this reader cannot open",
        vec![reference],
        Some(condition(Some(&revision), None)),
    );
    let receipt = analyse_as(&rig.pm, &mem, &Caller::Public).unwrap();

    assert_eq!(
        receipt.outcome,
        Outcome::Unknown,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(receipt.to_json()["result"], "unknown");
    assert!(
        receipt.checked.is_empty(),
        "an unreadable citation was reported as checked: {:?}",
        receipt.checked
    );
    assert!(
        receipt
            .skipped
            .iter()
            .any(|entry| entry.contains("not readable by this reader")),
        "the receipt did not name the reader refusal: {:?}",
        receipt.skipped
    );
    assert_not_pass("unreadable wiki citation", &receipt);
}

#[test]
fn unsupported_citation_kinds_with_a_condition_are_unknown() {
    let rig = Rig::new();
    let digest = format!("sha256:{}", "0".repeat(64));
    let native = EvidenceRef {
        kind: EvidenceKind::NativeRecord,
        source: "demo/memory/other".into(),
        revision: digest.clone(),
    };
    let external = EvidenceRef {
        kind: EvidenceKind::External,
        source: "https://example.invalid/spec".into(),
        revision: digest,
    };
    for reference in [&native, &external] {
        let (resolution, content) = resolve(&rig.pm, "demo", &Caller::Public, reference).unwrap();
        assert!(
            matches!(&resolution, Resolution::Unsupported(_)),
            "{:?} was not reported as unsupported: {resolution:?}",
            reference.kind
        );
        assert!(content.is_none(), "{:?} returned bytes", reference.kind);
    }

    let mem = memory(
        "unsupported",
        "A claim whose sources this analysis cannot read",
        vec![native, external],
        Some(condition(None, None)),
    );
    let receipt = analyse(&rig.pm, &mem).unwrap();

    assert_eq!(
        receipt.outcome,
        Outcome::Unknown,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(receipt.to_json()["result"], "unknown");
    assert!(
        receipt.checked.is_empty(),
        "an unsupported citation was reported as checked: {:?}",
        receipt.checked
    );
    assert_eq!(
        receipt.skipped.len(),
        2,
        "each unsupported citation must be reported separately: {:?}",
        receipt.skipped
    );
    assert_not_pass("unsupported citation kinds", &receipt);
}

#[test]
fn a_condition_without_evidence_references_is_unknown() {
    let rig = Rig::new();
    let mem = memory(
        "no-refs",
        "A claim that names a condition but cites nothing",
        Vec::new(),
        Some(condition(Some(&rig.commit), Some(&rig.commit))),
    );
    let receipt = analyse(&rig.pm, &mem).unwrap();

    assert_eq!(
        receipt.outcome,
        Outcome::Unknown,
        "a condition with no citation must not pass (checked {:?}, skipped {:?}, findings {:?})",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(receipt.to_json()["result"], "unknown");
    assert_not_pass("condition without references", &receipt);
}

#[test]
fn a_secret_in_a_citation_reference_is_refused_and_redacted() {
    let rig = Rig::new();
    let token = fixture_token();

    // The credential rides in the reference itself, not in resolved content.
    for (label, reference) in [
        ("source", git_ref(&format!("{token}.txt"), &rig.commit)),
        (
            "revision",
            wiki_ref("global/acceptance.md", &format!("fnv1a:{token}")),
        ),
    ] {
        let mem = memory(
            &format!("ref-secret-{label}"),
            "A claim whose citation carries a credential",
            vec![reference],
            Some(condition(Some(&rig.commit), None)),
        );
        let receipt = analyse(&rig.pm, &mem).unwrap();

        assert_eq!(
            receipt.outcome,
            Outcome::Blocked,
            "{label}: a credential-shaped reference was not refused (skipped {:?})",
            receipt.skipped
        );
        assert_eq!(receipt.to_json()["result"], "blocked");
        assert!(
            receipt.checked.is_empty(),
            "{label}: a refused reference was resolved: {:?}",
            receipt.checked
        );
        assert!(
            receipt
                .findings
                .iter()
                .any(|finding| finding.contains("citation reference was refused")),
            "{label}: the receipt did not name the reference refusal: {:?}",
            receipt.findings
        );
        let serialised = receipt.to_json().to_string();
        assert!(
            !serialised.contains(&token),
            "{label}: the receipt echoed the citation secret: {serialised}"
        );
        assert!(
            receipt.findings.iter().all(|f| !f.contains(&token)),
            "{label}: a finding echoed the citation secret: {:?}",
            receipt.findings
        );
    }
}

#[test]
fn secret_bearing_cited_content_blocks_without_echoing_it() {
    let rig = Rig::new();
    let token = fixture_token();
    let revision = rig.commit_bytes("leak.txt", format!("token: {token}\n").as_bytes());

    // The same content through the scan entry point names a rule and never
    // the value.
    let finding = scan_content(&format!("token: {token}\n"))
        .unwrap()
        .expect("a credential-shaped value must be refused");
    assert!(
        !finding.contains(&token),
        "the scan finding echoed the secret: {finding}"
    );

    let mem = memory(
        "content-secret",
        "A claim whose cited content carries a credential",
        vec![git_ref("leak.txt", &revision)],
        Some(condition(Some(&revision), Some(&revision))),
    );
    let receipt = analyse(&rig.pm, &mem).unwrap();

    assert_eq!(
        receipt.outcome,
        Outcome::Blocked,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(receipt.to_json()["result"], "blocked");
    assert_eq!(
        receipt.checked.len(),
        1,
        "the cited blob did not resolve: {:?}",
        receipt.checked
    );
    assert!(
        receipt
            .findings
            .iter()
            .any(|entry| entry.contains("cited content in leak.txt") && entry.contains("refused")),
        "the receipt did not name the content refusal: {:?}",
        receipt.findings
    );
    let serialised = receipt.to_json().to_string();
    assert!(
        !serialised.contains(&token),
        "the receipt echoed the cited secret: {serialised}"
    );
}

#[test]
fn invalid_utf8_cited_content_without_a_token_is_unknown_not_a_pass() {
    let rig = Rig::new();
    let mut bytes = vec![0xff, 0xfe];
    bytes.extend_from_slice(b" plain binary\n");
    let revision = rig.commit_bytes("binary.txt", &bytes);

    let mem = memory(
        "non-utf8",
        "A claim whose cited bytes are not text",
        vec![git_ref("binary.txt", &revision)],
        Some(condition(Some(&revision), Some(&revision))),
    );
    let receipt = analyse(&rig.pm, &mem).unwrap();

    // The bytes resolved, so the citation is not "missing"; but they could
    // not be scanned, so the analysis must not claim a pass either.
    assert_eq!(
        receipt.checked.len(),
        1,
        "the cited blob did not resolve: {:?}",
        receipt.checked
    );
    assert_eq!(
        receipt.outcome,
        Outcome::Unknown,
        "unscanned content was not reported as unknown (skipped {:?}, findings {:?})",
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(receipt.to_json()["result"], "unknown");
    assert!(
        receipt.findings.is_empty(),
        "unscannable but credential-free bytes were reported as a refusal: {:?}",
        receipt.findings
    );
    assert!(
        receipt
            .skipped
            .iter()
            .any(|entry| entry.contains("not UTF-8")),
        "the receipt did not say why the content was not scanned: {:?}",
        receipt.skipped
    );
    assert_not_pass("non-UTF-8 cited content", &receipt);
}

#[test]
fn invalid_utf8_cited_content_with_an_embedded_token_never_passes_or_echoes_it() {
    let rig = Rig::new();
    let token = fixture_token();
    let mut bytes = vec![0xff, 0xfe];
    bytes.extend_from_slice(format!(" token: {token}\n").as_bytes());
    let revision = rig.commit_bytes("binary-token.txt", &bytes);

    let mem = memory(
        "non-utf8-token",
        "A claim whose cited bytes hide a credential",
        vec![git_ref("binary-token.txt", &revision)],
        Some(condition(Some(&revision), Some(&revision))),
    );
    let receipt = analyse(&rig.pm, &mem).unwrap();

    // Either the bytes are refused, or the receipt admits the scan was
    // incomplete. A pass would mean the token rode along unscanned.
    assert_eq!(
        receipt.checked.len(),
        1,
        "the cited blob did not resolve: {:?}",
        receipt.checked
    );
    assert_not_pass("non-UTF-8 cited content with a token", &receipt);
    let refused = receipt
        .findings
        .iter()
        .any(|finding| finding.contains("refused"));
    let incomplete = receipt
        .skipped
        .iter()
        .any(|entry| entry.contains("not UTF-8"));
    assert!(
        refused || incomplete,
        "the receipt neither refused the credential nor reported the unscanned bytes: \
         skipped {:?}, findings {:?}",
        receipt.skipped,
        receipt.findings
    );
    let serialised = receipt.to_json().to_string();
    assert!(
        !serialised.contains(&token),
        "the receipt echoed the embedded token: {serialised}"
    );
}

#[test]
fn a_valid_wiki_revision_passes_proposal_validation_and_resolution() {
    let rig = Rig::new();
    assert!(
        matches!(
            cadence_agent::home::layout().unwrap().source,
            cadence_agent::home::Source::Legacy
        ),
        "this fixture needs the legacy <tracker>/wiki vault; a CADENCE_HOME \
         would move the vault outside the tracker and no Wiki citation could resolve"
    );
    let revision = rig.commit_wiki(
        "global/acceptance.md",
        "# CAD-348 Evidence Fixture\n\nThe page this record cites.\n",
    );
    let reference = wiki_ref("global/acceptance.md", &revision);

    // The exact citation a proposal would carry must pass proposal
    // validation: a retained 40-hex lowercase Git commit is immutable.
    policy::validate_refs(std::slice::from_ref(&reference))
        .expect("a retained Wiki commit is a valid immutable citation");
    let options = ProposalOptions {
        evidence_refs: vec![reference.clone()],
        verification_condition: Some(condition(Some(&revision), None)),
        ..Default::default()
    };
    policy::validate_options(&options)
        .expect("the proposal's Wiki citation and condition are valid");

    let mem = memory(
        "wiki-pass",
        "The cited wiki page is the source of this claim",
        vec![reference],
        Some(condition(Some(&revision), None)),
    );
    let receipt = analyse_as(&rig.pm, &mem, &Caller::Public).unwrap();

    assert_eq!(
        receipt.outcome,
        Outcome::Pass,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(receipt.to_json()["result"], "pass");
    assert!(
        receipt
            .checked
            .iter()
            .any(|entry| entry.starts_with(&format!("wiki:global/acceptance.md@{revision} "))),
        "the validated citation did not resolve at the revision it names: {:?}",
        receipt.checked
    );
    assert!(
        receipt.skipped.is_empty(),
        "a passing receipt skipped work: {:?}",
        receipt.skipped
    );
    assert!(
        receipt.findings.is_empty(),
        "a passing receipt carried findings: {:?}",
        receipt.findings
    );
    assert_eq!(
        receipt.source_revision.as_deref(),
        Some(revision.as_str()),
        "the receipt did not report the pinned source revision"
    );
}

#[test]
fn applicability_is_checked_at_the_pinned_target() {
    let rig = Rig::new();
    // A later commit that leaves the cited file byte-identical, and another
    // that changes it. The target must be the pinned revision, not HEAD.
    let untouched = rig.commit_bytes("unrelated.txt", b"nothing cited\n");
    let moved = rig.commit_bytes("evidence.txt", b"changed at the target\n");

    let unchanged = memory(
        "same",
        "Cites a source that did not change",
        vec![git_ref("evidence.txt", &rig.commit)],
        Some(condition(Some(&rig.commit), Some(&untouched))),
    );
    let receipt = analyse(&rig.pm, &unchanged).unwrap();
    assert_eq!(
        receipt.outcome,
        Outcome::Pass,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    let at_target = format!("evidence.txt is unchanged at target {untouched}");
    assert!(
        receipt
            .checked
            .iter()
            .any(|entry| entry.contains(&at_target)),
        "the receipt did not confirm the source at its pinned target: {:?}",
        receipt.checked
    );

    let changed = memory(
        "moved",
        "Cites a source that changed at the target",
        vec![git_ref("evidence.txt", &rig.commit)],
        Some(condition(Some(&rig.commit), Some(&moved))),
    );
    let receipt = analyse(&rig.pm, &changed).unwrap();
    assert_eq!(
        receipt.outcome,
        Outcome::Revise,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert!(
        receipt
            .findings
            .iter()
            .any(|finding| finding.contains("differs between cited revision")),
        "the receipt did not name the changed source: {:?}",
        receipt.findings
    );
    assert_not_pass("source changed at the target", &receipt);
}

#[test]
fn attestation_evidence_references_are_resolved_and_secret_refused() {
    let rig = Rig::new();
    let attested = with_attestation(
        memory(
            "attested",
            "A claim attested by an operator citation",
            Vec::new(),
            Some(condition(Some(&rig.commit), None)),
        ),
        vec![git_ref("evidence.txt", &rig.commit)],
    );
    let receipt = analyse(&rig.pm, &attested).unwrap();

    assert_eq!(
        receipt.outcome,
        Outcome::Pass,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(
        receipt.checked.len(),
        1,
        "the attestation's citation was not resolved: {:?}",
        receipt.checked
    );
    assert!(
        receipt.checked[0].starts_with(&format!("git:evidence.txt@{} ", rig.commit)),
        "the receipt resolved something other than the attestation citation: {:?}",
        receipt.checked
    );

    let token = fixture_token();
    let refused = with_attestation(
        memory(
            "attested-secret",
            "A claim whose attestation carries a credential",
            Vec::new(),
            Some(condition(Some(&rig.commit), None)),
        ),
        vec![git_ref(&format!("{token}.txt"), &rig.commit)],
    );
    let receipt = analyse(&rig.pm, &refused).unwrap();

    assert_eq!(
        receipt.outcome,
        Outcome::Blocked,
        "a credential in an attestation citation was not refused (skipped {:?})",
        receipt.skipped
    );
    assert_eq!(receipt.to_json()["result"], "blocked");
    assert!(
        receipt.checked.is_empty(),
        "a refused attestation citation was resolved: {:?}",
        receipt.checked
    );
    assert!(
        receipt
            .findings
            .iter()
            .any(|finding| finding.contains("citation reference was refused")),
        "the receipt did not name the attestation reference refusal: {:?}",
        receipt.findings
    );
    let serialised = receipt.to_json().to_string();
    assert!(
        !serialised.contains(&token),
        "the receipt echoed the attestation secret: {serialised}"
    );
}

#[test]
fn oversized_wiki_historical_text_is_unknown_without_checked() {
    let rig = Rig::new();
    let limit = cadence_agent::memory::evidence::MAX_CITED_BYTES;
    let body = "a".repeat(limit + 1);
    let revision = rig.commit_wiki("global/oversized.md", &body);
    let mem = memory(
        "oversized-wiki",
        "A claim citing a page above the read bound",
        vec![wiki_ref("global/oversized.md", &revision)],
        Some(condition(Some(&revision), None)),
    );
    let receipt = analyse_as(&rig.pm, &mem, &Caller::Public).unwrap();

    assert!(
        receipt.checked.is_empty(),
        "an over-bound page was reported as checked: {:?}",
        receipt.checked
    );
    assert_eq!(
        receipt.outcome,
        Outcome::Unknown,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(receipt.to_json()["result"], "unknown");
    assert!(
        receipt
            .skipped
            .iter()
            .any(|entry| entry.contains(&format!("above the {limit}-byte read bound"))),
        "the receipt did not name the read bound: {:?}",
        receipt.skipped
    );
    assert_not_pass("oversized Wiki text", &receipt);
}

#[test]
fn bounded_wiki_blob_with_a_secret_blocks_and_an_oversized_blob_is_unknown() {
    let rig = Rig::new();
    let token = fixture_token();
    let secret = format!("token: {token}\n");
    let revision = rig.commit_wiki_blob("global/report.bin", secret.as_bytes(), None);
    let mem = memory(
        "wiki-blob-secret",
        "A claim citing a blob that carries a credential",
        vec![wiki_ref("global/report.bin", &revision)],
        Some(condition(Some(&revision), None)),
    );
    let receipt = analyse_as(&rig.pm, &mem, &Caller::Public).unwrap();

    assert_eq!(
        receipt.checked.len(),
        1,
        "the bounded blob did not resolve: {:?}",
        receipt.checked
    );
    assert_eq!(
        receipt.outcome,
        Outcome::Blocked,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(receipt.to_json()["result"], "blocked");
    let blob_refusal = receipt
        .findings
        .iter()
        .find(|entry| entry.contains("cited content in global/report.bin"));
    assert!(
        blob_refusal.is_some_and(|entry| entry.contains("refused")),
        "the receipt did not name the blob content refusal: {:?}",
        receipt.findings
    );
    let serialised = receipt.to_json().to_string();
    assert!(
        !serialised.contains(&token),
        "the receipt echoed the blob secret: {serialised}"
    );

    let limit = cadence_agent::memory::evidence::MAX_CITED_BYTES;
    let oversized = rig.commit_wiki_blob(
        "global/oversized.bin",
        b"small object",
        Some((limit + 1) as u64),
    );
    let mem = memory(
        "wiki-blob-oversized",
        "A claim citing a blob above the read bound",
        vec![wiki_ref("global/oversized.bin", &oversized)],
        Some(condition(Some(&oversized), None)),
    );
    let receipt = analyse_as(&rig.pm, &mem, &Caller::Public).unwrap();

    assert!(
        receipt.checked.is_empty(),
        "an over-bound blob was reported as checked: {:?}",
        receipt.checked
    );
    assert_eq!(
        receipt.outcome,
        Outcome::Unknown,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(receipt.to_json()["result"], "unknown");
    assert!(
        receipt
            .skipped
            .iter()
            .any(|entry| entry.contains(&format!("above the {limit}-byte read bound"))),
        "the receipt did not name the blob read bound: {:?}",
        receipt.skipped
    );
    assert_not_pass("oversized Wiki blob", &receipt);
}

#[test]
fn a_wiki_target_with_identical_page_bytes_passes_and_names_the_target() {
    let rig = Rig::new();
    let cited = rig.commit_wiki("global/acceptance.md", "# Acceptance\n\nfirst text\n");
    // A later tracker commit that does not touch the cited page, so its bytes
    // are identical at both revisions.
    let target = rig.commit_wiki("global/other.md", "# Other\n");
    let reference = wiki_ref("global/acceptance.md", &cited);
    let pinned = condition_with_wiki_target(Some(&cited), None, Some(&target));

    // The condition a proposal would carry must pass proposal validation: a
    // retained tracker commit is the only accepted Wiki target.
    policy::validate_options(&ProposalOptions {
        evidence_refs: vec![reference.clone()],
        verification_condition: Some(pinned.clone()),
        ..Default::default()
    })
    .expect("a retained Wiki commit is a valid Wiki target");

    let mem = memory(
        "wiki-target-same",
        "The cited page still says the same thing at the Wiki target",
        vec![reference],
        Some(pinned),
    );
    let receipt = analyse_as(&rig.pm, &mem, &Caller::Public).unwrap();

    assert_eq!(
        receipt.outcome,
        Outcome::Pass,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(receipt.to_json()["result"], "pass");
    assert!(
        receipt
            .checked
            .iter()
            .any(|entry| entry.starts_with(&format!("wiki:global/acceptance.md@{cited} "))),
        "the citation did not resolve at the revision it names: {:?}",
        receipt.checked
    );
    let at_target = format!("global/acceptance.md is unchanged at Wiki target {target}");
    assert!(
        receipt
            .checked
            .iter()
            .any(|entry| entry.contains(&at_target)),
        "the receipt did not confirm the page at its Wiki target: {:?}",
        receipt.checked
    );
    assert!(
        receipt.skipped.is_empty(),
        "a passing receipt skipped work: {:?}",
        receipt.skipped
    );
    assert!(
        receipt.findings.is_empty(),
        "a passing receipt carried findings: {:?}",
        receipt.findings
    );
    assert_eq!(
        receipt.wiki_target_revision.as_deref(),
        Some(target.as_str()),
        "the receipt did not report the Wiki target it checked"
    );
    assert_eq!(receipt.to_json()["wiki_target_revision"], target);
    assert!(
        receipt.target_revision.is_none(),
        "a Wiki target was reported as the project target: {:?}",
        receipt.target_revision
    );
}

#[test]
fn a_wiki_target_with_changed_page_bytes_revises() {
    let rig = Rig::new();
    let cited = rig.commit_wiki("global/acceptance.md", "# Acceptance\n\nfirst text\n");
    let target = rig.commit_wiki("global/acceptance.md", "# Acceptance\n\nsecond text\n");

    let mem = memory(
        "wiki-target-changed",
        "The cited page changed after the revision it names",
        vec![wiki_ref("global/acceptance.md", &cited)],
        Some(condition_with_wiki_target(
            Some(&cited),
            None,
            Some(&target),
        )),
    );
    let receipt = analyse_as(&rig.pm, &mem, &Caller::Public).unwrap();

    assert_eq!(
        receipt.outcome,
        Outcome::Revise,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(receipt.to_json()["result"], "revise");
    assert!(
        receipt
            .findings
            .iter()
            .any(|finding| finding.contains("differs between cited revision")
                && finding.contains(&target)),
        "the receipt did not name the page that moved: {:?}",
        receipt.findings
    );
    assert!(
        !receipt
            .checked
            .iter()
            .any(|entry| entry.contains("unchanged at Wiki target")),
        "changed page bytes were reported as unchanged: {:?}",
        receipt.checked
    );
    assert_not_pass("page changed at the Wiki target", &receipt);
}

#[test]
fn a_wiki_target_missing_the_cited_page_revises() {
    let rig = Rig::new();
    // The target commit predates the page: a real tracker commit whose tree
    // carries no original at this path. A miss must never fall back to the
    // live page or to the cited revision.
    let target = rig.commit_wiki("global/other.md", "# Other\n");
    let cited = rig.commit_wiki("global/acceptance.md", "# Acceptance\n");

    let mem = memory(
        "wiki-target-missing-page",
        "The cited page is absent from the Wiki target",
        vec![wiki_ref("global/acceptance.md", &cited)],
        Some(condition_with_wiki_target(
            Some(&cited),
            None,
            Some(&target),
        )),
    );
    let receipt = analyse_as(&rig.pm, &mem, &Caller::Public).unwrap();

    assert_eq!(
        receipt.outcome,
        Outcome::Revise,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(receipt.to_json()["result"], "revise");
    assert!(
        receipt
            .findings
            .iter()
            .any(|finding| finding.contains(&format!(
                "global/acceptance.md is absent at Wiki target {target}"
            ))),
        "the receipt did not name the page absent from its target: {:?}",
        receipt.findings
    );
    assert!(
        !receipt
            .checked
            .iter()
            .any(|entry| entry.contains("unchanged at Wiki target")),
        "an absent page was reported as unchanged: {:?}",
        receipt.checked
    );
    assert_not_pass("page absent at the Wiki target", &receipt);
}

#[test]
fn an_unknown_wiki_target_is_incomplete_never_a_pass() {
    let rig = Rig::new();
    let cited = rig.commit_wiki("global/acceptance.md", "# Acceptance\n");
    // Commit-shaped, and no such object in the vault.
    let unknown = "0".repeat(40);

    let mem = memory(
        "wiki-target-unknown",
        "The Wiki target this record names is not in the tracker",
        vec![wiki_ref("global/acceptance.md", &cited)],
        Some(condition_with_wiki_target(
            Some(&cited),
            None,
            Some(&unknown),
        )),
    );
    let receipt = analyse_as(&rig.pm, &mem, &Caller::Public).unwrap();

    assert_eq!(
        receipt.outcome,
        Outcome::Unknown,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(receipt.to_json()["result"], "unknown");
    assert!(
        receipt
            .checked
            .iter()
            .any(|entry| entry.starts_with(&format!("wiki:global/acceptance.md@{cited} "))),
        "the cited revision was not resolved: {:?}",
        receipt.checked
    );
    assert!(
        receipt
            .skipped
            .iter()
            .any(|entry| entry.contains("Wiki target check skipped")
                && entry.contains("global/acceptance.md")),
        "the receipt did not report the unresolved Wiki target: {:?}",
        receipt.skipped
    );
    assert!(
        !receipt
            .checked
            .iter()
            .any(|entry| entry.contains("unchanged at Wiki target")),
        "an unresolved Wiki target was reported as checked: {:?}",
        receipt.checked
    );
    assert!(
        receipt.findings.is_empty(),
        "an unresolved target was reported as a change: {:?}",
        receipt.findings
    );
    assert_eq!(
        receipt.wiki_target_revision.as_deref(),
        Some(unknown.as_str()),
        "the receipt did not report the target it was asked to check"
    );
    assert_not_pass("unknown Wiki target", &receipt);
}

#[test]
fn an_unreadable_wiki_target_is_incomplete_never_a_pass() {
    let rig = Rig::new();
    let limit = cadence_agent::memory::evidence::MAX_CITED_BYTES;
    let cited = rig.commit_wiki("global/acceptance.md", "# Acceptance\n");
    // A real tracker commit whose page is above the bounded reader's cap: the
    // target cannot be read, so the analysis must say so rather than pass.
    let target = rig.commit_wiki("global/acceptance.md", &"a".repeat(limit + 1));

    let mem = memory(
        "wiki-target-unreadable",
        "The Wiki target this record names is above the read bound",
        vec![wiki_ref("global/acceptance.md", &cited)],
        Some(condition_with_wiki_target(
            Some(&cited),
            None,
            Some(&target),
        )),
    );
    let receipt = analyse_as(&rig.pm, &mem, &Caller::Public).unwrap();

    assert_eq!(
        receipt.outcome,
        Outcome::Unknown,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(receipt.to_json()["result"], "unknown");
    assert!(
        receipt
            .skipped
            .iter()
            .any(|entry| entry.contains("Wiki target check skipped")
                && entry.contains(&format!("above the {limit}-byte read bound"))),
        "the receipt did not name the target read refusal: {:?}",
        receipt.skipped
    );
    assert!(
        !receipt
            .checked
            .iter()
            .any(|entry| entry.contains("unchanged at Wiki target")),
        "an unreadable Wiki target was reported as checked: {:?}",
        receipt.checked
    );
    assert_not_pass("unreadable Wiki target", &receipt);
}

#[test]
fn a_wiki_target_is_read_with_the_same_caller_acl_as_the_citation() {
    let rig = Rig::new();
    let cited = rig.commit_wiki("users/other/private.md", "# Another member's page\n");
    let target = rig.commit_wiki("global/other.md", "# Other\n");

    let mem = memory(
        "wiki-target-acl",
        "The cited page is outside this reader's scope",
        vec![wiki_ref("users/other/private.md", &cited)],
        Some(condition_with_wiki_target(
            Some(&cited),
            None,
            Some(&target),
        )),
    );
    let receipt = analyse_as(&rig.pm, &mem, &Caller::Public).unwrap();

    assert_eq!(
        receipt.outcome,
        Outcome::Unknown,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert!(
        receipt.checked.is_empty(),
        "a citation outside the caller's scope was reported as checked: {:?}",
        receipt.checked
    );
    assert!(
        receipt
            .skipped
            .iter()
            .any(|entry| entry.contains("not readable by this reader")),
        "the receipt did not name the reader refusal: {:?}",
        receipt.skipped
    );
    assert!(
        !receipt
            .skipped
            .iter()
            .any(|entry| entry.contains("Wiki target check skipped")),
        "a target check ran for a citation the caller cannot read: {:?}",
        receipt.skipped
    );
    assert_not_pass("Wiki target behind the caller's ACL", &receipt);
}

#[test]
fn a_project_target_does_not_stand_in_for_a_wiki_target() {
    let rig = Rig::new();
    let cited = rig.commit_wiki("global/acceptance.md", "# Acceptance\n");
    // A project-side commit that leaves the cited file byte-identical. It is
    // the wrong repository for a Wiki citation, so it cannot confirm anything.
    let project_target = rig.commit_bytes("unrelated.txt", b"nothing cited\n");

    let mem = memory(
        "wiki-target-project",
        "The project target is not the Wiki target",
        vec![wiki_ref("global/acceptance.md", &cited)],
        Some(condition(Some(&cited), Some(&project_target))),
    );
    let receipt = analyse_as(&rig.pm, &mem, &Caller::Public).unwrap();

    assert_eq!(
        receipt.outcome,
        Outcome::Unknown,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(receipt.to_json()["result"], "unknown");
    assert!(
        receipt
            .skipped
            .iter()
            .any(|entry| entry.contains("no Wiki target revision recorded")),
        "the receipt did not separate the project target from the Wiki target: {:?}",
        receipt.skipped
    );
    assert!(
        !receipt
            .checked
            .iter()
            .any(|entry| entry.contains("unchanged at")),
        "a project commit was reported as confirming a Wiki page: {:?}",
        receipt.checked
    );
    assert_eq!(
        receipt.target_revision.as_deref(),
        Some(project_target.as_str()),
        "the receipt did not report the project target"
    );
    assert!(
        receipt.wiki_target_revision.is_none(),
        "a project target was reported as a Wiki target: {:?}",
        receipt.wiki_target_revision
    );
    assert_not_pass("project target used for a Wiki citation", &receipt);
}

#[test]
fn an_absent_wiki_target_keeps_the_recorded_condition_representation() {
    let source = "a".repeat(40);
    let target = "b".repeat(40);
    // Exactly the representation revision 2 wrote: three keys, no Wiki target.
    let recorded = serde_json::json!({
        "check_id": "evidence-analysis",
        "source_revision": source.clone(),
        "target_revision": target.clone(),
    });

    let parsed: VerificationCondition = serde_json::from_value(recorded.clone())
        .expect("a recorded condition without a Wiki target still deserializes");
    assert_eq!(
        parsed.wiki_target_revision, None,
        "an absent Wiki target did not deserialize as None"
    );
    assert_eq!(parsed, condition(Some(&source), Some(&target)));
    assert_eq!(
        serde_json::to_value(&parsed).unwrap(),
        recorded,
        "an absent Wiki target changed the recorded condition representation"
    );

    // An explicit null is the same absence, not a second representation.
    let mut with_null = recorded.clone();
    with_null["wiki_target_revision"] = serde_json::Value::Null;
    let parsed: VerificationCondition =
        serde_json::from_value(with_null).expect("an explicit null Wiki target is absence");
    assert_eq!(parsed.wiki_target_revision, None);
    assert_eq!(serde_json::to_value(&parsed).unwrap(), recorded);

    // The condition is covered by the claim digest, so the policy-level
    // representation must not move either.
    let recorded_policy = policy_with(Vec::new(), Some(parsed));
    let semantic = serde_json::to_value(policy::semantic_fields(&recorded_policy)).unwrap();
    assert_eq!(
        semantic["verification_condition"], recorded,
        "the policy's recorded condition changed: {semantic}"
    );
    assert!(
        !semantic.to_string().contains("wiki_target_revision"),
        "an absent Wiki target leaked into the covered policy: {semantic}"
    );

    // A named Wiki target is recorded, and is part of what the digest covers.
    let named = condition_with_wiki_target(Some(&source), Some(&target), Some(&source));
    let semantic = serde_json::to_value(policy::semantic_fields(&policy_with(
        Vec::new(),
        Some(named),
    )))
    .unwrap();
    assert_eq!(
        semantic["verification_condition"]["wiki_target_revision"], source,
        "a named Wiki target was not recorded: {semantic}"
    );
    assert_ne!(
        semantic["verification_condition"], recorded,
        "a named Wiki target left the covered condition unchanged: {semantic}"
    );
}

#[test]
fn a_wiki_target_selector_that_is_not_a_retained_commit_is_refused() {
    let rig = Rig::new();
    let cited = rig.commit_wiki("global/acceptance.md", "# Acceptance\n");
    let reference = wiki_ref("global/acceptance.md", &cited);

    let refused = [
        "HEAD".to_string(),
        "a".repeat(39),
        "a".repeat(64),
        "A".repeat(40),
        format!("fnv1a:{}", "0".repeat(40)),
        String::new(),
    ];
    for selector in &refused {
        let options = ProposalOptions {
            evidence_refs: vec![reference.clone()],
            verification_condition: Some(condition_with_wiki_target(
                Some(&cited),
                None,
                Some(selector.as_str()),
            )),
            ..Default::default()
        };
        let error = policy::validate_options(&options)
            .expect_err("a Wiki target that is not a retained commit was accepted");
        assert!(
            error.to_string().contains("40-digit lowercase commit"),
            "{selector:?}: the refusal did not name the retained-commit rule: {error}"
        );

        let mem = memory(
            "wiki-target-invalid",
            "A record whose Wiki target is not a retained commit",
            vec![reference.clone()],
            Some(condition_with_wiki_target(
                Some(&cited),
                None,
                Some(selector.as_str()),
            )),
        );
        let receipt = analyse_as(&rig.pm, &mem, &Caller::Public).unwrap();
        assert_not_pass(&format!("Wiki target selector {selector:?}"), &receipt);
        assert!(
            !receipt
                .checked
                .iter()
                .any(|entry| entry.contains("unchanged at Wiki target")),
            "{selector:?}: a refused selector was reported as checked: {:?}",
            receipt.checked
        );
    }

    // The one accepted shape is the full lowercase commit the tracker retains.
    policy::validate_options(&ProposalOptions {
        evidence_refs: vec![reference],
        verification_condition: Some(condition_with_wiki_target(Some(&cited), None, Some(&cited))),
        ..Default::default()
    })
    .expect("a full lowercase tracker commit is a valid Wiki target");
}

#[test]
fn a_missing_target_commit_is_unknown_but_an_absent_path_at_a_real_target_revises() {
    let rig = Rig::new();
    // A definite later commit that exists and no longer carries the path.
    git(&rig.repo, &["rm", "-q", "--", "evidence.txt"]);
    let removed = commit_tree(&rig.repo, "remove the cited path");

    let absent = memory(
        "target-path-absent",
        "Cites a path removed at the pinned target",
        vec![git_ref("evidence.txt", &rig.commit)],
        Some(condition(Some(&rig.commit), Some(&removed))),
    );
    let receipt = analyse(&rig.pm, &absent).unwrap();
    assert_eq!(
        receipt.outcome,
        Outcome::Revise,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert!(
        receipt
            .findings
            .iter()
            .any(|finding| finding.contains("is absent at target")),
        "the receipt did not name the absent path at a real target: {:?}",
        receipt.findings
    );

    // A target commit that does not exist cannot say the path was removed:
    // the check is unanswered, not a revision.
    let missing_target = "0".repeat(40);
    let unresolved = memory(
        "target-missing-commit",
        "Cites a target commit that does not exist",
        vec![git_ref("evidence.txt", &rig.commit)],
        Some(condition(Some(&rig.commit), Some(&missing_target))),
    );
    let receipt = analyse(&rig.pm, &unresolved).unwrap();
    assert_eq!(
        receipt.outcome,
        Outcome::Unknown,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(receipt.to_json()["result"], "unknown");
    assert!(
        receipt
            .findings
            .iter()
            .all(|finding| !finding.contains("is absent at target")),
        "a nonexistent target commit was reported as an absent path: {:?}",
        receipt.findings
    );
    assert!(
        receipt
            .skipped
            .iter()
            .any(|entry| entry.contains("target") || entry.contains(&missing_target)),
        "the receipt did not record the unresolved target check: {:?}",
        receipt.skipped
    );
    assert_eq!(
        receipt.checked.len(),
        1,
        "the cited source did not resolve: {:?}",
        receipt.checked
    );
    assert_not_pass("nonexistent target commit", &receipt);
}

#[test]
fn an_unreadable_project_record_is_unknown_and_its_scope_is_sanitized() {
    let rig = Rig::new();
    let token = fixture_token();
    // A sibling record that cannot be loaded. Its name carries a credential so
    // a naive error passthrough would echo it into the receipt.
    let dir = rig.pm.dir.join("demo").join("memory");
    std::fs::create_dir_all(&dir).unwrap();
    let malformed = "---\nnot: [valid\n---\n\nbody\n";
    std::fs::write(dir.join(format!("{token}.md")), malformed).unwrap();

    let mem = memory(
        "candidate",
        "A claim whose project pool cannot be fully read",
        vec![git_ref("evidence.txt", &rig.commit)],
        Some(condition(Some(&rig.commit), None)),
    );
    let receipt = analyse(&rig.pm, &mem).unwrap();

    assert_eq!(
        receipt.outcome,
        Outcome::Unknown,
        "an unreadable sibling did not make the analysis incomplete \
         (checked {:?}, skipped {:?}, findings {:?})",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(receipt.to_json()["result"], "unknown");
    assert!(
        receipt.findings.is_empty(),
        "an unreadable sibling was reported as a refusal: {:?}",
        receipt.findings
    );
    assert!(
        receipt.skipped.iter().any(|entry| entry.contains("demo")),
        "the receipt did not name the sanitized project scope: {:?}",
        receipt.skipped
    );
    let serialised = receipt.to_json().to_string();
    assert!(
        !serialised.contains(&token),
        "the receipt leaked the unreadable record's name: {serialised}"
    );
    assert_not_pass("unreadable project pool", &receipt);
}

#[test]
fn reordered_and_repeated_component_selectors_still_detect_duplicates() {
    let rig = Rig::new();
    persist(
        &rig,
        &with_scope(
            memory("existing", "Use the pinned digest", Vec::new(), None),
            &["alpha", "beta"],
            &[],
        ),
    );

    let reordered = with_scope(
        memory("reordered", "use the   pinned digest", Vec::new(), None),
        &["beta", "alpha"],
        &[],
    );
    let receipt = analyse(&rig.pm, &reordered).unwrap();
    assert_eq!(
        receipt.outcome,
        Outcome::Revise,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert!(
        receipt
            .findings
            .iter()
            .any(|finding| finding.contains("identical claim")),
        "reordered components missed the duplicate: {:?}",
        receipt.findings
    );

    let repeated = with_scope(
        memory("repeated", "Use the pinned digest", Vec::new(), None),
        &["alpha", "alpha", "beta"],
        &[],
    );
    let receipt = analyse(&rig.pm, &repeated).unwrap();
    assert_eq!(
        receipt.outcome,
        Outcome::Revise,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert!(
        receipt
            .findings
            .iter()
            .any(|finding| finding.contains("identical claim")),
        "repeated selectors missed the duplicate: {:?}",
        receipt.findings
    );

    // A different component set is a different scope, even with the same body.
    let different = with_scope(
        memory("different", "Use the pinned digest", Vec::new(), None),
        &["alpha", "gamma"],
        &[],
    );
    let receipt = analyse(&rig.pm, &different).unwrap();
    assert!(
        receipt.findings.is_empty(),
        "a different component set was treated as the same scope: {:?}",
        receipt.findings
    );
    assert_eq!(
        receipt.outcome,
        Outcome::Unknown,
        "a condition-less record is unknown, not a duplicate"
    );

    // A different path selector is a different scope too.
    let paths_differ = with_scope(
        memory("paths-differ", "Use the pinned digest", Vec::new(), None),
        &["alpha", "beta"],
        &["src/adapter/**"],
    );
    let receipt = analyse(&rig.pm, &paths_differ).unwrap();
    assert!(
        receipt.findings.is_empty(),
        "a different path selector was treated as the same scope: {:?}",
        receipt.findings
    );
}

#[test]
fn reordered_component_selectors_still_detect_contradictions() {
    let rig = Rig::new();
    persist(
        &rig,
        &with_scope(
            memory("target", "Original guidance", Vec::new(), None),
            &["alpha", "beta"],
            &[],
        ),
    );

    let citing = with_scope(
        memory(
            "citing",
            "New guidance\ncontradicts: target\n",
            Vec::new(),
            None,
        ),
        &["beta", "alpha"],
        &[],
    );
    let receipt = analyse(&rig.pm, &citing).unwrap();
    assert_eq!(
        receipt.outcome,
        Outcome::Revise,
        "checked {:?}, skipped {:?}, findings {:?}",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert!(
        receipt
            .findings
            .iter()
            .any(|finding| finding.contains("contradicted")),
        "reordered components missed the contradiction: {:?}",
        receipt.findings
    );

    // The same body naming the same target from another selector set is not a
    // contradiction in this scope.
    let other_scope = with_scope(
        memory(
            "citing-other",
            "New guidance\ncontradicts: target\n",
            Vec::new(),
            None,
        ),
        &["alpha"],
        &[],
    );
    let receipt = analyse(&rig.pm, &other_scope).unwrap();
    assert!(
        receipt.findings.is_empty(),
        "a contradiction across different scopes was reported: {:?}",
        receipt.findings
    );
}

#[test]
fn a_git_replacement_cannot_swap_the_cited_commits_bytes() {
    let rig = Rig::new();
    let token = fixture_token();
    let secret = format!("token: {token}\n");
    let cited = rig.commit_bytes("leak.txt", secret.as_bytes());
    let harmless = rig.commit_bytes("leak.txt", b"harmless replacement\n");
    git(&rig.repo, &["replace", &cited, &harmless]);
    let replace_ref = format!("refs/replace/{cited}");
    let installed = git(&rig.repo, &["show-ref", "--verify", &replace_ref]);
    assert!(
        installed.contains(&replace_ref),
        "the fixture did not install the replacement ref"
    );
    assert_eq!(
        git(&rig.repo, &["cat-file", "-p", &format!("{cited}:leak.txt")]),
        "harmless replacement",
        "the fixture's replacement ref does not affect a plain object read"
    );

    let mem = memory(
        "replaced-git",
        "Cites a commit whose object has a replacement ref",
        vec![git_ref("leak.txt", &cited)],
        Some(condition(Some(&cited), None)),
    );
    let receipt = analyse(&rig.pm, &mem).unwrap();

    assert_eq!(
        receipt.outcome,
        Outcome::Blocked,
        "a replacement ref hid the cited commit's secret \
         (checked {:?}, skipped {:?}, findings {:?})",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(receipt.to_json()["result"], "blocked");
    assert_eq!(
        receipt.checked.len(),
        1,
        "the cited blob did not resolve: {:?}",
        receipt.checked
    );
    let original_digest = format!("sha256 {}", sha256_hex(secret.as_bytes()));
    assert!(
        receipt.checked[0].contains(&original_digest),
        "the analysis read the replacement bytes instead of the cited commit's: {:?}",
        receipt.checked
    );
    assert!(
        receipt
            .findings
            .iter()
            .any(|finding| finding.contains("cited content in leak.txt")),
        "the receipt did not name the cited content refusal: {:?}",
        receipt.findings
    );
    let serialised = receipt.to_json().to_string();
    assert!(
        !serialised.contains(&token),
        "the receipt echoed the cited secret: {serialised}"
    );
    assert_not_pass("replaced Git commit", &receipt);
}

#[test]
fn a_git_replacement_cannot_swap_a_wiki_page_at_its_pinned_commit() {
    let rig = Rig::new();
    let token = fixture_token();
    let secret = format!("# Pinned\n\ntoken: {token}\n");
    let cited = rig.commit_wiki("global/pinned.md", &secret);
    let harmless = rig.commit_wiki("global/pinned.md", "# Pinned\n\nharmless replacement\n");
    git(&rig.pm.dir, &["replace", &cited, &harmless]);
    let replace_ref = format!("refs/replace/{cited}");
    let installed = git(&rig.pm.dir, &["show-ref", "--verify", &replace_ref]);
    assert!(
        installed.contains(&replace_ref),
        "the fixture did not install the replacement ref"
    );
    assert_eq!(
        git(
            &rig.pm.dir,
            &["cat-file", "-p", &format!("{cited}:wiki/global/pinned.md")]
        ),
        "# Pinned\n\nharmless replacement",
        "the fixture's replacement ref does not affect a plain object read"
    );

    let mem = memory(
        "replaced-wiki",
        "Cites a wiki commit whose object has a replacement ref",
        vec![wiki_ref("global/pinned.md", &cited)],
        Some(condition(Some(&cited), None)),
    );
    let receipt = analyse_as(&rig.pm, &mem, &Caller::Public).unwrap();

    assert_eq!(
        receipt.outcome,
        Outcome::Blocked,
        "a replacement ref hid the pinned page's secret \
         (checked {:?}, skipped {:?}, findings {:?})",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(receipt.to_json()["result"], "blocked");
    assert_eq!(
        receipt.checked.len(),
        1,
        "the cited page did not resolve: {:?}",
        receipt.checked
    );
    let original_digest = format!("sha256 {}", sha256_hex(secret.as_bytes()));
    assert!(
        receipt.checked[0].contains(&original_digest),
        "the analysis read the replacement page instead of the pinned commit's: {:?}",
        receipt.checked
    );
    assert!(
        receipt
            .findings
            .iter()
            .any(|finding| finding.contains("cited content in global/pinned.md")),
        "the receipt did not name the pinned page's refusal: {:?}",
        receipt.findings
    );
    let serialised = receipt.to_json().to_string();
    assert!(
        !serialised.contains(&token),
        "the receipt echoed the pinned page's secret: {serialised}"
    );
    assert_not_pass("replaced Wiki commit", &receipt);

    // A replacement that would be over the read bound must not push the small
    // pinned page out of the analysis: the pinned object is still the source.
    let limit = cadence_agent::memory::evidence::MAX_CITED_BYTES;
    let small_text = "# Bounded\n\nsmall page\n";
    let small = rig.commit_wiki("global/bounded.md", small_text);
    let huge = rig.commit_wiki("global/bounded.md", &"x".repeat(limit + 1));
    git(&rig.pm.dir, &["replace", &small, &huge]);
    assert_eq!(
        git(
            &rig.pm.dir,
            &["cat-file", "-s", &format!("{small}:wiki/global/bounded.md")]
        ),
        (limit + 1).to_string(),
        "the fixture's oversized replacement does not affect a plain size read"
    );
    let mem = memory(
        "replaced-wiki-bound",
        "Cites a small page whose object has an oversized replacement",
        vec![wiki_ref("global/bounded.md", &small)],
        Some(condition(Some(&small), None)),
    );
    let receipt = analyse_as(&rig.pm, &mem, &Caller::Public).unwrap();

    assert_eq!(
        receipt.outcome,
        Outcome::Pass,
        "a replacement ref pushed the pinned small page over the read bound \
         (checked {:?}, skipped {:?}, findings {:?})",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    let pinned_digest = format!("sha256 {}", sha256_hex(small_text.as_bytes()));
    assert!(
        receipt
            .checked
            .iter()
            .any(|entry| entry.contains(&pinned_digest)),
        "the analysis did not read the pinned small page: {:?}",
        receipt.checked
    );
    assert!(
        receipt.skipped.is_empty(),
        "a passing receipt skipped work: {:?}",
        receipt.skipped
    );
}

#[test]
fn an_unregistered_project_is_unsupported_not_a_missing_citation() {
    let rig = Rig::new();
    let reference = git_ref("evidence.txt", &rig.commit);
    let (resolution, content) =
        resolve(&rig.pm, "unregistered", &Caller::Public, &reference).unwrap();
    assert!(
        matches!(&resolution, Resolution::Unsupported(_)),
        "an unregistered project was not reported as unsupported: {resolution:?}"
    );
    assert!(
        content.is_none(),
        "an unregistered project returned bytes to scan"
    );

    let mut mem = memory(
        "unregistered-project",
        "Cites a Git source from a project that is not registered",
        vec![reference],
        Some(condition(Some(&rig.commit), Some(&rig.commit))),
    );
    mem.project = "unregistered".into();
    let receipt = analyse(&rig.pm, &mem).unwrap();

    assert_eq!(
        receipt.outcome,
        Outcome::Unknown,
        "an unavailable repository was not reported as unknown \
         (checked {:?}, skipped {:?}, findings {:?})",
        receipt.checked,
        receipt.skipped,
        receipt.findings
    );
    assert_eq!(receipt.to_json()["result"], "unknown");
    assert!(
        receipt.checked.is_empty(),
        "an unavailable repository was reported as checked: {:?}",
        receipt.checked
    );
    assert!(
        receipt.findings.is_empty(),
        "an unavailable repository was reported as a refusal: {:?}",
        receipt.findings
    );
    assert!(
        receipt
            .skipped
            .iter()
            .any(|entry| entry.contains("unregistered")),
        "the receipt did not name the unavailable project: {:?}",
        receipt.skipped
    );
    assert_not_pass("unregistered project", &receipt);
}
