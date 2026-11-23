//! The delivery-requirements evaluator now lives in `cadence-core`, so the
//! hosted tenant and the CLI classify with the same code (CAD-1946).
//!
//! Re-exported here so every existing path — the CLI, the daemon RPC, the gate
//! script and the acceptance suites — keeps its name.

pub use cadence_core::delivery_requirements::*;
