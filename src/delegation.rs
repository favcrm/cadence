//! Delegated approvals (CAD-918): the pure half of `audit approve
//! --delegated`. The daemon gathers the facts; this module decides.

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::Value;
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

#[derive(Deserialize)]
struct List {
    paths: Vec<String>,
}

#[derive(Deserialize)]
pub struct RiskPaths {
    delegable: List,
    trigger1: List,
    schema: List,
    trigger4: List,
    trigger7: List,
    scripts_allowlist: List,
}

/// What one changed path allows.
#[derive(Debug, PartialEq, Eq)]
pub enum PathClass {
    /// On the allowlist and in no trigger list.
    Delegable,
    /// A schema/store-version path: only under a scope pre-approval.
    Schema,
    /// Needs the operator: the triggers it hits, or none when it is
    /// simply outside the allowlist.
    Operator(Vec<&'static str>),
}

impl RiskPaths {
    pub fn load() -> RiskPaths {
        toml::from_str(RISK_PATHS_TOML).expect("docs/roles/risk-paths.toml parses")
    }

    /// The named lists, for the doc-pinning test and reporting.
    pub fn lists(&self) -> [(&'static str, &[String]); 6] {
        [
            ("delegable", &self.delegable.paths),
            ("trigger1", &self.trigger1.paths),
            ("schema", &self.schema.paths),
            ("trigger4", &self.trigger4.paths),
            ("trigger7", &self.trigger7.paths),
            ("scripts_allowlist", &self.scripts_allowlist.paths),
        ]
    }

    /// Fails closed: a path is delegable only when the allowlist names
    /// it and no trigger list does.
    pub fn classify(&self, path: &str) -> PathClass {
        let hit = |l: &List| l.paths.iter().any(|g| crate::review::glob_match(g, path));
        let triggers: Vec<&'static str> = [
            ("1", &self.trigger1),
            ("4", &self.trigger4),
            ("7", &self.trigger7),
        ]
        .into_iter()
        .filter(|(_, l)| hit(l))
        .map(|(t, _)| t)
        .collect();
        if !triggers.is_empty() {
            PathClass::Operator(triggers)
        } else if hit(&self.schema) {
            PathClass::Schema
        } else if hit(&self.delegable) {
            PathClass::Delegable
        } else {
            PathClass::Operator(vec![])
        }
    }
}

/// Every path a unified diff touches — both sides of `diff --git` and
/// of `rename from`/`rename to` — since gh's file list carries only a
/// rename's new path. A quoted or ambiguous header refuses.
pub fn diff_paths(diff: &str) -> Result<BTreeSet<String>> {
    let bad = |l: &str| Error::rejected(format!("unparseable diff header: {l}"));
    let mut out = BTreeSet::new();
    for l in diff.lines() {
        if let Some(rest) = l.strip_prefix("diff --git ") {
            let (a, b) = rest
                .strip_prefix("a/")
                .and_then(|r| r.split_once(" b/"))
                .filter(|(_, b)| !rest.contains('"') && !b.contains(" b/"))
                .ok_or_else(|| bad(l))?;
            out.extend([a.to_string(), b.to_string()]);
        } else if let Some(p) = l
            .strip_prefix("rename from ")
            .or_else(|| l.strip_prefix("rename to "))
            .or_else(|| l.strip_prefix("copy from "))
            .or_else(|| l.strip_prefix("copy to "))
        {
            if p.contains('"') {
                return Err(bad(l));
            }
            out.insert(p.to_string());
        }
    }
    Ok(out)
}

/// The alias a `From:` line names: its first token, without backticks
/// or a trailing `(model)`, lowercased — `` `pm-d` (claude opus) `` is
/// `pm-d`.
pub fn alias_of(from: &str) -> String {
    let token = from.split_whitespace().next().unwrap_or_default();
    let token = token.trim_matches('`');
    token
        .split('(')
        .next()
        .unwrap_or_default()
        .trim_matches('`')
        .to_lowercase()
}

/// `owner/name` lowercased, or `None` for anything else (a host prefix,
/// a URL, extra segments).
pub fn repo_slug(raw: &str) -> Option<String> {
    let ok = |p: &str| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    let (owner, name) = raw.split_once('/')?;
    (ok(owner) && ok(name)).then(|| raw.to_ascii_lowercase())
}

/// CI is green only from GitHub check runs: every check the base branch
/// requires (`protection.required_status_checks.checks`) must have a
/// completed, successful run from the required app. Commit statuses
/// count for nothing, except that a `qa-verdict` in any state but
/// SUCCESS refuses.
pub fn ci_green(branch: &Value, runs: &Value, rollup: &[Value]) -> std::result::Result<(), String> {
    let required = branch["protection"]["required_status_checks"]["checks"]
        .as_array()
        .filter(|r| !r.is_empty())
        .ok_or("the base branch declares no required checks")?;
    let listed = runs["check_runs"].as_array().map_or(0, Vec::len);
    if runs["total_count"].as_u64() != Some(listed as u64) {
        return Err("gh did not list every check run".into());
    }
    for req in required {
        let name = req["context"].as_str().unwrap_or_default();
        let app = req["app_id"].as_u64();
        let mine: Vec<&Value> = runs["check_runs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| r["name"] == name)
            .filter(|r| match app {
                Some(id) => r["app"]["id"].as_u64() == Some(id),
                None => r["app"]["slug"] == "github-actions",
            })
            .collect();
        if mine.is_empty() {
            return Err(format!("required check '{name}' has no run from its app"));
        }
        if !mine
            .iter()
            .all(|r| r["status"] == "completed" && r["conclusion"] == "success")
        {
            return Err(format!(
                "required check '{name}' is not a completed success"
            ));
        }
    }
    let qa = |r: &&Value| r["context"] == "qa-verdict" || r["name"] == "qa-verdict";
    if let Some(r) = rollup.iter().find(qa) {
        let state = r["state"]
            .as_str()
            .or(r["conclusion"].as_str())
            .unwrap_or("");
        if !state.eq_ignore_ascii_case("success") {
            return Err(format!("qa-verdict is {state}"));
        }
    }
    Ok(())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The digest a scope pre-approval binds: the ticket's body.
pub fn scope_digest(body: &str) -> String {
    sha256_hex(body.trim().as_bytes())
}

/// The ticket a PR title names: the id before the first `:`.
pub fn title_issue(title: &str) -> Option<String> {
    let id = title.split(':').next()?.trim();
    crate::issue::model::check_id(id)
        .ok()
        .map(|_| id.to_string())
}

fn names_ident(text: &str, sym: &str) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    text.match_indices(sym).any(|(i, _)| {
        text[..i].chars().next_back().is_none_or(|c| !ident(c))
            && text[i + sym.len()..]
                .chars()
                .next()
                .is_none_or(|c| !ident(c))
    })
}

/// The verdict notes a delegated approval rests on: PASS notes for
/// this ticket and PR, on exactly `head`, from two distinct reviewers
/// (by alias, [`alias_of`]) who are neither an author nor the approver.
/// Every verdict note on this head must class it `auto` or `delegated`,
/// and with a scope pre-approval the chosen notes must name its id.
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
    let mut chosen: Vec<(&Note, String)> = Vec::new();
    let mut newest_first = on_head;
    newest_first.sort_by(|a, b| b.path.cmp(&a.path));
    for n in newest_first {
        let from = alias_of(n.from.as_deref().unwrap_or_default());
        let pass = n
            .verdict
            .as_deref()
            .is_some_and(|v| v.eq_ignore_ascii_case("pass"));
        let names_scope = scope_id
            .is_none_or(|id| std::fs::read_to_string(&n.path).is_ok_and(|t| names_ident(&t, id)));
        if pass
            && !from.is_empty()
            && n.prs.contains(&pr)
            && !excluded.contains(&from)
            && names_scope
            && !chosen.iter().any(|(_, f)| *f == from)
        {
            chosen.push((n, from));
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
    Ok(chosen.into_iter().take(2).map(|(n, _)| n).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_is_an_allowlist_that_fails_closed() {
        let rp = RiskPaths::load();
        let op = |p: &str| matches!(rp.classify(p), PathClass::Operator(_));
        for p in [
            ".github/workflows/ci.yml",
            "docs/roles/risk-paths.toml",
            "src/peer.rs",
            "src/audit/mod.rs",
            "src/cli/mod.rs",
            "tests/scripts/test_ci_gate_evidence.py",
            "build.rs",
            ".cargo/config.toml",
            "ui/package.json",
            "src/daemon/jobs_rpc.rs",
            "scripts/auto-stage.py",
        ] {
            assert!(op(p), "{p}");
        }
        assert_eq!(rp.classify("src/store/schema.rs"), PathClass::Schema);
        for p in [
            "src/cli/job.rs",
            "src/issue/board.rs",
            "tests/board_issue.rs",
            "ui/src/App.svelte",
        ] {
            assert_eq!(rp.classify(p), PathClass::Delegable, "{p}");
        }
    }

    #[test]
    fn diff_paths_reads_both_sides_of_a_rename() {
        let diff = "diff --git a/src/audit.rs b/src/issue/board2.rs\nsimilarity index 99%\n\
                    rename from src/audit.rs\nrename to src/issue/board2.rs\n";
        let got = diff_paths(diff).unwrap();
        assert!(got.contains("src/audit.rs") && got.contains("src/issue/board2.rs"));
        assert!(diff_paths("diff --git \"a/x y\" \"b/x y\"\n").is_err());
    }

    #[test]
    fn alias_of_strips_decoration() {
        assert_eq!(alias_of("pm-d (claude opus)"), "pm-d");
        assert_eq!(alias_of("`R1` (claude)"), "r1");
        assert_eq!(alias_of("r1(model)"), "r1");
        assert_eq!(repo_slug("Acme/App").as_deref(), Some("acme/app"));
        assert_eq!(repo_slug("github.com/acme/app"), None);
    }

    /// The doc and the compiled lists cannot drift either way: the
    /// doc's "Mechanical path lists" section enumerates every list
    /// exactly, the script allowlist matches, and every path the
    /// trigger prose names is covered by that trigger's list.
    #[test]
    fn risk_classes_doc_matches_the_compiled_lists() {
        let doc = include_str!("../docs/roles/risk-classes.md");
        let rp = RiskPaths::load();
        let ticks = |l: &str| -> Vec<String> {
            l.split('`')
                .skip(1)
                .step_by(2)
                .map(str::to_string)
                .collect()
        };
        let section = doc.split("### Mechanical path lists").nth(1).unwrap();
        let section = section.split("\n#").next().unwrap();
        for (name, paths) in rp.lists() {
            if name == "scripts_allowlist" {
                continue;
            }
            let line = section
                .lines()
                .find(|l| l.starts_with(&format!("- **{name}**")))
                .unwrap_or_else(|| panic!("doc lists no {name}"));
            assert_eq!(ticks(line), paths, "{name}: doc and risk-paths.toml differ");
        }
        let allow = doc
            .split("### Script allowlist (auto-eligible)")
            .nth(1)
            .unwrap();
        let allow = allow.split("Each path must").next().unwrap();
        let listed: Vec<&str> = allow
            .lines()
            .filter_map(|l| l.strip_prefix("- `")?.split('`').next())
            .collect();
        assert_eq!(listed, rp.lists()[5].1);
        let pathlike = |t: &str| {
            !t.contains(' ')
                && !t.contains('(')
                && (t.contains('/')
                    || [".rs", ".toml", ".md", ".json", ".lock"]
                        .iter()
                        .any(|e| t.ends_with(e)))
        };
        for (n, name) in [
            ("1.", "trigger1"),
            ("2.", "schema"),
            ("4.", "trigger4"),
            ("7.", "trigger7"),
        ] {
            let para = doc.lines().find(|l| l.starts_with(n)).unwrap();
            let list = rp.lists().into_iter().find(|(k, _)| *k == name).unwrap().1;
            for t in ticks(para).into_iter().filter(|t| pathlike(t)) {
                assert!(
                    list.iter().any(|g| crate::review::glob_match(g, &t)),
                    "trigger {n} names `{t}`, which {name} does not cover"
                );
            }
        }
    }
}
