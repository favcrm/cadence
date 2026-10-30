//! CAD-1946 refusal acceptance seam — reserved for the reviewer.
//!
//! AGENTS.md "Gates and security work": the ONE acceptance check proving
//! the hosted merge-gate reads refuse the bad cases is written from the
//! ticket by someone other than the implementer, and the implementer may
//! not edit or weaken it. It belongs in this file, which is compiled as a
//! test child of `ui::cli_delivery_reads` and so can call `serve` directly.
//! Cases named by the kickoff: an agent caller, a member caller, a moved
//! head, a foreign repo/PR pair, and an unreachable org.
