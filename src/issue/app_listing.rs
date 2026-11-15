//! CAD-1129 §3: the `listing` manifest section — display-only catalog
//! copy, digest-covered, beside the capability contract it describes.
//!
//! A `listing:` mapping in `app.md` frontmatter is optional and never
//! part of `needs` — it cannot grant a capability, declare a slot or
//! carry a workflow verb. Every value is plain text: no HTML, no
//! markdown, no control characters; every referenced path stays under
//! `assets/`; every slot it names is declared in `needs`; unknown keys
//! refuse with the same fail-loud rule the rest of `app.md` applies.
//!
//! Prices and costs are refused ANYWHERE in a bundle (decision 8): the
//! keys `cost`, `price` and `pricing` fail the parse wherever they
//! appear, at any depth, in `listing` or anywhere else in the
//! frontmatter. The host's spend safety stays in the frozen quote,
//! `max_charge_minor` and the global per-call bound — the UI simply
//! never renders a price.

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::model;

/// Largest `listing` block, measured on its YAML source, bytes.
const LISTING_CAP: usize = 16 * 1024;

/// The host's closed category list — anything else refuses.
const CATEGORIES: &[&str] = &[
    "marketing",
    "customers",
    "operations",
    "finance",
    "content",
    "other",
];

/// `app.md` keys a price would hide behind (decision 8): refused
/// anywhere in the frontmatter mapping, at any depth.
const PRICE_KEYS: &[&str] = &["cost", "price", "pricing"];

/// One parsed `listing` block — plain JSON value, already checked,
/// plus the asset paths it references (the caller proves each exists
/// in the bundle's own file inventory).
#[derive(Clone, Debug)]
pub struct Listing {
    pub value: Value,
    pub assets: Vec<String>,
}

fn check_plain_text(
    value: &serde_yaml::Value,
    what: &str,
    max: usize,
    allow_newlines: bool,
) -> Result<String> {
    let Some(text) = value.as_str() else {
        return Err(Error::rejected(format!("listing {what} is a plain string")));
    };
    let text = text.trim();
    if text.is_empty()
        || text.encode_utf16().count() > max
        || text.chars().any(|c| {
            (c.is_control() && (!allow_newlines || c != '\n'))
                || matches!(c, '\u{2028}' | '\u{2029}')
        })
        || text.contains('<')
        || text.contains('>')
        || text.contains("](")
        || text.starts_with('#')
        || text.starts_with("http://")
        || text.starts_with("https://")
    {
        return Err(Error::rejected(format!(
            "listing {what} is ≤{max} chars of plain text — no markup, links or controls"
        )));
    }
    Ok(text.to_string())
}

fn plain_text(value: &serde_yaml::Value, what: &str, max: usize) -> Result<String> {
    check_plain_text(value, what, max, false)
}

fn plain_multiline_text(value: &serde_yaml::Value, what: &str, max: usize) -> Result<String> {
    check_plain_text(value, what, max, true)
}

/// `assets/<leaf>.svg` — flat, tag-shaped stem, real file under the
/// bundle's `assets/` (paths outside are refused).
fn asset_path(value: &serde_yaml::Value, what: &str, max_kb: u64) -> Result<String> {
    let raw = value
        .as_str()
        .ok_or_else(|| Error::rejected(format!("listing {what} is an assets/ path")))?;
    if raw.len() > 128 {
        return Err(Error::rejected(format!("listing {what} path is too long")));
    }
    let Some(leaf) = raw.strip_prefix("assets/") else {
        return Err(Error::rejected(format!(
            "listing {what} '{raw}' — assets stay under assets/"
        )));
    };
    let Some(stem) = leaf.strip_suffix(".svg") else {
        return Err(Error::rejected(format!(
            "listing {what} '{raw}' — assets are flat *.svg files"
        )));
    };
    if stem.is_empty()
        || stem.contains('/')
        || !stem
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(Error::rejected(format!(
            "listing {what} '{raw}' — the file name is [A-Za-z0-9_-]+.svg"
        )));
    }
    let _ = max_kb;
    Ok(raw.to_string())
}

fn known_keys(map: &serde_yaml::Mapping, allowed: &[&str], what: &str) -> Result<()> {
    for key in map.keys() {
        let Some(k) = key.as_str() else {
            return Err(Error::rejected(format!(
                "listing {what} has a non-string key"
            )));
        };
        if !allowed.contains(&k) {
            return Err(Error::rejected(format!(
                "listing {what} key '{k}' is unknown — v1 knows {}",
                allowed.join(", ")
            )));
        }
    }
    Ok(())
}

fn yget<'a>(map: &'a serde_yaml::Mapping, key: &str) -> Option<&'a serde_yaml::Value> {
    map.get(serde_yaml::Value::String(key.to_string()))
}

fn seq<'a>(
    map: &'a serde_yaml::Mapping,
    key: &str,
    what: &str,
    max: usize,
) -> Result<Vec<&'a serde_yaml::Value>> {
    match yget(map, key) {
        None | Some(serde_yaml::Value::Null) => Ok(vec![]),
        Some(serde_yaml::Value::Sequence(items)) => {
            if items.len() > max {
                return Err(Error::rejected(format!(
                    "listing {what} carries more than {max} entries"
                )));
            }
            Ok(items.iter().collect())
        }
        Some(_) => Err(Error::rejected(format!("listing {what} is a list"))),
    }
}

/// Refuse `cost`/`price`/`pricing` at any depth of a YAML value
/// (decision 8) — including `listing` and every other frontmatter key.
pub(crate) fn refuse_price_keys(value: &serde_yaml::Value) -> Result<()> {
    match value {
        serde_yaml::Value::Mapping(map) => {
            for (k, v) in map {
                if let Some(key) = k.as_str() {
                    if PRICE_KEYS.contains(&key) {
                        return Err(Error::rejected(format!(
                            "app.md key '{key}': prices are not shown (CAD-1129 decision 8) — \
                             spend safety lives in the host's frozen quote, the charge \
                             ceiling and the global per-call bound"
                        )));
                    }
                }
                refuse_price_keys(v)?;
            }
            Ok(())
        }
        serde_yaml::Value::Sequence(items) => {
            for item in items {
                refuse_price_keys(item)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Parse `listing` from the frontmatter's raw YAML value. `slots` is
/// every slot the app declares (`needs.connections` plus
/// `needs.capabilities`) — `access_notes` and `setup` may name only
/// those. Asset paths are collected on the result; the caller checks
/// each against the bundle's file inventory.
pub(crate) fn parse(
    value: &serde_yaml::Value,
    yaml_len: usize,
    slots: &[String],
) -> Result<Listing> {
    if yaml_len > LISTING_CAP {
        return Err(Error::rejected(format!(
            "app.md `listing:` is over {LISTING_CAP} bytes — catalog copy is small"
        )));
    }
    let serde_yaml::Value::Mapping(map) = value else {
        return Err(Error::rejected(
            "app.md `listing:` is a mapping of catalog display fields",
        ));
    };
    known_keys(
        map,
        &[
            "tagline",
            "icon",
            "screenshots",
            "category",
            "tags",
            "publisher",
            "about",
            "can",
            "access_notes",
            "changes",
            "setup",
            "data",
            "i18n",
        ],
        "",
    )?;
    let tagline = match yget(map, "tagline") {
        Some(v) => Some(plain_text(v, "tagline", 80)?),
        None => None,
    };
    let icon = match yget(map, "icon") {
        Some(v) => Some(asset_path(v, "icon", 64)?),
        None => None,
    };
    let mut shots = Vec::new();
    for item in seq(map, "screenshots", "screenshots", 5)? {
        let serde_yaml::Value::Mapping(shot) = item else {
            return Err(Error::rejected(
                "listing screenshots entries are {file, caption}",
            ));
        };
        known_keys(shot, &["file", "caption"], "screenshots")?;
        let file = asset_path(
            yget(shot, "file")
                .ok_or_else(|| Error::rejected("listing screenshots entries need a `file:`"))?,
            "screenshots.file",
            256,
        )?;
        let caption = match yget(shot, "caption") {
            Some(v) => Some(plain_text(v, "screenshots.caption", 60)?),
            None => None,
        };
        shots.push(json!({"file": file, "caption": caption}));
    }
    let category = match yget(map, "category") {
        Some(v) => {
            let cat = plain_text(v, "category", 20)?;
            if !CATEGORIES.contains(&cat.as_str()) {
                return Err(Error::rejected(format!(
                    "listing category '{cat}' — v1 knows {}",
                    CATEGORIES.join(", ")
                )));
            }
            Some(cat)
        }
        None => None,
    };
    let mut tags = Vec::new();
    for item in seq(map, "tags", "tags", 5)? {
        let tag = item
            .as_str()
            .ok_or_else(|| Error::rejected("listing tags are tag-shaped strings"))?;
        if !model::valid_tag(tag) {
            return Err(Error::rejected(format!(
                "listing tag '{tag}' is not tag-shaped"
            )));
        }
        tags.push(json!(tag));
    }
    let publisher =
        match yget(map, "publisher") {
            None | Some(serde_yaml::Value::Null) => None,
            Some(serde_yaml::Value::Mapping(p)) => {
                known_keys(p, &["name", "url"], "publisher")?;
                let name = plain_text(
                    yget(p, "name")
                        .ok_or_else(|| Error::rejected("listing publisher needs a `name:`"))?,
                    "publisher.name",
                    40,
                )?;
                let url = match yget(p, "url") {
                    Some(v) => {
                        let u = v.as_str().ok_or_else(|| {
                            Error::rejected("listing publisher.url is an https:// link")
                        })?;
                        if u.len() > 200 || !u.starts_with("https://") {
                            return Err(Error::rejected(
                                "listing publisher.url is an https:// link, ≤200 chars",
                            ));
                        }
                        Some(u.to_string())
                    }
                    None => None,
                };
                Some(json!({"name": name, "url": url}))
            }
            Some(_) => return Err(Error::rejected(
                "listing publisher is {name, url?} — display only; the trust chip is host-computed",
            )),
        };
    let about = match yget(map, "about") {
        Some(v) => Some(plain_multiline_text(v, "about", 600)?),
        None => None,
    };
    let mut can = Vec::new();
    for item in seq(map, "can", "can", 5)? {
        can.push(json!(plain_text(item, "can", 100)?));
    }
    let access_notes = match yget(map, "access_notes") {
        None | Some(serde_yaml::Value::Null) => None,
        Some(serde_yaml::Value::Mapping(notes)) => {
            let mut out = serde_json::Map::new();
            for (k, v) in notes {
                let slot = k
                    .as_str()
                    .ok_or_else(|| Error::rejected("listing access_notes keys are slot names"))?;
                if !slots.iter().any(|s| s == slot) {
                    return Err(Error::rejected(format!(
                        "listing access_notes.{slot} — the app declares no such slot"
                    )));
                }
                out.insert(slot.to_string(), json!(plain_text(v, "access_notes", 140)?));
            }
            Some(Value::Object(out))
        }
        Some(_) => {
            return Err(Error::rejected(
                "listing access_notes is a {slot: note} mapping",
            ))
        }
    };
    let mut changes = Vec::new();
    for item in seq(map, "changes", "changes", 5)? {
        let serde_yaml::Value::Mapping(entry) = item else {
            return Err(Error::rejected(
                "listing changes entries are {version, notes, keeps?}",
            ));
        };
        known_keys(entry, &["version", "notes", "keeps"], "changes")?;
        let version = plain_text(
            yget(entry, "version")
                .ok_or_else(|| Error::rejected("listing changes entries need `version:`"))?,
            "changes.version",
            40,
        )?;
        let mut notes = Vec::new();
        let Some(serde_yaml::Value::Sequence(list)) = yget(entry, "notes") else {
            return Err(Error::rejected("listing changes.notes is a list"));
        };
        if list.len() > 5 {
            return Err(Error::rejected("listing changes.notes holds ≤5 bullets"));
        }
        for note in list {
            notes.push(json!(plain_text(note, "changes.notes", 120)?));
        }
        let keeps = match yget(entry, "keeps") {
            Some(v) => Some(plain_text(v, "changes.keeps", 140)?),
            None => None,
        };
        changes.push(json!({"version": version, "notes": notes, "keeps": keeps}));
    }
    let mut setup = Vec::new();
    for item in seq(map, "setup", "setup", 6)? {
        let serde_yaml::Value::Mapping(step) = item else {
            return Err(Error::rejected(
                "listing setup entries are {slot|connection, label, help?, recommended?}",
            ));
        };
        known_keys(
            step,
            &["slot", "connection", "label", "help", "recommended"],
            "setup",
        )?;
        let slot = match yget(step, "slot").or_else(|| yget(step, "connection")) {
            Some(v) => {
                let name = v
                    .as_str()
                    .ok_or_else(|| Error::rejected("listing setup slot is a declared slot name"))?;
                if !slots.iter().any(|s| s == name) {
                    return Err(Error::rejected(format!(
                        "listing setup slot '{name}' — the app declares no such slot"
                    )));
                }
                name.to_string()
            }
            None => {
                return Err(Error::rejected(
                    "listing setup entries target a declared `slot:` or `connection:`",
                ))
            }
        };
        let label = plain_text(
            yget(step, "label")
                .ok_or_else(|| Error::rejected("listing setup entries need a `label:`"))?,
            "setup.label",
            80,
        )?;
        let help = match yget(step, "help") {
            Some(v) => Some(plain_text(v, "setup.help", 140)?),
            None => None,
        };
        let recommended = match yget(step, "recommended") {
            Some(v) => Some(plain_text(v, "setup.recommended", 140)?),
            None => None,
        };
        setup.push(json!({"slot": slot, "label": label, "help": help, "recommended": recommended}));
    }
    let data = match yget(map, "data") {
        None | Some(serde_yaml::Value::Null) => None,
        Some(serde_yaml::Value::Mapping(d)) => {
            known_keys(d, &["stores", "personal"], "data")?;
            let mut stores = Vec::new();
            if let Some(serde_yaml::Value::Sequence(list)) = yget(d, "stores") {
                if list.len() > 5 {
                    return Err(Error::rejected("listing data.stores holds ≤5 sentences"));
                }
                for item in list {
                    stores.push(json!(plain_text(item, "data.stores", 120)?));
                }
            }
            let personal = match yget(d, "personal") {
                Some(serde_yaml::Value::Bool(b)) => Some(*b),
                Some(_) => return Err(Error::rejected("listing data.personal is true or false")),
                None => None,
            };
            Some(json!({"stores": stores, "personal": personal}))
        }
        Some(_) => {
            return Err(Error::rejected(
                "listing data is {stores?, personal?} — plain sentences",
            ))
        }
    };
    let i18n = match yget(map, "i18n") {
        None | Some(serde_yaml::Value::Null) => Value::Null,
        Some(value) => parse_listing_i18n(value, slots)?,
    };
    // The referenced assets: the caller proves each is in the
    // bundle's inventory (`assets/` flat *.svg). Collected, not
    // checked here — the parse runs before the file list exists.
    let mut referenced: Vec<String> = Vec::new();
    if let Some(icon) = &icon {
        referenced.push(icon.clone());
    }
    for shot in &shots {
        if let Some(file) = shot["file"].as_str() {
            referenced.push(file.to_string());
        }
    }
    Ok(Listing {
        assets: referenced,
        value: json!({
            "tagline": tagline,
            "icon": icon,
            "screenshots": shots,
            "category": category,
            "tags": tags,
            "publisher": publisher,
            "about": about,
            "can": can,
            "access_notes": access_notes,
            "changes": changes,
            "setup": setup,
            "data": data,
            "i18n": i18n,
        }),
    })
}

fn parse_i18n_locales(
    value: &serde_yaml::Value,
    allowed: &[&str],
    what: &str,
    mut parse_entry: impl FnMut(&str, &serde_yaml::Mapping) -> Result<serde_json::Map<String, Value>>,
) -> Result<Value> {
    let serde_yaml::Value::Mapping(locales) = value else {
        return Err(Error::rejected(format!("{what} must be a locale mapping")));
    };
    if locales.is_empty() || locales.len() > cadence_core::app_screen_decl::MAX_I18N_LOCALES {
        return Err(Error::rejected(format!(
            "{what} requires 1..={} locale entries",
            cadence_core::app_screen_decl::MAX_I18N_LOCALES
        )));
    }
    let mut out = serde_json::Map::new();
    let mut seen_locales = std::collections::HashSet::new();
    for (k, v) in locales {
        let Some(locale) = k.as_str() else {
            return Err(Error::rejected(format!(
                "{what} keys must be BCP-47 locale tags"
            )));
        };
        if !cadence_core::app_screen_decl::valid_locale_tag(locale) {
            return Err(Error::rejected(format!(
                "{what} locale key '{locale}' must be a valid BCP-47 tag"
            )));
        }
        if !seen_locales.insert(locale.to_ascii_lowercase()) {
            return Err(Error::rejected(format!(
                "{what} locale '{locale}' is declared twice"
            )));
        }
        let serde_yaml::Value::Mapping(entry) = v else {
            return Err(Error::rejected(format!(
                "{what}.{locale} must be a mapping"
            )));
        };
        known_keys(entry, allowed, &format!("{what}.{locale}"))?;
        let parsed = parse_entry(locale, entry)?;
        if parsed.is_empty() {
            return Err(Error::rejected(format!(
                "{what}.{locale} must specify at least one localized field"
            )));
        }
        out.insert(locale.to_string(), Value::Object(parsed));
    }
    Ok(Value::Object(out))
}

fn parse_optional_text_fields(
    entry: &serde_yaml::Mapping,
    fields: &[(&str, &str, usize, bool)],
    out: &mut serde_json::Map<String, Value>,
) -> Result<()> {
    for &(key, label, max, multiline) in fields {
        if let Some(v) = yget(entry, key) {
            if !v.is_null() {
                let text = if multiline {
                    plain_multiline_text(v, label, max)?
                } else {
                    plain_text(v, label, max)?
                };
                out.insert(key.to_string(), json!(text));
            }
        }
    }
    Ok(())
}

fn parse_listing_i18n(value: &serde_yaml::Value, slots: &[String]) -> Result<Value> {
    const KEYS: &[&str] = &[
        "title",
        "tagline",
        "summary",
        "about",
        "can",
        "access_notes",
    ];
    parse_i18n_locales(value, KEYS, "listing i18n", |locale, entry| {
        let mut obj = serde_json::Map::new();
        parse_optional_text_fields(
            entry,
            &[
                ("title", "i18n.title", 80, false),
                ("tagline", "i18n.tagline", 80, false),
                ("summary", "i18n.summary", 160, false),
                ("about", "i18n.about", 600, true),
            ],
            &mut obj,
        )?;
        if yget(entry, "can").is_some_and(|v| !v.is_null()) {
            let items = seq(entry, "can", &format!("i18n.{locale}.can"), 5)?;
            if items.is_empty() {
                return Err(Error::rejected(format!(
                    "listing i18n.{locale}.can cannot be empty"
                )));
            }
            let mut can = Vec::with_capacity(items.len());
            for item in items {
                can.push(json!(plain_text(item, "i18n.can", 100)?));
            }
            obj.insert("can".to_string(), Value::Array(can));
        }
        if let Some(v) = yget(entry, "access_notes") {
            if !v.is_null() {
                let serde_yaml::Value::Mapping(notes) = v else {
                    return Err(Error::rejected(format!(
                        "listing i18n.{locale}.access_notes is a {{slot: note}} mapping"
                    )));
                };
                let mut note_map = serde_json::Map::new();
                for (k, nv) in notes {
                    let slot = k.as_str().ok_or_else(|| {
                        Error::rejected("listing i18n access_notes keys are slot names")
                    })?;
                    if !slots.iter().any(|s| s == slot) {
                        return Err(Error::rejected(format!(
                            "listing i18n.{locale}.access_notes.{slot} — the app declares no such slot"
                        )));
                    }
                    note_map.insert(
                        slot.to_string(),
                        json!(plain_text(nv, "i18n.access_notes", 140)?),
                    );
                }
                if !note_map.is_empty() {
                    obj.insert("access_notes".to_string(), Value::Object(note_map));
                }
            }
        }
        Ok(obj)
    })
}

/// Parse top-level `i18n:` from `app.md` frontmatter (`{ <locale>: { title?, summary?, tagline? } }`).
pub(crate) fn parse_manifest_i18n(value: &serde_yaml::Value) -> Result<Value> {
    parse_i18n_locales(
        value,
        &["title", "summary", "tagline"],
        "app.md i18n",
        |_locale, entry| {
            let mut obj = serde_json::Map::new();
            parse_optional_text_fields(
                entry,
                &[
                    ("title", "i18n.title", 80, false),
                    ("summary", "i18n.summary", 160, false),
                    ("tagline", "i18n.tagline", 80, false),
                ],
                &mut obj,
            )?;
            Ok(obj)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_listing_i18n_and_refuses_invalid_entries() {
        let slots = vec!["crm.read".to_string()];
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
tagline: Track customers cleanly.
i18n:
  zh-TW:
    title: 客戶管理
    tagline: 輕鬆管理客戶關係與商機
    summary: 一站式管理客戶、分眾名單與行銷活動
    about: 為中小企業打造的客戶關係工具
    can:
      - 建立與維護客戶檔案
    access_notes:
      crm.read: 讀取工作區客戶資料
  zh-HK:
    title: 客戶管理
"#,
        )
        .unwrap();
        let parsed = parse(&yaml, 512, &slots).unwrap();
        assert_eq!(parsed.value["i18n"]["zh-TW"]["title"], "客戶管理");
        assert_eq!(
            parsed.value["i18n"]["zh-TW"]["access_notes"]["crm.read"],
            "讀取工作區客戶資料"
        );
        assert_eq!(parsed.value["i18n"]["zh-HK"]["title"], "客戶管理");

        for bad in [
            "i18n: {}",
            "i18n:\n  zh_TW:\n    title: 客戶管理",
            "i18n:\n  zh-tw:\n    title: A\n  zh-TW:\n    title: B",
            "i18n:\n  zh-TW: {}",
            "i18n:\n  zh-TW:\n    unknown: 1",
            "i18n:\n  zh-TW:\n    access_notes:\n      missing.slot: x",
            "i18n:\n  zh-TW:\n    title: \"line1\\nline2\"",
            "i18n:\n  zh-TW:\n    summary: \"line1\\nline2\"",
            "i18n:\n  zh-TW:\n    title: \"bad\\u2028line\"",
        ] {
            let v: serde_yaml::Value = serde_yaml::from_str(bad).unwrap();
            assert!(parse(&v, 256, &slots).is_err(), "{bad}");
        }

        let long_summary = format!("i18n:\n  zh-TW:\n    summary: {}", "x".repeat(161));
        let v: serde_yaml::Value = serde_yaml::from_str(&long_summary).unwrap();
        assert!(parse(&v, 512, &slots).is_err(), "summary > 160");

        let ok_multiline_about = "i18n:\n  zh-TW:\n    about: \"第一段\\n第二段\"";
        let v: serde_yaml::Value = serde_yaml::from_str(ok_multiline_about).unwrap();
        assert_eq!(
            parse(&v, 512, &slots).unwrap().value["i18n"]["zh-TW"]["about"],
            "第一段\n第二段"
        );

        let emoji_ok = format!("i18n:\n  zh-TW:\n    title: {}", "😀".repeat(40));
        let v: serde_yaml::Value = serde_yaml::from_str(&emoji_ok).unwrap();
        assert!(
            parse(&v, 512, &slots).is_ok(),
            "40 astral emoji = 80 UTF-16 units"
        );
        let emoji_over = format!("i18n:\n  zh-TW:\n    title: {}", "😀".repeat(41));
        let v: serde_yaml::Value = serde_yaml::from_str(&emoji_over).unwrap();
        assert!(
            parse(&v, 512, &slots).is_err(),
            "41 astral emoji = 82 UTF-16 units > 80"
        );

        let mut too_many = String::from("i18n:\n");
        for i in 0..9 {
            too_many.push_str(&format!("  en-x{i}:\n    title: T{i}\n"));
        }
        let v: serde_yaml::Value = serde_yaml::from_str(&too_many).unwrap();
        assert!(parse(&v, 512, &slots).is_err());
    }

    #[test]
    fn parses_manifest_top_level_i18n() {
        let v: serde_yaml::Value = serde_yaml::from_str(
            r#"
zh-TW:
  title: 會議紀錄
  summary: 自動整理重點與待辦事項
"#,
        )
        .unwrap();
        let parsed = parse_manifest_i18n(&v).unwrap();
        assert_eq!(parsed["zh-TW"]["title"], "會議紀錄");

        for bad in [
            "zh-TW:\n  about: Not allowed on top-level i18n",
            "zh-TW:\n  title: '<b>bad</b>'",
        ] {
            let v: serde_yaml::Value = serde_yaml::from_str(bad).unwrap();
            assert!(parse_manifest_i18n(&v).is_err(), "{bad}");
        }
    }
}
