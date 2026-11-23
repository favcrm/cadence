//! Run the platform-minted contract check without building the library test harness.

use cadence_agent::board_identity;

#[path = "../tests/fixtures/agenticos/board_assertion.rs"]
mod fixture;

fn main() {
    fixture::check();
}
