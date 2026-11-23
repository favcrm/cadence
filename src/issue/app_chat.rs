//! CAD-1110: the install-time validator for `app-chat.json`, the data-only
//! `app-chat/v1` descriptor an app package ships next to `app.md`
//! (contract: docs/design/CAD-1109-app-chat-v1.md).
//!
//! This is the daemon's notation of the same grammar as
//! `contracts/app-chat/v1/app-chat.schema.json` and the board's
//! `parseAppChat` (ui/src/features/app-shell/chat/contract.ts). The
//! forbidden-key list and the two host registries (attachments, card
//! actions) are READ FROM THE SCHEMA FILE, never copied, so the three
//! notations cannot drift apart silently. The descriptor is data: this
//! module refuses it on the first violation, before the package is
//! staged, and never executes or interprets any of it.
//!
//! The same size/JSON/shape bounds are re-checked by the daemon read
//! (`app_chat_descriptor`) only as far as size and JSON validity; the
//! client's `parseAppChat` still fails closed on a bad descriptor.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

mod fields;

use std::collections::HashSet;
use std::sync::OnceLock;

use serde_json::Value;

use crate::error::Result;
use fields::{
    array, card_i18n, context_i18n, fail, ident, is_match, is_screen, label_i18n, object, prompts,
    scan, text, unique,
};

/// The package member holding the descriptor, a top-level regular file.
pub const FILE: &str = "app-chat.json";
/// Whole descriptor, UTF-8 bytes serialized as shipped.
pub const MAX_BYTES: usize = 16 * 1024;
const MAX_NODES: usize = 1024;
const MAX_DEPTH: usize = 12;

const SCHEMA: &str = include_str!("../../contracts/app-chat/v1/app-chat.schema.json");

struct Grammar {
    forbidden: HashSet<String>,
    attachments: Vec<String>,
    actions: Vec<String>,
}

fn grammar() -> &'static Grammar {
    static G: OnceLock<Grammar> = OnceLock::new();
    G.get_or_init(|| {
        let schema: Value = serde_json::from_str(SCHEMA).expect("app-chat schema is JSON");
        let list = |v: &Value| -> Vec<String> {
            v.as_array()
                .expect("schema list")
                .iter()
                .map(|s| s.as_str().expect("schema string").to_string())
                .collect()
        };
        let defs = &schema["$defs"];
        Grammar {
            forbidden: list(&defs["safeObject"]["propertyNames"]["not"]["enum"])
                .into_iter()
                .collect(),
            attachments: list(&defs["attachment"]["properties"]["id"]["enum"]),
            actions: list(
                &defs["directive"]["properties"]["card"]["properties"]["buttons"]["items"]
                    ["properties"]["run"]["enum"],
            ),
        }
    })
}

/// Refuse the descriptor text unless it is a valid `app-chat/v1` for
/// `installed_app` (the manifest's `app`). Returns the parsed value.
pub fn validate(raw: &str, installed_app: &str) -> Result<Value> {
    if raw.len() > MAX_BYTES {
        return fail("$", format!("descriptor exceeds {MAX_BYTES} bytes"));
    }
    let value: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return fail("$", "not valid JSON"),
    };
    scan(&value, "$", &mut 0, 0)?;
    let root = object(
        &value,
        "$",
        &[
            "contract",
            "app",
            "contexts",
            "attachments",
            "directives",
            "subjects",
            "presentation",
        ],
    )?;
    if root.get("contract").and_then(Value::as_str) != Some("app-chat/v1") {
        return fail("contract", "expected app-chat/v1");
    }
    let app = ident(root.get("app"), "app")?;
    if app != installed_app {
        return fail("app", "descriptor names a different app than the package");
    }
    let mut seen = HashSet::new();
    for (i, c) in array(root.get("contexts"), "contexts", 12)?
        .iter()
        .enumerate()
    {
        let p = format!("contexts.{i}");
        let c = object(c, &p, &["id", "label", "prompts", "record", "i18n"])?;
        unique(
            &mut seen,
            ident(c.get("id"), &format!("{p}.id"))?,
            &p,
            "context id",
        )?;
        text(c.get("label"), &format!("{p}.label"), 40)?;
        prompts(c.get("prompts"), &format!("{p}.prompts"))?;
        context_i18n(c, &p)?;
        if let Some(r) = c.get("record") {
            let rp = format!("{p}.record");
            let r = object(r, &rp, &["label", "prompts", "i18n"])?;
            text(r.get("label"), &format!("{rp}.label"), 40)?;
            prompts(r.get("prompts"), &format!("{rp}.prompts"))?;
            context_i18n(r, &rp)?;
        }
    }
    let mut seen = HashSet::new();
    for (i, a) in array(root.get("attachments"), "attachments", 4)?
        .iter()
        .enumerate()
    {
        let p = format!("attachments.{i}");
        let a = object(a, &p, &["id", "label", "i18n"])?;
        let id = ident(a.get("id"), &format!("{p}.id"))?;
        if !grammar().attachments.contains(&id) {
            return fail(&format!("{p}.id"), format!("not a host capability: {id}"));
        }
        unique(&mut seen, id, &p, "attachment")?;
        if a.contains_key("label") {
            text(a.get("label"), &format!("{p}.label"), 40)?;
        }
        label_i18n(a, &p)?;
    }
    let mut seen = HashSet::new();
    for (i, d) in array(root.get("directives"), "directives", 8)?
        .iter()
        .enumerate()
    {
        let p = format!("directives.{i}");
        let d = object(d, &p, &["match", "card", "render", "size"])?;
        let m = text(d.get("match"), &format!("{p}.match"), 64)?;
        if !is_match(&m) {
            return fail(&format!("{p}.match"), "expected cadence_<snake_case>");
        }
        unique(&mut seen, m, &p, "directive match")?;
        match (d.get("card"), d.get("render")) {
            (Some(card), None) => {
                if d.contains_key("size") {
                    return fail(&format!("{p}.size"), "size belongs to a render directive");
                }
                card_shape(card, &format!("{p}.card"))?;
            }
            (None, Some(render)) => {
                let r = text(Some(render), &format!("{p}.render"), 40)?;
                if !is_screen(&r) {
                    return fail(&format!("{p}.render"), "expected screen:<tag>");
                }
                if let Some(size) = d.get("size") {
                    if !matches!(size.as_str(), Some("small" | "medium" | "large")) {
                        return fail(&format!("{p}.size"), "expected small, medium or large");
                    }
                }
            }
            _ => return fail(&p, "a directive has exactly one of card or render"),
        }
    }
    let mut seen = HashSet::new();
    for (i, s) in array(root.get("subjects"), "subjects", 8)?
        .iter()
        .enumerate()
    {
        let p = format!("subjects.{i}");
        let s = object(s, &p, &["kind", "label", "i18n"])?;
        unique(
            &mut seen,
            ident(s.get("kind"), &format!("{p}.kind"))?,
            &p,
            "subject kind",
        )?;
        text(s.get("label"), &format!("{p}.label"), 40)?;
        label_i18n(s, &p)?;
    }
    if let Some(pres) = root.get("presentation") {
        let pres = object(pres, "presentation", &["showContext"])?;
        if pres.get("showContext").is_some_and(|v| !v.is_boolean()) {
            return fail("presentation.showContext", "expected a boolean");
        }
    }
    Ok(value)
}

fn card_shape(card: &Value, p: &str) -> Result<()> {
    let c = object(card, p, &["title", "text", "fields", "buttons", "i18n"])?;
    text(c.get("title"), &format!("{p}.title"), 80)?;
    if c.contains_key("text") {
        text(c.get("text"), &format!("{p}.text"), 240)?;
    }
    card_i18n(c, p)?;
    for (i, f) in array(c.get("fields"), &format!("{p}.fields"), 4)?
        .iter()
        .enumerate()
    {
        let fp = format!("{p}.fields.{i}");
        let f = object(f, &fp, &["label", "from", "i18n"])?;
        text(f.get("label"), &format!("{fp}.label"), 40)?;
        ident(f.get("from"), &format!("{fp}.from"))?;
        label_i18n(f, &fp)?;
    }
    for (i, b) in array(c.get("buttons"), &format!("{p}.buttons"), 2)?
        .iter()
        .enumerate()
    {
        let bp = format!("{p}.buttons.{i}");
        let b = object(b, &bp, &["label", "run", "view", "i18n"])?;
        text(b.get("label"), &format!("{bp}.label"), 40)?;
        let run = ident(b.get("run"), &format!("{bp}.run"))?;
        if !grammar().actions.contains(&run) {
            return fail(&format!("{bp}.run"), format!("not a host action: {run}"));
        }
        // open-view is the one v1 action and takes exactly a view id.
        ident(b.get("view"), &format!("{bp}.view"))?;
        label_i18n(b, &bp)?;
    }
    Ok(())
}

/// The read path's own check: size and JSON validity only. The grammar is
/// the validator's (install time) and the client's, never the route's.
pub fn size_and_json(raw: &str) -> Result<Value> {
    if raw.len() > MAX_BYTES {
        return fail("$", format!("descriptor exceeds {MAX_BYTES} bytes"));
    }
    serde_json::from_str(raw).or_else(|_| fail("$", "not valid JSON"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn good() -> Value {
        json!({
            "contract": "app-chat/v1", "app": "demo",
            "contexts": [{"id": "home", "label": "Home", "prompts": ["Hi"], "record": {"label": "Thing", "prompts": ["More"]}}],
            "attachments": [{"id": "csv-import", "label": "Import"}],
            "directives": [
                {"match": "cadence_note", "card": {"title": "Note", "fields": [{"label": "T", "from": "title"}], "buttons": [{"label": "Open", "run": "open-view", "view": "home"}]}},
                {"match": "cadence_preview", "render": "screen:preview", "size": "large"}
            ],
            "subjects": [{"kind": "campaign", "label": "Campaign"}],
            "presentation": {"showContext": true}
        })
    }

    fn check(v: &Value) -> Result<Value> {
        validate(&v.to_string(), "demo")
    }

    #[test]
    fn accepts_the_contract_example_and_refuses_each_violation() {
        check(&good()).unwrap();
        let mut bad = good();
        bad["contract"] = json!("app-chat/v2");
        assert!(check(&bad).is_err(), "unknown contract tag");
        let mut bad = good();
        bad["app"] = json!("other");
        assert!(check(&bad).is_err(), "app differs from the package");
        let mut bad = good();
        bad["directives"][0]["card"]["buttons"][0]["run"] = json!("delete-everything");
        assert!(check(&bad).is_err(), "run outside the host registry");
        let mut bad = good();
        bad["attachments"][0]["id"] = json!("shell");
        assert!(check(&bad).is_err(), "attachment outside the host registry");
        let mut bad = good();
        bad["contexts"][0]["install_id"] = json!("x");
        assert!(check(&bad).is_err(), "forbidden key");
        let mut bad = good();
        bad["directives"][0]["render"] = json!("screen:x");
        assert!(check(&bad).is_err(), "both card and render");
        let mut bad = good();
        bad["directives"][1]["size"] = json!("huge");
        assert!(check(&bad).is_err(), "unknown size");
        let mut bad = good();
        bad["extra"] = json!(1);
        assert!(check(&bad).is_err(), "unknown key");
        let mut bad = good();
        bad["contexts"][0]["prompts"] = json!(["a", "b", "c", "d"]);
        assert!(check(&bad).is_err(), "more than 3 prompts");
        let mut bad = good();
        bad["contexts"][0]["label"] = json!("x".repeat(41));
        assert!(check(&bad).is_err(), "label one over");
        let mut ok = good();
        ok["contexts"][0]["label"] = json!("x".repeat(40));
        check(&ok).unwrap();
        assert!(
            validate(&"x".repeat(MAX_BYTES + 1), "demo").is_err(),
            "over 16 KiB"
        );
        assert!(validate("{", "demo").is_err(), "torn JSON");
    }

    #[test]
    fn the_schema_is_the_source_of_the_registries_and_forbidden_keys() {
        let g = grammar();
        assert_eq!(g.attachments, ["csv-import"]);
        assert_eq!(g.actions, ["open-view"]);
        assert!(g.forbidden.contains("install_id") && g.forbidden.contains("action"));
    }

    #[test]
    fn validates_i18n_at_each_display_surface_and_refuses_violations() {
        let mut ok = good();
        ok["contexts"][0]["i18n"] = json!({"zh-TW":{"label":"客","prompts":["列"]}});
        ok["contexts"][0]["record"] =
            json!({"label":"R","prompts":["R0"],"i18n":{"zh-TW":{"label":"客","prompts":["摘"]}}});
        ok["attachments"] =
            json!([{"id":"csv-import","label":"Imp","i18n":{"zh-TW":{"label":"匯"}}}]);
        ok["subjects"] = json!([{"kind":"c","label":"C","i18n":{"zh-TW":{"label":"客"}}}]);
        ok["directives"] = json!([{"match":"cadence_m","card":{"title":"T","text":"D","i18n":{"zh-TW":{"title":"匯","text":"完"}},"fields":[{"label":"R","from":"r","i18n":{"zh-TW":{"label":"筆"}}}],"buttons":[{"label":"O","run":"open-view","view":"v","i18n":{"zh-TW":{"label":"開"}}}]}}]);
        validate(&ok.to_string(), "demo").unwrap();

        for (k, v) in [
            ("ctx", json!({})),
            ("ctx", json!({"zh_TW":{"label":"客"}})),
            ("ctx", json!({"zh-TW":{}})),
            ("ctx", json!({"zh-TW":{"prompts":[]}})),
            ("ctx", json!({"zh-TW":{"label":"   "}})),
            ("ctx", json!({"zh-TW":{"prompts":["\u{0085}"]}})),
            ("att", json!({"zh-TW":{"title":"x"}})),
            ("att", json!({"zh-TW":{"label":"   "}})),
            ("card", json!({"zh-TW":{"label":"x"}})),
            ("card", json!({"zh-TW":{"text":"x\u{0085}y"}})),
            ("sub", json!({"zh-TW":{"label":"客\n"}})),
            ("sub", json!({"zh-TW":{"label":"\u{0085}"}})),
            ("sub", json!({"zh-TW":{"label":"   "}})),
            (
                "sub",
                json!({"zh-tw":{"label":"客"},"zh-TW":{"label":"客"}}),
            ),
        ] {
            let mut b = ok.clone();
            match k {
                "ctx" => b["contexts"][0]["i18n"] = v,
                "att" => b["attachments"][0]["i18n"] = v,
                "card" => b["directives"][0]["card"]["i18n"] = v,
                _ => b["subjects"][0]["i18n"] = v,
            }
            assert!(validate(&b.to_string(), "demo").is_err(), "{k}");
        }
        let mut b = ok.clone();
        b["subjects"][0]["i18n"] = Value::Object(
            (0..9)
                .map(|i| (format!("en-x{i}"), json!({"label":"X"})))
                .collect(),
        );
        assert!(validate(&b.to_string(), "demo").is_err());
    }
}
