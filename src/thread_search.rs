//! CAD-2663: an agent searches its own earlier conversation.
//!
//! The continuity pack keeps the last turns word for word and one line per
//! older turn, so an exact number, decision or name from an older turn is
//! gone after a compaction. This is the lookup: an exact phrase first, then
//! entries ranked by how many of the query's words they contain, then a few
//! entries either side of a hit.
//!
//! This module is pure: query grammar, scoring, snippets and the byte caps.
//! The daemon (`daemon/thread_search_rpc.rs`) decides which entries the
//! caller may see; nothing here widens that.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use crate::error::{Error, Result};

/// Longest query, in bytes.
pub const QUERY_MAX: usize = 200;
/// Most distinct words a query may hold.
pub const WORDS_MAX: usize = 6;
/// Default and largest number of hits one search returns.
pub const HITS_DEFAULT: usize = 5;
pub const HITS_MAX: usize = 10;
/// Visible entries one search reads, newest first. Older ones are reported
/// as not searched rather than silently skipped.
pub const SCAN_MAX: usize = 4_000;
/// Longest snippet around a hit, in bytes.
pub const SNIPPET_MAX: usize = 400;
/// All snippets of one answer together, in bytes.
pub const HITS_BYTES_MAX: usize = 4_000;
/// Default and largest number of entries read on each side of a hit.
pub const SIDE_DEFAULT: usize = 3;
pub const SIDE_MAX: usize = 6;
/// Longest entry text in a window, in bytes.
pub const WINDOW_ENTRY_MAX: usize = 1_500;
/// All entry texts of one window together, in bytes.
pub const WINDOW_BYTES_MAX: usize = 8_000;

/// A validated query: the whole phrase and its distinct words, lowercased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub phrase: String,
    pub words: Vec<String>,
}

impl Query {
    pub fn parse(raw: &str) -> Result<Self> {
        if raw.len() > QUERY_MAX || raw.chars().any(char::is_control) {
            return Err(Error::rejected(format!(
                "thread search query must be at most {QUERY_MAX} bytes with no control characters"
            )));
        }
        let mut words: Vec<String> = Vec::new();
        for word in raw.split_whitespace().map(str::to_lowercase) {
            if !words.contains(&word) {
                words.push(word);
            }
        }
        if words.is_empty() {
            return Err(Error::rejected("thread search needs a query"));
        }
        if words.len() > WORDS_MAX {
            return Err(Error::rejected(format!(
                "thread search takes at most {WORDS_MAX} words; search for the exact phrase or the rarest words"
            )));
        }
        let phrase = raw
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        Ok(Self { phrase, words })
    }

    /// How `text` matches: whether it holds the whole phrase, and how many
    /// distinct words it holds. `None` when it holds none.
    pub fn score(&self, text: &str) -> Option<Score> {
        let haystack = normalized(text);
        let words = self
            .words
            .iter()
            .filter(|word| haystack.contains(word.as_str()))
            .count();
        (words > 0).then(|| Score {
            phrase: self.words.len() > 1 && haystack.contains(&self.phrase),
            words,
        })
    }

    /// Up to [`SNIPPET_MAX`] bytes of `text` around its first match.
    pub fn snippet(&self, text: &str) -> String {
        // Lowercase with a map from each lowercased byte back to the start
        // of the original character, so an offset survives characters whose
        // lowercase form has another byte length.
        let mut lower = String::with_capacity(text.len());
        let mut origin = Vec::with_capacity(text.len());
        for (at, ch) in text.char_indices() {
            for low in ch.to_lowercase() {
                lower.push(low);
                origin.resize(lower.len(), at);
            }
        }
        let at = std::iter::once(self.phrase.as_str())
            .chain(self.words.iter().map(String::as_str))
            .find_map(|needle| lower.find(needle))
            .and_then(|at| origin.get(at).copied())
            .unwrap_or(0);
        window(text, at, SNIPPET_MAX)
    }
}

/// How well one entry matches. Orders phrase hits first, then by words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Score {
    pub phrase: bool,
    pub words: usize,
}

/// Lowercased text with whitespace runs collapsed, so a phrase split across
/// a line break still matches.
fn normalized(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// `max` bytes of `text` starting a little before `at`, cut on char
/// boundaries and marked with an ellipsis where it was cut.
fn window(text: &str, at: usize, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let lead = max / 4;
    let mut start = at.saturating_sub(lead);
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (start + max).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end - start + 8);
    if start > 0 {
        out.push('…');
    }
    out.push_str(&text[start..end]);
    if end < text.len() {
        out.push('…');
    }
    out
}

/// The `thread_search` request both the CLI and the MCP tool send: only the
/// fields given, and never an agent, thread or conversation. The daemon
/// validates every value.
pub fn request(
    query: Option<&str>,
    around: Option<&str>,
    limit: Option<u64>,
    before: Option<u64>,
    after: Option<u64>,
) -> serde_json::Value {
    let mut params = serde_json::Map::new();
    let mut put = |key: &str, value: serde_json::Value| {
        params.insert(key.to_string(), value);
    };
    if let Some(query) = query {
        put("query", query.into());
    }
    if let Some(around) = around {
        put("around", around.into());
    }
    for (key, value) in [("limit", limit), ("before", before), ("after", after)] {
        if let Some(value) = value {
            put(key, value.into());
        }
    }
    serde_json::Value::Object(params)
}

/// At most `max` bytes of `text` from its start, marked where cut.
pub fn clip(text: &str, max: usize) -> String {
    window(text, 0, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_grammar_is_bounded() {
        let q = Query::parse("  Rate   LIMIT  rate ").unwrap();
        assert_eq!(q.phrase, "rate limit rate");
        assert_eq!(q.words, vec!["rate", "limit"]);
        assert!(Query::parse("   ").is_err());
        assert!(Query::parse("a b c d e f g").is_err());
        assert!(Query::parse(&"x".repeat(QUERY_MAX + 1)).is_err());
        assert!(Query::parse("bad\u{7}bell").is_err());
    }

    #[test]
    fn phrase_ranks_above_scattered_words() {
        let q = Query::parse("retry budget 42").unwrap();
        let phrase = q.score("We set the retry budget 42 last week").unwrap();
        let scattered = q.score("budget was 42; retry later").unwrap();
        let one = q.score("the budget").unwrap();
        assert!(phrase.phrase && !scattered.phrase);
        assert!(phrase > scattered && scattered > one);
        assert_eq!(q.score("nothing relevant"), None);
        // A phrase split by a line break still matches.
        assert!(q.score("retry\n  budget 42").unwrap().phrase);
    }

    #[test]
    fn snippets_and_clips_are_bounded_on_char_boundaries() {
        let q = Query::parse("needle").unwrap();
        let text = format!("{}needle{}", "é".repeat(400), "ü".repeat(400));
        let snip = q.snippet(&text);
        assert!(snip.contains("needle"));
        assert!(snip.len() <= SNIPPET_MAX + 2 * '…'.len_utf8());
        assert!(snip.starts_with('…') && snip.ends_with('…'));
        // Where lowercasing changes byte lengths, the match is still shown.
        let q = Query::parse("Needle").unwrap();
        let text = format!("{}Needle{}", "İ".repeat(300), "x".repeat(300));
        assert!(q.snippet(&text).contains("Needle"), "{}", q.snippet(&text));
        let clipped = clip(&"ß".repeat(1000), 101);
        assert!(clipped.len() <= 101 + '…'.len_utf8());
        assert_eq!(clip("short", 100), "short");
    }
}
