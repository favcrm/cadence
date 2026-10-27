//! CAD-1355 independent acceptance check — written by the check author from
//! the ticket, not by the implementer (AGENTS.md "Gates and security work").
//! The implementer wired this file as a `cfg(all(test, feature = "test-seam"))`
//! sibling of `cli::update` and changed nothing inside it.
//!
//! The contract under test (ticket "Reproduction"/"Acceptance"), exercised
//! through the actual routed production path: real clap argv →
//! `Cli::try_parse_from` → `action_dispatch::run_cli` — the same function
//! `run()` delegates to after parse — never a duplicated `resolve`+`dispatch`
//! arm, fake resolver, client or env shortcut:
//!
//! 1. A registry whose *saved default* (`selected`) is a remote org must not
//!    retarget `cadence update`: a bare `update --check` argv runs the whole
//!    dispatch and reaches the updater's own first refusal —
//!    `upgrade::current` → `Current::NotSymlink` on an owned regular-file
//!    `--link`, which fires before `Gh::check_auth` (no gh, no network, no
//!    credential read, no wake, no remote RPC). The saved default is not an
//!    explicit selector: it neither blocks nor moves the local-only route.
//!    The native path is derived from that deterministic read-only refusal,
//!    not from any resolver-result inspection.
//! 2. The registry file and the remote org's credential directory are
//!    byte-for-byte unchanged and no `local` row materialized — the whole
//!    routed run never writes `orgs.json`, never creates the canonical-local
//!    arm, never renews a credential and never wakes a container.
//! 3. Explicit remote selectors — `--org <remote>` and `CADENCE_ORG=<remote>`
//!    — are deliberate selections and the routed argv refuses with clear
//!    local-only guidance before `remote::run`/transport/credentials/wake,
//!    never as a remote credential/transport failure and never as a remote
//!    update attempt.
//! 4. The operator proof still binds the native route: seam `agent:` and
//!    `unproven` callers are refused through the real `update_caller`/
//!    `require_operator` guard inside the routed run, even though the
//!    destination resolved local — routing is not authorization. A bad
//!    caller on a *bare* `update` (no read-only flags) still stops at the
//!    guard before any lease, drain, install or restart effect.
//!
//! Isolation and the deliberate non-pin: process-global env decides org
//! resolution, so the whole check runs in a subprocess of this test binary
//! filtered `--exact` to itself (the repo's CAD-1406 self-relaunch
//! convention), under a unique RAII `TempDir`. The child gets owned
//! `HOME`/`XDG_CONFIG_HOME`/`XDG_STATE_HOME`/`TMPDIR` and a hand-written
//! `orgs.json` with correct schema — `CADENCE_PM_DIR`, `CADENCE_STATE_DIR`,
//! `CADENCE_HOME`, `CADENCE_PROFILE` and `CADENCE_ORG` are removed, because
//! pinning any of them in the saved-default case would take the pinned arm
//! *before* registry selection and stop exercising the bug (the parent must
//! not bind them either). The ambient state root is minted
//! `<state>/seam/token` — the same file an armed fixture daemon writes — so
//! `process_asserted` honors the seam's scoped assertion against this dir,
//! never a forged `--as` or authorization field. The child's CWD is an owned
//! dir so `run_cli`'s `permission_replay` master-classify can never read a
//! repo checkout's rules.

use super::action_dispatch::run_cli;
use super::*;
use cadence_agent::test_seam::{self, Asserted};

/// Private sentinel: set only on the relaunched child, so the parent wrapper
/// spawns the check and the child runs it — env mutations never touch the
/// shared suite process.
const CHILD_SENTINEL: &str = "CADENCE_CAD1355_ACCEPTANCE_CHILD";

/// The saved default remote org's name, endpoint and workspace — matching
/// the `orgs.json` schema `org.rs` reads (`mode: "remote"`, `https://`
/// endpoint, workspace-style `org_id`).
const REMOTE_ORG: &str = "demo-company";
const REMOTE_ENDPOINT: &str = "https://demo.cadencecloud.app";
const REMOTE_ORG_ID: &str = "ws_demo_company";

#[test]
fn cad1355_saved_default_remote_routes_native_update_locally() {
    if std::env::var_os(CHILD_SENTINEL).is_none() {
        // Parent: relaunch this exact test in a child whose HOME/XDG/TMPDIR
        // are all under a unique lane temp root. The org registry the
        // routing reads is `$XDG_CONFIG_HOME/cadence/orgs.json` (else
        // `~/.config/...`), and the ambient state dir `client::state_dir`
        // resolves is `$XDG_STATE_HOME/cadence` — both owned so the
        // registry is a fixture and the seam can arm on the state dir. A
        // unique RAII root — never preemptively removed, deleted only by
        // TempDir's own drop after the child exits.
        let root = tempfile::Builder::new()
            .prefix("cad1355-acceptance-")
            .tempdir()
            .unwrap();
        for dir in ["tmp", "home", "xdg-config", "xdg-state", "xdg-data", "cwd"] {
            std::fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cli::update_routing_acceptance::cad1355_saved_default_remote_routes_native_update_locally",
                "--test-threads",
                "1",
                "--nocapture",
            ])
            .env(CHILD_SENTINEL, "1")
            .env("TMPDIR", root.path().join("tmp"))
            .env("HOME", root.path().join("home"))
            .env("XDG_CONFIG_HOME", root.path().join("xdg-config"))
            .env("XDG_STATE_HOME", root.path().join("xdg-state"))
            .env("XDG_DATA_HOME", root.path().join("xdg-data"))
            // `run_cli`'s permission-replay path reads the process CWD for
            // master-command classification — point it at an owned empty
            // dir, never the repo checkout or an unowned root.
            .current_dir(root.path().join("cwd"))
            // `XDG_CONFIG_HOME` also owns the remote credential dir
            // `call_verb`/`auth_dir(None)` resolves
            // ($XDG_CONFIG_HOME/cadence/remote-auth), so any credential
            // read/renew lands on owned bytes the child diffs, never the
            // operator's real credential dir.
            // Deliberately NOT set: CADENCE_PM_DIR / CADENCE_STATE_DIR /
            // CADENCE_HOME / CADENCE_PROFILE — a pin would take
            // resolve_update's pinned arm before registry selection and
            // skip the bug. CADENCE_ORG is also unset: the saved default
            // must carry the routing, not an explicit selector.
            .env_remove("CADENCE_ALIAS")
            .env_remove("CADENCE_ORG")
            .env_remove("CADENCE_PM_DIR")
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("CADENCE_HOME")
            .env_remove("CADENCE_PROFILE")
            .env_remove("CADENCE_ROLLOUT_AS")
            .env_remove("CADENCE_TEST_AS")
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "child acceptance run failed:\n{text}");
        assert!(text.contains("1 passed"), "{text}");
        return;
    }

    // ---- child: the actual check. Every path owned under this child's
    //      TMPDIR/HOME/XDG; no pin env was bound, so org resolution must
    //      come from the registry file the fixture writes. ----
    let fixture = Fixture::new();

    // The saved-default-remote registry, written through the real
    // `orgs.json` path and schema `org.rs` reads: `selected` names the
    // remote org, one `remote` destination row, no `local` row (that arm
    // only materializes on `switch local`, which must never run here).
    fixture.write_registry(&json!({
        "selected": {"org": REMOTE_ORG},
        "connections": [{
            "selection": {"org": REMOTE_ORG},
            "destination": {
                "mode": "remote",
                "endpoint": REMOTE_ENDPOINT,
                "org_id": REMOTE_ORG_ID,
            },
        }],
    }));
    let registry_before = fixture.registry_bytes();
    let auth_before = fixture.auth_bytes();

    // The seam-armed ambient state root `client::state_dir()` resolves
    // ($XDG_STATE_HOME/cadence, owned): mint `<state>/seam/token`, the same
    // file an armed fixture daemon writes, so `process_asserted` honors the
    // scoped assertion on the resolved dir.
    fixture.arm_ambient_state();

    // A regular file — never a symlink — as the native link. `update --check`
    // on it must reach `upgrade::current` → `Current::NotSymlink`, the
    // updater's own first refusal, before `Gh::check_auth` (no gh/network)
    // and before any install/restart/mutation.
    let native_link = fixture.root.path().join("native-case/cadence");
    let native_releases = fixture.root.path().join("native-case/releases");
    std::fs::create_dir_all(native_link.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&native_releases).unwrap();
    std::fs::write(&native_link, "a regular file, never a symlink").unwrap();

    // The seam only arms the ambient root; if the routed run ever picks a
    // different state dir (e.g. a canonical local row, which this fixture
    // deliberately lacks) the guard's `process_asserted` would refuse —
    // which this check would surface as a guard refusal, keeping the check
    // honest rather than faking the arm everywhere.

    // ---- 1 + 2. Saved default remote: `update --check` argv routes
    //        native, reaches the layout refusal, changes nothing. ----
    //
    // The actual routed path: the same argv a shell would pass, parsed by
    // the real clap grammar, run through `run_cli` — the function `run()`
    // delegates to — so a `Commands::Update` mis-wired to `org::resolve`
    // would resolve the remote default and fail these assertions.
    let check_argv = vec![
        "cadence".to_string(),
        "update".to_string(),
        "--check".to_string(),
        "--link".to_string(),
        native_link.to_string_lossy().into_owned(),
        "--releases-dir".to_string(),
        native_releases.to_string_lossy().into_owned(),
    ];
    let outcome = test_seam::scoped(Asserted::Operator, || run_cli(parse(&check_argv)));
    let err = outcome.expect_err("--check on a regular-file link must refuse");
    let msg = err.to_string();
    assert!(
        msg.contains("is not a symlink"),
        "the routed argv must reach the native updater's layout refusal \
         (Current::NotSymlink) — this is the proof it resolved local and \
         entered `update::run_update`, got: {msg}"
    );
    assert!(
        !is_guard_refusal(&msg),
        "a proven operator must not hit the identity guard on the local \
         route: {msg}"
    );
    assert!(
        !is_remote_refusal(&msg),
        "a saved remote default must not surface remote routing text: {msg}"
    );
    assert!(
        !is_credential_refusal(&msg),
        "a saved remote default must not reach remote credentials/wake: {msg}"
    );

    // The registry and the credential dir are byte-for-byte unchanged:
    // no org switch, no materialized `local` row, no credential read or
    // renewal, no wake — the whole routed run is read-only on the org
    // layer.
    assert_eq!(
        fixture.registry_bytes(),
        registry_before,
        "update routing must not write or re-shape orgs.json"
    );
    assert_eq!(
        fixture.auth_bytes(),
        auth_before,
        "update routing must not read, renew or write remote credentials"
    );

    // A bare `update` (no `--check`) on the same link also reaches the
    // native layout refusal — routing, not the flag set, is what the
    // ticket pins. `--check` stays the deterministic stop because the
    // layout guard fires before the rollout-lease/drain stage either way.
    let bare_argv = vec![
        "cadence".to_string(),
        "update".to_string(),
        "--link".to_string(),
        native_link.to_string_lossy().into_owned(),
        "--releases-dir".to_string(),
        native_releases.to_string_lossy().into_owned(),
    ];
    let bare = test_seam::scoped(Asserted::Operator, || run_cli(parse(&bare_argv)));
    let msg = bare
        .expect_err("bare update on a regular-file link refuses")
        .to_string();
    assert!(
        msg.contains("is not a symlink"),
        "bare update must reach the same native layout refusal, got: {msg}"
    );
    assert!(
        !is_remote_refusal(&msg),
        "bare update hit remote routing: {msg}"
    );

    // ---- 3. Explicit remote selectors refuse with local-only guidance
    //        inside the routed resolution, before remote::run — so before
    //        the verb allowlist, credential dir, wake loop or transport. ----
    for (selector, extra_args, env_org) in [
        ("--org flag", vec!["--org", REMOTE_ORG], None),
        ("CADENCE_ORG env", vec![], Some(REMOTE_ORG)),
    ] {
        let _env = EnvGuard::new(&[("CADENCE_ORG", env_org)]);
        let mut argv = vec![
            "cadence".to_string(),
            "update".to_string(),
            "--check".to_string(),
            "--link".to_string(),
            native_link.to_string_lossy().into_owned(),
            "--releases-dir".to_string(),
            native_releases.to_string_lossy().into_owned(),
        ];
        argv.extend(extra_args.into_iter().map(str::to_string));
        let err = match test_seam::scoped(Asserted::Operator, || run_cli(parse(&argv))) {
            Err(err) => err,
            Ok(code) => panic!(
                "{selector} update returned {code} — an explicit remote \
                 selection must refuse, never run local or remote update"
            ),
        };
        let msg = err.to_string();
        assert!(
            is_local_only_refusal(&msg),
            "{selector} refusal must give local-only update guidance, got: {msg}"
        );
        assert!(
            !is_remote_refusal(&msg),
            "{selector} refusal reached the remote-dispatch vocabulary, got: {msg}"
        );
        assert!(
            !is_credential_refusal(&msg),
            "{selector} refusal looks like a credential/transport failure, got: {msg}"
        );
        // The refusal must not name the remote verb-allowlist refusal —
        // that would prove the command reached `remote::run`, i.e. a
        // remote verb was attempted.
        assert!(
            !msg.contains("not available on a remote org"),
            "{selector} refusal came from remote::run's allowlist, meaning \
             update reached remote dispatch: {msg}"
        );
    }

    // Registry and credentials still untouched after the selector probes.
    assert_eq!(fixture.registry_bytes(), registry_before);
    assert_eq!(fixture.auth_bytes(), auth_before);

    // ---- 4. Operator proof still binds the native route, on both flag
    //        shapes. ----
    //
    // Routing local is not authorization: a bad caller on the routed path
    // must stop at `update_caller`/`require_operator` before the layout
    // check (so before lease/drain/install/restart on the bare shape).
    // `--check` proves the guard on the read-only face; bare `update`
    // proves the same guard precedes every update effect.
    for (who, desc) in [
        (Asserted::Agent("worker-1".into()), "test-seam agent"),
        (Asserted::Unproven, "unproven process"),
    ] {
        for (shape, check) in [("--check", true), ("bare", false)] {
            let mut argv = vec!["cadence".to_string(), "update".to_string()];
            if check {
                argv.push("--check".to_string());
            }
            argv.extend([
                "--link".to_string(),
                native_link.to_string_lossy().into_owned(),
                "--releases-dir".to_string(),
                native_releases.to_string_lossy().into_owned(),
            ]);
            let outcome = test_seam::scoped(who.clone(), || run_cli(parse(&argv)));
            let err = outcome.expect_err(&format!(
                "{desc} must stay refused on the local route ({shape})"
            ));
            let msg = err.to_string();
            assert!(
                is_guard_refusal(&msg),
                "{desc} refusal on the local route ({shape}) must name the \
                 operator guard, got: {msg}"
            );
            assert!(
                !msg.contains("is not a symlink"),
                "{desc} ({shape}) reached the updater past the operator \
                 guard: {msg}"
            );
        }
    }
    // Final: the org layer was read-only end to end — no switch, no `local`
    // row, no credential or wake side effect.
    assert_eq!(fixture.registry_bytes(), registry_before);
    assert_eq!(fixture.auth_bytes(), auth_before);
}

/// Parse the argv a shell would pass through the real clap grammar — the
/// same `Cli` shape `run()` builds from `std::env::args_os` — so the check
/// enters the routed path exactly as a production invocation does.
fn parse(argv: &[String]) -> Cli {
    Cli::try_parse_from(argv)
        .unwrap_or_else(|e| panic!("fixture argv failed clap parse (check the flag names): {e}"))
}

/// The update guard's own vocabulary: anything naming operator proof,
/// `--as`, panes or managed endpoints comes from `update_caller` /
/// `require_operator`, not the routing layer.
fn is_guard_refusal(message: &str) -> bool {
    message.contains("--as")
        || message.contains("operator action")
        || message.contains("operator")
        || message.contains("pane")
        || message.contains("managed")
}

/// The remote dispatch's own vocabulary — the allowlist refusal and the
/// remote-target banner, proof the command reached `remote::run` or a
/// remote transport was attempted.
fn is_remote_refusal(message: &str) -> bool {
    message.contains("not available on a remote org")
        || message.contains("remote verbs")
        || message.contains("Use `--org local`")
        || message.contains("cadence org switch")
}

/// A refusal that gives the ticket's local-only update guidance — names
/// the local install/update as the only route, not a credential or
/// transport failure.
fn is_local_only_refusal(message: &str) -> bool {
    (message.contains("local") || message.contains("update"))
        && (message.contains("only")
            || message.contains("refus")
            || message.contains("cannot")
            || message.contains("not available")
            || message.contains("must")
            || message.contains("local install"))
}

/// Remote credential/transport vocabulary — proof the call got as far as
/// reading or renewing a credential or waking the container, which an
/// explicit-remote update refusal must never reach.
fn is_credential_refusal(message: &str) -> bool {
    message.contains("credential")
        || message.contains("credential directory")
        || message.contains("waking")
        || message.contains("renew")
        || message.contains("transport")
}

/// Env vars the child mutates; every case restores them on drop so a panic
/// cannot leak a forged selector into the rest of the (child) run.
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

/// The owned filesystem the child binds: `HOME` + `XDG_*` from the parent
/// wrapper, a registry file under `XDG_CONFIG_HOME/cadence/orgs.json`, the
/// `auth_dir` credential path under the same root, and the ambient state
/// root `client::state_dir()` resolves so the seam can arm on it.
struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        Self { root }
    }

    /// `orgs.json` — the real path `org::RegistryFile::path` resolves
    /// under `XDG_CONFIG_HOME` (the `~/.config` fallback is owned too).
    fn registry_path(&self) -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .expect("the parent wrapper binds XDG_CONFIG_HOME for the child");
        base.join("cadence").join("orgs.json")
    }

    fn write_registry(&self, registry: &Value) {
        let path = self.registry_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec_pretty(registry).unwrap()).unwrap();
    }

    fn registry_bytes(&self) -> Vec<u8> {
        std::fs::read(self.registry_path()).unwrap_or_default()
    }

    /// The remote credential dir `auth_dir(None)` resolves:
    /// `$XDG_CONFIG_HOME/cadence/remote-auth`. Read recursively so a
    /// renewal or new credential file anywhere inside it is caught.
    fn auth_bytes(&self) -> Vec<u8> {
        let dir = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .expect("the parent wrapper binds XDG_CONFIG_HOME for the child")
            .join("cadence")
            .join("remote-auth");
        dir_bytes(&dir)
    }

    /// The ambient state root `client::state_dir()` resolves with no
    /// `CADENCE_STATE_DIR`/`CADENCE_HOME` pin: `$XDG_STATE_HOME/cadence`.
    /// Mint `seam/token` so `process_asserted` honors the scoped assertion
    /// on this dir — the same file an armed fixture daemon writes.
    fn arm_ambient_state(&self) -> PathBuf {
        let state = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .expect("the parent wrapper binds XDG_STATE_HOME for the child")
            .join("cadence");
        std::fs::create_dir_all(state.join("seam")).unwrap();
        std::fs::write(state.join("seam/token"), "cad1355-fixture\n").unwrap();
        state
    }
}

/// A directory's full contents as one byte stream (sorted so the snapshot
/// is deterministic) — the diff that proves no credential file appeared.
fn dir_bytes(dir: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            out.extend_from_slice(&dir_bytes(&path));
        } else {
            out.extend_from_slice(path.to_string_lossy().as_bytes());
            out.extend_from_slice(&std::fs::read(&path).unwrap_or_default());
        }
    }
    out
}
