//! The one integration suite (CAD-1970).
//!
//! The per-module acceptance suites and the contract tests for this repo's own
//! tooling were retired. What remains is one target so `cargo nextest` has
//! something real to run, plus the lib and bins unit suites.
//!
//! The assertion is about the shipped artifact, which no unit test can make:
//! the binary that CI just built reports a version and exits successfully.

#[test]
fn the_built_binary_reports_its_version() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--version")
        .output()
        .expect("cadence --version runs");
    assert!(
        output.status.success(),
        "cadence --version exited {:?}",
        output.status
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("cadence"),
        "unexpected --version output: {text:?}"
    );
}
