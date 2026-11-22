//! Localized `listing.i18n` and manifest `i18n` parsing for `issue::app_listing`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use serde_json::{json, Value};

use crate::error::{Error, Result};

use super::{known_keys, plain_multiline_text, plain_text, seq, yget};

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

pub(super) fn parse_listing_i18n(value: &serde_yaml::Value, slots: &[String]) -> Result<Value> {
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
                ("summary", "i18n.summary", 280, true),
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
                if notes.is_empty() {
                    return Err(Error::rejected(format!(
                        "listing i18n.{locale}.access_notes must not be empty"
                    )));
                }
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
                    ("summary", "i18n.summary", 280, true),
                    ("tagline", "i18n.tagline", 80, false),
                ],
                &mut obj,
            )?;
            Ok(obj)
        },
    )
}
