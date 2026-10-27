//! Server-created runtime identity, readiness and the broker writer fence.
//!
//! `QualifiedRuntime` has private fields, is not `Clone`/`Serialize`/`Deserialize`
//! and has no public or crate-wide literal constructor: it is minted only by
//! [`super::client::RootOwnerClient::recheck`] from a positive owner reply after
//! a consumed obligation, a live container measurement and a verified registry.
//! A stored row, receipt, boolean, signature or profile file cannot create one.
//!
//! [`CurrentEligibility`] is the short read guard the daemon holds across final
//! admission. Refreshing it with a newer qualification is the only writer that
//! enables dispatch; a broker fence (from the private root owner listener)
//! takes the same writer and permanently refuses that runtime generation.

use std::sync::{RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

use super::strict;

/// How long one owner `recheck` remains a valid local eligibility witness.
/// Short by construction: every dispatch takes a fresh witness before its
/// final transaction, and expiry alone denies (no silent stale authority).
const ELIGIBILITY_WINDOW: Duration = Duration::from_secs(5);

/// Public (secret-free) identity of one qualified runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeIdentity {
    pub(crate) workspace_id: String,
    pub(crate) runtime_id: String,
    pub(crate) agent_uid: String,
    pub(crate) session_binding: String,
    pub(crate) server_generation: u64,
    pub(crate) runtime_generation: u64,
    pub(crate) owner_epoch: u64,
    pub(crate) os_uid: u32,
    pub(crate) container_id: String,
    pub(crate) image_digest: String,
    pub(crate) profile_digest: String,
    pub(crate) loaded_registry_digest: String,
    pub(crate) loaded_receipt_id: String,
    pub(crate) node_pid: u32,
    pub(crate) node_starttime: u64,
}

impl RuntimeIdentity {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "workspace_id": self.workspace_id,
            "runtime_id": self.runtime_id,
            "agent_uid": self.agent_uid,
            "session_binding": self.session_binding,
            "server_generation": self.server_generation,
            "runtime_generation": self.runtime_generation,
            "owner_epoch": self.owner_epoch,
            "os_uid": self.os_uid,
            "container_id": self.container_id,
            "image_digest": self.image_digest,
            "profile_digest": self.profile_digest,
            "loaded_registry_digest": self.loaded_registry_digest,
            "loaded_receipt_id": self.loaded_receipt_id,
            "node_pid": self.node_pid,
            "node_starttime": self.node_starttime,
        })
    }

    pub(crate) fn from_json(v: &serde_json::Value) -> Result<Self> {
        let m = strict::object(
            v,
            &[
                "workspace_id",
                "runtime_id",
                "agent_uid",
                "session_binding",
                "server_generation",
                "runtime_generation",
                "owner_epoch",
                "os_uid",
                "container_id",
                "image_digest",
                "profile_digest",
                "loaded_registry_digest",
                "loaded_receipt_id",
                "node_pid",
                "node_starttime",
            ],
        )?;
        let bounded = |key: &str, max: usize| -> Result<String> {
            let s = strict::string(m, key)?;
            if s.is_empty() || s.len() > max || s.bytes().any(|b| b.is_ascii_control()) {
                return Err(Error::rejected(format!("{key} is malformed")));
            }
            Ok(s.to_string())
        };
        let digest = |key: &str| -> Result<String> {
            let s = strict::string(m, key)?;
            strict::digest(s)?;
            Ok(s.to_string())
        };
        Ok(Self {
            workspace_id: bounded("workspace_id", 200)?,
            runtime_id: bounded("runtime_id", 200)?,
            agent_uid: bounded("agent_uid", 128)?,
            session_binding: bounded("session_binding", 200)?,
            server_generation: strict::positive_u64(m, "server_generation")?,
            runtime_generation: strict::positive_u64(m, "runtime_generation")?,
            owner_epoch: strict::positive_u64(m, "owner_epoch")?,
            os_uid: {
                let n = strict::safe_u64(m, "os_uid")?;
                if n == 0 || n > u32::MAX as u64 {
                    return Err(Error::rejected("os_uid is out of range"));
                }
                n as u32
            },
            container_id: bounded("container_id", 64)?,
            image_digest: digest("image_digest")?,
            profile_digest: digest("profile_digest")?,
            loaded_registry_digest: digest("loaded_registry_digest")?,
            loaded_receipt_id: bounded("loaded_receipt_id", 200)?,
            node_pid: {
                let n = strict::safe_u64(m, "node_pid")?;
                if n == 0 || n > 4_194_304 {
                    return Err(Error::rejected("node_pid is out of range"));
                }
                n as u32
            },
            node_starttime: strict::safe_u64(m, "node_starttime")?,
        })
    }

    pub(crate) fn agent_uid(&self) -> &str {
        &self.agent_uid
    }
    pub(crate) fn runtime_id(&self) -> &str {
        &self.runtime_id
    }
    pub(crate) fn runtime_generation(&self) -> u64 {
        self.runtime_generation
    }
    pub(crate) fn os_uid(&self) -> u32 {
        self.os_uid
    }
    pub(crate) fn session_binding(&self) -> &str {
        &self.session_binding
    }
}

/// A live, freshly rechecked runtime. Not cloneable, not serializable, no
/// caller-constructible literal. Holding one is evidence that the owner just
/// re-verified the epoch, recipient, live container and loaded registry.
pub(crate) struct QualifiedRuntime {
    identity: RuntimeIdentity,
    qualified_at: Instant,
    expires_at: Instant,
}

impl QualifiedRuntime {
    pub(super) fn mint(identity: RuntimeIdentity) -> Self {
        let now = Instant::now();
        Self {
            identity,
            qualified_at: now,
            expires_at: now + ELIGIBILITY_WINDOW,
        }
    }

    pub(crate) fn identity(&self) -> &RuntimeIdentity {
        &self.identity
    }

    /// True only while the short witness has not expired. Expired is a refusal,
    /// never a silent extension.
    pub(crate) fn is_current(&self) -> bool {
        Instant::now() < self.expires_at
    }

    pub(crate) fn qualified_at(&self) -> Instant {
        self.qualified_at
    }
}

impl std::fmt::Debug for QualifiedRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QualifiedRuntime")
            .field("runtime_id", &self.identity.runtime_id)
            .field("generation", &self.identity.runtime_generation)
            .field("current", &self.is_current())
            .finish()
    }
}

/// Secret-free status for operator/HTTP surfaces.
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct RuntimeStatus {
    pub(crate) v: u32,
    pub(crate) runtime_id: String,
    pub(crate) generation: u64,
    pub(crate) owner_epoch: u64,
    /// The owner authority's database/custody epoch.
    pub(crate) database_epoch: u64,
    pub(crate) phase: String,
    pub(crate) agent_uid: String,
    pub(crate) session_binding: String,
    pub(crate) os_uid: u32,
    pub(crate) qualified: bool,
    pub(crate) node_pid: Option<u32>,
    pub(crate) loaded_registry_digest: Option<String>,
    pub(crate) fenced_reason: Option<String>,
}

impl RuntimeStatus {
    /// Parse the owner's status reply. Secret-free fields only.
    pub(crate) fn from_json(v: &serde_json::Value) -> Result<Self> {
        let m = strict::object(
            v,
            &[
                "v",
                "runtime_id",
                "generation",
                "owner_epoch",
                "database_epoch",
                "phase",
                "agent_uid",
                "session_binding",
                "os_uid",
                "qualified",
                "node_pid",
                "loaded_registry_digest",
                "fenced_reason",
            ],
        )?;
        if strict::safe_u64(m, "v")? != 1 {
            return Err(Error::rejected("status v must be exactly 1"));
        }
        let optional_string = |key: &str| -> Result<Option<String>> {
            match m.get(key) {
                None | Some(serde_json::Value::Null) => Ok(None),
                Some(value) => value
                    .as_str()
                    .map(|s| Some(s.to_string()))
                    .ok_or_else(|| Error::rejected(format!("{key} is not a string"))),
            }
        };
        let os_uid = strict::safe_u64(m, "os_uid")?;
        if os_uid == 0 || os_uid > u32::MAX as u64 {
            return Err(Error::rejected("status os_uid is out of range"));
        }
        Ok(Self {
            v: 1,
            runtime_id: strict::string(m, "runtime_id")?.to_string(),
            generation: strict::positive_u64(m, "generation")?,
            owner_epoch: strict::positive_u64(m, "owner_epoch")?,
            database_epoch: strict::positive_u64(m, "database_epoch")?,
            phase: strict::string(m, "phase")?.to_string(),
            agent_uid: strict::string(m, "agent_uid")?.to_string(),
            session_binding: strict::string(m, "session_binding")?.to_string(),
            os_uid: os_uid as u32,
            qualified: strict::boolean(m, "qualified")?,
            node_pid: match m.get("node_pid") {
                None | Some(serde_json::Value::Null) => None,
                Some(value) => Some(
                    value
                        .as_u64()
                        .filter(|n| *n > 0 && *n <= 4_194_304)
                        .ok_or_else(|| Error::rejected("status node_pid is out of range"))?
                        as u32,
                ),
            },
            loaded_registry_digest: optional_string("loaded_registry_digest")?,
            fenced_reason: optional_string("fenced_reason")?,
        })
    }
}

/// The exact broker-fence acknowledgement.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct FenceAck {
    pub(crate) v: u32,
    pub(crate) runtime_id: String,
    pub(crate) owner_epoch: u64,
    pub(crate) generation: u64,
    pub(crate) nonce: String,
    pub(crate) fenced_at_ms: u64,
}

struct FenceWatermark {
    runtime_id: String,
    generation: u64,
    owner_epoch: u64,
    at_ms: u64,
}

struct EligibilityState {
    current: Option<QualifiedRuntime>,
    fence: Option<FenceWatermark>,
}

/// The daemon's short current-eligibility guard.
pub(crate) struct CurrentEligibility {
    state: RwLock<EligibilityState>,
}

impl CurrentEligibility {
    pub(crate) fn new() -> Self {
        Self {
            state: RwLock::new(EligibilityState {
                current: None,
                fence: None,
            }),
        }
    }

    /// Writer: install a freshly rechecked runtime. Refuses a runtime whose
    /// generation/epoch is already fenced; refreshes are the only way a
    /// dispatch becomes eligible.
    pub(crate) fn refresh(&self, runtime: QualifiedRuntime) -> Result<()> {
        if !runtime.is_current() {
            return Err(Error::rejected(
                "owner recheck witness expired before it could be installed",
            ));
        }
        let mut state = self
            .state
            .write()
            .map_err(|_| Error::internal("eligibility lock poisoned"))?;
        if let Some(fence) = &state.fence {
            if fence.runtime_id == runtime.identity.runtime_id
                && (fence.owner_epoch > runtime.identity.owner_epoch
                    || (fence.owner_epoch == runtime.identity.owner_epoch
                        && runtime.identity.runtime_generation <= fence.generation))
            {
                return Err(Error::rejected(
                    "runtime generation is fenced — a fresh launch is required",
                ));
            }
        }
        state.current = Some(runtime);
        Ok(())
    }

    /// Writer: clear eligibility (lock/unlock, container exit, uncertainty).
    pub(crate) fn invalidate(&self, reason: &str) {
        if let Ok(mut state) = self.state.write() {
            state.current = None;
            let _ = reason;
        }
    }

    /// Writer-FENCE: the owner took its runtime fence; the daemon stops
    /// admitting this generation before acknowledging. Idempotent for the same
    /// exact tuple; a lower generation for the same runtime still fences.
    pub(crate) fn fence_generation(
        &self,
        runtime_id: &str,
        owner_epoch: u64,
        generation: u64,
        nonce: &str,
    ) -> Result<FenceAck> {
        if runtime_id.is_empty() || runtime_id.len() > 200 {
            return Err(Error::rejected("fence runtime id is malformed"));
        }
        if nonce.is_empty() || nonce.len() > 64 {
            return Err(Error::rejected("fence nonce is malformed"));
        }
        let at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let mut state = self
            .state
            .write()
            .map_err(|_| Error::internal("eligibility lock poisoned"))?;
        state.current = None;
        let replace = match &state.fence {
            None => true,
            Some(existing) => {
                existing.runtime_id != runtime_id
                    || existing.owner_epoch < owner_epoch
                    || (existing.owner_epoch == owner_epoch && existing.generation <= generation)
            }
        };
        if replace {
            state.fence = Some(FenceWatermark {
                runtime_id: runtime_id.to_string(),
                generation,
                owner_epoch,
                at_ms,
            });
        }
        Ok(FenceAck {
            v: 1,
            runtime_id: runtime_id.to_string(),
            owner_epoch,
            generation,
            nonce: nonce.to_string(),
            fenced_at_ms: at_ms,
        })
    }

    /// Shared reader held across the final admission transaction.
    pub(crate) fn reader(&self) -> Result<CurrentReader<'_>> {
        let guard = self
            .state
            .read()
            .map_err(|_| Error::internal("eligibility lock poisoned"))?;
        Ok(CurrentReader { guard })
    }
}

impl Default for CurrentEligibility {
    fn default() -> Self {
        Self::new()
    }
}

/// The short read guard. Holding it blocks the fence writer; the guarded
/// runtime expires by itself.
pub(crate) struct CurrentReader<'a> {
    guard: RwLockReadGuard<'a, EligibilityState>,
}

impl CurrentReader<'_> {
    pub(crate) fn qualified(&self) -> Option<&QualifiedRuntime> {
        self.guard.current.as_ref().filter(|q| q.is_current())
    }

    pub(crate) fn require(&self) -> Result<&QualifiedRuntime> {
        self.qualified()
            .ok_or_else(|| Error::rejected("no current qualified runtime — admission refused"))
    }

    pub(crate) fn runtime_id(&self) -> Option<&str> {
        self.guard
            .current
            .as_ref()
            .map(|q| q.identity.runtime_id.as_str())
    }
}
