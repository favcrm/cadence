//! Crate root shim for `crates/cadence-daemon` so submodule lookups stay
//! anchored inside `src/daemon/` while `src/daemon.rs` remains in place.
include!("../daemon.rs");
