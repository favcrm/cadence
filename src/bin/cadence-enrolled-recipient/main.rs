//! Fixed non-setuid recipient. ACK is consumption transport, never Pi launch.
fn main() {
    if cadence_agent::enrolled_recipient_entry().is_err() {
        eprintln!("enrolled recipient UNKNOWN — no retry or launch evidence");
        std::process::exit(1);
    }
}
