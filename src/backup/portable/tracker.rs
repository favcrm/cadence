//! Tracker admission, metadata/config inspection, and tracked-file copy for portable packages.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::fs;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

use super::{safe_relative, write_private_mode};
use crate::error::{Error, Result};
use crate::fd_fs;

/// Admit an ordinary Git tracker before any git reader runs. The checks are
/// on metadata only: `.git` must be a real directory; no `objects/info/alternates`,
/// no `commondir`/`gitdir` redirection, no `worktrees`, `modules` or `info/grafts`,
/// no config `include`, no `hooks` entries beyond `.sample`, no symlink or
/// hardlink inside `.git`. The tracker must be clean. Returns HEAD.
pub fn admit_tracker(tracker: &Path) -> Result<String> {
    admit_tracker_metadata(tracker, Admission::Export)?;
    let status = git_plain(
        tracker,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    if !status.trim().is_empty() {
        return Err(Error::rejected(format!(
            "tracker {} refused: not clean; commit or stash before export",
            tracker.display()
        )));
    }
    git_plain(tracker, &["rev-parse", "--verify", "HEAD"])
}

/// Export refuses active hooks and `core.hooksPath`. Capture admits regular
/// installed hooks and one plain `core.hooksPath` key. The configured
/// directory is not read. Source git still forces `core.hooksPath=/dev/null`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Admission {
    Export,
    Capture,
}

pub(super) fn admit_tracker_metadata(tracker: &Path, admission: Admission) -> Result<()> {
    let refuse =
        |why: &str| Error::rejected(format!("tracker {} refused: {why}", tracker.display()));
    let dotgit = tracker.join(".git");
    let meta = fs::symlink_metadata(&dotgit)
        .map_err(|_| refuse("not an ordinary checkout (.git missing)"))?;
    if !meta.is_dir() {
        return Err(refuse(
            ".git is not a plain directory (gitfile or linked worktree)",
        ));
    }
    for banned in [
        "commondir",
        "gitdir",
        "objects/info/alternates",
        "info/grafts",
        "modules",
        "worktrees",
        "shallow",
    ] {
        if fs::symlink_metadata(dotgit.join(banned)).is_ok() {
            return Err(refuse(&format!("{banned} is present; unsupported layout")));
        }
    }
    // Export only. `is_dir` follows links, and the byte compare reads hook
    // files. Capture must not do either; the metadata walk still refuses a
    // symlink, hardlink, or special file, including inside `hooks/`.
    if admission == Admission::Export {
        let hooks = dotgit.join("hooks");
        if hooks.is_dir() && has_active_hooks(&hooks)? {
            return Err(refuse("active git hooks"));
        }
    }
    check_metadata_tree(&dotgit, tracker)?;
    check_config_keys(&dotgit.join("config"), tracker, admission)?;
    Ok(())
}

/// Read `.git/config` as keys only, before git loads it. Includes and
/// conditional includes refuse, as do keys that name a path or command git
/// would run or read elsewhere. No values enter any evidence.
///
/// Capture ignores a plain `[core]` `hooksPath` key and does not parse its
/// value. A subsection, or any other path or command key, still refuses.
fn check_config_keys(config: &Path, tracker: &Path, admission: Admission) -> Result<()> {
    let refuse =
        |why: String| Error::rejected(format!("tracker {} refused: {why}", tracker.display()));
    let text = match fs::read(config) {
        Ok(b) => b,
        Err(_) => return Err(refuse("unreadable .git/config".into())),
    };
    let text = String::from_utf8(text).map_err(|_| refuse("non-UTF-8 .git/config".into()))?;
    let mut section = String::new();
    let mut plain_section = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(head) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let mut parts = head.split_whitespace();
            let name = parts.next().unwrap_or_default().to_ascii_lowercase();
            if name == "include" || name == "includeif" {
                return Err(refuse(format!("config {name} section is not supported")));
            }
            section = name;
            plain_section = parts.next().is_none();
            continue;
        }
        let key = line
            .split(['=', ' ', '\t'])
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let full = format!("{section}.{key}");
        if admission == Admission::Capture && plain_section && full == "core.hookspath" {
            continue;
        }
        const PATH_OR_COMMAND: &[&str] = &[
            "core.worktree",
            "core.hookspath",
            "core.attributesfile",
            "core.fsmonitor",
            "core.gitproxy",
            "core.sshcommand",
            "core.askpass",
            "core.pager",
            "core.editor",
            "core.alternaterefscommand",
            "core.alternaterefsprefixes",
            "core.excludesfile",
            "core.commongitdir",
        ];
        if PATH_OR_COMMAND.contains(&full.as_str()) {
            return Err(refuse(format!(
                "config key {full} names a path or command; not supported"
            )));
        }
        if key.starts_with("include") {
            return Err(refuse(format!("config key {full} is an include directive")));
        }
    }
    Ok(())
}

/// Walk `.git` metadata without following links. Every entry must be a
/// regular file or directory owned by the current user; a symlink, device,
/// hardlinked file, or foreign owner refuses.
fn check_metadata_tree(dir: &Path, tracker: &Path) -> Result<()> {
    let me = fd_fs::geteuid();
    let mut count = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d)? {
            let entry = entry?;
            count += 1;
            if count > 10_000 {
                return Err(Error::rejected(format!(
                    "tracker {} refused: metadata inventory exceeds 10000 entries",
                    tracker.display()
                )));
            }
            let path = entry.path();
            let meta = path.symlink_metadata()?;
            if meta.file_type().is_symlink() {
                return Err(Error::rejected(format!(
                    "tracker {} refused: symlink in .git metadata",
                    tracker.display()
                )));
            }
            if meta.uid() != me {
                return Err(Error::rejected(format!(
                    "tracker {} refused: foreign-owned .git metadata",
                    tracker.display()
                )));
            }
            if meta.is_dir() {
                stack.push(path);
            } else if !meta.is_file() || meta.nlink() > 1 {
                return Err(Error::rejected(format!(
                    "tracker {} refused: special or hardlinked .git entry",
                    tracker.display()
                )));
            }
        }
    }
    Ok(())
}

/// Active hooks refuse, except the exact native-managed hooks that
/// `issue init` installs (same name, same bytes, regular single-link file).
/// Those never run here: every git call disables hooks.
fn has_active_hooks(dir: &Path) -> Result<bool> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".sample") {
            continue;
        }
        let path = entry.path();
        let meta = path.symlink_metadata()?;
        if !meta.is_file() || meta.nlink() > 1 {
            return Ok(true);
        }
        let native = crate::issue::hooks::HOOKS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, bytes)| bytes.as_bytes());
        match native {
            Some(bytes) if fs::read(&path)? == bytes => {}
            _ => return Ok(true),
        }
    }
    Ok(false)
}

/// The PM root in a copied tracker: `.` when `pm.yaml` sits at the root,
/// `pm` when it sits in a `pm/` dir, `None` when neither. Both is ambiguous.
pub(super) fn pm_root_of(tracker_dir: &Path) -> Result<Option<String>> {
    match (
        tracker_dir.join("pm.yaml").is_file(),
        tracker_dir.join("pm").join("pm.yaml").is_file(),
    ) {
        (true, true) => Err(Error::rejected(
            "tracker has pm.yaml at its root and in pm/; the PM root is ambiguous",
        )),
        (true, false) => Ok(Some(".".into())),
        (false, true) => Ok(Some("pm".into())),
        (false, false) => Ok(None),
    }
}

/// Run git against a throwaway index copy, with no user, system or global
/// config, no hooks and no fsmonitor. The tracker's own index is never
/// refreshed or written.
pub(super) fn git_plain(dir: &Path, args: &[&str]) -> Result<String> {
    let out = git_bytes(dir, args)?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// Run git and return raw stdout. No UTF-8 conversion and no trimming, so
/// a blob's bytes (including trailing newlines and non-UTF-8 content) are
/// exported exactly as committed.
///
/// `-c core.hooksPath=/dev/null` is installed before `args`, with a cleared
/// environment and a private index. Capture admits a source `core.hooksPath`
/// key only because every source git command comes through here. A later
/// `-c core.hooksPath` in `args` would replace the override.
pub(super) fn git_bytes(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let scratch = tempfile::Builder::new()
        .prefix("cad1163-git-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()?;
    let index = scratch.path().join("index");
    let real_index = dir.join(".git").join("index");
    if real_index.is_file() {
        fs::copy(&real_index, &index)?;
    }
    let mut cmd = Command::new("git");
    cmd.arg("--git-dir")
        .arg(dir.join(".git"))
        .arg("--work-tree")
        .arg(dir)
        .args([
            "--no-optional-locks",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.splitIndex=false",
        ])
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("GIT_INDEX_FILE", &index)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("HOME", scratch.path());
    let out = crate::reaper::output(&mut cmd)
        .map_err(|e| Error::rejected(format!("git unavailable: {e}")))?;
    if !out.status.success() {
        return Err(Error::rejected(format!("git {} failed", args.join(" "))));
    }
    Ok(out.stdout)
}

/// Credential-bearing tracker files: refused by name before any blob is read,
/// on export and again on restore. A tracker is never copied as a credential store.
pub(super) fn check_tracker_path(path: &str) -> Result<()> {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    let secret_name = name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name.ends_with(".p12")
        || name.ends_with(".pfx")
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
        || name.starts_with("id_ecdsa")
        || name.starts_with("id_dsa")
        || matches!(
            name.as_str(),
            ".netrc" | ".npmrc" | ".pypirc" | ".git-credentials" | "credentials" | "auth.json"
        );
    if secret_name {
        return Err(Error::rejected(format!(
            "tracker file {path} is credential-named; the portable tracker refuses it"
        )));
    }
    Ok(())
}

/// Closed decoder for a portable `pm.yaml`. Tracker metadata mirrors the
/// native `PmConfig` tree; runtime sections reuse their closed native types.
/// Unknown fields refuse at every level. Values are never copied into an
/// error: a mismatch reports only the generic reason.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // metadata fields close the schema; runtime strings are scanned
struct PortablePmConfig {
    schema: u32,
    #[serde(default)]
    statuses: Vec<String>,
    #[serde(default)]
    link_types: Vec<String>,
    #[serde(default)]
    artifact_max_bytes: u64,
    #[serde(default)]
    notes_dir: String,
    #[serde(default)]
    wiki: PortableWiki,
    #[serde(default)]
    review: PortableReview,
    #[serde(default)]
    approvals: PortableApprovals,
    #[serde(default)]
    pi: crate::pi_policy::PiPolicy,
    #[serde(default)]
    host: crate::doctor::host::HostOverrides,
    #[serde(default)]
    hosted: crate::lease::Hosted,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // validation-only: the fields close the schema, they are not read
struct PortableWiki {
    #[serde(default)]
    max_upload_bytes: u64,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // validation-only: the fields close the schema, they are not read
struct PortableReview {
    #[serde(default)]
    pair: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    never: std::collections::BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // validation-only: the fields close the schema, they are not read
struct PortableApprovals {
    #[serde(default)]
    tailnet_logins: Vec<String>,
}

/// Top-level `pm.yaml` keys (the closed `PortablePmConfig` field set). Only a
/// key name is reported, never a value.
const PM_CONFIG_KEYS: &[&str] = &[
    "schema",
    "statuses",
    "link_types",
    "artifact_max_bytes",
    "notes_dir",
    "wiki",
    "review",
    "approvals",
    "pi",
    "host",
    "hosted",
];

pub(super) fn check_tracker_content(path: &str, bytes: &[u8]) -> Result<()> {
    if path != "pm/pm.yaml" && path != "pm.yaml" {
        return Ok(());
    }
    let refuse = |why: &str| Error::rejected(format!("tracker file {path} refused: {why}"));
    let text = std::str::from_utf8(bytes).map_err(|_| refuse("not valid YAML"))?;
    let value: serde_yaml::Value =
        serde_yaml::from_str(text).map_err(|_| refuse("not valid YAML"))?;
    let map = value.as_mapping().ok_or_else(|| refuse("not a mapping"))?;
    for (key, section) in map {
        let name = key.as_str().ok_or_else(|| refuse("non-string key"))?;
        if !PM_CONFIG_KEYS.contains(&name) {
            return Err(refuse(&format!(
                "unknown top-level key {name}; credential-bearing config is not portable"
            )));
        }
        if matches!(name, "pi" | "host" | "hosted") && !section.is_mapping() {
            return Err(refuse(&format!("runtime section {name} must be a mapping")));
        }
    }
    let config = serde_yaml::from_value::<PortablePmConfig>(value).map_err(|_| {
        refuse("does not match the pm.yaml schema (unknown or mistyped key at some level)")
    })?;
    let check_text = |text: &str| -> Result<()> {
        if crate::secret::scan(text, Some(path))?
            .iter()
            .any(|finding| finding.severity == crate::secret::Severity::Block)
        {
            return Err(refuse("credential-shaped config content is not portable"));
        }
        Ok(())
    };
    // Raw text covers comments; decoded runtime strings cover YAML escapes.
    check_text(text)?;
    for value in config
        .pi
        .providers
        .iter()
        .chain(&config.pi.agentic_providers)
        .chain(&config.pi.models.allow)
        .chain(config.pi.models.master_allow.iter().flatten())
        .chain(config.pi.models.worker_allow.iter().flatten())
        .chain(config.pi.models.default.master.iter())
        .chain(config.pi.models.default.worker.iter())
        .chain(config.host.priority_lanes.iter().flatten())
        .chain(
            config
                .host
                .auto_stop_idle_secs_by_provider
                .iter()
                .flat_map(|providers| providers.keys()),
        )
        .chain(config.hosted.lease.iter())
    {
        check_text(value)?;
    }
    Ok(())
}

/// Copy the tracked regular files at HEAD (no `.git` directory is copied).
pub(super) fn copy_tracker(tracker: &Path, dst: &Path, head: &str) -> Result<()> {
    fs::DirBuilder::new().mode(0o700).create(dst)?;
    let listing = git_plain(tracker, &["ls-tree", "-r", "-z", "--full-tree", head])?;
    for record in listing.split('\0').filter(|r| !r.is_empty()) {
        let (meta, path) = record
            .split_once('\t')
            .ok_or_else(|| Error::rejected("malformed tracker listing"))?;
        let mode = meta.split(' ').next().unwrap_or_default();
        if mode != "100644" && mode != "100755" {
            return Err(Error::rejected(format!(
                "tracker entry {path} has mode {mode}; only regular files are exported"
            )));
        }
        let rel = Path::new(path);
        if !safe_relative(rel) {
            return Err(Error::rejected(format!("unsafe tracker path {path}")));
        }
        check_tracker_path(path)?;
        let blob = git_bytes(tracker, &["show", &format!("{head}:{path}")])?;
        check_tracker_content(path, &blob)?;
        let target = dst.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        write_private_mode(&target, &blob, if mode == "100755" { 0o700 } else { 0o600 })?;
    }
    Ok(())
}
