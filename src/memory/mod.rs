//! Project memory — reviewed, scoped facts shared across agents.
//!
//! One fact per file at `<pm>/<project>/memory/<slug>.md`: YAML
//! frontmatter plus a body holding the fact (≤5 lines), a `**Why:**`
//! line and a `**How to apply:**` line.
//!
//! Authority-bearing writes go through daemon RPC. The daemon resolves the
//! Unix peer to one live, owned native PTY endpoint and supplies the
//! authenticated proposer/reviewer proof. Two distinct non-author PM/worker
//! endpoint identities review the same semantic digest; an authenticated PM
//! endpoint finalizes. Legacy records remain readable but are blocked from
//! retrieval until a corrected native proposal is reviewed.
//!
//! Only accepted memories with a valid quorum match a dispatch:
//! `scope.project` or any of components/path-globs/tags/providers intersecting
//! the dispatch context. `cadence dispatch` renders the top matches into a
//! lessons file and names it in the kickoff; the briefing lists eligible
//! project-wide `rule`s.

pub mod cli;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::issue::{board, git, history, parse, project, time, write, Pm};
use crate::proc;

pub const TYPES: &[&str] = &["rule", "gotcha", "decision", "recipe"];
pub const STATUSES: &[&str] = &["proposed", "accepted", "rejected", "superseded"];
pub const CONFIDENCES: &[&str] = &["low", "medium", "high"];

/// The small, daemon-issued identity proof that memory records retain.
///
/// `alias` is useful to a human reading a record, but it is not the
/// identity key.  The persisted registration discriminator stays stable
/// across a resume, while endpoint generation and process start are retained
/// as live provenance; a later registration of the alias cannot inherit its
/// reviews.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct IdentityProof {
    pub alias: String,
    /// Registration discriminator from the daemon's persisted Agent row.
    /// It survives endpoint resumes but changes when an alias is removed
    /// and registered again.
    pub registration: u64,
    pub generation: String,
    pub process_start: u64,
    pub role: String,
}

impl IdentityProof {
    pub fn stable_id(&self) -> String {
        format!("{}#{}", self.alias, self.registration)
    }
}

fn valid_identity_proof(proof: &IdentityProof) -> bool {
    !proof.alias.is_empty()
        && proof.registration != 0
        && !proof.generation.is_empty()
        && proof.process_start != 0
        && matches!(proof.role.as_str(), "pm" | "worker")
}

/// An immutable review receipt.  Receipts are retained in the memory file;
/// lifecycle status and timestamps do not enter the semantic revision digest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewReceipt {
    pub reviewer: String,
    pub identity: String,
    pub generation: String,
    pub process_start: u64,
    pub role: String,
    pub operation: String,
    pub cycle: u64,
    pub digest: String,
    pub verdict: String,
    pub evidence: String,
    pub recorded_at: String,
}

/// Durable PM finalization receipt.  A review quorum is only eligible for
/// retrieval after the PM records this receipt; reviewer receipts alone are
/// never an acceptance or revalidation decision.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinalizationReceipt {
    pub operation: String,
    pub cycle: u64,
    pub digest: String,
    pub finalizer: IdentityProof,
    pub finalized_at: String,
}

/// Identity resolved by the daemon from a Unix socket peer.  It is kept in
/// this module so every write path consumes the same proof shape and no CLI
/// or HTTP request can construct one from claimed fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeIdentity {
    pub proof: IdentityProof,
}

impl NativeIdentity {
    pub fn stable_id(&self) -> String {
        self.proof.stable_id()
    }
}

/// Dispatch injection caps — the lessons file stays a quick scan.
pub const LESSON_MAX_ENTRIES: usize = 12;
pub const LESSON_MAX_BYTES: usize = 4 * 1024;

/// `scope:` — every axis is optional; an empty scope matches only via
/// `project: true` (which is itself one of the axes).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Scope {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<String>,
    /// Globs relative to a project repo root (`src/adapter/**`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Project-wide: matches every dispatch in the project.
    #[serde(default, skip_serializing_if = "is_false")]
    pub project: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Memory file frontmatter. `id` is the slug = filename stem.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Front {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub status: String,
    #[serde(default)]
    pub scope: Scope,
    /// Issue id, note path or commit this fact was learned from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub confidence: String,
    pub created: String,
    /// Last time a human/PM re-checked the fact against reality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<String>,
    /// Slug this memory replaces (set on the new file by `supersede`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    /// CADENCE_ALIAS of the proposer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Daemon-authenticated proposer proof.  Legacy `author` strings are
    /// intentionally not upgraded into this field when they are loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_proof: Option<IdentityProof>,
    /// Authenticated contributors included in the semantic revision.  The
    /// current CLI does not add contributors, but retaining the field makes
    /// imported records explicit rather than silently trusted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contributors: Vec<IdentityProof>,
    /// Current review cycle.  A new semantic revision starts a new cycle;
    /// verify/revalidation cycles are distinct from initial acceptance.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub review_cycle: u64,
    /// The review operation currently collecting receipts.  It is
    /// cleared only by an authenticated PM finalization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_operation: Option<String>,
    /// Immutable review history.  Empty on legacy records, which therefore
    /// remain visible but can never satisfy the quorum.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviews: Vec<ReviewReceipt>,
    /// Immutable PM finalization history.  This is separate from the active
    /// cycle so retrieval survives status transitions and a later verify can
    /// start a genuinely new cycle.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub finalizations: Vec<FinalizationReceipt>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// A loaded memory file.
#[derive(Clone, Debug)]
pub struct Memory {
    pub project: String,
    pub front: Front,
    pub body: String,
    pub path: PathBuf,
}

/// Slug grammar: `[a-z0-9][a-z0-9-]{0,47}`, no trailing `-`.
pub fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 48
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.contains("--")
}

pub fn check_slug(slug: &str) -> Result<String> {
    if valid_slug(slug) {
        Ok(slug.to_string())
    } else {
        Err(Error::rejected(format!(
            "Invalid memory slug '{slug}' — 1-48 lowercase letters, digits or single hyphens"
        )))
    }
}

/// `<pm>/<key>/memory/` — created lazily by `propose`.
pub fn memory_dir(pm: &Pm, key: &str) -> PathBuf {
    pm.dir.join(key).join("memory")
}

/// `---\n<yaml>\n---\n<body>` — same fence grammar as issue files.
pub fn parse_memory(text: &str) -> Result<(Front, String)> {
    let (yaml, body) =
        parse::split_front(text).map_err(|e| Error::rejected(format!("memory {e}")))?;
    let front: Front = serde_yaml::from_str(yaml)
        .map_err(|e| Error::rejected(format!("memory frontmatter is not valid YAML: {e}")))?;
    Ok((front, body.to_string()))
}

/// The body contract: a fact block (≤5 non-empty lines) followed by
/// `**Why:**` and `**How to apply:**` markers. Returns
/// `(fact_lines, why, how)`; sections may span multiple lines.
fn body_parts(body: &str) -> (Vec<String>, String, String) {
    let mut fact = Vec::new();
    let mut why = Vec::new();
    let mut how = Vec::new();
    let mut section = 0; // 0 = fact, 1 = why, 2 = how
    for line in body.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("**Why:**") {
            section = 1;
            if !rest.trim().is_empty() {
                why.push(rest.trim().to_string());
            }
            continue;
        }
        if let Some(rest) = t.strip_prefix("**How to apply:**") {
            section = 2;
            if !rest.trim().is_empty() {
                how.push(rest.trim().to_string());
            }
            continue;
        }
        match section {
            0 => {
                if !t.is_empty() || !fact.is_empty() {
                    fact.push(line.to_string());
                }
            }
            1 => why.push(line.to_string()),
            _ => how.push(line.to_string()),
        }
    }
    let trim = |v: &mut Vec<String>| {
        while v.first().is_some_and(|l| l.trim().is_empty()) {
            v.remove(0);
        }
        while v.last().is_some_and(|l| l.trim().is_empty()) {
            v.pop();
        }
    };
    trim(&mut fact);
    trim(&mut why);
    trim(&mut how);
    (fact, why.join("\n"), how.join("\n"))
}

/// The one-line fact used in lessons files and list views.
pub fn fact_line(body: &str) -> String {
    body_parts(body).0.first().cloned().unwrap_or_default()
}

/// The first `**How to apply:**` line — briefings and lessons carry it.
pub fn apply_line(body: &str) -> String {
    let (_, _, how) = body_parts(body);
    how.lines().next().unwrap_or_default().trim().to_string()
}

/// Body-contract errors — shared by `propose`/`accept --edit` and lint.
/// One fact stays a fact — bounded in lines AND bytes so a stuffed
/// paragraph can't slip past the line cap.
pub const FACT_MAX_BYTES: usize = 512;

fn lint_body(slug: &str, body: &str, err: &mut dyn FnMut(String)) {
    let (fact, why, how) = body_parts(body);
    let fact_lines = fact.iter().filter(|l| !l.trim().is_empty()).count();
    if fact_lines == 0 {
        err(format!("{slug}: missing fact (the lines before **Why:**)"));
    } else if fact_lines > 5 {
        err(format!("{slug}: fact is {fact_lines} lines — the cap is 5"));
    }
    let fact_bytes: usize = fact.iter().map(|l| l.len()).sum();
    if fact_bytes > FACT_MAX_BYTES {
        err(format!(
            "{slug}: fact is {fact_bytes} bytes — the cap is {FACT_MAX_BYTES}"
        ));
    }
    if why.trim().is_empty() {
        err(format!("{slug}: missing '**Why:**' section"));
    }
    if how.trim().is_empty() {
        err(format!("{slug}: missing '**How to apply:**' section"));
    }
}

/// Read one memory file; skips (returns None) anything not a real file.
fn load_file(path: &Path, key: &str) -> Result<Option<Memory>> {
    if !board::is_real_file(path) {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)?;
    let (front, body) = parse_memory(&text)?;
    Ok(Some(Memory {
        project: key.to_string(),
        front,
        body,
        path: path.to_path_buf(),
    }))
}

/// Every memory in one project, sorted by slug. Strict: the first
/// load error fails the whole call — see `load_project_report` for
/// the keep-going variant.
pub fn load_project(pm_dir: &Path, key: &str) -> Result<Vec<Memory>> {
    let (out, errors) = load_project_report(pm_dir, key);
    match errors.into_iter().next() {
        Some(e) => Err(Error::rejected(format!("memory: {e}"))),
        None => Ok(out),
    }
}

/// Every memory across every project.
pub fn load_all(pm_dir: &Path) -> Result<Vec<Memory>> {
    let mut out = Vec::new();
    for p in project::list(pm_dir)? {
        out.extend(load_project(pm_dir, &p.key)?);
    }
    Ok(out)
}

/// `load_project` that keeps going past a broken file — returns the
/// good memories plus one `<project>/<file>: <error>` string per
/// failure, so callers can surface instead of swallowing them. An
/// absent memory dir is a valid empty store; a directory that cannot
/// be enumerated (permissions, a file in its place, …) is an error.
pub fn load_project_report(pm_dir: &Path, key: &str) -> (Vec<Memory>, Vec<String>) {
    let dir = pm_dir.join(key).join("memory");
    let (mut out, mut errors) = (Vec::new(), Vec::new());
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (out, errors),
        Err(e) => {
            errors.push(format!("{key}/memory: cannot list directory: {e}"));
            return (out, errors);
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                errors.push(format!("{key}/memory: directory entry unreadable: {e}"));
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".md") || name.starts_with('.') {
            continue;
        }
        match load_file(&entry.path(), key) {
            // Hand-edited scope globs skip write-time validation —
            // quarantine over-complex patterns here, before matching
            // could ever see them.
            Ok(Some(m)) => match m.front.scope.paths.iter().find(|p| glob_over_budget(p)) {
                Some(pat) => errors.push(format!(
                    "{key}/{name}: path scope '{pat}' is too complex — quarantined at load"
                )),
                None => out.push(m),
            },
            Ok(None) => {}
            Err(e) => errors.push(format!("{key}/{name}: {e}")),
        }
    }
    out.sort_by(|a, b| a.front.id.cmp(&b.front.id));
    (out, errors)
}

/// `load_all` in the same error-collecting shape.
pub fn load_all_report(pm_dir: &Path) -> (Vec<Memory>, Vec<String>) {
    let (mut out, mut errors) = (Vec::new(), Vec::new());
    match project::list(pm_dir) {
        Ok(projects) => {
            for p in projects {
                let (mems, errs) = load_project_report(pm_dir, &p.key);
                out.extend(mems);
                errors.extend(errs);
            }
        }
        Err(e) => errors.push(e.to_string()),
    }
    (out, errors)
}

/// One-line summary of load errors for stderr: `N memory file(s)
/// failed to load; first: <msg>` — None when clean.
pub fn load_errors_line(errors: &[String]) -> Option<String> {
    if errors.is_empty() {
        return None;
    }
    Some(format!(
        "memory: {} file(s) failed to load; first: {}",
        errors.len(),
        errors[0]
    ))
}

/// Resolve `<slug>` to `(project, memory)`. `--project` pins the
/// project; without it every project's memory dir is scanned and an
/// ambiguous or missing slug is an error.
pub fn find(pm: &Pm, flag: Option<&str>, slug: &str) -> Result<(project::Project, Memory)> {
    check_slug(slug)?;
    let projects = project::list(&pm.dir)?;
    let candidates: Vec<&project::Project> = match flag {
        Some(key) => vec![projects
            .iter()
            .find(|p| p.key == key)
            .ok_or_else(|| project::unknown_project(key, &pm.dir))?],
        None => projects.iter().collect(),
    };
    let mut hits = Vec::new();
    for p in candidates {
        if let Some(m) = load_file(&memory_dir(pm, &p.key).join(format!("{slug}.md")), &p.key)? {
            hits.push((p.clone(), m));
        }
    }
    match hits.len() {
        0 => Err(Error::rejected(format!(
            "Unknown memory '{slug}' — `cadence memory ls` lists what exists"
        ))),
        1 => Ok(hits.pop().unwrap()),
        _ => Err(Error::rejected(format!(
            "'{slug}' exists in several projects — pass --project"
        ))),
    }
}

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
    let bytes = serde_json::to_vec(&revision).expect("memory semantic revision is serializable");
    format!("{:x}", Sha256::digest(bytes))
}

/// Return the exact current review cycle for an operation. A cycle is only
/// live while its operation is recorded in `active_operation`; finalization
/// clears that marker so an old quorum cannot be submitted twice.
fn operation_cycle(front: &Front, operation: &str) -> Option<u64> {
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

fn proposal_identity_matches(front: &Front, identity: &IdentityProof) -> bool {
    front
        .author_proof
        .as_ref()
        .is_some_and(|author| same_identity_or_alias(author, identity))
        || front
            .contributors
            .iter()
            .any(|contributor| same_identity_or_alias(contributor, identity))
}

fn next_verify_cycle(front: &Front) -> u64 {
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
    let contributor_ids: std::collections::HashSet<String> = mem
        .front
        .contributors
        .iter()
        .map(IdentityProof::stable_id)
        .collect();
    let contributor_aliases: std::collections::HashSet<String> = mem
        .front
        .contributors
        .iter()
        .map(|c| c.alias.clone())
        .collect();
    let mut identities = std::collections::HashSet::new();
    let mut aliases = std::collections::HashSet::new();
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

/// One tracker commit for a memory write: subject plus `Memory:` and
/// `Actor:` trailers — deliberately no `Issue:` trailer, memory writes
/// are not issue writes. Returns whether a commit object was actually
/// created — `Pm::commit` no-ops on an empty staged diff (e.g. a
/// verify that re-stamps the same second), which callers report
/// honestly as `committed: false`.
fn commit_mem(pm: &Pm, slug: &str, subject: &str, actor: &str) -> Result<bool> {
    let who = write::actor_who(actor, None);
    let before = git(&pm.dir, &["rev-parse", "HEAD"]).unwrap_or_default();
    pm.commit(&format!("{subject}\n\nMemory: {slug}\nActor: {who}\n"))?;
    let after = git(&pm.dir, &["rev-parse", "HEAD"]).unwrap_or_default();
    Ok(!before.is_empty() && before != after)
}

fn save_mem(mem: &Memory) -> Result<()> {
    let tmp = mem.path.with_extension("md.tmp");
    std::fs::write(&tmp, render_memory(&mem.front, &mem.body)?)?;
    std::fs::rename(&tmp, &mem.path)?;
    Ok(())
}

/// Render memory files without normalizing their Markdown body. The generic
/// issue renderer intentionally removes one leading newline for issue-file
/// ergonomics, but memory bodies are part of the semantic digest and must
/// round-trip byte-for-byte through a review write.
fn render_memory(front: &impl Serialize, body: &str) -> Result<String> {
    let yaml = serde_yaml::to_string(front)
        .map_err(|e| Error::internal(format!("cannot serialise memory frontmatter: {e}")))?;
    Ok(format!("---\n{yaml}---\n\n{body}"))
}

/// `**` recursion in glob_match is exponential on adversarial
/// patterns (`**a**a**a**`) — the bound a path scope may carry,
/// enforced at write time (`check_front`) and re-applied at load
/// (`load_project_report`) so a hand-edited file never reaches
/// matching with an unbounded pattern.
fn glob_over_budget(pat: &str) -> bool {
    pat.len() > 200 || pat.matches("**").count() > 2
}

/// Frontmatter + body validation used by every write and by lint.
/// `components` is the project's declared list (empty = anything goes).
fn check_front(mem: &Memory, components: &[String]) -> Result<()> {
    let f = &mem.front;
    if f.id != mem.path.file_stem().unwrap_or_default().to_string_lossy() {
        return Err(Error::rejected(format!(
            "memory id '{}' must match its filename",
            f.id
        )));
    }
    check_slug(&f.id)?;
    if !TYPES.contains(&f.kind.as_str()) {
        return Err(Error::rejected(format!(
            "Unknown memory type '{}' — one of {}",
            f.kind,
            TYPES.join(" ")
        )));
    }
    if !STATUSES.contains(&f.status.as_str()) {
        return Err(Error::rejected(format!(
            "Unknown memory status '{}' — one of {}",
            f.status,
            STATUSES.join(" ")
        )));
    }
    if !CONFIDENCES.contains(&f.confidence.as_str()) {
        return Err(Error::rejected(format!(
            "Unknown confidence '{}' — one of {}",
            f.confidence,
            CONFIDENCES.join(" ")
        )));
    }
    for c in &f.scope.components {
        if !components.is_empty() && !components.iter().any(|d| d == c) {
            return Err(Error::rejected(format!(
                "Unknown component '{c}' — {} declares: {}",
                mem.project,
                components.join(", ")
            )));
        }
    }
    for pat in &f.scope.paths {
        if glob_over_budget(pat) {
            return Err(Error::rejected(format!(
                "path scope '{pat}' is too complex — ≤200 chars, ≤2 `**` segments"
            )));
        }
    }
    let mut errs = Vec::new();
    lint_body(&f.id, &mem.body, &mut |e| errs.push(e));
    if let Some(first) = errs.into_iter().next() {
        return Err(Error::rejected(first));
    }
    Ok(())
}

/// Build and persist a proposal from content supplied by the authenticated
/// daemon caller. The caller supplies raw file content rather than a path so
/// the daemon, not a client-controlled path, owns the write decision.
#[allow(clippy::too_many_arguments)]
pub fn propose_native(
    pm: &Pm,
    key: &str,
    kind: &str,
    scope: &Scope,
    source: Option<&str>,
    confidence: Option<&str>,
    from_text: Option<&str>,
    text: Option<&str>,
    slug: Option<&str>,
    actor: &NativeIdentity,
) -> Result<Value> {
    if !valid_identity_proof(&actor.proof) {
        return Err(Error::rejected("native proposer identity is incomplete"));
    }
    let projects = project::list(&pm.dir)?;
    let proj = projects
        .iter()
        .find(|p| p.key == key)
        .ok_or_else(|| project::unknown_project(key, &pm.dir))?;
    let (mut front, body) = match (from_text, text) {
        (Some(raw), None) => match parse_memory(raw) {
            Ok((f, b)) => (Some(f), b),
            Err(_) => (None, raw.to_string()),
        },
        (None, Some(t)) => (None, t.to_string()),
        (None, None) => {
            return Err(Error::rejected(
                "propose needs content — text or authenticated file content",
            ))
        }
        (Some(_), Some(_)) => return Err(Error::rejected("propose takes one content source")),
    };
    let slug = match slug {
        Some(s) => check_slug(s)?,
        None => {
            let base = fact_line(&body);
            let derived: String = crate::issue::start::slugify(&base)
                .split('-')
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join("-");
            check_slug(if derived.is_empty() || derived == "work" {
                "lesson"
            } else {
                &derived
            })?
        }
    };
    if front
        .as_ref()
        .is_some_and(|candidate| !candidate.contributors.is_empty())
    {
        return Err(Error::rejected(
            "authenticated multi-contributor proposals are unsupported; submit one native author proposal and review it independently",
        ));
    }
    let now = time::iso(time::now_epoch());
    let mut front = front.take().unwrap_or_else(|| Front {
        id: slug.clone(),
        kind: kind.to_string(),
        status: "proposed".to_string(),
        scope: scope.clone(),
        source: source.map(str::to_string),
        confidence: confidence.unwrap_or("medium").to_string(),
        created: now.clone(),
        verified_at: None,
        supersedes: None,
        author: None,
        author_proof: None,
        contributors: Vec::new(),
        review_cycle: 0,
        active_operation: None,
        reviews: Vec::new(),
        finalizations: Vec::new(),
    });
    front.id = slug.clone();
    front.status = "proposed".to_string();
    front.verified_at = None;
    front.author = Some(actor.proof.alias.clone());
    front.author_proof = Some(actor.proof.clone());
    front.contributors.clear();
    front.review_cycle = 0;
    front.active_operation = None;
    front.reviews.clear();
    front.finalizations.clear();
    let path = memory_dir(pm, key).join(format!("{slug}.md"));
    let mem = Memory {
        project: key.to_string(),
        front,
        body,
        path,
    };
    check_front(&mem, &proj.components)?;
    let _lock = pm.lock()?;
    if mem.path.exists() {
        return Err(Error::rejected(format!(
            "Memory '{slug}' already exists — use a new authenticated proposal"
        )));
    }
    std::fs::create_dir_all(memory_dir(pm, key))?;
    save_mem(&mem)?;
    let committed = commit_mem(
        pm,
        &slug,
        &format!("{key}/memory/{slug}: proposed"),
        &actor.proof.alias,
    )?;
    Ok(json!({
        "project": key,
        "slug": slug,
        "status": "proposed",
        "digest": semantic_digest(&mem),
        "quorum": {"eligible": false, "reason": "review required"},
        "path": mem.path,
        "committed": committed
    }))
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

pub fn submit_review(
    pm: &Pm,
    flag: Option<&str>,
    slug: &str,
    request: &ReviewRequest<'_>,
    actor: &NativeIdentity,
) -> Result<Value> {
    let operation = request.operation;
    let verdict = request.verdict;
    let evidence = request.evidence;
    let expected_digest = request.expected_digest;
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
    let (proj, _) = find(pm, flag, slug)?;
    let _lock = pm.lock()?;
    let (_, mut mem) = find(pm, Some(&proj.key), slug)?;
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
    save_mem(&mem)?;
    let committed = commit_mem(
        pm,
        slug,
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

/// PM-only finalization after two distinct non-author PM/worker receipts. A
/// PM does not become a third reviewer merely by accepting the result.
pub fn finalize_native(
    pm: &Pm,
    flag: Option<&str>,
    slug: &str,
    operation: &str,
    expected_digest: &str,
    actor: &NativeIdentity,
) -> Result<Value> {
    if !valid_identity_proof(&actor.proof) || actor.proof.role != "pm" {
        return Err(Error::rejected(
            "memory finalization is restricted to an authenticated PM endpoint",
        ));
    }
    if !matches!(operation, "accept" | "verify") {
        return Err(Error::rejected(
            "memory finalization operation is unsupported",
        ));
    }
    let (proj, _) = find(pm, flag, slug)?;
    let _lock = pm.lock()?;
    let (_, mut mem) = find(pm, Some(&proj.key), slug)?;
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
    mem.front.verified_at = Some(finalized_at.clone());
    mem.front.finalizations.push(FinalizationReceipt {
        operation: operation.to_string(),
        cycle,
        digest: digest.clone(),
        finalizer: actor.proof.clone(),
        finalized_at,
    });
    mem.front.active_operation = None;
    check_front(&mem, &proj.components)?;
    save_mem(&mem)?;
    let committed = commit_mem(
        pm,
        slug,
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
    actor: &NativeIdentity,
) -> Result<Value> {
    if !valid_identity_proof(&actor.proof) || actor.proof.role != "pm" {
        return Err(Error::rejected(
            "memory rejection is restricted to an authenticated PM endpoint",
        ));
    }
    let (proj, _) = find(pm, flag, slug)?;
    let _lock = pm.lock()?;
    let (_, mut mem) = find(pm, Some(&proj.key), slug)?;
    if mem.front.status == "superseded" {
        return Err(Error::rejected("a superseded memory cannot be rejected"));
    }
    mem.front.status = "rejected".to_string();
    mem.front.verified_at = None;
    mem.front.active_operation = None;
    check_front(&mem, &proj.components)?;
    save_mem(&mem)?;
    let committed = commit_mem(
        pm,
        slug,
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
/// that this file writer does not have. Refuse it until CAD-193 supplies
/// that primitive rather than performing two sequential writes.
pub fn supersede_native(
    _pm: &Pm,
    _flag: Option<&str>,
    _old: &str,
    _new: &str,
    _actor: &NativeIdentity,
) -> Result<Value> {
    Err(Error::rejected(
        "memory supersede is unsupported until crash-atomic pair recovery is available",
    ))
}

// ── Matching ─────────────────────────────────────────────────────

/// What a dispatch/match call knows about the target.
#[derive(Clone, Debug, Default)]
pub struct MatchCtx {
    pub components: Vec<String>,
    /// Concrete repo-relative paths the issue is likely to touch.
    pub paths: Vec<String>,
    pub providers: Vec<String>,
    pub tags: Vec<String>,
}

/// `*` within a path segment, `**` across segments, `?` one char.
/// Unanchored suffixes (`src/**` alone) still require the leading
/// segments to match — globs are anchored at both ends.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    fn inner(p: &[u8], s: &[u8]) -> bool {
        if p.is_empty() {
            return s.is_empty();
        }
        if p.starts_with(b"**") {
            let rest = &p[2..];
            let rest = rest.strip_prefix(b"/").unwrap_or(rest);
            // `**` spans slashes: try every split point of the string.
            return (0..=s.len()).any(|i| inner(rest, &s[i..]));
        }
        match p[0] {
            b'*' => {
                for i in 0..=s.len() {
                    if inner(&p[1..], &s[i..]) {
                        return true;
                    }
                    if i == s.len() || s[i] == b'/' {
                        return false;
                    }
                }
                false
            }
            b'?' => !s.is_empty() && s[0] != b'/' && inner(&p[1..], &s[1..]),
            c => !s.is_empty() && s[0] == c && inner(&p[1..], &s[1..]),
        }
    }
    inner(pattern.as_bytes(), path.as_bytes())
}

/// Union semantics per the kickoff: a memory applies when ANY scope
/// axis matches the context.
fn applies(scope: &Scope, ctx: &MatchCtx) -> bool {
    scope.project
        || scope.components.iter().any(|c| ctx.components.contains(c))
        || scope
            .paths
            .iter()
            .any(|g| ctx.paths.iter().any(|p| glob_match(g, p)))
        || scope.tags.iter().any(|t| ctx.tags.contains(t))
        || scope.providers.iter().any(|p| ctx.providers.contains(p))
}

fn type_rank(kind: &str) -> u8 {
    match kind {
        "rule" => 0,
        "gotcha" => 1,
        "recipe" => 2,
        _ => 3,
    }
}

fn confidence_rank(confidence: &str) -> u8 {
    match confidence {
        "high" => 0,
        "medium" => 1,
        _ => 2,
    }
}

/// Accepted memories that apply to `ctx`, ranked: type
/// (rule>gotcha>recipe>decision), confidence (high first), newest
/// verified_at first.
pub fn match_memories(memories: &[Memory], ctx: &MatchCtx) -> Vec<Memory> {
    let mut hits: Vec<Memory> = memories
        .iter()
        .filter(|m| {
            m.front.status == "accepted" && retrieval_status(m).0 && applies(&m.front.scope, ctx)
        })
        .cloned()
        .collect();
    hits.sort_by(|a, b| {
        (
            type_rank(&a.front.kind),
            confidence_rank(&a.front.confidence),
            b.front.verified_at.clone().unwrap_or_default(),
            &a.front.id,
        )
            .cmp(&(
                type_rank(&b.front.kind),
                confidence_rank(&b.front.confidence),
                a.front.verified_at.clone().unwrap_or_default(),
                &b.front.id,
            ))
    });
    hits
}

/// Paths the issue's recorded code commits touched — the path-scope
/// input for issue matching. Absent commits/repos simply yield none.
fn issue_paths(pm_dir: &Path, issue: &board::Issue) -> Vec<String> {
    let mut paths = Vec::new();
    let (commits, _) = history::code_commits(pm_dir, issue);
    for c in commits {
        let (Some(repo), Some(sha)) = (c["repo"].as_str(), c["sha"].as_str()) else {
            continue;
        };
        let dir = project::expand_home(repo);
        let out = git_bounded(
            &dir,
            &["show", "--format=", "--name-only", sha],
            Duration::from_secs(5),
        )
        .unwrap_or_default();
        paths.extend(out.lines().map(str::to_string));
    }
    // Explicit commit refs too — a ref path is a sha in a project repo.
    for r in issue.front.refs.iter().filter(|r| r.kind == "commit") {
        let Some(sha) = r.path.as_deref() else {
            continue;
        };
        let Ok(projects) = project::list(pm_dir) else {
            break;
        };
        for p in projects.iter().filter(|p| p.key == issue.project) {
            for repo in &p.repos {
                let Some(path) = &repo.path else { continue };
                let dir = project::expand_home(path);
                if git_bounded(
                    &dir,
                    &["cat-file", "-e", &format!("{sha}^{{commit}}")],
                    Duration::from_secs(5),
                )
                .is_err()
                {
                    continue;
                }
                if let Ok(out) = git_bounded(
                    &dir,
                    &["show", "--format=", "--name-only", sha],
                    Duration::from_secs(5),
                ) {
                    paths.extend(out.lines().map(str::to_string));
                }
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

/// `git` bounded through `proc::run_bounded` — returns trimmed stdout.
fn git_bounded(dir: &Path, args: &[&str], timeout: Duration) -> Result<String> {
    let out = proc::run_bounded(
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args),
        timeout,
    )
    .map_err(|e| Error::rejected(format!("git {} in {}: {e}", args.join(" "), dir.display())))?;
    if !out.status.success() {
        return Err(Error::rejected(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Match context for an issue: component, frontmatter tags,
/// recorded-commit paths, and the target worker's provider.
pub fn issue_ctx(pm: &Pm, issue: &board::Issue, provider: Option<&str>) -> Result<MatchCtx> {
    Ok(MatchCtx {
        components: issue.front.component.clone().into_iter().collect(),
        paths: issue_paths(&pm.dir, issue),
        providers: provider.map(|p| vec![p.to_string()]).unwrap_or_default(),
        tags: issue.front.tags.clone(),
    })
}

/// The dispatch/match surface: accepted memories applying to an
/// issue. Report-mode load — valid records still match when sibling
/// files are broken; the caller surfaces `errors`.
pub fn match_for_issue(
    pm: &Pm,
    issue: &board::Issue,
    provider: Option<&str>,
) -> Result<(Vec<Memory>, Vec<String>)> {
    let ctx = issue_ctx(pm, issue, provider)?;
    let (pool, errors) = load_project_report(&pm.dir, &issue.project);
    Ok((match_memories(&pool, &ctx), errors))
}

// ── Lessons rendering (dispatch) ─────────────────────────────────

/// Render the lessons file for a dispatch: each entry is the slug, the
/// one-line fact and the how-to-apply. Capped at `LESSON_MAX_ENTRIES`
/// memories and `LESSON_MAX_BYTES` total — a truncated tail is noted.
/// Returns `(text, slugs)`; empty input yields an empty string.
pub fn render_lessons(matched: &[Memory]) -> (String, Vec<String>) {
    let mut out = String::from("# Lessons — matched project memories\n\n");
    let mut slugs = Vec::new();
    let mut omitted = 0usize;
    for m in matched.iter().take(LESSON_MAX_ENTRIES) {
        let (fact, _why, how) = body_parts(&m.body);
        let fact = fact.join(" ").trim().to_string();
        let how = how.lines().next().unwrap_or_default().trim().to_string();
        let entry = format!(
            "- `{}` ({}): {}\n  apply: {}\n",
            m.front.id, m.front.kind, fact, how
        );
        if out.len() + entry.len() > LESSON_MAX_BYTES {
            omitted += 1;
            continue;
        }
        out.push_str(&entry);
        slugs.push(m.front.id.clone());
    }
    let extra = matched.len().saturating_sub(LESSON_MAX_ENTRIES) + omitted;
    if extra > 0 {
        out.push_str(&format!(
            "\n({extra} more matched — `cadence memory ls` lists them)\n"
        ));
    }
    if slugs.is_empty() {
        return (String::new(), vec![]);
    }
    (out, slugs)
}

/// Accepted project-wide `rule`s — the briefing's memory section.
/// Broken files are skipped, not fatal; callers surface the errors.
pub fn project_rules(pm: &Pm, key: &str) -> (Vec<Memory>, Vec<String>) {
    let (mems, errors) = load_project_report(&pm.dir, key);
    let mut rules: Vec<Memory> = mems
        .into_iter()
        .filter(|m| {
            m.front.status == "accepted"
                && retrieval_status(m).0
                && m.front.scope.project
                && m.front.kind == "rule"
        })
        .collect();
    rules.sort_by(|a, b| a.front.id.cmp(&b.front.id));
    (rules, errors)
}

// ── Staleness ────────────────────────────────────────────────────

/// `YYYY-MM-DD[THH:MM:SSZ]` → epoch seconds; needed to bound the
/// staleness window without chrono. Byte-sliced — a non-ASCII or
/// short timestamp is "unknown" (None), never a panic.
fn iso_epoch(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let n = |i: usize, j: usize| -> Option<i64> {
        let slice = b.get(i..j)?;
        if slice.iter().all(|c| c.is_ascii_digit()) {
            std::str::from_utf8(slice).ok()?.parse().ok()
        } else {
            None
        }
    };
    let (y, m, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    let (hh, mm, ss) = if b.len() >= 19 {
        (n(11, 13)?, n(14, 16)?, n(17, 19)?)
    } else {
        (0, 0, 0)
    };
    // days-from-civil (Howard Hinnant) → seconds.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}

/// Stale accepted memories: `verified_at` missing or older than the
/// `days` window, or path globs matching files changed in a project
/// repo after `verified_at` (the changed-file scan stays inside the
/// window). Informational — `verify` is the refresh. Broken files are
/// skipped and returned as the second tuple element.
pub fn stale(pm: &Pm, days: u64) -> (Vec<Value>, Vec<String>) {
    let mut out = Vec::new();
    let (mems, errors) = load_all_report(&pm.dir);
    let since_epoch = time::now_epoch() - (days as i64) * 86400;
    for m in mems {
        if m.front.status != "accepted" {
            continue;
        }
        let verified = m
            .front
            .verified_at
            .as_deref()
            .and_then(iso_epoch)
            .unwrap_or(0);
        if verified < since_epoch {
            out.push(json!({
                "project": m.project, "slug": m.front.id,
                "verified_at": m.front.verified_at,
                "reason": "not verified within the window",
                "changed": [],
            }));
            continue;
        }
        if m.front.scope.paths.is_empty() {
            continue;
        }
        // Changes count only after verified_at AND inside the window.
        let bound = verified.max(since_epoch);
        let since = time::iso(bound.max(0));
        let Some(proj) = project::list(&pm.dir)
            .unwrap_or_default()
            .into_iter()
            .find(|p| p.key == m.project)
        else {
            continue;
        };
        for repo in &proj.repos {
            let Some(path) = &repo.path else { continue };
            let dir = project::expand_home(path);
            if !dir.is_dir() {
                continue;
            }
            let mut args: Vec<String> = vec![
                "log".into(),
                format!("--since={since}"),
                "--format=".into(),
                "--name-only".into(),
                "--".into(),
            ];
            args.extend(m.front.scope.paths.iter().cloned());
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let Ok(log) = git_bounded(&dir, &arg_refs, Duration::from_secs(10)) else {
                continue;
            };
            let changed: Vec<&str> = log.lines().filter(|l| !l.is_empty()).collect();
            if !changed.is_empty() {
                out.push(json!({
                    "project": m.project, "slug": m.front.id,
                    "verified_at": m.front.verified_at,
                    "reason": "paths changed after verified_at",
                    "repo": path,
                    "changed": changed.iter().take(10).collect::<Vec<_>>(),
                }));
                break;
            }
        }
    }
    (out, errors)
}

// ── Lint ─────────────────────────────────────────────────────────

/// Validate every memory file in one project's `memory/` dir —
/// invoked from `issue lint` (which owns the PM-wide report) and by
/// `memory lint`. `err`/`warn` feed the caller's accumulators.
pub fn lint_dir(
    dir: &Path,
    proj: &project::Project,
    err: &mut dyn FnMut(String),
    warn: &mut dyn FnMut(String),
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // An absent dir lints clean — nothing to check.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            err(format!("{}/memory: cannot list directory: {e}", proj.key));
            return;
        }
    };
    let mut slugs = Vec::new();
    let mut mems = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                err(format!(
                    "{}/memory: directory entry unreadable: {e}",
                    proj.key
                ));
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".md") || name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            err(format!(
                "{}/memory/{name}: symlink — the board never follows links",
                proj.key
            ));
            continue;
        }
        let slug = name.trim_end_matches(".md");
        if !valid_slug(slug) {
            err(format!("{}/memory/{name}: bad slug grammar", proj.key));
            continue;
        }
        match load_file(&path, &proj.key) {
            Ok(Some(m)) => {
                if m.front.id != slug {
                    err(format!(
                        "{}/memory/{name}: frontmatter id '{}' does not match filename",
                        proj.key, m.front.id
                    ));
                }
                slugs.push(slug.to_string());
                mems.push(m);
            }
            Ok(None) => {}
            // A file that cannot even be parsed is a warning, not a
            // commit-blocking error: every read path already skips and
            // reports it (load_errors/memory_errors/lessons_error), so
            // erroring here would let one stray file brick every
            // tracker commit — including `issue start` mid-dispatch.
            Err(e) => warn(format!("{}/memory/{name}: {e}", proj.key)),
        }
    }
    for m in &mems {
        for e in check_front(m, &proj.components).err().into_iter() {
            err(format!("{}/memory/{}.md: {e}", m.project, m.front.id));
        }
        let mut body_errs = Vec::new();
        lint_body(&m.front.id, &m.body, &mut |e| body_errs.push(e));
        for e in body_errs {
            err(format!("{}/memory/{}.md: {e}", m.project, m.front.id));
        }
        if let Some(target) = &m.front.supersedes {
            if !slugs.contains(target) {
                err(format!(
                    "{}/memory/{}.md: supersedes '{target}' which does not exist",
                    m.project, m.front.id
                ));
            }
        }
        if m.front.status == "accepted"
            && m.front.scope.paths.is_empty()
            && !m.front.scope.project
            && m.front.scope.components.is_empty()
            && m.front.scope.tags.is_empty()
            && m.front.scope.providers.is_empty()
        {
            warn(format!(
                "{}/memory/{}.md: accepted but has no scope — it matches nothing",
                m.project, m.front.id
            ));
        }
    }
}

/// Card/list payload for `ls` and the UI.
pub fn card_json(m: &Memory) -> Value {
    let digest = semantic_digest(m);
    let (eligible, reason) = retrieval_status(m);
    let (accept_eligible, accept_reason) = quorum_status(m, "accept");
    let (verify_eligible, verify_reason) = quorum_status(m, "verify");
    json!({
        "project": m.project,
        "slug": m.front.id,
        "type": m.front.kind,
        "status": m.front.status,
        "confidence": m.front.confidence,
        "scope": {
            "project": m.front.scope.project,
            "components": m.front.scope.components,
            "paths": m.front.scope.paths,
            "providers": m.front.scope.providers,
            "tags": m.front.scope.tags,
        },
        "source": m.front.source,
        "author": m.front.author,
        "created": m.front.created,
        "verified_at": m.front.verified_at,
        "supersedes": m.front.supersedes,
        "revision_digest": digest,
        "review_cycle": m.front.review_cycle,
        "active_operation": m.front.active_operation,
        "review_count": m.front.reviews.len(),
        "finalization_count": m.front.finalizations.len(),
        "finalized_operations": m.front.finalizations.iter().map(|r| json!({
            "operation": r.operation.clone(),
            "cycle": r.cycle,
            "digest": r.digest.clone(),
            "finalized_at": r.finalized_at.clone(),
            "finalizer": r.finalizer.alias.clone(),
        })).collect::<Vec<_>>(),
        "quorum": {
            "eligible": eligible,
            "reason": reason,
            "accept": {"eligible": accept_eligible, "reason": accept_reason},
            "verify": {"eligible": verify_eligible, "reason": verify_reason},
        },
        "fact": fact_line(&m.body),
        "path": m.path,
    })
}

pub fn detail_json(m: &Memory) -> Value {
    let mut v = card_json(m);
    v["body"] = json!(m.body);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proof(alias: &str, registration: u64) -> IdentityProof {
        IdentityProof {
            alias: alias.to_string(),
            registration,
            generation: format!("gen-{registration}"),
            process_start: registration + 100,
            role: if alias == "pm" || alias.starts_with("pm-") {
                "pm".to_string()
            } else {
                "worker".to_string()
            },
        }
    }

    fn memory(status: &str, cycle: u64, author: Option<IdentityProof>) -> Memory {
        Memory {
            project: "demo".to_string(),
            front: Front {
                id: "lesson".to_string(),
                kind: "rule".to_string(),
                status: status.to_string(),
                scope: Scope {
                    project: true,
                    ..Scope::default()
                },
                source: Some("CAD-191".to_string()),
                confidence: "medium".to_string(),
                created: "2026-01-01T00:00:00Z".to_string(),
                verified_at: None,
                supersedes: None,
                author: author.as_ref().map(|p| p.alias.clone()),
                author_proof: author,
                contributors: Vec::new(),
                review_cycle: cycle,
                active_operation: None,
                reviews: Vec::new(),
                finalizations: Vec::new(),
            },
            body: "fact\n\n**Why:** evidence\n\n**How to apply:** use it\n".to_string(),
            path: PathBuf::from("/tmp/lesson.md"),
        }
    }

    fn receipt(actor: &IdentityProof, operation: &str, cycle: u64, digest: &str) -> ReviewReceipt {
        ReviewReceipt {
            reviewer: actor.alias.clone(),
            identity: actor.stable_id(),
            generation: actor.generation.clone(),
            process_start: actor.process_start,
            role: actor.role.clone(),
            operation: operation.to_string(),
            cycle,
            digest: digest.to_string(),
            verdict: "pass".to_string(),
            evidence: "independent evidence".to_string(),
            recorded_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn finalization(
        actor: &IdentityProof,
        operation: &str,
        cycle: u64,
        digest: &str,
    ) -> FinalizationReceipt {
        FinalizationReceipt {
            operation: operation.to_string(),
            cycle,
            digest: digest.to_string(),
            finalizer: actor.clone(),
            finalized_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn mutation_fixture() -> (tempfile::TempDir, Pm) {
        let dir = tempfile::tempdir().unwrap();
        let pm = Pm::init(dir.path()).unwrap();
        let project_dir = dir.path().join("demo");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(
            project_dir.join("project.yaml"),
            "key: demo\nprefix: D\ncomponents: []\n",
        )
        .unwrap();
        pm.commit("project fixture\n\nActor: test\n").unwrap();
        (dir, pm)
    }

    fn native(alias: &str, registration: u64) -> NativeIdentity {
        NativeIdentity {
            proof: proof(alias, registration),
        }
    }

    #[test]
    fn native_review_finalize_consumes_cycle_and_requires_new_verify_cycle() {
        let (_dir, pm) = mutation_fixture();
        let author = native("worker-author", 1);
        let pm_actor = native("pm", 4);
        let worker_a = native("worker-a", 2);
        let worker_b = native("worker-b", 3);
        let body = "fact\n\n**Why:** evidence\n\n**How to apply:** use it\n";
        propose_native(
            &pm,
            "demo",
            "rule",
            &Scope {
                project: true,
                ..Scope::default()
            },
            Some("CAD-191"),
            Some("high"),
            None,
            Some(body),
            Some("lesson"),
            &author,
        )
        .unwrap();
        let (_, mem) = find(&pm, Some("demo"), "lesson").unwrap();
        let digest = semantic_digest(&mem);
        submit_review(
            &pm,
            Some("demo"),
            "lesson",
            &ReviewRequest {
                operation: "accept",
                verdict: "pass",
                evidence: "worker-a acceptance evidence",
                expected_digest: &digest,
            },
            &worker_a,
        )
        .unwrap();
        submit_review(
            &pm,
            Some("demo"),
            "lesson",
            &ReviewRequest {
                operation: "accept",
                verdict: "pass",
                evidence: "worker-b acceptance evidence",
                expected_digest: &digest,
            },
            &worker_b,
        )
        .unwrap();
        finalize_native(&pm, Some("demo"), "lesson", "accept", &digest, &pm_actor).unwrap();
        let (_, accepted) = find(&pm, Some("demo"), "lesson").unwrap();
        assert!(retrieval_status(&accepted).0);

        let bytes_after_accept = std::fs::read(&accepted.path).unwrap();
        let repeated =
            finalize_native(&pm, Some("demo"), "lesson", "accept", &digest, &pm_actor).unwrap_err();
        assert!(repeated.to_string().contains("already finalized"));
        assert_eq!(bytes_after_accept, std::fs::read(&accepted.path).unwrap());

        for (worker, evidence) in [
            (&worker_a, "worker-a verify evidence"),
            (&worker_b, "worker-b verify evidence"),
        ] {
            submit_review(
                &pm,
                Some("demo"),
                "lesson",
                &ReviewRequest {
                    operation: "verify",
                    verdict: "pass",
                    evidence,
                    expected_digest: &digest,
                },
                worker,
            )
            .unwrap();
        }
        let (_, before_verify_finalize) = find(&pm, Some("demo"), "lesson").unwrap();
        assert_eq!(before_verify_finalize.front.review_cycle, 2);
        assert!(!retrieval_status(&before_verify_finalize).0);
        finalize_native(&pm, Some("demo"), "lesson", "verify", &digest, &pm_actor).unwrap();
        let (_, verified) = find(&pm, Some("demo"), "lesson").unwrap();
        assert!(retrieval_status(&verified).0);

        let bytes_after_verify = std::fs::read(&verified.path).unwrap();
        let repeated =
            finalize_native(&pm, Some("demo"), "lesson", "verify", &digest, &pm_actor).unwrap_err();
        assert!(repeated.to_string().contains("already finalized"));
        assert_eq!(bytes_after_verify, std::fs::read(&verified.path).unwrap());

        // The same two authenticated workers may review again, but only in
        // cycle three.  Cycle-two receipts cannot make this cycle eligible.
        for (worker, evidence) in [
            (&worker_a, "worker-a cycle-three evidence"),
            (&worker_b, "worker-b cycle-three evidence"),
        ] {
            submit_review(
                &pm,
                Some("demo"),
                "lesson",
                &ReviewRequest {
                    operation: "verify",
                    verdict: "pass",
                    evidence,
                    expected_digest: &digest,
                },
                worker,
            )
            .unwrap();
        }
        let (_, cycle_three) = find(&pm, Some("demo"), "lesson").unwrap();
        assert_eq!(cycle_three.front.review_cycle, 3);
        assert!(!retrieval_status(&cycle_three).0);
        finalize_native(&pm, Some("demo"), "lesson", "verify", &digest, &pm_actor).unwrap();
        let (_, final_memory) = find(&pm, Some("demo"), "lesson").unwrap();
        assert_eq!(final_memory.front.finalizations.len(), 3);
        assert!(retrieval_status(&final_memory).0);
    }

    #[test]
    fn leading_blank_memory_body_keeps_digest_through_acceptance() {
        let (_dir, pm) = mutation_fixture();
        let author = native("worker-author", 1);
        let pm_actor = native("pm", 4);
        let worker_a = native("worker-a", 2);
        let worker_b = native("worker-b", 3);
        let body = "\nleading blank fact\n\n**Why:** preserve the exact body\n\n**How to apply:** retain it\n";
        let proposed = propose_native(
            &pm,
            "demo",
            "rule",
            &Scope {
                project: true,
                ..Scope::default()
            },
            Some("CAD-191"),
            Some("high"),
            None,
            Some(body),
            Some("leading-blank"),
            &author,
        )
        .unwrap();
        let digest = proposed["digest"].as_str().unwrap().to_string();
        let (_, loaded) = find(&pm, Some("demo"), "leading-blank").unwrap();
        assert_eq!(loaded.body, body);
        assert_eq!(semantic_digest(&loaded), digest);

        for (worker, evidence) in [
            (&worker_a, "worker-a inspected the preserved body"),
            (&worker_b, "worker-b inspected the preserved body"),
        ] {
            submit_review(
                &pm,
                Some("demo"),
                "leading-blank",
                &ReviewRequest {
                    operation: "accept",
                    verdict: "pass",
                    evidence,
                    expected_digest: &digest,
                },
                worker,
            )
            .unwrap();
        }
        finalize_native(
            &pm,
            Some("demo"),
            "leading-blank",
            "accept",
            &digest,
            &pm_actor,
        )
        .unwrap();
        let (_, accepted) = find(&pm, Some("demo"), "leading-blank").unwrap();
        assert_eq!(accepted.body, body);
        assert_eq!(semantic_digest(&accepted), digest);
        assert!(retrieval_status(&accepted).0);
    }

    #[test]
    fn native_review_refuses_scope_and_source_changes_after_load() {
        let (_dir, pm) = mutation_fixture();
        let author = native("worker-author", 1);
        let reviewer = native("worker-reviewer", 2);
        let body = "fact\n\n**Why:** evidence\n\n**How to apply:** use it\n";
        propose_native(
            &pm,
            "demo",
            "rule",
            &Scope {
                project: true,
                ..Scope::default()
            },
            Some("CAD-191"),
            Some("high"),
            None,
            Some(body),
            Some("stale-review"),
            &author,
        )
        .unwrap();
        let (_, loaded) = find(&pm, Some("demo"), "stale-review").unwrap();
        let digest = semantic_digest(&loaded);
        let path = loaded.path.clone();

        // Simulate another writer changing the file after a reviewer loaded
        // its digest. The RPC reloads under the PM lock and must refuse the
        // stale review without appending a receipt.
        let mut changed_source = loaded.clone();
        changed_source.front.source = Some("CAD-191-revised".to_string());
        save_mem(&changed_source).unwrap();
        let before_refused_source = std::fs::read(&path).unwrap();
        let err = submit_review(
            &pm,
            Some("demo"),
            "stale-review",
            &ReviewRequest {
                operation: "accept",
                verdict: "pass",
                evidence: "stale source review",
                expected_digest: &digest,
            },
            &reviewer,
        )
        .unwrap_err();
        assert!(err.to_string().contains("revision changed"), "{err}");
        assert_eq!(before_refused_source, std::fs::read(&path).unwrap());
        let (_, after_source) = find(&pm, Some("demo"), "stale-review").unwrap();
        assert!(after_source.front.reviews.is_empty());
        assert_eq!(
            after_source.front.source.as_deref(),
            Some("CAD-191-revised")
        );

        // Restore the original loaded revision, then exercise the same
        // reload boundary with an applicability change rather than source.
        save_mem(&loaded).unwrap();
        let mut changed_scope = loaded.clone();
        changed_scope.front.scope.paths = vec!["src/**".to_string()];
        save_mem(&changed_scope).unwrap();
        let before_refused_scope = std::fs::read(&path).unwrap();
        let err = submit_review(
            &pm,
            Some("demo"),
            "stale-review",
            &ReviewRequest {
                operation: "accept",
                verdict: "pass",
                evidence: "stale scope review",
                expected_digest: &digest,
            },
            &reviewer,
        )
        .unwrap_err();
        assert!(err.to_string().contains("revision changed"), "{err}");
        assert_eq!(before_refused_scope, std::fs::read(&path).unwrap());
        let (_, after_scope) = find(&pm, Some("demo"), "stale-review").unwrap();
        assert!(after_scope.front.reviews.is_empty());
        assert_eq!(after_scope.front.scope.paths, vec!["src/**".to_string()]);
    }

    #[test]
    fn review_in_wrong_project_refuses_without_mutating_memory() {
        let (dir, pm) = mutation_fixture();
        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            other.join("project.yaml"),
            "key: other\nprefix: O\ncomponents: []\n",
        )
        .unwrap();
        let author = native("worker-author", 1);
        let reviewer = native("worker-reviewer", 2);
        propose_native(
            &pm,
            "demo",
            "rule",
            &Scope {
                project: true,
                ..Scope::default()
            },
            Some("CAD-191"),
            Some("high"),
            None,
            Some("fact\n\n**Why:** evidence\n\n**How to apply:** use it\n"),
            Some("project-bound"),
            &author,
        )
        .unwrap();
        let (_, mem) = find(&pm, Some("demo"), "project-bound").unwrap();
        let digest = semantic_digest(&mem);
        let before = std::fs::read(&mem.path).unwrap();
        let err = submit_review(
            &pm,
            Some("other"),
            "project-bound",
            &ReviewRequest {
                operation: "accept",
                verdict: "pass",
                evidence: "wrong project",
                expected_digest: &digest,
            },
            &reviewer,
        )
        .unwrap_err();
        assert!(err.to_string().contains("Unknown memory"), "{err}");
        assert_eq!(before, std::fs::read(&mem.path).unwrap());
        let (_, unchanged) = find(&pm, Some("demo"), "project-bound").unwrap();
        assert!(unchanged.front.reviews.is_empty());
        assert!(unchanged.front.active_operation.is_none());
    }

    #[test]
    fn supersede_refusal_preserves_existing_memory_bytes() {
        let (_dir, pm) = mutation_fixture();
        let author = native("worker-author", 1);
        let pm_actor = native("pm", 2);
        propose_native(
            &pm,
            "demo",
            "rule",
            &Scope {
                project: true,
                ..Scope::default()
            },
            Some("CAD-191"),
            Some("medium"),
            None,
            Some("old fact\n\n**Why:** evidence\n\n**How to apply:** use it\n"),
            Some("old-memory"),
            &author,
        )
        .unwrap();
        let (_, old) = find(&pm, Some("demo"), "old-memory").unwrap();
        let before = std::fs::read(&old.path).unwrap();
        let err =
            supersede_native(&pm, Some("demo"), "old-memory", "new-memory", &pm_actor).unwrap_err();
        assert!(
            err.to_string().contains("crash-atomic pair recovery"),
            "{err}"
        );
        assert_eq!(before, std::fs::read(&old.path).unwrap());
        assert!(!memory_dir(&pm, "demo").join("new-memory.md").exists());
    }

    #[test]
    fn retrieval_rejects_persisted_self_finalizer_corruption() {
        let (_dir, pm) = mutation_fixture();
        let author = native("pm-author", 1);
        propose_native(
            &pm,
            "demo",
            "rule",
            &Scope {
                project: true,
                ..Scope::default()
            },
            Some("CAD-191"),
            Some("high"),
            None,
            Some("self-finalized fact\n\n**Why:** corrupt persisted receipt\n\n**How to apply:** refuse it\n"),
            Some("corrupt-finalizer"),
            &author,
        )
        .unwrap();
        let (_, mut corrupted) = find(&pm, Some("demo"), "corrupt-finalizer").unwrap();
        corrupted.front.status = "accepted".to_string();
        corrupted.front.review_cycle = 1;
        let digest = semantic_digest(&corrupted);
        corrupted.front.reviews = vec![
            receipt(&proof("worker-a", 2), "accept", 1, &digest),
            receipt(&proof("worker-b", 3), "accept", 1, &digest),
        ];
        // Use a distinct registration to prove alias reuse cannot launder a
        // proposer into an authenticated PM finalizer after persistence.
        corrupted.front.finalizations =
            vec![finalization(&proof("pm-author", 99), "accept", 1, &digest)];
        corrupted.front.active_operation = None;
        save_mem(&corrupted).unwrap();

        let (_, loaded) = find(&pm, Some("demo"), "corrupt-finalizer").unwrap();
        let before = std::fs::read(&loaded.path).unwrap();
        let (eligible, reason) = retrieval_status(&loaded);
        assert!(!eligible);
        assert!(reason.contains("no matching PM acceptance finalization"));
        assert_eq!(before, std::fs::read(&loaded.path).unwrap());
    }

    #[test]
    fn proposer_cannot_finalize_its_own_acceptance() {
        let (_dir, pm) = mutation_fixture();
        let author = native("pm-author", 1);
        let finalizer = native("pm", 4);
        let worker_a = native("worker-a", 2);
        let worker_b = native("worker-b", 3);
        let body = "fact\n\n**Why:** evidence\n\n**How to apply:** use it\n";
        propose_native(
            &pm,
            "demo",
            "rule",
            &Scope {
                project: true,
                ..Scope::default()
            },
            Some("CAD-191"),
            Some("high"),
            None,
            Some(body),
            Some("pm-authored"),
            &author,
        )
        .unwrap();
        let (_, mem) = find(&pm, Some("demo"), "pm-authored").unwrap();
        let digest = semantic_digest(&mem);
        for (worker, evidence) in [
            (&worker_a, "worker-a evidence"),
            (&worker_b, "worker-b evidence"),
        ] {
            submit_review(
                &pm,
                Some("demo"),
                "pm-authored",
                &ReviewRequest {
                    operation: "accept",
                    verdict: "pass",
                    evidence,
                    expected_digest: &digest,
                },
                worker,
            )
            .unwrap();
        }
        let path = find(&pm, Some("demo"), "pm-authored").unwrap().1.path;
        let before = std::fs::read(&path).unwrap();
        let err = finalize_native(&pm, Some("demo"), "pm-authored", "accept", &digest, &author)
            .unwrap_err();
        assert!(err.to_string().contains("proposer or contributor"));
        assert_eq!(before, std::fs::read(&path).unwrap());
        assert!(!retrieval_status(&find(&pm, Some("demo"), "pm-authored").unwrap().1).0);

        finalize_native(
            &pm,
            Some("demo"),
            "pm-authored",
            "accept",
            &digest,
            &finalizer,
        )
        .unwrap();
        let (_, accepted) = find(&pm, Some("demo"), "pm-authored").unwrap();
        assert!(retrieval_status(&accepted).0);
    }

    #[test]
    fn accepted_retrieval_requires_two_distinct_worker_aliases() {
        let mut mem = memory("accepted", 1, Some(proof("author", 1)));
        let digest = semantic_digest(&mem);
        mem.front
            .reviews
            .push(receipt(&proof("worker-a", 2), "accept", 1, &digest));
        assert!(!retrieval_status(&mem).0);
        mem.front
            .reviews
            .push(receipt(&proof("worker-b", 3), "accept", 1, &digest));
        assert!(!retrieval_status(&mem).0);
        mem.front
            .finalizations
            .push(finalization(&proof("pm", 4), "accept", 1, &digest));
        assert!(retrieval_status(&mem).0);
        assert_eq!(
            match_memories(
                &[mem],
                &MatchCtx {
                    ..Default::default()
                }
            )
            .len(),
            1
        );
    }

    #[test]
    fn quorum_status_reports_legacy_identity_before_cycle_state() {
        let mem = memory("proposed", 0, None);
        let (eligible, reason) = quorum_status(&mem, "accept");
        assert!(!eligible);
        assert!(reason.contains("no authenticated native identity"));
    }

    #[test]
    fn match_ranking_and_union_semantics_use_only_finalized_memories() {
        fn eligible(
            id: &str,
            kind: &str,
            confidence: &str,
            scope: Scope,
            verified_at: &str,
            registration: u64,
        ) -> Memory {
            let author = proof(&format!("author-{id}"), registration);
            let mut mem = memory("accepted", 1, Some(author));
            mem.front.id = id.to_string();
            mem.front.kind = kind.to_string();
            mem.front.confidence = confidence.to_string();
            mem.front.scope = scope;
            mem.front.verified_at = Some(verified_at.to_string());
            mem.path = PathBuf::from(format!("/tmp/{id}.md"));
            let digest = semantic_digest(&mem);
            mem.front.reviews.push(receipt(
                &proof(&format!("review-a-{id}"), registration + 100),
                "accept",
                1,
                &digest,
            ));
            mem.front.reviews.push(receipt(
                &proof(&format!("review-b-{id}"), registration + 200),
                "accept",
                1,
                &digest,
            ));
            mem.front.finalizations.push(finalization(
                &proof(&format!("pm-{id}"), registration + 300),
                "accept",
                1,
                &digest,
            ));
            mem
        }

        let memories = vec![
            eligible(
                "r-project",
                "rule",
                "high",
                Scope {
                    project: true,
                    ..Scope::default()
                },
                "2026-01-01T00:00:00Z",
                1000,
            ),
            eligible(
                "r-comp",
                "rule",
                "medium",
                Scope {
                    components: vec!["daemon".to_string()],
                    ..Scope::default()
                },
                "2026-01-01T00:00:00Z",
                2000,
            ),
            eligible(
                "r-low",
                "rule",
                "low",
                Scope {
                    project: true,
                    ..Scope::default()
                },
                "2026-01-01T00:00:00Z",
                3000,
            ),
            eligible(
                "g-comp-hi",
                "gotcha",
                "high",
                Scope {
                    components: vec!["daemon".to_string()],
                    ..Scope::default()
                },
                "2026-01-01T00:00:00Z",
                4000,
            ),
            eligible(
                "g-tag",
                "gotcha",
                "medium",
                Scope {
                    tags: vec!["flaky".to_string()],
                    ..Scope::default()
                },
                "2026-01-01T00:00:00Z",
                5000,
            ),
            eligible(
                "g-comp-lo",
                "gotcha",
                "low",
                Scope {
                    components: vec!["daemon".to_string()],
                    ..Scope::default()
                },
                "2026-01-01T00:00:00Z",
                6000,
            ),
            eligible(
                "c-path",
                "recipe",
                "medium",
                Scope {
                    paths: vec!["src/**".to_string()],
                    ..Scope::default()
                },
                "2026-01-01T00:00:00Z",
                7000,
            ),
            eligible(
                "c-prov",
                "recipe",
                "medium",
                Scope {
                    providers: vec!["claude".to_string()],
                    ..Scope::default()
                },
                "2026-01-01T00:00:00Z",
                8000,
            ),
            eligible(
                "d-prov",
                "decision",
                "high",
                Scope {
                    providers: vec!["claude".to_string()],
                    ..Scope::default()
                },
                "2026-01-01T00:00:00Z",
                9000,
            ),
            eligible(
                "x-other",
                "gotcha",
                "medium",
                Scope {
                    components: vec!["other".to_string()],
                    ..Scope::default()
                },
                "2026-01-01T00:00:00Z",
                10000,
            ),
            memory("proposed", 0, Some(proof("pending-author", 11000))),
        ];
        let names = |ctx: MatchCtx| {
            match_memories(&memories, &ctx)
                .into_iter()
                .map(|m| m.front.id)
                .collect::<Vec<_>>()
        };

        assert_eq!(
            names(MatchCtx {
                components: vec!["daemon".to_string()],
                paths: vec!["src/adapter/x.rs".to_string()],
                tags: vec!["flaky".to_string()],
                providers: vec!["claude".to_string()],
            }),
            vec![
                "r-project",
                "r-comp",
                "r-low",
                "g-comp-hi",
                "g-tag",
                "g-comp-lo",
                "c-path",
                "c-prov",
                "d-prov",
            ]
        );
        assert_eq!(names(MatchCtx::default()), vec!["r-project", "r-low"]);
        assert_eq!(
            names(MatchCtx {
                components: vec!["other".to_string()],
                ..MatchCtx::default()
            }),
            vec!["r-project", "r-low", "x-other"]
        );
    }

    #[test]
    fn alias_reuse_cannot_supply_the_second_vote() {
        let mut mem = memory("accepted", 1, Some(proof("author", 1)));
        let digest = semantic_digest(&mem);
        mem.front
            .reviews
            .push(receipt(&proof("worker-a", 2), "accept", 1, &digest));
        mem.front
            .reviews
            .push(receipt(&proof("worker-b", 3), "accept", 1, &digest));
        mem.front
            .finalizations
            .push(finalization(&proof("pm", 4), "accept", 1, &digest));
        assert!(retrieval_status(&mem).0);
        // A later record corruption cannot make a finalized cycle eligible:
        // the PM receipt is durable evidence of what was finalized, not a
        // replacement for rechecking its retained reviewer receipts.
        mem.front.reviews[1] = receipt(&proof("worker-a", 9), "accept", 1, &digest);
        assert!(!retrieval_status(&mem).0);
    }

    #[test]
    fn finalized_quorum_requires_both_retained_receipts() {
        let mut mem = memory("accepted", 1, Some(proof("author", 1)));
        let digest = semantic_digest(&mem);
        mem.front
            .reviews
            .push(receipt(&proof("worker-a", 2), "accept", 1, &digest));
        mem.front
            .reviews
            .push(receipt(&proof("worker-b", 3), "accept", 1, &digest));
        mem.front
            .finalizations
            .push(finalization(&proof("pm", 4), "accept", 1, &digest));
        assert!(retrieval_status(&mem).0);
        mem.front.reviews.pop();
        assert!(!retrieval_status(&mem).0);
    }

    #[test]
    fn author_alias_cannot_vote_after_reregistration() {
        let mut mem = memory("accepted", 1, Some(proof("author", 1)));
        let digest = semantic_digest(&mem);
        mem.front
            .reviews
            .push(receipt(&proof("author", 99), "accept", 1, &digest));
        mem.front
            .reviews
            .push(receipt(&proof("worker-b", 3), "accept", 1, &digest));
        mem.front
            .finalizations
            .push(finalization(&proof("pm", 4), "accept", 1, &digest));
        assert!(!retrieval_status(&mem).0);
    }

    #[test]
    fn authenticated_pm_can_be_a_non_author_reviewer() {
        let mut mem = memory("accepted", 1, Some(proof("author", 1)));
        let digest = semantic_digest(&mem);
        mem.front
            .reviews
            .push(receipt(&proof("pm-reviewer", 4), "accept", 1, &digest));
        mem.front
            .reviews
            .push(receipt(&proof("worker-a", 2), "accept", 1, &digest));
        mem.front
            .finalizations
            .push(finalization(&proof("pm", 5), "accept", 1, &digest));
        assert!(retrieval_status(&mem).0);
    }

    #[test]
    fn changed_claim_invalidates_old_receipts() {
        let mut mem = memory("accepted", 1, Some(proof("author", 1)));
        let digest = semantic_digest(&mem);
        mem.front
            .reviews
            .push(receipt(&proof("worker-a", 2), "accept", 1, &digest));
        mem.front
            .reviews
            .push(receipt(&proof("worker-b", 3), "accept", 1, &digest));
        mem.front
            .finalizations
            .push(finalization(&proof("pm", 4), "accept", 1, &digest));
        mem.body = "changed\n\n**Why:** new\n\n**How to apply:** new\n".to_string();
        assert!(!retrieval_status(&mem).0);
    }

    #[test]
    fn accepted_legacy_record_is_visible_but_blocked() {
        let mem = memory("accepted", 1, None);
        let (eligible, reason) = retrieval_status(&mem);
        assert!(!eligible);
        assert!(reason.contains("authenticated proposer"));
    }

    #[test]
    fn verify_requires_a_fresh_cycle() {
        let mut mem = memory("accepted", 1, Some(proof("author", 1)));
        let digest = semantic_digest(&mem);
        mem.front
            .reviews
            .push(receipt(&proof("worker-a", 2), "accept", 1, &digest));
        mem.front
            .reviews
            .push(receipt(&proof("worker-b", 3), "accept", 1, &digest));
        mem.front
            .finalizations
            .push(finalization(&proof("pm", 4), "accept", 1, &digest));
        assert!(retrieval_status(&mem).0);
        mem.front.active_operation = Some("verify".to_string());
        mem.front.review_cycle = 2;
        assert!(!retrieval_status(&mem).0);
        mem.front
            .reviews
            .push(receipt(&proof("worker-a", 2), "verify", 2, &digest));
        mem.front
            .reviews
            .push(receipt(&proof("worker-b", 3), "verify", 2, &digest));
        assert!(!retrieval_status(&mem).0);
        mem.front
            .finalizations
            .push(finalization(&proof("pm", 4), "verify", 2, &digest));
        mem.front.active_operation = None;
        assert!(retrieval_status(&mem).0);
        mem.front.reviews[2].verdict = "revise".to_string();
        assert!(!retrieval_status(&mem).0);
        mem.front.reviews[2].verdict = "pass".to_string();

        // A finalized verify cycle is consumed.  The next cycle starts at
        // three and cannot reuse the two workers' cycle-two receipts.
        mem.front.active_operation = Some("verify".to_string());
        mem.front.review_cycle = 3;
        assert!(!retrieval_status(&mem).0);
        mem.front
            .reviews
            .push(receipt(&proof("worker-a", 2), "verify", 3, &digest));
        mem.front
            .reviews
            .push(receipt(&proof("worker-b", 3), "verify", 3, &digest));
        assert!(!retrieval_status(&mem).0);
        mem.front
            .finalizations
            .push(finalization(&proof("pm", 4), "verify", 3, &digest));
        mem.front.active_operation = None;
        assert!(retrieval_status(&mem).0);
        // The latest finalized verify cycle is authoritative.  Keeping an
        // older valid cycle cannot hide corruption in cycle three.
        let latest = mem.front.finalizations.pop().unwrap();
        assert!(!retrieval_status(&mem).0);
        mem.front.finalizations.push(latest);
        mem.front.finalizations[2].digest = "0".repeat(64);
        assert!(!retrieval_status(&mem).0);
        mem.front.finalizations[2].digest = digest.clone();
        mem.front.reviews[4].verdict = "revise".to_string();
        assert!(!retrieval_status(&mem).0);
    }
}
