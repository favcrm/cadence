//! Test-only board ports (CAD-1712).
//!
//! Board fixtures used to pick a port by binding it, keep the listener as a
//! reservation, then drop it so the real board could bind. Two tests in one
//! process could pick the same port in that window and the second board would
//! fail with `fixture BLOCKED: board could not bind` — which ejects PRs from the
//! merge queue. One cursor per process removes the collision; the bind in
//! [`reserve_board_port`] still proves the port is free of outside holders.

use std::net::TcpListener;
use std::sync::atomic::{AtomicU16, Ordering};

/// First and last board ports fixtures may use (the staging range).
const FIRST: u16 = 3110;
const LAST: u16 = 3199;

/// Ports another fixture reserves for its own purpose.
const RESERVED: [u16; 2] = [3139, 3140];

static CURSOR: AtomicU16 = AtomicU16::new(0);

/// The next board port this process has not handed out, bound as the caller's
/// reservation. The caller drops the listener immediately before starting the
/// board, exactly as the inline scan did.
pub(crate) fn reserve_board_port() -> (u16, TcpListener) {
    let span = LAST - FIRST + 1;
    for _ in 0..span {
        let offset = CURSOR.fetch_add(1, Ordering::Relaxed) % span;
        let port = FIRST + offset;
        if RESERVED.contains(&port) {
            continue;
        }
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
            return (port, listener);
        }
    }
    panic!("fixture BLOCKED: no isolated board port in {FIRST}-{LAST}");
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
}
