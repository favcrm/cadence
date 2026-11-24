use cadence_agent::platform::adapter::PlatformAdapter;
use cadence_agent::platform::agenticos_external::board_fixture::*;
use cadence_agent::platform::agenticos_external::{MANIFEST_PIN, PLATFORM};
use cadence_agent::platform::AppCapabilityError;
use serde_json::{json, Value};
use std::io::Read;
use std::time::Duration;

#[path = "../platform/agenticos_external/tests/hosted_source_board.rs"]
mod hosted_source_board;
