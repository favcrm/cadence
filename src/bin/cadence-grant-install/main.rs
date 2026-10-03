//! Fixed non-setuid carrier. No installation/privileged execution this batch.
fn main() {
    if cadence_agent::installer_carrier_entry().is_err() {
        eprintln!("fixed installer carrier unavailable/UNKNOWN");
        std::process::exit(1);
    }
}
