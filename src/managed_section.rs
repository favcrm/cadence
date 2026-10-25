//! One `<!-- cadence:managed-begin M --> … <!-- cadence:managed-end M -->`
//! block inside an agent file.
//!
//! The writer replaces only that block (or appends it) and keeps every other
//! byte of the operator's file. The reader returns the scalar on the block's
//! first non-empty line — the text after its first `:` — so a value is never
//! found by scanning arbitrary prose. A half-open, duplicated or label-less
//! block is an error: a malformed profile is surfaced, never read as unset.

use crate::error::{Error, Result};

/// Escaping can double an existing 4 KiB raw bootstrap field. Keep those
/// persisted values readable without truncation or a profile migration.
pub const VALUE_MAX: usize = 2 * 4_096;

/// Values the seeded templates use to mean "nothing agreed yet".
pub const UNSET_PLACEHOLDERS: [&str; 2] = ["(not set yet)", "(none recorded yet)"];

fn begin(marker: &str) -> String {
    format!("<!-- cadence:managed-begin {marker} -->")
}

fn end(marker: &str) -> String {
    format!("<!-- cadence:managed-end {marker} -->")
}

fn malformed(marker: &str, why: &str) -> Error {
    Error::rejected(format!(
        "managed section '{marker}' is malformed: {why}; fix the file before it is used"
    ))
}

/// One value line per field: control characters removed and any HTML comment
/// opener neutralised, so a value can never open or close a section or add a
/// second line.
pub fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .replace("<!--", "&lt;!--")
        .replace("-->", "--&gt;")
        .trim()
        .to_string()
}

/// Replace one managed block (or append it), keeping all other text. A block
/// that opens without closing refuses: appending would leave two begins.
pub fn set(text: &str, marker: &str, body: &str) -> Result<String> {
    let (open, close) = (begin(marker), end(marker));
    match (text.find(&open), text.find(&close)) {
        (Some(b), Some(e)) if e > b => {
            if text[b + open.len()..].contains(open.as_str()) {
                return Err(malformed(marker, "duplicate begin marker"));
            }
            if text[e + close.len()..].contains(close.as_str()) {
                return Err(malformed(marker, "duplicate end marker"));
            }
            let block = format!("{open}\n{body}\n{close}");
            let after = e + close.len();
            Ok(format!("{}{}{}", &text[..b], block, &text[after..]))
        }
        (None, None) => {
            let block = format!("{open}\n{body}\n{close}");
            let mut out = text.to_string();
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
            out.push_str(&block);
            out.push('\n');
            Ok(out)
        }
        (Some(_), None) => Err(malformed(marker, "no end marker")),
        (None, Some(_)) => Err(malformed(marker, "end marker without a begin marker")),
        (Some(_), Some(_)) => Err(malformed(marker, "end marker precedes the begin marker")),
    }
}

/// The raw body of one managed block; `None` only when the block is absent.
pub fn body<'a>(text: &'a str, marker: &str) -> Result<Option<&'a str>> {
    let (open, close) = (begin(marker), end(marker));
    match (text.find(&open), text.find(&close)) {
        (None, None) => Ok(None),
        (Some(b), Some(e)) if e > b => {
            if text[b + open.len()..].contains(open.as_str()) {
                return Err(malformed(marker, "duplicate begin marker"));
            }
            if text[e + close.len()..].contains(close.as_str()) {
                return Err(malformed(marker, "duplicate end marker"));
            }
            Ok(Some(&text[b + open.len()..e]))
        }
        (Some(_), None) => Err(malformed(marker, "no end marker")),
        (None, Some(_)) => Err(malformed(marker, "end marker without a begin marker")),
        (Some(_), Some(_)) => Err(malformed(marker, "end marker precedes the begin marker")),
    }
}

/// The scalar in one managed block: the text after the first `:` on the first
/// non-empty line. `None` when the block is absent, its value is empty, or it
/// carries a seeded "not set yet" placeholder.
pub fn value(text: &str, marker: &str) -> Result<Option<String>> {
    let Some(body) = body(text, marker)? else {
        return Ok(None);
    };
    let line = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| malformed(marker, "the block has no value line"))?;
    let (_, raw) = line
        .split_once(':')
        .ok_or_else(|| malformed(marker, "the value line has no ':'"))?;
    let value = raw.trim();
    if value.is_empty() || UNSET_PLACEHOLDERS.contains(&value) {
        return Ok(None);
    }
    if value.len() > VALUE_MAX {
        return Err(malformed(marker, "the value is over the cap"));
    }
    Ok(Some(value.to_string()))
}

/// The line a block writes for one scalar; an empty value writes the empty
/// value line, which reads back as unset.
pub fn line(label: &str, value: &str) -> String {
    format!("{label}: {}", clean(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_replaces_only_the_block() {
        let before = "# Identity\n\n<!-- cadence:managed-begin name -->\nName: (not set yet)\n\
                      <!-- cadence:managed-end name -->\n\nOperator prose.\n";
        let after = set(before, "name", &line("name", "Nova")).unwrap();
        assert!(after.starts_with("# Identity\n\n"));
        assert!(after.ends_with("Operator prose.\n"));
        assert_eq!(value(&after, "name").unwrap().as_deref(), Some("Nova"));
    }

    #[test]
    fn absent_block_reads_unset_and_append_round_trips() {
        let text = "# Soul\n\nBe kind.\n";
        assert_eq!(value(text, "tone").unwrap(), None);
        let next = set(text, "tone", &line("tone", "plain")).unwrap();
        assert!(next.contains("Be kind."));
        assert_eq!(value(&next, "tone").unwrap().as_deref(), Some("plain"));
    }

    #[test]
    fn placeholders_and_empty_values_read_unset() {
        let text = "<!-- cadence:managed-begin name -->\nName: (not set yet)\n\
                    <!-- cadence:managed-end name -->\n";
        assert_eq!(value(text, "name").unwrap(), None);
        let cleared = set(text, "name", &line("name", "")).unwrap();
        assert_eq!(value(&cleared, "name").unwrap(), None);
    }

    #[test]
    fn malformed_blocks_refuse_instead_of_guessing() {
        for text in [
            "<!-- cadence:managed-begin name -->\nName: Nova\n",
            "Name: Nova\n<!-- cadence:managed-end name -->\n",
            "<!-- cadence:managed-end name -->x<!-- cadence:managed-begin name -->",
            "<!-- cadence:managed-begin name -->a<!-- cadence:managed-end name -->\
             <!-- cadence:managed-begin name -->b<!-- cadence:managed-end name -->",
            "<!-- cadence:managed-begin name -->Name: Nova<!-- cadence:managed-end name -->\
             <!-- cadence:managed-end name -->",
        ] {
            assert!(value(text, "name").is_err(), "{text:?} was not refused");
        }
    }

    #[test]
    fn a_value_cannot_break_out_of_its_line() {
        let next = set("# V\n", "name", &line("name", "No<!--va\nme")).unwrap();
        assert_eq!(
            value(&next, "name").unwrap().as_deref(),
            Some("No&lt;!--vame")
        );
    }
}
