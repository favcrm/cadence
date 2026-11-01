//! The one integration suite (CAD-1970).
//!
//! The per-module acceptance suites and the contract tests for this repo's own
//! tooling were retired. What remains is one target so `cargo nextest` has
//! something to run, plus the lib and bins unit suites.
//!
//! The assertion is behavioural and pure — no process, socket or clock: an
//! identifier the daemon accepts is one every caller can rely on.

#[test]
fn an_identifier_is_validated_before_anything_uses_it() {
    assert!(cadence_agent::proto::identifier("cad1234", "issue id").is_ok());
    assert!(cadence_agent::proto::identifier("not a valid id", "issue id").is_err());
}
