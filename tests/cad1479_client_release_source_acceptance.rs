//! CAD-1479 INDEPENDENT backend-default release-source acceptance —
//! client-only path on the real built binary.
//!
//! Written by the acceptance author from the ticket contract, not the
//! implementer; the implementer may not edit or weaken this file
//! (AGENTS.md "Gates and security work").
//!
//! ONE deterministic bad case through the real call path: an actual
//! client-only installation (the shape `install.sh --client-only`
//! leaves — receipt `<prefix>/client-install.json`,
//! `<prefix>/releases/<tag>/cadence`, a `cadence-client` link) runs
//! `cadence update --check` with NO `--version`, so target selection
//! must consult the release source. The subprocess is spawned from the
//! SAME test binary, which CI/pre-push builds with
//! `--features test-seam`, so the implementation's own
//! `CADENCE_RELEASE_SOURCE` seam env carries the default source
//! (`releases::resolve_base(None)`) — the owned endpoint the request
//! is minted for is a fixture loopback that answers `entry:null`
//! (backend up, nothing published).
//!
//! The ticket's refusal: the catalogue publishes no public-client
//! release, so `client_update::latest_tag` must refuse "the release
//! source publishes no public-client release yet — refusing" (exit 3),
//! before any fetch or state work. Two-sided proof: the loopback's
//! request log records the real
//! `GET /v1/cadence/releases/latest?product=cadence&profile=public-client`
//! the default selection minted, and a `gh` sentinel on PATH proves the
//! GitHub API route never ran — no silent fallback.
//!
//! DETERMINISM: nothing here touches api-v2.agenticos.hk, DNS, a proxy
//! or any real network — the default is redirected only inside the
//! test-seam child through `CADENCE_RELEASE_SOURCE`, which compiles out
//! of every release build. No production listener, no credential file,
//! no native update, no install/restart, no R2.

#![cfg(all(unix, feature = "test-seam"))]

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

/// The real built binary under test (test-seam build, so its own
/// default-source seam env is honored inside it).
const BIN: &str = env!("CARGO_BIN_EXE_cadence");
const TAG: &str = concat!("v", env!("CARGO_PKG_VERSION"));
const REJECTED_EXIT: i32 = 3;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const TARGET_TRIPLE: &str = "x86_64-linux";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const TARGET_TRIPLE: &str = "aarch64-linux";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TARGET_TRIPLE: &str = "aarch64-macos";
#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "aarch64"),
)))]
const TARGET_TRIPLE: &str = "unsupported";

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

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A loopback catalogue answering every request with the valid empty
/// `latest` envelope; records each request line to `log`.
struct Server {
    base: String,
    log: PathBuf,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn serve_empty_latest(log: PathBuf) -> Server {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
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
                let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
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
                if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&log2) {
                    let _ = writeln!(f, "{line}");
                }
                let body = br#"{"ok":true,"data":{"product":"cadence","profile":"public-client","entry":null}}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(response.as_bytes());
                let _ = s.write_all(body);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return,
        }
    });
    Server {
        base: format!("http://127.0.0.1:{port}"),
        log,
        stop,
        handle: Some(handle),
    }
}

/// THE check: a real client-only installation's `update --check`
/// consults the owned default source (the loopback sees the real
/// public-client latest request), refuses "publishes no public-client
/// release yet" (exit 3), and never invokes `gh` — no GitHub fallback.
#[test]
fn cad1479_client_update_check_defaults_to_owned_source_no_github_fallback() {
    let tmp = tempfile::Builder::new()
        .prefix("c1479-client-")
        .tempdir()
        .unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    let home = root.join("home");
    let xdg_config = root.join("xdg-config");
    let xdg_data = root.join("xdg-data");
    let xdg_state = root.join("xdg-state");
    let cwd = root.join("cwd");
    let sentinel_bin = root.join("sentinel-bin");
    for d in [
        &home,
        &xdg_config,
        &xdg_data,
        &xdg_state,
        &cwd,
        &sentinel_bin,
    ] {
        fs::create_dir_all(d).unwrap();
    }
    // gh/curl/wget sentinels: log then fail — any line means a GitHub
    // route or external fetch ran at all.
    let external_log = root.join("external-calls.log");
    for name in ["gh", "curl", "wget"] {
        let p = sentinel_bin.join(name);
        fs::write(
            &p,
            format!(
                "#!/bin/sh\necho \"{name} $*\" >> \"{}\"\nexit 97\n",
                external_log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }

    // The client-only installation: real binary under
    // <prefix>/releases/<tag>/, checksum + manifest the receipt's own
    // verify_release_dir accepts, receipt, and the link the child runs
    // through.
    let prefix = xdg_data.join("cadence-client");
    let dir = prefix.join("releases").join(TAG);
    fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("cadence");
    fs::copy(BIN, &bin).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    let digest = sha256_hex(&fs::read(&bin).unwrap());
    fs::write(dir.join("cadence.sha256"), format!("{digest}  cadence\n")).unwrap();
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "source_sha": "1111111111111111111111111111111111111111",
            "version": TAG,
            "run_id": 1u64, "run_attempt": 1u64,
            "rustc": "fixture", "cargo": "fixture",
            "features": ["ui"], "checks": ["fmt","clippy","test","build","ui"],
            "target": TARGET_TRIPLE, "runner": RUNNER,
            "sha256": digest, "built_at": "2026-10-16T00:00:00Z",
        }))
        .unwrap(),
    )
    .unwrap();
    let link = home.join(".local/bin/cadence-client");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&bin, &link).unwrap();
    fs::write(
        prefix.join("client-install.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": 1, "kind": "client-only",
            "prefix": prefix.to_str().unwrap(),
            "link": link.to_str().unwrap(),
            "target": TARGET_TRIPLE,
        }))
        .unwrap(),
    )
    .unwrap();

    let requests_log = root.join("requests.log");
    let server = serve_empty_latest(requests_log.clone());

    let path = format!("{}:/usr/bin:/bin", sentinel_bin.display());
    let out = cadence_agent::reaper::output(
        Command::new(&link)
            .env_clear()
            .env("PATH", &path)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &xdg_config)
            .env("XDG_DATA_HOME", &xdg_data)
            .env("XDG_STATE_HOME", &xdg_state)
            .env("TMPDIR", root.join("tmp"))
            // The implementation's own test-seam default-source
            // override — the real `resolve_base(None)` seam entry.
            .env("CADENCE_RELEASE_SOURCE", &server.base)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .args(["update", "--check"]),
    )
    .expect("the client binary must run");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(REJECTED_EXIT),
        "an empty public-client catalogue must refuse (exit {REJECTED_EXIT}), got \
         {:?}\nstdout+stderr: {}{stderr}",
        out.status,
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr.contains("publishes no public-client release"),
        "the refusal must name the empty public-client catalogue, got: {stderr}"
    );
    // Proof the default selection routed to the owned source: the
    // loopback served the exact latest request.
    let requests = fs::read_to_string(&server.log).unwrap_or_default();
    assert!(
        requests.lines().any(|l| l
            == "GET /v1/cadence/releases/latest?product=cadence&profile=public-client HTTP/1.1"),
        "the default must mint the public-client latest request — \
         requests: {requests:?}"
    );
    // No GitHub fallback: the gh sentinel never ran.
    assert!(
        !external_log.exists(),
        "client update must never invoke gh/curl/wget — log: {:?}",
        fs::read_to_string(&external_log).unwrap_or_default()
    );
    // And no local state was created by the refused check.
    assert!(
        !xdg_state.join("cadence").exists(),
        "the refused check must create no state dir"
    );
}

// Keep the unused-import lint quiet on platforms this gate excludes.
#[allow(dead_code)]
fn _unused(_: &Path, _: &Mutex<()>) {}
