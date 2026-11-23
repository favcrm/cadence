//! Workflow gate keys, approval checks, and template validation for `issue::workflow`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::HashSet;
use std::path::Path;

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::issue::{model, parse, plan, project};

use super::{
    bare_placeholder, canonical, file_of, normalize_deps, parse_template, read_for, ticket_meta,
};

/// The approval-affecting skeleton: the declared input names with
/// their required/optional flag and declared shape (an input can feed
/// `{{name}}` in a gated position, and a shape decides what values are
/// accepted at all, so the contract is pinned — but never the `ask` or
/// `example` wording), the ticket count, and each ticket's metadata
/// lines (recognised keys, values normalised, sorted per ticket).
/// Wording — titles, prose, acceptance items, `ask` and `example` text
/// — is not here, so a wording-only edit keeps the digest.
pub fn gate_keys(text: &str) -> Result<String> {
    let (_yaml, body) = parse::split_front(text).map_err(|e| {
        Error::rejected(format!("{e} — a workflow is a plan file with frontmatter"))
    })?;
    let tpl = parse_template(text)?;
    let mut keys = String::from("inputs=");
    keys.push_str(
        &tpl.inputs
            .iter()
            .map(|(n, s)| {
                let mut key = format!("{n}{}", if s.optional { "?" } else { "!" });
                if let Some(kind) = s.kind {
                    key.push_str(&format!(":{}", kind.as_str()));
                }
                if s.context_default {
                    key.push_str(":context_default");
                }
                if let Some(default) = &s.default {
                    use sha2::{Digest, Sha256};
                    key.push_str(&format!(
                        ":default:{:x}",
                        Sha256::digest(default.as_bytes())
                    ));
                }
                key
            })
            .collect::<Vec<_>>()
            .join(","),
    );
    if !tpl.distinct.is_empty() {
        // The distinctness contract is approval-affecting too — dropping
        // `distinct:` would silently re-admit worker == reviewer.
        let mut d = tpl.distinct.clone();
        d.sort();
        keys.push_str(&format!(";distinct={}", d.join(",")));
    }
    if let Some(slot) = &tpl.publication_slot {
        keys.push_str(&format!(";publication_slot={slot}"));
    }
    if !tpl.capability_slots.is_empty() {
        keys.push_str(&format!(
            ";capability_slots={}",
            tpl.capability_slots.join(",")
        ));
    }
    if let Some(slot) = &tpl.required_asset_slot {
        keys.push_str(&format!(";required_asset_slot={slot}"));
    }
    keys.push('\n');
    let metas = ticket_meta(body)?;
    keys.push_str(&format!("tickets={}\n", metas.len()));
    for (i, meta) in metas.iter().enumerate() {
        let mut pairs: Vec<String> = meta
            .iter()
            .map(|(k, v)| {
                if k == "depends_on" {
                    format!("{k}={}", normalize_deps(v))
                } else {
                    format!("{k}={v}")
                }
            })
            .collect();
        pairs.sort();
        keys.push_str(&format!("#{} {}\n", i + 1, pairs.join(";")));
    }
    Ok(keys)
}

/// `sha256:<hex>` of [`gate_keys`] — what `workflow approve` records
/// and `plan propose --workflow` matches against.
pub fn gate_digest(text: &str) -> Result<String> {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(gate_keys(text)?.as_bytes());
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("sha256:{hex}"))
}

/// A `depends_on` cycle over `Dep::Ticket` edges — `1 → 2 → 1` parses
/// fine (deps may name later tickets) but can never run. Returns the
/// cycle's ticket positions for the refusal.
fn dep_cycle(doc: &plan::PlanDoc) -> Option<Vec<usize>> {
    let n = doc.tickets.len();
    let mut color = vec![0u8; n];
    let mut stack = Vec::new();
    fn dfs(
        doc: &plan::PlanDoc,
        at: usize,
        color: &mut [u8],
        stack: &mut Vec<usize>,
    ) -> Option<Vec<usize>> {
        color[at] = 1;
        stack.push(at);
        for dep in &doc.tickets[at].depends_on {
            if let plan::Dep::Ticket(k) = dep {
                match color[*k] {
                    1 => {
                        let from = stack.iter().position(|t| t == k).unwrap_or(0);
                        return Some(stack[from..].to_vec());
                    }
                    0 => {
                        if let Some(c) = dfs(doc, *k, color, stack) {
                            return Some(c);
                        }
                    }
                    _ => {}
                }
            }
        }
        stack.pop();
        color[at] = 2;
        None
    }
    for t in 0..n {
        if color[t] == 0 {
            if let Some(c) = dfs(doc, t, &mut color, &mut stack) {
                return Some(c);
            }
        }
    }
    None
}

/// The agents a ticket's `agent:` may name, and where each came from:
/// the project's PROJECT.md `agents:` map, the agent files under
/// `<pm>/agents/`, and (when given) the daemon's registered aliases.
/// `project_key` must pass [`model::check_key`] — it joins the pm dir.
pub fn known_agents(
    pm_dir: &Path,
    project_key: Option<&str>,
    daemon_aliases: &[String],
) -> (HashSet<String>, Vec<String>) {
    let mut agents = HashSet::new();
    let mut sources = Vec::new();
    if let Some(key) = project_key {
        // A malformed key is no project — never a path to join.
        if let Ok(key) = model::check_key(key) {
            if let Ok(text) = std::fs::read_to_string(pm_dir.join(&key).join("PROJECT.md")) {
                if let Ok((yaml, _)) = parse::split_front(&text) {
                    if let Ok(serde_yaml::Value::Mapping(m)) =
                        serde_yaml::from_str::<serde_yaml::Value>(yaml)
                    {
                        if let Some(serde_yaml::Value::Mapping(a)) =
                            m.get(serde_yaml::Value::String("agents".to_string()))
                        {
                            for k in a.keys().filter_map(|k| k.as_str()) {
                                agents.insert(k.to_string());
                            }
                            if !a.is_empty() {
                                sources.push(format!("{key}/PROJECT.md agents:"));
                            }
                        }
                    }
                }
            }
        }
    }
    let dir = pm_dir.join("agents");
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut found = false;
        for e in entries.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                agents.insert(e.file_name().to_string_lossy().to_string());
                found = true;
            }
        }
        if found {
            sources.push("<pm>/agents/".to_string());
        }
    }
    if !daemon_aliases.is_empty() {
        agents.extend(daemon_aliases.iter().cloned());
        sources.push("the daemon registry".to_string());
    }
    (agents, sources)
}

/// Every approval the daemon recorded: `"<project>/<name>"` → payload
/// (`digest`, `by`, `at`). Read straight from the store, read-only —
/// like `issue retro`'s corroboration — so `check`/`ls`/`show` work
/// with the daemon down. `None` means absent or unreadable; readers
/// show approval as unknown then, never as granted.
pub fn fetch_approvals(state_dir: &Path) -> Option<Map<String, Value>> {
    let path = state_dir.join("cadence.sqlite3");
    if !path.exists() {
        return None;
    }
    let conn = crate::store::open_read_only(&path).ok()?;
    let mut stmt = conn
        .prepare("SELECT payload FROM events WHERE alias=? AND kind=? ORDER BY seq")
        .ok()?;
    let rows = stmt
        .query_map(
            rusqlite::params![
                crate::store::APPROVAL_STREAM,
                crate::store::WORKFLOW_APPROVED_EVENT
            ],
            |r| r.get::<_, String>(0),
        )
        .ok()?;
    let mut out = Map::new();
    for raw in rows.flatten() {
        let Ok(payload) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if let (Some(p), Some(n)) = (payload["project"].as_str(), payload["name"].as_str()) {
            out.insert(approval_key(p, n), payload);
        }
    }
    Some(out)
}

/// The key an approval is stored under.
pub fn approval_key(project: &str, name: &str) -> String {
    format!("{project}/{name}")
}

/// Is `digest` the approved digest for `project/name`? An unreachable
/// daemon is no approval — fail closed, like `plan propose` refusing.
pub fn approved(
    project: &str,
    name: &str,
    digest: &str,
    approvals: Option<&Map<String, Value>>,
) -> bool {
    approvals
        .and_then(|a| a.get(&approval_key(project, name)))
        .and_then(|p| p["digest"].as_str())
        == Some(digest)
}

/// A `check` verdict on workflow text: errors refuse; notes say what
/// was skipped and why. `meta` is the raw per-ticket metadata
/// ([`ticket_meta`]), `doc` the canonically rendered plan.
///
/// This is the NON-installed verdict: a `skill:` directive refuses here,
/// because nothing on this path freezes a package. [`check_installed_text`]
/// is the installed bundle's own verdict, where the tag is admissible and the
/// caller proves it resolves.
pub(crate) fn check_text(
    text: &str,
    agents: &HashSet<String>,
    agent_sources: &[String],
) -> (Vec<String>, Vec<String>, Option<plan::PlanDoc>) {
    check_text_for(text, agents, agent_sources, SkillDirective::Refuse)
}

/// The installed bundle's verdict: a `skill:` directive is admissible (it is
/// this bundle's own material), and the caller — `app::validate_contents` —
/// proves each tag resolves to a complete package and run creation re-proves
/// it. Every other check is identical to [`check_text`].
pub(crate) fn check_installed_text(
    text: &str,
    agents: &HashSet<String>,
    agent_sources: &[String],
) -> (Vec<String>, Vec<String>, Option<plan::PlanDoc>) {
    check_text_for(text, agents, agent_sources, SkillDirective::Installed)
}

/// Whether a `skill:` directive is admissible in the text being checked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SkillDirective {
    Refuse,
    Installed,
}

fn check_text_for(
    text: &str,
    agents: &HashSet<String>,
    agent_sources: &[String],
    skill: SkillDirective,
) -> (Vec<String>, Vec<String>, Option<plan::PlanDoc>) {
    let mut errors = Vec::new();
    let mut notes = Vec::new();
    let tpl = match parse_template(text) {
        Ok(t) => t,
        Err(e) => {
            errors.push(e.to_string());
            return (errors, notes, None);
        }
    };
    let rendered = match canonical(text, &tpl.inputs) {
        Ok(r) => r,
        Err(e) => {
            errors.push(format!("canonical render: {e}"));
            return (errors, notes, None);
        }
    };
    let doc = match plan::parse_plan(&rendered) {
        Ok(doc) => doc,
        Err(e) => {
            errors.push(e.to_string());
            return (errors, notes, None);
        }
    };
    if let Some(cycle) = dep_cycle(&doc) {
        let names: Vec<String> = cycle
            .iter()
            .map(|i| format!("{} \"{}\"", i + 1, doc.tickets[*i].title))
            .collect();
        errors.push(format!(
            "depends_on cycle: tickets {} depend on each other — none can start",
            names.join(" → ")
        ));
    }
    let metas =
        ticket_meta(parse::split_front(text).map(|(_, b)| b).unwrap_or("")).unwrap_or_default();
    // The parse-parity check compares against what the plan parser
    // actually read — the rendered file's metadata, not the raw
    // `{{placeholder}}` forms (which always differ from the rendered
    // values the parser captured).
    let metas_seen = ticket_meta(parse::split_front(&rendered).map(|(_, b)| b).unwrap_or(""))
        .unwrap_or_default();
    let mut reviewer_checked = false;
    for (i, meta) in metas.iter().enumerate() {
        let label = doc
            .tickets
            .get(i)
            .map(|t| format!("Ticket {} \"{}\"", i + 1, t.title))
            .unwrap_or_else(|| format!("Ticket {}", i + 1));
        let get = |k: &str| {
            meta.iter()
                .find(|(key, _)| key.as_str() == k)
                .map(|(_, v)| v.as_str())
        };
        // CAD-1867: `skill:` is admissible only in an installed bundle's own
        // workflow, where the tag resolves against that bundle. Every other
        // reader of this text refuses it rather than dropping it.
        if skill == SkillDirective::Refuse {
            if let Some(tag) = get("skill") {
                errors.push(format!(
                    "{label}: `skill: {tag}` names installed-app skill material — only an \
                     installed app run freezes a skill package into its snapshot; this \
                     workflow is not an installed app's workflow"
                ));
            }
        }
        // The plan parser reads metadata only until the first key it
        // does not know — `size:`/`agent:`/`depends_on:` written AFTER
        // a `reviewer:`/`tries:`/`uses:` line is body text to it, so
        // the gate keys would claim a skeleton the run never sees.
        // Refuse the ordering rather than let them diverge.
        if let Some(ticket) = doc.tickets.get(i) {
            let seen = metas_seen.get(i);
            let get_seen = |k: &str| {
                seen.and_then(|m| {
                    m.iter()
                        .find(|(key, _)| key.as_str() == k)
                        .map(|(_, v)| v.as_str())
                })
            };
            for (key, parsed) in [
                ("size", ticket.size.as_deref()),
                ("agent", ticket.agent.as_deref()),
            ] {
                let claimed = get_seen(key);
                if claimed.is_some() && claimed != parsed {
                    errors.push(format!(
                        "{label}: `{key}` sits after an unknown metadata line — the plan \
                         parser already stopped reading metadata, so it lands in the \
                         ticket body. Put `size:`/`agent:`/`depends_on:` before \
                         `reviewer:`/`tries:`/`uses:`/`skill:`"
                    ));
                }
            }
            let claimed_deps = get_seen("depends_on")
                .map(normalize_deps)
                .unwrap_or_default();
            let mut parsed_deps: Vec<String> = ticket
                .depends_on
                .iter()
                .map(|d| match d {
                    plan::Dep::Ticket(k) => (k + 1).to_string(),
                    plan::Dep::Issue(i) => i.clone(),
                })
                .collect();
            parsed_deps.sort();
            if claimed_deps != parsed_deps.join(",") {
                errors.push(format!(
                    "{label}: `depends_on` sits after an unknown metadata line — the plan \
                     parser already stopped reading metadata, so the dependency lands in \
                     the ticket body. Put `depends_on:` before `reviewer:`/`tries:`/`uses:`/`skill:`"
                ));
            }
        }
        if let Some(agent) = get("agent") {
            if agent.contains("{{") {
                notes.push(format!(
                    "{label}: agent is a placeholder — its value is checked at propose"
                ));
            } else if !agents.is_empty() && !agents.contains(agent) {
                errors.push(format!(
                    "{label}: agent '{agent}' is unknown — not in {}",
                    agent_sources.join(", ")
                ));
            } else if agents.is_empty() {
                notes.push(format!(
                    "{label}: agent '{agent}' cannot be verified — no agent source \
                     (PROJECT.md agents:, <pm>/agents/, the daemon registry) has any"
                ));
            }
        }
        if let Some(reviewer) = get("reviewer") {
            reviewer_checked = true;
            let agent = get("agent");
            if reviewer.contains("{{") || agent.is_some_and(|a| a.contains("{{")) {
                // Render-time values: independence holds only when both
                // sides are bare placeholders whose inputs `distinct:`
                // pins apart — anything else cannot be proven or
                // enforced, so it refuses rather than notes.
                match (agent.and_then(bare_placeholder), bare_placeholder(reviewer)) {
                    _ if agent.is_none() => notes.push(format!(
                        "{label}: reviewer is templated and the ticket has no `agent:` — \
                         nothing to collide with"
                    )),
                    (Some(a), Some(r)) if a == r => errors.push(format!(
                        "{label}: agent and reviewer are the same input `{{{{{a}}}}}` — \
                         they always render equal"
                    )),
                    (Some(a), Some(r))
                        if tpl.distinct.iter().any(|d| d == a)
                            && tpl.distinct.iter().any(|d| d == r) =>
                    {
                        notes.push(format!(
                            "{label}: reviewer≠agent enforced at render by `distinct:`"
                        ));
                    }
                    _ => errors.push(format!(
                        "{label}: agent and reviewer are templated — the render cannot \
                         prove reviewer≠agent. Make each a bare `{{{{input}}}}` and pin \
                         them: `distinct: [worker, reviewer]`"
                    )),
                }
            } else if agent == Some(reviewer) {
                errors.push(format!(
                    "{label}: reviewer '{reviewer}' is the ticket's own agent — a \
                     review must be independent"
                ));
            }
        }
    }
    if !reviewer_checked {
        notes.push(
            "reviewer check skipped: the plan format has no `reviewer:` field and \
             PROJECT.md declares no reviewer route — nothing to compare to `agent:`"
                .to_string(),
        );
    }
    (errors, notes, Some(doc))
}

/// `workflow check <file|name>` — a file path, or a name in
/// `--project`'s `workflows/`. The report lists every refusal; the CLI
/// exits non-zero when `errors` is non-empty.
pub fn check(
    pm_dir: &Path,
    target: &str,
    project_key: Option<&str>,
    state_dir: &Path,
) -> Result<Value> {
    // A path that exists, or names one (`a/b`, `x.md`), is a file;
    // else a stored workflow name needing a project. The file form
    // resolves `--project` or the cwd's project leniently — without
    // one, `agent:` is still checked against <pm>/agents/ and the
    // daemon registry.
    if let Some(k) = project_key {
        model::check_key(k)?;
    }
    let cwd = std::env::current_dir()?;
    let resolve_lenient =
        |flag: Option<&str>| project::resolve(pm_dir, flag, &cwd).ok().map(|p| p.key);
    let (path, project, name) = {
        let p = Path::new(target);
        if p.is_file() || target.ends_with(".md") || target.contains('/') {
            (p.to_path_buf(), resolve_lenient(project_key), None)
        } else {
            let key = project::resolve(pm_dir, project_key, &cwd)?;
            (
                file_of(pm_dir, &key.key, target)?,
                Some(key.key),
                Some(target.to_string()),
            )
        }
    };
    if path.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{} is a symlink — the tracker never follows links",
            path.display()
        )));
    }
    let text = match &name {
        // The stored form names itself when absent.
        Some(name) => read_for(pm_dir, project.as_deref().unwrap_or_default(), name)?,
        None => std::fs::read_to_string(&path)
            .map_err(|e| Error::rejected(format!("cannot read {}: {e}", path.display())))?,
    };
    let daemon_agents = daemon_aliases(state_dir);
    let (agents, sources) = known_agents(pm_dir, project.as_deref(), &daemon_agents);
    let (errors, notes, doc) = check_text(&text, &agents, &sources);
    let mut out = json!({
        "ok": errors.is_empty(),
        "errors": errors,
        "notes": notes,
        "path": path,
        "project": project,
        "name": name,
    });
    if let Some(doc) = doc {
        out["tickets"] = json!(doc.tickets.len());
        out["title"] = json!(doc.title);
    }
    if let Ok(tpl) = parse_template(&text) {
        out["inputs"] = json!(tpl
            .inputs
            .iter()
            .map(|(k, s)| { json!({"name": k, "ask": s.ask, "optional": s.optional}) })
            .collect::<Vec<_>>());
    }
    if let (Some(project), Some(name)) = (&project, &name) {
        if let Ok(digest) = gate_digest(&text) {
            out["digest"] = json!(digest);
            let approvals = fetch_approvals(state_dir);
            out["approved"] = match &approvals {
                Some(a) => json!(approved(project, name, &digest, Some(a))),
                None => json!("unknown — daemon unreachable"),
            };
        }
    }
    Ok(out)
}

/// The daemon's registered agent aliases, best-effort — `check` and
/// `add`/`edit` resolve `agent:` against them as one of the sources.
pub(super) fn daemon_aliases(state_dir: &Path) -> Vec<String> {
    crate::client::rpc(state_dir, "agent_list", json!({}))
        .ok()
        .and_then(|v| v["agents"].as_array().cloned())
        .map(|a| {
            a.iter()
                .filter_map(|a| a["alias"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}
