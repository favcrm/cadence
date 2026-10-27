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
//! `cadence update --version TAG` (receipt-less, CAD-1437) must refuse with the stable text
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
//! a local daemon command (`daemon status`) must refuse with
//! "client-only installation cannot run local daemon commands" and exit 3,
//! before any state is created (`$XDG_STATE_HOME/cadence` must not exist).
//!
//! Row 3, invalid provenance: a real client-only installation of v0.0.9 asks
//! `update --version v0.1.0`. The owned loopback catalogue serves a coherent
//! published entry, archive, checksum, build bundle and publication receipt;
//! the owned `gh` stand-in fails the offline `attestation verify --bundle`.
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
//! subcommand `update` with `--version` and `--check` (CAD-1437 unified
//! interface); the retired `client-update` must be an unknown subcommand; the receipt keys above; the two
//! stable refusal texts; exit code 3. Nothing else is required: no
//! `--link`/`--releases-dir`/state knobs, and no operator authority is used.
//!
//! TRANSPORT FIXTURE (CAD-1479; reworked by the assigned independent fixture
//! maintainer, not the implementer): the client updater's default release
//! source is now the owned `public-client` catalogue over HTTPS
//! (`releases::DEFAULT_BASE`), never the GitHub release/API. The rows that
//! once faked `gh release download`/`gh api` now serve the REAL catalogue
//! contract from an owned loopback: the strict `/v1/cadence` envelopes and
//! the exact artifact bytes (archive, checksum, retained build bundle and
//! the post-publication receipt pair), reached only through the
//! implementation's own debug `CADENCE_RELEASE_SOURCE` test-seam env on the
//! child (which compiles out of every release build). The `gh` stand-in is
//! reduced to the bounded OFFLINE `attestation verify --bundle` calls the
//! real verifier still makes — it answers the documented `--format json`
//! shape (policy only, no crypto) and still carries the failing and
//! retargeting cases. No row's guard or assertion intent changed: every
//! refusal still reaches the same production guard, the candidate binary
//! still must not execute or stage, and the byte-for-byte snapshot checks
//! are unchanged. The source-dependent rows need the `test-seam` feature on
//! the built binary (the loopback seam compiles out of release builds), so
//! they assert `cfg!(feature = "test-seam")` with an actionable message
//! before spawning rather than touching the real network or skipping
//! silently — no row is cfg'd, ignored or filtered out, and on the plain
//! default configuration every row still runs or fails loudly.
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
const HTTP_CALLS: &str = "http-calls.log";
const EXECUTED: &str = "EXECUTED";
/// The owned catalogue's product/profile path segments (the contract's
/// `/v1/cadence` namespace; the fixture mints these, never an endpoint-
/// supplied path).
const PROFILE: &str = "public-client";

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
        self.run_env(program, args, path, &[])
    }

    /// `run_with_path` plus explicit extra env pairs — the ONLY one this
    /// file passes is the implementation's own debug `CADENCE_RELEASE_SOURCE`
    /// loopback seam (compiles out of release builds). Passing it without
    /// the `test-seam` feature refuses loudly: the child would route to the
    /// real production catalogue instead of the owned loopback.
    fn run_env(&self, program: &Path, args: &[&str], path: &str, env: &[(&str, &str)]) -> Output {
        assert!(
            env.is_empty() || cfg!(feature = "test-seam"),
            "this row drives the owned default release source through the debug \
             CADENCE_RELEASE_SOURCE seam — rebuild and run this target with \
             `--features test-seam` (CI's native and cross-build legs do); a \
             binary without the seam would reach the real production catalogue"
        );
        let mut cmd = Command::new(program);
        cmd.env_clear()
            .env("PATH", path)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.xdg_config)
            .env("XDG_DATA_HOME", &self.xdg_data)
            .env("XDG_STATE_HOME", &self.xdg_state)
            .env("TMPDIR", &self.tmp)
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cadence_agent::reaper::output(&mut cmd).expect("cadence binary must run")
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
/// installed-release check passes and `update --version` reaches the download.
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
    let candidate = format!(
        "#!/bin/sh\necho executed >> '{}'\n",
        root.join(EXECUTED).display()
    );
    write_archive_with(root, tag, runner, &candidate)
}

/// Owned archive for `tag` around the given candidate script bytes, with a
/// full manifest and checksums. Returns the archive and its sha256 line.
fn write_archive_with(root: &Path, tag: &str, runner: &str, candidate: &str) -> (PathBuf, PathBuf) {
    let fixtures = root.join("fixtures");
    let stage = fixtures.join("stage");
    fs::create_dir_all(&stage).unwrap();
    fs::write(stage.join("cadence"), candidate).unwrap();
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

// ---- Owned loopback catalogue + offline-bundle gh oracle (CAD-1479
// transport fixture). The `public-client` release source is fetched over the
// real `HttpGh` code path; only the endpoint is a fixture loopback reached
// through the implementation's own debug `CADENCE_RELEASE_SOURCE` seam. `gh`
// is invoked ONLY for `attestation verify --bundle` (the offline Sigstore
// check the real updater still runs); it answers the documented `--format
// json` success shape — a policy oracle, never provenance.

/// An owned loopback server that speaks the REAL `/v1/cadence` contract:
/// strict `{ok,data}`/`{ok:false,error}` envelopes for detail/latest and raw
/// artifact bytes at the canonical artifact paths, each request line logged
/// to the owned http-calls log. It serves only what `routes` lists; anything
/// else is the contract's own `not_found` refusal.
struct Catalogue {
    base: String,
    log: PathBuf,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Catalogue {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn serve_catalogue(root: &Path, routes: Vec<(String, Vec<u8>)>) -> Catalogue {
    use std::io::{Read, Write};
    let log = root.join(HTTP_CALLS);
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let log2 = log.clone();
    let handle = std::thread::spawn(move || loop {
        if stop2.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        match listener.accept() {
            Ok((mut s, _)) => {
                // BSD/macOS accept() inherits the listener's O_NONBLOCK on the
                // connected socket (Linux does not): force it back to blocking
                // so multi-MB archive bodies cannot fail write_all early.
                s.set_nonblocking(false)
                    .expect("accepted stream must be blocking");
                let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(30)));
                let _ = s.set_write_timeout(Some(std::time::Duration::from_secs(30)));
                let mut buf = Vec::new();
                let mut c = [0u8; 8192];
                loop {
                    match s.read(&mut c) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&c[..n]);
                            if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 65536 {
                                break;
                            }
                        }
                    }
                }
                let line = String::from_utf8_lossy(&buf)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                let req = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&log2) {
                    let _ = writeln!(f, "{line}");
                }
                let (status, body) = match routes.iter().find(|(p, _)| *p == req) {
                    Some((_, b)) => ("200 OK", b.clone()),
                    None => (
                        "404 Not Found",
                        br#"{"ok":false,"error":{"code":"not_found","message":"absent"}}"#.to_vec(),
                    ),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(response.as_bytes());
                let _ = s.write_all(&body);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(_) => return,
        }
    });
    Catalogue {
        base: format!("http://127.0.0.1:{port}"),
        log,
        stop,
        handle: Some(handle),
    }
}

/// One artifact slot of the published public-client catalogue entry the
/// fixture serves: route path (the canonical `/v1/cadence/artifacts/…`),
/// body bytes, the entry's artifact metadata and the detail's url record.
struct FixtureArtifacts {
    routes: Vec<(String, Vec<u8>)>,
    meta: Vec<serde_json::Value>,
    urls: Vec<serde_json::Value>,
    /// `sha256:<hex>` of the release archive — binds the receipt and the
    /// catalogue record.
    archive_digest: String,
    /// sha256 hex of the receipt bytes — the receipt bundle's verified
    /// subject digest.
    receipt_digest: String,
    /// The retained build bundle bytes the entry serves.
    build_bundle: Vec<u8>,
    /// The publication bundle bytes the entry serves.
    publication_bundle: Vec<u8>,
}

/// Mint a coherent published `public-client` entry for `tag` around the
/// owned `(archive, sha_file)` pair: the archive, its checksum file, the
/// retained `build-attestation-<target>.jsonl` and the post-publication
/// `publication-receipt.json` + `publication-attestation.jsonl`, every
/// artifact record digest/size-bound to the bytes the route serves (the
/// real consumer re-checks each download against these records).
/// `source` is the manifest's source commit the receipt and signer bind.
fn catalogue_artifacts(
    tag: &str,
    source: &str,
    archive: &Path,
    sha_file: &Path,
) -> FixtureArtifacts {
    let asset = format!("cadence-{tag}-{TARGET}.tar.gz");
    let sha_name = format!("{asset}.sha256");
    let bundle_name = format!("build-attestation-{TARGET}.jsonl");
    let archive_bytes = fs::read(archive).unwrap();
    let archive_digest = sha256_hex(&archive_bytes);
    let sha_bytes = fs::read(sha_file).unwrap();
    let build_bundle = br#"{"bundleRole":"build","signer":"ci.yml"}"#.to_vec();
    let publication_bundle = br#"{"bundleRole":"publication-receipt","signer":"ci.yml"}"#.to_vec();
    // The closed receipt the real `verify_publication_receipt` binds: repo,
    // tag, manifest source and the exact per-target archive digests this
    // entry carries.
    let receipt_bytes = serde_json::to_vec(&json!({
        "schema": 1,
        "repo": "favcrm/cadence",
        "tag": tag,
        "source_sha": source,
        "archives": { asset.clone(): format!("sha256:{archive_digest}") },
        "published_at": "2026-10-16T00:00:00Z",
    }))
    .unwrap();
    let receipt_digest = sha256_hex(&receipt_bytes);
    let art_path = |name: &str| format!("/v1/cadence/artifacts/cadence/{PROFILE}/{tag}/{name}");
    let record = |name: &str, platform: &str, bytes: &[u8], content_type: &str| {
        json!({
            "name": name,
            "profile": PROFILE,
            "platform": platform,
            "sha256": format!("sha256:{}", sha256_hex(bytes)),
            "size": bytes.len(),
            "contentType": content_type,
        })
    };
    let names_and_bytes: Vec<(String, &str, Vec<u8>, &str)> = vec![
        (
            asset.clone(),
            TARGET,
            archive_bytes,
            "application/octet-stream",
        ),
        (sha_name.clone(), TARGET, sha_bytes, "text/plain"),
        (
            bundle_name.clone(),
            TARGET,
            build_bundle.clone(),
            "application/json",
        ),
        (
            cadence_agent::upgrade::PUBLICATION_RECEIPT.to_string(),
            "publication",
            receipt_bytes.clone(),
            "application/json",
        ),
        (
            cadence_agent::upgrade::PUBLICATION_BUNDLE.to_string(),
            "publication",
            publication_bundle.clone(),
            "application/json",
        ),
    ];
    let mut routes = Vec::new();
    let mut meta = Vec::new();
    let mut urls = Vec::new();
    for (name, platform, bytes, content_type) in &names_and_bytes {
        let path = art_path(name);
        meta.push(record(name, platform, bytes, content_type));
        urls.push(json!({"name": name, "url": path}));
        routes.push((path, bytes.clone()));
    }
    FixtureArtifacts {
        routes,
        meta,
        urls,
        archive_digest,
        receipt_digest,
        build_bundle,
        publication_bundle,
    }
}

/// Serve the published detail + latest routes for `tag` on an owned
/// loopback and return the server plus the fixture artifacts.
fn serve_release(
    root: &Path,
    tag: &str,
    source: &str,
    archive: &Path,
    sha_file: &Path,
) -> (Catalogue, FixtureArtifacts) {
    let artifacts = catalogue_artifacts(tag, source, archive, sha_file);
    let entry = json!({
        "id": format!("cadence/{PROFILE}/{tag}"),
        "product": "cadence",
        "version": tag,
        "sourceSha": source,
        "profile": PROFILE,
        "visibility": "public",
        "status": "published",
        "publishedAt": "2026-10-16T00:00:00Z",
        "withdrawnAt": null,
        "recordedAt": "2026-10-16T00:00:00Z",
        "productionCandidateId": null,
        "artifacts": artifacts.meta,
    });
    let mut routes = vec![
        (
            format!("/v1/cadence/releases/cadence/{PROFILE}/{tag}"),
            serde_json::to_vec(
                &json!({"ok":true,"data":{"entry":entry,"artifacts":artifacts.urls}}),
            )
            .unwrap(),
        ),
        (
            format!("/v1/cadence/releases/latest?product=cadence&profile={PROFILE}"),
            serde_json::to_vec(
                &json!({"ok":true,"data":{"product":"cadence","profile":PROFILE,"entry":entry}}),
            )
            .unwrap(),
        ),
    ];
    routes.extend(artifacts.routes.clone());
    (serve_catalogue(root, routes), artifacts)
}

/// The documented `gh attestation verify --format json` success output
/// `check_subjects` parses: one entry whose statement subject binds the
/// expected artifact name+digest and whose certificate records the signer
/// source commit.
fn gh_verify_success(subject: &str, digest: &str, signer_sha: &str) -> String {
    serde_json::to_string(&json!([{
        "verificationResult": {
            "statement": {"subject": [{"name": subject, "digest": {"sha256": digest}}]},
            "signature": {"certificate": {"sourceRepositoryDigest": signer_sha}},
        }
    }]))
    .unwrap()
}

/// Write the shared `gh` oracle prologue: an `attestation verify --bundle`
/// parser that records every call (`ATTEST_CALLED` plus the argv) and
/// enforces the exact policy argv the real `OfflineGh::verify_bundle`
/// mints (repo, exact ci.yml@refs/tags/<tag> cert-identity, issuer,
/// --source-ref, --deny-self-hosted-runners, --format json). `extra_case`
/// is spliced into the per-subject dispatch; `extra_setup` into the script
/// body. The bundle's basename selects the response; bundle BYTES are
/// compared against the fixture copies so a swapped bundle fails.
fn write_gh_oracle(
    bin_dir: &Path,
    calls: &Path,
    tag: &str,
    source: &str,
    mode: &str,
    archive_name: &str,
    fixture: &FixtureArtifacts,
) {
    fs::create_dir_all(bin_dir).unwrap();
    let exp_build = bin_dir.join("expected-build-bundle.jsonl");
    fs::write(&exp_build, &fixture.build_bundle).unwrap();
    let exp_pub = bin_dir.join("expected-publication-bundle.jsonl");
    fs::write(&exp_pub, &fixture.publication_bundle).unwrap();
    let tag_ref = format!("refs/tags/{tag}");
    let build_ok = gh_verify_success(archive_name, &fixture.archive_digest, source);
    let pub_ok = gh_verify_success(
        cadence_agent::upgrade::PUBLICATION_RECEIPT,
        &fixture.receipt_digest,
        source,
    );
    let rcpt_name = cadence_agent::upgrade::PUBLICATION_BUNDLE;
    // `mode` == "fail": every verify exits 1 after logging (invalid
    // provenance bad case). `mode` == "ok": the documented success shape.
    // `mode` == "retarget:<alias>=<other>": on the SECOND verify — the one
    // carrying --source-digest after the manifest was read — retarget the
    // ancestor alias once, then succeed like "ok".
    let on_verify_body = match mode {
        "fail" => "  echo \"fake-gh: attestation policy refused\" >&2\n  exit 1\n".to_string(),
        m if m == "ok" || m.starts_with("retarget:") => {
            let retarget = if let Some(spec) = m.strip_prefix("retarget:") {
                let (alias, other) = spec.split_once('=').expect("retarget spec");
                format!(
                    r#"  if [ -n "$src_digest" ] && [ "$(grep -c '^RETARGET$' '{calls}' 2>/dev/null)" = 0 ]; then
    echo RETARGET >> '{calls}'
    rm -f '{alias}' && ln -s '{other}' '{alias}' || exit 1
  fi
"#,
                    calls = calls.display(),
                    alias = alias,
                    other = other,
                )
            } else {
                String::new()
            };
            format!(
                "{retarget}  case \"$bundle_base\" in\n    expected-build-bundle.jsonl|build-attestation-*.jsonl)\n      cmp -s \"$bundle\" '{exp_build}' || fail \"build bundle bytes differ\"\n      cat <<'JSON'\n{build_ok}\nJSON\n      ;;\n    expected-publication-bundle.jsonl|{rcpt_name})\n      cmp -s \"$bundle\" '{exp_pub}' || fail \"publication bundle bytes differ\"\n      cat <<'JSON'\n{pub_ok}\nJSON\n      ;;\n    *) fail \"unexpected bundle '$bundle'\";;\n  esac\n  exit 0\n",
                exp_build = exp_build.display(),
                exp_pub = exp_pub.display(),
                rcpt_name = rcpt_name,
                build_ok = build_ok,
                pub_ok = pub_ok,
            )
        }
        other => panic!("unknown gh oracle mode {other}"),
    };
    let script = format!(
        r#"#!/bin/sh
# CAD-1425 offline attestation oracle: policy argv + documented json only.
echo "$*" >> '{calls}'
case "$1 $2" in
"attestation verify")
  subject=""; bundle=""; repo=""; ident=""; issuer=""; src_ref=""; fmt=""; src_digest=""; deny=""
  prev=""
  for a in "$@"; do
    case "$prev" in
      --bundle) bundle="$a";;
      --repo) repo="$a";;
      --cert-identity) ident="$a";;
      --cert-oidc-issuer) issuer="$a";;
      --source-ref) src_ref="$a";;
      --source-digest) src_digest="$a";;
      --format) fmt="$a";;
      verify) subject="$a";;
    esac
    [ "$a" = "--deny-self-hosted-runners" ] && deny=1
    prev="$a"
  done
  bundle_base=$(basename "$bundle")
  fail() {{ echo "fake-gh: $1" >&2; exit 1; }}
  echo "ATTEST_CALLED subject=$subject digest=${{src_digest:-NONE}} bundle=$bundle_base" >> '{calls}'
  [ "$repo" = "{repo}" ] || fail "repo '$repo'"
  [ "$fmt" = "json" ] || fail "format '$fmt'"
  [ "$issuer" = "https://token.actions.githubusercontent.com" ] || fail "issuer '$issuer'"
  [ "$src_ref" = "{tag_ref}" ] || fail "source-ref '$src_ref'"
  [ -n "$deny" ] || fail "missing --deny-self-hosted-runners"
  [ "$ident" = "https://github.com/{repo}/.github/workflows/{wf}@{tag_ref}" ] || fail "cert-identity '$ident'"
  # stage-correct: --source-digest is absent pre-manifest, then pinned to
  # the manifest source; a wrong value refuses.
  if [ -n "$src_digest" ] && [ "$src_digest" != "{source}" ]; then fail "--source-digest '$src_digest'"; fi
{on_verify_body}  ;;
*) exit 97 ;;
esac
"#,
        calls = calls.display(),
        repo = "favcrm/cadence",
        wf = cadence_agent::upgrade::WORKFLOW,
        tag_ref = tag_ref,
        source = source,
        on_verify_body = on_verify_body,
    );
    let gh = bin_dir.join("gh");
    fs::write(&gh, script).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn client_update_refuses_receipt_less_and_local_daemon_commands_before_effect() {
    // ---- Row 1: ordinary receipt-less installation, public update refused.
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
        let out = sb.run(&link, &["update", "--version", TAG]);

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
    //      refused from the copied real binary (`daemon status`; plain
    //      `update` is now the public path, CAD-1437).
    {
        let sb = Sandbox::new();
        // No saved remote default: `daemon status` from a client routes to a
        // saved remote org by design, so the local refusal needs a local default.
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
        let out = sb.run(&link, &["daemon", "status"]);

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
        // The owned loopback serves the published release (the manifest's
        // FIXTURE_SOURCE_SHA is the entry's and receipt's source commit);
        // the offline attestation oracle refuses every verify.
        let (catalogue, fixture) =
            serve_release(&sb.root, TAG, FIXTURE_SOURCE_SHA, &archive, &sha_file);
        let bin_dir = sb.root.join("provenance-bin");
        write_gh_oracle(
            &bin_dir,
            &sb.root.join(GH_CALLS),
            TAG,
            FIXTURE_SOURCE_SHA,
            "fail",
            &format!("cadence-{TAG}-{TARGET}.tar.gz"),
            &fixture,
        );
        let path = format!("{}:/usr/bin:/bin", bin_dir.display());

        let before = snapshot(&sb.root);
        let out = sb.run_env(
            &link,
            &["update", "--version", TAG],
            &path,
            &[("CADENCE_RELEASE_SOURCE", catalogue.base.as_str())],
        );

        assert_refused(&out, "did not verify — refusing to execute or install it");
        let http = fs::read_to_string(&catalogue.log).unwrap_or_default();
        assert!(
            http.lines()
                .any(|l| l.starts_with("GET /v1/cadence/releases/cadence/")),
            "the fixture release detail must be fetched from the owned source, log: {http:?}"
        );
        let calls = fs::read_to_string(sb.root.join(GH_CALLS)).unwrap_or_default();
        assert!(
            calls.contains("ATTEST_CALLED"),
            "the attestation guard must be reached, log: {calls:?}"
        );
        assert!(
            !sb.root.join(EXECUTED).exists(),
            "the candidate must not execute after failed provenance"
        );
        // Only the owned gh call log and owned catalogue request log may
        // differ: scratch download is cleaned.
        let mut after = snapshot(&sb.root);
        after.remove(Path::new(GH_CALLS));
        after.remove(Path::new(HTTP_CALLS));
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
    let path = format!("{}:/usr/bin:/bin", sb.sentinel_bin.display());
    run_installer_with_path(sb, args, &path)
}

/// `scripts/install.sh` with an explicit PATH (stand-ins that serve only the
/// fixed release URLs).
fn run_installer_with_path(sb: &Sandbox, args: &[&str], path: &str) -> Output {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/install.sh");
    let mut argv = vec![script.to_str().unwrap()];
    argv.extend_from_slice(args);
    sb.run_with_path(Path::new("/bin/sh"), &argv, path)
}

/// Owned archive for `tag` whose binary is the REAL built `cadence`, with a
/// correct outer and inner checksum and a full manifest whose source commit is
/// the one that binary reports for `--version`. The updater then runs the
/// real candidate `--version` and `update --help`. Returns the archive, its
/// sha256 line file and the manifest's source commit (the built binary's
/// reported commit) for the catalogue entry and receipt binding.
fn write_real_candidate_archive(root: &Path, tag: &str) -> (PathBuf, PathBuf, String) {
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
    (archive, sha_file, source)
}

/// A `gh` stand-in for the INSTALLER rows only: `install.sh --client-only`
/// still fetches GitHub release URLs and calls the live-shaped
/// `gh attestation verify --signer-workflow` (no `--bundle`), so this
/// oracle answers those calls with exit 0. It is used ONLY for installer
/// fixtures (the updater path uses `write_gh_oracle`); a stand-in
/// attestation is NOT provenance evidence.
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
    let (archive, sha_file, source) = write_real_candidate_archive(&sb.root, CANDIDATE_TAG);
    let (catalogue, fixture) = serve_release(&sb.root, CANDIDATE_TAG, &source, &archive, &sha_file);
    let bin_dir = sb.root.join("provenance-ok-bin");
    write_gh_oracle(
        &bin_dir,
        &sb.root.join(GH_CALLS),
        CANDIDATE_TAG,
        &source,
        "ok",
        &format!("cadence-{CANDIDATE_TAG}-{TARGET}.tar.gz"),
        &fixture,
    );
    let path = format!("{}:/usr/bin:/bin", bin_dir.display());

    // Updater: the real built CLI, identified through the symlinked prefix.
    let out = sb.run_env(
        &inst.link,
        &["update", "--version", CANDIDATE_TAG],
        &path,
        &[("CADENCE_RELEASE_SOURCE", catalogue.base.as_str())],
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(
        out.status.code(),
        Some(0),
        "update --version must install {CANDIDATE_TAG}, got {:?}\n{stderr}",
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
        let out = sb.run(&inst.link, &["update", "--version", CANDIDATE_TAG]);
        assert_refused(&out, CLIENT_REFUSAL);
        assert_no_external_call(&sb);
        assert_eq!(
            snapshot(&sb.root),
            before,
            "{case}: a malformed receipt must leave every path unchanged"
        );
    }
}

/// CAD-1430 retarget badcase: the receipt prefix's ancestor alias is pointed at
/// a DIFFERENT owned physical root during the source-bound attestation —
/// under CAD-1479 that is the SECOND offline `attestation verify --bundle`,
/// the one carrying `--source-digest` after the candidate's manifest was
/// read and before the commit lock (the `retarget:` oracle mode below
/// preserves exactly that interception point). The fixed updater must
/// refuse at the canonical prefix recheck with no staging and no link
/// switch in either root.
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
    let (archive, sha_file, source) = write_real_candidate_archive(&sb.root, CANDIDATE_TAG);
    let (catalogue, fixture) = serve_release(&sb.root, CANDIDATE_TAG, &source, &archive, &sha_file);
    let bin_dir = sb.root.join("provenance-retarget-bin");
    write_gh_oracle(
        &bin_dir,
        &sb.root.join(GH_CALLS),
        CANDIDATE_TAG,
        &source,
        &format!("retarget:{}={}", alias.display(), other.display()),
        &format!("cadence-{CANDIDATE_TAG}-{TARGET}.tar.gz"),
        &fixture,
    );
    let path = format!("{}:/usr/bin:/bin", bin_dir.display());

    // Expected end state: everything as before, except the owned alias now
    // points at the other root (the fixture's own retarget).
    let mut expected = snapshot(&sb.root);
    let out = sb.run_env(
        &inst.link,
        &["update", "--version", CANDIDATE_TAG],
        &path,
        &[("CADENCE_RELEASE_SOURCE", catalogue.base.as_str())],
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
    after.remove(Path::new(HTTP_CALLS));
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

// ---- CAD-1437 unified-update rows (independent acceptance author; the
// implementer may not edit these). They run the actual built binary against
// owned fixtures. A `gh` stand-in here is NOT provenance evidence.

const CLIENT_HELP_MARKER: &str = "Upgrade a client-only installation";
/// The native-only `update --help` text from before CAD-1437. It names no
/// client mode and must never qualify a candidate as client-capable.
const NATIVE_UPDATE_HELP: &str =
    "Update to the newest attested green main build in one command (CAD-561)";

/// A `curl` stand-in that serves ONLY the fixed public GitHub release URLs for
/// `tag` from the owned archive and checksum. The installer's client-only path
/// has no mirror seam, so this stands in for the network, not for provenance.
fn write_fake_curl_github(bin_dir: &Path, tag: &str, archive: &Path, sha_file: &Path) {
    let prefix = format!("https://github.com/favcrm/cadence/releases/download/{tag}/");
    let script = format!(
        r#"#!/bin/sh
out=
prev=
for a in "$@"; do
  if [ "$prev" = -o ]; then out=$a; fi
  prev=$a
  url=$a
done
case "$url" in
'{prefix}'*.tar.gz.sha256) cp '{sha}' "$out" || exit 1 ;;
'{prefix}'*.tar.gz) cp '{archive}' "$out" || exit 1 ;;
*) echo "$url" >> '{log}'; exit 97 ;;
esac
"#,
        sha = sha_file.display(),
        archive = archive.display(),
        log = bin_dir.join("curl-refused.log").display(),
    );
    let curl = bin_dir.join("curl");
    fs::write(&curl, script).unwrap();
    fs::set_permissions(&curl, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A validated client-only installation at INSTALLED_TAG, linked at
/// `~/.local/bin/cadence-client`, with a saved remote default org that must
/// not divert a public update.
fn validated_client(sb: &Sandbox) -> PathBuf {
    sb.write_remote_state();
    let prefix = sb.xdg_data.join("cadence-client");
    let bin = install_full_release(&prefix, INSTALLED_TAG, RUNNER);
    let link = sb.home.join(".local/bin/cadence-client");
    sb.link(&link, &bin);
    write_receipt(&prefix, &link);
    link
}

/// Primary check path: `update --check --version TAG` on a validated client
/// selects the tag from the local receipt, even under a saved remote default,
/// and reports that the artifact is neither downloaded nor verified.
#[test]
fn unified_update_check_selects_public_path_from_local_receipt() {
    let sb = Sandbox::new();
    let link = validated_client(&sb);
    let before = snapshot(&sb.root);
    let out = sb.run(&link, &["update", "--check", "--version", TAG]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(
        out.status.code(),
        Some(0),
        "update --check must succeed, got {:?}\n{stderr}",
        out.status
    );
    assert!(
        stdout.contains(TAG),
        "the check must select {TAG}, got:\n{stdout}"
    );
    assert!(
        stdout.contains("not downloaded or verified"),
        "the check must not claim verification, got:\n{stdout}"
    );
    assert_no_external_call(&sb);
    assert!(!sb.xdg_state.exists(), "the check must create no state");
    assert_eq!(snapshot(&sb.root), before, "the check must change nothing");
}

/// The one CAD-1437 bad-case guard entry. Each row runs the actual binary and
/// must refuse before any fetch, state or native action: native-only flags and
/// subcommands on a validated client (including explicitly supplied defaults),
/// the retired `client-update` invocations (unknown, not retained), a receipt-
/// less or malformed identity, and an old native-only help candidate.
#[test]
fn unified_update_refuses_native_flags_and_retired_command_before_effect() {
    let native: &[&[&str]] = &[
        &["update", "--version", TAG, "--keep", "3"],
        &["update", "--version", TAG, "--drain", "10m"],
        &["update", "--version", TAG, "--repo", "favcrm/cadence"],
        &["update", "--version", TAG, "--as", "operator:ada"],
        &["update", "--version", TAG, "--to", FIXTURE_SOURCE_SHA],
        &["update", "--version", TAG, "--now"],
        &["update", "--version", TAG, "--rollback"],
        &["update", "status"],
    ];
    for &args in native {
        let sb = Sandbox::new();
        let link = validated_client(&sb);
        let before = snapshot(&sb.root);
        let out = sb.run(&link, args);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            !out.status.success(),
            "{args:?} on a client must refuse, got {:?}\n{stderr}",
            out.status
        );
        assert!(
            !stderr.trim().is_empty(),
            "{args:?} must carry a refusal, got empty stderr"
        );
        assert_no_external_call(&sb);
        assert!(!sb.xdg_state.exists(), "{args:?} must create no state");
        assert_eq!(snapshot(&sb.root), before, "{args:?} must change nothing");
    }

    // The retired command is an unknown subcommand (clap usage error, exit 2),
    // not an alias, hidden entry or help-only hook.
    let retired: &[&[&str]] = &[
        &["client-update", "--check"],
        &["client-update", "--version", TAG],
        &["client-update", "--help"],
    ];
    for &args in retired {
        let sb = Sandbox::new();
        let link = validated_client(&sb);
        let before = snapshot(&sb.root);
        let out = sb.run(&link, args);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            !out.status.success(),
            "{args:?} must not run, got {:?}\n{stderr}",
            out.status
        );
        assert!(
            stderr.contains("unrecognized subcommand") && stderr.contains("client-update"),
            "{args:?} must be refused as unrecognized, got:\n{stderr}"
        );
        assert!(
            !stdout.contains(CLIENT_HELP_MARKER),
            "{args:?} must not print a retained client help, got:\n{stdout}"
        );
        assert_no_external_call(&sb);
        assert!(!sb.xdg_state.exists(), "{args:?} must create no state");
        assert_eq!(snapshot(&sb.root), before, "{args:?} must change nothing");
    }

    // Receipt-less ordinary install: a native flag on the public selector.
    {
        let sb = Sandbox::new();
        sb.write_remote_state();
        let prefix = sb.xdg_data.join("cadence");
        let bin = sb.install_release(&prefix);
        let link = sb.home.join(".local/bin/cadence");
        sb.link(&link, &bin);
        let before = snapshot(&sb.root);
        let out = sb.run(&link, &["update", "--version", TAG, "--keep", "3"]);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            !out.status.success(),
            "receipt-less update with a native flag must refuse, got {:?}\n{stderr}",
            out.status
        );
        assert_no_external_call(&sb);
        assert!(!sb.xdg_state.exists(), "the refusal must create no state");
        assert_eq!(
            snapshot(&sb.root),
            before,
            "the refusal must change nothing"
        );
    }

    // Malformed receipt (ambiguous identity) cannot opt into a public update.
    {
        let sb = Sandbox::new();
        sb.write_remote_state();
        let inst = symlinked_client_install(&sb);
        let other = sb.root.join("linkdir/other-prefix");
        write_shell_receipt(
            &inst.physical_prefix,
            other.to_str().unwrap(),
            inst.link.to_str().unwrap(),
        );
        let before = snapshot(&sb.root);
        let out = sb.run(
            &inst.link,
            &["update", "--version", CANDIDATE_TAG, "--keep", "3"],
        );
        assert_refused(&out, CLIENT_REFUSAL);
        assert_no_external_call(&sb);
        assert!(!sb.xdg_state.exists(), "the refusal must create no state");
        assert_eq!(
            snapshot(&sb.root),
            before,
            "the refusal must change nothing"
        );
    }

    // A candidate whose `update --help` is only the old native help must be
    // refused by the client capability probe, before any install. The shell
    // stand-in runs only as the candidate; any other call records EXECUTED.
    {
        let sb = Sandbox::new();
        let link = validated_client(&sb);
        let candidate = format!(
            "#!/bin/sh\ncase \"$1\" in\n--version) echo 'cadence {}+{}' ;;\nupdate) echo '{}' ;;\n*) echo executed >> '{}' ;;\nesac\n",
            &CANDIDATE_TAG[1..],
            FIXTURE_SOURCE_SHA,
            NATIVE_UPDATE_HELP,
            sb.root.join(EXECUTED).display()
        );
        let (archive, sha_file) = write_archive_with(&sb.root, CANDIDATE_TAG, RUNNER, &candidate);
        let (catalogue, fixture) = serve_release(
            &sb.root,
            CANDIDATE_TAG,
            FIXTURE_SOURCE_SHA,
            &archive,
            &sha_file,
        );
        let bin_dir = sb.root.join("provenance-ok-bin");
        write_gh_oracle(
            &bin_dir,
            &sb.root.join(GH_CALLS),
            CANDIDATE_TAG,
            FIXTURE_SOURCE_SHA,
            "ok",
            &format!("cadence-{CANDIDATE_TAG}-{TARGET}.tar.gz"),
            &fixture,
        );
        let path = format!("{}:/usr/bin:/bin", bin_dir.display());
        let before = snapshot(&sb.root);
        let out = sb.run_env(
            &link,
            &["update", "--version", CANDIDATE_TAG],
            &path,
            &[("CADENCE_RELEASE_SOURCE", catalogue.base.as_str())],
        );
        assert_refused(&out, "lacks client-only support");
        assert!(
            !sb.root.join(EXECUTED).exists(),
            "a candidate without the client marker must not execute beyond its probe"
        );
        let mut after = snapshot(&sb.root);
        after.remove(Path::new(GH_CALLS));
        after.remove(Path::new(HTTP_CALLS));
        assert_eq!(
            after, before,
            "the old-help candidate must install nothing and change no link"
        );
    }

    // The installer's own probe refuses the same old-help candidate, before
    // any receipt, link or prefix write.
    {
        let sb = Sandbox::new();
        let candidate = format!(
            "#!/bin/sh\ncase \"$1\" in\n--version) echo 'cadence {}+{}' ;;\nupdate) echo '{}' ;;\n*) echo executed >> '{}' ;;\nesac\n",
            &CANDIDATE_TAG[1..],
            FIXTURE_SOURCE_SHA,
            NATIVE_UPDATE_HELP,
            sb.root.join(EXECUTED).display()
        );
        let (archive, sha_file) = write_archive_with(&sb.root, CANDIDATE_TAG, RUNNER, &candidate);
        let bin_dir = write_fake_gh_attest_ok(&sb.root, &archive, &sha_file);
        write_fake_curl_github(&bin_dir, CANDIDATE_TAG, &archive, &sha_file);
        let path = format!("{}:/usr/bin:/bin", bin_dir.display());
        let prefix = sb.xdg_data.join("cadence-client");
        let before = snapshot(&sb.root);
        let out = run_installer_with_path(
            &sb,
            &[
                "--client-only",
                "--version",
                CANDIDATE_TAG,
                "--prefix",
                prefix.to_str().unwrap(),
            ],
            &path,
        );
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert_eq!(out.status.code(), Some(1), "got {:?}\n{stderr}", out.status);
        assert!(
            stderr.contains("is not capable of client-only installs"),
            "the installer must refuse the native-only help, got:\n{stderr}"
        );
        assert!(
            !sb.root.join(EXECUTED).exists(),
            "the installer probe must not execute the candidate beyond its probe"
        );
        let mut after = snapshot(&sb.root);
        after.remove(Path::new(GH_CALLS));
        assert_eq!(
            after, before,
            "the installer refusal must write no prefix, receipt or link"
        );
    }
}

/// Primary installer path: `install.sh --client-only` with the real unified
/// candidate writes the receipt and the `cadence-client` link after its probe.
/// The `gh` and `curl` stand-ins are NOT provenance: they only answer the
/// fixed download URLs and a passing attestation call.
#[test]
fn unified_installer_probe_accepts_real_candidate_and_links_client() {
    let sb = Sandbox::new();
    let (archive, sha_file, _source) = write_real_candidate_archive(&sb.root, CANDIDATE_TAG);
    let bin_dir = write_fake_gh_attest_ok(&sb.root, &archive, &sha_file);
    write_fake_curl_github(&bin_dir, CANDIDATE_TAG, &archive, &sha_file);
    let path = format!("{}:/usr/bin:/bin", bin_dir.display());
    let prefix = sb.xdg_data.join("cadence-client");
    let out = run_installer_with_path(
        &sb,
        &[
            "--client-only",
            "--version",
            CANDIDATE_TAG,
            "--prefix",
            prefix.to_str().unwrap(),
        ],
        &path,
    );
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(
        out.status.code(),
        Some(0),
        "installer must accept the unified candidate, got {:?}\n{stderr}",
        out.status
    );
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(prefix.join(RECEIPT_FILE)).unwrap()).unwrap();
    assert_eq!(receipt["kind"], "client-only");
    let dest = prefix.join("releases").join(CANDIDATE_TAG).join(BINARY);
    assert!(dest.is_file(), "the release must be installed at {dest:?}");
    assert_eq!(
        fs::read_link(sb.home.join(".local/bin/cadence-client")).unwrap(),
        dest,
        "the cadence-client link must point at the installed release"
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

// Independently owned OMP CAD-1128 legacy artifact compatibility oracles.
// Keep these on the existing macOS/ARM integration target, with no lib mount.
#[path = "cad1425_client_update_refusal_acceptance/omp_security_compatibility.rs"]
mod omp_cad1128_security_compatibility;
