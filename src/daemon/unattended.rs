//! CAD-225 bounded preauthorization: one explicit operator-issued
//! unattended grant — `workflow approve --unattended` — permits exactly
//! ONE admission of ONE exact render of a named stored project
//! workflow, and the daemon-internal landing that consumes it
//! ([`super::unattended_landing`]). This module owns the grant record,
//! the durable run receipt, admission and the authorization query.
//!
//! ## Authority model
//!
//! * Issuance is operator-only: `rpc_workflow_approve` proves
//!   `operator_connection` before anything here runs, and an agent can
//!   never produce a grant — the store row is written only from this
//!   operator path, carries `provenance: "native"` and a fresh
//!   `grant-<32hex>` id, and binds the full template SHA-256, the
//!   SHA-256 of the exact rendered plan (effective inputs included), a
//!   normalized `owner/repo` and a normalized path allowlist.
//! * `plan_propose --workflow` is the only admission: the live grant's
//!   render hash must equal the render the proposer's inputs produce
//!   byte-for-byte. Caller-supplied grant/approval flags on
//!   `plan_propose` are refused in `plans_rpc` and confer nothing.
//! * One grant admits one plan: each durable `unattended_run` mark on
//!   `APPROVAL_STREAM` binds the grant id, the allocated epic and ticket
//!   ids and the canonical original semantics of every file. Repeating
//!   the same request returns the same epic and tickets — never a
//!   second run.
//! * An ordinary `workflow approve` (no `unattended`), a replacement
//!   grant or expiry revokes future automatic admission, dispatch and
//!   landing for every run bound to the old grant. `renew` is the only
//!   refresh: it must reproduce every immutable bound — template,
//!   render, repo, paths, inputs — and changes only the expiry.
//!
//! ## Durable admission / crash recovery
//!
//! Under `unattended_lock` WRITE → the PM write lock → the store write
//! tx: (1) an `intent` mark records the expected ids and canonical
//! semantics BEFORE any tracker file exists; (2) one tracker commit
//! creates the approved epic and its ready tickets; (3) an `admitted`
//! mark is the run receipt. A crash leaves the intent behind: the next
//! identical proposal either completes the admission from it (absent
//! files → the same ids; identical files → the missing receipt) or,
//! when anything diverged, marks the intent `held` and refuses — an
//! unknown partial outcome is never inferred successful and never
//! allocates again. Until reconciliation the ids stay reserved:
//! `Store::unattended_reserved_ids` keeps ordinary plan creation off
//! them.
//!
//! ## Lock order
//!
//! `unattended_lock` WRITE → PM write lock → store write tx.
//! `unattended_authorization_for_issue` takes NO lock — a read-only
//! store/tracker query that is also called under `dispatch_lock`
//! (job gate) and `delivery_lock` (landing). Grant writers never take
//! `dispatch_lock` or `delivery_lock` while holding `unattended_lock`,
//! and the WRITE guard is dropped before wake/resume (callers in
//! `plans_rpc` scope the guard to the record/admit section).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::*;
use crate::issue::model;
use crate::issue::workflow as wf;
use crate::issue::write as tracker;

/// Marks only this module writes — native provenance separates them
/// from anything an agent could claim.
const PROVENANCE: &str = "native";

/// Durable mark states (`status` on a `unattended_run` event).
const MARK_INTENT: &str = "intent";
const MARK_HELD: &str = "held";
const MARK_ADMITTED: &str = "admitted";

const TTL_DEFAULT: u64 = 3600;
const TTL_MIN: u64 = 1;
const TTL_MAX: u64 = 86_400;

/// Inner schema of the grant object recorded inside the
/// `workflow_approved` payload's `unattended` key.
const GRANT_KEYS: &[&str] = &[
    "provenance",
    "grant_id",
    "revision",
    "repo",
    "paths",
    "inputs",
    "template_sha256",
    "render_sha256",
    "ttl_secs",
    "issued_at",
    "expires_at",
    "renewed",
];
/// Keys an `unattended` RPC object may carry — anything else refuses.
const GRANT_RPC_KEYS: &[&str] = &["repo", "paths", "inputs", "ttl_secs"];
/// Top-level `workflow_approve` fields that are grant material: they
/// are only ever read inside `unattended`, or as `renew`/`ttl_secs` on
/// a renewal — never bare.
const GRANT_TOP_FIELDS: &[&str] = &[
    "repo",
    "paths",
    "inputs",
    "ttl",
    "allow_path",
    "allow_paths",
    "grant_id",
    "template_sha256",
    "render_sha256",
    "expires_at",
    "approved",
    "approve",
    "state",
    "decided_by",
];
/// Durable run mark schema — `reason` only on a `held` mark.
const MARK_KEYS: &[&str] = &[
    "provenance",
    "status",
    "grant_id",
    "project",
    "workflow",
    "repo",
    "paths",
    "template_sha256",
    "render_sha256",
    "expires_at",
    "proposed_by",
    "epic",
    "tickets",
    "canonical",
    "fingerprints",
    "reason",
];

fn coded(code: &'static str, message: impl Into<String>) -> Error {
    Error::invalid(code, message)
}

fn sha256_prefixed(text: &str) -> String {
    let hash = Sha256::digest(text.as_bytes());
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

fn sha256_shape(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    })
}

/// `grant-` + 32 lowercase hex of real entropy — unguessable, unique.
fn fresh_grant_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
        .map_err(|e| Error::internal(format!("cannot mint a grant id: {e}")))?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("grant-{hex}"))
}

fn valid_grant_id(id: &str) -> bool {
    id.strip_prefix("grant-").is_some_and(|hex| {
        hex.len() == 32
            && hex
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    })
}

/// The honest tracker attribution for a grant-admitted plan — the
/// workflow grant decided, never a fabricated direct operator decision.
fn grant_decision(grant_id: &str) -> String {
    format!("workflow grant {grant_id}")
}

/// `OWNER/REPO` normalized: trimmed, lowercased, exactly two segments of
/// `[a-z0-9._-]` — the spelling [`AuthorizedRun::repo`] carries and the
/// landing path compares `to_ascii_lowercase` against.
fn normalize_repo(input: &str) -> Result<String> {
    let raw = input.trim();
    if raw.is_empty() || raw.chars().any(char::is_control) {
        return Err(Error::rejected(
            "repo must be OWNER/REPO, never empty or control characters",
        ));
    }
    let parts: Vec<&str> = raw.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|p| {
            p.is_empty()
                || *p == "."
                || *p == ".."
                || !p
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        })
    {
        return Err(Error::rejected(format!(
            "repo '{raw}' — expected OWNER/REPO (letters, digits, . _ -)"
        )));
    }
    Ok(format!(
        "{}/{}",
        parts[0].to_ascii_lowercase(),
        parts[1].to_ascii_lowercase()
    ))
}

/// The repo is a *registered* repository of the project — and stays one.
/// Registration is path + remote, so the check reads the CURRENT remote
/// identity of each registered checkout ([`project::repo_identity`]):
/// a `git remote set-url origin https://github.com/evil/…` since the
/// grant revokes it on the spot. The live normalized remote must equal
/// `github.com/{owner}/{repo}` case-insensitively — the exact
/// [`crate::delivery::project_pr_refusal`] comparison, never a suffix
/// match. A repo with no resolvable checkout falls back to its recorded
/// remote (the only registration evidence left).
fn registered_repo(pm_dir: &Path, project_key: &str, repo: &str) -> Result<bool> {
    let projects = crate::issue::project::list(pm_dir)?;
    let Some(project) = projects.iter().find(|p| p.key == project_key) else {
        return Err(crate::issue::project::unknown_project(project_key, pm_dir));
    };
    let want =
        crate::issue::project::normalize_remote(&format!("github.com/{repo}")).to_ascii_lowercase();
    Ok(project.repos.iter().any(|r| {
        let recorded = |repo: &crate::issue::project::Repo| {
            repo.remote.as_deref().is_some_and(|rec| {
                crate::issue::project::normalize_remote(rec).to_ascii_lowercase() == want
            })
        };
        match r.path.as_deref() {
            // A resolvable registered checkout is authoritative: the
            // grant survives only while its CURRENT remote names the
            // bound repo — a `remote set-url` or a dropped origin
            // revokes on the spot; the recorded string cannot rescue it.
            Some(path) => match crate::issue::project::repo_identity(
                &crate::issue::project::expand_home(path),
            ) {
                Some((_, live)) => live.is_some_and(|n| n.to_ascii_lowercase() == want),
                None => recorded(r),
            },
            None => recorded(r),
        }
    }))
}

/// One allowlist entry normalized the way the landing glob match
/// (`crate::review::glob_match`) expects: repo-relative, `/`-separated,
/// no absolute/traversal/control forms, and never a whole-repository
/// wildcard — `src/specific/**` is fine, `*`, `**`, `**/*` refuse.
fn normalize_path(input: &str) -> Result<String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(Error::rejected("an allowed path may not be empty"));
    }
    if raw.chars().any(char::is_control) || raw.contains('\\') {
        return Err(Error::rejected(format!(
            "allowed path '{raw}' — repo-relative `/` paths only"
        )));
    }
    if raw.starts_with('/')
        || raw.ends_with('/')
        || raw.starts_with("~/")
        || (raw.len() >= 2 && raw.as_bytes()[1] == b':')
    {
        return Err(Error::rejected(format!(
            "allowed path '{raw}' must be repo-relative — no absolute or drive paths"
        )));
    }
    if raw.split('/').any(|part| matches!(part, "" | "." | "..")) {
        return Err(Error::rejected(format!(
            "allowed path '{raw}' carries a ''/'.'/'..' segment — no traversal"
        )));
    }
    if raw.chars().all(|c| matches!(c, '*' | '/' | '?')) {
        return Err(Error::rejected(format!(
            "allowed path '{raw}' would match the whole repository — name a subtree \
             or file instead (`src/specific/**`, `docs/readme.md`)"
        )));
    }
    Ok(raw.to_string())
}

/// Sorted, de-duplicated normalized allowlist — the stored order the
/// grant and every run receipt compare byte-for-byte.
fn normalize_paths(values: &[String]) -> Result<Vec<String>> {
    if values.is_empty() {
        return Err(Error::rejected(
            "--unattended needs at least one --allow-path <repo-relative path>",
        ));
    }
    let mut out = BTreeSet::new();
    for v in values {
        out.insert(normalize_path(v)?);
    }
    Ok(out.into_iter().collect())
}

/// `inputs` as the renderer sees them — an object of strings.
fn input_map(value: Option<&Value>) -> Result<BTreeMap<String, String>> {
    match value {
        None | Some(Value::Null) => Ok(BTreeMap::new()),
        Some(Value::Object(map)) => {
            let mut out = BTreeMap::new();
            for (k, v) in map {
                let Some(s) = v.as_str() else {
                    return Err(Error::rejected(format!(
                        "input '{k}' must be a string — `--input {k}=<value>`"
                    )));
                };
                out.insert(k.clone(), s.to_string());
            }
            Ok(out)
        }
        Some(_) => Err(Error::rejected(
            "'inputs' must be an object of string values — `--input k=v`",
        )),
    }
}

/// The effective input values a render binds: the provided value when
/// nonempty, else the template default — the same rule [`wf::render`]
/// applies. Stored so the grant names exactly what was bound; the
/// render SHA-256 is what admission compares.
fn effective_inputs(
    template_text: &str,
    provided: &BTreeMap<String, String>,
) -> Result<Map<String, Value>> {
    let tpl = wf::parse_template(template_text)?;
    let mut out = Map::new();
    for (k, spec) in &tpl.inputs {
        let value = provided
            .get(k)
            .filter(|v| !v.is_empty())
            .cloned()
            .or_else(|| spec.default.clone())
            .unwrap_or_default();
        out.insert(k.clone(), Value::String(value));
    }
    Ok(out)
}

fn json_string_map(map: &BTreeMap<String, String>) -> Map<String, Value> {
    map.iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect()
}

/// A parsed, fully validated grant as stored inside the latest
/// `workflow_approved` payload's `unattended` object. Anything else —
/// missing key, extra key, wrong type, non-native provenance — parses
/// to a refusal and confers no authority.
struct Grant {
    grant_id: String,
    revision: u32,
    repo: String,
    paths: Vec<String>,
    inputs: BTreeMap<String, String>,
    template_sha256: String,
    render_sha256: String,
    ttl_secs: u64,
    expires_at: f64,
}

fn parse_grant(value: &Value, what: &str) -> Result<Grant> {
    let malformed = |detail: String| coded("unattended_malformed", format!("{what}: {detail}"));
    let Some(map) = value.as_object() else {
        return Err(malformed("the grant is not an object".to_string()));
    };
    for key in map.keys() {
        if !GRANT_KEYS.contains(&key.as_str()) {
            return Err(malformed(format!("unknown grant key '{key}'")));
        }
    }
    for key in GRANT_KEYS.iter().filter(|k| **k != "renewed") {
        if !map.contains_key(*key) {
            return Err(malformed(format!("grant is missing '{key}'")));
        }
    }
    if let Some(r) = map.get("renewed") {
        if r.as_bool() != Some(true) {
            return Err(malformed("'renewed' is not `true`".to_string()));
        }
    }
    if map["provenance"].as_str() != Some(PROVENANCE) {
        return Err(malformed("grant provenance is not native".to_string()));
    }
    let grant_id = map["grant_id"].as_str().unwrap_or_default().to_string();
    let revision = map["revision"]
        .as_u64()
        .and_then(|revision| u32::try_from(revision).ok())
        .ok_or_else(|| malformed("grant revision is not a bounded integer".to_string()))?;
    if !valid_grant_id(&grant_id) {
        return Err(malformed("grant_id is not a daemon-minted id".to_string()));
    }
    let repo = map["repo"].as_str().unwrap_or_default().to_string();
    if normalize_repo(&repo).map_err(|e| malformed(e.to_string()))? != repo {
        return Err(malformed("repo is not normalized".to_string()));
    }
    let paths: Vec<String> = map["paths"]
        .as_array()
        .ok_or_else(|| malformed("paths is not an array".to_string()))?
        .iter()
        .map(|v| {
            let s = v
                .as_str()
                .ok_or_else(|| malformed("a path is not a string".to_string()))?;
            if normalize_path(s).map_err(|e| malformed(e.to_string()))? != s {
                return Err(malformed("a path is not normalized".to_string()));
            }
            Ok(s.to_string())
        })
        .collect::<Result<_>>()?;
    if paths.is_empty() {
        return Err(malformed("paths is empty".to_string()));
    }
    let mut sorted = paths.clone();
    sorted.sort();
    sorted.dedup();
    if sorted != paths {
        return Err(malformed(
            "paths are not the sorted de-duplicated record".to_string(),
        ));
    }
    let inputs = map["inputs"]
        .as_object()
        .ok_or_else(|| malformed("inputs is not an object".to_string()))?
        .iter()
        .map(|(k, v)| {
            v.as_str()
                .map(|s| (k.clone(), s.to_string()))
                .ok_or_else(|| malformed("an input value is not a string".to_string()))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let template_sha256 = map["template_sha256"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let render_sha256 = map["render_sha256"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if !sha256_shape(&template_sha256) || !sha256_shape(&render_sha256) {
        return Err(malformed("a recorded digest is not a sha256".to_string()));
    }
    let ttl_secs = map["ttl_secs"].as_u64().unwrap_or_default();
    if !(TTL_MIN..=TTL_MAX).contains(&ttl_secs) {
        return Err(malformed("ttl_secs is outside 1..=86400".to_string()));
    }
    let issued_at = map["issued_at"].as_f64().unwrap_or_default();
    let expires_at = map["expires_at"].as_f64().unwrap_or_default();
    if !issued_at.is_finite()
        || !expires_at.is_finite()
        || expires_at != issued_at + ttl_secs as f64
    {
        return Err(malformed("issue/expiry times are inconsistent".to_string()));
    }
    Ok(Grant {
        grant_id,
        revision,
        repo,
        paths,
        inputs,
        template_sha256,
        render_sha256,
        ttl_secs,
        expires_at,
    })
}

/// A durable `unattended_run` mark — intent, held or admitted —
/// strictly parsed. A malformed mark never confers authority.
struct Mark {
    status: String,
    grant_id: String,
    project: String,
    workflow: String,
    repo: String,
    paths: Vec<String>,
    template_sha256: String,
    render_sha256: String,
    expires_at: f64,
    proposed_by: String,
    epic: String,
    tickets: Vec<String>,
    canonical: BTreeMap<String, String>,
    reason: Option<String>,
}

impl Mark {
    /// Every issue id this mark binds: the epic and each ticket.
    fn binds(&self, issue: &str) -> bool {
        self.epic == issue || self.tickets.iter().any(|t| t == issue)
    }

    fn ids(&self) -> Vec<&str> {
        let mut ids = vec![self.epic.as_str()];
        ids.extend(self.tickets.iter().map(String::as_str));
        ids
    }
}

fn parse_mark(value: &Value, seq: usize) -> Result<Mark> {
    let malformed = |detail: String| {
        coded(
            "unattended_malformed",
            format!("unattended run record #{seq}: {detail}"),
        )
    };
    let Some(map) = value.as_object() else {
        return Err(malformed("the record is not an object".to_string()));
    };
    for key in map.keys() {
        if !MARK_KEYS.contains(&key.as_str()) {
            return Err(malformed(format!("unknown record key '{key}'")));
        }
    }
    for key in MARK_KEYS.iter().filter(|k| **k != "reason") {
        if !map.contains_key(*key) {
            return Err(malformed(format!("record is missing '{key}'")));
        }
    }
    if map["provenance"].as_str() != Some(PROVENANCE) {
        return Err(malformed("record provenance is not native".to_string()));
    }
    let status = map["status"].as_str().unwrap_or_default().to_string();
    if !matches!(status.as_str(), MARK_INTENT | MARK_HELD | MARK_ADMITTED) {
        return Err(malformed(format!("unknown status '{status}'")));
    }
    let grant_id = map["grant_id"].as_str().unwrap_or_default().to_string();
    if !valid_grant_id(&grant_id) {
        return Err(malformed("grant_id is not a daemon-minted id".to_string()));
    }
    let project = map["project"].as_str().unwrap_or_default().to_string();
    model::check_key(&project).map_err(|e| malformed(e.to_string()))?;
    let workflow = map["workflow"].as_str().unwrap_or_default().to_string();
    if workflow.is_empty() || workflow.contains('/') {
        return Err(malformed(
            "workflow is not a stored-workflow name".to_string(),
        ));
    }
    let repo = map["repo"].as_str().unwrap_or_default().to_string();
    if normalize_repo(&repo).map_err(|e| malformed(e.to_string()))? != repo {
        return Err(malformed("repo is not normalized".to_string()));
    }
    let paths: Vec<String> = map["paths"]
        .as_array()
        .ok_or_else(|| malformed("paths is not an array".to_string()))?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| malformed("a path is not a string".to_string()))
        })
        .collect::<Result<_>>()?;
    let template_sha256 = map["template_sha256"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let render_sha256 = map["render_sha256"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if !sha256_shape(&template_sha256) || !sha256_shape(&render_sha256) {
        return Err(malformed("a recorded digest is not a sha256".to_string()));
    }
    let expires_at = map["expires_at"].as_f64().unwrap_or_default();
    if !expires_at.is_finite() || expires_at <= 0.0 {
        return Err(malformed("expires_at is not a positive epoch".to_string()));
    }
    let proposed_by = map["proposed_by"].as_str().unwrap_or_default().to_string();
    if proposed_by.is_empty() {
        return Err(malformed("proposed_by is empty".to_string()));
    }
    let epic = map["epic"].as_str().unwrap_or_default().to_string();
    model::check_id(&epic).map_err(|e| malformed(e.to_string()))?;
    let tickets: Vec<String> = map["tickets"]
        .as_array()
        .ok_or_else(|| malformed("tickets is not an array".to_string()))?
        .iter()
        .map(|v| {
            let s = v
                .as_str()
                .ok_or_else(|| malformed("a ticket id is not a string".to_string()))?;
            model::check_id(s).map_err(|e| malformed(e.to_string()))?;
            Ok(s.to_string())
        })
        .collect::<Result<_>>()?;
    if tickets.is_empty() || tickets.contains(&epic) {
        return Err(malformed(
            "the ticket list is empty or names the epic".to_string(),
        ));
    }
    let canonical: BTreeMap<String, String> = map["canonical"]
        .as_object()
        .ok_or_else(|| malformed("canonical is not an object".to_string()))?
        .iter()
        .map(|(k, v)| {
            v.as_str()
                .map(|s| (k.clone(), s.to_string()))
                .ok_or_else(|| malformed("a canonical value is not a string".to_string()))
        })
        .collect::<Result<_>>()?;
    let fingerprints: BTreeMap<String, String> = map["fingerprints"]
        .as_object()
        .ok_or_else(|| malformed("fingerprints is not an object".to_string()))?
        .iter()
        .map(|(k, v)| {
            v.as_str()
                .map(|s| (k.clone(), s.to_string()))
                .ok_or_else(|| malformed("a fingerprint is not a string".to_string()))
        })
        .collect::<Result<_>>()?;
    // The fingerprint map must cover exactly the bound ids, and every
    // fingerprint must be the sha256 of the stored canonical text — a
    // mark whose pair disagrees with itself is tampered, not binding.
    let expected: BTreeSet<&String> = canonical.keys().collect();
    if expected != fingerprints.keys().collect::<BTreeSet<_>>() {
        return Err(malformed(
            "canonical and fingerprint key sets differ".to_string(),
        ));
    }
    let bound: BTreeSet<&String> = std::iter::once(&epic).chain(tickets.iter()).collect();
    if expected != bound {
        return Err(malformed(
            "canonical does not cover exactly the bound ids".to_string(),
        ));
    }
    for (id, canon) in &canonical {
        if fingerprints.get(id.as_str()).map(String::as_str)
            != Some(sha256_prefixed(canon).as_str())
        {
            return Err(malformed(format!(
                "{id}'s fingerprint does not match its canonical"
            )));
        }
    }
    let reason = match map.get("reason") {
        None => None,
        Some(Value::String(s)) => Some(s.clone()),
        _ => return Err(malformed("reason is not a string".to_string())),
    };
    Ok(Mark {
        status,
        grant_id,
        project,
        workflow,
        repo,
        paths,
        template_sha256,
        render_sha256,
        expires_at,
        proposed_by,
        epic,
        tickets,
        canonical,
        reason,
    })
}

/// The native authorization an issue-bound automatic step consumes:
/// the operator grant that admitted the run, read back at decision
/// time. `expires_at` is the *live* grant's expiry — an operator
/// `renew` extends exactly this field and nothing else.
pub(super) struct AuthorizedRun {
    pub grant_id: String,
    pub project: String,
    pub epic: String,
    /// Normalized `owner/repo`.
    pub repo: String,
    /// Normalized repo-relative allowlist entries (glob-safe).
    pub paths: Vec<String>,
    pub expires_at: f64,
}

/// Live authority carried into a bound queue write, never from request fields.
pub(super) struct DispatchAuthority {
    pub checkout: String,
    pub expires_at: f64,
}

/// Validated workflow and worker snapshot shared by issuance and renewal.
struct ApprovalContext<'a> {
    pm_dir: &'a Path,
    project: &'a str,
    name: &'a str,
    text: &'a str,
    daemon_aliases: &'a [String],
}

/// The server-side bounds of a grant candidate or renewal — template
/// hash, exact render hash, effective inputs — computed once.
struct GrantBounds {
    template_sha256: String,
    render_sha256: String,
    effective: Map<String, Value>,
    warnings: Vec<crate::secret::Finding>,
}

impl Shared {
    /// Every `unattended_run` mark in order, strictly parsed. One
    /// malformed record poisons the whole read — an unreadable stream
    /// can never silently pass as "no binding".
    fn unattended_marks(&self) -> Result<Vec<Mark>> {
        self.store
            .unattended_run_payloads()?
            .iter()
            .enumerate()
            .map(|(i, v)| parse_mark(v, i + 1))
            .collect()
    }

    fn latest_mark<'a>(marks: &'a [Mark], grant_id: &str) -> Option<&'a Mark> {
        marks.iter().rev().find(|m| m.grant_id == grant_id)
    }

    fn validate_grant_history(&self, project: &str, name: &str, grant: &Grant) -> Result<()> {
        if !self
            .store
            .unattended_grant_current(project, name, &grant.grant_id, grant.revision)?
        {
            return Err(coded(
                "unattended_revoked",
                "a replaced grant or captured older renewal cannot regain authority; the operator must issue a fresh grant",
            ));
        }
        Ok(())
    }

    /// The current operator grant on this workflow approval.
    ///
    /// `Ok(None)` only when no grant exists to interrogate: no approval
    /// row, a foreign (non-`operator`) row, or no `unattended` object —
    /// callers then take their ordinary/absent path. Once an
    /// `unattended` field exists it must still be a well-formed, native,
    /// unexpired grant — a malformed, forged-provenance or expired one
    /// is an `Err`, never a silent "no grant" that would let a changed
    /// render land an ordinary backlog plan beside bound work.
    fn live_grant(&self, project: &str, name: &str) -> Result<Option<(Value, Grant)>> {
        let approvals = self.store.workflow_approvals()?;
        let Some(payload) = approvals.get(&wf::approval_key(project, name)) else {
            return Ok(None);
        };
        // The event must be the operator's own approval write — `by`
        // is daemon-recorded, never caller-supplied.
        if payload["by"].as_str() != Some("operator") {
            return Ok(None);
        }
        let Some(unattended) = payload.get("unattended").filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        let grant = parse_grant(unattended, "stored grant")?;
        self.validate_grant_history(project, name, &grant)?;
        if crate::issue::time::now_epoch() as f64 >= grant.expires_at {
            return Err(coded(
                "unattended_expired",
                format!(
                    "grant {} on {project}/{name} expired — the operator renews it with \
                     `workflow approve {name} --project {project} --renew {}`",
                    grant.grant_id, grant.grant_id
                ),
            ));
        }
        Ok(Some((payload.clone(), grant)))
    }

    /// Native run presence without tracker/Git reads. The caller's grant
    /// read guard keeps marks stable; malformed history must still refuse.
    pub(super) fn unattended_issue_bound(&self, issue: &str) -> Result<bool> {
        Ok(self
            .unattended_marks()?
            .iter()
            .any(|mark| mark.binds(issue)))
    }

    /// `unattended_authorization_for_issue` — the contract interface.
    ///
    /// `Ok(None)` only when no native unattended run ever bound this
    /// issue. A bound run whose grant is revoked (latest approval has
    /// no grant or a different grant id), expired, malformed, or whose
    /// bound issues' original semantics changed returns `Err` — an
    /// automatic dispatch/enqueue must refuse, never fall back to the
    /// plan's tracker approval. Read-only: no locks, no writes; safe
    /// under `dispatch_lock` (job gate) and `delivery_lock` (landing).
    pub(super) fn unattended_authorization_for_issue(
        &self,
        issue: &str,
    ) -> Result<Option<AuthorizedRun>> {
        model::check_id(issue).map_err(|e| {
            coded(
                "unattended_malformed",
                format!("'{issue}' is not an issue id ({e}) — no authority"),
            )
        })?;
        let marks = self.unattended_marks()?;
        let mut bound: BTreeSet<&str> = BTreeSet::new();
        for mark in &marks {
            if mark.binds(issue) {
                bound.insert(mark.grant_id.as_str());
            }
        }
        if bound.is_empty() {
            return Ok(None);
        }
        if bound.len() != 1 {
            return Err(coded(
                "unattended_malformed",
                format!("{issue} is bound by more than one unattended run — refused"),
            ));
        }
        let grant_id = *bound.iter().next().unwrap_or(&"");
        let mark = Self::latest_mark(&marks, grant_id)
            .ok_or_else(|| coded("unattended_malformed", "a bound grant has no latest record"))?;
        if !mark.binds(issue) {
            return Err(coded(
                "unattended_malformed",
                format!(
                    "{issue}'s earliest binding was grant {grant_id}, but its latest \
                     record no longer names it — refused"
                ),
            ));
        }
        if mark.status != MARK_ADMITTED {
            return Err(coded(
                "unattended_held",
                format!(
                    "{issue} is bound by an unacknowledged/held admission under grant \
                     {grant_id} — it stays held for reconciliation; a duplicate \
                     `plan propose --workflow` of the exact render reconciles it"
                ),
            ));
        }
        // A live grant for the same id: not revoked, not replaced.
        let (_approval, grant) = match self.live_grant(&mark.project, &mark.workflow)? {
            Some(pair) => pair,
            // `live_grant` already refuses a malformed or expired
            // `unattended` field; `None` here means the row has no live
            // grant at all — replaced or revoked.
            None => {
                return Err(coded(
                    "unattended_revoked",
                    format!(
                        "{issue}'s unattended run grant {grant_id} is revoked — the \
                         workflow's latest operator approval carries no such grant"
                    ),
                ));
            }
        };
        if grant.grant_id != grant_id {
            return Err(coded(
                "unattended_revoked",
                format!(
                    "{issue}'s unattended run grant {grant_id} was replaced by {} — \
                     only a fresh `plan propose --workflow` admits under the new one",
                    grant.grant_id
                ),
            ));
        }
        // The receipt's immutable bounds must equal the grant's — a
        // grant id that survived a scope rewrite is forged.
        if grant.repo != mark.repo
            || grant.paths != mark.paths
            || grant.template_sha256 != mark.template_sha256
            || grant.render_sha256 != mark.render_sha256
        {
            return Err(coded(
                "unattended_malformed",
                format!(
                    "{issue}'s run receipt and grant {grant_id} disagree on the \
                     immutable bounds — refused"
                ),
            ));
        }
        let pm_dir = self.pm_dir().map_err(|e| {
            coded(
                "unattended_malformed",
                format!("tracker dir for {issue} cannot be resolved ({e}) — refused"),
            )
        })?;
        // The bound template still on disk — a post-grant edit voids
        // the exact-render authority.
        let template = wf::read_for(&pm_dir, &mark.project, &mark.workflow).map_err(|e| {
            coded(
                "unattended_mismatch",
                format!("the bound workflow '{}' is unreadable ({e})", mark.workflow),
            )
        })?;
        if sha256_prefixed(&template) != grant.template_sha256 {
            return Err(coded(
                "unattended_mismatch",
                format!(
                    "workflow '{}' changed since grant {grant_id} — its bound render \
                     is void",
                    mark.workflow
                ),
            ));
        }
        if !registered_repo(&pm_dir, &mark.project, &mark.repo)? {
            return Err(coded(
                "unattended_mismatch",
                format!(
                    "{} is no longer a registered repository of {}",
                    mark.repo, mark.project
                ),
            ));
        }
        // Original semantics must read back unchanged: a ticket whose
        // title/body/worker/size/deps/membership moved is changed work,
        // never the authorized run.
        for id in mark.ids() {
            let loaded =
                crate::issue::board::load_issue(&pm_dir, &mark.project, id).map_err(|e| {
                    coded(
                        "unattended_mismatch",
                        format!("{id} of run grant {grant_id} is unreadable ({e}) — refused"),
                    )
                })?;
            let now = tracker::granted_semantics(&loaded.front, &loaded.body);
            if mark.canonical.get(id) != Some(&now) {
                return Err(coded(
                    "unattended_mismatch",
                    format!(
                        "{id}'s original semantics changed since run grant {grant_id} \
                         admitted it — refused; tracker metadata cannot re-authorize it"
                    ),
                ));
            }
        }
        Ok(Some(AuthorizedRun {
            grant_id: grant.grant_id.clone(),
            project: mark.project.clone(),
            epic: mark.epic.clone(),
            repo: mark.repo.clone(),
            paths: mark.paths.clone(),
            expires_at: grant.expires_at,
        }))
    }

    /// `workflow_approve` body, grant-aware. The caller has already
    /// passed `operator_connection`. Holds `unattended_lock` WRITE for
    /// the whole read-check-record so a replacement never interleaves
    /// an admission or an enqueue decision.
    pub(super) fn write_workflow_approval(&self, params: &Value) -> Result<Value> {
        let project = required_str(params, "project")?;
        model::check_key(project)?;
        let name = required_str(params, "name")?;
        for field in GRANT_TOP_FIELDS {
            if params.get(field).is_some() {
                return Err(Error::rejected(format!(
                    "'{field}' is grant material — it lives inside 'unattended' (or \
                     'renew'/'ttl_secs' on a renewal), never as a bare field"
                )));
            }
        }
        let unattended = params.get("unattended").filter(|v| !v.is_null());
        let renew = optional_str(params, "renew");
        let ttl_secs = params.get("ttl_secs");
        if unattended.is_some() && renew.is_some() {
            return Err(Error::rejected(
                "'unattended' and 'renew' are exclusive — a fresh grant or a renewal",
            ));
        }
        if renew.is_none() && unattended.is_none() && ttl_secs.is_some() {
            return Err(Error::rejected(
                "'ttl_secs' is grant material — pass it inside 'unattended' or with 'renew'",
            ));
        }
        let _write = self
            .unattended_lock
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let pm_dir = self.pm_dir()?;
        if !crate::issue::project::list(&pm_dir)?
            .iter()
            .any(|p| p.key == project)
        {
            return Err(crate::issue::project::unknown_project(project, &pm_dir));
        }
        let text = wf::read_for(&pm_dir, project, name)?;
        // Every approval path keeps the current `workflow check` gate —
        // a workflow that cannot be proposed cannot hold a grant.
        let aliases: Vec<String> = self
            .store
            .agents()?
            .iter()
            .map(|a| a.alias.clone())
            .collect();
        let (agents, sources) = wf::known_agents(&pm_dir, Some(project), &aliases);
        let (errors, notes, _) = wf::check_text(&text, &agents, &sources);
        if !errors.is_empty() {
            return Err(Error::rejected(format!(
                "workflow '{name}' fails `workflow check` — approve it only after these \
                 are fixed: {}",
                errors.join("; ")
            )));
        }
        let now = crate::issue::time::now_epoch();
        let mut payload = json!({
            "project": project,
            "name": name,
            "digest": wf::gate_digest(&text)?,
            "by": "operator",
            "at": crate::issue::time::iso(now),
            "notes": notes,
        });
        let mut warnings: Vec<crate::secret::Finding> = Vec::new();
        let approval = ApprovalContext {
            pm_dir: &pm_dir,
            project,
            name,
            text: &text,
            daemon_aliases: &aliases,
        };
        if let Some(object) = unattended {
            let (grant, found) = self.build_grant(&approval, object, now)?;
            payload["unattended"] = grant;
            warnings = found;
        } else if let Some(grant_id) = renew {
            let ttl = match ttl_secs {
                None | Some(Value::Null) => None,
                Some(v) => Some(
                    v.as_u64()
                        .filter(|n| (TTL_MIN..=TTL_MAX).contains(n))
                        .ok_or_else(|| {
                            Error::rejected("'ttl_secs' must be an integer in 1..=86400")
                        })?,
                ),
            };
            let (grant, found) = self.renew_grant(&approval, grant_id, ttl, now)?;
            payload["unattended"] = grant;
            warnings = found;
        }
        if !warnings.is_empty() {
            payload["secret_warnings"] = crate::secret::warnings_json(&warnings);
        }
        // `record_workflow_approval` appends; the latest record wins, so
        // this write is also the replacement/revocation boundary the
        // contract serializes with admission and enqueue.
        self.store.record_workflow_approval(payload.clone())?;
        Ok(payload)
    }

    /// Build the strict grant object for `approve --unattended`. Every
    /// bound is derived server-side; caller content is only repo,
    /// paths, inputs and ttl.
    fn build_grant(
        &self,
        approval: &ApprovalContext<'_>,
        object: &Value,
        now: i64,
    ) -> Result<(Value, Vec<crate::secret::Finding>)> {
        let &ApprovalContext {
            pm_dir,
            project,
            name,
            text,
            daemon_aliases,
        } = approval;
        let Some(map) = object.as_object() else {
            return Err(Error::rejected(
                "'unattended' must be an object {repo, paths, inputs, ttl_secs}",
            ));
        };
        for key in map.keys() {
            if !GRANT_RPC_KEYS.contains(&key.as_str()) {
                return Err(Error::rejected(format!(
                    "unknown unattended field '{key}' — allowed: repo, paths, \
                     inputs, ttl_secs"
                )));
            }
        }
        for required in ["repo", "paths", "inputs"] {
            if !map.contains_key(required) {
                return Err(Error::rejected(format!(
                    "--unattended needs '{required}' (`--repo`, `--allow-path`, \
                     `--input`)"
                )));
            }
        }
        let repo = normalize_repo(map["repo"].as_str().unwrap_or_default())?;
        if !registered_repo(pm_dir, project, &repo)? {
            return Err(Error::rejected(format!(
                "--repo {repo} is not a registered repository of project '{project}' — \
                 `cadence issue project show {project}` lists them"
            )));
        }
        let raw_paths: Vec<String> = map["paths"]
            .as_array()
            .ok_or_else(|| {
                Error::rejected("'paths' must be an array of repo-relative paths/globs")
            })?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| Error::rejected("each allowed path must be a string"))
            })
            .collect::<Result<_>>()?;
        let paths = normalize_paths(&raw_paths)?;
        let ttl_secs = match map.get("ttl_secs") {
            None | Some(Value::Null) => TTL_DEFAULT,
            Some(v) => v
                .as_u64()
                .filter(|n| (TTL_MIN..=TTL_MAX).contains(n))
                .ok_or_else(|| Error::rejected("'ttl_secs' must be an integer in 1..=86400"))?,
        };
        let provided = input_map(map.get("inputs"))?;
        let bounds = self.grant_bounds(pm_dir, project, name, text, &provided, daemon_aliases)?;
        let grant_id = fresh_grant_id()?;
        Ok((
            json!({
                "provenance": PROVENANCE,
                "grant_id": grant_id,
                "revision": 0,
                "repo": repo,
                "paths": paths,
                "inputs": bounds.effective,
                "template_sha256": bounds.template_sha256,
                "render_sha256": bounds.render_sha256,
                "ttl_secs": ttl_secs,
                "issued_at": now as f64,
                "expires_at": now as f64 + ttl_secs as f64,
            }),
            bounds.warnings,
        ))
    }

    /// The immutable bounds of a grant candidate or renewal — template
    /// hash, exact render hash, effective inputs — with the shared
    /// refusals: the workflow must execute on workers, every rendered
    /// ticket must name a still-known, daemon-registered worker alias
    /// (validated again here so a removed or never-registered worker
    /// cannot slip under an approved template), and the render passes
    /// the same secret scan as a proposal.
    fn grant_bounds(
        &self,
        pm_dir: &Path,
        project: &str,
        name: &str,
        text: &str,
        provided: &BTreeMap<String, String>,
        daemon_aliases: &[String],
    ) -> Result<GrantBounds> {
        let tpl = wf::parse_template(text)?;
        if tpl.execution != wf::Execution::Agent {
            return Err(Error::rejected(format!(
                "workflow '{name}' executes on the host — an unattended grant needs a \
                 worker team, so it cannot be granted"
            )));
        }
        let rendered = wf::render(text, provided)?;
        let doc = crate::issue::plan::parse_plan(&rendered)?;
        // Rendered workers must be known aliases AND registered daemon
        // agents — `check_text` on the template cannot prove a
        // `{{input}}` agent, so the grant checks the concrete render.
        let (known, sources) = wf::known_agents(pm_dir, Some(project), daemon_aliases);
        for (n, ticket) in doc.tickets.iter().enumerate() {
            let label = format!("ticket {} \"{}\"", n + 1, ticket.title);
            let Some(agent) = ticket.agent.as_deref() else {
                return Err(Error::rejected(format!(
                    "{label} has no `agent:` — an unattended run needs a known worker \
                     on every ticket"
                )));
            };
            if !known.contains(agent) {
                return Err(Error::rejected(format!(
                    "{label}: agent '{agent}' is unknown — not in {}",
                    sources.join(", ")
                )));
            }
            if !daemon_aliases.iter().any(|a| a == agent) {
                return Err(Error::rejected(format!(
                    "{label}: agent '{agent}' is not a registered worker — an \
                     unattended run needs daemon-registered workers only"
                )));
            }
        }
        let allow = crate::secret::Allowlist::load(&self.state_dir)?;
        let warnings = crate::secret::guard_with("plan", &rendered, &allow)?;
        Ok(GrantBounds {
            template_sha256: sha256_prefixed(text),
            render_sha256: sha256_prefixed(&rendered),
            effective: effective_inputs(text, provided)?,
            warnings,
        })
    }

    /// `renew <grant-id>`: re-issue the SAME grant — same id, same
    /// immutable bounds — with a fresh expiry. Any drift (template,
    /// render, repo, paths, inputs, a replaced or absent grant)
    /// refuses; only `issued_at`/`ttl_secs`/`expires_at` move. The
    /// bound run's receipt keeps its original fingerprints.
    fn renew_grant(
        &self,
        approval: &ApprovalContext<'_>,
        grant_id: &str,
        ttl: Option<u64>,
        now: i64,
    ) -> Result<(Value, Vec<crate::secret::Finding>)> {
        let &ApprovalContext {
            pm_dir,
            project,
            name,
            text,
            daemon_aliases,
        } = approval;
        if !valid_grant_id(grant_id) {
            return Err(Error::rejected(
                "--renew names a daemon-minted grant id (`grant-…`), printed by the \
                 original `--unattended` approval",
            ));
        }
        let approvals = self.store.workflow_approvals()?;
        let Some(payload) = approvals.get(&wf::approval_key(project, name)) else {
            return Err(Error::rejected(format!(
                "workflow '{name}' has no approval to renew — approve it first"
            )));
        };
        let Some(unattended) = payload.get("unattended") else {
            return Err(Error::rejected(format!(
                "workflow '{name}'s latest approval carries no unattended grant — the \
                 grant was revoked; only a fresh `--unattended` approval issues one"
            )));
        };
        let grant = parse_grant(unattended, "stored grant").map_err(|e| {
            Error::rejected(format!(
                "the stored grant is malformed ({e}) — it confers nothing and cannot \
                 be renewed; approve `--unattended` fresh"
            ))
        })?;
        self.validate_grant_history(project, name, &grant)?;
        if grant.grant_id != grant_id {
            return Err(Error::rejected(format!(
                "grant {grant_id} is not the live grant ({}) — a replaced grant is \
                 revoked for good; `--unattended` issues a fresh one",
                grant.grant_id
            )));
        }
        let bounds =
            self.grant_bounds(pm_dir, project, name, text, &grant.inputs, daemon_aliases)?;
        if bounds.template_sha256 != grant.template_sha256
            || bounds.render_sha256 != grant.render_sha256
            || bounds.effective != json_string_map(&grant.inputs)
        {
            return Err(Error::rejected(format!(
                "workflow '{name}' no longer renders the bound plan — grant {grant_id} \
                 cannot be renewed over changed wording/template/inputs; approve a \
                 fresh `--unattended` grant instead"
            )));
        }
        let ttl_secs = ttl.unwrap_or(grant.ttl_secs);
        let revision = grant.revision.checked_add(1).ok_or_else(|| {
            Error::rejected("grant renewal revision is exhausted; issue a fresh grant")
        })?;
        Ok((
            json!({
                "provenance": PROVENANCE,
                "grant_id": grant.grant_id,
                "revision": revision,
                "repo": grant.repo,
                "paths": grant.paths,
                "inputs": json_string_map(&grant.inputs),
                "template_sha256": grant.template_sha256,
                "render_sha256": grant.render_sha256,
                "ttl_secs": ttl_secs,
                "issued_at": now as f64,
                "expires_at": now as f64 + ttl_secs as f64,
                "renewed": true,
            }),
            bounds.warnings,
        ))
    }

    /// `plan_propose --workflow` admission: when a live grant exactly
    /// binds this render, materialize the approved plan + ready tickets
    /// in one tracker commit and return the result. `Ok(None)` means
    /// "no unattended admission" — the caller falls back to the
    /// ordinary proposed-plan path unchanged. Any live-grant refusal
    /// (worker gone, held intent, diverged recovery) is an `Err` —
    /// never a silent ordinary proposal of bound work.
    ///
    /// Holds `unattended_lock` WRITE end-to-end: the grant is re-read
    /// under it so a replacement cannot slip between check and
    /// reservation, and `create_granted_plan` takes the PM write lock
    /// inside the same critical section while the durable intent lands
    /// before any tracker file.
    pub(super) fn admit_unattended_workflow(
        &self,
        project: &str,
        name: &str,
        provided: &BTreeMap<String, String>,
        actor: &str,
    ) -> Result<Option<Value>> {
        // App workflows stay manual — the `<app>/<wf>` path owns them.
        if name.contains('/') {
            return Ok(None);
        }
        let _write = self
            .unattended_lock
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let pm_dir = self.pm_dir()?;
        let text = wf::read_for(&pm_dir, project, name)?;
        let Some((approval, grant)) = self.live_grant(project, name)? else {
            return Ok(None);
        };
        // Once a live grant exists it OWNS this workflow's proposals —
        // nothing silently lands an ordinary backlog plan beside the
        // bound render (zero side effects on a refused proposal).
        if approval["digest"].as_str() != Some(wf::gate_digest(&text)?.as_str()) {
            return Err(coded(
                "unattended_mismatch",
                format!(
                    "workflow '{name}' was edited after grant {} — the recorded gate \
                     digest no longer matches; the operator approves a fresh grant",
                    grant.grant_id
                ),
            ));
        }
        if grant.template_sha256 != sha256_prefixed(&text) {
            return Err(coded(
                "unattended_mismatch",
                format!(
                    "workflow '{name}' no longer matches the template grant {} bound — \
                     its render is void; the operator approves a fresh grant",
                    grant.grant_id
                ),
            ));
        }
        let rendered = wf::render(&text, provided)?;
        if sha256_prefixed(&rendered) != grant.render_sha256 {
            return Err(coded(
                "unattended_mismatch",
                format!(
                    "these inputs do not render the plan grant {} bound — the proposal \
                     is refused; it can only auto-admit byte-identically",
                    grant.grant_id
                ),
            ));
        }
        // The exact render is bound — everything below refuses rather
        // than falling back to an unapproved proposal.
        if !registered_repo(&pm_dir, project, &grant.repo)? {
            return Err(coded(
                "unattended_revoked",
                format!(
                    "{} is no longer a registered repository of {project} — grant {} \
                     admits nothing",
                    grant.repo, grant.grant_id
                ),
            ));
        }
        let aliases: Vec<String> = self
            .store
            .agents()?
            .iter()
            .map(|a| a.alias.clone())
            .collect();
        let bounds = self.grant_bounds(&pm_dir, project, name, &text, provided, &aliases)?;
        let doc = crate::issue::plan::parse_plan(&rendered)?;
        if doc.tickets.len() > crate::issue::plan::MAX_TICKETS {
            return Err(Error::rejected(format!(
                "Plan has {} tickets — at most {}",
                doc.tickets.len(),
                crate::issue::plan::MAX_TICKETS
            )));
        }
        let pm = self.pm()?;
        let marks = self.unattended_marks()?;
        let mut out = match Self::latest_mark(&marks, &grant.grant_id) {
            Some(mark) if mark.status == MARK_HELD => {
                return Err(coded(
                    "unattended_held",
                    format!(
                        "grant {}'s earlier admission is held: {} — reconcile it \
                         before proposing again; nothing new was allocated",
                        grant.grant_id,
                        mark.reason.as_deref().unwrap_or("unknown partial outcome")
                    ),
                ));
            }
            Some(mark) if mark.status == MARK_ADMITTED => {
                // One grant, one plan: verify the admitted run still
                // reads back exactly, then return the same ids.
                self.verify_mark(&pm_dir, mark)?;
                let epic =
                    crate::issue::board::load_issue(&pm_dir, project, &mark.epic).map_err(|e| {
                        coded(
                            "unattended_mismatch",
                            format!("the admitted epic {} is unreadable ({e})", mark.epic),
                        )
                    })?;
                let plan = epic.front.plan.clone();
                json!({
                    "epic": mark.epic.clone(),
                    "project": project,
                    "title": epic.front.title,
                    "state": "approved",
                    "proposed_by": mark.proposed_by,
                    "decided_by": grant_decision(&grant.grant_id),
                    "decided_at": plan.and_then(|p| p.decided_at),
                    "tickets": mark.tickets.clone(),
                    "ready": mark.tickets.clone(),
                    "committed": true,
                    "reused": true,
                })
            }
            Some(mark) => self.recover_mark(&pm, project, name, &doc, mark, &grant)?,
            None => self.fresh_admission(&pm, project, name, &doc, &grant, actor)?,
        };
        out["grant_id"] = json!(grant.grant_id);
        // The LIVE expiry (renewal extends it) — master wake recovery
        // keys the native granted plan on epic/grant_id/grant_expires_at.
        out["grant_expires_at"] = json!(grant.expires_at);
        out["unattended"] = json!(true);
        if !bounds.warnings.is_empty() {
            out["secret_warnings"] = crate::secret::warnings_json(&bounds.warnings);
        }
        Ok(Some(out))
    }

    /// Re-prove the live grant under the PM write lock, immediately
    /// before the durable intent or recovery write — the admission's
    /// `unattended_lock` section could not stop the grant expiring, an
    /// operator replacing the approval, or ordinary tracker edits
    /// racing the lock wait. The live grant must still exist with the
    /// same id and every immutable bound (template, render, repo,
    /// paths, inputs), and the repo plus the rendered workers must
    /// still pass `grant_bounds` — a refusal records nothing.
    fn revalidate_grant(
        &self,
        pm_dir: &Path,
        project: &str,
        name: &str,
        grant: &Grant,
    ) -> Result<()> {
        let Some((_approval, live)) = self.live_grant(project, name)? else {
            return Err(coded(
                "unattended_revoked",
                format!(
                    "grant {} on {project}/{name} was revoked or replaced while waiting \
                     for the tracker lock",
                    grant.grant_id
                ),
            ));
        };
        if live.grant_id != grant.grant_id
            || live.template_sha256 != grant.template_sha256
            || live.render_sha256 != grant.render_sha256
            || live.repo != grant.repo
            || live.paths != grant.paths
            || live.inputs != grant.inputs
        {
            return Err(coded(
                "unattended_revoked",
                format!(
                    "grant {}'s bounds changed while waiting for the tracker lock — \
                     refused, nothing recorded",
                    grant.grant_id
                ),
            ));
        }
        if !registered_repo(pm_dir, project, &grant.repo)? {
            return Err(coded(
                "unattended_revoked",
                format!(
                    "{} is no longer a registered repository of {project} — grant {} \
                     admits nothing",
                    grant.repo, grant.grant_id
                ),
            ));
        }
        // Template/render hashes and the rendered-worker registry,
        // again — `grant.inputs` is the effective map stored at
        // issuance, so the re-derived bounds must byte-match or the
        // grant's render changed.
        let text = wf::read_for(pm_dir, project, name)?;
        let aliases: Vec<String> = self
            .store
            .agents()?
            .iter()
            .map(|a| a.alias.clone())
            .collect();
        let bounds = self.grant_bounds(pm_dir, project, name, &text, &grant.inputs, &aliases)?;
        if bounds.template_sha256 != grant.template_sha256
            || bounds.render_sha256 != grant.render_sha256
            || bounds.effective != json_string_map(&grant.inputs)
        {
            return Err(coded(
                "unattended_mismatch",
                format!(
                    "grant {}'s bound template/render/inputs drifted while waiting for \
                     the tracker lock — refused, nothing recorded",
                    grant.grant_id
                ),
            ));
        }
        Ok(())
    }

    /// First admission under a grant: reserve ids, record the durable
    /// intent inside the PM lock before any tracker file exists, commit
    /// the approved plan in one shot, then record the `admitted`
    /// receipt.
    fn fresh_admission(
        &self,
        pm: &crate::issue::Pm,
        project: &str,
        name: &str,
        doc: &crate::issue::plan::PlanDoc,
        grant: &Grant,
        actor: &str,
    ) -> Result<Value> {
        let mut intent: Option<Value> = None;
        let out = tracker::create_granted_plan(
            pm,
            project,
            doc,
            Some(name),
            actor,
            &grant_decision(&grant.grant_id),
            None,
            None,
            true,
            &|| self.store.unattended_reserved_ids(),
            &mut |preview: &tracker::GrantedPreview| {
                // Re-prove the live grant under the PM lock — the
                // unattended_lock section could not stop expiry,
                // revocation or tracker edits racing the lock wait.
                self.revalidate_grant(&pm.dir, project, name, grant)?;
                let payload = self.mark_payload(grant, project, name, preview, actor);
                self.store.record_unattended_run(payload.clone())?;
                intent = Some(payload);
                Ok(())
            },
        )?;
        let Some(intent) = intent else {
            // `create_granted_plan` writes only after the callback ran,
            // so a created plan always has its intent on record.
            return Err(coded(
                "unattended_malformed",
                "admission committed without a recorded intent — held",
            ));
        };
        self.store.record_unattended_run(intent_receipt(&intent))?;
        Ok(out)
    }

    /// Continue (or hold) an `intent` mark: rebuild the same plan with
    /// the same ids and the original proposer, require the rebuilt
    /// canonical to equal the recorded one, then finish exactly once —
    /// absent files are written; matching files still require the scoped
    /// commit before the receipt. Divergent files are held, never re-allocated.
    fn recover_mark(
        &self,
        pm: &crate::issue::Pm,
        project: &str,
        name: &str,
        doc: &crate::issue::plan::PlanDoc,
        mark: &Mark,
        grant: &Grant,
    ) -> Result<Value> {
        let result = tracker::create_granted_plan(
            pm,
            project,
            doc,
            Some(name),
            &mark.proposed_by,
            &grant_decision(&grant.grant_id),
            Some((mark.epic.as_str(), mark.tickets.as_slice())),
            Some(&mark.canonical),
            true,
            &|| Ok(HashSet::new()),
            &mut |_| self.revalidate_grant(&pm.dir, project, name, grant),
        );
        match result {
            Ok(out) => {
                // Files were created or verified whole under the intent;
                // the receipt lands now. The intent stays in history.
                self.store
                    .record_unattended_run(self.receipt_of(mark, MARK_ADMITTED, None))?;
                Ok(out)
            }
            Err(e) => {
                let reason = e.to_string();
                self.store.record_unattended_run(self.receipt_of(
                    mark,
                    MARK_HELD,
                    Some(&reason),
                ))?;
                Err(coded(
                    "unattended_held",
                    format!(
                        "grant {}'s admission is held: {reason} — its ids stay \
                         reserved; nothing new was allocated",
                        grant.grant_id
                    ),
                ))
            }
        }
    }

    /// The intent payload for a fresh admission — written before any
    /// tracker file exists, inside the PM lock.
    fn mark_payload(
        &self,
        grant: &Grant,
        project: &str,
        name: &str,
        preview: &tracker::GrantedPreview,
        actor: &str,
    ) -> Value {
        let fingerprints: Map<String, Value> = preview
            .semantics
            .iter()
            .map(|(id, canon)| (id.clone(), json!(sha256_prefixed(canon))))
            .collect();
        let canonical: Map<String, Value> = preview
            .semantics
            .iter()
            .map(|(id, canon)| (id.clone(), json!(canon)))
            .collect();
        json!({
            "provenance": PROVENANCE,
            "status": MARK_INTENT,
            "grant_id": grant.grant_id,
            "project": project,
            "workflow": name,
            "repo": grant.repo,
            "paths": grant.paths,
            "template_sha256": grant.template_sha256,
            "render_sha256": grant.render_sha256,
            "expires_at": grant.expires_at,
            "proposed_by": actor,
            "epic": preview.epic,
            "tickets": preview.tickets,
            "canonical": canonical,
            "fingerprints": fingerprints,
        })
    }

    /// A `held`/`admitted` mark from an existing intent or receipt —
    /// the immutable fields carry over verbatim; only status (and a
    /// held reason) change.
    fn receipt_of(&self, mark: &Mark, status: &str, reason: Option<&str>) -> Value {
        let fingerprints: Map<String, Value> = mark
            .canonical
            .iter()
            .map(|(id, canon)| (id.clone(), json!(sha256_prefixed(canon))))
            .collect();
        let canonical: Map<String, Value> = mark
            .canonical
            .iter()
            .map(|(id, canon)| (id.clone(), json!(canon)))
            .collect();
        let mut payload = json!({
            "provenance": PROVENANCE,
            "status": status,
            "grant_id": mark.grant_id,
            "project": mark.project,
            "workflow": mark.workflow,
            "repo": mark.repo,
            "paths": mark.paths,
            "template_sha256": mark.template_sha256,
            "render_sha256": mark.render_sha256,
            "expires_at": mark.expires_at,
            "proposed_by": mark.proposed_by,
            "epic": mark.epic,
            "tickets": mark.tickets,
            "canonical": canonical,
            "fingerprints": fingerprints,
        });
        if let Some(reason) = reason {
            payload["reason"] = json!(reason);
        }
        payload
    }

    /// Verify an admitted run's files still carry the recorded original
    /// semantics — the readback half of the authorization check.
    fn verify_mark(&self, pm_dir: &Path, mark: &Mark) -> Result<()> {
        for id in mark.ids() {
            let loaded =
                crate::issue::board::load_issue(pm_dir, &mark.project, id).map_err(|e| {
                    coded(
                        "unattended_mismatch",
                        format!("{id} of run grant {} is unreadable ({e})", mark.grant_id),
                    )
                })?;
            let now = tracker::granted_semantics(&loaded.front, &loaded.body);
            if mark.canonical.get(id) != Some(&now) {
                return Err(coded(
                    "unattended_mismatch",
                    format!(
                        "{id}'s original semantics changed since grant {} admitted it",
                        mark.grant_id
                    ),
                ));
            }
        }
        Ok(())
    }
}

/// A recorded intent payload flipped to `admitted` — the receipt keeps
/// the intent's immutable fields verbatim.
fn intent_receipt(intent: &Value) -> Value {
    let mut receipt = intent.clone();
    receipt["status"] = json!(MARK_ADMITTED);
    receipt
}
