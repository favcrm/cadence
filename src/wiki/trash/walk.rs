//! Trash tree walking, `.blob` pointer reading, and bounded total counting.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::super::DEPTH_CAP;
use super::history::Candidate;
use super::{
    key_order, logical_name, lower_hex, refused, valid_key, COUNT_CAP, POINTER_CAP, STORE_MARKER,
    UNSAFE_TREE,
};
use crate::error::{Error, Result};

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
pub(super) enum WalkFault {
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

    pub(super) fn into_error(self) -> Error {
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
pub(super) enum Seek<'a> {
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
pub(super) fn has_twin(dir: &Path, logical: &str, blob: bool) -> std::io::Result<bool> {
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
pub(super) struct Pointer {
    pub(super) sha256: String,
    pub(super) size: u64,
}

pub(super) fn read_pointer(pointer: &Path) -> std::result::Result<Pointer, String> {
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
pub(super) fn node_size(abs: &Path, kind: &str) -> Option<u64> {
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
pub(super) type Emit<'a> = &'a mut dyn FnMut(&str, &'static str, Option<u64>) -> bool;

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
pub(super) fn walk_children(
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

/// Bounded top-K of live landing keys, newest first, strictly below
/// `cursor`. Memory is the page size, not the trash's size.
pub(super) fn select_keys(
    trash: &Path,
    want: usize,
    cursor: Option<&str>,
) -> Result<(Vec<String>, bool)> {
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
pub(super) struct Totals {
    pub(super) items: Option<u64>,
    pub(super) size: Option<u64>,
    pub(super) reason: Option<&'static str>,
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

pub(super) fn totals(cand: &Candidate) -> Totals {
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
pub(super) fn unproven_row(key: &str, reason: &'static str) -> Value {
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
