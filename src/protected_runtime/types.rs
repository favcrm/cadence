//! The exact wire replies of the root owner control socket.
//!
//! Both the owner and the daemon client construct/parse these with the strict
//! JSON helpers. No reply can create authority: a `PreparedRuntime` is an
//! allocation, a `StartOutcome` is an observation in progress, and only the
//! client-side recheck mints a `QualifiedRuntime`.

use std::path::PathBuf;

use crate::error::{Error, Result};

use super::strict;

#[derive(Clone, Debug)]
pub(crate) struct PreparedRuntime {
    pub(crate) runtime_id: String,
    /// The immutable owner-generated operation id — the canonical challenge id
    /// the installer correlates on.
    pub(crate) operation_id: String,
    pub(crate) generation: u64,
    pub(crate) owner_epoch: u64,
    pub(crate) os_uid: u32,
    pub(crate) os_gid: u32,
    pub(crate) container_name: String,
    pub(crate) ops_socket: PathBuf,
    pub(crate) attest_socket: PathBuf,
    /// The exact canonical LaunchRequest the owner committed in
    /// `prepare_runtime`. The supervisor parses these bounded bytes and builds
    /// the canonical challenge from them.
    pub(crate) launch_request: serde_json::Value,
    pub(crate) launch_request_digest: String,
    /// The measured supervisor recipient the owner bound to the intent.
    pub(crate) recipient_pid: u32,
    pub(crate) recipient_starttime: u64,
    pub(crate) recipient_generation: String,
    pub(crate) recipient_nonce: String,
    /// The actual owner-launched/enrolled pinned installer.
    pub(crate) installer_pid: u32,
    pub(crate) installer_starttime: u64,
    pub(crate) installer_generation: String,
    /// `None` while the obligation is allocating: the canonical challenge is
    /// bound once by `enroll_pending` after `challenge` returns.
    pub(crate) challenge: Option<serde_json::Value>,
    pub(crate) challenge_digest: Option<String>,
    pub(crate) agent_uid: String,
    pub(crate) session_binding: String,
    pub(crate) server_generation: u64,
    pub(crate) profile_digest: String,
}

impl PreparedRuntime {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "runtime_id": self.runtime_id,
            "operation_id": self.operation_id,
            "generation": self.generation,
            "owner_epoch": self.owner_epoch,
            "os_uid": self.os_uid,
            "os_gid": self.os_gid,
            "container_name": self.container_name,
            "ops_socket": self.ops_socket.to_string_lossy(),
            "attest_socket": self.attest_socket.to_string_lossy(),
            "launch_request": self.launch_request,
            "launch_request_digest": self.launch_request_digest,
            "recipient_pid": self.recipient_pid,
            "recipient_starttime": self.recipient_starttime,
            "recipient_generation": self.recipient_generation,
            "recipient_nonce": self.recipient_nonce,
            "installer_pid": self.installer_pid,
            "installer_starttime": self.installer_starttime,
            "installer_generation": self.installer_generation,
            "challenge": self.challenge,
            "challenge_digest": self.challenge_digest,
            "agent_uid": self.agent_uid,
            "session_binding": self.session_binding,
            "server_generation": self.server_generation,
            "profile_digest": self.profile_digest,
        })
    }

    pub(crate) fn from_json(v: &serde_json::Value) -> Result<Self> {
        let m = strict::object(
            v,
            &[
                "runtime_id",
                "operation_id",
                "generation",
                "owner_epoch",
                "os_uid",
                "os_gid",
                "container_name",
                "ops_socket",
                "attest_socket",
                "launch_request",
                "launch_request_digest",
                "recipient_pid",
                "recipient_starttime",
                "recipient_generation",
                "recipient_nonce",
                "installer_pid",
                "installer_starttime",
                "installer_generation",
                "challenge",
                "challenge_digest",
                "agent_uid",
                "session_binding",
                "server_generation",
                "profile_digest",
            ],
        )?;
        let operation_id = strict::string(m, "operation_id")?;
        if !strict::is_uuid(operation_id) {
            return Err(Error::rejected("operation_id must be a UUID"));
        }
        let launch_request = m
            .get("launch_request")
            .filter(|v| v.is_object())
            .cloned()
            .ok_or_else(|| Error::rejected("launch_request must be an object"))?;
        let hex32 = |key: &str| -> Result<String> {
            let value = strict::string(m, key)?;
            if !strict::is_lower_hex(value, 32) {
                return Err(Error::rejected(format!("{key} must be 32 lowercase hex")));
            }
            Ok(value.to_string())
        };
        let challenge = match m.get("challenge") {
            Some(serde_json::Value::Null) => None,
            Some(value) if value.is_object() => Some(value.clone()),
            _ => return Err(Error::rejected("challenge must be null or an object")),
        };
        let challenge_digest = match m.get("challenge_digest") {
            Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(d)) => {
                strict::digest(d)?;
                Some(d.clone())
            }
            _ => return Err(Error::rejected("challenge_digest must be null or a digest")),
        };
        if challenge.is_some() != challenge_digest.is_some() {
            return Err(Error::rejected(
                "challenge and challenge_digest must be present together",
            ));
        }
        Ok(Self {
            runtime_id: bounded(strict::string(m, "runtime_id")?, 200, "runtime_id")?,
            operation_id: operation_id.to_string(),
            generation: strict::positive_u64(m, "generation")?,
            owner_epoch: strict::positive_u64(m, "owner_epoch")?,
            os_uid: uid(m, "os_uid")?,
            os_gid: uid(m, "os_gid")?,
            container_name: bounded(strict::string(m, "container_name")?, 128, "container_name")?,
            ops_socket: absolute(strict::string(m, "ops_socket")?, "ops_socket")?,
            attest_socket: absolute(strict::string(m, "attest_socket")?, "attest_socket")?,
            launch_request,
            launch_request_digest: {
                let d = strict::string(m, "launch_request_digest")?;
                strict::digest(d)?;
                d.to_string()
            },
            recipient_pid: uid(m, "recipient_pid")?,
            recipient_starttime: strict::positive_u64(m, "recipient_starttime")?,
            recipient_generation: hex32("recipient_generation")?,
            recipient_nonce: {
                let nonce = strict::string(m, "recipient_nonce")?;
                if !strict::is_uuid(nonce) {
                    return Err(Error::rejected("recipient_nonce must be a UUID"));
                }
                nonce.to_string()
            },
            installer_pid: uid(m, "installer_pid")?,
            installer_starttime: strict::positive_u64(m, "installer_starttime")?,
            installer_generation: hex32("installer_generation")?,
            challenge,
            challenge_digest,
            agent_uid: bounded(strict::string(m, "agent_uid")?, 128, "agent_uid")?,
            session_binding: bounded(
                strict::string(m, "session_binding")?,
                200,
                "session_binding",
            )?,
            server_generation: strict::positive_u64(m, "server_generation")?,
            profile_digest: {
                let d = strict::string(m, "profile_digest")?;
                strict::digest(d)?;
                d.to_string()
            },
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct StartOutcome {
    pub(crate) runtime_id: String,
    pub(crate) generation: u64,
    pub(crate) owner_epoch: u64,
    pub(crate) phase: String,
    pub(crate) node_pid: u32,
    pub(crate) node_starttime: u64,
    pub(crate) container_id: String,
}

impl StartOutcome {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "runtime_id": self.runtime_id,
            "generation": self.generation,
            "owner_epoch": self.owner_epoch,
            "phase": self.phase,
            "node_pid": self.node_pid,
            "node_starttime": self.node_starttime,
            "container_id": self.container_id,
        })
    }

    pub(crate) fn from_json(v: &serde_json::Value) -> Result<Self> {
        let m = strict::object(
            v,
            &[
                "runtime_id",
                "generation",
                "owner_epoch",
                "phase",
                "node_pid",
                "node_starttime",
                "container_id",
            ],
        )?;
        Ok(Self {
            runtime_id: bounded(strict::string(m, "runtime_id")?, 200, "runtime_id")?,
            generation: strict::positive_u64(m, "generation")?,
            owner_epoch: strict::positive_u64(m, "owner_epoch")?,
            phase: bounded(strict::string(m, "phase")?, 32, "phase")?,
            node_pid: uid(m, "node_pid")?,
            node_starttime: strict::safe_u64(m, "node_starttime")?,
            container_id: bounded(strict::string(m, "container_id")?, 64, "container_id")?,
        })
    }
}

fn bounded(s: &str, max: usize, name: &str) -> Result<String> {
    if s.is_empty() || s.len() > max || s.bytes().any(|b| b.is_ascii_control()) {
        return Err(Error::rejected(format!("{name} is malformed")));
    }
    Ok(s.to_string())
}

fn uid(m: &serde_json::Map<String, serde_json::Value>, key: &str) -> Result<u32> {
    let n = strict::safe_u64(m, key)?;
    if n == 0 || n > u32::MAX as u64 {
        return Err(Error::rejected(format!("{key} is out of range")));
    }
    Ok(n as u32)
}

fn absolute(s: &str, name: &str) -> Result<PathBuf> {
    if !s.starts_with('/') || s.len() > 512 {
        return Err(Error::rejected(format!("{name} must be an absolute path")));
    }
    Ok(PathBuf::from(s))
}
