//! CAD-1429 test-only harness for the Publish Web workflow. As of CAD-1642 the
//! workflow lives in `crate::web_sites`; the CLI is a thin staging client, so
//! these tests exercise the same protocol code the daemon adapter runs.

#[path = "cli/web_transport_tests.rs"]
mod transport_tests;

#[path = "cli/web_tls_acceptance.rs"]
mod tls_acceptance;
