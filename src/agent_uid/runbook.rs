//! `cadence agent-uid runbook` — the operator's runbook, verbatim.
//!
//! `RUNBOOK.md` beside this file is the source of truth — it is
//! tracked (unlike `docs/adr/`, which is local working material), so
//! the binary carries exactly what T2 reviewed. The local
//! `docs/adr/0007-provision-runbook.md` copy is generated from it.

use crate::error::Result;

/// The runbook text — included at build time, never read from disk at
/// run time (the installed binary works without the checkout).
pub const RUNBOOK: &str = include_str!("RUNBOOK.md");

/// Print the runbook. Always succeeds, always exit 0 — a read.
pub fn cli() -> Result<i32> {
    print!("{RUNBOOK}");
    Ok(0)
}
