//! Issue vocabulary, readiness rules and the board card type, shared by the
//! daemon and hosted. Pure: no filesystem; dependency state is an input.
use crate::error::{Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Every issue status; `dropped` issues leave the board.
pub const STATUSES: &[&str] = &["backlog", "ready", "doing", "review", "done", "dropped"];
pub const PRIORITIES: &[&str] = &["P0", "P1", "P2", "P3"];
/// Most tags one issue may carry.
pub const TAG_MAX: usize = 12;
/// Statuses a blocker must reach to count as closed.
const CLOSED: &[&str] = &["done", "dropped"];

/// Issue id grammar: `<PREFIX>-<n>`, prefix uppercase letters/digits
/// starting with a letter, n a positive number. Validated before any
/// path is built from an id.
pub fn valid_id(id: &str) -> bool {
    let Some((prefix, num)) = id.rsplit_once('-') else {
        return false;
    };
    !prefix.is_empty()
        && prefix.len() <= 16
        && prefix.starts_with(|c: char| c.is_ascii_uppercase())
        && prefix
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && !num.is_empty()
        && num.len() <= 9
        && num.chars().all(|c| c.is_ascii_digit())
        && num != "0"
}

pub fn check_id(id: &str) -> Result<String> {
    if valid_id(id) {
        Ok(id.to_string())
    } else {
        Err(Error::rejected(format!(
            "Invalid issue id '{id}' — expected <PREFIX>-<n> like CAD-16"
        )))
    }
}

/// A project key or alias-safe name: lowercase letters, digits, hyphens.
pub fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 32
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !key.starts_with('-')
}

/// Keys a project may never take: `<pm>/agents/` holds the agent files
/// (AGENT-FILESYSTEM.md; the master's `agents/master/`, CAD-339).
pub const RESERVED_KEYS: &[&str] = &["agents"];

pub fn check_key(key: &str) -> Result<String> {
    if valid_key(key) {
        Ok(key.to_string())
    } else {
        Err(Error::rejected(format!(
            "Invalid project key '{key}' — 1-32 lowercase letters, digits or hyphens"
        )))
    }
}

/// A key a new project may take: well formed and not reserved.
pub fn check_new_key(key: &str) -> Result<()> {
    check_key(key)?;
    if RESERVED_KEYS.contains(&key) {
        return Err(Error::rejected(format!(
            "Project key '{key}' is reserved — <pm>/{key}/ holds the agent files"
        )));
    }
    Ok(())
}

/// `CADENCE` → valid; an issue prefix is 1-8 uppercase letters/digits
/// starting with a letter.
pub fn check_prefix(prefix: &str) -> Result<()> {
    let ok = !prefix.is_empty()
        && prefix.len() <= 8
        && prefix
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && prefix.starts_with(|c: char| c.is_ascii_uppercase());
    if ok {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "Invalid prefix '{prefix}' — uppercase letters/digits starting with a letter"
        )))
    }
}

/// The key's first three letters/digits, uppercased (`reminders` →
/// `REM`); a key that starts with a digit needs `--prefix`.
pub fn derive_prefix(key: &str) -> Result<String> {
    let prefix: String = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(3)
        .collect::<String>()
        .to_ascii_uppercase();
    check_prefix(&prefix).map_err(|_| {
        Error::rejected(format!(
            "Cannot derive an issue prefix from '{key}' — pass --prefix"
        ))
    })?;
    Ok(prefix)
}

pub fn check_status(status: &str) -> Result<()> {
    if STATUSES.contains(&status) {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "Unknown status '{status}' — one of {}",
            STATUSES.join(" ")
        )))
    }
}

pub fn check_priority(priority: &str) -> Result<()> {
    if PRIORITIES.contains(&priority) {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "Unknown priority '{priority}' — one of {}",
            PRIORITIES.join(" ")
        )))
    }
}

/// Tag grammar: `[a-z0-9][a-z0-9-]{0,31}`.
pub fn valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 32
        && !tag.starts_with('-')
        && tag
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The stored shape of a tag list: every tag well-formed, sorted,
/// de-duplicated, at most `TAG_MAX`.
pub fn normalize_tags(tags: &[String]) -> Result<Vec<String>> {
    for tag in tags {
        if !valid_tag(tag) {
            return Err(Error::rejected(format!(
                "Invalid tag '{tag}' — 1-32 lowercase letters, digits or hyphens, \
                 not starting with a hyphen"
            )));
        }
    }
    let mut out = tags.to_vec();
    out.sort();
    out.dedup();
    if out.len() > TAG_MAX {
        return Err(Error::rejected(format!(
            "{} tags — an issue carries at most {TAG_MAX}",
            out.len()
        )));
    }
    Ok(out)
}

/// A `blocked_by` target and its current status. A target that cannot be
/// found counts as open: pass any status that is not `done` or `dropped`.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Blocker {
    pub id: String,
    pub status: String,
}

pub fn is_closed(status: &str) -> bool {
    CLOSED.contains(&status)
}

/// The leaf rule: `backlog`/`ready` and not blocked.
pub fn leaf_ready(status: &str, blocked: bool) -> bool {
    matches!(status, "backlog" | "ready") && !blocked
}

fn open_ids(blockers: &[Blocker]) -> Vec<&str> {
    blockers
        .iter()
        .filter(|b| !is_closed(&b.status))
        .map(|b| b.id.as_str())
        .collect()
}

/// `status=ready` refuses while a `blocked_by` target is still open (CAD-757).
pub fn check_ready(id: &str, blockers: &[Blocker]) -> Result<()> {
    let open = open_ids(blockers);
    if open.is_empty() {
        return Ok(());
    }
    Err(Error::rejected(format!(
        "{id}: status=ready but blocked_by {} still open — \
         the queue skips blocked tickets anyway; land {} or \
         `cadence issue unlink {id} blocked_by <id>` first",
        open.join(", "),
        if open.len() == 1 { "it" } else { "them" },
    )))
}

/// Input of `issue.validate`: every field is optional and checked when set.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidateInput {
    pub id: Option<String>,
    pub project_key: Option<String>,
    pub tags: Option<Vec<String>>,
    pub status: Option<String>,
    pub priority: Option<String>,
}

/// One refused field.
#[derive(Serialize, JsonSchema)]
pub struct FieldRefusal {
    pub field: String,
    pub message: String,
}

/// Success value of `issue.validate`.
#[derive(Serialize, JsonSchema)]
pub struct ValidateValue {
    pub valid: bool,
    /// Sorted, de-duplicated tags; present when `tags` was given and valid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    pub refusals: Vec<FieldRefusal>,
}

pub fn validate(input: &ValidateInput) -> ValidateValue {
    let mut refusals = Vec::new();
    let mut note = |field: &str, result: Result<()>| {
        if let Err(error) = result {
            refusals.push(FieldRefusal {
                field: field.into(),
                message: error.to_string(),
            });
        }
    };
    if let Some(id) = &input.id {
        note("id", check_id(id).map(drop));
    }
    if let Some(key) = &input.project_key {
        note("project_key", check_new_key(key));
    }
    if let Some(status) = &input.status {
        note("status", check_status(status));
    }
    if let Some(priority) = &input.priority {
        note("priority", check_priority(priority));
    }
    let mut tags = None;
    if let Some(given) = &input.tags {
        match normalize_tags(given) {
            Ok(normalized) => tags = Some(normalized),
            Err(error) => note("tags", Err(error)),
        }
    }
    ValidateValue {
        valid: refusals.is_empty(),
        tags,
        refusals,
    }
}

/// Input of `issue.readiness`. `id` names the issue in the refusal.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadinessInput {
    pub id: String,
    pub status: String,
    pub blockers: Vec<Blocker>,
}

/// Success value of `issue.readiness`.
#[derive(Serialize, JsonSchema)]
pub struct ReadinessValue {
    pub ready: bool,
    pub blocked: bool,
    /// Why `status=ready` would be refused; present only then.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
}

pub fn readiness(input: &ReadinessInput) -> ReadinessValue {
    let blocked = !open_ids(&input.blockers).is_empty();
    let refusal = if input.status == "ready" {
        check_ready(&input.id, &input.blockers)
            .err()
            .map(|error| error.to_string())
    } else {
        None
    };
    ReadinessValue {
        ready: leaf_ready(&input.status, blocked),
        blocked,
        refusal,
    }
}

/// A typed link kept on an issue: PR, commit, note, preview, message, ...
/// `note` refs use `path` (the notes directory), preview refs store the
/// publish path — never a signed URL.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct Ref {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// `issue finish` marks worktree/branch refs closed rather than
    /// deleting them — the ref stays as history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<bool>,
    /// `message` refs only: the worktree the dispatch ran against, so
    /// a re-start under `--name` leaves the earlier kickoff bound to
    /// its own pair instead of every worktree the issue ever opens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    /// `worktree` refs only: the effective cargo target dir for the
    /// checkout — the shared cache or the worktree-local `target/`
    /// when the project opted out — so `issue finish` and
    /// `doctor --host` account for the right bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo_target: Option<String>,
    /// `message` refs only: the dispatch recipient — structured so
    /// finish finds the agent holding the bound message without
    /// parsing it back out of the display label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

/// The in-flight claim on a card, with its age.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct ClaimCard {
    pub by: String,
    pub at: String,
    pub session: Option<String>,
    pub last_seen: Option<String>,
    pub note: Option<String>,
    pub stale: Option<String>,
    pub age_secs: Option<i64>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct CardCounts {
    pub comments: usize,
    pub artifacts: usize,
    pub refs: usize,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct CardChecks {
    pub done: u64,
    pub total: u64,
}

/// The base issue card. `GET /api/issues` and `issue ls --json` return this
/// plus keys added by `work::card_json` and the board read model, so the
/// schema allows additional properties.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[schemars(extend("additionalProperties" = true))]
pub struct IssueCard {
    pub id: String,
    pub project: String,
    pub title: String,
    pub status: String,
    /// `rollup` | `notes` | `file` — where `status` came from.
    pub status_source: String,
    pub priority: String,
    pub owner: Option<String>,
    pub claim: Option<ClaimCard>,
    pub component: Option<String>,
    pub tags: Vec<String>,
    pub paths: Vec<String>,
    pub parent: Option<String>,
    pub blocked_by: Vec<String>,
    pub relates: Vec<String>,
    pub duplicate_of: Option<String>,
    pub refs: Vec<Ref>,
    pub container: bool,
    pub ready: bool,
    pub blocked: bool,
    pub blocked_reason: Option<String>,
    pub created: String,
    /// Content hash of `issue.md`: the `if_rev` token for writes.
    pub rev: String,
    pub counts: CardCounts,
    pub checks: CardChecks,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_grammar() {
        assert!(valid_id("CAD-16"));
        assert!(valid_id("OPS-3"));
        assert!(valid_id("A-1"));
        assert!(valid_id("CAD-007"));
        assert!(!valid_id("cad-16"));
        assert!(!valid_id("CAD16"));
        assert!(!valid_id("CAD-"));
        assert!(!valid_id("CAD-0"));
        assert!(!valid_id("-CAD-16"));
        assert!(!valid_id("CAD-16/extra"));
        assert!(!valid_id("../CAD-16"));
        assert!(!valid_id("CAD-16.md"));
        assert!(!valid_id("1-2"));
    }

    #[test]
    fn tag_grammar_and_normalizing() {
        assert!(valid_tag("ui"));
        assert!(valid_tag("2026-q4"));
        assert!(valid_tag(&"a".repeat(32)));
        assert!(!valid_tag(&"a".repeat(33)));
        assert!(!valid_tag(""));
        assert!(!valid_tag("-ui"));
        assert!(!valid_tag("UI"));
        assert!(!valid_tag("a b"));
        assert!(!valid_tag("a,b"));
        let tags = |t: &[&str]| t.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            normalize_tags(&tags(&["ui", "api", "ui"])).unwrap(),
            tags(&["api", "ui"])
        );
        assert!(normalize_tags(&tags(&["Bad"])).is_err());
        let many: Vec<String> = (0..13).map(|n| format!("t{n}")).collect();
        assert!(normalize_tags(&many).is_err());
    }

    #[test]
    fn key_grammar() {
        assert!(valid_key("cadence"));
        assert!(valid_key("ops-2"));
        assert!(!valid_key("Cadence"));
        assert!(!valid_key("a_b"));
        assert!(!valid_key("../x"));
    }
}
