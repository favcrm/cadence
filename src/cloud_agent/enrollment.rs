//! CAD-1853 Decision A: explicit cloud-purpose enrollment of ONE local peer.
//!
//! The bridge `hct_` credential (the explicitly consented human login that
//! carries `bridge.enroll` + `cloud_agent.read/write`) enrolls a child whose
//! immutable sender is the peer's local `agents.uid` + registration stamp.
//! The child credential is requested as a strict subset (only the cloud-agent
//! scopes), validated against the response, and stored only in the daemon's
//! owner-only per-source file. It is never placed in a child environment,
//! prompt, argv or log, and never handed to a managed runtime.
//!
//! The daemon separately proves the target peer's real split UID/pid-start
//! before calling this; a same-UID daemon refuses earlier.

use serde_json::json;

use crate::error::{Error, Result};
use crate::remote_cli::RemoteTarget;
use crate::store::cloud_agent::{CloudSource, SOURCE_ACTIVE, SOURCE_PEER};
use crate::store::{Agent, CloudAgent, Store};

pub const SCOPES: [&str; 2] = ["cloud_agent.read", "cloud_agent.write"];

fn token_ok(value: &str) -> bool {
    value.strip_prefix("hct_").is_some_and(|suffix| {
        suffix.len() == 43
            && suffix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    })
}

/// Enroll `peer` against `target` using the saved bridge login and record the
/// explicit source grant. The caller has already proved split-UID/pid-start
/// and the bridge's live scopes.
pub fn enroll_peer(
    store: &Store,
    target: &RemoteTarget,
    agent: &CloudAgent,
    peer: &Agent,
    granted_by: &str,
) -> Result<CloudSource> {
    let epoch = crate::cloud_agent::registration_epoch(peer.created);
    let body = json!({
        "version": "hosted-cadence-auth.v1",
        "organization_id": target.org_id,
        "audience": target.endpoint,
        "client_label": "Cadence local team",
        "agents": [{
            "client_agent_id": peer.uid,
            "role": "cloud_agent",
            "purpose": "cloud_agent",
            "sender": {"uid": peer.uid, "registration_epoch": epoch},
            "requested_capabilities": SCOPES,
        }],
    });
    // ONE verified parent snapshot performs the enrollment POST; it is the
    // owner this child is saved under, so a login replaced mid-request can
    // neither enroll nor record the child under a different human.
    let (status, body, pin) = crate::remote_cli::cloud_agent_enroll_post(target, &body)?;
    let owner = crate::store::cloud_agent::Owner::new(pin.issuer, pin.subject_id);
    let body = body.ok_or_else(|| {
        Error::unknown_coded(
            "cloud_agent_peer_enrollment_uncertain",
            "cloud-peer enrollment answered without a JSON body",
        )
    })?;
    if status != 200 {
        let code = body["error"]["code"].as_str().unwrap_or("refused");
        return Err(match (status, code) {
            (400, _) => Error::invalid(
                "cloud_agent_peer_enrollment_refused",
                "cloud-peer enrollment request was refused as invalid",
            ),
            (401, _) | (403, _) => Error::gate_coded(
                "cloud_agent_peer_enrollment_refused",
                "the bridge login may not enroll this cloud peer (credential/scope/membership)",
            ),
            _ => Error::unknown_coded(
                "cloud_agent_peer_enrollment_uncertain",
                "cloud-peer enrollment outcome is unknown",
            ),
        });
    }
    if body["organization_id"] != target.org_id || body["audience"] != target.endpoint {
        return Err(Error::unknown_coded(
            "cloud_agent_peer_enrollment_uncertain",
            "cloud-peer enrollment answered for a different tenant or audience",
        ));
    }
    let children = body["agents"]
        .as_array()
        .filter(|a| a.len() == 1)
        .ok_or_else(|| {
            Error::unknown_coded(
                "cloud_agent_peer_enrollment_uncertain",
                "cloud-peer enrollment returned no single child",
            )
        })?;
    let child = &children[0];
    let credential = &child["credential"];
    let caps_ok = child["capabilities"].as_array().is_some_and(|caps| {
        caps.len() == SCOPES.len()
            && SCOPES
                .iter()
                .all(|scope| caps.iter().any(|cap| cap.as_str() == Some(*scope)))
    });
    let echoed_uid = child["sender"]["uid"].as_str();
    let sender = &child["sender"];
    let sender_ok = match (echoed_uid, sender["registration_epoch"].as_i64()) {
        (Some(uid), Some(stamp)) => uid == peer.uid && stamp == epoch,
        _ => false,
    };
    if child["principal_subject_id"].as_str() != Some(owner.subject.as_str()) {
        return Err(Error::gate_coded(
            "cloud_agent_peer_enrollment_refused",
            "the enrolled child is bound to a different parent subject than the verified \
             enrolling owner; no custody was stored",
        ));
    }
    if child["role"].as_str() != Some("cloud_agent")
        || child["purpose"].as_str() != Some("cloud_agent")
        || child["client_agent_id"].as_str() != Some(peer.uid.as_str())
        || child["organization_id"] != target.org_id
        || child["audience"] != target.endpoint
        || !sender_ok
        || !caps_ok
        || !token_ok(credential["access_token"].as_str().unwrap_or(""))
        || credential["token_type"].as_str() != Some("Bearer")
        || credential["expires_at"].as_u64().unwrap_or(0) == 0
        || credential["expires_at"].as_u64() <= Some(crate::store::now_public() as u64)
    {
        return Err(Error::unknown_coded(
            "cloud_agent_peer_enrollment_uncertain",
            "enrollment response did not match the exact requested cloud peer/sender/scopes",
        ));
    }
    let record = json!({
        "agent_id": child["agent_id"],
        "credential_id": credential["credential_id"],
        "token": credential["access_token"],
        "expires_at": credential["expires_at"],
        "role": child["role"],
        "sender": {"uid": peer.uid, "registration_epoch": epoch},
    });
    crate::remote_cli::cloud_agent_save_peer(target, &owner, &peer.uid, epoch, &record)?;
    let source = CloudSource {
        cloud_agent_id: agent.id.clone(),
        kind: SOURCE_PEER.to_string(),
        uid: peer.uid.clone(),
        registration_epoch: epoch,
        owner,
        label: peer.alias.clone(),
        scope: crate::store::CLOUD_AGENT_CONSENT_SCOPE.to_string(),
        granted_by: granted_by.to_string(),
        granted_at: crate::store::now_public(),
        invoker_id: None,
        state: SOURCE_ACTIVE.to_string(),
    };
    store.cloud_agent_source_grant(&source)?;
    Ok(source)
}
