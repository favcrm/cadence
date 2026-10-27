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
//!
//! CAD-1455: the staging promotion also emits a *retained* evidence package —
//! the exact `production-candidate.json` bytes it attested with
//! `staging.yml` and the binary's `ci.yml` build-provenance bundle — so a
//! consumer can prove the promotion ran entirely offline (`gh attestation
//! verify --bundle`, no `gh api`, no `gh run download`, no login).
//! [`OfflineGh::verify_retained`] is that offline proof, anchored to an
//! embedded reviewed trusted-root snapshot, never to package-supplied
//! bytes. It never replaces the online path's guards and grants no
//! execution authority by itself: a build signature alone is not a
//! production approval, unsigned metadata never is, and the attested
//! receipt proves the promotion *ran* — not that the run concluded
//! successfully, which stays an online/catalogue check.

use std::cell::RefCell;
use std::fs;
use std::io::{ErrorKind, Read};
use std::os::unix::fs::{symlink, OpenOptionsExt, PermissionsExt};
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
/// A successful dispatch includes staging and production approval.
pub const STAGING_WORKFLOW: &str = "staging.yml";
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

/// A successful dispatch includes staging and production approval.
/// Files of a retained evidence package (CAD-1455), next to [`BINARY`],
/// [`SHA_FILE`] and [`MANIFEST`]: the exact promoted receipt bytes,
/// their `staging.yml` attestation bundle, the binary's `ci.yml`
/// build-provenance bundle, and the completed-promotion receipt with
/// its `promotion-completion.yml` bundle. The package also carries
/// `trusted_root.jsonl`, but that is untrusted transport data only —
/// the verifier anchors to the embedded [`TRUSTED_ROOT_SNAPSHOT`],
/// never to package bytes.
pub const RECEIPT_FILE: &str = "production-candidate.json";
pub const PROMOTION_BUNDLE: &str = "promotion-attestation.jsonl";
pub const BUILD_BUNDLE: &str = "build-attestation.jsonl";
pub const COMPLETION_FILE: &str = "promotion-completion.json";
pub const COMPLETION_BUNDLE: &str = "completion-attestation.jsonl";
/// The producer-signed publication-time source facts the offline
/// verifier consumes for on-main/ancestry/schema checks it cannot
/// re-ask GitHub about.
pub const SOURCE_FACTS_FILE: &str = "source-facts.json";
/// The trusted main workflow that attests the completed-promotion
/// receipt after the upstream `staging.yml` run has already succeeded.
pub const COMPLETION_WORKFLOW: &str = "promotion-completion.yml";
/// The public-client publication receipt a `ci.yml` tag run emits and
/// attests only AFTER `Publish the GitHub Release` has made the real
/// release public — post-publication authority a build-time bundle
/// cannot give (`cli/client_update.rs` verifies it through
/// [`OfflineGh::verify_bundle`]).
pub const PUBLICATION_RECEIPT: &str = "publication-receipt.json";
pub const PUBLICATION_BUNDLE: &str = "publication-attestation.jsonl";

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

/// The approved artifact's immutable subject; workflow status, not this
/// JSON alone, proves that staging and production approval completed.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionCandidate {
    pub schema: u64,
    pub source_sha: String,
    pub ci_run_id: u64,
    pub ci_run_attempt: u64,
    pub sha256: String,
    pub promotion_run_id: u64,
    pub promotion_run_attempt: u64,
}

/// A lowercase 64-hex sha256.
fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// The receipt's own closed shape: every field the producer writes,
/// well formed and nonzero. Who approved what is proven separately
/// (online by the promotion run, offline by the receipt's attested
/// bytes) — this only says the JSON is a candidate.
fn candidate_fields(candidate: &ProductionCandidate) -> bool {
    candidate.schema == 1
        && is_full_sha(&candidate.source_sha)
        && candidate.ci_run_id != 0
        && candidate.ci_run_attempt != 0
        && is_sha256_hex(&candidate.sha256)
        && candidate.promotion_run_id != 0
        && candidate.promotion_run_attempt != 0
}

/// CAD-1455: parse a retained `production-candidate.json` on its own
/// merits — the closed schema and field bounds, without the live
/// promotion-run cross-check [`parse_production_candidate`] adds.
/// Offline the attested receipt bytes, not a fresh API answer, carry
/// the promotion proof.
fn parse_retained_candidate(text: &str) -> Result<ProductionCandidate> {
    let candidate: ProductionCandidate = serde_json::from_str(text)
        .map_err(|e| Error::rejected(format!("invalid production candidate receipt: {e}")))?;
    if !candidate_fields(&candidate) {
        return Err(Error::rejected(
            "production candidate receipt fails its own schema",
        ));
    }
    Ok(candidate)
}

pub fn parse_production_candidate(text: &str, promotion: &Run) -> Result<ProductionCandidate> {
    let candidate: ProductionCandidate = serde_json::from_str(text)
        .map_err(|e| Error::rejected(format!("invalid production candidate receipt: {e}")))?;
    if !candidate_fields(&candidate)
        || candidate.promotion_run_id != promotion.id
        || candidate.promotion_run_attempt != promotion.attempt
        || promotion.event != "workflow_dispatch"
        || promotion.head_branch != MAIN
        || promotion.status != "completed"
        || promotion.conclusion != "success"
    {
        return Err(Error::rejected(
            "production candidate receipt does not match a successful main promotion",
        ));
    }
    Ok(candidate)
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
    /// The main CI run selected by the latest successful production
    /// promotion. A green CI build alone is not a production candidate.
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
    /// CAD-1455: the published release's recorded source SHA, selected
    /// explicitly — a *published-subject* fact, not a live run lookup.
    /// The owned HTTPS source (`crate::releases::HttpGh`) answers from
    /// the catalogue's latest published entry; a live source (`Gh`,
    /// `OfflineGh`) refuses rather than fabricating a run status.
    fn published_source(&self) -> Result<Option<String>> {
        Err(Error::rejected(
            "this release source answers no published-release selection; \
             the online GitHub path selects the latest approved promotion run",
        ))
    }
    /// CAD-1455: prove a retained evidence package offline. `dir` holds
    /// [`BINARY`], [`SHA_FILE`], [`MANIFEST`], [`RECEIPT_FILE`],
    /// [`PROMOTION_BUNDLE`], [`BUILD_BUNDLE`], [`COMPLETION_FILE`],
    /// [`COMPLETION_BUNDLE`] and [`SOURCE_FACTS_FILE`]; a package
    /// `trusted_root.jsonl` is untrusted transport data and is never
    /// consulted. The verification must not contact GitHub and must
    /// bind the receipt's exact bytes, the binary's exact bytes, a
    /// completed-promotion receipt and its signed source-facts/manifest
    /// digests to the recorded `source_sha`. On success the returned
    /// [`RetainedProof`] keeps the captured workspace alive so a later
    /// install consumes the same verified bytes, and carries the
    /// signed *publication-time* eligibility snapshot (on-main,
    /// ancestry, schema as observed at promotion) — an authenticated
    /// record, never a fresh live main lookup, and unable to know a
    /// later main rewrite. Sources that cannot prove that (including
    /// this online `gh` path, which keeps its live-API proof) refuse.
    fn verify_retained(&self, dir: &Path) -> Result<RetainedProof> {
        let _ = dir;
        Err(Error::rejected(
            "this release source cannot verify a retained offline package; \
             the online GitHub path proves promotion live",
        ))
    }
    /// CAD-1455: whether this source installs a *published* retained
    /// package — the owned HTTPS route (`crate::releases::HttpGh`) —
    /// instead of a live CI artifact run. `Gh` is not published; it
    /// keeps the live proof.
    fn published(&self) -> bool {
        false
    }
    /// CAD-1455: fetch and prove the published release for `sha`,
    /// returning its [`RetainedProof`] with the captured workspace
    /// still owned so the install consumes the identical verified
    /// bytes — never re-downloaded, never a caller path. Sources that
    /// cannot prove a published package (the online `gh` path) refuse.
    fn published_proof(&self, sha: &str) -> Result<RetainedProof> {
        let _ = sha;
        Err(Error::rejected(
            "this release source has no published retained package; \
             the online GitHub path downloads and proves a CI artifact",
        ))
    }
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
    candidate: RefCell<Option<ProductionCandidate>>,
}

const GH_TIMEOUT: Duration = Duration::from_secs(60);
const GH_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

impl Gh {
    pub fn new(repo: &str) -> Self {
        Self {
            program: PathBuf::from("gh"),
            repo: repo.to_string(),
            candidate: RefCell::new(None),
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
        *self.candidate.borrow_mut() = None;
        let out = self.run_ok(
            &[
                "run",
                "list",
                "--repo",
                &self.repo,
                "--workflow",
                STAGING_WORKFLOW,
                "--event",
                "workflow_dispatch",
                "--branch",
                MAIN,
                "--status",
                "success",
                "--limit",
                "1",
                "--json",
                "databaseId,attempt,headSha,headBranch,event,status,conclusion",
            ],
            GH_TIMEOUT,
        )?;
        let Some(promotion) = parse_runs(&out)?.into_iter().next() else {
            return Ok(None);
        };
        // A skipped workflow is not approval. Every job bearing either
        // gate name must have completed successfully in the latest attempt.
        let jobs = self.jobs(promotion.id)?;
        for name in ["stage", "promote"] {
            let gates: Vec<_> = jobs.iter().filter(|j| j.name == name).collect();
            if gates.is_empty()
                || gates
                    .iter()
                    .any(|j| j.status != "completed" || j.conclusion != "success")
            {
                return Err(Error::rejected(format!(
                    "production candidate run {} has no passing {name} gate",
                    promotion.id
                )));
            }
        }
        let dir = tempfile::tempdir()?;
        self.download(promotion.id, "production-candidate", dir.path())?;
        let receipt = fs::read_to_string(dir.path().join("production-candidate.json"))?;
        let candidate = parse_production_candidate(&receipt, &promotion)?;
        let path = format!("repos/{}/actions/runs/{}", self.repo, candidate.ci_run_id);
        let out = self.run_ok(&["api", &path], GH_TIMEOUT)?;
        let ci: Value = serde_json::from_slice(&out)
            .map_err(|e| Error::rejected(format!("unreadable candidate CI run: {e}")))?;
        if ci["id"].as_u64() != Some(candidate.ci_run_id)
            || ci["run_attempt"].as_u64() != Some(candidate.ci_run_attempt)
            || ci["head_sha"].as_str() != Some(candidate.source_sha.as_str())
            || ci["head_branch"] != MAIN
            || ci["event"] != "push"
            || ci["path"] != ".github/workflows/ci.yml"
            || ci["status"] != "completed"
            || ci["conclusion"] != "success"
            || ci["repository"]["full_name"].as_str() != Some(self.repo.as_str())
            || ci["head_repository"]["full_name"].as_str() != Some(self.repo.as_str())
        {
            return Err(Error::rejected("production candidate CI run changed or does not match the approved artifact; stage and approve again"));
        }
        let run = Run {
            id: candidate.ci_run_id,
            attempt: candidate.ci_run_attempt,
            head_sha: candidate.source_sha.clone(),
            head_branch: MAIN.into(),
            event: "push".into(),
            status: "completed".into(),
            conclusion: "success".into(),
        };
        *self.candidate.borrow_mut() = Some(candidate);
        Ok(Some(run))
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
        let mut runs = self.runs("push", Some(MAIN), &["--commit", sha, "--limit", "20"])?;
        if let Some(candidate) = self.candidate.borrow().as_ref() {
            if candidate.source_sha == sha {
                runs.retain(|r| {
                    r.id == candidate.ci_run_id && r.attempt == candidate.ci_run_attempt
                });
            }
        }
        Ok(runs)
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
        if let Some(candidate) = self.candidate.borrow().as_ref() {
            if candidate.source_sha == sha && sha256_file(binary)? != candidate.sha256 {
                return Err(Error::rejected("binary digest differs from the staged production candidate; stage and approve again"));
            }
        }
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
// retained offline evidence (CAD-1455)
// ---------------------------------------------------------------------------

/// The certificate SAN an in-repo GitHub Actions signer gets:
/// `https://github.com/<owner>/<repo>/.github/workflows/<workflow>@refs/heads/<ref>`.
/// It is exact (`--cert-identity`, no regex), which is stricter than the
/// historical prefix `--signer-workflow` matching.
fn cert_identity(repo: &str, workflow: &str, source_ref: &str) -> String {
    format!("https://github.com/{repo}/.github/workflows/{workflow}@{source_ref}")
}

/// The Sigstore/GitHub trusted root the offline verifier anchors to.
/// This is a *proposed* snapshot newly captured from the installed `gh`
/// CLI's own TUF-resolved `gh attestation trusted-root` output
/// (public-good `tuf-repo-cdn.sigstore.dev` + GitHub `tuf-repo.github.com`
/// instances), not a credential and not package data: an attested
/// artifact can never supply its own trust anchor, so the package's
/// `trusted_root.jsonl` is untrusted transport data and this embedded
/// snapshot is the only root the verifier passes to
/// `--custom-trusted-root`. It is source material pending independent
/// review — its provenance and sha256 are recorded below so a reviewer
/// can re-derive it from the same official source before trusting it.
/// Refreshing it means capturing the same official source again and
/// re-pinning its sha256 in review, never trusting producer bytes.
pub const TRUSTED_ROOT_SNAPSHOT: &str = include_str!("upgrade/trusted_root.jsonl");

/// SHA-256 of [`TRUSTED_ROOT_SNAPSHOT`], recorded at capture so the
/// snapshot's provenance is auditable in review.
pub const TRUSTED_ROOT_SNAPSHOT_SHA256: &str =
    "65ca537f6ed8a47fd0e560c421baa1f6c1efb8b25fc200d8c5c02c0e92eb2b9c";

/// Bounds on the package files the verifier copies into its capture
/// workspace: a receipt is a few hundred bytes, a Sigstore bundle a few
/// hundred KB at most, a manifest a few hundred bytes. Anything larger
/// is refused unread rather than streamed into memory. The binary is
/// hashed, not loaded.
pub(crate) const RECEIPT_MAX: u64 = 64 * 1024;
pub(crate) const BUNDLE_MAX: u64 = 32 * 1024 * 1024;
pub(crate) const MANIFEST_MAX: u64 = 64 * 1024;
pub(crate) const SHA_FILE_MAX: u64 = 4096;
/// `source-facts.json` holds the bounded ancestor list; the producer
/// refuses rather than truncate beyond this many entries.
const SOURCE_FACTS_ANCESTORS_MAX: usize = 20_000;
pub(crate) const SOURCE_FACTS_MAX: u64 = 8 * 1024 * 1024;
/// The release binary is bounded too: a real build is a few hundred MB
/// at most; an absurd package member is refused before it is copied.
pub(crate) const BINARY_MAX: u64 = 512 * 1024 * 1024;

/// Copy `path` into `dest` only when it is a regular file no larger
/// than `max`, opening it without following the final symlink so a
/// package cannot point a name at outside bytes. The verified bytes are
/// what is copied; the verifier then reads only the captured copy, so a
/// mutable attacker path can never be re-read mid-check.
fn capture_bounded(path: &Path, max: u64, dest: &Path, what: &str) -> Result<()> {
    // O_NONBLOCK alongside O_NOFOLLOW: a FIFO or device would otherwise
    // block the open before its type is ever checked. Only a regular
    // file of a bounded size is accepted, and the copy itself is hard-
    // capped so a file that grows mid-read cannot fill the disk.
    let src = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| {
            Error::rejected(format!(
                "retained package cannot read {what} ({}): {e}",
                path.display()
            ))
        })?;
    let meta = src.metadata().map_err(Error::from)?;
    if !meta.is_file() {
        return Err(Error::rejected(format!(
            "{} ({what}) is not a regular file — refusing the retained package",
            path.display()
        )));
    }
    if meta.len() == 0 || meta.len() > max {
        return Err(Error::rejected(format!(
            "{} ({what}) is {} bytes, outside the allowed 1..={max} — refusing \
             the retained package",
            path.display(),
            meta.len()
        )));
    }
    let mut dst = fs::File::create(dest)?;
    let mut src = src.take(max + 1);
    let copied = std::io::copy(&mut src, &mut dst)?;
    if copied > max {
        return Err(Error::rejected(format!(
            "{} ({what}) exceeded {max} bytes while it was captured — refusing \
             the retained package",
            path.display()
        )));
    }
    if copied != meta.len() {
        return Err(Error::rejected(format!(
            "{} ({what}) changed size while it was captured — refusing the \
             retained package",
            path.display()
        )));
    }
    dst.sync_all()?;
    Ok(())
}

/// A package captured into an owned workspace. The verifier reads and
/// hashes only these copies, never the caller's paths again. The
/// workspace is owned by [`RetainedProof`] so the captured subjects
/// stay alive and immutable for the operation — the install step can
/// consume these exact verified bytes, not re-read attacker paths.
pub struct Captured {
    _workspace: tempfile::TempDir,
    /// The captured package directory.
    pub dir: PathBuf,
}

/// Copy the package's small entries into a fresh owned directory and
/// capture the binary by digest (it is large; the verifier re-opens
/// the captured copy for hashing and for `gh`'s subject arg).
fn capture_package(dir: &Path) -> Result<Captured> {
    let workspace = tempfile::Builder::new()
        .prefix("cadence-retained-")
        .tempdir()?;
    let dest = workspace.path();
    for (name, max) in [
        (RECEIPT_FILE, RECEIPT_MAX),
        (PROMOTION_BUNDLE, BUNDLE_MAX),
        (BUILD_BUNDLE, BUNDLE_MAX),
        (COMPLETION_FILE, RECEIPT_MAX),
        (COMPLETION_BUNDLE, BUNDLE_MAX),
        (SOURCE_FACTS_FILE, SOURCE_FACTS_MAX),
        (SHA_FILE, SHA_FILE_MAX),
        (MANIFEST, MANIFEST_MAX),
    ] {
        capture_bounded(&dir.join(name), max, &dest.join(name), name)?;
    }
    // The binary is captured by the same bounded copy so it cannot
    // change under the verifier; it is the largest member and is only
    // hashed/run, never held in memory.
    let bin = dest.join(BINARY);
    capture_bounded(&dir.join(BINARY), BINARY_MAX, &bin, BINARY)?;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755))?;
    Ok(Captured {
        dir: dest.to_path_buf(),
        _workspace: workspace,
    })
}

/// A [`ReleaseSource`] that proves a retained evidence package without
/// GitHub. Its [`ReleaseSource::verify_retained`] is the offline proof;
/// every online query the trait also names (compare, run lists,
/// downloads) stays unimplemented — a retained package carries no live
/// answers, and this source never invents them. Consumers combine this
/// with the existing on-main/ancestry/lease/operator guards; it is a
/// verifier, not a full transport, until the catalogue contract lands.
pub struct OfflineGh {
    pub program: PathBuf,
    pub repo: String,
}

impl OfflineGh {
    pub fn new(repo: &str) -> Self {
        Self {
            program: PathBuf::from("gh"),
            repo: repo.to_string(),
        }
    }

    /// `gh attestation verify <subject> --bundle <bundle>
    /// --custom-trusted-root <snapshot>` under the pinned policy. The
    /// bundle makes the verification local and the embedded snapshot —
    /// not any package file — is the trust anchor, so no `gh api`,
    /// `gh run download` or login happens. `subject` is the captured
    /// copy in the verifier's workspace, never the caller's path.
    /// Returns [`BundleFacts`] read back out of the verified result.
    ///
    /// `source_digest`, when `Some`, pins `--source-digest` — the
    /// commit the attestation claims as its source (`source_sha` for
    /// the build bundle). The promotion pass leaves it unpinned:
    /// `staging.yml` signs at its own run head, which legitimately
    /// differs from the promoted `source_sha`, and the certificate is
    /// the authoritative record of which one it was.
    ///
    /// gh 2.83.2 makes `--cert-identity`/`--cert-identity-regex`/
    /// `--signer-repo`/`--signer-workflow` mutually exclusive — this
    /// verifier passes the exact `--cert-identity` SAN (which already
    /// spells `https://github.com/<repo>/.github/workflows/<wf>@ref`,
    /// pinning the signer's repository and workflow together) plus
    /// `--repo`, so `--signer-repo` must NOT be added: both flags
    /// abort before any crypto. Whether an explicit certificate
    /// `sourceRepositoryURI` binding is still required on top is being
    /// determined by an independent wrong-repo negative against the
    /// same policy; the signer commit itself is always reported from
    /// the verified certificate, never asserted by the caller.
    ///
    /// Subject binding is double-checked: `gh` proves the signature
    /// covers the subject's bytes, and [`check_subjects`] re-reads the
    /// statement's recorded subject name and digest so a bundle minted
    /// for a different artifact is never accepted.
    ///
    /// Also used by the public-client update path (`cli/client_update.rs`)
    /// to verify a published release asset's retained build bundle
    /// offline under the same pinned policy — that caller supplies its
    /// own subject inside its owned scratch dir.
    #[allow(clippy::too_many_arguments)]
    pub fn verify_bundle(
        &self,
        subject: &Path,
        bundle: &Path,
        root: &Path,
        workflow: &str,
        source_ref: &str,
        source_digest: Option<&str>,
        expect_subject: &str,
        expect_digest: &str,
        what: &str,
    ) -> Result<BundleFacts> {
        let subject = subject.to_string_lossy();
        let bundle = bundle.to_string_lossy();
        let root = root.to_string_lossy();
        let identity = cert_identity(&self.repo, workflow, source_ref);
        let mut args = vec![
            "attestation",
            "verify",
            &subject,
            "--bundle",
            &bundle,
            "--custom-trusted-root",
            &root,
            "--repo",
            &self.repo,
            "--cert-identity",
            &identity,
            "--cert-oidc-issuer",
            "https://token.actions.githubusercontent.com",
            "--source-ref",
            source_ref,
            "--deny-self-hosted-runners",
            "--format",
            "json",
        ];
        if let Some(sha) = source_digest {
            args.extend(["--source-digest", sha]);
        }
        let mut cmd = Command::new(&self.program);
        cmd.args(&args).env("GH_PROMPT_DISABLED", "1");
        let out = crate::proc::run_bounded(&mut cmd, GH_TIMEOUT).map_err(|e| match e {
            crate::proc::BoundedError::Spawn(err) if err.kind() == ErrorKind::NotFound => {
                Error::rejected(
                    "gh (GitHub CLI) is not on PATH — the offline bundle verifier needs it",
                )
            }
            other => Error::internal(format!("gh attestation verify: {other}")),
        })?;
        if !out.status.success() {
            return Err(Error::rejected(format!(
                "{what} attestation did not verify offline for {} — refusing: {}",
                self.repo,
                tail(&out.stderr)
            )));
        }
        check_subjects(&out.stdout, expect_subject, expect_digest)
            .map_err(|e| Error::rejected(format!("{what} {e}")))
    }
}

/// The authenticated facts an offline verification reports back: the
/// attestation's source-repository digest (the commit the verified
/// certificate records as its source — with the exact non-reusable
/// workflow profiles pinned here, the run's head commit) and the
/// subject digest the statement was verified over. Both come from
/// `gh`'s `--format json` `verificationResult`, never from the caller.
pub struct BundleFacts {
    /// `signature.certificate.sourceRepositoryDigest`: the source
    /// repo/run-context commit the certificate attests, meaningful as
    /// a run-head comparison only because the exact in-repo workflow
    /// profile is pinned — not a generic reusable signer-workflow id.
    pub signer_sha: String,
    pub subject_digest: String,
}

/// A retained evidence package proven offline, with its captured
/// workspace kept alive so the install step can consume the same
/// verified bytes. `source_sha` is the promoted commit the receipt
/// binds; `completion` is the attested completed-promotion receipt;
/// `facts` is the signed publication-time eligibility snapshot the
/// offline on-main/ancestry/schema guards consume.
pub struct RetainedProof {
    /// The captured package directory (owned, immutable for the op).
    pub captured: Captured,
    /// The promoted `source_sha` the receipts and manifest bind.
    pub source_sha: String,
    /// The completed-promotion receipt as parsed from the package.
    pub completion: CompletionReceipt,
    /// The signed publication-time source facts (on-main, ancestry and
    /// schema observed at promotion, never a live lookup).
    pub facts: SourceFacts,
}

/// The closed completion receipt `promotion-completion.yml` emits and
/// attests after the upstream `staging.yml` run has already succeeded.
/// It binds the proven promotion run, its head SHA (the promotion
/// signer's commit), the exact promoted receipt bytes, the promoted
/// `source_sha`/`binary_sha256`, the signed `source-facts.json` and
/// `manifest.json` digests (so neither can be swapped later), and this
/// completing run's own head — all kept distinct; no field is equated
/// with another.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionReceipt {
    pub schema: u64,
    pub promotion_run_id: u64,
    pub promotion_run_attempt: u64,
    pub promotion_head_sha: String,
    pub receipt_sha256: String,
    pub source_sha: String,
    pub ci_run_id: u64,
    pub ci_run_attempt: u64,
    pub binary_sha256: String,
    pub source_facts_sha256: String,
    pub manifest_sha256: String,
    pub completion_run_id: u64,
    pub completion_run_attempt: u64,
    pub completion_head_sha: String,
}

fn completion_fields(c: &CompletionReceipt) -> bool {
    c.schema == 1
        && c.promotion_run_id != 0
        && c.promotion_run_attempt != 0
        && is_full_sha(&c.promotion_head_sha)
        && is_sha256_hex(&c.receipt_sha256)
        && is_full_sha(&c.source_sha)
        && c.ci_run_id != 0
        && c.ci_run_attempt != 0
        && is_sha256_hex(&c.binary_sha256)
        && is_sha256_hex(&c.source_facts_sha256)
        && is_sha256_hex(&c.manifest_sha256)
        && c.completion_run_id != 0
        && c.completion_run_attempt != 0
        && is_full_sha(&c.completion_head_sha)
}

/// The publication-time source facts the completed-promotion producer
/// signs: what the online path's `on_main`/`compare`/`schema_version`
/// queries answered, fixed at promotion time. `observed_main_head` is
/// the main head the producer saw when it proved ancestry; `ancestors`
/// is the bounded deduplicated commit list including `source_sha` so a
/// later no-backward check can test membership without an API;
/// `schema_version` is `SCHEMA_VERSION` in `src/rollout.rs` at
/// `source_sha`, parsed by the same rule [`parse_schema_version`] uses.
/// This is an authenticated snapshot of eligibility at publication —
/// never a live main lookup, and it cannot know a later main rewrite.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceFacts {
    pub schema: u64,
    pub source_sha: String,
    pub observed_main_head: String,
    pub ancestors: Vec<String>,
    pub schema_version: i64,
}

fn source_facts_fields(f: &SourceFacts) -> bool {
    f.schema == 1
        && is_full_sha(&f.source_sha)
        && is_full_sha(&f.observed_main_head)
        && !f.ancestors.is_empty()
        && f.ancestors.len() <= SOURCE_FACTS_ANCESTORS_MAX
        && f.ancestors.iter().all(|a| is_full_sha(a))
        // deduplicated, ending at the candidate itself
        && f.ancestors.last().map(String::as_str) == Some(f.source_sha.as_str())
        && {
            let mut dedup = f.ancestors.clone();
            dedup.sort();
            dedup.dedup();
            dedup.len() == f.ancestors.len()
        }
}

/// Parse the closed source-facts file on its own field merits; the
/// completion receipt's `source_facts_sha256` binds its exact bytes.
fn parse_source_facts(text: &str) -> Result<SourceFacts> {
    let facts: SourceFacts = serde_json::from_str(text)
        .map_err(|e| Error::rejected(format!("invalid source facts: {e}")))?;
    if !source_facts_fields(&facts) {
        return Err(Error::rejected("source facts fail their own schema"));
    }
    Ok(facts)
}

/// Parse the closed completion receipt on its own field merits; the
/// attestation over its exact bytes plus the captured receipt binding
/// prove it, not a live run query.
fn parse_completion_receipt(text: &str) -> Result<CompletionReceipt> {
    let receipt: CompletionReceipt = serde_json::from_str(text)
        .map_err(|e| Error::rejected(format!("invalid completion receipt: {e}")))?;
    if !completion_fields(&receipt) {
        return Err(Error::rejected("completion receipt fails its own schema"));
    }
    Ok(receipt)
}

/// Read `gh attestation verify --format json` output: a list of verified
/// entries, each carrying `verificationResult.statement.subject` (the
/// attested artifacts) and `verificationResult.signature.certificate`
/// (populated from the OIDC token — not forgeable by the workflow). At
/// least one subject entry must name `expect_subject` with digest
/// `expect_digest`, no subject may name a *different* artifact, and all
/// certificates must agree on one source-repository digest. An entry
/// with an empty subject list is refused, not accepted vacuously.
/// Returns [`BundleFacts`] for the caller's report.
fn check_subjects(out: &[u8], expect_subject: &str, expect_digest: &str) -> Result<BundleFacts> {
    let value: Value = serde_json::from_slice(out)
        .map_err(|e| Error::internal(format!("unreadable attestation verification output: {e}")))?;
    let entries = value
        .as_array()
        .ok_or_else(|| Error::rejected("attestation verification returned no attestation list"))?;
    if entries.is_empty() {
        return Err(Error::rejected(
            "attestation verification accepted no bundle entry — refusing",
        ));
    }
    let mut signer: Option<String> = None;
    let mut matched = false;
    for entry in entries {
        let result = &entry["verificationResult"];
        let subjects = result["statement"]["subject"]
            .as_array()
            .ok_or_else(|| Error::rejected("attestation statement carries no subject list"))?;
        if subjects.is_empty() {
            return Err(Error::rejected(
                "attestation statement carries an empty subject list — refusing",
            ));
        }
        for subject in subjects {
            let name = subject["name"].as_str().unwrap_or_default();
            let digest = subject["digest"]["sha256"].as_str().unwrap_or_default();
            if name != expect_subject || digest != expect_digest {
                return Err(Error::rejected(format!(
                    "attestation subject {name} sha256 {digest} is not the expected \
                     {expect_subject} sha256 {expect_digest} — refusing"
                )));
            }
            matched = true;
        }
        let cert = &result["signature"]["certificate"];
        let sha = cert["sourceRepositoryDigest"]
            .as_str()
            .filter(|s| is_full_sha(s))
            .ok_or_else(|| {
                Error::rejected(
                    "attestation certificate reports no full signer source-repository digest",
                )
            })?;
        match &signer {
            Some(previous) if previous != sha => {
                return Err(Error::rejected(
                    "attestation entries disagree on the source-repository commit — refusing",
                ));
            }
            _ => signer = Some(sha.to_string()),
        }
    }
    if !matched {
        return Err(Error::rejected(
            "no attestation subject matched the expected artifact — refusing",
        ));
    }
    Ok(BundleFacts {
        signer_sha: signer.expect("nonempty entries"),
        subject_digest: expect_digest.to_string(),
    })
}

impl ReleaseSource for OfflineGh {
    fn repo(&self) -> &str {
        &self.repo
    }

    fn check_auth(&self) -> Result<()> {
        // Offline verification needs no GitHub login; `gh attestation
        // verify --bundle` runs unauthenticated.
        Ok(())
    }

    fn latest_green_main(&self) -> Result<Option<Run>> {
        Err(Error::rejected(OFFLINE_NO_LIVE))
    }

    fn on_main(&self, _sha: &str) -> Result<OnMain> {
        Err(Error::rejected(OFFLINE_NO_LIVE))
    }

    fn compare(&self, _base: &str, _head: &str) -> Result<Option<String>> {
        Err(Error::rejected(OFFLINE_NO_LIVE))
    }

    fn main_runs(&self, _sha: &str) -> Result<Vec<Run>> {
        Err(Error::rejected(OFFLINE_NO_LIVE))
    }

    fn merge_group_runs(&self, _sha: &str) -> Result<Vec<Run>> {
        Err(Error::rejected(OFFLINE_NO_LIVE))
    }

    fn jobs(&self, _run_id: u64) -> Result<Vec<Job>> {
        Err(Error::rejected(OFFLINE_NO_LIVE))
    }

    fn artifact(&self, _run_id: u64, _name: &str) -> Result<ArtifactState> {
        Err(Error::rejected(OFFLINE_NO_LIVE))
    }

    fn download(&self, _run_id: u64, _name: &str, _dest: &Path) -> Result<()> {
        Err(Error::rejected(OFFLINE_NO_LIVE))
    }

    fn verify_attestation(&self, _binary: &Path, _sha: &str) -> Result<String> {
        Err(Error::rejected(OFFLINE_NO_LIVE))
    }

    fn merged_titles(&self, _base: &str, _head: &str) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn schema_version(&self, _sha: &str) -> Result<Option<i64>> {
        Ok(None)
    }

    /// Prove a retained package's bytes offline, in order:
    ///
    /// 1. the package is copied into an owned capture workspace
    ///    (`O_NOFOLLOW|O_NONBLOCK`, bounded hard-read-capped files,
    ///    size-rechecked), and only the captured bytes are ever read,
    ///    hashed or handed to `gh`;
    /// 2. the captured receipt parses as the closed
    ///    [`ProductionCandidate`] and its `sha256` names the captured
    ///    binary's bytes;
    /// 3. the promotion bundle — `staging.yml` at `refs/heads/main` —
    ///    is verified against the embedded trusted-root snapshot over
    ///    the receipt's exact captured bytes; the certificate's signer
    ///    commit is reported, not pinned to `source_sha`;
    /// 4. the captured `cadence.sha256` and `manifest.json` agree with
    ///    the receipt's `sha256`/`source_sha`;
    /// 5. the build bundle — `ci.yml` at `refs/heads/main` on
    ///    `source_sha` as `--source-digest` — attests the binary's
    ///    exact captured bytes;
    /// 6. the completion receipt parses as its closed schema, binds
    ///    the same `source_sha`/receipt/binary digests and promotion
    ///    run, and its `promotion-completion.yml` bundle verifies over
    ///    the captured completion bytes — proving the promotion run
    ///    *concluded successfully*, not just that it ran.
    ///
    /// Only then is the binary verified to the CI build *and* to a
    /// completed approved production promotion. A build signature
    /// alone, a mid-run receipt alone, or a valid signature over
    /// tampered/unsigned metadata all refuse — and so does a missing
    /// or unverifiable completion receipt. The returned
    /// [`RetainedProof`] owns the captured workspace so the install
    /// step can consume the identical verified bytes; no path is
    /// re-read after capture.
    fn verify_retained(&self, dir: &Path) -> Result<RetainedProof> {
        let captured = capture_package(dir)?;
        let pkg = captured.dir.clone();
        // The embedded snapshot is the trust anchor; write it into the
        // workspace for `gh --custom-trusted-root`. The package's own
        // trusted_root.jsonl is transport data and is never read here.
        let root = pkg.join(".trusted-root.jsonl");
        fs::write(&root, TRUSTED_ROOT_SNAPSHOT)?;
        let binary = pkg.join(BINARY);
        let receipt_path = pkg.join(RECEIPT_FILE);
        let receipt_bytes = fs::read(&receipt_path)?;
        let receipt_sha = sha256_bytes(&receipt_bytes);
        let candidate = parse_retained_candidate(&String::from_utf8_lossy(&receipt_bytes))?;
        let digest = sha256_file(&binary)?;
        if candidate.sha256 != digest {
            return Err(Error::rejected(format!(
                "retained receipt binds sha256 {} but the package binary hashes to {digest} — \
                 refusing; the package does not hold the promoted bytes",
                candidate.sha256
            )));
        }
        let main_ref = format!("refs/heads/{MAIN}");
        // One promotion verification: crypto + exact staging.yml/main
        // cert identity + signer-repo + runner policy against the
        // embedded root. The certificate's source-repository digest is
        // the authentic run-head fact reported back.
        let promotion = self.verify_bundle(
            &receipt_path,
            &pkg.join(PROMOTION_BUNDLE),
            &root,
            STAGING_WORKFLOW,
            &main_ref,
            None,
            RECEIPT_FILE,
            &receipt_sha,
            "promotion",
        )?;
        // The completed-promotion receipt is the eligibility proof the
        // mid-run receipt cannot give: it must parse, bind the same
        // promotion run, source and digests, and its own
        // promotion-completion.yml bundle must verify over its captured
        // bytes. Missing or unverifiable completion refuses the whole
        // package — a catalogue or the mid-run receipt alone never
        // stands in for completed eligibility.
        let completion_path = pkg.join(COMPLETION_FILE);
        let completion_bytes = fs::read(&completion_path).map_err(|e| {
            Error::rejected(format!(
                "retained package has no completed-promotion receipt ({}): {e} — \
                 refusing; the promotion's final success is unproven",
                completion_path.display()
            ))
        })?;
        let completion_sha = sha256_bytes(&completion_bytes);
        let completion_receipt =
            parse_completion_receipt(&String::from_utf8_lossy(&completion_bytes))?;
        if completion_receipt.source_sha != candidate.source_sha
            || completion_receipt.promotion_run_id != candidate.promotion_run_id
            || completion_receipt.promotion_run_attempt != candidate.promotion_run_attempt
            || completion_receipt.receipt_sha256 != receipt_sha
            || completion_receipt.binary_sha256 != digest
            || completion_receipt.ci_run_id != candidate.ci_run_id
            || completion_receipt.ci_run_attempt != candidate.ci_run_attempt
        {
            return Err(Error::rejected(
                "completion receipt does not bind the same promotion run, source and \
                 digests as the attested candidate — refusing",
            ));
        }
        // The promotion and completion runs are distinct commits and
        // neither needs to equal source_sha; but each receipt's
        // recorded head must match its verified certificate's
        // source-repository digest exactly, so a receipt naming
        // another commit refuses.
        if completion_receipt.promotion_head_sha != promotion.signer_sha {
            return Err(Error::rejected(
                "completion receipt's promotion head differs from the attested \
                 staging.yml run-head commit — refusing",
            ));
        }
        let completion = self.verify_bundle(
            &completion_path,
            &pkg.join(COMPLETION_BUNDLE),
            &root,
            COMPLETION_WORKFLOW,
            &main_ref,
            None,
            COMPLETION_FILE,
            &completion_sha,
            "completion",
        )?;
        // The certificate is authoritative for the completing run's
        // head; the receipt's recorded head must match it, so a receipt
        // claiming another commit refuses.
        if completion_receipt.completion_head_sha != completion.signer_sha {
            return Err(Error::rejected(
                "completion receipt's recorded head differs from its attested \
                 run-head commit — refusing",
            ));
        }
        // The signed publication-time facts are what the offline
        // on-main/ancestry/schema guards consume: their captured bytes
        // must hash to the digest the completion receipt binds, then
        // parse closed and name the same source. The manifest's
        // captured bytes must likewise hash to its bound digest before
        // its contents are trusted.
        let facts_path = pkg.join(SOURCE_FACTS_FILE);
        let facts_bytes = fs::read(&facts_path).map_err(|e| {
            Error::rejected(format!(
                "retained package has no source facts ({}): {e} — refusing; \
                 publication-time eligibility is unproven",
                facts_path.display()
            ))
        })?;
        if sha256_bytes(&facts_bytes) != completion_receipt.source_facts_sha256 {
            return Err(Error::rejected(
                "source facts digest does not match the attested completion receipt — refusing",
            ));
        }
        let facts = parse_source_facts(&String::from_utf8_lossy(&facts_bytes))?;
        if facts.source_sha != candidate.source_sha {
            return Err(Error::rejected(
                "source facts name a different candidate than the attested receipt — refusing",
            ));
        }
        let manifest_path = pkg.join(MANIFEST);
        let manifest_bytes = fs::read(&manifest_path)?;
        if sha256_bytes(&manifest_bytes) != completion_receipt.manifest_sha256 {
            return Err(Error::rejected(
                "manifest digest does not match the attested completion receipt — refusing",
            ));
        }
        self.verify_bundle(
            &binary,
            &pkg.join(BUILD_BUNDLE),
            &root,
            WORKFLOW,
            &main_ref,
            Some(&candidate.source_sha),
            BINARY,
            &candidate.sha256,
            "build",
        )?;
        let recorded = check_sha256(&binary, &pkg.join(SHA_FILE))?;
        if recorded != candidate.sha256 {
            return Err(Error::rejected(format!(
                "{SHA_FILE} records {recorded}, the attested receipt binds {} — refusing",
                candidate.sha256
            )));
        }
        check_manifest(&manifest_path, &candidate.source_sha, &recorded)?;
        Ok(RetainedProof {
            captured,
            source_sha: candidate.source_sha.clone(),
            completion: completion_receipt,
            facts,
        })
    }
}

const OFFLINE_NO_LIVE: &str =
    "a retained offline source answers no live GitHub queries — refusing; \
     online checks (on-main, run history, downloads) need the GitHub source";

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::new().chain_update(bytes).finalize())
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
            // CAD-1455: a published release source selects the latest
            // published subject explicitly — a catalogue fact, never a
            // fabricated live run.
            if src.published() {
                let sha = src.published_source()?.ok_or_else(|| {
                    Error::rejected(format!(
                        "the configured release source publishes no {MAIN} release in {} \
                         yet — refusing",
                        src.repo()
                    ))
                })?;
                if !is_full_sha(&sha) {
                    return Err(Error::rejected(format!(
                        "the published release names source `{sha}`, not a full commit id \
                         — refusing the configured source"
                    )));
                }
                verified.insert(
                    "resolved".into(),
                    json!(format!(
                        "--latest-main → published release {MAIN} source {} ({})",
                        src.repo(),
                        sha
                    )),
                );
                refuse_backwards(src, from_sha.as_deref(), &sha)?;
                (sha, false, Ok(()))
            } else {
                let run = src.latest_green_main()?.ok_or_else(|| {
                    Error::rejected(format!(
                        "no approved production candidate in {} — stage a successful main CI artifact \
                         and approve the production job in `{STAGING_WORKFLOW}`",
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
                        "--latest-main → approved production candidate from {MAIN} CI run {} ({})",
                        run.id, run.head_sha
                    )),
                );
                refuse_backwards(src, from_sha.as_deref(), &run.head_sha)?;
                (run.head_sha, false, Ok(()))
            }
        }
    };

    check_release_entries(layout, &sha)?;
    // CAD-1455: a published release source installs its captured,
    // already-verified package — not a live CI artifact download. The
    // proof's owned workspace hands the exact verified subjects to the
    // install below; nothing is re-downloaded or re-read from caller
    // paths. An explicit --sha the source cannot publish refuses
    // inside `published_proof` (a live eligibility lookup is not
    // invented for it).
    let mut published_proof: Option<RetainedProof> = None;
    if src.published() {
        published_proof = Some(src.published_proof(&sha)?);
    }
    let installed_path = layout.binary(&sha);
    let mut trust = TRUST_ATTESTED;
    let mut digest = None;
    // Why an on-disk release was not reused, when it was not.
    let mut local_rejected: Option<String> = None;
    // A published release has no live local-record cross-check: reuse
    // an on-disk copy only when its binary hashes to the verified
    // published bytes AND its recorded checksum/manifest agree (the
    // same bar a downloaded install leaves behind); the proof's
    // captured package replaces anything else.
    let mut published_reusable = false;
    if let Some(proof) = &published_proof {
        let verified_digest = sha256_file(&proof.captured.dir.join(BINARY))?;
        published_reusable = installed_path.is_file()
            && check_sha256(&installed_path, &layout.release_dir(&sha).join(SHA_FILE))
                .is_ok_and(|d| d == verified_digest)
            && layout.release_dir(&sha).join(MANIFEST).is_file()
            && check_manifest(
                &layout.release_dir(&sha).join(MANIFEST),
                &sha,
                &verified_digest,
            )
            .is_ok();
        if published_reusable {
            verified.insert(
                "download".into(),
                json!("skipped — the verified published build is already installed"),
            );
        }
        digest = Some(verified_digest);
        verified.insert(
            "attestation".into(),
            json!("verified published release package (offline retained proof)"),
        );
    } else if installed_path.is_file() {
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

    // `published_path`: the captured verified package to install when
    // the published bytes are not already on disk. `source` records
    // which route produced the install.
    let mut published_path: Option<PathBuf> = None;
    let source = match &published_proof {
        Some(_) if published_reusable => {
            // Reused the already-installed verified copy; the proof's
            // workspace only verifies, it is not installed from.
            "installed-release"
        }
        Some(proof) => {
            published_path = Some(proof.captured.dir.clone());
            "published-release"
        }
        None if digest.is_some() => "installed-release",
        None => "ci-artifact",
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
        if let Some(pkg) = &published_path {
            // Install the published package's captured subjects — the
            // identical verified bytes the offline proof captured.
            install_files(pkg, &layout.release_dir(&sha))?;
            installed = true;
            let persisted = sha256_file(&installed_path)?;
            if persisted != digest {
                return Err(Error::rejected(format!(
                    "installed copy {} hashes to {persisted}, not the verified {digest} — \
                     the link was not moved; remove that file and rerun",
                    installed_path.display()
                )));
            }
        } else if let Some(tmp) = &staged {
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
        "would_install": req.dry_run && matches!(source, "ci-artifact" | "published-release"),
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
    // Publication-time facts cannot place a newer or unknown installed
    // commit. Latest-mode must prove a forward move, not treat an
    // unanswerable reverse comparison as permission to downgrade.
    if src.published() && src.compare(from, to)?.as_deref() != Some("ahead") {
        return Err(Error::rejected(format!(
            "the published --latest-main release {to} is not proven newer than the linked \
             {from} — refusing backwards or unknown ancestry. To downgrade on purpose, \
             pass --sha {to} explicitly"
        )));
    }
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
    let matching: Vec<_> = jobs.iter().filter(|j| j.name == TEST_JOB).collect();
    if matching.is_empty() {
        return Ok(Err(format!("no `{TEST_JOB}` job")));
    }
    // Required checks identify a name, not a unique job: a successful
    // collision must not hide a failed, skipped or unfinished test job.
    Ok(
        match matching
            .into_iter()
            .find(|j| j.status != "completed" || j.conclusion != "success")
        {
            Some(j) if j.conclusion.is_empty() => Err(format!("{TEST_JOB} ({})", j.status)),
            Some(j) => Err(format!("{TEST_JOB} {} ({})", j.conclusion, j.status)),
            None => Ok(()),
        },
    )
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

    /// The embedded trust anchor is pinned by a recorded digest so a
    /// reviewer can re-derive its provenance. That pin is only worth
    /// anything if the embedded bytes still hash to it: an edit to
    /// `upgrade/trusted_root.jsonl` that does not re-pin the digest
    /// would otherwise leave the recorded provenance quietly false.
    #[test]
    fn the_embedded_trusted_root_snapshot_matches_its_recorded_digest() {
        let digest = format!("{:x}", Sha256::digest(TRUSTED_ROOT_SNAPSHOT.as_bytes()));
        assert_eq!(
            digest, TRUSTED_ROOT_SNAPSHOT_SHA256,
            "the embedded trusted-root snapshot no longer matches its recorded \
             sha256 — re-pin TRUSTED_ROOT_SNAPSHOT_SHA256 in review, never \
             silently"
        );
    }

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

    /// CAD-1455 policy checks, not crypto evidence: `capture_bounded`,
    /// `capture_package` and `check_subjects` enforce the file and
    /// statement rules the verifier relies on; the actual signature
    /// check belongs to the real `gh attestation verify` on production
    /// bytes. These fake JSON/policy cases prove only the parsing and
    /// bounds, never that a real bundle verifies.
    #[test]
    fn cad1455_retained_policy_checks_refuse_bad_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join("out");
        // Missing file is refused, not read.
        assert!(capture_bounded(&dir.path().join("missing"), 1024, &dst, "file").is_err());
        // A directory is not a regular file.
        assert!(capture_bounded(dir.path(), 1024, &dst, "file").is_err());
        // Empty and oversized files are refused.
        let empty = dir.path().join("empty");
        fs::write(&empty, b"").unwrap();
        assert!(capture_bounded(&empty, 4, &dst, "file").is_err());
        let big = dir.path().join("big");
        fs::write(&big, b"0123456789").unwrap();
        assert!(capture_bounded(&big, 4, &dst, "file").is_err());
        // A bounded capture succeeds and copies the bytes.
        capture_bounded(&big, 16, &dst, "file").unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"0123456789");
        // A symlink is refused without following it.
        let target = dir.path().join("real");
        fs::write(&target, b"x").unwrap();
        let link = dir.path().join("link");
        symlink(&target, &link).unwrap();
        assert!(capture_bounded(&link, 16, &dst, "file").is_err());

        // check_subjects: wrong subject name or digest refuses; the
        // certificate's signer digest must be a full sha and identical
        // across entries.
        let entry = |name: &str, digest: &str, signer: &str| {
            json!({"verificationResult": {
                "statement": {"subject": [{"name": name, "digest": {"sha256": digest}}]},
                "signature": {"certificate": {"sourceRepositoryDigest": signer}}
            }})
        };
        let empty_subject = |signer: &str| {
            json!({"verificationResult": {
                "statement": {"subject": []},
                "signature": {"certificate": {"sourceRepositoryDigest": signer}}
            }})
        };
        let sha = "a".repeat(64);
        let signer = "b".repeat(40);
        let good = serde_json::to_vec(&json!([entry("cadence", &sha, &signer)])).unwrap();
        let facts = check_subjects(&good, "cadence", &sha).unwrap();
        assert_eq!(facts.signer_sha, signer);
        assert_eq!(facts.subject_digest, sha);
        // Wrong subject digest is refused.
        let bad = serde_json::to_vec(&json!([entry("cadence", &"c".repeat(64), &signer)])).unwrap();
        assert!(check_subjects(&bad, "cadence", &sha).is_err());
        // Missing signer digest is refused.
        let no_signer = serde_json::to_vec(&json!([entry("cadence", &sha, "")])).unwrap();
        assert!(check_subjects(&no_signer, "cadence", &sha).is_err());
        // An empty subject list refuses, never vacuously accepted.
        let empty = serde_json::to_vec(&json!([empty_subject(&signer)])).unwrap();
        assert!(check_subjects(&empty, "cadence", &sha).is_err());
        // Empty entry list is refused.
        assert!(check_subjects(b"[]", "cadence", &sha).is_err());
    }

    /// The closed completion receipt parser enforces the same schema
    /// discipline; it binds run/source/digest facts, never a live run.
    #[test]
    fn cad1455_completion_receipt_is_closed() {
        let receipt = json!({
            "schema": 1,
            "promotion_run_id": 7,
            "promotion_run_attempt": 2,
            "promotion_head_sha": "a".repeat(40),
            "receipt_sha256": "b".repeat(64),
            "source_sha": "c".repeat(40),
            "ci_run_id": 11,
            "ci_run_attempt": 3,
            "binary_sha256": "d".repeat(64),
            "source_facts_sha256": "f".repeat(64),
            "manifest_sha256": "0".repeat(64),
            "completion_run_id": 21,
            "completion_run_attempt": 1,
            "completion_head_sha": "e".repeat(40)
        })
        .to_string();
        let parsed = parse_completion_receipt(&receipt).unwrap();
        assert_eq!(parsed.source_sha, "c".repeat(40));
        assert_eq!(parsed.completion_run_id, 21);
        // An extra field is refused by deny_unknown_fields.
        let mut v: Value = serde_json::from_str(&receipt).unwrap();
        v["extra"] = json!(true);
        assert!(parse_completion_receipt(&v.to_string()).is_err());
        // A zero id or malformed sha fails the field check.
        v = serde_json::from_str(&receipt).unwrap();
        v["promotion_run_id"] = json!(0);
        assert!(parse_completion_receipt(&v.to_string()).is_err());
        v = serde_json::from_str(&receipt).unwrap();
        v["binary_sha256"] = json!("zz");
        assert!(parse_completion_receipt(&v.to_string()).is_err());
    }

    /// The closed source-facts parser enforces the same schema
    /// discipline: a nonempty deduplicated full-SHA ancestor list
    /// ending at the candidate, plus the publication-time schema.
    #[test]
    fn cad1455_source_facts_is_closed() {
        let candidate = "a".repeat(40);
        let parent = "b".repeat(40);
        let facts = json!({
            "schema": 1,
            "source_sha": candidate,
            "observed_main_head": "c".repeat(40),
            "ancestors": [parent, candidate],
            "schema_version": 46
        })
        .to_string();
        let parsed = parse_source_facts(&facts).unwrap();
        assert_eq!(parsed.source_sha, candidate);
        assert_eq!(parsed.ancestors.len(), 2);
        assert_eq!(parsed.schema_version, 46);
        let mut v: Value = serde_json::from_str(&facts).unwrap();
        v["extra"] = json!(true);
        assert!(parse_source_facts(&v.to_string()).is_err());
        // The candidate must be the list's tail.
        v = serde_json::from_str(&facts).unwrap();
        v["ancestors"] = json!([candidate, parent]);
        assert!(parse_source_facts(&v.to_string()).is_err());
        // Duplicates refuse.
        v = serde_json::from_str(&facts).unwrap();
        v["ancestors"] = json!([parent, parent, candidate]);
        assert!(parse_source_facts(&v.to_string()).is_err());
        // An empty list and a non-SHA entry refuse.
        v = serde_json::from_str(&facts).unwrap();
        v["ancestors"] = json!([]);
        assert!(parse_source_facts(&v.to_string()).is_err());
        v = serde_json::from_str(&facts).unwrap();
        v["ancestors"] = json!(["zz", candidate]);
        assert!(parse_source_facts(&v.to_string()).is_err());
    }

    /// The retained candidate parser accepts the closed schema only and
    /// does not need a live run; the online parser still requires the
    /// promotion run to match.
    #[test]
    fn cad1455_retained_candidate_needs_no_run_but_is_closed() {
        let receipt = json!({
            "schema": 1,
            "source_sha": "a".repeat(40),
            "ci_run_id": 11,
            "ci_run_attempt": 2,
            "sha256": "b".repeat(64),
            "promotion_run_id": 99,
            "promotion_run_attempt": 1
        })
        .to_string();
        let candidate = parse_retained_candidate(&receipt).unwrap();
        assert_eq!(candidate.source_sha, "a".repeat(40));
        assert_eq!(candidate.promotion_run_id, 99);
        // An extra field is refused by deny_unknown_fields.
        let forged = json!({
            "schema": 1,
            "source_sha": "a".repeat(40),
            "ci_run_id": 11,
            "ci_run_attempt": 2,
            "sha256": "b".repeat(64),
            "promotion_run_id": 99,
            "promotion_run_attempt": 1,
            "extra": true
        })
        .to_string();
        assert!(parse_retained_candidate(&forged).is_err());
        // The online parser still needs the matching promotion run.
        let run = Run {
            id: 99,
            attempt: 1,
            head_sha: "c".repeat(40),
            head_branch: MAIN.into(),
            event: "workflow_dispatch".into(),
            status: "completed".into(),
            conclusion: "success".into(),
        };
        assert!(parse_production_candidate(&receipt, &run).is_ok());
        let wrong = Run { id: 1, ..run };
        assert!(parse_production_candidate(&receipt, &wrong).is_err());
    }
}
