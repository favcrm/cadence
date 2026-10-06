// Independently authored acceptance: copied verbatim from root Spec/security check.
use super::*;
#[test]
fn protected_profile_caller_elections_and_missing_authority_never_spawn() {
    use std::ffi::OsString;
    let base = vec![
        "exec".to_string(),
        "--profile".to_string(),
        "pi-guest".to_string(),
        format!("--alias-sha256={}", "a".repeat(64)),
        format!("--generation={}", "b".repeat(32)),
    ];
    let before = protected_effect_count();
    // Every caller election must fail the REAL parser, not just a fake policy.
    for election in [
        "--path=/tmp/attacker",
        "--node=/tmp/attacker",
        "--fd=9",
        "--hash=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "--uid=0",
        "--key=caller-key",
        "--verified=true",
        "--restored=true",
        "--eligible=true",
    ] {
        let mut args = base.clone();
        args.push(election.to_string());
        args.extend(["--".to_string(), "/tmp/attacker".to_string()]);
        let args: Vec<OsString> = args.into_iter().map(OsString::from).collect();
        assert!(
            policy::parse(&args).is_err(),
            "caller election accepted: {election}"
        );
        assert_eq!(
            protected_effect_count(),
            before,
            "effect before parser refusal"
        );
    }
    // Valid routing segments/provider tokens are NOT production authority.
    // Exercise actual dispatch with missing REAL release/private prerequisites.
    let mut args = base;
    args.extend([
        "--".to_string(),
        "/tmp/attacker".to_string(),
        "--verified=true".to_string(),
    ]);
    let args: Vec<OsString> = args.into_iter().map(OsString::from).collect();
    match policy::parse(&args) {
        Ok(request) => {
            let refused = dispatch_protected_request(request);
            assert!(
                refused.is_err(),
                "unavailable production authority admitted"
            );
        }
        // Rejecting provider-elected executable tokens at parse is legitimate,
        // but cannot substitute for the valid-profile dispatcher case below.
        Err(_) => {}
    }
    let args: Vec<OsString> = [
        "exec",
        "--profile",
        "pi-guest",
        &format!("--alias-sha256={}", "a".repeat(64)),
        &format!("--generation={}", "b".repeat(32)),
        "--",
        "--mode",
        "rpc",
        "--no-session",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    let request = policy::parse(&args).expect("valid fixed protected routing syntax");
    assert!(
        dispatch_protected_request(request).is_err(),
        "missing compiled pins/private grant/current authority must refuse"
    );
    assert_eq!(
        protected_effect_count(),
        before,
        "unavailable/forged profile reached nodeopen, identity drop or exec"
    );

    // Focused additive UNIT: the exact shared NON-x64 production refusal body.
    // On x64 this is NOT its supported dispatcher, nor ARM/main/NSS execution.
    // Valid selector DATA proves only that the early missing-model gate passes;
    // it cannot elect a Root image, private operation, OwnedLaunch or authority.
    let valid_args: Vec<OsString> = [
        "exec",
        "--profile",
        "pi-guest",
        &format!("--alias-sha256={}", "a".repeat(64)),
        &format!("--generation={}", "b".repeat(32)),
        "--",
        "--mode",
        "rpc",
        "--no-session",
        "--model",
        "openai-codex/gpt-6.1-sol",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    let valid_request = policy::parse(&valid_args).expect("explicit-model profile must parse");
    let selection = match &valid_request {
        policy::Request::PiGuest(profile) => profile
            .selection()
            .expect("mandatory real valid selection DATA baseline"),
        _ => panic!("protected syntax did not produce actual PiGuest profile"),
    };
    assert_eq!(selection.alias_sha256, "a".repeat(64));
    assert_eq!(selection.generation, "b".repeat(32));
    assert_eq!(selection.model, "openai-codex/gpt-6.1-sol");
    assert_eq!(
        selection.role,
        protected_pi_profile::authority::Role::Master
    );
    let before_shared = protected_effect_count();
    let unsupported = refuse_unsupported_protected_request(valid_request)
        .expect_err("valid selection bypassed actual unsupported-custody refusal");
    assert_eq!(unsupported.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(
        unsupported.to_string(),
        "protected Pi launch requires Linux x86_64 constructor custody; unsupported target refused"
    );
    assert_eq!(
        protected_effect_count(),
        before_shared,
        "shared unsupported refusal crossed a protected effect marker"
    );

    // Distinguish the old syntax-valid no-model DATA rejection from reaching
    // that final platform guard. Never fill a model default or skip the probe.
    let missing_request = policy::parse(&args).expect("no-model routing syntax must still parse");
    match &missing_request {
        policy::Request::PiGuest(profile) => assert_eq!(
            profile
                .selection()
                .expect_err("missing model silently selected a default"),
            "protected owner requires explicit model"
        ),
        _ => panic!("no-model protected syntax did not produce PiGuest"),
    }
    let missing = refuse_unsupported_protected_request(missing_request)
        .expect_err("missing selection model bypassed its actual DATA guard");
    assert_eq!(missing.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(
        missing.to_string(),
        "protected owner requires explicit model"
    );
    assert_ne!(missing.to_string(), unsupported.to_string());
    assert_eq!(
        protected_effect_count(),
        before_shared,
        "missing-model refusal crossed a protected effect marker"
    );
}
