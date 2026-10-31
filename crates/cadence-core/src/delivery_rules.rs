//! Pure path and glob rules the delivery policy and the requirements
//! evaluator share.
//!
//! These moved out of the native crate so the hosted tenant evaluates a
//! changed path with the same grammar the CLI uses. There is one
//! implementation: `src/issue/areas.rs` and `src/review/analysis.rs` re-export
//! from here.

use crate::error::{Error, Result};

/// Longest accepted area/planned path, in bytes.
pub const PATH_MAX: usize = 256;

/// Wildcard ceiling, because wildcards are what makes matching cost grow.
pub const WILD_MAX: usize = 16;

/// `*`/`**`/`?` glob match: `*` stays inside a path component, `**`
/// crosses directories, `?` is one non-`/` character.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    glob_rec(pattern.as_bytes(), path.as_bytes())
}

fn glob_rec(p: &[u8], s: &[u8]) -> bool {
    if p.is_empty() {
        return s.is_empty();
    }
    match p[0] {
        b'*' if p.get(1) == Some(&b'*') => {
            // `**/`: also try matching zero directories.
            if p.get(2) == Some(&b'/') && glob_rec(&p[3..], s) {
                return true;
            }
            for i in 0..=s.len() {
                if glob_rec(&p[2..], &s[i..]) {
                    return true;
                }
            }
            false
        }
        b'*' => {
            for i in 0..=s.len() {
                if glob_rec(&p[1..], &s[i..]) {
                    return true;
                }
                if i < s.len() && s[i] == b'/' {
                    break;
                }
            }
            false
        }
        b'?' => !s.is_empty() && s[0] != b'/' && glob_rec(&p[1..], &s[1..]),
        c => !s.is_empty() && s[0] == c && glob_rec(&p[1..], &s[1..]),
    }
}

/// Guard one area/planned path: file-level only, repo-relative, one
/// path segment list without escapes, and a bounded wildcard count.
pub fn check_path(p: &str) -> Result<()> {
    // `p` is agent-writable and echoed raw into the message; scrub the
    // display copy so a control/format byte never reaches a warning —
    // the match below still reads `p` itself.
    let bad = |why: &str| Err(Error::rejected(format!("path '{}' {why}", scrub(p))));
    if p.is_empty() {
        return bad("is empty");
    }
    if p.len() > PATH_MAX {
        return bad(&format!("is longer than {PATH_MAX} bytes"));
    }
    if p.contains('#') {
        return bad(
            "names a symbol — areas and planned paths are file-level globs only \
             (symbol-level areas are not supported)",
        );
    }
    if p.chars().any(|c| c.is_control() || c == '\\' || c == ',') {
        return bad("contains a control character, a backslash or a comma");
    }
    if p.starts_with('/') || p.starts_with('~') {
        return bad("must be relative to the repo root");
    }
    let body = p.strip_suffix('/').unwrap_or(p);
    if body
        .split('/')
        .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return bad("has an empty, '.' or '..' segment");
    }
    if p.chars().filter(|c| matches!(c, '*' | '?')).count() > WILD_MAX {
        return bad(&format!("has more than {WILD_MAX} wildcards"));
    }
    Ok(())
}

/// Agent-controlled strings (refs, aliases, planted frontmatter,
/// filenames, a `changed_error`'s git stderr) reach terminals, tracker
/// comments and the overview JSON through warnings. Strip anything
/// that moves a cursor or mirrors text: control characters and bidi or
/// other invisible format marks. Matching fields stay raw — this is
/// for display only.
pub fn scrub(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() && !is_format(*c))
        .collect()
}

/// Invisible or direction-overriding marks — the bidi controls,
/// joiners, soft hyphen, BOM, word joiners, interlinear anchors and
/// hangul fillers — none of which a path, ref or alias legitimately
/// carries.
pub fn is_format(c: char) -> bool {
    matches!(c,
        '\u{00AD}'              // soft hyphen
        | '\u{061C}'            // arabic letter mark
        | '\u{115F}' | '\u{1160}' | '\u{FFA0}'  // hangul fillers
        | '\u{200B}'..='\u{200F}' // ZWSP ZWNJ ZWJ LRM RLM
        | '\u{202A}'..='\u{202E}' // LRE RLE PDF RLO LRO
        | '\u{2060}'..='\u{2064}' // word joiner, invisible operators
        | '\u{2066}'..='\u{2069}' // LRI RLI FSI PDI
        | '\u{FEFF}'            // BOM / ZWNBSP
        | '\u{FFF9}'..='\u{FFFB}' // interlinear annotation anchors
    )
}
