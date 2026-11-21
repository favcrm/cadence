//! Remote operation lifecycle, readback, cancellation, and canonical
//! queue/finish helpers for `cloud_agent`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use serde_json::json;

use crate::error::{Error, Result};
use crate::remote_cli::RemoteTarget;
use crate::store::{CloudAgent, CloudOperation, Store};

use super::{
    discover_source, target_for_source, CloudTransport, RemoteOperation, RemoteStatus, Sender,
    Source,
};

pub(super) fn apply_remote(
    store: &Store,
    agent: &CloudAgent,
    operation: &CloudOperation,
    outcome: &RemoteOperation,
) -> Result<CloudOperation> {
    if let Some(session) = outcome.remote_session_id.as_deref() {
        if !operation.remote_session_id.is_empty() && operation.remote_session_id != session {
            store.cloud_agent_operation_set(
                &operation.cloud_agent_id,
                &operation.operation_id,
                crate::store::cloud_agent::STATUS_UNKNOWN,
                outcome.remote_operation_id.as_deref(),
                None,
                None,
                Some((
                    "cloud_agent_session_mismatch",
                    "the contract answered with a different provider session than this \
                     source's work conversation is bound to; no reply was recorded and \
                     the outcome stays unknown",
                )),
            )?;
            return Err(Error::unknown_coded(
                "cloud_agent_session_mismatch",
                "the contract answered with a different provider session than this \
                 source's work conversation is bound to; no reply was recorded",
            ));
        }
        store.cloud_agent_bind_session(
            &agent.id,
            &operation.source_kind,
            &operation.source_uid,
            operation.registration_epoch,
            &operation.owner,
            &operation.thread_id,
            &operation.conversation_id,
            session,
            outcome.provider_session_id.as_deref(),
            outcome.invoker_id.as_deref(),
        )?;
    } else if let Some(invoker) = outcome.invoker_id.as_deref() {
        store.cloud_agent_source_pin_invoker(
            &operation.cloud_agent_id,
            &operation.source_kind,
            &operation.source_uid,
            operation.registration_epoch,
            &operation.owner,
            invoker,
        )?;
    }
    let remote_session_id = outcome
        .remote_session_id
        .as_deref()
        .unwrap_or(operation.remote_session_id.as_str());
    if outcome.status == RemoteStatus::Completed {
        let Some(reply) = outcome.reply.as_deref() else {
            store.cloud_agent_operation_set(
                &operation.cloud_agent_id,
                &operation.operation_id,
                crate::store::cloud_agent::STATUS_UNKNOWN,
                outcome.remote_operation_id.as_deref(),
                None,
                None,
                Some((
                    "cloud_agent_reply_missing",
                    "the contract reported completion without assistant text; the turn \
                     stays unknown and is read back — no reply was fabricated",
                )),
            )?;
            return Err(Error::unknown_coded(
                "cloud_agent_reply_missing",
                "the contract reported completion without assistant text — the turn is unknown",
            ));
        };
        return canonical_finish(
            store,
            agent,
            operation,
            crate::store::cloud_agent::STATUS_COMPLETED,
            Some(reply),
            None,
            outcome.remote_operation_id.as_deref(),
            remote_session_id,
        );
    }
    let error = match (outcome.status, &outcome.error) {
        (RemoteStatus::Failed, Some((code, message))) => Some((code.as_str(), message.as_str())),
        (RemoteStatus::Failed, None) => Some((
            "cloud_agent_remote_failed",
            "the cloud runtime reported the turn failed",
        )),
        _ => None,
    };
    if outcome.status == RemoteStatus::Unknown {
        // An intermediate unknown is NOT a final result: no canonical thread
        // result and no deterministic report id are written. The operation
        // stays non-terminal so a later real completion can settle once.
        return canonical_finish(
            store,
            agent,
            operation,
            crate::store::cloud_agent::STATUS_UNKNOWN,
            None,
            Some("cloud_agent_uncertain"),
            outcome.remote_operation_id.as_deref(),
            remote_session_id,
        );
    }
    if let Some((code, message)) = error {
        return canonical_finish(
            store,
            agent,
            operation,
            crate::store::cloud_agent::STATUS_FAILED,
            None,
            Some(code),
            outcome.remote_operation_id.as_deref(),
            remote_session_id,
        )
        .map_err(|finish_error| {
            Error::unknown_coded(
                "cloud_agent_uncertain",
                format!("could not finish {code} ({message}): {finish_error}"),
            )
        });
    }
    store.cloud_agent_operation_set(
        &operation.cloud_agent_id,
        &operation.operation_id,
        outcome.status.as_str(),
        outcome.remote_operation_id.as_deref(),
        None,
        None,
        None,
    )
}

/// Read one operation back. This never invokes the model.
///
/// The live credential/scope is re-checked BEFORE any transport call, and a
/// settled reply is served only AFTER the platform's own readback authorizes
/// it: the `cloud_agent_operations` row is the reconciliation target, never
/// the answer. A revocation that landed upstream therefore refuses a
/// settled read, and an unreachable readback is reported as uncertainty
/// rather than served from the cache. A NONTERMINAL readback is answered
/// with the platform's own state for that read, even when another caller
/// settled the same operation while it was in flight: this read never
/// learned a reply, so it never returns one.
pub fn read_operation(
    store: &Store,
    transport: &dyn CloudTransport,
    agent: &CloudAgent,
    target: &RemoteTarget,
    operation_id: &str,
    source: &Source,
    authorize: impl Fn() -> Result<()>,
) -> Result<CloudOperation> {
    read_operation_with(
        store,
        transport,
        agent,
        target,
        operation_id,
        source,
        &authorize,
    )
}

pub(super) fn read_operation_with(
    store: &Store,
    transport: &dyn CloudTransport,
    agent: &CloudAgent,
    target: &RemoteTarget,
    operation_id: &str,
    source: &Source,
    authorize: &dyn Fn() -> Result<()>,
) -> Result<CloudOperation> {
    let operation = store
        .cloud_agent_operation(&agent.id, operation_id)?
        .ok_or_else(|| {
            Error::invalid(
                "cloud_agent_operation_unknown",
                "no such cloud-agent operation for this agent",
            )
        })?;
    if !source.matches(&operation) {
        return Err(Error::gate_coded(
            "cloud_agent_source_mismatch",
            "this operation belongs to a different invoking source or logical owner; \
             nothing was read",
        ));
    }
    authorize()?;
    // A queue-only operation was never sent (no wake yet): reading it would
    // invent a remote "unknown" the contract never had and poison the later
    // explicit wake. Report the durable local queue state, zero transport.
    // A CLAIMED row that the contract accepted as queued is not queue-only:
    // it crossed the transport boundary, so only the platform can say what
    // became of it.
    if operation.status == crate::store::cloud_agent::STATUS_QUEUED
        && operation.remote_operation_id.is_none()
        && operation.claimed_at <= 0.0
    {
        return Ok(operation);
    }
    let settled_before = operation.settled();
    let target = &target_for_source(target, source);
    let mut outcome = match transport.read(target, &operation.conversation_id, operation_id, source)
    {
        Ok(outcome) => outcome,
        Err(Error::OutcomeUnknown(_)) => {
            // An unreachable readback proves nothing about the platform's
            // state, so it may never be answered from the local receipt:
            // that row can be settled and carry the reply text. A
            // non-terminal row records the uncertainty for the reconciler;
            // a settled receipt is left exactly as it is — the durable
            // terminal receipt is kept for reconciliation, never deleted
            // here and never served as this read's answer.
            if !settled_before {
                let _ = store.cloud_agent_operation_set(
                    &operation.cloud_agent_id,
                    &operation.operation_id,
                    crate::store::cloud_agent::STATUS_UNKNOWN,
                    operation.remote_operation_id.as_deref(),
                    None,
                    None,
                    Some((
                        "cloud_agent_uncertain",
                        "readback was interrupted; the remote outcome is still unknown",
                    )),
                );
            }
            return Err(Error::unknown_coded(
                "cloud_agent_uncertain",
                "readback was unreachable; the remote outcome is still unknown — nothing \
                 was served from the local receipt",
            ));
        }
        Err(error) => return Err(error),
    };
    if operation.remote_session_id.is_empty() && outcome.remote_session_id.is_none() {
        // A first submit may have been accepted without its acknowledgement.
        // Operation-only readback recovers text, not source/session metadata;
        // discover that same conversation with this source's OWN read grant.
        let grant = store
            .cloud_agent_source(
                &agent.id,
                source.kind,
                &source.uid,
                source.epoch,
                &source.owner,
            )?
            .ok_or_else(|| {
                Error::gate_coded(
                    "cloud_agent_authority_missing",
                    "the retained operation has no source authority for metadata recovery",
                )
            })?;
        authorize()?;
        let remote = discover_source(
            transport,
            agent,
            target,
            &operation.conversation_id,
            source,
            grant.invoker_id.as_deref(),
        )?;
        outcome.remote_session_id = Some(remote.remote_session_id);
        outcome.provider_session_id = remote.provider_session_id;
        outcome.invoker_id = Some(remote.invoker_id);
    }
    let terminal = matches!(
        outcome.status,
        RemoteStatus::Completed | RemoteStatus::Failed
    );
    if !terminal {
        // The platform's own answer to THIS read is nonterminal, so that is
        // the state the caller gets, with no reply text in it — whatever the
        // local row says by now. Another read (or the driver) may have
        // settled this operation while this readback was in flight; its
        // durable terminal receipt is kept for reconciliation and canonical
        // history, but it is never this read's answer, because this read
        // never learned that reply, and returning it would disclose content
        // the caller's own authoritative answer did not contain.
        if !settled_before {
            // Record the observed nonterminal state for the reconciler. The
            // settle is a terminal-preserving compare-and-set, so a receipt
            // that settled concurrently stays settled (and stays the
            // reconciliation target) while this read keeps its own answer.
            apply_remote(store, agent, &operation, &outcome)?;
        }
        let mut pending = operation;
        pending.status = outcome.status.as_str().to_string();
        pending.remote_operation_id = outcome.remote_operation_id.or(pending.remote_operation_id);
        if let Some(session) = outcome.remote_session_id {
            pending.remote_session_id = session;
        }
        pending.reply_text = None;
        pending.reply_message_id = None;
        let (error_code, error_message) = match outcome.error {
            Some((code, message)) => (Some(code), Some(message)),
            None => (None, None),
        };
        pending.error_code = error_code;
        pending.error_message = error_message;
        return Ok(pending);
    }
    let answer = apply_remote(store, agent, &operation, &outcome)?;
    if !settled_before {
        // A non-settled row is answered by the readback itself. A settled row
        // that settles here from a concurrent read keeps its terminal
        // receipt (monotonic: the reply is never withdrawn by a late read).
        return Ok(answer);
    }
    if outcome.status.as_str() != operation.status {
        // The platform's terminal answer contradicts the settled receipt.
        // Neither copy may be presented as authorized, so this is an explicit
        // refusal to reconcile by hand — never the cached reply.
        return Err(Error::unknown_coded(
            "cloud_agent_receipt_conflict",
            "the platform's terminal readback disagrees with this operation's settled \
             receipt; the receipt and the canonical history are retained — reconcile this \
             operation explicitly, do not trust either copy",
        ));
    }
    // Same outcome, so the readback's own answer is what the caller gets: it
    // is recorded on the receipt (the reconciliation target) and served in
    // place of a possibly stale cached copy. The receipt's status and the
    // canonical Thread history are not rewritten.
    store.cloud_agent_operation_reconcile(
        &operation.cloud_agent_id,
        &operation.operation_id,
        outcome.remote_operation_id.as_deref(),
        outcome.reply.as_deref(),
        outcome
            .error
            .as_ref()
            .map(|(code, message)| (code.as_str(), message.as_str())),
    )
}

/// Explicit cancellation is unsupported: the contract answers
/// `cancel_unsupported` and no local state is ever rendered as stopped.
pub fn cancel(
    store: &Store,
    transport: &dyn CloudTransport,
    target: &RemoteTarget,
    agent: &CloudAgent,
    operation_id: &str,
    source: &Source,
    authorize: impl Fn() -> Result<()>,
) -> Result<()> {
    let operation = store
        .cloud_agent_operation(&agent.id, operation_id)?
        .ok_or_else(|| {
            Error::invalid(
                "cloud_agent_operation_unknown",
                "no such cloud-agent operation for this agent",
            )
        })?;
    if !source.matches(&operation) {
        return Err(Error::gate_coded(
            "cloud_agent_source_mismatch",
            "this operation belongs to a different invoking source or logical owner; \
             nothing was read",
        ));
    }
    authorize()?;
    let target = &target_for_source(target, source);
    transport.cancel(target, &operation.conversation_id, operation_id, source)
}

/// Enqueue the request as a real canonical `messages` row on the cloud
/// agent's alias. This is the same durable message lifecycle the daemon
/// uses for any worker: sender_uid/recipient_uid feed the cross-agent work
/// conversation projection, and `finish` routes the reply to `reply_to`
/// under its original UID fence.
pub(super) fn canonical_enqueue(
    store: &Store,
    agent: &CloudAgent,
    operation: &CloudOperation,
    sender: &Sender,
    expected_recipient_uid: Option<&str>,
) -> Result<()> {
    let reply_to = match sender {
        Sender::Operator => None,
        Sender::Agent(alias) => Some(alias.as_str()),
    };
    let sender_row = match sender {
        Sender::Operator => crate::store::Sender::Operator,
        Sender::Agent(alias) => crate::store::Sender::Agent(alias.clone()),
    };
    let sender_uid = (operation.source_kind != crate::store::cloud_agent::SOURCE_USER)
        .then_some(operation.source_uid.as_str());
    // One core transaction fences the ORIGINAL source UID + registration
    // stamp and the pinned recipient UID before the row, marks the canonical
    // message running and attaches the operation witness. `expected_uid`
    // is the contextual reply's captured recipient, checked in the same
    // transaction — never a pre-read that could go stale.
    store.enqueue_cloud(
        &agent.alias,
        &operation.request_text,
        reply_to,
        &operation.operation_id,
        &sender_row,
        &operation.recipient_uid,
        expected_recipient_uid,
        sender_uid,
        operation.registration_epoch,
        &agent.id,
    )?;
    Ok(())
}

/// Persist the genuine result and route it in ONE core transaction.
///
/// The canonical message is finished (including its UID-fenced
/// `worker_result`) and the cloud receipt is CAS-settled together, so two
/// concurrent readbacks — or a crash between the two writes — can never
/// duplicate the Thread result or its deterministic report id. An
/// intermediate `unknown` is only a receipt update: it writes no result and
/// consumes no report id, leaving the later real completion able to settle.
#[allow(clippy::too_many_arguments)]
pub(super) fn canonical_finish(
    store: &Store,
    agent: &CloudAgent,
    operation: &CloudOperation,
    status: &str,
    reply: Option<&str>,
    error_code: Option<&str>,
    remote_operation_id: Option<&str>,
    remote_session_id: &str,
) -> Result<CloudOperation> {
    if status == crate::store::cloud_agent::STATUS_UNKNOWN {
        return store.cloud_agent_operation_set(
            &operation.cloud_agent_id,
            &operation.operation_id,
            crate::store::cloud_agent::STATUS_UNKNOWN,
            remote_operation_id,
            None,
            None,
            error_code.map(|code| (code, "the cloud turn's outcome is not known yet")),
        );
    }
    let result = json!({
        "status": status,
        "text": reply,
        "cloud_agent": agent.id,
        "cloud_agent_alias": agent.alias,
        "operation": operation.operation_id,
        "remote_operation_id": remote_operation_id,
        "remote_session_id": remote_session_id,
        "runtime": agent.runtime,
        "model": agent.model,
        "error": error_code,
        "source": {
            "kind": operation.source_kind,
            "uid": operation.source_uid,
            "registration_epoch": operation.registration_epoch,
            "label": operation.sender_label,
            "owner": operation.owner.to_json(),
        },
    });
    store.finish_cloud(
        &operation.cloud_agent_id,
        &operation.operation_id,
        &agent.alias,
        status,
        &result,
        error_code,
        remote_operation_id,
        reply,
    )?;
    store
        .cloud_agent_operation(&operation.cloud_agent_id, &operation.operation_id)?
        .ok_or_else(|| {
            Error::unknown_coded(
                "cloud_agent_uncertain",
                "the settled cloud operation could not be read back",
            )
        })
}
