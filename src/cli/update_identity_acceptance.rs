//! CAD-1406 independent acceptance check — written by the check author from
//! the ticket, not by the implementer (AGENTS.md "Gates and security work").
//! The implementer wires this file as a `cfg(all(test, feature = "test-seam"))`
//! sibling of `cli::update` and changes nothing inside it.
//!
//! The contract under test (ticket "Agreed scope" / "Acceptance"), exercised
//! through the real `cli::update::update_caller` + `run_update` guard path —
//! never a fake daemon, client field or env shortcut:
//!
//! 1. A process with real operator proof runs `cadence update` with NO `--as`:
//!    `update_caller` admits it and labels it from the proven local identity
//!    (`source == "native"`, `operator:`-prefixed — never USER/CADENCE_* or a
//!    client field). `run_update --check` then reaches the updater's own first
//!    refusal — `upgrade::current` → `Current::NotSymlink`, which fires before
//!    `check_auth` (no gh, no network, no mutation) — proving the call passed
//!    the identity guard. Explicit `--as operator:<name>` stays accepted
//!    verbatim (`source == "as"`).
//! 2. A test-seam `agent:<alias>` process, an `unproven` process and a pane
//!    env (`CADENCE_ALIAS`) stay refused on BOTH entry shapes — omitted
//!    `--as` and forged `--as operator:ops` — plus forged `USER` /
//!    `CADENCE_ROLLOUT_AS`.
//! 3. An established hosted runtime — the real `[hosted]` pm.yaml lifecycle
//!    contract (`hosted.lease: file:…`, resolved through `CADENCE_PM_DIR`
//!    like `hosted_config`) — is refused before `Layout::detect`, release
//!    selection, drain or any binary/restart effect, and the refusal tells
//!    the operator that AgenticOS/image management owns updates.
//!
//! Isolation: process-env identity guards (`CADENCE_ALIAS`, `CADENCE_PM_DIR`)
//! and the update's tracker lookup are process-global, so the whole check
//! runs in a subprocess of this test binary filtered `--exact` to itself
//! (the repo's `CAD868` self-relaunch convention), with its own
//! `TMPDIR`/`HOME`/`XDG_*`/`CADENCE_PM_DIR` under the lane temp root — the
//! parent mutates nothing in the shared suite's environment.
//!
//! Seam note: `Asserted::Operator` stands in for the `/proc` ancestry proof
//! only — that walk has its own tests in `peer.rs`. The asserted-operator
//! early return in `update_caller` predates this ticket; the label and
//! ordering assertions below are what the ticket's contract requires
//! (proof runs, deterministic label, refusals intact on both `--as` shapes).

use super::*;
use cadence_agent::test_seam::{self, Asserted};

/// Private sentinel: set only on the relaunched child, so the parent wrapper
/// spawns the check and the child runs it — env mutations never touch the
/// shared suite process.
const CHILD_SENTINEL: &str = "CADENCE_CAD1406_ACCEPTANCE_CHILD";

#[test]
fn cad1406_native_update_no_as_proven_operator_and_hosted_runtime_refusal() {
    if std::env::var_os(CHILD_SENTINEL).is_none() {
        // Parent: relaunch this exact test in a child whose HOME/XDG/TMPDIR
        // and tracker dir are all under the lane temp root, so every env
        // the guard or the hosted lookup reads is owned by this check and
        // nothing leaks into (or out of) the concurrently running suite.
        // A unique RAII root — never preemptively removed, deleted only
        // by TempDir's own drop after the child exits.
        let root = tempfile::Builder::new()
            .prefix("cad1406-acceptance-")
            .tempdir()
            .unwrap();
        for dir in ["tmp", "home", "xdg-state", "xdg-data", "pm"] {
            std::fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cli::update_identity_acceptance::cad1406_native_update_no_as_proven_operator_and_hosted_runtime_refusal",
                "--test-threads",
                "1",
                "--nocapture",
            ])
            .env(CHILD_SENTINEL, "1")
            .env("TMPDIR", root.path().join("tmp"))
            .env("HOME", root.path().join("home"))
            .env("XDG_STATE_HOME", root.path().join("xdg-state"))
            .env("XDG_DATA_HOME", root.path().join("xdg-data"))
            // A pm dir with no pm.yaml: the hosted lookup reads "lifecycle
            // off" and can never fall through to the operator's real tracker.
            .env("CADENCE_PM_DIR", root.path().join("pm"))
            .env_remove("CADENCE_ALIAS")
            .env_remove("CADENCE_ROLLOUT_AS")
            .env_remove("CADENCE_STATE_DIR")
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

    // ---- child: the actual check, every path owned under the child's
    //      TMPDIR, every env mutation confined to this process. ----
    let fixture = Fixture::new();
    // Hosted-case paths: nonexistent but owned, inside the fixture root.
    let link = fixture._root.path().join("hosted-case/bin/cadence");
    let releases = fixture._root.path().join("hosted-case/releases");

    // ---- 1. Proven native operator, bare `cadence update` — no --as. ----
    // The caller resolution and the operator proof are the real
    // `update_caller`/`require_operator` path.
    let caller = test_seam::scoped(Asserted::Operator, || {
        update::update_caller(&fixture.state, None)
            .expect("a proven operator must reach the updater without typing --as")
    });
    assert!(
        caller.identity.starts_with("operator:"),
        "the audit label must derive from the proven local identity, got {:?}",
        caller.identity
    );
    assert_eq!(
        caller.source, "native",
        "no --as was given; the label's source must be the native proof, \
         not a client-supplied field"
    );
    // The same proven caller reaches the real `run_update` pipeline. The
    // deterministic proof it passed the identity guard is the updater's
    // own FIRST refusal: `update::check` → `upgrade::current` →
    // `Current::NotSymlink`, which fires before `check_auth` — no gh, no
    // network, no mutation. The link is a real regular file the fixture
    // owns, so the refusal is exact and read-only. `CADENCE_PM_DIR` names
    // the child's empty pm dir, so the hosted probe reads lifecycle-off
    // without ever reaching a real tracker.
    let native_link = fixture._root.path().join("native-case/cadence");
    let native_releases = fixture._root.path().join("native-case/releases");
    std::fs::create_dir_all(native_link.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&native_releases).unwrap();
    std::fs::write(&native_link, "a regular file, never a symlink").unwrap();
    let outcome = test_seam::scoped(Asserted::Operator, || {
        let mut a = update_args();
        a.check = true;
        a.link = Some(native_link.clone());
        a.releases_dir = Some(native_releases.clone());
        update::run_update(&fixture.state, a)
    });
    let err = outcome.expect_err("--check on a regular-file link must refuse");
    let msg = err.to_string();
    assert!(
        msg.contains("is not a symlink"),
        "the proven caller must reach the updater's own layout refusal, got: {msg}"
    );
    assert!(
        !is_guard_refusal(&msg),
        "a proven operator without --as must not hit the identity guard: {msg}"
    );

    // Explicit `--as operator:<name>` stays accepted on the same proof and
    // the label is verbatim — compatibility is identity-preserving.
    let explicit = test_seam::scoped(Asserted::Operator, || {
        update::update_caller(&fixture.state, Some("operator:ops"))
            .expect("explicit --as operator:<name> remains compatible")
    });
    assert_eq!(explicit.identity, "operator:ops");
    assert_eq!(explicit.source, "as");

    // ---- 2. Agent, managed descendant and unproven callers stay refused —
    //        on the new omitted-`--as` shape AND a forged `--as`, with
    //        forged env identity alongside. ----
    for (who, desc) in [
        (
            Asserted::Agent("worker-1".into()),
            "test-seam agent (managed descendant)",
        ),
        (Asserted::Unproven, "unproven process"),
    ] {
        for as_identity in [None, Some("operator:ops")] {
            let refused = test_seam::scoped(who.clone(), || {
                update::update_caller(&fixture.state, as_identity)
            });
            let err = refused.expect_err(&format!("{desc} must stay refused (as={as_identity:?})"));
            assert!(
                is_guard_refusal(&err.to_string()),
                "{desc} refusal (as={as_identity:?}) should name the operator guard, got: {err}"
            );
            // The forged label must never reach the updater's audit record:
            // there is no caller to run `run_update` under.
        }
    }

    // A pane environment (CADENCE_ALIAS on this process) is refused on both
    // `--as` shapes — the env check answers before proof. Mutating env is
    // safe here: the child is this test alone.
    {
        let _env = EnvGuard::new(&[
            ("CADENCE_ALIAS", Some("worker-9")),
            ("USER", Some("root")),
            ("CADENCE_ROLLOUT_AS", Some("operator:ops")),
        ]);
        for as_identity in [None, Some("operator:ops")] {
            let refused = test_seam::scoped(Asserted::Agent("worker-9".into()), || {
                update::update_caller(&fixture.state, as_identity)
            });
            assert!(
                refused.is_err(),
                "a pane/managed descendant stays refused (as={as_identity:?}) with forged env"
            );
        }
    }

    // ---- 3. Established hosted runtime refuses before any update effect.
    //        `hosted.lease: file:…` is the real pm.yaml contract the daemon
    //        leases on (lease.rs::Hosted); a container marker guess is not
    //        it. CADENCE_PM_DIR already names this fixture's tracker. ----
    fixture.hosted("file:/run/cadence/lease.json");
    for (label, mut a) in [
        ("bare update", update_args()),
        ("--check", {
            let mut a = update_args();
            a.check = true;
            a
        }),
    ] {
        a.link = Some(link.clone());
        a.releases_dir = Some(releases.clone());
        let refused =
            test_seam::scoped(Asserted::Operator, || update::run_update(&fixture.state, a));
        let err = refused.expect_err(&format!(
            "established hosted runtime must refuse native {label}"
        ));
        let msg = err.to_string();
        assert!(
            msg.contains("image") || msg.contains("AgenticOS") || msg.contains("hosted"),
            "hosted refusal must carry actionable image-owner guidance, got: {msg}"
        );
        // The refusal happens before any effect: no releases dir, no
        // link and no pending marker may be created.
        assert!(
            !releases.exists() && !link.exists(),
            "hosted refusal created update effects under {label}"
        );
        assert!(
            cadence_agent::update::pending_update(&fixture.state).is_none(),
            "hosted refusal recorded a pending update marker"
        );
    }
}

/// Env vars the child mutates; every case restores them on drop so a panic
/// cannot leak a forged identity into the rest of the (child) run.
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

/// An isolated, seam-armed state dir plus the tracker dir the hosted
/// lookup actually reads (`CADENCE_PM_DIR`, bound by the parent wrapper).
/// The seam token is minted directly (the same file a `daemon run
/// --test-seam` fixture mints) so `process_asserted` honors the scoped
/// `Asserted` identity against this dir.
struct Fixture {
    _root: tempfile::TempDir,
    state: PathBuf,
    pm: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        std::fs::create_dir_all(state.join("seam")).unwrap();
        std::fs::write(state.join("seam/token"), "cad1406-fixture").unwrap();
        // The pm dir the guard's `hosted:` lookup reads — the child's
        // CADENCE_PM_DIR (never the fallback tracker). Created empty, so
        // "native" reads lifecycle-off until `hosted()` writes the lease.
        let pm = PathBuf::from(
            std::env::var_os("CADENCE_PM_DIR")
                .expect("the parent wrapper binds CADENCE_PM_DIR for the child"),
        );
        std::fs::create_dir_all(&pm).unwrap();
        Self {
            _root: root,
            state,
            pm,
        }
    }

    /// `pm.yaml [hosted]` with a lease spec — the established hosted
    /// lifecycle config `daemon serve` leases on. `lease` absent or `off`
    /// is a native install, not a managed runtime.
    fn hosted(&self, lease: &str) {
        std::fs::write(
            self.pm.join("pm.yaml"),
            format!("hosted:\n  lease: \"{lease}\"\n"),
        )
        .unwrap();
    }
}

/// Baseline args; every caller fills `link`/`releases_dir` from owned
/// fixture paths under the test temp root — never an unowned root path.
fn update_args() -> UpdateArgs {
    UpdateArgs {
        status: false,
        check: false,
        rollback: false,
        drain: "0s".into(),
        now: true,
        keep: 3,
        backup_dir: None,
        json: true, // collect progress lines instead of printing
        progress: None,
        to: None,
        as_identity: None,
        repo: "unused/fixture".into(), // never reached: the layout refuses first
        release_source: None,
        link: None,
        releases_dir: None,
    }
}

/// The update guard's own vocabulary: anything naming operator proof,
/// `--as`, panes or managed endpoints comes from `update_caller` /
/// `require_operator`, not from the update pipeline.
fn is_guard_refusal(message: &str) -> bool {
    message.contains("--as")
        || message.contains("operator action")
        || message.contains("operator")
        || message.contains("pane")
        || message.contains("managed")
}
