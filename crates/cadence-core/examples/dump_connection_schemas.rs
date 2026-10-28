//! Emit the v1 Connection JSON Schema next to the historical core contracts.
//!
//! `cargo run -p cadence-core --example dump-connection-schemas -- <dir>`
//! writes one file per type; the `core-wasm` workflow runs it into `dist/`.
fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "dist".to_string());
    let dir = std::path::PathBuf::from(dir);
    cadence_core::connection_dispatch::schema::write_connection_schemas(&dir)
        .expect("write generated Connection schemas");
    for (name, _) in cadence_core::connection_dispatch::schema::connection_schemas() {
        println!("{name}");
    }
}
