//! The single-line task acceptance format shared by dispatch, review, and Store.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use crate::issue::parse::AcceptanceItem;

/// The full acceptance list on one line, or `None` when there are no items.
///
/// CAD-300: each item is numbered and its text is a JSON string
/// literal — `1) [ ] "a"; 2) [x] "b"` — so text that looks like the
/// listing's own syntax (`; `, `[x]`, `2)`) stays inside its quotes
/// and cannot read as another item or a checked box.
/// [`parse_acceptance_listing`] reads it back exactly. Quoting escapes
/// every control character and U+2028/U+2029, so the result can ride a
/// single-line pty kickoff without losing them.
pub fn acceptance_listing(items: &[AcceptanceItem]) -> Option<String> {
    (!items.is_empty()).then(|| inline_listing(items))
}

/// The still-unchecked criteria of a stored task, renumbered from one.
pub(crate) fn outstanding_listing(acceptance: Option<&str>) -> Option<String> {
    acceptance_listing(&outstanding_items(acceptance))
}

fn inline_listing(items: &[AcceptanceItem]) -> String {
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let mark = if item.checked { 'x' } else { ' ' };
            format!("{}) [{mark}] {}", i + 1, quote_item(&item.text))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// CAD-160: a whole CAD-300 listing yields its `[ ]` items. Any other
/// non-blank text (a free-form `--accept`) is one unchecked item.
fn outstanding_items(acceptance: Option<&str>) -> Vec<AcceptanceItem> {
    let Some(text) = acceptance.map(str::trim).filter(|t| !t.is_empty()) else {
        return vec![];
    };
    match parse_acceptance_listing(text) {
        Some((items, "")) => items.into_iter().filter(|i| !i.checked).collect(),
        _ => vec![AcceptanceItem {
            text: text.to_string(),
            checked: false,
        }],
    }
}

/// `text` as a JSON string literal that is also safe on one pty line:
/// `"` and `\` are escaped as JSON requires, tab as `\t`, and every
/// other control character (C0, DEL, C1) and the line/paragraph
/// separators U+2028/U+2029 as `\uXXXX`. All of those are in the BMP,
/// so four hex digits always suffice.
fn quote_item(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') => {
                out.push_str(&format!("\\u{:04x}", u32::from(c)));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// CAD-300: read the numbered, JSON-quoted items at the start of `s`,
/// returning them and the unconsumed suffix (e.g. the kickoff's `.`).
/// Returns `None` if `s` does not start at `1)` or an expected item has
/// an invalid checkbox marker or JSON string literal.
pub(crate) fn parse_acceptance_listing(s: &str) -> Option<(Vec<AcceptanceItem>, &str)> {
    let mut items = vec![];
    let mut rest = s;
    loop {
        rest = rest.strip_prefix(&format!("{}) ", items.len() + 1))?;
        let checked = match rest.get(..4)? {
            "[x] " => true,
            "[ ] " => false,
            _ => return None,
        };
        rest = &rest[4..];
        if !rest.starts_with('"') {
            return None;
        }
        let mut quoted = serde_json::Deserializer::from_str(rest).into_iter::<String>();
        let text = quoted.next()?.ok()?;
        rest = &rest[quoted.byte_offset()..];
        items.push(AcceptanceItem { text, checked });
        match rest.strip_prefix("; ") {
            Some(next) if next.starts_with(&format!("{}) ", items.len() + 1)) => rest = next,
            _ => return Some((items, rest)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CAD-160: the outstanding criteria of a stored task acceptance.
    #[test]
    fn outstanding_items_keep_only_unchecked_criteria() {
        let items = vec![
            AcceptanceItem {
                text: "a; 2) [ ] \"b\"".into(),
                checked: false,
            },
            AcceptanceItem {
                text: "done".into(),
                checked: true,
            },
        ];
        let listing = acceptance_listing(&items).unwrap();
        assert_eq!(outstanding_items(Some(&listing)), items[..1].to_vec());
        // Free-form text, or a listing with trailing prose, is one item.
        for text in ["green tests", &format!("{listing} and more")] {
            assert_eq!(
                outstanding_items(Some(text)),
                vec![AcceptanceItem {
                    text: text.to_string(),
                    checked: false
                }]
            );
        }
        assert!(outstanding_items(None).is_empty());
        assert!(outstanding_items(Some("  ")).is_empty());
    }
}
