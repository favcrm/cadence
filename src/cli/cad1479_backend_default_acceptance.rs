//! CAD-1479 INDEPENDENT backend-default release-source acceptance —
//! native `cadence update` path.
//!
//! Written by the acceptance author from the ticket contract, not the
//! implementer; the implementer may not edit or weaken this file
//! (AGENTS.md "Gates and security work"). Wired as a
//! `#[cfg(all(test, feature = "test-seam"))]` sibling of `cli::update`,
//! beside update_routing_acceptance/update_identity_acceptance.
//!
//! Ticket contract under test — the REAL routed call path, never a
//! fake that mirrors new client code: clap argv ->
//! `Cli::try_parse_from` -> `action_dispatch::run_cli` ->
//! `cli::update::run_update`, with the caller's operator assertion
//! carried by the repo's own seam (`scoped(Asserted::Operator)` +
//! minted `<state>/seam/token`, the update_routing_acceptance
//! convention).
//!
//!   A. Ordinary `update --check` (no `--release-source`) selects the
//!      owned backend, never the GitHub route. PROOF AT BOTH SIDES: an
//!      owned gh sentinel on PATH logs+refuses any GitHub call (the
//!      sentinel staying silent proves no GitHub API/CLI use), and an
//!      owned loopback listener records the actual catalogue request —
//!      `CADENCE_RELEASE_SOURCE` is the implementation's own test-seam
//!      default-source override (`releases::resolve_base`), honored
//!      only in seam builds, so the request the REAL default selection
//!      mints lands on the fixture.
//!   C. An unavailable backend (dead loopback port) refuses with no
//!      gh call; an env default that is not a plain https:// origin
//!      refuses at the origin guard before any fetch.
//!   D. The explicit `--release-source https://…` override still goes
//!      through `resolve_base(Some)` — exercised with an unroutable
//!      https origin; the refusal names the configured source and the
//!      gh sentinel stays silent.
//!   E. Fences preserved: an image-managed runtime (`hosted:` lease in
//!      the fixture pm.yaml) refuses BEFORE any source work, and a
//!      pane caller (`CADENCE_ALIAS`) still stops at the operator
//!      guard. Neither run may touch the loopback or gh.
//!   F. Pin: `releases::DEFAULT_BASE` is exactly
//!      `https://api-v2.agenticos.hk`, and `resolve_base(None)` with
//!      no env returns exactly that origin (the pinned production
//!      selection; the loopback cases above carry the routing proof).
//!
//! The client-only path's real-binary rows live in the integration
//! test `tests/cad1479_client_release_source_acceptance.rs` —
//! `CARGO_BIN_EXE_cadence` is defined only for integration targets,
//! never for this bin crate's unit tests, so a unit module cannot
//! spawn the real `cadence`. Together the two files cover the ticket's
//! native AND client surfaces on real call paths.
//!
//! DETERMINISM: every request the checks rely on is served or refused
//! by an owned loopback server; nothing here assumes the real
//! api-v2.agenticos.hk is reachable, empty or live — the rows stand
//! identically when public-client goes live.
//!
//! LIMITS: no production listener (fixed HTTPS is never opened), no
//! credential files, no R2 or tenant mutation. TLS and real catalogue
//! contents remain deployment evidence.

#![cfg(all(test, feature = "test-seam"))]

use super::action_dispatch::run_cli;
use super::*;
use cadence_agent::test_seam::{self, Asserted};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

/// The owned backend the ticket pins as the default release source.
const OWNED_BACKEND: &str = "https://api-v2.agenticos.hk";
/// The AOS-255 catalogue prefix the owned source mints requests under.
const RELEASES_PREFIX: &str = "/v1/cadence";

/// Serialize env mutation across this module's cases — each binds its
/// env through `EnvGuard` under the lock, so no two cases race on
/// process-global variables.
static SERIAL: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// owned loopback catalogue (the HttpGh test-seam's only allowed origin)
// ---------------------------------------------------------------------------

/// An HTTP listener on an owned loopback port that records every
/// request target and answers a fixed status/body — the stand-in for
/// the owned endpoint inside the test-seam default. Serves at most
/// `max` connections then stops.
struct Loopback {
    base: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    requests: std::sync::Arc<Mutex<Vec<String>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn serve_requests(status: &str, body: Vec<u8>, max: usize) -> Loopback {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let requests: std::sync::Arc<Mutex<Vec<String>>> = Default::default();
    let (stop2, reqs2) = (stop.clone(), requests.clone());
    let status = status.to_string();
    let handle = std::thread::spawn(move || {
        let mut served = 0usize;
        loop {
            if stop2.load(std::sync::atomic::Ordering::SeqCst) || served >= max {
                return;
            }
            match listener.accept() {
                Ok((mut s, _)) => {
                    served += 1;
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
                    let head = String::from_utf8_lossy(&buf).into_owned();
                    let mut parts = head.split_whitespace().map(str::to_string);
                    let method = parts.next().unwrap_or_default();
                    let path = parts.next().unwrap_or_default();
                    reqs2.lock().unwrap().push(format!("{method} {path}"));
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = s.write_all(response.as_bytes());
                    let _ = s.write_all(&body);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => return,
            }
        }
    });
    Loopback {
        base: format!("http://127.0.0.1:{port}"),
        stop,
        requests,
        handle: Some(handle),
    }
}

/// An owned loopback port with NOTHING listening — the unavailable
/// backend (connection refused).
fn dead_base() -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

fn loopback_paths(server: &Loopback) -> Vec<String> {
    server.requests.lock().unwrap().clone()
}

// ---------------------------------------------------------------------------
// fixture root: owned HOME/XDG + gh sentinel + armed state dir
// ---------------------------------------------------------------------------

struct Root {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    home: PathBuf,
    xdg_config: PathBuf,
    xdg_data: PathBuf,
    xdg_state: PathBuf,
    sentinels: PathBuf,
    external_log: PathBuf,
}

impl Root {
    fn new() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("c1479-acc-")
            .tempdir()
            .unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        let sb = Root {
            home: dir.join("home"),
            xdg_config: dir.join("xdg-config"),
            xdg_data: dir.join("xdg-data"),
            xdg_state: dir.join("xdg-state"),
            sentinels: dir.join("sentinels"),
            external_log: dir.join("external-calls.log"),
            dir,
            _tmp: tmp,
        };
        for path in [
            &sb.home,
            &sb.xdg_config,
            &sb.xdg_data,
            &sb.xdg_state,
            &sb.sentinels,
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        // A gh stand-in: it only RECORDS that the GitHub CLI was
        // invoked, then fails. Any logged line is a red flag — the
        // update paths must never reach `gh` on the owned-source route.
        let gh = sb.sentinels.join("gh");
        std::fs::write(
            &gh,
            format!(
                "#!/bin/sh\necho \"gh $*\" >> \"{}\"\nexit 97\n",
                sb.external_log.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        sb
    }

    fn external_calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.external_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The ambient state dir `client::state_dir()` resolves under this
    /// XDG_STATE_HOME, with a minted seam token so the scoped operator
    /// assertion is honored on it (the update_routing_acceptance
    /// convention).
    fn arm_state(&self) -> PathBuf {
        let state = self.xdg_state.join("cadence");
        std::fs::create_dir_all(state.join("seam")).unwrap();
        std::fs::write(state.join("seam/token"), "cad1479-fixture\n").unwrap();
        state
    }

    /// The env the routed run reads, bound to this fixture. Returns the
    /// guard restoring everything on drop.
    fn bind_env(&self) -> EnvGuard {
        EnvGuard::new(&[
            ("HOME", self.home.to_str()),
            ("XDG_CONFIG_HOME", self.xdg_config.to_str()),
            ("XDG_DATA_HOME", self.xdg_data.to_str()),
            ("XDG_STATE_HOME", self.xdg_state.to_str()),
            // An owned pm dir with no pm.yaml: the hosted lookup reads
            // lifecycle-off unless a case writes a hosted lease.
            ("CADENCE_PM_DIR", self.dir.join("pm").to_str()),
            ("CADENCE_ALIAS", None),
            ("CADENCE_ORG", None),
            ("CADENCE_STATE_DIR", None),
            ("CADENCE_HOME", None),
            ("CADENCE_PROFILE", None),
            ("CADENCE_ROLLOUT_AS", None),
            ("CADENCE_TEST_AS", None),
        ])
    }

    /// The linked-release fixture: `~/.local/bin/cadence` ->
    /// `<xdg-data>/cadence/releases/<sha>/cadence` so
    /// `upgrade::current` resolves a known sha and target selection
    /// runs against the source.
    fn link_release(&self) {
        let sha = "2222222222222222222222222222222222222222";
        let releases = self.xdg_data.join("cadence/releases");
        let bin = releases.join(sha).join("cadence");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "installed\n").unwrap();
        let link = self.home.join(".local/bin/cadence");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&bin, &link).unwrap();
    }

    /// The absent-link fixture: `--link` names a path that does not
    /// exist and `--releases-dir` an empty owned dir
    /// (`Current::Absent`) — the install's ordinary update shape when
    /// no upgrade link is present yet.
    fn absent_link_args(&self) -> Vec<String> {
        let releases = self.xdg_data.join("cadence/releases");
        std::fs::create_dir_all(&releases).unwrap();
        vec![
            "--link".to_string(),
            self.home
                .join(".local/bin/cadence")
                .to_string_lossy()
                .into_owned(),
            "--releases-dir".to_string(),
            releases.to_string_lossy().into_owned(),
        ]
    }

    /// `cadence update <args>` through the real routed path as an
    /// asserted operator, with the sentinel-only PATH.
    fn update_argv(&self, extra: &[&str]) -> Vec<String> {
        let mut argv = vec!["cadence".to_string(), "update".to_string()];
        argv.extend(extra.iter().map(|s| s.to_string()));
        argv
    }

    fn run(&self, argv: &[String]) -> Result<i32> {
        let path = format!("{}:/usr/bin:/bin", self.sentinels.display());
        let _path = EnvGuard::new(&[("PATH", Some(path.as_str()))]);
        test_seam::scoped(Asserted::Operator, || run_cli(parse(argv)))
    }
}

/// Parse the argv a shell would pass through the real clap grammar —
/// the same `Cli` shape `run()` builds from `std::env::args_os`.
fn parse(argv: &[String]) -> Cli {
    Cli::try_parse_from(argv)
        .unwrap_or_else(|e| panic!("fixture argv failed clap parse (check the flag names): {e}"))
}

/// Env vars a case binds; every case restores them on drop.
struct EnvGuard {
    saves: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl EnvGuard {
    fn new(vars: &[(&'static str, Option<&str>)]) -> Self {
        let mut saves = Vec::new();
        for (key, value) in vars {
            saves.push((*key, std::env::var_os(key)));
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        Self { saves }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, previous) in self.saves.drain(..) {
            match previous {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// The refusal vocabulary an owned-source failure carries (never a
/// GitHub-route message).
fn owned_source_vocab(msg: &str) -> bool {
    msg.contains("release source")
        || msg.contains("configured")
        || msg.contains("publishes no")
        || msg.contains(OWNED_BACKEND)
}

// ---------------------------------------------------------------------------
// The checks — one test per contract row so a red row names its contract
// ---------------------------------------------------------------------------

/// F. The pinned default itself: the constant is the ticket's owned
/// origin, and `resolve_base(None)` — the production default selection
/// — returns exactly it when no env override is set.
#[test]
fn cad1479_default_source_constant_is_the_pinned_owned_origin() {
    let _serial = SERIAL.lock().unwrap();
    let _env = EnvGuard::new(&[("CADENCE_RELEASE_SOURCE", None)]);
    assert_eq!(
        cadence_agent::releases::DEFAULT_BASE,
        OWNED_BACKEND,
        "the default release source must be the ticket's owned backend"
    );
    assert_eq!(
        cadence_agent::releases::resolve_base(None).expect("resolve_base(None)"),
        OWNED_BACKEND,
        "the no-flag selection must be the pinned owned origin"
    );
}

/// A. Ordinary `update --check` on a linked release: the default source
/// request lands on the owned loopback (the seam-default env carries
/// the same `resolve_base(None)` selection), and the gh sentinel never
/// runs — no GitHub route is touched.
#[test]
fn cad1479_update_check_default_routes_to_owned_source_not_github() {
    let _serial = SERIAL.lock().unwrap();
    let root = Root::new();
    root.arm_state();
    let _env = root.bind_env();
    root.link_release();
    // A catalogue that answers latest with no published entry — a real,
    // honest backend answer. The run must refuse "publishes no release"
    // through the owned source, and the loopback must have served the
    // request.
    let server = serve_requests(
        "200 OK",
        br#"{"ok":true,"data":{"product":"cadence","profile":"operator-host","entry":null}}"#
            .to_vec(),
        4,
    );
    let _src = EnvGuard::new(&[("CADENCE_RELEASE_SOURCE", Some(server.base.as_str()))]);

    let out = root.run(&root.update_argv(&["--check"]));

    let requests = loopback_paths(&server);
    assert!(
        requests
            .iter()
            .any(|r| r.starts_with("GET ") && r.contains(RELEASES_PREFIX)),
        "the default update must consult the owned catalogue — the seam \
         default carried no request; requests: {requests:?}"
    );
    assert!(
        root.external_calls().is_empty(),
        "the default update must never invoke the GitHub CLI — logged: {:?}",
        root.external_calls()
    );
    match out {
        Err(e) => assert!(
            owned_source_vocab(&e.to_string()),
            "the catalogue-empty refusal must name the owned source, got: {}",
            e
        ),
        Ok(_) => panic!(
            "update --check cannot succeed: the catalogue published no \
             operator-host release (requests: {requests:?})"
        ),
    }
}

/// C1. A dead default backend refuses — connection refused surfaces as
/// the configured source's refusal, never a gh call.
#[test]
fn cad1479_update_check_refuses_when_default_backend_down_no_fallback() {
    let _serial = SERIAL.lock().unwrap();
    let root = Root::new();
    root.arm_state();
    let _env = root.bind_env();
    let _src = EnvGuard::new(&[("CADENCE_RELEASE_SOURCE", Some(dead_base().as_str()))]);

    let mut extra = vec!["--check"];
    let mut owned_args = root.absent_link_args();
    let mut argv = vec!["cadence".to_string(), "update".to_string()];
    argv.extend(extra.drain(..).map(str::to_string));
    argv.append(&mut owned_args);
    let out = root.run(&argv);

    let msg = out
        .expect_err("an unreachable default backend must refuse")
        .to_string();
    assert!(
        root.external_calls().is_empty(),
        "backend-down must never fall back to gh, log: {:?} (refusal: {msg})",
        root.external_calls()
    );
    assert!(
        owned_source_vocab(&msg),
        "backend-down refusal must name the source's refusal, got: {msg}"
    );
}

/// C2. A default-source env value that is not a plain https:// origin
/// is refused at the origin guard — before any fetch and before gh.
/// (The seam env is the only in-process handle on the production
/// default selection; the same `parse_origin` guard covers the argv
/// override, separately proven on the real binary in the integration
/// test.)
#[test]
fn cad1479_default_source_rejects_invalid_origins_before_effect() {
    let _serial = SERIAL.lock().unwrap();
    let root = Root::new();
    root.arm_state();
    let _env = root.bind_env();
    let mut argv = vec![
        "cadence".to_string(),
        "update".to_string(),
        "--check".to_string(),
    ];
    argv.extend(root.absent_link_args());
    for bad in [
        "not a url",
        "ftp://127.0.0.1:9",
        "https://example.com/some/path",
        "https://user@evil.example",
    ] {
        let _src = EnvGuard::new(&[("CADENCE_RELEASE_SOURCE", Some(bad))]);
        let out = root.run(&argv);
        let msg = out
            .expect_err("an invalid default-source origin must refuse")
            .to_string();
        assert!(
            msg.contains("release source") || msg.contains("origin") || msg.contains("https"),
            "origin {bad:?} refusal must name the origin rule, got: {msg}"
        );
        assert!(
            root.external_calls().is_empty(),
            "origin {bad:?} must never reach gh, log: {:?}",
            root.external_calls()
        );
    }
}

/// D. The explicit `--release-source <https>` override is still
/// accepted through the same source selection — exercised with an
/// unroutable https origin, whose failure must refuse naming the
/// configured source with the gh sentinel silent.
#[test]
fn cad1479_explicit_https_override_attempted_and_refuses_without_fallback() {
    let _serial = SERIAL.lock().unwrap();
    let root = Root::new();
    root.arm_state();
    let _env = root.bind_env();
    // Closed owned loopback port: TCP refused at once, no external TEST-NET wait.
    let override_origin = dead_base().replacen("http://", "https://", 1);
    let mut argv = vec![
        "cadence".to_string(),
        "update".to_string(),
        "--check".to_string(),
        "--release-source".to_string(),
        override_origin,
    ];
    argv.extend(root.absent_link_args());
    let out = root.run(&argv);
    let msg = out
        .expect_err("an explicit source that cannot serve must refuse")
        .to_string();
    assert!(
        root.external_calls().is_empty(),
        "explicit-source failure must not fall back to gh, log: {:?} (refusal: {msg})",
        root.external_calls()
    );
    assert!(
        owned_source_vocab(&msg),
        "explicit-source refusal must name the configured source, got: {msg}"
    );
}

/// E1. An established image-managed runtime refuses BEFORE any source
/// work — neither the loopback nor gh may be touched.
#[test]
fn cad1479_managed_runtime_refuses_before_any_source_call() {
    let _serial = SERIAL.lock().unwrap();
    let root = Root::new();
    root.arm_state();
    let pm = root.dir.join("pm");
    std::fs::create_dir_all(&pm).unwrap();
    std::fs::write(
        pm.join("pm.yaml"),
        "hosted:\n  lease: file:/run/cadence/lease\n",
    )
    .unwrap();
    let _env = root.bind_env();
    let server = serve_requests("500 Internal Server Error", b"{}".to_vec(), 2);
    let _src = EnvGuard::new(&[("CADENCE_RELEASE_SOURCE", Some(server.base.as_str()))]);
    let out = root.run(&root.update_argv(&["--check"]));
    let msg = out
        .expect_err("an image-managed runtime must refuse the native updater")
        .to_string();
    assert!(
        msg.contains("image-managed") || msg.contains("hosted"),
        "managed-runtime refusal must name the hosted/image-managed contract, got: {msg}"
    );
    assert!(
        loopback_paths(&server).is_empty() && root.external_calls().is_empty(),
        "managed refusal must precede every source call — requests: {:?}, gh: {:?}",
        loopback_paths(&server),
        root.external_calls()
    );
}

/// E2. A pane caller (`CADENCE_ALIAS`) is refused by the operator
/// guard before any source work — the fence is unchanged.
#[test]
fn cad1479_pane_caller_refused_before_any_source_call() {
    let _serial = SERIAL.lock().unwrap();
    let root = Root::new();
    root.arm_state();
    let _env = root.bind_env();
    let _pane = EnvGuard::new(&[("CADENCE_ALIAS", Some("worker-1"))]);
    let server = serve_requests("500 Internal Server Error", b"{}".to_vec(), 2);
    let _src = EnvGuard::new(&[("CADENCE_RELEASE_SOURCE", Some(server.base.as_str()))]);
    let argv = root.update_argv(&["--check"]);
    // No scoped assertion: the pane env alone must carry the refusal.
    let path = format!("{}:/usr/bin:/bin", root.sentinels.display());
    let _path = EnvGuard::new(&[("PATH", Some(path.as_str()))]);
    let out = run_cli(parse(&argv));
    let msg = out
        .expect_err("a pane caller must refuse cadence update")
        .to_string();
    assert!(
        msg.contains("operator") || msg.contains("pane"),
        "pane refusal must name the operator guard, got: {msg}"
    );
    assert!(
        loopback_paths(&server).is_empty() && root.external_calls().is_empty(),
        "operator refusal must precede every source call"
    );
}
