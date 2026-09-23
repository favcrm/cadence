//! Verified process identity for strict build-slot enrollments
//! (CAD-230 phase a).
//!
//! A managed provider (a Claude or Codex process the daemon launched)
//! has no tmux pane, so the legacy "nearest registered pane" rule can
//! never admit it or the cargo its tools run. The daemon instead mints
//! an [`Enrollment`] when the endpoint opens, binding the owner actor
//! and owner generation to the provider's exact process identity —
//! `(pid, /proc starttime, uid)` — and admits a caller only by
//! re-verifying that identity through `/proc` on every call.
//!
//! Everything here reads `/proc` through [`ProcFs`], whose root is
//! injectable so tests drive uid mismatches, recycled pids, broken
//! ancestry and unreadable entries from fixture trees (the
//! `doctor --host` `proc_root` pattern). Nothing here ever falls back
//! to the legacy alive-only check (`pid_matches`): an unreadable or
//! inconsistent read is `unknown`, never "probably fine".

use std::path::PathBuf;

use serde_json::{json, Value};

/// One exact process: the pid, its `/proc/<pid>/stat` starttime
/// (field 22, jiffies since boot) and its uid. Two processes sharing a
/// pid number differ in starttime — the recycling check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcIdentity {
    pub pid: u32,
    pub starttime: u64,
    pub uid: u32,
}

impl ProcIdentity {
    pub fn to_json(self) -> Value {
        json!({"pid": self.pid, "starttime": self.starttime, "uid": self.uid})
    }

    /// Strict parse — every field present and in range, or `None`.
    pub fn from_json(v: &Value) -> Option<Self> {
        Some(Self {
            pid: u32::try_from(v["pid"].as_u64()?).ok()?,
            starttime: v["starttime"].as_u64()?,
            uid: u32::try_from(v["uid"].as_u64()?).ok()?,
        })
    }
}

/// Tri-state liveness of a recorded identity: `Alive` retains,
/// `Dead` (gone, or the pid now names a different process) may free,
/// `Unknown` (unreadable or inconsistent) never frees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    Alive,
    Dead,
    Unknown,
}

impl Liveness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alive => "alive",
            Self::Dead => "dead",
            Self::Unknown => "unknown",
        }
    }
}

/// What one `/proc` read of a pid says.
enum Read {
    Found {
        ppid: u32,
        starttime: u64,
        /// Real and effective uid from `status`.
        uid: (u32, u32),
    },
    /// No such pid — the process is gone.
    Gone,
    /// Present but unreadable or malformed — proves nothing.
    Unreadable(String),
}

/// Deepest ancestry the verifier walks — a real process tree is far
/// shallower; anything deeper is treated as unverifiable.
const MAX_DEPTH: usize = 256;

/// `/proc`, rooted where the caller says — `/proc` in the daemon, a
/// fixture tree of `<pid>/stat` + `<pid>/status` files in tests.
#[derive(Clone, Debug)]
pub struct ProcFs {
    root: PathBuf,
}

impl Default for ProcFs {
    fn default() -> Self {
        Self {
            root: PathBuf::from("/proc"),
        }
    }
}

impl ProcFs {
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn read_file(&self, pid: u32, name: &str) -> Result<String, Read> {
        std::fs::read_to_string(self.root.join(pid.to_string()).join(name)).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Read::Gone
            } else {
                Read::Unreadable(format!("/proc/{pid}/{name}: {e}"))
            }
        })
    }

    fn read(&self, pid: u32) -> Read {
        if pid == 0 {
            return Read::Gone;
        }
        let stat = match self.read_file(pid, "stat") {
            Ok(s) => s,
            Err(r) => return r,
        };
        // comm (field 2) may hold spaces and parens — split after the
        // last ')'. Field 3 (state) is then index 0: ppid (field 4) is
        // index 1 and starttime (field 22) index 19.
        let fields: Vec<&str> = match stat.rsplit_once(')') {
            Some((_, after)) => after.split_whitespace().collect(),
            None => return Read::Unreadable(format!("/proc/{pid}/stat: malformed")),
        };
        let (Some(ppid), Some(starttime)) = (
            fields.get(1).and_then(|v| v.parse::<u32>().ok()),
            fields.get(19).and_then(|v| v.parse::<u64>().ok()),
        ) else {
            return Read::Unreadable(format!("/proc/{pid}/stat: malformed"));
        };
        let status = match self.read_file(pid, "status") {
            Ok(s) => s,
            Err(r) => return r,
        };
        let uid = status.lines().find_map(|l| {
            let mut ids = l.strip_prefix("Uid:")?.split_whitespace();
            Some((ids.next()?.parse().ok()?, ids.next()?.parse().ok()?))
        });
        let Some(uid) = uid else {
            return Read::Unreadable(format!("/proc/{pid}/status: no Uid line"));
        };
        Read::Found {
            ppid,
            starttime,
            uid,
        }
    }

    /// The live identity of `pid` now. A process whose real and
    /// effective uid differ has no single identity — refused.
    pub fn identity(&self, pid: u32) -> Result<ProcIdentity, String> {
        match self.read(pid) {
            Read::Found {
                starttime,
                uid: (real, effective),
                ..
            } => {
                if real != effective {
                    return Err(format!(
                        "pid {pid} runs with real uid {real} but effective uid {effective}"
                    ));
                }
                Ok(ProcIdentity {
                    pid,
                    starttime,
                    uid: effective,
                })
            }
            Read::Gone => Err(format!("pid {pid} is not running")),
            Read::Unreadable(why) => Err(why),
        }
    }

    /// Is the recorded process still that same process? A missing pid
    /// or a different starttime proves it gone; an unreadable entry or
    /// a changed uid proves nothing either way.
    pub fn liveness(&self, id: &ProcIdentity) -> (Liveness, &'static str) {
        match self.read(id.pid) {
            Read::Gone => (Liveness::Dead, "holder died"),
            Read::Found { starttime, .. } if starttime != id.starttime => {
                (Liveness::Dead, "pid recycled")
            }
            Read::Found { uid, .. } if uid != (id.uid, id.uid) => {
                (Liveness::Unknown, "holder uid changed")
            }
            Read::Found { .. } => (Liveness::Alive, "alive"),
            Read::Unreadable(_) => (Liveness::Unknown, "holder unreadable"),
        }
    }

    /// Does `pid` still name the process `starttime` recorded? `true`
    /// when the entry cannot be read — callers treat an unreadable
    /// candidate as a match and let the strict verifier refuse it.
    pub fn same_start(&self, pid: u32, starttime: u64) -> bool {
        match self.read(pid) {
            Read::Found { starttime: now, .. } => now == starttime,
            Read::Gone => false,
            Read::Unreadable(_) => true,
        }
    }

    /// The complete, verified ancestry from `peer` up to the enrolled
    /// `root`, peer first and root last. Every hop is read at call
    /// time and must run as `uid` (real and effective); a parent can
    /// never have started after its child (a pid recycled mid-walk
    /// fails here); the root must be exactly the recorded process.
    /// Any unreadable hop, a walk that reaches init without meeting
    /// the root, or a chain deeper than [`MAX_DEPTH`] refuses — the
    /// peer is the root or a verified descendant of it, or nothing.
    pub fn verified_descent(
        &self,
        peer: u32,
        root: &ProcIdentity,
        uid: u32,
    ) -> Result<Vec<u32>, String> {
        let mut segment = Vec::new();
        let mut pid = peer;
        let mut child_start: Option<u64> = None;
        for _ in 0..MAX_DEPTH {
            let (ppid, starttime, ids) = match self.read(pid) {
                Read::Found {
                    ppid,
                    starttime,
                    uid,
                } => (ppid, starttime, uid),
                Read::Gone => {
                    return Err(format!(
                        "incomplete ancestry: pid {pid} vanished during the walk"
                    ))
                }
                Read::Unreadable(why) => return Err(format!("incomplete ancestry: {why}")),
            };
            if ids != (uid, uid) {
                return Err(format!(
                    "pid {pid} runs as uid {}/{} — not the daemon's uid {uid}",
                    ids.0, ids.1
                ));
            }
            if child_start.is_some_and(|child| starttime > child) {
                return Err(format!(
                    "ancestry changed during the walk: pid {pid} started after \
                     its child (recycled pid)"
                ));
            }
            segment.push(pid);
            if pid == root.pid {
                if starttime != root.starttime {
                    return Err(format!(
                        "enrolled root pid {pid} is a different process now \
                         (starttime {starttime}, enrolled {})",
                        root.starttime
                    ));
                }
                if root.uid != uid {
                    return Err(format!(
                        "enrolled root runs as uid {} — not the daemon's uid {uid}",
                        root.uid
                    ));
                }
                return Ok(segment);
            }
            if ppid <= 1 {
                return Err(format!(
                    "pid {peer} does not descend from enrolled root pid {}",
                    root.pid
                ));
            }
            child_start = Some(starttime);
            pid = ppid;
        }
        Err(format!(
            "ancestry of pid {peer} is deeper than {MAX_DEPTH} — unverifiable"
        ))
    }
}

/// Whether an enrollment may still authorize new work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthState {
    Active,
    /// Past its daemon-capped lifetime; the same owner generation and
    /// root identity may renew it at the next endpoint open.
    Expired,
    /// The owner row changed, the endpoint closed, or a newer
    /// enrollment superseded it — never admits again.
    Revoked(String),
}

impl AuthState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Expired => "expired",
            Self::Revoked(_) => "revoked",
        }
    }
}

/// A daemon-minted strict enrollment. `worker` equals `root` for a
/// managed provider (the provider process itself); the field exists so
/// a later runner (CAD-236) can record one direct child.
#[derive(Clone, Debug)]
pub struct Enrollment {
    pub id: String,
    pub owner_actor: String,
    pub owner_generation: String,
    pub root: ProcIdentity,
    pub worker: ProcIdentity,
    pub issued_epoch: f64,
    pub expires_epoch: f64,
    /// Monotonic twin of `expires_epoch` — expiry rides the slot clock
    /// like every other age.
    pub expires_at: f64,
    pub auth: AuthState,
}

impl Enrollment {
    pub fn to_json(&self) -> Value {
        let mut j = json!({
            "enrollment_id": self.id,
            "owner_actor": self.owner_actor,
            "owner_generation": self.owner_generation,
            "root": self.root.to_json(),
            "worker": self.worker.to_json(),
            "issued_epoch": self.issued_epoch,
            "expires_epoch": self.expires_epoch,
            "auth_state": self.auth.as_str(),
        });
        if let AuthState::Revoked(reason) = &self.auth {
            j["revoked_reason"] = json!(reason);
        }
        j
    }

    /// Strict parse of a persisted enrollment; `now`/`wall` restore the
    /// monotonic expiry against this run's clock.
    pub fn from_json(v: &Value, now: f64, wall: f64) -> Option<Self> {
        let expires_epoch = v["expires_epoch"].as_f64()?;
        let auth = match v["auth_state"].as_str()? {
            "active" => AuthState::Active,
            "expired" => AuthState::Expired,
            "revoked" => AuthState::Revoked(
                v["revoked_reason"]
                    .as_str()
                    .unwrap_or("revoked")
                    .to_string(),
            ),
            _ => return None,
        };
        Some(Self {
            id: v["enrollment_id"].as_str()?.to_string(),
            owner_actor: v["owner_actor"].as_str()?.to_string(),
            owner_generation: v["owner_generation"].as_str()?.to_string(),
            root: ProcIdentity::from_json(&v["root"])?,
            worker: ProcIdentity::from_json(&v["worker"])?,
            issued_epoch: v["issued_epoch"].as_f64()?,
            expires_epoch,
            expires_at: now + (expires_epoch - wall),
            auth,
        })
    }
}

/// A caller the daemon verified against an enrollment: the owner's
/// alias is its lane, and `segment` — the peer up to the enrolled
/// root, every hop verified — is every pid it may bind a hold to.
#[derive(Clone, Debug)]
pub struct StrictCaller {
    pub enrollment_id: String,
    pub lane: String,
    pub segment: Vec<u32>,
}
