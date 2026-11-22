//! Template rendering and post-render validation for `issue::workflow`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::error::{Error, Result};
use crate::issue::{parse, plan};

use super::{
    bare_placeholder, check_shape, parse_template, shape_problem, ticket_meta, InputSpec, Template,
    TicketMetas, WORKFLOW_ONLY_KEYS,
};

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

/// Validate only explicitly declared content defaults without supplying team
/// inputs or rendering an execution plan. Errors deliberately omit values.
pub fn check_context_defaults(text: &str, defaults: &BTreeMap<String, String>) -> Result<()> {
    let template = parse_template(text)?;
    for (name, value) in defaults {
        let spec = template
            .inputs
            .get(name)
            .filter(|spec| spec.context_default)
            .ok_or_else(|| Error::rejected("context input is not declared safe content"))?;
        if value.trim() != value
            || (!spec.optional && value.is_empty())
            || value.chars().any(bad_value_char)
            || (spec.default.is_some() && !bounded_content(value))
            || spec
                .kind
                .is_some_and(|kind| shape_problem(kind, value).is_some())
        {
            return Err(Error::rejected(
                "context input does not satisfy its declared content shape",
            ));
        }
    }
    Ok(())
}

/// Render `text` with `values`: every `{{name}}` becomes its value
/// (absent inputs render empty). `inputs:` drops out of the
/// frontmatter — the result is a plan file for [`plan::parse_plan`].
/// Substitution inside the frontmatter runs over YAML scalars, so a
/// value carrying quotes or newlines cannot corrupt the meta; the body
/// substitutes verbatim.
pub(super) fn render_values(text: &str, values: &BTreeMap<String, String>) -> Result<String> {
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
/// approved skeleton — an input that declares a shape (`kind:`) must
/// match it (`bad_shape`), and `distinct:` inputs must differ. After
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
        if tpl.inputs[k].default.is_some() && !v.is_empty() && !bounded_content(v) {
            return Err(Error::rejected(format!(
                "input '{k}' exceeds its content guidance bound"
            )));
        }
    }
    // A declared shape (`kind:`) is enforced on the value itself, for
    // every caller — the daemon, the CLI, the board's preview. Iterated
    // over the declared inputs (sorted) so the refusal order is stable.
    for (k, spec) in &tpl.inputs {
        if let (Some(kind), Some(v)) = (spec.kind, provided.get(k)) {
            check_shape(kind, k, v)?;
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
        .map(|k| {
            let value = provided
                .get(k)
                .filter(|value| !value.is_empty())
                .cloned()
                .or_else(|| tpl.inputs[k].default.clone())
                .unwrap_or_default();
            (k.clone(), value)
        })
        .collect();
    let rendered = render_values(text, &values)?;
    check_rendered(text, &tpl, &rendered)?;
    Ok(rendered)
}

/// A character an input value may never carry, by Unicode general
/// category rather than a named code-point list (CAD-507 review): every
/// `Cf` format control — the soft hyphen, the bidi and
/// interlinear-annotation controls, tag characters, the BOM — plus the
/// whole `Default_Ignorable_Code_Point` class: the invisible `Mn`
/// nonspacing marks (grapheme joiner, Khmer vowel inherents, Mongolian
/// and emoji variation selectors), the `Lo` Hangul fillers U+115F,
/// U+1160, U+3164 and U+FFA0 — real letters that render as blank and
/// alias past `distinct:` — and the unassigned `Cn` reserves. A format
/// control, filler or variation selector Unicode assigns later refuses
/// without a table update. One consequence to know: an emoji spelled
/// with VS16 (U+FE0F), e.g. a pasted `❤️`, refuses — the selector is
/// default-ignorable — while the bare emoji passes. Control characters
/// and whitespace that is not an ordinary interior space keep refusing
/// through the other arms — each is invisible or splitting in some
/// reader, and none changes what a value names.
fn bad_value_char(c: char) -> bool {
    /// `\p{Cf}` ∪ `\p{Default_Ignorable_Code_Point}` — `Cf` stays named
    /// because Unicode holds a few format controls outside the class.
    static INVISIBLE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"[\p{Cf}\p{Default_Ignorable_Code_Point}]")
            .expect("a constant Unicode-category pattern compiles")
    });
    c.is_control()
        || INVISIBLE.is_match(c.encode_utf8(&mut [0; 4]))
        || (c.is_whitespace() && c != ' ')
}

/// Short, visible, one-line guidance; never enough room to smuggle a
/// second task or crowd out the fixed source and review instructions.
pub(crate) fn bounded_content(value: &str) -> bool {
    value.trim() == value
        && value.chars().count() <= 512
        && value.len() <= 2048
        && !value.chars().any(bad_value_char)
}

/// Validate operator-pasted source facts at the provider boundary without
/// rewriting their spacing or otherwise changing the frozen run input.
pub(crate) fn visible_source_facts(value: &str) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.len() <= 16 * 1024
        && !value.chars().any(bad_value_char)
}

/// A provider caption remains byte-exact in its receipt. Only this derived
/// display line enters a rendered workflow, whose input grammar is deliberately
/// single-line to prevent provider data from becoming plan structure.
pub fn source_input_line(caption: &str) -> Result<String> {
    let clean: String = caption
        .chars()
        .map(|c| if bad_value_char(c) { ' ' } else { c })
        .collect();
    let line = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() || line.len() > 16 * 1024 {
        return Err(Error::rejected(
            "selected source caption is empty or too large after normalization",
        ));
    }
    Ok(line)
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
pub(super) fn check_rendered(text: &str, tpl: &Template, rendered: &str) -> Result<()> {
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
pub(super) fn canonical(text: &str, inputs: &BTreeMap<String, InputSpec>) -> Result<String> {
    let values: BTreeMap<String, String> = inputs.keys().map(|k| (k.clone(), k.clone())).collect();
    render_values(text, &values)
}

/// The plan file's body split at `##` headings — the same split
/// [`plan::parse_plan`] does: `(intro, sections)`.
pub(super) fn split_sections(body: &str) -> (String, Vec<(String, String)>) {
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
pub(super) fn normalize_deps(value: &str) -> String {
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
