//! `cadence upgrade` (CAD-334): install the exact build CI tested on main.
//!
//! On a push to `main`, after every gate passed on that sha, CI's
//! `release-artifact` job builds (with `contents: read` only) and
//! `release-attest` attests the binary and uploads
//! `cadence-<sha>-x86_64-linux`: the binary, `cadence.sha256` and
//! `manifest.json`. Since CAD-409 the gates of a queued merge ran in the
//! merge_group run of that same sha (the merge queue moves `main` to the
//! exact commit it tested), and the push run skips them; a direct push
//! still runs every gate in its push run.
//!
//! Before anything is installed, this module proves, in order:
//! 1. the sha is on `main` (GitHub compare: `identical` or `ahead`);
//! 2. CI's `test` job passed on that exact sha: in a push run on `main`
//!    (a direct push, and every build before CAD-409), or else in a
//!    merge_group run on a `gh-readonly-queue/main/*` ref whose head is
//!    that sha (a queued merge);
//! 3. the push run on `main` for that sha still holds the artifact (not
//!    missing, not expired);
//! 4. the downloaded binary hashes to the value in `cadence.sha256`;
//! 5. `manifest.json`'s `source_sha` is the requested sha;
//! 6. `gh attestation verify` accepts the binary for this repo, signed by
//!    `ci.yml`, built from `refs/heads/main` at that sha;
//! 7. only then is the binary run: `--version` must end in `+<sha>`.
//!
//! The install writes `<releases>/<sha>/cadence` (0755) through a temp
//! file and a rename, re-hashes the persisted copy, then repoints the
//! `cadence` symlink by creating a new link at a temp name and renaming it
//! over the old one. `rename(2)` replaces the name atomically, so the link
//! never goes missing; the link is re-hashed through and pointed back if
//! it does not resolve to the verified bytes.
//!
//! A release already on disk is reused only when it is proven to be the
//! CI build (recorded checksum/manifest, then the attestation, before it
//! is ever run); `--latest-main` also requires its CI manifest, and
//! otherwise downloads and replaces it. `--latest-main` never moves the
//! link backwards. An explicit `--sha` rollback to an unattested release
//! works offline or with `allow_unattested`, and is labelled
//! [`TRUST_UNATTESTED`]. A `<sha>` entry that is not a real directory, or
//! a file in it that is not a regular file (a symlink out of the release
//! tree, say), is refused rather than reused or installed through.
//!
//! Restarting the daemon is never automatic. GitHub access sits behind
//! [`ReleaseSource`] so tests run against a fake and never call GitHub.

use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// The repository whose CI builds and attests the binary.
pub const DEFAULT_REPO: &str = "favcrm/cadence";
/// The workflow file that builds, tests and attests.
pub const WORKFLOW: &str = "ci.yml";
/// The only branch whose builds are installable.
pub const MAIN: &str = "main";
/// Platform suffix of the artifact name.
pub const TARGET: &str = "x86_64-linux";
/// The CI job that must have passed on the exact sha.
pub const TEST_JOB: &str = "test";
/// Ref prefix of the merge queue's temporary branches for `main`; a
/// merge_group run on one of them is test evidence for its head sha.
pub const QUEUE_REF_PREFIX: &str = "gh-readonly-queue/main/";
/// Files inside the artifact and inside `<releases>/<sha>/`.
pub const BINARY: &str = "cadence";
pub const SHA_FILE: &str = "cadence.sha256";
pub const MANIFEST: &str = "manifest.json";
/// The restart `--restart` runs with the newly installed binary, and the
/// one printed without it: the same lease-gated path as
/// `overview::CMD_RESTART_WHEN_IDLE`.
pub const RESTART_ARGS: [&str; 4] = ["daemon", "restart", "--when-idle", "--ui"];

/// `cadence-<sha>-x86_64-linux`, the name CI uploads under.
pub fn artifact_name(sha: &str) -> String {
    format!("cadence-{sha}-{TARGET}")
}

/// A full lowercase 40-hex commit id — never an abbreviation, a ref
/// name, or anything a shell or `gh` could read as an option.
pub fn is_full_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The restart command printed when `--restart` was not given.
pub fn restart_command(as_identity: Option<&str>) -> String {
    let mut cmd = format!("cadence {}", RESTART_ARGS.join(" "));
    if let Some(id) = as_identity {
        cmd.push_str(&format!(" --as {id}"));
    }
    cmd
}

/// One CI workflow run, as `gh run list --json` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub id: u64,
    pub attempt: u64,
    pub head_sha: String,
    pub head_branch: String,
    pub event: String,
    pub status: String,
    pub conclusion: String,
}

/// One job of a run (latest attempt).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub name: String,
    pub status: String,
    pub conclusion: String,
}

/// Where a sha sits relative to `main`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnMain {
    /// `main` equals or descends from the sha.
    Yes,
    /// GitHub knows the commit, but `main` does not contain it; carries
    /// the compare status (`behind` or `diverged`).
    No(String),
    /// GitHub does not know the commit at all.
    Unknown,
}

/// Whether a run still holds the named artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactState {
    Present,
    Expired,
    Missing,
}

/// Everything `upgrade` needs from GitHub. [`Gh`] shells out to the
/// `gh` CLI; tests pass a fake.
pub trait ReleaseSource {
    /// `owner/name` — the attestation is verified against it.
    fn repo(&self) -> &str;
    /// Refuse early, with the fix, when `gh` is missing or logged out.
    fn check_auth(&self) -> Result<()>;
    /// Newest successful push run of the workflow on `main`.
    fn latest_green_main(&self) -> Result<Option<Run>>;
    fn on_main(&self, sha: &str) -> Result<OnMain>;
    /// GitHub compare `base...head`: `ahead` when `head` descends from
    /// `base`, `behind` when `base` descends from `head`, `identical`,
    /// `diverged`; `None` when GitHub does not know one of them.
    fn compare(&self, base: &str, head: &str) -> Result<Option<String>>;
    /// Push runs of the workflow on `main` for exactly this sha.
    fn main_runs(&self, sha: &str) -> Result<Vec<Run>>;
    /// merge_group runs of the workflow whose head is exactly this sha
    /// (any `gh-readonly-queue/*` branch; the caller filters).
    fn merge_group_runs(&self, sha: &str) -> Result<Vec<Run>>;
    fn jobs(&self, run_id: u64) -> Result<Vec<Job>>;
    fn artifact(&self, run_id: u64, name: &str) -> Result<ArtifactState>;
    /// Download the artifact's files into `dest`.
    fn download(&self, run_id: u64, name: &str, dest: &Path) -> Result<()>;
    /// Verify the build-provenance attestation of `binary` for
    /// [`Self::repo`], built from `sha` on `main`. Returns a short
    /// description of what was verified.
    fn verify_attestation(&self, binary: &Path, sha: &str) -> Result<String>;
    /// CAD-561: the PR titles merged between `base` and `head` — what
    /// `cadence update --check` shows as the change summary. A squash
    /// merge's subject is the PR title with its number, which is what
    /// GitHub's compare endpoint returns; a source that cannot answer
    /// returns an empty list rather than failing the check.
    fn merged_titles(&self, base: &str, head: &str) -> Result<Vec<String>>;
    /// CAD-561: `SCHEMA_VERSION` in `src/rollout.rs` at `sha`, so
    /// `--check` can say whether the new build crosses the store's
    /// schema without downloading or running it. `None` when the file
    /// or the constant cannot be read.
    fn schema_version(&self, sha: &str) -> Result<Option<i64>>;
}

// ---------------------------------------------------------------------------
// gh-backed source
// ---------------------------------------------------------------------------

/// [`ReleaseSource`] over the GitHub CLI. Every call is bounded.
pub struct Gh {
    pub program: PathBuf,
    pub repo: String,
}

const GH_TIMEOUT: Duration = Duration::from_secs(60);
const GH_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

impl Gh {
    pub fn new(repo: &str) -> Self {
        Self {
            program: PathBuf::from("gh"),
            repo: repo.to_string(),
        }
    }

    fn run(&self, args: &[&str], timeout: Duration) -> Result<Output> {
        let mut cmd = Command::new(&self.program);
        cmd.args(args).env("GH_PROMPT_DISABLED", "1");
        crate::proc::run_bounded(&mut cmd, timeout).map_err(|e| match e {
            crate::proc::BoundedError::Spawn(err) if err.kind() == ErrorKind::NotFound => {
                Error::rejected(
                    "gh (GitHub CLI) is not on PATH — install it and run `gh auth login`; \
                     `cadence upgrade` downloads CI artifacts through gh",
                )
            }
            other => Error::internal(format!("gh {}: {other}", args.join(" "))),
        })
    }

    /// Run and require exit 0; the error carries gh's stderr.
    fn run_ok(&self, args: &[&str], timeout: Duration) -> Result<Vec<u8>> {
        let out = self.run(args, timeout)?;
        if out.status.success() {
            return Ok(out.stdout);
        }
        Err(Error::internal(format!(
            "gh {} failed: {}",
            args.join(" "),
            tail(&out.stderr)
        )))
    }

    /// `gh run list` of [`WORKFLOW`] for one event, optionally on one
    /// branch.
    fn runs(&self, event: &str, branch: Option<&str>, extra: &[&str]) -> Result<Vec<Run>> {
        let mut args = vec![
            "run",
            "list",
            "--repo",
            &self.repo,
            "--workflow",
            WORKFLOW,
            "--event",
            event,
            "--json",
            "databaseId,attempt,headSha,headBranch,event,status,conclusion",
        ];
        if let Some(branch) = branch {
            args.extend_from_slice(&["--branch", branch]);
        }
        args.extend_from_slice(extra);
        let out = self.run_ok(&args, GH_TIMEOUT)?;
        parse_runs(&out)
    }
}

fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    let start = text.len().saturating_sub(600);
    let start = (start..=text.len())
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(text.len());
    text[start..].to_string()
}

/// Parse `gh run list --json databaseId,attempt,headSha,...`.
pub fn parse_runs(bytes: &[u8]) -> Result<Vec<Run>> {
    let v: Value = serde_json::from_slice(bytes)
        .map_err(|e| Error::internal(format!("unreadable gh run list output: {e}")))?;
    let runs = v
        .as_array()
        .ok_or_else(|| Error::internal("gh run list did not return a list"))?;
    Ok(runs
        .iter()
        .filter_map(|r| {
            Some(Run {
                id: r["databaseId"].as_u64()?,
                attempt: r["attempt"].as_u64().unwrap_or(1),
                head_sha: r["headSha"].as_str()?.to_string(),
                head_branch: r["headBranch"].as_str().unwrap_or_default().to_string(),
                event: r["event"].as_str().unwrap_or_default().to_string(),
                status: r["status"].as_str().unwrap_or_default().to_string(),
                conclusion: r["conclusion"].as_str().unwrap_or_default().to_string(),
            })
        })
        .collect())
}

/// Parse `gh run view --json jobs`.
pub fn parse_jobs(bytes: &[u8]) -> Result<Vec<Job>> {
    let v: Value = serde_json::from_slice(bytes)
        .map_err(|e| Error::internal(format!("unreadable gh run view output: {e}")))?;
    Ok(v["jobs"]
        .as_array()
        .map(|jobs| {
            jobs.iter()
                .map(|j| Job {
                    name: j["name"].as_str().unwrap_or_default().to_string(),
                    status: j["status"].as_str().unwrap_or_default().to_string(),
                    conclusion: j["conclusion"].as_str().unwrap_or_default().to_string(),
                })
                .collect()
        })
        .unwrap_or_default())
}

impl ReleaseSource for Gh {
    fn repo(&self) -> &str {
        &self.repo
    }

    fn check_auth(&self) -> Result<()> {
        let out = self.run(&["auth", "status", "--hostname", "github.com"], GH_TIMEOUT)?;
        if out.status.success() {
            return Ok(());
        }
        Err(Error::rejected(format!(
            "gh is not authenticated for github.com — run `gh auth login` \
             (`cadence upgrade` downloads CI artifacts through gh): {}",
            tail(&out.stderr)
        )))
    }

    fn latest_green_main(&self) -> Result<Option<Run>> {
        Ok(self
            .runs("push", Some(MAIN), &["--status", "success", "--limit", "1"])?
            .into_iter()
            .next())
    }

    fn on_main(&self, sha: &str) -> Result<OnMain> {
        Ok(match self.compare(sha, MAIN)? {
            Some(status) if matches!(status.as_str(), "identical" | "ahead") => OnMain::Yes,
            Some(status) => OnMain::No(status),
            None => OnMain::Unknown,
        })
    }

    fn compare(&self, base: &str, head: &str) -> Result<Option<String>> {
        let path = format!("repos/{}/compare/{base}...{head}", self.repo);
        let out = self.run(&["api", &path, "--jq", ".status"], GH_TIMEOUT)?;
        if out.status.success() {
            return Ok(Some(
                String::from_utf8_lossy(&out.stdout).trim().to_string(),
            ));
        }
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("404") || err.contains("Not Found") || err.contains("No common ancestor") {
            return Ok(None);
        }
        Err(Error::internal(format!(
            "gh api {path} failed: {}",
            tail(&out.stderr)
        )))
    }

    fn main_runs(&self, sha: &str) -> Result<Vec<Run>> {
        self.runs("push", Some(MAIN), &["--commit", sha, "--limit", "20"])
    }

    fn merge_group_runs(&self, sha: &str) -> Result<Vec<Run>> {
        self.runs("merge_group", None, &["--commit", sha, "--limit", "20"])
    }

    fn jobs(&self, run_id: u64) -> Result<Vec<Job>> {
        let id = run_id.to_string();
        let out = self.run_ok(
            &["run", "view", &id, "--repo", &self.repo, "--json", "jobs"],
            GH_TIMEOUT,
        )?;
        parse_jobs(&out)
    }

    fn artifact(&self, run_id: u64, name: &str) -> Result<ArtifactState> {
        let path = format!(
            "repos/{}/actions/runs/{run_id}/artifacts?name={name}",
            self.repo
        );
        let out = self.run_ok(&["api", &path], GH_TIMEOUT)?;
        let v: Value = serde_json::from_slice(&out)
            .map_err(|e| Error::internal(format!("unreadable artifact list: {e}")))?;
        let found = v["artifacts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|a| a["name"].as_str() == Some(name))
            .collect::<Vec<_>>();
        Ok(if found.is_empty() {
            ArtifactState::Missing
        } else if found.iter().all(|a| a["expired"].as_bool() == Some(true)) {
            ArtifactState::Expired
        } else {
            ArtifactState::Present
        })
    }

    fn download(&self, run_id: u64, name: &str, dest: &Path) -> Result<()> {
        let id = run_id.to_string();
        let dest = dest.to_string_lossy();
        self.run_ok(
            &[
                "run", "download", &id, "--repo", &self.repo, "--name", name, "--dir", &dest,
            ],
            GH_DOWNLOAD_TIMEOUT,
        )
        .map_err(|e| {
            Error::rejected(format!(
                "could not download artifact {name} from run {run_id} \
                 (expired, deleted, or no access): {e}"
            ))
        })?;
        Ok(())
    }

    fn verify_attestation(&self, binary: &Path, sha: &str) -> Result<String> {
        let signer = format!("{}/.github/workflows/{WORKFLOW}", self.repo);
        let source_ref = format!("refs/heads/{MAIN}");
        let bin = binary.to_string_lossy();
        let args = [
            "attestation",
            "verify",
            &bin,
            "--repo",
            &self.repo,
            "--signer-workflow",
            &signer,
            "--source-ref",
            &source_ref,
            "--source-digest",
            sha,
            "--deny-self-hosted-runners",
        ];
        let out = self.run(&args, GH_TIMEOUT)?;
        if out.status.success() {
            return Ok(format!(
                "build provenance verified: repo {}, signer {signer}, {source_ref} at {sha}",
                self.repo
            ));
        }
        Err(Error::rejected(format!(
            "attestation did not verify for {} — refusing to install: {}",
            self.repo,
            tail(&out.stderr)
        )))
    }

    /// The compare endpoint's commit subjects, filtered to the squash
    /// merges that carry a PR number (`CAD-123: title (#456)`) — the
    /// merged PR titles between the two builds, newest last.
    fn merged_titles(&self, base: &str, head: &str) -> Result<Vec<String>> {
        let path = format!("repos/{}/compare/{base}...{head}", self.repo);
        let out = self.run(
            &[
                "api",
                &path,
                "--jq",
                r#"[.commits[].commit.message | split("\n")[0]] | .[]"#,
            ],
            GH_TIMEOUT,
        )?;
        if !out.status.success() {
            return Ok(Vec::new());
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| is_merged_pr_subject(line))
            .map(str::to_string)
            .collect())
    }

    /// `src/rollout.rs` at that sha, read as raw content, parsed for
    /// `pub const SCHEMA_VERSION: i64 = N;`. Best effort: an API failure
    /// (or a sha GitHub does not serve) answers `None`, and `--check`
    /// says the schema could not be read.
    fn schema_version(&self, sha: &str) -> Result<Option<i64>> {
        let path = format!("repos/{}/contents/src/rollout.rs?ref={sha}", self.repo);
        let out = self.run(
            &["api", "-H", "Accept: application/vnd.github.raw", &path],
            GH_TIMEOUT,
        )?;
        if !out.status.success() {
            return Ok(None);
        }
        Ok(parse_schema_version(&String::from_utf8_lossy(&out.stdout)))
    }
}

/// A squash-merge commit subject: the PR title followed by `(#N)`.
pub fn is_merged_pr_subject(subject: &str) -> bool {
    let trimmed = subject.trim_end();
    trimmed.ends_with(')')
        && trimmed.rfind("(#").is_some_and(|at| {
            trimmed[at + 2..trimmed.len() - 1]
                .bytes()
                .all(|b| b.is_ascii_digit())
        })
}

/// The first `pub const SCHEMA_VERSION: i64 = <n>;` in `src/rollout.rs`.
pub fn parse_schema_version(source: &str) -> Option<i64> {
    source.lines().find_map(|line| {
        let rest = line
            .trim()
            .strip_prefix("pub const SCHEMA_VERSION: i64 = ")?;
        rest.trim().trim_end_matches(';').trim().parse().ok()
    })
}

// ---------------------------------------------------------------------------
// install layout
// ---------------------------------------------------------------------------

/// Where releases live and which symlink puts one on `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// `<data>/cadence/releases` — one `<sha>/cadence` per release.
    pub releases: PathBuf,
    /// `~/.local/bin/cadence` — a symlink to one release's binary.
    pub link: PathBuf,
}

impl Layout {
    /// Explicit paths win. Otherwise the link is `$HOME/.local/bin/cadence`,
    /// and the releases dir is read off the link's current target when it
    /// has the `<dir>/<40-hex>/cadence` shape (the layout the live install
    /// already uses) or the `<dir>/v<version>/cadence` shape `install.sh`
    /// writes (CAD-311), else `$XDG_DATA_HOME/cadence/releases`, else
    /// `$HOME/.local/share/cadence/releases`.
    pub fn detect(link: Option<PathBuf>, releases: Option<PathBuf>) -> Result<Self> {
        let home = || {
            std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(PathBuf::from)
                .ok_or_else(|| Error::internal("HOME is not set — pass --link and --releases-dir"))
        };
        let link = match link {
            Some(link) => link,
            None => home()?.join(".local/bin").join(BINARY),
        };
        let releases = match releases {
            Some(dir) => dir,
            None => match fs::read_link(&link).ok().and_then(|t| releases_dir_of(&t)) {
                Some(dir) => dir,
                None => match std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
                    Some(data) => PathBuf::from(data).join("cadence/releases"),
                    None => home()?.join(".local/share/cadence/releases"),
                },
            },
        };
        Ok(Self { releases, link })
    }

    pub fn release_dir(&self, sha: &str) -> PathBuf {
        self.releases.join(sha)
    }

    pub fn binary(&self, sha: &str) -> PathBuf {
        self.release_dir(sha).join(BINARY)
    }
}

/// `<dir>/<40-hex>/cadence` → `(<dir>, sha)`.
fn release_of(target: &Path) -> Option<(PathBuf, String)> {
    if target.file_name()? != BINARY {
        return None;
    }
    let sha_dir = target.parent()?;
    let sha = sha_dir.file_name()?.to_str()?;
    if !is_full_sha(sha) {
        return None;
    }
    Some((sha_dir.parent()?.to_path_buf(), sha.to_string()))
}

/// A release tag as `install.sh` names its directory: `v` and a version
/// made of ASCII alphanumerics and `.+_-`, starting with a digit.
pub fn is_version_tag(s: &str) -> bool {
    s.strip_prefix('v').is_some_and(|v| {
        v.starts_with(|c: char| c.is_ascii_digit())
            && v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'_' | b'-'))
    })
}

/// The releases dir holding a link target: `<dir>/<40-hex>/cadence` (an
/// upgrade) or `<dir>/v<version>/cadence` (an `install.sh` release).
pub(crate) fn releases_dir_of(target: &Path) -> Option<PathBuf> {
    if let Some((dir, _)) = release_of(target) {
        return Some(dir);
    }
    if target.file_name()? != BINARY {
        return None;
    }
    let tag_dir = target.parent()?;
    if !is_version_tag(tag_dir.file_name()?.to_str()?) {
        return None;
    }
    Some(tag_dir.parent()?.to_path_buf())
}

/// What the link points at now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Current {
    /// No link yet.
    Absent,
    /// A symlink; `sha` when the target is a release under `releases`.
    Link {
        target: PathBuf,
        sha: Option<String>,
    },
    /// A regular file or directory — never replaced.
    NotSymlink,
}

pub fn current(layout: &Layout) -> Result<Current> {
    match fs::symlink_metadata(&layout.link) {
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Current::Absent),
        Err(e) => Err(e.into()),
        Ok(m) if !m.file_type().is_symlink() => Ok(Current::NotSymlink),
        Ok(_) => {
            let target = fs::read_link(&layout.link)?;
            let sha = release_of(&target)
                .filter(|(dir, _)| dir == &layout.releases)
                .map(|(_, sha)| sha);
            Ok(Current::Link { target, sha })
        }
    }
}

// ---------------------------------------------------------------------------
// the upgrade itself
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Sha(String),
    LatestMain,
}

#[derive(Debug, Clone)]
pub struct Request {
    pub target: Target,
    pub dry_run: bool,
    /// Explicit `--sha` only: accept a release already on disk whose
    /// attestation does not verify (a hand-built release) as a rollback
    /// target. It is reported as an unattested local release.
    pub allow_unattested: bool,
    /// The state dir whose store is backed up (CAD-314,
    /// `backup::before_self_update`) before the link moves. A failed
    /// backup refuses the upgrade before anything is installed. `None`
    /// skips it (tests of the install path alone).
    pub backup_state_dir: Option<PathBuf>,
}

/// `trust` in the report: what the installed binary is known to be.
pub const TRUST_ATTESTED: &str = "attested CI build";
pub const TRUST_UNATTESTED: &str = "unattested local release";

/// Resolve, verify, and (unless `dry_run`) install and repoint. Returns
/// the JSON report; restarting is the caller's separate, explicit step.
///
/// A release already under `releases/` is reused only when it proves to
/// be the CI build: recorded checksum and manifest match, and the
/// attestation verifies. `--latest-main` also needs a CI manifest (with
/// `run_id`); anything less is replaced by a fresh download. An explicit
/// `--sha` rollback may still use an unattested release when `gh` is
/// unavailable (offline) or with `allow_unattested`, and the report then
/// says [`TRUST_UNATTESTED`], never the tested build.
/// The pre-update backup must still be on disk and still verify when the
/// install starts: `before_self_update` returning `Ok` is not enough (its
/// own retention could remove what it just wrote). A skipped backup (no
/// store yet) has nothing to check.
fn verify_backup_pair(backup: &Value) -> Result<()> {
    if backup.get("skipped").is_some() {
        return Ok(());
    }
    let (Some(db), Some(manifest)) = (backup["db"].as_str(), backup["manifest"].as_str()) else {
        return Err(Error::rejected(
            "upgrade refused before installing: the pre-update backup reported no db/manifest",
        ));
    };
    for path in [db, manifest] {
        let regular = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file());
        if !regular {
            return Err(Error::rejected(format!(
                "upgrade refused before installing: the pre-update backup {path} is missing \
                 right after it was taken; nothing was installed. Check `cadence backup` \
                 retention in its directory, then retry"
            )));
        }
    }
    crate::backup::verify(Path::new(manifest)).map_err(|e| {
        Error::rejected(format!(
            "upgrade refused before installing: the pre-update backup {manifest} no longer \
             verifies ({e}); nothing was installed"
        ))
    })?;
    Ok(())
}

pub fn run(src: &dyn ReleaseSource, layout: &Layout, req: &Request) -> Result<Value> {
    if let Target::Sha(sha) = &req.target {
        if !is_full_sha(sha) {
            return Err(Error::rejected(format!(
                "--sha must be a full 40-character lowercase commit id, got `{sha}` \
                 (copy it from `gh run list` or `git rev-parse`)"
            )));
        }
    }
    let now = current(layout)?;
    if now == Current::NotSymlink {
        return Err(Error::rejected(format!(
            "{} exists and is not a symlink — refusing to replace it. Move it into \
             {}/<sha>/cadence (or aside), then rerun",
            layout.link.display(),
            layout.releases.display()
        )));
    }
    let from_sha = match &now {
        Current::Link { sha, .. } => sha.clone(),
        _ => None,
    };
    let from_target = match &now {
        Current::Link { target, .. } => Some(target.clone()),
        _ => None,
    };

    let mut verified = serde_json::Map::new();
    let (sha, explicit, online) = match &req.target {
        Target::Sha(sha) => (sha.clone(), true, src.check_auth()),
        Target::LatestMain => {
            src.check_auth()?;
            let run = src.latest_green_main()?.ok_or_else(|| {
                Error::rejected(format!(
                    "no successful `{WORKFLOW}` push run on {MAIN} in {} — nothing to install; \
                     check `gh run list --workflow {WORKFLOW} --branch {MAIN}`",
                    src.repo()
                ))
            })?;
            if !is_full_sha(&run.head_sha) {
                return Err(Error::internal(format!(
                    "run {} reports head sha `{}`, not a full commit id",
                    run.id, run.head_sha
                )));
            }
            verified.insert(
                "resolved".into(),
                json!(format!(
                    "--latest-main → newest successful {MAIN} run {} ({})",
                    run.id, run.head_sha
                )),
            );
            refuse_backwards(src, from_sha.as_deref(), &run.head_sha)?;
            (run.head_sha, false, Ok(()))
        }
    };

    check_release_entries(layout, &sha)?;
    let installed_path = layout.binary(&sha);
    let mut trust = TRUST_ATTESTED;
    let mut digest = None;
    // Why an on-disk release was not reused, when it was not.
    let mut local_rejected: Option<String> = None;
    if installed_path.is_file() {
        match local_release(src, layout, &sha, explicit, &online, req, &mut verified)? {
            Local::Use {
                digest: d,
                trust: t,
            } => {
                digest = Some(d);
                trust = t;
            }
            Local::Replace(reason) => {
                verified.insert(
                    "local_release".into(),
                    json!(format!(
                        "not reused ({reason}); the CI build is downloaded to replace it"
                    )),
                );
                local_rejected = Some(reason);
            }
        }
    }

    let source = if digest.is_some() {
        "installed-release"
    } else {
        "ci-artifact"
    };
    let mut run_id = None;
    // Holds the downloaded files until they are installed or dropped.
    let mut staged: Option<tempfile::TempDir> = None;
    if digest.is_none() {
        let fetched = online
            .map_err(|e| Error::rejected(e.to_string()))
            .and_then(|()| {
                let id = verify_ci(src, &sha, &mut verified)?;
                let tmp = tempfile::Builder::new()
                    .prefix("cadence-upgrade-")
                    .tempdir()?;
                src.download(id, &artifact_name(&sha), tmp.path())?;
                let d = verify_download(src, tmp.path(), &sha, &mut verified)?;
                Ok((id, tmp, d))
            });
        let (id, tmp, d) = match (fetched, &local_rejected) {
            (Ok(v), _) => v,
            (Err(e), Some(reason)) if explicit => {
                return Err(Error::rejected(format!(
                    "{e}. The release already at {} is not an attested CI build ({reason}). \
                     To roll back to it anyway, rerun with --allow-unattested; it will be \
                     reported as an {TRUST_UNATTESTED}",
                    installed_path.display()
                )));
            }
            (Err(e), _) => return Err(e),
        };
        run_id = Some(id);
        digest = Some(d);
        staged = Some(tmp);
    }
    let digest = digest.expect("set by the local or the CI path");
    if trust == TRUST_UNATTESTED {
        verified.insert(
            "download".into(),
            json!("skipped — release already installed"),
        );
    }

    let link_changed = from_target.as_deref() != Some(installed_path.as_path());
    let mut installed = false;
    let mut repointed = false;
    // CAD-314: a self-update takes a verified backup of the store before
    // anything is installed or the link moves. A failed backup refuses.
    let mut backup = Value::Null;
    if !req.dry_run && link_changed {
        if let Some(state_dir) = &req.backup_state_dir {
            backup = crate::backup::before_self_update(state_dir).map_err(|e| {
                Error::rejected(format!(
                    "upgrade refused before installing: the pre-update backup of {} \
                     failed: {e}",
                    state_dir.display()
                ))
            })?;
            verify_backup_pair(&backup)?;
        }
    }
    if !req.dry_run {
        if let Some(tmp) = &staged {
            install_files(tmp.path(), &layout.release_dir(&sha))?;
            installed = true;
            // The persisted copy, not just the staged one, must be the
            // verified bytes before the link moves to it.
            let persisted = sha256_file(&installed_path)?;
            if persisted != digest {
                return Err(Error::rejected(format!(
                    "installed copy {} hashes to {persisted}, not the verified {digest} — \
                     the link was not moved; remove that file and rerun",
                    installed_path.display()
                )));
            }
        }
        repointed = repoint(&layout.link, &installed_path)?;
        confirm_link(&layout.link, &digest, from_target.as_deref())?;
    }

    let mut report = json!({
        "dry_run": req.dry_run,
        "repo": src.repo(),
        "from_sha": from_sha,
        "from_target": from_target,
        "to_sha": sha,
        "source": source,
        "trust": trust,
        "run_id": run_id,
        "artifact": (source == "ci-artifact").then(|| artifact_name(&sha)),
        "installed_path": installed_path,
        "link": layout.link,
        "installed": installed,
        "repointed": repointed,
        "would_install": req.dry_run && source == "ci-artifact",
        "would_repoint": req.dry_run && link_changed,
        "verified": Value::Object(verified),
        "backup": backup,
        "restarted": false,
    });
    if trust == TRUST_UNATTESTED {
        report["warning"] = json!(format!(
            "{TRUST_UNATTESTED}: {sha} is NOT verified as the tested CI build — \
             its build-provenance attestation was not checked or did not verify"
        ));
    }
    Ok(report)
}

/// `--latest-main` never moves the link backwards: when the resolved sha
/// is an ancestor of the currently linked one, refuse and point at an
/// explicit `--sha` for a deliberate downgrade.
fn refuse_backwards(src: &dyn ReleaseSource, from: Option<&str>, to: &str) -> Result<()> {
    let Some(from) = from.filter(|f| *f != to) else {
        return Ok(());
    };
    if src.compare(to, from)?.as_deref() == Some("ahead") {
        return Err(Error::rejected(format!(
            "--latest-main resolved {to}, which is older than the linked {from} (an \
             ancestor on {MAIN}) — refusing to move backwards. The newest green run may \
             still be pending for newer commits; to downgrade on purpose, pass \
             --sha {to} explicitly"
        )));
    }
    Ok(())
}

enum Local {
    /// Reuse the on-disk release: its digest and how far it is trusted.
    Use { digest: String, trust: &'static str },
    /// Not proven to be the CI build — download and replace it.
    Replace(String),
}

/// Decide whether the release already at `<releases>/<sha>/cadence` can be
/// reused. Recorded checksum and manifest are checked first; the binary is
/// attested before it is ever executed.
fn local_release(
    src: &dyn ReleaseSource,
    layout: &Layout,
    sha: &str,
    explicit: bool,
    online: &Result<()>,
    req: &Request,
    verified: &mut serde_json::Map<String, Value>,
) -> Result<Local> {
    let mut found = serde_json::Map::new();
    let (digest, ci_manifest) = match local_records(layout, sha, &mut found) {
        Ok(v) => v,
        // A recorded checksum or manifest that no longer matches is
        // tampering; a named rollback stops, --latest-main replaces it.
        Err(e) if explicit => return Err(e),
        Err(e) => return Ok(Local::Replace(e.to_string())),
    };
    if !explicit && !ci_manifest {
        return Ok(Local::Replace(
            "no CI manifest with a run_id — not installed from CI".into(),
        ));
    }
    let binary = layout.binary(sha);
    let trust = match online {
        Err(why) => {
            // Only reachable for an explicit --sha: --latest-main needs gh.
            found.insert(
                "attestation".into(),
                json!(format!("skipped: offline ({why})")),
            );
            TRUST_UNATTESTED
        }
        Ok(()) => match src.verify_attestation(&binary, sha) {
            Ok(desc) => {
                found.insert("attestation".into(), json!(desc));
                TRUST_ATTESTED
            }
            Err(e) if explicit && req.allow_unattested => {
                found.insert("attestation".into(), json!(format!("failed: {e}")));
                TRUST_UNATTESTED
            }
            Err(e) => return Ok(Local::Replace(format!("attestation did not verify: {e}"))),
        },
    };
    found.insert("version".into(), json!(check_version(&binary, sha)?));
    verified.extend(found);
    if trust == TRUST_ATTESTED {
        verified.insert(
            "download".into(),
            json!("skipped — attested release already installed"),
        );
    }
    Ok(Local::Use { digest, trust })
}

/// Steps 1–3: on main, `test` passed on that exact sha (push run, else
/// merge_group run), artifact present. Returns the push run that holds
/// the artifact.
fn verify_ci(
    src: &dyn ReleaseSource,
    sha: &str,
    verified: &mut serde_json::Map<String, Value>,
) -> Result<u64> {
    match src.on_main(sha)? {
        OnMain::Yes => {
            verified.insert("on_main".into(), json!(true));
        }
        OnMain::No(status) => {
            return Err(Error::rejected(format!(
                "{sha} is not on {MAIN} in {} (compare status: {status}) — only builds of \
                 merged commits are installable; pick a sha from `git log origin/{MAIN}`",
                src.repo()
            )));
        }
        OnMain::Unknown => {
            return Err(Error::rejected(format!(
                "{sha} is not a commit GitHub knows in {} — not on {MAIN}; check the sha",
                src.repo()
            )));
        }
    }
    let mut runs: Vec<Run> = src
        .main_runs(sha)?
        .into_iter()
        .filter(|r| r.head_sha == sha && r.head_branch == MAIN && r.event == "push")
        .collect();
    if runs.is_empty() {
        return Err(Error::rejected(format!(
            "no `{WORKFLOW}` push run on {MAIN} for {sha} — only the push run builds the \
             release artifact; pull-request and merge-queue runs hold none. Wait for the \
             post-merge run (`gh run list --workflow {WORKFLOW} --branch {MAIN} --commit {sha}`)"
        )));
    }
    runs.sort_by_key(|r| std::cmp::Reverse(r.id));
    let mut seen = Vec::new();
    // The push run whose `test` passed (a direct push, or any build from
    // before CAD-409) is both the evidence and the artifact's run.
    let mut evidence = None;
    for run in &runs {
        match test_passed(src, run)? {
            Ok(()) => {
                evidence = Some((
                    run,
                    format!(
                        "{TEST_JOB} success in push run {} attempt {}",
                        run.id, run.attempt
                    ),
                ));
                break;
            }
            Err(state) => seen.push(format!("run {}: {state}", run.id)),
        }
    }
    // CAD-409: a queued merge skips the gates in its push run; the
    // merge_group run that tested this exact sha is the evidence, and the
    // newest push run holds the artifact.
    if evidence.is_none() {
        let mut queued: Vec<Run> = src
            .merge_group_runs(sha)?
            .into_iter()
            .filter(|r| {
                r.head_sha == sha
                    && r.event == "merge_group"
                    && r.head_branch.starts_with(QUEUE_REF_PREFIX)
            })
            .collect();
        queued.sort_by_key(|r| std::cmp::Reverse(r.id));
        if queued.is_empty() {
            seen.push(format!(
                "no merge_group run on {QUEUE_REF_PREFIX}* for this sha"
            ));
        }
        for run in &queued {
            match test_passed(src, run)? {
                Ok(()) => {
                    evidence = Some((
                        &runs[0],
                        format!(
                            "{TEST_JOB} success in merge_group run {} attempt {} ({}); \
                             artifact from push run {}",
                            run.id, run.attempt, run.head_branch, runs[0].id
                        ),
                    ));
                    break;
                }
                Err(state) => seen.push(format!("merge_group run {}: {state}", run.id)),
            }
        }
    }
    let Some((run, how)) = evidence else {
        return Err(Error::rejected(format!(
            "CI's `{TEST_JOB}` job has not passed for {sha} on {MAIN} ({}) — refusing to \
             install an untested build; wait for the run or pick a green sha",
            seen.join("; ")
        )));
    };
    verified.insert("test_job".into(), json!(how));
    let name = artifact_name(sha);
    match src.artifact(run.id, &name)? {
        ArtifactState::Present => {}
        ArtifactState::Expired => {
            return Err(Error::rejected(format!(
                "artifact {name} of run {} has expired (CI keeps it 90 days) — pick a newer \
                 sha, or roll back to a release already under the releases dir",
                run.id
            )));
        }
        ArtifactState::Missing => {
            return Err(Error::rejected(format!(
                "run {} has no artifact {name} — the `release-artifact`/`release-attest` jobs \
                 did not run or did not finish (builds before CAD-334 have none); check \
                 `gh run view {}`",
                run.id, run.id
            )));
        }
    }
    Ok(run.id)
}

/// `Ok` when the run's latest attempt has a successful [`TEST_JOB`];
/// otherwise the state seen, for the refusal.
fn test_passed(src: &dyn ReleaseSource, run: &Run) -> Result<std::result::Result<(), String>> {
    let jobs = src.jobs(run.id)?;
    Ok(match jobs.iter().find(|j| j.name == TEST_JOB) {
        Some(j) if j.conclusion == "success" => Ok(()),
        Some(j) if j.conclusion.is_empty() => Err(format!("{TEST_JOB} ({})", j.status)),
        Some(j) => Err(format!("{TEST_JOB} {}", j.conclusion)),
        None => Err(format!("no `{TEST_JOB}` job")),
    })
}

/// Steps 4–7 on the downloaded files.
fn verify_download(
    src: &dyn ReleaseSource,
    dir: &Path,
    sha: &str,
    verified: &mut serde_json::Map<String, Value>,
) -> Result<String> {
    for file in [BINARY, SHA_FILE, MANIFEST] {
        if !dir.join(file).is_file() {
            return Err(Error::rejected(format!(
                "artifact {} is missing {file} — refusing to install",
                artifact_name(sha)
            )));
        }
    }
    let binary = dir.join(BINARY);
    let digest = check_sha256(&binary, &dir.join(SHA_FILE))?;
    verified.insert("sha256".into(), json!(digest));
    let manifest = check_manifest(&dir.join(MANIFEST), sha, &digest)?;
    verified.insert("manifest_source_sha".into(), json!(sha));
    verified.insert("manifest".into(), manifest);
    // The zip does not keep the mode; the attestation covers bytes only.
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))?;
    let attestation = src.verify_attestation(&binary, sha)?;
    verified.insert("attestation".into(), json!(attestation));
    // Only an attested binary is ever executed.
    verified.insert("version".into(), json!(check_version(&binary, sha)?));
    Ok(digest)
}

/// An installed release's recorded checksum and manifest, when present.
/// Returns its digest and whether a CI manifest (with `run_id`) was
/// recorded. Older, hand-installed releases have neither file; they are
/// reported as unrecorded rather than silently "verified". Never runs it.
fn local_records(
    layout: &Layout,
    sha: &str,
    verified: &mut serde_json::Map<String, Value>,
) -> Result<(String, bool)> {
    let dir = layout.release_dir(sha);
    let binary = dir.join(BINARY);
    let sha_file = dir.join(SHA_FILE);
    let digest = if sha_file.is_file() {
        let digest = check_sha256(&binary, &sha_file)?;
        verified.insert("sha256".into(), json!(digest));
        digest
    } else {
        let digest = sha256_file(&binary)?;
        verified.insert(
            "sha256".into(),
            json!(format!("{digest} (no recorded {SHA_FILE} to compare)")),
        );
        digest
    };
    let manifest = dir.join(MANIFEST);
    let ci_manifest = if manifest.is_file() {
        let m = check_manifest(&manifest, sha, &digest)?;
        let from_ci = m["run_id"].as_u64().is_some();
        verified.insert("manifest".into(), m);
        verified.insert("manifest_source_sha".into(), json!(sha));
        from_ci
    } else {
        verified.insert("manifest".into(), json!("absent — not installed from CI"));
        false
    };
    Ok((digest, ci_manifest))
}

/// CAD-379: `<releases>/<sha>` must be a real directory and each file the
/// upgrade reads or writes in it a regular file (`symlink_metadata`, not
/// following links). Verified bytes reached through a symlink live outside
/// the release tree, where the link would keep chasing whatever that path
/// holds later; so such an entry is refused before it is attested, run,
/// written through or linked, and left for the operator to remove.
fn check_release_entries(layout: &Layout, sha: &str) -> Result<()> {
    let dir = layout.release_dir(sha);
    let entries = std::iter::once((dir.clone(), true))
        .chain([BINARY, SHA_FILE, MANIFEST].map(|file| (dir.join(file), false)));
    for (path, want_dir) in entries {
        let kind = match fs::symlink_metadata(&path) {
            // No release dir: nothing under it to check.
            Err(e) if e.kind() == ErrorKind::NotFound && want_dir => return Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
            Ok(m) => m.file_type(),
        };
        let what = if kind.is_symlink() {
            "is a symlink"
        } else if want_dir && !kind.is_dir() {
            "is not a directory"
        } else if !want_dir && !kind.is_file() {
            "is not a regular file"
        } else {
            continue;
        };
        return Err(Error::rejected(format!(
            "release entry {} {what} — refusing to use or install through it; a release \
             must be a real {}/<sha>/ directory holding the files themselves. Remove it and \
             rerun",
            path.display(),
            layout.releases.display()
        )));
    }
    Ok(())
}

/// After the swap, the link must resolve to the verified bytes. If it does
/// not, point it back at `previous` (or remove it when there was none) and
/// refuse.
pub fn confirm_link(link: &Path, digest: &str, previous: Option<&Path>) -> Result<()> {
    let actual = sha256_file(link)?;
    if actual == digest {
        return Ok(());
    }
    let restored = match previous {
        Some(prev) => repoint(link, prev).map(|_| format!("pointed back at {}", prev.display())),
        None => fs::remove_file(link)
            .map(|()| "removed".to_string())
            .map_err(Error::from),
    };
    Err(Error::rejected(format!(
        "{} resolves to bytes hashing {actual}, not the verified {digest} — the link was {}",
        link.display(),
        match restored {
            Ok(what) => what,
            Err(e) => format!("NOT restored ({e}); fix it by hand"),
        }
    )))
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Compare the binary's digest with the first field of a `sha256sum` line.
fn check_sha256(binary: &Path, sha_file: &Path) -> Result<String> {
    let recorded = fs::read_to_string(sha_file)?;
    let expected = recorded
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let actual = sha256_file(binary)?;
    if expected.len() != 64 || expected != actual {
        return Err(Error::rejected(format!(
            "sha256 mismatch for {}: {} records `{expected}`, the file hashes to {actual} — \
             refusing to install",
            binary.display(),
            sha_file.display()
        )));
    }
    Ok(actual)
}

fn check_manifest(path: &Path, sha: &str, digest: &str) -> Result<Value> {
    let text = fs::read_to_string(path)?;
    let manifest: Value = serde_json::from_str(&text).map_err(|e| {
        Error::rejected(format!(
            "{} is not valid JSON ({e}) — refusing to install",
            path.display()
        ))
    })?;
    let source = manifest["source_sha"].as_str().unwrap_or_default();
    if source != sha {
        return Err(Error::rejected(format!(
            "manifest source_sha `{source}` is not the requested {sha} — the artifact was \
             built from another commit; refusing to install"
        )));
    }
    if let Some(recorded) = manifest["sha256"].as_str() {
        if recorded != digest {
            return Err(Error::rejected(format!(
                "manifest sha256 `{recorded}` does not match the binary ({digest}) — \
                 refusing to install"
            )));
        }
    }
    Ok(manifest)
}

/// `<binary> --version` must name the exact commit: `cadence X+<sha>`.
fn check_version(binary: &Path, sha: &str) -> Result<String> {
    // A just-written file can be briefly "text file busy" while a
    // concurrently forked child still holds its write descriptor; that is
    // transient, so retry it a few times before refusing.
    let mut attempts = 0;
    let out = loop {
        let mut cmd = Command::new(binary);
        cmd.arg("--version");
        match crate::proc::run_bounded(&mut cmd, Duration::from_secs(20)) {
            Err(crate::proc::BoundedError::Spawn(e))
                if e.raw_os_error() == Some(libc::ETXTBSY) && attempts < 20 =>
            {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                return Err(Error::rejected(format!(
                    "{} --version did not run ({e}) — refusing to install",
                    binary.display()
                )));
            }
            Ok(out) => break out,
        }
    };
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || !version.ends_with(&format!("+{sha}")) {
        return Err(Error::rejected(format!(
            "{} --version reports `{version}`, expected a build of {sha} — refusing to install",
            binary.display()
        )));
    }
    Ok(version)
}

// ---------------------------------------------------------------------------
// filesystem steps
// ---------------------------------------------------------------------------

/// Copy the verified files into `<releases>/<sha>/`, each through a temp
/// file in that directory plus a rename. The binary goes last, so a
/// present `cadence` always has its checksum and manifest beside it.
fn install_files(staged: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;
    for (file, mode) in [(MANIFEST, 0o644), (SHA_FILE, 0o644), (BINARY, 0o755)] {
        write_atomic(&staged.join(file), &dest.join(file), mode)?;
    }
    Ok(())
}

fn write_atomic(from: &Path, to: &Path, mode: u32) -> Result<()> {
    let dir = to
        .parent()
        .ok_or_else(|| Error::internal(format!("{} has no parent", to.display())))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".upgrade-")
        .tempfile_in(dir)?;
    std::io::copy(&mut fs::File::open(from)?, tmp.as_file_mut())?;
    tmp.as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    tmp.as_file().sync_all()?;
    tmp.persist(to).map_err(|e| Error::from(e.error))?;
    Ok(())
}

/// Point `link` at `target` without a moment where `link` is missing: a
/// new symlink is made at a temp name in the same directory and renamed
/// over the old one (`rename(2)` replaces atomically). Returns false when
/// the link already points there. Refuses to replace a non-symlink.
pub fn repoint(link: &Path, target: &Path) -> Result<bool> {
    match fs::symlink_metadata(link) {
        Ok(m) if !m.file_type().is_symlink() => {
            return Err(Error::rejected(format!(
                "{} is not a symlink — refusing to replace it",
                link.display()
            )));
        }
        Ok(_) if fs::read_link(link)? == target => return Ok(false),
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let dir = link
        .parent()
        .ok_or_else(|| Error::internal(format!("{} has no parent", link.display())))?;
    fs::create_dir_all(dir)?;
    let name = link
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| BINARY.to_string());
    let tmp = dir.join(format!(
        ".{name}.upgrade-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    symlink(target, &tmp)?;
    if let Err(e) = fs::rename(&tmp, link) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(true)
}

#[cfg(test)]
mod backup_pair_tests {
    use super::*;

    #[test]
    fn cad396_a_missing_pre_update_backup_refuses_the_install() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("cadence-pre-update-x.sqlite3");
        let manifest = dir.path().join("cadence-pre-update-x.manifest.json");
        fs::write(&manifest, b"{}").unwrap();
        let backup = json!({"backup": true, "db": db, "manifest": manifest});
        let err = verify_backup_pair(&backup).unwrap_err().to_string();
        assert!(err.contains("missing"), "{err}");
        // Present but not a verified pair: refused too.
        fs::write(&db, b"x").unwrap();
        let err = verify_backup_pair(&backup).unwrap_err().to_string();
        assert!(err.contains("no longer verifies"), "{err}");
        // No store yet: nothing to check.
        verify_backup_pair(&json!({"skipped": "no database"})).unwrap();
    }
}
