//! Stateless credentials must not require a home directory or local daemon.

#[test]
fn environment_status_without_home_reaches_issuer_validation() {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    command
        .env_clear()
        .args(["auth", "status"])
        .env("CADENCE_TOKEN", "agc_synthetic")
        .env("CADENCE_ISSUER", "http://invalid.example")
        .env("CADENCE_ORG", "ws_fixture");
    // Reject plaintext before network access. This proves the env-only path
    // reaches issuer validation without HOME/XDG_CONFIG_HOME or storage.
    let output = cadence_agent::reaper::output(&mut command).unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("Issuer must be an HTTPS origin"), "{error}");
    assert!(!error.contains("Set HOME"));
    assert!(!error.contains("agc_synthetic"));
}
