//! build_slot: area tests split from tests/integration.rs (CAD-426).
//! End-to-end tests: real socket daemon in-process, fake provider.
//! These exercise the observable contract — queue order, idempotency,
//! restart fencing, approval brokering, serialization — without model calls.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod common;
use common::*;

use cadence_agent::client;
use serde_json::json;
use serde_json::Value;
use std::path::Path;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tempfile::TempDir;

/// `slot_release` naming the holding (lane, pid) — the identity the
/// grant was bound to.
fn slot_release(d: &TestDaemon, token: &str, lane: &str, pid: u32) -> Value {
    d.rpc(
        "slot_release",
        json!({"token": token, "lane": lane, "pid": pid}),
    )
    .unwrap()
}

/// N+1 acquires: the last queues until a release, FIFO order is kept,
/// and slot events land on the caller's stream. Every call here runs
/// as `SELF_LANE` — identity is connection-derived (CAD-113).
#[test]
fn slot_acquire_queues_until_release() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    plant_self(&d);
    let g1 = slot_acquire(&d, "build", SELF_LANE, "r1");
    assert_eq!(g1["granted"], true);
    let t1 = g1["token"].as_str().unwrap().to_string();
    assert!(t1.starts_with("slot-"), "the daemon mints the token: {t1}");
    // The next acquire queues — answered, never hung.
    let q = slot_acquire(&d, "build", SELF_LANE, "r2");
    assert_eq!(q["granted"], false);
    assert_eq!(q["position"], 1);
    let s = d.rpc("slot_status", json!({})).unwrap();
    assert_eq!(s["pools"]["build"]["held"].as_array().unwrap().len(), 1);
    // The owner sees its own token — the hold's pid is on its chain.
    assert_eq!(
        s["pools"]["build"]["held"][0]["token"], t1,
        "the holding process's own chain sees its token"
    );
    let waiting = s["waiting"].as_array().unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0]["lane"], SELF_LANE);
    // A re-poll keeps the original place — same request id, same
    // position, no second slot_waited.
    let q = slot_acquire(&d, "build", SELF_LANE, "r2");
    assert_eq!(q["position"], 1);
    // Release frees the pool; the waiter's next poll grants.
    slot_release(&d, &t1, SELF_LANE, std::process::id());
    let g2 = slot_acquire(&d, "build", SELF_LANE, "r2");
    assert_eq!(g2["granted"], true);
    let t2 = g2["token"].as_str().unwrap().to_string();
    assert_ne!(t2, t1, "each grant mints a fresh token");
    // And a re-poll of a granted id returns the same token (the CLI's
    // poll loop depends on this idempotency).
    let again = slot_acquire(&d, "build", SELF_LANE, "r2");
    assert_eq!(again["token"], t2);
    let kinds = |a: &str| {
        d.events(a)
            .iter()
            .map(|e| e["kind"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    let own = kinds(SELF_LANE);
    assert!(own.contains(&"slot_acquired".to_string()));
    assert!(own.contains(&"slot_released".to_string()));
    assert_eq!(
        own.iter().filter(|k| *k == "slot_waited").count(),
        1,
        "one slot_waited for the whole wait: {own:?}"
    );
}

/// A holder whose pid dies frees its slot on the next acquire —
/// nothing kills the work, the slot just stops being owed by a corpse.
/// The hold binds to a lane shell's pid: killing the shell kills the
/// hold's owner.
#[test]
fn slot_dead_holder_is_reaped() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    plant_self(&d);
    let home = TempDir::new().unwrap();
    let mut holder = LaneShell::spawn(home.path());
    plant_pane(&d, "dev-1", holder.pid());
    // The holder's child claims its own pane — `$$` in the shell is
    // the planted pane pid itself.
    let g = holder.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "build", "pid": holder.pid(), "request_id": "r1"}),
    );
    assert_eq!(g["ok"], true, "{g}");
    let token = g["result"]["token"].as_str().unwrap().to_string();
    holder.child.kill().unwrap();
    holder.child.wait().unwrap(); // reap the zombie so kill(pid,0) answers ESRCH
    let g2 = slot_acquire(&d, "build", SELF_LANE, "r2");
    assert_eq!(g2["granted"], true, "dead holder's slot must free");
    // The reap names the cause on the dead lane's stream.
    let evs = d.events("dev-1");
    assert!(
        evs.iter()
            .any(|e| e["kind"].as_str() == Some("slot_released")
                && e["payload"]["reason"].as_str() == Some("holder died")),
        "{evs:?}"
    );
    // Releasing the dead token is a named refusal, not a silent pass
    // — and nobody can claim the dead pid anyway.
    let err = d
        .rpc(
            "slot_release",
            json!({"token": token, "pid": std::process::id()}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("Unknown slot token"), "{err}");
}

/// BLOCKER: two callers sharing a request_id — the second queues, it
/// never adopts the first's hold; a same-identity re-poll does. The
/// "different pid" is the test's own parent — a second pid on the
/// connection's ancestry that may legitimately be claimed (CAD-113).
#[test]
fn slot_duplicate_request_id_different_pid_queues() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    plant_self(&d);
    let parent = std::os::unix::process::parent_id();
    let g1 = slot_acquire_pid(&d, "build", SELF_LANE, parent, "r1");
    assert_eq!(g1["granted"], true);
    // Same request_id claiming a different pid — a different caller:
    // queued, never granted the first's hold.
    let q = slot_acquire(&d, "build", SELF_LANE, "r1");
    assert_eq!(q["granted"], false, "must not adopt another caller's hold");
    let s = d.rpc("slot_status", json!({})).unwrap();
    assert_eq!(s["waiting"].as_array().unwrap().len(), 1);
    // The true holder re-polls and still gets its own token.
    let again = slot_acquire_pid(&d, "build", SELF_LANE, parent, "r1");
    assert_eq!(again["token"], g1["token"]);
}

/// BLOCKER: release binds to the holding (lane, pid) — both derived
/// from the connection now. A foreign lane's release is a named
/// refusal; a claimed pid off the caller's own ancestry is refused
/// before the token is even looked at. The hold survives both.
#[test]
fn slot_release_foreign_caller_is_rejected() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    plant_self(&d);
    let home = TempDir::new().unwrap();
    let mut foreign = LaneShell::spawn(home.path());
    plant_pane(&d, "dev-2", foreign.pid());
    let g = slot_acquire(&d, "build", SELF_LANE, "r1");
    let token = g["token"].as_str().unwrap().to_string();
    // The foreign lane knows the token but its derived lane doesn't
    // match the hold — refused. (The `pid` claim is honest here.)
    let f = foreign.rpc(
        &d.state,
        "slot_release",
        json!({"token": token, "pid": foreign.pid()}),
    );
    assert_eq!(f["ok"], false, "{f}");
    assert!(
        f["error"]["message"]
            .as_str()
            .unwrap()
            .contains("another caller"),
        "{f}"
    );
    // A claimed pid off the caller's own chain — a sibling lane's pid
    // is a live pid the shell does not descend from — is refused
    // outright, before the token is even looked at.
    let sibling = LaneShell::spawn(home.path());
    let f = foreign.rpc(
        &d.state,
        "slot_release",
        json!({"token": token, "pid": sibling.pid()}),
    );
    assert_eq!(f["ok"], false, "{f}");
    assert!(
        f["error"]["message"]
            .as_str()
            .unwrap()
            .contains("cannot claim"),
        "{f}"
    );
    // The hold still stands — the pool stays full.
    let q = slot_acquire(&d, "build", SELF_LANE, "r2");
    assert_eq!(q["granted"], false, "failed release must not free the slot");
    // And the true holder releases normally.
    slot_release(&d, &token, SELF_LANE, std::process::id());
}

/// ACCEPTANCE: `slot_status` reveals a token only to the connection
/// whose derived identity owns the hold — two real lanes. The owner
/// sees its token; a foreign lane passing the owner's `lane` sees the
/// hold but never the token (CAD-113 identity fork, option A).
#[test]
fn slot_status_reveals_tokens_only_to_the_owner() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    let home = TempDir::new().unwrap();
    let mut owner = LaneShell::spawn(home.path());
    plant_pane(&d, "owner", owner.pid());
    plant_self(&d); // the foreign observer
    let g = owner.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "build", "pid": owner.pid(), "request_id": "r1"}),
    );
    assert_eq!(g["result"]["granted"], true, "{g}");
    let token = g["result"]["token"].as_str().unwrap().to_string();
    // The owner's own status reveals its token — via the real CLI
    // too: the cadence child derives this lane from its ancestry.
    let s = owner.rpc(&d.state, "slot_status", json!({}));
    let held = s["result"]["pools"]["build"]["held"].as_array().unwrap();
    assert_eq!(held[0]["token"], token, "owner sees its own token");
    let (rc, out) = owner.cadence(&d.state, "build-slot status --json");
    assert_eq!(rc, 0, "{out}");
    let cli: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        cli["pools"]["build"]["held"][0]["token"], token,
        "owner CLI sees its own token"
    );
    // The foreign lane's status sees the hold but not the token —
    // even naming the owner's lane in the request.
    let s = d.rpc("slot_status", json!({"lane": "owner"})).unwrap();
    let held = s["pools"]["build"]["held"].as_array().unwrap();
    assert_eq!(held.len(), 1);
    assert!(
        held[0].get("token").is_none(),
        "foreign caller must not see the token: {held:?}"
    );
    assert_eq!(held[0]["lane"], "owner");
}

/// ACCEPTANCE: a `slot_acquire` whose claimed `pid` is not the socket
/// peer or one of its /proc ancestors is refused — the daemon never
/// rebinds it (CAD-113 identity fork, option A).
#[test]
fn slot_acquire_refuses_a_pid_off_the_caller_chain() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    let home = TempDir::new().unwrap();
    let mut lane = LaneShell::spawn(home.path());
    plant_pane(&d, "dev-1", lane.pid());
    // A sibling lane's pid is live but off this caller's ancestry.
    let other = LaneShell::spawn(home.path());
    let r = lane.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "build", "pid": other.pid(), "request_id": "r1"}),
    );
    assert_eq!(r["ok"], false, "{r}");
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("cannot claim"),
        "{r}"
    );
    // Nothing queued or held under either identity.
    plant_self(&d);
    let s = d.rpc("slot_status", json!({})).unwrap();
    assert!(s["waiting"].as_array().unwrap().is_empty());
    assert!(s["pools"]["build"]["held"].as_array().unwrap().is_empty());
    // An honest claim — the caller's own pid — grants normally.
    let g = lane.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "build", "pid": lane.pid(), "request_id": "r2"}),
    );
    assert_eq!(g["result"]["granted"], true, "{g}");
}

/// ACCEPTANCE: a caller detached from every registered pane derives
/// no identity at all — all three slot RPCs refuse it, and nothing is
/// stamped `operator` (the PR-#71 fail-open pattern, closed here).
#[test]
fn slot_rpc_refuses_an_underivable_caller() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    let home = TempDir::new().unwrap();
    // `stray` descends from the test process but nothing in its
    // ancestry is a registered pty pane — no pane is planted for it.
    let mut stray = LaneShell::spawn(home.path());
    // `observer` is a real lane so we can inspect the pools afterward.
    let mut observer = LaneShell::spawn(home.path());
    plant_pane(&d, "observer", observer.pid());
    for (method, params) in [
        (
            "slot_acquire",
            json!({"kind": "build", "pid": stray.pid(), "request_id": "r1"}),
        ),
        (
            "slot_release",
            json!({"token": "slot-x", "pid": stray.pid()}),
        ),
        ("slot_status", json!({})),
    ] {
        let r = stray.rpc(&d.state, method, params);
        assert_eq!(r["ok"], false, "{method}: {r}");
        assert!(
            r["error"]["message"]
                .as_str()
                .unwrap()
                .contains("caller identity underivable"),
            "{method} must refuse identity-less callers: {r}"
        );
    }
    // Nothing was recorded — and especially not as `operator`.
    let s = observer.rpc(&d.state, "slot_status", json!({}));
    assert_eq!(s["ok"], true, "{s}");
    assert!(s["result"]["waiting"].as_array().unwrap().is_empty());
    assert!(s["result"]["pools"]["build"]["held"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(
        !s["result"].to_string().contains("operator"),
        "no operator identity may appear: {}",
        s["result"]
    );
}

/// BLOCKER (r3): `slot_acquired` rides the victim's event stream —
/// readable by any local caller via `agent_events`. It must never
/// carry the token: token+lane+pid are the entire release credential,
/// so a peer's stream can never be mined for one. (r5: the victim is
/// a real second connection identity — a lane shell.)
#[test]
fn slot_acquired_event_cannot_release_a_peers_hold() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    let home = TempDir::new().unwrap();
    let mut victim = LaneShell::spawn(home.path());
    plant_pane(&d, "victim", victim.pid());
    plant_self(&d); // the snoop: every d.rpc runs as SELF_LANE
    let g = victim.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "build", "pid": victim.pid(), "request_id": "r1"}),
    );
    assert_eq!(g["ok"], true, "{g}");
    let token = g["result"]["token"].as_str().unwrap().to_string();
    // The peer reads the victim's stream — sees the acquisition…
    let ev = d
        .events("victim")
        .into_iter()
        .find(|e| e["kind"].as_str() == Some("slot_acquired"))
        .expect("victim emitted slot_acquired");
    assert!(
        ev["payload"].get("token").is_none() && !ev["payload"].to_string().contains(&token),
        "slot_acquired leaks the release credential: {}",
        ev["payload"]
    );
    // …but the visible fields can't release anything: a guessed token
    // is an unknown-token rejection and the hold survives.
    let err = d
        .rpc(
            "slot_release",
            json!({"token": "slot-guess", "pid": std::process::id()}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("Unknown slot token"), "{err}");
    // Even the real token under a foreign identity is refused — the
    // derived lane (pane-self) is not the hold's lane, whatever the
    // request's `lane` field claims.
    let err = d
        .rpc(
            "slot_release",
            json!({"token": token, "lane": "victim", "pid": std::process::id()}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("another caller"), "{err}");
    // And status passing the victim's lane still shows no token.
    let s = d.rpc("slot_status", json!({"lane": "victim"})).unwrap();
    let held = s["pools"]["build"]["held"].as_array().unwrap();
    assert_eq!(held.len(), 1);
    assert!(held[0].get("token").is_none(), "foreign token hidden");
    // The owner releases normally under its own connection identity.
    let r = victim.rpc(
        &d.state,
        "slot_release",
        json!({"token": token, "pid": victim.pid()}),
    );
    assert_eq!(r["ok"], true, "{r}");
}

/// BLOCKER: holds survive a daemon restart — persisted slots.json is
/// revalidated at boot: live holders keep their slots (never
/// re-granted), dead holders are dropped with a named reason.
#[test]
fn slot_restart_revalidates_holders() {
    let state = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let d = TestDaemon::start_on_opts(state.path().to_path_buf(), slot_opts(2, 1, 900, &[]));
    // Two holds: one bound to a lane shell that dies before the
    // restart, one bound to the test process which outlives it.
    let mut doomed = LaneShell::spawn(home.path());
    plant_pane(&d, "dev-1", doomed.pid());
    let g = doomed.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "build", "pid": doomed.pid(), "request_id": "r0"}),
    );
    assert_eq!(g["ok"], true, "{g}");
    plant_self(&d);
    let live = slot_acquire(&d, "build", SELF_LANE, "r1");
    assert_eq!(live["granted"], true);
    let live_tok = live["token"].as_str().unwrap().to_string();
    let live_pid = std::process::id();
    doomed.child.kill().unwrap();
    doomed.child.wait().unwrap();
    drop(d); // shutdown → serve returns → state dir kept
    let d2 = TestDaemon::start_on_opts(state.path().to_path_buf(), slot_opts(2, 1, 900, &[]));
    // The agent rows persisted but a clean shutdown clears endpoint
    // fields — re-stamp the live pane's facts before deriving.
    plant_pane(&d2, SELF_LANE, live_pid);
    // The live hold survived with its token intact; the dead one's
    // slot was reaped — one held, one free.
    let s = d2.rpc("slot_status", json!({})).unwrap();
    let held = s["pools"]["build"]["held"].as_array().unwrap();
    assert_eq!(held.len(), 1, "one live holder survives: {held:?}");
    assert_eq!(held[0]["token"], live_tok);
    assert_eq!(held[0]["pid"], live_pid);
    // The boot reap named the dead holder's cause on its lane.
    let evs = d2.events("dev-1");
    assert!(
        evs.iter()
            .any(|e| e["kind"].as_str() == Some("slot_released")
                && e["payload"]["reason"].as_str() == Some("holder died")),
        "{evs:?}"
    );
    // And an acquire never re-grants the survivor's slot — one free
    // slot grants once, then the pool is full again.
    let g = slot_acquire(&d2, "build", SELF_LANE, "r9");
    assert_eq!(g["granted"], true);
    let q = slot_acquire(&d2, "build", SELF_LANE, "r10");
    assert_eq!(q["granted"], false, "restarted holds keep the pool bounded");
    // The survivor still releases by its minted token.
    slot_release(&d2, &live_tok, SELF_LANE, live_pid);
}

/// Regression for CI 35542407390: a planted pane row must survive a
/// daemon restart's relaunch sweep untouched. The sweep relaunches
/// every enabled actor-owning row; an actor whose open can't verify
/// the planted pane exit-detaches it — clearing the pid/generation
/// the caller-identity pane map resolves by — or a real open's
/// `set_identity` overwrites it. Either way the next slot call fails
/// closed ("descends from no registered pane"). plant_pane's rows are
/// actorless (`inbox` pair) and `enabled=0`, so the sweep never
/// touches them: the planted facts persist through the whole window.
#[test]
fn slot_planted_pane_row_survives_restart() {
    let state = TempDir::new().unwrap();
    let live_pid = std::process::id();
    let d = TestDaemon::start_on_opts(state.path().to_path_buf(), slot_opts(2, 1, 900, &[]));
    plant_pane(&d, SELF_LANE, live_pid);
    let g = slot_acquire(&d, "build", SELF_LANE, "r1");
    assert_eq!(g["granted"], true, "{g}");
    // Canary in the pre-fix shape — an enabled (fake, pty) row the boot
    // relaunch sweep must launch. Its actor's adapter build fails
    // deterministically and the exit-detach emits `attention`. The alias
    // sorts after every other agent, so once its outcome lands the sweep
    // has spawned an actor for every earlier row.
    let conn = rusqlite::Connection::open(state.path().join("cadence.sqlite3")).unwrap();
    conn.execute(
        "INSERT INTO agents(alias,provider,endpoint_kind,role,cwd,sandbox,
            state,enabled,pid,generation,session_id,created,updated)
         VALUES('zz-canary','fake','pty','worker',?1,'read-only',
            'stopped',1,0,'planted','planted',0,0)",
        [state.path().to_str().unwrap()],
    )
    .unwrap();
    drop(conn);
    drop(d);
    let d2 = TestDaemon::start_on_opts(state.path().to_path_buf(), slot_opts(2, 1, 900, &[]));
    plant_pane(&d2, SELF_LANE, live_pid);
    // Positive window-closed signal (CAD-221): never assert absence
    // inside a window that may not have opened. The canary's `attention`
    // proves the sweep ran and an actor outcome landed on the very path
    // that would destroy a vulnerable planted row — the lane assertion
    // below is made only after that window provably closed.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if d2
            .events("zz-canary")
            .iter()
            .any(|e| e["kind"].as_str() == Some("attention"))
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "canary never detached — the relaunch sweep did not run: {:?}",
            d2.events("zz-canary")
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let conn = rusqlite::Connection::open(state.path().join("cadence.sqlite3")).unwrap();
    let p: i64 = conn
        .query_row("SELECT pid FROM agents WHERE alias=?", [SELF_LANE], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(p as u32, live_pid, "no actor clobbered the plant");
    assert!(
        !d2.events(SELF_LANE)
            .iter()
            .any(|e| e["kind"].as_str() == Some("attention")),
        "no actor should ever have launched: {:?}",
        d2.events(SELF_LANE)
    );
    let g = slot_acquire(&d2, "build", SELF_LANE, "r9");
    assert_eq!(g["granted"], true, "{g}");
}

/// CAD-230 phase a ACCEPTANCE: the daemon enrolls a managed provider
/// it launched (no tmux pane) from the pid it recorded. A build-slot
/// acquire from that exact process, and from a verified descendant
/// (its tool subprocess), is admitted under the owner's lane whatever
/// the request claims; a double-forked detached grandchild, a pane-less
/// unrelated process and a forged alias/lane/pid are refused. Owner
/// generation drift then revokes: no new work — and no fallback to an
/// outer pane even with one registered above it — while the live hold
/// stays accounted until its exact holder releases it.
#[test]
fn slot_managed_endpoint_enrolls_and_admits_only_its_verified_processes() {
    let d = TestDaemon::start_opts(slot_opts(2, 1, 900, &[]));
    let home = TempDir::new().unwrap();
    let mut observer = LaneShell::spawn(home.path());
    plant_pane(&d, "observer", observer.pid());
    let mut wk = ManagedWorker::start(&d, "wk");
    let status = |observer: &mut LaneShell| {
        let s = observer.rpc(&d.state, "slot_status", json!({}));
        assert_eq!(s["ok"], true, "{s}");
        s["result"].clone()
    };
    let enrollment = status(&mut observer)["enrollments"][0].clone();
    assert_eq!(enrollment["owner_actor"], "wk");
    assert_eq!(enrollment["auth_state"], "active");
    assert_eq!(enrollment["root"]["pid"], wk.pid);
    assert_eq!(enrollment["root"], enrollment["worker"]);

    // The enrolled root itself — a forged lane in the request changes
    // nothing: the hold is the owner's.
    let g = wk.rpc(
        "self",
        "slot_acquire",
        json!({"kind": "build", "pid": "$PID", "request_id": "root-1",
               "lane": "observer"}),
    );
    assert_eq!(g["result"]["granted"], true, "{g}");
    let root_token = g["result"]["token"].as_str().unwrap().to_string();
    // A verified descendant (the provider's tool subprocess) is
    // admitted too, holding for its own pid; it exits right after, so
    // its hold is the next reap's proven death.
    let g = wk.rpc(
        "child",
        "slot_acquire",
        json!({"kind": "suite", "pid": "$PID", "request_id": "child-1"}),
    );
    assert_eq!(g["result"]["granted"], true, "{g}");
    let s = status(&mut observer);
    let held = s["pools"]["build"]["held"].as_array().unwrap();
    assert_eq!(held.len(), 1, "{s}");
    assert_eq!(held[0]["lane"], "wk");
    assert_eq!(held[0]["pid"], wk.pid);
    assert_eq!(held[0]["binding"], "strict");
    assert_eq!(held[0]["auth_state"], "active");
    assert_eq!(held[0]["liveness"], "alive");
    assert_eq!(held[0]["accounting"], "held");
    assert_eq!(held[0]["owner_generation"], enrollment["owner_generation"]);
    assert!(held[0].get("token").is_none(), "the observer never sees it");
    assert!(
        s["pools"]["suite"]["held"].as_array().unwrap().is_empty(),
        "the exited descendant's hold was reaped: {s}"
    );
    assert!(d
        .events("wk")
        .iter()
        .any(|e| e["kind"] == "slot_released" && e["payload"]["reason"] == "holder died"));

    // Off the root's ancestry (setsid + double fork): refused.
    let r = wk.rpc(
        "detached",
        "slot_acquire",
        json!({"kind": "build", "pid": "$PID", "request_id": "detached-1"}),
    );
    assert_eq!(r["ok"], false, "{r}");
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("caller identity underivable"),
        "{r}"
    );
    // A pane-less unrelated process claiming the alias, the lane and
    // even the root's pid: refused.
    let mut stray = LaneShell::spawn(home.path());
    let r = stray.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "build", "pid": wk.pid, "request_id": "forged-1",
               "lane": "wk", "alias": "wk"}),
    );
    assert_eq!(r["ok"], false, "{r}");
    let (rc, out) = stray.cadence(
        &d.state,
        "build-slot acquire build --pid $$ --lane wk --wait-secs 0",
    );
    assert_ne!(rc, 0, "{out}");
    assert!(out.contains("caller identity underivable"), "{out}");
    let r = stray.run(&format!(
        "CADENCE_ALIAS=wk {} --state-dir {} build-slot status",
        env!("CARGO_BIN_EXE_cadence"),
        d.state.display()
    ));
    assert_ne!(r.0, 0, "an env alias is no identity: {}", r.1);
    assert_eq!(
        status(&mut observer)["pools"]["build"]["held"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "no refused caller got a hold"
    );

    // Owner generation drift: plant the test process as a pane ABOVE
    // the provider first — the strict path must not fall through to it.
    plant_self(&d);
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE agents SET generation='drifted' WHERE alias='wk'",
        [],
    )
    .unwrap();
    drop(conn);
    let r = wk.rpc(
        "self",
        "slot_acquire",
        json!({"kind": "build", "pid": "$PID", "request_id": "root-2"}),
    );
    assert_eq!(r["ok"], false, "{r}");
    assert!(
        r["error"]["message"].as_str().unwrap().contains("revoked"),
        "{r}"
    );
    let s = status(&mut observer);
    let held = s["pools"]["build"]["held"].as_array().unwrap();
    assert_eq!(held.len(), 1, "revocation frees nothing: {s}");
    assert_eq!(held[0]["auth_state"], "revoked");
    assert_eq!(held[0]["liveness"], "alive");
    // Only the exact holder's release frees it.
    let r = wk.rpc(
        "self",
        "slot_release",
        json!({"token": root_token, "pid": "$PID"}),
    );
    assert_eq!(r["result"]["released"], true, "{r}");
    assert!(status(&mut observer)["pools"]["build"]["held"]
        .as_array()
        .unwrap()
        .is_empty());
}

/// CAD-230 review BLOCKING #1: once a managed endpoint's enrollment is
/// revoked, its provider must never fall through to a pane registered
/// ABOVE it — not when it held nothing at the drift, and not after it
/// released its last hold. Each acquire is refused and no hold of any
/// binding (legacy included) appears.
#[test]
fn slot_revoked_managed_endpoint_never_falls_back_to_an_outer_pane() {
    let d = TestDaemon::start_opts(slot_opts(2, 1, 900, &[]));
    let home = TempDir::new().unwrap();
    let mut observer = LaneShell::spawn(home.path());
    plant_pane(&d, "observer", observer.pid());
    let mut wk = ManagedWorker::start(&d, "wk");
    // The outer pane: the test process, an ancestor of the provider.
    plant_self(&d);
    let held = |observer: &mut LaneShell| {
        let s = observer.rpc(&d.state, "slot_status", json!({}));
        assert_eq!(s["ok"], true, "{s}");
        let mut all = s["result"]["pools"]["build"]["held"]
            .as_array()
            .unwrap()
            .clone();
        all.extend(
            s["result"]["pools"]["suite"]["held"]
                .as_array()
                .unwrap()
                .clone(),
        );
        all
    };
    let drift = |generation: &str| {
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "UPDATE agents SET generation=?1 WHERE alias='wk'",
            [generation],
        )
        .unwrap();
    };
    let refused = |r: &Value, route: &str| {
        assert_eq!(r["ok"], false, "{route}: {r}");
        assert!(
            r["error"]["message"].as_str().unwrap().contains("revoked"),
            "{route}: {r}"
        );
    };

    // Route: drift while holding NOTHING — twice, as the reviewer did.
    drift("drift-1");
    for req in ["a1", "a2"] {
        let r = wk.rpc(
            "self",
            "slot_acquire",
            json!({"kind": "build", "pid": "$PID", "request_id": req}),
        );
        refused(&r, "no-hold drift");
    }
    assert!(held(&mut observer).is_empty(), "no legacy hold appeared");

    // Route: release of the last hold after revocation. A fresh
    // enrollment needs a reopen; take a new endpoint for the same
    // provider mock under another alias.
    let mut wk2 = ManagedWorker::start(&d, "wk2");
    let g = wk2.rpc(
        "self",
        "slot_acquire",
        json!({"kind": "build", "pid": "$PID", "request_id": "b1"}),
    );
    assert_eq!(g["result"]["granted"], true, "{g}");
    let token = g["result"]["token"].as_str().unwrap().to_string();
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE agents SET generation='drift-2' WHERE alias='wk2'",
        [],
    )
    .unwrap();
    drop(conn);
    let r = wk2.rpc(
        "self",
        "slot_release",
        json!({"token": token, "pid": "$PID"}),
    );
    assert_eq!(r["result"]["released"], true, "{r}");
    for req in ["b2", "b3"] {
        let r = wk2.rpc(
            "self",
            "slot_acquire",
            json!({"kind": "build", "pid": "$PID", "request_id": req}),
        );
        refused(&r, "last-hold release");
    }
    assert!(held(&mut observer).is_empty(), "no legacy hold appeared");
    // The revoked endpoints are still listed — the tombstones that
    // keep the outer pane out.
    let s = observer.rpc(&d.state, "slot_status", json!({}));
    let revoked = s["result"]["enrollments"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["auth_state"] == "revoked")
        .count();
    assert_eq!(revoked, 2, "{s}");
}

/// CAD-230: `slot_reconcile` is operator authority — an agent's
/// connection (a pane or an enrolled endpoint) is refused, as are
/// identity-shaped request fields; and it never frees a live hold.
#[test]
fn slot_reconcile_refuses_agents_and_live_holds() {
    let d = TestDaemon::start_opts(slot_opts(2, 1, 900, &[]));
    let home = TempDir::new().unwrap();
    let mut pane = LaneShell::spawn(home.path());
    plant_pane(&d, "pane-1", pane.pid());
    let mut wk = ManagedWorker::start(&d, "wk");
    let g = wk.rpc(
        "self",
        "slot_acquire",
        json!({"kind": "build", "pid": "$PID", "request_id": "r1"}),
    );
    let token = g["result"]["token"].as_str().unwrap().to_string();
    let s = pane.rpc(&d.state, "slot_status", json!({}));
    let hold = s["result"]["pools"]["build"]["held"][0].clone();
    let enrollment = hold["enrollment_id"].as_str().unwrap().to_string();
    let start = s["result"]["enrollments"][0]["root"]["starttime"].clone();
    let evidence = json!({
        "owner_generation": hold["owner_generation"], "pid": wk.pid,
        "starttime": start, "uid": s["result"]["enrollments"][0]["root"]["uid"],
        "observed_at": "now", "process_read": "claimed exited",
        "command_outcome": "done", "side_effect_review": "none",
    });
    let params = json!({"enrollment_id": enrollment, "token": token,
                        "evidence": evidence});
    // A pane and the enrolled endpoint itself are agents.
    let r = pane.rpc(&d.state, "slot_reconcile", params.clone());
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("operator action"),
        "{r}"
    );
    let r = wk.rpc("self", "slot_reconcile", params.clone());
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("operator action"),
        "{r}"
    );
    // The operator (`operator_rpc`: no pane, no enrollment, no agent
    // ancestry) with a forged `by` is refused before anything else…
    let mut forged = params.clone();
    forged["by"] = json!("operator");
    let err = d.operator_rpc("slot_reconcile", forged).unwrap_err();
    assert!(err.to_string().contains("'by'"), "{err}");
    // …and without it still cannot free a live holder.
    let err = d.operator_rpc("slot_reconcile", params).unwrap_err();
    assert!(err.to_string().contains("is alive"), "{err}");
    let s = pane.rpc(&d.state, "slot_status", json!({}));
    assert_eq!(
        s["result"]["pools"]["build"]["held"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

/// CAD-276 item 1: `slot_reconcile` is the operator's only on POSITIVE
/// proof — deriving no slot identity is not enough. A `setsid` +
/// double-fork detach off a managed tool and off a registered pane
/// derives no slot identity at all, yet is refused: with
/// `CADENCE_ALIAS` still in its environment by the alias, and with the
/// alias scrubbed by its orphaned session (the session leader exited).
/// The harness's operator caller (`operator_rpc`: no pane, no endpoint,
/// no alias on its ancestry) passes the gate and meets the live-hold rule.
#[test]
fn slot_reconcile_refuses_detached_agent_processes() {
    let d = TestDaemon::start_opts(slot_opts(2, 1, 900, &[]));
    let home = TempDir::new().unwrap();
    let mut pane = LaneShell::spawn(home.path());
    plant_pane(&d, "pane-1", pane.pid());
    let mut wk = ManagedWorker::start(&d, "wk");
    let g = wk.rpc(
        "self",
        "slot_acquire",
        json!({"kind": "build", "pid": "$PID", "request_id": "r1"}),
    );
    let token = g["result"]["token"].as_str().unwrap().to_string();
    let s = pane.rpc(&d.state, "slot_status", json!({}));
    let hold = s["result"]["pools"]["build"]["held"][0].clone();
    let root = s["result"]["enrollments"][0]["root"].clone();
    let params = json!({
        "enrollment_id": hold["enrollment_id"], "token": token,
        "evidence": {
            "owner_generation": hold["owner_generation"], "pid": root["pid"],
            "starttime": root["starttime"], "uid": root["uid"],
            "observed_at": "now", "process_read": "claimed exited",
            "command_outcome": "done", "side_effect_review": "none",
        },
    });
    let refused = |r: &Value, route: &str, why: &str| {
        assert_eq!(r["ok"], false, "{route}: {r}");
        let msg = r["error"]["message"].as_str().unwrap_or_default();
        assert!(msg.contains("not provably the operator"), "{route}: {r}");
        assert!(msg.contains(why), "{route}: wanted '{why}': {r}");
    };

    // Off a managed tool: the provider's env carries CADENCE_ALIAS.
    let r = wk.rpc("detached", "slot_reconcile", params.clone());
    refused(&r, "managed detach", "CADENCE_ALIAS");
    let r = wk.rpc("detached-bare", "slot_reconcile", params.clone());
    refused(&r, "managed detach, alias scrubbed", "session leader");

    // Off a registered pane — the same double fork, run from the pane.
    let script = d.dir.path().join("claude-enroll.py");
    let frame = json!({"method": "slot_reconcile", "params": params}).to_string();
    assert!(
        !frame.contains('\''),
        "the frame rides a single-quoted argv"
    );
    let outs = TempDir::new().unwrap();
    for (i, (env, why)) in [
        ("CADENCE_ALIAS=pane-1", "CADENCE_ALIAS"),
        ("env -u CADENCE_ALIAS", "session leader"),
    ]
    .into_iter()
    .enumerate()
    {
        let out = outs.path().join(format!("pane-{i}.json"));
        let (rc, text) = pane.run(&format!(
            "{env} python3 {} --detached {} '{frame}' {} {}",
            script.display(),
            client::socket_path(&d.state).display(),
            out.display(),
            pane.pid()
        ));
        assert_eq!(rc, 0, "{text}");
        let deadline = Instant::now() + Duration::from_secs(20);
        while !out.exists() {
            assert!(Instant::now() < deadline, "pane detach {i} never answered");
            thread::sleep(Duration::from_millis(20));
        }
        let r: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        refused(&r, &format!("pane detach ({env})"), why);
    }

    // The operator passes the gate; the live holder is still refused.
    let err = d.operator_rpc("slot_reconcile", params).unwrap_err();
    assert!(err.to_string().contains("is alive"), "{err}");
    let s = pane.rpc(&d.state, "slot_status", json!({}));
    assert_eq!(
        s["result"]["pools"]["build"]["held"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "no refused caller freed anything: {s}"
    );
}

/// CAD-276 item 4, end to end through the CLI a managed tool runs: a
/// manual `acquire --pid <provider root>` from the provider's tool
/// subprocess is refused (the root outlives the work), while
/// `build-slot run` — which binds its own pid and execs the command —
/// is granted, and the hold dies with the command.
#[test]
fn slot_managed_tool_cannot_bind_the_provider_root() {
    let d = TestDaemon::start_opts(slot_opts(2, 1, 900, &[]));
    let home = TempDir::new().unwrap();
    let mut observer = LaneShell::spawn(home.path());
    plant_pane(&d, "observer", observer.pid());
    let mut wk = ManagedWorker::start(&d, "wk");
    let cadence = env!("CARGO_BIN_EXE_cadence");
    let state = d.state.to_str().unwrap().to_string();
    let root = wk.pid.to_string();
    let r = wk.exec(&[
        cadence,
        "--state-dir",
        &state,
        "build-slot",
        "acquire",
        "build",
        "--pid",
        &root,
        "--wait-secs",
        "0",
    ]);
    assert_ne!(r["rc"], 0, "{r}");
    assert!(
        r["err"]
            .as_str()
            .unwrap()
            .contains("enrolled provider root"),
        "{r}"
    );
    let r = wk.exec(&[
        cadence,
        "--state-dir",
        &state,
        "build-slot",
        "run",
        "build",
        "--wait-secs",
        "10",
        "--",
        "sh",
        "-c",
        "echo token=$CADENCE_BUILD_SLOT_TOKEN pid=$CADENCE_BUILD_SLOT_PID pid_now=$$",
    ]);
    assert_eq!(r["rc"], 0, "{r}");
    let out = r["out"].as_str().unwrap();
    assert!(out.contains("token=slot-"), "{r}");
    // The hold bound the exec'd command's own pid.
    let pid = out.split("pid=").nth(1).unwrap().split_whitespace().next();
    let now = out
        .split("pid_now=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next();
    assert_eq!(pid, now, "{r}");
    // The command exited: its hold is the next read's proven death.
    let s = observer.rpc(&d.state, "slot_status", json!({}));
    assert!(
        s["result"]["pools"]["build"]["held"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{s}"
    );
    assert!(d.events("wk").iter().any(|e| e["kind"] == "slot_acquired"));
}

// ---------- CAD-230 phase b: exec-bound run, daemon-launched runners ----------

/// CAD-230 phase b1 ACCEPTANCE: a managed endpoint's `build-slot run`
/// holds a strict, exec-bound slot naming exactly the process it execs
/// into (`$$` of the command IS the recorded holder). While it runs, no
/// other process can release it — a pane agent naming the token and the
/// holder pid, or its own pid, is refused. An exec request claiming an
/// ancestor is refused outright. When the command exits the daemon's
/// own watcher frees the hold — observed on the event stream, with no
/// release call and no slot call from anyone.
#[test]
fn build_slot_run_exec_bound_hold_ends_with_the_command() {
    let d = TestDaemon::start_opts(slot_opts(2, 1, 900, &[]));
    let home = TempDir::new().unwrap();
    let mut observer = LaneShell::spawn(home.path());
    plant_pane(&d, "observer", observer.pid());
    let mut wk = ManagedWorker::start(&d, "wk");
    // `exec` must claim the requesting peer itself, never an ancestor.
    let r = wk.rpc(
        "child",
        "slot_acquire",
        json!({"kind": "build", "pid": wk.pid, "request_id": "anc", "exec": true}),
    );
    assert_eq!(r["ok"], false, "{r}");
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("exec-bound"),
        "{r}"
    );
    let work = TempDir::new().unwrap();
    let tok = work.path().join("tok");
    let go = work.path().join("go");
    let script = format!(
        "echo \"$CADENCE_BUILD_SLOT_TOKEN $CADENCE_BUILD_SLOT_PID $$\" > {t}.tmp && \
         mv {t}.tmp {t}; while [ ! -e {g} ]; do sleep 0.05; done; exit 7",
        t = tok.display(),
        g = go.display()
    );
    let n = wk.send(json!({"how": "exec", "argv": [
        env!("CARGO_BIN_EXE_cadence"), "--state-dir", d.state.to_str().unwrap(),
        "build-slot", "run", "build", "--wait-secs", "10", "--", "sh", "-c", script,
    ]}));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !tok.exists() {
        assert!(Instant::now() < deadline, "run never started its command");
        thread::sleep(Duration::from_millis(20));
    }
    let line = std::fs::read_to_string(&tok).unwrap();
    let f: Vec<&str> = line.split_whitespace().collect();
    let (token, holder) = (f[0].to_string(), f[1].parse::<u64>().unwrap());
    assert_eq!(f[1], f[2], "the exec'd command is the bound pid: {line}");
    let s = observer.rpc(&d.state, "slot_status", json!({}));
    let held = s["result"]["pools"]["build"]["held"].as_array().unwrap();
    assert_eq!(held.len(), 1, "{s}");
    assert_eq!(held[0]["pid"], holder);
    assert_eq!(held[0]["lane"], "wk");
    assert_eq!(held[0]["binding"], "strict");
    assert_eq!(held[0]["exec_bound"], true);
    // Forged releases from another process are refused.
    let r = observer.rpc(
        &d.state,
        "slot_release",
        json!({"token": token, "pid": holder, "lane": "wk"}),
    );
    assert_eq!(r["ok"], false, "{r}");
    let r = observer.rpc(&d.state, "slot_release", json!({"token": token}));
    assert_eq!(r["ok"], false, "{r}");
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("another caller"),
        "{r}"
    );
    std::fs::write(&go, "").unwrap();
    let r = wk.answer(n, "exec-bound run");
    assert_eq!(r["rc"], 7, "run exits with the command's code: {r}");
    // Freed by the daemon on its own — no slot call is made here.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let freed = d.events("wk").iter().any(|e| {
            e["kind"] == "slot_released"
                && e["payload"]["pid"] == holder
                && ["holder died", "holder exited"]
                    .contains(&e["payload"]["reason"].as_str().unwrap_or(""))
        });
        if freed {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the watcher never freed the hold: {:?}",
            d.events("wk")
        );
        thread::sleep(Duration::from_millis(50));
    }
    let s = observer.rpc(&d.state, "slot_status", json!({}));
    assert!(
        s["result"]["pools"]["build"]["held"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{s}"
    );
}

/// A tracker project `p` for runner tests (CAD-230b): one git repo with
/// a commit and `recipes` (YAML, already indented) under
/// `build.recipes`. This test's daemon — and every CLI and lane shell
/// spawned after this call — reads the tracker through its own
/// `CADENCE_PM_DIR`.
fn runner_project(recipes: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("README"), "runner fixture\n").unwrap();
    for args in [
        &["init", "-q"][..],
        &["add", "."],
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "fixture",
        ],
    ] {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(pm.join("p")).unwrap();
    std::fs::write(
        pm.join("p/project.yaml"),
        format!(
            "key: p\nprefix: P\nrepos:\n- path: {}\nbuild:\n  recipes:\n{recipes}",
            repo.display()
        ),
    )
    .unwrap();
    test_env().set("CADENCE_PM_DIR", pm.to_str().unwrap());
    (dir, repo)
}

/// One recipe line: `name` running `sh -c script` with `env` allowlisted.
fn sh_recipe(name: &str, script: &str, env: &[&str]) -> String {
    format!(
        "    {name}:\n      argv: [sh, -c, {}]\n      env: {}\n",
        serde_json::to_string(script).unwrap(),
        serde_json::to_string(env).unwrap()
    )
}

/// `text` as one single-quoted POSIX shell word.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The runner id a `build-slot launch` announced on stderr.
fn launched_runner(stderr: &str) -> String {
    stderr
        .split_whitespace()
        .find(|w| w.starts_with("run-"))
        .unwrap_or_else(|| panic!("no runner id in: {stderr}"))
        .to_string()
}

/// CAD-230 phase b2 ACCEPTANCE: an authorized pane agent launches a
/// project's fixed recipe. The daemon runs it under a strict,
/// exec-bound slot held by exactly the process it spawned (the
/// recipe's `$$`), accounted to the requester's lane; the recipe's own
/// tree may read status but may not nest a slot request; the CLI
/// streams the log and exits with the recipe's code; the receipt binds
/// recipe, launch digest and source HEAD; and the exit frees the slot
/// and prunes the runner's enrollment.
#[test]
fn build_slot_launch_runs_a_fixed_recipe_under_an_exec_bound_slot() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    plant_self(&d);
    let home = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let cadence = env!("CARGO_BIN_EXE_cadence");
    let state = d.state.display();
    let status = out.path().join("status.json");
    let nested = out.path().join("nested.txt");
    let script = format!(
        "echo ok; echo \"pid $$\"; {cadence} --state-dir {state} build-slot status --json \
         > {}; {cadence} --state-dir {state} build-slot acquire build --pid $$ > {} 2>&1; \
         exit 3",
        status.display(),
        nested.display()
    );
    let (_proj, repo) = runner_project(&sh_recipe("ok", &script, &["PATH", "HOME"]));
    let run = cadence_at(
        home.path(),
        &d.state,
        &["build-slot", "launch", "ok", "--project", "p"],
    );
    let (stdout, stderr) = (
        String::from_utf8_lossy(&run.stdout).to_string(),
        String::from_utf8_lossy(&run.stderr).to_string(),
    );
    assert_eq!(run.status.code(), Some(3), "{stdout}\n{stderr}");
    assert!(stdout.starts_with("ok\n"), "the log streams: {stdout}");
    let id = launched_runner(&stderr);
    let r = cadence_at(home.path(), &d.state, &["build-slot", "runner", &id]);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let receipt: Value = serde_json::from_slice(&r.stdout).unwrap();
    assert_eq!(receipt["state"], "exited", "{receipt}");
    assert_eq!(receipt["exit_code"], 3);
    assert_eq!(receipt["complete"], true);
    assert_eq!(
        (receipt["project"].as_str(), receipt["recipe"].as_str()),
        (Some("p"), Some("ok"))
    );
    assert_eq!(
        receipt["requester"],
        json!({"kind": "pane", "lane": SELF_LANE})
    );
    assert_eq!(receipt["argv"], json!(["sh", "-c", script]));
    assert_eq!(receipt["digest"].as_str().unwrap().len(), 64);
    let head = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert_eq!(
        receipt["head_sha"].as_str().unwrap(),
        String::from_utf8_lossy(&head.stdout).trim()
    );
    assert!(receipt["started"].as_f64().is_some() && receipt["ended"].as_f64().is_some());
    assert_eq!(receipt["dirty"], false);
    let pid = receipt["pid"].as_u64().unwrap();
    let log = std::fs::read_to_string(receipt["log_path"].as_str().unwrap()).unwrap();
    assert!(log.contains("ok\n"), "{log}");
    // The exec'd recipe is the enrolled process: exec kept the pid.
    assert!(log.contains(&format!("pid {pid}\n")), "{log}");
    // While it ran: one strict, exec-bound hold on the requester's lane,
    // held by exactly that process under the runner's enrollment.
    let s: Value = serde_json::from_str(&std::fs::read_to_string(&status).unwrap()).unwrap();
    let held = s["pools"]["build"]["held"].as_array().unwrap();
    assert_eq!(held.len(), 1, "{s}");
    assert_eq!(held[0]["pid"], pid);
    assert_eq!(held[0]["lane"], SELF_LANE);
    assert_eq!(held[0]["binding"], "strict");
    assert_eq!(held[0]["exec_bound"], true);
    assert_eq!(held[0]["enrollment_id"], receipt["enrollment_id"]);
    assert!(s["enrollments"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["runner_id"] == id.as_str() && e["root"]["pid"] == pid));
    let nested = std::fs::read_to_string(&nested).unwrap();
    assert!(nested.contains("already holds its slot"), "{nested}");
    // After: the slot is free and the runner's enrollment is gone.
    let s = d.rpc("slot_status", json!({})).unwrap();
    assert!(
        s["pools"]["build"]["held"].as_array().unwrap().is_empty(),
        "{s}"
    );
    assert!(!s["enrollments"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["runner_id"] == id.as_str()));
    assert!(d
        .events(SELF_LANE)
        .iter()
        .any(|e| e["kind"] == "runner_finished" && e["payload"]["runner_id"] == id.as_str()));
}

/// CAD-230 phase b2 ACCEPTANCE: the refusals. An unknown recipe names
/// what the project defines; a request carrying any command-,
/// environment- or identity-shaped field is refused (the caller never
/// supplies argv, cwd or env); trailing CLI argv is refused; a caller
/// with no pane, no enrolled endpoint and no operator proof is refused
/// with the rule named; and none of it launches anything. The same
/// recipe launches for the pane agent and for an enrolled managed
/// endpoint.
#[test]
fn build_slot_launch_refuses_unknown_recipes_injection_and_unproven_callers() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    let home = TempDir::new().unwrap();
    let (proj, _repo) = runner_project(&sh_recipe("ok", "echo fixed", &[]));
    let mut agent = LaneShell::spawn(home.path());
    plant_pane(&d, "dev-1", agent.pid());
    let (rc, out) = agent.cadence(&d.state, "build-slot launch nope --project p");
    assert_ne!(rc, 0, "{out}");
    assert!(
        out.contains("Unknown recipe 'nope'") && out.contains("it defines: ok"),
        "{out}"
    );
    for (field, value) in [
        ("argv", json!(["sh", "-c", "echo injected"])),
        ("cmd", json!("echo injected")),
        ("env", json!({"LD_PRELOAD": "x"})),
        ("cwd", json!("/")),
        ("lane", json!("dev-1")),
        ("alias", json!("dev-1")),
        ("pid", json!(1)),
    ] {
        let mut params = json!({"recipe": "ok", "project": "p"});
        params[field] = value;
        let r = agent.rpc(&d.state, "slot_launch", params);
        assert_eq!(r["ok"], false, "{field}: {r}");
        let msg = r["error"]["message"].as_str().unwrap();
        assert!(
            msg.contains(&format!("'{field}' is refused")),
            "{field}: {msg}"
        );
    }
    let (rc, out) = agent.cadence(
        &d.state,
        "build-slot launch ok --project p -- echo injected",
    );
    assert_ne!(rc, 0, "trailing argv is refused: {out}");
    let mut stray = LaneShell::spawn(home.path());
    let r = stray.rpc(
        &d.state,
        "slot_launch",
        json!({"recipe": "ok", "project": "p"}),
    );
    assert_eq!(r["ok"], false, "{r}");
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("needs a pane agent, an enrolled managed endpoint or the proven operator"),
        "{r}"
    );
    let receipts = |state: &Path| {
        std::fs::read_dir(cadence_agent::runner::runners_dir(state))
            .map(|d| {
                d.flatten()
                    .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
                    .count()
            })
            .unwrap_or(0)
    };
    assert_eq!(receipts(&d.state), 0, "no refusal launched anything");
    // The same recipe, for callers the daemon can admit.
    let (rc, out) = agent.cadence(&d.state, "build-slot launch ok --project p");
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("fixed"), "{out}");
    let mut wk = ManagedWorker::start(&d, "wk");
    let pm = proj.path().join("pm");
    let r = wk.exec(&[
        "env",
        &format!("CADENCE_PM_DIR={}", pm.display()),
        env!("CARGO_BIN_EXE_cadence"),
        "--state-dir",
        d.state.to_str().unwrap(),
        "build-slot",
        "launch",
        "ok",
        "--project",
        "p",
    ]);
    assert_eq!(r["rc"], 0, "{r}");
    assert!(r["out"].as_str().unwrap().contains("fixed"), "{r}");
    assert_eq!(receipts(&d.state), 2);
}

/// CAD-230 phase b2 (review): what of a runner's tree is contained. A
/// descendant that detaches (`setsid -f`, reparented off the runner)
/// but keeps the runner's environment is still not the operator, so it
/// cannot launch as `(operator)`; and a straggler the recipe left in its
/// process group is ended with the recipe, never left building after
/// its slot frees. A detached descendant that ALSO scrubs the runner's
/// environment is refused too, by the daemon-descendant rule under the
/// child subreaper (CAD-308) — see
/// `build_slot_launch_detached_scrubbed_descendant_is_refused`.
#[test]
fn build_slot_launch_runner_tree_is_contained() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    plant_self(&d);
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let req = work.path().join("req.json");
    let resp = work.path().join("resp.json");
    let straggler = work.path().join("straggler");
    std::fs::write(
        &req,
        cadence_agent::proto::request("slot_launch", json!({"recipe": "ok", "project": "p"}))
            .to_string(),
    )
    .unwrap();
    let py = format!(
        "import socket;s=socket.socket(socket.AF_UNIX);s.connect({sock:?});\
         s.sendall(open({req:?},'rb').read()+b'\\n');\
         open({out:?}+'.tmp','w').write(s.makefile().readline());\
         import os;os.rename({out:?}+'.tmp',{out:?})",
        sock = client::socket_path(&d.state).display().to_string(),
        req = req.display().to_string(),
        out = resp.display().to_string(),
    );
    let script = format!(
        "sleep 60 & echo $! > {st}; setsid -f python3 -c {py}; \
         while [ ! -e {r} ]; do sleep 0.05; done; exit 0",
        st = straggler.display(),
        py = shell_quote(&py),
        r = resp.display()
    );
    let (_proj, _repo) = runner_project(&sh_recipe("ok", &script, &["PATH"]));
    let run = cadence_at(
        home.path(),
        &d.state,
        &["build-slot", "launch", "ok", "--project", "p"],
    );
    assert_eq!(
        run.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let answer: Value = serde_json::from_str(&std::fs::read_to_string(&resp).unwrap()).unwrap();
    assert_eq!(answer["ok"], false, "{answer}");
    let msg = answer["error"]["message"].as_str().unwrap();
    assert!(msg.contains("CADENCE_RUNNER_ID"), "{msg}");
    let pid: u32 = std::fs::read_to_string(&straggler)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let gone = stat.is_empty()
            || stat
                .rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'));
        if gone {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "straggler {pid} outlived its recipe"
        );
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        std::fs::read_dir(cadence_agent::runner::runners_dir(&d.state))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
            .count(),
        1,
        "the detached descendant launched nothing"
    );
}

/// Wait until `pid` has no `/proc` entry — REAPED, not merely exited:
/// an unreaped zombie keeps its entry (state `Z`).
fn wait_reaped(pid: u32, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Path::new(&format!("/proc/{pid}")).exists() {
        assert!(
            Instant::now() < deadline,
            "{what} (pid {pid}) was never reaped: {}",
            std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// Wait until the daemon has no zombie child left (bounded).
fn wait_no_daemon_zombies(daemon_pid: u64) {
    let zombies = || -> Vec<u32> {
        let Ok(tasks) = std::fs::read_dir(format!("/proc/{daemon_pid}/task")) else {
            return Vec::new();
        };
        tasks
            .flatten()
            .filter_map(|t| std::fs::read_to_string(t.path().join("children")).ok())
            .flat_map(|c| {
                c.split_whitespace()
                    .filter_map(|p| p.parse::<u32>().ok())
                    .collect::<Vec<_>>()
            })
            .filter(|pid| {
                std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
                    s.rsplit_once(')')
                        .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'))
                })
            })
            .collect()
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let left = zombies();
        if left.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "zombie children of the daemon linger: {left:?}"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

/// CAD-308 (flips the residual CAD-230 phase b2 pinned): a recipe
/// descendant that detaches AND scrubs the runner's environment —
/// `env -u CADENCE_RUNNER_ID -u CADENCE_RUNNER_DIGEST setsid -f …`,
/// stdio to /dev/null — re-parents to the daemon, its child subreaper,
/// instead of to init. It is still a daemon descendant, so operator
/// proof refuses it by that rule: no `(operator)` launch, no nested
/// runner. Once it exits the daemon reaps it — no zombie lingers. A
/// real `daemon run` process: only that one is the subreaper.
#[test]
fn build_slot_launch_detached_scrubbed_descendant_is_refused() {
    let dir = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let req = work.path().join("req.json");
    let resp = work.path().join("resp.json");
    let pidf = work.path().join("detached.pid");
    std::fs::write(
        &req,
        cadence_agent::proto::request("slot_launch", json!({"recipe": "inner", "project": "p"}))
            .to_string(),
    )
    .unwrap();
    let py = format!(
        "import socket,os;open({pid:?},'w').write(str(os.getpid()));\
         s=socket.socket(socket.AF_UNIX);s.connect({sock:?});\
         s.sendall(open({req:?},'rb').read()+b'\\n');\
         open({out:?}+'.tmp','w').write(s.makefile().readline());\
         os.rename({out:?}+'.tmp',{out:?})",
        pid = pidf.display().to_string(),
        sock = client::socket_path(dir.path()).display().to_string(),
        req = req.display().to_string(),
        out = resp.display().to_string(),
    );
    let outer = format!(
        "env -u CADENCE_RUNNER_ID -u CADENCE_RUNNER_DIGEST setsid -f python3 -c {py} \
         </dev/null >/dev/null 2>&1; \
         while [ ! -e {r} ]; do sleep 0.05; done; exit 0",
        py = shell_quote(&py),
        r = resp.display()
    );
    let recipes = sh_recipe("outer", &outer, &["PATH"]) + &sh_recipe("inner", "echo inner", &[]);
    let (_proj, _repo) = runner_project(&recipes);
    let d = TestDaemon::start_process_in(dir);
    let _reaper = DaemonReaper::new(&d.state);
    let daemon_pid = subreaper_daemon_pid(&d);
    let mut lane = LaneShell::spawn(home.path());
    plant_pane(&d, "dev-1", lane.pid());
    let (rc, out) = lane.cadence(&d.state, "build-slot launch outer --project p");
    assert_eq!(rc, 0, "{out}");
    let answer: Value = serde_json::from_str(&std::fs::read_to_string(&resp).unwrap()).unwrap();
    assert_eq!(answer["ok"], false, "{answer}");
    let msg = answer["error"]["message"].as_str().unwrap();
    assert!(msg.contains("not provably the operator"), "{msg}");
    assert!(
        msg.contains(&format!("descends from the daemon (pid {daemon_pid})")),
        "refused by the daemon-descendant rule: {msg}"
    );
    assert_eq!(
        std::fs::read_dir(cadence_agent::runner::runners_dir(&d.state))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
            .count(),
        1,
        "the detached descendant launched nothing"
    );
    let pid: u32 = std::fs::read_to_string(&pidf)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    wait_reaped(pid, "the detached descendant, adopted by the daemon");
}

/// CAD-308 with the CAD-276 shape: a managed provider's tool runs
/// `env -u CADENCE_ALIAS setsid -f … build-slot reconcile` — its own
/// session leader, the alias scrubbed, stdio redirected. Without the
/// subreaper it re-parented to init and passed operator proof; under
/// `daemon run` it re-parents to the daemon, is refused by the
/// daemon-descendant rule, and is reaped by the daemon once it exits.
///
/// The same detach from a registered pane whose shell the daemon did
/// NOT launch still passes the operator gate — it reaches the
/// live-holder rule. That is the documented pane-route residual
/// (CAD-280), asserted as today's behaviour on purpose.
#[test]
fn slot_reconcile_refuses_a_managed_tools_setsid_detach() {
    let dir = TempDir::new().unwrap();
    // A hermetic tracker dir for the daemon (no host pm.yaml).
    let (_proj, _repo) = runner_project(&sh_recipe("ok", "echo ok", &[]));
    let mock = ManagedWorker::install(dir.path(), dir.path(), "wk");
    let d = TestDaemon::start_process_in(dir);
    let _reaper = DaemonReaper::new(&d.state);
    let daemon_pid = subreaper_daemon_pid(&d);
    let home = TempDir::new().unwrap();
    let mut pane = LaneShell::spawn(home.path());
    plant_pane(&d, "pane-1", pane.pid());
    let mut wk = mock.enroll(&d, "wk");
    let g = wk.rpc(
        "self",
        "slot_acquire",
        json!({"kind": "build", "pid": "$PID", "request_id": "r1"}),
    );
    let token = g["result"]["token"].as_str().unwrap().to_string();
    let s = pane.rpc(&d.state, "slot_status", json!({}));
    let hold = s["result"]["pools"]["build"]["held"][0].clone();
    let root = s["result"]["enrollments"][0]["root"].clone();
    let work = TempDir::new().unwrap();
    let evidence = work.path().join("evidence.json");
    std::fs::write(
        &evidence,
        json!({
            "owner_generation": hold["owner_generation"], "pid": root["pid"],
            "starttime": root["starttime"], "uid": root["uid"],
            "observed_at": "now", "process_read": "claimed exited",
            "command_outcome": "done", "side_effect_review": "none",
        })
        .to_string(),
    )
    .unwrap();
    // `sh detach.sh OUT`: record its pid, run the reconcile, land the
    // CLI's output and exit code at OUT.
    let script = work.path().join("detach.sh");
    std::fs::write(
        &script,
        format!(
            "echo $$ > \"$1.pid\"\n\
             {bin} --state-dir {state} build-slot reconcile {enr} {token} \
             --evidence \"$(cat {ev})\" > \"$1.tmp\" 2>&1\n\
             echo \"rc=$?\" >> \"$1.tmp\"\n\
             mv \"$1.tmp\" \"$1\"\n",
            bin = env!("CARGO_BIN_EXE_cadence"),
            state = d.state.display(),
            enr = hold["enrollment_id"].as_str().unwrap(),
            ev = evidence.display(),
        ),
    )
    .unwrap();
    let detach = |out: &Path| {
        format!(
            "env -u CADENCE_ALIAS setsid -f sh {} {} </dev/null >/dev/null 2>&1",
            script.display(),
            out.display()
        )
    };
    let landed = |out: &Path, route: &str| {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !out.exists() {
            assert!(Instant::now() < deadline, "{route}: never answered");
            thread::sleep(Duration::from_millis(20));
        }
        std::fs::read_to_string(out).unwrap()
    };

    // Off a managed tool: re-parented to the daemon, refused.
    let out = work.path().join("managed.out");
    let r = wk.exec(&["sh", "-c", &detach(&out)]);
    assert_eq!(r["rc"], 0, "{r}");
    let text = landed(&out, "managed detach");
    assert!(text.contains("not provably the operator"), "{text}");
    assert!(
        text.contains(&format!("descends from the daemon (pid {daemon_pid})")),
        "refused by the daemon-descendant rule: {text}"
    );
    let pid: u32 = std::fs::read_to_string(work.path().join("managed.out.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    wait_reaped(pid, "the detached reconcile shell, adopted by the daemon");

    // Off a pane the daemon did not launch: the residual (CAD-280).
    let out = work.path().join("pane.out");
    let (rc, run) = pane.run(&detach(&out));
    assert_eq!(rc, 0, "{run}");
    let text = landed(&out, "pane detach");
    assert!(
        !text.contains("not provably the operator") && text.contains("is alive"),
        "the pane-route residual (CAD-280) passes the operator gate and meets \
         the live-holder rule (a gate refusal here means this host re-parents \
         the test's orphans to a process carrying an agent's env): {text}"
    );

    let s = pane.rpc(&d.state, "slot_status", json!({}));
    assert_eq!(
        s["result"]["pools"]["build"]["held"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "no detached caller freed anything: {s}"
    );
}

/// CAD-308: a managed endpoint under the active reaper starts, runs
/// tool subprocesses and stops as it always did. Its provider is the
/// adapter's own child: the adapter collects it on stop (no `/proc`
/// entry left, no `ECHILD` anywhere), the agent reads `stopped` with no
/// error, no zombie child of the daemon lingers, and nothing the daemon
/// spawned itself is counted as adopted.
#[test]
fn subreaper_managed_endpoint_starts_and_stops_cleanly() {
    let dir = TempDir::new().unwrap();
    // A hermetic tracker dir for the daemon (no host pm.yaml).
    let (_proj, _repo) = runner_project(&sh_recipe("ok", "echo ok", &[]));
    let mock = ManagedWorker::install(dir.path(), dir.path(), "wk");
    let d = TestDaemon::start_process_in(dir);
    let _reaper = DaemonReaper::new(&d.state);
    let daemon_pid = subreaper_daemon_pid(&d);
    let mut wk = mock.enroll(&d, "wk");
    let root = wk.pid;
    for code in [0, 3] {
        let r = wk.exec(&["sh", "-c", &format!("exit {code}")]);
        assert_eq!(r["rc"], code, "{r}");
    }
    let health = d.rpc("health", json!({})).unwrap();
    assert_eq!(health["adopted_live"], 0, "the provider is owned: {health}");
    d.rpc("agent_stop", json!({"alias": "wk"})).unwrap();
    let agent = d.wait_agent("wk", "stopped", 20);
    assert!(agent["error"].is_null(), "{agent}");
    wait_reaped(root, "the managed provider, collected by its adapter");
    wait_no_daemon_zombies(daemon_pid);
    let health = d.rpc("health", json!({})).unwrap();
    assert_eq!(health["adopted_live"], 0, "{health}");
    let events = d
        .events("wk")
        .iter()
        .map(Value::to_string)
        .collect::<String>();
    let log = std::fs::read_to_string(d.dir.path().join("daemon-process.log")).unwrap();
    for text in [&events, &log] {
        assert!(!text.contains("No child processes"), "{text}");
    }
}

/// CAD-308: the reaper never takes an owned child's exit status. Under
/// `daemon run`, recipes that each leave a burst of detached children
/// exiting at once — orphans the daemon adopts and must reap — still
/// end with their own exit code on every receipt: the runner threads'
/// waits and the `git` calls around each gate all got their statuses.
/// Afterwards no zombie child of the daemon lingers, and `daemon.log`
/// says once that the daemon is the subreaper.
#[test]
fn subreaper_reaps_orphans_while_owners_keep_their_exit_statuses() {
    let dir = TempDir::new().unwrap();
    let flood = "i=0; while [ $i -lt 40 ]; do setsid -f sh -c 'exit 0' \
                 </dev/null >/dev/null 2>&1; i=$((i+1)); done; exit 7";
    let (_proj, _repo) = runner_project(&sh_recipe("flood", flood, &["PATH"]));
    let d = TestDaemon::start_process_in(dir);
    let _reaper = DaemonReaper::new(&d.state);
    let daemon_pid = subreaper_daemon_pid(&d);
    let home = TempDir::new().unwrap();
    let mut lane = LaneShell::spawn(home.path());
    plant_pane(&d, "dev-1", lane.pid());
    let launch = format!(
        "{} --state-dir {} build-slot launch flood --project p >/dev/null 2>&1",
        env!("CARGO_BIN_EXE_cadence"),
        d.state.display()
    );
    // Three at once — the default pool's three build slots — twice.
    for _ in 0..2 {
        lane.run(&format!("{launch} & {launch} & {launch} & wait"));
    }
    let receipts: Vec<_> = std::fs::read_dir(cadence_agent::runner::runners_dir(&d.state))
        .unwrap()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let id = name.strip_suffix(".json")?.to_string();
            Some(cadence_agent::runner::read_receipt(&d.state, &id).unwrap())
        })
        .collect();
    assert_eq!(receipts.len(), 6, "{receipts:?}");
    for r in &receipts {
        assert_eq!(
            (r.state.as_str(), r.exit_code),
            ("exited", Some(7)),
            "every owner kept its child's status: {r:?}"
        );
    }
    // Every child of the daemon that is a zombie now must be gone soon:
    // nothing owns the adopted orphans but the reaper.
    wait_no_daemon_zombies(daemon_pid);
    let health = d.rpc("health", json!({})).unwrap();
    assert!(
        health["adopted_reaped_total"].as_u64().unwrap() >= 6 * 40,
        "every flooded orphan was adopted and reaped: {health}"
    );
    let log = std::fs::read_to_string(d.dir.path().join("daemon-process.log")).unwrap();
    assert_eq!(
        log.matches(&format!("subreaper: pid {daemon_pid} "))
            .count(),
        1,
        "{log}"
    );
}

/// CAD-230 phase b2: a launch queues like any other request — behind a
/// held slot it waits, and when its wait runs out the gate never opens:
/// the recipe never starts, the receipt says `timed_out`, and the queue
/// keeps no waiter for it.
#[test]
fn build_slot_launch_times_out_in_the_queue_without_running() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    plant_self(&d);
    let home = TempDir::new().unwrap();
    let (_proj, _repo) = runner_project(&sh_recipe("ok", "echo RAN", &[]));
    let g = slot_acquire(&d, "build", SELF_LANE, "hold");
    assert_eq!(g["granted"], true);
    let run = cadence_at(
        home.path(),
        &d.state,
        &[
            "build-slot",
            "launch",
            "ok",
            "--project",
            "p",
            "--wait-secs",
            "1",
        ],
    );
    let stderr = String::from_utf8_lossy(&run.stderr).to_string();
    assert!(!run.status.success(), "{stderr}");
    assert!(stderr.contains("timed_out"), "{stderr}");
    let id = launched_runner(&stderr);
    let receipt = cadence_agent::runner::read_receipt(&d.state, &id).unwrap();
    assert_eq!(receipt.state, "timed_out");
    assert_eq!((receipt.exit_code, receipt.started), (None, None));
    let log = std::fs::read_to_string(&receipt.log_path).unwrap();
    assert!(!log.contains("RAN"), "the gate never opened: {log}");
    let s = d.rpc("slot_status", json!({})).unwrap();
    assert!(s["waiting"].as_array().unwrap().is_empty(), "{s}");
    assert_eq!(s["pools"]["build"]["held"].as_array().unwrap().len(), 1);
}

/// CAD-230 phase b2: the launch digest names the checkout's HEAD, so a
/// checkout that moves while the runner queues is not the source it
/// bound — the gate never opens and the receipt says why. A dirty tree
/// at launch is recorded, never hidden.
#[test]
fn build_slot_launch_refuses_a_source_that_moved_while_queued() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    plant_self(&d);
    let home = TempDir::new().unwrap();
    let (_proj, repo) = runner_project(&sh_recipe("ok", "echo RAN", &[]));
    std::fs::write(repo.join("README"), "edited, uncommitted\n").unwrap();
    let hold = slot_acquire(&d, "build", SELF_LANE, "hold");
    let run = cadence_at(
        home.path(),
        &d.state,
        &["build-slot", "launch", "ok", "--project", "p", "--detach"],
    );
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let id = String::from_utf8_lossy(&run.stdout).trim().to_string();
    let r = cadence_agent::runner::read_receipt(&d.state, &id).unwrap();
    assert!(r.dirty, "an uncommitted edit is recorded: {r:?}");
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["commit", "-qam", "moved"]);
    slot_release(
        &d,
        hold["token"].as_str().unwrap(),
        SELF_LANE,
        std::process::id(),
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    let r = loop {
        let r = cadence_agent::runner::read_receipt(&d.state, &id).unwrap();
        if r.is_terminal() {
            break r;
        }
        assert!(Instant::now() < deadline, "runner never ended: {r:?}");
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(r.state, "refused", "{r:?}");
    assert!(r.reason.as_deref().unwrap().contains("HEAD moved"), "{r:?}");
    assert_eq!(r.started, None);
    let log = std::fs::read_to_string(&r.log_path).unwrap();
    assert!(!log.contains("RAN"), "the gate never opened: {log}");
    let s = d.rpc("slot_status", json!({})).unwrap();
    assert!(
        s["pools"]["build"]["held"].as_array().unwrap().is_empty(),
        "{s}"
    );
}

/// CAD-230 phase b2 restart semantics: a runner in flight when the
/// daemon restarts is reported `unknown` with its receipt incomplete —
/// never relaunched. Its hold stays accounted (the process lives) under
/// a revoked enrollment, until the process is proven dead.
#[test]
fn build_slot_launch_in_flight_at_restart_is_unknown_never_relaunched() {
    let state = TempDir::new().unwrap();
    let d = TestDaemon::start_on_opts(state.path().to_path_buf(), slot_opts(2, 1, 900, &[]));
    plant_self(&d);
    let home = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let marker = marks.path().join("starts");
    let script = format!("echo start >> {}; sleep 60", marker.display());
    let (_proj, _repo) = runner_project(&sh_recipe("long", &script, &[]));
    let run = cadence_at(
        home.path(),
        &d.state,
        &["build-slot", "launch", "long", "--project", "p", "--detach"],
    );
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let id = String::from_utf8_lossy(&run.stdout).trim().to_string();
    let deadline = Instant::now() + Duration::from_secs(20);
    let pid = loop {
        let r = cadence_agent::runner::read_receipt(&d.state, &id).unwrap();
        if r.state == "running" && marker.exists() {
            break r.pid.unwrap();
        }
        assert!(Instant::now() < deadline, "runner never ran: {r:?}");
        thread::sleep(Duration::from_millis(50));
    };
    drop(d);
    let d2 = TestDaemon::start_on_opts(state.path().to_path_buf(), slot_opts(2, 1, 900, &[]));
    plant_pane(&d2, SELF_LANE, std::process::id());
    let r = cadence_agent::runner::read_receipt(&d2.state, &id).unwrap();
    assert_eq!(r.state, "unknown", "{r:?}");
    assert!(!r.complete);
    assert_eq!(r.last_state.as_deref(), Some("running"));
    assert!(d2
        .events(SELF_LANE)
        .iter()
        .any(|e| e["kind"] == "runner_unknown" && e["payload"]["runner_id"] == id.as_str()));
    let s = d2.rpc("slot_status", json!({})).unwrap();
    let held = s["pools"]["build"]["held"].as_array().unwrap();
    assert_eq!(held.len(), 1, "the live runner stays accounted: {s}");
    assert_eq!(held[0]["pid"], pid);
    assert_eq!(held[0]["liveness"], "alive");
    assert_eq!(held[0]["auth_state"], "revoked");
    thread::sleep(Duration::from_millis(1500));
    let starts = std::fs::read_to_string(&marker).unwrap();
    assert_eq!(starts.lines().count(), 1, "never relaunched: {starts}");
    // Cleanup: end the orphan; proof of its death frees the hold.
    unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let s = d2.rpc("slot_status", json!({})).unwrap();
        if s["pools"]["build"]["held"].as_array().unwrap().is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the dead runner's hold stayed: {s}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// `starve_secs` promotes a long waiter ahead of a priority lane:
/// priority wins inside the window, the starved waiter wins after it.
/// The slot clock is injected — the test advances it instead of
/// sleeping, so timing stays exact under host load. The waiter lanes
/// are real connection identities — one lane shell each (CAD-113).
#[test]
fn slot_starve_promotes_long_waiter() {
    let clock = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let d = TestDaemon::start_opts(slot_opts_clock(1, 1, 3, &["qa-1"], Some(clock.clone())));
    let home = TempDir::new().unwrap();
    plant_self(&d);
    let mut dev = LaneShell::spawn(home.path());
    plant_pane(&d, "dev-2", dev.pid());
    let mut qa = LaneShell::spawn(home.path());
    plant_pane(&d, "qa-1", qa.pid());
    let h1 = slot_acquire(&d, "build", SELF_LANE, "h1")["token"]
        .as_str()
        .unwrap()
        .to_string();
    // Ordinary waiter first, priority waiter second.
    let w = dev.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "build", "pid": dev.pid(), "request_id": "w1"}),
    );
    assert_eq!(w["result"]["granted"], false, "{w}");
    let w = qa.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "test", "pid": qa.pid(), "request_id": "w2"}),
    );
    assert_eq!(w["result"]["granted"], false, "{w}");
    slot_release(&d, &h1, SELF_LANE, std::process::id());
    // Inside the starve window the reviewer lane's test wins.
    let g = qa.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "test", "pid": qa.pid(), "request_id": "w2"}),
    );
    assert_eq!(
        g["result"]["granted"], true,
        "priority lane should outrank: {g}"
    );
    let w2 = g["result"]["token"].as_str().unwrap().to_string();
    // Once w1 has waited past starve_secs it outranks even a new
    // priority request — the never-starve bound.
    clock.store(4, std::sync::atomic::Ordering::Relaxed);
    let w = qa.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "test", "pid": qa.pid(), "request_id": "w3"}),
    );
    assert_eq!(w["result"]["granted"], false, "{w}");
    let r = qa.rpc(
        &d.state,
        "slot_release",
        json!({"token": w2, "pid": qa.pid()}),
    );
    assert_eq!(r["ok"], true, "{r}");
    let g = dev.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "build", "pid": dev.pid(), "request_id": "w1"}),
    );
    assert_eq!(
        g["result"]["granted"], true,
        "starved waiter must outrank priority: {g}"
    );
    let s = d.rpc("slot_status", json!({})).unwrap();
    let w3 = s["waiting"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["request_id"] == "w3")
        .expect("w3 still queued");
    assert_eq!(w3["priority"], true);
}

/// suite draws on its own pool — a full suite queue never jams the
/// build lanes, and `test` shares the build pool. The two pool users
/// claim different pids on this connection's own ancestry (CAD-113):
/// the cross-pool deadlock guard keys on `(lane, pid)`, so the same
/// process must never hold one pool while queueing the other.
#[test]
fn slot_pools_are_independent() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    plant_self(&d);
    let me = std::process::id();
    let parent = std::os::unix::process::parent_id();
    assert_eq!(
        slot_acquire_pid(&d, "suite", SELF_LANE, me, "s1")["granted"],
        true
    );
    assert_eq!(
        slot_acquire_pid(&d, "suite", SELF_LANE, me, "s2")["granted"],
        false,
        "second suite must queue"
    );
    // The suite pool being full does not touch build.
    assert_eq!(
        slot_acquire_pid(&d, "build", SELF_LANE, parent, "b1")["granted"],
        true
    );
    // test shares the build pool — now full too.
    assert_eq!(
        slot_acquire_pid(&d, "test", SELF_LANE, parent, "t1")["granted"],
        false
    );
}

/// The CLI: `--wait-secs 0` fails fast with a named error, a free slot
/// grants a bare token, release returns it, and `status` shows the
/// pool both ways.
#[test]
fn build_slot_cli_acquire_release_status() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    plant_self(&d);
    let home = TempDir::new().unwrap();
    let t1 = slot_acquire(&d, "build", SELF_LANE, "r1")["token"]
        .as_str()
        .unwrap()
        .to_string(); // build pool full
    let me = std::process::id().to_string(); // the CLI child's parent
    let out = cadence_at(
        home.path(),
        &d.state,
        &[
            "build-slot",
            "acquire",
            "build",
            "--wait-secs",
            "0",
            "--pid",
            &me,
        ],
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("No build slot free"), "{err}");
    // --pid is required — a bare acquire refuses rather than binding
    // a transient parent the work outlives.
    let out = cadence_at(
        home.path(),
        &d.state,
        &["build-slot", "acquire", "build", "--wait-secs", "0"],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--pid"));
    // Free it through the CLI — release names the holding lane; the
    // default pid (the CLI's parent = this test) matches the hold.
    let out = cadence_at(
        home.path(),
        &d.state,
        &["build-slot", "release", &t1, "--lane", "dev-1"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("released slot-"));
    let out = cadence_at(
        home.path(),
        &d.state,
        &[
            "build-slot",
            "acquire",
            "build",
            "--wait-secs",
            "0",
            "--lane",
            "dev-9",
            "--pid",
            &me,
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        token.starts_with("slot-"),
        "bare minted token on stdout: {token:?}"
    );
    // The token round-trips: release by exactly what acquire printed,
    // same lane, same default pid.
    let out = cadence_at(
        home.path(),
        &d.state,
        &["build-slot", "release", &token, "--lane", "dev-9"],
    );
    assert!(out.status.success());
    // `--lane` is advisory only (CAD-113): the daemon derives the
    // caller's lane from the connection, so a release naming another
    // lane still acts on — and only on — the caller's own hold.
    let g = slot_acquire(&d, "build", SELF_LANE, "r9");
    let t9 = g["token"].as_str().unwrap().to_string();
    let out = cadence_at(
        home.path(),
        &d.state,
        &["build-slot", "release", &t9, "--lane", "dev-2"],
    );
    assert!(
        out.status.success(),
        "own hold releases whatever --lane claims: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // status --json shows the empty pool; bad kind is a named error.
    let out = cadence_at(home.path(), &d.state, &["build-slot", "status", "--json"]);
    let s: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(s["pools"]["build"]["held"].as_array().unwrap().is_empty());
    let out = cadence_at(
        home.path(),
        &d.state,
        &[
            "build-slot",
            "acquire",
            "bogus",
            "--wait-secs",
            "0",
            "--pid",
            &me,
        ],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("build, test or suite"));
}

/// `build-slot run` binds the hold to the REAL command process: the
/// CLI acquires with its own pid then execs, so the slot's holder IS
/// the running command — its exit frees the slot.
#[test]
fn build_slot_run_binds_the_real_process() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    plant_self(&d);
    let home = TempDir::new().unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "build-slot",
            "run",
            "build",
            "--wait-secs",
            "5",
            "--",
            "sleep",
            "30",
        ])
        .env("HOME", home.path())
        .envs(test_env().vars())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // After exec the spawned pid IS `sleep 30` — the hold must bind
    // to exactly that process, not a wrapper that already exited.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let s = d.rpc("slot_status", json!({"lane": "unknown"})).unwrap();
        let held = s["pools"]["build"]["held"].as_array().unwrap();
        if held
            .iter()
            .any(|h| h["pid"].as_u64() == Some(child.id() as u64))
        {
            break;
        }
        assert!(Instant::now() < deadline, "run never held the slot: {s}");
        thread::sleep(Duration::from_millis(50));
    }
    // The command's exit frees its slot on the next read.
    child.kill().unwrap();
    child.wait().unwrap();
    let s = d.rpc("slot_status", json!({"lane": "unknown"})).unwrap();
    assert!(
        s["pools"]["build"]["held"].as_array().unwrap().is_empty(),
        "the command's exit frees its slot: {s}"
    );
    // A short command exits cleanly through run.
    let out = cadence_at(
        home.path(),
        &d.state,
        &[
            "build-slot",
            "run",
            "build",
            "--wait-secs",
            "5",
            "--",
            "true",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The CLI's queued path: `--wait-secs > 0` polls until a release
/// frees the pool — and `--pid` binds the hold to the named holder
/// (this test process, the CLI's parent).
#[test]
fn build_slot_cli_wait_then_grant() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    plant_self(&d);
    let home = TempDir::new().unwrap();
    let t1 = slot_acquire(&d, "build", SELF_LANE, "r1")["token"]
        .as_str()
        .unwrap()
        .to_string();
    let me = std::process::id().to_string();
    let cli = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "build-slot",
            "acquire",
            "build",
            "--wait-secs",
            "15",
            "--lane",
            "dev-9",
            "--pid",
            &me,
        ])
        .env("HOME", home.path())
        .envs(test_env().vars())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Let it queue — the waiter shows in status, then a release
    // frees the pool and the next poll grants.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let s = d.rpc("slot_status", json!({"lane": "dev-9"})).unwrap();
        if !s["waiting"].as_array().unwrap().is_empty() {
            break;
        }
        assert!(Instant::now() < deadline, "CLI never queued: {s}");
        thread::sleep(Duration::from_millis(50));
    }
    slot_release(&d, &t1, SELF_LANE, std::process::id());
    let out = cli.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(token.starts_with("slot-"), "minted token: {token}");
    // The explicit --pid bound the hold to the named pid — the test
    // process, still alive. The CLI's `--lane dev-9` was advisory:
    // the derived lane is this pane's alias.
    let s = d.rpc("slot_status", json!({})).unwrap();
    let held = s["pools"]["build"]["held"].as_array().unwrap();
    assert_eq!(held[0]["pid"].as_u64().unwrap() as u32, std::process::id());
    assert_eq!(held[0]["lane"], SELF_LANE);
    slot_release(&d, &token, SELF_LANE, std::process::id());
}
