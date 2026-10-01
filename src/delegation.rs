//! Delegated approvals (CAD-918): the pure half of `audit approve
//! --delegated`. The daemon gathers the facts; this module decides.

use std::collections::BTreeSet;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::audit::Note;
use crate::error::{Error, Result};

/// The single source of the path lists, compiled in: the daemon applies
/// its own build's lists, never the PR's.
pub const RISK_PATHS_TOML: &str = include_str!("../docs/roles/risk-paths.toml");

/// The approval action a delegated approval records. It is not
/// `merge`, so a reader that binds operator approvals never counts it.
pub const DELEGATED_ACTION: &str = "delegated-merge";
/// The approval action of a ticket-time scope pre-approval.
pub const SCOPE_ACTION: &str = "scope";

#[derive(Deserialize, Default)]
struct List {
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    symbols: Vec<String>,
}

#[derive(Deserialize)]
pub struct RiskPaths {
    trigger1: List,
    schema: List,
    trigger4: List,
    trigger7: List,
    scripts_allowlist: List,
}

/// A trigger the diff touched, and what touched it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub trigger: &'static str,
    pub what: String,
}

impl Hit {
    /// Triggers 4 and 7 are never delegable and never pre-approvable.
    pub fn hard(&self) -> bool {
        matches!(self.trigger, "4" | "7")
    }
}

impl RiskPaths {
    pub fn load() -> RiskPaths {
        toml::from_str(RISK_PATHS_TOML).expect("docs/roles/risk-paths.toml parses")
    }

    pub fn allowlist(&self) -> &[String] {
        &self.scripts_allowlist.paths
    }

    /// Every trigger `files` and the unified `diff` touch.
    pub fn hits(&self, files: &[String], diff: &str) -> Vec<Hit> {
        let mut out = Vec::new();
        let lists = [
            ("1", &self.trigger1),
            ("schema", &self.schema),
            ("4", &self.trigger4),
            ("7", &self.trigger7),
        ];
        for (trigger, list) in lists {
            let exempt = |f: &str| {
                matches!(trigger, "4" | "7") && self.scripts_allowlist.paths.iter().any(|a| a == f)
            };
            for f in files {
                if !exempt(f) && list.paths.iter().any(|g| crate::review::glob_match(g, f)) {
                    out.push(Hit {
                        trigger,
                        what: f.clone(),
                    });
                }
            }
            for sym in &list.symbols {
                if diff_names(diff, sym) {
                    out.push(Hit {
                        trigger,
                        what: format!("symbol {sym}"),
                    });
                }
            }
        }
        out
    }
}

/// A diff line that changes, or a hunk header that sits inside, code
/// naming `sym` as a whole identifier.
fn diff_names(diff: &str, sym: &str) -> bool {
    diff.lines().any(|l| {
        let changed = (l.starts_with('+') || l.starts_with('-'))
            && !l.starts_with("+++")
            && !l.starts_with("---");
        (changed || l.starts_with("@@")) && names_ident(l, sym)
    })
}

fn names_ident(line: &str, sym: &str) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    line.match_indices(sym).any(|(i, _)| {
        line[..i].chars().next_back().is_none_or(|c| !ident(c))
            && line[i + sym.len()..]
                .chars()
                .next()
                .is_none_or(|c| !ident(c))
    })
}

/// The digest a scope pre-approval binds: the ticket's body.
pub fn scope_digest(body: &str) -> String {
    Sha256::digest(body.trim().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The ticket a PR title names: the id before the first `:`.
pub fn title_issue(title: &str) -> Option<String> {
    let id = title.split(':').next()?.trim();
    crate::issue::model::check_id(id)
        .ok()
        .map(|_| id.to_string())
}

/// The verdict notes a delegated approval rests on: PASS notes for
/// this ticket and PR, on exactly `head`, from two distinct reviewers
/// who are neither an author nor the approver. Every verdict note on
/// this head must class it `auto` or `delegated`, and with a scope
/// pre-approval the chosen notes must name its id.
pub(crate) fn pick_verdicts<'a>(
    notes: &'a [Note],
    issue: &str,
    pr: u64,
    head: &str,
    excluded: &BTreeSet<String>,
    scope_id: Option<&str>,
) -> Result<Vec<&'a Note>> {
    let on_head: Vec<&Note> = notes
        .iter()
        .filter(|n| n.kind == "verdict" && n.issue.as_deref() == Some(issue))
        .filter(|n| n.head_sha.as_deref() == Some(head))
        .collect();
    if let Some(n) = on_head
        .iter()
        .find(|n| !matches!(n.class.as_deref(), Some("auto" | "delegated")))
    {
        return Err(Error::rejected(format!(
            "verdict note {} states Risk: {} — a delegated approval needs every \
             reviewer to state auto or delegated (when unsure, human)",
            n.path.display(),
            n.class.as_deref().unwrap_or("(none)")
        )));
    }
    let mut chosen: Vec<&Note> = Vec::new();
    let mut newest_first = on_head;
    newest_first.sort_by(|a, b| b.path.cmp(&a.path));
    for n in newest_first {
        let Some(from) = n.from.as_deref().filter(|f| !f.is_empty()) else {
            continue;
        };
        let pass = n
            .verdict
            .as_deref()
            .is_some_and(|v| v.eq_ignore_ascii_case("pass"));
        let names_scope = scope_id
            .is_none_or(|id| std::fs::read_to_string(&n.path).is_ok_and(|t| names_ident(&t, id)));
        if pass
            && n.prs.contains(&pr)
            && !excluded.contains(from)
            && names_scope
            && !chosen.iter().any(|c| c.from.as_deref() == Some(from))
        {
            chosen.push(n);
        }
    }
    if chosen.len() < 2 {
        return Err(Error::rejected(format!(
            "a delegated approval needs two PASS verdict notes for {issue} PR #{pr} on head \
             {head} from distinct reviewers who are neither the author nor the approver{}; \
             found {}",
            if scope_id.is_some() {
                ", each naming the scope pre-approval"
            } else {
                ""
            },
            chosen.len()
        )));
    }
    chosen.truncate(2);
    Ok(chosen)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hits_split_hard_and_pre_approvable_triggers() {
        let rp = RiskPaths::load();
        let files = |f: &[&str]| f.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let hard = |f: &[&str]| rp.hits(&files(f), "").iter().any(Hit::hard);
        assert!(hard(&[".github/workflows/ci.yml"]));
        assert!(hard(&["docs/roles/risk-paths.toml"]));
        assert!(hard(&["scripts/install-cadence-nextest"]));
        assert!(!hard(&["scripts/auto-stage.py"]));
        assert!(!hard(&["src/store/schema.rs"]));
        assert!(rp.hits(&files(&["src/store/schema.rs"]), "")[0].trigger == "schema");
        assert!(rp.hits(&files(&["src/cli/job.rs"]), "").is_empty());
        let diff = "@@ -1,3 +1,3 @@ fn write_caller(req: &Request) {\n-    a\n+    b\n";
        assert_eq!(rp.hits(&[], diff)[0].trigger, "1");
        assert!(rp.hits(&[], "+ let my_write_caller_x = 1;\n").is_empty());
    }

    /// The doc and the compiled lists cannot drift: risk-classes.md
    /// links this file, and its script allowlist is exactly ours.
    #[test]
    fn risk_classes_doc_matches_the_compiled_lists() {
        let doc = include_str!("../docs/roles/risk-classes.md");
        assert!(doc.contains("docs/roles/risk-paths.toml"));
        let section = doc
            .split("### Script allowlist (auto-eligible)")
            .nth(1)
            .and_then(|s| s.split("Each path must").next())
            .unwrap();
        let listed: Vec<&str> = section
            .lines()
            .filter_map(|l| l.strip_prefix("- `")?.split('`').next())
            .collect();
        assert_eq!(listed, RiskPaths::load().allowlist());
    }
}
