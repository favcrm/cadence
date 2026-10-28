//! Evidence analysis for one memory record (CAD-348, non-privileged half).
//!
//! Resolves each structured citation at the immutable revision it names,
//! scans the resolved content for secrets, and reports same-scope duplicates
//! and the documented finite contradiction and overfit fixtures as a bounded receipt.
//!
//! Read-only by construction: nothing here launches a check, a runner or a
//! provider, and a missing or unsupported citation is recorded as such rather
//! than substituted with HEAD, live bytes or a near-match. The receipt is
//! evidence about the *record's citations*, not a trust decision: finalization
//! stays with the native policy path.

use serde_json::{json, Value};

use super::policy::{EvidenceKind, EvidenceRef};
use super::{semantic_digest, Memory};
use crate::error::Result;
use crate::issue::{project, Pm};

/// Largest cited blob this analysis will read. A bigger source is reported as
/// skipped, never partially scanned and called checked.
pub const MAX_CITED_BYTES: usize = 64 * 1024;

/// The documented contradiction fixture set is intentionally tiny: a body line
/// `contradicts: <slug>` naming another memory in the same project and scope.
/// This is a fixture, not general semantic contradiction detection.
pub const CONTRADICTION_PREFIX: &str = "contradicts:";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Revise,
    Blocked,
    Unknown,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Pass => "pass",
            Outcome::Revise => "revise",
            Outcome::Blocked => "blocked",
            Outcome::Unknown => "unknown",
        }
    }
}

/// One citation's resolution, kept separate from the aggregate outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// The cited bytes were read at the cited revision.
    Resolved { bytes: usize, sha256: String },
    /// The citation is valid data but its source cannot be resolved here.
    Missing(String),
    /// The citation kind needs a capability this read-only path does not have.
    Unsupported(String),
}

/// A bounded, machine-readable analysis receipt.
#[derive(Clone, Debug)]
pub struct Receipt {
    pub candidate_digest: String,
    pub check_id: String,
    pub source_revision: Option<String>,
    pub target_revision: Option<String>,
    pub wiki_target_revision: Option<String>,
    pub outcome: Outcome,
    pub checked: Vec<String>,
    pub skipped: Vec<String>,
    pub findings: Vec<String>,
}

impl Receipt {
    pub fn to_json(&self) -> Value {
        json!({
            "candidate_digest": self.candidate_digest,
            "check_id": self.check_id,
            "source_revision": self.source_revision,
            "target_revision": self.target_revision,
            "wiki_target_revision": self.wiki_target_revision,
            "result": self.outcome.as_str(),
            "checked": self.checked,
            "skipped": self.skipped,
            "findings": self.findings,
            "limits": {
                "max_cited_bytes": MAX_CITED_BYTES,
                "contradiction_fixture": format!("body line `{CONTRADICTION_PREFIX} <slug>`"),
                "overfit_fixture": "fact line `<environment> cron = <comma-separated minutes> was broken`",
                "semantic_detection": "not claimed",
            },
        })
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Read `path` at `revision` from the project's first registered repo.
fn git_blob(
    pm: &Pm,
    key: &str,
    source: &str,
    revision: &str,
) -> Result<(Resolution, Option<Vec<u8>>)> {
    let projects = project::list(&pm.dir)?;
    let Some(selected) = projects.iter().find(|p| p.key == key) else {
        return Ok((
            Resolution::Unsupported(format!("project '{key}' is not in the tracker")),
            None,
        ));
    };
    let Some(repo) = selected.repos.iter().find_map(|r| r.path.as_deref()) else {
        return Ok((
            Resolution::Unsupported(format!(
                "project '{key}' registers no repo to resolve Git evidence against"
            )),
            None,
        ));
    };
    let root = project::expand_home(repo);
    let revision = revision.strip_prefix("sha256:").unwrap_or(revision);
    if revision.len() != 40 || !revision.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok((
            Resolution::Missing("Git evidence must cite a 40-hex commit".into()),
            None,
        ));
    }
    let mut cmd = std::process::Command::new("git");
    cmd.args(["--no-replace-objects", "-C"])
        .arg(&root)
        .args(["cat-file", "-t", revision]);
    let commit = crate::reaper::output(&mut cmd)?;
    if !commit.status.success() || commit.stdout.trim_ascii() != b"commit" {
        return Ok((
            Resolution::Unsupported(format!("Git commit {revision} is unavailable")),
            None,
        ));
    }
    let spec = format!("{revision}:{source}");
    let mut cmd = std::process::Command::new("git");
    cmd.args(["--no-replace-objects", "-C"])
        .arg(&root)
        .args(["cat-file", "-s", &spec]);
    let size = crate::reaper::output(&mut cmd)?;
    if !size.status.success() {
        let mut cmd = std::process::Command::new("git");
        cmd.args(["--no-replace-objects", "-C"])
            .arg(&root)
            .args(["ls-tree", "-z", revision, "--", source]);
        let tree = crate::reaper::output(&mut cmd)?;
        let resolution = if tree.status.success() && tree.stdout.is_empty() {
            Resolution::Missing(format!("{source} does not exist at {revision}"))
        } else {
            Resolution::Unsupported(format!("Git source {source} cannot be read at {revision}"))
        };
        return Ok((resolution, None));
    }
    let size: usize = String::from_utf8_lossy(&size.stdout)
        .trim()
        .parse()
        .unwrap_or(usize::MAX);
    if size > MAX_CITED_BYTES {
        return Ok((
            Resolution::Unsupported(format!(
                "{source} at {revision} is {size} bytes, above the {MAX_CITED_BYTES}-byte read bound"
            )),
            None,
        ));
    }
    let mut cmd = std::process::Command::new("git");
    cmd.args(["--no-replace-objects", "-C"])
        .arg(&root)
        .args(["cat-file", "blob", &spec]);
    let blob = crate::reaper::output(&mut cmd)?;
    if !blob.status.success() {
        return Ok((
            Resolution::Unsupported(format!("Git source {source} cannot be read at {revision}")),
            None,
        ));
    }
    if blob.stdout.len() > MAX_CITED_BYTES {
        return Ok((
            Resolution::Unsupported(format!("{source} at {revision} exceeds the read bound")),
            None,
        ));
    }
    Ok((
        Resolution::Resolved {
            bytes: blob.stdout.len(),
            sha256: sha256_hex(&blob.stdout),
        },
        Some(blob.stdout),
    ))
}

/// Resolve one citation for `caller`. Never falls back to HEAD or live bytes.
pub fn resolve(
    pm: &Pm,
    key: &str,
    caller: &crate::wiki::Caller,
    reference: &EvidenceRef,
) -> Result<(Resolution, Option<Vec<u8>>)> {
    match reference.kind {
        EvidenceKind::Git => git_blob(pm, key, &reference.source, &reference.revision),
        EvidenceKind::Wiki => {
            match crate::wiki::read_revision_for_evidence(
                pm,
                caller,
                &reference.source,
                &reference.revision,
                MAX_CITED_BYTES,
            ) {
                Ok(Some(value)) => {
                    let sha256 = value
                        .get("sha256")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    if sha256.is_empty() {
                        return Ok((
                            Resolution::Missing(format!(
                                "wiki '{}' has no content-addressed original at {}",
                                reference.source, reference.revision
                            )),
                            None,
                        ));
                    }
                    let content = if let Some(text) = value.get("text").and_then(|v| v.as_str()) {
                        text.as_bytes().to_vec()
                    } else if let Some(encoded) = value.get("data_base64").and_then(|v| v.as_str())
                    {
                        use base64::Engine as _;
                        match base64::engine::general_purpose::STANDARD.decode(encoded) {
                            Ok(bytes) => bytes,
                            Err(_) => {
                                return Ok((
                                    Resolution::Unsupported(
                                        "wiki original could not be decoded".into(),
                                    ),
                                    None,
                                ))
                            }
                        }
                    } else {
                        return Ok((
                            Resolution::Unsupported("wiki original bytes are unavailable".into()),
                            None,
                        ));
                    };
                    Ok((
                        Resolution::Resolved {
                            bytes: content.len(),
                            sha256,
                        },
                        Some(content),
                    ))
                }
                Ok(None) => Ok((
                    Resolution::Missing(format!(
                        "wiki '{}' is absent at {}",
                        reference.source, reference.revision
                    )),
                    None,
                )),
                Err(error) => Ok((
                    Resolution::Unsupported(format!(
                        "wiki citation '{}' is not readable by this reader: {error}",
                        reference.source
                    )),
                    None,
                )),
            }
        }
        EvidenceKind::NativeRecord => Ok((
            Resolution::Unsupported("native records are not readable by path".into()),
            None,
        )),
        EvidenceKind::External => Ok((
            Resolution::Unsupported("external sources are not fetched by this analysis".into()),
            None,
        )),
    }
}

/// Scan resolved cited content. The rule and position are reported; the secret
/// itself never is.
pub fn scan_content(text: &str) -> Result<Option<String>> {
    match crate::secret::guard("memory evidence", text) {
        Ok(_) => Ok(None),
        Err(error) => Ok(Some(error.to_string())),
    }
}

fn normalised_body(mem: &Memory) -> String {
    mem.body
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn same_scope(a: &super::Scope, b: &super::Scope) -> bool {
    fn same_set(a: &[String], b: &[String]) -> bool {
        a.iter().all(|item| b.contains(item)) && b.iter().all(|item| a.contains(item))
    }
    a.project == b.project
        && (a.project
            || (same_set(&a.components, &b.components)
                && same_set(&a.paths, &b.paths)
                && same_set(&a.providers, &b.providers)
                && same_set(&a.tags, &b.tags)))
}

/// Same-project, same-scope duplicates by normalised body.
fn duplicate_of(pool: &[Memory], mem: &Memory) -> Option<String> {
    let mine = normalised_body(mem);
    if mine.is_empty() {
        return None;
    }
    pool.iter()
        .filter(|other| {
            other.front.id != mem.front.id
                && other.project == mem.project
                && same_scope(&other.front.scope, &mem.front.scope)
        })
        .find(|other| normalised_body(other) == mine)
        .map(|other| other.front.id.clone())
}

/// The documented bounded contradiction fixture: `contradicts: <slug>`.
fn contradiction_of(pool: &[Memory], mem: &Memory) -> Option<String> {
    for line in mem.body.lines() {
        let trimmed = line.trim();
        let lowered = trimmed.to_lowercase();
        let Some(rest) = lowered.strip_prefix(CONTRADICTION_PREFIX) else {
            continue;
        };
        let slug = rest.trim();
        if slug.is_empty() {
            continue;
        }
        if let Some(other) = pool.iter().find(|other| {
            other.front.id == slug
                && other.project == mem.project
                && same_scope(&other.front.scope, &mem.front.scope)
        }) {
            return Some(other.front.id.clone());
        }
    }
    None
}
/// Bounded CAD-808 fixture: an incident answer presented as the fact itself.
/// This deliberately does not inspect cited sources or claim general detection.
fn overfit_cron_incident(body: &str) -> bool {
    fn incident_line(fact: &str) -> bool {
        let mut words = fact.split_ascii_whitespace();
        let (Some(_environment), Some(cron), Some(equals), Some(minutes), Some(was), Some(broken)) = (
            words.next(),
            words.next(),
            words.next(),
            words.next(),
            words.next(),
            words.next(),
        ) else {
            return false;
        };
        if words.next().is_some()
            || !cron.eq_ignore_ascii_case("cron")
            || equals != "="
            || !was.eq_ignore_ascii_case("was")
            || !broken.trim_end_matches('.').eq_ignore_ascii_case("broken")
        {
            return false;
        }
        let mut count = 0;
        for minute in minutes.split(',') {
            if minute.is_empty() || !minute.bytes().all(|b| b.is_ascii_digit()) {
                return false;
            }
            if !minute.parse::<u8>().is_ok_and(|value| value < 60) {
                return false;
            }
            count += 1;
        }
        count >= 2
    }
    body.lines()
        .take_while(|line| {
            let line = line.trim();
            !line.starts_with("**Why:**") && !line.starts_with("**How to apply:**")
        })
        .any(incident_line)
}

/// Analyse one record's citations and content. Read-only.
pub fn analyse(pm: &Pm, mem: &Memory) -> Result<Receipt> {
    analyse_as(pm, mem, &crate::wiki::Caller::Public)
}

/// Analyse with an explicit reader, so a Wiki citation resolves exactly as the
/// reader itself could read it and never gains access it did not already have.
pub fn analyse_as(pm: &Pm, mem: &Memory, caller: &crate::wiki::Caller) -> Result<Receipt> {
    let policy = mem.front.policy.as_ref();
    let condition = policy.and_then(|p| p.verification_condition.as_ref());
    let mut refs = policy
        .into_iter()
        .flat_map(|p| {
            p.evidence_refs
                .iter()
                .chain(p.attestations.iter().flat_map(|a| a.evidence_refs.iter()))
        })
        .peekable();

    let mut checked = Vec::new();
    let mut skipped = Vec::new();
    let mut findings = Vec::new();
    let mut blocked = false;
    let mut revise = false;
    if refs.peek().is_none() {
        skipped.push("no evidence citations recorded".into());
    }

    for reference in refs {
        let reference_finding = match scan_content(&reference.source)? {
            Some(finding) => Some(finding),
            None => scan_content(&reference.revision)?,
        };
        if let Some(finding) = reference_finding {
            blocked = true;
            findings.push(format!("citation reference was refused: {finding}"));
            continue;
        }
        // Admission may allow warning-only or operator-allowlisted matches;
        // those values still must not be displayed in a receipt.
        let display_source = crate::secret::redact_text(&reference.source)?;
        let display_revision = crate::secret::redact_text(&reference.revision)?;
        let (resolution, content) = resolve(pm, &mem.project, caller, reference)?;
        match resolution {
            Resolution::Resolved { bytes, sha256 } => {
                checked.push(format!(
                    "{}:{}@{} ({} bytes, sha256 {})",
                    match reference.kind {
                        EvidenceKind::Git => "git",
                        EvidenceKind::Wiki => "wiki",
                        EvidenceKind::NativeRecord => "native",
                        EvidenceKind::External => "external",
                    },
                    display_source,
                    display_revision,
                    bytes,
                    sha256
                ));
                match content {
                    Some(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        if let Some(finding) = scan_content(&text)? {
                            blocked = true;
                            findings.push(format!(
                                "cited content in {} at {} was refused: {finding}",
                                display_source, display_revision
                            ));
                        }
                        if matches!(text, std::borrow::Cow::Owned(_)) {
                            skipped.push(format!(
                                "content scan incomplete for {}: source is not UTF-8",
                                display_source
                            ));
                        }
                    }
                    None => skipped.push(format!(
                        "content scan skipped for {}: original bytes are unavailable",
                        display_source
                    )),
                }
                // Applicability is a separate check at the target the condition
                // pins, never an assumption that the source still describes it.
                if let Some(target) = condition.and_then(|c| c.target_revision.as_deref()) {
                    if reference.kind == EvidenceKind::Git && target != reference.revision {
                        match git_blob(pm, &mem.project, &reference.source, target)? {
                            (
                                Resolution::Resolved {
                                    sha256: at_target, ..
                                },
                                _,
                            ) if at_target == sha256 => {
                                checked.push(format!(
                                    "{} is unchanged at target {target}",
                                    display_source
                                ));
                            }
                            (Resolution::Resolved { .. }, _) => {
                                revise = true;
                                findings.push(format!(
                                    "{} differs between cited revision {} and target {target}",
                                    display_source, display_revision
                                ));
                            }
                            (Resolution::Missing(_), _) => {
                                revise = true;
                                findings.push(format!(
                                    "{} is absent at target {target}",
                                    display_source
                                ));
                            }
                            (Resolution::Unsupported(reason), _) => {
                                skipped.push(format!("target check skipped: {reason}"));
                            }
                        }
                    }
                }
                if reference.kind == EvidenceKind::Wiki {
                    if let Some(target) = condition.and_then(|c| c.wiki_target_revision.as_deref())
                    {
                        if target == reference.revision {
                            checked.push(format!(
                                "{} is unchanged at Wiki target {target}",
                                display_source
                            ));
                        } else {
                            match crate::wiki::read_revision_for_evidence(
                                pm,
                                caller,
                                &reference.source,
                                target,
                                MAX_CITED_BYTES,
                            ) {
                                Ok(Some(value)) => {
                                    match value.get("sha256").and_then(|v| v.as_str()) {
                                        Some(digest) if digest == sha256 => {
                                            checked.push(format!(
                                                "{} is unchanged at Wiki target {target}",
                                                display_source
                                            ));
                                        }
                                        Some(_) => {
                                            revise = true;
                                            findings.push(format!(
                                            "{} differs between cited revision {} and Wiki target {target}",
                                            display_source, display_revision
                                        ));
                                        }
                                        None => skipped.push(format!(
                                            "Wiki target check skipped for {}: no content digest",
                                            display_source
                                        )),
                                    }
                                }
                                Ok(None) => {
                                    revise = true;
                                    findings.push(format!(
                                        "{} is absent at Wiki target {target}",
                                        display_source
                                    ));
                                }
                                Err(error) => skipped.push(format!(
                                    "Wiki target check skipped for {}: {error}",
                                    display_source
                                )),
                            }
                        }
                    } else if condition
                        .and_then(|c| c.target_revision.as_deref())
                        .is_some()
                    {
                        skipped.push(format!(
                            "target applicability skipped for {}: no Wiki target revision recorded",
                            display_source
                        ));
                    }
                }
            }
            Resolution::Missing(reason) => {
                blocked = true;
                findings.push(format!(
                    "{} citation {}@{} is missing: {reason}",
                    match reference.kind {
                        EvidenceKind::Git => "git",
                        EvidenceKind::Wiki => "wiki",
                        EvidenceKind::NativeRecord => "native",
                        EvidenceKind::External => "external",
                    },
                    display_source,
                    display_revision
                ));
            }
            Resolution::Unsupported(reason) => {
                skipped.push(format!("{}: {reason}", display_source));
            }
        }
    }

    let (pool, errors) = super::load_project_report(&pm.dir, &mem.project);
    if !errors.is_empty() {
        let project = crate::secret::redact_text(&mem.project)?;
        skipped.push(format!(
            "duplicate and contradiction checks incomplete: memories in project {project} could not be read"
        ));
    }
    if let Some(slug) = duplicate_of(&pool, mem) {
        revise = true;
        findings.push(format!(
            "same project and scope already carries an identical claim: {slug}"
        ));
    }
    if let Some(slug) = contradiction_of(&pool, mem) {
        revise = true;
        findings.push(format!(
            "body names {slug} as contradicted in the same project and scope"
        ));
    }
    if overfit_cron_incident(&mem.body) {
        revise = true;
        findings.push("fact states an incident-specific cron answer; check whether the scheduled job actually fired instead".into());
    }

    let outcome = if blocked {
        Outcome::Blocked
    } else if revise {
        Outcome::Revise
    } else if condition.is_none() || !skipped.is_empty() {
        if condition.is_none() {
            skipped.push("no verification condition recorded".into());
        }
        Outcome::Unknown
    } else {
        Outcome::Pass
    };
    // Reasons and identifiers can include other user-controlled text too.
    // Redact at the output boundary independently of the operator allowlist.
    for text in checked.iter_mut().chain(&mut skipped).chain(&mut findings) {
        *text = crate::secret::redact_text(text)?;
    }
    let display = |value: Option<&str>| -> Result<Option<String>> {
        value.map(crate::secret::redact_text).transpose()
    };

    Ok(Receipt {
        candidate_digest: semantic_digest(mem),
        check_id: crate::secret::redact_text(
            condition
                .map(|c| c.check_id.as_str())
                .unwrap_or("evidence-analysis"),
        )?,
        source_revision: display(condition.and_then(|c| c.source_revision.as_deref()))?,
        target_revision: display(condition.and_then(|c| c.target_revision.as_deref()))?,
        wiki_target_revision: display(condition.and_then(|c| c.wiki_target_revision.as_deref()))?,
        outcome,
        checked,
        skipped,
        findings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::Pm;

    fn project_with_repo(pm: &Pm, repo: &std::path::Path) {
        let dir = pm.dir.join("demo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("project.yaml"),
            format!(
                "key: demo\nprefix: DEMO\nrepos:\n  - path: {}\n",
                repo.display()
            ),
        )
        .unwrap();
    }

    fn memory(id: &str, body: &str, refs: Vec<EvidenceRef>) -> Memory {
        Memory {
            project: "demo".into(),
            front: crate::memory::Front {
                id: id.into(),
                kind: "rule".into(),
                status: "proposed".into(),
                scope: crate::memory::Scope::default(),
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
                policy: Some(crate::memory::policy::PolicyV2 {
                    revision: 2,
                    nature: crate::memory::policy::Nature::Judgement,
                    provenance: crate::memory::policy::Provenance::Operator,
                    audience: crate::memory::policy::Audience::Project,
                    roles: Vec::new(),
                    evidence_refs: refs,
                    verification_condition: None,
                    author: crate::memory::policy::Authorship {
                        actor: crate::memory::policy::ActorSnapshot::Operator { uid: 1000 },
                        model: None,
                    },
                    contributors: Vec::new(),
                    attestations: Vec::new(),
                    reviews: Vec::new(),
                    finalizations: Vec::new(),
                    feedback: Default::default(),
                }),
            },
            body: body.into(),
            path: std::path::PathBuf::from("/nonexistent"),
        }
    }

    #[test]
    fn a_missing_git_citation_blocks_without_reading_head() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        crate::issue::git(&repo, &["init", "-q"]).unwrap();
        std::fs::write(repo.join("evidence.txt"), "original\n").unwrap();
        crate::issue::git(&repo, &["add", "--", "evidence.txt"]).unwrap();
        crate::issue::git(
            &repo,
            &[
                "-c",
                "user.name=CAD-348 fixture",
                "-c",
                "user.email=cad348@fixture.invalid",
                "commit",
                "-q",
                "-m",
                "evidence",
            ],
        )
        .unwrap();
        let head = crate::issue::git(&repo, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();

        let pm = Pm::init(&root.path().join("pm")).unwrap();
        project_with_repo(&pm, &repo);
        let good = memory(
            "cited",
            "A claim",
            vec![EvidenceRef {
                kind: EvidenceKind::Git,
                source: "evidence.txt".into(),
                revision: head.clone(),
            }],
        );
        let receipt = analyse(&pm, &good).unwrap();
        assert_eq!(
            receipt.outcome,
            Outcome::Unknown,
            "condition-less is unknown"
        );
        assert_eq!(receipt.checked.len(), 1, "the cited blob resolved");
        assert!(receipt.findings.is_empty(), "{:?}", receipt.findings);

        let absent = memory(
            "absent",
            "Another claim",
            vec![EvidenceRef {
                kind: EvidenceKind::Git,
                source: "not-there.txt".into(),
                revision: head,
            }],
        );
        let receipt = analyse(&pm, &absent).unwrap();
        assert_eq!(receipt.outcome, Outcome::Blocked);
        assert!(receipt.findings[0].contains("is missing"));
    }

    #[test]
    fn secret_bearing_cited_content_blocks_and_is_not_echoed() {
        // Assembled at runtime: a literal credential shape in the source tree
        // trips the repository secret scan, which must stay clean.
        let token = format!("figd_{}", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dE3fG5h");
        let text = format!("token: {token}\n");
        let finding = scan_content(&text).unwrap();
        assert!(finding.is_some(), "a secret must be refused");
        let reported = finding.unwrap();
        assert!(
            !reported.contains(&token),
            "the secret was echoed: {reported}"
        );
    }

    #[test]
    fn same_scope_duplicates_revise_and_different_scope_does_not() {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let mut a = memory("a", "Use the pinned digest", Vec::new());
        a.front.scope = crate::memory::Scope {
            project: true,
            ..Default::default()
        };
        let mut b = memory("b", "use the   pinned digest", Vec::new());
        b.front.scope = crate::memory::Scope {
            project: true,
            ..Default::default()
        };
        let receipt = analyse(&pm, &a).unwrap();
        assert_eq!(receipt.outcome, Outcome::Unknown);
        let mut pool = vec![a.clone(), b.clone()];
        assert_eq!(duplicate_of(&pool, &a).as_deref(), Some("b"));
        b.front.scope = crate::memory::Scope {
            project: false,
            ..Default::default()
        };
        pool[1] = b.clone();
        assert_eq!(
            duplicate_of(&pool, &a),
            None,
            "different scope is not a duplicate"
        );
    }

    fn persist(pm: &Pm, mem: &Memory) {
        let dir = pm.dir.join(&mem.project).join("memory");
        std::fs::create_dir_all(&dir).unwrap();
        let yaml = serde_yaml::to_string(&mem.front).unwrap();
        std::fs::write(
            dir.join(format!("{}.md", mem.front.id)),
            format!("---\n{yaml}---\n\n{}", mem.body),
        )
        .unwrap();
    }

    #[test]
    fn the_documented_contradiction_fixture_revises() {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let target = memory("target", "Original guidance", Vec::new());
        let citing = memory("citing", "New guidance\ncontradicts: target\n", Vec::new());
        persist(&pm, &target);
        persist(&pm, &citing);
        let (loaded, errors) = crate::memory::load_project_report(&pm.dir, "demo");
        assert_eq!(loaded.len(), 2, "loader errors: {errors:?}");
        let receipt = analyse(&pm, &citing).unwrap();
        assert_eq!(receipt.outcome, Outcome::Revise, "{:?}", receipt.findings);
        assert!(receipt.to_json()["limits"]["semantic_detection"]
            .as_str()
            .unwrap()
            .contains("not claimed"));
    }

    fn repo_with_two_revisions() -> (tempfile::TempDir, std::path::PathBuf, String, String) {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        crate::issue::git(&repo, &["init", "-q"]).unwrap();
        std::fs::write(repo.join("evidence.txt"), "original\n").unwrap();
        crate::issue::git(&repo, &["add", "--", "evidence.txt"]).unwrap();
        crate::issue::git(
            &repo,
            &[
                "-c",
                "user.name=CAD-348 fixture",
                "-c",
                "user.email=cad348@fixture.invalid",
                "commit",
                "-q",
                "-m",
                "source",
            ],
        )
        .unwrap();
        let source = crate::issue::git(&repo, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();
        std::fs::write(repo.join("evidence.txt"), "changed at the target\n").unwrap();
        crate::issue::git(&repo, &["add", "--", "evidence.txt"]).unwrap();
        crate::issue::git(
            &repo,
            &[
                "-c",
                "user.name=CAD-348 fixture",
                "-c",
                "user.email=cad348@fixture.invalid",
                "commit",
                "-q",
                "-m",
                "target",
            ],
        )
        .unwrap();
        let target = crate::issue::git(&repo, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();
        (root, repo, source, target)
    }

    fn with_condition(mut mem: Memory, source: &str, target: &str) -> Memory {
        let policy = mem.front.policy.as_mut().unwrap();
        policy.verification_condition = Some(crate::memory::policy::VerificationCondition {
            check_id: "evidence-analysis".into(),
            source_revision: Some(source.into()),
            target_revision: Some(target.into()),
            wiki_target_revision: None,
        });
        mem
    }

    #[test]
    fn bounded_overfit_fixture_distinguishes_incident_answers_from_guidance() {
        let (root, repo, source, _) = repo_with_two_revisions();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        project_with_repo(&pm, &repo);
        let reference = EvidenceRef {
            kind: EvidenceKind::Git,
            source: "evidence.txt".into(),
            revision: source.clone(),
        };
        let check = |id: &str, body: &str| {
            let mem = with_condition(memory(id, body, vec![reference.clone()]), &source, &source);
            analyse(&pm, &mem).unwrap()
        };

        let incident = check("incident", "staging cron = 7,22,37,52 was broken");
        assert_eq!(incident.outcome, Outcome::Revise, "{:?}", incident.findings);
        assert!(incident.findings.iter().any(|f| f.contains("incident")));
        let multiline = check(
            "multiline",
            "An incident was recorded.\nstaging cron = 7,22,37,52 was broken\n\n**Why:** The job missed a run.",
        );
        assert_eq!(
            multiline.outcome,
            Outcome::Revise,
            "{:?}",
            multiline.findings
        );

        let general = check(
            "general",
            "An active schedule does not prove a job fired.\n\n**Why:** CAD-808 cites staging cron = 7,22,37,52 as evidence.\n\n**How to apply:** Check the actual job execution record.",
        );
        assert_eq!(general.outcome, Outcome::Pass, "{:?}", general.findings);

        let scoped = check(
            "scoped",
            "For scheduler v2.4, use cron = 7,22,37,52 when the documented deployment scope requires it.",
        );
        assert_eq!(scoped.outcome, Outcome::Pass, "{:?}", scoped.findings);
    }

    #[test]
    fn applicability_is_checked_at_the_pinned_target() {
        let (root, repo, source, target) = repo_with_two_revisions();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        project_with_repo(&pm, &repo);

        let same = with_condition(
            memory(
                "same",
                "Cites a source that did not change",
                vec![EvidenceRef {
                    kind: EvidenceKind::Git,
                    source: "evidence.txt".into(),
                    revision: source.clone(),
                }],
            ),
            &source,
            &source,
        );
        let receipt = analyse(&pm, &same).unwrap();
        assert_eq!(receipt.outcome, Outcome::Pass, "{:?}", receipt.findings);

        let moved = with_condition(
            memory(
                "moved",
                "Cites a source that changed at the target",
                vec![EvidenceRef {
                    kind: EvidenceKind::Git,
                    source: "evidence.txt".into(),
                    revision: source.clone(),
                }],
            ),
            &source,
            &target,
        );
        let receipt = analyse(&pm, &moved).unwrap();
        assert_eq!(receipt.outcome, Outcome::Revise, "{:?}", receipt.findings);
        assert!(
            receipt
                .findings
                .iter()
                .any(|f| f.contains("differs between cited revision")),
            "{:?}",
            receipt.findings
        );
    }

    #[test]
    fn an_unreadable_wiki_citation_is_recorded_not_guessed() {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let citing = memory(
            "wiki",
            "Cites a wiki page this reader cannot open",
            vec![EvidenceRef {
                kind: EvidenceKind::Wiki,
                source: "projects/demo/policy.md".into(),
                revision: "fnv1a:0123456789abcdef".into(),
            }],
        );
        let receipt = analyse_as(&pm, &citing, &crate::wiki::Caller::Public).unwrap();
        assert!(
            receipt.skipped.iter().any(|s| s.contains("wiki")),
            "{:?}",
            receipt.skipped
        );
        assert!(receipt.checked.is_empty());
        assert_ne!(
            receipt.outcome,
            Outcome::Pass,
            "an unresolved citation is not a pass"
        );
    }
}
