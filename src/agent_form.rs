//! CAD-2285: the master's A2UI form - the closed pilot profile the daemon
//! enforces before any entry is written.
//!
//! The master sends one `{"version":"1.0","createSurface":{...}}` envelope
//! (A2UI v1.0, a2ui.org). It is checked strictly against a subset of the
//! basic catalog (`catalogs/basic/v1/catalog.json` in
//! github.com/a2ui-project/a2ui @ 24d1bf9c83): each pilot component's
//! fields are a table ([`spec`]), every field is checked by its type, and
//! the components form one tree under `root` within size limits. Anything
//! else refuses the whole form and names the path; nothing is written. The
//! board renders only this subset and shows the entry's text otherwise.
//!
//! The form carries no authority: its one action kind is an `event` the
//! board turns into the operator's own chat message. There are no URLs,
//! no renderer functions beyond checks, and no secret fields.

use serde_json::{Map, Value};

use crate::error::{Error, Result};

/// The catalog the envelope must name. The pilot is a subset of it.
pub const BASIC_CATALOG: &str = "https://a2ui.org/specification/v1_0/catalogs/basic/catalog.json";
/// The largest envelope the CLI and the RPC accept.
pub const MAX_BYTES: usize = 32 * 1024;
const MAX_COMPONENTS: usize = 40;
const MAX_DEPTH: usize = 4;
const MAX_OPTIONS: usize = 12;
const MAX_CHECKS: usize = 8;
const MAX_CONTEXT: usize = 16;
const MAX_TEXT: usize = 2000;
const MAX_LABEL: usize = 200;
const MAX_ID: usize = 64;
const MAX_POINTER: usize = 128;
const MAX_DATA_BYTES: usize = 8 * 1024;
const MAX_FALLBACK: usize = 4000;
const CHECK_FUNCTIONS: &[&str] = &["required", "length", "numeric", "email"];
const ALIGN: &[&str] = &["start", "center", "end", "stretch"];
const JUSTIFY: &[&str] = &[
    "start",
    "center",
    "end",
    "spaceBetween",
    "spaceAround",
    "spaceEvenly",
    "stretch",
];

/// What one field may hold.
#[derive(Clone, Copy)]
enum Ty {
    /// A literal string, or a `{"@path"}` binding, of at most this many chars.
    Dyn(usize),
    DynBool,
    DynList,
    Id,
    Ids,
    Bool,
    OneOf(&'static [&'static str]),
    Action,
    Checks,
    Options,
}

/// A field: its name, type, and whether it is required.
type Field = (&'static str, Ty, bool);

const CHECKS: Field = ("@checks", Ty::Checks, false);
const LABEL: Field = ("label", Ty::Dyn(MAX_LABEL), false);

/// The pilot catalog: each component's own fields. `None` is not in it.
fn spec(component: &str) -> Option<&'static [Field]> {
    use Ty::*;
    Some(match component {
        "Text" => &[
            ("text", Dyn(MAX_TEXT), true),
            ("variant", OneOf(&["caption", "body"]), false),
        ],
        "Row" | "Column" => &[
            ("children", Ids, true),
            ("align", OneOf(ALIGN), false),
            ("justify", OneOf(JUSTIFY), false),
        ],
        "Card" => &[("child", Id, true)],
        "Divider" => &[("axis", OneOf(&["horizontal"]), false)],
        "Button" => &[
            ("child", Id, true),
            (
                "variant",
                OneOf(&["default", "primary", "borderless"]),
                false,
            ),
            ("action", Action, true),
            CHECKS,
        ],
        "CheckBox" => &[
            ("label", Dyn(MAX_LABEL), true),
            ("value", DynBool, true),
            CHECKS,
        ],
        "ChoicePicker" => &[
            LABEL,
            ("options", Options, true),
            ("value", DynList, true),
            (
                "variant",
                OneOf(&["multipleSelection", "mutuallyExclusive"]),
                false,
            ),
            ("displayStyle", OneOf(&["chips", "checkbox"]), false),
            CHECKS,
        ],
        "TextField" => &[
            ("label", Dyn(MAX_LABEL), true),
            ("value", Dyn(MAX_TEXT), true),
            ("placeholder", Dyn(MAX_LABEL), false),
            (
                "variant",
                OneOf(&["shortText", "longText", "number"]),
                false,
            ),
            CHECKS,
        ],
        "DateTimeInput" => &[
            LABEL,
            ("value", Dyn(MAX_LABEL), true),
            ("enableDate", Bool, false),
            ("enableTime", Bool, false),
            ("min", Dyn(MAX_LABEL), false),
            ("max", Dyn(MAX_LABEL), false),
            CHECKS,
        ],
        _ => return None,
    })
}

const PILOT: &str =
    "Text, Row, Column, Card, Divider, Button, CheckBox, ChoicePicker, TextField, DateTimeInput";

/// A checked form: the envelope as sent, the fallback text every other
/// surface shows, and the digest a replay is compared by.
#[derive(Debug)]
pub struct Form {
    pub envelope: Value,
    pub fallback: String,
    pub digest: String,
}

fn bad(path: &str, why: impl std::fmt::Display) -> Error {
    Error::rejected(format!("form {path}: {why}"))
}

/// Check one envelope against the pilot profile.
pub fn check(envelope: &Value) -> Result<Form> {
    let bytes = serde_json::to_vec(envelope).map_err(|e| bad("", e))?;
    if bytes.len() > MAX_BYTES {
        return Err(bad("", format!("is over {MAX_BYTES} bytes")));
    }
    let top = object(envelope, "")?;
    fields(top, "", &["version", "createSurface"])?;
    if top.get("version").and_then(Value::as_str) != Some("1.0") {
        return Err(bad("/version", "must be \"1.0\""));
    }
    let p = "/createSurface";
    let create = object(required(top, "", "createSurface")?, p)?;
    fields(
        create,
        p,
        &[
            "surfaceId",
            "catalogId",
            "components",
            "dataModel",
            "sendDataModel",
        ],
    )?;
    ident(
        required(create, p, "surfaceId")?,
        "/createSurface/surfaceId",
    )?;
    if required(create, p, "catalogId")?.as_str() != Some(BASIC_CATALOG) {
        return Err(bad(
            "/createSurface/catalogId",
            format!("must be the basic catalog, {BASIC_CATALOG}"),
        ));
    }
    if let Some(v) = create.get("sendDataModel") {
        boolean(v, "/createSurface/sendDataModel")?;
    }
    if let Some(data) = create.get("dataModel") {
        object(data, "/createSurface/dataModel")?;
        if serde_json::to_vec(data).map_or(usize::MAX, |b| b.len()) > MAX_DATA_BYTES {
            return Err(bad(
                "/createSurface/dataModel",
                format!("is over {MAX_DATA_BYTES} bytes"),
            ));
        }
    }
    let p = "/createSurface/components";
    let list = required(create, "/createSurface", "components")?
        .as_array()
        .ok_or_else(|| bad(p, "must be an array"))?;
    if list.is_empty() || list.len() > MAX_COMPONENTS {
        return Err(bad(p, format!("takes 1-{MAX_COMPONENTS} components")));
    }
    let mut by_id: Vec<(&str, &Map<String, Value>)> = Vec::with_capacity(list.len());
    for (i, c) in list.iter().enumerate() {
        let path = format!("{p}/{i}");
        let obj = component(c, &path)?;
        let id = obj["id"].as_str().unwrap_or_default();
        if by_id.iter().any(|(seen, _)| *seen == id) {
            return Err(bad(&path, format!("id '{id}' is used twice")));
        }
        by_id.push((id, obj));
    }
    let order = tree(&by_id)?;
    if by_id.iter().filter(|(_, c)| kind(c) == "Card").count() > 1 {
        return Err(bad(p, "takes at most one Card"));
    }
    if !by_id.iter().any(|(_, c)| kind(c) == "Button") {
        return Err(bad(p, "needs a Button whose event sends the answer"));
    }
    Ok(Form {
        envelope: envelope.clone(),
        fallback: fallback(&order),
        digest: crate::delegation::sha256_hex(&bytes),
    })
}

fn kind(c: &Map<String, Value>) -> &str {
    c.get("component")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// One component: a pilot type, its own fields only, each of its type.
fn component<'a>(v: &'a Value, path: &str) -> Result<&'a Map<String, Value>> {
    let obj = object(v, path)?;
    ident(required(obj, path, "id")?, &format!("{path}/id"))?;
    let name = required(obj, path, "component")?
        .as_str()
        .unwrap_or_default();
    let spec = spec(name).ok_or_else(|| {
        bad(
            &format!("{path}/component"),
            format!("'{name}' is not in the pilot catalog ({PILOT})"),
        )
    })?;
    if name == "TextField" && obj.get("variant").and_then(Value::as_str) == Some("obscured") {
        return Err(bad(
            &format!("{path}/variant"),
            "obscured fields are never accepted - a form must not collect secrets",
        ));
    }
    for key in obj.keys() {
        let known = matches!(key.as_str(), "id" | "component" | "weight")
            || spec.iter().any(|f| f.0 == key);
        if !known {
            return Err(bad(
                &format!("{path}/{key}"),
                format!("is not a field of {name} in the pilot catalog"),
            ));
        }
    }
    if obj.get("weight").is_some_and(|w| !w.is_number()) {
        return Err(bad(&format!("{path}/weight"), "must be a number"));
    }
    for &(field, ty, needed) in spec {
        let at = format!("{path}/{field}");
        match obj.get(field) {
            Some(v) => value(ty, v, &at)?,
            None if needed => return Err(bad(&at, "is required")),
            None => {}
        }
    }
    Ok(obj)
}

fn value(ty: Ty, v: &Value, path: &str) -> Result<()> {
    match ty {
        Ty::Dyn(_) if v.is_object() => binding(v, path),
        Ty::Dyn(max) => text(v, path, max),
        Ty::DynBool if v.is_object() => binding(v, path),
        Ty::DynBool | Ty::Bool => boolean(v, path),
        Ty::DynList if v.is_object() => binding(v, path),
        Ty::DynList => list(v, path, 0, MAX_OPTIONS, |s, p| text(s, p, MAX_LABEL)),
        Ty::Id => ident(v, path),
        Ty::Ids => list(v, path, 1, MAX_COMPONENTS, ident),
        Ty::OneOf(allowed) => match v.as_str() {
            Some(s) if allowed.contains(&s) => Ok(()),
            _ => Err(bad(path, format!("must be one of {}", allowed.join(", ")))),
        },
        Ty::Action => action(v, path),
        Ty::Checks => list(v, path, 1, MAX_CHECKS, check_rule),
        Ty::Options => list(v, path, 1, MAX_OPTIONS, |o, p| {
            let o = object(o, p)?;
            fields(o, p, &["label", "value"])?;
            value(
                Ty::Dyn(MAX_LABEL),
                required(o, p, "label")?,
                &format!("{p}/label"),
            )?;
            text(required(o, p, "value")?, &format!("{p}/value"), MAX_LABEL)
        }),
    }
}

/// A list of `min..=max` items, each checked by `item`.
fn list(
    v: &Value,
    path: &str,
    min: usize,
    max: usize,
    item: impl Fn(&Value, &str) -> Result<()>,
) -> Result<()> {
    let items = v.as_array().ok_or_else(|| bad(path, "must be a list"))?;
    if items.len() < min || items.len() > max {
        return Err(bad(path, format!("takes {min}-{max} entries")));
    }
    items
        .iter()
        .enumerate()
        .try_for_each(|(i, x)| item(x, &format!("{path}/{i}")))
}

/// The tree under `root`: every reference resolves, each component is
/// used once, depth stays within the limit, and nothing is left outside.
/// Returns the components in reading order.
fn tree<'a>(by_id: &[(&'a str, &'a Map<String, Value>)]) -> Result<Vec<&'a Map<String, Value>>> {
    let p = "/createSurface/components";
    let find = |id: &str| by_id.iter().find(|(k, _)| *k == id).map(|(_, c)| *c);
    let root = find("root").ok_or_else(|| bad(p, "needs a component with id 'root'"))?;
    let mut order = Vec::with_capacity(by_id.len());
    let mut seen: Vec<&str> = Vec::with_capacity(by_id.len());
    let mut stack: Vec<(&str, &Map<String, Value>, usize)> = vec![("root", root, 1)];
    while let Some((id, c, depth)) = stack.pop() {
        if seen.contains(&id) {
            return Err(bad(
                p,
                format!("'{id}' is placed twice; each component appears once"),
            ));
        }
        if depth > MAX_DEPTH {
            return Err(bad(p, format!("'{id}' is nested deeper than {MAX_DEPTH}")));
        }
        seen.push(id);
        order.push(c);
        let mut kids: Vec<&str> = match c.get("children").and_then(Value::as_array) {
            Some(list) => list.iter().filter_map(Value::as_str).collect(),
            None => c.get("child").and_then(Value::as_str).into_iter().collect(),
        };
        if kind(c) == "Button"
            && kids
                .first()
                .and_then(|k| find(k))
                .is_some_and(|l| kind(l) != "Text")
        {
            return Err(bad(
                p,
                format!("Button '{id}' must label itself with a Text"),
            ));
        }
        kids.reverse();
        for k in kids {
            let child = find(k)
                .ok_or_else(|| bad(p, format!("'{id}' refers to '{k}', which is not defined")))?;
            stack.push((k, child, depth + 1));
        }
    }
    match by_id.iter().find(|(id, _)| !seen.contains(id)) {
        Some((id, _)) => Err(bad(p, format!("'{id}' is not under 'root'"))),
        None => Ok(order),
    }
}

fn literal<'a>(c: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    c.get(k).and_then(Value::as_str).map(str::trim)
}

/// The text every surface that cannot draw the form shows: its title, then
/// one line per field with its choices.
fn fallback(order: &[&Map<String, Value>]) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut titled = false;
    for &c in order {
        match kind(c) {
            "Text" if !titled => {
                if let Some(t) = literal(c, "text").filter(|t| !t.is_empty()) {
                    lines.push(t.to_string());
                    titled = true;
                }
            }
            "ChoicePicker" => {
                let opts = c
                    .get("options")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten();
                let names: Vec<&str> = opts
                    .filter_map(|o| o["label"].as_str().or_else(|| o["value"].as_str()))
                    .collect();
                let one = c.get("variant").and_then(Value::as_str) == Some("mutuallyExclusive");
                let how = if one { "choose one" } else { "choose any" };
                lines.push(format!(
                    "- {}: {} ({how})",
                    literal(c, "label").unwrap_or("Choose"),
                    names.join(", ")
                ));
            }
            "TextField" | "DateTimeInput" => lines.push(format!(
                "- {}",
                literal(c, "label").unwrap_or("Your answer")
            )),
            "CheckBox" => lines.push(format!(
                "- {} (yes or no)",
                literal(c, "label").unwrap_or("Confirm")
            )),
            _ => {}
        }
    }
    lines.push("Answer the form in the board, or reply here.".to_string());
    let mut out = lines.join("\n");
    if out.len() > MAX_FALLBACK {
        let mut end = MAX_FALLBACK;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
    }
    out
}

fn action(v: &Value, path: &str) -> Result<()> {
    let obj = object(v, path)?;
    if obj.contains_key("functionCall") {
        return Err(bad(
            &format!("{path}/functionCall"),
            "a form button only sends an event; it runs no function",
        ));
    }
    fields(obj, path, &["event"])?;
    let p = format!("{path}/event");
    let event = object(required(obj, path, "event")?, &p)?;
    fields(event, &p, &["name", "context"])?;
    let name = required(event, &p, "name")?.as_str().unwrap_or_default();
    let ok = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    if name.is_empty() || name.len() > MAX_ID || !name.chars().all(ok) {
        return Err(bad(&format!("{p}/name"), "must be 1-64 of [A-Za-z0-9._-]"));
    }
    let Some(ctx) = event.get("context") else {
        return Ok(());
    };
    let ctx = object(ctx, &format!("{p}/context"))?;
    if ctx.len() > MAX_CONTEXT {
        return Err(bad(
            &format!("{p}/context"),
            format!("takes at most {MAX_CONTEXT} keys"),
        ));
    }
    for (k, v) in ctx {
        let kp = format!("{p}/context/{k}");
        if k.is_empty() || k.len() > MAX_ID {
            return Err(bad(&kp, "key must be 1-64 characters"));
        }
        match v {
            Value::Object(_) => binding(v, &kp)?,
            Value::Array(_) => list(v, &kp, 0, MAX_OPTIONS, |s, p| text(s, p, MAX_LABEL))?,
            Value::String(_) => text(v, &kp, MAX_LABEL)?,
            _ => {}
        }
    }
    Ok(())
}

/// One `{condition, message?}` rule; the condition is a binding or one of
/// the pilot check functions with its own arguments.
fn check_rule(r: &Value, p: &str) -> Result<()> {
    let r = object(r, p)?;
    fields(r, p, &["condition", "message"])?;
    if let Some(m) = r.get("message") {
        text(m, &format!("{p}/message"), MAX_LABEL)?;
    }
    let cp = format!("{p}/condition");
    let cond = required(r, p, "condition")?;
    if object(cond, &cp)?.contains_key("@path") {
        return binding(cond, &cp);
    }
    let cond = object(cond, &cp)?;
    fields(cond, &cp, &["@call", "args"])?;
    let call = required(cond, &cp, "@call")?.as_str().unwrap_or_default();
    if !CHECK_FUNCTIONS.contains(&call) {
        return Err(bad(
            &format!("{cp}/@call"),
            format!(
                "'{call}' is not a pilot check ({})",
                CHECK_FUNCTIONS.join(", ")
            ),
        ));
    }
    let ap = format!("{cp}/args");
    let args = object(required(cond, &cp, "args")?, &ap)?;
    let allowed: &[&str] = match call {
        "length" | "numeric" => &["value", "min", "max"],
        _ => &["value"],
    };
    fields(args, &ap, allowed)?;
    let value = required(args, &ap, "value")?;
    if value.is_object() {
        binding(value, &format!("{ap}/value"))?;
    }
    match ["min", "max"]
        .into_iter()
        .find(|k| args.get(*k).is_some_and(|n| !n.is_number()))
    {
        Some(k) => Err(bad(&format!("{ap}/{k}"), "must be a number")),
        None => Ok(()),
    }
}

fn object<'a>(v: &'a Value, path: &str) -> Result<&'a Map<String, Value>> {
    v.as_object().ok_or_else(|| bad(path, "must be an object"))
}

fn required<'a>(obj: &'a Map<String, Value>, path: &str, key: &str) -> Result<&'a Value> {
    obj.get(key)
        .ok_or_else(|| bad(&format!("{path}/{key}"), "is required"))
}

fn fields(obj: &Map<String, Value>, path: &str, allowed: &[&str]) -> Result<()> {
    match obj.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(bad(&format!("{path}/{k}"), "is not accepted")),
        None => Ok(()),
    }
}

fn ident(v: &Value, path: &str) -> Result<()> {
    let s = v.as_str().unwrap_or_default();
    let ok = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-');
    if s.is_empty() || s.len() > MAX_ID || !s.chars().all(ok) {
        return Err(bad(path, "must be an id of 1-64 [A-Za-z0-9_-]"));
    }
    Ok(())
}

fn text(v: &Value, path: &str, max: usize) -> Result<()> {
    let s = v.as_str().ok_or_else(|| bad(path, "must be a string"))?;
    if s.chars().count() > max {
        return Err(bad(path, format!("is longer than {max} characters")));
    }
    if s.chars().any(|c| c.is_control() && c != '\n') {
        return Err(bad(path, "must not carry control characters"));
    }
    Ok(())
}

fn boolean(v: &Value, path: &str) -> Result<()> {
    if v.is_boolean() {
        Ok(())
    } else {
        Err(bad(path, "must be true or false"))
    }
}

/// `{"@path": "/a/b"}` - a JSON Pointer into the form's own data.
fn binding(v: &Value, path: &str) -> Result<()> {
    let obj = object(v, path)?;
    fields(obj, path, &["@path"])?;
    let p = obj.get("@path").and_then(Value::as_str).unwrap_or_default();
    if !p.starts_with('/') || p.len() > MAX_POINTER || p.chars().any(char::is_control) {
        return Err(bad(
            &format!("{path}/@path"),
            format!("must be an absolute JSON Pointer of at most {MAX_POINTER} characters"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The mockup's form: a Card with a title, two pickers, a time, a
    /// checkbox and a send button.
    fn weekly() -> Value {
        json!({"version": "1.0", "createSurface": {
            "surfaceId": "s-weekly", "catalogId": BASIC_CATALOG,
            "components": [
                {"id": "root", "component": "Card", "child": "col"},
                {"id": "col", "component": "Column",
                 "children": ["title", "projects", "when", "closed", "send"]},
                {"id": "title", "component": "Text", "text": "Weekly blocked summary"},
                {"id": "projects", "component": "ChoicePicker", "label": "Projects",
                 "variant": "multipleSelection", "displayStyle": "chips",
                 "options": [{"label": "Website", "value": "website"},
                             {"label": "Support", "value": "support"}],
                 "value": {"@path": "/projects"},
                 "@checks": [{"condition": {"@call": "required",
                                            "args": {"value": {"@path": "/projects"}}},
                              "message": "Pick at least one project."}]},
                {"id": "when", "component": "Row", "children": ["day", "time"]},
                {"id": "day", "component": "ChoicePicker", "label": "Day",
                 "variant": "mutuallyExclusive",
                 "options": [{"label": "Monday", "value": "mon"},
                             {"label": "Friday", "value": "fri"}],
                 "value": {"@path": "/day"}},
                {"id": "time", "component": "DateTimeInput", "label": "Time",
                 "enableTime": true, "value": {"@path": "/time"}},
                {"id": "closed", "component": "CheckBox",
                 "label": "Include items closed this week", "value": {"@path": "/closed"}},
                {"id": "send", "component": "Button", "variant": "primary", "child": "send_label",
                 "action": {"event": {"name": "weekly_summary.answer",
                                      "context": {"projects": {"@path": "/projects"}}}}},
                {"id": "send_label", "component": "Text", "text": "Send answer"}
            ],
            "dataModel": {"projects": ["website"], "day": ["mon"], "time": "09:00", "closed": false}
        }})
    }

    fn components(v: &mut Value) -> &mut Vec<Value> {
        v["createSurface"]["components"].as_array_mut().unwrap()
    }

    fn refused(v: &Value, needle: &str) {
        let err = check(v).expect_err("the form must be refused").to_string();
        assert!(err.contains(needle), "{err:?} should mention {needle:?}");
    }

    #[test]
    fn the_pilot_form_passes_with_a_fallback_and_a_digest() {
        let form = check(&weekly()).unwrap();
        assert_eq!(
            form.fallback,
            "Weekly blocked summary\n- Projects: Website, Support (choose any)\n\
             - Day: Monday, Friday (choose one)\n- Time\n\
             - Include items closed this week (yes or no)\n\
             Answer the form in the board, or reply here."
        );
        assert_eq!(form.digest.len(), 64);
        assert_eq!(check(&weekly()).unwrap().digest, form.digest);
    }

    #[test]
    fn components_outside_the_pilot_are_refused() {
        for name in ["Modal", "Image", "McpApp", "List"] {
            let mut v = weekly();
            components(&mut v)[2] = json!({"id": "title", "component": name});
            refused(&v, "is not in the pilot catalog");
        }
    }

    #[test]
    fn secret_url_and_unknown_fields_are_refused() {
        let mut v = weekly();
        components(&mut v)[2] = json!({"id": "title", "component": "TextField",
            "label": "Password", "value": "", "variant": "obscured"});
        refused(&v, "must not collect secrets");

        let mut v = weekly();
        components(&mut v)[2]["url"] = json!("https://example.com/x.png");
        refused(&v, "/url: is not a field of Text");

        let mut v = weekly();
        components(&mut v)[3]["filterable"] = json!(true);
        refused(&v, "/filterable: is not a field of ChoicePicker");

        let mut v = weekly();
        v["createSurface"]["metadata"] = json!({});
        refused(&v, "/createSurface/metadata: is not accepted");

        let mut v = weekly();
        v["createSurface"]["catalogId"] = json!("https://example.com/catalog.json");
        refused(&v, "must be the basic catalog");
    }

    #[test]
    fn actions_and_checks_stay_inside_the_pilot() {
        let mut v = weekly();
        components(&mut v)[8]["action"] =
            json!({"functionCall": {"@call": "openUrl", "args": {"url": "https://x"}}});
        refused(&v, "runs no function");

        let mut v = weekly();
        components(&mut v)[3]["@checks"][0]["condition"]["@call"] = json!("regex");
        refused(&v, "'regex' is not a pilot check");
    }

    #[test]
    fn the_tree_and_limits_are_enforced() {
        // depth 5: root > col > when > day ... nest one more Column.
        let mut v = weekly();
        let list = components(&mut v);
        list[4] = json!({"id": "when", "component": "Row", "children": ["deeper"]});
        list.push(json!({"id": "deeper", "component": "Column", "children": ["day", "time"]}));
        refused(&v, "nested deeper than 4");

        let mut v = weekly();
        components(&mut v)[1]["children"] =
            json!(["title", "projects", "when", "closed", "missing"]);
        refused(&v, "'missing', which is not defined");

        let mut v = weekly();
        components(&mut v)[1]["children"] = json!(["title", "projects", "when", "send"]);
        refused(&v, "'closed' is not under 'root'");

        let mut v = weekly();
        components(&mut v)[4]["children"] = json!(["day", "time", "title"]);
        refused(&v, "'title' is placed twice");

        let mut v = weekly();
        let filler: Vec<Value> = (0..31)
            .map(|i| json!({"id": format!("t{i}"), "component": "Text", "text": "x"}))
            .collect();
        components(&mut v).extend(filler);
        refused(&v, "takes 1-40 components");

        let mut v = weekly();
        components(&mut v)[3]["options"] = json!((0..13)
            .map(|i| json!({"label": format!("o{i}"), "value": format!("o{i}")}))
            .collect::<Vec<_>>());
        refused(&v, "/options: takes 1-12 entries");

        let mut v = weekly();
        components(&mut v).retain(|c| c["id"] != "send" && c["id"] != "send_label");
        components(&mut v)[1]["children"] = json!(["title", "projects", "when", "closed"]);
        refused(&v, "needs a Button");
    }
}
