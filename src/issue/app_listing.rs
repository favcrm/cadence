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

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::model;

mod i18n;
mod validate;

use i18n::parse_listing_i18n;
pub(crate) use i18n::parse_manifest_i18n;
pub(crate) use validate::refuse_price_keys;
use validate::{asset_path, known_keys, plain_multiline_text, plain_text, seq, yget};

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

/// One parsed `listing` block — plain JSON value, already checked,
/// plus the asset paths it references (the caller proves each exists
/// in the bundle's own file inventory).
#[derive(Clone, Debug)]
pub struct Listing {
    pub value: Value,
    pub assets: Vec<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_listing_i18n_and_refuses_invalid_entries() {
        let slots = vec!["crm.read".to_string()];
        let y = |s: &str| serde_yaml::from_str::<serde_yaml::Value>(s).unwrap();
        let p = parse(
            &y("{tagline: T., i18n: {zh-TW: {title: 客, tagline: 輕, summary: 總, about: 介, can: [維], access_notes: {crm.read: 讀}}, zh-HK: {title: 客}}}"),
            512,
            &slots,
        )
        .unwrap();
        assert_eq!(p.value["i18n"]["zh-TW"]["title"], "客");
        assert_eq!(p.value["i18n"]["zh-TW"]["access_notes"]["crm.read"], "讀");
        assert_eq!(p.value["i18n"]["zh-HK"]["title"], "客");

        for bad in [
            "{}",
            "{zh_TW: {title: A}}",
            "{zh-tw: {title: A}, zh-TW: {title: B}}",
            "{zh-TW: {}}",
            "{zh-TW: {unknown: 1}}",
            "{zh-TW: {access_notes: {missing.slot: x}}}",
            "{zh-TW: {title: \"客\", access_notes: {}}}",
            "{zh-TW: {title: \"a\\nb\"}}",
            "{zh-TW: {title: \"a\\u2028b\"}}",
        ] {
            assert!(
                parse(&y(&format!("{{i18n: {bad}}}")), 256, &slots).is_err(),
                "{bad}"
            );
        }

        let ml = parse(
            &y("{i18n: {zh-TW: {summary: \"a\\nb\", about: \"c\\nd\"}}}"),
            512,
            &slots,
        )
        .unwrap();
        assert_eq!(
            (
                &ml.value["i18n"]["zh-TW"]["summary"],
                &ml.value["i18n"]["zh-TW"]["about"]
            ),
            (&json!("a\nb"), &json!("c\nd"))
        );

        for (k, ch, n, ok) in [
            ("summary", "x", 280, true),
            ("summary", "x", 281, false),
            ("title", "😀", 40, true),
            ("title", "😀", 41, false),
        ] {
            assert_eq!(
                parse(
                    &y(&format!("{{i18n: {{zh-TW: {{{k}: {}}}}}}}", ch.repeat(n))),
                    512,
                    &slots
                )
                .is_ok(),
                ok,
                "{k}:{n}"
            );
        }

        let nine: String = (0..9)
            .map(|i| format!("en-x{i}: {{title: T{i}}}"))
            .collect::<Vec<_>>()
            .join(", ");
        assert!(parse(&y(&format!("{{i18n: {{{nine}}}}}")), 512, &slots).is_err());
    }

    #[test]
    fn parses_manifest_top_level_i18n() {
        let y = |s: &str| serde_yaml::from_str::<serde_yaml::Value>(s).unwrap();
        assert_eq!(
            parse_manifest_i18n(&y("{zh-TW: {title: 客, summary: 總}}")).unwrap()["zh-TW"]["title"],
            "客"
        );
        for bad in ["{zh-TW: {about: x}}", "{zh-TW: {title: '<b>bad</b>'}}"] {
            assert!(parse_manifest_i18n(&y(bad)).is_err(), "{bad}");
        }
    }
}
