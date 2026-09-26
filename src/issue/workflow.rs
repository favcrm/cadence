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
/// plus `inputs` and `distinct` (inputs whose values must differ at
/// render — `distinct: [worker, reviewer]` keeps a reviewer from being
/// the worker) and `label` (the human name the board's primary action
/// uses — "New post"; wording, like the title). Anything else refuses
/// at parse — the same fail-loud rule the plan parser applies with
/// `deny_unknown_fields`.
const META_KEYS: &[&str] = &["title", "goal", "non_goals", "inputs", "distinct", "label"];

/// Workflow-only frontmatter keys: pulled out at parse and removed
/// before rendering, because the plan parser denies unknown fields.
const WORKFLOW_ONLY_KEYS: &[&str] = &["inputs", "distinct", "label"];

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

/// A parsed workflow template: the declared inputs, the `distinct:`
/// group — inputs whose rendered values must pairwise differ — and the
/// optional human `label:`. The plan structure itself is checked by
/// rendering and running [`plan::parse_plan`].
#[derive(Clone, Debug)]
pub struct Template {
    pub inputs: BTreeMap<String, InputSpec>,
    pub distinct: Vec<String>,
    pub label: Option<String>,
}

/// `{{name}}` — the input name is a bare word, like an alias but with
/// an optional leading `_`.
fn valid_input_name(name: &str) -> bool {
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
/// makes `distinct:` meaningful; `x-{{a}}` mixes literal text in.
fn bare_placeholder(v: &str) -> Option<&str> {
    v.strip_prefix("{{")?
        .strip_suffix("}}")
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

/// The frontmatter of a template, split into what the workflow knows:
/// its declared inputs, the must-differ group and the human label. The
/// plan's own metadata (title, goal, non_goals) stays in the file.
struct Front {
    inputs: BTreeMap<String, InputSpec>,
    distinct: Vec<String>,
    label: Option<String>,
}

fn parse_front(yaml: &str) -> Result<Front> {
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
    // `label:` is the human name the board's action uses — one line.
    let label = match map.remove(serde_yaml::Value::String("label".to_string())) {
        None | Some(serde_yaml::Value::Null) => None,
        Some(serde_yaml::Value::String(s)) => {
            let s = s.trim();
            if s.is_empty() {
                None
            } else if s.chars().count() > 60 || s.chars().any(char::is_control) {
                return Err(Error::rejected(
                    "workflow `label:` — ≤60 chars, no control characters",
                ));
            } else {
                Some(s.to_string())
            }
        }
        Some(_) => {
            return Err(Error::rejected(
                "workflow `label:` is one line of text — what the run is called, \
                 like \"New post\"",
            ))
        }
    };
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
    // `distinct: [a, b]` — inputs whose rendered values must pairwise
    // differ (worker vs reviewer). Names must be declared inputs.
    let mut distinct = Vec::new();
    if let Some(v) = map.remove(serde_yaml::Value::String("distinct".to_string())) {
        let serde_yaml::Value::Sequence(list) = v else {
            return Err(Error::rejected(
                "workflow `distinct:` must be a list of input names — `distinct: [a, b]`",
            ));
        };
        for item in list {
            let Some(name) = item.as_str() else {
                return Err(Error::rejected(
                    "workflow `distinct:` entries are input names — `distinct: [a, b]`",
                ));
            };
            if !inputs.contains_key(name) {
                return Err(Error::rejected(format!(
                    "distinct: '{name}' is not a declared input — declare it under `inputs:`"
                )));
            }
            distinct.push(name.to_string());
        }
        if distinct.len() < 2 {
            return Err(Error::rejected(
                "workflow `distinct:` needs at least two input names — it pins them apart",
            ));
        }
    }
    Ok(Front {
        inputs,
        distinct,
        label,
    })
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

/// Per-ticket metadata lines as `(key, value)` pairs, in file order.
pub(crate) type TicketMetas = Vec<Vec<(String, String)>>;

/// One `##` section's leading metadata: the `key: value` lines in
/// [`TICKET_META_KEYS`] directly under the heading — the same rule
/// `parse_ticket` applies to its own keys, extended with the
/// workflow's extras. A `{{` in a `STATIC_META_KEYS` value refuses:
/// the dependency graph and sizes must be checkable statically.
pub(crate) fn ticket_meta(body: &str) -> Result<TicketMetas> {
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
    let front = parse_front(yaml)?;
    placeholders(text, &front.inputs)?;
    ticket_meta(body)?;
    Ok(Template {
        inputs: front.inputs,
        distinct: front.distinct,
        label: front.label,
    })
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
    // The workflow-only keys are meta for the workflow, not the plan:
    // the plan parser denies unknown fields, so none reaches the
    // rendered file.
    for key in WORKFLOW_ONLY_KEYS {
        meta.remove(serde_yaml::Value::String(key.to_string()));
    }
    let mut meta = serde_yaml::Value::Mapping(meta);
    subst_yaml(&mut meta, values);
    let yaml = serde_yaml::to_string(&meta)
        .map_err(|e| Error::internal(format!("workflow frontmatter: {e}")))?;
    Ok(format!("---\n{yaml}---\n{}", substitute(body, values)))
}

/// Render the template with `provided` (`k=v` pairs): unknown names
/// and missing required inputs refuse with a named reason; absent
/// optionals render as empty. A value must be a single line — a
/// newline or control character would inject plan structure under an
/// approved skeleton — and `distinct:` inputs must differ. After
/// substitution the rendered plan's skeleton (ticket count, per-ticket
/// metadata keys, `depends_on` edges) must match the template's own:
/// defence in depth if the one-line rule is ever loosened.
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
    for (k, v) in provided {
        if v.trim() != v {
            return Err(Error::invalid(
                "one_line",
                format!(
                    "input '{k}' carries leading or trailing whitespace — the plan \
                     parser trims it away, so what was compared and what lands would \
                     differ; pass '{}' instead",
                    v.trim()
                ),
            ));
        }
        if let Some(c) = v.chars().find(|&c| bad_value_char(c)) {
            return Err(Error::invalid(
                "one_line",
                format!(
                    "input '{k}' must be a single line of visible characters — \
                     U+{:04X} is a control, separator, invisible or non-space \
                     whitespace character that can smuggle structure or alias \
                     equality past the checks",
                    c as u32
                ),
            ));
        }
    }
    // `distinct:` compares the trimmed values — the same normalization
    // the plan parser applies to meta values — and, for inputs filling
    // agent:/reviewer:-style positions, the rendered positions too
    // (`check_rendered`), so the check can't drift from the parser.
    for (i, a) in tpl.distinct.iter().enumerate() {
        for b in &tpl.distinct[i + 1..] {
            let (va, vb) = (
                provided.get(a).map(|s| s.trim()).unwrap_or(""),
                provided.get(b).map(|s| s.trim()).unwrap_or(""),
            );
            if va == vb {
                return Err(Error::invalid(
                    "not_distinct",
                    format!("inputs '{a}' and '{b}' must differ (`distinct:`) — both are '{va}'"),
                ));
            }
        }
    }
    let values: BTreeMap<String, String> = tpl
        .inputs
        .keys()
        .map(|k| (k.clone(), provided.get(k).cloned().unwrap_or_default()))
        .collect();
    let rendered = render_values(text, &values)?;
    check_rendered(text, &tpl, &rendered)?;
    Ok(rendered)
}

/// A character an input value may never carry: control characters,
/// Unicode line/paragraph separators, zero-width and word-joiner
/// format characters, bidi controls, the BOM, and any whitespace that
/// is not an ordinary interior space. Each is invisible or splitting
/// in some reader — none changes what a value names.
fn bad_value_char(c: char) -> bool {
    c.is_control()
        || ('\u{200B}'..='\u{200F}').contains(&c) // ZWSP, ZWNJ, ZWJ, LRM, RLM
        || ('\u{2028}'..='\u{202E}').contains(&c) // line/para separators, bidi
        || ('\u{2060}'..='\u{2069}').contains(&c) // word joiner, invisible ops, isolates
        || c == '\u{FEFF}'
        || (c.is_whitespace() && c != ' ')
}

/// Parse a rendered plan into its doc and per-ticket metadata lines —
/// the parse doubles as the render's own validity check (acceptance,
/// sizes, aliases).
fn parsed(text: &str) -> Result<(plan::PlanDoc, TicketMetas)> {
    let doc = plan::parse_plan(text)?;
    let body = parse::split_front(text).map(|(_, b)| b).unwrap_or("");
    let metas = ticket_meta(body)?;
    Ok((doc, metas))
}

/// A ticket's skeleton atoms: one per recognised metadata-key
/// occurrence plus one `dep:<token>` per normalised `depends_on` edge.
/// Values never appear: an input's whole job is to fill them.
fn atoms_of(meta: &[(String, String)]) -> Vec<String> {
    let mut atoms = Vec::new();
    for (key, value) in meta {
        if key == "depends_on" {
            for tok in normalize_deps(value).split(',') {
                if !tok.is_empty() {
                    atoms.push(format!("dep:{tok}"));
                }
            }
        } else {
            atoms.push(key.clone());
        }
    }
    atoms.sort();
    atoms
}

/// The second render guard (CAD-487 review): the rendered file's
/// skeleton must equal the template's canonical one — same ticket
/// count, same metadata keys per ticket, same `depends_on` edges. Any
/// difference names itself; the refusal is `render_diverged`.
fn check_rendered(text: &str, tpl: &Template, rendered: &str) -> Result<()> {
    let canon_text = canonical(text, &tpl.inputs)?;
    let (_, canon_meta) = parsed(&canon_text).map_err(|e| {
        Error::rejected(format!(
            "the workflow does not render to a valid plan even canonically — \
             run `cadence workflow check` on it: {e}"
        ))
    })?;
    let (rdoc, rmeta) = parsed(rendered).map_err(|e| {
        Error::invalid(
            "render_diverged",
            format!("the inputs render to an invalid plan — {e}"),
        )
    })?;
    let want: Vec<Vec<String>> = canon_meta.iter().map(|m| atoms_of(m)).collect();
    let got: Vec<Vec<String>> = rmeta.iter().map(|m| atoms_of(m)).collect();
    if want.len() != got.len() {
        return Err(Error::invalid(
            "render_diverged",
            format!(
                "the inputs render {} tickets where the template declares {}",
                got.len(),
                want.len()
            ),
        ));
    }
    for (i, (w, g)) in want.iter().zip(got.iter()).enumerate() {
        if w != g {
            return Err(Error::invalid(
                "render_diverged",
                format!(
                    "the inputs change ticket {}'s skeleton — template has [{}], \
                     the render has [{}]",
                    i + 1,
                    w.join(", "),
                    g.join(", ")
                ),
            ));
        }
    }
    // The second distinctness layer (CAD-487 r3): compare what each
    // pinned input rendered *at its positions* — `agent:` is the
    // parser's own value, other keys are the rendered meta line —
    // never the raw input strings. An input that normalises onto
    // another (a separator the parser trims, a stray space) still
    // refuses, because this compares what the plan will carry.
    if tpl.distinct.is_empty() {
        return Ok(());
    }
    let tbody = parse::split_front(text).map(|(_, b)| b).unwrap_or("");
    let tmeta = ticket_meta(tbody)?;
    let mut positions: BTreeMap<&str, Vec<(usize, &str)>> = BTreeMap::new();
    for (i, meta) in tmeta.iter().enumerate() {
        for (key, raw) in meta {
            if let Some(name) = bare_placeholder(raw) {
                if tpl.distinct.iter().any(|d| d == name) {
                    positions.entry(name).or_default().push((i, key.as_str()));
                }
            }
        }
    }
    let rendered_at = |i: usize, key: &str| -> Option<&str> {
        if key == "agent" {
            rdoc.tickets.get(i).and_then(|t| t.agent.as_deref())
        } else {
            rmeta
                .get(i)
                .and_then(|m| m.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()))
        }
    };
    for (i, a) in tpl.distinct.iter().enumerate() {
        for b in &tpl.distinct[i + 1..] {
            for (ta, ka) in positions.get(a.as_str()).into_iter().flatten() {
                for (tb, kb) in positions.get(b.as_str()).into_iter().flatten() {
                    if let (Some(va), Some(vb)) = (rendered_at(*ta, ka), rendered_at(*tb, kb)) {
                        if va == vb {
                            return Err(Error::invalid(
                                "not_distinct",
                                format!(
                                    "inputs '{a}' and '{b}' must differ (`distinct:`) — \
                                     both render '{va}' ({ka} on ticket {}, {kb} on ticket {})",
                                    ta + 1,
                                    tb + 1
                                ),
                            ));
                        }
                    }
                }
            }
        }
    }
    Ok(())
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
    if !tpl.distinct.is_empty() {
        // The distinctness contract is approval-affecting too — dropping
        // `distinct:` would silently re-admit worker == reviewer.
        let mut d = tpl.distinct.clone();
        d.sort();
        keys.push_str(&format!(";distinct={}", d.join(",")));
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
        assert_eq!(tpl.label, None, "no label is fine");
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
