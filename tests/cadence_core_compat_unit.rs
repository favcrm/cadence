//! Canonical compatibility-rule unit checks, kept in the root integration
//! suite because the required result-test selection runs this package's
//! targets. They exercise the public `cadence_core::app_compat` surface that
//! the daemon's admission path uses.
use cadence_core::app_compat::{
    parse_requires, Compat, HostContracts, SemVer, SUPPORTED_CONTRACTS,
};

/// Parse `needs:` YAML into the mapping `parse_requires` expects.
fn needs(indent_yaml: &str) -> serde_yaml::Mapping {
    let v: serde_yaml::Value = serde_yaml::from_str(&format!("needs:\n{indent_yaml}")).unwrap();
    v.get("needs").unwrap().as_mapping().unwrap().clone()
}

fn host_at(core: &str) -> HostContracts<'static> {
    HostContracts {
        core: SemVer::parse(core).unwrap(),
        supported: SUPPORTED_CONTRACTS,
    }
}

#[test]
fn absent_requires_is_the_legacy_package() {
    let compat = parse_requires(None).unwrap();
    assert!(!compat.declared());
    let compat = parse_requires(Some(&needs("  connections: [cms]"))).unwrap();
    assert!(!compat.declared());
    // Backward compatibility: a legacy manifest admits on any host.
    assert!(compat.check(&host_at("0.0.1")).is_ok());
}

#[test]
fn core_range_admits_in_range_and_refuses_out() {
    let compat = parse_requires(Some(&needs("  requires:\n    core: '>=0.2.0 <2.0.0'"))).unwrap();
    assert!(compat.declared());
    assert_eq!(compat.core.as_ref().unwrap().minimum().unwrap().major, 0);
    assert!(compat.check(&host_at("0.2.0")).is_ok());
    assert!(compat.check(&host_at("1.9.9")).is_ok());
    let e = compat.check(&host_at("0.1.0")).unwrap_err();
    assert!(e.to_string().contains("requires core"), "{e}");
    let e = compat.check(&host_at("2.0.0")).unwrap_err();
    assert!(e.to_string().contains("requires core"), "{e}");
}

#[test]
fn unknown_contract_refuses() {
    // A storage/effect contract does not exist — the registry never
    // invents one, so a package that names it refuses as unknown.
    let compat = parse_requires(Some(&needs(
        "  requires:\n    contracts: {app-storage: [1]}",
    )))
    .unwrap();
    let e = compat.check(&host_at("0.1.0")).unwrap_err();
    assert!(e.to_string().contains("app-storage"), "{e}");
    assert!(e.to_string().contains("none"), "{e}");
}

#[test]
fn supported_and_unsupported_majors() {
    let compat = parse_requires(Some(&needs(
        "  requires:\n    contracts: {app-chat: [1], app-actions: [1]}",
    )))
    .unwrap();
    assert!(compat.check(&host_at("0.1.0")).is_ok());
    // Requiring a major the host does not ship refuses, naming it.
    let compat =
        parse_requires(Some(&needs("  requires:\n    contracts: {app-chat: [2]}"))).unwrap();
    let e = compat.check(&host_at("0.1.0")).unwrap_err();
    assert!(e.to_string().contains("app-chat"), "{e}");
    assert!(e.to_string().contains('2'), "{e}");
}

#[test]
fn malformed_supplied_requires_fails_closed() {
    for bad in [
        "  requires: ''",                                 // not a mapping
        "  requires: {}",                                 // empty declaration
        "  requires: {contracts: {}}",                    // empty contract map
        "  requires:\n    core: 0.2",                     // non-string range
        "  requires:\n    core: '>=1.2'",                 // partial version
        "  requires:\n    core: '>=01.2.3'",              // leading zero
        "  requires:\n    core: ['>=1.2.3']",             // list, not string
        "  requires:\n    contracts: {app-chat: 1}",      // bare major
        "  requires:\n    contracts: {app-chat: 'v1'}",   // string major
        "  requires:\n    contracts: {app-chat: []}",     // empty list
        "  requires:\n    contracts: {app-chat: [0]}",    // zero major
        "  requires:\n    contracts: {app-chat: [-1]}",   // negative
        "  requires:\n    contracts: {app-chat: [1, 1]}", // repeated
        "  requires:\n    contracts: {AppChat: [1]}",     // bad name
        "  requires:\n    bogus: {}",                     // unknown key
        "  requires:\n    core: '>=0.1.0'\n    contracts: {app-chat: [2.5]}",
    ] {
        assert!(
            parse_requires(Some(&needs(bad))).is_err(),
            "{bad} must refuse, not degrade"
        );
    }
}

/// Semver §9 grammar: a malformed literal refuses at parse, never
/// parses partially — `1.0.0-alpha..1`, `1.0.0+`, `1.0.0-01` and a
/// second `+` all fail closed.
#[test]
fn semver_parse_bounds() {
    for bad in [
        "",
        "1",
        "1.2",
        "1.2.3.4",
        "v1.2.3",
        "1.2.x",
        "01.2.3",
        "1.0.0-",
        "1.0.0-alpha..1",
        "1.0.0-alpha.",
        "1.0.0-.x",
        "1.0.0+",
        "1.0.0+build.",
        "1.0.0+build..meta",
        "1.0.0+a+b",
        "1.0.0-01",
        "1.0.0-alpha_1",
    ] {
        assert!(SemVer::parse(bad).is_err(), "{bad}");
    }
    let v = SemVer::parse("1.2.3-beta.1+build").unwrap();
    assert_eq!(v.major, 1);
    assert_eq!(v.to_string(), "1.2.3-beta.1");
    // Release outranks prerelease at the same triple.
    assert!(SemVer::parse("1.0.0").unwrap() > SemVer::parse("1.0.0-beta").unwrap());
}

/// Semver §11.4 precedence — the real ordering: prerelease below
/// release, numeric before alphanumeric, numeric by value,
/// alphanumeric lexically, and a shorter prefix of equal
/// identifiers below the longer. A requirement on one prerelease
/// never admits another.
#[test]
fn semver_full_precedence() {
    let chain = [
        "1.0.0-alpha",
        "1.0.0-alpha.1",
        "1.0.0-alpha.beta",
        "1.0.0-beta",
        "1.0.0-beta.2",
        "1.0.0-beta.11",
        "1.0.0-rc.1",
        "1.0.0",
    ];
    for i in 0..chain.len() {
        for j in 0..chain.len() {
            let a = SemVer::parse(chain[i]).unwrap();
            let b = SemVer::parse(chain[j]).unwrap();
            let (ia, ib) = (chain[i], chain[j]);
            assert_eq!(a.cmp(&b), i.cmp(&j), "{ia} vs {ib}");
        }
    }
    // A package pinning one prerelease refuses another: `=1.0.0-alpha`
    // is not satisfied by `1.0.0-beta` — the exact bug the review caught.
    let compat = parse_requires(Some(&needs("  requires: {core: '=1.0.0-alpha'}"))).unwrap();
    let err = compat.check(&host_at("1.0.0-beta")).unwrap_err();
    assert!(err.to_string().contains("requires core"), "{err}");
    assert!(compat.check(&host_at("1.0.0-alpha")).is_ok());
    // A host on a prerelease sits below the stable floor a package
    // floors on — `>=1.0.0` refuses `1.0.0-rc.1`.
    let compat = parse_requires(Some(&needs("  requires: {core: '>=1.0.0'}"))).unwrap();
    assert!(compat.check(&host_at("1.0.0-rc.1")).is_err());
    assert!(compat.check(&host_at("1.0.0")).is_ok());
    // …and above it admits a prerelease still inside the range.
    let compat = parse_requires(Some(&needs("  requires: {core: '>=1.0.0-alpha'}"))).unwrap();
    assert!(compat.check(&host_at("1.0.0-beta")).is_ok());
}

#[test]
fn digest_line_changes_on_requires() {
    let legacy = Compat::legacy();
    assert!(legacy.digest_line().is_empty());
    let declared = parse_requires(Some(&needs(
        "  requires:\n    core: '>=0.1.0'\n    contracts: {app-chat: [1]}",
    )))
    .unwrap();
    let line = declared.digest_line();
    assert!(line.starts_with("requires="), "{line}");
    assert!(line.contains("app-chat"), "{line}");
    // Different requirement → different digest line (re-gate).
    let other = parse_requires(Some(&needs(
        "  requires:\n    core: '>=0.2.0'\n    contracts: {app-chat: [1]}",
    )))
    .unwrap();
    assert_ne!(declared.digest_line(), other.digest_line());
}
