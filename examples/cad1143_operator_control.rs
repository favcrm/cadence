//! CAD-1143 destinations-read and prepared-intent operator control
//! (external, independently authored by cc13-pi-acc793 — never the
//! implementer).
//!
//! The implementation lives in `cad1143_operator_control/imp.rs`, kept
//! behind `cfg(feature = "test-seam")` so the whole fixture compiles only
//! with the seam. This shim always provides exactly one `main`: without
//! the feature it prints the required build flags and exits 2; with it,
//! it runs the real operator control. Splitting the module keeps the
//! default `cargo build --examples` target compilable — a crate-level
//! `#![cfg]` on the whole file would leave it with no `main` at all.

#[cfg(feature = "test-seam")]
#[path = "cad1143_operator_control/imp.rs"]
mod imp;

#[cfg(feature = "test-seam")]
fn main() {
    imp::control();
}

#[cfg(not(feature = "test-seam"))]
fn main() {
    eprintln!(
        "build with `--features test-seam`: cargo run --features test-seam \
         --example cad1143_operator_control"
    );
    std::process::exit(2);
}
