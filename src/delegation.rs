//! Delegated approvals (CAD-918): the pure half of `audit approve
//! --delegated`. The daemon gathers the facts; this module decides.

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

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
    trigger3: List,
    trigger4: List,
    trigger6: List,
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
    pub fn lists(&self) -> [(&'static str, &[String]); 8] {
        [
            ("delegable", &self.delegable.paths),
            ("trigger1", &self.trigger1.paths),
            ("schema", &self.schema.paths),
            ("trigger3", &self.trigger3.paths),
            ("trigger4", &self.trigger4.paths),
            ("trigger6", &self.trigger6.paths),
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
            ("3", &self.trigger3),
            ("4", &self.trigger4),
            ("6", &self.trigger6),
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

/// The alias a `From:` line names — exactly `cadence audit verdicts`'
/// reviewer (`audit::reviewer_identity`), so both readers agree.
pub fn alias_of(from: &str) -> String {
    crate::audit::reviewer_identity(from)
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

/// The workflow whose `pull_request` runs carry the required checks.
pub const CI_WORKFLOW: &str = ".github/workflows/ci.yml";

/// CI is green only from GitHub check runs: every check the base branch
/// requires (`protection.required_status_checks.checks`) must have a
/// completed, successful newest run from the required app, inside the
/// check suite of a `pull_request` run of [`CI_WORKFLOW`] for `head` that
/// belongs to THIS PR — its `pull_requests[]` names `pr` on `base` (from
/// `workflows`, `actions/runs?head_sha=`; a fork run lists none and
/// counts for nothing). `runs` is `check-runs?filter=all`, so a later
/// run elsewhere cannot hide this suite's. A same-named run any other
/// workflow or PR posts counts for nothing; so do commit statuses,
/// except that a `qa-verdict` in any state but SUCCESS refuses.
pub fn ci_green(
    branch: &Value,
    runs: &Value,
    workflows: &Value,
    (head, pr, base): (&str, u64, &str),
    rollup: &[Value],
) -> std::result::Result<(), String> {
    let required = branch["protection"]["required_status_checks"]["checks"]
        .as_array()
        .filter(|r| !r.is_empty())
        .ok_or("the base branch declares no required checks")?;
    for (what, list, key) in [
        ("check run", runs, "check_runs"),
        ("workflow run", workflows, "workflow_runs"),
    ] {
        let listed = list[key].as_array().map_or(0, Vec::len);
        if list["total_count"].as_u64() != Some(listed as u64) {
            return Err(format!("gh did not list every {what}"));
        }
    }
    let suites: Vec<u64> = workflows["workflow_runs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| {
            w["path"] == CI_WORKFLOW && w["event"] == "pull_request" && w["head_sha"] == head
        })
        .filter(|w| {
            w["pull_requests"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|p| p["number"].as_u64() == Some(pr) && p["base"]["ref"] == base)
        })
        .filter_map(|w| w["check_suite_id"].as_u64())
        .collect();
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
            .filter(|r| {
                r["check_suite"]["id"]
                    .as_u64()
                    .is_some_and(|id| suites.contains(&id))
            })
            .collect();
        if mine.is_empty() {
            return Err(format!(
                "required check '{name}' has no run from its app in a pull_request run of \
                 {CI_WORKFLOW}"
            ));
        }
        // The newest run in this PR's own suites decides (a rerun).
        let newest = mine.iter().max_by_key(|r| r["id"].as_u64().unwrap_or(0));
        if !newest.is_some_and(|r| r["status"] == "completed" && r["conclusion"] == "success") {
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

/// The verdict notes a delegated approval rests on, read by the one
/// reader `cadence audit verdicts` and `scripts/enqueue-reviewed` use
/// (`audit::verdicts_in`: notes of this ticket and PR on exactly `head`,
/// symlinks skipped, each with the outcome its title, section and inline
/// line agree on). Two PASS notes from distinct reviewers who are neither
/// an author nor the approver; a conflicted note refuses, and so does
/// any note on this head whose Risk is not `auto` or `delegated`. With a
/// scope pre-approval the chosen notes must name its id. Answers
/// `(path, reviewer)` pairs.
pub(crate) fn pick_verdicts(
    rows: &Value,
    excluded: &BTreeSet<String>,
    scope_id: Option<&str>,
) -> Result<Vec<(String, String)>> {
    let text = |r: &Value, k: &str| r[k].as_str().unwrap_or_default().to_string();
    let mut on_head: Vec<&Value> = rows["verdicts"].as_array().into_iter().flatten().collect();
    if let Some(r) = on_head.iter().find(|r| r["outcome"] == "conflict") {
        return Err(Error::rejected(format!(
            "verdict note {} is a conflict — its title, `## Verdict` section and \
             inline `Verdict:` line disagree",
            text(r, "path")
        )));
    }
    if let Some(r) = on_head.iter().find(|r| {
        !matches!(
            text(r, "risk").to_lowercase().as_str(),
            "auto" | "delegated"
        )
    }) {
        return Err(Error::rejected(format!(
            "verdict note {} states Risk: {} — a delegated approval needs every \
             reviewer to state auto or delegated (when unsure, human)",
            text(r, "path"),
            text(r, "risk")
        )));
    }
    let mut chosen: Vec<(String, String)> = Vec::new();
    on_head.sort_by_key(|r| std::cmp::Reverse(text(r, "name")));
    for r in on_head {
        let (path, from) = (text(r, "path"), text(r, "reviewer"));
        let names_scope = scope_id
            .is_none_or(|id| std::fs::read_to_string(&path).is_ok_and(|t| names_ident(&t, id)));
        if r["outcome"] == "pass"
            && !from.is_empty()
            && !excluded.contains(&from)
            && names_scope
            && !chosen.iter().any(|(_, f)| *f == from)
        {
            chosen.push((path, from));
        }
    }
    if chosen.len() < 2 {
        let (issue, pr, head) = (text(rows, "issue"), &rows["pr"], text(rows, "head"));
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
    fn classify_is_an_allowlist_that_fails_closed() {
        let rp = RiskPaths::load();
        let op = |p: &str| matches!(rp.classify(p), PathClass::Operator(_));
        // Every path the round-2 review named, and the round-1 ones.
        for p in [
            ".github/workflows/ci.yml",
            "docs/roles/risk-paths.toml",
            "src/peer.rs",
            "src/audit/mod.rs",
            "build.rs",
            ".cargo/config.toml",
            "tests/scripts/test_ci_gate_evidence.py",
            "src/daemon/jobs_rpc.rs",
            "scripts/auto-stage.py",
            "ui/package.json",
            "ui/vite.config.ts",
            "ui/scripts/check-module-names.mjs",
            "ui/.npmrc",
            "ui/.pnpmfile.cjs",
            "ui/pnpm-workspace.yaml",
            "ui/src/App.svelte",
            "ui/tests/board.test.ts",
            "src/cli/job.rs",
            "src/cli/mod.rs",
            "src/cli/audit.rs",
            "src/cli/upgrade.rs",
            "src/cli/update.rs",
            "src/cli/rollout.rs",
            "src/cli/daemon.rs",
            "src/cli/agent_uid.rs",
            "src/cli/secret.rs",
            "src/cli/connection.rs",
            "src/cli/platform.rs",
            "src/cli/remote_result.rs",
            "src/cli/master.rs",
            "src/issue/model.rs",
            "src/issue/board.rs",
            "src/issue/parse.rs",
            "tests/operator_lineage.rs",
            "tests/master_permission.rs",
            "tests/build_identity.rs",
            "tests/delegated_approval.rs",
            "tests/daemon.rs",
            "docs/design/CONTRACT-TEMPLATE.md",
            "docs/design/hosted-migration.md",
            "docs/cadence/project-context.yaml",
            "docs/START-HERE.md",
            "docs/ARCHITECTURE.md",
            "docs/BOARD.md",
        ] {
            assert!(op(p), "{p} must need the operator");
        }
        assert_eq!(rp.classify("src/store/schema.rs"), PathClass::Schema);
        for p in [
            "src/cli/status.rs",
            "src/issue/write.rs",
            "tests/board_issue.rs",
            "docs/guides/wiki-knowledge.md",
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
        assert_eq!(alias_of("pm-d, standards"), "pm-d");
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
        // Every document the context manifest serves agents at runtime
        // needs the operator, including any entry added later.
        let manifest: serde_yaml::Value =
            serde_yaml::from_str(include_str!("../docs/cadence/project-context.yaml")).unwrap();
        let docs = manifest["documents"].as_sequence().unwrap();
        assert!(!docs.is_empty());
        for d in docs {
            let path = d["path"].as_str().unwrap();
            assert!(
                matches!(rp.classify(path), PathClass::Operator(_)),
                "context manifest entry {path} must need the operator"
            );
        }
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
        assert_eq!(
            listed,
            rp.lists()
                .iter()
                .find(|(k, _)| *k == "scripts_allowlist")
                .unwrap()
                .1
        );
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
            ("3.", "trigger3"),
            ("4.", "trigger4"),
            // Trigger 6's prose names `docs/CHARTER.md` only as a "see also".
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
