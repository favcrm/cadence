//! CAD-482: the test-only caller-identity seam, adversarially.
//!
//! These tests pin the seam's contract on both build shapes:
//!
//! - With `--features test-seam` an armed fixture daemon honors the
//!   frame's `test_caller` — the asserted identity is the caller,
//!   whatever the runner's ancestry or environment says (the F14
//!   case: identical in a pane and in CI). Forged tokens, unarmed
//!   daemons, the production state dir and dirs outside the temp root
//!   all refuse, loudly.
//! - Without the feature the same wire shapes are refused rather than
//!   silently ignored — a seam request can never degrade to ambient
//!   identity.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]

mod common;
use common::*;

use cadence_agent::{client, daemon};
use serde_json::json;
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use tempfile::TempDir;

/// `agent_register`'s params for a fixture agent.
#[cfg(feature = "test-seam")]
fn register(alias: &str, cwd: &Path) -> Value {
    json!({"alias": alias, "provider": "fake",
           "endpoint_kind": "fake", "cwd": cwd})
}

/// One raw frame exchange on the daemon socket — the wire shape a
/// hostile caller would craft by hand. Answers the response frame.
fn raw_rpc(state: &Path, frame: Value) -> Value {
    let mut s = UnixStream::connect(client::socket_path(state)).unwrap();
    s.write_all(format!("{frame}\n").as_bytes()).unwrap();
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

/// `Ok(v)` or the error text — for comparing two calls' fates.
#[cfg(feature = "test-seam")]
fn outcome(r: cadence_agent::Result<Value>) -> std::result::Result<Value, String> {
    r.map_err(|e| e.to_string())
}

// ---------- the asserted caller is exactly what the test named ----------

#[cfg(feature = "test-seam")]
#[test]
fn asserted_operator_runs_operator_actions() {
    let d = TestDaemon::start();
    assert!(cadence_agent::test_seam::armed(&d.state));
    // Whatever ancestry this test runs under, the asserted operator
    // gets the operator's answer — `agent_register` is operator-gated
    // (CAD-149/431).
    let cwd = d.dir.path().to_path_buf();
    cadence_agent::test_seam::scoped(cadence_agent::test_seam::Asserted::Operator, || {
        d.rpc("agent_register", register("seam-op", &cwd))
            .unwrap_or_else(|e| panic!("asserted operator refused: {e}"));
    });
    assert_eq!(
        d.rpc("agent_show", json!({"alias": "seam-op"})).unwrap()["agent"]["alias"],
        "seam-op"
    );
}

/// The mutation-proof half: under a seam scope the ambient caller —
/// operator in CI, pane-descended here — is never consulted. Asserted
/// `unproven` refuses an operator-gated call in every environment.
#[cfg(feature = "test-seam")]
#[test]
fn asserted_unproven_is_refused_even_where_ambient_is_operator() {
    let d = TestDaemon::start();
    let cwd = d.dir.path().to_path_buf();
    let err =
        cadence_agent::test_seam::scoped(cadence_agent::test_seam::Asserted::Unproven, || {
            outcome(d.rpc("agent_register", register("seam-no", &cwd)))
        })
        .expect_err("asserted unproven must be refused, whatever the runner's ambient identity");
    assert!(
        err.contains("unproven") || err.contains("operator"),
        "refusal should name the caller rule, got: {err}"
    );
    assert!(d.rpc("agent_show", json!({"alias": "seam-no"})).is_err());
}

#[cfg(feature = "test-seam")]
#[test]
fn asserted_agent_is_that_agent_and_only_that_agent() {
    let d = TestDaemon::start();
    d.register("w1");
    d.register("w2");
    // An asserted agent is verified against the registry: `w1` may
    // stop itself (agent_stop is OnAgent SelfService) …
    cadence_agent::test_seam::scoped(
        cadence_agent::test_seam::Asserted::Agent("w1".into()),
        || {
            d.rpc("agent_stop", json!({"alias": "w1"}))
                .unwrap_or_else(|e| panic!("agent stopping itself refused: {e}"));
        },
    );
    // … but an asserted `w2` cannot touch `w1`'s row …
    d.register("w1b");
    let err = cadence_agent::test_seam::scoped(
        cadence_agent::test_seam::Asserted::Agent("w2".into()),
        || outcome(d.rpc("agent_stop", json!({"alias": "w1b"}))),
    )
    .expect_err("one agent may not mutate another");
    assert!(err.contains("cannot change another agent"), "{err}");
    // … an unregistered assertion refuses rather than inventing an
    // identity …
    let err = cadence_agent::test_seam::scoped(
        cadence_agent::test_seam::Asserted::Agent("ghost".into()),
        || outcome(d.rpc("agent_stop", json!({"alias": "w2"}))),
    )
    .expect_err("an unregistered asserted agent must refuse");
    assert!(!err.is_empty());
    // … and no asserted agent runs the operator's register action.
    let cwd = d.dir.path().to_path_buf();
    let err = cadence_agent::test_seam::scoped(
        cadence_agent::test_seam::Asserted::Agent("w2".into()),
        || outcome(d.rpc("agent_register", register("seam-y", &cwd))),
    )
    .expect_err("an agent caller cannot register agents (CAD-149)");
    assert!(err.contains("operator") || err.contains("agent"), "{err}");
}

// ---------- adversarial wire shapes ----------

#[cfg(feature = "test-seam")]
#[test]
fn forged_token_and_half_assertions_are_refused() {
    let d = TestDaemon::start();
    // A wrong token is refused even on a Read method — the assertion
    // itself is the problem, not the verb.
    let frame = raw_rpc(
        &d.state,
        json!({"method": "health", "params": {},
               "test_caller": {"token": "not-the-fixtures-token", "as": "operator"}}),
    );
    let err = cadence_agent::proto::unwrap(frame).expect_err("a forged token must refuse");
    assert!(err.to_string().contains("test seam"), "{err}");
    // A half assertion — token with no `as` — refuses too.
    let token = cadence_agent::test_seam::Seam::token_at(&d.state).unwrap();
    let frame = raw_rpc(
        &d.state,
        json!({"method": "health", "params": {},
               "test_caller": {"token": token}}),
    );
    cadence_agent::proto::unwrap(frame).expect_err("a token without 'as' must refuse");
    // An unknown `as` value refuses.
    let frame = raw_rpc(
        &d.state,
        json!({"method": "health", "params": {},
               "test_caller": {"token": token, "as": "superuser"}}),
    );
    let err = cadence_agent::proto::unwrap(frame).expect_err("an unknown identity must refuse");
    assert!(err.to_string().contains("agent:<alias>"), "{err}");
}

#[cfg(feature = "test-seam")]
#[test]
fn unarmed_daemon_refuses_the_field() {
    // A daemon that did not ask for the seam refuses `test_caller`
    // outright — the asserted path can never smuggle into a fixture
    // that did not opt in.
    let d = TestDaemon::start_opts(daemon::ServeOptions {
        test_seam: false,
        ..daemon_opts()
    });
    let frame = raw_rpc(
        &d.state,
        json!({"method": "health", "params": {},
               "test_caller": {"token": "x", "as": "operator"}}),
    );
    let err =
        cadence_agent::proto::unwrap(frame).expect_err("unarmed daemon must refuse test_caller");
    assert!(err.to_string().contains("armed"), "{err}");
}

/// Frames on ONE connection: scope is per-dispatch. An asserted frame
/// must not bleed into the next unasserted frame on the same socket.
#[cfg(feature = "test-seam")]
#[test]
fn assertion_scope_is_per_frame_on_one_connection() {
    let d = TestDaemon::start();
    let token = cadence_agent::test_seam::Seam::token_at(&d.state).unwrap();
    let cwd = d.dir.path().to_path_buf();
    let mut s = UnixStream::connect(client::socket_path(&d.state)).unwrap();
    let send = |s: &mut UnixStream, frame: Value| {
        s.write_all(format!("{frame}\n").as_bytes()).unwrap();
    };
    send(
        &mut s,
        json!({"method": "agent_register", "params": register("seq-op", &cwd),
               "test_caller": {"token": token, "as": "operator"}}),
    );
    send(
        &mut s,
        json!({"method": "agent_register", "params": register("seq-no", &cwd),
               "test_caller": {"token": token, "as": "unproven"}}),
    );
    // Third frame asserts nothing: it must land as the ambient caller,
    // not as the previous frame's operator.
    send(
        &mut s,
        json!({"method": "agent_register", "params": register("seq-env", &cwd)}),
    );
    let mut lines = BufReader::new(s).lines().map(|l| {
        cadence_agent::proto::unwrap(serde_json::from_str(&l.unwrap()).unwrap())
            .map_err(|e| e.to_string())
    });
    assert!(
        lines.next().unwrap().is_ok(),
        "asserted operator frame refused"
    );
    assert!(
        lines.next().unwrap().is_err(),
        "asserted unproven frame passed"
    );
    // The unasserted frame's fate equals a plainly-ambient call's — in
    // a pane both are unproven-refused, in CI both are operator-ok;
    // either way the seam added nothing to it.
    let ambient = outcome(d.rpc("agent_register", register("seq-ambient", &cwd)));
    assert_eq!(
        lines.next().unwrap().is_ok(),
        ambient.is_ok(),
        "an unasserted frame must resolve as the ambient caller"
    );
}

/// Parallel connections asserting different identities resolve their
/// own, not each other's — the scope is the dispatch's, not global.
/// Three callers on `agent_register` (operator-gated, CAD-149):
/// the operator passes, an asserted agent gets the CAD-149 refusal,
/// asserted `unproven` gets the unattributed refusal — racing, on
/// separate connections, so a thread-local leak would scramble the
/// three answers.
#[cfg(feature = "test-seam")]
#[test]
fn concurrent_assertions_do_not_leak() {
    let d = TestDaemon::start();
    d.register("w-racer");
    let token = cadence_agent::test_seam::Seam::token_at(&d.state).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let state = d.state.clone();
    let run = move |who: &'static str, ok: Option<&'static str>| {
        let (state, token, barrier) = (state.clone(), token.clone(), barrier.clone());
        let cwd = d.dir.path().to_path_buf();
        std::thread::spawn(move || {
            for i in 0..8 {
                barrier.wait();
                let frame = raw_rpc(
                    &state,
                    json!({"method": "agent_register",
                           "params": register(&format!("race-{who}-{i}"), &cwd),
                           "test_caller": {"token": token, "as": who}}),
                );
                match ok {
                    None => assert!(
                        cadence_agent::proto::unwrap(frame).is_ok(),
                        "asserted {who} refused"
                    ),
                    Some(want) => {
                        let err = cadence_agent::proto::unwrap(frame)
                            .expect_err("refused caller must stay refused");
                        assert!(
                            err.to_string().contains(want),
                            "asserted {who} got the wrong caller's refusal: {err} (want '{want}')"
                        );
                    }
                }
            }
        })
    };
    let op = run("operator", None);
    let agent = run("agent:w-racer", Some("may register only its own"));
    let unproven = run("unproven", Some("not provably the operator"));
    op.join().unwrap();
    agent.join().unwrap();
    unproven.join().unwrap();
}

// ---------- confinement ----------

#[cfg(feature = "test-seam")]
#[test]
fn arming_refuses_the_production_state_dir() {
    // Asserting `arm_if_requested` directly (not through serve_with)
    // keeps the check independent of any earlier daemon gate. The dir
    // is the resolved production default — on this host it may exist;
    // the seam must refuse it and, refused or not, it is left exactly
    // as found.
    let default = client::default_state_dir().unwrap();
    let err = cadence_agent::test_seam::arm_if_requested(&default, true)
        .map(|_| ())
        .expect_err("the seam must refuse the production default state dir");
    // Which refusal fires first is env-dependent: when the env-derived
    // default IS the real production dir the env-independent N1 bound
    // answers before the default-dir check.
    let msg = err.to_string();
    assert!(
        msg.contains("default state dir") || msg.contains("production state dir"),
        "{msg}"
    );
}

#[cfg(feature = "test-seam")]
#[test]
fn arming_refuses_state_outside_the_temp_root() {
    // A dir under the repository worktree is real state, not a fixture
    // — the seam must refuse it and say why.
    let outside = std::env::current_dir()
        .unwrap()
        .join(format!("seam-outside-{}", std::process::id()));
    let err = daemon::serve_with(
        &outside,
        daemon::ServeOptions {
            test_seam: true,
            ..daemon_opts()
        },
    )
    .expect_err("the seam must refuse a state dir outside the temp root");
    assert!(err.to_string().contains("temp root"), "{err}");
    let _ = std::fs::remove_dir_all(&outside);
}

/// N2 (rev-312): a misspelled `CADENCE_TEST_AS` on a spawned caller
/// must refuse the way a forged wire frame does — never fall through
/// to ambient, where the same process in CI would run as the operator.
#[cfg(feature = "test-seam")]
#[test]
fn unparseable_as_env_refuses_the_call_loudly() {
    let d = TestDaemon::start();
    let (ok, _out, err) = op::cli_as(
        env!("CARGO_BIN_EXE_cadence"),
        &d.state,
        &["agent", "list"],
        &[],
        "operatr", // a typo of 'operator'
    );
    assert!(!ok, "an unparseable CADENCE_TEST_AS must fail the call");
    assert!(
        err.contains("not an identity") || err.contains("CADENCE_TEST_AS"),
        "the refusal should name the bad assertion, got: {err}"
    );
}

/// N1 (rev-312): the production refusal is bound to the real uid's
/// passwd home, not to HOME/XDG/TMPDIR — a doctored environment cannot
/// rename the dir the seam must never arm. The refusal runs before
/// `create_dir_all`, so nothing is written there.
#[cfg(feature = "test-seam")]
#[test]
fn arming_refuses_the_real_production_state_dir() {
    let Some(home) = real_passwd_home() else {
        // No passwd entry for this uid — the bound has nothing to
        // resolve against; the env-derived checks still stand.
        return;
    };
    // Point every env-derived handle at a decoy — in a private child
    // process, so the doc never mutates the shared process env: only
    // the passwd-home bound can still name `prod`. Without it the arm
    // falls through to the temp-root refusal — an Err, but a
    // different message.
    let decoy_dir = TempDir::new().unwrap();
    let decoy = decoy_dir.path().to_str().unwrap().to_string();
    if !in_own_process(
        "arming_refuses_the_real_production_state_dir",
        &[("HOME", &decoy), ("XDG_STATE_HOME", &decoy)],
    ) {
        return;
    }
    let prod = home.join(".local/state/cadence");
    let err = cadence_agent::test_seam::arm_if_requested(&prod, true)
        .map(|_| ())
        .expect_err("the seam must refuse the real production state dir");
    assert!(
        err.to_string().contains("production state dir"),
        "the refusal should name the production dir, got: {err}"
    );
}

/// The uid's home directory from its passwd entry — never $HOME, which
/// a caller controls.
#[cfg(all(feature = "test-seam", unix))]
fn real_passwd_home() -> Option<std::path::PathBuf> {
    // SAFETY: getuid/getpwuid need no setup; pw_dir is borrowed, never
    // freed.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        let dir = std::ffi::CStr::from_ptr((*pw).pw_dir).to_string_lossy();
        (!dir.is_empty()).then(|| std::path::PathBuf::from(dir.into_owned()))
    }
}

/// A spawn-time env request on a build without the feature must never
/// silently degrade to ambient identity — the daemon refuses to serve.
#[cfg(not(feature = "test-seam"))]
#[test]
fn without_the_feature_arm_is_refused() {
    let dir = TempDir::new().unwrap();
    let err = daemon::serve_with(
        dir.path(),
        daemon::ServeOptions {
            test_seam: true,
            ..daemon_opts()
        },
    )
    .expect_err("requesting the seam without the feature must refuse");
    assert!(err.to_string().contains("test-seam"), "{err}");
}

/// And a `test_caller` frame is refused on the wire, not ignored.
#[cfg(not(feature = "test-seam"))]
#[test]
fn without_the_feature_the_field_is_refused() {
    let d = TestDaemon::start();
    let frame = raw_rpc(
        &d.state,
        json!({"method": "health", "params": {},
               "test_caller": {"token": "x", "as": "operator"}}),
    );
    let err = cadence_agent::proto::unwrap(frame).expect_err("test_caller must refuse");
    assert!(err.to_string().contains("test-seam"), "{err}");
}
