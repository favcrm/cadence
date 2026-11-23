//! Frontmatter, placeholder, and ticket metadata parsing for `issue::workflow`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::BTreeMap;

use crate::error::{Error, Result};
use crate::issue::{model, parse};

use super::{
    bounded_content, shape_problem, split_sections, valid_input_name, Execution, InputKind,
    InputSpec, META_KEYS, STATIC_META_KEYS, TICKET_META_KEYS,
};

/// The frontmatter of a template, split into what the workflow knows:
/// its declared inputs, the must-differ group and the human label. The
/// plan's own metadata (title, goal, non_goals) stays in the file.
pub(super) struct Front {
    pub(super) inputs: BTreeMap<String, InputSpec>,
    pub(super) input_order: Vec<String>,
    pub(super) distinct: Vec<String>,
    pub(super) label: Option<String>,
    pub(super) publication_slot: Option<String>,
    pub(super) capability_slots: Vec<String>,
    pub(super) required_asset_slot: Option<String>,
    pub(super) execution: Execution,
}

pub(super) fn parse_front(yaml: &str) -> Result<Front> {
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
    let publication_slot = match map.remove(serde_yaml::Value::String("publication_slot".into())) {
        None => None,
        Some(serde_yaml::Value::String(slot)) if crate::issue::model::valid_tag(&slot) => {
            Some(slot)
        }
        Some(_) => {
            return Err(Error::rejected(
                "workflow publication_slot must be one static slot name",
            ))
        }
    };
    let capability_slots = match map.remove(serde_yaml::Value::String("capability_slots".into())) {
        None => Vec::new(),
        Some(serde_yaml::Value::Sequence(slots)) if !slots.is_empty() && slots.len() <= 8 => {
            let mut names = Vec::new();
            for slot in slots {
                let name = slot
                    .as_str()
                    .filter(|s| model::valid_tag(s))
                    .ok_or_else(|| {
                        Error::rejected("workflow capability_slots must contain static slot names")
                    })?;
                if names.iter().any(|existing| existing == name)
                    || publication_slot.as_deref() == Some(name)
                {
                    return Err(Error::rejected("workflow capability_slots must be distinct from each other and publication_slot"));
                }
                names.push(name.to_string());
            }
            names
        }
        Some(_) => return Err(Error::rejected(
            "workflow capability_slots must be a nonempty list of at most eight static slot names",
        )),
    };
    let required_asset_slot =
        match map.remove(serde_yaml::Value::String("required_asset_slot".into())) {
            None => None,
            Some(serde_yaml::Value::String(slot))
                if capability_slots.iter().any(|name| name == &slot) =>
            {
                Some(slot)
            }
            Some(_) => {
                return Err(Error::rejected(
                    "workflow required_asset_slot must name a declared capability slot",
                ))
            }
        };
    // CAD-1171: `execution: host` runs the declared capability slots
    // in-process for the operator's own click — no worker, no owner PM.
    // It needs at least one capability slot and, because a host step
    // cannot produce text, every rendered step must omit `agent:` (the
    // run gate refuses an assigned host step).
    let execution = match map.remove(serde_yaml::Value::String("execution".into())) {
        None => Execution::Agent,
        Some(serde_yaml::Value::String(value)) if value == "host" => Execution::Host,
        Some(serde_yaml::Value::String(value)) if value == "agent" => Execution::Agent,
        Some(_) => {
            return Err(Error::rejected(
                "workflow execution must be `host` or `agent`",
            ))
        }
    };
    if execution == Execution::Host && capability_slots.is_empty() {
        return Err(Error::rejected(
            "a host-execution workflow must declare at least one capability slot",
        ));
    }
    let inputs_val = map.remove(serde_yaml::Value::String("inputs".to_string()));
    let mut inputs = BTreeMap::new();
    let mut input_order: Vec<String> = Vec::new();
    if let Some(inputs_val) = inputs_val {
        let serde_yaml::Value::Mapping(specs) = inputs_val else {
            return Err(Error::rejected(
                "workflow `inputs:` must be a map of name → { ask, optional, kind, example, context_default }",
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
                    ..InputSpec::default()
                },
                serde_yaml::Value::Mapping(m) => {
                    let mut spec = InputSpec::default();
                    for (sk, sv) in m {
                        match (sk.as_str(), sv) {
                            (Some("ask"), serde_yaml::Value::String(s)) => spec.ask = Some(s),
                            (Some("optional"), serde_yaml::Value::Bool(b)) => spec.optional = b,
                            (Some("context_default"), serde_yaml::Value::Bool(b)) => {
                                spec.context_default = b
                            }
                            (Some("context_default"), _) => {
                                return Err(Error::rejected("context_default must be a boolean"))
                            }
                            (Some("default"), serde_yaml::Value::String(s)) => {
                                spec.default = Some(s)
                            }
                            (Some("default"), _) => {
                                return Err(Error::rejected(
                                    "input default must be one line of text",
                                ))
                            }
                            (Some("kind"), serde_yaml::Value::String(s)) => {
                                let Some(kind) = InputKind::parse(s.trim()) else {
                                    return Err(Error::rejected(format!(
                                        "input '{name}': unknown kind '{}' — v0 knows slug",
                                        s.trim()
                                    )));
                                };
                                spec.kind = Some(kind);
                            }
                            (Some("kind"), _) => {
                                return Err(Error::rejected(format!(
                                    "input '{name}': `kind:` is a shape name — v0 knows slug"
                                )))
                            }
                            (Some("example"), serde_yaml::Value::String(s)) => {
                                let s = s.trim();
                                if s.chars().count() > 80 || s.chars().any(char::is_control) {
                                    return Err(Error::rejected(format!(
                                        "input '{name}': `example:` — ≤80 chars, one line, \
                                         no control characters"
                                    )));
                                }
                                if !s.is_empty() {
                                    spec.example = Some(s.to_string());
                                }
                            }
                            (Some("example"), _) => {
                                return Err(Error::rejected(format!(
                                    "input '{name}': `example:` is one line of text — the \
                                     value the run form shows in the empty field"
                                )))
                            }
                            (Some(k), _) => {
                                return Err(Error::rejected(format!(
                                    "input '{name}': unknown key '{k}' — ask, optional, \
                                     kind, example, context_default"
                                )))
                            }
                            (None, _) => {
                                return Err(Error::rejected(format!(
                                    "input '{name}': keys must be strings — ask, optional, \
                                     kind, example, context_default"
                                )))
                            }
                        }
                    }
                    spec
                }
                _ => {
                    return Err(Error::rejected(format!(
                        "input '{name}' must be a string (the ask) or a map \
                         {{ ask, optional, kind, example, context_default }}"
                    )))
                }
            };
            // An example the declared shape refuses would suggest a value
            // the engine then refuses — checked together, after the map.
            if let (Some(kind), Some(example)) = (spec.kind, spec.example.as_deref()) {
                if shape_problem(kind, example).is_some() {
                    return Err(Error::rejected(format!(
                        "input '{name}': `example: '{example}'` does not fit `kind: {}` — \
                         an example must be a value the shape accepts",
                        kind.as_str()
                    )));
                }
            }
            if let Some(default) = spec.default.as_deref() {
                if !spec.optional
                    || !spec.context_default
                    || default.is_empty()
                    || !bounded_content(default)
                    || spec
                        .kind
                        .is_some_and(|kind| shape_problem(kind, default).is_some())
                {
                    return Err(Error::rejected(format!(
                        "input '{name}': default must be optional, content-safe and bounded"
                    )));
                }
            }
            inputs.insert(name.to_string(), spec);
            input_order.push(name.to_string());
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
        input_order,
        distinct,
        label,
        publication_slot,
        capability_slots,
        required_asset_slot,
        execution,
    })
}

/// Scan for `{{` … `}}` placeholders: each inner name must be a
/// declared input. Any other `{{` is a template error — there is no
/// literal `{{`.
pub(super) fn placeholders(text: &str, inputs: &BTreeMap<String, InputSpec>) -> Result<()> {
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

/// CAD-1867: the per-ticket `skill:` tags of workflow text, in ticket order —
/// `None` for a ticket that declares no skill package.
///
/// The tag is static (a `{{input}}` refuses in [`ticket_meta`]), and the line
/// must be the LAST metadata line of its ticket: the plan parser stops reading
/// metadata at the first key it does not know, so anything written after
/// `skill:` would land in the step body and never reach the run.
pub fn skill_tags(text: &str) -> Result<Vec<Option<String>>> {
    let (_, body) = parse::split_front(text)?;
    let metas = ticket_meta(body)?;
    let mut out = Vec::with_capacity(metas.len());
    for (i, meta) in metas.iter().enumerate() {
        let mut tag: Option<String> = None;
        for (position, (key, value)) in meta.iter().enumerate() {
            if key != "skill" {
                continue;
            }
            if position + 1 != meta.len() {
                return Err(Error::rejected(format!(
                    "Ticket {}: a `skill:` line must be the last metadata line — the plan \
                     parser stops reading metadata at the first key it does not know, so \
                     anything after `skill:` would land in the step body",
                    i + 1
                )));
            }
            if value.is_empty() {
                return Err(Error::rejected(format!(
                    "Ticket {}: `skill:` needs a package name — `skill: <tag>`",
                    i + 1
                )));
            }
            if !model::valid_tag(value) {
                return Err(Error::rejected(format!(
                    "Ticket {}: `skill: {value}` — a skill package name is [a-z0-9-], \
                     ≤32 bytes",
                    i + 1
                )));
            }
            tag = Some(value.clone());
        }
        out.push(tag);
    }
    Ok(out)
}

/// Refuse a `skill:` directive in text that is not an installed bundle's own
/// workflow. `workflow check`, a stored project workflow's `plan propose` and
/// an installed app's `<app>/<workflow>` proposal all read workflow text
/// without freezing any package, so a `skill:` there would be silently
/// dropped — this refuses it instead.
pub fn refuse_skill_directive(text: &str) -> Result<()> {
    let tags = skill_tags(text)?;
    if let Some(tag) = tags.into_iter().flatten().next() {
        return Err(Error::rejected(format!(
            "`skill: {tag}` names installed-app skill material — only an installed app run \
             freezes a skill package into its snapshot; this workflow is not an installed \
             app's workflow, so the directive cannot be honored"
        )));
    }
    Ok(())
}
