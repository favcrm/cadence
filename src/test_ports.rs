//! Test-only board ports (CAD-1712).
//!
//! Board fixtures used to pick a port by binding it, keep the listener as a
//! reservation, then drop it so the real board could bind. Two tests in one
//! process could pick the same port in that window and the second board would
//! fail with `fixture BLOCKED: board could not bind` — which ejects PRs from the
//! merge queue. One cursor per process removes the collision; the bind in
//! [`reserve_board_port`] still proves the port is free of outside holders.

use std::net::TcpListener;
use std::sync::atomic::{AtomicU32, Ordering};

/// First and last board ports fixtures may use (the staging range).
const FIRST: u16 = 3110;
const LAST: u16 = 3199;

/// Ports another fixture reserves for its own purpose.
const RESERVED: [u16; 2] = [3139, 3140];

/// Saturating: once the range is spent the cursor stays spent, so even a
/// caught panic can never wrap it back to a port that was already issued.
static CURSOR: AtomicU32 = AtomicU32::new(0);

/// The next board port this process has not handed out, bound as the caller's
/// reservation. The caller drops the listener immediately before starting the
/// board, exactly as the inline scan did.
pub(crate) fn reserve_board_port() -> (u16, TcpListener) {
    loop {
        // No wrap: a port this process has handed out is never offered again,
        // even after its reservation is dropped. Exhaustion fails closed.
        let offset = loop {
            let value = CURSOR.load(Ordering::Relaxed);
            if CURSOR
                .compare_exchange_weak(
                    value,
                    value.saturating_add(1),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                break value;
            }
        };
        let port = match candidate(offset) {
            Candidate::Port(port) => port,
            // A reserved slot costs a cursor step, not the rest of the range.
            Candidate::Reserved => continue,
            Candidate::Spent => {
                panic!("fixture BLOCKED: no isolated board port in {FIRST}-{LAST}")
            }
        };
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
            return (port, listener);
        }
    }
}

/// What the cursor finds at `offset`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Candidate {
    Port(u16),
    /// A port another fixture owns: never handed out, and skipped.
    Reserved,
    /// Past the end of the range: the cursor is spent.
    Spent,
}

fn candidate(offset: u32) -> Candidate {
    if offset > u32::from(LAST - FIRST) {
        return Candidate::Spent;
    }
    let port = FIRST + offset as u16;
    if RESERVED.contains(&port) {
        Candidate::Reserved
    } else {
        Candidate::Port(port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn the_cursor_never_repeats_a_port_in_one_process() {
        let mut seen = HashSet::new();
        for _ in 0..8 {
            let (port, reservation) = reserve_board_port();
            assert!(seen.insert(port), "{port} handed out twice");
            assert!((FIRST..=LAST).contains(&port), "{port}");
            assert!(!RESERVED.contains(&port), "{port} is reserved");
            drop(reservation);
        }
    }

    #[test]
    fn a_spent_cursor_fails_closed_instead_of_wrapping() {
        // Every offset in the range maps to a distinct port, to a reserved
        // slot, or past the end. A port is never offered twice, and a reserved
        // slot is skipped rather than ending the range.
        let mut ports = HashSet::new();
        let mut reserved = Vec::new();
        for offset in 0..=u32::from(LAST - FIRST) {
            match candidate(offset) {
                Candidate::Port(port) => {
                    assert!((FIRST..=LAST).contains(&port), "{port}");
                    assert!(ports.insert(port), "{port} issued twice");
                }
                Candidate::Reserved => reserved.push(FIRST + offset as u16),
                Candidate::Spent => panic!("offset {offset} ended the range early"),
            }
        }
        assert_eq!(reserved, RESERVED);
        assert_eq!(
            ports.len(),
            (LAST - FIRST + 1 - RESERVED.len() as u16) as usize
        );
        assert_eq!(candidate(u32::from(LAST - FIRST) + 1), Candidate::Spent);
        assert_eq!(candidate(u32::MAX), Candidate::Spent);
    }
}
