//! CAD-1123 HP4 acceptance check. RESERVED: written by someone other than
//! the implementer (the reviewer or the ticket author), per AGENTS.md
//! "Gates and security work"; the implementer must not edit it.
//!
//! It must prove these cases are refused, against the real guards:
//! - a double send (two `social_publish_start now` taps, or start then
//!   `social_publish_send_now`) makes one provider call;
//! - a non-operator (agent, detached child, board member) is refused on
//!   `social_publish_start`, `social_publish_reschedule` and
//!   `app_binding_publish_set`, over the RPC and the HTTP routes;
//! - a reschedule race (stale `expected_due_epoch`, or a claim in between)
//!   changes nothing and leaves the old schedule;
//! - a forged destination (a `destination_id`, `toolkit`, `grant_id`,
//!   `timezone`, scope or `approval_id` in the request) is refused: the
//!   destination comes only from the run's publication binding.
#![cfg(feature = "test-seam")]
