//! Slot registry serialization, durable writes and restart recovery.
//! Live admission, caller proof and scheduling remain in the parent module.

use super::{
    pid_alive, pid_matches, strict_unavailable, AuthState, Blocked, Enrollment, Liveness,
    ProcIdentity, SlotClock, SlotEvent, SlotHold, SlotKind, Slots, StrictBind,
};
use crate::error::Result;
use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

impl Slots {
    /// Where holds persist — the daemon points this at
    /// `<state>/slots.json`; unit tests leave it unset. The same dir
    /// is the disk-floor probe's filesystem.
    pub fn persist_to(&mut self, path: PathBuf) {
        self.disk_probe_path = path.parent().map(|p| p.to_path_buf());
        self.persist_path = Some(path);
    }

    /// Atomic best-effort write of the hold registry — a reader never
    /// sees a torn file, and a write failure is logged, not fatal:
    /// the in-memory registry stays authoritative either way. The tmp
    /// file is fsynced before the rename and the directory after it,
    /// so a crash cannot lose a persisted hold silently. Seniority is
    /// deliberately not persisted: it measures a wait, and no waiter
    /// survives a restart — every caller re-polls into a fresh anchor.
    ///
    /// This is the legacy writer, but it serializes the WHOLE state:
    /// once any strict record exists (or the file already is v2) it
    /// writes the v2 envelope with every enrollment and strict hold,
    /// so a legacy grant, release or reap can never drop strict state.
    /// A rejected state file is never overwritten — it is evidence.
    /// Returns whether the file now matches memory.
    pub(super) fn persist(&mut self) -> bool {
        let Some(path) = self.persist_path.clone() else {
            return true;
        };
        if self.blocked.as_ref().is_some_and(|b| b.preserve_file) {
            return false;
        }
        let v2 =
            self.v2 || !self.enrollments.is_empty() || self.held.iter().any(|h| h.strict.is_some());
        let doc = if v2 {
            Self::v2_doc(&self.enrollments, &self.held, self.state_generation + 1)
        } else {
            json!({
                "version": 1,
                "holds": self.held.iter().map(SlotHold::legacy_json).collect::<Vec<_>>(),
            })
        };
        match write_atomic(&path, &doc) {
            Ok(()) => {
                if v2 {
                    self.v2 = true;
                    self.state_generation += 1;
                }
                true
            }
            Err(e) => {
                eprintln!("slots: persist {} failed: {e}", path.display());
                false
            }
        }
    }

    /// The v2 envelope: enrollments, strict holds (each naming its
    /// enrollment), and every legacy hold under `legacy_holds`.
    fn v2_doc(enrollments: &[Enrollment], held: &[SlotHold], generation: u64) -> Value {
        json!({
            "format": "cadence-slots",
            "version": 2,
            "state_generation": generation.to_string(),
            "enrollments": enrollments.iter().map(Enrollment::to_json).collect::<Vec<_>>(),
            "holds": held.iter().filter_map(|h| {
                let b = h.strict.as_ref()?;
                let mut row = json!({
                    "token": h.token, "request_id": h.request_id,
                    "kind": h.kind.as_str(), "lane": h.lane,
                    "enrollment_id": b.enrollment_id,
                    "owner_generation": b.owner_generation,
                    "holder": b.holder.to_json(),
                    "acquired_epoch": h.acquired_epoch,
                });
                // Additive (CAD-230b): absent reads as not exec-bound.
                if b.exec_bound {
                    row["exec_bound"] = json!(true);
                }
                Some(row)
            }).collect::<Vec<_>>(),
            "legacy_holds": held.iter()
                .filter(|h| h.strict.is_none())
                .map(SlotHold::legacy_json)
                .collect::<Vec<_>>(),
        })
    }

    /// The fail-closed strict writer. The next state is written FIRST
    /// and swapped into memory only once the write landed: a failure
    /// leaves disk and memory exactly as they were — no grant, no free
    /// — and makes strict admission unavailable until a restart loads
    /// a good file.
    ///
    /// A revoked or expired enrollment is a TOMBSTONE: it stays while
    /// any hold names it or while its root process is alive or
    /// unknown, because [`Self::nearest_enrolled_root`] must keep
    /// seeing that exact root — otherwise its processes would fall
    /// through to an outer pane's legacy binding. It is pruned only
    /// once nothing holds under it and its root is proven dead.
    pub(super) fn commit(
        &mut self,
        enrollments: Vec<Enrollment>,
        held: Vec<SlotHold>,
    ) -> Result<()> {
        if let Some(b) = &self.blocked {
            return Err(strict_unavailable(&b.reason));
        }
        let enrollments: Vec<Enrollment> = enrollments
            .into_iter()
            .filter(|e| {
                e.auth == AuthState::Active
                    || held
                        .iter()
                        .any(|h| h.enrollment_id() == Some(e.id.as_str()))
                    || self.proc.liveness(&e.root).0 != Liveness::Dead
            })
            .collect();
        if let Some(path) = self.persist_path.clone() {
            let generation = self.state_generation + 1;
            if let Err(e) = write_atomic(&path, &Self::v2_doc(&enrollments, &held, generation)) {
                let reason = format!("strict state write to {} failed: {e}", path.display());
                eprintln!("slots: {reason}");
                self.blocked = Some(Blocked {
                    reason: reason.clone(),
                    preserve_file: false,
                });
                return Err(strict_unavailable(&reason));
            }
            self.v2 = true;
            self.state_generation = generation;
        }
        self.enrollments = enrollments;
        self.held = held;
        Ok(())
    }

    /// The state file itself is unusable: keep it untouched as
    /// evidence and refuse strict admission.
    fn reject_file(&mut self, reason: String) {
        eprintln!("slots: strict admission unavailable — {reason}");
        self.blocked = Some(Blocked {
            reason,
            preserve_file: true,
        });
    }

    /// Revalidate persisted holds at daemon start: a hold survives
    /// only while its recorded process is still the same live
    /// process. Survivors keep their tokens (in-flight releases and
    /// re-polls still resolve); the dead are dropped with a named
    /// reason rather than silently re-granted. Returns the boot-time
    /// release events for the daemon to emit.
    ///
    /// Order (CAD-230): the envelope is validated whole before any of
    /// it is used — unknown version, malformed JSON or schema, and
    /// duplicate records reject the file (kept as evidence, strict
    /// admission unavailable). Then enrollments (expiry, root identity)
    /// before strict holds (tri-state: alive retains, dead frees,
    /// unknown stays accounted) before legacy holds (unchanged rules).
    /// Waiters are never persisted, so none is ever replayed.
    pub fn restore(&mut self, clk: SlotClock) -> Vec<SlotEvent> {
        let mut events = Vec::new();
        let Some(path) = self.persist_path.clone() else {
            return events;
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return events,
            Err(e) => {
                self.reject_file(format!("cannot read {}: {e}", path.display()));
                return events;
            }
        };
        let Ok(doc) = serde_json::from_str::<Value>(&text) else {
            self.reject_file(format!("{} is unparsable", path.display()));
            return events;
        };
        let loaded = match (doc.get("format"), doc.get("version")) {
            (Some(format), Some(version)) if format == "cadence-slots" && version == 2 => {
                self.restore_v2(&doc, clk, &mut events)
            }
            (Some(format), version) => Err(format!(
                "unknown envelope (format {format}, version {})",
                version.cloned().unwrap_or(Value::Null)
            )),
            (None, Some(version)) if version != 1 => Err(format!("unknown version {version}")),
            // Anything that is not a well-formed envelope — `{}`,
            // `null`, `[]`, a non-list `holds` — is malformed state:
            // kept as evidence, never rewritten, strict blocked.
            (None, _) if !doc.is_object() || !doc["holds"].is_array() => {
                Err("not a well-formed v1 envelope (an object with a 'holds' list)".to_string())
            }
            // The v1 shape — or a version-less file an older writer
            // left: the legacy rows, under the unchanged rules.
            (None, _) => {
                self.restore_legacy(&doc["holds"], clk, &mut events);
                Ok(())
            }
        };
        if let Err(why) = loaded {
            self.reject_file(format!("{}: {why}", path.display()));
            return events;
        }
        // Seniority is never persisted: it measures an unserved wait
        // and no waiter survives a restart — re-polling callers
        // anchor fresh. (A stale `seniority` block in an old file is
        // simply ignored.) A boot rewrite that fails issues no strict
        // grant until a later restart writes cleanly.
        if !self.persist() && self.blocked.is_none() {
            self.blocked = Some(Blocked {
                reason: format!("boot rewrite of {} failed", path.display()),
                preserve_file: false,
            });
        }
        events
    }

    /// Legacy (v1 / `legacy_holds`) rows — the CAD-113 rules exactly:
    /// the alive-only fallback, per-row skips, dead holders reaped.
    fn restore_legacy(&mut self, holds: &Value, clk: SlotClock, events: &mut Vec<SlotEvent>) {
        let (now, wall) = (clk.mono, clk.wall);
        let Some(holds) = holds.as_array() else {
            return;
        };
        for h in holds {
            let (Some(kind), Some(lane), Some(pid)) = (
                h["kind"].as_str().and_then(|k| SlotKind::parse(k).ok()),
                h["lane"].as_str(),
                h["pid"].as_u64().map(|p| p as u32),
            ) else {
                continue;
            };
            let start = h["pid_start"].as_u64();
            let held_secs = (wall - h["acquired_epoch"].as_f64().unwrap_or(wall)).max(0.0);
            if !pid_matches(pid, start) {
                let reason = if pid_alive(pid) {
                    "pid recycled"
                } else {
                    "holder died"
                };
                events.push((
                    lane.to_string(),
                    "slot_released",
                    json!({"kind": kind.as_str(), "pid": pid,
                           "held_secs": held_secs, "reason": reason}),
                ));
                continue;
            }
            let Some(token) = h["token"].as_str().map(str::to_string) else {
                eprintln!(
                    "slots: dropping persisted hold with no token \
                     ({lane} {kind:?} pid {pid}) — persist file truncated?"
                );
                continue;
            };
            self.held.push(SlotHold {
                token,
                request_id: h["request_id"].as_str().unwrap_or_default().to_string(),
                kind,
                lane: lane.to_string(),
                pid,
                pid_start: start,
                acquired_at: now - held_secs,
                acquired_epoch: wall - held_secs,
                strict: None,
            });
        }
    }

    /// Validate the whole v2 envelope, then apply it in order. Any
    /// schema fault — or a same-root enrollment pair across owners, in
    /// any authorization state — is an `Err` before anything is applied.
    fn restore_v2(
        &mut self,
        doc: &Value,
        clk: SlotClock,
        events: &mut Vec<SlotEvent>,
    ) -> std::result::Result<(), String> {
        let (now, wall) = (clk.mono, clk.wall);
        let generation: u64 = doc["state_generation"]
            .as_str()
            .and_then(|g| g.parse().ok())
            .ok_or("state_generation missing or malformed")?;
        let list = |key: &str| {
            doc[key]
                .as_array()
                .ok_or_else(|| format!("'{key}' missing or not a list"))
        };
        let mut enrollments = Vec::new();
        for e in list("enrollments")? {
            let parsed = Enrollment::from_json(e, now, wall)
                .ok_or_else(|| format!("malformed enrollment {e}"))?;
            // Ids are unique. A shared root of ONE owner never rejects
            // the file (CAD-276) — a same-root supersession, which
            // `strict_caller` resolves deterministically — but across
            // owners, in any authorization state, the pair has no
            // lineage: the daemon never writes it (`enroll` refuses a
            // process enrolled for another owner) and the file is
            // invalid (CAD-289).
            if enrollments.iter().any(|o: &Enrollment| o.id == parsed.id) {
                return Err(format!("duplicate enrollment {}", parsed.id));
            }
            if let Some(o) = enrollments.iter().find(|o: &&Enrollment| {
                o.root == parsed.root && o.owner_actor != parsed.owner_actor
            }) {
                return Err(format!(
                    "enrollments {} and {} share root pid {} across owners ({} / {}) \
                     — one provider process is enrolled for one owner",
                    o.id, parsed.id, parsed.root.pid, o.owner_actor, parsed.owner_actor
                ));
            }
            enrollments.push(parsed);
        }
        let mut tokens = std::collections::HashSet::new();
        let mut strict_rows = Vec::new();
        for h in list("holds")? {
            let row = (|| {
                let enrollment_id = h["enrollment_id"].as_str()?.to_string();
                let lane = enrollments
                    .iter()
                    .find(|e| e.id == enrollment_id)?
                    .owner_actor
                    .clone();
                Some((
                    h["token"].as_str()?.to_string(),
                    h["request_id"].as_str()?.to_string(),
                    SlotKind::parse(h["kind"].as_str()?).ok()?,
                    lane,
                    StrictBind {
                        enrollment_id,
                        owner_generation: h["owner_generation"].as_str()?.to_string(),
                        holder: ProcIdentity::from_json(&h["holder"])?,
                        liveness: Liveness::Unknown,
                        exec_bound: match h.get("exec_bound") {
                            None => false,
                            Some(v) => v.as_bool()?,
                        },
                    },
                    h["acquired_epoch"].as_f64()?,
                ))
            })()
            .ok_or_else(|| format!("malformed or unbound strict hold {h}"))?;
            if !tokens.insert(row.0.clone()) {
                return Err(format!("duplicate hold token {}", row.0));
            }
            strict_rows.push(row);
        }
        let legacy = list("legacy_holds")?;
        for h in legacy {
            let ok = h["token"].as_str().is_some()
                && h["kind"]
                    .as_str()
                    .and_then(|k| SlotKind::parse(k).ok())
                    .is_some()
                && h["lane"].as_str().is_some()
                && h["pid"].as_u64().is_some()
                && h["acquired_epoch"].as_f64().is_some();
            if !ok {
                return Err(format!("malformed legacy hold {h}"));
            }
            if !tokens.insert(h["token"].as_str().unwrap_or_default().to_string()) {
                return Err(format!("duplicate hold token {}", h["token"]));
            }
        }
        // Validated — apply: enrollments, then strict holds, then
        // legacy holds.
        self.v2 = true;
        self.state_generation = generation;
        for e in &mut enrollments {
            if e.auth == AuthState::Active && wall >= e.expires_epoch {
                e.auth = AuthState::Expired;
            }
            if matches!(e.auth, AuthState::Revoked(_)) {
                continue;
            }
            // A runner's owner is the previous daemon's in-memory
            // runner record (CAD-230b): it did not survive the restart,
            // so the enrollment admits nothing more. Its hold stays
            // accounted under the tri-state rule below until the
            // process is proven dead — it is never relaunched.
            if e.runner.is_some() {
                e.auth = AuthState::Revoked(
                    "daemon restarted — runner outcome unknown, never relaunched".into(),
                );
                continue;
            }
            match self.proc.liveness(&e.root).0 {
                Liveness::Alive => {}
                Liveness::Dead => {
                    e.auth = AuthState::Revoked("root process gone at restart".into())
                }
                Liveness::Unknown => {
                    e.auth = AuthState::Revoked("root liveness unknown at restart".into())
                }
            }
        }
        self.enrollments = enrollments;
        for (token, request_id, kind, lane, mut bind, acquired_epoch) in strict_rows {
            let held_secs = (wall - acquired_epoch).max(0.0);
            let (liveness, why) = self.proc.liveness(&bind.holder);
            if liveness == Liveness::Dead {
                events.push((
                    lane,
                    "slot_released",
                    json!({"kind": kind.as_str(), "pid": bind.holder.pid,
                           "held_secs": held_secs, "reason": why}),
                ));
                continue;
            }
            bind.liveness = liveness;
            self.held.push(SlotHold {
                token,
                request_id,
                kind,
                lane,
                pid: bind.holder.pid,
                pid_start: Some(bind.holder.starttime),
                acquired_at: now - held_secs,
                acquired_epoch: wall - held_secs,
                strict: Some(bind),
            });
        }
        self.restore_legacy(&doc["legacy_holds"], clk, events);
        Ok(())
    }
}

/// Atomic write: tmp file (mode 0600) fsynced before the rename, the
/// directory after it, so a crash cannot lose or tear a record.
fn write_atomic(path: &Path, doc: &Value) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut f| {
            f.write_all(doc.to_string().as_bytes())?;
            f.sync_all()
        })
        .and_then(|_| std::fs::rename(&tmp, path))?;
    // The rename is durable only once its directory is.
    if let Some(dir) = path.parent() {
        let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
    }
    Ok(())
}
