//! Fixed waiting client; construction alone cannot release/consume/launch.
fn main() {
    if cadence_agent::installer_waiting_client_entry().is_err() {
        eprintln!("fixed waiting installer unavailable/UNKNOWN");
        std::process::exit(1);
    }
}
