//! CAD-2018: the operator's native trash browser — bounded, paginated
//! listing of `wiki rm` landings and of one landing's contents.
//!
//! `rm` lands a page, dir or blob pointer at
//! `.trash/<stamp>-<nonce>/<rel>` and commits exactly one subject:
//! `wiki: rm <orig> → .trash/<stamp>-<nonce>/<rel>` (`<rel>` is
//! `<orig>` for a text page or dir and `<orig>.blob` for a blob
//! pointer, plus the ` (<actor>)` suffix every wiki commit carries).
//! That commit is the ONLY proof of a landing's original root: the
//! on-disk chain alone cannot tell a `rm` of `a/b` from a `rm` of
//! `a/b/c` when `a/b` held a single child. The chain therefore offers
//! every prefix it can — it stops where a directory branches, because a
//! deleted directory may hold any number of children — and the commit
//! picks exactly one. The subject is compared WHOLE with the one `rm`
//! would have written for that candidate, never split on the `→`
//! display delimiter, which a legal original path may itself contain.
//! A landing whose commit is missing, unreadable or unmatched is
//! reported with no path ([`ORIGIN_UNVERIFIED`]) — never guessed — and
//! `items` refuses it outright, because a logical path it cannot prove
//! must not be invented.
//!
//! A landing a restore consumed is skipped: `mv` out of `.trash/`
//! renames the leaf and leaves its parent dirs behind, so the leftover
//! chain is not trash. A restore of ONE DESCENDANT (the legacy CLI
//! accepts any accepted source) moves only that entry: the landing stays
//! listed and `items` serves what is left, so a partial restore never
//! hides recoverable data. Only a leftover chain that holds no
//! recoverable file, once the whole history has been read, is dropped.
//!
//! History is read in bounded pages ([`EVIDENCE_PAGE`] commits at a
//! time, [`EVIDENCE_SCAN_CAP`] in all) and keeps paging while a selected
//! key is unresolved, so a page full of restored landings cannot hide an
//! older live landing's proof behind a fixed cap. A scan that stops at
//! the cap without a proof reports [`EVIDENCE_TRUNCATED`], never
//! [`ORIGIN_UNVERIFIED`]: absence of proof is not proof of absence.
//! Paging stops early only for evidence that settles the question: the
//! exact `rm` commit that proves a candidate root, or a `mv` of the
//! WHOLE landing (`.trash/<key>`, no path after the key), which consumed
//! it. A `mv` of one descendant is not evidence about the root — a
//! legacy partial restore leaves the landing's own directories behind —
//! so it never shortens the scan, and a truncated scan never drops a
//! landing: an unread history stays visible as unknown.
//!
//! Both reads are operator-only (trash is operator-managed; `mv`
//! already refuses a non-operator restore), paginated (`limit` default
//! 50, cap 100) and bounded. The landing scan keeps a bounded top-K
//! selection. A landing's tree is walked lazily in pre-order with an
//! explicit cursor: each level is enumerated once, streaming, and only
//! its smallest `limit + 1` entries past the cursor are kept, so a level
//! of any width costs one scan and O(`limit`) memory — a listing is
//! never materialized. Lexical order has no index to seek in, so one
//! page costs O(the width of the levels it visits) in reads; no entry is
//! ever skipped or hidden. An `items` cursor is bound to the landing id
//! it was minted for, so a cursor from another deletion (of the same
//! folder, or of a nested one) is refused instead of omitting the rows
//! before it. The landing row's totals walk streams the same way and
//! counts visited directories and files separately under [`COUNT_CAP`]:
//! a tree that cannot be counted within that bound reports `item_count`
//! and `size` as `null` (never zero) and stays listed and restorable —
//! large is not unsafe. A landing holding a symlink, a special file, a
//! non-UTF-8 or non-logical name, or two entries with one logical name
//! is `unsafe_tree` and never restored through this API.
//! Paths handed back are the ORIGINAL logical wiki paths; blob pointer
//! files collapse to their logical name, `wiki mkdir`'s `.gitkeep`
//! marker is never a page, and a directory landing's item list starts
//! at its children (the root folder is the landing itself). No contents
//! are ever read.
//!
//! A landing that cannot be proven is still listed — the operator must
//! see it — under its opaque `landing_id` with `from`, `path`,
//! `deleted_at`, `actor` and the totals all `null`, `kind` naming the
//! landing directory itself, `restorable:false` and a `reason` code.
//! Nothing about its origin is guessed, so there is no selector to read
//! or restore it with. A landing's destination is never judged here:
//! restoring to an alternate path is legitimate, and only `mv` reports
//! a real conflict.

use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Value};

use super::{normalize, vault_dir, Caller, Op, DEPTH_CAP};
use crate::error::{Error, Result};
use crate::issue::Pm;

mod history;
mod walk;

use history::{evidence, landing_chain, proven_origin, Candidate, ChainFault, Evidence, Target};
#[cfg(test)]
use history::{expected_subject, subject_matches, Proof};
use walk::{
    has_twin, node_size, read_pointer, select_keys, totals, unproven_row, walk_children, Seek,
    WalkFault,
};

/// The daemon RPC verbs and the board paths this module answers. They
/// are the seam an independent acceptance check binds to (denied RPC
/// frame / denied HTTP peer), so they are named once, here.
pub const LIST_RPC: &str = "wiki_trash_list";
pub const ITEMS_RPC: &str = "wiki_trash_items";
pub const LIST_PATH: &str = "/api/wiki/trash";
pub const ITEMS_PATH: &str = "/api/wiki/trash/items";

/// Pagination: `limit` defaults to 50 and never exceeds 100.
pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 100;

/// Stable `reason` codes for a landing that cannot be restored as-is.
/// A restorable landing carries `null`.
pub const ORIGIN_UNVERIFIED: &str = "origin_unverified";
pub const UNSAFE_TREE: &str = "unsafe_tree";
pub const UNSAFE_ORIGIN: &str = "unsafe_origin";
pub const AMBIGUOUS_LANDING: &str = "ambiguous_landing";
pub const EMPTY_LANDING: &str = "empty_landing";
/// The history scan stopped at [`EVIDENCE_SCAN_CAP`] before this
/// landing's `rm` commit was seen. Distinct from
/// [`ORIGIN_UNVERIFIED`]: the proof may exist, the scan just did not
/// reach it — never reported as "no proof".
pub const EVIDENCE_TRUNCATED: &str = "evidence_truncated";

/// Nodes a landing's count/size walk may visit before the totals stop
/// being provable (both are then `null`). Directories and files are
/// counted separately, so an all-directory tree (empty folders, or a
/// leftover chain) costs no more than a page-filled one, and one very
/// wide directory stops at the same bound. Reaching it is not a fault:
/// large is not unsafe.
pub(super) const COUNT_CAP: usize = 10_000;
/// Commits the first history page asks for; each further page doubles,
/// up to [`EVIDENCE_SCAN_CAP`]. Paging continues while a selected key is
/// unresolved, so a window full of restored landings cannot truncate an
/// older landing's proof.
pub(super) const EVIDENCE_PAGE: usize = 512;
/// Commits one listing may read in all, across pages. Reaching it with
/// an unresolved key is reported as [`EVIDENCE_TRUNCATED`] — bounded
/// work never becomes a claim that no proof exists.
pub(super) const EVIDENCE_SCAN_CAP: usize = 4096;
/// Longest accepted cursor / `from` string, before decoding.
const SELECTOR_CAP: usize = 4096;
/// A blob pointer file is small JSON; never read more than this.
pub(super) const POINTER_CAP: u64 = 4096;
/// The marker `wiki mkdir` writes into every directory it creates.
pub(super) const STORE_MARKER: &str = ".gitkeep";

/// `wiki trash refused: …` — the verb prefix every refusal carries.
pub(super) fn refused(why: impl std::fmt::Display) -> Error {
    Error::rejected(format!("wiki trash refused: {why}"))
}

/// Trash is the operator's: `mv` already refuses a `.trash/` source
/// from anyone else, and a listing is the same authority.
fn require_operator(caller: &Caller) -> Result<()> {
    match caller {
        Caller::Operator => Ok(()),
        other => Err(refused(format!(
            "the trash is operator-managed — this caller is '{}'; \
             restore and list from the operator's own session",
            other.actor()
        ))),
    }
}

/// `limit`: absent → [`DEFAULT_LIMIT`]; 0 or over [`MAX_LIMIT`] is
/// refused, so no page is ever larger than the contract's.
fn page_limit(limit: Option<u64>, verb: &str) -> Result<usize> {
    match limit {
        None => Ok(DEFAULT_LIMIT),
        Some(0) => Err(refused(format!("{verb}: 'limit' must be at least 1"))),
        Some(n) if n > MAX_LIMIT as u64 => {
            Err(refused(format!("{verb}: 'limit' is capped at {MAX_LIMIT}")))
        }
        Some(n) => Ok(n as usize),
    }
}

fn encode_cursor(key: &str) -> String {
    URL_SAFE_NO_PAD.encode(key.as_bytes())
}

fn decode_cursor(raw: &str) -> Result<String> {
    if raw.is_empty() || raw.len() > SELECTOR_CAP {
        return Err(refused("the cursor is malformed"));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| refused("the cursor is malformed"))?;
    String::from_utf8(bytes).map_err(|_| refused("the cursor is malformed"))
}

/// An `items` cursor names the landing it was minted for AND the last row
/// handed out. Both travel in one opaque value: a cursor from another
/// deletion (of the same folder, or of a nested one) must be refused, not
/// applied — applying it would resume mid-tree and silently omit every
/// entry before it. A NUL separates them because neither a landing key
/// nor a logical path can carry one.
fn encode_items_cursor(key: &str, path: &str) -> String {
    URL_SAFE_NO_PAD.encode(format!("{key}\u{0}{path}").as_bytes())
}

fn decode_items_cursor(raw: &str) -> Result<(String, String)> {
    let text = decode_cursor(raw)?;
    let Some((key, path)) = text.split_once('\u{0}') else {
        return Err(refused("the cursor is malformed"));
    };
    if !valid_key(key) || path.is_empty() {
        return Err(refused("the cursor is malformed"));
    }
    Ok((key.to_string(), path.to_string()))
}

/// A landing key's sort value: the stamp numerically (so a legacy
/// short stamp still orders by time), then the nonce.
pub(super) fn key_order(key: &str) -> (u64, String) {
    match key.split_once('-') {
        Some((stamp, nonce)) => (stamp.parse::<u64>().unwrap_or(0), nonce.to_string()),
        None => (0, key.to_string()),
    }
}

/// A lowercase hex digit — the only spelling `put_blob` writes for a
/// sha256 or a landing nonce.
pub(super) fn lower_hex(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
}

/// `rm`'s landing-directory grammar, taken from the ONE definition `mv`
/// also validates a restore with: a listing must never advertise a
/// selector the restore parser rejects.
pub(super) fn valid_key(key: &str) -> bool {
    super::valid_landing_key(key)
}

/// The logical wiki name one directory entry carries, or `None` when the
/// name the wiki path grammar refuses (dot-prefixed, `%`, control bytes,
/// over-long) or cannot be spelled as a logical entry at all.
///
/// One managed `.blob` suffix is the pointer's own, so a pointer file
/// `x.blob` is the logical page `x`. The name left behind must itself be
/// a plain logical name: `x.blob.blob` (logical `x.blob`, which `mv`
/// would refuse as a page) is not one, and neither is a directory
/// spelled `<name>.blob` — `normalize` never admits that suffix, so such
/// a directory is unaddressable.
pub(super) fn logical_name(name: &str, is_dir: bool) -> Option<&str> {
    if name.is_empty()
        || name.starts_with('.')
        || name.len() > 255
        || name
            .chars()
            .any(|c| c == '\\' || c == '%' || c.is_control() || c == '\u{7f}' || c == '\u{202e}')
    {
        return None;
    }
    match name.strip_suffix(".blob") {
        Some(logical) => {
            if is_dir || logical.is_empty() || logical.ends_with(".blob") {
                return None;
            }
            Some(logical)
        }
        None => Some(name),
    }
}

/// Whether a landing still holds anything recoverable, walked lazily
/// under [`COUNT_CAP`]. A leftover chain whose every entry is a directory
/// or the store's own `.gitkeep` holds no data; anything else — a file, a
/// special file, an unreadable tree — counts as recoverable, so an
/// operator still sees it. Never blocks: no content is read.
fn holds_files(dir: &Path) -> bool {
    let mut seen = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&cur) else {
            return true;
        };
        for entry in read {
            seen += 1;
            if seen > COUNT_CAP {
                return true;
            }
            let Ok(entry) = entry else {
                return true;
            };
            let Ok(ft) = entry.file_type() else {
                return true;
            };
            if ft.is_symlink() || (!ft.is_dir() && !ft.is_file()) {
                return true;
            }
            if ft.is_file() {
                if entry.file_name().to_str() == Some(STORE_MARKER) {
                    continue;
                }
                return true;
            }
            stack.push(entry.path());
        }
    }
    false
}

/// The landing row, or `None` when the landing is not trash any more
/// (empty, or a restore left nothing behind).
fn landing_row(
    abs: &Path,
    key: &str,
    chain: &std::result::Result<Vec<Candidate>, ChainFault>,
    ev: &Evidence,
) -> Result<Option<Value>> {
    match abs.symlink_metadata() {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
        Ok(_) => return Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let chain = match chain {
        Ok(chain) => chain,
        Err(ChainFault::Empty) => return Ok(None),
        Err(fault) => return Ok(Some(unproven_row(key, fault.reason()))),
    };
    let (cand, proof) = match proven_origin(key, chain, ev) {
        Ok(found) => found,
        // A restore moved something out of this key, the scan read the
        // WHOLE history without finding a proof, and no recoverable file
        // is left: this is the leftover chain `mv` leaves behind, not
        // trash. A landing that still holds one stays listed — a partial
        // restore never hides the rest. A TRUNCATED scan proves nothing,
        // so it never drops a landing: an unread history stays visible as
        // unknown rather than disappearing.
        Err(_) if ev.moved && !ev.truncated && !holds_files(abs) => return Ok(None),
        Err(reason) => return Ok(Some(unproven_row(key, reason))),
    };
    // The root is proven; the destination is NOT judged here — an
    // alternate destination is a legitimate restore, and only `mv`
    // reports a real conflict.
    //
    // A blob landing's chain name is its `<orig>.blob` pointer; the
    // logical root (and the `from` `mv` accepts) is the page name.
    let logical = match cand.kind {
        "blob" => cand.rel.strip_suffix(".blob").unwrap_or(cand.rel.as_str()),
        _ => cand.rel.as_str(),
    };
    let mut reason = None;
    let totals = totals(cand);
    if let Some(fault) = totals.reason {
        reason = Some(fault);
    } else {
        let segs: Vec<&str> = logical.split('/').collect();
        let path_ok = normalize(logical).is_ok_and(|norm| norm == logical)
            && !logical.is_empty()
            && super::allowed_by_table(&Caller::Operator, Op::Write, &segs).is_ok();
        if !path_ok {
            reason = Some(UNSAFE_ORIGIN);
        }
    }
    let restorable = reason.is_none();
    Ok(Some(json!({
        "landing_id": key,
        "from": format!(".trash/{key}/{logical}"),
        "path": logical,
        "kind": cand.kind,
        "deleted_at": proof.at.saturating_mul(1000),
        "actor": proof.actor.clone(),
        "item_count": totals.items,
        "size": totals.size,
        "restorable": restorable,
        "reason": reason,
    })))
}

/// `GET /api/wiki/trash` — the operator's landings, newest first.
pub fn list(pm: &Pm, caller: &Caller, limit: Option<u64>, cursor: Option<&str>) -> Result<Value> {
    require_operator(caller)?;
    let limit = page_limit(limit, "list")?;
    let cursor_key = cursor.map(decode_cursor).transpose()?;
    if cursor_key.as_deref().is_some_and(|key| !valid_key(key)) {
        return Err(refused("the cursor is malformed"));
    }
    let vault = vault_dir(pm)?;
    let trash = super::trash_dir(&vault);
    match trash.symlink_metadata() {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(refused(
                "the trash directory is not a plain directory — refusing to list it",
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({"landings": [], "next_cursor": Value::Null}))
        }
        Err(e) => return Err(e.into()),
    }
    let (keys, window_full) = select_keys(&trash, limit + 1, cursor_key.as_deref())?;
    // Each selected key's on-disk chain is read once, then history is
    // scanned for exactly those keys — paging until every one is proven.
    let chains: Vec<(String, std::result::Result<Vec<Candidate>, ChainFault>)> = keys
        .iter()
        .map(|key| (key.clone(), landing_chain(&trash.join(key))))
        .collect();
    let targets: Vec<Target<'_>> = chains
        .iter()
        .map(|(key, chain)| Target {
            key: key.as_str(),
            chain,
        })
        .collect();
    let ev = evidence(pm, &trash, &targets);
    let mut landings = Vec::new();
    let mut seen: Option<String> = None;
    let mut stopped = false;
    for (key, chain) in &chains {
        if landings.len() == limit {
            stopped = true;
            break;
        }
        seen = Some(key.clone());
        let empty = Evidence::default();
        let row = landing_row(&trash.join(key), key, chain, ev.get(key).unwrap_or(&empty))?;
        if let Some(row) = row {
            landings.push(row);
        }
    }
    let next_cursor = match &seen {
        Some(key) if stopped || window_full => Value::String(encode_cursor(key)),
        _ => Value::Null,
    };
    Ok(json!({"landings": landings, "next_cursor": next_cursor}))
}

/// A `from` selector: `.trash/<key>/<logical path>`, the exact value
/// `mv` accepts. Both spellings of a blob landing (`x` and `x.blob`)
/// are canonicalized to the logical one.
fn parse_from(from: &str) -> Result<(String, String)> {
    if from.len() > SELECTOR_CAP {
        return Err(refused("the landing selector is too long"));
    }
    let Some(rest) = from.strip_prefix(".trash/") else {
        return Err(refused(format!(
            "'from' must be a '.trash/…' landing selector — got '{from}'"
        )));
    };
    let Some((key, tail)) = rest.split_once('/') else {
        return Err(refused("'from' names no path inside the landing"));
    };
    if !valid_key(key) {
        return Err(refused("'from' does not name a landing"));
    }
    let logical = tail.strip_suffix(".blob").unwrap_or(tail);
    let norm = normalize(logical)?;
    if norm.is_empty() || norm != logical {
        return Err(refused(
            "'from' does not name a canonical wiki path inside the landing",
        ));
    }
    Ok((key.to_string(), norm))
}

/// `GET /api/wiki/trash/items?from=` — one landing's contents as
/// original logical paths, pre-order. A directory landing answers its
/// descendants (the root folder is the landing itself, never a row); a
/// page landing answers the page.
pub fn items(
    pm: &Pm,
    caller: &Caller,
    from: &str,
    limit: Option<u64>,
    cursor: Option<&str>,
) -> Result<Value> {
    require_operator(caller)?;
    let limit = page_limit(limit, "items")?;
    let (key, orig) = parse_from(from)?;
    let cursor_path = match cursor {
        None => None,
        Some(raw) => {
            let (cursor_key, path) = decode_items_cursor(raw)?;
            // The cursor is bound to the deletion it was minted for: a
            // different landing (even of the same folder) has different
            // rows, and resuming there would silently omit them.
            if cursor_key != key {
                return Err(refused("the cursor does not belong to this landing"));
            }
            Some(path)
        }
    };
    let vault = vault_dir(pm)?;
    let trash = super::trash_dir(&vault);
    match trash.symlink_metadata() {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(refused(
                "the trash directory is not a plain directory — refusing to read it",
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(refused("no such landing"))
        }
        Err(e) => return Err(e.into()),
    }
    let abs = trash.join(&key);
    match abs.symlink_metadata() {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
        Ok(_) => return Err(refused("no such landing")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(refused("no such landing"))
        }
        Err(e) => return Err(e.into()),
    }
    let chain = landing_chain(&abs);
    let ev = evidence(
        pm,
        &trash,
        &[Target {
            key: key.as_str(),
            chain: &chain,
        }],
    );
    let empty = Evidence::default();
    let ev = ev.get(&key).unwrap_or(&empty);
    let cands = match &chain {
        Ok(cands) => cands,
        Err(fault) => {
            return Err(refused(format!(
                "the landing cannot be described ({})",
                fault.reason()
            )))
        }
    };
    let (cand, _) = match proven_origin(&key, cands, ev) {
        Ok(found) => found,
        Err(ORIGIN_UNVERIFIED) => {
            return Err(refused(format!(
                "the landing's original path is not proven by its commit \
                 ({ORIGIN_UNVERIFIED}) — its contents cannot be named"
            )))
        }
        Err(EVIDENCE_TRUNCATED) => {
            return Err(refused(format!(
                "the landing's history was not read to the end \
                 ({EVIDENCE_TRUNCATED}) — its contents cannot be named"
            )))
        }
        Err(reason) => {
            return Err(refused(format!(
                "the landing's original path is ambiguous ({reason})"
            )))
        }
    };
    // `from` must name the PROVEN root, not an arbitrary prefix or
    // descendant: only the root is the landing's selector.
    let proven_logical = match cand.kind {
        "blob" => cand.rel.strip_suffix(".blob").unwrap_or(cand.rel.as_str()),
        _ => cand.rel.as_str(),
    };
    if orig != proven_logical {
        return Err(refused("'from' is not the landing's proven original path"));
    }

    // A directory landing's entries are its DESCENDANTS — the root
    // folder is the landing itself, named by `path` in the listing —
    // while a page landing's one entry is the page. This is the shared
    // wire's convention; `items` never repeats the root folder row.
    let is_dir = cand.kind == "dir";
    let mut cursor_segs: Option<Vec<String>> = None;
    let mut page_spent = false;
    match &cursor_path {
        None => {}
        Some(path) if path == &orig => {
            if is_dir {
                return Err(refused("the cursor does not belong to this landing"));
            }
            // The page was the only entry: nothing follows it.
            page_spent = true;
        }
        Some(path) => match path.strip_prefix(&format!("{orig}/")) {
            Some(rest) => {
                let segs: Vec<String> = rest.split('/').map(str::to_string).collect();
                if segs.iter().any(String::is_empty) {
                    return Err(refused("the cursor is malformed"));
                }
                if !is_dir {
                    return Err(refused("the cursor does not belong to this landing"));
                }
                cursor_segs = Some(segs);
            }
            None => return Err(refused("the cursor does not belong to this landing")),
        },
    }
    let mut seek = match &cursor_segs {
        Some(segs) => Seek::At(segs.as_slice()),
        None => Seek::Past,
    };
    let mut entries: Vec<Value> = Vec::new();
    let mut more = false;
    let mut last: Option<String> = None;
    if !page_spent {
        let mut emit = |path: &str, kind: &'static str, size: Option<u64>| -> bool {
            if entries.len() == limit {
                more = true;
                return false;
            }
            entries.push(json!({"path": path, "kind": kind, "size": size}));
            last = Some(path.to_string());
            true
        };
        if is_dir {
            // `limit + 1` is the page bound: the walk keeps one entry
            // more than the page can hold at every level, so a row past
            // the page is always attempted — which is what proves
            // `next_cursor` — while a wide directory is scanned once and
            // never collected. (An entry emits at least one row, so the
            // first `limit + 1` entries of a level are all the walk can
            // need to fill `limit` rows and see one more.)
            walk_children(&cand.abs, &orig, 1, &mut seek, limit + 1, &mut emit)
                .map_err(WalkFault::into_error)?;
            if matches!(seek, Seek::At(_)) {
                // The cursor entry is gone: resuming would silently skip
                // live rows, so the page refuses instead. (The walk
                // refuses a cursor it cannot find on its own; this keeps
                // the guard explicit at the API boundary.)
                return Err(refused(
                    "the cursor no longer names a live entry — reopen the landing",
                ));
            }
        } else {
            emit(&orig, cand.kind, node_size(&cand.abs, cand.kind));
        }
    }
    let next_cursor = match (more, last) {
        (true, Some(path)) => Value::String(encode_items_cursor(&key, &path)),
        _ => Value::Null,
    };
    Ok(json!({
        "from": format!(".trash/{key}/{orig}"),
        "entries": entries,
        "next_cursor": next_cursor,
    }))
}

// ---------- restore-time proof (called by `wiki mv`) ----------

/// `wiki mv refused: …` for a landing that cannot be put back.
fn restore_refused(why: impl std::fmt::Display) -> Error {
    Error::rejected(format!(
        "wiki mv refused: the .trash/ landing cannot be restored — {why}"
    ))
}

/// Prove a `.trash/` landing is safe to move back into the live tree.
/// `mv_real` calls this under the SAME PM write lock as the rename, so
/// no writer can swap the tree between the proof and the move:
///
/// - its own name and every entry name is a logical wiki name (no
///   dot-prefixed, `%`-carrying, control or over-long segment, no
///   directory spelled `<name>.blob`, no pointer whose logical name
///   still ends in `.blob`), and no two entries share one logical name
///   (`x` beside `x.blob` would hide one of them from every read);
/// - no symlink and no special file (FIFO, socket, device) anywhere: a
///   planted link would escape the vault the moment the tree is live,
///   and a FIFO opened for reading could block this write lock forever;
/// - every `.blob` pointer is ONE complete record the ordinary blob
///   reader accepts (a 64-lowercase-hex sha256, a non-negative integer
///   size, string mime/name), read no-follow/non-blocking and bounded by
///   [`POINTER_CAP`];
/// - the blob store is a real, non-symlink directory and each stored
///   object is a regular, non-symlink file of the pointer's recorded
///   size — a restore never introduces a page whose bytes are missing,
///   truncated or pointed at something else;
/// - every LOGICAL path it will occupy once restored to `to` is a
///   canonical API path: the root is `to`, each descendant is
///   `to/<logical name>`, and each passes the grammar [`normalize`]
///   applies (segment shape and bytes, [`DEPTH_CAP`] segments,
///   `PATH_CAP` bytes). `mv` already normalizes `to` alone, but a
///   directory's descendants inherit that root's depth and byte budget:
///   restored to a destination already at the depth or length cap, a
///   child would be moved into place and then refused by every read. The
///   proof therefore walks the source tree once and checks each
///   descendant's destination path as it goes — one traversal, not a
///   second walk.
///
/// What this does NOT claim: a privileged process (or one that already
/// owns the vault) can still change the filesystem after the check; the
/// guarantee ends at this daemon's own write lock.
pub(super) fn check_restore_destination(vault: &Path, src: &Path, to: &str) -> Result<()> {
    check_restore(vault, src, Some(to))
}

/// The source-only half of the proof, kept for the unit tests that pin
/// one refusal each. Production restores always prove the destination
/// too, through [`check_restore_destination`].
#[cfg(test)]
pub(super) fn check_restore_source(vault: &Path, src: &Path) -> Result<()> {
    check_restore(vault, src, None)
}

fn check_restore(vault: &Path, src: &Path, to: Option<&str>) -> Result<()> {
    let blobs = super::blobs_dir(vault);
    let meta = src
        .symlink_metadata()
        .map_err(|e| restore_refused(format!("its source cannot be read ({e})")))?;
    if meta.file_type().is_symlink() {
        return Err(restore_refused("its source is a symlink"));
    }
    let name = src
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| restore_refused("its source name is not UTF-8"))?;
    if logical_name(name, meta.is_dir()).is_none() {
        return Err(restore_refused(format!(
            "'{name}' is not a logical wiki name"
        )));
    }
    if meta.is_dir() {
        if let Some(to) = to {
            check_destination(to, None)?;
        }
        return check_tree(&blobs, src, 1, to);
    }
    if !meta.is_file() {
        return Err(restore_refused(
            "its source is not a page, a directory or a blob pointer",
        ));
    }
    // A page or pointer lands at `to` itself; a directory's descendants
    // are budgeted from there in `check_tree`.
    if let Some(to) = to {
        check_destination(to, None)?;
    }
    // A blob page's landing holds its `<orig>.blob` pointer; a text
    // page's landing holds the page itself and needs no pointer proof.
    if name.ends_with(".blob") {
        check_pointer(&blobs, src)
    } else {
        Ok(())
    }
}

/// The logical path one restored entry occupies: `to` for the landing
/// root, `to/<logical>` for a descendant, judged by the very grammar
/// [`normalize`] applies to an API path (segment shape, UTF-8 bytes,
/// [`DEPTH_CAP`] segments, `PATH_CAP` bytes). A descendant's name is its
/// LOGICAL name, so a pointer's managed `.blob` suffix never spends the
/// page's budget. `rel` is already a logical name (`logical_name`), and
/// `to` is the normalized destination.
///
/// The result must also be CANONICAL — `normalize` returns it unchanged —
/// the same proof `items`/`from` and `landing_row` use: a path
/// `normalize` rewrites (an NFD name, say) is one no read can address.
fn check_destination(to: &str, rel: Option<&str>) -> Result<String> {
    let logical = match rel {
        Some(rel) => format!("{to}/{rel}"),
        None => to.to_string(),
    };
    match normalize(&logical) {
        Ok(norm) if norm == logical => Ok(logical),
        Ok(_) => Err(restore_refused(format!(
            "restored to '{to}' it would occupy '{logical}', which is not a \
             canonical wiki path"
        ))),
        Err(e) => Err(restore_refused(format!(
            "restored to '{to}' it would occupy '{logical}', which the wiki \
             path grammar refuses ({e})"
        ))),
    }
}

/// The blob store itself, validated exactly as the ordinary byte reader
/// validates it: a real directory, never a symlink to somewhere else.
/// Without this, a symlinked `.blobs` would make a landing look restorable
/// whose bytes the normal reader refuses to serve.
fn blob_store(blobs: &Path) -> Result<&Path> {
    match blobs.symlink_metadata() {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => Ok(blobs),
        Ok(_) => Err(restore_refused("the blob store is not a real directory")),
        Err(e) => Err(restore_refused(format!(
            "the blob store is unavailable ({e})"
        ))),
    }
}

/// Walk one landing tree, proving each entry and — when the restore has
/// a destination — the logical path that entry will occupy. `to` is the
/// destination of `dir` itself (the landing root's `to`, then
/// `to/<logical>` per level), so a descendant's depth and byte budget
/// follow its ancestors without a second traversal.
fn check_tree(blobs: &Path, dir: &Path, depth: usize, to: Option<&str>) -> Result<()> {
    if depth > DEPTH_CAP + 2 {
        return Err(restore_refused("its tree is too deep"));
    }
    let read = std::fs::read_dir(dir)
        .map_err(|e| restore_refused(format!("its tree cannot be read ({e})")))?;
    // A twin lookup per entry bounds memory and avoids quadratic scans of
    // large directories while proving the same logical-name invariant.
    for entry in read {
        let entry = entry.map_err(|e| restore_refused(format!("its tree cannot be read ({e})")))?;
        let ft = entry
            .file_type()
            .map_err(|e| restore_refused(format!("an entry cannot be read ({e})")))?;
        if ft.is_symlink() {
            return Err(restore_refused(format!(
                "it holds a symlink ({})",
                entry.file_name().to_string_lossy()
            )));
        }
        // Only a directory or a regular file is a wiki entry. A FIFO or a
        // device would be moved into the live tree — and a FIFO read
        // below could wait on a writer that never comes, holding the PM
        // write lock while it waits.
        if !ft.is_dir() && !ft.is_file() {
            return Err(restore_refused(format!(
                "it holds a special file ({})",
                entry.file_name().to_string_lossy()
            )));
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| restore_refused("it holds a name that is not UTF-8"))?;
        // The store's own `wiki mkdir` marker is part of a legitimate
        // directory and is never a page; anything else dot-prefixed is
        // not a logical name and is refused.
        if name == STORE_MARKER && ft.is_file() {
            continue;
        }
        let Some(logical) = logical_name(&name, ft.is_dir()) else {
            return Err(restore_refused(format!(
                "it holds '{name}', which is not a logical wiki name"
            )));
        };
        // One logical name per directory: `x` beside `x.blob` is
        // ambiguous — reads pick `x` and writes refuse — so it is never
        // moved back into the live tree.
        match has_twin(dir, logical, !ft.is_dir() && name.ends_with(".blob")) {
            Ok(true) => {
                return Err(restore_refused(format!(
                    "it holds two entries for the logical name '{logical}'"
                )));
            }
            Ok(false) => {}
            Err(error) => {
                return Err(restore_refused(format!(
                    "a logical name cannot be checked ({error})"
                )));
            }
        }
        // The path this entry will occupy once the landing is live: its
        // LOGICAL name (a pointer's `.blob` is the store's, not the
        // page's) appended to its parent's destination. Checking it here
        // keeps the proof to the one walk that already reads the tree.
        let child_to = to
            .map(|prefix| check_destination(prefix, Some(logical)))
            .transpose()?;
        if ft.is_dir() {
            check_tree(blobs, &entry.path(), depth + 1, child_to.as_deref())?;
        } else if name.ends_with(".blob") {
            check_pointer(blobs, &entry.path())?;
        }
    }
    Ok(())
}

/// A `.blob` pointer must be one complete record the ordinary reader
/// accepts, and must reference a stored object that is still there and
/// still the size the pointer recorded.
fn check_pointer(blobs: &Path, pointer: &Path) -> Result<()> {
    let record = read_pointer(pointer).map_err(restore_refused)?;
    let store = blob_store(blobs)?;
    let stored = store.join(&record.sha256);
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&stored)
        .map_err(|e| {
            restore_refused(format!(
                "the stored bytes for {} cannot be opened ({e})",
                record.sha256
            ))
        })?;
    let meta = file.metadata().map_err(|e| {
        restore_refused(format!(
            "the stored bytes for {} cannot be read ({e})",
            record.sha256
        ))
    })?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(restore_refused(format!(
            "the stored bytes for {} are not a regular file",
            record.sha256
        )));
    }
    if meta.len() != record.size {
        return Err(restore_refused(format!(
            "the stored bytes for {} are {} bytes, not the pointer's {}",
            record.sha256,
            meta.len(),
            record.size
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(rel: &str, kind: &'static str) -> Candidate {
        Candidate {
            rel: rel.to_string(),
            abs: PathBuf::from("/nowhere"),
            kind,
        }
    }

    fn proof(subject: &str) -> Proof {
        Proof {
            subject: subject.to_string(),
            at: 1_793_598_009,
            actor: Some("operator".to_string()),
        }
    }

    fn evidence_of(subject: &str) -> Evidence {
        Evidence {
            proof: Some(proof(subject)),
            ..Evidence::default()
        }
    }

    fn rels(chain: &[Candidate]) -> Vec<&str> {
        chain.iter().map(|cand| cand.rel.as_str()).collect()
    }

    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn vault() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".blobs")).unwrap();
        dir
    }

    fn pointer_body(size: &str, extra: &str) -> String {
        format!(
            "{{\"sha256\":\"{DIGEST}\",\"size\":{size},\
             \"mime\":\"application/pdf\",\"name\":\"x.pdf\"}}{extra}"
        )
    }

    fn write_pointer(vault: &Path, body: &str) -> PathBuf {
        let landing = vault.join(".trash").join("1-00000000").join("global");
        std::fs::create_dir_all(&landing).unwrap();
        let pointer = landing.join("x.pdf.blob");
        std::fs::write(&pointer, body).unwrap();
        pointer
    }

    fn mkfifo(path: &Path) {
        let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "mkfifo");
    }

    #[test]
    fn landing_keys_are_the_rm_shape_only() {
        assert!(valid_key("1793598009220-a71d40e5"));
        assert!(valid_key("1-00000000"));
        assert!(!valid_key("1793598009220-A71D40E5"));
        assert!(!valid_key("1793598009220-a71d40e"));
        assert!(!valid_key("-a71d40e5"));
        assert!(!valid_key("1793598009220"));
        assert!(!valid_key("1793598009220-a71d40e5/x"));
        assert!(!valid_key("../a71d40e5"));
    }

    #[test]
    fn from_selectors_are_canonical_landing_paths() {
        assert_eq!(
            parse_from(".trash/1793598009220-a71d40e5/projects/atlas/import").unwrap(),
            (
                "1793598009220-a71d40e5".to_string(),
                "projects/atlas/import".to_string()
            )
        );
        // Both blob spellings canonicalize to the logical page.
        assert_eq!(
            parse_from(".trash/1793598009220-a71d40e5/global/x.pdf")
                .unwrap()
                .1,
            "global/x.pdf"
        );
        assert_eq!(
            parse_from(".trash/1793598009220-a71d40e5/global/x.pdf.blob")
                .unwrap()
                .1,
            "global/x.pdf"
        );
        for bad in [
            "projects/atlas",
            ".trash/1793598009220-a71d40e5",
            ".trash/1793598009220-a71d40e5/",
            ".trash/../a71d40e5/x",
            ".trash/1793598009220-a71d40e5/../x",
            ".trash/1793598009220-a71d40e5/a//b",
            ".trash/1793598009220-a71d40e5/a/.hidden",
            ".trash/1793598009220-a71d40e5/a%2Fb",
            ".trash/1793598009220-a71d40e5/.blob",
            "/.trash/1793598009220-a71d40e5/x",
        ] {
            assert!(parse_from(bad).is_err(), "admitted {bad}");
        }
    }

    #[test]
    fn the_commit_proves_the_root_and_its_actor_suffix() {
        let c = cand("projects/atlas/import", "dir");
        let expected = expected_subject("1793598009220-a71d40e5", &c);
        assert_eq!(
            expected,
            "wiki: rm projects/atlas/import → \
             .trash/1793598009220-a71d40e5/projects/atlas/import"
        );
        assert!(subject_matches(&expected, &expected));
        assert!(subject_matches(
            &format!("{expected} (operator)"),
            &expected
        ));
        assert!(subject_matches(
            &format!("{expected} (fable (ui))"),
            &expected
        ));
        assert!(!subject_matches(
            "wiki: rm projects/atlas → .trash/1793598009220-a71d40e5/projects/atlas",
            &expected
        ));
        assert!(!subject_matches(
            &format!("{expected} (operator) x"),
            &expected
        ));
        // A blob's left side is the logical page, its right side the
        // pointer the landing holds.
        let blob = cand("global/x.pdf.blob", "blob");
        assert_eq!(
            expected_subject("1-00000000", &blob),
            "wiki: rm global/x.pdf → .trash/1-00000000/global/x.pdf.blob"
        );
    }

    #[test]
    fn a_chain_prefix_only_matches_the_commit_that_wrote_it() {
        // `a/b` held one file, so the disk alone cannot say whether `a/b`
        // or `a/b/c.md` was removed; the commit decides.
        let chain = [
            cand("a", "dir"),
            cand("a/b", "dir"),
            cand("a/b/c.md", "text"),
        ];
        let ev = evidence_of("wiki: rm a/b → .trash/1-00000000/a/b (operator)");
        let (root, hit) = proven_origin("1-00000000", &chain, &ev).expect("one proof");
        assert_eq!(root.rel, "a/b");
        assert_eq!(root.kind, "dir");
        assert_eq!(hit.at, 1_793_598_009);

        let none = Evidence::default();
        assert_eq!(
            proven_origin("1-00000000", &chain, &none).unwrap_err(),
            ORIGIN_UNVERIFIED
        );
        // A proof that spells no candidate proves no root — it is not an
        // ambiguity to resolve by guessing.
        assert_eq!(
            proven_origin(
                "1-00000000",
                &chain,
                &evidence_of("wiki: rm a/c → .trash/1-00000000/a/c")
            )
            .unwrap_err(),
            ORIGIN_UNVERIFIED
        );
        // A scan that never reached the end of history says exactly that.
        let truncated = Evidence {
            truncated: true,
            ..Evidence::default()
        };
        assert_eq!(
            proven_origin("1-00000000", &chain, &truncated).unwrap_err(),
            EVIDENCE_TRUNCATED
        );
    }

    #[test]
    fn a_multi_child_directory_offers_its_own_root() {
        let dir = tempfile::tempdir().unwrap();
        let landing = dir.path().join("1-00000000");
        std::fs::create_dir_all(landing.join("global/manual")).unwrap();
        for name in ["a.md", "b.md", "c.md"] {
            std::fs::write(landing.join("global/manual").join(name), name).unwrap();
        }
        // The branching directory is a deleted root with ordinary
        // contents, not an ambiguity: its own candidate is offered and the
        // commit picks it.
        let chain = landing_chain(&landing).unwrap();
        assert_eq!(rels(&chain), ["global", "global/manual"]);
        let ev = evidence_of("wiki: rm global/manual → .trash/1-00000000/global/manual (operator)");
        let (root, _) = proven_origin("1-00000000", &chain, &ev).expect("the commit picks it");
        assert_eq!(root.rel, "global/manual");
        assert_eq!(root.kind, "dir");
    }

    #[test]
    fn a_single_child_chain_still_needs_the_commit() {
        let dir = tempfile::tempdir().unwrap();
        let landing = dir.path().join("1-00000000");
        std::fs::create_dir_all(landing.join("a/b")).unwrap();
        std::fs::write(landing.join("a/b/c.md"), "c").unwrap();
        let chain = landing_chain(&landing).unwrap();
        assert_eq!(rels(&chain), ["a", "a/b", "a/b/c.md"]);
        let ev = evidence_of("wiki: rm a/b → .trash/1-00000000/a/b");
        let (root, _) = proven_origin("1-00000000", &chain, &ev).unwrap();
        assert_eq!(root.rel, "a/b");
    }

    #[test]
    fn a_partial_restore_keeps_the_rest_visible() {
        let dir = tempfile::tempdir().unwrap();
        let landing = dir.path().join("1-00000000");
        std::fs::create_dir_all(landing.join("global/manual")).unwrap();
        std::fs::write(landing.join("global/manual/b.md"), "b").unwrap();
        // `a.md` was restored through the legacy descendant move; the
        // landing still holds `b.md` and must stay listable.
        let ev = Evidence {
            proof: Some(proof(
                "wiki: rm global/manual → .trash/1-00000000/global/manual (operator)",
            )),
            moved: true,
            truncated: false,
        };
        let chain = landing_chain(&landing);
        let row = landing_row(&landing, "1-00000000", &chain, &ev)
            .unwrap()
            .expect("a partially restored landing is still trash");
        assert_eq!(row["path"], "global/manual");
        assert_eq!(row["item_count"], 1);
        assert_eq!(row["restorable"], true);

        // The root itself restored out leaves an empty chain: that is what
        // `mv` leaves behind, not trash.
        std::fs::remove_dir_all(landing.join("global/manual")).unwrap();
        let chain = landing_chain(&landing);
        assert!(landing_row(&landing, "1-00000000", &chain, &ev)
            .unwrap()
            .is_none());

        // Without a restore, the same unprovable chain is REPORTED — an
        // unknown origin stays visible, never hidden or invented.
        let quiet = Evidence {
            proof: Some(proof(
                "wiki: rm global/manual → .trash/1-00000000/global/manual",
            )),
            ..Evidence::default()
        };
        let row = landing_row(&landing, "1-00000000", &chain, &quiet)
            .unwrap()
            .expect("an unproven landing stays visible");
        assert_eq!(row["reason"], ORIGIN_UNVERIFIED);
        assert_eq!(row["from"], Value::Null);
    }

    #[test]
    fn only_a_file_makes_a_leftover_chain_recoverable() {
        let dir = tempfile::tempdir().unwrap();
        let landing = dir.path().join("1-00000000");
        std::fs::create_dir_all(landing.join("a/b")).unwrap();
        assert!(!holds_files(&landing));
        std::fs::write(landing.join("a/b").join(STORE_MARKER), "").unwrap();
        assert!(!holds_files(&landing), "the store marker is not data");
        std::fs::write(landing.join("a/b/page.md"), "p").unwrap();
        assert!(holds_files(&landing));
    }

    #[test]
    fn limits_are_bounded_and_out_of_range_is_refused() {
        assert_eq!(page_limit(None, "list").unwrap(), DEFAULT_LIMIT);
        assert_eq!(page_limit(Some(7), "list").unwrap(), 7);
        assert_eq!(
            page_limit(Some(MAX_LIMIT as u64), "list").unwrap(),
            MAX_LIMIT
        );
        assert!(page_limit(Some(MAX_LIMIT as u64 + 1), "list").is_err());
        assert!(page_limit(Some(10_000), "list").is_err());
        assert!(page_limit(Some(0), "list").is_err());
    }

    #[test]
    fn only_logical_names_describe_a_landing() {
        assert_eq!(logical_name("page.md", false), Some("page.md"));
        assert_eq!(logical_name("page.pdf.blob", false), Some("page.pdf"));
        assert_eq!(logical_name("global", true), Some("global"));
        for bad in [".hidden", ".blob", "a%b", "a\\b", "a\u{7f}b", ""] {
            assert_eq!(logical_name(bad, false), None, "admitted {bad}");
        }
        assert_eq!(
            logical_name(&"x".repeat(256), false),
            None,
            "admitted an over-long segment"
        );
        // The managed suffix is exactly one, and never a directory's:
        // `x.blob.blob` would be the page `x.blob`, which `mv` refuses.
        assert_eq!(logical_name("x.blob.blob", false), None);
        assert_eq!(logical_name("x.blob", true), None);
    }

    #[test]
    fn a_restore_accepts_one_complete_pointer_record() {
        let vault = vault();
        std::fs::write(vault.path().join(".blobs").join(DIGEST), b"hello").unwrap();
        let pointer = write_pointer(vault.path(), &pointer_body("5", ""));
        check_restore_source(vault.path(), &pointer)
            .expect("a complete record over a stored object");
    }

    #[test]
    fn a_restore_refuses_partial_or_truncated_pointer_records() {
        let vault = vault();
        std::fs::write(vault.path().join(".blobs").join(DIGEST), b"hello").unwrap();
        let truncated = pointer_body("5", "");
        let truncated = truncated.trim_end_matches('}').to_string();
        for body in [
            // No size at all, and sizes the ordinary reader refuses.
            format!("{{\"sha256\":\"{DIGEST}\",\"mime\":\"application/pdf\",\"name\":\"x.pdf\"}}"),
            pointer_body("\"5\"", ""),
            pointer_body("-5", ""),
            // Missing mime / missing name.
            format!("{{\"sha256\":\"{DIGEST}\",\"size\":5,\"name\":\"x.pdf\"}}"),
            format!("{{\"sha256\":\"{DIGEST}\",\"size\":5,\"mime\":\"application/pdf\"}}"),
            // A valid JSON prefix followed by trailing bytes.
            pointer_body("5", " {}"),
            // Truncated JSON, and a size the stored object disagrees with.
            truncated,
            pointer_body("6", ""),
            // Not a digest.
            "{\"sha256\":\"nope\",\"size\":5,\"mime\":\"application/pdf\",\"name\":\"x.pdf\"}"
                .to_string(),
        ] {
            let pointer = write_pointer(vault.path(), &body);
            assert!(
                check_restore_source(vault.path(), &pointer).is_err(),
                "admitted {body}"
            );
        }
    }

    #[test]
    fn a_restore_refuses_a_symlinked_blob_store() {
        let vault = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join(DIGEST), b"hello").unwrap();
        std::os::unix::fs::symlink(outside.path(), vault.path().join(".blobs")).unwrap();
        let pointer = write_pointer(vault.path(), &pointer_body("5", ""));
        let error = check_restore_source(vault.path(), &pointer)
            .expect_err("a symlinked store is not the vault's own")
            .to_string();
        assert!(error.contains("blob store"), "{error}");
    }

    #[test]
    fn a_restore_refuses_special_files_without_blocking() {
        let vault = vault();
        let landing = vault
            .path()
            .join(".trash")
            .join("1-00000000")
            .join("global");
        std::fs::create_dir_all(&landing).unwrap();
        let fifo = landing.join("source.blob");
        mkfifo(&fifo);
        // A blocking open would hang this proof (and the PM write lock it
        // runs under); it must refuse instead.
        let error = check_restore_source(vault.path(), &landing)
            .expect_err("a FIFO is not a page")
            .to_string();
        assert!(error.contains("special file"), "{error}");
        assert!(check_restore_source(vault.path(), &fifo).is_err());
    }

    #[test]
    fn a_restore_refuses_two_entries_with_one_logical_name() {
        let vault = vault();
        std::fs::write(vault.path().join(".blobs").join(DIGEST), b"hello").unwrap();
        let landing = vault
            .path()
            .join(".trash")
            .join("1-00000000")
            .join("global");
        std::fs::create_dir_all(&landing).unwrap();
        std::fs::write(landing.join("x"), "text").unwrap();
        std::fs::write(landing.join("x.blob"), pointer_body("5", "")).unwrap();
        let error = check_restore_source(vault.path(), &landing)
            .expect_err("`x` beside `x.blob` hides one of them")
            .to_string();
        assert!(error.contains("logical name"), "{error}");
    }

    #[test]
    fn a_restore_refuses_a_pointer_whose_logical_name_is_managed() {
        let vault = vault();
        std::fs::write(vault.path().join(".blobs").join(DIGEST), b"hello").unwrap();
        let landing = vault
            .path()
            .join(".trash")
            .join("1-00000000")
            .join("global");
        std::fs::create_dir_all(&landing).unwrap();
        let pointer = landing.join("x.blob.blob");
        std::fs::write(&pointer, pointer_body("5", "")).unwrap();
        assert!(
            check_restore_source(vault.path(), &pointer).is_err(),
            "the page `x.blob` is never addressable"
        );
        // A directory spelled `<name>.blob` is unaddressable too.
        let dir_landing = landing.join("y.blob");
        std::fs::create_dir_all(&dir_landing).unwrap();
        assert!(check_restore_source(vault.path(), &dir_landing).is_err());
    }
}
