# Manual acceptance harnesses

## CAD-1485 native round-2 acceptance

- Crate: `tests/manual/cad1485_round2_acceptance/Cargo.toml`
- Target: `tests/manual/cad1485_round2_native_acceptance.rs` (`native-r2`)
- Run (QA-admitted executor only):
  `cargo test --manifest-path tests/manual/cad1485_round2_acceptance/Cargo.toml --test native-r2`
- `cadence-agent` is a NORMAL library dependency, not `cfg(test)`. The target keeps the production issuer transport defaults and the isolated `/tmp/c1485-r2q-99da1/native` root. It is not registered in the default CI test set.
