//! `cadence upgrade` (CAD-334): install the exact build CI tested on main.
//!
//! CI's `release-artifact` job runs only on a push to `main`, after every
//! gate passed on that sha. It uploads `cadence-<sha>-x86_64-linux`
//! holding the binary, `cadence.sha256` and `manifest.json`, and a
//! build-provenance attestation over the binary.
//!
//! Before anything is installed, this module proves, in order:
//! 1. the sha is on `main` (GitHub compare: `identical` or `ahead`);
//! 2. a CI push run on `main` for that exact sha has a successful `test` job;
//! 3. that run still holds the artifact (not missing, not expired);
//! 4. the downloaded binary hashes to the value in `cadence.sha256`;
//! 5. `manifest.json`'s `source_sha` is the requested sha;
//! 6. `gh attestation verify` accepts the binary for this repo, signed by
//!    `ci.yml`, built from `refs/heads/main` at that sha;
//! 7. only then is the binary run: `--version` must end in `+<sha>`.
//!
//! The install writes `<releases>/<sha>/cadence` (0755) through a temp
//! file and a rename, then repoints the `cadence` symlink by creating a
//! new link at a temp name and renaming it over the old one. `rename(2)`
//! replaces the name atomically, so the link never goes missing. Earlier
//! releases are kept: `--sha <previous>` for a release that is already on
//! disk verifies it locally and only repoints the link — no download.
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
    /// Push runs of the workflow on `main` for exactly this sha.
    fn main_runs(&self, sha: &str) -> Result<Vec<Run>>;
    fn jobs(&self, run_id: u64) -> Result<Vec<Job>>;
    fn artifact(&self, run_id: u64, name: &str) -> Result<ArtifactState>;
    /// Download the artifact's files into `dest`.
    fn download(&self, run_id: u64, name: &str, dest: &Path) -> Result<()>;
    /// Verify the build-provenance attestation of `binary` for
    /// [`Self::repo`], built from `sha` on `main`. Returns a short
    /// description of what was verified.
    fn verify_attestation(&self, binary: &Path, sha: &str) -> Result<String>;
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

    fn runs(&self, extra: &[&str]) -> Result<Vec<Run>> {
        let mut args = vec![
            "run",
            "list",
            "--repo",
            &self.repo,
            "--workflow",
            WORKFLOW,
            "--branch",
            MAIN,
            "--event",
            "push",
            "--json",
            "databaseId,attempt,headSha,headBranch,event,status,conclusion",
        ];
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
            .runs(&["--status", "success", "--limit", "1"])?
            .into_iter()
            .next())
    }

    fn on_main(&self, sha: &str) -> Result<OnMain> {
        let path = format!("repos/{}/compare/{sha}...{MAIN}", self.repo);
        let out = self.run(&["api", &path, "--jq", ".status"], GH_TIMEOUT)?;
        if out.status.success() {
            let status = String::from_utf8_lossy(&out.stdout).trim().to_string();
            return Ok(match status.as_str() {
                "identical" | "ahead" => OnMain::Yes,
                _ => OnMain::No(status),
            });
        }
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("404") || err.contains("Not Found") || err.contains("No common ancestor") {
            return Ok(OnMain::Unknown);
        }
        Err(Error::internal(format!(
            "gh api {path} failed: {}",
            tail(&out.stderr)
        )))
    }

    fn main_runs(&self, sha: &str) -> Result<Vec<Run>> {
        self.runs(&["--commit", sha, "--limit", "20"])
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
    /// already uses), else `$XDG_DATA_HOME/cadence/releases`, else
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
            None => match fs::read_link(&link).ok().and_then(|t| release_of(&t)) {
                Some((dir, _)) => dir,
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
}

/// Resolve, verify, and (unless `dry_run`) install and repoint. Returns
/// the JSON report; restarting is the caller's separate, explicit step.
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
    let mut run_id = None;
    let (sha, source) = match &req.target {
        // Rollback / reinstall: already on disk → no network at all.
        Target::Sha(sha) if layout.binary(sha).is_file() => (sha.clone(), "installed-release"),
        Target::Sha(sha) => (sha.clone(), "ci-artifact"),
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
            let source = if layout.binary(&run.head_sha).is_file() {
                "installed-release"
            } else {
                "ci-artifact"
            };
            (run.head_sha, source)
        }
    };

    let installed_path = layout.binary(&sha);
    // Holds the downloaded files until they are installed or dropped.
    let mut staged: Option<tempfile::TempDir> = None;
    if source == "installed-release" {
        verify_local(layout, &sha, &mut verified)?;
    } else {
        if req.target != Target::LatestMain {
            src.check_auth()?;
        }
        let id = verify_ci(src, &sha, &mut verified)?;
        run_id = Some(id);
        let tmp = tempfile::Builder::new()
            .prefix("cadence-upgrade-")
            .tempdir()?;
        let name = artifact_name(&sha);
        src.download(id, &name, tmp.path())?;
        verify_download(src, tmp.path(), &sha, &mut verified)?;
        staged = Some(tmp);
    }

    let link_changed = from_target.as_deref() != Some(installed_path.as_path());
    let mut installed = false;
    let mut repointed = false;
    if !req.dry_run {
        if let Some(tmp) = &staged {
            install_files(tmp.path(), &layout.release_dir(&sha))?;
            installed = true;
        }
        repointed = repoint(&layout.link, &installed_path)?;
    }

    Ok(json!({
        "dry_run": req.dry_run,
        "repo": src.repo(),
        "from_sha": from_sha,
        "from_target": from_target,
        "to_sha": sha,
        "source": source,
        "run_id": run_id,
        "artifact": (source == "ci-artifact").then(|| artifact_name(&sha)),
        "installed_path": installed_path,
        "link": layout.link,
        "installed": installed,
        "repointed": repointed,
        "would_install": req.dry_run && source == "ci-artifact",
        "would_repoint": req.dry_run && link_changed,
        "verified": Value::Object(verified),
        "restarted": false,
    }))
}

/// Steps 1–3: on main, `test` passed on that exact sha, artifact present.
/// Returns the run that holds the artifact.
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
            "no `{WORKFLOW}` push run on {MAIN} for {sha} — pull-request and merge-queue runs \
             do not count; wait for the post-merge run (`gh run list --workflow {WORKFLOW} \
             --branch {MAIN} --commit {sha}`)"
        )));
    }
    runs.sort_by_key(|r| std::cmp::Reverse(r.id));
    let mut seen = Vec::new();
    let mut chosen = None;
    for run in &runs {
        let jobs = src.jobs(run.id)?;
        let test = jobs.iter().find(|j| j.name == TEST_JOB);
        let state = match test {
            Some(j) if j.conclusion == "success" => {
                chosen = Some(run);
                break;
            }
            Some(j) if j.conclusion.is_empty() => format!("{} ({})", TEST_JOB, j.status),
            Some(j) => format!("{} {}", TEST_JOB, j.conclusion),
            None => format!("no `{TEST_JOB}` job"),
        };
        seen.push(format!("run {}: {state}", run.id));
    }
    let Some(run) = chosen else {
        return Err(Error::rejected(format!(
            "CI's `{TEST_JOB}` job has not passed for {sha} on {MAIN} ({}) — refusing to \
             install an untested build; wait for the run or pick a green sha",
            seen.join("; ")
        )));
    };
    verified.insert(
        "test_job".into(),
        json!(format!(
            "{TEST_JOB} success in run {} attempt {}",
            run.id, run.attempt
        )),
    );
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
                "run {} has no artifact {name} — the `release-artifact` job did not run or \
                 did not finish (builds before CAD-334 have none); check `gh run view {}`",
                run.id, run.id
            )));
        }
    }
    Ok(run.id)
}

/// Steps 4–7 on the downloaded files.
fn verify_download(
    src: &dyn ReleaseSource,
    dir: &Path,
    sha: &str,
    verified: &mut serde_json::Map<String, Value>,
) -> Result<()> {
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
    Ok(())
}

/// An installed release: its recorded checksum and manifest when present,
/// then `--version`. Older, hand-installed releases have neither file;
/// they are reported as unrecorded rather than silently "verified".
fn verify_local(
    layout: &Layout,
    sha: &str,
    verified: &mut serde_json::Map<String, Value>,
) -> Result<()> {
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
    if manifest.is_file() {
        verified.insert("manifest".into(), check_manifest(&manifest, sha, &digest)?);
        verified.insert("manifest_source_sha".into(), json!(sha));
    } else {
        verified.insert(
            "manifest".into(),
            json!("absent — installed before CAD-334"),
        );
    }
    verified.insert(
        "download".into(),
        json!("skipped — release already installed"),
    );
    verified.insert("version".into(), json!(check_version(&binary, sha)?));
    Ok(())
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
