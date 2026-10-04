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
}
