//! Root-only protected runtime owner.
//!
//! The binary is deliberately trivial: every reviewable behaviour lives in
//! `cadence_agent::protected_runtime`. It refuses to run as any uid other than
//! 0 and it performs no build-time action.
fn main() {
    std::process::exit(cadence_agent::protected_runtime::run_owner_cli());
}
