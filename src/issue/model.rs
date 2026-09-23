//! Issue frontmatter model and the closed vocabularies.
//!
//! One issue = one folder `<pm>/<project>/<ID>/` holding `issue.md`
//! (YAML frontmatter + Markdown body), `comments/` and `artifacts/`.
//! Paths encode nothing that can change: no title slug, no status, no
//! parent.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Board columns minus `dropped` — dropped issues leave the board.
pub const STATUSES: &[&str] = &["backlog", "ready", "doing", "review", "done", "dropped"];
pub const PRIORITIES: &[&str] = &["P0", "P1", "P2", "P3"];
/// Link fields stored on one side only; inverses are computed.
pub const LINK_KINDS: &[&str] = &["blocked_by", "relates", "parent", "duplicate_of"];
pub const REF_KINDS: &[&str] = &[
    "pr", "commit", "note", "preview", "message", "url", "branch", "worktree",
];
/// Fields `issue set` may write.
pub const SETTABLE: &[&str] = &["status", "priority", "owner", "component", "title", "tags"];
/// Most tags one issue may carry.
pub const TAG_MAX: usize = 12;

/// `note` refs use `path` (the notes directory), preview refs store the
/// publish path — never a signed URL.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
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

/// CAD-383: who holds an issue that is in flight, since when, and why.
/// Written by `issue claim`, by `issue start`/`dispatch` when they move
/// an unclaimed issue into work, and replaced by a recorded take-over.
/// `by` is the PM (or other requester) that took the issue — `owner`
/// stays the lane doing the work — so the claimant can re-dispatch or
/// reassign without a take-over.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Claim {
    pub by: String,
    /// RFC 3339 UTC — the claim age is measured from here.
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// `issue.md` YAML frontmatter. No `project` field (the folder says
/// it), and never session, job or loop fields.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Front {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// CAD-383: the in-flight claim — see [`Claim`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<Claim>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    /// Free-form slicing labels — stored sorted and de-duplicated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Intake kind for `cadence report` issues
    /// (`question|feedback|idea|bug`) — its own field, not a positional
    /// tag, so an extra tag can never mislabel it. Absent on ordinary
    /// issues.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// CAD-359/360: set on an epic that is a plan — see [`Plan`].
    /// Absent on every other issue, so older binaries still read the
    /// file (issue frontmatter is parsed leniently).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<Plan>,
    /// Optional task size `S|M|L` — plan progress weights it 1|3|8,
    /// unsized counts as M (docs/design/WORK-MODEL.md).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relates: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<Ref>,
    pub created: String,
}

impl Front {
    pub fn new(id: &str, title: &str, created: &str) -> Self {
        Self {
            id: id.to_string(),
            title: title.to_string(),
            status: "backlog".to_string(),
            priority: "P2".to_string(),
            owner: None,
            claim: None,
            component: None,
            tags: vec![],
            kind: None,
            plan: None,
            size: None,
            parent: None,
            blocked_by: vec![],
            relates: vec![],
            duplicate_of: None,
            refs: vec![],
            created: created.to_string(),
        }
    }
}

/// CAD-359/360: the plan an epic carries. `state` is `proposed` until
/// the operator decides; only an `approved` plan's tickets dispatch.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    pub state: String,
    pub proposed_by: String,
    /// RFC 3339 UTC.
    pub proposed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<String>,
    /// Why a plan was rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Plan states, in order.
pub const PLAN_STATES: &[&str] = &["proposed", "approved", "rejected"];
/// Task sizes and their progress weights; unsized counts as `M`.
pub const SIZES: &[(&str, u64)] = &[("S", 1), ("M", 3), ("L", 8)];

/// The progress weight of a size — `S`=1, `M`=3, `L`=8; unsized or
/// unknown is `M`.
pub fn size_weight(size: Option<&str>) -> u64 {
    SIZES
        .iter()
        .find(|(s, _)| Some(*s) == size)
        .map_or(3, |(_, w)| *w)
}

/// Comment file frontmatter (`comments/<UTC-basic>-<author>.md`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommentFront {
    pub author: String,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

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

pub fn check_key(key: &str) -> Result<String> {
    if valid_key(key) {
        Ok(key.to_string())
    } else {
        Err(Error::rejected(format!(
            "Invalid project key '{key}' — 1-32 lowercase letters, digits or hyphens"
        )))
    }
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

/// System vocabulary exempt from a project's declared `tags:`
/// allowlist — `cadence report`'s `intake` + kind tags file into any
/// project without seeding its vocabulary first.
pub fn system_tag(tag: &str) -> bool {
    matches!(tag, "intake" | "question" | "feedback" | "idea" | "bug")
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

/// Artifact basename grammar — shared by the upload route and the
/// constrained read, so every stored file is fetchable:
/// `[A-Za-z0-9._-]{1,120}`, never a leading dot.
pub fn valid_artifact_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 120
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

pub fn check_link_kind(kind: &str) -> Result<()> {
    if LINK_KINDS.contains(&kind) {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "Unknown link kind '{kind}' — one of {}",
            LINK_KINDS.join(" ")
        )))
    }
}

pub fn check_ref_kind(kind: &str) -> Result<()> {
    if REF_KINDS.contains(&kind) {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "Unknown ref kind '{kind}' — one of {}",
            REF_KINDS.join(" ")
        )))
    }
}

/// A ref value git would parse as an option (`-x`,
/// `--upload-pack=<cmd>`) when it is passed as an argument. Tracker
/// files are plain markdown any agent can write, so this runs where refs
/// are written and again where `issue finish` reads them (CAD-144).
pub fn check_ref_value(value: &str) -> Result<()> {
    if value.starts_with('-') {
        Err(Error::rejected(format!(
            "Ref value '{value}' begins with '-' — git would read it as an option"
        )))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_grammar() {
        assert!(valid_id("CAD-16"));
        assert!(valid_id("OPS-3"));
        assert!(valid_id("A-1"));
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
