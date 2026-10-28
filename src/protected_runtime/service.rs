//! The root-owned `cadence-runtime-owner` service: the finite control socket,
//! the eight exact runtime methods plus the checkpoint/restore authority, the
//! deterministic Docker lifecycle and the post-start measurement/attestation
//! promotion.
//!
//! Every method measures its peer from the kernel (`SO_PEERCRED` + held
//! `/proc/<pid>/exe` digest + live starttime + enrolled generation) before it
//! reads authority out of the request. A request field can never name a
//! caller, epoch, image, mount, command or readiness. The service holds no
//! user credential key and never touches provider traffic.

use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::daemon::supervisor_grant::{
    canonical_challenge_bytes, canonical_challenge_value, parse_challenge_bytes, parse_request,
    LaunchBinding, Lineage, Pins, Recipient, SupervisorChallenge,
};
use crate::error::{Error, Result};

use super::attestation;
use super::docker::DockerClient;
use super::frame;
use super::keys::OwnerSigner;
use super::measure;
use super::peer;
use super::profile::LocalProfile;
use super::proof;
use super::qualified::{FenceAck, RuntimeIdentity, RuntimeStatus};
use super::spec::{self, RuntimeSpec};
use super::store::{
    AllocParams, CheckpointAck, CheckpointV1, ConsumeOutcome, LedgerRecordV1, Obligation,
    OwnerStore,
};
use super::strict;
use super::types::{PreparedRuntime, StartOutcome};
use super::{MAX_FRAME_BYTES, REQUEST_BUDGET, RESPONSE_BUDGET};

/// How long the owner waits for the pinned observer's first verified registry.
const OBSERVER_BUDGET: Duration = Duration::from_secs(120);

/// One owner-launched installer child. The capsule is staged after the
/// allocating intent commits and is written only by `release_installer_v1`.
struct InstallerChild {
    child: Child,
    pid: u32,
    starttime: u64,
    started_at: std::time::Instant,
    capsule: Option<Vec<u8>>,
    released: bool,
}

pub(crate) struct RuntimeOwnerService {
    profile: LocalProfile,
    store: Arc<OwnerStore>,
    docker: DockerClient,
    /// The engine data root opened once from the fixed `/info` fact and held
    /// for the owner's lifetime. Replacing the path or bind mount later cannot
    /// redirect this trusted directory descriptor; every per-container proof
    /// still re-opens its container directory and metadata descriptors.
    engine_mount_root: super::measure_mounts::EngineMountRoot,
    signer: OwnerSigner,
    /// The exact owner-launched installer children, keyed by runtime id. Kept
    /// so the owner can reap/kill its own child and so a pid is never guessed.
    installers: Mutex<HashMap<String, InstallerChild>>,
}

impl RuntimeOwnerService {
    pub(crate) fn new(
        profile: LocalProfile,
        store: Arc<OwnerStore>,
        docker: DockerClient,
        signer: OwnerSigner,
    ) -> Result<Self> {
        let engine_mount_root = super::measure_mounts::EngineMountRoot::from_docker(&docker)?;
        Ok(Self {
            profile,
            store,
            docker,
            engine_mount_root,
            signer,
            installers: Mutex::new(HashMap::new()),
        })
    }

    /// Reconcile deterministic owned containers on startup. Only objects whose
    /// owner label, runtime id, nonce, profile digest and name match a stored
    /// obligation are ever stopped/removed. An unknown/foreign container is
    /// left alone; a failed retirement keeps the obligation non-executable.
    pub(crate) fn reconcile_owned(&self) -> Result<()> {
        let listed = self.docker.list_owned(spec::OWNER_LABEL)?;
        for container in listed {
            let labels = match container.get("Labels").and_then(|v| v.as_object()) {
                Some(labels) => labels,
                None => continue,
            };
            let runtime_id = match labels.get(spec::LABEL_RUNTIME).and_then(|v| v.as_str()) {
                Some(id) => id,
                None => continue,
            };
            let nonce = match labels.get(spec::LABEL_NONCE).and_then(|v| v.as_str()) {
                Some(nonce) => nonce,
                None => continue,
            };
            let profile_label = labels
                .get(spec::LABEL_PROFILE)
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let name = container
                .get("Names")
                .and_then(|v| v.as_array())
                .and_then(|names| names.first())
                .and_then(|v| v.as_str())
                .map(|n| n.trim_start_matches('/').to_string())
                .unwrap_or_default();
            let row = match self.store.get(runtime_id) {
                Ok(row) => row,
                // Unknown runtime id: never stop an object we cannot prove.
                Err(_) => continue,
            };
            if row.operation_id != nonce
                || row.container_name != name
                || profile_label != self.profile.digest()
            {
                continue;
            }
            if matches!(row.phase.as_str(), "revoking" | "revoked") {
                if let Some(id) = container.get("Id").and_then(|v| v.as_str()) {
                    let _ = self.docker.stop(id);
                    let _ = self.docker.remove(id);
                }
            }
        }
        Ok(())
    }

    fn supervisor_exe_hex(&self) -> String {
        self.profile
            .supervisor()
            .exe_digest
            .strip_prefix("sha256:")
            .unwrap_or("")
            .to_string()
    }

    /// Measure the enrolled supervisor and require the generation that was
    /// enrolled for this exact live process. The first successful hello enrolls
    /// the presented generation against the measured pid/starttime; any later
    /// presentation must equal it.
    fn admit_supervisor(&self, stream: &UnixStream, presented: &str) -> Result<peer::Peer> {
        if !strict::is_lower_hex(presented, 32) {
            return Err(Error::rejected(
                "supervisor generation must be 32 lowercase hex",
            ));
        }
        let (uid, gid, _pid) = peer::credentials_full(stream)?;
        if uid != self.profile.supervisor().uid || gid != self.profile.supervisor().gid {
            return Err(Error::rejected(
                "socket peer is not the configured supervisor uid/gid",
            ));
        }
        let measured = peer::admit_exe(
            stream,
            self.profile.supervisor().uid,
            &self.supervisor_exe_hex(),
        )?;
        match self
            .store
            .peer_generation("supervisor", measured.pid, measured.starttime)?
        {
            Some(enrolled) if enrolled != presented => Err(Error::rejected(
                "presented supervisor generation is not the enrolled one",
            )),
            Some(_) => Ok(measured),
            None => {
                self.store.record_peer("supervisor", presented, &measured)?;
                Ok(measured)
            }
        }
    }

    /// Require an already-enrolled supervisor process (no new enrollment).
    /// The presented generation must equal the one enrolled for this exact
    /// live pid/starttime.
    fn require_supervisor(&self, stream: &UnixStream, presented: &str) -> Result<peer::Peer> {
        if !strict::is_lower_hex(presented, 32) {
            return Err(Error::rejected(
                "supervisor generation must be 32 lowercase hex",
            ));
        }
        let (uid, gid, _pid) = peer::credentials_full(stream)?;
        if uid != self.profile.supervisor().uid || gid != self.profile.supervisor().gid {
            return Err(Error::rejected(
                "socket peer is not the configured supervisor uid/gid",
            ));
        }
        let measured = peer::admit_exe(
            stream,
            self.profile.supervisor().uid,
            &self.supervisor_exe_hex(),
        )?;
        match self
            .store
            .peer_generation("supervisor", measured.pid, measured.starttime)?
        {
            Some(enrolled) if enrolled == presented => Ok(measured),
            Some(_) => Err(Error::rejected(
                "presented supervisor generation is not the enrolled one",
            )),
            None => Err(Error::rejected(
                "supervisor peer is not enrolled — a fresh hello is required",
            )),
        }
    }

    fn require_installer_generation(&self, presented: &str) -> Result<()> {
        if presented != self.profile.installer().generation.as_str() {
            return Err(Error::rejected(
                "installer generation is not the pinned enrollment",
            ));
        }
        Ok(())
    }

    fn begin_wire(&self) -> Result<()> {
        super::store::verify_private_file(self.store.path())
    }
}

/// One control connection handled on its own thread.
fn handle_control(service: Arc<RuntimeOwnerService>, stream: UnixStream) {
    let _ = handle_control_inner(&service, &stream);
}

fn handle_control_inner(service: &RuntimeOwnerService, stream: &UnixStream) -> Result<()> {
    let response = (|| -> Result<serde_json::Value> {
        service.begin_wire()?;
        service.reap_installers();
        let body = frame::read_frame(stream, REQUEST_BUDGET, MAX_FRAME_BYTES)?;
        let value = strict::parse(&body)?;
        let envelope = strict::object(&value, &["v", "request_id", "method"])?;
        if strict::safe_u64(envelope, "v")? != 1 {
            return Err(Error::rejected("owner frame v must be exactly 1"));
        }
        let request_id = strict::string(envelope, "request_id")?;
        if !strict::is_uuid(request_id) {
            return Err(Error::rejected("request_id must be a UUID"));
        }
        let method = strict::object(
            envelope
                .get("method")
                .ok_or_else(|| Error::rejected("method missing"))?,
            &["kind", "args"],
        )?;
        let kind = strict::string(method, "kind")?;
        let args = method
            .get("args")
            .ok_or_else(|| Error::rejected("method args missing"))?;
        match kind {
            "hello_v1" => service.hello_v1(stream, args),
            "prepare_runtime" => service.prepare(stream, args),
            "release_installer_v1" => service.release_installer(stream, args),
            "measure_challenge_installer_v1" => service.measure_challenge_installer(stream, args),
            "measure_installer_v1" => service.measure_installer(stream, args),
            "enroll_pending" => service.enroll_pending(stream, args),
            "enrolled" => service.grant_enrollment_status(stream, args, false),
            "recheck_grant" => service.grant_enrollment_status(stream, args, true),
            "consume" => service.consume(stream, args),
            "start_runtime" => service.start_runtime(stream, args),
            "recheck" => service.recheck(stream, args),
            "fence" => service.fence(stream, args),
            "observe" => service.observe(stream, args),
            "prestart_eligibility_v1" => service.prestart_eligibility(stream, args),
            "open_console_v1" => service.open_console(stream, args),
            "checkpoint" => service.checkpoint(stream, args),
            "checkpoint_receipt_v1" => service.checkpoint_receipt_v1(stream, args),
            "restore_begin" => service.restore_begin(stream, args),
            "restore_complete" => service.restore_complete(stream, args),
            _ => Err(Error::rejected("unknown owner method")),
        }
    })();
    let payload = match response {
        Ok(data) => {
            // Every success reply echoes the owner's current epoch. The
            // supervisor's authenticated client requires it on each exchange;
            // a changed/missing value invalidates the channel. The reply is
            // delivered over the socket whose kernel peer was already measured.
            let epoch = service.store.owner_epoch().unwrap_or(0);
            frame::ok_with_epoch(data, epoch)
        }
        Err(e) => frame::err(error_code(e.kind()), fixed_message(e.kind())),
    };
    frame::write_frame(stream, &payload, RESPONSE_BUDGET)
}

fn error_code(kind: &str) -> &'static str {
    match kind {
        "rejected" => "management_forbidden",
        "conflict" => "stale_revision",
        "busy" => "rate_limit",
        _ => "unavailable",
    }
}

fn fixed_message(kind: &str) -> &'static str {
    match kind {
        "rejected" => "owner refused the request",
        "conflict" => "owner state conflicted",
        "busy" => "owner is busy",
        _ => "owner authority unavailable",
    }
}

impl RuntimeOwnerService {
    /// `hello_v1` — the supervisor supplies a fresh 256-bit nonce and its
    /// stable per-process generation. The owner measures the caller from the
    /// accepted socket (uid/gid/pid/starttime + pinned supervisor executable),
    /// measures its own held executable and returns the exactly-typed
    /// `OwnerPeerProofV1`. A first hello records the process enrollment once;
    /// an existing same pid/starttime cannot change its generation.
    fn hello_v1(&self, stream: &UnixStream, args: &serde_json::Value) -> Result<serde_json::Value> {
        let m = strict::object(args, &["request_nonce", "supervisor_generation"])?;
        let nonce = strict::string(m, "request_nonce")?;
        proof::validate_nonce(nonce)?;
        let supervisor_generation = strict::string(m, "supervisor_generation")?;
        proof::validate_generation(supervisor_generation)?;
        let measured = self.admit_supervisor(stream, supervisor_generation)?;
        let owner = proof::measure_owner_self(&self.profile)?;
        let epoch = self.store.owner_epoch()?;
        proof::sign_peer_proof(
            &self.signer,
            &self.profile,
            epoch,
            nonce,
            &owner,
            &measured,
            supervisor_generation,
        )
    }

    /// The exact measured supervisor the allocation is bound to.
    fn require_allocation_recipient(&self, row: &Obligation, measured: &peer::Peer) -> Result<()> {
        if row.recipient_pid != Some(measured.pid)
            || row.recipient_starttime != Some(measured.starttime)
        {
            return Err(Error::rejected(
                "allocation is bound to a different measured supervisor instance",
            ));
        }
        Ok(())
    }

    /// Only the owner-launched/enrolled installer of this exact allocation is
    /// accepted — never an arbitrary pid or a caller-created enrollment.
    fn require_enrolled_installer(
        &self,
        row: &Obligation,
        installer_pid: u32,
        installer_starttime: u64,
    ) -> Result<String> {
        if row.installer_pid != Some(installer_pid)
            || row.installer_starttime != Some(installer_starttime)
        {
            return Err(Error::rejected(
                "installer peer is not the owner-launched/enrolled installer for this allocation",
            ));
        }
        let generation = row
            .installer_generation
            .clone()
            .ok_or_else(|| Error::rejected("allocation has no enrolled installer generation"))?;
        self.require_installer_generation(&generation)?;
        Ok(generation)
    }

    /// `measure_challenge_installer_v1` — challenge-only measurement of the
    /// actual enrolled installer for the exact allocating intent. The signed
    /// proof is usable only by `GrantCore::challenge`; it conveys no install,
    /// consume, start or custody authority.
    fn measure_challenge_installer(
        &self,
        stream: &UnixStream,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let m = strict::object(
            args,
            &[
                "allocation_id",
                "launch_request_digest",
                "installer_pid",
                "installer_starttime",
                "request_nonce",
                "supervisor_generation",
            ],
        )?;
        let supervisor_generation = strict::string(m, "supervisor_generation")?;
        let measured = self.require_supervisor(stream, supervisor_generation)?;
        let allocation_id = strict::string(m, "allocation_id")?;
        if !strict::is_uuid(allocation_id) {
            return Err(Error::rejected("allocation_id must be a UUID"));
        }
        let launch_request_digest = strict::string(m, "launch_request_digest")?;
        strict::digest(launch_request_digest)?;
        let installer_pid = peer_pid(m, "installer_pid")?;
        let installer_starttime = strict::positive_u64(m, "installer_starttime")?;
        let nonce = strict::string(m, "request_nonce")?;
        proof::validate_nonce(nonce)?;

        let row = self.store.get(allocation_id)?;
        if row.phase != super::store::PHASE_ALLOCATING {
            return Err(Error::conflict_coded(
                "owner_phase_conflict",
                "runtime is not allocating",
            ));
        }
        if row.launch_request_digest.as_deref() != Some(launch_request_digest) {
            return Err(Error::conflict_coded(
                "owner_launch_mismatch",
                "launch request digest is not the committed intent",
            ));
        }
        self.require_allocation_recipient(&row, &measured)?;
        let installer_generation =
            self.require_enrolled_installer(&row, installer_pid, installer_starttime)?;
        if row.owner_epoch != self.store.owner_epoch()? {
            return Err(Error::conflict_coded(
                "owner_epoch_changed",
                "allocation belongs to a previous owner epoch",
            ));
        }
        proof::sign_challenge_peer_proof(
            &self.signer,
            &self.profile,
            row.owner_epoch,
            nonce,
            allocation_id,
            launch_request_digest,
            &measured,
            supervisor_generation,
            installer_pid,
            installer_starttime,
            &installer_generation,
        )
    }

    /// `measure_installer_v1` — the prepared installer proof. Only the exact
    /// prepared obligation, its enrolled supervisor recipient and its enrolled
    /// installer are accepted for the exact canonical challenge digest.
    fn measure_installer(
        &self,
        stream: &UnixStream,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let m = strict::object(
            args,
            &[
                "challenge_digest",
                "installer_pid",
                "installer_starttime",
                "request_nonce",
                "supervisor_generation",
            ],
        )?;
        let supervisor_generation = strict::string(m, "supervisor_generation")?;
        let measured = self.require_supervisor(stream, supervisor_generation)?;
        let challenge_digest = strict::string(m, "challenge_digest")?;
        strict::digest(challenge_digest)?;
        let installer_pid = peer_pid(m, "installer_pid")?;
        let installer_starttime = strict::positive_u64(m, "installer_starttime")?;
        let nonce = strict::string(m, "request_nonce")?;
        proof::validate_nonce(nonce)?;

        let row = self
            .store
            .by_challenge(challenge_digest)?
            .ok_or_else(|| Error::rejected("no prepared obligation for this challenge"))?;
        if row.phase != super::store::PHASE_PREPARED {
            return Err(Error::rejected(
                "obligation is not prepared — the installer proof refuses",
            ));
        }
        if row.owner_epoch != self.store.owner_epoch()? {
            return Err(Error::conflict_coded(
                "owner_epoch_changed",
                "obligation belongs to a previous owner epoch",
            ));
        }
        self.require_allocation_recipient(&row, &measured)?;
        let installer_generation =
            self.require_enrolled_installer(&row, installer_pid, installer_starttime)?;
        proof::sign_installer_proof(
            &self.signer,
            &self.profile,
            row.owner_epoch,
            nonce,
            challenge_digest,
            &measured,
            installer_pid,
            installer_starttime,
            &installer_generation,
        )
    }

    /// `prepare_runtime` — authenticated allocating intent. The owner
    /// allocates the runtime, commits the exact canonical LaunchRequest/digest,
    /// the measured supervisor recipient and the actual owner-launched pinned
    /// installer; the canonical challenge stays null until `enroll_pending`.
    fn prepare(&self, stream: &UnixStream, args: &serde_json::Value) -> Result<serde_json::Value> {
        let m = strict::object(
            args,
            &[
                "stable_agent_uid",
                "session_binding",
                "server_generation",
                "profile_digest",
                "expected_owner_epoch",
                "supervisor_generation",
            ],
        )?;
        let agent_uid = strict::string(m, "stable_agent_uid")?;
        if !is_agent_uid(agent_uid) {
            return Err(Error::rejected("stable_agent_uid is not agt-<32 hex>"));
        }
        let session_binding = strict::string(m, "session_binding")?;
        if session_binding.is_empty()
            || session_binding.len() > 200
            || session_binding.bytes().any(|b| b.is_ascii_control())
        {
            return Err(Error::rejected("session_binding is malformed"));
        }
        let server_generation = strict::positive_u64(m, "server_generation")?;
        let profile_digest = strict::string(m, "profile_digest")?;
        strict::digest(profile_digest)?;
        if profile_digest != self.profile.digest() {
            return Err(Error::rejected(
                "requested profile digest is not this deployment's profile",
            ));
        }
        let expected_epoch = strict::positive_u64(m, "expected_owner_epoch")?;
        let supervisor_generation = strict::string(m, "supervisor_generation")?;
        // The cold order is hello/enrollment → prepare. A process that has not
        // completed `hello_v1` cannot earn enrollment through `prepare`.
        let measured = self.require_supervisor(stream, supervisor_generation)?;

        let epoch = self.store.owner_epoch()?;
        if expected_epoch != epoch {
            return Err(Error::conflict_coded(
                "owner_epoch_changed",
                "expected owner epoch is not current",
            ));
        }
        let (uid_start, uid_end) = self.profile.uid_range();
        let allocation = self.store.allocate(AllocParams {
            workspace_id: self.profile.workspace_id(),
            agent_uid,
            session_binding,
            server_generation,
            expected_owner_epoch: epoch,
            database_epoch: self.profile.database_epoch(),
            profile_digest: self.profile.digest(),
            max_runtimes: self.profile.max_runtimes(),
            uid_start,
            uid_end,
            run_dir: self.profile.run_dir(),
        })?;
        let launch_bytes = proof::launch_request_bytes(
            &self.profile,
            &allocation.runtime_id,
            allocation.generation,
            &allocation.operation_id,
        )?;
        let launch_request_digest = proof::launch_request_digest(&launch_bytes);
        let launch_request_json = String::from_utf8(launch_bytes)
            .map_err(|_| Error::internal("launch request is not UTF-8"))?;
        let recipient_nonce = uuid::Uuid::new_v4().to_string();
        // The pinned installer is launched by the owner itself. Its capsule is
        // withheld until the allocating intent is committed, so the installer
        // cannot race the challenge handshake.
        let (installer_pid, installer_starttime) = match self.launch_installer(&allocation) {
            Ok(launched) => launched,
            Err(e) => {
                let _ = self.store.fence(
                    &allocation.runtime_id,
                    allocation.generation,
                    "prepare did not complete",
                );
                return Err(e);
            }
        };
        if let Err(e) = self.store.set_launch_intent(
            &allocation.runtime_id,
            &launch_request_json,
            &launch_request_digest,
            measured.pid,
            measured.starttime,
            supervisor_generation,
            &recipient_nonce,
            installer_pid,
            installer_starttime,
            &self.profile.installer().generation,
        ) {
            self.kill_installer(&allocation.runtime_id);
            let _ = self.store.fence(
                &allocation.runtime_id,
                allocation.generation,
                "launch intent was not committed",
            );
            return Err(e);
        }
        if let Err(e) = self
            .stage_installer_capsule(&allocation.runtime_id)
            .and_then(|()| self.prepare_container(&allocation))
        {
            self.kill_installer(&allocation.runtime_id);
            let _ = self.store.fence(
                &allocation.runtime_id,
                allocation.generation,
                "prepare did not complete",
            );
            return Err(e);
        }
        let launch_request: serde_json::Value = serde_json::from_str(&launch_request_json)
            .map_err(|_| Error::internal("launch request is not valid JSON"))?;
        Ok(PreparedRuntime {
            runtime_id: allocation.runtime_id.clone(),
            operation_id: allocation.operation_id.clone(),
            generation: allocation.generation,
            owner_epoch: allocation.owner_epoch,
            os_uid: allocation.os_uid,
            os_gid: allocation.os_gid,
            container_name: allocation.container_name.clone(),
            ops_socket: PathBuf::from(&allocation.ops_socket),
            attest_socket: PathBuf::from(&allocation.attest_socket),
            launch_request,
            launch_request_digest,
            recipient_pid: measured.pid,
            recipient_starttime: measured.starttime,
            recipient_generation: supervisor_generation.to_string(),
            recipient_nonce,
            installer_pid,
            installer_starttime,
            installer_generation: self.profile.installer().generation.clone(),
            challenge: None,
            challenge_digest: None,
            agent_uid: agent_uid.to_string(),
            session_binding: session_binding.to_string(),
            server_generation,
            profile_digest: self.profile.digest().to_string(),
        }
        .to_json())
    }

    /// Spawn only the fixed pinned installer entry, with a cleared environment
    /// and no caller-selected argv. The child's stdin stays open until
    /// `release_installer_v1` writes the staged capsule. The owner measures the
    /// actual child: root uid, pinned executable digest, stable starttime.
    #[allow(clippy::disallowed_methods)] // the pinned installer is launched with a fixed argv
    fn launch_installer(&self, allocation: &super::store::Allocation) -> Result<(u32, u64)> {
        if self.profile.profile_path() != Path::new(super::PROFILE_PATH) {
            return Err(Error::rejected(
                "owner-launched installer requires the fixed production profile path",
            ));
        }
        let fresh = LocalProfile::load_production()?;
        if fresh.digest() != self.profile.digest() {
            return Err(Error::rejected(
                "fixed production profile bytes changed since the owner loaded them",
            ));
        }
        let path = Path::new(&self.profile.installer().path);
        let mut child = Command::new(path)
            .arg(super::INSTALLER_LAUNCH_ARG)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| Error::unknown("pinned installer could not be launched"))?;
        let pid = child.id();
        let starttime = match peer::starttime(pid) {
            Some(value) => value,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::unknown("installer starttime is unreadable"));
            }
        };
        if let Err(e) = proof::installer_measurement(&self.profile, pid, starttime) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        let mut guard = self
            .installers
            .lock()
            .map_err(|_| Error::internal("installer registry mutex is poisoned"))?;
        if guard.contains_key(&allocation.runtime_id) {
            drop(guard);
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::conflict_coded(
                "owner_installer_conflict",
                "an installer child is already enrolled for this runtime",
            ));
        }
        guard.insert(
            allocation.runtime_id.clone(),
            InstallerChild {
                child,
                pid,
                starttime,
                started_at: std::time::Instant::now(),
                capsule: None,
                released: false,
            },
        );
        Ok((pid, starttime))
    }

    /// Build and stage (but do not write) the exact capsule for this committed
    /// allocation. The challenge is the same `expected_challenge` derivation
    /// `enroll_pending` validates; the profile digest binds the exact bytes the
    /// installer will load.
    fn stage_installer_capsule(&self, runtime_id: &str) -> Result<()> {
        let row = self.store.get(runtime_id)?;
        let challenge = self.expected_challenge(&row)?;
        let capsule = serde_json::json!({
            "v": 1,
            "kind": "owner_launched_install",
            "runtime_id": &row.runtime_id,
            "operation_id": &row.operation_id,
            "generation": row.generation,
            "installer_generation": row
                .installer_generation
                .as_deref()
                .ok_or_else(|| Error::internal("installer generation is not committed"))?,
            "recipient_generation": row
                .recipient_generation
                .as_deref()
                .ok_or_else(|| Error::internal("recipient generation is not committed"))?,
            "recipient_nonce": row
                .recipient_nonce
                .as_deref()
                .ok_or_else(|| Error::internal("recipient nonce is not committed"))?,
            "profile_digest": self.profile.digest(),
            "grant_socket": self.profile.supervisor_grant_socket().to_string_lossy(),
            "challenge": canonical_challenge_value(&challenge),
        });
        let mut bytes = serde_json::to_vec(&capsule)
            .map_err(|_| Error::internal("installer capsule cannot be encoded"))?;
        if bytes.len() + 1 > MAX_FRAME_BYTES {
            return Err(Error::internal("installer capsule exceeds the bound"));
        }
        bytes.push(b'\n');
        let mut guard = self
            .installers
            .lock()
            .map_err(|_| Error::internal("installer registry mutex is poisoned"))?;
        let entry = guard
            .get_mut(runtime_id)
            .ok_or_else(|| Error::internal("installer child is not enrolled"))?;
        if entry.released || entry.capsule.is_some() {
            return Err(Error::conflict_coded(
                "owner_installer_conflict",
                "installer capsule is already staged",
            ));
        }
        entry.capsule = Some(bytes);
        Ok(())
    }

    /// Reap exited children and kill only a child that exceeded its absolute
    /// outer lifetime. A live installer is never killed by a normal start.
    fn reap_installers(&self) {
        let Ok(mut guard) = self.installers.lock() else {
            return;
        };
        let mut gone = Vec::new();
        for (id, entry) in guard.iter_mut() {
            match entry.child.try_wait() {
                Ok(Some(_)) => gone.push(id.clone()),
                Ok(None) if entry.started_at.elapsed() > std::time::Duration::from_secs(120) => {
                    let _ = entry.child.kill();
                    let _ = entry.child.wait();
                    gone.push(id.clone());
                }
                _ => {}
            }
        }
        for id in gone {
            let _ = guard.remove(&id);
        }
    }

    fn kill_installer(&self, runtime_id: &str) {
        if let Ok(mut guard) = self.installers.lock() {
            if let Some(mut entry) = guard.remove(runtime_id) {
                let _ = entry.child.kill();
                let _ = entry.child.wait();
            }
        }
    }

    fn kill_all_installers(&self) {
        if let Ok(mut guard) = self.installers.lock() {
            for (_, mut entry) in guard.drain() {
                let _ = entry.child.kill();
                let _ = entry.child.wait();
            }
        }
    }

    fn challenge_pins(&self) -> Pins {
        Pins {
            source: self.profile.image().source.clone(),
            image: self.profile.image().reference.clone(),
            helper: self.profile.image().helper.clone(),
            node: self.profile.image().node.clone(),
            pi_graph: self.profile.image().pi_graph.clone(),
            policy: self.profile.image().policy.clone(),
        }
    }

    fn challenge_lineage(&self) -> Lineage {
        Lineage {
            reference: self.profile.lineage_reference().to_string(),
            database_epoch: self.profile.database_epoch(),
        }
    }

    /// Build dirs and create the deterministic owned container. The canonical
    /// challenge does not exist yet; it is bound once by `enroll_pending`.
    fn prepare_container(&self, allocation: &super::store::Allocation) -> Result<()> {
        spec::prepare_runtime_dir(
            self.profile.run_dir(),
            &allocation.runtime_id,
            self.profile.supervisor().gid,
            Path::new(&allocation.ops_socket),
            Path::new(&allocation.attest_socket),
        )?;
        let runtime_spec = RuntimeSpec {
            runtime_id: allocation.runtime_id.clone(),
            generation: allocation.generation,
            owner_epoch: allocation.owner_epoch,
            nonce: allocation.operation_id.clone(),
            os_uid: allocation.os_uid,
            os_gid: allocation.os_gid,
            container_name: allocation.container_name.clone(),
            ops_socket: PathBuf::from(&allocation.ops_socket),
            attest_socket: PathBuf::from(&allocation.attest_socket),
        };
        let body = spec::create_body(&self.profile, &runtime_spec)?;
        let created = self
            .docker
            .create(&allocation.container_name, &body)
            .map_err(|_| {
                Error::unknown(
                    "owned container create was not positively acknowledged; the obligation is fenced",
                )
            })?;
        self.store
            .record_container(&allocation.runtime_id, &created.id)
    }

    fn spec_for(&self, row: &Obligation) -> RuntimeSpec {
        RuntimeSpec {
            runtime_id: row.runtime_id.clone(),
            generation: row.generation,
            owner_epoch: row.owner_epoch,
            nonce: row.operation_id.clone(),
            os_uid: row.os_uid,
            os_gid: row.os_gid,
            container_name: row.container_name.clone(),
            ops_socket: PathBuf::from(&row.ops_socket),
            attest_socket: PathBuf::from(&row.attest_socket),
        }
    }

    /// The single owner-side derivation of the exact canonical challenge for a
    /// committed allocation. `enroll_pending` compares the supervisor's bytes
    /// to this value; the release capsule carries the same value to the
    /// installer. It adds no second challenge grammar.
    fn expected_challenge(&self, row: &Obligation) -> Result<SupervisorChallenge> {
        let launch_json = row
            .launch_request_json
            .as_deref()
            .ok_or_else(|| Error::rejected("allocation has no committed launch request"))?;
        let launch_value: serde_json::Value = serde_json::from_str(launch_json)
            .map_err(|_| Error::internal("stored launch request is not JSON"))?;
        let launch_request = parse_request(&launch_value)?;
        let recipient_pid = row
            .recipient_pid
            .ok_or_else(|| Error::rejected("allocation has no measured supervisor recipient"))?;
        let recipient_starttime = row
            .recipient_starttime
            .ok_or_else(|| Error::rejected("allocation has no measured supervisor recipient"))?;
        let recipient_generation = row
            .recipient_generation
            .clone()
            .ok_or_else(|| Error::rejected("allocation has no measured supervisor recipient"))?;
        let recipient_nonce = row
            .recipient_nonce
            .clone()
            .ok_or_else(|| Error::rejected("allocation has no recipient nonce"))?;
        Ok(SupervisorChallenge {
            launch: LaunchBinding {
                epoch: row.owner_epoch,
                request: launch_request,
            },
            recipient: Recipient {
                pid: recipient_pid,
                starttime: recipient_starttime.to_string(),
                generation: recipient_generation,
                nonce: recipient_nonce,
            },
            pins: self.challenge_pins(),
            lineage: self.challenge_lineage(),
        })
    }

    fn enroll_pending(
        &self,
        stream: &UnixStream,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let m = strict::object(args, &["runtime_id", "challenge", "supervisor_generation"])?;
        let runtime_id = strict::string(m, "runtime_id")?;
        let supervisor_generation = strict::string(m, "supervisor_generation")?;
        let measured = self.require_supervisor(stream, supervisor_generation)?;
        let challenge_value = m
            .get("challenge")
            .ok_or_else(|| Error::rejected("challenge missing"))?;
        let provided = parse_challenge_bytes(
            &serde_json::to_vec(challenge_value)
                .map_err(|_| Error::rejected("challenge is not encodable"))?,
        )?;
        let row = self.store.get(runtime_id)?;
        self.require_allocation_recipient(&row, &measured)?;
        if row.recipient_generation.as_deref() != Some(supervisor_generation) {
            return Err(Error::rejected(
                "allocation recipient generation is not this supervisor",
            ));
        }
        if row.phase != super::store::PHASE_ALLOCATING {
            return Err(Error::conflict_coded(
                "owner_phase_conflict",
                "runtime is not allocating",
            ));
        }
        let expected = self.expected_challenge(&row)?;
        let expected_bytes = canonical_challenge_bytes(&expected)?;
        if canonical_challenge_bytes(&provided)? != expected_bytes {
            return Err(Error::conflict_coded(
                "owner_challenge_mismatch",
                "enrolled challenge is not the exactly allocated launch request/recipient",
            ));
        }
        let digest = super::store::challenge_digest(&expected_bytes);
        let challenge_json = String::from_utf8(expected_bytes)
            .map_err(|_| Error::internal("challenge is not UTF-8"))?;
        self.store
            .set_challenge(runtime_id, &challenge_json, &digest)?;
        self.store.mark_prepared(runtime_id, &digest)?;
        Ok(serde_json::json!({
            "runtime_id": runtime_id,
            "phase": "prepared",
            "challenge_digest": digest,
        }))
    }

    /// `release_installer_v1` — deliver the staged capsule to the exact
    /// owner-launched installer for the committed allocation. Authenticated by
    /// the measured/enrolled supervisor and the exact row/profile/epoch. This
    /// only writes bytes to the owner's own child; it grants no install,
    /// consume, start or launch evidence.
    fn release_installer(
        &self,
        stream: &UnixStream,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let m = strict::object(
            args,
            &[
                "runtime_id",
                "operation_id",
                "generation",
                "supervisor_generation",
            ],
        )?;
        let supervisor_generation = strict::string(m, "supervisor_generation")?;
        let measured = self.require_supervisor(stream, supervisor_generation)?;
        let runtime_id = strict::string(m, "runtime_id")?;
        if !strict::is_uuid(runtime_id) {
            return Err(Error::rejected("runtime_id must be a UUID"));
        }
        let operation_id = strict::string(m, "operation_id")?;
        if !strict::is_uuid(operation_id) {
            return Err(Error::rejected("operation_id must be a UUID"));
        }
        let generation = strict::positive_u64(m, "generation")?;
        let row = self.store.get(runtime_id)?;
        if row.phase != super::store::PHASE_ALLOCATING {
            return Err(Error::conflict_coded(
                "owner_phase_conflict",
                "runtime is not allocating",
            ));
        }
        if row.operation_id != operation_id || row.generation != generation {
            return Err(Error::conflict_coded(
                "owner_launch_mismatch",
                "release does not name the committed allocation",
            ));
        }
        if row.profile_digest != self.profile.digest() {
            return Err(Error::rejected(
                "allocation profile is not this owner profile",
            ));
        }
        if row.owner_epoch != self.store.owner_epoch()? {
            return Err(Error::conflict_coded(
                "owner_epoch_changed",
                "allocation belongs to a previous owner epoch",
            ));
        }
        self.require_allocation_recipient(&row, &measured)?;
        if row.recipient_generation.as_deref() != Some(supervisor_generation) {
            return Err(Error::rejected(
                "allocation recipient generation is not this supervisor",
            ));
        }
        let installer_pid = row
            .installer_pid
            .ok_or_else(|| Error::rejected("allocation has no enrolled installer"))?;
        let installer_starttime = row
            .installer_starttime
            .ok_or_else(|| Error::rejected("allocation has no enrolled installer"))?;
        let installer_generation = row
            .installer_generation
            .clone()
            .ok_or_else(|| Error::rejected("allocation has no enrolled installer generation"))?;
        if installer_generation != self.profile.installer().generation {
            return Err(Error::rejected(
                "allocation installer generation is not the pinned one",
            ));
        }
        let fresh = LocalProfile::load_production()?;
        if fresh.digest() != self.profile.digest() {
            return Err(Error::rejected(
                "fixed production profile bytes changed before delivery",
            ));
        }

        let mut remove = false;
        let outcome: Result<()> =
            {
                let mut guard = self
                    .installers
                    .lock()
                    .map_err(|_| Error::internal("installer registry mutex is poisoned"))?;
                let Some(entry) = guard.get_mut(runtime_id) else {
                    return Err(Error::rejected("installer child is not enrolled"));
                };
                if entry.released {
                    return Err(Error::conflict_coded(
                        "owner_installer_conflict",
                        "installer capsule was already released",
                    ));
                }
                if entry.pid != installer_pid || entry.starttime != installer_starttime {
                    return Err(Error::rejected("installer child is not the enrolled one"));
                }
                if entry.capsule.is_none() {
                    return Err(Error::internal("installer capsule is not staged"));
                }
                match entry.child.try_wait() {
                    Ok(Some(_)) => {
                        remove = true;
                        Err(Error::unknown("installer exited before capsule delivery"))
                    }
                    Ok(None) => {
                        if let Err(e) =
                            proof::installer_measurement(&self.profile, entry.pid, entry.starttime)
                        {
                            remove = true;
                            Err(e)
                        } else {
                            let bytes = entry.capsule.take().ok_or_else(|| {
                                Error::internal("installer capsule is not staged")
                            })?;
                            let write = (|| -> Result<()> {
                                let stdin = entry.child.stdin.as_mut().ok_or_else(|| {
                                    Error::internal("installer stdin is not held")
                                })?;
                                stdin.write_all(&bytes).map_err(|_| {
                                    Error::unknown("installer capsule could not be delivered")
                                })?;
                                stdin.flush().map_err(|_| {
                                    Error::unknown("installer capsule could not be delivered")
                                })?;
                                entry.child.stdin.take();
                                entry.released = true;
                                Ok(())
                            })();
                            if write.is_err() {
                                remove = true;
                            }
                            write
                        }
                    }
                    Err(_) => {
                        remove = true;
                        Err(Error::unknown("installer child state is unreadable"))
                    }
                }
            };
        if remove {
            if let Ok(mut guard) = self.installers.lock() {
                if let Some(mut entry) = guard.remove(runtime_id) {
                    let _ = entry.child.kill();
                    let _ = entry.child.wait();
                }
            }
        }
        if let Err(e) = outcome {
            let _ = self
                .store
                .fence(runtime_id, generation, "installer capsule delivery failed");
            return Err(e);
        }
        Ok(serde_json::json!({"v": 1, "released": true}))
    }

    fn digest_from_challenge(&self, challenge: &serde_json::Value) -> Result<String> {
        let canonical = canonical_challenge_bytes(&parse_challenge_bytes(
            &serde_json::to_vec(challenge)
                .map_err(|_| Error::rejected("challenge is not encodable"))?,
        )?)?;
        Ok(super::store::challenge_digest(&canonical))
    }

    fn grant_enrollment_status(
        &self,
        stream: &UnixStream,
        args: &serde_json::Value,
        allow_consumed: bool,
    ) -> Result<serde_json::Value> {
        let m = strict::object(args, &["challenge", "supervisor_generation"])?;
        let supervisor_generation = strict::string(m, "supervisor_generation")?;
        let measured = self.require_supervisor(stream, supervisor_generation)?;
        let challenge = m
            .get("challenge")
            .ok_or_else(|| Error::rejected("challenge missing"))?;
        let digest = self.digest_from_challenge(challenge)?;
        if let Some(row) = self.store.by_challenge(&digest)? {
            self.require_allocation_recipient(&row, &measured)?;
        }
        let epoch = self.store.owner_epoch()?;
        let enrolled = if allow_consumed {
            self.store.recheck_grant(&digest, epoch)?
        } else {
            self.store.enrolled(&digest, epoch)?
        };
        Ok(serde_json::json!({"enrolled": enrolled, "owner_epoch": epoch}))
    }

    fn consume(&self, stream: &UnixStream, args: &serde_json::Value) -> Result<serde_json::Value> {
        let m = strict::object(args, &["challenge", "supervisor_generation"])?;
        let supervisor_generation = strict::string(m, "supervisor_generation")?;
        let measured = self.require_supervisor(stream, supervisor_generation)?;
        let challenge = m
            .get("challenge")
            .ok_or_else(|| Error::rejected("challenge missing"))?;
        let digest = self.digest_from_challenge(challenge)?;
        if let Some(row) = self.store.by_challenge(&digest)? {
            self.require_allocation_recipient(&row, &measured)?;
        }
        let outcome = self.store.consume_by_challenge(&digest)?;
        Ok(serde_json::json!({"consumed": outcome == ConsumeOutcome::Consumed}))
    }

    fn start_runtime(
        &self,
        stream: &UnixStream,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let m = strict::object(
            args,
            &["runtime_id", "expected_generation", "supervisor_generation"],
        )?;
        let supervisor_generation = strict::string(m, "supervisor_generation")?;
        self.require_supervisor(stream, supervisor_generation)?;
        let runtime_id = strict::string(m, "runtime_id")?;
        let expected = strict::positive_u64(m, "expected_generation")?;
        let row = self.store.get(runtime_id)?;
        if row.generation != expected {
            return Err(Error::conflict_coded(
                "owner_generation_changed",
                "expected generation is not current",
            ));
        }
        if row.phase != super::store::PHASE_CONSUMED {
            return Err(Error::rejected(
                "runtime is not positively consumed — start refuses",
            ));
        }
        let epoch = self.store.owner_epoch()?;
        if row.owner_epoch != epoch {
            return Err(Error::conflict_coded(
                "owner_epoch_changed",
                "runtime was authorized under a different owner epoch",
            ));
        }
        spec::seal_socket_path(Path::new(&row.ops_socket), row.os_uid, row.os_gid)?;
        let key = attestation::random32()?;
        let challenge_bytes = attestation::random32()?;
        let challenge_hex = attestation::hex32(&challenge_bytes);
        let listener =
            attestation::bind_attestation(Path::new(&row.attest_socket), row.os_uid, row.os_gid)?;
        self.store.mark_starting(runtime_id, expected)?;
        let started = self
            .docker
            .start(&row.container_name)
            .and_then(|()| {
                // Reconcile a lost start acknowledgement by exact owned name.
                self.docker.inspect(&row.container_name)
            })
            .map_err(|_| Error::unknown("container start was not positively acknowledged"));
        if started.is_err() {
            let _ = self
                .store
                .fence(runtime_id, expected, "container start unconfirmed");
            return Err(Error::unknown(
                "container start unconfirmed — obligation kept non-executable",
            ));
        }
        let spec = self.spec_for(&row);
        let measurement = match measure::measure_started(
            &self.profile,
            &spec,
            &self.docker,
            &self.engine_mount_root,
        ) {
            Ok(m) => m,
            Err(_) => {
                let _ = self
                    .store
                    .fence(runtime_id, expected, "post-start measurement failed");
                return Err(Error::unknown("post-start measurement refused readiness"));
            }
        };
        self.store.mark_observing(
            runtime_id,
            expected,
            &key,
            measurement.node_pid,
            measurement.node_starttime,
        )?;
        let expectation = attestation::AttestationExpectation {
            runtime_id: runtime_id.to_string(),
            generation: expected,
            os_uid: row.os_uid,
            node_pid: measurement.node_pid,
            node_starttime: measurement.node_starttime,
            key,
            challenge_hex,
            argv_digest: measurement.argv_digest.clone(),
            env_digest: measurement.env_digest.clone(),
        };
        spawn_observer(
            listener,
            expectation,
            Arc::clone(&self.store),
            self.profile.clone(),
            runtime_id.to_string(),
            expected,
        );
        Ok(StartOutcome {
            runtime_id: runtime_id.to_string(),
            generation: expected,
            owner_epoch: row.owner_epoch,
            phase: super::store::PHASE_OBSERVING.to_string(),
            node_pid: measurement.node_pid,
            node_starttime: measurement.node_starttime,
            container_id: measurement.container_id,
        }
        .to_json())
    }

    fn recheck(&self, stream: &UnixStream, args: &serde_json::Value) -> Result<serde_json::Value> {
        let m = strict::object(
            args,
            &["runtime_id", "expected_generation", "supervisor_generation"],
        )?;
        let supervisor_generation = strict::string(m, "supervisor_generation")?;
        self.require_supervisor(stream, supervisor_generation)?;
        let runtime_id = strict::string(m, "runtime_id")?;
        let expected = strict::positive_u64(m, "expected_generation")?;
        let row = self.store.get(runtime_id)?;
        if row.generation != expected {
            return Err(Error::conflict_coded(
                "owner_generation_changed",
                "expected generation is not current",
            ));
        }
        if !row.executable() {
            return Err(Error::rejected("runtime is not ready"));
        }
        let epoch = self.store.owner_epoch()?;
        if row.owner_epoch != epoch {
            return Err(Error::rejected("runtime owner epoch is not current"));
        }
        let observed = row
            .observed_registry_digest
            .as_deref()
            .ok_or_else(|| Error::rejected("runtime has no verified registry"))?;
        let registry_json = row
            .observed_registry_json
            .as_deref()
            .ok_or_else(|| Error::rejected("runtime has no verified registry payload"))?;
        let payload: serde_json::Value = serde_json::from_str(registry_json)
            .map_err(|_| Error::internal("stored registry payload is not JSON"))?;
        let verified = super::registry::verify_payload(&payload, &self.profile, row.generation)?;
        if verified.dynamic_digest() != observed
            || verified.stable_digest() != self.profile.registry().registry_digest
        {
            return Err(Error::rejected(
                "runtime registry is not the approved registry",
            ));
        }
        let container_id = row
            .container_id
            .clone()
            .ok_or_else(|| Error::rejected("runtime has no owned container"))?;
        let spec = self.spec_for(&row);
        let pid = row
            .node_pid
            .ok_or_else(|| Error::rejected("runtime has no measured init"))?;
        let starttime = row
            .node_starttime
            .ok_or_else(|| Error::rejected("runtime has no measured starttime"))?;
        measure::verify_live(
            &self.profile,
            &spec,
            &self.docker,
            &self.engine_mount_root,
            pid,
            starttime,
        )?;
        let identity = RuntimeIdentity {
            workspace_id: self.profile.workspace_id().to_string(),
            runtime_id: row.runtime_id.clone(),
            agent_uid: row.agent_uid.clone(),
            session_binding: row.session_binding.clone(),
            server_generation: row.server_generation,
            runtime_generation: row.generation,
            owner_epoch: row.owner_epoch,
            os_uid: row.os_uid,
            container_id,
            image_digest: self.profile.image().digest.clone(),
            profile_digest: self.profile.digest().to_string(),
            loaded_registry_digest: observed.to_string(),
            loaded_receipt_id: row
                .loaded_receipt_id
                .clone()
                .ok_or_else(|| Error::rejected("runtime has no loaded receipt"))?,
            node_pid: pid,
            node_starttime: starttime,
        };
        Ok(identity.to_json())
    }

    fn observe(&self, stream: &UnixStream, args: &serde_json::Value) -> Result<serde_json::Value> {
        let m = strict::object(args, &["runtime_id", "supervisor_generation"])?;
        let supervisor_generation = strict::string(m, "supervisor_generation")?;
        self.require_supervisor(stream, supervisor_generation)?;
        let runtime_id = strict::string(m, "runtime_id")?;
        let row = self.store.get(runtime_id)?;
        let qualified = row.executable();
        let status = RuntimeStatus {
            v: 1,
            runtime_id: row.runtime_id,
            generation: row.generation,
            owner_epoch: row.owner_epoch,
            database_epoch: self.store.database_epoch()?,
            phase: row.phase,
            agent_uid: row.agent_uid,
            session_binding: row.session_binding,
            os_uid: row.os_uid,
            qualified,
            node_pid: row.node_pid,
            loaded_registry_digest: row.observed_registry_digest,
            fenced_reason: row.fenced_reason,
        };
        serde_json::to_value(status).map_err(|_| Error::internal("status cannot be encoded"))
    }

    fn fence(&self, stream: &UnixStream, args: &serde_json::Value) -> Result<serde_json::Value> {
        let m = strict::object(
            args,
            &[
                "runtime_id",
                "expected_generation",
                "reason",
                "supervisor_generation",
            ],
        )?;
        let runtime_id = strict::string(m, "runtime_id")?;
        let expected = strict::positive_u64(m, "expected_generation")?;
        let reason = strict::string(m, "reason")?;
        let supervisor_generation = strict::string(m, "supervisor_generation")?;
        if reason.is_empty() || reason.len() > 200 {
            return Err(Error::rejected("fence reason is malformed"));
        }
        self.require_supervisor(stream, supervisor_generation)?;
        let row = self.store.get(runtime_id)?;
        if row.generation != expected {
            return Err(Error::conflict_coded(
                "owner_generation_changed",
                "expected generation is not current",
            ));
        }
        // 1. Owner fence first: stop new attestation/admission.
        self.store.fence(runtime_id, expected, reason)?;
        // 2. Take the broker generation writer fence and require its ack.
        let ack = self
            .broker_fence(runtime_id, row.owner_epoch, expected, reason)
            .map_err(|_| {
                Error::unknown(
                    "broker fence was not acknowledged — runtime remains revoking, never executable",
                )
            })?;
        // 3. Deterministically retire the owned container, then acknowledge.
        self.retire_owned(&row)
            .map_err(|_| Error::unknown("owned container retirement unconfirmed"))?;
        self.store.mark_revoked(runtime_id)?;
        self.kill_installer(runtime_id);
        serde_json::to_value(ack)
            .map_err(|_| Error::internal("fence acknowledgement cannot be encoded"))
    }

    /// `prestart_eligibility_v1` — the authenticated root-owner witness a
    /// protected store open/restore may validate against: workspace, owner and
    /// database epochs, restore state and the retained ledger watermark floor.
    /// Read-only; no caller field selects it.
    fn prestart_eligibility(
        &self,
        stream: &UnixStream,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let m = strict::object(args, &["supervisor_generation"])?;
        let supervisor_generation = strict::string(m, "supervisor_generation")?;
        self.require_supervisor(stream, supervisor_generation)?;
        let witness = self.store.prestart_eligibility()?;
        Ok(serde_json::json!({
            "v": 1,
            "workspace_id": witness.workspace_id,
            "owner_epoch": witness.owner_epoch,
            "database_epoch": witness.database_epoch,
            "restore_state": witness.restore_state,
            "min_sequence": witness.min_sequence,
            "min_digest": witness.min_digest
        }))
    }

    /// `open_console_v1` — attach exactly this owned runtime's Pi console.
    ///
    /// The caller must be the currently enrolled supervisor process (kernel
    /// peer re-measured), the runtime must be currently executable in this
    /// owner epoch, and the presented `recipient_nonce` must be the one the
    /// owner itself minted for this launch obligation. No container name,
    /// command, mount, URL or Docker flag is accepted; the container id comes
    /// from the owner's own row.
    fn open_console(
        &self,
        stream: &UnixStream,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let m = strict::object(
            args,
            &[
                "v",
                "runtime_id",
                "expected_generation",
                "recipient_nonce",
                "supervisor_generation",
            ],
        )?;
        if strict::safe_u64(m, "v")? != 1 {
            return Err(Error::rejected("open_console v must be exactly 1"));
        }
        let supervisor_generation = strict::string(m, "supervisor_generation")?;
        let measured = self.require_supervisor(stream, supervisor_generation)?;
        let runtime_id = strict::string(m, "runtime_id")?;
        let expected = strict::positive_u64(m, "expected_generation")?;
        let nonce = strict::string(m, "recipient_nonce")?;
        let row = self.store.get(runtime_id)?;
        if row.generation != expected {
            return Err(Error::conflict_coded(
                "owner_generation_changed",
                "expected generation is not current",
            ));
        }
        if !row.executable() {
            return Err(Error::rejected(
                "runtime is not ready — no console attachment",
            ));
        }
        if row.owner_epoch != self.store.owner_epoch()? {
            return Err(Error::rejected("runtime owner epoch is not current"));
        }
        if row.recipient_pid != Some(measured.pid)
            || row.recipient_starttime != Some(measured.starttime)
            || row.recipient_generation.as_deref() != Some(supervisor_generation)
        {
            return Err(Error::rejected(
                "console peer is not this runtime's enrolled recipient",
            ));
        }
        if row.recipient_nonce.as_deref() != Some(nonce) {
            return Err(Error::rejected(
                "console recipient nonce is not the enrolled launch nonce",
            ));
        }
        let container_id = row
            .container_id
            .clone()
            .ok_or_else(|| Error::rejected("runtime has no owned container"))?;
        let node_pid = row
            .node_pid
            .ok_or_else(|| Error::rejected("runtime has no measured init"))?;
        let node_starttime = row
            .node_starttime
            .ok_or_else(|| Error::rejected("runtime has no measured starttime"))?;
        let attach = self.docker.attach(&container_id)?;
        let token = super::attestation::hex32(&super::attestation::random32()?);
        let path = super::console::socket_path(self.profile.run_dir(), runtime_id);
        super::console::validate_path(self.profile.run_dir(), runtime_id, &path)?;
        super::console::spawn_owner(
            &path,
            self.profile.supervisor().gid,
            super::console::ConsoleGrant {
                supervisor_uid: self.profile.supervisor().uid,
                supervisor_pid: measured.pid,
                supervisor_starttime: measured.starttime,
                runtime_id: runtime_id.to_string(),
                generation: expected,
                token: token.clone(),
            },
            attach,
        )?;
        Ok(serde_json::json!({
            "v": 1,
            "runtime_id": runtime_id,
            "generation": expected,
            "console_path": path.to_string_lossy(),
            "token": token,
            "node_pid": node_pid,
            "node_starttime": node_starttime
        }))
    }

    fn checkpoint(
        &self,
        stream: &UnixStream,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let m = strict::object(
            args,
            &[
                "checkpoint",
                "prefix",
                "runtime_generation",
                "supervisor_generation",
            ],
        )?;
        let supervisor_generation = strict::string(m, "supervisor_generation")?;
        self.require_supervisor(stream, supervisor_generation)?;
        let _ = strict::positive_u64(m, "runtime_generation")?;
        let checkpoint: CheckpointV1 = serde_json::from_value(
            m.get("checkpoint")
                .ok_or_else(|| Error::rejected("checkpoint missing"))?
                .clone(),
        )
        .map_err(|_| Error::rejected("checkpoint is malformed"))?;
        let prefix: Vec<LedgerRecordV1> = serde_json::from_value(
            m.get("prefix")
                .ok_or_else(|| Error::rejected("prefix missing"))?
                .clone(),
        )
        .map_err(|_| Error::rejected("ledger prefix is malformed"))?;
        if prefix.len() > 64 {
            return Err(Error::rejected("ledger prefix exceeds the bound"));
        }
        let ack: CheckpointAck = self.store.checkpoint(&checkpoint, &prefix)?;
        serde_json::to_value(ack).map_err(|_| Error::internal("checkpoint ack cannot be encoded"))
    }

    /// Read-only receipt for an exact ledger head the owner already verified.
    /// No prefix, no watermark write, no checkpoint insert. Only the
    /// authenticated supervisor's current owner epoch/watermark can satisfy it.
    fn checkpoint_receipt_v1(
        &self,
        stream: &UnixStream,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let m = strict::object(args, &["checkpoint", "supervisor_generation"])?;
        let supervisor_generation = strict::string(m, "supervisor_generation")?;
        self.require_supervisor(stream, supervisor_generation)?;
        let checkpoint: CheckpointV1 = serde_json::from_value(
            m.get("checkpoint")
                .ok_or_else(|| Error::rejected("checkpoint missing"))?
                .clone(),
        )
        .map_err(|_| Error::rejected("checkpoint is malformed"))?;
        let ack = self.store.checkpoint_receipt(&checkpoint)?;
        serde_json::to_value(ack)
            .map_err(|_| Error::internal("checkpoint receipt cannot be encoded"))
    }

    fn restore_begin(
        &self,
        stream: &UnixStream,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let m = strict::object(
            args,
            &["checkpoint", "prefix", "reason", "supervisor_generation"],
        )?;
        let supervisor_generation = strict::string(m, "supervisor_generation")?;
        self.require_supervisor(stream, supervisor_generation)?;
        let reason = strict::string(m, "reason")?;
        if reason.is_empty() || reason.len() > 200 {
            return Err(Error::rejected("restore reason is malformed"));
        }
        let checkpoint: CheckpointV1 = serde_json::from_value(
            m.get("checkpoint")
                .ok_or_else(|| Error::rejected("checkpoint missing"))?
                .clone(),
        )
        .map_err(|_| Error::rejected("checkpoint is malformed"))?;
        let prefix: Vec<LedgerRecordV1> = serde_json::from_value(
            m.get("prefix")
                .ok_or_else(|| Error::rejected("prefix missing"))?
                .clone(),
        )
        .map_err(|_| Error::rejected("ledger prefix is malformed"))?;
        if prefix.len() > 64 {
            return Err(Error::rejected("ledger prefix exceeds the bound"));
        }
        let epoch = self.store.restore_begin(&checkpoint, &prefix, reason)?;
        self.kill_all_installers();
        Ok(serde_json::json!({"owner_epoch": epoch, "restore_state": "restoring"}))
    }

    fn restore_complete(
        &self,
        stream: &UnixStream,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let m = strict::object(args, &["expected_epoch", "supervisor_generation"])?;
        let supervisor_generation = strict::string(m, "supervisor_generation")?;
        self.require_supervisor(stream, supervisor_generation)?;
        let expected = strict::positive_u64(m, "expected_epoch")?;
        self.store.restore_complete(expected)?;
        Ok(serde_json::json!({"ok": true, "restore_state": "stable"}))
    }

    /// The owner→broker writer fence. The owner validates the supervisor-owned
    /// listening socket (inode uid/mode/accepting) and the connecting server's
    /// kernel identity, then sends a signed request and requires an exact ack.
    fn broker_fence(
        &self,
        runtime_id: &str,
        owner_epoch: u64,
        generation: u64,
        reason: &str,
    ) -> Result<FenceAck> {
        let path = self.profile.broker_fence_socket();
        let meta = std::fs::symlink_metadata(&path).map_err(|_| {
            Error::rejected("broker fence socket is missing — start the daemon listener")
        })?;
        if !meta.file_type().is_socket() {
            return Err(Error::rejected("broker fence path is not a socket"));
        }
        if meta.uid() != self.profile.supervisor().uid {
            return Err(Error::rejected(
                "broker fence socket is not owned by the supervisor account",
            ));
        }
        if meta.mode() & 0o077 != 0 {
            return Err(Error::rejected(
                "broker fence socket is accessible to group/other",
            ));
        }
        if !spec::is_listening(&path)? {
            return Err(Error::rejected("broker fence socket is not listening"));
        }
        let stream = UnixStream::connect(&path)
            .map_err(|_| Error::unknown("broker fence connect failed"))?;
        // The server is the supervisor account running the pinned binary; the
        // owner (root) can read its held exe fd.
        let supervisor = peer::admit_exe(
            &stream,
            self.profile.supervisor().uid,
            &self.supervisor_exe_hex(),
        )
        .map_err(|_| Error::unknown("broker fence peer is not the measured supervisor"))?;
        let _ = supervisor;
        let nonce = uuid::Uuid::new_v4().to_string();
        let signature = super::keys::sign_fence(
            &self.signer,
            runtime_id,
            owner_epoch,
            generation,
            reason,
            &nonce,
        )?;
        let request = serde_json::json!({
            "v": 1,
            "kind": "broker_fence",
            "runtime_id": runtime_id,
            "owner_epoch": owner_epoch,
            "expected_generation": generation,
            "reason": reason,
            "nonce": nonce,
            "signature": super::keys::b64(&signature),
        });
        let body = serde_json::to_vec(&request)
            .map_err(|_| Error::internal("fence request cannot be encoded"))?;
        frame::write_frame(&stream, &body, RESPONSE_BUDGET)?;
        let reply = frame::read_frame(&stream, REQUEST_BUDGET, MAX_FRAME_BYTES)?;
        let value = strict::parse(&reply)?;
        let envelope = strict::object(&value, &["ok", "data"])?;
        if !strict::boolean(envelope, "ok")? {
            return Err(Error::unknown("broker fence was refused"));
        }
        let data = envelope
            .get("data")
            .ok_or_else(|| Error::unknown("broker fence ack missing"))?;
        let m = strict::object(
            data,
            &[
                "v",
                "runtime_id",
                "owner_epoch",
                "generation",
                "nonce",
                "fenced_at_ms",
            ],
        )?;
        let ack_nonce = strict::string(m, "nonce")?;
        if strict::safe_u64(m, "v")? != 1
            || strict::string(m, "runtime_id")? != runtime_id
            || strict::safe_u64(m, "owner_epoch")? != owner_epoch
            || strict::safe_u64(m, "generation")? != generation
            || ack_nonce != nonce
        {
            return Err(Error::unknown(
                "broker fence ack does not bind the exact request",
            ));
        }
        Ok(FenceAck {
            v: 1,
            runtime_id: runtime_id.to_string(),
            owner_epoch,
            generation,
            nonce,
            fenced_at_ms: strict::safe_u64(m, "fenced_at_ms")?,
        })
    }

    /// Stop and remove only the exact owned container. A missing container is
    /// already retired; a label/name mismatch refuses (never stop a foreign
    /// object); an unconfirmed Docker failure keeps the obligation revoking.
    fn retire_owned(&self, row: &Obligation) -> Result<()> {
        let inspect = match self.docker.inspect(&row.container_name) {
            Ok(value) => value,
            Err(e) if e.kind() == "rejected" => return Ok(()),
            Err(_) => return Err(Error::unknown("container inspect unconfirmed")),
        };
        let labels = inspect
            .get("Config")
            .and_then(|c| c.get("Labels"))
            .and_then(|l| l.as_object())
            .ok_or_else(|| Error::unknown("owned container labels missing"))?;
        let label = |key: &str| labels.get(key).and_then(|v| v.as_str()).unwrap_or("");
        if label(spec::LABEL_RUNTIME) != row.runtime_id
            || label(spec::LABEL_NONCE) != row.operation_id
            || label(spec::LABEL_PROFILE) != self.profile.digest()
            || label(spec::LABEL_GENERATION) != row.generation.to_string()
        {
            return Err(Error::rejected(
                "container labels do not match the owned obligation",
            ));
        }
        let id = inspect
            .get("Id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::unknown("container id missing"))?;
        self.docker
            .stop(id)
            .map_err(|_| Error::unknown("container stop unconfirmed"))?;
        self.docker
            .remove(id)
            .map_err(|_| Error::unknown("container remove unconfirmed"))?;
        // The per-runtime console socket is owner-created state for this
        // runtime only; a retired runtime must not leave it attached.
        let console = super::console::socket_path(self.profile.run_dir(), &row.runtime_id);
        if let Ok(meta) = std::fs::symlink_metadata(&console) {
            use std::os::unix::fs::FileTypeExt;
            if meta.file_type().is_socket() && meta.uid() == 0 {
                let _ = std::fs::remove_file(&console);
            }
        }
        Ok(())
    }
}

/// Promote observing → ready after the measured observer's registry verifies,
/// then keep watching for drift.
fn spawn_observer(
    listener: UnixListener,
    expectation: attestation::AttestationExpectation,
    store: Arc<OwnerStore>,
    profile: LocalProfile,
    runtime_id: String,
    generation: u64,
) {
    std::thread::spawn(move || {
        let first = attestation::collect_first(&listener, &expectation, &profile, OBSERVER_BUDGET);
        let (stream, observation) = match first {
            Ok(value) => value,
            Err(_) => {
                let _ = store.fence(&runtime_id, generation, "no verified registry observation");
                return;
            }
        };
        if measure::verify_material(&profile, expectation.node_pid).is_err() {
            let _ = store.fence(
                &runtime_id,
                generation,
                "installed runtime material mismatch",
            );
            return;
        }
        let receipt_id = uuid::Uuid::new_v4().to_string();
        let payload_json = match serde_json::to_string(&observation.payload) {
            Ok(json) => json,
            Err(_) => {
                let _ = store.fence(&runtime_id, generation, "observation encoding failed");
                return;
            }
        };
        if store
            .record_observation(
                &runtime_id,
                generation,
                &expectation.key,
                observation.counter,
                &observation.digest,
                &payload_json,
                &receipt_id,
            )
            .is_err()
        {
            let _ = store.fence(&runtime_id, generation, "observation record refused");
            return;
        }
        let approved = profile.registry().registry_digest.clone();
        if observation.stable_digest != approved
            || store
                .mark_ready(&runtime_id, generation, &observation.verified)
                .is_err()
        {
            let _ = store.fence(&runtime_id, generation, "registry readiness refused");
            return;
        }
        attestation::watch(stream, expectation, observation.digest, store, runtime_id);
    });
}

/// A kernel pid field: positive, within the kernel pid_max bound.
fn peer_pid(m: &serde_json::Map<String, serde_json::Value>, key: &str) -> Result<u32> {
    let value = strict::positive_u64(m, key)?;
    if value > 4_194_304 {
        return Err(Error::rejected(format!("{key} is not a kernel pid")));
    }
    Ok(value as u32)
}

fn is_agent_uid(value: &str) -> bool {
    match value.strip_prefix("agt-") {
        Some(rest) => rest.len() == 32 && strict::is_lower_hex(rest, 32),
        None => false,
    }
}

// ─────────────────────── run dirs and the root CLI ───────────────────────

fn ensure_run_dirs(profile: &LocalProfile) -> Result<()> {
    let gid = profile.supervisor().gid;
    let run = profile.run_dir();
    for (dir, mode) in [
        (run.to_path_buf(), 0o750u32),
        (run.join("runtimes"), 0o750),
        (run.join("broker"), 0o770),
        (run.join("supervisor"), 0o770),
    ] {
        std::fs::create_dir_all(&dir)
            .map_err(|_| Error::internal("owner run directory cannot be created"))?;
        let meta = std::fs::symlink_metadata(&dir)
            .map_err(|_| Error::internal("owner run directory unreadable"))?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(Error::rejected(
                "owner run directory must be a real directory",
            ));
        }
        set_owner_mode(&dir, 0, gid, mode)?;
    }
    Ok(())
}

fn set_owner_mode(path: &Path, uid: u32, gid: u32, mode: u32) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::internal("path is not a C string"))?;
    if unsafe { libc::chown(c.as_ptr(), uid, gid) } != 0 {
        return Err(Error::internal("cannot set owner on owner path"));
    }
    let mut perms = std::fs::metadata(path)
        .map_err(|_| Error::internal("owner path metadata unreadable"))?
        .permissions();
    perms.set_mode(mode);
    std::fs::set_permissions(path, perms)
        .map_err(|_| Error::internal("cannot set mode on owner path"))?;
    Ok(())
}

fn bind_control(profile: &LocalProfile) -> Result<UnixListener> {
    let path = profile.control_socket();
    if path.exists() {
        let meta = std::fs::symlink_metadata(path)
            .map_err(|_| Error::internal("control socket metadata unreadable"))?;
        if !meta.file_type().is_socket() || meta.uid() != 0 {
            return Err(Error::rejected(
                "control socket path is not a stale root socket",
            ));
        }
        std::fs::remove_file(path)
            .map_err(|_| Error::internal("stale control socket cannot be replaced"))?;
    }
    let listener =
        UnixListener::bind(path).map_err(|_| Error::internal("control socket cannot be bound"))?;
    // Root owner + the configured supervisor *group* may connect (0660);
    // kernel uid/exe/enrollment admission is still exact per method, and no
    // guest has this group or a mount of the socket.
    set_owner_mode(path, 0, profile.supervisor().gid, 0o660)?;
    Ok(listener)
}

fn boot_id() -> Result<String> {
    let value = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map_err(|_| Error::rejected("kernel boot id is unavailable"))?;
    let value = value.trim().to_string();
    if !strict::is_uuid(&value) {
        return Err(Error::rejected("kernel boot id is malformed"));
    }
    Ok(value)
}

/// `cadence-runtime-owner [--config <absolute path>] [--check]`
pub(crate) fn run_cli() -> i32 {
    match run_cli_inner() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cadence-runtime-owner: {}", e);
            1
        }
    }
}

fn run_cli_inner() -> Result<i32> {
    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        return Err(Error::rejected(
            "cadence-runtime-owner must run as root (uid 0)",
        ));
    }
    let mut config: Option<PathBuf> = None;
    let mut check = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                let value = args
                    .next()
                    .ok_or_else(|| Error::usage("--config requires an absolute path"))?;
                config = Some(PathBuf::from(value));
            }
            "--check" => check = true,
            "--help" | "-h" => {
                println!(
                    "cadence-runtime-owner [--config <absolute path>] [--check]\n\
                     Root-owned protected runtime authority (native-oci-agent/1).\n\
                     --config validates and uses that root-owned profile instead of\n\
                     /var/lib/cadence/runtime-owner/profile.json.\n\
                     --check validates the profile, state database, owner key and\n\
                     Docker engine, prints a JSON summary and exits."
                );
                return Ok(0);
            }
            other => {
                return Err(Error::usage(format!("unknown argument {other}")));
            }
        }
    }
    let profile = match &config {
        Some(path) => LocalProfile::load_from(path)?,
        None => LocalProfile::load_production()?,
    };
    let boot = boot_id()?;
    ensure_run_dirs(&profile)?;
    let store = Arc::new(OwnerStore::open(
        profile.state_dir(),
        profile.workspace_id(),
        profile.database_epoch(),
        &boot,
    )?);
    super::store::verify_private_file(store.path())?;
    let signer = OwnerSigner::load(&profile)?;
    let docker = DockerClient::new(profile.docker_socket());
    docker.ping()?;
    let service = Arc::new(RuntimeOwnerService::new(
        profile.clone(),
        Arc::clone(&store),
        DockerClient::new(profile.docker_socket()),
        signer,
    )?);
    // Reconcile deterministic owned containers; failures leave obligations
    // non-executable rather than guessing.
    let reconcile = service.reconcile_owned();
    if check {
        println!(
            "{}",
            serde_json::json!({
                "v": 1,
                "profile_digest": profile.digest(),
                "workspace_id": profile.workspace_id(),
                "owner_epoch": store.owner_epoch()?,
                "database_epoch": store.database_epoch()?,
                "docker": "ok",
                "owner_key": "ok",
                "state_db": "ok",
                "reconcile": if reconcile.is_ok() { "ok" } else { "unconfirmed" },
            })
        );
        return Ok(0);
    }
    reconcile?;
    let listener = bind_control(&profile)?;
    println!(
        "{}",
        serde_json::json!({
            "v": 1,
            "ready": true,
            "control_socket": profile.control_socket().to_string_lossy(),
            "owner_epoch": store.owner_epoch()?,
        })
    );
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let service = Arc::clone(&service);
                std::thread::spawn(move || handle_control(service, stream));
            }
            Err(_) => continue,
        }
    }
    Ok(0)
}
