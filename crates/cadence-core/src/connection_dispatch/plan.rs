//! Pure, synchronous admission, frozen-plan verification and settlement.
//!
//! Every number is checked; every binding is compared field-by-field. The
//! planner consumes trusted-adapter facts (never caller assertions) and emits
//! the exact durable dispatch row plus a new ledger head. The output is DATA:
//! it never authorizes execution on its own.
use crate::connection_dispatch::canonical::{
    exact_byte_digest, material_digest_value, CONNECTION_DISPATCH_DOMAIN,
};
use crate::connection_dispatch::dto::{
    destination_digest_for, messages_digest, operation_material, validate_commit_v1,
    AdmissionRequestV1, AdmissionSnapshotV1, AgentOperationV1, ConnectionInvokeV1, CurrencyV1,
    DispatchCommitV1, DispatchError, DispatchReceiptV1, DispatchStateV1, ErrorCodeV1,
    ExistingDispatchStatusV1, FrozenCredentialIdentityV1, GrantStateV1, HumanActionRecordV1,
    OperationKindV1, OutcomeKindV1, OutcomeResultV1, PlanOutcomeV1, PreparedDispatchV1,
    PrincipalKindV1, ProviderKindV1, ReceiptCostV1, ReceiptErrorV1, ReceiptResultV1,
    ResolvedDestinationV1, SettlementV1, SmtpAcceptanceClaimV1, SmtpAcceptedV1,
    SmtpVerificationClaimV1, SmtpVerifiedV1, TextGeneratedV1, ValidatedOutcomeV1, RATE_WINDOW_MS,
};
use crate::connection_dispatch::policy::pi_model_decision;
use crate::connection_dispatch::primitives::{
    reserve_usd_micros, Counter, Digest, EpochMs, Ident, UsdMicros, UuidV1,
};
use serde_json::json;

fn refusal(code: ErrorCodeV1) -> DispatchError {
    DispatchError::new(code)
}

fn zero_digest() -> Digest {
    Digest::try_from(format!("sha256:{}", "0".repeat(64))).expect("a zero digest is canonical")
}

fn ident(value: &str) -> Result<Ident, DispatchError> {
    Ident::try_from(value.to_string()).map_err(|_| refusal(ErrorCodeV1::InvalidRequest))
}

fn principal_binding_digest(snapshot: &AdmissionSnapshotV1) -> Digest {
    material_digest_value(
        CONNECTION_DISPATCH_DOMAIN,
        &serde_json::to_value(&snapshot.principal).expect("a principal binding serializes"),
    )
}

struct OperationFacts {
    reservation: UsdMicros,
    metered: bool,
    model: Option<crate::connection_dispatch::primitives::ModelId>,
    quote_id: Option<UuidV1>,
    approval_id: Option<Ident>,
    input_digest: Digest,
    payload_digest: Digest,
    destination_digest: Digest,
}

fn required_capability<'a>(
    snapshot: &'a AdmissionSnapshotV1,
    invoke: &ConnectionInvokeV1,
) -> Result<&'a crate::connection_dispatch::dto::CapabilityV1, DispatchError> {
    let capability = snapshot
        .manifest
        .capabilities
        .iter()
        .find(|capability| capability.capability_id == invoke.capability_id)
        .ok_or_else(|| refusal(ErrorCodeV1::CapabilityUnavailable))?;
    if capability.operation != invoke.operation.kind()
        || capability.connection_scope != snapshot.connection.connection_id
    {
        return Err(refusal(ErrorCodeV1::CapabilityUnavailable));
    }
    Ok(capability)
}

fn smtp_destination(
    snapshot: &AdmissionSnapshotV1,
) -> Result<(&ResolvedDestinationV1, String), DispatchError> {
    let destination = &snapshot.resolved.destination;
    crate::connection_dispatch::dto::validate_resolved_destination_v1(destination)?;
    let label = destination
        .smtp_label()
        .ok_or_else(|| refusal(ErrorCodeV1::DestinationRefused))?;
    if snapshot.policy.allowed_destinations.is_empty()
        || !snapshot
            .policy
            .allowed_destinations
            .iter()
            .any(|allowed| allowed.as_str() == label.as_str())
    {
        return Err(refusal(ErrorCodeV1::DestinationRefused));
    }
    Ok((destination, label))
}

fn text_operation(
    snapshot: &AdmissionSnapshotV1,
    invoke: &ConnectionInvokeV1,
    operation: &crate::connection_dispatch::dto::TextGenerateV1,
    quote: Option<&crate::connection_dispatch::dto::ModelQuoteV1>,
) -> Result<OperationFacts, DispatchError> {
    if snapshot.connection.provider_kind
        != crate::connection_dispatch::dto::ProviderKindV1::Openrouter
    {
        return Err(refusal(ErrorCodeV1::OperationNotGranted));
    }
    let quote = quote.ok_or_else(|| refusal(ErrorCodeV1::PricingUnavailable))?;
    crate::connection_dispatch::dto::validate_model_quote_v1(quote)?;
    if quote.connection_id != snapshot.connection.connection_id
        || quote.credential_revision != snapshot.connection.credential_revision
        || quote.authorization_revision != snapshot.connection.authorization_revision
        || quote.policy_revision != snapshot.policy.policy_revision
        || quote.provider_profile != snapshot.policy.provider_profile
        || quote.model != operation.model
        || quote.quote_id != operation.quote_id
        || quote.max_cost_micros != operation.max_cost_micros
    {
        return Err(refusal(ErrorCodeV1::PriceChanged));
    }
    if quote.max_output_bound.get() < u64::from(operation.max_output_tokens.get()) {
        return Err(refusal(ErrorCodeV1::PricingUnavailable));
    }
    if snapshot.policy.allowed_models.is_empty()
        || !snapshot
            .policy
            .allowed_models
            .iter()
            .any(|allowed| allowed == &operation.model)
    {
        return Err(refusal(ErrorCodeV1::OperationNotGranted));
    }
    let grant = snapshot
        .grants
        .iter()
        .find(|grant| grant.agent_uid == snapshot.requester.agent_uid)
        .ok_or_else(|| refusal(ErrorCodeV1::OperationNotGranted))?;
    pi_model_decision(grant.role, &operation.model, snapshot.role_policy.as_ref())
        .map_err(|_| refusal(ErrorCodeV1::OperationNotGranted))?;
    let scope = snapshot
        .policy
        .data_scope(&operation.data_scope_id)
        .ok_or_else(|| refusal(ErrorCodeV1::DataPolicyRefused))?;
    match scope.source {
        crate::connection_dispatch::dto::DataScopeSourceV1::CallerText => {
            if !scope.allow_caller_text {
                return Err(refusal(ErrorCodeV1::DataPolicyRefused));
            }
        }
        crate::connection_dispatch::dto::DataScopeSourceV1::ImmutableSource => {
            let digest = messages_digest(&operation.messages);
            if scope.content_digest.as_ref() != Some(&digest) {
                return Err(refusal(ErrorCodeV1::DataPolicyRefused));
            }
        }
    }
    crate::connection_dispatch::dto::validate_resolved_destination_v1(
        &snapshot.resolved.destination,
    )?;
    let endpoint = snapshot
        .resolved
        .destination
        .https_endpoint()
        .ok_or_else(|| refusal(ErrorCodeV1::DestinationRefused))?;
    if !quote
        .endpoints
        .iter()
        .any(|allowed| allowed.as_str() == endpoint.as_str())
    {
        return Err(refusal(ErrorCodeV1::DestinationRefused));
    }
    let reservation = reserve_usd_micros(
        quote.context_bound.get(),
        quote.price.prompt_pico_per_token,
        u64::from(operation.max_output_tokens.get()),
        quote.price.completion_pico_per_token,
        quote.price.request_pico,
    )
    .map_err(|_| refusal(ErrorCodeV1::PricingUnavailable))?;
    if reservation.get() > operation.max_cost_micros.get() {
        return Err(refusal(ErrorCodeV1::SpendLimit));
    }
    let input_digest = material_digest_value(
        CONNECTION_DISPATCH_DOMAIN,
        &json!({"kind": "input@1", "operation": operation_material(invoke)}),
    );
    Ok(OperationFacts {
        reservation,
        metered: true,
        model: Some(operation.model.clone()),
        quote_id: Some(quote.quote_id.clone()),
        approval_id: None,
        input_digest,
        payload_digest: messages_digest(&operation.messages),
        destination_digest: destination_digest_for(&snapshot.resolved.destination),
    })
}

fn smtp_send_operation(
    snapshot: &AdmissionSnapshotV1,
    invoke: &ConnectionInvokeV1,
    operation: &crate::connection_dispatch::dto::SmtpSendV1,
    principal_digest: &Digest,
) -> Result<OperationFacts, DispatchError> {
    if snapshot.connection.provider_kind != crate::connection_dispatch::dto::ProviderKindV1::Smtp {
        return Err(refusal(ErrorCodeV1::OperationNotGranted));
    }
    let destination = &snapshot.resolved.destination;
    smtp_destination(snapshot)?;
    let approval = snapshot
        .approval
        .as_ref()
        .ok_or_else(|| refusal(ErrorCodeV1::ApprovalRequired))?;
    if approval.approval_id != operation.approved_send_id
        || approval.connection_id != snapshot.connection.connection_id
        || approval.credential_revision != snapshot.connection.credential_revision
        || approval.authorization_revision != snapshot.connection.authorization_revision
        || approval.policy_revision != snapshot.policy.policy_revision
        || approval.policy_digest != snapshot.policy_digest
        || approval.principal_digest != *principal_digest
    {
        return Err(refusal(ErrorCodeV1::ApprovalRequired));
    }
    if approval.consumed_dispatch_id.is_some() {
        return Err(refusal(ErrorCodeV1::ApprovalRequired));
    }
    if approval.body_digest != snapshot.resolved.provider_body_digest {
        return Err(refusal(ErrorCodeV1::DigestMismatch));
    }
    let destination_digest = destination_digest_for(destination);
    if approval.destination_digest != destination_digest {
        return Err(refusal(ErrorCodeV1::DestinationRefused));
    }
    let input_digest = material_digest_value(
        CONNECTION_DISPATCH_DOMAIN,
        &json!({"kind": "input@1", "operation": operation_material(invoke)}),
    );
    Ok(OperationFacts {
        reservation: UsdMicros::zero(),
        metered: false,
        model: None,
        quote_id: None,
        approval_id: Some(approval.approval_id.clone()),
        input_digest,
        payload_digest: approval.body_digest.clone(),
        destination_digest,
    })
}

fn human_smtp_operation(
    snapshot: &AdmissionSnapshotV1,
    operation_kind: OperationKindV1,
    record_id: &str,
    record: Option<&HumanActionRecordV1>,
    operation_material_value: serde_json::Value,
) -> Result<OperationFacts, DispatchError> {
    if snapshot.connection.provider_kind != ProviderKindV1::Smtp {
        return Err(refusal(ErrorCodeV1::UnsupportedVerification));
    }
    let (destination, _label) = smtp_destination(snapshot)?;
    let record = record.ok_or_else(|| refusal(ErrorCodeV1::RequestNotReady))?;
    if record.kind != operation_kind
        || record.record_id.as_str() != record_id
        || record.connection_id != snapshot.connection.connection_id
        || record.credential_revision != snapshot.connection.credential_revision
        || record.authorization_revision != snapshot.connection.authorization_revision
        || record.policy_revision != snapshot.policy.policy_revision
        || record.body_digest != snapshot.resolved.provider_body_digest
        || record.destination_digest != destination_digest_for(destination)
    {
        return Err(refusal(ErrorCodeV1::RequestChanged));
    }
    Ok(OperationFacts {
        reservation: UsdMicros::zero(),
        metered: false,
        model: None,
        quote_id: None,
        approval_id: None,
        input_digest: material_digest_value(
            CONNECTION_DISPATCH_DOMAIN,
            &json!({"kind": "input@1", "operation": operation_material_value}),
        ),
        payload_digest: material_digest_value(
            CONNECTION_DISPATCH_DOMAIN,
            &json!({"kind": "payload@1", "payload": operation_material_value}),
        ),
        destination_digest: destination_digest_for(destination),
    })
}

fn check_limits(
    snapshot: &AdmissionSnapshotV1,
    reservation: UsdMicros,
    metered: bool,
) -> Result<(), DispatchError> {
    let now = snapshot.now_ms.get();
    let lower = now.saturating_sub(RATE_WINDOW_MS);
    for (limits, exposure) in [
        (&snapshot.limits.principal, &snapshot.exposure.principal),
        (&snapshot.limits.account, &snapshot.exposure.account),
        (&snapshot.limits.workspace, &snapshot.exposure.workspace),
    ] {
        let in_window = exposure
            .recent_request_ms
            .iter()
            .filter(|timestamp| timestamp.get() > lower && timestamp.get() <= now)
            .count() as u64;
        let next = in_window
            .checked_add(1)
            .ok_or_else(|| refusal(ErrorCodeV1::RateLimit))?;
        if next > limits.requests_per_60s.get() {
            return Err(refusal(ErrorCodeV1::RateLimit));
        }
        let unresolved = exposure
            .unresolved
            .get()
            .checked_add(1)
            .ok_or_else(|| refusal(ErrorCodeV1::RateLimit))?;
        if unresolved > limits.unresolved.get() {
            return Err(refusal(ErrorCodeV1::RateLimit));
        }
        if !metered {
            continue;
        }
        if reservation.get() > limits.per_call_micros.get() {
            return Err(refusal(ErrorCodeV1::SpendLimit));
        }
        let daily = exposure
            .daily_settled_micros
            .checked_add(exposure.unresolved_reserved_micros)
            .and_then(|used| used.checked_add(reservation))
            .map_err(|_| refusal(ErrorCodeV1::SpendLimit))?;
        if daily.get() > limits.daily_micros.get() {
            return Err(refusal(ErrorCodeV1::SpendLimit));
        }
        let lifetime = exposure
            .lifetime_settled_micros
            .checked_add(exposure.unresolved_reserved_micros)
            .and_then(|used| used.checked_add(reservation))
            .map_err(|_| refusal(ErrorCodeV1::SpendLimit))?;
        if lifetime.get() > limits.lifetime_micros.get() {
            return Err(refusal(ErrorCodeV1::SpendLimit));
        }
    }
    Ok(())
}

pub fn plan_connection_dispatch_v1(
    snapshot: &AdmissionSnapshotV1,
    request: AdmissionRequestV1,
    quote: Option<&crate::connection_dispatch::dto::ModelQuoteV1>,
) -> Result<PlanOutcomeV1, DispatchError> {
    crate::connection_dispatch::dto::validate_admission_snapshot_v1(snapshot)?;
    request.validate()?;
    let kind = request.kind();
    let connection = &snapshot.connection;

    if request.connection_id() != &connection.connection_id
        || request.expected_credential_revision() != connection.credential_revision
        || request.expected_authorization_revision() != connection.authorization_revision
    {
        return Err(refusal(ErrorCodeV1::StaleRevision));
    }
    if !connection.enabled || !snapshot.policy.active {
        return Err(refusal(ErrorCodeV1::OperationNotGranted));
    }
    if kind.is_human_only() {
        if snapshot.principal.kind != PrincipalKindV1::Human {
            return Err(refusal(ErrorCodeV1::ManagementForbidden));
        }
        if !snapshot.policy.allowed_human_operations.contains(&kind) {
            return Err(refusal(ErrorCodeV1::OperationNotGranted));
        }
    } else {
        if snapshot.principal.kind != PrincipalKindV1::Agent {
            return Err(refusal(ErrorCodeV1::ManagementForbidden));
        }
        if !snapshot.policy.allowed_operations.contains(&kind) {
            return Err(refusal(ErrorCodeV1::OperationNotGranted));
        }
    }

    // Agent authority: exactly one active grant for the stable UID. Aliases
    // never authorize. Human-only operations carry no UID grant: their
    // authority is the claimed human record plus the explicit human policy.
    let grant = if kind.is_human_only() {
        None
    } else {
        let mut matching = snapshot
            .grants
            .iter()
            .filter(|grant| grant.agent_uid == snapshot.requester.agent_uid);
        let Some(grant) = matching.next() else {
            return Err(refusal(ErrorCodeV1::OperationNotGranted));
        };
        if matching.next().is_some() || grant.state != GrantStateV1::Active {
            return Err(refusal(ErrorCodeV1::OperationNotGranted));
        }
        Some(grant)
    };
    match (
        &snapshot.principal.binding,
        &connection.exchange,
        snapshot.runtime.custody_epoch,
    ) {
        (
            crate::connection_dispatch::dto::RuntimeBindingV1::Native(native),
            Some(_exchange),
            None,
        ) => {
            if grant.is_some_and(|grant| !grant.allowed_subject_ids.is_empty()) {
                return Err(refusal(ErrorCodeV1::OperationNotGranted));
            }
            if native.owner_generation != snapshot.runtime.owner_epoch
                || native.agent_generation != snapshot.requester.runtime_generation
            {
                return Err(refusal(ErrorCodeV1::PlacementChanged));
            }
            if snapshot.principal.native_session.is_none() {
                return Err(refusal(ErrorCodeV1::PrincipalRequired));
            }
        }
        (
            crate::connection_dispatch::dto::RuntimeBindingV1::Hosted(hosted),
            None,
            Some(custody_epoch),
        ) => {
            if hosted.custody_epoch != custody_epoch
                || hosted.placement_epoch != snapshot.runtime.owner_epoch
            {
                return Err(refusal(ErrorCodeV1::PlacementChanged));
            }
            if grant.is_some_and(|grant| {
                grant.allowed_subject_ids.is_empty()
                    || !grant
                        .allowed_subject_ids
                        .iter()
                        .any(|subject| subject == &snapshot.principal.principal_id)
            }) {
                return Err(refusal(ErrorCodeV1::OperationNotGranted));
            }
            if snapshot.principal.session_authority_ref.is_none() {
                return Err(refusal(ErrorCodeV1::PrincipalRequired));
            }
        }
        _ => return Err(refusal(ErrorCodeV1::LoadedStateMismatch)),
    }

    if let AdmissionRequestV1::Agent(invoke) = &request {
        if invoke.snapshot_id != snapshot.snapshot_id
            || invoke.loaded_receipt_id != snapshot.runtime.loaded_receipt_id
        {
            return Err(refusal(ErrorCodeV1::LoadedStateMismatch));
        }
        if !kind.is_human_only() {
            required_capability(snapshot, invoke)?;
        }
    }

    let principal_digest = principal_binding_digest(snapshot);
    let facts = match (&request, quote) {
        (AdmissionRequestV1::Agent(invoke), _) => match &invoke.operation {
            AgentOperationV1::TextGenerate(operation) => {
                text_operation(snapshot, invoke, operation, quote)?
            }
            AgentOperationV1::SmtpSend(operation) => {
                smtp_send_operation(snapshot, invoke, operation, &principal_digest)?
            }
            AgentOperationV1::SmtpTestSend(operation) => {
                let request_id = operation.test_request_id.clone();
                human_smtp_operation(
                    snapshot,
                    OperationKindV1::SmtpTestSend,
                    request_id.as_str(),
                    snapshot.test_send_record.as_ref(),
                    json!({"kind": "smtp.test_send@1", "test_request_id": request_id}),
                )?
            }
        },
        (AdmissionRequestV1::HumanSmtpVerify(verify), _) => {
            let request_id = verify.verification_request_id.clone();
            human_smtp_operation(
                snapshot,
                OperationKindV1::SmtpVerify,
                request_id.as_str(),
                snapshot.verify_record.as_ref(),
                json!({"kind": "smtp.verify@1", "verification_request_id": request_id}),
            )?
        }
    };
    let capability_id = match &request {
        AdmissionRequestV1::Agent(invoke) => invoke.capability_id.clone(),
        AdmissionRequestV1::HumanSmtpVerify(_) => ident("human.smtp.verify@1")?,
    };
    let mut commit = DispatchCommitV1 {
        v: crate::connection_dispatch::dto::CONNECTION_CONTRACT_VERSION,
        dispatch_id: snapshot.dispatch_id.clone(),
        request_id: snapshot.request_id.clone(),
        state: DispatchStateV1::Committed,
        workspace_id: connection.workspace_id.clone(),
        custody_domain: connection.custody_domain.clone(),
        connection_id: connection.connection_id.clone(),
        provider_kind: connection.provider_kind,
        connection_incarnation: connection.incarnation.clone(),
        credential_revision: connection.credential_revision,
        authorization_revision: connection.authorization_revision,
        policy_revision: connection.policy_revision,
        idempotency_key: ident(request.idempotency_key())?,
        idempotency_digest: zero_digest(),
        principal_binding_digest: principal_digest.clone(),
        snapshot_id: snapshot.snapshot_id.clone(),
        loaded_receipt_id: snapshot.runtime.loaded_receipt_id.clone(),
        manifest_digest: snapshot.manifest.material_digest.clone(),
        observed_registry_digest: snapshot.observed_registry_digest.clone(),
        runtime_generation: snapshot.runtime.runtime_generation,
        enrollment_id: snapshot.runtime.enrollment_id.clone(),
        capability_id,
        operation_kind: kind,
        model: facts.model,
        input_digest: facts.input_digest,
        payload_digest: facts.payload_digest,
        body_digest: snapshot.resolved.provider_body_digest.clone(),
        destination_digest: facts.destination_digest,
        quote_id: facts.quote_id,
        approval_id: facts.approval_id,
        request_timestamp_ms: snapshot.now_ms,
        database_epoch: snapshot.database_epoch,
        rate_dimensions: crate::connection_dispatch::dto::RateDimensionsV1 {
            principal_id: snapshot.principal.principal_id.clone(),
            account_id: connection.account.clone(),
            workspace_id: connection.workspace_id.clone(),
            window_ms: Counter::new(RATE_WINDOW_MS)
                .map_err(|_| refusal(ErrorCodeV1::InvalidRequest))?,
        },
        reserved_micros: facts.reservation,
        frozen_quote: if kind == OperationKindV1::TextGenerate {
            quote.cloned()
        } else {
            None
        },
        frozen_identity: FrozenCredentialIdentityV1 {
            workspace_id: connection.workspace_id.clone(),
            custody_domain: connection.custody_domain.clone(),
            connection_id: connection.connection_id.clone(),
            provider: ident(connection.provider_kind.as_str())?,
            account: connection.account.clone(),
            credential_revision: connection.credential_revision,
            key_id: connection.key_id.clone(),
            exchange: connection.exchange.clone(),
        },
        commitment_digest: zero_digest(),
        ledger_sequence: Counter::zero(),
        ledger_digest: zero_digest(),
    };
    let idempotency =
        material_digest_value(CONNECTION_DISPATCH_DOMAIN, &commit.idempotency_material());
    commit.idempotency_digest = idempotency.clone();
    if let Some(existing) = &snapshot.existing {
        if existing.idempotency_digest == idempotency {
            // Status only: a duplicate consumes no rate and no money.
            return Ok(PlanOutcomeV1::Duplicate(ExistingDispatchStatusV1 {
                dispatch_id: existing.dispatch_id.clone(),
                request_id: existing.request_id.clone(),
                state: existing.state,
                commitment_digest: existing.commitment_digest.clone(),
                payload_digest: existing.payload_digest.clone(),
                reserved_micros: existing.reserved_micros,
            }));
        }
        return Err(refusal(ErrorCodeV1::IdempotencyConflict));
    }
    if let (OperationKindV1::TextGenerate, Some(quote)) = (kind, quote) {
        if snapshot.now_ms.get() >= quote.metadata_expires_ms.get()
            || snapshot.now_ms.get() < quote.metadata_observed_ms.get()
        {
            return Err(refusal(ErrorCodeV1::PriceChanged));
        }
    }
    check_limits(snapshot, facts.reservation, facts.metered)?;
    // No fabricated journal head: the adapter finalizes the immutable binding
    // against the ACTUAL transaction head (PreparedDispatchV1::finalize).
    Ok(PlanOutcomeV1::Prepared(PreparedDispatchV1::new(
        commit,
        snapshot.ledger_head.clone(),
    )))
}

/// The frozen body bytes and credential identity must match the committed row
/// exactly before a trusted adapter materializes the credential. This is a
/// data check; the adapter's own permit authorizes the provider call.
pub fn verify_frozen_dispatch_v1(
    commit: &DispatchCommitV1,
    body: &[u8],
    credential_identity: &FrozenCredentialIdentityV1,
) -> Result<(), DispatchError> {
    validate_commit_v1(commit)?;
    if &commit.frozen_identity != credential_identity {
        return Err(refusal(ErrorCodeV1::DigestMismatch));
    }
    if exact_byte_digest(body) != commit.body_digest.as_str() {
        return Err(refusal(ErrorCodeV1::DigestMismatch));
    }
    Ok(())
}

struct SettledFacts {
    state: DispatchStateV1,
    actual: Option<UsdMicros>,
    error: Option<ReceiptErrorV1>,
}

fn model_result(
    commit: &DispatchCommitV1,
    outcome: &ValidatedOutcomeV1,
) -> Result<SettledFacts, DispatchError> {
    let result = outcome
        .result
        .as_ref()
        .ok_or_else(|| refusal(ErrorCodeV1::InvalidRequest))?;
    let usage = outcome
        .usage
        .as_ref()
        .ok_or_else(|| refusal(ErrorCodeV1::InvalidRequest))?;
    let quote = commit
        .frozen_quote
        .as_ref()
        .ok_or_else(|| refusal(ErrorCodeV1::PricingUnavailable))?;
    match result {
        OutcomeResultV1::TextGenerated(text) => {
            if Some(&text.model) != commit.model.as_ref() {
                return Err(refusal(ErrorCodeV1::StateConflict));
            }
            let actual = reserve_usd_micros(
                usage.input_tokens.get(),
                quote.price.prompt_pico_per_token,
                usage.output_tokens.get(),
                quote.price.completion_pico_per_token,
                quote.price.request_pico,
            )
            .map_err(|_| refusal(ErrorCodeV1::PricingUnavailable))?;
            if actual.get() > commit.reserved_micros.get() {
                Ok(SettledFacts {
                    state: DispatchStateV1::Overrun,
                    actual: Some(actual),
                    error: Some(ReceiptErrorV1::fixed(ErrorCodeV1::ReservationOverrun)),
                })
            } else {
                Ok(SettledFacts {
                    state: DispatchStateV1::Done,
                    actual: Some(actual),
                    error: None,
                })
            }
        }
        _ => Err(refusal(ErrorCodeV1::StateConflict)),
    }
}

/// Settle the one claimed dispatch. A settlement can win only once; a lost or
/// uncertain outcome stays reserved and is never resent or refunded here.
pub fn settle_connection_dispatch_v1(
    commit: &DispatchCommitV1,
    outcome: ValidatedOutcomeV1,
) -> Result<SettlementV1, DispatchError> {
    validate_commit_v1(commit)?;
    outcome.validate()?;

    let mut usage = None;
    let mut result = None;
    let mut error = None;
    let (state, actual) = match outcome.kind {
        OutcomeKindV1::Unknown => (DispatchStateV1::Unknown, None),
        OutcomeKindV1::Refused => {
            let code = outcome
                .refused_code
                .ok_or_else(|| refusal(ErrorCodeV1::InvalidRequest))?;
            error = Some(ReceiptErrorV1::fixed(code));
            (DispatchStateV1::Refused, Some(UsdMicros::zero()))
        }
        OutcomeKindV1::Done => match commit.operation_kind {
            OperationKindV1::TextGenerate => {
                let facts = model_result(commit, &outcome)?;
                usage = outcome.usage.clone();
                if let Some(OutcomeResultV1::TextGenerated(text)) = &outcome.result {
                    result = Some(ReceiptResultV1::TextGenerated(TextGeneratedV1 {
                        kind: text.kind,
                        model: text.model.clone(),
                        text: text.text.clone(),
                    }));
                }
                error = facts.error;
                (facts.state, facts.actual)
            }
            OperationKindV1::SmtpSend | OperationKindV1::SmtpTestSend => {
                match outcome.result.as_ref() {
                    Some(OutcomeResultV1::SmtpAccepted(accepted)) if accepted.accepted => {}
                    _ => return Err(refusal(ErrorCodeV1::StateConflict)),
                }
                result = Some(ReceiptResultV1::SmtpAccepted(SmtpAcceptedV1 {
                    kind: crate::connection_dispatch::dto::SmtpAcceptedKindV1::SmtpAccepted,
                    accepted: true,
                    claim: SmtpAcceptanceClaimV1::ServerAcceptanceOnly,
                }));
                (DispatchStateV1::Done, Some(UsdMicros::zero()))
            }
            OperationKindV1::SmtpVerify => {
                match outcome.result.as_ref() {
                    Some(OutcomeResultV1::SmtpVerified(verified)) if verified.verified => {}
                    _ => return Err(refusal(ErrorCodeV1::StateConflict)),
                }
                result = Some(ReceiptResultV1::SmtpVerified(SmtpVerifiedV1 {
                    kind: crate::connection_dispatch::dto::SmtpVerifiedKindV1::SmtpVerified,
                    verified: true,
                    claim: SmtpVerificationClaimV1::AuthenticationOnly,
                }));
                (DispatchStateV1::Done, Some(UsdMicros::zero()))
            }
        },
    };
    let usage = if matches!(state, DispatchStateV1::Done | DispatchStateV1::Overrun) {
        usage
    } else {
        None
    };
    let receipt = DispatchReceiptV1 {
        v: crate::connection_dispatch::dto::CONNECTION_CONTRACT_VERSION,
        dispatch_id: commit.dispatch_id.clone(),
        request_id: commit.request_id.clone(),
        payload_digest: commit.payload_digest.clone(),
        commitment_digest: commit.commitment_digest.clone(),
        state,
        result,
        error,
        cost: ReceiptCostV1 {
            currency: CurrencyV1::Usd,
            reserved_micros: commit.reserved_micros,
            actual_micros: actual,
        },
        usage,
    };
    receipt.validate()?;
    Ok(SettlementV1 {
        v: crate::connection_dispatch::dto::CONNECTION_CONTRACT_VERSION,
        fence: state == DispatchStateV1::Overrun,
        receipt,
    })
}

/// Longest quote age the planner accepts when a durable clock floor is used.
pub fn quote_age_is_current(
    now: EpochMs,
    observed: EpochMs,
    expires: EpochMs,
    clock_floor: EpochMs,
) -> bool {
    now.at_or_after_floor(clock_floor) && now.get() >= observed.get() && now.get() < expires.get()
}
