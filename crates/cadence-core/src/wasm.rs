//! The wasm ABIs. Single-threaded, buffer ownership crosses the seam
//! explicitly; the caller frees both buffers with their exact lengths.
//!
//! Two groups of exports exist:
//! * the historical `core_alloc`/`core_evaluate`/`core_free` evaluator, and
//! * the CAD-1085 Connection dispatch rules (`core_connection_*`).
//!
//! No custody crypto export exists in this build. Only the separate
//! `custody-crypto` build adds `core_custody_*` (see the custody_wasm module).
use crate::connection_dispatch::canonical::{
    exact_byte_digest, material_digest_for_domain, parse_strict, ParseError,
};
use crate::connection_dispatch::dto::validate_receipt_against_commit;
use crate::connection_dispatch::dto::AdmissionRequestV1;
use crate::connection_dispatch::dto::{
    checkpoint_matches_commit, validate_admission_snapshot_v1, validate_commit_v1,
    validate_connection_invoke_v1, validate_credential_request_v1, validate_dispatch_receipt_v1,
    validate_manifest_v1, validate_model_quote_v1, validate_session_call_v1, validate_snapshot_v1,
    validate_verify_connection_v1, CredentialRequestV1, FrozenCredentialIdentityV1, ManifestV1,
    PreparedDispatchV1, PrincipalBindingV1, SessionCallV1, SnapshotV1, VerifyConnectionV1,
    MAX_MANAGEMENT_BYTES, MAX_RAW_CREDENTIAL_BYTES, MAX_REFUSAL_BYTES,
};
use crate::connection_dispatch::dto::{
    AdmissionSnapshotV1, ConnectionInvokeV1, DispatchCommitV1, DispatchError, DispatchReceiptV1,
    ErrorCodeV1, HumanSmtpVerifyAdmissionV1, ModelQuoteV1, PlanOutcomeV1, ValidatedOutcomeV1,
    CONNECTION_CONTRACT_VERSION,
};
use crate::connection_dispatch::ledger::{
    extend_ledger_v1, genesis_ledger_head_v1, verify_ledger_chain_v1, CheckpointAckV1,
    CheckpointV1, LedgerHeadV1, LedgerMutationV1, LedgerRecordV1, LedgerWatermarkV1,
};
use crate::connection_dispatch::plan::verify_frozen_dispatch_v1;
use crate::connection_dispatch::plan::{
    plan_connection_dispatch_v1, settle_connection_dispatch_v1,
};
use crate::connection_dispatch::primitives::Revision;
use crate::connection_dispatch::secrets::{KnownSecretV1, ProjectionError};
use crate::evaluate::{evaluate, refusal_bytes, Code, MAX_INPUT_BYTES};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;
use serde_json::Value;

/// Largest single exchange through the Connection entries: a bounded snapshot
/// (256 KiB) plus one bounded operation (1 MiB) plus the envelope.
pub const MAX_CONNECTION_EXCHANGE_BYTES: usize = 2 * 1024 * 1024;
/// Buffer the host may allocate for either entry point.
pub const MAX_ALLOC_BYTES: usize = MAX_CONNECTION_EXCHANGE_BYTES;

#[no_mangle]
pub extern "C" fn core_alloc(length: usize) -> *mut u8 {
    if length > MAX_ALLOC_BYTES {
        return std::ptr::null_mut();
    }
    Box::into_raw(vec![0_u8; length].into_boxed_slice()) as *mut u8
}

/// Returns `(length << 32) | ptr` of the JSON response.
#[no_mangle]
pub unsafe extern "C" fn core_evaluate(ptr: *const u8, length: usize) -> u64 {
    let output = if ptr.is_null() || length > MAX_INPUT_BYTES {
        refusal_bytes(Code::InvalidCommand, "invalid input buffer")
    } else {
        evaluate(std::slice::from_raw_parts(ptr, length))
    };
    export(output)
}

#[no_mangle]
pub unsafe extern "C" fn core_free(ptr: *mut u8, length: usize) {
    if !ptr.is_null() {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
            ptr, length,
        )));
    }
}

fn export(output: Vec<u8>) -> u64 {
    let bytes = output.into_boxed_slice();
    let length = bytes.len() as u64;
    let ptr = Box::into_raw(bytes) as *mut u8 as u64;
    (length << 32) | ptr
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanEnvelopeV1 {
    v: u32,
    snapshot: AdmissionSnapshotV1,
    request: ConnectionInvokeV1,
    quote: Option<ModelQuoteV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HumanVerifyPlanEnvelopeV1 {
    v: u32,
    snapshot: AdmissionSnapshotV1,
    request: HumanSmtpVerifyAdmissionV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettleEnvelopeV1 {
    v: u32,
    commit: DispatchCommitV1,
    outcome: ValidatedOutcomeV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptEnvelopeV1 {
    v: u32,
    receipt: DispatchReceiptV1,
    commit: Option<DispatchCommitV1>,
}

fn ok_bytes(data: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"ok": true, "data": data})).expect("bounded JSON result")
}

fn error_bytes(error: &DispatchError) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "ok": false,
        "error": {
            "v": CONNECTION_CONTRACT_VERSION,
            "code": error.code,
            "message": error.message,
        }
    }))
    .expect("bounded JSON refusal")
}

fn parse_error(error: ParseError) -> Vec<u8> {
    let code = match error {
        ParseError::TooLarge => ErrorCodeV1::PayloadTooLarge,
        ParseError::TooDeep | ParseError::Ambiguous | ParseError::Invalid(_) => {
            ErrorCodeV1::InvalidRequest
        }
    };
    error_bytes(&DispatchError::new(code))
}

fn version(v: u32) -> Result<(), DispatchError> {
    if v == CONNECTION_CONTRACT_VERSION {
        Ok(())
    } else {
        Err(DispatchError::new(ErrorCodeV1::UnsupportedVersion))
    }
}

fn plan_agent(input: &[u8]) -> Vec<u8> {
    let envelope: PlanEnvelopeV1 = match parse_strict(input, MAX_CONNECTION_EXCHANGE_BYTES) {
        Ok(envelope) => envelope,
        Err(error) => return parse_error(error),
    };
    if let Err(error) = version(envelope.v) {
        return error_bytes(&error);
    }
    let PlanEnvelopeV1 {
        snapshot,
        request,
        quote,
        ..
    } = envelope;
    match plan_connection_dispatch_v1(
        &snapshot,
        AdmissionRequestV1::Agent(request),
        quote.as_ref(),
    ) {
        Ok(PlanOutcomeV1::Prepared(prepared)) => ok_bytes(json!({
            "kind": "prepared",
            "commit": prepared.commit_facts(),
            "previous_head": prepared.previous_head(),
        })),
        Ok(PlanOutcomeV1::Duplicate(status)) => {
            ok_bytes(json!({"kind": "duplicate", "status": status}))
        }
        Err(error) => error_bytes(&error),
    }
}

fn plan_human_verify(input: &[u8]) -> Vec<u8> {
    let envelope: HumanVerifyPlanEnvelopeV1 =
        match parse_strict(input, MAX_CONNECTION_EXCHANGE_BYTES) {
            Ok(envelope) => envelope,
            Err(error) => return parse_error(error),
        };
    if let Err(error) = version(envelope.v) {
        return error_bytes(&error);
    }
    let HumanVerifyPlanEnvelopeV1 {
        snapshot, request, ..
    } = envelope;
    match plan_connection_dispatch_v1(
        &snapshot,
        AdmissionRequestV1::HumanSmtpVerify(request),
        None,
    ) {
        Ok(PlanOutcomeV1::Prepared(prepared)) => ok_bytes(json!({
            "kind": "prepared",
            "commit": prepared.commit_facts(),
            "previous_head": prepared.previous_head(),
        })),
        Ok(PlanOutcomeV1::Duplicate(status)) => {
            ok_bytes(json!({"kind": "duplicate", "status": status}))
        }
        Err(error) => error_bytes(&error),
    }
}

fn settle(input: &[u8]) -> Vec<u8> {
    let envelope: SettleEnvelopeV1 = match parse_strict(input, MAX_CONNECTION_EXCHANGE_BYTES) {
        Ok(envelope) => envelope,
        Err(error) => return parse_error(error),
    };
    if let Err(error) = version(envelope.v) {
        return error_bytes(&error);
    }
    let SettleEnvelopeV1 {
        commit, outcome, ..
    } = envelope;
    match settle_connection_dispatch_v1(&commit, outcome) {
        Ok(settlement) => ok_bytes(json!(settlement)),
        Err(error) => error_bytes(&error),
    }
}

fn receipt(input: &[u8]) -> Vec<u8> {
    let envelope: ReceiptEnvelopeV1 = match parse_strict(input, MAX_CONNECTION_EXCHANGE_BYTES) {
        Ok(envelope) => envelope,
        Err(error) => return parse_error(error),
    };
    if let Err(error) = version(envelope.v) {
        return error_bytes(&error);
    }
    let ReceiptEnvelopeV1 {
        receipt, commit, ..
    } = envelope;
    let checked = match &commit {
        Some(commit) => validate_receipt_against_commit(&receipt, commit),
        None => validate_dispatch_receipt_v1(&receipt),
    };
    match checked {
        Ok(()) => ok_bytes(json!(receipt)),
        Err(error) => error_bytes(&error),
    }
}

unsafe fn exchange(ptr: *const u8, length: usize, handler: fn(&[u8]) -> Vec<u8>) -> u64 {
    if ptr.is_null() || length > MAX_CONNECTION_EXCHANGE_BYTES {
        return export(error_bytes(&DispatchError::new(
            ErrorCodeV1::PayloadTooLarge,
        )));
    }
    export(handler(std::slice::from_raw_parts(ptr, length)))
}

/// Plan one agent invocation from trusted-adapter facts.
#[no_mangle]
pub unsafe extern "C" fn core_connection_plan_v1(ptr: *const u8, length: usize) -> u64 {
    exchange(ptr, length, plan_agent)
}

/// Plan the internal human SMTP verification admission.
#[no_mangle]
pub unsafe extern "C" fn core_connection_plan_human_verify_v1(
    ptr: *const u8,
    length: usize,
) -> u64 {
    exchange(ptr, length, plan_human_verify)
}

/// Recompute a settlement receipt from the exact durable commit + validated
/// outcome. The adapter's durable CAS, not this call, guarantees one winner.
#[no_mangle]
pub unsafe extern "C" fn core_connection_settle_v1(ptr: *const u8, length: usize) -> u64 {
    exchange(ptr, length, settle)
}

/// Validate a dispatch receipt, optionally against the commit it claims.
#[no_mangle]
pub unsafe extern "C" fn core_connection_receipt_v1(ptr: *const u8, length: usize) -> u64 {
    exchange(ptr, length, receipt)
}

// ---------------------------------------------------------------------------
// Shared rule exports for the hosted adapter (no TS duplicate policy)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerGenesisEnvelopeV1 {
    v: u32,
    database_epoch: Revision,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerExtendEnvelopeV1 {
    v: u32,
    head: LedgerHeadV1,
    mutation: LedgerMutationV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerVerifyEnvelopeV1 {
    v: u32,
    records: Vec<LedgerRecordV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerAdvanceEnvelopeV1 {
    v: u32,
    watermark: LedgerWatermarkV1,
    records: Vec<LedgerRecordV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FinalizeEnvelopeV1 {
    v: u32,
    prepared: DispatchCommitV1,
    previous_head: LedgerHeadV1,
    head: LedgerHeadV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointMatchEnvelopeV1 {
    v: u32,
    commit: DispatchCommitV1,
    ack: CheckpointAckV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidateEnvelopeV1 {
    v: u32,
    kind: String,
    value: Value,
}

fn decode_value<T: DeserializeOwned>(value: Value) -> Result<T, DispatchError> {
    serde_json::from_value(value).map_err(|_| DispatchError::new(ErrorCodeV1::InvalidRequest))
}

fn ledger_error() -> DispatchError {
    DispatchError::new(ErrorCodeV1::LedgerMismatch)
}

fn digest_bytes(domain: &[u8], json: &[u8]) -> Vec<u8> {
    let Ok(domain) = std::str::from_utf8(domain) else {
        return error_bytes(&DispatchError::new(ErrorCodeV1::InvalidRequest));
    };
    let value: Value = match parse_strict(json, MAX_CONNECTION_EXCHANGE_BYTES) {
        Ok(value) => value,
        Err(error) => return parse_error(error),
    };
    ok_bytes(json!({"digest": material_digest_for_domain(domain, &value)}))
}

fn exact_digest(bytes: &[u8]) -> Vec<u8> {
    ok_bytes(json!({"digest": exact_byte_digest(bytes)}))
}

fn verify_frozen(commit_bytes: &[u8], body: &[u8], identity_bytes: &[u8]) -> Vec<u8> {
    let commit: DispatchCommitV1 = match parse_strict(commit_bytes, MAX_MANAGEMENT_BYTES) {
        Ok(commit) => commit,
        Err(error) => return parse_error(error),
    };
    let identity: FrozenCredentialIdentityV1 = match parse_strict(identity_bytes, MAX_REFUSAL_BYTES)
    {
        Ok(identity) => identity,
        Err(error) => return parse_error(error),
    };
    match verify_frozen_dispatch_v1(&commit, body, &identity) {
        Ok(()) => ok_bytes(json!({"verified": true})),
        Err(error) => error_bytes(&error),
    }
}

fn validate_any(input: &[u8]) -> Vec<u8> {
    let envelope: ValidateEnvelopeV1 = match parse_strict(input, MAX_CONNECTION_EXCHANGE_BYTES) {
        Ok(envelope) => envelope,
        Err(error) => return parse_error(error),
    };
    if let Err(error) = version(envelope.v) {
        return error_bytes(&error);
    }
    let ValidateEnvelopeV1 { kind, value, .. } = envelope;
    let checked: Result<(), DispatchError> =
        match kind.as_str() {
            "connection-invoke" => decode_value::<ConnectionInvokeV1>(value)
                .and_then(|value| validate_connection_invoke_v1(&value)),
            "admission-snapshot" => decode_value::<AdmissionSnapshotV1>(value)
                .and_then(|value| validate_admission_snapshot_v1(&value)),
            "model-quote" => decode_value::<ModelQuoteV1>(value)
                .and_then(|value| validate_model_quote_v1(&value)),
            "dispatch-commit" => {
                decode_value::<DispatchCommitV1>(value).and_then(|value| validate_commit_v1(&value))
            }
            "dispatch-receipt" => decode_value::<DispatchReceiptV1>(value)
                .and_then(|value| validate_dispatch_receipt_v1(&value)),
            "manifest" => {
                decode_value::<ManifestV1>(value).and_then(|value| validate_manifest_v1(&value))
            }
            "snapshot" => {
                decode_value::<SnapshotV1>(value).and_then(|value| validate_snapshot_v1(&value))
            }
            "principal-binding" => decode_value::<PrincipalBindingV1>(value).map(|_| ()),
            "credential-request" => decode_value::<CredentialRequestV1>(value)
                .and_then(|value| validate_credential_request_v1(&value)),
            "verify-connection" => decode_value::<VerifyConnectionV1>(value)
                .and_then(|value| validate_verify_connection_v1(&value)),
            "session-call" => decode_value::<SessionCallV1>(value)
                .and_then(|value| validate_session_call_v1(&value)),
            "checkpoint" => decode_value::<CheckpointV1>(value)
                .and_then(|value| value.validate().map_err(|_| ledger_error())),
            "checkpoint-ack" => decode_value::<CheckpointAckV1>(value)
                .and_then(|value| value.validate().map_err(|_| ledger_error())),
            "ledger-head" => decode_value::<LedgerHeadV1>(value)
                .and_then(|value| value.validate().map_err(|_| ledger_error())),
            "ledger-record" => decode_value::<LedgerRecordV1>(value).map(|_| ()),
            _ => return error_bytes(&DispatchError::new(ErrorCodeV1::InvalidRequest)),
        };
    match checked {
        Ok(()) => ok_bytes(json!({"valid": true})),
        Err(error) => error_bytes(&error),
    }
}

fn ledger_genesis(input: &[u8]) -> Vec<u8> {
    let envelope: LedgerGenesisEnvelopeV1 = match parse_strict(input, MAX_MANAGEMENT_BYTES) {
        Ok(envelope) => envelope,
        Err(error) => return parse_error(error),
    };
    if let Err(error) = version(envelope.v) {
        return error_bytes(&error);
    }
    ok_bytes(json!(genesis_ledger_head_v1(envelope.database_epoch)))
}

fn ledger_extend(input: &[u8]) -> Vec<u8> {
    let envelope: LedgerExtendEnvelopeV1 = match parse_strict(input, MAX_MANAGEMENT_BYTES) {
        Ok(envelope) => envelope,
        Err(error) => return parse_error(error),
    };
    if let Err(error) = version(envelope.v) {
        return error_bytes(&error);
    }
    match extend_ledger_v1(&envelope.head, &envelope.mutation) {
        Ok(head) => ok_bytes(json!(head)),
        Err(_) => error_bytes(&ledger_error()),
    }
}

fn ledger_verify(input: &[u8]) -> Vec<u8> {
    let envelope: LedgerVerifyEnvelopeV1 = match parse_strict(input, MAX_CONNECTION_EXCHANGE_BYTES)
    {
        Ok(envelope) => envelope,
        Err(error) => return parse_error(error),
    };
    if let Err(error) = version(envelope.v) {
        return error_bytes(&error);
    }
    match verify_ledger_chain_v1(&envelope.records) {
        Ok(head) => ok_bytes(json!(head)),
        Err(_) => error_bytes(&ledger_error()),
    }
}

fn ledger_advance(input: &[u8]) -> Vec<u8> {
    let envelope: LedgerAdvanceEnvelopeV1 = match parse_strict(input, MAX_CONNECTION_EXCHANGE_BYTES)
    {
        Ok(envelope) => envelope,
        Err(error) => return parse_error(error),
    };
    if let Err(error) = version(envelope.v) {
        return error_bytes(&error);
    }
    let mut watermark = envelope.watermark;
    match watermark.advance_from_chain(&envelope.records) {
        Ok(head) => ok_bytes(json!({"watermark": watermark, "head": head})),
        Err(_) => error_bytes(&ledger_error()),
    }
}

fn finalize_commit(input: &[u8]) -> Vec<u8> {
    let envelope: FinalizeEnvelopeV1 = match parse_strict(input, MAX_CONNECTION_EXCHANGE_BYTES) {
        Ok(envelope) => envelope,
        Err(error) => return parse_error(error),
    };
    if let Err(error) = version(envelope.v) {
        return error_bytes(&error);
    }
    match PreparedDispatchV1::resume(envelope.prepared, envelope.previous_head)
        .finalize(&envelope.head)
    {
        Ok(commit) => ok_bytes(json!(commit)),
        Err(error) => error_bytes(&error),
    }
}

fn checkpoint_match(input: &[u8]) -> Vec<u8> {
    let envelope: CheckpointMatchEnvelopeV1 = match parse_strict(input, MAX_MANAGEMENT_BYTES) {
        Ok(envelope) => envelope,
        Err(error) => return parse_error(error),
    };
    if let Err(error) = version(envelope.v) {
        return error_bytes(&error);
    }
    match checkpoint_matches_commit(&envelope.commit, &envelope.ack) {
        Ok(()) => ok_bytes(json!({"matches": true})),
        Err(error) => error_bytes(&error),
    }
}

unsafe fn bounded<'a>(ptr: *const u8, length: usize, max: usize) -> Option<&'a [u8]> {
    if ptr.is_null() || length > max {
        return None;
    }
    Some(std::slice::from_raw_parts(ptr, length))
}

/// `sha256:<hex>` of exact bytes (provider body digest).
#[no_mangle]
pub unsafe extern "C" fn core_exact_byte_digest_v1(ptr: *const u8, length: usize) -> u64 {
    match bounded(ptr, length, MAX_CONNECTION_EXCHANGE_BYTES) {
        Some(bytes) => export(exact_digest(bytes)),
        None => export(error_bytes(&DispatchError::new(
            ErrorCodeV1::PayloadTooLarge,
        ))),
    }
}

/// Canonical material digest for `<domain>\n<json>` (the same extractor the
/// native app material and ledger use).
#[no_mangle]
pub unsafe extern "C" fn core_material_digest_v1(
    domain_ptr: *const u8,
    domain_len: usize,
    json_ptr: *const u8,
    json_len: usize,
) -> u64 {
    match (
        bounded(domain_ptr, domain_len, 128),
        bounded(json_ptr, json_len, MAX_CONNECTION_EXCHANGE_BYTES),
    ) {
        (Some(domain), Some(json)) => export(digest_bytes(domain, json)),
        _ => export(error_bytes(&DispatchError::new(
            ErrorCodeV1::PayloadTooLarge,
        ))),
    }
}

/// Body bytes + frozen commit/identity comparison.
#[no_mangle]
pub unsafe extern "C" fn core_connection_verify_frozen_v1(
    commit_ptr: *const u8,
    commit_len: usize,
    body_ptr: *const u8,
    body_len: usize,
    identity_ptr: *const u8,
    identity_len: usize,
) -> u64 {
    match (
        bounded(commit_ptr, commit_len, MAX_MANAGEMENT_BYTES),
        bounded(body_ptr, body_len, MAX_CONNECTION_EXCHANGE_BYTES),
        bounded(identity_ptr, identity_len, MAX_REFUSAL_BYTES),
    ) {
        (Some(commit), Some(body), Some(identity)) => export(verify_frozen(commit, body, identity)),
        _ => export(error_bytes(&DispatchError::new(
            ErrorCodeV1::PayloadTooLarge,
        ))),
    }
}

/// Strict shared validation for one declared DTO kind.
#[no_mangle]
pub unsafe extern "C" fn core_connection_validate_v1(ptr: *const u8, length: usize) -> u64 {
    exchange(ptr, length, validate_any)
}

/// Bind a prepared dispatch to the ACTUAL journal head produced by the NEW
/// durable transaction, returning the finalized `DispatchCommitV1` row.
#[no_mangle]
pub unsafe extern "C" fn core_connection_finalize_v1(ptr: *const u8, length: usize) -> u64 {
    exchange(ptr, length, finalize_commit)
}

#[no_mangle]
pub unsafe extern "C" fn core_ledger_genesis_v1(ptr: *const u8, length: usize) -> u64 {
    exchange(ptr, length, ledger_genesis)
}

#[no_mangle]
pub unsafe extern "C" fn core_ledger_extend_v1(ptr: *const u8, length: usize) -> u64 {
    exchange(ptr, length, ledger_extend)
}

#[no_mangle]
pub unsafe extern "C" fn core_ledger_verify_v1(ptr: *const u8, length: usize) -> u64 {
    exchange(ptr, length, ledger_verify)
}

#[no_mangle]
pub unsafe extern "C" fn core_ledger_watermark_advance_v1(ptr: *const u8, length: usize) -> u64 {
    exchange(ptr, length, ledger_advance)
}

#[no_mangle]
pub unsafe extern "C" fn core_ledger_checkpoint_match_v1(ptr: *const u8, length: usize) -> u64 {
    exchange(ptr, length, checkpoint_match)
}

fn projection_error(error: ProjectionError) -> DispatchError {
    let code = match error {
        ProjectionError::SecretFragment => ErrorCodeV1::DataPolicyRefused,
        ProjectionError::TooLarge => ErrorCodeV1::PayloadTooLarge,
        ProjectionError::Malformed => ErrorCodeV1::InvalidRequest,
    };
    DispatchError::new(code)
}

fn known_secret(
    secret: &[u8],
    metadata: &[u8],
    text: Option<&[u8]>,
    json_value: Option<&[u8]>,
) -> Vec<u8> {
    let metadata: Vec<String> = match parse_strict(metadata, MAX_MANAGEMENT_BYTES) {
        Ok(metadata) => metadata,
        Err(error) => return parse_error(error),
    };
    let known = match KnownSecretV1::new(secret, &metadata) {
        Ok(known) => known,
        Err(error) => return error_bytes(&projection_error(error)),
    };
    let screened = if let Some(text) = text {
        let Ok(text) = std::str::from_utf8(text) else {
            return error_bytes(&DispatchError::new(ErrorCodeV1::InvalidRequest));
        };
        known.screen_text(text)
    } else if let Some(json_value) = json_value {
        match parse_strict::<Value>(json_value, MAX_CONNECTION_EXCHANGE_BYTES) {
            Ok(value) => known.screen_json(&value),
            Err(error) => return parse_error(error),
        }
    } else {
        return error_bytes(&DispatchError::new(ErrorCodeV1::InvalidRequest));
    };
    match screened {
        Ok(()) => ok_bytes(json!({"clear": true})),
        Err(error) => error_bytes(&projection_error(error)),
    }
}

/// Screen decoded text against the credential + account metadata (existing
/// native semantics: whole value or any 8-character window; a hit refuses).
#[no_mangle]
pub unsafe extern "C" fn core_known_secret_screen_v1(
    secret_ptr: *const u8,
    secret_len: usize,
    metadata_ptr: *const u8,
    metadata_len: usize,
    text_ptr: *const u8,
    text_len: usize,
) -> u64 {
    match (
        bounded(secret_ptr, secret_len, MAX_RAW_CREDENTIAL_BYTES),
        bounded(metadata_ptr, metadata_len, MAX_MANAGEMENT_BYTES),
        bounded(text_ptr, text_len, MAX_CONNECTION_EXCHANGE_BYTES),
    ) {
        (Some(secret), Some(metadata), Some(text)) => {
            export(known_secret(secret, metadata, Some(text), None))
        }
        _ => export(error_bytes(&DispatchError::new(
            ErrorCodeV1::PayloadTooLarge,
        ))),
    }
}

/// Structural companion: walk every decoded JSON string/key/scalar.
#[no_mangle]
pub unsafe extern "C" fn core_known_secret_screen_json_v1(
    secret_ptr: *const u8,
    secret_len: usize,
    metadata_ptr: *const u8,
    metadata_len: usize,
    json_ptr: *const u8,
    json_len: usize,
) -> u64 {
    match (
        bounded(secret_ptr, secret_len, MAX_RAW_CREDENTIAL_BYTES),
        bounded(metadata_ptr, metadata_len, MAX_MANAGEMENT_BYTES),
        bounded(json_ptr, json_len, MAX_CONNECTION_EXCHANGE_BYTES),
    ) {
        (Some(secret), Some(metadata), Some(json_value)) => {
            export(known_secret(secret, metadata, None, Some(json_value)))
        }
        _ => export(error_bytes(&DispatchError::new(
            ErrorCodeV1::PayloadTooLarge,
        ))),
    }
}
