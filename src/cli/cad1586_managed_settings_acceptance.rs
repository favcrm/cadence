//! CAD-1586 INDEPENDENT client-side managed-caller guard acceptance —
//! bounded CAD-1592 follow-up. Authored against the ticket contract and
//! the shipped guard, never an implementer's summary; the implementer
//! may not edit or weaken this file.
//!
//! REAL path under test: clap argv -> `Cli::try_parse_from` -> the same
//! `Commands` value `action_dispatch` hands `cli::remote::run` ->
//! `remote::run` itself (the private entry the production dispatch at
//! `action_dispatch.rs` calls once a Remote target resolves). No mock
//! RPC, no echo fixture: nothing below resolves credentials, orgs or the
//! network — the guard must refuse BEFORE any of that.
//!
//! Contract under test: a remote Settings mutation (and `master
//! status`/`master resume`) refuses for a managed caller — a
//! `RemoteTarget` carrying an assignment-bound `CliIdentityPin`
//! (subject/issuer copied from the live kickoff binding), or a caller
//! marked by `CADENCE_ALIAS`/`CADENCE_GRANT_TOKEN` — with the typed code
//! `settings_agent_session_required`. The stored login is owner-class
//! settings authority a hosted agent principal can never hold; spending
//! it would impersonate the member. Refusal must be typed: a generic
//! missing-credentials failure does NOT satisfy this contract.
//!
//! Env-marked callers ride the identical `||` arm as the pin. This file
//! never mutates process env (concurrent tests share it) and asserts
//! nothing on the unpinned path: under a managed suite env
//! (CADENCE_ALIAS/CADENCE_GRANT_TOKEN) an unpinned refusal is correct,
//! so only the pinned arm carries the deterministic contract.

use super::*;

/// A valid-looking remote destination the way `remote_cli` produces it:
/// a real issuer/org triple plus (when `managed`) the assignment pin an
/// `org`-allowed managed caller inherits from its live binding.
fn target(managed: bool) -> cadence_agent::remote_cli::RemoteTarget {
    cadence_agent::remote_cli::RemoteTarget {
        org: "acme".into(),
        endpoint: "https://agenticos.example/api".into(),
        org_id: "org_0123456789abcdef".into(),
        identity: managed.then(|| cadence_agent::remote_cli::CliIdentityPin {
            subject_id: "sub_managed_worker".into(),
            issuer: "https://agenticos.example".into(),
        }),
    }
}

/// Parse argv exactly as the binary entry does and return the `Commands`
/// the dispatcher passes on — the refusal must see the same value.
fn parsed(argv: &[&str]) -> Commands {
    Cli::try_parse_from(argv)
        .unwrap_or_else(|e| panic!("contract argv must parse: {argv:?} -> {e}"))
        .command
}

/// One settings mutation plus the two master verbs the contract names,
/// each carried on an assignment-pinned target. No credential exists on
/// this machine's state dir and none may be consulted first.
#[test]
fn assignment_pinned_target_refuses_settings_and_master_verbs_before_credentials() {
    // The whole clap `Commands` enum parses deep; match the
    // executable's normal main-thread stack, not libtest's smaller
    // worker stack (same convention as cad1568_native_messages).
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(managed_guard_scenario)
        .unwrap()
        .join()
        .unwrap();
}

fn managed_guard_scenario() {
    for argv in [
        &[
            "cadence",
            "settings",
            "agents",
            "pin",
            "put",
            "@scope/ext",
            "--pinned",
            "1.0.0",
            "--expected-revision",
            "7:deadbeef",
        ][..],
        &["cadence", "master", "status"][..],
        &["cadence", "master", "resume"][..],
    ] {
        let err = remote::run(target(true), parsed(argv), 0, None)
            .expect_err("managed remote settings/master call must refuse");
        assert_eq!(
            err.code(),
            Some("settings_agent_session_required"),
            "argv {argv:?} refused with the wrong error — only the typed guard \
             code proves the managed-caller refusal fired before credentials: {err}"
        );
    }
}
