//! Fixed no-argv provider-pipe entry, never a generic exec/sign RPC.
fn main() {
    if cadence_agent::root_constructor_entry().is_err() {
        eprintln!("root constructor UNKNOWN — no retry or launch evidence");
        std::process::exit(1);
    }
}
