//! Strict enrollment tests (CAD-230 phase a) — every process identity
//! here comes from an injected `/proc` fixture tree, so uid mismatches,
//! recycled pids, broken ancestry and unreadable entries are exact and
//! deterministic. Legacy holds in the mixed-state tests use the real
//! test process, as the legacy rules read the real `/proc`.

use super::*;
use std::path::Path;

/// The daemon uid the fixtures run as — any value works, the fixture
/// is the whole world.
const UID: u32 = 4242;

/// A fixture `/proc`: `<pid>/stat` (ppid field 4, starttime field 22)
/// and `<pid>/status` (`Uid:` real/effective/saved/fs).
struct FakeProc(tempfile::TempDir);

impl FakeProc {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }

    fn root(&self) -> &Path {
        self.0.path()
    }

    fn spawn(&self, pid: u32, ppid: u32, starttime: u64) -> &Self {
        self.spawn_as(pid, ppid, starttime, UID)
    }

    fn spawn_as(&self, pid: u32, ppid: u32, starttime: u64, uid: u32) -> &Self {
        let dir = self.root().join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        // After the last ')': state, ppid, 17 filler fields, starttime.
        let filler = "0 ".repeat(17);
        std::fs::write(
            dir.join("stat"),
            format!("{pid} (fake worker) S {ppid} {filler}{starttime} 0 0\n"),
        )
        .unwrap();
        std::fs::write(
            dir.join("status"),
            format!("Name:\tfake\nPPid:\t{ppid}\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
        )
        .unwrap();
        self
    }

    fn kill(&self, pid: u32) {
        std::fs::remove_dir_all(self.root().join(pid.to_string())).unwrap();
    }

    /// Present but unreadable as a process record.
    fn garble(&self, pid: u32) {
        std::fs::write(self.root().join(pid.to_string()).join("stat"), "garbage").unwrap();
    }
}

/// The standard tree: daemon 100 → provider root 200 → shell 300 →
/// cadence CLI 400; 310/320 are further tool processes of the shell;
/// 500 is a sibling of the root (another child of the daemon).
fn tree() -> FakeProc {
    let p = FakeProc::new();
    p.spawn(100, 1, 10)
        .spawn(200, 100, 50)
        .spawn(300, 200, 60)
        .spawn(310, 300, 61)
        .spawn(320, 300, 62)
        .spawn(400, 300, 70)
        .spawn(500, 100, 80);
    p
}

fn clk(now: f64) -> SlotClock {
    SlotClock::at(now, 1_000_000.0 + now)
}

fn strict_slots(proc: &FakeProc, build: usize) -> Slots {
    let mut s = Slots::new(SlotConfig {
        build_slots: build,
        suite_slots: 1,
        ..Default::default()
    });
    s.use_proc(proc.root(), UID);
    s
}

fn enroll(s: &mut Slots, owner: &str, generation: &str, root: u32) -> String {
    let (summary, events) = s.enroll(owner, generation, root, clk(0.0)).unwrap();
    assert!(events.iter().any(|e| e.1 == "slot_enrolled"));
    summary["enrollment_id"].as_str().unwrap().to_string()
}

fn acquire_as(s: &mut Slots, peer: u32, pid: u32, req: &str, now: f64) -> Result<Value> {
    let caller = s.strict_caller(peer, 200)?;
    s.acquire_strict(SlotKind::Build, &caller, pid, req, false, clk(now))
        .map(|(v, _)| v)
}

fn held_json(s: &mut Slots, now: f64) -> Vec<Value> {
    let (status, _) = s.status(
        SlotCaller {
            lane: "",
            pids: &[],
        },
        now,
    );
    status["pools"]["build"]["held"].as_array().unwrap().clone()
}

/// ACCEPTANCE (phase a): the enrollment binds owner + generation +
/// exact root identity; the root itself and a verified descendant (the
/// cargo its shell runs — the one deviation from design v3) are
/// admitted under the owner's lane, and only pids on the verified
/// peer-to-root segment may hold.
#[test]
fn enrolled_root_and_verified_descendant_are_admitted() {
    let p = tree();
    let mut s = strict_slots(&p, 2);
    let id = enroll(&mut s, "wk", "g1", 200);
    let e = &s.enrollments[0];
    assert_eq!(
        (e.owner_actor.as_str(), e.owner_generation.as_str()),
        ("wk", "g1")
    );
    assert_eq!(
        e.root,
        ProcIdentity {
            pid: 200,
            starttime: 50,
            uid: UID
        }
    );
    assert_eq!(e.worker, e.root);
    assert!(e.expires_epoch > e.issued_epoch);
    // The nearest enrolled root on the CLI's chain is the provider.
    assert_eq!(s.nearest_enrolled_root(&[400, 300, 200, 100]), Some(2));
    let caller = s.strict_caller(400, 200).unwrap();
    assert_eq!(caller.segment, vec![400, 300, 200]);
    assert_eq!(caller.enrollment_id, id);
    // Descendant: the CLI claims its shell (`--pid $$`).
    let g = acquire_as(&mut s, 400, 300, "r1", 0.0).unwrap();
    assert_eq!(g["granted"], true, "{g}");
    // Root: the provider process itself.
    let g = acquire_as(&mut s, 200, 200, "r2", 0.0).unwrap();
    assert_eq!(g["granted"], true, "{g}");
    for h in held_json(&mut s, 1.0) {
        assert_eq!(h["lane"], "wk");
        assert_eq!(h["binding"], "strict");
        assert_eq!(h["enrollment_id"], id.as_str());
        assert_eq!(h["owner_generation"], "g1");
        assert_eq!(h["auth_state"], "active");
        assert_eq!(h["liveness"], "alive");
        assert_eq!(h["accounting"], "held");
        assert_eq!(h["reconcile_required"], false);
    }
    // A pid above the root (the daemon) is off the verified segment.
    let err = acquire_as(&mut s, 400, 100, "r3", 0.0).unwrap_err();
    assert!(err.to_string().contains("cannot claim pid 100"), "{err}");
}

/// ACCEPTANCE: injected `/proc` faults — uid mismatch (peer, hop,
/// root), missing or changed starttime, incomplete ancestry, a sibling
/// and a recycled-parent chain — all refuse the strict caller. The
/// strict path has no fallback to try instead.
#[test]
fn injected_proc_faults_reject_strict_callers() {
    type Fault = fn(&FakeProc);
    let cases: [(&str, u32, Fault, &str); 8] = [
        (
            "peer uid",
            400,
            |p| {
                p.spawn_as(400, 300, 70, UID + 1);
            },
            "uid",
        ),
        (
            "hop uid",
            400,
            |p| {
                p.spawn_as(300, 200, 60, UID + 1);
            },
            "uid",
        ),
        (
            "root starttime changed",
            400,
            |p| {
                p.spawn(200, 100, 51);
            },
            "different process",
        ),
        ("root gone", 400, |p| p.kill(200), "incomplete ancestry"),
        ("hop missing", 400, |p| p.kill(300), "incomplete ancestry"),
        (
            "hop unreadable",
            400,
            |p| p.garble(300),
            "incomplete ancestry",
        ),
        ("sibling of the root", 500, |_| {}, "does not descend"),
        (
            "parent younger than child",
            400,
            |p| {
                p.spawn(300, 200, 99);
            },
            "started after its child",
        ),
    ];
    for (name, peer, fault, needle) in cases {
        let p = tree();
        let mut s = strict_slots(&p, 1);
        enroll(&mut s, "wk", "g1", 200);
        fault(&p);
        let err = s.strict_caller(peer, 200).unwrap_err().to_string();
        assert!(err.contains(needle), "{name}: {err}");
        assert!(s.held.is_empty() && s.waiting.is_empty(), "{name}");
    }
    // A root running as another uid is never enrolled at all.
    let p = tree();
    p.spawn_as(200, 100, 50, UID + 1);
    let mut s = strict_slots(&p, 1);
    let err = s.enroll("wk", "g1", 200, clk(0.0)).unwrap_err();
    assert!(err.to_string().contains("uid"), "{err}");
    // Nor is the daemon's own process, or init.
    for pid in [std::process::id(), 1] {
        assert!(s.enroll("wk", "g1", pid, clk(0.0)).is_err());
    }
}

/// ACCEPTANCE: owner-generation drift (or the owner's endpoint gone)
/// revokes on revalidation — the next strict acquire is refused, with
/// no fallback — but the identity still verifies, so the holder can
/// release what it holds.
#[test]
fn owner_generation_drift_revokes_new_work() {
    let p = tree();
    let mut s = strict_slots(&p, 2);
    enroll(&mut s, "wk", "g1", 200);
    let g = acquire_as(&mut s, 300, 300, "r1", 0.0).unwrap();
    let token = g["token"].as_str().unwrap().to_string();
    // Same generation: nothing happens.
    let same = HashMap::from([("wk".to_string(), Some("g1".to_string()))]);
    assert!(s.revalidate_owners(&same).is_empty());
    let drift = HashMap::from([("wk".to_string(), Some("g2".to_string()))]);
    let events = s.revalidate_owners(&drift);
    assert!(events
        .iter()
        .any(|e| e.1 == "slot_enrollment_revoked" && e.2["reason"] == "owner generation changed"));
    let err = acquire_as(&mut s, 400, 400, "r2", 1.0).unwrap_err();
    assert!(err.to_string().contains("revoked"), "{err}");
    // The live hold survives revocation, accounted.
    let h = &held_json(&mut s, 1.0)[0];
    assert_eq!(h["auth_state"], "revoked");
    assert_eq!(h["liveness"], "alive");
    // Its exact holder may still give it back.
    let caller = s.strict_caller(300, 200).unwrap();
    let (r, _) = s.release_strict(&token, &caller, 300, 2.0).unwrap();
    assert_eq!(r["released"], true);
    // An owner whose endpoint vanished revokes the same way.
    let mut s = strict_slots(&p, 1);
    enroll(&mut s, "wk", "g1", 200);
    let gone = HashMap::from([("wk".to_string(), None)]);
    let events = s.revalidate_owners(&gone);
    assert_eq!(events[0].2["reason"], "owner has no live endpoint");
}

/// Resume: the same owner generation AND root identity renew the same
/// enrollment; a changed generation or a recycled root pid mints a new
/// one and supersedes the old.
#[test]
fn resume_renews_only_the_same_generation_and_identity() {
    let p = tree();
    let mut s = strict_slots(&p, 1);
    let first = enroll(&mut s, "wk", "g1", 200);
    let (again, _) = s.enroll("wk", "g1", 200, clk(10.0)).unwrap();
    assert_eq!(again["enrollment_id"], first.as_str());
    assert_eq!(again["renewed"], true);
    let second = enroll(&mut s, "wk", "g2", 200);
    assert_ne!(second, first);
    // The superseded enrollment held nothing, so it is pruned.
    assert!(s.enrollments.iter().all(|e| e.id != first));
    p.spawn(200, 100, 55);
    let third = enroll(&mut s, "wk", "g2", 200);
    assert_ne!(third, second);
    assert_eq!(s.enrollments.len(), 1);
}

/// ACCEPTANCE: a strict hold frees only by an exact release — its own
/// enrollment's verified caller naming the recorded holder — never by
/// a legacy release of its token, a different process of the same
/// agent, or a recycled holder pid.
#[test]
fn strict_hold_frees_only_by_exact_release() {
    let p = tree();
    let mut s = strict_slots(&p, 1);
    enroll(&mut s, "wk", "g1", 200);
    let g = acquire_as(&mut s, 300, 300, "r1", 0.0).unwrap();
    let token = g["token"].as_str().unwrap().to_string();
    // The legacy release path cannot touch a strict hold.
    let err = s.release(&token, "wk", 300, 1.0).unwrap_err();
    assert!(err.to_string().contains("another caller"), "{err}");
    // Another process of the same agent is not the holder.
    let caller = s.strict_caller(400, 200).unwrap();
    let err = s.release_strict(&token, &caller, 400, 1.0).unwrap_err();
    assert!(err.to_string().contains("another caller"), "{err}");
    assert_eq!(s.held.len(), 1);
    // The exact holder releases.
    let caller = s.strict_caller(300, 200).unwrap();
    let (r, events) = s.release_strict(&token, &caller, 300, 2.0).unwrap();
    assert_eq!(r["released"], true);
    assert!(events.iter().any(|e| e.2["reason"] == "released"));
    assert!(s.held.is_empty());
}

/// ACCEPTANCE: revoke/expiry filters waiters — reported, before any
/// ranking — but never frees a live hold; past `max_hold_secs` a strict
/// hold is `expired_pending_reconcile`, still occupying its slot; only
/// the holder's proven death frees it.
#[test]
fn revoke_and_expiry_filter_waiters_but_never_free_a_live_hold() {
    let p = tree();
    let mut s = strict_slots(&p, 1);
    s.config.max_hold_secs = 100;
    enroll(&mut s, "wk", "g1", 200);
    let g = acquire_as(&mut s, 300, 300, "r1", 0.0).unwrap();
    assert_eq!(g["granted"], true);
    let q = acquire_as(&mut s, 310, 310, "r2", 0.0).unwrap();
    assert_eq!(q["granted"], false);
    assert_eq!(s.waiting.len(), 1);
    let events = s.revoke_owner("wk", "endpoint closed");
    assert_eq!(events[0].1, "slot_enrollment_revoked");
    let (status, events) = s.status(
        SlotCaller {
            lane: "",
            pids: &[],
        },
        1.0,
    );
    assert!(status["waiting"].as_array().unwrap().is_empty());
    assert!(events
        .iter()
        .any(|e| e.1 == "slot_wait_dropped" && e.2["reason"] == "revoked"));
    let h = &status["pools"]["build"]["held"][0];
    assert_eq!(
        (h["auth_state"].as_str(), h["liveness"].as_str()),
        (Some("revoked"), Some("alive"))
    );
    // Past the hold bound: still held, now pending reconcile.
    let h = held_json(&mut s, 150.0)[0].clone();
    assert_eq!(h["accounting"], "expired_pending_reconcile");
    assert_eq!(h["reconcile_required"], true);
    // Proven death frees it — and only it.
    p.kill(300);
    let (status, events) = s.status(
        SlotCaller {
            lane: "",
            pids: &[],
        },
        151.0,
    );
    assert!(status["pools"]["build"]["held"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(events
        .iter()
        .any(|e| e.1 == "slot_released" && e.2["reason"] == "holder died"));
    // Expiry: a fresh enrollment past its lifetime admits nothing new.
    let p = tree();
    let mut s = strict_slots(&p, 2);
    enroll(&mut s, "wk", "g1", 200);
    acquire_as(&mut s, 300, 300, "r1", 0.0).unwrap();
    let late = ENROLLMENT_TTL_SECS + 1.0;
    let err = acquire_as(&mut s, 400, 400, "r2", late).unwrap_err();
    assert!(err.to_string().contains("expired"), "{err}");
    // The next reap pass records the expiry; the hold is untouched.
    let h = held_json(&mut s, late)[0].clone();
    assert_eq!(s.enrollments[0].auth, AuthState::Expired);
    assert_eq!(
        (h["auth_state"].as_str(), h["liveness"].as_str()),
        (Some("expired"), Some("alive"))
    );
}

/// ACCEPTANCE: reconcile cannot force-free — a live or unknown holder
/// is refused whatever the evidence says; evidence must name the exact
/// recorded hold; only the daemon's own proof of death frees.
#[test]
fn reconcile_cannot_force_free_a_live_or_unknown_hold() {
    let p = tree();
    let mut s = strict_slots(&p, 1);
    let id = enroll(&mut s, "wk", "g1", 200);
    let g = acquire_as(&mut s, 300, 300, "r1", 0.0).unwrap();
    let token = g["token"].as_str().unwrap().to_string();
    let evidence = json!({
        "owner_generation": "g1", "pid": 300, "starttime": 60, "uid": UID,
        "observed_at": "2026-09-23T00:00:00Z", "process_read": "exited",
        "command_outcome": "cargo finished", "side_effect_review": "none",
    });
    let err = s.reconcile(&id, &token, &evidence, 1.0).unwrap_err();
    assert!(err.to_string().contains("alive"), "{err}");
    p.garble(300);
    let err = s.reconcile(&id, &token, &evidence, 1.0).unwrap_err();
    assert!(err.to_string().contains("unknown"), "{err}");
    p.kill(300);
    let mut wrong = evidence.clone();
    wrong["starttime"] = json!(61);
    let err = s.reconcile(&id, &token, &wrong, 1.0).unwrap_err();
    assert!(err.to_string().contains("does not match"), "{err}");
    let mut partial = evidence.clone();
    partial
        .as_object_mut()
        .unwrap()
        .remove("side_effect_review");
    let err = s.reconcile(&id, &token, &partial, 1.0).unwrap_err();
    assert!(err.to_string().contains("side_effect_review"), "{err}");
    assert_eq!(s.held.len(), 1, "nothing freed by a refused reconcile");
    let (r, events) = s.reconcile(&id, &token, &evidence, 2.0).unwrap();
    assert_eq!(r["reconciled"], true);
    assert_eq!(events[0].2["evidence"]["command_outcome"], "cargo finished");
    assert!(s.held.is_empty());
}

fn read(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// ACCEPTANCE: a valid v1 file migrates its rows to `legacy_holds` —
/// never to strict authorization — at the first strict record, and
/// every legacy grant/release/reap afterwards rewrites the whole v2
/// envelope: interleaved legacy traffic never drops strict state.
#[test]
fn v1_migrates_to_legacy_holds_and_legacy_writes_keep_strict_state() {
    let p = tree();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slots.json");
    let me = std::process::id();
    std::fs::write(
        &path,
        json!({"version": 1, "holds": [
            {"token": "slot-legacy", "request_id": "r0", "kind": "build",
             "lane": "pane-1", "pid": me, "pid_start": pid_start(me),
             "acquired_epoch": 999_990.0},
        ]})
        .to_string(),
    )
    .unwrap();
    let mut s = strict_slots(&p, 3);
    s.persist_to(path.clone());
    s.restore(clk(0.0));
    assert_eq!(s.held.len(), 1);
    assert!(s.held[0].strict.is_none(), "a v1 row is never strict");
    assert_eq!(
        read(&path)["version"],
        1,
        "no strict record yet: v1 shape kept"
    );
    let id = enroll(&mut s, "wk", "g1", 200);
    let doc = read(&path);
    assert_eq!(doc["format"], "cadence-slots");
    assert_eq!(doc["version"], 2);
    assert_eq!(doc["legacy_holds"][0]["token"], "slot-legacy");
    assert!(doc["holds"].as_array().unwrap().is_empty());
    assert_eq!(doc["enrollments"][0]["enrollment_id"], id.as_str());
    acquire_as(&mut s, 300, 300, "r1", 0.0).unwrap();
    // Interleave legacy traffic: grant, release, and a reap.
    let (g, _) = s
        .acquire(SlotKind::Build, "pane-2", me, "r2", false, clk(1.0))
        .unwrap();
    let t = g["token"].as_str().unwrap().to_string();
    s.release(&t, "pane-2", me, 2.0).unwrap();
    s.config.max_hold_secs = 1;
    s.status(
        SlotCaller {
            lane: "",
            pids: &[],
        },
        50.0,
    ); // reaps slot-legacy
    let doc = read(&path);
    assert_eq!(doc["version"], 2);
    assert_eq!(doc["enrollments"].as_array().unwrap().len(), 1);
    assert_eq!(doc["holds"].as_array().unwrap().len(), 1, "{doc}");
    assert_eq!(doc["holds"][0]["enrollment_id"], id.as_str());
    assert!(doc["legacy_holds"].as_array().unwrap().is_empty(), "{doc}");
    assert!(
        doc["state_generation"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            >= 4
    );
}

/// ACCEPTANCE: an unparsable file, an unknown version, a malformed or
/// duplicate record, or a strict hold naming no enrollment rejects the
/// state file: strict admission is unavailable, the file is retained
/// byte-for-byte as evidence (legacy traffic does not overwrite it),
/// and legacy grants keep working in memory.
#[test]
fn rejected_state_file_blocks_strict_without_discarding_evidence() {
    let enrollment = json!({
        "enrollment_id": "enr-1", "owner_actor": "wk", "owner_generation": "g1",
        "root": {"pid": 200, "starttime": 50, "uid": UID},
        "worker": {"pid": 200, "starttime": 50, "uid": UID},
        "issued_epoch": 1.0, "expires_epoch": 9e9, "auth_state": "active",
    });
    let envelope = |enrollments: Value, holds: Value| {
        json!({"format": "cadence-slots", "version": 2, "state_generation": "7",
               "enrollments": enrollments, "holds": holds, "legacy_holds": []})
        .to_string()
    };
    let hold = |token: &str, enrollment_id: &str| {
        json!({"token": token, "request_id": "r", "kind": "build", "lane": "wk",
               "enrollment_id": enrollment_id, "owner_generation": "g1",
               "holder": {"pid": 300, "starttime": 60, "uid": UID},
               "acquired_epoch": 1.0})
    };
    let mut malformed = enrollment.clone();
    malformed.as_object_mut().unwrap().remove("root");
    let cases = [
        ("unparsable", "{not json".to_string()),
        (
            "unknown version",
            json!({"format": "cadence-slots", "version": 3}).to_string(),
        ),
        (
            "unknown v1-era version",
            json!({"version": 9, "holds": []}).to_string(),
        ),
        (
            "malformed enrollment",
            envelope(json!([malformed]), json!([])),
        ),
        (
            "duplicate enrollment",
            envelope(json!([enrollment.clone(), enrollment.clone()]), json!([])),
        ),
        (
            "duplicate hold",
            envelope(
                json!([enrollment.clone()]),
                json!([hold("slot-a", "enr-1"), hold("slot-a", "enr-1")]),
            ),
        ),
        (
            "unbound strict hold",
            envelope(
                json!([enrollment.clone()]),
                json!([hold("slot-a", "enr-x")]),
            ),
        ),
    ];
    for (name, text) in cases {
        let p = tree();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("slots.json");
        std::fs::write(&path, &text).unwrap();
        let mut s = strict_slots(&p, 2);
        s.persist_to(path.clone());
        s.restore(clk(0.0));
        assert!(!s.strict_available(), "{name}");
        assert!(s.enroll("wk", "g1", 200, clk(0.0)).is_err(), "{name}");
        // Legacy callers still work — in memory; the evidence stays.
        let (g, _) = s
            .acquire(
                SlotKind::Build,
                "pane-1",
                std::process::id(),
                "r",
                false,
                clk(1.0),
            )
            .unwrap();
        assert_eq!(g["granted"], true, "{name}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "{name}");
        let (status, _) = s.status(
            SlotCaller {
                lane: "",
                pids: &[],
            },
            1.0,
        );
        assert_eq!(status["strict"]["available"], false, "{name}");
        assert_eq!(status["strict"]["reconcile_required"], true, "{name}");
    }
}

/// ACCEPTANCE: a strict write that fails leaves disk and memory
/// unchanged — no grant — and blocks further strict admission.
#[test]
fn strict_write_failure_grants_nothing() {
    let p = tree();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slots.json");
    let mut s = strict_slots(&p, 2);
    s.persist_to(path.clone());
    enroll(&mut s, "wk", "g1", 200);
    let before = std::fs::read_to_string(&path).unwrap();
    // The atomic writer's tmp path is a directory now: every write fails.
    std::fs::create_dir(path.with_extension("tmp")).unwrap();
    let err = acquire_as(&mut s, 300, 300, "r1", 0.0).unwrap_err();
    assert!(err.to_string().contains("unavailable"), "{err}");
    assert!(s.held.is_empty(), "no grant on a failed write");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    assert!(!s.strict_available());
    assert!(s.strict_caller(300, 200).is_err());
}

/// ACCEPTANCE: restart validates the envelope, then enrollments, then
/// strict holds — alive retains, dead frees (named), unknown stays
/// accounted — and no waiter is ever replayed.
#[test]
fn restart_validates_enrollments_before_holds() {
    let p = tree();
    p.spawn(600, 100, 90).spawn(610, 600, 91);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slots.json");
    let mut s = strict_slots(&p, 3);
    s.persist_to(path.clone());
    enroll(&mut s, "wk", "g1", 200);
    enroll(&mut s, "wk2", "h1", 600);
    for (peer, req) in [(300, "alive"), (310, "dies"), (320, "unknown")] {
        let g = acquire_as(&mut s, peer, peer, req, 0.0).unwrap();
        assert_eq!(g["granted"], true);
    }
    // wk2's worker holds nothing yet but queues — a waiter.
    let caller = s.strict_caller(610, 600).unwrap();
    let (q, _) = s
        .acquire_strict(SlotKind::Build, &caller, 610, "queued", false, clk(0.0))
        .unwrap();
    assert_eq!(q["granted"], false);
    drop(s);
    // Between runs: one holder died, one became unreadable, and wk2's
    // provider root is gone while its worker lives on.
    p.kill(310);
    p.garble(320);
    p.kill(600);
    let mut s = strict_slots(&p, 3);
    s.persist_to(path.clone());
    let events = s.restore(clk(0.0));
    assert!(s.strict_available());
    assert!(s.waiting.is_empty(), "no waiter is replayed");
    let auth: HashMap<String, AuthState> = s
        .enrollments
        .iter()
        .map(|e| (e.owner_actor.clone(), e.auth.clone()))
        .collect();
    assert_eq!(auth["wk"], AuthState::Active);
    assert!(matches!(
        auth.get("wk2"),
        None | Some(AuthState::Revoked(_))
    ));
    let pids: Vec<u32> = s.held.iter().map(|h| h.pid).collect();
    assert_eq!(
        pids,
        vec![300, 320],
        "dead freed, alive and unknown retained"
    );
    assert!(events
        .iter()
        .any(|e| e.1 == "slot_released" && e.2["pid"] == 310 && e.2["reason"] == "holder died"));
    let held = held_json(&mut s, 1.0);
    let unknown = held.iter().find(|h| h["pid"] == 320).unwrap();
    assert_eq!(unknown["liveness"], "unknown");
    assert_eq!(unknown["reconcile_required"], true);
    // Unknown stays accounted: one slot of three is free, not two.
    let q = acquire_as(&mut s, 400, 400, "r9", 2.0).unwrap();
    assert_eq!(q["granted"], true);
    let q = acquire_as(&mut s, 400, 400, "r10", 2.0).unwrap();
    assert_eq!(q["granted"], false, "the unknown hold still counts");
}
