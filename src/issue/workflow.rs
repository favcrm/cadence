//! Workflows (CAD-487): a workflow is a reusable plan file with
//! inputs, kept in the tracker next to PROJECT.md as
//! `<pm>/<project>/workflows/<name>.md`. The CLI is the only writer —
//! `workflow add`/`edit` land as one tracker commit each with `Actor:`
//! recorded, and `workflow check`/`ls`/`show` read.
//!
//! The format is the plan format ([`crate::issue::plan`]) plus an
//! `inputs:` frontmatter map — `name: { ask, optional, kind, example, context_default, default }`
//! — and `{{name}}` placeholders anywhere in the file. `plan propose
//! --workflow` renders the file: placeholders take the `--input`
//! values, `inputs:` drops out of the frontmatter, and the result goes
//! through the unchanged propose → approve → gate path (the plan
//! parser denies unknown fields, so it never sees `inputs`). A `{{`
//! that is not a well-formed `{{name}}` is a template error, never
//! literal text — the same fail-loud convention as the plan parser's
//! `deny_unknown_fields`.
//!
//! An input may declare a shape (`kind: slug`, CAD-571): render refuses
//! a value that does not match it — `bad_shape` — for every caller, so
//! a folder name like `../x` never reaches the plan prose. The shape is
//! approval-affecting (it decides what is accepted), so it is part of
//! the gate keys; `ask` and `example` are wording and are not.
//!
//! The file is gated like PROJECT.md's work keys (CAD-405): the
//! operator's `workflow approve` records a digest of its *gate keys* —
//! the parts that decide who does the work and in what order: the
//! ticket count and each ticket's metadata lines (`size`, `agent`,
//! `depends_on`, plus `reviewer`, `tries`, `uses`, which the plan
//! parser does not consume yet but a workflow may already carry for
//! the check and the gate). Wording — the title, goal, descriptions,
//! acceptance text, `inputs`' asks and examples — is not gated. A
//! structural edit, CLI or hand, changes the digest, so `propose`
//! refuses `workflow_unapproved` until the operator approves the new
//! keys; a rendered plan still needs `plan approve` before any ticket
//! dispatches. The gate is a process guard, not a security boundary —
//! same as the plan gate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::model;
use crate::issue::parse::{self};
use crate::issue::{plan, project, write, Pm};

mod check;
mod front;
mod render;

pub(crate) use check::check_installed_text;
pub use check::check_text;
use check::daemon_aliases;
pub use check::{
    approval_key, approved, check, fetch_approvals, gate_digest, gate_keys, known_agents,
};
use front::{parse_front, placeholders};
pub use front::{refuse_skill_directive, skill_tags};
pub(crate) use front::{ticket_meta, TicketMetas};
pub(crate) use render::{bounded_content, visible_source_facts};
use render::{canonical, normalize_deps, split_sections};
pub use render::{check_context_defaults, render, source_input_line};
#[cfg(test)]
use render::{check_rendered, render_values};

/// `<pm>/<project>/workflows/` — beside PROJECT.md.
pub const DIR: &str = "workflows";

/// Frontmatter keys a workflow file may carry: the plan's own three,
/// plus `inputs` and `distinct` (inputs whose values must differ at
/// render — `distinct: [worker, reviewer]` keeps a reviewer from being
/// the worker) and `label` (the human name the board's primary action
/// uses — "New post"; wording, like the title). Anything else refuses
/// at parse — the same fail-loud rule the plan parser applies with
/// `deny_unknown_fields`.
pub(super) const META_KEYS: &[&str] = &[
    "title",
    "goal",
    "non_goals",
    "inputs",
    "distinct",
    "label",
    "publication_slot",
    "capability_slots",
    "required_asset_slot",
    "execution",
];

/// Workflow-only frontmatter keys: pulled out at parse and removed
/// before rendering, because the plan parser denies unknown fields.
pub(super) const WORKFLOW_ONLY_KEYS: &[&str] = &[
    "inputs",
    "distinct",
    "label",
    "publication_slot",
    "capability_slots",
    "required_asset_slot",
    "execution",
];

/// Ticket metadata lines a workflow recognises: the plan's own plus
/// the approval-affecting fields later stages add (a `reviewer:` line
/// is checked, and `tries:`/`uses:` gate the file, even though the v0
/// plan parser leaves them in the issue body). CAD-1867: `skill:`
/// names the installed bundle's skill package for that step — a static
/// tag, resolved only by an installed app run.
pub(super) const TICKET_META_KEYS: &[&str] = &[
    "size",
    "agent",
    "depends_on",
    "reviewer",
    "tries",
    "uses",
    "action",
    "skill",
];

/// Metadata fields that must be static so `workflow check` can verify
/// them — a placeholder here is refused.
pub(super) const STATIC_META_KEYS: &[&str] = &["size", "depends_on", "action", "skill"];

/// One `inputs:` entry: what the proposer is asked, whether the run may
/// leave it blank, the shape its value must match at render (CAD-571),
/// and an example the run form shows in the empty field.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InputSpec {
    pub ask: Option<String>,
    pub optional: bool,
    pub kind: Option<InputKind>,
    pub example: Option<String>,
    /// Explicit content-only permission for installation context defaults.
    pub context_default: bool,
    /// App-authored, content-only fallback frozen into a new run. A context
    /// default and then a nonempty run input may override it.
    pub default: Option<String>,
}

/// A declared input shape: the value must match it at render, for every
/// caller. `slug` is the v0 kind — a folder name, safe as one path
/// segment (the app pages' `posts/<slug>/` prose, a drawer's live check).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    Slug,
}

impl InputKind {
    /// The declared spelling (`kind: slug`).
    pub fn as_str(self) -> &'static str {
        match self {
            InputKind::Slug => "slug",
        }
    }

    pub(super) fn parse(s: &str) -> Option<InputKind> {
        match s {
            "slug" => Some(InputKind::Slug),
            _ => None,
        }
    }
}

/// The longest folder name a `slug` input may carry — the same cap the
/// board's drawer applies (`slugProblem` in ui/.../apps.ts).
const SLUG_MAX: usize = 60;

/// Why `value` is not shaped as `kind`, or `None` when it is. The rule
/// is one place: render's refusal and the `example:` check both read it.
pub(super) fn shape_problem(kind: InputKind, value: &str) -> Option<&'static str> {
    match kind {
        InputKind::Slug => {
            if value.is_empty() || value.len() > SLUG_MAX {
                Some("1-60 characters")
            } else if value.split('-').any(str::is_empty) {
                Some("no leading, trailing or doubled hyphen")
            } else if !value
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                Some("lowercase letters, digits and hyphens only")
            } else {
                None
            }
        }
    }
}

/// The shape refusal for `name`'s value, or `Ok(())`.
pub(super) fn check_shape(kind: InputKind, name: &str, value: &str) -> Result<()> {
    if let Some(why) = shape_problem(kind, value) {
        return Err(Error::invalid(
            "bad_shape",
            format!(
                "input '{name}' must be a folder name (`kind: {}`) — {why}; got '{value}'",
                kind.as_str()
            ),
        ));
    }
    Ok(())
}

/// A parsed workflow template: the declared inputs, the `distinct:`
/// group — inputs whose rendered values must pairwise differ — and the
/// optional human `label:`. `input_order` is the file's own order (the
/// run form shows the inputs as the author wrote them; the map is
/// sorted). The plan structure itself is checked by rendering and
/// running [`plan::parse_plan`].
/// CAD-1171: who executes a run of this workflow. `Agent` (the default)
/// routes every step through its assigned worker; `Host` executes the
/// declared capability slots in-process for the operator's own click —
/// no worker, no owner PM — and is refused for any workflow that would
/// produce text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Execution {
    Agent,
    Host,
}

impl Execution {
    pub fn as_str(self) -> &'static str {
        match self {
            Execution::Agent => "agent",
            Execution::Host => "host",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Template {
    pub inputs: BTreeMap<String, InputSpec>,
    /// The `inputs:` names in declared order.
    pub input_order: Vec<String>,
    pub distinct: Vec<String>,
    pub label: Option<String>,
    pub publication_slot: Option<String>,
    pub capability_slots: Vec<String>,
    pub required_asset_slot: Option<String>,
    pub execution: Execution,
}

/// The declared inputs as the board renders them — file order, each
/// with its ask, optionality, declared shape and example.
pub fn inputs_json(tpl: &Template) -> Vec<Value> {
    let mut names = tpl.input_order.clone();
    for name in tpl.inputs.keys() {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
        .iter()
        .filter_map(|name| {
            tpl.inputs.get(name).map(|spec| {
                json!({
                    "name": name, "ask": spec.ask, "optional": spec.optional,
                    "kind": spec.kind.map(InputKind::as_str),
                    "example": spec.example,
                    "context_default": spec.context_default,
                    "default": spec.default,
                })
            })
        })
        .collect()
}

/// `{{name}}` — the input name is a bare word, like an alias but with
/// an optional leading `_`.
pub(super) fn valid_input_name(name: &str) -> bool {
    name.len() <= 64
        && !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The whole value is exactly `{{name}}` — returns the input name.
/// Only a bare placeholder renders exactly its input, which is what
/// makes `distinct:` meaningful; `x-{{a}}` mixes literal text in. The
/// name trims the way `placeholders` and `substitute` already trim,
/// so `{{ worker }}` names `worker`'s position for the distinct check.
pub(super) fn bare_placeholder(v: &str) -> Option<&str> {
    v.strip_prefix("{{")?
        .strip_suffix("}}")
        .map(str::trim)
        .filter(|n| valid_input_name(n))
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
/// `project` is a key, never a path fragment: `..`, `/` and absolute
/// paths refuse here so no caller reads outside the tracker.
fn dir_of(pm_dir: &Path, project: &str) -> Result<PathBuf> {
    model::check_key(project)?;
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
pub(super) fn file_of(pm_dir: &Path, project: &str, name: &str) -> Result<PathBuf> {
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
    let front = parse_front(yaml)?;
    placeholders(text, &front.inputs)?;
    let metadata = ticket_meta(body)?;
    for (name, spec) in &front.inputs {
        if !spec.context_default {
            continue;
        }
        if front.distinct.contains(name)
            || metadata.iter().flatten().any(|(_, value)| {
                let mut rest = value.as_str();
                while let Some(start) = rest.find("{{") {
                    let inner = &rest[start + 2..];
                    let Some(end) = inner.find("}}") else {
                        break;
                    };
                    if inner[..end].trim() == name {
                        return true;
                    }
                    rest = &inner[end + 2..];
                }
                false
            })
        {
            return Err(Error::rejected(
                "context defaults cannot reference structural workflow inputs",
            ));
        }
    }
    Ok(Template {
        inputs: front.inputs,
        input_order: front.input_order,
        distinct: front.distinct,
        label: front.label,
        publication_slot: front.publication_slot,
        capability_slots: front.capability_slots,
        required_asset_slot: front.required_asset_slot,
        execution: front.execution,
    })
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
    // The exists check runs under the lock — two concurrent `add`s must
    // not both pass it.
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
        // A symlinked workflows/ dir is never followed — name it in the
        // listing rather than silently walking outside the tracker.
        if dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            rows.push(json!({
                "project": p.key, "name": null,
                "error": "workflows/ is a symlink — the tracker never follows links",
            }));
            continue;
        }
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
        "inputs": tpl.as_ref().map(inputs_json),
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
    if dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        err(format!(
            "{project_key}/{DIR}: symlink — the board never follows links"
        ));
        return;
    }
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
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn cad692_publication_slot_is_static_gate_material_and_removed_from_plan() {
        let publication = WF.replace("inputs:", "publication_slot: publication\ninputs:");
        assert_eq!(
            parse_template(&publication)
                .unwrap()
                .publication_slot
                .as_deref(),
            Some("publication")
        );
        assert_ne!(gate_digest(&publication).unwrap(), gate_digest(WF).unwrap());
        let rendered = render(&publication, &inputs(&[("topic", "rust")])).unwrap();
        assert!(!rendered.contains("publication_slot:"));
        assert!(plan::parse_plan(&rendered).is_ok());
        for slot in ["{{topic}}", "[publication]", "bad.name", "null", "''"] {
            let bad = WF.replace("inputs:", &format!("publication_slot: {slot}\ninputs:"));
            assert!(parse_template(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn cad632_capability_slots_are_static_distinct_gate_material() {
        let configured = WF.replace("inputs:", "capability_slots: [source, image]\ninputs:");
        let template = parse_template(&configured).unwrap();
        assert_eq!(template.capability_slots, vec!["source", "image"]);
        assert_ne!(
            gate_digest(&configured).unwrap(),
            gate_digest(WF).unwrap(),
            "capability declarations must require fresh installation approval"
        );
        assert!(!render(&configured, &inputs(&[("topic", "rust")]))
            .unwrap()
            .contains("capability_slots:"));
        for declaration in ["[source, source]", "[]", "[{{source}}]"] {
            let bad = WF.replace(
                "inputs:",
                &format!("capability_slots: {declaration}\ninputs:"),
            );
            assert!(parse_template(&bad).is_err(), "{declaration}");
        }
    }

    #[test]
    fn cad714_required_asset_slot_is_declared_and_approval_affecting() {
        let configured = WF.replace(
            "inputs:",
            "capability_slots: [image]\nrequired_asset_slot: image\ninputs:",
        );
        assert_eq!(
            parse_template(&configured)
                .unwrap()
                .required_asset_slot
                .as_deref(),
            Some("image")
        );
        let optional = configured.replace("required_asset_slot: image\n", "");
        assert_ne!(
            gate_digest(&configured).unwrap(),
            gate_digest(&optional).unwrap()
        );
        let rendered = render(&configured, &inputs(&[("topic", "rust")])).unwrap();
        assert!(!rendered.contains("required_asset_slot:"));
        assert!(plan::parse_plan(&rendered).is_ok());
        for declaration in ["source", "{{topic}}", "[image]", "null"] {
            let bad = WF.replace(
                "inputs:",
                &format!("capability_slots: [image]\nrequired_asset_slot: {declaration}\ninputs:"),
            );
            assert!(parse_template(&bad).is_err(), "{declaration}");
        }
    }

    #[test]
    fn cad632_provider_caption_is_derived_as_one_safe_line() {
        assert_eq!(
            source_input_line("  Lunch\nserved\tnoon ❤️  ").unwrap(),
            "Lunch served noon ❤"
        );
        assert!(source_input_line("\n\t").is_err());
    }

    #[test]
    fn cad742_content_default_is_bounded_frozen_and_approval_affecting() {
        let with_default = WF.replace(
            "keyword: { ask: \"Phrase\", optional: true }",
            "keyword: { ask: \"Phrase\", optional: true, context_default: true, default: \"Write briefly\" }",
        );
        let template = parse_template(&with_default).unwrap();
        assert_eq!(
            inputs_json(&template)[1]["default"],
            serde_json::json!("Write briefly")
        );
        let fallback = render(&with_default, &inputs(&[("topic", "rust")])).unwrap();
        assert!(fallback.contains("Write briefly"));
        let reset = render(
            &with_default,
            &inputs(&[("topic", "rust"), ("keyword", "")]),
        )
        .unwrap();
        assert_eq!(fallback, reset);
        let changed = with_default.replace("Write briefly", "Write warmly");
        assert_ne!(
            gate_digest(&with_default).unwrap(),
            gate_digest(&changed).unwrap()
        );
        for value in [" Bad", "x\u{202e}y"] {
            let bad = with_default.replace("Write briefly", value);
            assert!(parse_template(&bad).is_err());
        }
        assert!(render(
            &with_default,
            &inputs(&[("topic", "rust"), ("keyword", "Bad\nstep")])
        )
        .is_err());
        let long = "x".repeat(513);
        assert!(render(
            &with_default,
            &inputs(&[("topic", "rust"), ("keyword", &long)])
        )
        .is_err());
        assert!(
            check_context_defaults(&with_default, &BTreeMap::from([("keyword".into(), long)]))
                .is_err()
        );
        assert!(visible_source_facts("Two  spaces preserve quoted facts"));
        assert!(!visible_source_facts("Line one\nLine two"));
    }

    #[test]
    fn cad742_manual_image_workflow_retains_prompt_guidance_for_both_steps() {
        let text =
            include_str!("../../tests/fixtures/apps/social-content/workflows/image-manual.md");
        let template = parse_template(text).unwrap();
        assert_eq!(template.capability_slots, vec!["image"]);
        assert_eq!(template.required_asset_slot.as_deref(), Some("image"));
        let workflow = crate::store::app_runs::LocalWorkflow::parse(
            text,
            &BTreeMap::from([
                ("subject".into(), "Customer follow-up".into()),
                (
                    "source".into(),
                    "JuicySuite CRM helps teams follow up".into(),
                ),
                ("writer".into(), "op-social-writer".into()),
                ("reviewer".into(), "op-social-reviewer".into()),
            ]),
        )
        .unwrap();
        assert_eq!(workflow.steps.len(), 2);
        for step in &workflow.steps {
            assert!(step.instruction.contains("Write a concise zh-HK caption"));
            assert!(step.instruction.contains("Create one editorial image"));
        }
    }

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
        assert_eq!(tpl.label, None, "no label is fine");
        // The declared order is kept for the board's form: `topic`
        // before `keyword` though the map sorts them the other way.
        assert_eq!(tpl.input_order, vec!["topic", "keyword"]);
        let rows = inputs_json(&tpl);
        assert_eq!(rows[0]["name"], "topic", "{rows:?}");
        assert_eq!(rows[1]["name"], "keyword", "{rows:?}");
    }

    /// CAD-563: `label:` is the human name the board's primary action
    /// uses — accepted, and wording: it is dropped before rendering (the
    /// plan parser denies unknown fields) and it never enters the gate
    /// digest, so a label-only edit keeps an approval.
    #[test]
    fn template_label_is_wording_not_gate() {
        let labelled = WF.replace("inputs:", "label: New post\ninputs:");
        let tpl = parse_template(&labelled).unwrap();
        assert_eq!(tpl.label.as_deref(), Some("New post"));
        // The render drops it and still parses as a plan.
        let out = render(&labelled, &inputs(&[("topic", "rust")])).unwrap();
        assert!(!out.contains("label:"), "{out}");
        assert!(plan::parse_plan(&out).is_ok(), "{out}");
        // The gate digest is unchanged by the label.
        assert_eq!(
            gate_digest(&labelled).unwrap(),
            gate_digest(WF).unwrap(),
            "a label-only edit is wording"
        );
        for bad in [
            WF.replace("inputs:", "label: [a]\ninputs:"),
            WF.replace("inputs:", &format!("label: '{}'\ninputs:", "x".repeat(61))),
        ] {
            assert!(parse_template(&bad).is_err(), "{bad}");
        }
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

    /// CAD-571 N3: the `inputs:` map's order is presentation (the run
    /// form shows the author's order), never the gate — reordering the
    /// declarations keeps the digest, so an existing approval survives.
    #[test]
    fn gate_digest_stable_on_input_reorder() {
        let reordered = WF.replace(
            "  topic: { ask: \"About what?\" }\n  keyword: { ask: \"Phrase\", optional: true }",
            "  keyword: { ask: \"Phrase\", optional: true }\n  topic: { ask: \"About what?\" }",
        );
        assert_ne!(reordered, WF, "the fixture must actually reorder");
        assert_eq!(gate_digest(&reordered).unwrap(), gate_digest(WF).unwrap());
        assert_eq!(
            parse_template(&reordered).unwrap().input_order,
            vec!["keyword", "topic"],
            "the form keeps the file's order"
        );
    }

    /// CAD-571: `kind: slug` shapes the value at render — the hostile
    /// and malformed folder names refuse `bad_shape` naming the input,
    /// a folder name renders into the prose, and the declaration is
    /// structural (gate keys). `example:` is wording: it never moves
    /// the digest, and one the shape would refuse refuses the file.
    #[test]
    fn input_kind_slug_shapes_the_value_at_render() {
        const SHAPED: &str = "---\ntitle: \"Post: {{topic}}\"\ngoal: \"Write {{topic}}\"\n\
label: New post\ninputs:\n  topic: { ask: \"About what?\", example: \"How we onboard\" }\n  slug: { ask: \"Folder name\", kind: slug }\n---\n\n\
## Write {{topic}}\nagent: dev-1\n\nposts/{{slug}}/post.md\n\n### Acceptance\n- [ ] written\n";
        let tpl = parse_template(SHAPED).unwrap();
        assert_eq!(tpl.inputs["slug"].kind, Some(InputKind::Slug));
        assert_eq!(
            tpl.inputs["topic"].example.as_deref(),
            Some("How we onboard")
        );
        let rows = inputs_json(&tpl);
        assert_eq!(rows[1]["kind"], "slug", "{rows:?}");
        assert_eq!(rows[0]["example"], "How we onboard", "{rows:?}");

        let out = render(SHAPED, &inputs(&[("topic", "t"), ("slug", "my-post")])).unwrap();
        assert!(out.contains("posts/my-post/post.md"), "{out}");

        for bad in [
            "../x",
            "/",
            "Upper",
            "with space",
            "a--b",
            "-lead",
            "trail-",
            "under_score",
            "dot.name",
        ] {
            let e = render(SHAPED, &inputs(&[("topic", "t"), ("slug", bad)])).unwrap_err();
            assert_eq!(e.code(), Some("bad_shape"), "{bad:?}: {e}");
            assert!(e.to_string().contains("'slug'"), "{bad:?}: {e}");
        }
        let long = "x".repeat(SLUG_MAX + 1);
        assert_eq!(
            render(SHAPED, &inputs(&[("topic", "t"), ("slug", &long)]))
                .unwrap_err()
                .code(),
            Some("bad_shape"),
            "over the cap"
        );
        // The cap boundary is inclusive.
        let exact = "x".repeat(SLUG_MAX);
        assert!(render(SHAPED, &inputs(&[("topic", "t"), ("slug", &exact)])).is_ok());

        // The shape is structural; the example is wording.
        let no_shape = SHAPED.replace(", kind: slug", "");
        assert_ne!(
            gate_digest(SHAPED).unwrap(),
            gate_digest(&no_shape).unwrap()
        );
        let no_example = SHAPED.replace(", example: \"How we onboard\"", "");
        assert_eq!(
            gate_digest(SHAPED).unwrap(),
            gate_digest(&no_example).unwrap(),
            "an example is wording"
        );
        // An example the shape refuses refuses the file; a shaped one
        // parses; an unknown kind and a non-string one refuse.
        let e = parse_template(&SHAPED.replace("kind: slug", "kind: slug, example: \"My Post\""))
            .unwrap_err()
            .to_string();
        assert!(e.contains("does not fit"), "{e}");
        let ok_example = SHAPED.replace("kind: slug", "kind: slug, example: \"my-post\"");
        assert_eq!(
            parse_template(&ok_example).unwrap().inputs["slug"]
                .example
                .as_deref(),
            Some("my-post")
        );
        let e = parse_template(&SHAPED.replace("kind: slug", "kind: folder"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("unknown kind"), "{e}");
        let e = parse_template(&SHAPED.replace("kind: slug", "kind: 3"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("shape name"), "{e}");
        // A shaped input that is optional and absent still renders
        // empty (the declaration is the author's; the form asks for it).
        let optional = SHAPED.replace("kind: slug", "kind: slug, optional: true");
        assert!(render(&optional, &inputs(&[("topic", "t")])).is_ok());
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
        // A newline never reaches the frontmatter — the value is
        // refused first, with the named reason.
        let e = render(WF, &inputs(&[("topic", "x\ny")])).unwrap_err();
        assert_eq!(e.code(), Some("one_line"), "{e}");
    }

    #[test]
    fn input_values_are_single_line() {
        // \n and \r split lines; other control characters (tab, NUL)
        // are refused too — all can smuggle structure or mislead a
        // reader. The named reason is `one_line`.
        for bad in ["x\ny", "x\ry", "x\ty", "x\u{0}y", "x\u{7}y"] {
            let e = render(WF, &inputs(&[("topic", bad)])).unwrap_err();
            assert_eq!(e.code(), Some("one_line"), "{bad:?} -> {e}");
            assert!(e.to_string().contains("'topic'"), "{e}");
        }
        // Mutation proof: if the check goes, the refusal's code does.
        // Unicode that is not control — punctuation, spaces — is fine.
        render(WF, &inputs(&[("topic", "a — b: c; d")])).unwrap();
    }

    #[test]
    fn input_values_refuse_invisible_and_separator_chars() {
        // CAD-487 r3: the pane-agent payloads that slipped a distinct
        // self-review past the raw-string compare. Every class the
        // review named, as interior and trailing positions.
        for bad in [
            "dev-1\u{2028}", // LINE SEPARATOR — parser trims it
            "dev-1\u{2029}", // PARAGRAPH SEPARATOR
            "dev-1 ",        // trailing ASCII space — trimmed
            " dev-1",        // leading ASCII space
            "dev-1\u{A0}",   // NBSP — is_whitespace, trimmed
            "dev-1\u{2007}", // FIGURE SPACE
            "dev-1\u{3000}", // IDEOGRAPHIC SPACE
            "dev\u{200B}1",  // ZERO WIDTH SPACE — interior invisible
            "dev\u{200D}1",  // ZERO WIDTH JOINER
            "\u{FEFF}dev-1", // BOM / zero-width no-break space
            "dev-1\u{202E}", // RIGHT-TO-LEFT OVERRIDE — bidi
            "dev-1\u{202A}", // LEFT-TO-RIGHT EMBEDDING
            "dev-1\u{2060}", // WORD JOINER
            "dev-1\u{2066}", // LEFT-TO-RIGHT ISOLATE
            "dev-1\u{2069}", // POP DIRECTIONAL ISOLATE
            "dev-1\u{200F}", // RIGHT-TO-LEFT MARK
            "dev-1\t2",      // interior tab — control + whitespace
            "dev-1 ",        // trailing space
        ] {
            let e = render(WF, &inputs(&[("topic", bad)])).unwrap_err();
            assert_eq!(e.code(), Some("one_line"), "{bad:?} -> {e}");
        }
        // Ordinary interior spaces and punctuation stay legal.
        render(WF, &inputs(&[("topic", "a real title — with spaces")])).unwrap();
        render(WF, &inputs(&[("topic", "double  space is fine")])).unwrap();
    }

    #[test]
    fn input_values_refuse_invisible_chars_by_category() {
        // CAD-507: the guard is by Unicode general category — every `Cf`
        // format control and every `Default_Ignorable_Code_Point`, so
        // codepoints the old list never named refuse: invisible `Mn`
        // marks, the blank-rendering `Lo` Hangul fillers, and the `Cn`
        // ignorable reserves. Mutation proof: a mutant keeping only the
        // named ranges (or only `Cf`+`Mn`) lets these through and the
        // `one_line` code is gone.
        for bad in [
            "dev\u{180E}1",  // MONGOLIAN VOWEL SEPARATOR — Cf
            "dev\u{00AD}1",  // SOFT HYPHEN — Cf
            "dev\u{061C}1",  // ARABIC LETTER MARK — Cf
            "dev\u{070F}1",  // SYRIAC ABBREVIATION MARK — Cf
            "dev\u{FFF9}1",  // INTERLINEAR ANNOTATION ANCHOR — Cf
            "dev\u{FFFA}1",  // INTERLINEAR ANNOTATION SEPARATOR — Cf
            "dev\u{FFFB}1",  // INTERLINEAR ANNOTATION TERMINATOR — Cf
            "dev\u{E0020}1", // TAG SPACE — Cf
            "dev\u{E0041}1", // TAG LATIN CAPITAL LETTER A — Cf
            "dev\u{E007F}1", // CANCEL TAG — Cf
            "dev\u{034F}1",  // COMBINING GRAPHEME JOINER — invisible Mn
            "dev\u{17B4}1",  // KHMER VOWEL INHERENT AQ — invisible Mn
            "dev\u{17B5}1",  // KHMER VOWEL INHERENT AA — invisible Mn
            "dev\u{180B}1",  // MONGOLIAN FREE VARIATION SELECTOR — Mn
            "dev\u{FE0F}1",  // VARIATION SELECTOR-16 — invisible Mn
            "dev\u{E0100}1", // VARIATION SELECTOR-17 — invisible Mn
            "dev\u{115F}1",  // HANGUL CHOSEONG FILLER — blank-rendering Lo
            "dev\u{1160}1",  // HANGUL JUNGSEONG FILLER — blank-rendering Lo
            "dev\u{3164}1",  // HANGUL FILLER — blank-rendering Lo
            "dev\u{FFA0}1",  // HALFWIDTH HANGUL FILLER — blank-rendering Lo
            "dev\u{FFF0}1",  // unassigned — default-ignorable Cn reserve
        ] {
            let e = render(WF, &inputs(&[("topic", bad)])).unwrap_err();
            assert_eq!(e.code(), Some("one_line"), "{bad:?} -> {e}");
        }
        // Every provided input is guarded — the optional `keyword`, a
        // prose `{{note}}` and an agent-position `{{who}}` alike.
        for bad in ["k\u{00AD}w", "k\u{034F}w", "k\u{FE0F}w"] {
            let e = render(WF, &inputs(&[("topic", "t"), ("keyword", bad)])).unwrap_err();
            assert_eq!(e.code(), Some("one_line"), "keyword {bad:?} -> {e}");
            for key in ["who", "note"] {
                let mut vals = inputs(&[("who", "dev-1"), ("note", "n")]);
                vals.insert(key.to_string(), bad.to_string());
                let e = render(WF_INJ, &vals).unwrap_err();
                assert_eq!(e.code(), Some("one_line"), "{key} {bad:?} -> {e}");
            }
        }
        // Visible combining marks stay legal — a mutant refusing all
        // `Mn` breaks an accented name.
        render(WF, &inputs(&[("topic", "Cafe\u{301} — naïve")])).unwrap();
    }

    /// CAD-487 r3, guard 2 for `distinct:`: the compare runs on what
    /// the plan carries — exercised through `check_rendered` directly,
    /// bypassing the value charset (as if a mutant removed it).
    #[test]
    fn distinct_compares_what_the_plan_parsed() {
        let tpl = parse_template(WF_DISTINCT).unwrap();
        // worker's raw value carries a trailing space — `agent:` trims
        // it in parse, so the rendered positions are BOTH `dev-1`.
        let rendered = render_values(
            WF_DISTINCT,
            &inputs(&[
                ("title", "t"),
                ("worker", "dev-1 "), // note the trailing space
                ("reviewer", "dev-1"),
            ]),
        )
        .unwrap();
        let e = check_rendered(WF_DISTINCT, &tpl, &rendered).unwrap_err();
        assert_eq!(e.code(), Some("not_distinct"), "{e}");
        assert!(
            e.to_string().contains("worker") && e.to_string().contains("render"),
            "{e}"
        );

        // `render` itself refuses the same input earlier, as one_line —
        // the two layers are independent.
        let e = render(
            WF_DISTINCT,
            &inputs(&[("title", "t"), ("worker", "dev-1 "), ("reviewer", "dev-1")]),
        )
        .unwrap_err();
        assert_eq!(e.code(), Some("one_line"), "{e}");
    }

    /// An input in a meta position (`{{who}}` on `agent:`) and one in
    /// the intro (`{{note}}`) — the two positions structure can be
    /// injected through.
    const WF_INJ: &str = "---\ntitle: T\ngoal: G\ninputs:\n  who: {}\n  note: {}\n---\n\n\
Intro {{note}}\n\n## Do\nagent: {{who}}\nsize: S\n\n### Acceptance\n- [ ] x\n\n\
## Check\nagent: qa-1\ndepends_on: 1\n\n### Acceptance\n- [ ] y\n";

    #[test]
    fn rendered_skeleton_must_match_the_template() {
        // Guard 2, exercised directly through `check_rendered`: an
        // injection that slipped past the value check (a mutant
        // dropping it) still cannot reach `propose` — the rendered
        // file's skeleton must equal the template's canonical one.
        let tpl = parse_template(WF_INJ).unwrap();
        let diverged = |key: &str, v: &str| {
            let mut vals = inputs(&[("who", "dev-1"), ("note", "n")]);
            vals.insert(key.to_string(), v.to_string());
            let rendered = render_values(WF_INJ, &vals).unwrap();
            check_rendered(WF_INJ, &tpl, &rendered).unwrap_err()
        };
        // A rogue ticket via the intro.
        let e = diverged(
            "note",
            "x\n\n## Rogue\nagent: qa-1\n\n### Acceptance\n- [ ] y\n",
        );
        assert_eq!(e.code(), Some("render_diverged"));
        assert!(e.to_string().contains("tickets"), "{e}");
        // A rogue depends_on edge on ticket 1 via a meta position.
        let e = diverged("who", "dev-1\ndepends_on: 2");
        assert_eq!(e.code(), Some("render_diverged"));
        assert!(e.to_string().contains("ticket 1"), "{e}");
        // An unknown metadata line: the plan parser stops reading
        // metadata at it, so `size:` lands in the body and the
        // skeleton shrinks.
        let e = diverged("who", "dev-1\nzz: 1");
        assert_eq!(e.code(), Some("render_diverged"));
        assert!(e.to_string().contains("ticket 1"), "{e}");
        // A duplicated known key changes the key count — caught too.
        let e = diverged("who", "dev-1\nsize: L");
        assert_eq!(e.code(), Some("render_diverged"), "{e}");

        // `render` refuses an input that leaves a required position
        // empty — the render is not a plan. This is the parity guard's
        // reachability witness: with the `check_rendered` call removed
        // (a mutant) `render` returns the broken text and this fails.
        let open_agent = WF_INJ.replace("  who: {}", "  who: { optional: true }");
        let e = render(&open_agent, &inputs(&[("note", "n")])).unwrap_err();
        assert_eq!(e.code(), Some("render_diverged"), "{e}");
    }

    const WF_DISTINCT: &str = "---\ntitle: \"T {{title}}\"\ngoal: \"G\"\n\
inputs:\n  title: {}\n  worker: {}\n  reviewer: {}\ndistinct: [worker, reviewer]\n---\n\n\
## Do\nagent: {{worker}}\nsize: S\n\n### Acceptance\n- [ ] x\n\n\
## Review\nagent: {{reviewer}}\nsize: S\ndepends_on: 1\n\n### Acceptance\n- [ ] y\n";

    #[test]
    fn distinct_refuses_equal_values() {
        let e = render(
            WF_DISTINCT,
            &inputs(&[("title", "t"), ("worker", "dev-1"), ("reviewer", "dev-1")]),
        )
        .unwrap_err();
        assert_eq!(e.code(), Some("not_distinct"), "{e}");
        assert!(
            e.to_string().contains("worker") && e.to_string().contains("reviewer"),
            "{e}"
        );
        // Differing values render.
        render(
            WF_DISTINCT,
            &inputs(&[("title", "t"), ("worker", "dev-1"), ("reviewer", "qa-1")]),
        )
        .unwrap();
    }

    #[test]
    fn distinct_validates_against_declared_inputs() {
        // Names an undeclared input.
        let bad = WF_DISTINCT.replace("distinct: [worker, reviewer]", "distinct: [worker, ghost]");
        let e = parse_template(&bad).unwrap_err().to_string();
        assert!(
            e.contains("ghost") && e.contains("not a declared input"),
            "{e}"
        );
        // Fewer than two names is meaningless.
        let bad = WF_DISTINCT.replace("distinct: [worker, reviewer]", "distinct: [worker]");
        assert!(parse_template(&bad).is_err());
        // Not a list at all.
        let bad = WF_DISTINCT.replace("distinct: [worker, reviewer]", "distinct: worker");
        assert!(parse_template(&bad).is_err());
        // Removing `distinct:` invalidates approval — it is gate-keyed.
        let without = WF_DISTINCT.replace("distinct: [worker, reviewer]\n", "");
        assert_ne!(
            gate_digest(&without).unwrap(),
            gate_digest(WF_DISTINCT).unwrap()
        );
    }

    /// CAD-507: `{{ worker }}` (spaced) names `worker` — `placeholders`
    /// and `substitute` already trim the inner name; `bare_placeholder`
    /// must too, or the parsed-position `distinct:` check cannot see the
    /// input's positions. Mutation proof: without the trim the spaced
    /// pair is invisible to both the render and the `check` paths.
    #[test]
    fn bare_placeholder_trims_spaced_names() {
        assert_eq!(bare_placeholder("{{ worker }}"), Some("worker"));
        assert_eq!(bare_placeholder("{{w}}"), Some("w"));
        // Whitespace alone is no name; literal text is never bare.
        assert_eq!(bare_placeholder("{{ }}"), None);
        assert_eq!(bare_placeholder("x-{{w}}"), None);

        let spaced = WF_DISTINCT
            .replace("agent: {{worker}}", "agent: {{ worker }}")
            .replace("agent: {{reviewer}}", "agent: {{ reviewer }}");
        // Full path: equal values refuse on `distinct:` — the spaced
        // placeholders rendered fine.
        let e = render(
            &spaced,
            &inputs(&[("title", "t"), ("worker", "dev-1"), ("reviewer", "dev-1")]),
        )
        .unwrap_err();
        assert_eq!(e.code(), Some("not_distinct"), "{e}");
        // The parsed-position layer: the spaced names record `worker`'s
        // and `reviewer`'s positions, so raw values that differ only by
        // what the plan parses away refuse as equal in the render.
        let tpl = parse_template(&spaced).unwrap();
        let rendered = render_values(
            &spaced,
            &inputs(&[
                ("title", "t"),
                ("worker", "dev-1 "), // the plan parses `dev-1`
                ("reviewer", "dev-1"),
            ]),
        )
        .unwrap();
        let e = check_rendered(&spaced, &tpl, &rendered).unwrap_err();
        assert_eq!(e.code(), Some("not_distinct"), "{e}");

        // `check` sees the spaced pair as bare placeholders: pinned by
        // `distinct:` they prove reviewer≠agent instead of refusing as
        // unprovable.
        let sources = vec!["test".to_string()];
        const WF_S: &str = "---\ntitle: T\ngoal: G\ninputs:\n  w: {}\n  r: {}\n\
distinct: [w, r]\n---\n\n## Do\nagent: {{ w }}\nsize: S\nreviewer: {{ r }}\n\n\
### Acceptance\n- [ ] x\n";
        let (errors, notes, _) = check_text(WF_S, &HashSet::new(), &sources);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(notes.iter().any(|n| n.contains("distinct")), "{notes:?}");
        // The same input on both sides refuses by name — spaced.
        let same = WF_S.replace("reviewer: {{ r }}", "reviewer: {{ w }}");
        let (errors, _, _) = check_text(&same, &HashSet::new(), &sources);
        assert!(
            errors.iter().any(|e| e.contains("same input")),
            "{errors:?}"
        );
    }

    #[test]
    fn check_text_templated_agent_reviewer() {
        // Templated agent/reviewer: independence holds only when both
        // are bare placeholders pinned in `distinct:` — else refused.
        const WF_REV: &str = "---\ntitle: T\ngoal: G\ninputs:\n  w: {}\n  r: {}\n---\n\n\
## Do\nagent: {{w}}\nsize: S\nreviewer: {{r}}\n\n### Acceptance\n- [ ] x\n";
        let agents = HashSet::new();
        let sources = vec!["test".to_string()];
        // No distinct: — cannot prove reviewer≠agent → refusal.
        let (errors, _, _) = check_text(WF_REV, &agents, &sources);
        assert!(errors.iter().any(|e| e.contains("distinct")), "{errors:?}");
        // With distinct: — enforced at render, noted.
        let pinned = WF_REV.replace("  r: {}", "  r: {}\ndistinct: [w, r]");
        let (errors, notes, _) = check_text(&pinned, &agents, &sources);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(notes.iter().any(|n| n.contains("distinct")), "{notes:?}");
        // Same input on both sides — always equal.
        let same = pinned.replace("reviewer: {{r}}", "reviewer: {{w}}");
        let (errors, _, _) = check_text(&same, &agents, &sources);
        assert!(
            errors.iter().any(|e| e.contains("same input")),
            "{errors:?}"
        );
        // Static reviewer + templated agent — unprovable.
        let half = pinned.replace("reviewer: {{r}}", "reviewer: qa-1");
        let (errors, _, _) = check_text(&half, &agents, &sources);
        assert!(errors.iter().any(|e| e.contains("cannot")), "{errors:?}");
    }

    #[test]
    fn check_text_meta_order_parity() {
        let agents: HashSet<String> = ["dev-1", "dev-2", "qa-1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let sources = vec!["test".to_string()];
        // `reviewer:` before `agent:` — the plan parser stops at the
        // unknown key, so `agent:` lands in the ticket body: refused,
        // and the refusal names the key.
        let bad = WF.replace(
            "agent: dev-1\nsize: S",
            "reviewer: qa-1\nagent: dev-1\nsize: S",
        );
        let (errors, _, _) = check_text(&bad, &agents, &sources);
        assert!(
            errors.iter().any(|e| e.contains("`agent` sits after")),
            "{errors:?}"
        );
        // Positive control: a correctly ordered `reviewer:` passes with
        // no parity error — kills mutants flipping the comparison.
        let ok = WF.replace(
            "agent: dev-1\nsize: S",
            "agent: dev-1\nsize: S\nreviewer: qa-1",
        );
        let (errors, _, _) = check_text(&ok, &agents, &sources);
        assert!(errors.is_empty(), "{errors:?}");
    }
}
