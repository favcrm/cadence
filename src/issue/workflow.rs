//! Workflows (CAD-487): a workflow is a reusable plan file with
//! inputs, kept in the tracker next to PROJECT.md as
//! `<pm>/<project>/workflows/<name>.md`. The CLI is the only writer —
//! `workflow add`/`edit` land as one tracker commit each with `Actor:`
//! recorded, and `workflow check`/`ls`/`show` read.
//!
//! The format is the plan format ([`crate::issue::plan`]) plus an
//! `inputs:` frontmatter map — `name: { ask, optional }` — and
//! `{{name}}` placeholders anywhere in the file. `plan propose
//! --workflow` renders the file: placeholders take the `--input`
//! values, `inputs:` drops out of the frontmatter, and the result goes
//! through the unchanged propose → approve → gate path (the plan
//! parser denies unknown fields, so it never sees `inputs`). A `{{`
//! that is not a well-formed `{{name}}` is a template error, never
//! literal text — the same fail-loud convention as the plan parser's
//! `deny_unknown_fields`.
//!
//! The file is gated like PROJECT.md's work keys (CAD-405): the
//! operator's `workflow approve` records a digest of its *gate keys* —
//! the parts that decide who does the work and in what order: the
//! ticket count and each ticket's metadata lines (`size`, `agent`,
//! `depends_on`, plus `reviewer`, `tries`, `uses`, which the plan
//! parser does not consume yet but a workflow may already carry for
//! the check and the gate). Wording — the title, goal, descriptions,
//! acceptance text, `inputs` — is not gated. A structural edit, CLI or
//! hand, changes the digest, so `propose` refuses
//! `workflow_unapproved` until the operator approves the new keys; a
//! rendered plan still needs `plan approve` before any ticket
//! dispatches. The gate is a process guard, not a security boundary —
//! same as the plan gate.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::issue::model;
use crate::issue::parse::{self};
use crate::issue::{plan, project, write, Pm};

/// `<pm>/<project>/workflows/` — beside PROJECT.md.
pub const DIR: &str = "workflows";

/// Frontmatter keys a workflow file may carry: the plan's own three,
/// plus `inputs`. Anything else refuses at parse — the same fail-loud
/// rule the plan parser applies with `deny_unknown_fields`.
const META_KEYS: &[&str] = &["title", "goal", "non_goals", "inputs"];

/// Ticket metadata lines a workflow recognises: the plan's own plus
/// the approval-affecting fields later stages add (a `reviewer:` line
/// is checked, and `tries:`/`uses:` gate the file, even though the v0
/// plan parser leaves them in the issue body).
const TICKET_META_KEYS: &[&str] = &["size", "agent", "depends_on", "reviewer", "tries", "uses"];

/// Metadata fields that must be static so `workflow check` can verify
/// them — a placeholder here is refused.
const STATIC_META_KEYS: &[&str] = &["size", "depends_on"];

/// One `inputs:` entry: what the proposer is asked, and whether the
/// run may leave it blank.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InputSpec {
    pub ask: Option<String>,
    pub optional: bool,
}

/// A parsed workflow template: the declared inputs. The plan structure
/// itself is checked by rendering and running [`plan::parse_plan`].
#[derive(Clone, Debug)]
pub struct Template {
    pub inputs: BTreeMap<String, InputSpec>,
}

/// `{{name}}` — the input name is a bare word, like an alias but with
/// an optional leading `_`.
fn valid_input_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// `workflows/<name>.md` — the name is a tag-shaped slug.
fn check_name(name: &str) -> Result<()> {
    if !model::valid_tag(name) {
        return Err(Error::rejected(format!(
            "workflow name '{name}' — 1-32 lowercase letters, digits or hyphens"
        )));
    }
    Ok(())
}

/// `<pm>/<project>/workflows/<name>.md` — the dir must be real.
fn dir_of(pm_dir: &Path, project: &str) -> Result<PathBuf> {
    let dir = pm_dir.join(project).join(DIR);
    if dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{project}/{DIR}/ is a symlink — the tracker never follows links"
        )));
    }
    Ok(dir)
}

/// The workflow file, refusing a symlinked dir or file (the tracker
/// never follows links).
fn file_of(pm_dir: &Path, project: &str, name: &str) -> Result<PathBuf> {
    check_name(name)?;
    let file = dir_of(pm_dir, project)?.join(format!("{name}.md"));
    Ok(file)
}

/// Read a stored workflow: real file only, a missing one names itself.
pub fn read_for(pm_dir: &Path, project: &str, name: &str) -> Result<String> {
    let file = file_of(pm_dir, project, name)?;
    if !crate::issue::board::is_real_file(&file) {
        return Err(Error::rejected(format!(
            "{} is not a regular file — unknown workflow '{name}' in {project}, \
             or it is a symlink; `cadence workflow ls --project {project}` lists them",
            file.display()
        )));
    }
    std::fs::read_to_string(&file)
        .map_err(|e| Error::rejected(format!("cannot read {}: {e}", file.display())))
}

/// The frontmatter of a template: a YAML mapping restricted to
/// [`META_KEYS`]; `inputs` is pulled out into [`InputSpec`]s.
fn parse_front(yaml: &str) -> Result<(serde_yaml::Mapping, BTreeMap<String, InputSpec>)> {
    let meta: serde_yaml::Value = serde_yaml::from_str(yaml)
        .map_err(|e| Error::rejected(format!("workflow frontmatter: {e}")))?;
    let mut map = match meta {
        serde_yaml::Value::Mapping(m) => m,
        _ => {
            return Err(Error::rejected(
                "workflow frontmatter is not a mapping — title, goal, non_goals, inputs",
            ))
        }
    };
    for key in map.keys() {
        let k = key.as_str().unwrap_or_default();
        if !META_KEYS.contains(&k) {
            return Err(Error::rejected(format!(
                "workflow frontmatter key '{k}' — v0 knows {}; store it and it can \
                 never be proposed (the plan parser denies unknown fields)",
                META_KEYS.join(", ")
            )));
        }
    }
    let inputs_val = map.remove(serde_yaml::Value::String("inputs".to_string()));
    let mut inputs = BTreeMap::new();
    if let Some(inputs_val) = inputs_val {
        let serde_yaml::Value::Mapping(specs) = inputs_val else {
            return Err(Error::rejected(
                "workflow `inputs:` must be a map of name → { ask, optional }",
            ));
        };
        for (k, v) in specs {
            let name = k.as_str().unwrap_or_default();
            if !valid_input_name(name) {
                return Err(Error::rejected(format!(
                    "input name '{}' — letters, digits, '-' or '_', starting with a \
                     letter or '_'",
                    name
                )));
            }
            let spec = match v {
                serde_yaml::Value::Null => InputSpec::default(),
                serde_yaml::Value::String(ask) => InputSpec {
                    ask: Some(ask),
                    optional: false,
                },
                serde_yaml::Value::Mapping(m) => {
                    let mut spec = InputSpec::default();
                    for (sk, sv) in m {
                        match (sk.as_str(), sv) {
                            (Some("ask"), serde_yaml::Value::String(s)) => spec.ask = Some(s),
                            (Some("optional"), serde_yaml::Value::Bool(b)) => spec.optional = b,
                            (Some(k), _) => {
                                return Err(Error::rejected(format!(
                                    "input '{name}': unknown key '{k}' — ask, optional"
                                )))
                            }
                            (None, _) => {
                                return Err(Error::rejected(format!(
                                    "input '{name}': keys must be strings — ask, optional"
                                )))
                            }
                        }
                    }
                    spec
                }
                _ => {
                    return Err(Error::rejected(format!(
                        "input '{name}' must be a string (the ask) or a map \
                         {{ ask, optional }}"
                    )))
                }
            };
            inputs.insert(name.to_string(), spec);
        }
    }
    Ok((map, inputs))
}

/// Scan for `{{` … `}}` placeholders: each inner name must be a
/// declared input. Any other `{{` is a template error — there is no
/// literal `{{`.
fn placeholders(text: &str, inputs: &BTreeMap<String, InputSpec>) -> Result<()> {
    let mut rest = text;
    while let Some(i) = rest.find("{{") {
        let inner = &rest[i + 2..];
        // A name is one token on one line — a `}}` found past a newline
        // or another `{{` belongs to a later placeholder; this one is
        // unclosed.
        let j = inner
            .find("}}")
            .filter(|j| !inner[..*j].contains('\n') && !inner[..*j].contains('{'));
        let Some(j) = j else {
            return Err(Error::rejected(
                "an unclosed '{{' — a placeholder is `{{name}}`, there is no literal `{{`",
            ));
        };
        let name = inner[..j].trim();
        if !valid_input_name(name) {
            return Err(Error::rejected(format!(
                "malformed placeholder '{{{{{name}}}}}' — letters, digits, '-' or '_', \
                 starting with a letter or '_'"
            )));
        }
        if !inputs.contains_key(name) {
            return Err(Error::rejected(format!(
                "placeholder '{{{{{name}}}}}' is not declared in `inputs:` — declared: {}",
                if inputs.is_empty() {
                    "none".to_string()
                } else {
                    inputs.keys().cloned().collect::<Vec<_>>().join(", ")
                }
            )));
        }
        rest = &inner[j + 2..];
    }
    Ok(())
}

/// One `##` section's leading metadata: the `key: value` lines in
/// [`TICKET_META_KEYS`] directly under the heading — the same rule
/// `parse_ticket` applies to its own keys, extended with the
/// workflow's extras. A `{{` in a `STATIC_META_KEYS` value refuses:
/// the dependency graph and sizes must be checkable statically.
fn ticket_meta(body: &str) -> Result<Vec<Vec<(String, String)>>> {
    let (_, sections) = split_sections(body);
    let mut out = Vec::with_capacity(sections.len());
    for (i, (heading, text)) in sections.iter().enumerate() {
        let label = format!("Ticket {} \"{heading}\"", i + 1);
        let mut meta = Vec::new();
        for line in text.split_inclusive('\n') {
            let line = line.trim_end_matches(['\n', '\r']);
            let Some((key, value)) = line.split_once(':') else {
                break;
            };
            let key = key.trim();
            if !TICKET_META_KEYS.contains(&key) {
                break;
            }
            let value = value.trim();
            if value.contains("{{") && STATIC_META_KEYS.contains(&key) {
                return Err(Error::rejected(format!(
                    "{label}: `{key}` cannot take a placeholder — it must be static so \
                     `workflow check` can verify it"
                )));
            }
            meta.push((key.to_string(), value.to_string()));
        }
        out.push(meta);
    }
    Ok(out)
}

/// Substitute `{{name}}` with `values[name]` over plain text; the
/// caller has already checked every placeholder is declared.
fn substitute(text: &str, values: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        let inner = &rest[i + 2..];
        let Some(j) = inner.find("}}") else {
            out.push_str("{{");
            rest = inner;
            continue;
        };
        let name = inner[..j].trim();
        out.push_str(values.get(name).map(String::as_str).unwrap_or(""));
        rest = &inner[j + 2..];
    }
    out.push_str(rest);
    out
}

/// Substitute inside YAML scalars — values can carry quotes and
/// newlines without breaking the frontmatter, because the mapping is
/// re-serialised after.
fn subst_yaml(value: &mut serde_yaml::Value, values: &BTreeMap<String, String>) {
    match value {
        serde_yaml::Value::String(s) => *s = substitute(s, values),
        serde_yaml::Value::Sequence(seq) => {
            for v in seq {
                subst_yaml(v, values);
            }
        }
        serde_yaml::Value::Mapping(map) => {
            let keys: Vec<serde_yaml::Value> = map.keys().cloned().collect();
            for mut k in keys {
                if let Some(mut v) = map.remove(&k) {
                    subst_yaml(&mut k, values);
                    subst_yaml(&mut v, values);
                    map.insert(k, v);
                }
            }
        }
        _ => {}
    }
}

/// `parse_template` — the format contract: frontmatter is a mapping of
/// [`META_KEYS`], `inputs:` is a well-formed spec map, every `{{name}}`
/// is declared, and no `size`/`depends_on` metadata takes a
/// placeholder. The ticket bodies are *not* parsed here — `check` and
/// `propose` run the rendered text through [`plan::parse_plan`].
pub fn parse_template(text: &str) -> Result<Template> {
    let (yaml, body) = parse::split_front(text).map_err(|e| {
        Error::rejected(format!(
            "{e} — a workflow is a plan file: frontmatter title, goal, non_goals, inputs"
        ))
    })?;
    let (_meta, inputs) = parse_front(yaml)?;
    placeholders(text, &inputs)?;
    ticket_meta(body)?;
    Ok(Template { inputs })
}

/// Render `text` with `values`: every `{{name}}` becomes its value
/// (absent inputs render empty). `inputs:` drops out of the
/// frontmatter — the result is a plan file for [`plan::parse_plan`].
/// Substitution inside the frontmatter runs over YAML scalars, so a
/// value carrying quotes or newlines cannot corrupt the meta; the body
/// substitutes verbatim.
fn render_values(text: &str, values: &BTreeMap<String, String>) -> Result<String> {
    let (yaml, body) = parse::split_front(text).map_err(|e| Error::rejected(e.to_string()))?;
    let mut meta: serde_yaml::Mapping = serde_yaml::from_str(yaml)
        .map_err(|e| Error::rejected(format!("workflow frontmatter: {e}")))?;
    meta.remove(serde_yaml::Value::String("inputs".to_string()));
    let mut meta = serde_yaml::Value::Mapping(meta);
    subst_yaml(&mut meta, values);
    let yaml = serde_yaml::to_string(&meta)
        .map_err(|e| Error::internal(format!("workflow frontmatter: {e}")))?;
    Ok(format!("---\n{yaml}---\n{}", substitute(body, values)))
}

/// Render the template with `provided` (`k=v` pairs): unknown names
/// and missing required inputs refuse with a named reason; absent
/// optionals render as empty.
pub fn render(text: &str, provided: &BTreeMap<String, String>) -> Result<String> {
    let tpl = parse_template(text)?;
    for k in provided.keys() {
        if !tpl.inputs.contains_key(k) {
            return Err(Error::rejected(format!(
                "unknown input '{k}' — the workflow takes: {}",
                if tpl.inputs.is_empty() {
                    "none (it has no inputs)".to_string()
                } else {
                    tpl.inputs.keys().cloned().collect::<Vec<_>>().join(", ")
                }
            )));
        }
    }
    let missing: Vec<String> = tpl
        .inputs
        .iter()
        .filter(|(name, spec)| !spec.optional && !provided.contains_key(name.as_str()))
        .map(|(name, spec)| match &spec.ask {
            Some(ask) => format!("'{name}' ({ask})"),
            None => format!("'{name}'"),
        })
        .collect();
    if !missing.is_empty() {
        return Err(Error::rejected(format!(
            "missing required input {} — pass `--input k=v` for each",
            missing.join(", ")
        )));
    }
    let values: BTreeMap<String, String> = tpl
        .inputs
        .keys()
        .map(|k| (k.clone(), provided.get(k).cloned().unwrap_or_default()))
        .collect();
    render_values(text, &values)
}

/// `render` for `check`: each `{{name}}` becomes its own name — a
/// value that is always a well-formed alias-shaped token, so the plan
/// structure (acceptance, metadata, `depends_on`) is checked exactly
/// as it will parse at propose.
fn canonical(text: &str, inputs: &BTreeMap<String, InputSpec>) -> Result<String> {
    let values: BTreeMap<String, String> = inputs.keys().map(|k| (k.clone(), k.clone())).collect();
    render_values(text, &values)
}

/// The plan file's body split at `##` headings — the same split
/// [`plan::parse_plan`] does: `(intro, sections)`.
fn split_sections(body: &str) -> (String, Vec<(String, String)>) {
    let mut intro = String::new();
    let mut sections: Vec<(String, String)> = vec![];
    let mut fences = parse::Fences::default();
    for line in body.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        if !fences.is_code(content) {
            if let Some((2, heading)) = parse::heading(content) {
                sections.push((heading.to_string(), String::new()));
                continue;
            }
        }
        match sections.last_mut() {
            Some((_, text)) => text.push_str(line),
            None => intro.push_str(line),
        }
    }
    (intro, sections)
}

/// Normalise a `depends_on` list for the gate: `1, [2]` and `2, 1`
/// read the same.
fn normalize_deps(value: &str) -> String {
    let mut toks: Vec<String> = value
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| t.strip_prefix('#').unwrap_or(t).to_string())
        .collect();
    toks.sort_unstable();
    toks.join(",")
}

/// The approval-affecting skeleton: the declared input names with
/// their required/optional flag (an input can feed `{{name}}` in a
/// gated position, so the contract is pinned — but never the `ask`
/// wording), the ticket count, and each ticket's metadata lines
/// (recognised keys, values normalised, sorted per ticket). Wording —
/// titles, prose, acceptance items, `ask` text — is not here, so a
/// wording-only edit keeps the digest.
pub fn gate_keys(text: &str) -> Result<String> {
    let (_yaml, body) = parse::split_front(text).map_err(|e| {
        Error::rejected(format!("{e} — a workflow is a plan file with frontmatter"))
    })?;
    let tpl = parse_template(text)?;
    let mut keys = String::from("inputs=");
    keys.push_str(
        &tpl.inputs
            .iter()
            .map(|(n, s)| format!("{n}{}", if s.optional { "?" } else { "!" }))
            .collect::<Vec<_>>()
            .join(","),
    );
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
pub fn known_agents(
    pm_dir: &Path,
    project_key: Option<&str>,
    daemon_aliases: &[String],
) -> (HashSet<String>, Vec<String>) {
    let mut agents = HashSet::new();
    let mut sources = Vec::new();
    if let Some(key) = project_key {
        if let Ok(text) = std::fs::read_to_string(pm_dir.join(key).join("PROJECT.md")) {
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
pub(crate) fn check_text(
    text: &str,
    agents: &HashSet<String>,
    agent_sources: &[String],
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
                         `reviewer:`/`tries:`/`uses:`"
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
                     the ticket body. Put `depends_on:` before `reviewer:`/`tries:`/`uses:`"
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
                notes.push(format!(
                    "{label}: reviewer is templated — reviewer≠agent is checked at render"
                ));
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
fn daemon_aliases(state_dir: &Path) -> Vec<String> {
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

/// `workflow add|edit` — validate, then write the file in one tracker
/// commit with the actor recorded. `add` refuses an existing file;
/// `edit` requires it. The check runs first: a workflow that cannot be
/// proposed is not stored.
pub fn write_file(
    pm: &Pm,
    project_key: &str,
    name: &str,
    text: &str,
    add: bool,
    state_dir: &Path,
    actor: &str,
) -> Result<Value> {
    check_name(name)?;
    if text.len() > plan::MAX_PLAN_BYTES {
        return Err(Error::rejected(format!(
            "Workflow is {} bytes — it renders to a plan, so the {}-byte cap applies; \
             split it",
            text.len(),
            plan::MAX_PLAN_BYTES
        )));
    }
    if !project::list(&pm.dir)?.iter().any(|p| p.key == project_key) {
        return Err(project::unknown_project(project_key, &pm.dir));
    }
    let file = file_of(&pm.dir, project_key, name)?;
    if file.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{} is a symlink — the tracker never follows links",
            file.display()
        )));
    }
    let verb = if add { "add" } else { "edit" };
    match (add, file.exists()) {
        (true, true) => {
            return Err(Error::rejected(format!(
                "workflow '{name}' already exists in {project_key} — `cadence workflow \
                 edit {name} --project {project_key}`"
            )))
        }
        (false, false) => {
            return Err(Error::rejected(format!(
                "no workflow '{name}' in {project_key} — `cadence workflow add`"
            )))
        }
        _ => {}
    }
    let daemon_agents = daemon_aliases(state_dir);
    let (agents, sources) = known_agents(&pm.dir, Some(project_key), &daemon_agents);
    let (errors, notes, _) = check_text(text, &agents, &sources);
    if !errors.is_empty() {
        return Err(Error::rejected(format!(
            "workflow {verb} refused — the file fails `workflow check`: {}",
            errors.join("; ")
        )));
    }
    let warnings = crate::secret::guard(&format!("workflow {name}"), text)?;
    let _lock = pm.lock()?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let old = if file.exists() {
        Some(std::fs::read_to_string(&file)?)
    } else {
        None
    };
    std::fs::write(&file, text)?;
    let foreign = match write::commit(
        pm,
        std::slice::from_ref(&file),
        &format!("{project_key}/{DIR}/{name}.md {verb}ed"),
        &[],
        actor,
    ) {
        Ok(f) => f,
        Err(e) => {
            if let Some(old) = &old {
                let _ = std::fs::write(&file, old);
            } else {
                let _ = std::fs::remove_file(&file);
            }
            return Err(e);
        }
    };
    let digest = gate_digest(text)?;
    let approvals = fetch_approvals(state_dir);
    let mut out = json!({
        "project": project_key,
        "name": name,
        "path": file,
        "committed": true,
        "digest": digest,
        "approved": approvals
            .as_ref()
            .map(|a| approved(project_key, name, &digest, Some(a)))
            .unwrap_or(false),
        "notes": notes,
    });
    // An edit that changes the gate keys silently loses approval —
    // name it.
    if let Some(old) = &old {
        let changed =
            gate_digest(old).ok().as_deref() != Some(out["digest"].as_str().unwrap_or_default());
        out["gate_changed"] = json!(changed);
        if changed
            && approvals
                .as_ref()
                .is_some_and(|a| a.get(&approval_key(project_key, name)).is_some())
        {
            out["unapproved"] = json!(format!(
                "gate keys changed — `cadence workflow approve {name} --project \
                 {project_key}` before `plan propose`"
            ));
        }
    }
    if !warnings.is_empty() {
        out["secret_warnings"] = crate::secret::warnings_json(&warnings);
    }
    write::attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// `workflow ls` — one row per `<pm>/<project>/workflows/*.md`.
pub fn ls(pm: &Pm, project_key: Option<&str>, state_dir: &Path) -> Result<Value> {
    let projects = project::list(&pm.dir)?;
    let projects: Vec<&project::Project> = match project_key {
        Some(key) => {
            let found: Vec<&project::Project> = projects.iter().filter(|p| p.key == key).collect();
            if found.is_empty() {
                return Err(project::unknown_project(key, &pm.dir));
            }
            found
        }
        None => projects.iter().collect(),
    };
    let approvals = fetch_approvals(state_dir);
    let mut rows = Vec::new();
    for p in projects {
        let dir = pm.dir.join(&p.key).join(DIR);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if e.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
                rows.push(json!({
                    "project": p.key, "name": name,
                    "error": "symlink — the tracker never follows links",
                }));
                continue;
            }
            let Some(name) = name.strip_suffix(".md").map(str::to_string) else {
                continue;
            };
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let mut row = json!({"project": p.key, "name": name, "path": path});
            match parse_template(&text) {
                Ok(tpl) => {
                    row["inputs"] = json!(tpl.inputs.keys().collect::<Vec<_>>());
                    match gate_digest(&text) {
                        Ok(digest) => {
                            row["digest"] = json!(digest);
                            row["approved"] = match &approvals {
                                Some(a) => {
                                    json!(approved(&p.key, &name, &digest, Some(a)))
                                }
                                None => json!("unknown — daemon unreachable"),
                            };
                        }
                        Err(e) => row["error"] = json!(e.to_string()),
                    }
                }
                Err(e) => row["error"] = json!(e.to_string()),
            }
            rows.push(row);
        }
    }
    rows.sort_by(|a, b| {
        (
            a["project"].as_str().unwrap_or(""),
            a["name"].as_str().unwrap_or(""),
        )
            .cmp(&(
                b["project"].as_str().unwrap_or(""),
                b["name"].as_str().unwrap_or(""),
            ))
    });
    Ok(json!({"workflows": rows, "count": rows.len()}))
}

/// `workflow show <name>` — the stored file's summary and approval
/// state.
pub fn show(pm: &Pm, project_key: &str, name: &str, state_dir: &Path) -> Result<Value> {
    if !project::list(&pm.dir)?.iter().any(|p| p.key == project_key) {
        return Err(project::unknown_project(project_key, &pm.dir));
    }
    let text = read_for(&pm.dir, project_key, name)?;
    let daemon_agents = daemon_aliases(state_dir);
    let (agents, sources) = known_agents(&pm.dir, Some(project_key), &daemon_agents);
    let (errors, notes, doc) = check_text(&text, &agents, &sources);
    let tpl = parse_template(&text).ok();
    let approvals = fetch_approvals(state_dir);
    let digest = gate_digest(&text).unwrap_or_default();
    Ok(json!({
        "project": project_key,
        "name": name,
        "path": file_of(&pm.dir, project_key, name)?,
        "ok": errors.is_empty(),
        "errors": errors,
        "notes": notes,
        "digest": digest,
        "approved": approvals
            .as_ref()
            .map(|a| approved(project_key, name, &digest, Some(a)))
            .map(|b| json!(b))
            .unwrap_or_else(|| json!("unknown — daemon unreachable")),
        "title": doc.as_ref().map(|d| d.title.clone()),
        "goal": doc.as_ref().map(|d| d.goal.clone()),
        "tickets": doc.map(|d| d.tickets.iter().enumerate().map(|(i, t)| json!({
            "n": i + 1,
            "title": t.title,
            "size": t.size,
            "agent": t.agent,
            "depends_on": t.depends_on.iter().map(|d| match d {
                plan::Dep::Ticket(k) => json!(k + 1),
                plan::Dep::Issue(i) => json!(i),
            }).collect::<Vec<_>>(),
            "acceptance": t.acceptance.len(),
        })).collect::<Vec<_>>()),
        "inputs": tpl.map(|t| t.inputs.iter().map(|(k, s)| json!({
            "name": k, "ask": s.ask, "optional": s.optional,
        })).collect::<Vec<_>>()),
    }))
}

/// `issue lint`'s view of `<pm>/<key>/workflows/` — each `.md` must be
/// a real file that parses as a template. Reported like the memory
/// dir's own lint.
pub fn lint_dir(
    dir: &Path,
    project_key: &str,
    err: &mut dyn FnMut(String),
    warn: &mut dyn FnMut(String),
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if e.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
            err(format!(
                "{project_key}/{DIR}/{name}: symlink — the board never follows links"
            ));
            continue;
        }
        if !name.ends_with(".md") {
            warn(format!(
                "{project_key}/{DIR}/{name}: not a workflow — workflow files end in .md"
            ));
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Err(e) = parse_template(&text) {
            err(format!("{project_key}/{DIR}/{name}: {e}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WF: &str = "---\ntitle: \"Post: {{topic}}\"\ngoal: \"Publish {{topic}} for {{keyword}}\"\n\
inputs:\n  topic: { ask: \"About what?\" }\n  keyword: { ask: \"Phrase\", optional: true }\n---\n\n\
Why.\n\n## Research {{topic}}\nagent: dev-1\nsize: S\n\nDo it.\n\n### Acceptance\n- [ ] brief written\n\n\
## Write\nagent: dev-2\ndepends_on: 1\n\n### Acceptance\n- [ ] post done\n";

    fn inputs(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn template_parses_inputs_and_placeholders() {
        let tpl = parse_template(WF).unwrap();
        assert_eq!(tpl.inputs.len(), 2);
        assert!(!tpl.inputs["topic"].optional);
        assert!(tpl.inputs["keyword"].optional);
    }

    #[test]
    fn render_substitutes_and_drops_inputs() {
        let out = render(WF, &inputs(&[("topic", "rust"), ("keyword", "cargo")])).unwrap();
        assert!(
            out.contains("title: 'Post: rust'") || out.contains("Post: rust"),
            "{out}"
        );
        assert!(!out.contains("inputs:"), "{out}");
        assert!(!out.contains("{{"), "{out}");
        // The rendered text is a plan the unchanged parser accepts.
        let doc = plan::parse_plan(&out).unwrap();
        assert_eq!(doc.title, "Post: rust");
        assert_eq!(doc.tickets.len(), 2);
        assert_eq!(doc.tickets[0].title, "Research rust");
        assert_eq!(doc.tickets[0].agent.as_deref(), Some("dev-1"));
        assert_eq!(doc.tickets[1].depends_on, vec![plan::Dep::Ticket(0)]);
    }

    #[test]
    fn render_refusals_name_the_input() {
        // Missing required.
        let e = render(WF, &inputs(&[])).unwrap_err().to_string();
        assert!(
            e.contains("missing required input") && e.contains("'topic'"),
            "{e}"
        );
        assert!(e.contains("About what?"), "{e}");
        // Unknown input.
        let e = render(WF, &inputs(&[("topic", "x"), ("bogus", "y")]))
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("unknown input 'bogus'") && e.contains("topic"),
            "{e}"
        );
        // Optional input may be absent — renders empty.
        let out = render(WF, &inputs(&[("topic", "x")])).unwrap();
        assert!(!out.contains("{{"), "{out}");
    }

    #[test]
    fn template_refuses_undeclared_and_malformed_placeholders() {
        let undeclared = WF.replace("{{keyword}}", "{{nope}}");
        let e = parse_template(&undeclared).unwrap_err().to_string();
        assert!(e.contains("{{nope}}") && e.contains("not declared"), "{e}");
        let unclosed = WF.replace("{{topic}}", "{{topic");
        let e = parse_template(&unclosed).unwrap_err().to_string();
        assert!(e.contains("unclosed '{{'"), "{e}");
        let malformed = WF.replace("{{topic}}", "{{a b}}");
        let e = parse_template(&malformed).unwrap_err().to_string();
        assert!(e.contains("malformed placeholder"), "{e}");
    }

    #[test]
    fn template_refuses_static_meta_placeholders() {
        // Declare the inputs so the refusal is the static-meta one.
        let bad = WF
            .replace("depends_on: 1", "depends_on: {{n}}")
            .replace("  keyword:", "  n: {}\n  keyword:");
        let e = parse_template(&bad).unwrap_err().to_string();
        assert!(e.contains("depends_on") && e.contains("placeholder"), "{e}");
        let bad = WF
            .replace("size: S", "size: {{s}}")
            .replace("  keyword:", "  s: {}\n  keyword:");
        let e = parse_template(&bad).unwrap_err().to_string();
        assert!(e.contains("size") && e.contains("placeholder"), "{e}");
    }

    #[test]
    fn template_refuses_unknown_frontmatter_keys() {
        let bad = WF.replace("inputs:", "runs: daily\ninputs:");
        let e = parse_template(&bad).unwrap_err().to_string();
        assert!(e.contains("'runs'"), "{e}");
    }

    #[test]
    fn gate_digest_stable_on_wording_changes_only() {
        let approved = gate_digest(WF).unwrap();
        // Wording: title, goal, prose, acceptance, ask text.
        let wording = WF
            .replace("Post: {{topic}}", "A post about {{topic}}!")
            .replace("Do it.", "Do it thoroughly, twice over.")
            .replace("brief written", "brief written and reviewed")
            .replace("About what?", "Pick a topic");
        assert_eq!(gate_digest(&wording).unwrap(), approved);
        // Approval-affecting: agent, depends_on, size, ticket count,
        // reviewer/tries/uses lines, input contract.
        for (from, to) in [
            ("agent: dev-1", "agent: dev-9"),
            ("depends_on: 1", "depends_on: "),
            ("size: S", "size: L"),
            ("optional: true", "optional: false"),
            ("  keyword: { ask: \"Phrase\", optional: true }", ""),
        ] {
            let edited = WF.replace(from, to);
            assert_ne!(
                gate_digest(&edited).unwrap_or_default(),
                approved,
                "{from} -> {to} must change the digest"
            );
        }
        let dropped_ticket = WF.split("## Write").next().unwrap().to_string();
        assert_ne!(gate_digest(&dropped_ticket).unwrap(), approved);
        let reviewer = WF.replace("size: S", "size: S\nreviewer: qa-1");
        assert_ne!(gate_digest(&reviewer).unwrap(), approved);
        // depends_on order/punctuation normalises.
        let dep_order = WF.replace("depends_on: 1", "depends_on: [1]");
        assert_eq!(gate_digest(&dep_order).unwrap(), approved);
    }

    #[test]
    fn check_text_refusals() {
        let agents: HashSet<String> = ["dev-1", "dev-2", "qa-1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let sources = vec!["test".to_string()];
        let (errors, _, doc) = check_text(WF, &agents, &sources);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(doc.unwrap().tickets.len(), 2);

        // Unknown agent.
        let bad = WF.replace("agent: dev-1", "agent: nobody");
        let (errors, _, _) = check_text(&bad, &agents, &sources);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("'nobody'") && e.contains("unknown")),
            "{errors:?}"
        );

        // depends_on cycle: ticket 1 depends on 2 and 2 on 1 — parses
        // (deps may name later tickets), check refuses. The line must
        // sit in the metadata block under the heading.
        let cyclic = WF.replacen("size: S", "size: S\ndepends_on: 2", 1);
        let (errors, _, _) = check_text(&cyclic, &agents, &sources);
        assert!(errors.iter().any(|e| e.contains("cycle")), "{errors:?}");

        // No acceptance — the plan parser refuses it.
        let no_acc = WF.replace("- [ ] post done\n", "");
        let (errors, _, _) = check_text(&no_acc, &agents, &sources);
        assert!(
            errors.iter().any(|e| e.contains("acceptance")),
            "{errors:?}"
        );

        // reviewer == the ticket's own agent.
        let self_review = WF.replace(
            "agent: dev-1\nsize: S",
            "agent: dev-1\nsize: S\nreviewer: dev-1",
        );
        let (errors, _, _) = check_text(&self_review, &agents, &sources);
        assert!(errors.iter().any(|e| e.contains("own agent")), "{errors:?}");

        // reviewer ≠ agent is fine; the extra key is gated.
        let ok_review = WF.replace(
            "agent: dev-1\nsize: S",
            "agent: dev-1\nsize: S\nreviewer: qa-1",
        );
        let (errors, _, _) = check_text(&ok_review, &agents, &sources);
        assert!(errors.is_empty(), "{errors:?}");

        // Unresolved placeholder — parse_template refuses.
        let undeclared = WF.replace("{{keyword}}", "{{nope}}");
        let (errors, _, _) = check_text(&undeclared, &agents, &sources);
        assert!(
            errors.iter().any(|e| e.contains("not declared")),
            "{errors:?}"
        );

        // A plan-parser key after an unknown meta line is body text —
        // the gate would claim a skeleton the run never sees.
        let misordered = WF.replace(
            "agent: dev-2\ndepends_on: 1",
            "agent: dev-2\nreviewer: qa-1\ndepends_on: 1",
        );
        let (errors, _, _) = check_text(&misordered, &agents, &sources);
        assert!(
            errors.iter().any(|e| e.contains("depends_on")),
            "{errors:?}"
        );
    }

    #[test]
    fn names_and_paths_are_safe() {
        let long = "x".repeat(40);
        for bad in ["../x", "a/b", ".x", "UPPER", "", long.as_str()] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
        for ok in ["code-change", "x", "a1-b2"] {
            check_name(ok).unwrap();
        }
    }

    #[test]
    fn yaml_values_cannot_break_frontmatter() {
        // Quotes and colons stay inside the scalar.
        let out = render(WF, &inputs(&[("topic", "a: \"quoted\""), ("keyword", "k")])).unwrap();
        let doc = plan::parse_plan(&out).unwrap();
        assert_eq!(doc.title, "Post: a: \"quoted\"");
        // A newline can't corrupt the frontmatter either — it lands in
        // the scalar and the plan parser refuses the field, named.
        let out = render(WF, &inputs(&[("topic", "x\ny")])).unwrap();
        let e = plan::parse_plan(&out).unwrap_err().to_string();
        assert!(e.contains("one line"), "{e}");
    }
}
