//! CAD-1073 safety-floor binary: the minimum executable coverage retained
//! while the legacy test suite is retired. Proves release-boundary and
//! refusal contracts that the reduced gate still enforces.
//!
//! This is intentionally small: it exercises the CLI's own refusal paths and
//! the test-seam exclusion without depending on the retired fixture
//! infrastructure.

use cadence_agent::reaper;
use std::process::Command;

#[test]
fn safety_floor_test_seam_refuses_release_build() {
    // The release binary must never carry the test seam. This assertion is
    // checked by the CI build job's "Assert the release binary carries no
    // test seam" step; we mirror the check here so the floor itself can fail.
    let mut cmd = Command::new("cargo");
    cmd.args(["check", "--release", "--locked", "--features", "test-seam"]);
    let out = reaper::output(&mut cmd).expect("cargo check failed");
    assert!(
        !out.status.success(),
        "test-seam release build must fail: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn safety_floor_cli_refuses_unknown_verb() {
    // Basic CLI refusal: unknown verbs exit non-zero. This proves the binary
    // still enforces its command surface during the clean-slate window.
    let mut cmd = Command::new("target/debug/cadence");
    cmd.args(["nonexistent-verb"]);
    let out = reaper::output(&mut cmd).expect("cadence binary must exist to test refusal");
    assert!(!out.status.success(), "unknown verb must refuse");
}
