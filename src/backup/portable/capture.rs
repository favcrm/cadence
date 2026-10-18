//! Private working-tree capture for a portable export.
//!
//! The source checkout is only read. Selection is NUL-delimited git metadata
//! (index, HEAD, non-ignored untracked names). File bytes are read from the
//! worktree, never through clean/smudge filters, and written into a fresh
//! repository. That repository's HEAD is a normal commit of those bytes so
//! the existing clean-tracker copy can export it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

const MAX_LISTING: usize = 32 * 1024 * 1024;
const MAX_ENTRIES: usize = 100_000;
const MAX_PATH_BYTES: usize = 4096;
const MAX_DEPTH: usize = 128;
const MAX_PM_YAML: u64 = 8 * 1024 * 1024;
const POLICY: &str = "tracked modifications and deletions plus non-ignored untracked regular files; hooks and clean/smudge filters are not run; not an atomic database and tracker snapshot";

/// Owned scratch directory. Removed on drop, including after a failed capture.
pub struct TrackerCapture {
    _scratch: tempfile::TempDir,
    repo: PathBuf,
    evidence: serde_json::Value,
    source: PathBuf,
    stamp: SourceStamp,
    inventory_sha256: String,
}

struct SourceStamp {
    head: String,
    head_file_sha256: String,
    index_sha256: String,
    config_sha256: String,
}

struct Rec {
    path: String,
    mode: String,
    size: u64,
    sha256: String,
}

impl TrackerCapture {
    /// Admit `source` and build a private clean snapshot of its working tree.
    pub fn new(source: &Path) -> Result<Self> {
        let stamp = stamp_source(source)?;
        let scratch = tempfile::Builder::new()
            .prefix("cad1549-capture-")
            .tempdir()?;
        let repo = scratch.path().join("repo");
        init_snapshot(&repo, scratch.path())?;
        let records = mirror_worktree(source, &repo)?;
        let inventory_sha256 = inventory_sha(&records);
        let snapshot_head = commit_snapshot(&repo, &records)?;
        let again = stamp_source(source)?;
        if !stamp.same(&again) || inventory_sha(&hash_worktree(source)?) != inventory_sha256 {
            return Err(Error::rejected("source tracker changed during capture"));
        }
        let files = records.len() as u64;
        let bytes = records.iter().try_fold(0u64, |acc, rec| {
            acc.checked_add(rec.size)
                .ok_or_else(|| Error::rejected("captured tracker byte count overflowed"))
        })?;
        let evidence = serde_json::json!({
            "source_head": stamp.head,
            "snapshot_head": snapshot_head,
            "files": files,
            "bytes": bytes,
            "inventory_sha256": inventory_sha256,
            "working_tree_policy": POLICY,
        });
        Ok(Self {
            _scratch: scratch,
            repo,
            evidence,
            source: source.to_path_buf(),
            stamp,
            inventory_sha256,
        })
    }

    /// Private snapshot checkout. Valid until this value is dropped.
    pub fn path(&self) -> &Path {
        &self.repo
    }

    /// Re-read source metadata and the selected worktree. Drift refuses.
    pub fn verify_unchanged(&self) -> Result<()> {
        let again = stamp_source(&self.source)?;
        if !self.stamp.same(&again)
            || inventory_sha(&hash_worktree(&self.source)?) != self.inventory_sha256
        {
            return Err(Error::rejected("source tracker changed during capture"));
        }
        Ok(())
    }

    /// After independent package hash verification, bind the materialized
    /// tracker's complete byte and executable-mode inventory to the capture.
    pub fn verify_package(&self, package: &Path, members: &[super::Member]) -> Result<()> {
        let mut hasher = Sha256::new();
        for member in members {
            let Some(path) = member.path.strip_prefix("tracker/") else {
                continue;
            };
            let mode = if package.join(&member.path).symlink_metadata()?.mode() & 0o111 != 0 {
                "100755"
            } else {
                "100644"
            };
            hash_record(&mut hasher, mode, member.bytes, &member.sha256, path);
        }
        if format!("{:x}", hasher.finalize()) != self.inventory_sha256 {
            return Err(Error::rejected(
                "exported tracker differs from captured source inventory",
            ));
        }
        Ok(())
    }

    /// Hashes, counts, and policy. No file contents and no config values.
    pub fn evidence(&self) -> serde_json::Value {
        self.evidence.clone()
    }
}

impl SourceStamp {
    fn same(&self, other: &Self) -> bool {
        self.head == other.head
            && self.head_file_sha256 == other.head_file_sha256
            && self.index_sha256 == other.index_sha256
            && self.config_sha256 == other.config_sha256
    }
}

/// A `[filter]` section names a clean or smudge command. The parent config
/// reader allows it, so capture refuses it before any source git process
/// and again after the snapshot is built. The error carries no command and
/// no config value.
fn refuse_source_filters(source: &Path) -> Result<()> {
    let bytes = fs::read(source.join(".git/config"))
        .map_err(|_| Error::rejected("tracker refused: unreadable .git/config"))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Error::rejected("tracker refused: non-UTF-8 .git/config"))?;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let head = rest.split(']').next().unwrap_or_default();
            let name = head
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if name == "filter" || name.starts_with("filter.") {
                return Err(Error::rejected(
                    "working-tree capture refuses Git filter configuration",
                ));
            }
        }
    }
    Ok(())
}

fn stamp_source(source: &Path) -> Result<SourceStamp> {
    super::admit_tracker_metadata(source)?;
    refuse_source_filters(source)?;
    let head = git_source(
        source,
        &["rev-parse", "--verify", "--end-of-options", "HEAD"],
    )?;
    let head = std::str::from_utf8(&head)
        .map_err(|_| Error::rejected("tracker HEAD is not hex"))?
        .trim();
    if !is_hex(head) {
        return Err(Error::rejected("tracker HEAD is not hex"));
    }
    let (index_sha256, _) = super::hash_file(&source.join(".git/index"))?;
    let (config_sha256, _) = super::hash_file(&source.join(".git/config"))?;
    Ok(SourceStamp {
        head_file_sha256: super::hash_file(&source.join(".git/HEAD"))?.0,
        head: head.to_string(),
        index_sha256,
        config_sha256,
    })
}

fn init_snapshot(repo: &Path, scratch: &Path) -> Result<()> {
    let template = scratch.join("template");
    fs::DirBuilder::new().mode(0o700).create(&template)?;
    let mut cmd = git_env(scratch);
    cmd.arg("init")
        .arg("-q")
        .arg("-b")
        .arg("snapshot")
        .arg(repo)
        .env("GIT_TEMPLATE_DIR", &template);
    run(&mut cmd, "init")?;
    // Highest-precedence attributes: status and later copies must not
    // translate the raw bytes this capture hashed.
    fs::create_dir_all(repo.join(".git/info"))?;
    super::write_private(
        &repo.join(".git/info/attributes"),
        b"* -text -filter -ident -eol -export-subst -working-tree-encoding\n",
    )?;
    let mut config = fs::OpenOptions::new()
        .append(true)
        .open(repo.join(".git/config"))?;
    config.write_all(b"\n[core]\n\tautocrlf = false\n")?;
    Ok(())
}

fn mirror_worktree(source: &Path, repo: &Path) -> Result<Vec<Rec>> {
    let paths = select_paths(source)?;
    let mut records = Vec::with_capacity(paths.len());
    for path in paths {
        if !matches!(classify_worktree(source, &path)?, Worktree::File) {
            return Err(Error::rejected("source tracker changed during capture"));
        }
        let src = source.join(&path);
        let meta = fs::symlink_metadata(&src)?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err(Error::rejected(format!(
                "tracker entry {path} is not a regular file"
            )));
        }
        if meta.nlink() > 1 {
            return Err(Error::rejected(format!(
                "tracker entry {path} is hardlinked"
            )));
        }
        let mode = if meta.mode() & 0o111 != 0 {
            "100755"
        } else {
            "100644"
        };
        let dest = repo.join(&path);
        let (sha256, size) =
            copy_hashed(&src, &dest, if mode == "100755" { 0o755 } else { 0o644 })?;
        if path == "pm.yaml" || path == "pm/pm.yaml" {
            if size > MAX_PM_YAML {
                return Err(Error::rejected(format!(
                    "tracker file {path} refused: exceeds the pm.yaml size bound"
                )));
            }
            let bytes = fs::read(&dest)?;
            super::check_tracker_content(&path, &bytes)?;
        }
        records.push(Rec {
            path,
            mode: mode.to_string(),
            size,
            sha256,
        });
    }
    Ok(records)
}

fn hash_worktree(source: &Path) -> Result<Vec<Rec>> {
    let paths = select_paths(source)?;
    let mut records = Vec::with_capacity(paths.len());
    for path in paths {
        if !matches!(
            classify_worktree(source, &path)
                .map_err(|_| { Error::rejected("source tracker changed during capture") })?,
            Worktree::File
        ) {
            return Err(Error::rejected("source tracker changed during capture"));
        }
        let src = source.join(&path);
        let meta = fs::symlink_metadata(&src)
            .map_err(|_| Error::rejected("source tracker changed during capture"))?;
        if meta.file_type().is_symlink() || !meta.is_file() || meta.nlink() > 1 {
            return Err(Error::rejected("source tracker changed during capture"));
        }
        let mode = if meta.mode() & 0o111 != 0 {
            "100755"
        } else {
            "100644"
        };
        let (sha256, size) = super::hash_file(&src)
            .map_err(|_| Error::rejected("source tracker changed during capture"))?;
        records.push(Rec {
            path,
            mode: mode.to_string(),
            size,
            sha256,
        });
    }
    Ok(records)
}

fn inventory_sha(records: &[Rec]) -> String {
    let mut hasher = Sha256::new();
    for rec in records {
        hash_record(&mut hasher, &rec.mode, rec.size, &rec.sha256, &rec.path);
    }
    format!("{:x}", hasher.finalize())
}

fn hash_record(hasher: &mut Sha256, mode: &str, size: u64, sha256: &str, path: &str) {
    hasher.update(mode.as_bytes());
    hasher.update(b" ");
    hasher.update(size.to_string().as_bytes());
    hasher.update(b" ");
    hasher.update(sha256.as_bytes());
    hasher.update(b" ");
    hasher.update(path.as_bytes());
    hasher.update(b"\0");
}

struct StageEnt {
    mode: String,
    stage: String,
}

fn select_paths(source: &Path) -> Result<Vec<String>> {
    let stage = parse_stage(&git_source(source, &["ls-files", "-z", "--stage"])?)?;
    let flags = parse_flags(&git_source(source, &["ls-files", "-z", "-v"])?)?;
    let untracked = parse_paths(&git_source(
        source,
        &["ls-files", "-z", "-o", "--exclude-standard", "--full-name"],
    )?)?;
    if stage.len() + untracked.len() > MAX_ENTRIES {
        return Err(Error::rejected("tracker listing exceeds 100000 entries"));
    }
    for (path, tag) in &flags {
        if *tag != b'H' {
            return Err(Error::rejected(format!(
                "tracker entry {path} has an unsupported index flag"
            )));
        }
    }
    for (path, ent) in &stage {
        if ent.stage != "0" {
            return Err(Error::rejected(format!("tracker entry {path} is unmerged")));
        }
        if !flags.contains_key(path) {
            return Err(Error::rejected("source tracker changed during capture"));
        }
    }
    let mut wanted = BTreeMap::<String, bool>::new();
    for path in stage.keys() {
        wanted.insert(path.clone(), false);
    }
    for path in &untracked {
        if wanted.insert(path.clone(), true).is_some() {
            return Err(Error::rejected("source tracker changed during capture"));
        }
    }
    let mut selected = Vec::new();
    for (path, is_untracked) in wanted {
        let stage_ent = stage.get(&path);
        if let Some(ent) = stage_ent {
            if ent.mode != "100644" && ent.mode != "100755" {
                if matches!(classify_worktree(source, &path)?, Worktree::Missing) {
                    continue;
                }
                return Err(Error::rejected(format!(
                    "tracker entry {path} has unsupported mode {}",
                    ent.mode
                )));
            }
        }
        match classify_worktree(source, &path)? {
            Worktree::Missing | Worktree::Replaced => {
                if is_untracked {
                    return Err(Error::rejected("source tracker changed during capture"));
                }
            }
            Worktree::File => {
                super::check_tracker_path(&path)?;
                selected.push(path);
            }
        }
    }
    Ok(selected)
}

enum Worktree {
    Missing,
    Replaced,
    File,
}

fn classify_worktree(source: &Path, path: &str) -> Result<Worktree> {
    vet_git_path(path)?;
    let mut acc = source.to_path_buf();
    let parts: Vec<&str> = path.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        acc.push(part);
        let meta = match fs::symlink_metadata(&acc) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Worktree::Missing);
            }
            Err(err) => return Err(err.into()),
        };
        if meta.file_type().is_symlink() {
            return Err(Error::rejected(format!(
                "tracker path {path} contains a symlink"
            )));
        }
        let last = i + 1 == parts.len();
        if last {
            if meta.is_file() {
                return Ok(Worktree::File);
            }
            if meta.is_dir() {
                return Ok(Worktree::Replaced);
            }
            return Err(Error::rejected(format!(
                "tracker entry {path} is not a regular file"
            )));
        } else if !meta.is_dir() {
            if meta.is_file() {
                return Ok(Worktree::Replaced);
            }
            return Err(Error::rejected(format!(
                "tracker entry {path} is not a regular file"
            )));
        }
    }
    Ok(Worktree::Missing)
}

fn vet_git_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > MAX_PATH_BYTES
        || path.as_bytes().contains(&0)
        || path.split('/').any(|part| part.is_empty())
        || path.split('/').count() > MAX_DEPTH
        || path == ".git"
        || path.starts_with(".git/")
    {
        return Err(Error::rejected(format!("unsafe tracker path {path}")));
    }
    if !super::safe_relative(Path::new(path)) {
        return Err(Error::rejected(format!("unsafe tracker path {path}")));
    }
    Ok(())
}

fn commit_snapshot(repo: &Path, records: &[Rec]) -> Result<String> {
    let oids = blob_oids(repo, records)?;
    let tree = write_tree(repo, records, &oids)?;
    let mut cmd = git_repo(repo);
    cmd.args(["commit-tree", &tree]);
    let out = run_stdin(&mut cmd, "commit-tree", b"working-tree snapshot\n")?;
    let commit = one_hex(&out)?;
    let mut update = git_repo(repo);
    update.args(["update-ref", "HEAD", &commit]);
    run(&mut update, "update-ref")?;
    let mut read = git_repo(repo);
    read.args(["read-tree", "HEAD"]);
    run(&mut read, "read-tree")?;
    Ok(commit)
}

fn blob_oids(repo: &Path, records: &[Rec]) -> Result<Vec<String>> {
    let mut oids = vec![String::new(); records.len()];
    let mut batch = Vec::new();
    for (i, rec) in records.iter().enumerate() {
        let path = repo.join(&rec.path);
        if path.as_os_str().as_bytes().contains(&b'\n') {
            oids[i] = hash_stdin(repo, &path)?;
        } else {
            batch.push(i);
        }
    }
    if batch.is_empty() {
        return Ok(oids);
    }
    let mut listed = Vec::new();
    for i in &batch {
        listed.extend_from_slice(repo.join(&records[*i].path).as_os_str().as_bytes());
        listed.push(b'\n');
    }
    let mut cmd = git_repo(repo);
    cmd.args(["hash-object", "-w", "--no-filters", "--stdin-paths"]);
    let out = run_stdin(&mut cmd, "hash-object", &listed)?;
    let lines: Vec<&str> = std::str::from_utf8(&out)
        .map_err(|_| Error::rejected("git hash-object returned a non-hex id"))?
        .lines()
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() != batch.len() {
        return Err(Error::rejected(
            "git hash-object returned an unexpected id count",
        ));
    }
    for (slot, line) in batch.into_iter().zip(lines) {
        if !is_hex(line) {
            return Err(Error::rejected("git hash-object returned a non-hex id"));
        }
        oids[slot] = line.to_string();
    }
    Ok(oids)
}

fn hash_stdin(repo: &Path, file: &Path) -> Result<String> {
    let mut cmd = git_repo(repo);
    cmd.args(["hash-object", "-w", "--no-filters", "--stdin"]);
    cmd.stdin(Stdio::from(
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(file)?,
    ));
    one_hex(&run(&mut cmd, "hash-object")?)
}

fn write_tree(repo: &Path, records: &[Rec], oids: &[String]) -> Result<String> {
    let mut index = Vec::new();
    for (rec, oid) in records.iter().zip(oids) {
        index.extend_from_slice(format!("{} {oid} 0\t", rec.mode).as_bytes());
        index.extend_from_slice(rec.path.as_bytes());
        index.push(0);
    }
    let mut update = git_repo(repo);
    update.args(["update-index", "-z", "--index-info"]);
    run_stdin(&mut update, "update-index", &index)?;
    let mut tree = git_repo(repo);
    tree.arg("write-tree");
    one_hex(&run(&mut tree, "write-tree")?)
}

fn copy_hashed(src: &Path, dest: &Path, mode: u32) -> Result<(String, u64)> {
    if let Some(parent) = dest.parent() {
        ensure_dirs(parent)?;
    }
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(src)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dest)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        output.write_all(&buf[..n])?;
        total = total
            .checked_add(n as u64)
            .ok_or_else(|| Error::rejected("captured tracker byte count overflowed"))?;
    }
    output.set_permissions(fs::Permissions::from_mode(mode))?;
    Ok((format!("{:x}", hasher.finalize()), total))
}

fn ensure_dirs(dir: &Path) -> Result<()> {
    let mut built = PathBuf::new();
    for part in dir.components() {
        built.push(part);
        match fs::symlink_metadata(&built) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(Error::rejected("snapshot directory is a symlink"));
            }
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(Error::rejected(
                    "snapshot path conflicts with a non-directory",
                ));
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                fs::DirBuilder::new().mode(0o700).create(&built)?;
            }
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}

fn parse_stage(bytes: &[u8]) -> Result<BTreeMap<String, StageEnt>> {
    let mut out = BTreeMap::new();
    for rec in nul_records(bytes) {
        let tab = rec
            .iter()
            .position(|b| *b == b'\t')
            .ok_or_else(|| Error::rejected("malformed tracker listing"))?;
        let meta = std::str::from_utf8(&rec[..tab])
            .map_err(|_| Error::rejected("malformed tracker listing"))?;
        let mut parts = meta.split(' ');
        let mode = parts.next().unwrap_or("");
        let oid = parts.next().unwrap_or("");
        let stage = parts.next().unwrap_or("");
        if parts.next().is_some() || !is_hex(oid) || mode.is_empty() || stage.is_empty() {
            return Err(Error::rejected("malformed tracker listing"));
        }
        let path = utf8_path(&rec[tab + 1..])?;
        vet_git_path(&path)?;
        if out
            .insert(
                path,
                StageEnt {
                    mode: mode.to_string(),
                    stage: stage.to_string(),
                },
            )
            .is_some()
        {
            return Err(Error::rejected("malformed tracker listing"));
        }
    }
    Ok(out)
}

fn parse_flags(bytes: &[u8]) -> Result<BTreeMap<String, u8>> {
    let mut out = BTreeMap::new();
    for rec in nul_records(bytes) {
        if rec.len() < 2 || rec[1] != b' ' {
            return Err(Error::rejected("malformed tracker listing"));
        }
        let path = utf8_path(&rec[2..])?;
        vet_git_path(&path)?;
        if out.insert(path, rec[0]).is_some() {
            return Err(Error::rejected("malformed tracker listing"));
        }
    }
    Ok(out)
}

fn parse_paths(bytes: &[u8]) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for rec in nul_records(bytes) {
        let path = utf8_path(rec)?;
        vet_git_path(&path)?;
        out.insert(path);
    }
    Ok(out)
}

fn utf8_path(bytes: &[u8]) -> Result<String> {
    String::from_utf8(bytes.to_vec()).map_err(|_| {
        Error::rejected("tracker path uses an encoding the snapshot format cannot represent")
    })
}

fn nul_records(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.split(|b| *b == 0).filter(|rec| !rec.is_empty())
}

fn is_hex(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn one_hex(bytes: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Error::rejected("git returned a non-hex id"))?
        .trim();
    if !is_hex(text) || text.contains('\n') {
        return Err(Error::rejected("git returned a non-hex id"));
    }
    Ok(text.to_string())
}

fn git_source(source: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut full = Vec::with_capacity(args.len() + 2);
    full.push("-c");
    full.push("protocol.allow=never");
    full.extend_from_slice(args);
    let out = super::git_bytes(source, &full)?;
    if out.len() > MAX_LISTING {
        return Err(Error::rejected("tracker listing exceeds 32MiB"));
    }
    Ok(out)
}

fn git_env(home: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "cadence")
        .env("GIT_AUTHOR_EMAIL", "cadence@localhost")
        .env("GIT_COMMITTER_NAME", "cadence")
        .env("GIT_COMMITTER_EMAIL", "cadence@localhost")
        .env("GIT_AUTHOR_DATE", "@0 +0000")
        .env("GIT_COMMITTER_DATE", "@0 +0000")
        .env("HOME", home);
    cmd
}

fn git_repo(repo: &Path) -> Command {
    let home = repo.parent().unwrap_or(repo);
    let mut cmd = git_env(home);
    cmd.arg("--git-dir")
        .arg(repo.join(".git"))
        .arg("--work-tree")
        .arg(repo)
        .args([
            "--no-optional-locks",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "protocol.allow=never",
            "-c",
            "core.autocrlf=false",
        ]);
    cmd
}

fn run(cmd: &mut Command, what: &str) -> Result<Vec<u8>> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let out = crate::reaper::spawn(cmd)
        .map_err(|err| Error::rejected(format!("git unavailable: {err}")))?
        .wait_with_output()
        .map_err(|err| Error::rejected(format!("git unavailable: {err}")))?;
    if !out.status.success() {
        return Err(Error::rejected(format!("git {what} failed")));
    }
    Ok(out.stdout)
}

fn run_stdin(cmd: &mut Command, what: &str, stdin: &[u8]) -> Result<Vec<u8>> {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = crate::reaper::spawn(cmd)
        .map_err(|err| Error::rejected(format!("git unavailable: {err}")))?;
    let mut pipe = child
        .stdin
        .take()
        .ok_or_else(|| Error::rejected(format!("git {what} failed")))?;
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || pipe.write_all(stdin));
        let out = child
            .wait_with_output()
            .map_err(|err| Error::rejected(format!("git unavailable: {err}")))?;
        writer
            .join()
            .map_err(|_| Error::rejected(format!("git {what} writer failed")))??;
        if !out.status.success() {
            return Err(Error::rejected(format!("git {what} failed")));
        }
        Ok(out.stdout)
    })
}
