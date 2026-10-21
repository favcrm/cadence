//! CAD-1429 independent acceptance: `cadence web` routing from a client-only
//! installation (driver seam: `src/cli/action_dispatch.rs`, `Commands::Web`
//! resolved before the client-only router).
//!
//! Written by the acceptance author, not the source implementer. It runs the
//! ACTUAL built `cadence` binary (`CARGO_BIN_EXE_cadence`) installed the same
//! way as the CAD-1425 fixture: a copied release under `<prefix>/releases/<tag>/`
//! with checksum and manifest, a `<prefix>/client-install.json` receipt
//! (schema 1, kind "client-only") and a separate link. It runs under
//! `env_clear`, owned HOME/XDG/TMPDIR/cwd and a PATH holding only `gh`, `curl`
//! and `wget` sentinels that record any external attempt.
//!
//! What it proves on the real dispatch seam:
//! - Row 1: a client-only install with a saved remote default reaches the
//!   `web` handler. It refuses at the handler's own missing-credential guard
//!   (exit 3), not with the remote allowlist refusal ("not available on a
//!   remote org") and not with the local-daemon refusal. No credential is
//!   created, no sentinel runs, and nothing is printed to stdout.
//! - Row 2: a client-only install with a local default refuses at the `web`
//!   workspace guard ("cadence web needs a remote workspace ... nothing was
//!   sent"), exit 3, before any request or state.
//!
//! It does NOT use a credential, a device grant, TLS hooks or test-seam
//! authority. It does not claim to cover a successful remote web request;
//! that path is blocked on the library-test TLS hook and is not run here.
#![cfg(unix)]

use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_cadence");
const REJECTED_EXIT: i32 = 3;
const REMOTE_ORG: &str = "demo-company";
const REMOTE_ENDPOINT: &str = "https://demo.cadencecloud.app";
const REMOTE_ORG_ID: &str = "ws_fixture";
const FIXTURE_SOURCE_SHA: &str = "1429142914291429142914291429142914291429";
/// The release tag the copied binary is installed under; follows the Cargo version.
const TAG: &str = concat!("v", env!("CARGO_PKG_VERSION"));
const EXECUTED_SENTINEL_LOG: &str = "external-calls.log";
/// The stable remote-allowlist refusal that the pre-fix router produced.
const ALLOWLIST_REFUSAL: &str = "not available on a remote org";
/// The stable local-daemon refusal a client-only install gives local commands.
const LOCAL_DAEMON_REFUSAL: &str = "client-only installation cannot run local daemon commands";

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

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

struct Sandbox {
    _root: tempfile::TempDir,
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
            .prefix("c1429route-")
            .tempdir_in("/tmp")
            .unwrap();
        // Canonical so the receipt paths match `current_exe` on hosts where
        // /tmp is a symlink.
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
            external_log: root.join(EXECUTED_SENTINEL_LOG),
        };
        // XDG_STATE_HOME is set but NOT created: any state creation by a
        // refused call would show up as a new path.
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

    /// The saved default remote org in the real `orgs.json` schema. When
    /// `auth_dir` is true, also an owner-only (0700) empty `remote-auth`
    /// directory: a machine that has used the CLI before but holds no grant
    /// for this org. When false, `remote-auth` is genuinely absent (a fresh
    /// machine). No credential file is written in either case, so a
    /// remote-selected `web` call must refuse at the handler's own guard,
    /// never succeed on a fixture token.
    fn write_remote_default(&self, auth_dir: bool) {
        let cadence_config = self.xdg_config.join("cadence");
        fs::create_dir_all(&cadence_config).unwrap();
        if auth_dir {
            let auth = cadence_config.join("remote-auth");
            fs::create_dir_all(&auth).unwrap();
            fs::set_permissions(&auth, fs::Permissions::from_mode(0o700)).unwrap();
        }
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
    }

    /// Copy the real built binary into `<prefix>/releases/<TAG>/` with a
    /// checksum and a CI-shaped manifest, and return the copied binary path.
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

    /// Write the `client-install.json` receipt the installer writes (schema 1).
    fn write_receipt(&self, prefix: &Path, link: &Path) {
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

    /// Install a client-only copy at `<xdg_data>/cadence-client` and link it
    /// as `$HOME/.local/bin/cadence-client`. Returns the link to run.
    fn client_only_link(&self) -> PathBuf {
        let prefix = self.xdg_data.join("cadence-client");
        let bin = self.install_release(&prefix);
        let link = self.home.join(".local/bin/cadence-client");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(&bin, &link).unwrap();
        self.write_receipt(&prefix, &link);
        link
    }

    /// Run `program args` with a cleared environment. Uses the reaper helper
    /// (the repo's `Command::output` ban applies to tests too).
    fn run(&self, program: &Path, args: &[&str]) -> Output {
        cadence_agent::reaper::output(
            Command::new(program)
                .env_clear()
                .env("PATH", self.sentinel_bin.to_str().unwrap())
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

    fn no_external_call(&self) -> bool {
        !self.external_log.exists()
    }

    fn no_credential_material(&self) -> bool {
        fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, found);
                } else {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if name.starts_with("cli-") || name.starts_with("web-") {
                        found.push(p);
                    }
                }
            }
        }
        let mut found = Vec::new();
        walk(&self.xdg_config, &mut found);
        walk(&self.home, &mut found);
        found.is_empty()
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Row 1: a client-only install with a saved remote default reaches the web
/// handler and refuses at its own credential guard.
#[test]
fn client_only_web_with_remote_default_reaches_handler_not_allowlist() {
    let sb = Sandbox::new();
    sb.write_remote_default(true);
    let link = sb.client_only_link();

    let out = sb.run(&link, &["web", "status", "acc-site"]);
    let combined = text(&out);

    assert_eq!(
        out.status.code(),
        Some(REJECTED_EXIT),
        "client-only `web` must refuse with exit {REJECTED_EXIT}, got {:?}\n{combined}",
        out.status
    );
    // The new handler's own guard: the saved org has no credential, so the
    // web module refuses before any request is built.
    assert!(
        combined.contains(&format!("no remote credential for org '{REMOTE_ORG}'")),
        "client-only `web` must reach the web handler's credential guard, got:\n{combined}"
    );
    // Pre-fix router signature: the remote allowlist refused `web`.
    assert!(
        !combined.contains(ALLOWLIST_REFUSAL),
        "client-only `web` was refused by the remote allowlist, not the web handler:\n{combined}"
    );
    // Not the client-only local-daemon refusal (the router's local arm).
    assert!(
        !combined.contains(LOCAL_DAEMON_REFUSAL),
        "client-only `web` fell through to the local-command refusal:\n{combined}"
    );
    assert!(
        out.stdout.is_empty(),
        "a refused web call must print nothing to stdout"
    );
    assert!(
        sb.no_external_call(),
        "a refused web call must not reach gh, curl or wget"
    );
    assert!(
        sb.no_credential_material(),
        "a refused web call must not create credential or pending material"
    );
    assert!(
        !sb.xdg_state.exists(),
        "a refused web call must not create daemon state"
    );
}

/// Row 2: a client-only install with no remote selection refuses at the web
/// workspace guard, before any request, and says nothing was sent.
#[test]
fn client_only_web_with_local_default_refuses_before_any_request() {
    let sb = Sandbox::new();
    let link = sb.client_only_link();

    let out = sb.run(&link, &["web", "status", "acc-site"]);
    let combined = text(&out);

    assert_eq!(
        out.status.code(),
        Some(REJECTED_EXIT),
        "local-default `web` must refuse with exit {REJECTED_EXIT}, got {:?}\n{combined}",
        out.status
    );
    assert!(
        combined.contains("cadence web needs a remote workspace")
            && combined.contains("nothing was sent"),
        "local-default `web` must give the workspace guidance, got:\n{combined}"
    );
    assert!(
        !combined.contains(ALLOWLIST_REFUSAL),
        "local-default `web` reached the remote allowlist:\n{combined}"
    );
    assert!(
        sb.no_external_call(),
        "a refused web call must not reach gh, curl or wget"
    );
    assert!(
        sb.no_credential_material(),
        "a refused web call must not create credential or pending material"
    );
}

/// Row 3: fresh machine. Saved remote default, `remote-auth` genuinely ABSENT.
/// Previously this surfaced as an `internal` ENOENT (exit 70). The fixed
/// behaviour is now covered, not only the known gap: all seven web actions must
/// refuse with exit 3, name login and the missing credential, say nothing was
/// sent, and create no credential directory, pending record, state or external
/// call. Prepare gets a real HTML file so its freeze succeeds and the refusal
/// comes from the credential guard, not from a missing input.
#[test]
fn client_only_web_with_remote_default_and_absent_auth_dir_refuses_every_action() {
    let sb = Sandbox::new();
    sb.write_remote_default(false);
    let link = sb.client_only_link();
    let input = sb.cwd.join("index.html");
    fs::write(&input, "<!doctype html><title>acc</title>").unwrap();
    let input_arg = input.to_str().unwrap().to_string();
    let cases: Vec<Vec<&str>> = vec![
        vec!["web", "prepare", input_arg.as_str(), "--name", "acc-site"],
        vec!["web", "publish", "rel_33333333"],
        vec!["web", "status", "acc-site"],
        vec!["web", "releases", "acc-site"],
        vec!["web", "submissions", "acc-site"],
        vec!["web", "rollback", "acc-site", "rel_33333333"],
        vec!["web", "unpublish", "acc-site"],
    ];
    for args in &cases {
        let out = sb.run(&link, args);
        let combined = text(&out);
        assert_eq!(
            out.status.code(),
            Some(REJECTED_EXIT),
            "{args:?} must refuse with exit {REJECTED_EXIT} on a fresh machine, got {:?}\n{combined}",
            out.status
        );
        let lower = combined.to_lowercase();
        assert!(
            lower.contains("login") && lower.contains("credential"),
            "{args:?} must tell the operator to log in for a credential, got:\n{combined}"
        );
        assert!(
            combined.contains("nothing was sent"),
            "{args:?} must state nothing was sent, got:\n{combined}"
        );
        assert!(
            !combined.contains(ALLOWLIST_REFUSAL),
            "{args:?} was refused by the remote allowlist:\n{combined}"
        );
        assert!(
            !combined.contains(LOCAL_DAEMON_REFUSAL),
            "{args:?} fell through to the local-command refusal:\n{combined}"
        );
        assert!(
            !combined.contains("internal"),
            "{args:?} surfaced as an internal error, not a rejected refusal:\n{combined}"
        );
        assert!(
            out.stdout.is_empty(),
            "{args:?} must print nothing to stdout"
        );
    }
    assert!(
        sb.no_external_call(),
        "fresh-machine web refusals must not reach gh, curl or wget"
    );
    assert!(
        !sb.xdg_config.join("cadence/remote-auth").exists(),
        "a refused web call must not create the remote credential directory"
    );
    assert!(
        sb.no_credential_material(),
        "a refused web call must not create credential or pending material"
    );
    assert!(
        !sb.xdg_state.exists(),
        "a refused web call must not create daemon state"
    );
}
