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

/// CAD-2413: the `areas:` glob grammar, shared with a delivery policy's
/// `risk.when.paths` (moved here from the binary's `issue::areas`).
fn area_wild(seg: &str) -> bool {
    seg.contains('*') || seg.contains('?')
}

/// The glob's segments, a trailing `/` read as `/**`.
fn area_segments(glob: &str) -> Vec<&str> {
    match glob.strip_suffix('/') {
        Some(dir) => dir.split('/').chain(std::iter::once("**")).collect(),
        None => glob.split('/').collect(),
    }
}

/// One pattern segment against one path segment. `*` is the only
/// pattern that consumes a variable run, so a single backtracking
/// point — the last `*` seen — suffices: retry means that `*` takes
/// one more byte. O(|pat| × |s|) worst case, no exponential retry, so
/// `src/******************z` against sixty `a`s returns in
/// microseconds instead of hanging a render.
fn seg_match(pat: &[u8], s: &[u8]) -> bool {
    let (mut p, mut c) = (0usize, 0usize);
    // (pattern index just past the last `*`, string index it resumes at).
    let mut star: Option<(usize, usize)> = None;
    while c < s.len() {
        match pat.get(p) {
            Some(b'*') => {
                star = Some((p + 1, c));
                p += 1;
            }
            Some(&pc) if pc == b'?' || pc == s[c] => {
                p += 1;
                c += 1;
            }
            // Mismatch or pattern exhausted mid-string: the last `*`
            // absorbs one more byte, or there is no match.
            _ => match star {
                Some((sp, sc)) => {
                    p = sp;
                    c = sc + 1;
                    star = Some((sp, c));
                }
                None => return false,
            },
        }
    }
    while pat.get(p) == Some(&b'*') {
        p += 1;
    }
    p == pat.len()
}

/// Pattern segments against path segments — `**` matches any run of
/// segments, everything else is one [`seg_match`]. A dynamic program
/// over (pattern × path): `dp[i][j]` is "`pat[i..]` matches
/// `path[j..]`", filled back to front in O(|pat| × |path|) cells, so
/// stacked `**`s cannot multiply work.
fn segs_match(pat: &[&str], path: &[&str]) -> bool {
    let (n, m) = (pat.len(), path.len());
    let mut dp = vec![vec![false; m + 1]; n + 1];
    for i in (0..=n).rev() {
        for j in (0..=m).rev() {
            dp[i][j] = match pat.get(i) {
                None => j == m,
                Some(&"**") => dp[i + 1][j] || (j < m && dp[i][j + 1]),
                Some(p) => j < m && seg_match(p.as_bytes(), path[j].as_bytes()) && dp[i + 1][j + 1],
            };
        }
    }
    dp[0][0]
}

/// Does `glob` cover the concrete file `path`? A glob without wildcards
/// is a file or a directory: it covers itself and everything under it.
pub fn area_match(glob: &str, path: &str) -> bool {
    let pat = area_segments(glob);
    let file: Vec<&str> = path.split('/').collect();
    if segs_match(&pat, &file) {
        return true;
    }
    !pat.iter().any(|s| area_wild(s)) && file.len() > pat.len() && file[..pat.len()] == pat[..]
}

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
