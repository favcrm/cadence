//! Landing-chain discovery and bounded git-history origin-proof scanner.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::super::{git_rel, DEPTH_CAP};
use super::{
    logical_name, valid_key, AMBIGUOUS_LANDING, EMPTY_LANDING, EVIDENCE_PAGE, EVIDENCE_SCAN_CAP,
    EVIDENCE_TRUNCATED, ORIGIN_UNVERIFIED, STORE_MARKER, UNSAFE_TREE,
};
use crate::issue::Pm;

/// One possible original root of a landing: the on-disk chain prefix
/// `rel`, at `abs`, with the kind that prefix would be.
#[derive(Debug)]
pub(super) struct Candidate {
    pub(super) rel: String,
    pub(super) abs: PathBuf,
    pub(super) kind: &'static str,
}

/// Why a landing's on-disk chain cannot name a root at all.
#[derive(Debug, Clone, Copy)]
pub(super) enum ChainFault {
    Empty,
    Unsafe,
}

impl ChainFault {
    pub(super) fn reason(&self) -> &'static str {
        match self {
            ChainFault::Empty => EMPTY_LANDING,
            ChainFault::Unsafe => UNSAFE_TREE,
        }
    }
}

/// The chain `rm` laid down under one landing: one entry at each level
/// down to the deleted root. Every prefix is a possible root (a dir
/// holding one file looks like a deeper file), so the caller must let
/// the commit pick.
///
/// The walk STOPS at the first directory holding more than one entry:
/// that directory is a deleted root with ordinary contents, not an
/// artificial ancestor, and its own candidate is already in `out`. An
/// ancestor above a deleted root always holds exactly one entry (the
/// chain to it), so nothing deeper can be a candidate — refusing the
/// whole landing there is what made a normal multi-file delete
/// unreadable. Reading a level also stops at its second entry, so the
/// chain costs at most two names per level whatever the landing's width.
pub(super) fn landing_chain(landing: &Path) -> std::result::Result<Vec<Candidate>, ChainFault> {
    let mut out = Vec::new();
    let mut cur = landing.to_path_buf();
    let mut rel = String::new();
    let mut depth = 0usize;
    loop {
        let read = match std::fs::read_dir(&cur) {
            Ok(read) => read,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(if depth == 0 {
                    ChainFault::Empty
                } else {
                    ChainFault::Unsafe
                })
            }
            Err(_) => return Err(ChainFault::Unsafe),
        };
        let mut names: Vec<(String, PathBuf, bool)> = Vec::new();
        for entry in read {
            let Ok(entry) = entry else {
                return Err(ChainFault::Unsafe);
            };
            let Ok(ft) = entry.file_type() else {
                return Err(ChainFault::Unsafe);
            };
            if ft.is_symlink() {
                return Err(ChainFault::Unsafe);
            }
            if !ft.is_dir() && !ft.is_file() {
                return Err(ChainFault::Unsafe);
            }
            let Ok(name) = entry.file_name().into_string() else {
                return Err(ChainFault::Unsafe);
            };
            // `wiki mkdir`'s marker is not a chain entry: a directory
            // holding only its `.gitkeep` is the leaf, and one holding
            // the marker plus a page still has exactly one page.
            if name == STORE_MARKER && ft.is_file() {
                continue;
            }
            names.push((name, entry.path(), ft.is_dir()));
            // A second entry is all the chain can use: `cur` branches, so
            // no deeper name can be a candidate. Stop reading here — a
            // deleted root holding a million pages costs two entries, not
            // its width. The rest of the tree's safety is proved by the
            // totals walk and, before any restore, by the restore proof.
            if names.len() > 1 {
                break;
            }
        }
        match names.len() {
            // A landing with nothing in it is not trash (a consumed
            // landing's leftover parents are handled by the commit
            // evidence, not here).
            0 if depth == 0 => return Err(ChainFault::Empty),
            // The current dir is the leaf, and its candidate is already
            // in `out` (an empty dir `rm` moved).
            0 => return Ok(out),
            1 => {}
            // Branching: `cur` is a deleted directory holding several
            // entries. Its candidate is already in `out`; a deeper entry
            // cannot be the root.
            _ => return Ok(out),
        }
        let (name, abs, is_dir) = names.remove(0);
        if logical_name(&name, is_dir).is_none() {
            return Err(ChainFault::Unsafe);
        }
        // The chain keeps the PHYSICAL name: a blob pointer's rel is its
        // `<orig>.blob` file, which is what `rm` wrote on the right of
        // its subject and what the landing actually holds.
        if !rel.is_empty() {
            rel.push('/');
        }
        rel.push_str(&name);
        let kind = if is_dir {
            "dir"
        } else if name.ends_with(".blob") {
            "blob"
        } else {
            "text"
        };
        out.push(Candidate {
            rel: rel.clone(),
            abs: abs.clone(),
            kind,
        });
        depth += 1;
        if !is_dir {
            return Ok(out);
        }
        cur = abs;
        if depth > DEPTH_CAP + 2 {
            return Err(ChainFault::Unsafe);
        }
    }
}

/// The commit that created one landing: its subject, author time
/// (seconds) and `Actor:` trailer.
#[derive(Clone, Debug)]
pub(super) struct Proof {
    pub(super) subject: String,
    pub(super) at: u64,
    pub(super) actor: Option<String>,
}

/// What the tracker says about ONE selected landing.
#[derive(Default)]
pub(super) struct Evidence {
    /// The `rm` commit that created it, matched WHOLE against the subject
    /// `rm` writes for a candidate root — never split out of the display
    /// text, which a legal original path may itself resemble.
    pub(super) proof: Option<Proof>,
    /// A restore moved something out of this landing's key
    /// (`wiki: mv .trash/<key>/…`). That label's first segment is the key,
    /// so the association is exact.
    pub(super) moved: bool,
    /// The scan stopped at [`EVIDENCE_SCAN_CAP`] before this landing's
    /// proof was seen: absence of proof is not proof of absence.
    pub(super) truncated: bool,
}

/// One selected landing the history must prove: its key and the
/// candidate roots its on-disk chain offers (or why the walk stopped).
pub(super) struct Target<'a> {
    pub(super) key: &'a str,
    pub(super) chain: &'a std::result::Result<Vec<Candidate>, ChainFault>,
}

/// The `rm`/`mv` records the tracker carries for the selected landing
/// paths, read in bounded pages until every target is resolved.
#[derive(Default)]
struct Scan {
    /// Every `rm` subject seen; exact matching happens per candidate.
    rm: Vec<Proof>,
    /// Keys a restore moved something out of.
    moved: HashSet<String>,
    /// Keys whose WHOLE landing left the trash (`wiki: mv .trash/<key>`,
    /// no path after the key): nothing of that landing is left to prove.
    consumed: HashSet<String>,
    /// The scan reached the end of the tracker's history.
    complete: bool,
}

/// Read history for the selected landing paths page by page, stopping as
/// soon as every target is resolved, when the history ends, or at
/// [`EVIDENCE_SCAN_CAP`] commits. A vault outside the tracker (no commit
/// evidence exists) yields nothing, and every landing is then reported
/// unverified — never guessed.
fn scan_history(pm: &Pm, trash: &Path, targets: &[Target<'_>]) -> Scan {
    let mut scan = Scan::default();
    if targets.is_empty() {
        scan.complete = true;
        return scan;
    }
    let Ok(trash_rel) = git_rel(pm, trash) else {
        // The vault is outside the tracker: no commit evidence exists at
        // all, so nothing was left unread either.
        scan.complete = true;
        return scan;
    };
    let paths: Vec<std::ffi::OsString> = targets
        .iter()
        .map(|target| trash_rel.join(target.key).into_os_string())
        .collect();
    let mut read = 0usize;
    let mut page = EVIDENCE_PAGE;
    loop {
        let want = page.min(EVIDENCE_SCAN_CAP - read);
        if want == 0 {
            return scan;
        }
        let mut args: Vec<std::ffi::OsString> = vec![
            "-C".into(),
            pm.dir.clone().into_os_string(),
            "log".into(),
            format!("--skip={read}").into(),
            format!("-n{want}").into(),
            "-z".into(),
            "--format=%at%x1f%B%x1e".into(),
            "--".into(),
        ];
        args.extend(paths.iter().cloned());
        let out_run = match crate::reaper::output(std::process::Command::new("git").args(&args)) {
            Ok(out) if out.status.success() => out,
            // A failure before any page was read means there is no
            // evidence to read; after one, the scan stopped early.
            _ => {
                scan.complete = read == 0;
                return scan;
            }
        };
        let text = String::from_utf8_lossy(&out_run.stdout);
        let mut seen = 0usize;
        for rec in text.split('\u{1e}') {
            let rec = rec.trim_start_matches('\0');
            if rec.is_empty() {
                continue;
            }
            seen += 1;
            let Some((at, body)) = rec.split_once('\u{1f}') else {
                continue;
            };
            let subject = body.lines().next().unwrap_or("").to_string();
            let actor = body
                .lines()
                .find_map(|l| l.strip_prefix("Actor: "))
                .map(str::to_string);
            let at = at.trim().parse::<u64>().unwrap_or(0);
            if let Some(rest) = subject.strip_prefix("wiki: mv .trash/") {
                // `.trash/<key>/<rel>` names the key as its first path
                // segment; the whole-landing form `.trash/<key>` has no
                // segment, so its key is what precedes the arrow. A key is
                // digits and hex only, so neither split can be misled by a
                // path that itself looks like the display delimiter.
                let (key, whole) = match rest.split_once('/') {
                    Some((key, _)) if valid_key(key) => (key, false),
                    _ => match rest.split_once(" → ") {
                        Some((key, _)) if valid_key(key) => (key, true),
                        _ => continue,
                    },
                };
                scan.moved.insert(key.to_string());
                if whole {
                    // The landing ITSELF was moved back, so nothing of it
                    // is left to prove. A move of one DESCENDANT says
                    // nothing about the root: a legacy partial restore
                    // leaves the landing's own directories behind, and
                    // the `rm` commit that proves them may sit well past
                    // the first history page.
                    scan.consumed.insert(key.to_string());
                }
                continue;
            }
            if subject.starts_with("wiki: rm ") {
                scan.rm.push(Proof { subject, at, actor });
            }
        }
        read += seen;
        // A short page is the end of the tracker's history.
        if seen < want {
            scan.complete = true;
            return scan;
        }
        if targets.iter().all(|target| resolved(target, &scan)) {
            return scan;
        }
        // Each page re-walks the history with `--skip`, so fewer, larger
        // pages cost less than many small ones.
        page = page.saturating_mul(2);
    }
}

/// Whether this target needs no further history: a chain that offers no
/// candidate (or cannot be walked at all) has nothing to prove, one
/// whose exact `rm` subject was seen is done, and a landing a restore
/// moved out WHOLE is consumed.
///
/// A move of ONE descendant is deliberately not evidence. A legacy
/// partial restore leaves the landing's own directories behind, so a
/// file-empty tree plus any move would stop the scan before the `rm`
/// commit — which can sit past the first page — and report a real empty
/// folder as unproven (or drop it). Only the root's own proof, or the
/// whole landing leaving the trash, settles the question.
fn resolved(target: &Target<'_>, scan: &Scan) -> bool {
    let Ok(cands) = target.chain else {
        return true;
    };
    scan.consumed.contains(target.key)
        || cands
            .iter()
            .any(|cand| rm_proof(target.key, cand, scan).is_some())
}

/// The subject `rm` writes for one candidate root: the logical path on
/// the left, the on-disk chain on the right.
pub(super) fn expected_subject(key: &str, cand: &Candidate) -> String {
    let logical = cand
        .rel
        .strip_suffix(".blob")
        .filter(|_| cand.kind == "blob")
        .unwrap_or(cand.rel.as_str());
    format!("wiki: rm {logical} → .trash/{key}/{}", cand.rel)
}

/// Whether `subject` is exactly the expected one, allowing the
/// ` (<actor>)` suffix `commit_message` appends.
pub(super) fn subject_matches(subject: &str, expected: &str) -> bool {
    if subject == expected {
        return true;
    }
    match subject.strip_prefix(expected) {
        Some(rest) => rest.starts_with(" (") && rest.ends_with(')'),
        None => false,
    }
}

/// The `rm` record that spells one candidate root for one key.
fn rm_proof<'a>(key: &str, cand: &Candidate, scan: &'a Scan) -> Option<&'a Proof> {
    let expected = expected_subject(key, cand);
    scan.rm
        .iter()
        .find(|proof| subject_matches(&proof.subject, &expected))
}

/// Attach the scan's answer to each target.
pub(super) fn evidence(pm: &Pm, trash: &Path, targets: &[Target<'_>]) -> HashMap<String, Evidence> {
    let scan = scan_history(pm, trash, targets);
    let mut out = HashMap::with_capacity(targets.len());
    for target in targets {
        let mut ev = Evidence {
            moved: scan.moved.contains(target.key),
            ..Evidence::default()
        };
        if let Ok(cands) = target.chain {
            ev.proof = cands
                .iter()
                .find_map(|cand| rm_proof(target.key, cand, &scan).cloned());
            // Only a question the scan never answered is "truncated": a
            // landing a restore moved out whole is settled, not unread.
            ev.truncated = ev.proof.is_none()
                && !cands.is_empty()
                && !scan.complete
                && !scan.consumed.contains(target.key);
        }
        out.insert(target.key.to_string(), ev);
    }
    out
}

/// The one candidate root the landing's commit proves, with that commit.
/// No proof at all → [`ORIGIN_UNVERIFIED`] (or [`EVIDENCE_TRUNCATED`]
/// when the scan never reached the end of history); a proof that spells
/// no candidate → unverified; two candidates spelled by one subject →
/// ambiguous, which cannot happen for a key `rm` minted and is refused
/// rather than guessed.
pub(super) fn proven_origin<'a>(
    key: &str,
    cands: &'a [Candidate],
    ev: &'a Evidence,
) -> std::result::Result<(&'a Candidate, &'a Proof), &'static str> {
    let Some(proof) = &ev.proof else {
        return Err(if ev.truncated {
            EVIDENCE_TRUNCATED
        } else {
            ORIGIN_UNVERIFIED
        });
    };
    let mut hits = cands
        .iter()
        .filter(|cand| subject_matches(&proof.subject, &expected_subject(key, cand)));
    let first = hits.next();
    let second = hits.next();
    match (first, second) {
        (Some(cand), None) => Ok((cand, proof)),
        (None, _) => Err(ORIGIN_UNVERIFIED),
        (Some(_), Some(_)) => Err(AMBIGUOUS_LANDING),
    }
}
