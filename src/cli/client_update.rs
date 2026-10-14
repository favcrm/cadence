//! CAD-1425: the client-only installation, upgraded by `cadence update` (CAD-1437).
//!
//! `install.sh --client-only` puts a public release in its own prefix
//! (`<prefix>/releases/<tag>/`, the layout `install.sh` already uses), behind
//! a separate `cadence-client` link, and records `<prefix>/client-install.json`.
//! This module identifies that installation from the running executable,
//! refuses every local daemon or state command from it, and upgrades it from
//! the fixed public release source once the archive's build-provenance
//! attestation verifies and its manifest matches this platform and version.

use super::*;
use cadence_agent::proc::BoundedError;
use cadence_agent::upgrade::{is_full_sha, is_version_tag, sha256_file, DEFAULT_REPO, WORKFLOW};
use serde::Deserialize;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const RECEIPT: &str = "client-install.json";
const BINARY: &str = "cadence";
const SHA_FILE: &str = "cadence.sha256";
const MANIFEST: &str = "manifest.json";
const ARCHIVE_MEMBERS: [&str; 3] = [BINARY, SHA_FILE, MANIFEST];
const CHECKS: [&str; 5] = ["fmt", "clippy", "test", "build", "ui"];
const GH_TIMEOUT: Duration = Duration::from_secs(60);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
const TOOL_TIMEOUT: Duration = Duration::from_secs(60);
const CANDIDATE_TIMEOUT: Duration = Duration::from_secs(30);
/// The unified `update --help` marker `install.sh --client-only` also requires.
/// Only a client-capable release prints it; the native-only update help does not.
const CLIENT_UPDATE_MARKER: &str = "Upgrade a client-only installation";
/// The only `update` arguments a client-only installation accepts from the
/// command line (clap ids). Everything else refuses, even at its default.
const CLIENT_ARGS: [&str; 2] = ["check", "version"];

/// (asset target, CI runner) this build can install, as `ci.yml` names them.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PLATFORM: Option<(&str, &str)> = Some(("x86_64-linux", "ubuntu-22.04"));
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const PLATFORM: Option<(&str, &str)> = Some(("aarch64-linux", "ubuntu-22.04-arm"));
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PLATFORM: Option<(&str, &str)> = Some(("aarch64-macos", "macos-14"));
#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "aarch64"),
)))]
const PLATFORM: Option<(&str, &str)> = None;

/// The `client-install.json` written by `install.sh --client-only`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u64,
    kind: String,
    prefix: String,
    link: String,
    target: String,
}

/// A positively identified client-only installation.
pub(crate) struct ClientInstall {
    prefix: PathBuf,
    /// The validated absolute prefix as the receipt spells it (it canonicalizes
    /// to `prefix`); used for the staged release path and switch target.
    receipt_prefix: PathBuf,
    link: PathBuf,
    /// The link's raw target when the installation was identified.
    link_target: PathBuf,
    tag: String,
}

fn ambiguous(reason: impl std::fmt::Display) -> Error {
    Error::rejected(format!(
        "client-only installation required: its installation is ambiguous ({reason}) — \
         refusing; nothing was changed"
    ))
}

/// The refusal for a process that is not a client-only installation.
pub(crate) fn client_only_required() -> Error {
    Error::rejected(
        "client-only installation required — `cadence update --version` upgrades only an \
         installation made by `install.sh --client-only` (its receipt is \
         <prefix>/client-install.json). This executable is not one, so nothing was \
         checked, fetched or changed",
    )
}

/// The refusal for a local daemon or state command run from a client-only
/// installation. It never reaches the daemon, a sandbox or local state.
pub(crate) fn local_command_refusal() -> Error {
    Error::rejected(
        "client-only installation cannot run local daemon commands — this executable is a \
         client-only install. Run daemon and state commands from an ordinary cadence \
         installation, or select a remote org with --org",
    )
}

/// The native daemon updater proves its operator through Linux procfs, which
/// macOS lacks. Checked before any org or state work; grants no bypass.
#[cfg(target_os = "macos")]
pub(crate) fn native_platform_refusal() -> Result<()> {
    Err(Error::rejected(
        "native daemon updates are unsupported on macOS — the native updater \
         cannot prove the operator on this platform. Upgrade a public client \
         with the documented installer-based release flow (docs/INSTALL-AGENT.md); \
         `--as` and environment variables do not bypass this",
    ))
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn native_platform_refusal() -> Result<()> {
    Ok(())
}

fn current_binary() -> Option<PathBuf> {
    std::env::current_exe().ok()?.canonicalize().ok()
}

/// Identify `exe` as a client-only installation. `None` for anything that
/// is not one (an ordinary install has no receipt). A receipt that is present
/// but does not match its layout is an error: it never falls back to
/// ordinary behaviour.
fn identify(exe: &Path) -> Result<Option<ClientInstall>> {
    let Some(tag_dir) = exe.parent() else {
        return Ok(None);
    };
    let Some(tag) = tag_dir
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|t| is_version_tag(t))
    else {
        return Ok(None);
    };
    let Some(releases) = tag_dir.parent() else {
        return Ok(None);
    };
    if releases.file_name().and_then(|n| n.to_str()) != Some("releases") {
        return Ok(None);
    }
    let Some(prefix) = releases.parent() else {
        return Ok(None);
    };
    let receipt = prefix.join(RECEIPT);
    let meta = match fs::symlink_metadata(&receipt) {
        Ok(meta) => meta,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ambiguous(format!("cannot read the receipt: {e}"))),
    };
    if !meta.is_file() {
        return Err(ambiguous("the receipt is not a regular file"));
    }
    let text = fs::read_to_string(&receipt).map_err(|e| ambiguous(format!("{e}")))?;
    let r: Receipt =
        serde_json::from_str(&text).map_err(|_| ambiguous("the receipt is malformed"))?;
    if r.schema != 1
        || r.kind != "client-only"
        || PLATFORM.map(|(target, _)| target) != Some(r.target.as_str())
    {
        return Err(ambiguous(
            "the receipt names another schema, kind or platform",
        ));
    }
    let real_prefix = PathBuf::from(&r.prefix);
    let link = PathBuf::from(&r.link);
    let is_plain_dir = |p: &Path| fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
    if !real_prefix.is_absolute()
        || !link.is_absolute()
        || fs::canonicalize(&real_prefix).ok().as_deref() != Some(prefix)
        || !is_plain_dir(&real_prefix)
        || !is_plain_dir(releases)
        || !is_plain_dir(tag_dir)
    {
        return Err(ambiguous(
            "the prefix is not the plain directory the receipt names",
        ));
    }
    let link_is_symlink = fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink());
    if !link_is_symlink || fs::canonicalize(&link).ok().as_deref() != Some(exe) {
        return Err(ambiguous(
            "the receipt link does not resolve to this executable",
        ));
    }
    let link_target = fs::read_link(&link)
        .map_err(|e| ambiguous(format!("cannot read the receipt link: {e}")))?;
    Ok(Some(ClientInstall {
        prefix: prefix.to_path_buf(),
        receipt_prefix: real_prefix,
        link,
        link_target,
        tag: tag.to_string(),
    }))
}

/// True when this executable is a client-only installation. Used before every
/// local command so none of them can run from one.
pub(crate) fn client_only_mode() -> Result<bool> {
    let Some(exe) = current_binary() else {
        return Ok(false);
    };
    Ok(identify(&exe)?.is_some())
}

/// `cadence org` from a client-only installation. List and Inspect read the
/// registry as ordinary. Switch changes the saved default only to a registered
/// remote org: `local`, a local destination or an unknown name refuses before
/// `org::run` writes anything.
pub(crate) fn client_org(action: org::OrgAction) -> Result<i32> {
    if let org::OrgAction::Switch { name } = &action {
        if name == "local" {
            return Err(local_command_refusal());
        }
        if let org::Resolved::Local(_) = org::resolve(Some(name.as_str()), None, None)? {
            return Err(local_command_refusal());
        }
    }
    org::run(action)
}

/// Route a non-update command from a client-only installation. Only an
/// actual remote org may proceed (over the org's own transport); a local
/// result, saved default or pin refuses before sandbox adoption or any RPC.
pub(crate) fn route_client_only(cli: Cli) -> Result<i32> {
    let tracker = std::env::var_os("CADENCE_PM_DIR")
        .map(|d| cadence_agent::home::guard_tracker(PathBuf::from(d)))
        .transpose()?;
    match org::resolve(cli.org.as_deref(), cli.state_dir.clone(), tracker)? {
        org::Resolved::Remote(target) => remote::run(target, cli.command, cli.wake_timeout),
        org::Resolved::Local(_) => Err(local_command_refusal()),
    }
}

/// Refuse any command-line argument a client-only `update` does not accept.
/// The typed set comes from clap's value source, so an explicit default such
/// as `--drain 10m` or `--keep 3` refuses too, and global selectors refuse as
/// well. A missing record (no entry read the command line) refuses too.
fn refuse_native_options(cli: &Cli) -> Result<()> {
    let Some(typed) = &cli.command_line else {
        return Err(Error::rejected(
            "client-only update could not read its command line — refusing; nothing was changed",
        ));
    };
    if matches!(
        cli.command,
        Commands::Update {
            action: Some(_),
            ..
        }
    ) {
        return Err(Error::rejected(
            "client-only update has no subcommands — refusing `update status`; nothing was changed",
        ));
    }
    let refused: Vec<&str> = typed
        .iter()
        .map(String::as_str)
        .filter(|id| !CLIENT_ARGS.contains(id))
        .collect();
    if !refused.is_empty() {
        return Err(Error::rejected(format!(
            "client-only update accepts only --check and --version TAG; refusing the native \
             option(s) {} — nothing was checked, fetched or changed",
            refused.join(", ")
        )));
    }
    Ok(())
}

fn require_install() -> Result<ClientInstall> {
    let exe = current_binary().ok_or_else(client_only_required)?;
    identify(&exe)?.ok_or_else(client_only_required)
}

/// A client-only installation's `update`: the public release, client only.
/// Refuses every command-line option outside `CLIENT_ARGS` (native flags,
/// global `--org`/`--state-dir`, explicit defaults, `update status`) before
/// any identity, network or state work.
pub(crate) fn run(cli: &Cli, check: bool, version: Option<&str>) -> Result<i32> {
    refuse_native_options(cli)?;
    let install = require_install()?;
    let (target, runner) = PLATFORM.ok_or_else(|| {
        Error::rejected(
            "update supports Ubuntu x86_64/aarch64 and Apple silicon macOS only — \
             this platform has no published client release",
        )
    })?;
    let current = install.tag.clone();
    verify_release_dir(
        &install.prefix.join("releases").join(&current),
        &current,
        target,
        runner,
    )?;
    let wanted = match version {
        Some(tag) => tag.to_string(),
        None => latest_tag()?,
    };
    let (Some(have), Some(want)) = (version_key(&current), version_key(&wanted)) else {
        return Err(Error::rejected(format!(
            "cannot order {current} and {wanted}: update accepts only vMAJOR.MINOR.PATCH \
             release tags"
        )));
    };
    if want == have {
        println!("update: {current} is the current release");
        return Ok(0);
    }
    if want < have {
        return Err(Error::rejected(format!(
            "update refuses to downgrade {current} to {wanted} — it installs only newer \
             releases"
        )));
    }
    if check {
        println!(
            "update: {current} installed; selected target {wanted}; artifact not downloaded or verified"
        );
        return Ok(0);
    }
    let verified = fetch_verified(&wanted, target, runner)?;
    let _lock = CommitLock::take(&install.prefix)?;
    if fs::canonicalize(&install.receipt_prefix).ok().as_deref() != Some(install.prefix.as_path()) {
        return Err(ambiguous(
            "the prefix changed while the release was verified",
        ));
    }
    let dest = stage_release(&install.receipt_prefix, &wanted, &verified)?;
    switch_link(&install.link, &install.link_target, &dest.join(BINARY))?;
    println!("update: installed {wanted} at {}", dest.display());
    println!(
        "linked {} -> {}",
        install.link.display(),
        dest.join(BINARY).display()
    );
    Ok(0)
}

/// `vMAJOR.MINOR.PATCH` with plain digits; anything else has no known order.
fn version_key(tag: &str) -> Option<[u64; 3]> {
    let mut parts = tag.strip_prefix('v')?.split('.');
    let mut key = [0u64; 3];
    for slot in &mut key {
        let part = parts.next()?;
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    parts.next().is_none().then_some(key)
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The digest from a `sha256sum`-style line that names exactly `name`.
fn parse_sha_line(text: &str, name: &str) -> Result<String> {
    let mut fields = text.split_whitespace();
    match (fields.next(), fields.next(), fields.next()) {
        (Some(hex), Some(file), None) if file == name && is_sha256(hex) => Ok(hex.to_string()),
        _ => Err(Error::rejected(format!(
            "{name} does not hold the sha256 of {name} — refusing"
        ))),
    }
}

fn read_manifest(path: &Path) -> Result<serde_json::Value> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes)
        .map_err(|_| Error::rejected("the release manifest is not valid JSON — refusing"))
}

/// The manifest `ci.yml` writes for a release asset (the fields
/// `scripts/clean-install.py` checks), bound to this tag, platform and
/// binary digest. Returns the source commit it names.
fn check_manifest(
    manifest: &serde_json::Value,
    tag: &str,
    target: &str,
    runner: &str,
    digest: &str,
) -> Result<String> {
    let source = manifest["source_sha"].as_str().filter(|s| is_full_sha(s));
    let matches = manifest["version"] == tag
        && manifest["target"] == target
        && manifest["runner"] == runner
        && manifest["sha256"] == digest
        && manifest["features"] == serde_json::json!(["ui"])
        && manifest["checks"] == serde_json::json!(CHECKS)
        && manifest["run_id"].is_u64()
        && manifest["run_attempt"].is_u64()
        && ["rustc", "cargo", "built_at"]
            .iter()
            .all(|key| manifest[*key].is_string());
    match (matches, source) {
        (true, Some(source)) => Ok(source.to_string()),
        _ => Err(Error::rejected(format!(
            "the release manifest does not match {tag} for {target} and this binary — refusing"
        ))),
    }
}

/// The release dir's bytes must hash to its checksum, and the directory
/// must be a plain directory, not a symlink.
fn verify_release_bytes(dir: &Path, digest: &str) -> Result<()> {
    let plain = fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir());
    let recorded = parse_sha_line(&fs::read_to_string(dir.join(SHA_FILE))?, BINARY)?;
    if !plain || recorded != digest || sha256_file(&dir.join(BINARY))? != digest {
        return Err(Error::rejected(format!(
            "{} does not hold this release's binary — move it aside and rerun",
            dir.display()
        )));
    }
    Ok(())
}

/// The installed release is checked before anything is downloaded or changed.
fn verify_release_dir(dir: &Path, tag: &str, target: &str, runner: &str) -> Result<String> {
    let digest = parse_sha_line(&fs::read_to_string(dir.join(SHA_FILE))?, BINARY)?;
    verify_release_bytes(dir, &digest)?;
    check_manifest(
        &read_manifest(&dir.join(MANIFEST))?,
        tag,
        target,
        runner,
        &digest,
    )?;
    Ok(digest)
}

/// Run a program with a timeout; non-zero exit is a refusal.
fn run_ok(cmd: &mut Command, timeout: Duration, what: &str) -> Result<Vec<u8>> {
    let out = cadence_agent::proc::run_bounded(cmd, timeout).map_err(|e| match e {
        BoundedError::Spawn(err) if err.kind() == ErrorKind::NotFound => Error::rejected(format!(
            "{what} is not on PATH — update needs the GitHub CLI (gh) to fetch and \
             verify the release"
        )),
        other => Error::internal(format!("{what}: {other}")),
    })?;
    if out.status.success() {
        return Ok(out.stdout);
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stderr = stderr.trim();
    let start = stderr.len().saturating_sub(600);
    let start = (start..=stderr.len())
        .find(|i| stderr.is_char_boundary(*i))
        .unwrap_or(stderr.len());
    Err(Error::rejected(format!(
        "{what} failed: {}",
        &stderr[start..]
    )))
}

fn gh(args: &[&str], timeout: Duration) -> Result<Vec<u8>> {
    run_ok(
        Command::new("gh").args(args).env("GH_PROMPT_DISABLED", "1"),
        timeout,
        "gh",
    )
}

/// The newest published release tag (not a draft or prerelease).
fn latest_tag() -> Result<String> {
    let path = format!("repos/{DEFAULT_REPO}/releases/latest");
    let out = gh(&["api", &path, "--jq", ".tag_name"], GH_TIMEOUT)?;
    let tag = String::from_utf8_lossy(&out).trim().to_string();
    if version_key(&tag).is_none() {
        return Err(Error::rejected(format!(
            "the latest release tag '{tag}' is not a supported client version — refusing"
        )));
    }
    Ok(tag)
}

/// Member names of a release archive must be exactly the three regular files.
fn check_listing(listing: &str) -> Result<()> {
    let mut seen: Vec<&str> = Vec::new();
    for line in listing.lines() {
        let name = line.split_whitespace().last().unwrap_or_default();
        if !line.starts_with('-') || !ARCHIVE_MEMBERS.contains(&name) || seen.contains(&name) {
            return Err(Error::rejected(
                "the release archive holds an unexpected member — refusing",
            ));
        }
        seen.push(name);
    }
    if seen.len() != ARCHIVE_MEMBERS.len() {
        return Err(Error::rejected(
            "the release archive does not hold exactly cadence, cadence.sha256 and manifest.json",
        ));
    }
    Ok(())
}

/// A downloaded release whose checks passed. The binary has been attested
/// and then run once for its `--version`; it is only copied from here.
struct Verified {
    _scratch: tempfile::TempDir,
    dir: PathBuf,
    digest: String,
}

/// Download the tag's archive and verify it in order: outer checksum, the
/// archive's member set, the inner checksum and manifest, then the
/// build-provenance attestation bound to this repository, the `ci.yml`
/// signer, the tag ref and the manifest's source commit. Only after all of
/// those does the candidate binary run.
fn fetch_verified(tag: &str, target: &str, runner: &str) -> Result<Verified> {
    let scratch = tempfile::tempdir()?;
    let asset = format!("cadence-{tag}-{target}.tar.gz");
    let sha_name = format!("{asset}.sha256");
    let dir_arg = scratch.path().to_string_lossy().into_owned();
    gh(
        &[
            "release",
            "download",
            tag,
            "--repo",
            DEFAULT_REPO,
            "--pattern",
            &asset,
            "--pattern",
            &sha_name,
            "--dir",
            &dir_arg,
        ],
        DOWNLOAD_TIMEOUT,
    )?;
    let tarball = scratch.path().join(&asset);
    let outer = parse_sha_line(&fs::read_to_string(scratch.path().join(&sha_name))?, &asset)?;
    if sha256_file(&tarball)? != outer {
        return Err(Error::rejected(format!(
            "checksum mismatch for {asset} — refusing to install"
        )));
    }
    attest(&tarball, &asset, tag, None)?;
    let listing = run_ok(
        Command::new("tar").arg("-tvzf").arg(&tarball),
        TOOL_TIMEOUT,
        "tar",
    )?;
    check_listing(&String::from_utf8_lossy(&listing))?;
    let extract = scratch.path().join("x");
    fs::create_dir(&extract)?;
    run_ok(
        Command::new("tar")
            .arg("-xzf")
            .arg(&tarball)
            .arg("-C")
            .arg(&extract)
            .args(ARCHIVE_MEMBERS),
        TOOL_TIMEOUT,
        "tar",
    )?;
    for name in ARCHIVE_MEMBERS {
        if !fs::symlink_metadata(extract.join(name)).is_ok_and(|m| m.is_file()) {
            return Err(Error::rejected(format!(
                "{asset} is missing a regular {name}"
            )));
        }
    }
    let digest = parse_sha_line(&fs::read_to_string(extract.join(SHA_FILE))?, BINARY)?;
    if sha256_file(&extract.join(BINARY))? != digest {
        return Err(Error::rejected(format!(
            "the binary in {asset} does not match its cadence.sha256 — refusing"
        )));
    }
    let source = check_manifest(
        &read_manifest(&extract.join(MANIFEST))?,
        tag,
        target,
        runner,
        &digest,
    )?;
    attest(&tarball, &asset, tag, Some(source.as_str()))?;
    let candidate = extract.join(BINARY);
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755))?;
    let version = run_ok(
        Command::new(&candidate)
            .arg("--version")
            .current_dir(scratch.path()),
        CANDIDATE_TIMEOUT,
        "the candidate cadence --version",
    )?;
    let expected = format!("cadence {}+{source}", &tag[1..]);
    if String::from_utf8_lossy(&version).trim() != expected {
        return Err(Error::rejected(format!(
            "the candidate reports a different version than {expected} — refusing to install"
        )));
    }
    // Side-effect-free: `update --help` only prints clap's help and never
    // reaches identify() or any state. Without this marker the release has no
    // unified client-only mode and must never become a client installation.
    let help = run_ok(
        Command::new(&candidate)
            .args(["update", "--help"])
            .current_dir(scratch.path()),
        CANDIDATE_TIMEOUT,
        "the candidate cadence update --help",
    )
    .map_err(|_| {
        Error::rejected(format!(
            "{tag} release lacks client-only support (its cadence has no client-only update) — \
             refusing to install it as a client; choose a newer public release"
        ))
    })?;
    if !String::from_utf8_lossy(&help).contains(CLIENT_UPDATE_MARKER) {
        return Err(Error::rejected(format!(
            "{tag} release lacks client-only support (its cadence has no client-only update) — \
             refusing to install it as a client; choose a newer public release"
        )));
    }
    Ok(Verified {
        _scratch: scratch,
        dir: extract,
        digest,
    })
}

/// Verify the archive's build-provenance attestation: this repository, the
/// `ci.yml` signer and the tag ref, plus the manifest's source commit once
/// the manifest has been read.
fn attest(tarball: &Path, asset: &str, tag: &str, source: Option<&str>) -> Result<()> {
    let signer = format!("{DEFAULT_REPO}/.github/workflows/{WORKFLOW}");
    let source_ref = format!("refs/tags/{tag}");
    let tarball_arg = tarball.to_string_lossy().into_owned();
    let mut args: Vec<&str> = vec![
        "attestation",
        "verify",
        &tarball_arg,
        "--repo",
        DEFAULT_REPO,
        "--signer-workflow",
        &signer,
        "--source-ref",
        &source_ref,
    ];
    if let Some(source) = source {
        args.extend(["--source-digest", source]);
    }
    args.push("--deny-self-hosted-runners");
    gh(&args, TOOL_TIMEOUT).map(|_| ()).map_err(|_| {
        Error::rejected(format!(
            "build-provenance attestation for {asset} did not verify — refusing to execute or \
             install it"
        ))
    })
}

/// Exclusive commit lock shared with `install.sh --client-only`: the
/// `<prefix>.install-lock` directory, held only across the commit.
struct CommitLock(PathBuf);

impl CommitLock {
    fn take(prefix: &Path) -> Result<Self> {
        let name = prefix.file_name().unwrap_or_default().to_string_lossy();
        let path = prefix.with_file_name(format!("{name}.install-lock"));
        match fs::create_dir(&path) {
            Ok(()) => Ok(Self(path)),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(Error::rejected(format!(
                "another client-only install holds {} — refusing; rerun when it finishes",
                path.display()
            ))),
            Err(e) => Err(e.into()),
        }
    }
}

impl Drop for CommitLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

/// Copy a verified release into `<prefix>/releases/<tag>`. An existing dir
/// is kept only when it holds the same bytes; otherwise it is refused, and
/// nothing is overwritten. Staged in a sibling dir, then renamed into place.
fn stage_release(prefix: &Path, tag: &str, verified: &Verified) -> Result<PathBuf> {
    let releases = prefix.join("releases");
    let dest = releases.join(tag);
    if fs::symlink_metadata(&dest).is_ok() {
        verify_release_bytes(&dest, &verified.digest)?;
        return Ok(dest);
    }
    let stage = releases.join(format!(".client-update-{tag}-{}", std::process::id()));
    fs::create_dir(&stage)?;
    let copied = (|| -> Result<()> {
        for name in ARCHIVE_MEMBERS {
            fs::copy(verified.dir.join(name), stage.join(name))?;
        }
        fs::set_permissions(stage.join(BINARY), fs::Permissions::from_mode(0o755))?;
        verify_release_bytes(&stage, &verified.digest)?;
        fs::rename(&stage, &dest)?;
        Ok(())
    })();
    if copied.is_err() {
        let _ = fs::remove_dir_all(&stage);
    }
    copied.map(|()| dest)
}

/// Atomically point the client link at `target`. The link must still hold
/// `expected` (the target it was identified with) under the commit lock; a
/// link changed since then is never overwritten. The link is replaced by a
/// rename, so a failure before it leaves the active executable untouched.
fn switch_link(link: &Path, expected: &Path, target: &Path) -> Result<()> {
    let current = fs::read_link(link).ok();
    if current.as_deref() == Some(target) {
        return Ok(());
    }
    if current.as_deref() != Some(expected) {
        return Err(Error::rejected(format!(
            "{} changed since this installation was identified — refusing to overwrite it; rerun",
            link.display()
        )));
    }
    let name = link.file_name().unwrap_or_default().to_string_lossy();
    let tmp = link.with_file_name(format!(".{name}.client-update-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    symlink(target, &tmp)?;
    if let Err(e) = fs::rename(&tmp, link) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}
