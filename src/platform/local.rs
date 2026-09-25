//! CAD-546 / ADR 0006: the built-in `local` platform — the connected-
//! platform contract's own proof vehicle. No network, no real
//! credential: the one declared tool `publish` is a `send`, so every
//! post lands only through the shared effect gate — staged with its
//! preview, held in Needs-you, released by the operator's press alone.
//!
//! Execution writes the approved post into the local outbox,
//! `<outbox>/<project>/<effect_id>/`:
//!
//! - `post.md` — the rendered markdown (the title as `# …`, then the
//!   body verbatim — exactly what the staged preview showed);
//! - `attachments/<name>` — byte copies of the declared attachment
//!   files, confined to the *requesting task's* worktree: an
//!   attachment path containing `..`, any symlink component, or an
//!   absolute path outside that worktree refuses the whole call;
//! - `index.json` — the publish record (effect id, digests, the
//!   outcome payload) the Outbox board view lists.
//!
//! Every directory is `0700` and every file `0600`; the item lands by
//! rename from a sibling tmp dir, so a reader never sees a partial
//! item. Re-executing under the same `effect_id` replays the recorded
//! outcome only when the input is byte-identical — a collision under
//! a different input is refused.
//!
//! The adapter holds no credential itself. Custody still rides the
//! real gate: the operator enrolls a placeholder token for
//! `local/<account>` once so grants and the credential record bind —
//! the gate's custody load and grant checks run unchanged; the bytes
//! are never read here (`execute` takes them only because the gate
//! attaches them).

use std::fs::{self, File};
use std::io::{Error as IoError, Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::contract_fixture::{ToolTable, Verified};
use crate::platform::adapter::PlatformAdapter;

/// The platform name `ServeOptions::platforms` registers this adapter
/// under — `platform_call {platform:"local", …}` reaches it.
pub const PLATFORM: &str = "local";

/// The one tool the `local` table declares — everything else gates as
/// `send` too, but `execute` refuses it: publishing is all `local` does.
const TOOL_PUBLISH: &str = "publish";

/// The reviewed tool table, declared in code — `local` is built-in, so
/// the table and its pin live with the adapter itself.
const TABLE_JSON: &str = r#"{
    "platform": "local",
    "manifest_version": "cadence-local/1",
    "tools": [
        {
            "tool": "publish",
            "effect": "send",
            "scopes": ["publish"],
            "label": "Publish a markdown post into the local outbox"
        }
    ]
}"#;

/// Bound on the markdown body an input may carry (bytes).
const BODY_CAP: usize = 256 * 1024;
/// Bound on the title (chars).
const TITLE_CAP: usize = 200;
/// Bound on the declared attachment count.
const ATTACHMENT_CAP: usize = 50;
/// Bound on one attachment's size (bytes).
const ATTACHMENT_BYTES_CAP: u64 = 32 * 1024 * 1024;
/// The basename cap the common filesystems enforce (bytes).
const NAME_CAP: usize = 255;
/// How many items `platform_outbox` lists.
const LIST_CAP: usize = 500;
/// The preview excerpt stored on an outbox item (chars).
const ITEM_PREVIEW_CAP: usize = 400;

/// The default outbox root — `~/.local/share/cadence/outbox`, the same
/// `XDG_DATA_HOME` resolution the daemon's other data paths use.
pub fn default_outbox_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|h| h.join(".local/share"))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("cadence").join("outbox")
}

/// Register the `local` adapter on `opts` for a production daemon:
/// the outbox resolves under the daemon's data dir and the board link
/// under the persisted `ui.json` port. The outbox root is also pinned
/// on `opts.outbox_dir` so the `platform_outbox` read serves exactly
/// what this adapter writes.
pub fn register(state_dir: &Path, opts: &mut crate::daemon::ServeOptions) {
    let port = crate::ui::persisted_opts(state_dir).port.unwrap_or(3010);
    register_at(
        state_dir,
        opts,
        default_outbox_dir(),
        format!("http://127.0.0.1:{port}"),
    );
}

/// [`register`] with the outbox root and board origin explicit — the
/// test seam ("the daemon's state/data-dir options"): a test daemon
/// registers here so nothing lands under the real HOME.
pub fn register_at(
    state_dir: &Path,
    opts: &mut crate::daemon::ServeOptions,
    outbox: PathBuf,
    board_url: String,
) {
    opts.outbox_dir = Some(outbox.clone());
    opts.platforms.insert(
        PLATFORM.to_string(),
        Arc::new(LocalAdapter {
            table: ToolTable::from_json(&serde_json::from_str(TABLE_JSON).expect("table parses"))
                .expect("local tool table parses"),
            state_dir: state_dir.to_path_buf(),
            outbox,
            board_url,
        }),
    );
}

/// The `local` platform adapter.
pub struct LocalAdapter {
    table: ToolTable,
    /// The daemon's state dir — the effect row names its requesting
    /// agent, and the agent row names the task worktree attachments
    /// confine to. Opened read-only at execute time.
    state_dir: PathBuf,
    /// The outbox root: `<outbox>/<project>/<effect_id>/`.
    outbox: PathBuf,
    /// The board's loopback origin (`http://127.0.0.1:<port>`) the
    /// outcome's `board_url` links into.
    board_url: String,
}

/// A parsed, shape-checked `publish` input. Attachment paths keep
/// both the raw string (for refusals) and the normalized path (for
/// confinement, resolved at execute time against the requesting
/// worktree).
struct Post {
    project: String,
    title: String,
    body: String,
    attachments: Vec<(String, PathBuf)>,
}

fn parse_post(input: &Value) -> Result<Post, String> {
    let need_str = |field: &str| -> Result<&str, String> {
        input
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("missing or non-string '{field}'"))
    };
    let project = need_str("project")?;
    crate::proto::identifier(project, "project")
        .map_err(|e| format!("'project' is not a safe outbox name: {e}"))?;
    let title = need_str("title")?;
    if title.is_empty() || title.chars().count() > TITLE_CAP {
        return Err(format!("'title' must be 1-{TITLE_CAP} chars"));
    }
    let body = need_str("body")?;
    if body.len() > BODY_CAP {
        return Err(format!("'body' must be at most {BODY_CAP} bytes"));
    }
    let mut attachments = Vec::new();
    if let Some(list) = input.get("attachments") {
        let list = list
            .as_array()
            .ok_or_else(|| "'attachments' must be an array of paths".to_string())?;
        if list.len() > ATTACHMENT_CAP {
            return Err(format!("at most {ATTACHMENT_CAP} attachments"));
        }
        for a in list {
            let s = a
                .as_str()
                .ok_or_else(|| "an attachment path must be a string".to_string())?;
            attachments.push((s.to_string(), check_path(s)?));
        }
    }
    Ok(Post {
        project: project.to_string(),
        title: title.to_string(),
        body: body.to_string(),
        attachments,
    })
}

/// The syntactic half of attachment confinement: no `..`, ever; a `.`
/// component is normalized away; an absolute path keeps its root so
/// the lexical prefix test can admit only paths under the worktree.
fn check_path(raw: &str) -> Result<PathBuf, String> {
    if raw.is_empty() {
        return Err("an attachment path is empty".to_string());
    }
    let mut out = PathBuf::new();
    for c in Path::new(raw).components() {
        match c {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            Component::RootDir => out.push(c.as_os_str()),
            Component::ParentDir | Component::Prefix(_) => {
                return Err(format!(
                    "attachment '{raw}': `..` escapes the task worktree — refused"
                ));
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err(format!("attachment '{raw}' names no file"));
    }
    Ok(out)
}

/// The rendered post — `# <title>` then the body verbatim. This is the
/// exact markdown `post.md` carries and what `preview` shows inside
/// the staged effect's operator view.
fn render_post(post: &Post) -> String {
    format!("# {}\n\n{}", post.title, post.body)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

impl LocalAdapter {
    /// The requesting task's worktree — the confinement root. The
    /// effect row's `agent` names the caller (connection-derived at
    /// stage time); its agent row holds the registered `cwd`. Never
    /// trusted from the input: the store is the only authority.
    fn requesting_worktree(&self, effect_id: &str) -> Result<PathBuf, String> {
        let conn = rusqlite::Connection::open_with_flags(
            self.state_dir.join("cadence.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|e| format!("cannot open the store to resolve the requester: {e}"))?;
        let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
        let agent: String = conn
            .query_row(
                "SELECT agent FROM platform_effects WHERE effect_id=?1",
                [effect_id],
                |r| r.get(0),
            )
            .map_err(|_| {
                format!("no platform_effects row for '{effect_id}' — the requester is unresolvable")
            })?;
        let cwd: String = conn
            .query_row("SELECT cwd FROM agents WHERE alias=?1", [&agent], |r| {
                r.get(0)
            })
            .map_err(|_| {
                format!("requesting agent '{agent}' has no record — the worktree is unresolvable")
            })?;
        let cwd = Path::new(&cwd);
        let canon = cwd
            .canonicalize()
            .map_err(|e| format!("the requesting task's worktree {cwd:?} does not resolve: {e}"))?;
        if !canon.is_dir() {
            return Err(format!(
                "the requesting task's worktree {} is not a directory",
                canon.display()
            ));
        }
        Ok(canon)
    }

    /// Resolve `rel` (or the absolute-inside-root shape) against the
    /// canonical worktree root and read it confined: every component a
    /// real directory or file — never a symlink — and the path never
    /// naming anything outside `root`.
    fn read_attachment(&self, root: &Path, rel: &Path, raw: &str) -> Result<Attachment, String> {
        let inner: PathBuf = if rel.is_absolute() {
            rel.strip_prefix(root)
                .map_err(|_| {
                    format!(
                        "attachment '{raw}': an absolute path must name a file inside \
                         the task worktree ({})",
                        root.display()
                    )
                })?
                .to_path_buf()
        } else {
            rel.to_path_buf()
        };
        if inner.as_os_str().is_empty() {
            return Err(format!(
                "attachment '{raw}' names the worktree itself, not a file"
            ));
        }
        let root_c = std::ffi::CString::new(root.as_os_str().as_bytes())
            .map_err(|_| "the task worktree path holds a NUL".to_string())?;
        let root_fd = unsafe {
            libc::open(
                root_c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if root_fd < 0 {
            return Err(format!(
                "the task worktree {} cannot be opened: {}",
                root.display(),
                IoError::last_os_error()
            ));
        }
        let file = open_confined(root_fd, &inner, raw);
        unsafe { libc::close(root_fd) };
        let mut file = file?;
        let meta = file
            .metadata()
            .map_err(|e| format!("attachment '{raw}': cannot stat: {e}"))?;
        if !meta.is_file() {
            return Err(format!("attachment '{raw}' is not a regular file"));
        }
        if meta.len() > ATTACHMENT_BYTES_CAP {
            return Err(format!(
                "attachment '{raw}' exceeds {ATTACHMENT_BYTES_CAP} bytes"
            ));
        }
        let name = inner
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| n.len() <= NAME_CAP && *n != "." && *n != "..")
            .ok_or_else(|| {
                format!("attachment '{raw}': the file name is not a safe UTF-8 basename")
            })?
            .to_string();
        let mut bytes = Vec::with_capacity(meta.len().min(1 << 20) as usize);
        file.read_to_end(&mut bytes)
            .map_err(|e| format!("attachment '{raw}': cannot read: {e}"))?;
        Ok(Attachment { name, bytes })
    }

    /// `<project>/<effect_id>` — the item's outbox dir name. The
    /// effect id is daemon-minted (`eid-…`), but it lands on the
    /// filesystem, so the grammar is checked anyway.
    fn item_dir(&self, project: &str, effect_id: &str) -> Result<PathBuf, String> {
        crate::proto::identifier(effect_id, "effect_id")
            .map_err(|e| format!("the effect id is not a safe outbox name: {e}"))?;
        Ok(self.outbox.join(project).join(effect_id))
    }

    /// Write one file `0600` inside `dir` — fresh tmp only, so
    /// `create_new` can never clobber.
    fn write_private(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
        let path = dir.join(name);
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        f.write_all(bytes)
            .and_then(|()| f.sync_all())
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("cannot set 0600 on {}: {e}", path.display()))?;
        Ok(())
    }

    /// `mkdir -p` then `0700`: a pre-existing dir is re-modeled too, so
    /// a wider mode never survives in the outbox tree.
    fn ensure_dir(path: &Path) -> Result<(), String> {
        fs::create_dir_all(path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot set 0700 on {}: {e}", path.display()))?;
        Ok(())
    }
}

/// Open `rel` under `root_fd`, refusing every symlink — each path
/// component is opened `O_NOFOLLOW` (intermediate ones `O_DIRECTORY`)
/// atomically, so a link planted or swapped in anywhere inside the
/// worktree fails the open rather than redirecting it outside.
fn open_confined(root_fd: RawFd, rel: &Path, raw: &str) -> Result<File, String> {
    let nofollow = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let dir_flags = nofollow | libc::O_DIRECTORY;
    let mut fd = root_fd;
    let comps: Vec<_> = rel.components().collect();
    for (i, c) in comps.iter().enumerate() {
        let name = match c {
            Component::Normal(n) => n,
            // `..` was refused at parse and an absolute shape was
            // already stripped — anything else is a bug in the caller.
            _ => return Err(format!("attachment '{raw}': unsafe path component")),
        };
        let cname = std::ffi::CString::new(name.as_bytes())
            .map_err(|_| format!("attachment '{raw}': NUL in a path component"))?;
        let flags = if i + 1 == comps.len() {
            nofollow
        } else {
            dir_flags
        };
        let next = unsafe { libc::openat(fd, cname.as_ptr(), flags) };
        if fd != root_fd {
            unsafe { libc::close(fd) };
        }
        if next < 0 {
            let e = IoError::last_os_error();
            let why = if e.raw_os_error() == Some(libc::ELOOP)
                || e.raw_os_error() == Some(libc::ENOTDIR)
            {
                "a symlink or non-directory is in the path — refused"
            } else {
                "cannot be opened inside the task worktree"
            };
            return Err(format!("attachment '{raw}': {why} ({e})"));
        }
        fd = next;
    }
    // `rel` is never empty (checked by the caller), so `fd` here is a
    // fresh descriptor, never `root_fd`.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// One attachment verified and read under the worktree fd.
struct Attachment {
    /// Its basename — the name it lands under in `attachments/`.
    name: String,
    bytes: Vec<u8>,
}

impl PlatformAdapter for LocalAdapter {
    fn table(&self) -> &ToolTable {
        &self.table
    }

    /// The platform is this adapter — the manifest pin is the adapter's
    /// own table version, so classification honors the declared effects.
    fn reported_manifest_version(&self) -> Option<String> {
        self.table.manifest_version.clone()
    }

    fn preview(&self, account: &str, tool: &str, input: &Value) -> String {
        if tool != TOOL_PUBLISH {
            return format!("local/{tool}: no such tool — the table declares only publish");
        }
        match parse_post(input) {
            Ok(post) => {
                let mut out = format!(
                    "Publish to the local outbox (account {account}, project {}):\n\n{}",
                    post.project,
                    render_post(&post)
                );
                if !post.attachments.is_empty() {
                    out.push_str("\n\nattachments:");
                    for (raw, _) in &post.attachments {
                        out.push_str(&format!("\n  {raw}"));
                    }
                }
                out
            }
            Err(e) => format!("publish (input will fail at execute): {e}"),
        }
    }

    fn execute(
        &self,
        _credential: &[u8],
        tool: &str,
        input: &Value,
        idempotency_key: &str,
        _expected_hash: Option<&str>,
    ) -> Result<Value, String> {
        if tool != TOOL_PUBLISH {
            return Err(format!(
                "local has no tool '{tool}' — the table declares only publish"
            ));
        }
        let post = parse_post(input)?;
        let item_dir = self.item_dir(&post.project, idempotency_key)?;
        let input_sha = sha256_hex(input.to_string().as_bytes());

        // Idempotent replay (C9): an item for this key whose recorded
        // input is byte-identical replays its recorded outcome — no
        // second write, no divergence. Any other content is a refused
        // collision, never an overwrite.
        if let Ok(text) = fs::read_to_string(item_dir.join("index.json")) {
            let index: Value = serde_json::from_str(&text)
                .map_err(|e| format!("the recorded outbox index does not parse: {e}"))?;
            if index["input_sha256"].as_str() == Some(input_sha.as_str()) {
                let result = index["result"].clone();
                return if result.is_null() {
                    Err("the recorded outbox index carries no result".to_string())
                } else {
                    Ok(result)
                };
            }
            return Err(format!(
                "outbox item {idempotency_key} exists under a different input — refusing"
            ));
        }
        if item_dir.symlink_metadata().is_ok() {
            return Err(format!(
                "outbox item {idempotency_key} exists without an index — refusing to overwrite"
            ));
        }

        let root = self.requesting_worktree(idempotency_key)?;
        let mut attachments: Vec<Attachment> = Vec::with_capacity(post.attachments.len());
        for (raw, rel) in &post.attachments {
            let att = self.read_attachment(&root, rel, raw)?;
            if attachments.iter().any(|a| a.name == att.name) {
                return Err(format!(
                    "attachments share the name '{}' — refused",
                    att.name
                ));
            }
            attachments.push(att);
        }

        let post_md = render_post(&post);
        let post_sha = sha256_hex(post_md.as_bytes());
        let att_shas: Vec<String> = attachments
            .iter()
            .map(|a| format!("{} {}", a.name, sha256_hex(&a.bytes)))
            .collect();
        let content_sha = sha256_hex(format!("{post_sha}\n{}", att_shas.join("\n")).as_bytes());
        let published_at = crate::issue::time::iso(crate::issue::time::now_epoch());
        let board_url = format!("{}/outbox?item={idempotency_key}", self.board_url);
        let result = json!({
            "platform_ref": format!("outbox/{}/{idempotency_key}", post.project),
            "board_url": board_url,
            "url": board_url,
            "path": item_dir.display().to_string(),
            "attachments": attachments.len(),
        });

        // Stage the whole item in a sibling tmp dir, then rename it
        // into place: the item dir is complete the moment it exists.
        let project_dir = item_dir.parent().unwrap().to_path_buf();
        Self::ensure_dir(&self.outbox)?;
        Self::ensure_dir(&project_dir)?;
        let tmp = project_dir.join(format!(".tmp-{idempotency_key}"));
        if let Ok(m) = tmp.symlink_metadata() {
            // Crash residue from an earlier attempt — removed, never
            // followed (a symlink goes as a file).
            let r = if m.file_type().is_symlink() || m.is_file() {
                fs::remove_file(&tmp)
            } else {
                fs::remove_dir_all(&tmp)
            };
            r.map_err(|e| format!("cannot clear {}: {e}", tmp.display()))?;
        }
        fs::create_dir(&tmp).map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot set 0700 on {}: {e}", tmp.display()))?;
        let publish = |tmp: &Path| -> Result<(), String> {
            Self::write_private(tmp, "post.md", post_md.as_bytes())?;
            if !attachments.is_empty() {
                let adir = tmp.join("attachments");
                fs::create_dir(&adir)
                    .map_err(|e| format!("cannot create {}: {e}", adir.display()))?;
                fs::set_permissions(&adir, fs::Permissions::from_mode(0o700))
                    .map_err(|e| format!("cannot set 0700 on {}: {e}", adir.display()))?;
                for a in &attachments {
                    Self::write_private(&adir, &a.name, &a.bytes)?;
                }
            }
            let index = json!({
                "effect_id": idempotency_key,
                "project": post.project,
                "title": post.title,
                "published_at": published_at,
                "post_sha256": post_sha,
                "content_sha256": content_sha,
                "input_sha256": input_sha,
                "attachments": attachments.iter().map(|a| json!({
                    "name": a.name, "sha256": sha256_hex(&a.bytes), "bytes": a.bytes.len(),
                })).collect::<Vec<_>>(),
                "result": result,
            });
            Self::write_private(tmp, "index.json", index.to_string().as_bytes())
        };
        if let Err(e) = publish(&tmp) {
            let _ = fs::remove_dir_all(&tmp);
            return Err(e);
        }
        fs::rename(&tmp, &item_dir)
            .map_err(|e| format!("cannot land {} in the outbox: {e}", item_dir.display()))?;
        Ok(result)
    }

    /// §5.4 step 6 read-back for `local`: the outbox item is the
    /// platform record — find an index for this exact input whose
    /// `post.md` still renders to the approved content and whose
    /// recorded attachments are all still byte-intact.
    fn read_back(&self, tool: &str, input: &Value) -> Verified {
        if tool != TOOL_PUBLISH {
            return Verified::False;
        }
        let Ok(post) = parse_post(input) else {
            return Verified::False;
        };
        let input_sha = sha256_hex(input.to_string().as_bytes());
        let post_sha = sha256_hex(render_post(&post).as_bytes());
        match find_intact(&self.outbox, &input_sha, &post_sha) {
            Some(true) => Verified::True,
            _ => Verified::False,
        }
    }

    /// `local` holds no reviewed source artifact — nothing to pin.
    fn source_hash(&self, _source: &str) -> Option<String> {
        None
    }
}

/// Does the item dir's content match its index — `post.md` intact and
/// every recorded attachment present with its recorded digest, with
/// nothing added or missing?
fn item_intact(dir: &Path, index: &Value, post_sha: &str) -> bool {
    let post = fs::read(dir.join("post.md")).ok();
    if post.map(|b| sha256_hex(&b)).as_deref() != Some(post_sha) {
        return false;
    }
    let recorded: Vec<&str> = index["attachments"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x["name"].as_str()).collect())
        .unwrap_or_default();
    let adir = dir.join("attachments");
    if recorded.is_empty() {
        return !adir.exists();
    }
    let mut names = Vec::new();
    for a in index["attachments"].as_array().into_iter().flatten() {
        let (Some(name), Some(sha)) = (a["name"].as_str(), a["sha256"].as_str()) else {
            return false;
        };
        let bytes = fs::read(adir.join(name)).ok();
        if bytes.map(|b| sha256_hex(&b)).as_deref() != Some(sha) {
            return false;
        }
        names.push(name.to_string());
    }
    let Ok(read) = fs::read_dir(&adir) else {
        return false;
    };
    let mut on_disk: Vec<String> = read
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    names.sort();
    on_disk.sort();
    names == on_disk
}

/// The outbox scan read-back uses: `Some(true)` = an item for
/// `input_sha` exists and verifies, `Some(false)` = it exists but
/// diverges, `None` = no item names this input.
fn find_intact(outbox: &Path, input_sha: &str, post_sha: &str) -> Option<bool> {
    let projects = fs::read_dir(outbox).ok()?;
    let mut found = None;
    for project in projects.flatten() {
        let Ok(items) = fs::read_dir(project.path()) else {
            continue;
        };
        for item in items.flatten() {
            let Some(index) = fs::read_to_string(item.path().join("index.json"))
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            else {
                continue;
            };
            if index["input_sha256"].as_str() != Some(input_sha) {
                continue;
            }
            let intact = item_intact(&item.path(), &index, post_sha);
            found = Some(found.unwrap_or(false) || intact);
        }
    }
    found
}

/// The Outbox board's read model, relayed by the daemon:
/// `GET /api/outbox` lists items, `?effect_id=` returns one item with
/// its rendered `post`. Files that do not parse are skipped — the
/// outbox may hold items from older or foreign writers.
pub fn list_items(outbox: &Path, effect_id: Option<&str>) -> crate::error::Result<Value> {
    if let Some(eid) = effect_id {
        crate::proto::identifier(eid, "effect_id")?;
        return match find_item(outbox, eid) {
            Some((dir, index)) => {
                let mut item = item_json(&dir, &index);
                item["post"] = fs::read_to_string(dir.join("post.md"))
                    .map(Value::from)
                    .unwrap_or(Value::Null);
                Ok(json!({"item": item}))
            }
            None => Err(crate::error::Error::rejected(format!(
                "no outbox item '{eid}'"
            ))),
        };
    }
    let mut items = Vec::new();
    if let Ok(projects) = fs::read_dir(outbox) {
        for project in projects.flatten() {
            let Ok(entries) = fs::read_dir(project.path()) else {
                continue;
            };
            for item in entries.flatten() {
                let Some(index) = fs::read_to_string(item.path().join("index.json"))
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                else {
                    continue;
                };
                items.push(item_json(&item.path(), &index));
            }
        }
    }
    items.sort_by(|a, b| b["published_at"].as_str().cmp(&a["published_at"].as_str()));
    items.truncate(LIST_CAP);
    Ok(json!({"items": items}))
}

/// Find the one item dir for `effect_id` (any project).
fn find_item(outbox: &Path, eid: &str) -> Option<(PathBuf, Value)> {
    for project in fs::read_dir(outbox).ok()?.flatten() {
        let dir = project.path().join(eid);
        let Some(index) = fs::read_to_string(dir.join("index.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        else {
            continue;
        };
        if index["effect_id"].as_str() == Some(eid) {
            return Some((dir, index));
        }
    }
    None
}

/// The JSON one outbox item reports to the board — the index's own
/// fields plus a bounded `preview` excerpt of the post and the item's
/// path and board link lifted from the recorded result (operator-only
/// read; the path is the operator's own machine).
fn item_json(dir: &Path, index: &Value) -> Value {
    let post = fs::read_to_string(dir.join("post.md")).unwrap_or_default();
    let mut preview: String = post.chars().take(ITEM_PREVIEW_CAP).collect();
    if post.chars().count() > ITEM_PREVIEW_CAP {
        preview.push('…');
    }
    let mut item = index.clone();
    if let Some(o) = item.as_object_mut() {
        o.insert("preview".into(), Value::from(preview));
        o.insert("path".into(), Value::from(dir.display().to_string()));
        if let Some(url) = o
            .get("result")
            .and_then(|r| r.get("board_url"))
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            o.insert("board_url".into(), Value::from(url));
        }
        o.remove("result");
        o.remove("input_sha256");
    }
    item
}
