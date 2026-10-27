//! The supervisor-side client for the root owner control socket.
//!
//! Every call is one strict newline JSON frame with a fresh UUID request id and
//! a closed reply envelope. `RootOwnerClient::connect` performs the r5
//! `hello_v1` handshake: a fresh 256-bit nonce, the supervisor's stable
//! per-process generation, the kernel `SO_PEERCRED` of the accepted root owner,
//! the pinned owner public key and the signed `OwnerPeerProofV1`. Only then
//! does the client hold an `AuthenticatedOwnerPeer`.
//!
//! Every subsequent exchange re-reads the owner's live kernel pid/starttime and
//! requires the owner-echoed epoch to be the authenticated one. A changed or
//! missing peer invalidates the channel and requires a fresh hello — transport
//! failure is an error/refusal, never a synthesized epoch or readiness.
//! There is no caller-injectable owner, socket, key or epoch.

use std::path::PathBuf;

use crate::daemon::supervisor_grant::{
    canonical_challenge_value, ConsumeOutcome, SupervisorChallenge,
};
use crate::error::{Error, Result};

use super::frame;
use super::profile::LocalProfile;
use super::proof;
use super::qualified::{FenceAck, QualifiedRuntime, RuntimeIdentity, RuntimeStatus};
use super::store::LedgerRecordV1;
use super::strict;
use super::types::{PreparedRuntime, StartOutcome};
use super::{CheckpointAck, CheckpointV1, MAX_FRAME_BYTES, REQUEST_BUDGET, RESPONSE_BUDGET};

/// The facts the supervisor sends for one allocation. Agent identity, session
/// binding and the expected owner epoch are server-derived by the caller.
#[derive(Clone, Debug)]
pub(crate) struct PrepareRequest {
    pub(crate) stable_agent_uid: String,
    pub(crate) session_binding: String,
    pub(crate) server_generation: u64,
    pub(crate) expected_owner_epoch: u64,
}

/// The measured owner identity a successful `hello_v1` establishes. Private
/// fields: only [`RootOwnerClient::connect`] constructs one.
struct AuthenticatedOwnerPeer {
    owner_pid: u32,
    owner_starttime: u64,
    owner_epoch: u64,
}

/// Live owner evidence for one runtime, produced only by
/// [`RootOwnerClient::owner_evidence`] from authenticated owner RPCs. Secret
/// free; it carries no snapshot/receipt lifetime because the owner does not
/// mint one — the dispatch layer must use the real loaded-receipt contract for
/// those, never an invented timestamp.
#[derive(Clone, Debug)]
pub(crate) struct OwnerEvidence {
    pub(crate) workspace_id: String,
    /// The enrolled stable participant identity this runtime was allocated
    /// for. Live owner row data, never a caller field.
    pub(crate) agent_uid: String,
    /// The random nonauthorizing enrollment selector for this runtime.
    pub(crate) session_binding: String,
    pub(crate) custody_domain: &'static str,
    pub(crate) custody_epoch: u64,
    pub(crate) owner_epoch: u64,
    pub(crate) runtime_generation: u64,
    pub(crate) enrollment_id: String,
    pub(crate) phase: String,
    pub(crate) fenced: bool,
    pub(crate) fenced_reason: Option<String>,
    pub(crate) loaded_receipt_id: String,
    pub(crate) observed_registry_digest: String,
    pub(crate) node_pid: u32,
    pub(crate) node_starttime: u64,
    pub(crate) os_uid: u32,
    pub(crate) container_id: String,
    pub(crate) image_digest: String,
    pub(crate) profile_digest: String,
}

/// The root owner's authenticated prestart witness / ledger floor. Secret
/// free; produced only from `prestart_eligibility_v1`.
#[derive(Clone, Debug)]
pub(crate) struct PrestartEligibility {
    pub(crate) workspace_id: String,
    pub(crate) owner_epoch: u64,
    pub(crate) database_epoch: u64,
    pub(crate) restore_state: String,
    pub(crate) min_sequence: u64,
    pub(crate) min_digest: String,
}

/// The authenticated selector for one owned-console attachment. Private
/// fields; produced only by [`RootOwnerClient::open_console`] from the
/// owner's own reply and re-validated by the console connect.
#[derive(Clone, Debug)]
pub(crate) struct ConsoleOpen {
    pub(crate) path: PathBuf,
    pub(crate) token: String,
    pub(crate) node_pid: u32,
    pub(crate) node_starttime: u64,
}

/// One connection-scoped owner client. The socket path comes from the
/// root-owned profile; nothing here is caller-configurable.
pub(crate) struct RootOwnerClient {
    profile: LocalProfile,
    supervisor_generation: String,
    socket: PathBuf,
    peer: Option<AuthenticatedOwnerPeer>,
}

impl RootOwnerClient {
    /// Authenticate with the root owner (`hello_v1` + kernel peer + pinned key)
    /// before any authority call is possible.
    pub(crate) fn connect(profile: &LocalProfile, supervisor_generation: String) -> Result<Self> {
        if !strict::is_lower_hex(&supervisor_generation, 32) {
            return Err(Error::rejected(
                "supervisor generation must be 32 lowercase hex",
            ));
        }
        let mut client = Self {
            profile: profile.clone(),
            supervisor_generation,
            socket: profile.control_socket().to_path_buf(),
            peer: None,
        };
        let peer = client.hello_v1()?;
        client.peer = Some(peer);
        Ok(client)
    }

    pub(crate) fn profile(&self) -> &LocalProfile {
        &self.profile
    }

    /// The authenticated owner epoch every exchange is bound to.
    pub(crate) fn owner_epoch(&self) -> u64 {
        self.peer.as_ref().map(|p| p.owner_epoch).unwrap_or(0)
    }

    /// Bind a signed fence callback to this channel's authenticated live owner.
    /// Root's executable was measured by the signed hello; the supervisor must
    /// not attempt an unprivileged `/proc/<root>/exe` open on the callback.
    pub(crate) fn verify_owner_callback(
        &self,
        stream: &std::os::unix::net::UnixStream,
        owner_epoch: u64,
    ) -> Result<()> {
        let expected = self
            .peer
            .as_ref()
            .ok_or_else(|| Error::rejected("owner channel is not authenticated"))?;
        let (pid, starttime) = owner_kernel_peer(stream)?;
        if pid != expected.owner_pid
            || starttime != expected.owner_starttime
            || owner_epoch != expected.owner_epoch
        {
            return Err(Error::rejected(
                "broker fence callback is not the authenticated live owner",
            ));
        }
        Ok(())
    }

    fn hello_v1(&self) -> Result<AuthenticatedOwnerPeer> {
        let nonce = fresh_hex64()?;
        let supervisor_pid = std::process::id();
        let supervisor_starttime = super::peer::starttime(supervisor_pid)
            .ok_or_else(|| Error::unknown("supervisor starttime unreadable"))?;
        let (data, envelope_epoch, peer_pid, peer_starttime) = self.raw_exchange(
            "hello_v1",
            serde_json::json!({
                "request_nonce": nonce,
                "supervisor_generation": self.supervisor_generation,
            }),
        )?;
        let unsigned =
            proof::verify_proof(&self.profile, proof::PEER_DOMAIN, proof::PEER_FIELDS, &data)?;
        let m = unsigned
            .as_object()
            .ok_or_else(|| Error::rejected("owner peer proof is not an object"))?;
        let owner_pid = u32::try_from(strict::positive_u64(m, "owner_pid")?)
            .map_err(|_| Error::rejected("owner_pid is out of range"))?;
        let owner_starttime = strict::positive_u64(m, "owner_starttime")?;
        let owner_epoch = strict::positive_u64(m, "owner_epoch")?;
        let key_id = proof::owner_key_id(&self.profile);
        if strict::safe_u64(m, "v")? != 1
            || strict::string(m, "request_nonce")? != nonce.as_str()
            || strict::string(m, "workspace_id")? != self.profile.workspace_id()
            || strict::string(m, "owner_exe_digest")? != self.profile.owner_exe_digest()
            || strict::string(m, "profile_digest")? != self.profile.digest()
            || strict::safe_u64(m, "supervisor_pid")? != u64::from(supervisor_pid)
            || strict::safe_u64(m, "supervisor_starttime")? != supervisor_starttime
            || strict::string(m, "supervisor_exe_digest")?
                != self.profile.supervisor().exe_digest.as_str()
            || strict::string(m, "supervisor_generation")? != self.supervisor_generation.as_str()
            || strict::string(m, "key_id")? != key_id.as_str()
            || owner_epoch != envelope_epoch
            || peer_pid != owner_pid
            || peer_starttime != owner_starttime
        {
            return Err(Error::rejected(
                "owner peer proof does not bind this supervisor and kernel peer",
            ));
        }
        Ok(AuthenticatedOwnerPeer {
            owner_pid,
            owner_starttime,
            owner_epoch,
        })
    }

    /// One raw exchange: connect, measure the accepted root-owner kernel peer,
    /// write/read one bounded frame, re-measure the peer and return the reply
    /// data with the owner-echoed epoch.
    fn raw_exchange(
        &self,
        kind: &str,
        args: serde_json::Value,
    ) -> Result<(serde_json::Value, u64, u32, u64)> {
        let stream = std::os::unix::net::UnixStream::connect(&self.socket)
            .map_err(|_| Error::unknown("root owner control socket is unavailable"))?;
        let (pid_before, starttime_before) = owner_kernel_peer(&stream)?;
        let request = serde_json::json!({
            "v": 1,
            "request_id": uuid::Uuid::new_v4().to_string(),
            "method": {"kind": kind, "args": args},
        });
        let body = serde_json::to_vec(&request)
            .map_err(|_| Error::internal("owner request cannot be encoded"))?;
        frame::write_frame(&stream, &body, RESPONSE_BUDGET)?;
        let reply = frame::read_frame(&stream, REQUEST_BUDGET, MAX_FRAME_BYTES)?;
        let (pid_after, starttime_after) = owner_kernel_peer(&stream)?;
        if pid_before != pid_after || starttime_before != starttime_after {
            return Err(Error::unknown(
                "root owner peer changed during the exchange — refusing",
            ));
        }
        let value = strict::parse(&reply)?;
        let envelope = value
            .as_object()
            .ok_or_else(|| Error::unknown("owner reply is not an object"))?;
        let ok = envelope
            .get("ok")
            .and_then(|v| v.as_bool())
            .ok_or_else(|| Error::unknown("owner reply has no verdict"))?;
        if !ok {
            let error = envelope
                .get("error")
                .and_then(|v| v.as_object())
                .ok_or_else(|| Error::unknown("owner error missing"))?;
            let code = error
                .get("code")
                .and_then(|v| v.as_str())
                .unwrap_or("unavailable");
            return Err(match code {
                "management_forbidden" => Error::rejected("owner refused the request"),
                "stale_revision" => Error::conflict_coded(
                    "owner_conflict",
                    "owner state conflicted with the request",
                ),
                "rate_limit" => Error::busy("owner is busy"),
                _ => Error::unknown("owner authority unavailable"),
            });
        }
        let epoch = envelope
            .get("owner_epoch")
            .and_then(|v| v.as_u64())
            .filter(|v| *v > 0)
            .ok_or_else(|| Error::unknown("owner reply does not echo its epoch"))?;
        let data = envelope
            .get("data")
            .cloned()
            .ok_or_else(|| Error::unknown("owner reply has no data"))?;
        Ok((data, epoch, pid_after, starttime_after))
    }

    /// One authenticated exchange: the live owner pid/starttime and the echoed
    /// epoch must equal the `hello_v1` facts.
    fn exchange(&self, kind: &str, args: serde_json::Value) -> Result<(serde_json::Value, u64)> {
        let peer = self
            .peer
            .as_ref()
            .ok_or_else(|| Error::unknown("owner channel is not authenticated"))?;
        let (data, epoch, pid, starttime) = self.raw_exchange(kind, args)?;
        if pid != peer.owner_pid || starttime != peer.owner_starttime {
            return Err(Error::unknown(
                "owner peer is not the authenticated instance — fresh hello required",
            ));
        }
        Ok((data, epoch))
    }

    fn call(&self, kind: &str, args: serde_json::Value) -> Result<serde_json::Value> {
        let peer = self
            .peer
            .as_ref()
            .ok_or_else(|| Error::unknown("owner channel is not authenticated"))?;
        let (data, epoch) = self.exchange(kind, args)?;
        if epoch != peer.owner_epoch {
            return Err(Error::unknown(
                "owner epoch changed — a fresh hello is required",
            ));
        }
        Ok(data)
    }

    /// Prepare one allocating intent. The reply carries the canonical
    /// LaunchRequest/digest, the measured recipient and the owner-launched
    /// installer; `challenge` is null until `enroll_pending` binds it.
    pub(crate) fn prepare_runtime(&self, request: &PrepareRequest) -> Result<PreparedRuntime> {
        let data = self.call(
            "prepare_runtime",
            serde_json::json!({
                "stable_agent_uid": request.stable_agent_uid,
                "session_binding": request.session_binding,
                "server_generation": request.server_generation,
                "profile_digest": self.profile.digest(),
                "expected_owner_epoch": request.expected_owner_epoch,
                "supervisor_generation": self.supervisor_generation,
            }),
        )?;
        PreparedRuntime::from_json(&data)
    }

    /// Release the staged capsule to the owner-launched installer for this
    /// exact committed allocation. The reply is delivery evidence only; it is
    /// not launch, consume or readiness authority.
    pub(crate) fn release_installer(
        &self,
        runtime_id: &str,
        operation_id: &str,
        generation: u64,
    ) -> Result<()> {
        let data = self.call(
            "release_installer_v1",
            serde_json::json!({
                "runtime_id": runtime_id,
                "operation_id": operation_id,
                "generation": generation,
                "supervisor_generation": self.supervisor_generation,
            }),
        )?;
        let m = strict::object(&data, &["v", "released"])?;
        if strict::safe_u64(m, "v")? != 1 || !strict::boolean(m, "released")? {
            return Err(Error::unknown(
                "owner release reply does not acknowledge capsule delivery",
            ));
        }
        Ok(())
    }

    /// Challenge-only installer measurement. The returned owner document must
    /// be verified by the caller against its own nonce/kernel peer/intent.
    pub(crate) fn measure_challenge_installer(
        &self,
        allocation_id: &str,
        launch_request_digest: &str,
        installer_pid: u32,
        installer_starttime: u64,
        request_nonce: &str,
    ) -> Result<serde_json::Value> {
        self.call(
            "measure_challenge_installer_v1",
            serde_json::json!({
                "allocation_id": allocation_id,
                "launch_request_digest": launch_request_digest,
                "installer_pid": installer_pid,
                "installer_starttime": installer_starttime,
                "request_nonce": request_nonce,
                "supervisor_generation": self.supervisor_generation,
            }),
        )
    }

    /// Prepared installer measurement. The returned owner document must be
    /// verified against the exact canonical challenge digest and kernel peer.
    pub(crate) fn measure_installer(
        &self,
        challenge_digest: &str,
        installer_pid: u32,
        installer_starttime: u64,
        request_nonce: &str,
    ) -> Result<serde_json::Value> {
        self.call(
            "measure_installer_v1",
            serde_json::json!({
                "challenge_digest": challenge_digest,
                "installer_pid": installer_pid,
                "installer_starttime": installer_starttime,
                "request_nonce": request_nonce,
                "supervisor_generation": self.supervisor_generation,
            }),
        )
    }

    pub(crate) fn enroll_pending(
        &self,
        runtime_id: &str,
        challenge: &SupervisorChallenge,
    ) -> Result<()> {
        let _ = self.call(
            "enroll_pending",
            serde_json::json!({
                "runtime_id": runtime_id,
                "challenge": canonical_challenge_value(challenge),
                "supervisor_generation": self.supervisor_generation,
            }),
        )?;
        Ok(())
    }

    pub(crate) fn enrolled(&self, challenge: &SupervisorChallenge) -> Result<Option<u64>> {
        self.grant_enrollment_status("enrolled", challenge)
    }

    pub(crate) fn recheck_grant(&self, challenge: &SupervisorChallenge) -> Result<bool> {
        Ok(self
            .grant_enrollment_status("recheck_grant", challenge)?
            .is_some())
    }

    fn grant_enrollment_status(
        &self,
        method: &str,
        challenge: &SupervisorChallenge,
    ) -> Result<Option<u64>> {
        let data = self.call(
            method,
            serde_json::json!({
                "challenge": canonical_challenge_value(challenge),
                "supervisor_generation": self.supervisor_generation,
            }),
        )?;
        let m = strict::object(&data, &["enrolled", "owner_epoch"])?;
        let present = strict::boolean(m, "enrolled")?;
        let epoch = strict::safe_u64(m, "owner_epoch")?;
        Ok(if present { Some(epoch) } else { None })
    }

    pub(crate) fn consume(&self, challenge: &SupervisorChallenge) -> Result<ConsumeOutcome> {
        let data = self.call(
            "consume",
            serde_json::json!({
                "challenge": canonical_challenge_value(challenge),
                "supervisor_generation": self.supervisor_generation,
            }),
        )?;
        let m = strict::object(&data, &["consumed"])?;
        Ok(if strict::boolean(m, "consumed")? {
            ConsumeOutcome::Consumed
        } else {
            ConsumeOutcome::Unknown
        })
    }

    pub(crate) fn start_runtime(&self, runtime_id: &str, generation: u64) -> Result<StartOutcome> {
        let data = self.call(
            "start_runtime",
            serde_json::json!({
                "runtime_id": runtime_id,
                "expected_generation": generation,
                "supervisor_generation": self.supervisor_generation,
            }),
        )?;
        StartOutcome::from_json(&data)
    }

    /// Fresh recheck → the only place a `QualifiedRuntime` is minted.
    pub(crate) fn recheck(&self, runtime_id: &str, generation: u64) -> Result<QualifiedRuntime> {
        let data = self.call(
            "recheck",
            serde_json::json!({
                "runtime_id": runtime_id,
                "expected_generation": generation,
                "supervisor_generation": self.supervisor_generation,
            }),
        )?;
        let identity = RuntimeIdentity::from_json(&data)?;
        Ok(QualifiedRuntime::mint(identity))
    }

    /// Live owner evidence for one runtime: an authenticated `observe` plus a
    /// fresh `recheck` that must agree. This is the typed surface the protected
    /// dispatch eligibility adapter consumes; it never certifies readiness by
    /// itself and never synthesizes a timestamp or epoch on failure.
    pub(crate) fn owner_evidence(
        &self,
        runtime_id: &str,
        generation: u64,
    ) -> Result<OwnerEvidence> {
        let status = self.observe(runtime_id)?;
        let qualified = self.recheck(runtime_id, generation)?;
        let identity = qualified.identity();
        if status.runtime_id != identity.runtime_id
            || status.generation != identity.runtime_generation
            || status.owner_epoch != identity.owner_epoch
            || !qualified.is_current()
        {
            return Err(Error::unknown(
                "owner status and live recheck disagree — refusal",
            ));
        }
        Ok(OwnerEvidence {
            workspace_id: identity.workspace_id.clone(),
            agent_uid: identity.agent_uid.clone(),
            session_binding: identity.session_binding.clone(),
            custody_domain: "local",
            custody_epoch: status.database_epoch,
            owner_epoch: identity.owner_epoch,
            runtime_generation: identity.runtime_generation,
            enrollment_id: identity.runtime_id.clone(),
            phase: status.phase.clone(),
            fenced: !status.qualified || status.phase != "ready",
            fenced_reason: status.fenced_reason.clone(),
            loaded_receipt_id: identity.loaded_receipt_id.clone(),
            observed_registry_digest: identity.loaded_registry_digest.clone(),
            node_pid: identity.node_pid,
            node_starttime: identity.node_starttime,
            os_uid: identity.os_uid,
            container_id: identity.container_id.clone(),
            image_digest: identity.image_digest.clone(),
            profile_digest: identity.profile_digest.clone(),
        })
    }

    /// The authenticated external prestart witness. This is the root physical
    /// issuer's provenance document; it is not a grant, a receipt or an epoch
    /// that a local database can assert by itself.
    pub(crate) fn prestart_eligibility(&self) -> Result<PrestartEligibility> {
        let data = self.call(
            "prestart_eligibility_v1",
            serde_json::json!({"supervisor_generation": self.supervisor_generation}),
        )?;
        let m = strict::object(
            &data,
            &[
                "v",
                "workspace_id",
                "owner_epoch",
                "database_epoch",
                "restore_state",
                "min_sequence",
                "min_digest",
            ],
        )?;
        if strict::safe_u64(m, "v")? != 1 {
            return Err(Error::unknown("owner prestart witness is malformed"));
        }
        Ok(PrestartEligibility {
            workspace_id: strict::string(m, "workspace_id")?.to_string(),
            owner_epoch: strict::positive_u64(m, "owner_epoch")?,
            database_epoch: strict::positive_u64(m, "database_epoch")?,
            restore_state: strict::string(m, "restore_state")?.to_string(),
            min_sequence: strict::safe_u64(m, "min_sequence")?,
            min_digest: strict::string(m, "min_digest")?.to_string(),
        })
    }

    /// `open_console_v1` — open the one bounded owned-console attachment for
    /// this runtime. The reply names a path and a one-use token; the owner
    /// re-measures this process when the console socket is accepted. A
    /// selector or path alone is never authority.
    pub(crate) fn open_console(
        &self,
        runtime_id: &str,
        generation: u64,
        recipient_nonce: &str,
    ) -> Result<ConsoleOpen> {
        let data = self.call(
            "open_console_v1",
            serde_json::json!({
                "v": 1,
                "runtime_id": runtime_id,
                "expected_generation": generation,
                "recipient_nonce": recipient_nonce,
                "supervisor_generation": self.supervisor_generation,
            }),
        )?;
        let m = strict::object(
            &data,
            &[
                "v",
                "runtime_id",
                "generation",
                "console_path",
                "token",
                "node_pid",
                "node_starttime",
            ],
        )?;
        if strict::safe_u64(m, "v")? != 1
            || strict::string(m, "runtime_id")? != runtime_id
            || strict::safe_u64(m, "generation")? != generation
        {
            return Err(Error::unknown(
                "owner console reply does not bind this runtime",
            ));
        }
        let path = PathBuf::from(strict::string(m, "console_path")?);
        super::console::validate_path(self.profile.run_dir(), runtime_id, &path)?;
        let token = strict::string(m, "token")?.to_string();
        if !strict::is_lower_hex(&token, 64) {
            return Err(Error::unknown("owner console token is malformed"));
        }
        let node_pid = u32::try_from(strict::positive_u64(m, "node_pid")?)
            .map_err(|_| Error::unknown("owner console node pid is out of range"))?;
        let node_starttime = strict::positive_u64(m, "node_starttime")?;
        Ok(ConsoleOpen {
            path,
            token,
            node_pid,
            node_starttime,
        })
    }

    pub(crate) fn fence(
        &self,
        runtime_id: &str,
        generation: u64,
        reason: &str,
    ) -> Result<FenceAck> {
        let data = self.call(
            "fence",
            serde_json::json!({
                "runtime_id": runtime_id,
                "expected_generation": generation,
                "reason": reason,
                "supervisor_generation": self.supervisor_generation,
            }),
        )?;
        let m = strict::object(
            &data,
            &[
                "v",
                "runtime_id",
                "owner_epoch",
                "generation",
                "nonce",
                "fenced_at_ms",
            ],
        )?;
        Ok(FenceAck {
            v: 1,
            runtime_id: strict::string(m, "runtime_id")?.to_string(),
            owner_epoch: strict::safe_u64(m, "owner_epoch")?,
            generation: strict::safe_u64(m, "generation")?,
            nonce: strict::string(m, "nonce")?.to_string(),
            fenced_at_ms: strict::safe_u64(m, "fenced_at_ms")?,
        })
    }

    pub(crate) fn observe(&self, runtime_id: &str) -> Result<RuntimeStatus> {
        let data = self.call(
            "observe",
            serde_json::json!({
                "runtime_id": runtime_id,
                "supervisor_generation": self.supervisor_generation,
            }),
        )?;
        RuntimeStatus::from_json(&data)
    }

    pub(crate) fn checkpoint(
        &self,
        checkpoint: &CheckpointV1,
        prefix: &[LedgerRecordV1],
        runtime_generation: u64,
    ) -> Result<CheckpointAck> {
        let data = self.call(
            "checkpoint",
            serde_json::json!({
                "checkpoint": checkpoint,
                "prefix": prefix,
                "runtime_generation": runtime_generation,
                "supervisor_generation": self.supervisor_generation,
            }),
        )?;
        serde_json::from_value(data)
            .map_err(|_| Error::unknown("owner checkpoint ack is malformed"))
    }

    /// Read-only receipt for an older head the owner already verified. This
    /// never advances the watermark and never carries records; the exact
    /// checkpoint (owner/db epoch, Local domain, head, binding) is validated by
    /// the owner against its live authority and stored receipt row.
    pub(crate) fn checkpoint_receipt(&self, checkpoint: &CheckpointV1) -> Result<CheckpointAck> {
        let data = self.call(
            "checkpoint_receipt_v1",
            serde_json::json!({
                "checkpoint": checkpoint,
                "supervisor_generation": self.supervisor_generation,
            }),
        )?;
        serde_json::from_value(data)
            .map_err(|_| Error::unknown("owner checkpoint receipt is malformed"))
    }

    /// Restore begin deliberately advances the owner epoch. After it succeeds
    /// this client's authenticated epoch is stale; the caller must reconnect
    /// (`RootOwnerClient::connect`, fresh hello) before the next exchange.
    pub(crate) fn restore_begin(
        &self,
        checkpoint: &CheckpointV1,
        prefix: &[LedgerRecordV1],
        reason: &str,
    ) -> Result<u64> {
        let (data, epoch) = self.exchange(
            "restore_begin",
            serde_json::json!({
                "checkpoint": checkpoint,
                "prefix": prefix,
                "reason": reason,
                "supervisor_generation": self.supervisor_generation,
            }),
        )?;
        let m = strict::object(&data, &["owner_epoch", "restore_state"])?;
        let returned = strict::positive_u64(m, "owner_epoch")?;
        if returned != epoch || returned <= self.owner_epoch() {
            return Err(Error::unknown(
                "owner restore epoch is not a strictly newer authenticated epoch",
            ));
        }
        Ok(returned)
    }

    /// Restore complete on a freshly connected client (the epoch advanced).
    pub(crate) fn restore_complete(&self, expected_epoch: u64) -> Result<()> {
        let (data, epoch) = self.exchange(
            "restore_complete",
            serde_json::json!({
                "expected_epoch": expected_epoch,
                "supervisor_generation": self.supervisor_generation,
            }),
        )?;
        let m = strict::object(&data, &["ok", "restore_state"])?;
        if !strict::boolean(m, "ok")? || epoch != expected_epoch {
            return Err(Error::unknown("owner restore completion is not this epoch"));
        }
        Ok(())
    }
}

/// The accepted connected socket's kernel peer must be the root owner: uid 0,
/// a nonzero live pid and its current `/proc` starttime. The owner's
/// executable digest is bound by the signed `hello_v1` proof, never by an
/// unprivileged `/proc/<root>/exe` open.
fn owner_kernel_peer(stream: &std::os::unix::net::UnixStream) -> Result<(u32, u64)> {
    let (uid, pid) = super::peer::credentials(stream)?;
    if uid != 0 {
        return Err(Error::rejected("root owner control peer is not uid 0"));
    }
    let starttime = super::peer::starttime(pid)
        .ok_or_else(|| Error::unknown("owner starttime is unreadable"))?;
    Ok((pid, starttime))
}

fn fresh_hex64() -> Result<String> {
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).map_err(|_| Error::unknown("entropy unavailable"))?;
    Ok(raw.iter().map(|b| format!("{b:02x}")).collect())
}

/// The daemon's durable-obligation + epoch port, backed by the root owner.
/// Transport failure maps to refusal (`false`/`Unknown`), never success.
impl crate::daemon::supervisor_grant::ExternalConsume for RootOwnerClient {
    fn enroll_pending(&self, _challenge: &SupervisorChallenge) -> bool {
        // Enrollment is the owner's `prepare_runtime` + `enroll_pending`; the
        // daemon never enrolls at receipt. This port is deliberately not the
        // grant path.
        false
    }

    fn enrolled(&self, challenge: &SupervisorChallenge) -> bool {
        matches!(RootOwnerClient::enrolled(self, challenge), Ok(Some(_)))
    }

    fn consume(&self, challenge: &SupervisorChallenge) -> ConsumeOutcome {
        match RootOwnerClient::consume(self, challenge) {
            Ok(outcome) => outcome,
            Err(_) => ConsumeOutcome::Unknown,
        }
    }

    fn recheck(&self, challenge: &SupervisorChallenge) -> bool {
        matches!(RootOwnerClient::recheck_grant(self, challenge), Ok(true))
    }

    fn current_epoch(&self, challenge: &SupervisorChallenge) -> Result<u64> {
        RootOwnerClient::enrolled(self, challenge)?
            .ok_or_else(|| Error::rejected("owner has no prepared obligation for this challenge"))
    }
}
