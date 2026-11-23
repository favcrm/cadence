//! Run the platform-minted contract check without building the library test harness.

use cadence_agent::board_identity;

mod board_assertion;

fn main() {
    board_assertion::check();
}
