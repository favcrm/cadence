//! CAD-1429 test-only harness. Compiles the production `cli/web.rs` functions
//! into the library unit-test binary, so `cfg!(test)` in the library (which
//! admits the loopback `http://127.0.0.1` pin the transport accepts only in
//! tests) applies to the same code the `cadence` binary runs.
//!
//! This module is test-only and adds no production behavior. The `super`
//! prelude mirrors what `cli/web.rs` expects from its binary-side parent.

use clap::Subcommand;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::{Error, Result};

#[path = "cli/web.rs"]
mod web;

#[path = "cli/web_transport_tests.rs"]
mod transport_tests;

#[path = "cli/web_tls_acceptance.rs"]
mod tls_acceptance;
