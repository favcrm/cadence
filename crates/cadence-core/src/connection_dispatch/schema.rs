//! Generated JSON Schema for the v1 Connection contract surface.
//!
//! The ordinary `core-wasm` job runs the `dump_connection_schemas` example to
//! emit these into the release artifact next to the historical contract
//! schemas. They are public metadata shapes: no secret, key or ciphertext
//! field exists in any of them.
use crate::connection_dispatch::dto::{
    AdmissionSnapshotV1, ConnectionInvokeV1, CredentialRequestArgsV1, CredentialRequestV1,
    DispatchCommitV1, DispatchReceiptV1, ErrorCodeV1, LoadedReceiptV1, ManifestV1, ModelQuoteV1,
    PrincipalBindingV1, QuoteArgsV1, ResultArgsV1, SessionCallV1, SettlementV1, SnapshotArgsV1,
    SnapshotV1, StageSendArgsV1, VerifyConnectionV1,
};
use crate::connection_dispatch::ledger::{CheckpointAckV1, CheckpointV1, LedgerHeadV1};
use schemars::schema_for;

fn pretty(schema: schemars::Schema) -> String {
    serde_json::to_string_pretty(&schema).expect("schema serializes") + "\n"
}

/// `(repository-relative path, generated schema)` for every Connection contract.
pub fn connection_committed_schemas() -> Vec<(&'static str, String)> {
    vec![
        (
            "contracts/cadence-core/v1/connection-invoke.schema.json",
            pretty(schema_for!(ConnectionInvokeV1)),
        ),
        (
            "contracts/cadence-core/v1/connection-admission-snapshot.schema.json",
            pretty(schema_for!(AdmissionSnapshotV1)),
        ),
        (
            "contracts/cadence-core/v1/connection-model-quote.schema.json",
            pretty(schema_for!(ModelQuoteV1)),
        ),
        (
            "contracts/cadence-core/v1/connection-dispatch-commit.schema.json",
            pretty(schema_for!(DispatchCommitV1)),
        ),
        (
            "contracts/cadence-core/v1/connection-dispatch-receipt.schema.json",
            pretty(schema_for!(DispatchReceiptV1)),
        ),
        (
            "contracts/cadence-core/v1/connection-settlement.schema.json",
            pretty(schema_for!(SettlementV1)),
        ),
        (
            "contracts/cadence-core/v1/connection-manifest.schema.json",
            pretty(schema_for!(ManifestV1)),
        ),
        (
            "contracts/cadence-core/v1/connection-snapshot.schema.json",
            pretty(schema_for!(SnapshotV1)),
        ),
        (
            "contracts/cadence-core/v1/connection-loaded-receipt.schema.json",
            pretty(schema_for!(LoadedReceiptV1)),
        ),
        (
            "contracts/cadence-core/v1/connection-principal-binding.schema.json",
            pretty(schema_for!(PrincipalBindingV1)),
        ),
        (
            "contracts/cadence-core/v1/connection-credential-request.schema.json",
            pretty(schema_for!(CredentialRequestV1)),
        ),
        (
            "contracts/cadence-core/v1/connection-request-args.schema.json",
            pretty(schema_for!(CredentialRequestArgsV1)),
        ),
        (
            "contracts/cadence-core/v1/connection-verify.schema.json",
            pretty(schema_for!(VerifyConnectionV1)),
        ),
        (
            "contracts/cadence-core/v1/connection-session-call.schema.json",
            pretty(schema_for!(SessionCallV1)),
        ),
        (
            "contracts/cadence-core/v1/connection-snapshot-args.schema.json",
            pretty(schema_for!(SnapshotArgsV1)),
        ),
        (
            "contracts/cadence-core/v1/connection-quote-args.schema.json",
            pretty(schema_for!(QuoteArgsV1)),
        ),
        (
            "contracts/cadence-core/v1/connection-stage-send-args.schema.json",
            pretty(schema_for!(StageSendArgsV1)),
        ),
        (
            "contracts/cadence-core/v1/connection-result-args.schema.json",
            pretty(schema_for!(ResultArgsV1)),
        ),
        (
            "contracts/cadence-core/v1/connection-ledger.schema.json",
            pretty(schema_for!(LedgerHeadV1)),
        ),
        (
            "contracts/cadence-core/v1/connection-ledger-record.schema.json",
            pretty(schema_for!(
                crate::connection_dispatch::ledger::LedgerRecordV1
            )),
        ),
        (
            "contracts/cadence-core/v1/connection-checkpoint.schema.json",
            pretty(schema_for!(CheckpointV1)),
        ),
        (
            "contracts/cadence-core/v1/connection-checkpoint-ack.schema.json",
            pretty(schema_for!(CheckpointAckV1)),
        ),
        (
            "contracts/cadence-core/v1/connection-error.schema.json",
            pretty(schema_for!(ErrorCodeV1)),
        ),
    ]
}

/// `(file name, generated schema)` for the release and schema dump producer.
pub fn connection_schemas() -> Vec<(&'static str, String)> {
    const PREFIX: &str = "contracts/cadence-core/v1/";
    connection_committed_schemas()
        .into_iter()
        .map(|(path, body)| {
            (
                path.strip_prefix(PREFIX)
                    .expect("Connection schema path prefix"),
                body,
            )
        })
        .collect()
}

/// Write every generated schema into `dir` (creating it).
pub fn write_connection_schemas(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for (name, body) in connection_schemas() {
        std::fs::write(dir.join(name), body)?;
    }
    Ok(())
}
