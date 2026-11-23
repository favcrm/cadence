//! Idea prompt rendering, structured plan parsing, and title deduplication.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use super::{Proposed, PROMPT_CHARS};

pub fn one_line(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut space = false;
    for ch in text.chars() {
        let ch = if ch.is_control() { ' ' } else { ch };
        if ch == ' ' {
            if space || out.is_empty() {
                continue;
            }
            space = true;
        } else {
            space = false;
        }
        out.push(ch);
        if out.chars().count() >= max_chars {
            break;
        }
    }
    out.trim().to_string()
}

pub fn research_prompt(issue: &str, title: &str, body: &str) -> String {
    format!(
        "ROLE:researcher Issue {issue}. Research this idea and reply with one note, no code. \
         Cover what already exists in this repo, prior art outside it, feasibility, rough cost \
         and operational burden, and the main risk. Title: {title}. Body: {body}",
        title = one_line(title, 200),
        body = one_line(body, PROMPT_CHARS),
    )
}

pub fn plan_prompt(issue: &str, title: &str, body: &str, research: &str) -> String {
    format!(
        "ROLE:architect Issue {issue}. Write a plan with at least three options including \
         do-nothing and the smallest useful thing, a recommendation, acceptance checks a \
         reviewer could run, and a proposed ticket breakdown. No code, no pull request, and \
         do not dispatch a developer. Use a single line with the fields options, \
         recommendation, acceptance, and tickets. Separate options with |. Separate tickets \
         with || and use a title, then an em dash, then the acceptance. Title: {title}. \
         Idea: {body}. Research: {research}",
        title = one_line(title, 200),
        body = one_line(body, PROMPT_CHARS),
        research = one_line(research, PROMPT_CHARS),
    )
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedPlan {
    pub recommendation: String,
    pub tickets: Vec<Proposed>,
}

/// The architect's reply. A template echo (`<third>`, `<title>`) is
/// not a plan. `do-nothing` and a smallest-useful option are required.
pub fn parse_plan(text: &str) -> std::result::Result<ParsedPlan, String> {
    let lower = text.to_ascii_lowercase();
    let options = field(&lower, text, "options:", &["recommendation:"])
        .ok_or_else(|| "plan is missing the options field".to_string())?;
    let recommendation = field(&lower, text, "recommendation:", &["acceptance:"])
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "plan is missing a recommendation".to_string())?;
    let acceptance = field(&lower, text, "acceptance:", &["tickets:"])
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "plan is missing acceptance checks".to_string())?;
    let tickets_raw = field(&lower, text, "tickets:", &[])
        .ok_or_else(|| "plan is missing the tickets field".to_string())?;
    let opts: Vec<String> = options
        .split('|')
        .map(normalize_option)
        .filter(|s| !s.is_empty())
        .collect();
    if opts.len() < 3 {
        return Err("plan needs at least three options".into());
    }
    if opts.iter().any(|o| o.contains('<')) {
        return Err("plan options still contain a template placeholder".into());
    }
    if !opts.iter().any(|o| o == "do-nothing" || o == "donothing") {
        return Err("plan options must include do-nothing".into());
    }
    if !opts.iter().any(|o| o.contains("smallest")) {
        return Err("plan options must include the smallest useful thing".into());
    }
    let mut tickets = Vec::new();
    for part in tickets_raw.split("||") {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (title, acc) = part
            .split_once(" — ")
            .or_else(|| part.split_once(" -- "))
            .ok_or_else(|| format!("ticket {part:?} needs a title and an acceptance"))?;
        let title = title.trim();
        let acc = acc.trim();
        if title.is_empty() || acc.is_empty() || title.contains('<') || acc.contains('<') {
            return Err("a proposed ticket still contains a placeholder".into());
        }
        tickets.push(Proposed {
            title: one_line(title, 120),
            acceptance: one_line(acc, 400),
        });
    }
    if tickets.is_empty() {
        return Err("plan lists no proposed tickets".into());
    }
    let _ = acceptance;
    Ok(ParsedPlan {
        recommendation: one_line(recommendation, 200),
        tickets,
    })
}

fn field<'a>(lower: &str, raw: &'a str, key: &str, next: &[&str]) -> Option<&'a str> {
    let start = lower.find(key)? + key.len();
    let rest = &lower[start..];
    let end = next
        .iter()
        .filter_map(|n| rest.find(n))
        .min()
        .unwrap_or(rest.len());
    Some(raw[start..start + end].trim())
}

fn normalize_option(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.trim().chars() {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Open issues whose title is the same, contains the other (when the
/// shorter is at least 12 characters), or shares most of its words.
pub fn near_duplicate(title: &str, open: &[(&str, &str)]) -> Option<String> {
    let mine = normalize_title(title);
    if mine.is_empty() {
        return None;
    }
    let mine_tokens = tokens(&mine);
    for (id, other) in open {
        let theirs = normalize_title(other);
        if theirs.is_empty() {
            continue;
        }
        if mine == theirs {
            return Some((*id).to_string());
        }
        let (short, long) = if mine.len() <= theirs.len() {
            (mine.as_str(), theirs.as_str())
        } else {
            (theirs.as_str(), mine.as_str())
        };
        if short.len() >= 12 && long.contains(short) {
            return Some((*id).to_string());
        }
        let other_tokens = tokens(&theirs);
        if jaccard(&mine_tokens, &other_tokens) >= 0.8 {
            return Some((*id).to_string());
        }
    }
    None
}

fn normalize_title(title: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            space = false;
        } else if !space && !out.is_empty() {
            out.push(' ');
            space = true;
        }
    }
    out.trim().to_string()
}

fn tokens(title: &str) -> Vec<String> {
    title
        .split_whitespace()
        .filter(|w| w.len() >= 3)
        .map(str::to_string)
        .collect()
}

fn jaccard(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.iter().filter(|t| b.contains(t)).count();
    let mut union: Vec<&String> = a.iter().collect();
    for t in b {
        if !union.contains(&t) {
            union.push(t);
        }
    }
    inter as f64 / union.len() as f64
}
