//! CAD-1425 independent acceptance check (written by the acceptance author,
//! not the source implementer; AGENTS.md "Gates and security work"). The
//! implementer must not edit or weaken this file.
//!
//! One bad-case check, two rows, each run against the ACTUAL built `cadence`
//! binary (`CARGO_BIN_EXE_cadence`) as a subprocess with `env_clear`, every
//! home/XDG/temp/working directory inside an owned temp root, and a sentinel
//! PATH that holds only `gh`, `curl` and `wget` stand-ins. The sentinels do
//! not stand in for production behaviour: they only record that an external
//! fetch or execution was attempted. No fake CLI is ever executed.
//!
//! Row 1, ordinary receipt-less installation: `$HOME/.local/bin/cadence` links
//! to a real release at `$XDG_DATA_HOME/cadence/releases/<tag>/cadence` with
//! CI-shaped `cadence.sha256` and `manifest.json`, and NO `client-install.json`.
//! `cadence client-update --check` must refuse with the stable text
//! "client-only installation required" and exit 3 (`rejected`), before any
//! network or sentinel call. The saved remote registry
//! (`$XDG_CONFIG_HOME/cadence/orgs.json`) and remote credential
//! (`$XDG_CONFIG_HOME/cadence/remote-auth/cli-<org>.json`) must be unchanged.
//!
//! Row 2, positive client-only installation: the real built binary is copied
//! to `<prefix>/releases/<tag>/cadence` with the same checksum/manifest, a
//! `<prefix>/client-install.json` receipt (schema 1, kind "client-only",
//! absolute prefix, absolute link, release-asset target) and a separate link
//! `$HOME/.local/bin/cadence-client`. Running that copy through its link with
//! a local daemon command (`update --check`) must refuse with
//! "client-only installation cannot run local daemon commands" and exit 3,
//! before any state is created (`$XDG_STATE_HOME/cadence` must not exist).
//!
//! Row 3, invalid provenance: a real client-only installation of v0.0.9 asks
//! `client-update --version v0.1.0`. The owned fixture archive, checksum and
//! manifest are valid; the owned `gh` stand-in fails `attestation verify`.
//! Must refuse with exit 3 at the attestation guard, after `ATTEST_CALLED`,
//! before the candidate (which writes EXECUTED if run) is executed or staged.
//!
//! Row 4, ordinary installer aimed at a client-owned prefix: `sh
//! scripts/install.sh --prefix <client prefix>` must refuse with exit 1 and the
//! text "client-only prefix" before any network call or write.
//!
//! Every row asserts the whole owned temp root is byte-for-byte unchanged
//! (row 3 excludes only its owned gh call log)
//! (file sha256, mode, dir mode and symlink target): active link, install
//! bytes, remote registry, credentials, and no new file or directory anywhere
//! (including sentinel logs).
//!
//! Interface needs (from the approved CAD-1425 interface, not source): the
//! subcommand `client-update` with `--check`; the receipt keys above; the two
//! stable refusal texts; exit code 3. Nothing else is required: no
//! `--link`/`--releases-dir`/state knobs, and no test-seam or operator
//! authority is used.
//!
//! Compiled on unix only, so it runs on Linux and on a future Mac run. Linux
//! execution is NOT Mac execution evidence. A build that compiles zero tests
//! from this file is not a pass.
#![cfg(unix)]

use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_cadence");
const CLIENT_REFUSAL: &str = "client-only installation required";
const LOCAL_REFUSAL: &str = "client-only installation cannot run local daemon commands";
const REJECTED_EXIT: i32 = 3;
const TAG: &str = "v0.1.0";
const REMOTE_ORG: &str = "demo-company";
const REMOTE_ENDPOINT: &str = "https://demo.cadencecloud.app";
const REMOTE_ORG_ID: &str = "ws_fixture";
/// Fixture value in the CI manifest shape; it is not provenance and the
/// check does not rely on it for any verdict.
const FIXTURE_SOURCE_SHA: &str = "1425142514251425142514251425142514251425";
/// The installed client release in the provenance row (the target is TAG).
const INSTALLED_TAG: &str = "v0.0.9";
/// The release tag the two actual-built-binary rows install. The built `cadence`
/// reports this package version, so the fixture follows the Cargo version bump.
const CANDIDATE_TAG: &str = concat!("v", env!("CARGO_PKG_VERSION"));
const GH_CALLS: &str = "gh-calls.log";
const EXECUTED: &str = "EXECUTED";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const TARGET: &str = "x86_64-linux";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const TARGET: &str = "aarch64-linux";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TARGET: &str = "aarch64-macos";
#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "aarch64"),
)))]
const TARGET: &str = "unsupported";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const RUNNER: &str = "ubuntu-22.04";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const RUNNER: &str = "ubuntu-22.04-arm";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const RUNNER: &str = "macos-14";
#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "aarch64"),
)))]
const RUNNER: &str = "unsupported";

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Dir(u32),
    File(String, u32),
    Link(PathBuf),
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Every path under `root` (symlinks not followed), with its content hash or
/// link target and its mode.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Entry> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            let mode = meta.permissions().mode() & 0o7777;
            let value = if meta.file_type().is_symlink() {
                Entry::Link(fs::read_link(&path).unwrap())
            } else if meta.is_dir() {
                stack.push(path.clone());
                Entry::Dir(mode)
            } else {
                Entry::File(sha256_hex(&fs::read(&path).unwrap()), mode)
            };
            out.insert(rel, value);
        }
    }
    out
}

struct Sandbox {
    _root: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    xdg_config: PathBuf,
    xdg_data: PathBuf,
    xdg_state: PathBuf,
    tmp: PathBuf,
    cwd: PathBuf,
    sentinel_bin: PathBuf,
    external_log: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let tempdir = tempfile::Builder::new()
            .prefix("c1425acc-")
            .tempdir_in("/tmp")
            .unwrap();
        // Canonical so receipt paths match what the kernel reports for
        // `current_exe` on hosts where /tmp is a symlink.
        let root = fs::canonicalize(tempdir.path()).unwrap();
        let sb = Sandbox {
            _root: tempdir,
            home: root.join("home"),
            xdg_config: root.join("xdg-config"),
            xdg_data: root.join("xdg-data"),
            xdg_state: root.join("xdg-state"),
            tmp: root.join("tmp"),
            cwd: root.join("cwd"),
            sentinel_bin: root.join("sentinel-bin"),
            external_log: root.join("external-calls.log"),
            root,
        };
        // XDG_STATE_HOME is set but NOT created: any state creation by the
        // refused run then shows up as a new path in the snapshot.
        for dir in [
            &sb.home,
            &sb.xdg_config,
            &sb.xdg_data,
            &sb.tmp,
            &sb.cwd,
            &sb.sentinel_bin,
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        for name in ["gh", "curl", "wget"] {
            let path = sb.sentinel_bin.join(name);
            fs::write(
                &path,
                format!(
                    "#!/bin/sh\necho {name} >> \"{}\"\nexit 97\n",
                    sb.external_log.display()
                ),
            )
            .unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        sb
    }

    /// Saved remote default and a remote credential, written in the real
    /// on-disk locations the CLI reads. The credential bytes are fixture text.
    fn write_remote_state(&self) {
        let cadence_config = self.xdg_config.join("cadence");
        fs::create_dir_all(&cadence_config).unwrap();
        fs::write(
            cadence_config.join("orgs.json"),
            serde_json::to_vec_pretty(&json!({
                "selected": {"org": REMOTE_ORG},
                "connections": [{
                    "selection": {"org": REMOTE_ORG},
                    "destination": {
                        "mode": "remote",
                        "endpoint": REMOTE_ENDPOINT,
                        "org_id": REMOTE_ORG_ID,
                    },
                }],
            }))
            .unwrap(),
        )
        .unwrap();
        let auth = cadence_config.join("remote-auth");
        fs::create_dir_all(&auth).unwrap();
        fs::set_permissions(&auth, fs::Permissions::from_mode(0o700)).unwrap();
        let cred = auth.join(format!("cli-{REMOTE_ORG}.json"));
        fs::write(
            &cred,
            format!(
                "{{\"org\":\"{REMOTE_ORG}\",\"org_id\":\"{REMOTE_ORG_ID}\",\
                 \"endpoint\":\"{REMOTE_ENDPOINT}\",\"token\":\"hct_fixture_not_a_secret\"}}"
            ),
        )
        .unwrap();
        fs::set_permissions(&cred, fs::Permissions::from_mode(0o600)).unwrap();
    }

    /// Copy the real built binary into `<prefix>/releases/<TAG>/` with the
    /// installer's checksum and manifest files. Returns the release binary.
    fn install_release(&self, prefix: &Path) -> PathBuf {
        let dir = prefix.join("releases").join(TAG);
        fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("cadence");
        fs::copy(BIN, &bin).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let digest = sha256_hex(&fs::read(&bin).unwrap());
        fs::write(dir.join("cadence.sha256"), format!("{digest}  cadence\n")).unwrap();
        fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec_pretty(&json!({
                "source_sha": FIXTURE_SOURCE_SHA,
                "version": TAG,
                "target": TARGET,
                "sha256": digest,
            }))
            .unwrap(),
        )
        .unwrap();
        bin
    }

    fn link(&self, link: &Path, target: &Path) {
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(target, link).unwrap();
    }

    /// Run `program args` with a cleared environment: owned HOME, XDG, TMPDIR
    /// and cwd, and a PATH that holds only the sentinel stand-ins.
    fn run(&self, program: &Path, args: &[&str]) -> Output {
        self.run_with_path(program, args, self.sentinel_bin.to_str().unwrap())
    }

    /// `run` with an explicit PATH (rows 3 and 4).
    fn run_with_path(&self, program: &Path, args: &[&str], path: &str) -> Output {
        cadence_agent::reaper::output(
            Command::new(program)
                .env_clear()
                .env("PATH", path)
                .env("HOME", &self.home)
                .env("XDG_CONFIG_HOME", &self.xdg_config)
                .env("XDG_DATA_HOME", &self.xdg_data)
                .env("XDG_STATE_HOME", &self.xdg_state)
                .env("TMPDIR", &self.tmp)
                .current_dir(&self.cwd)
                .stdin(Stdio::null())
                .args(args),
        )
        .expect("cadence binary must run")
    }
}

fn assert_refused(out: &Output, text: &str) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(REJECTED_EXIT),
        "refusal must exit {REJECTED_EXIT} (rejected), got {:?}\n{stderr}",
        out.status
    );
    assert!(
        stderr.contains(text),
        "refusal must carry the stable text {text:?}, got:\n{stderr}"
    );
}

fn assert_no_external_call(sb: &Sandbox) {
    assert!(
        !sb.external_log.exists(),
        "refused run must not reach gh, curl or wget, log: {:?}",
        fs::read_to_string(&sb.external_log).ok()
    );
}

/// The full CI manifest (the fields `scripts/clean-install.py` checks), bound
/// to a tag, runner and binary digest.
fn full_manifest(tag: &str, runner: &str, digest: &str) -> serde_json::Value {
    json!({
        "source_sha": FIXTURE_SOURCE_SHA,
        "version": tag,
        "run_id": 1,
        "run_attempt": 1,
        "rustc": "rustc fixture",
        "cargo": "cargo fixture",
        "features": ["ui"],
        "target": TARGET,
        "runner": runner,
        "checks": ["fmt", "clippy", "test", "build", "ui"],
        "sha256": digest,
        "built_at": "2026-10-13T00:00:00Z",
    })
}

/// A client-only release dir for `tag` with the full manifest, so the
/// installed-release check passes and `client-update` reaches the download.
fn install_full_release(prefix: &Path, tag: &str, runner: &str) -> PathBuf {
    let dir = prefix.join("releases").join(tag);
    fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("cadence");
    fs::copy(BIN, &bin).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    let digest = sha256_hex(&fs::read(&bin).unwrap());
    fs::write(dir.join("cadence.sha256"), format!("{digest}  cadence\n")).unwrap();
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&full_manifest(tag, runner, &digest)).unwrap(),
    )
    .unwrap();
    bin
}

/// The client-install.json receipt for `prefix` and `link` (schema 1).
fn write_receipt(prefix: &Path, link: &Path) {
    fs::write(
        prefix.join("client-install.json"),
        serde_json::to_vec_pretty(&json!({
            "schema": 1,
            "kind": "client-only",
            "prefix": prefix.to_str().unwrap(),
            "link": link.to_str().unwrap(),
            "target": TARGET,
        }))
        .unwrap(),
    )
    .unwrap();
}

/// Owned fixture archive `cadence-<tag>-<TARGET>.tar.gz` with regular members
/// cadence, cadence.sha256 and manifest.json. Its binary is harmless shell
/// bytes that record EXECUTED if ever run. Returns the archive and the sha256
/// line naming it.
fn write_candidate_archive(root: &Path, tag: &str, runner: &str) -> (PathBuf, PathBuf) {
    let fixtures = root.join("fixtures");
    let stage = fixtures.join("stage");
    fs::create_dir_all(&stage).unwrap();
    let candidate = format!(
        "#!/bin/sh\necho executed >> '{}'\n",
        root.join(EXECUTED).display()
    );
    fs::write(stage.join("cadence"), &candidate).unwrap();
    let digest = sha256_hex(candidate.as_bytes());
    fs::write(stage.join("cadence.sha256"), format!("{digest}  cadence\n")).unwrap();
    fs::write(
        stage.join("manifest.json"),
        serde_json::to_vec_pretty(&full_manifest(tag, runner, &digest)).unwrap(),
    )
    .unwrap();
    let asset = format!("cadence-{tag}-{TARGET}.tar.gz");
    let archive = fixtures.join(&asset);
    let tar = cadence_agent::reaper::output(
        Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&stage)
            .args(["cadence", "cadence.sha256", "manifest.json"]),
    )
    .expect("tar must run");
    assert!(tar.status.success(), "fixture archive must build");
    let archive_digest = sha256_hex(&fs::read(&archive).unwrap());
    let sha_file = fixtures.join(format!("{asset}.sha256"));
    fs::write(&sha_file, format!("{archive_digest}  {asset}\n")).unwrap();
    (archive, sha_file)
}

/// A `gh` stand-in for row 3 only: `release download` copies the owned fixture
/// archive and checksum into the requested `--dir`; `attestation verify`
/// records ATTEST_CALLED and exits nonzero. Returns the directory holding it.
fn write_fake_gh(root: &Path, archive: &Path, sha_file: &Path) -> PathBuf {
    let bin_dir = root.join("provenance-bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let script = format!(
        r#"#!/bin/sh
echo "$*" >> '{calls}'
case "$1 $2" in
"release download")
  dir=
  prev=
  for a in "$@"; do
    if [ "$prev" = --dir ]; then dir=$a; fi
    prev=$a
  done
  cp '{archive}' '{sha}' "$dir/" || exit 1
  ;;
"attestation verify")
  echo ATTEST_CALLED >> '{calls}'
  exit 1
  ;;
*) exit 97 ;;
esac
"#,
        calls = root.join(GH_CALLS).display(),
        archive = archive.display(),
        sha = sha_file.display(),
    );
    let gh = bin_dir.join("gh");
    fs::write(&gh, script).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    bin_dir
}

#[test]
fn client_update_refuses_receipt_less_and_local_daemon_commands_before_effect() {
    // ---- Row 1: ordinary receipt-less installation, client-update refused.
    {
        let sb = Sandbox::new();
        sb.write_remote_state();
        let prefix = sb.xdg_data.join("cadence");
        let bin = sb.install_release(&prefix);
        let link = sb.home.join(".local/bin/cadence");
        sb.link(&link, &bin);
        assert!(
            !prefix.join("client-install.json").exists(),
            "fixture must be receipt-less"
        );

        let before = snapshot(&sb.root);
        let out = sb.run(&link, &["client-update", "--check"]);

        assert_refused(&out, CLIENT_REFUSAL);
        assert_no_external_call(&sb);
        assert!(!sb.xdg_state.exists(), "refusal must create no state dir");
        assert_eq!(
            snapshot(&sb.root),
            before,
            "receipt-less refusal must leave the active link, install bytes, \
             remote registry and credentials unchanged, and create nothing"
        );
    }

    // ---- Row 2: positive client-only installation, local daemon command
    //      refused from the copied real binary.
    {
        let sb = Sandbox::new();
        sb.write_remote_state();
        let prefix = sb.xdg_data.join("cadence-client");
        let bin = sb.install_release(&prefix);
        let link = sb.home.join(".local/bin/cadence-client");
        sb.link(&link, &bin);
        fs::write(
            prefix.join("client-install.json"),
            serde_json::to_vec_pretty(&json!({
                "schema": 1,
                "kind": "client-only",
                "prefix": prefix.to_str().unwrap(),
                "link": link.to_str().unwrap(),
                "target": TARGET,
            }))
            .unwrap(),
        )
        .unwrap();

        let before = snapshot(&sb.root);
        let out = sb.run(&link, &["update", "--check"]);

        assert_refused(&out, LOCAL_REFUSAL);
        assert_no_external_call(&sb);
        assert!(
            !sb.xdg_state.exists(),
            "client-only refusal must not create state before refusing"
        );
        assert_eq!(
            snapshot(&sb.root),
            before,
            "client-only local-command refusal must leave every install, \
             link, registry, credential and state path unchanged"
        );
    }

    // ---- Row 3: invalid provenance from a real client-only installation.
    //      Archive, checksum and manifest are valid; attestation fails. The
    //      refusal must come from the attestation guard, before execution.
    {
        let sb = Sandbox::new();
        sb.write_remote_state();
        let prefix = sb.xdg_data.join("cadence-client");
        let bin = install_full_release(&prefix, INSTALLED_TAG, RUNNER);
        let link = sb.home.join(".local/bin/cadence-client");
        sb.link(&link, &bin);
        write_receipt(&prefix, &link);
        let (archive, sha_file) = write_candidate_archive(&sb.root, TAG, RUNNER);
        let bin_dir = write_fake_gh(&sb.root, &archive, &sha_file);
        let path = format!("{}:/usr/bin:/bin", bin_dir.display());

        let before = snapshot(&sb.root);
        let out = sb.run_with_path(&link, &["client-update", "--version", TAG], &path);

        assert_refused(&out, "did not verify — refusing to execute or install it");
        let calls = fs::read_to_string(sb.root.join(GH_CALLS)).unwrap_or_default();
        assert!(
            calls.lines().any(|l| l.starts_with("release download ")),
            "the fixture download must reach gh, log: {calls:?}"
        );
        assert!(
            calls.contains("ATTEST_CALLED"),
            "the attestation guard must be reached, log: {calls:?}"
        );
        assert!(
            !sb.root.join(EXECUTED).exists(),
            "the candidate must not execute after failed provenance"
        );
        // Only the owned gh call log may differ: scratch download is cleaned.
        let mut after = snapshot(&sb.root);
        after.remove(Path::new(GH_CALLS));
        assert_eq!(
            after, before,
            "failed provenance must leave the active link, install bytes, \
             registry, credentials and staging unchanged"
        );
    }

    // ---- Row 4: ordinary installer aimed at a client-owned prefix. It must
    //      refuse before any network call or write.
    {
        let sb = Sandbox::new();
        sb.write_remote_state();
        let prefix = sb.xdg_data.join("cadence-client");
        let bin = sb.install_release(&prefix);
        let link = sb.home.join(".local/bin/cadence-client");
        sb.link(&link, &bin);
        write_receipt(&prefix, &link);

        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/install.sh");
        let path = format!("{}:/usr/bin:/bin", sb.sentinel_bin.display());
        let before = snapshot(&sb.root);
        let out = sb.run_with_path(
            Path::new("/bin/sh"),
            &[
                script.to_str().unwrap(),
                "--version",
                TAG,
                "--prefix",
                prefix.to_str().unwrap(),
            ],
            &path,
        );

        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(1),
            "ordinary installer on a client prefix must exit 1, got {:?}\n{stderr}",
            out.status
        );
        assert!(
            stderr.contains("client-only prefix"),
            "refusal must carry the stable text \"client-only prefix\", got:\n{stderr}"
        );
        assert_no_external_call(&sb);
        assert_eq!(
            snapshot(&sb.root),
            before,
            "installer refusal on a client prefix must write nothing"
        );
    }
}

// ---- CAD-1430 rows (independent acceptance author; the implementer may not
// edit these). Fixtures are owned and local. The `gh` stand-ins answer only
// the download and attestation calls; a stand-in attestation is NOT
// provenance evidence, and public release provenance is not checked here.

const RECEIPT_FILE: &str = "client-install.json";
const EXTERNAL_LOG: &str = "external-calls.log";
const BINARY: &str = "cadence";
const SHA_FILE: &str = "cadence.sha256";
const MANIFEST: &str = "manifest.json";

fn is_full_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The receipt bytes `install.sh` writes (`client_receipt`), so the shell's
/// exact-match check sees what the installer itself would have written.
fn write_shell_receipt(dir: &Path, prefix: &str, link: &str) {
    let text = format!(
        "{{\n  \"schema\": 1,\n  \"kind\": \"client-only\",\n  \"prefix\": \"{prefix}\",\n  \"link\": \"{link}\",\n  \"target\": \"{TARGET}\"\n}}\n"
    );
    fs::write(dir.join(RECEIPT_FILE), text).unwrap();
}

/// The snapshot without the owned sentinel call log, the only file a refused
/// installer run may add.
fn snapshot_without_sentinel_log(sb: &Sandbox) -> BTreeMap<PathBuf, Entry> {
    let mut snap = snapshot(&sb.root);
    snap.remove(Path::new(EXTERNAL_LOG));
    snap
}

/// `scripts/install.sh` with the sentinel PATH (curl, wget and gh only log and
/// exit 97, so nothing is downloaded).
fn run_installer(sb: &Sandbox, args: &[&str]) -> Output {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/install.sh");
    let path = format!("{}:/usr/bin:/bin", sb.sentinel_bin.display());
    let mut argv = vec![script.to_str().unwrap()];
    argv.extend_from_slice(args);
    sb.run_with_path(Path::new("/bin/sh"), &argv, &path)
}

/// Owned archive for `tag` whose binary is the REAL built `cadence`, with a
/// correct outer and inner checksum and a full manifest whose source commit is
/// the one that binary reports for `--version`. client-update then runs the
/// real candidate `--version` and `client-update --help`.
fn write_real_candidate_archive(root: &Path, tag: &str) -> (PathBuf, PathBuf) {
    let version = cadence_agent::reaper::output(Command::new(BIN).arg("--version"))
        .expect("cadence --version must run");
    let version = String::from_utf8_lossy(&version.stdout).trim().to_string();
    let source = version.rsplit('+').next().unwrap_or_default().to_string();
    assert!(
        is_full_sha(&source),
        "built cadence must report a full commit, got {version:?}"
    );
    assert_eq!(
        version,
        format!("cadence {}+{source}", &tag[1..]),
        "the built binary must be a candidate for {tag}"
    );

    let fixtures = root.join("fixtures-real");
    let stage = fixtures.join("stage");
    fs::create_dir_all(&stage).unwrap();
    fs::copy(BIN, stage.join(BINARY)).unwrap();
    fs::set_permissions(stage.join(BINARY), fs::Permissions::from_mode(0o755)).unwrap();
    let digest = sha256_hex(&fs::read(stage.join(BINARY)).unwrap());
    fs::write(stage.join(SHA_FILE), format!("{digest}  cadence\n")).unwrap();
    let mut manifest = full_manifest(tag, RUNNER, &digest);
    manifest["source_sha"] = json!(source);
    fs::write(
        stage.join(MANIFEST),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let asset = format!("cadence-{tag}-{TARGET}.tar.gz");
    let archive = fixtures.join(&asset);
    let tar = cadence_agent::reaper::output(
        Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&stage)
            .args([BINARY, SHA_FILE, MANIFEST]),
    )
    .expect("tar must run");
    assert!(tar.status.success(), "fixture archive must build");
    let sha_file = fixtures.join(format!("{asset}.sha256"));
    let archive_digest = sha256_hex(&fs::read(&archive).unwrap());
    fs::write(&sha_file, format!("{archive_digest}  {asset}\n")).unwrap();
    (archive, sha_file)
}

/// A `gh` stand-in for the positive updater row: `release download` copies the
/// owned archive and checksum into `--dir`; `attestation verify` succeeds.
/// This stands in for the signed attestation only. It is not provenance.
fn write_fake_gh_attest_ok(root: &Path, archive: &Path, sha_file: &Path) -> PathBuf {
    let bin_dir = root.join("provenance-ok-bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let script = format!(
        r#"#!/bin/sh
echo "$*" >> '{calls}'
case "$1 $2" in
"release download")
  dir=
  prev=
  for a in "$@"; do
    if [ "$prev" = --dir ]; then dir=$a; fi
    prev=$a
  done
  cp '{archive}' '{sha}' "$dir/" || exit 1
  ;;
"attestation verify")
  echo ATTEST_CALLED >> '{calls}'
  exit 0
  ;;
*) exit 97 ;;
esac
"#,
        calls = root.join(GH_CALLS).display(),
        archive = archive.display(),
        sha = sha_file.display(),
    );
    let gh = bin_dir.join("gh");
    fs::write(&gh, script).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    bin_dir
}

/// A client-only installation whose prefix is spelled through a symlinked
/// ancestor (`<root>/linkdir -> <root>/realdir`). The receipt prefix and the
/// link keep the raw spelling, as `install.sh` writes them.
struct SymlinkedInstall {
    raw_prefix: PathBuf,
    physical_prefix: PathBuf,
    link: PathBuf,
}

fn symlinked_client_install(sb: &Sandbox) -> SymlinkedInstall {
    let real = sb.root.join("realdir");
    fs::create_dir_all(&real).unwrap();
    symlink(&real, sb.root.join("linkdir")).unwrap();
    let physical_prefix = real.join("cadence-client");
    let raw_prefix = sb.root.join("linkdir/cadence-client");
    install_full_release(&physical_prefix, INSTALLED_TAG, RUNNER);
    let link = sb.home.join(".local/bin/cadence-client");
    sb.link(
        &link,
        &raw_prefix.join("releases").join(INSTALLED_TAG).join(BINARY),
    );
    write_shell_receipt(
        &physical_prefix,
        raw_prefix.to_str().unwrap(),
        link.to_str().unwrap(),
    );
    SymlinkedInstall {
        raw_prefix,
        physical_prefix,
        link,
    }
}

#[test]
fn fresh_client_install_refuses_prerelease_and_build_tags_before_download() {
    for tag in ["v0.1.0-rc.1", "v0.1.0+build.7", "v1.0"] {
        let sb = Sandbox::new();
        let prefix = sb.xdg_data.join("cadence-client");
        let before = snapshot(&sb.root);
        let out = run_installer(
            &sb,
            &[
                "--client-only",
                "--version",
                tag,
                "--prefix",
                prefix.to_str().unwrap(),
            ],
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{tag}: fresh client install must exit 1, got {:?}\n{stderr}",
            out.status
        );
        assert!(
            stderr.contains("is not a stable vMAJOR.MINOR.PATCH release tag"),
            "{tag}: refusal must name the stable tag rule, got:\n{stderr}"
        );
        assert_no_external_call(&sb);
        assert_eq!(
            snapshot(&sb.root),
            before,
            "{tag}: refusal must write nothing"
        );
    }
}

#[test]
fn fresh_client_install_keeps_stable_tag_eligible() {
    let sb = Sandbox::new();
    let prefix = sb.xdg_data.join("cadence-client");
    let before = snapshot(&sb.root);
    let out = run_installer(
        &sb,
        &[
            "--client-only",
            "--version",
            TAG,
            "--prefix",
            prefix.to_str().unwrap(),
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "got {:?}\n{stderr}", out.status);
    assert!(
        !stderr.contains("stable vMAJOR.MINOR.PATCH"),
        "a stable tag must not hit the tag refusal, got:\n{stderr}"
    );
    assert!(
        stderr.contains("download failed"),
        "a stable tag must reach its download (the sentinel refuses it), got:\n{stderr}"
    );
    let calls = fs::read_to_string(&sb.external_log).unwrap_or_default();
    assert!(
        calls.lines().any(|l| l == "curl"),
        "the stable tag must attempt the sentinel download, log: {calls:?}"
    );
    assert_eq!(
        snapshot_without_sentinel_log(&sb),
        before,
        "a stable tag stopped at download must write nothing"
    );
}

#[test]
fn client_update_round_trip_keeps_receipt_prefix_through_symlinked_ancestor() {
    let sb = Sandbox::new();
    sb.write_remote_state();
    let inst = symlinked_client_install(&sb);
    let receipt = inst.physical_prefix.join(RECEIPT_FILE);
    let receipt_before = fs::read(&receipt).unwrap();
    let (archive, sha_file) = write_real_candidate_archive(&sb.root, CANDIDATE_TAG);
    let bin_dir = write_fake_gh_attest_ok(&sb.root, &archive, &sha_file);
    let path = format!("{}:/usr/bin:/bin", bin_dir.display());

    // Updater: the real built CLI, identified through the symlinked prefix.
    let out = sb.run_with_path(
        &inst.link,
        &["client-update", "--version", CANDIDATE_TAG],
        &path,
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(
        out.status.code(),
        Some(0),
        "client-update must install {CANDIDATE_TAG}, got {:?}\n{stderr}",
        out.status
    );
    let new_dir = inst.raw_prefix.join("releases").join(CANDIDATE_TAG);
    assert!(
        stdout.contains(new_dir.to_str().unwrap()),
        "the updater must report the raw receipt spelling, got:\n{stdout}"
    );
    assert_eq!(
        fs::read_link(&inst.link).unwrap(),
        new_dir.join(BINARY),
        "the link must keep the raw receipt spelling"
    );
    assert!(new_dir.join(BINARY).is_file());
    assert_eq!(
        fs::read(&receipt).unwrap(),
        receipt_before,
        "the receipt must not change"
    );
    assert_no_external_call(&sb);
    assert!(!sb.xdg_state.exists(), "the updater must create no state");

    // Installer, same physical installation, raw spelling: it must pass the
    // link and receipt checks and stop only at its own download.
    let before = snapshot_without_sentinel_log(&sb);
    let prefix = inst.raw_prefix.to_str().unwrap();
    let out = run_installer(
        &sb,
        &[
            "--client-only",
            "--version",
            CANDIDATE_TAG,
            "--prefix",
            prefix,
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(1), "got {:?}\n{stderr}", out.status);
    assert!(
        stderr.contains("download failed"),
        "the installer must accept the round-tripped installation, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("does not point into")
            && !stderr.contains("does not match this client-only installation"),
        "the installer must not reject its own installation, got:\n{stderr}"
    );
    let calls = fs::read_to_string(&sb.external_log).unwrap_or_default();
    assert!(calls.lines().any(|l| l == "curl"), "log: {calls:?}");
    assert_eq!(
        snapshot_without_sentinel_log(&sb),
        before,
        "the installer stop must write nothing"
    );

    // Ordinary installer, strict exclusion: never into a client-only prefix,
    // and never over a link into one.
    let before = snapshot_without_sentinel_log(&sb);
    let out = run_installer(&sb, &["--version", CANDIDATE_TAG, "--prefix", prefix]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(1), "got {:?}\n{stderr}", out.status);
    assert!(
        stderr.contains("client-only prefix"),
        "ordinary install into a client prefix must refuse, got:\n{stderr}"
    );
    assert_eq!(snapshot_without_sentinel_log(&sb), before);

    sb.link(&sb.home.join(".local/bin/cadence"), &new_dir.join(BINARY));
    let fresh = sb.xdg_data.join("cadence");
    let before = snapshot_without_sentinel_log(&sb);
    let out = run_installer(
        &sb,
        &[
            "--version",
            CANDIDATE_TAG,
            "--prefix",
            fresh.to_str().unwrap(),
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(1), "got {:?}\n{stderr}", out.status);
    assert!(
        stderr.contains("points into a client-only prefix"),
        "an ordinary link into a client prefix must refuse, got:\n{stderr}"
    );
    assert_eq!(snapshot_without_sentinel_log(&sb), before);
}

#[test]
fn client_update_refuses_malformed_symlinked_receipts_before_effect() {
    // (a) receipt prefix names a directory that is not the installation.
    // (b) receipt link names a path that is not this installation's link.
    for case in ["prefix", "link"] {
        let sb = Sandbox::new();
        sb.write_remote_state();
        let inst = symlinked_client_install(&sb);
        let (prefix, link) = if case == "prefix" {
            (sb.root.join("linkdir/other-prefix"), inst.link.clone())
        } else {
            (
                inst.raw_prefix.clone(),
                sb.home.join(".local/bin/cadence-missing"),
            )
        };
        write_shell_receipt(
            &inst.physical_prefix,
            prefix.to_str().unwrap(),
            link.to_str().unwrap(),
        );
        let before = snapshot(&sb.root);
        let out = sb.run(&inst.link, &["client-update", "--check"]);
        assert_refused(&out, CLIENT_REFUSAL);
        assert_no_external_call(&sb);
        assert_eq!(
            snapshot(&sb.root),
            before,
            "{case}: a malformed receipt must leave every path unchanged"
        );
    }
}

/// A `gh` stand-in that retargets the owned ancestor alias ONCE, during the
/// source-bound attestation callback (the call that carries `--source-digest`),
/// after the candidate's manifest was read and before the commit lock. The
/// attestation itself succeeds: this is a fixture, not provenance.
fn write_fake_gh_retarget(
    root: &Path,
    archive: &Path,
    sha_file: &Path,
    alias: &Path,
    other: &Path,
) -> PathBuf {
    let bin_dir = root.join("provenance-retarget-bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let calls = root.join(GH_CALLS);
    let script = format!(
        r#"#!/bin/sh
echo "$*" >> '{calls}'
case "$1 $2" in
"release download")
  dir=
  prev=
  for a in "$@"; do
    if [ "$prev" = --dir ]; then dir=$a; fi
    prev=$a
  done
  cp '{archive}' '{sha}' "$dir/" || exit 1
  ;;
"attestation verify")
  case "$*" in
  *--source-digest*)
    if [ "$(grep -c '^RETARGET$' '{calls}' 2>/dev/null)" = 0 ]; then
      echo RETARGET >> '{calls}'
      rm -f '{alias}' && ln -s '{other}' '{alias}' || exit 1
    fi
    ;;
  esac
  exit 0
  ;;
*) exit 97 ;;
esac
"#,
        calls = calls.display(),
        archive = archive.display(),
        sha = sha_file.display(),
        alias = alias.display(),
        other = other.display(),
    );
    let gh = bin_dir.join("gh");
    fs::write(&gh, script).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    bin_dir
}

/// CAD-1430 retarget badcase: the receipt prefix's ancestor alias is pointed at
/// a DIFFERENT owned physical root during the source-bound attestation, after
/// identify() and before the commit lock. The fixed updater must refuse at the
/// canonical prefix recheck with no staging and no link switch in either root.
#[test]
fn client_update_refuses_alias_retarget_before_staging_into_other_root() {
    let sb = Sandbox::new();
    sb.write_remote_state();
    let inst = symlinked_client_install(&sb);
    // Another physical root with a releases dir and a witness file: staging
    // through the raw spelling would mutate it if the recheck were missing.
    let other = sb.root.join("otherdir");
    let other_prefix = other.join("cadence-client");
    fs::create_dir_all(other_prefix.join("releases")).unwrap();
    fs::write(
        other_prefix.join("releases").join("witness.txt"),
        "witness\n",
    )
    .unwrap();
    let alias = sb.root.join("linkdir");
    let (archive, sha_file) = write_real_candidate_archive(&sb.root, CANDIDATE_TAG);
    let bin_dir = write_fake_gh_retarget(&sb.root, &archive, &sha_file, &alias, &other);
    let path = format!("{}:/usr/bin:/bin", bin_dir.display());

    // Expected end state: everything as before, except the owned alias now
    // points at the other root (the fixture's own retarget).
    let mut expected = snapshot(&sb.root);
    let out = sb.run_with_path(
        &inst.link,
        &["client-update", "--version", CANDIDATE_TAG],
        &path,
    );
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    let calls = fs::read_to_string(sb.root.join(GH_CALLS)).unwrap_or_default();
    assert!(
        calls.lines().any(|l| l == "RETARGET"),
        "the alias must be retargeted during the attestation callback, log: {calls:?}"
    );
    assert_eq!(
        fs::read_link(&alias).unwrap(),
        other,
        "the fixture must have retargeted the ancestor alias"
    );
    assert_refused(&out, CLIENT_REFUSAL);
    assert!(
        stderr.contains("the prefix changed while the release was verified"),
        "the refusal must come from the canonical prefix recheck, got:\n{stderr}"
    );
    assert_no_external_call(&sb);
    assert!(!sb.xdg_state.exists(), "the refusal must create no state");

    expected.insert(PathBuf::from("linkdir"), Entry::Link(other.clone()));
    let mut after = snapshot(&sb.root);
    after.remove(Path::new(GH_CALLS));
    assert_eq!(
        after, expected,
        "the refused retarget must stage nothing and switch no link in either root"
    );
    assert!(
        !other_prefix.join("releases").join(CANDIDATE_TAG).exists(),
        "nothing may be staged into the other root"
    );
    assert_eq!(
        fs::read_to_string(other_prefix.join("releases").join("witness.txt")).unwrap(),
        "witness\n",
        "the other root's witness must be untouched"
    );
}

/// Native macOS daemon-update refusal (CAD-1425), moved here from the former
/// standalone file so Linux CI's every-target-runs-a-test floor is met. This
/// module compiles only on macOS; Linux runs nothing from it and that is not
/// Mac execution evidence.
#[cfg(target_os = "macos")]
mod native_macos {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const BIN: &str = env!("CARGO_BIN_EXE_cadence");
    const REFUSAL: &str = "native daemon updates are unsupported on macOS";

    /// Every path under `root` (symlinks not followed), relative to `root`.
    fn snapshot(root: &Path) -> BTreeSet<PathBuf> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                seen.insert(path.strip_prefix(root).unwrap().to_path_buf());
                if fs::symlink_metadata(&path).unwrap().is_dir() {
                    stack.push(path);
                }
            }
        }
        seen
    }

    #[test]
    fn native_update_refuses_on_macos_before_any_effect() {
        let root = tempfile::Builder::new()
            .prefix("c1425-")
            .tempdir_in("/tmp")
            .unwrap();
        let root = root.path();
        let home = root.join("home");
        let xdg_config = root.join("xdg-config");
        let xdg_state = root.join("xdg-state");
        let tmp = root.join("tmp");
        let cwd = root.join("cwd");
        let state = root.join("state");
        for dir in [&home, &xdg_config, &xdg_state, &tmp, &cwd, &state] {
            fs::create_dir_all(dir).unwrap();
        }
        let before = snapshot(root);

        // Inherited environment is dropped entirely, so no CADENCE_* or GH_*
        // variable from the caller can reach the refusal path.
        let output = cadence_agent::reaper::output(
            Command::new(BIN)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &home)
                .env("XDG_CONFIG_HOME", &xdg_config)
                .env("XDG_STATE_HOME", &xdg_state)
                .env("TMPDIR", &tmp)
                .current_dir(&cwd)
                .stdin(std::process::Stdio::null())
                .arg("--state-dir")
                .arg(&state)
                .args(["update", "--check", "--as", "operator:x"]),
        )
        .expect("cadence binary must run");

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "native update on macOS must exit nonzero, got {:?}\n{stderr}",
            output.status
        );
        assert!(
            stderr.contains(REFUSAL),
            "refusal must carry the stable text {REFUSAL:?}, got:\n{stderr}"
        );
        assert!(
            !stderr.contains("/proc/"),
            "refusal must not surface a procfs failure, got:\n{stderr}"
        );
        assert_eq!(
            snapshot(root),
            before,
            "the refused run must create no config, runtime or install artifact"
        );
    }
}
