//! CAD-1420: the member assistant's private bootstrap files.
//!
//! Literal filenames only (`BOOTSTRAP.md`, `IDENTITY.md`, `SOUL.md`,
//! `USER.md`, `AGENTS.md`) inside the principal's canonical private cwd.
//! Every open is fd-relative from a trusted root with `O_NOFOLLOW`; a symlink
//! or non-regular file refuses, it is never followed or read as empty. A
//! managed section is replaced or appended while the rest of the file is kept.
//! Writes build a bounded replacement in an exclusive temp file, fsync it and
//! rename it over the target inside the same root. Nothing is truncated in
//! place. No network, no generic document framework.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::ffi::CString;
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::path::Path;

use crate::error::{Error, Result};
use crate::fd_fs::DirFd;

/// The platform-supplied, one-time setup file.
pub const BOOTSTRAP: &str = "BOOTSTRAP.md";
/// The durable profile files, each owning one managed section.
pub const IDENTITY: &str = "IDENTITY.md";
pub const SOUL: &str = "SOUL.md";
pub const USER: &str = "USER.md";
/// Platform-managed instructions: seeded once, never rewritten from chat.
pub const AGENTS: &str = "AGENTS.md";
/// Byte cap per profile field and for the whole profile object.
pub const FIELD_MAX: usize = 4096;
pub const PROFILE_MAX: usize = 16384;
/// Largest existing file read or replaced. Larger files refuse.
pub const FILE_MAX: u64 = 64 * 1024;

const BOOTSTRAP_TEMPLATE: &str = include_str!("../../agents/member-assistant/BOOTSTRAP.md");
const IDENTITY_TEMPLATE: &str = include_str!("../../agents/member-assistant/IDENTITY.md");
const SOUL_TEMPLATE: &str = include_str!("../../agents/member-assistant/SOUL.md");
const USER_TEMPLATE: &str = include_str!("../../agents/member-assistant/USER.md");
const AGENTS_TEMPLATE: &str = include_str!("../../agents/member-assistant/AGENTS.md");

/// The closed profile object a member may agree to.
///
/// `name` and `tone` are the ASSISTANT's display name and agreed way of
/// speaking (IDENTITY.md, SOUL.md). `user_name` is how the assistant addresses
/// the USER and lives in its own USER.md section — never IDENTITY.md.
/// `preferences` holds other agreed user preferences. `look` is the avatar
/// slug (`params.look`, not a file) and must be one of the catalogue's eleven.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Profile {
    pub name: Option<String>,
    pub tone: Option<String>,
    pub preferences: Option<String>,
    pub user_name: Option<String>,
    pub look: Option<String>,
}

impl Profile {
    /// Refuse unknown fields, non-strings and oversized values before any
    /// effect. Reserved managed-section markers are refused, not stored.
    /// An empty `name`/`tone`/`user_name` is the explicit clear.
    pub fn parse(object: &serde_json::Map<String, serde_json::Value>) -> Result<Self> {
        let mut total = 0usize;
        let mut out = Profile::default();
        for (key, value) in object {
            let text = value
                .as_str()
                .ok_or_else(|| Error::rejected("bootstrap profile values must be strings"))?;
            if text.len() > FIELD_MAX {
                return Err(Error::rejected("bootstrap profile field is too long"));
            }
            if text.contains("cadence:managed-") {
                return Err(Error::rejected(
                    "bootstrap profile field uses a reserved managed-section marker",
                ));
            }
            total += text.len();
            if key == "look" {
                let look = crate::agent_roles::canonical_look(text.trim()).ok_or_else(|| {
                    Error::rejected(format!(
                        "bootstrap profile look '{text}' is not one of: {}",
                        crate::agent_roles::LOOKS.join(", ")
                    ))
                })?;
                out.look = Some(look.to_string());
                continue;
            }
            let slot = match key.as_str() {
                "name" => &mut out.name,
                "tone" => &mut out.tone,
                "preferences" => &mut out.preferences,
                "user_name" => &mut out.user_name,
                other => {
                    return Err(Error::rejected(format!(
                        "bootstrap profile field '{other}' is not accepted"
                    )))
                }
            };
            let cleaned = crate::managed_section::clean(text);
            if cleaned.len() > crate::managed_section::VALUE_MAX {
                return Err(Error::rejected(
                    "bootstrap profile field is too long after escaping",
                ));
            }
            *slot = Some(cleaned);
        }
        if total > PROFILE_MAX {
            return Err(Error::rejected("bootstrap profile is too long"));
        }
        Ok(out)
    }
}

/// The committed scalar identity of one member assistant.
///
/// Read from the managed sections only: `name` from IDENTITY.md and `tone`
/// from SOUL.md. `None` means the file has no block or the agreed value is
/// empty/the seeded placeholder; a malformed block or a read failure is an
/// error, never a silent unset.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Identity {
    pub name: Option<String>,
    pub tone: Option<String>,
}

/// Read the effective identity from the assistant's private root.
pub fn read_identity(root: &Path) -> Result<Identity> {
    let identity = read_profile_file(root, IDENTITY)?;
    let soul = read_profile_file(root, SOUL)?;
    Ok(Identity {
        name: crate::managed_section::value(&identity, "name")?,
        tone: crate::managed_section::value(&soul, "tone")?,
    })
}

/// Open `root` as a directory fd, refusing a symlink at the root itself.
fn open_root(root: &Path) -> Result<DirFd> {
    DirFd::open_nofollow(root).map_err(|err| {
        if err.kind() == ErrorKind::InvalidInput {
            Error::rejected("bootstrap root contains an interior nul")
        } else {
            Error::rejected("bootstrap root is missing or a symlink")
        }
    })
}

fn literal(name: &str) -> Result<CString> {
    if !matches!(name, BOOTSTRAP | IDENTITY | SOUL | USER | AGENTS) {
        return Err(Error::rejected(
            "bootstrap file name is not in the managed set",
        ));
    }
    CString::new(name).map_err(|_| Error::rejected("bad bootstrap file name"))
}

/// Open a managed file read-only, no-follow, non-blocking, and require a
/// regular file. `Ok(None)` only for a genuinely absent file.
fn open_read(root: &DirFd, name: &str) -> Result<Option<File>> {
    let cname = literal(name)?;
    let file = match root.open_readonly_nofollow(&cname) {
        Ok(file) => file,
        Err(err) => {
            return match err.raw_os_error() {
                Some(libc::ENOENT) => Ok(None),
                Some(libc::ELOOP) => Err(Error::rejected(format!(
                    "bootstrap file '{name}' is a symlink; refused"
                ))),
                _ => Err(Error::rejected(format!(
                    "bootstrap file '{name}' could not be opened"
                ))),
            };
        }
    };
    if !file.metadata()?.file_type().is_file() {
        return Err(Error::rejected(format!(
            "bootstrap file '{name}' is not a regular file"
        )));
    }
    Ok(Some(file))
}

/// Read at most `FILE_MAX + 1` bytes; a larger file refuses, never truncates.
fn read_bounded(file: &mut File, name: &str) -> Result<String> {
    let mut bytes = Vec::new();
    Read::take(&mut *file, FILE_MAX + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > FILE_MAX {
        return Err(Error::rejected(format!(
            "bootstrap file '{name}' is over the size cap"
        )));
    }
    String::from_utf8(bytes)
        .map_err(|_| Error::rejected(format!("bootstrap file '{name}' is not UTF-8")))
}

/// Seed the profile files and the platform AGENTS.md when missing. BOOTSTRAP
/// is created only when `include_bootstrap` is true, which the caller sets
/// only while the stored state is `pending`. Exclusive create never overwrites
/// an existing file, including a symlink. Every other error propagates.
pub fn seed(root: &Path, include_bootstrap: bool) -> Result<()> {
    let dir = open_root(root)?;
    let mut files = vec![
        (IDENTITY, IDENTITY_TEMPLATE),
        (SOUL, SOUL_TEMPLATE),
        (USER, USER_TEMPLATE),
        (AGENTS, AGENTS_TEMPLATE),
    ];
    if include_bootstrap {
        files.push((BOOTSTRAP, BOOTSTRAP_TEMPLATE));
    }
    let mut created = false;
    for (name, text) in files {
        let cname = literal(name)?;
        let mut file = match dir.open_file_nofollow(
            &cname,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        ) {
            Ok(file) => file,
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(Error::rejected(format!(
                    "bootstrap seed of '{name}' failed: {err}"
                )));
            }
        };
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        created = true;
    }
    if created {
        fsync_dir(&dir)?;
    }
    Ok(())
}

/// Flush the trusted root's directory entry (a create or a rename) to disk.
/// A failure is returned, so the caller never commits state on an unflushed
/// directory entry.
fn fsync_dir(root: &DirFd) -> Result<()> {
    root.sync_all()
        .map_err(|_| Error::rejected("bootstrap directory sync failed; state is not committed"))
}

/// Replace one managed file atomically: bounded read, exclusive temp file,
/// fsync, `renameat` within the trusted root. The temp name is removed on any
/// failure. Existing content is never truncated in place.
fn replace_file(root: &DirFd, name: &str, next: &str) -> Result<()> {
    let cname = literal(name)?;
    let temp = format!(".{name}.{}.tmp", std::process::id());
    let ctemp = CString::new(temp).map_err(|_| Error::rejected("bad temp name"))?;
    let mut file = root
        .open_file_nofollow(&ctemp, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, 0o600)
        .map_err(|_| {
            Error::rejected(format!("bootstrap temp for '{name}' could not be created"))
        })?;
    let written = file
        .write_all(next.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    if written.is_err() {
        let _ = root.unlink_child(&ctemp);
        return Err(Error::rejected(format!(
            "bootstrap write of '{name}' failed"
        )));
    }
    if root.rename_child(&ctemp, &cname).is_err() {
        let _ = root.unlink_child(&ctemp);
        return Err(Error::rejected(format!(
            "bootstrap rename of '{name}' failed"
        )));
    }
    fsync_dir(root)
}

/// Write the agreed profile fields into IDENTITY/SOUL/USER. Each target must
/// already exist (seeded at runtime provision) and be a regular file; a missing
/// or symlinked target refuses rather than being created or followed.
pub fn write_profile(root: &Path, profile: &Profile) -> Result<()> {
    let dir = open_root(root)?;
    let plan: [(&str, &str, Option<&String>); 4] = [
        (IDENTITY, "name", profile.name.as_ref()),
        (SOUL, "tone", profile.tone.as_ref()),
        (USER, "preferences", profile.preferences.as_ref()),
        (USER, "user_name", profile.user_name.as_ref()),
    ];
    for (name, marker, value) in plan {
        let Some(value) = value else { continue };
        let mut file = open_read(&dir, name)?
            .ok_or_else(|| Error::rejected(format!("bootstrap file '{name}' is missing")))?;
        let current = read_bounded(&mut file, name)?;
        drop(file);
        let next = crate::managed_section::set(
            &current,
            marker,
            &crate::managed_section::line(marker, value),
        )?;
        if next.len() as u64 > FILE_MAX {
            return Err(Error::rejected(format!(
                "bootstrap file '{name}' would exceed the cap"
            )));
        }
        replace_file(&dir, name, &next)?;
    }
    Ok(())
}

/// Best-effort removal of BOOTSTRAP.md after completion is committed. A
/// failure never reopens setup: injection depends on the stored state.
pub fn remove_bootstrap(root: &Path) -> Result<()> {
    let dir = open_root(root)?;
    let cname = literal(BOOTSTRAP)?;
    match dir.unlink_child(&cname) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(_) => Err(Error::rejected(
            "bootstrap file cleanup failed (state is already committed)",
        )),
    }
}

/// Read one managed file for prompt context. Only a genuinely absent file is
/// empty. A symlink, a non-regular file, an oversized file or an I/O error is
/// returned to the caller, never silently read as empty.
pub fn read_profile_file(root: &Path, name: &str) -> Result<String> {
    let dir = open_root(root)?;
    match open_read(&dir, name)? {
        None => Ok(String::new()),
        Some(mut file) => read_bounded(&mut file, name),
    }
}
