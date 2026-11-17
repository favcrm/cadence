//! CAD-1129 H9: capability → plain-language sentence, computed by the
//! host — never by the bundle. The detail page's "What it can access"
//! and each Explorer card read from this table, so the same contract
//! reads the same way everywhere and a bundle cannot describe its own
//! powers in kinder words.
//!
//! One row per declared `(capability, effect)`:
//! `text.publish/send` → "Can post for you, only when you approve"
//! (warn, chip "Can post for you"); unknown capabilities fall back by
//! effect — read < draft < send — toward the stronger warning.
//! `listing.access_notes[slot]` is appended under the host sentence,
//! never replaces it. No price or credit wording (decision 8).

use serde_json::{json, Value};

use crate::issue::app::Manifest;

/// A row's level: `info` is neutral, `warn` names an outward power.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Info,
    Warn,
}

/// The chip a row carries, when it carries one: the two- or three-word
/// power ("Can post for you", "Uses AI", "Personal data").
fn chip(level: Level, text: &str) -> Value {
    match level {
        Level::Warn => json!({"level": "warn", "text": text}),
        Level::Info => json!({"level": "info", "text": text}),
    }
}

/// One access row: icon hint, title, sentence, and an optional chip.
fn row(
    icon: &str,
    title: &str,
    sentence: &str,
    chip_value: Option<Value>,
    note: Option<&str>,
) -> Value {
    let mut value = json!({
        "icon": icon,
        "title": title,
        "sentence": sentence,
    });
    if let Some(chip_value) = chip_value {
        value["chip"] = chip_value;
    }
    if let Some(note) = note {
        value["note"] = json!(note);
    }
    value
}

/// Map one declared `(capability, effect)` slot to its plain row.
/// Unknown capabilities fall back by effect, toward the stronger
/// warning — an unrecognized `send` reads "Can send or publish for
/// you", never nothing.
fn capability_row(slot: &str, capability: &str, effect: &str, note: Option<&str>) -> Value {
    let (icon, title) = match capability {
        "text.publish" => ("send", "Posting and sending"),
        "social.read" => ("insta", "Your social account"),
        "media.generate" => ("spark", "AI images"),
        "crm.read" => ("people", "Your customer list"),
        "calendar.read" | "calendar.write" => ("cal", "Your calendar"),
        "reviews.reply" => ("chat2", "Your reviews profile"),
        _ => match effect {
            "send" => ("send", "Outside actions"),
            "draft" => ("spark", "AI drafting"),
            _ => ("db", "Reading"),
        },
    };
    let (sentence, chip_value) = match (capability, effect) {
        ("text.publish", "send") => (
            "Can post for you, only when you approve",
            Some(chip(Level::Warn, "Can post for you")),
        ),
        ("social.read", "read") => ("Reads posts from the account you connect", None),
        ("media.generate", "draft") => (
            "Makes pictures with AI when you ask",
            Some(chip(Level::Info, "Uses AI")),
        ),
        ("crm.read", "read") => (
            "Reads your customer list, only while it's on",
            Some(chip(Level::Warn, "Personal data")),
        ),
        ("calendar.write", "draft") => (
            "Adds bookings to your calendar when you ask",
            Some(chip(Level::Warn, "Can edit")),
        ),
        ("calendar.read", "read") => ("Reads your free/busy times", None),
        ("reviews.reply", "send") => (
            "Posts replies, only when you approve",
            Some(chip(Level::Warn, "Can post for you")),
        ),
        (_, "send") => (
            "Can send or publish for you",
            Some(chip(Level::Warn, "Can send for you")),
        ),
        (_, "draft") => (
            "Drafts with AI when you ask",
            Some(chip(Level::Info, "Uses AI")),
        ),
        _ => ("Reads data from the account you connect", None),
    };
    let title = if title == "Outside actions" || title == "AI drafting" || title == "Reading" {
        format!("{title} · {slot}")
    } else {
        title.to_string()
    };
    row(icon, &title, sentence, chip_value, note)
}

/// The "never" line: what the app CANNOT do, from what's absent.
/// Absent a `send` effect: "Can't post or send anything". Personal
/// data absence is stated per-access, not here.
fn never_line(manifest: &Manifest, dated_reads: bool) -> String {
    let sends = manifest
        .capabilities
        .values()
        .any(|need| need.effect == "send");
    let drafts = manifest
        .capabilities
        .values()
        .any(|need| need.effect == "draft");
    let mut clauses: Vec<String> = Vec::new();
    if !sends {
        clauses.push("can't post or send anything".to_string());
    }
    if !drafts && !sends {
        clauses.push("can't act outside this workspace".to_string());
    }
    // CAD-2379: an app that reads dates across apps no longer claims it
    // cannot see other apps' data; its own row says what it does see.
    if !dated_reads {
        clauses.push("can't see other apps' data".to_string());
    }
    clauses.push("your logins stay with Cadence; the app never sees a password".to_string());
    let mut line = String::new();
    for (index, clause) in clauses.iter().enumerate() {
        if index == 0 {
            line.push_str(&capitalize(clause));
        } else if index == clauses.len() - 1 {
            line.push_str(&format!("; and {clause}."));
        } else {
            line.push_str(&format!("; {clause}"));
        }
    }
    line
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// The whole "What it can access" projection for one manifest: a row
/// per declared capability slot, a row per legacy connection slot
/// (untyped, still host-worded), a host-facts row when the app holds
/// records, a dates row when the app declares `calendar.query`
/// (`dated_reads`, computed by the host from its screen packages), then the
/// never-line. `notes` is `listing.access_notes` keyed by slot — appended
/// under the host sentence, never replacing.
pub fn access_rows(
    manifest: &Manifest,
    notes: Option<&serde_json::Map<String, Value>>,
    holds_records: bool,
    personal: bool,
    dated_reads: bool,
) -> Value {
    let mut rows = Vec::new();
    for (slot, need) in &manifest.capabilities {
        let note = notes.and_then(|n| n.get(slot)).and_then(|v| v.as_str());
        rows.push(capability_row(slot, &need.capability, &need.effect, note));
    }
    for slot in &manifest.connections {
        let note = notes.and_then(|n| n.get(slot)).and_then(|v| v.as_str());
        rows.push(row(
            "plug",
            &format!("Connection · {slot}"),
            "Uses the account you connect for it",
            None,
            note,
        ));
    }
    if holds_records || personal {
        rows.push(row(
            "db",
            "Its own data",
            "Kept in this workspace only",
            personal.then(|| chip(Level::Warn, "Personal data")),
            None,
        ));
    }
    if dated_reads {
        rows.push(row(
            "cal",
            "Dates across your apps",
            "Sees dates from your apps and work: titles, times and status only",
            Some(chip(Level::Warn, "Reads across apps")),
            None,
        ));
    }
    json!({
        "access": rows,
        "never": never_line(manifest, dated_reads),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        crate::issue::app::parse_manifest(
            "---\napp: demo\ntitle: Demo\nversion: '0.1.0'\nneeds:\n  connections: []\nlisting:\n  tagline: t\n---\n\nbody\n",
        )
        .unwrap()
    }

    /// CAD-2379: an app that declares the dated read shows the host's row and
    /// no never-line that contradicts it; any other app keeps both as before.
    #[test]
    fn a_dated_read_adds_its_row_and_drops_the_contradicting_never_line() {
        let with = access_rows(&manifest(), None, false, false, true);
        let rows = with["access"].as_array().unwrap();
        let row = rows
            .iter()
            .find(|r| r["title"] == "Dates across your apps")
            .expect("the dates row");
        assert_eq!(
            row["sentence"],
            "Sees dates from your apps and work: titles, times and status only"
        );
        assert_eq!(row["chip"]["level"], "warn");
        assert_eq!(row["chip"]["text"], "Reads across apps");
        let never = with["never"].as_str().unwrap();
        assert!(!never.contains("other apps"), "{never}");
        assert!(never.contains("your logins stay with Cadence"), "{never}");

        let without = access_rows(&manifest(), None, false, false, false);
        assert!(!without["access"].to_string().contains("Dates across"));
        assert!(without["never"]
            .as_str()
            .unwrap()
            .contains("can't see other apps' data"));
    }
}
