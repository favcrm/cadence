//! Pre-publish secret scan (CAD-109).
//!
//! One scanner for every place cadence writes agent-authored text somewhere
//! durable or outward. `issue comment`, `report`, `memory propose` and the
//! intake GitHub relay call [`guard`]: a blocking finding refuses the write
//! with the rule id named and nothing is written. `cadence secret scan` runs
//! the same check on stdin or a file.
//!
//! Rules:
//!
//! - the gitleaks rule pack, vendored and version-pinned in
//!   `src/secret/gitleaks.toml` ([`GITLEAKS_VERSION`], MIT; the licence and
//!   upstream commit are in that file's header). It is RE2 syntax, which the
//!   `regex` crate compiles after one translation: a brace RE2 reads as a
//!   literal is escaped (`re2_braces`). It runs in ASCII mode, which is
//!   RE2's `\w`/`\s`/`\b`.
//! - a cadence-owned prefix set that fires on a bare token with no keyword
//!   nearby (`figd_`, `sk-ant-`, `sk-proj-`, `github_pat_`, `glpat-`, `npm_`,
//!   `dvn_`, `AIza`). Both gitleaks and trufflehog rely on a nearby keyword
//!   for several of these, and a token quoted on its own line defeats that
//!   (CAD-109 research note).
//! - the CAD-108 argv rule: a secret-named flag or `NAME=value` whose value
//!   [`redact_argv`] would redact and which looks random.
//!
//! Speed: gitleaks' own keyword prefilter decides whether a rule runs, and a
//! rule's regex compiles on first use, once per process.
//!
//! Severity: every rule blocks except `generic-api-key`. That rule only warns
//! in this phase because it is the most false-positive-prone rule in the pack.
//!
//! Operator allowlist: `<state dir>/secret-allowlist.toml` (the directory
//! that also holds `intake-relay.yaml`). It is edited by hand, and entries
//! are keyed by rule id and optionally by a finding's `fingerprint`. There is
//! no flag that bypasses the scan. A malformed allowlist or a rule pack that
//! does not load refuses the write: the scan fails closed.
//!
//! A finding never carries the secret. `redacted` is at most a four-character
//! prefix plus `…`. `fingerprint` is the first 16 hex digits of the value's
//! SHA-256, which is enough to allowlist one value without storing it.

use std::path::Path;
use std::sync::OnceLock;

use regex::bytes::{Captures, Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::doctor::host::redact_argv;
use crate::error::{Error, Result};

#[cfg(test)]
mod tests;

/// The pinned upstream release of `gitleaks.toml`.
pub const GITLEAKS_VERSION: &str = "v8.30.1";
/// The upstream commit the pinned tag names.
pub const GITLEAKS_COMMIT: &str = "83d9cd684c87d95d656c1458ef04895a7f1cbd8e";
/// Operator allowlist file name, inside the state dir.
pub const ALLOWLIST_FILE: &str = "secret-allowlist.toml";

const PACK_TOML: &str = include_str!("gitleaks.toml");

/// Rules that only warn in this phase.
const WARN_ONLY: &[&str] = &["generic-api-key"];

/// `generic-api-key`, `pypi-upload-token` and `vault-batch-token` exceed the
/// regex crate's default 10 MB compiled-program limit.
const REGEX_SIZE_LIMIT: usize = 64 << 20;

/// Name words the argv rule requires. They match the CAD-108 rule
/// `(key|token|secret|password|passwd|auth)`, and `redact_argv` still has
/// to agree that the name is secret-bearing.
const ARGV_WORDS: &[&str] = &["key", "token", "secret", "passw", "auth"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Block,
    Warn,
}

/// One credential-shaped span. There is deliberately no field that holds
/// the matched text.
#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    pub rule: String,
    /// 1-based line of the secret's first character.
    pub line: usize,
    /// 1-based character column of the secret's first character.
    pub column: usize,
    /// At most a four-character prefix plus `…`.
    pub redacted: String,
    pub severity: Severity,
    /// First 16 hex digits of SHA-256(secret), for an allowlist entry.
    pub fingerprint: String,
}

// ---------- rule pack ----------

#[derive(Deserialize)]
struct RawPack {
    #[serde(default)]
    allowlist: Option<RawAllow>,
    #[serde(default)]
    rules: Vec<RawRule>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRule {
    id: String,
    #[serde(default)]
    regex: Option<String>,
    #[serde(default)]
    secret_group: usize,
    #[serde(default)]
    entropy: f64,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    allowlists: Vec<RawAllow>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAllow {
    #[serde(default)]
    condition: Option<String>,
    #[serde(default)]
    regex_target: Option<String>,
    #[serde(default)]
    regexes: Vec<String>,
    #[serde(default)]
    stopwords: Vec<String>,
    #[serde(default)]
    paths: Vec<String>,
}

/// RE2 (Go) reads a `{` or `}` that cannot be part of a counted repetition
/// as a literal character. Some gitleaks allowlist patterns rely on this,
/// for example `^\$(?:\d+|{\d+})$`. The `regex` crate rejects such a brace,
/// so escape it. Character classes and escapes are copied unchanged.
fn re2_braces(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len() + 8);
    let mut i = 0;
    let mut class = false;
    // Just after `[` or `[^`, a `]` is a literal member of the class.
    let mut class_start = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            out.push(c);
            if let Some(next) = chars.get(i + 1) {
                out.push(*next);
            }
            i += 2;
            class_start = false;
            continue;
        }
        if class {
            if c == '[' && chars.get(i + 1) == Some(&':') {
                // A POSIX class such as `[:alnum:]`. Copy it through `:]`.
                let rest: String = chars[i..].iter().collect();
                if let Some(end) = rest.find(":]") {
                    out.push_str(&rest[..end + 2]);
                    i += rest[..end + 2].chars().count();
                    class_start = false;
                    continue;
                }
            }
            if c == ']' && !class_start {
                class = false;
            }
            class_start = class_start && c == '^';
            out.push(c);
            i += 1;
            continue;
        }
        match c {
            '[' => {
                class = true;
                class_start = true;
                out.push(c);
            }
            '{' => {
                let rest: String = chars[i + 1..].iter().collect();
                let count = rest.find('}').is_some_and(|end| {
                    let body = &rest[..end];
                    let (lo, hi) = body.split_once(',').unwrap_or((body, "0"));
                    !lo.is_empty()
                        && lo.bytes().all(|b| b.is_ascii_digit())
                        && hi.bytes().all(|b| b.is_ascii_digit())
                });
                let after_atom = i > 0 && !matches!(chars[i - 1], '(' | '|');
                if count && after_atom {
                    let end = rest.find('}').unwrap();
                    out.push('{');
                    out.push_str(&rest[..=end]);
                    i += end + 2;
                    continue;
                }
                out.push_str("\\{");
            }
            '}' => out.push_str("\\}"),
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

fn compile(src: &str) -> std::result::Result<Regex, String> {
    RegexBuilder::new(&re2_braces(src))
        .unicode(false)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|e| e.to_string())
}

/// A pattern that compiles on first use and stays compiled.
struct Lazy {
    src: String,
    re: OnceLock<std::result::Result<Regex, String>>,
}

impl Lazy {
    fn new(src: impl Into<String>) -> Self {
        Self {
            src: src.into(),
            re: OnceLock::new(),
        }
    }

    fn get(&self) -> Result<&Regex> {
        self.re
            .get_or_init(|| compile(&self.src))
            .as_ref()
            .map_err(|e| unavailable(&format!("a rule pattern does not compile ({e})")))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Secret,
    Match,
    Line,
}

/// A gitleaks allowlist: path, regex and stopword checks joined by
/// `condition` (OR by default, AND when set).
struct Allow {
    all: bool,
    target: Target,
    regexes: Vec<Lazy>,
    stopwords: Vec<String>,
    paths: Vec<Lazy>,
}

impl Allow {
    fn from_raw(raw: RawAllow) -> std::result::Result<Self, String> {
        let target = match raw.regex_target.as_deref() {
            None | Some("secret") => Target::Secret,
            Some("match") => Target::Match,
            Some("line") => Target::Line,
            Some(other) => return Err(format!("unknown regexTarget '{other}'")),
        };
        let all = match raw.condition.as_deref() {
            None | Some("OR") => false,
            Some("AND") => true,
            Some(other) => return Err(format!("unknown allowlist condition '{other}'")),
        };
        Ok(Self {
            all,
            target,
            regexes: raw.regexes.into_iter().map(Lazy::new).collect(),
            stopwords: raw
                .stopwords
                .into_iter()
                .map(|w| w.to_ascii_lowercase())
                .collect(),
            paths: raw.paths.into_iter().map(Lazy::new).collect(),
        })
    }

    fn allows(
        &self,
        secret: &[u8],
        matched: &[u8],
        line: &[u8],
        path: Option<&str>,
    ) -> Result<bool> {
        let mut checks = Vec::with_capacity(3);
        if !self.paths.is_empty() {
            let hit = match path {
                Some(p) => any_match(&self.paths, p.as_bytes())?,
                None => false,
            };
            checks.push(hit);
        }
        if !self.regexes.is_empty() {
            let hay = match self.target {
                Target::Secret => secret,
                Target::Match => matched,
                Target::Line => line,
            };
            checks.push(any_match(&self.regexes, hay)?);
        }
        if !self.stopwords.is_empty() {
            let low = String::from_utf8_lossy(secret).to_ascii_lowercase();
            checks.push(self.stopwords.iter().any(|w| low.contains(w.as_str())));
        }
        Ok(if self.all {
            !checks.is_empty() && checks.iter().all(|c| *c)
        } else {
            checks.iter().any(|c| *c)
        })
    }
}

fn any_match(patterns: &[Lazy], hay: &[u8]) -> Result<bool> {
    for p in patterns {
        if p.get()?.is_match(hay) {
            return Ok(true);
        }
    }
    Ok(false)
}

type Confirm = fn(&Captures) -> bool;

struct Rule {
    id: String,
    regex: Option<Lazy>,
    secret_group: usize,
    entropy: f64,
    keywords: Vec<String>,
    path: Option<Lazy>,
    allows: Vec<Allow>,
    /// A cadence-rule check that runs after the regex matches.
    confirm: Option<Confirm>,
}

impl Rule {
    fn severity(&self) -> Severity {
        if WARN_ONLY.contains(&self.id.as_str()) {
            Severity::Warn
        } else {
            Severity::Block
        }
    }
}

struct Pack {
    global: Allow,
    rules: Vec<Rule>,
}

/// A cadence-owned rule. The prefix doubles as the keyword prefilter. That
/// only decides whether the regex runs, so no context keyword is needed.
struct CadenceRule {
    id: &'static str,
    regex: &'static str,
    /// The capture group holding the secret.
    group: usize,
    entropy: f64,
    keywords: &'static [&'static str],
    confirm: Option<Confirm>,
}

const CADENCE_RULES: &[CadenceRule] = &[
    CadenceRule {
        id: "cadence-figma-token",
        regex: r"\b(figd_[A-Za-z0-9_-]{20,})",
        group: 1,
        entropy: 3.0,
        keywords: &["figd_"],
        confirm: None,
    },
    CadenceRule {
        id: "cadence-anthropic-key",
        regex: r"\b(sk-ant-[A-Za-z0-9_-]{20,})",
        group: 1,
        entropy: 3.0,
        keywords: &["sk-ant-"],
        confirm: None,
    },
    CadenceRule {
        id: "cadence-openai-project-key",
        regex: r"\b(sk-proj-[A-Za-z0-9_-]{20,})",
        group: 1,
        entropy: 3.0,
        keywords: &["sk-proj-"],
        confirm: None,
    },
    CadenceRule {
        id: "cadence-github-fine-grained-pat",
        regex: r"\b(github_pat_[A-Za-z0-9_]{20,})",
        group: 1,
        entropy: 3.0,
        keywords: &["github_pat_"],
        confirm: None,
    },
    CadenceRule {
        id: "cadence-gitlab-pat",
        regex: r"\b(glpat-[A-Za-z0-9_-]{20,})",
        group: 1,
        entropy: 3.0,
        keywords: &["glpat-"],
        confirm: None,
    },
    CadenceRule {
        id: "cadence-npm-token",
        regex: r"\b(npm_[A-Za-z0-9]{36})\b",
        group: 1,
        entropy: 3.0,
        keywords: &["npm_"],
        confirm: None,
    },
    CadenceRule {
        id: "cadence-devin-key",
        regex: r"\b(dvn_[A-Za-z0-9_-]{24,})",
        group: 1,
        entropy: 3.5,
        keywords: &["dvn_"],
        confirm: None,
    },
    CadenceRule {
        id: "cadence-google-api-key",
        regex: r"\b(AIza[A-Za-z0-9_-]{35})",
        group: 1,
        entropy: 3.5,
        keywords: &["aiza"],
        confirm: None,
    },
    CadenceRule {
        id: "cadence-argv-secret",
        // `--name value`, `--name=value`, or `[export ]NAME=value`, at a
        // word start. Group 3 is the value.
        regex: r#"(?:^|[\s"'`(\[{,;|&])(?:(--?[A-Za-z][A-Za-z0-9_.-]*)(?:=|[ \t]+)|(?:export[ \t]+)?([A-Za-z_][A-Za-z0-9_]*)=)["']?([^\s"'`]{16,})"#,
        group: 3,
        entropy: 3.5,
        keywords: &["key", "token", "secret", "passw", "auth"],
        confirm: Some(argv_confirm),
    },
];

fn argv_confirm(caps: &Captures) -> bool {
    let text = |i: usize| caps.get(i).map(|m| String::from_utf8_lossy(m.as_bytes()));
    let Some(value) = text(3) else {
        return false;
    };
    if !random_value(&value) {
        return false;
    }
    let (name, argv) = match (text(1), text(2)) {
        (Some(flag), _) => (
            flag.trim_start_matches('-').to_string(),
            vec![flag.to_string(), value.to_string()],
        ),
        (None, Some(env)) => (env.to_string(), vec![format!("{env}={value}")]),
        _ => return false,
    };
    let lower = name.to_ascii_lowercase();
    ARGV_WORDS.iter().any(|w| lower.contains(w)) && redact_argv(&argv).contains("[REDACTED]")
}

/// A value that looks generated rather than written. It needs letters and
/// digits and no placeholder, path or URL shape. Git SHAs and UUIDs are
/// ordinary values, not secrets.
fn random_value(v: &str) -> bool {
    let hex = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_hexdigit());
    let uuid = {
        let segs: Vec<&str> = v.split('-').collect();
        segs.len() == 5
            && segs
                .iter()
                .zip([8_usize, 4, 4, 4, 12])
                .all(|(p, n)| p.len() == n && hex(p))
    };
    v.len() >= 16
        && v.bytes().any(|b| b.is_ascii_alphabetic())
        && v.bytes().any(|b| b.is_ascii_digit())
        && !v.starts_with(['$', '<', '{', '%', '[', '(', '/', '~', '.', '*', '-'])
        && !v.contains("://")
        && !v.contains("REDACTED")
        && !(matches!(v.len(), 40 | 64) && hex(v))
        && !uuid
}

fn load_pack() -> std::result::Result<Pack, String> {
    let raw: RawPack = toml::from_str(PACK_TOML).map_err(|e| format!("gitleaks.toml: {e}"))?;
    if raw.rules.is_empty() {
        return Err("gitleaks.toml has no rules".to_string());
    }
    let global = Allow::from_raw(raw.allowlist.unwrap_or_default())?;
    let mut rules: Vec<Rule> = CADENCE_RULES
        .iter()
        .map(|c| Rule {
            id: c.id.to_string(),
            regex: Some(Lazy::new(c.regex)),
            secret_group: c.group,
            entropy: c.entropy,
            keywords: c.keywords.iter().map(|k| k.to_string()).collect(),
            path: None,
            allows: Vec::new(),
            confirm: c.confirm,
        })
        .collect();
    for r in raw.rules {
        let allows = r
            .allowlists
            .into_iter()
            .map(Allow::from_raw)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| format!("rule {}: {e}", r.id))?;
        rules.push(Rule {
            id: r.id,
            regex: r.regex.map(Lazy::new),
            secret_group: r.secret_group,
            entropy: r.entropy,
            keywords: r.keywords.iter().map(|k| k.to_ascii_lowercase()).collect(),
            path: r.path.map(Lazy::new),
            allows,
            confirm: None,
        });
    }
    Ok(Pack { global, rules })
}

fn pack() -> Result<&'static Pack> {
    static PACK: OnceLock<std::result::Result<Pack, String>> = OnceLock::new();
    PACK.get_or_init(load_pack)
        .as_ref()
        .map_err(|e| unavailable(e))
}

fn unavailable(why: &str) -> Error {
    Error::internal(format!(
        "secret scan unavailable: {why}. Refusing to write."
    ))
}

// ---------- scan ----------

/// Shannon entropy in bits per byte, the same measure gitleaks' per-rule
/// `entropy` threshold uses.
fn entropy(data: &[u8]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    let mut counts = [0usize; 256];
    for b in data {
        counts[*b as usize] += 1;
    }
    let n = data.len() as f64;
    counts
        .iter()
        .filter(|c| **c > 0)
        .map(|c| {
            let p = *c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

fn redact(secret: &str) -> String {
    let keep = (secret.chars().count() / 4).min(4);
    format!("{}…", secret.chars().take(keep).collect::<String>())
}

/// The SHA-256 prefix a custody record carries for its credential —
/// the same display form findings use (ADR 0006 §5.3).
pub fn fingerprint(secret: &[u8]) -> String {
    let digest = Sha256::digest(secret);
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// The full line(s) covering `start..end`.
fn line_span(text: &[u8], start: usize, end: usize) -> &[u8] {
    let from = text[..start]
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |i| i + 1);
    let to = text[end..]
        .iter()
        .position(|b| *b == b'\n')
        .map_or(text.len(), |i| end + i);
    &text[from..to]
}

fn line_col(text: &[u8], at: usize) -> (usize, usize) {
    let before = &text[..at];
    let line = before.iter().filter(|b| **b == b'\n').count() + 1;
    let start = before
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |i| i + 1);
    let column = String::from_utf8_lossy(&before[start..]).chars().count() + 1;
    (line, column)
}

/// Scan `text`. `path` (the file's path, when there is one) enables gitleaks'
/// path-scoped rules and path allowlists. Findings come back in text order,
/// one per secret span; where rules overlap, a blocking finding wins.
/// The operator allowlist is not applied here — see [`Allowlist`].
pub fn scan(text: &str, path: Option<&str>) -> Result<Vec<Finding>> {
    Ok(scan_spans(text, path)?
        .into_iter()
        .map(|(_, _, f)| f)
        .collect())
}

/// Replace every credential-shaped span in `text` (blocking and
/// warn-only alike) with `[redacted:<rule>]`. For text cadence stores
/// on its own initiative — thread entries, tool-call summaries — where
/// refusing is not an option but the value must never land. The
/// operator allowlist is deliberately not applied: redacting a false
/// positive costs a few characters, storing a real one is a leak.
///
/// Redaction covers the UNION of every raw rule match: overlapping or
/// adjacent spans merge (`end = max(end, next.end)`), so a narrower
/// blocking match inside a wider warn-only one, or a match reaching past
/// its neighbour, can never leave part of either in the text. The
/// one-finding-per-span collapse is for [`scan`]'s report only. The
/// marker names the first blocking rule in the merged span, else the
/// first rule.
///
/// A private key is also redacted from its BEGIN header to its END
/// marker, or to the end of the text when no END follows ([`pem_blocks`],
/// CAD-410): a head- or line-limited read, or a text cut at a scan limit,
/// carries the key body without the END the gitleaks rule needs.
pub fn redact_text(text: &str) -> Result<String> {
    // (start, end, rule, is blocking)
    let mut hits: Vec<(usize, usize, String, bool)> = raw_hits(text, None)?
        .into_iter()
        .map(|(start, end, f)| (start, end, f.rule, f.severity == Severity::Block))
        .collect();
    hits.extend(
        pem_blocks(text)?
            .into_iter()
            .map(|(start, end)| (start, end, PEM_RULE.to_string(), true)),
    );
    if hits.is_empty() {
        return Ok(text.to_string());
    }
    hits.sort_by_key(|(start, end, _, _)| (*start, std::cmp::Reverse(*end)));
    // Merge into (start, end, label rule, label is blocking).
    let mut merged: Vec<(usize, usize, String, bool)> = Vec::new();
    for (start, end, rule, block) in hits {
        // Spans are byte offsets from a UTF-8 `&str` scanned as bytes;
        // widen to char boundaries so slicing never panics.
        let start = floor_char_boundary(text, start);
        let end = ceil_char_boundary(text, end);
        if end <= start {
            continue;
        }
        match merged.last_mut() {
            Some(last) if start <= last.1 => {
                last.1 = last.1.max(end);
                if block && !last.3 {
                    last.2 = rule;
                    last.3 = true;
                }
            }
            _ => merged.push((start, end, rule, block)),
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end, rule, _) in merged {
        out.push_str(&text[at..start]);
        out.push_str(&format!("[redacted:{rule}]"));
        at = end;
    }
    out.push_str(&text[at..]);
    Ok(out)
}

/// The gitleaks rule a private-key block is redacted under.
const PEM_RULE: &str = "private-key";

/// Every private-key block in `text`, from its BEGIN header to the next
/// END marker, or to the end of the text when none follows. The header is
/// the gitleaks `private-key` rule's own. Redaction only: that rule, and
/// so [`scan`]'s report, still needs both markers.
fn pem_blocks(text: &str) -> Result<Vec<(usize, usize)>> {
    static MARKERS: OnceLock<std::result::Result<(Regex, Regex), String>> = OnceLock::new();
    let (begin, end) = MARKERS
        .get_or_init(|| {
            Ok((
                compile(r"(?i)-----BEGIN[ A-Z0-9_-]{0,100}PRIVATE KEY(?: BLOCK)?-----")?,
                compile(r"(?i)-----END[ A-Z0-9_-]{0,100}PRIVATE KEY(?: BLOCK)?-----")?,
            ))
        })
        .as_ref()
        .map_err(|e| unavailable(e))?;
    let bytes = text.as_bytes();
    Ok(begin
        .find_iter(bytes)
        .map(|header| {
            let stop = end
                .find_at(bytes, header.end())
                .map_or(bytes.len(), |m| m.end());
            (header.start(), stop)
        })
        .collect())
}

fn floor_char_boundary(text: &str, mut i: usize) -> usize {
    i = i.min(text.len());
    while !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(text: &str, mut i: usize) -> usize {
    i = i.min(text.len());
    while !text.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// [`scan`] with each finding's byte span in `text`: [`raw_hits`]
/// collapsed to one finding per secret span, for reporting.
fn scan_spans(text: &str, path: Option<&str>) -> Result<Vec<(usize, usize, Finding)>> {
    let mut hits = raw_hits(text, path)?;
    // One finding per secret span: cadence rules come first in the rule
    // order, then the first blocking rule replaces a warn-only one.
    hits.sort_by_key(|(start, _, _)| *start);
    let mut out: Vec<(usize, usize, Finding)> = Vec::with_capacity(hits.len());
    for hit in hits {
        match out.last_mut() {
            Some(last) if hit.0 < last.1 => {
                if last.2.severity == Severity::Warn && hit.2.severity == Severity::Block {
                    *last = hit;
                }
            }
            _ => out.push(hit),
        }
    }
    Ok(out)
}

/// Every rule match in `text` with its byte span, in rule order, before
/// any overlap is collapsed.
fn raw_hits(text: &str, path: Option<&str>) -> Result<Vec<(usize, usize, Finding)>> {
    let pack = pack()?;
    let bytes = text.as_bytes();
    let lower = text.to_ascii_lowercase();
    let mut hits: Vec<(usize, usize, Finding)> = Vec::new();
    for rule in &pack.rules {
        let Some(regex) = &rule.regex else {
            continue; // path-only rules flag files, not text
        };
        if let Some(scope) = &rule.path {
            match path {
                Some(p) if scope.get()?.is_match(p.as_bytes()) => {}
                _ => continue,
            }
        }
        if !rule.keywords.is_empty() && !rule.keywords.iter().any(|k| lower.contains(k.as_str())) {
            continue;
        }
        for caps in regex.get()?.captures_iter(bytes) {
            let whole = caps.get(0).expect("group 0 always matches");
            let secret = if rule.secret_group > 0 {
                caps.get(rule.secret_group)
            } else {
                caps.iter().skip(1).flatten().find(|m| !m.is_empty())
            }
            .unwrap_or(whole);
            let value = secret.as_bytes();
            if value.is_empty() {
                continue;
            }
            if rule.entropy > 0.0 && entropy(value) <= rule.entropy {
                continue;
            }
            if rule.confirm.is_some_and(|confirm| !confirm(&caps)) {
                continue;
            }
            let line = line_span(bytes, whole.start(), whole.end());
            let mut allowed = pack.global.allows(value, whole.as_bytes(), line, path)?;
            for allow in &rule.allows {
                if allowed {
                    break;
                }
                allowed = allow.allows(value, whole.as_bytes(), line, path)?;
            }
            if allowed {
                continue;
            }
            let (line, column) = line_col(bytes, secret.start());
            hits.push((
                secret.start(),
                secret.end(),
                Finding {
                    rule: rule.id.clone(),
                    line,
                    column,
                    redacted: redact(&String::from_utf8_lossy(value)),
                    severity: rule.severity(),
                    fingerprint: fingerprint(value),
                },
            ));
        }
    }
    Ok(hits)
}

// ---------- operator allowlist ----------

/// `<state dir>/secret-allowlist.toml`, edited by hand by the operator:
///
/// ```toml
/// [[allow]]
/// rule = "generic-api-key"          # every finding of this rule
/// reason = "our docs quote example keys"
///
/// [[allow]]
/// rule = "cadence-argv-secret"
/// fingerprint = "0123456789abcdef"  # only this value (from `secret scan`)
/// reason = "public test fixture"
/// ```
///
/// A missing file is an empty allowlist. A file that does not parse refuses
/// every guarded write until it is fixed.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allowlist {
    #[serde(default)]
    allow: Vec<AllowEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowEntry {
    rule: String,
    #[serde(default)]
    fingerprint: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    reason: Option<String>,
}

impl Allowlist {
    pub fn path(state_dir: &Path) -> std::path::PathBuf {
        state_dir.join(ALLOWLIST_FILE)
    }

    pub fn load(state_dir: &Path) -> Result<Self> {
        let path = Self::path(state_dir);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(unavailable(&format!(
                    "cannot read {} ({e})",
                    path.display()
                )))
            }
        };
        Self::parse(&text).map_err(|e| unavailable(&format!("{}: {e}", path.display())))
    }

    fn parse(text: &str) -> std::result::Result<Self, String> {
        let list: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        if list.allow.iter().any(|a| a.rule.trim().is_empty()) {
            return Err("every [[allow]] entry needs a rule id".to_string());
        }
        Ok(list)
    }

    pub fn permits(&self, f: &Finding) -> bool {
        self.allow.iter().any(|a| {
            a.rule == f.rule
                && a.fingerprint
                    .as_deref()
                    .is_none_or(|fp| fp == f.fingerprint)
        })
    }
}

// ---------- chokepoints ----------

/// The `cadence secret scan` payload. The bool is true when any finding
/// blocks.
pub fn report(text: &str, path: Option<&str>, allow: &Allowlist) -> Result<(Value, bool)> {
    let all = scan(text, path)?;
    let allowlisted = all.iter().filter(|f| allow.permits(f)).count();
    let findings: Vec<Finding> = all.into_iter().filter(|f| !allow.permits(f)).collect();
    let blocking = findings
        .iter()
        .filter(|f| f.severity == Severity::Block)
        .count();
    Ok((
        json!({
            "findings": findings,
            "blocking": blocking,
            "warnings": findings.len() - blocking,
            "allowlisted": allowlisted,
            "rules": format!("gitleaks {GITLEAKS_VERSION} + cadence"),
        }),
        blocking > 0,
    ))
}

/// The write-path check. It loads the operator allowlist from the state dir
/// (`CADENCE_STATE_DIR`, else the XDG default), scans `text` and refuses on a
/// blocking finding. `what` names the write in the error. `Ok` returns the
/// warn-only findings so the caller can surface them.
pub fn guard(what: &str, text: &str) -> Result<Vec<Finding>> {
    let allow = match crate::client::state_dir() {
        Ok(dir) => Allowlist::load(&dir)?,
        Err(_) => Allowlist::default(),
    };
    guard_with(what, text, &allow)
}

pub fn guard_with(what: &str, text: &str, allow: &Allowlist) -> Result<Vec<Finding>> {
    let (block, warn): (Vec<Finding>, Vec<Finding>) = scan(text, None)?
        .into_iter()
        .filter(|f| !allow.permits(f))
        .partition(|f| f.severity == Severity::Block);
    if block.is_empty() {
        Ok(warn)
    } else {
        Err(refusal(what, &block))
    }
}

/// The refusal error. It names each rule, line and column and the redacted
/// prefix, never the value.
pub fn refusal(what: &str, block: &[Finding]) -> Error {
    let list = block
        .iter()
        .map(|f| {
            format!(
                "rule {} at line {} col {} ({})",
                f.rule, f.line, f.column, f.redacted
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    Error::invalid(
        "secret_detected",
        format!(
            "{what} refused: credential-shaped text found ({list}). Nothing was written. \
             Remove the value; do not paste tokens or tool output that carries them. \
             Check text with `cadence secret scan`. A false positive is for the operator \
             to allowlist in <state dir>/{ALLOWLIST_FILE}."
        ),
    )
}

/// Warn-only findings as a JSON array for a write's result.
pub fn warnings_json(warn: &[Finding]) -> Value {
    json!(warn)
}
