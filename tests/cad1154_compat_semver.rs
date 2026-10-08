//! CAD-1154 follow-up acceptance checks — authored independently of the
//! compat implementation after PM verification flagged the SemVer edge:
//! prerelease identifiers were being dropped and `host()` invented a
//! `0.0.0` fallback. These tests prove the required refusal semantics at
//! the public contract surface (`parse_requires` → `Compat::check`
//! against a supplied `HostContracts`, the same call the daemon's
//! `validate_contents` admission runs) and at the real install seam.
//!
//! The contract under test is the ticket's REQ-002/REQ-003: a package
//! declares the minimum core it needs, and an incompatible requirement
//! is refused with the unmet requirement named — never silently admitted
//! and never evaluated against a version the host did not actually run.
//!
//! Unlike the author's own unit tests, these checks drive the boundary
//! the way a hostile or careless package does: through `parse_manifest`
//! and `app_workspace_install`, and through `Compat::check` with host
//! descriptors the test constructs to represent hosts this build does
//! not control (a prerelease host, a foreign-contract host). The
//! implementation's internals (`PreId`, `cmp`) are never touched — only
//! the semver results a requirement actually produces.
//!
//! Authorship boundary: the compat implementer must not edit or weaken
//! these checks. The refused/accepted pairs below are the contract.
#![cfg(feature = "test-seam")]

use cadence_agent::issue::app::{
    compat_host, parse_requires, Compat, HostContracts, SemVer, SUPPORTED_CONTRACTS,
};
use serde_yaml::Value as Yaml;

/// Build a `HostContracts` naming `core` on this build's real contract
/// registry — the stand-in for "the host this package lands on".
fn host_at(core: &str) -> HostContracts<'static> {
    HostContracts {
        core: SemVer::parse(core).unwrap_or_else(|_| panic!("fixture version {core}")),
        supported: SUPPORTED_CONTRACTS,
    }
}

/// `needs:` mapping for `parse_requires`, matching the manifest shape.
fn needs(yaml: &str) -> serde_yaml::Mapping {
    let v: Yaml = serde_yaml::from_str(&format!("needs:\n{yaml}")).unwrap();
    v.get("needs").unwrap().as_mapping().unwrap().clone()
}

/// Parse a `requires:` block the way a package writes it.
fn requires(yaml: &str) -> Compat {
    parse_requires(Some(&needs(yaml)))
        .unwrap_or_else(|e| panic!("fixture `requires:` must parse: {e}\n{yaml}"))
}

// ---------------------------------------------------------------------
// A. Prerelease ordering — the dropped-identifier bug class
// ---------------------------------------------------------------------

/// A host on a prerelease does NOT satisfy a package's stable floor:
/// `>=1.0.0` must refuse `1.0.0-rc.1`. If identifiers were dropped to a
/// bare tag, or ordering treated prerelease as equal to release, this
/// passes wrongly — that is exactly the regression PM flagged.
#[test]
fn prerelease_host_does_not_satisfy_stable_floor() {
    let compat = requires("  requires: {core: '>=1.0.0'}");
    // Every prerelease of the floor's own triple is below it.
    for pre in ["1.0.0-alpha", "1.0.0-beta.2", "1.0.0-rc.1", "1.0.0-alpha.1"] {
        let e = compat.check(&host_at(pre)).unwrap_err().to_string();
        assert!(
            e.contains("requires core") || e.contains("core"),
            "prerelease {pre} wrongly satisfied '>=1.0.0' (or refusal unnamed): {e}"
        );
    }
    // Positive control: the release itself, and a later release, satisfy.
    assert!(compat.check(&host_at("1.0.0")).is_ok());
    assert!(compat.check(&host_at("2.4.7")).is_ok());
}

/// Differing prereleases of the same triple are NOT interchangeable:
/// a package pinning `=1.0.0-alpha` is not satisfied by `1.0.0-beta`,
/// `1.0.0-alpha.1`, or the release. This is the case that fails if the
/// prerelease is collapsed to a tag — the declaration supports naming a
/// specific prerelease, so distinct ones must order distinctly.
#[test]
fn differing_prereleases_do_not_compare_equal() {
    let compat = requires("  requires: {core: '=1.0.0-alpha'}");
    for other in [
        "1.0.0-beta",
        "1.0.0-alpha.1",
        "1.0.0-rc.1",
        "1.0.0",
        "1.0.0-alpha.beta",
    ] {
        assert!(
            compat.check(&host_at(other)).is_err(),
            "=1.0.0-alpha wrongly satisfied by {other}"
        );
    }
    assert!(compat.check(&host_at("1.0.0-alpha")).is_ok());
}

/// Within a range, ordering follows real semver precedence — not
/// declaration order, not tag collapse: `1.0.0-alpha < 1.0.0-alpha.1 <
/// 1.0.0-beta < 1.0.0-rc.1 < 1.0.0`. A package that floors on a
/// prerelease admits later prereleases but never earlier ones.
#[test]
fn prerelease_range_orders_by_semver_precedence() {
    let compat = requires("  requires: {core: '>=1.0.0-alpha <1.0.0'}");
    // Below the prerelease floor: refuse even though the triple matches.
    assert!(
        compat.check(&host_at("1.0.0-alpha")).is_ok(),
        "floor itself"
    );
    for low in ["0.9.9", "1.0.0-0"] {
        assert!(
            compat.check(&host_at(low)).is_err(),
            "{low} wrongly inside '>=1.0.0-alpha <1.0.0'"
        );
    }
    // Later prereleases of the same triple are inside the range.
    for inside in ["1.0.0-beta", "1.0.0-rc.1"] {
        assert!(
            compat.check(&host_at(inside)).is_ok(),
            "{inside} should satisfy '>=1.0.0-alpha <1.0.0'"
        );
    }
    // The release is outside (excluded by the upper bound).
    assert!(compat.check(&host_at("1.0.0")).is_err());
}

/// The parsed minimum surfaces the real floor — a prerelease floor is
/// reported as that prerelease, not rounded up to the release, so the
/// refusal receipt tells the truth about what is required.
#[test]
fn minimum_reports_the_prerelease_floor_not_the_release() {
    let compat = requires("  requires: {core: '>=1.0.0-alpha'}");
    let min = compat
        .core
        .as_ref()
        .and_then(|r| r.minimum())
        .expect("range has a floor");
    assert_eq!(
        min.to_string(),
        "1.0.0-alpha",
        "minimum rounded the prerelease floor up to the release"
    );
}

// ---------------------------------------------------------------------
// B. Malformed requirements refuse before any mutation
// ---------------------------------------------------------------------

/// A malformed `requires:` — a bad version literal, a non-mapping, an
/// unknown key, a malformed contract list — fails the manifest at
/// parse, never reaches admission, and (proven at the install seam
/// below) writes nothing. Each is a supplied requirement that must
/// fail closed rather than degrade to the legacy package.
#[test]
fn malformed_requires_literals_refuse_at_parse() {
    for bad in [
        // SemVer literal violations inside the declared range.
        "  requires: {core: '>=1.0.0-alpha..1'}", // empty prerelease id
        "  requires: {core: '>=1.0.0-'}",         // trailing dash, empty pre
        "  requires: {core: '>=1.0.0+'}",         // empty build
        "  requires: {core: '>=1.0.0+build..x'}", // empty build id
        "  requires: {core: '>=1.0.0-01'}",       // leading-zero numeric pre
        "  requires: {core: '>=1.0.0-alpha_1'}",  // illegal ident byte
        "  requires: {core: '>=1.0.0.1'}",        // four-part core
        "  requires: {core: '>=v1.0.0'}",         // v-prefix is not semver
        "  requires: {core: '>=01.0.0'}",         // leading-zero major
        "  requires: {core: ''}",                 // empty range
        "  requires: {core: 123}",                // non-string range
        // Structure violations.
        "  requires: ''",                             // not a mapping
        "  requires: {bogus: x}",                     // unknown key
        "  requires: {contracts: {app-chat: 1}}",     // bare major, not a list
        "  requires: {contracts: {app-chat: []}}",    // empty list
        "  requires: {contracts: {app-chat: [0]}}",   // zero major
        "  requires: {contracts: {app-chat: [1,1]}}", // repeated major
        "  requires: {contracts: {App-Chat: [1]}}",   // bad contract name
    ] {
        let parsed = parse_requires(Some(&needs(bad)));
        assert!(parsed.is_err(), "malformed requires must refuse: {bad}");
    }
}

/// The install seam: a package whose `requires:` is malformed is refused
/// at `app_workspace_install` BEFORE any catalog row, journal file or
/// record store exists. The refusal happens on the manifest, ahead of
/// every other admission stage, so a hostile bundle cannot smuggle a
/// bad requirement past validation into a half-written install.
#[test]
fn malformed_requires_refuses_install_before_any_mutation() {
    use cadence_agent::test_seam::{scoped, Asserted, Seam};
    use cadence_agent::{client, daemon};
    use serde_json::json;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::time::Duration;

    let root = tempfile::Builder::new().prefix("c1154m").tempdir().unwrap();
    let pm_dir = root.path().join("pm");
    cadence_agent::issue::Pm::init(&pm_dir).unwrap();
    let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
    env.set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let opts = daemon::ServeOptions {
        provider_env: env,
        stop: Some(stop.clone()),
        test_seam: true,
        auto_stop: Some(daemon::AutoStopSetting::off()),
        report_router: Some(0),
        checkup: Some(0),
        ..Default::default()
    };
    let state = root.path().join("s");
    let stop_thread = stop.clone();
    let daemon_thread = {
        let state = state.clone();
        std::thread::spawn(move || daemon::serve_with(&state, opts).unwrap())
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(2)).is_err()
        || Seam::token_at(&state).is_none()
    {
        assert!(std::time::Instant::now() < deadline, "daemon never started");
        std::thread::sleep(Duration::from_millis(50));
    }
    let _guard = scopeguard(stop_thread, daemon_thread);

    // A package carrying a malformed requires.core — an empty
    // prerelease identifier that semver §9 forbids.
    let app = root.path().join("app");
    std::fs::create_dir_all(app.join("workflows")).unwrap();
    std::fs::write(
        app.join("app.md"),
        "---\napp: bad-pre\ntitle: Bad\nversion: '1.0.0'\nneeds:\n  requires: {core: '>=1.0.0-alpha..1'}\n---\n\nGuide.\n",
    )
    .unwrap();
    std::fs::write(
        app.join("workflows/w.md"),
        "---\ntitle: t\ngoal: g\nlabel: l\ninputs:\n  writer: { ask: \"w\" }\n---\n\n## s\nagent: {{writer}}\nsize: S\naction: local.text.produce\n\nx\n\n### Acceptance\n- [ ] y\n",
    )
    .unwrap();

    let refused = scoped(Asserted::Operator, || {
        client::rpc(&state, "app_workspace_install", json!({"source": app}))
    })
    .expect_err("malformed requires must refuse install");
    let reason = refused.to_string();
    assert!(
        reason.contains("requires") || reason.contains("version") || reason.contains("installable"),
        "refusal must name the requirement it rejected, got: {reason}"
    );
    // No catalog row and no record file exist for the refused install.
    let listed = scoped(Asserted::Operator, || {
        client::rpc(&state, "app_workspace_list", json!({}))
    });
    match listed {
        Ok(v) => assert!(
            v.as_array().map(Vec::is_empty).unwrap_or(true),
            "malformed requires left a catalog row: {v}"
        ),
        Err(e) => assert!(
            e.to_string().contains("catalog source is missing"),
            "unexpected catalog error: {e}"
        ),
    }
    let records_dir = state.join(cadence_agent::store::app_records::RECORDS_DIR);
    if records_dir.is_dir() {
        assert!(
            std::fs::read_dir(&records_dir).unwrap().next().is_none(),
            "malformed requires left a record file"
        );
    }
}

/// A host the build cannot describe itself is never evaluated against
/// an invented floor: `compat_host()` returns `Result` and must either
/// report the real compiled-in version or fail — there is no silent
/// `0.0.0` fallback that would satisfy every `>=` floor by default.
/// This is the fail-closed contract PM asked to be pinned.
#[test]
fn host_descriptor_is_real_or_refused_never_invented() {
    let host = compat_host().unwrap_or_else(|e| {
        panic!("compat_host() must report this build's real version or fail, got: {e}")
    });
    // The reported version is the compiled-in literal, parsed — not a
    // fabricated default. Prove it by comparing against the env var.
    let literal = env!("CARGO_PKG_VERSION");
    let parsed =
        SemVer::parse(literal).unwrap_or_else(|_| panic!("CARGO_PKG_VERSION {literal} must parse"));
    assert_eq!(
        host.core.to_string(),
        parsed.to_string(),
        "host() reported a version that is not this build's own"
    );
    // And it is not the invented zero: a real build never reports
    // 0.0.0-prerelease as a floor-satisfying identity unless that is
    // genuinely its version.
    if literal != "0.0.0" && literal != "0.0.0-prerelease" {
        assert_ne!(
            host.core.to_string(),
            "0.0.0",
            "host() invented a zero-version fallback"
        );
    }
}

/// Drop the daemon cleanly even on panic — the fixture owns its thread
/// and must not leak a listener into the next test's port scan.
fn scopeguard(
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: std::thread::JoinHandle<()>,
) -> impl Drop {
    struct Guard(
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        Option<std::thread::JoinHandle<()>>,
    );
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.store(true, SeqCstGuard);
            if let Some(h) = self.1.take() {
                let _ = h.join();
            }
        }
    }
    use std::sync::atomic::Ordering::SeqCst as SeqCstGuard;
    Guard(stop, Some(handle))
}
