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

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Value};

use super::{allowed, git_rel, normalize, vault_dir, Caller, Op, DEPTH_CAP};
use crate::error::{Error, Result};
use crate::issue::Pm;

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
const COUNT_CAP: usize = 10_000;
/// Commits the first history page asks for; each further page doubles,
/// up to [`EVIDENCE_SCAN_CAP`]. Paging continues while a selected key is
/// unresolved, so a window full of restored landings cannot truncate an
/// older landing's proof.
const EVIDENCE_PAGE: usize = 512;
/// Commits one listing may read in all, across pages. Reaching it with
/// an unresolved key is reported as [`EVIDENCE_TRUNCATED`] — bounded
/// work never becomes a claim that no proof exists.
const EVIDENCE_SCAN_CAP: usize = 4096;
/// Longest accepted cursor / `from` string, before decoding.
const SELECTOR_CAP: usize = 4096;
/// A blob pointer file is small JSON; never read more than this.
const POINTER_CAP: u64 = 4096;
/// The marker `wiki mkdir` writes into every directory it creates.
const STORE_MARKER: &str = ".gitkeep";

/// `wiki trash refused: …` — the verb prefix every refusal carries.
fn refused(why: impl std::fmt::Display) -> Error {
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
fn key_order(key: &str) -> (u64, String) {
    match key.split_once('-') {
        Some((stamp, nonce)) => (stamp.parse::<u64>().unwrap_or(0), nonce.to_string()),
        None => (0, key.to_string()),
    }
}

/// A lowercase hex digit — the only spelling `put_blob` writes for a
/// sha256 or a landing nonce.
fn lower_hex(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
}

/// `rm`'s landing-directory grammar, taken from the ONE definition `mv`
/// also validates a restore with: a listing must never advertise a
/// selector the restore parser rejects.
fn valid_key(key: &str) -> bool {
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
fn logical_name(name: &str, is_dir: bool) -> Option<&str> {
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

fn kind_of(is_dir: bool, blob: bool) -> &'static str {
    if is_dir {
        "dir"
    } else if blob {
        "blob"
    } else {
        "text"
    }
}

/// Why a walk refused to describe a landing.
enum WalkFault {
    /// A symlink, a non-logical name or an ambiguous pair — the tree
    /// cannot be restored as logical paths.
    Unsafe,
    /// The page cursor names an entry the tree no longer has; resuming
    /// would silently skip live rows.
    Stale,
    Io(String),
}

impl WalkFault {
    /// The `reason` code for a landing this walk could not describe.
    /// `Stale` never reaches a landing row (only a paged `items` walk
    /// seeks), so it maps to the same "cannot describe" code.
    fn reason(&self) -> &'static str {
        UNSAFE_TREE
    }

    fn into_error(self) -> Error {
        match self {
            WalkFault::Unsafe => refused(format!(
                "the landing holds a symlink or a name the wiki path grammar \
                 refuses ({UNSAFE_TREE}) — it is never restored through this API"
            )),
            WalkFault::Stale => {
                refused("the cursor no longer names a live entry — reopen the landing")
            }
            WalkFault::Io(why) => Error::internal(why),
        }
    }
}

/// Where a paged walk resumes: `Past` emits everything, `At(segs)`
/// seeks the entry at `segs` and emits everything after it (the cursor
/// node itself was already handed out).
#[derive(Clone, Copy)]
enum Seek<'a> {
    Past,
    At(&'a [String]),
}

/// One logical entry of a landing directory: the name the wiki path
/// grammar gives it, where it lives, and what it is. Only an entry a
/// listing can describe becomes one — a symlink, a special file, a name
/// the grammar refuses or a second spelling of one logical name stops
/// the level with [`WalkFault::Unsafe`].
///
/// The derived order starts at `logical` (a `String` orders byte-wise,
/// exactly like the listing), and two entries in one level never share
/// one, so it is the page's own order.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Entry {
    logical: String,
    abs: PathBuf,
    is_dir: bool,
    blob: bool,
}

impl Entry {
    fn kind(&self) -> &'static str {
        kind_of(self.is_dir, self.blob)
    }
}

/// One directory's entries, yielded one at a time and checked as they
/// come, so a directory of any width costs O(1) memory here and a
/// caller that has seen enough simply stops reading.
struct Level {
    dir: PathBuf,
    read: std::fs::ReadDir,
}

/// A text/directory entry and a pointer with the same logical name cannot
/// coexist. Only a confirmed missing twin proves the name is unambiguous.
fn has_twin(dir: &Path, logical: &str, blob: bool) -> std::io::Result<bool> {
    // A pointer spelling longer than the store's 255-byte entry-name
    // limit is not a possible logical twin. Avoid ENAMETOOLONG for a
    // perfectly valid text page or directory near that limit.
    if !blob && logical.len() > 255 - ".blob".len() {
        return Ok(false);
    }
    let twin = if blob {
        logical.to_string()
    } else {
        format!("{logical}.blob")
    };
    match dir.join(twin).symlink_metadata() {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn read_level(dir: &Path) -> std::result::Result<Level, WalkFault> {
    let read = std::fs::read_dir(dir)
        .map_err(|e| WalkFault::Io(format!("wiki trash: cannot read {}: {e}", dir.display())))?;
    Ok(Level {
        dir: dir.to_path_buf(),
        read,
    })
}

impl Iterator for Level {
    type Item = std::result::Result<Entry, WalkFault>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let entry = match self.read.next()? {
                Ok(entry) => entry,
                Err(e) => {
                    return Some(Err(WalkFault::Io(format!(
                        "wiki trash: cannot read {}: {e}",
                        self.dir.display()
                    ))));
                }
            };
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(e) => {
                    return Some(Err(WalkFault::Io(format!(
                        "wiki trash: cannot stat {}: {e}",
                        entry.path().display()
                    ))));
                }
            };
            if ft.is_symlink() {
                return Some(Err(WalkFault::Unsafe));
            }
            // Only a real directory or a regular file is a logical entry:
            // a FIFO, socket or device is neither describable nor
            // restorable.
            if !ft.is_dir() && !ft.is_file() {
                return Some(Err(WalkFault::Unsafe));
            }
            let name = match entry.file_name().into_string() {
                Ok(name) => name,
                Err(_) => return Some(Err(WalkFault::Unsafe)),
            };
            // `wiki mkdir` writes a `.gitkeep` into every directory it
            // makes (git cannot track an empty dir). It is the store's own
            // marker, invisible to `ls`, and never a page of the landing.
            if name == STORE_MARKER && ft.is_file() {
                continue;
            }
            let Some(logical) = logical_name(&name, ft.is_dir()) else {
                return Some(Err(WalkFault::Unsafe));
            };
            let blob = !ft.is_dir() && name.ends_with(".blob");
            // `x` beside `x.blob` are ONE logical name, and a listing must
            // never show one as if the other did not exist. Whichever half
            // is seen first refuses the level. Proving the absence of the
            // twin without collecting the level costs one stat per entry;
            // that is the price of bounded memory, not of order.
            match has_twin(&self.dir, logical, blob) {
                Ok(true) => return Some(Err(WalkFault::Unsafe)),
                Ok(false) => {}
                Err(error) => {
                    return Some(Err(WalkFault::Io(format!(
                        "wiki trash: cannot check a logical name in {}: {error}",
                        self.dir.display()
                    ))))
                }
            }
            return Some(Ok(Entry {
                logical: logical.to_string(),
                abs: entry.path(),
                is_dir: ft.is_dir(),
                blob,
            }));
        }
    }
}

/// One `.blob` pointer record, read exactly as the ordinary blob reader
/// requires it (`blob_view` in `wiki`): a regular, non-symlink file no
/// larger than [`POINTER_CAP`], opened no-follow/non-blocking, holding
/// ONE complete JSON object with a 64-lowercase-hex `sha256`, a
/// non-negative integer `size` and string `mime`/`name`.
///
/// A truncated record, a valid JSON prefix followed by trailing bytes, a
/// FIFO (whose read could block while a writer never comes) or a symlink
/// is an error — never a silently smaller or partially trusted pointer.
struct Pointer {
    sha256: String,
    size: u64,
}

fn read_pointer(pointer: &Path) -> std::result::Result<Pointer, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(pointer)
        .map_err(|e| format!("a blob pointer cannot be opened ({e})"))?;
    let meta = file
        .metadata()
        .map_err(|e| format!("a blob pointer cannot be read ({e})"))?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err("a blob pointer is not a regular file".to_string());
    }
    if meta.len() > POINTER_CAP {
        return Err(format!(
            "a blob pointer is {} bytes, over the {POINTER_CAP}-byte cap",
            meta.len()
        ));
    }
    let mut text = String::new();
    file.take(POINTER_CAP)
        .read_to_string(&mut text)
        .map_err(|e| format!("a blob pointer cannot be read ({e})"))?;
    let meta: Value = serde_json::from_str(&text)
        .map_err(|_| "a blob pointer is not one complete pointer record".to_string())?;
    let sha256 = meta
        .get("sha256")
        .and_then(Value::as_str)
        .filter(|sha| sha.len() == 64 && sha.bytes().all(lower_hex))
        .ok_or_else(|| "a blob pointer does not name a sha256 digest".to_string())?;
    let size = meta
        .get("size")
        .and_then(Value::as_u64)
        .ok_or_else(|| "a blob pointer has no non-negative integer size".to_string())?;
    for field in ["mime", "name"] {
        if meta.get(field).and_then(Value::as_str).is_none() {
            return Err(format!("a blob pointer '{field}' is not a string"));
        }
    }
    Ok(Pointer {
        sha256: sha256.to_string(),
        size,
    })
}

/// The size a node contributes to a landing's byte total. A blob
/// pointer's size is the stored object's, read from the pointer JSON;
/// anything unreadable is `None` — never a guess.
fn node_size(abs: &Path, kind: &str) -> Option<u64> {
    if kind == "dir" {
        return None;
    }
    if kind == "blob" {
        return read_pointer(abs).ok().map(|pointer| pointer.size);
    }
    match abs.symlink_metadata() {
        Ok(m) if m.is_file() && !m.file_type().is_symlink() => Some(m.len()),
        _ => None,
    }
}

/// Emit one node; `false` stops the walk (the page is full).
type Emit<'a> = &'a mut dyn FnMut(&str, &'static str, Option<u64>) -> bool;

/// Pre-order walk of one landing directory: sorted children, dirs
/// before their contents, no symlink followed, no subtree collected.
/// `Ok(false)` means the emit callback stopped the walk.
///
/// Each level is enumerated once, streaming, and only its smallest
/// `keep` entries past the cursor are retained: an entry emits at least
/// one row, so the smallest `keep` entries are all the walk can need to
/// emit `keep - 1` rows and prove that one more exists. The whole level
/// is still read — lexical order has no index to seek in, and the
/// level's own safety must be proved — but memory stays O(`keep`), never
/// O(the level's width).
fn walk_children(
    dir: &Path,
    base: &str,
    depth: usize,
    seek: &mut Seek<'_>,
    keep: usize,
    emit: Emit<'_>,
) -> std::result::Result<bool, WalkFault> {
    if depth > DEPTH_CAP + 2 {
        return Err(WalkFault::Unsafe);
    }
    // `At` with no segment left is `Past`: the cursor was consumed.
    if matches!(*seek, Seek::At(rest) if rest.is_empty()) {
        *seek = Seek::Past;
    }
    let at = match *seek {
        Seek::Past => None,
        Seek::At(rest) => Some(rest),
    };
    let head = at.and_then(|rest| rest.first().map(String::as_str));
    let (rows, cursor) = level_entries(dir, head, keep)?;
    if let Some(rest) = at {
        let Some(cursor) = cursor else {
            // The cursor names no entry at this level: resuming would
            // silently skip live rows.
            return Err(WalkFault::Stale);
        };
        if rest.len() > 1 && !cursor.is_dir {
            return Err(WalkFault::Stale);
        }
        // The cursor node itself was already handed out; its children
        // are the next rows in pre-order.
        *seek = Seek::Past;
        if cursor.is_dir {
            let path = format!("{base}/{}", cursor.logical);
            let mut next = if rest.len() > 1 {
                Seek::At(&rest[1..])
            } else {
                Seek::Past
            };
            if !walk_children(&cursor.abs, &path, depth + 1, &mut next, keep, &mut *emit)? {
                return Ok(false);
            }
        }
    }
    for entry in rows {
        let path = format!("{base}/{}", entry.logical);
        if !emit(&path, entry.kind(), node_size(&entry.abs, entry.kind())) {
            return Ok(false);
        }
        if entry.is_dir {
            let mut past = Seek::Past;
            if !walk_children(&entry.abs, &path, depth + 1, &mut past, keep, &mut *emit)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// The entries one level offers the walk, in byte-wise logical order,
/// plus the cursor's own entry when this level holds it.
///
/// `after` is the cursor segment this level resumes from: entries before
/// it are past for good, the entry that IS it is not a row (its children
/// are), and the rest are candidates. Only the smallest `keep` candidates
/// are retained — each emits at least one row, so no page of `keep - 1`
/// rows can need more — while the whole level is read, because lexical
/// order has no index to seek in and the level's own safety must be
/// proved.
fn level_entries(
    dir: &Path,
    after: Option<&str>,
    keep: usize,
) -> std::result::Result<(Vec<Entry>, Option<Entry>), WalkFault> {
    // A bounded max-heap: it holds at most `keep` entries and pops its
    // largest when a smaller candidate arrives, so the level costs
    // O(`keep`) memory however wide it is. `into_sorted_vec` hands the
    // page back in byte-wise logical order.
    let mut rows: BinaryHeap<Entry> = BinaryHeap::new();
    let mut cursor = None;
    for entry in read_level(dir)? {
        let entry = entry?;
        if let Some(head) = after {
            let order = entry.logical.as_str().cmp(head);
            match order {
                // Wholly before the cursor: this entry and its tree are
                // past.
                Ordering::Less => continue,
                // The cursor node itself was already handed out.
                Ordering::Equal => {
                    cursor = Some(entry);
                    continue;
                }
                Ordering::Greater => {}
            }
        }
        if rows.len() == keep {
            // Full: only a candidate smaller than the largest kept entry
            // can be a row of this page. The level is still read to its
            // end, so nothing is skipped and its safety is proved.
            if rows.peek().is_some_and(|largest| entry < *largest) {
                rows.pop();
            } else {
                continue;
            }
        }
        rows.push(entry);
    }
    Ok((rows.into_sorted_vec(), cursor))
}

/// One possible original root of a landing: the on-disk chain prefix
/// `rel`, at `abs`, with the kind that prefix would be.
#[derive(Debug)]
struct Candidate {
    rel: String,
    abs: PathBuf,
    kind: &'static str,
}

/// Why a landing's on-disk chain cannot name a root at all.
#[derive(Debug, Clone, Copy)]
enum ChainFault {
    Empty,
    Unsafe,
}

impl ChainFault {
    fn reason(&self) -> &'static str {
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
fn landing_chain(landing: &Path) -> std::result::Result<Vec<Candidate>, ChainFault> {
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
struct Proof {
    subject: String,
    at: u64,
    actor: Option<String>,
}

/// What the tracker says about ONE selected landing.
#[derive(Default)]
struct Evidence {
    /// The `rm` commit that created it, matched WHOLE against the subject
    /// `rm` writes for a candidate root — never split out of the display
    /// text, which a legal original path may itself resemble.
    proof: Option<Proof>,
    /// A restore moved something out of this landing's key
    /// (`wiki: mv .trash/<key>/…`). That label's first segment is the key,
    /// so the association is exact.
    moved: bool,
    /// The scan stopped at [`EVIDENCE_SCAN_CAP`] before this landing's
    /// proof was seen: absence of proof is not proof of absence.
    truncated: bool,
}

/// One selected landing the history must prove: its key and the
/// candidate roots its on-disk chain offers (or why the walk stopped).
struct Target<'a> {
    key: &'a str,
    chain: &'a std::result::Result<Vec<Candidate>, ChainFault>,
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
fn expected_subject(key: &str, cand: &Candidate) -> String {
    let logical = cand
        .rel
        .strip_suffix(".blob")
        .filter(|_| cand.kind == "blob")
        .unwrap_or(cand.rel.as_str());
    format!("wiki: rm {logical} → .trash/{key}/{}", cand.rel)
}

/// Whether `subject` is exactly the expected one, allowing the
/// ` (<actor>)` suffix `commit_message` appends.
fn subject_matches(subject: &str, expected: &str) -> bool {
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
fn evidence(pm: &Pm, trash: &Path, targets: &[Target<'_>]) -> HashMap<String, Evidence> {
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
fn proven_origin<'a>(
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

/// Bounded top-K of live landing keys, newest first, strictly below
/// `cursor`. Memory is the page size, not the trash's size.
fn select_keys(trash: &Path, want: usize, cursor: Option<&str>) -> Result<(Vec<String>, bool)> {
    let cursor_order = cursor.map(|key| {
        let (stamp, nonce) = key_order(key);
        (stamp, nonce, key)
    });
    let read = match std::fs::read_dir(trash) {
        Ok(read) => read,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), false)),
        Err(e) => return Err(e.into()),
    };
    let mut heap: BinaryHeap<std::cmp::Reverse<(u64, String, String)>> = BinaryHeap::new();
    for entry in read {
        let entry = entry?;
        let name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => continue,
        };
        if !valid_key(&name) {
            continue;
        }
        let ft = entry.file_type()?;
        if !ft.is_dir() || ft.is_symlink() {
            continue;
        }
        let (stamp, nonce) = key_order(&name);
        if let Some((cursor_stamp, cursor_nonce, cursor_key)) = &cursor_order {
            if (stamp, nonce.as_str(), name.as_str())
                >= (*cursor_stamp, cursor_nonce.as_str(), *cursor_key)
            {
                continue;
            }
        }
        heap.push(std::cmp::Reverse((stamp, nonce, name)));
        if heap.len() > want {
            heap.pop();
        }
    }
    let full = heap.len() == want;
    let keys: Vec<String> = heap
        .into_sorted_vec()
        .into_iter()
        .map(|std::cmp::Reverse((_, _, key))| key)
        .collect();
    Ok((keys, full))
}

/// A landing's page count and byte total, walked lazily under
/// [`COUNT_CAP`]. `None` means "not provable", never zero.
struct Totals {
    items: Option<u64>,
    size: Option<u64>,
    reason: Option<&'static str>,
}

/// The count walk's ledger. Directories and files are counted
/// separately, so a tree of empty directories costs the same budget as a
/// tree of pages and neither can spend unbounded work. The byte total is
/// only reported when every file's size was readable.
#[derive(Default)]
struct Count {
    dirs: usize,
    files: u64,
    size: u64,
    /// A file whose size could not be read: the byte total is unknown.
    size_failed: bool,
    capped: bool,
}

impl Count {
    /// Either bound reached: the walk stops and the totals stay
    /// unproven — never zero, never a fault.
    fn full(&self) -> bool {
        self.dirs >= COUNT_CAP || self.files >= COUNT_CAP as u64
    }
}

/// Count one directory's tree, streaming every level and stopping the
/// moment [`COUNT_CAP`] directories or files have been visited. Nothing
/// is collected, so memory is the depth; the work is bounded by the
/// budget even for a tree that is nothing but empty directories or one
/// enormous directory. A tree the budget cannot finish is NOT unsafe:
/// its totals are unknown and it stays listed and restorable. The
/// tradeoff is exact: beyond the budget this walk proves no safety
/// either, so a landing that large is judged — before any restore — by
/// the restore proof, which walks the whole tree under the write lock.
fn count_tree(dir: &Path, depth: usize, count: &mut Count) -> std::result::Result<(), WalkFault> {
    if depth > DEPTH_CAP + 2 {
        return Err(WalkFault::Unsafe);
    }
    if count.full() {
        count.capped = true;
        return Ok(());
    }
    count.dirs += 1;
    for entry in read_level(dir)? {
        let entry = entry?;
        if entry.is_dir {
            count_tree(&entry.abs, depth + 1, count)?;
            if count.capped {
                return Ok(());
            }
            continue;
        }
        if count.full() {
            count.capped = true;
            return Ok(());
        }
        count.files += 1;
        match node_size(&entry.abs, entry.kind()) {
            Some(n) => count.size = count.size.saturating_add(n),
            None => count.size_failed = true,
        }
    }
    Ok(())
}

fn totals(cand: &Candidate) -> Totals {
    let mut count = Count::default();
    let mut fault = None;
    if cand.kind == "dir" {
        if let Err(e) = count_tree(&cand.abs, 1, &mut count) {
            fault = Some(e.reason());
        }
    } else {
        // A page or pointer landing is one entry, read directly.
        count.files = 1;
        match node_size(&cand.abs, cand.kind) {
            Some(n) => count.size = n,
            None => count.size_failed = true,
        }
    }
    if let Some(reason) = fault {
        return Totals {
            items: None,
            size: None,
            reason: Some(reason),
        };
    }
    Totals {
        items: (!count.capped).then_some(count.files),
        size: (!count.capped && !count.size_failed).then_some(count.size),
        reason: None,
    }
}

/// A landing whose original root is not proven: it is still reported
/// (the operator must see it), but nothing about its identity is
/// invented — `from`/`path`/`kind`/`deleted_at`/`actor`/totals are
/// `null`, `restorable` is false and `reason` says why. `items` and `mv`
/// have no selector to act on, so it cannot be read
/// or restored through this API.
fn unproven_row(key: &str, reason: &'static str) -> Value {
    json!({
        "landing_id": key,
        "from": Value::Null,
        "path": Value::Null,
        "kind": Value::Null,
        "deleted_at": Value::Null,
        "actor": Value::Null,
        "item_count": Value::Null,
        "size": Value::Null,
        "restorable": false,
        "reason": reason,
    })
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
            && allowed(&Caller::Operator, Op::Write, &segs).is_ok();
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
