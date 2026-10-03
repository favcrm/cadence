//! Fixed host-only observer; diagnostics never grant enrollment/release.
fn main() {
    if cadence_agent::installer_observer_entry().is_err() {
        eprintln!("fixed installer observation unavailable/UNKNOWN");
        std::process::exit(1);
    }
}
