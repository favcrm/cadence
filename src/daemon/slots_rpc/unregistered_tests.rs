//! CAD-1021: the queue path for a caller with no registered pane or
//! managed endpoint. One test per enforced rule; each was run with its
//! guard removed (see the PR body) and fails there.

use super::super::*;
use crate::daemon::identity::SlotPeer;
use crate::slots::SlotKind;
use std::process::{Command, Stdio};

/// A same-uid process the test started, killed by pid on drop. `sh`'s
/// `setsid ... &` reparents it: no pane, no daemon, no test ancestry.
struct Peer(u32);

impl Peer {
    /// A `sleep` carrying a FORGED `CADENCE_ALIAS` — the operator proof
    /// refuses it, and a slot identity must not come from it either.
    fn forged() -> Peer {
        let child = Command::new("sleep")
            .arg("120")
            .env("CADENCE_ALIAS", "cc13-pm")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        std::mem::forget(child); // killed (and reaped by init) in drop
        Peer(pid)
    }

    /// The same, detached the way an agent's `setsid` child is: its
    /// parent shell exits and init adopts it.
    fn detached() -> Peer {
        let out = Command::new("sh")
            .args([
                "-c",
                "setsid sleep 120 </dev/null >/dev/null 2>&1 & echo $!",
            ])
            .env("CADENCE_ALIAS", "cc13-pm")
            .output()
            .unwrap();
        let pid: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        // The shell has exited; wait until /proc shows the new parent.
        for _ in 0..100 {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            let ppid = stat
                .rsplit(')')
                .next()
                .and_then(|r| r.split_whitespace().nth(1));
            if ppid != Some(&std::process::id().to_string()) && ppid.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Peer(pid)
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        unsafe { libc::kill(self.0 as i32, libc::SIGKILL) };
    }
}

fn generous() -> (Option<u64>, Option<u64>) {
    (Some(64 << 30), Some(1 << 40))
}

fn daemon() -> (tempfile::TempDir, Arc<Shared>) {
    let root = tempfile::Builder::new()
        .prefix("c1021-")
        .tempdir_in("/tmp")
        .unwrap();
    let shared = Shared::new(root.path(), &ServeOptions::default()).unwrap();
    shared.slots.lock().unwrap().use_resources(Some(generous));
    (root, shared)
}

fn label() -> String {
    format!("{}{}", crate::slots::UNREGISTERED_LANE_PREFIX, unsafe {
        libc::geteuid()
    })
}

fn run_params(kind: &str, req: &str, pid: u32) -> Value {
    // Every identity-shaped field a forger could try rides along.
    json!({"kind": kind, "request_id": req, "pid": pid, "exec": true,
           "probe": false, "lane": "cc13-pm", "alias": "cc13-pm",
           "by": "operator", "operator": true, "actor": "cc13-pm"})
}

fn held(shared: &Shared) -> Vec<(String, u32)> {
    let st = shared
        .rpc_slot_status(&json!({}), std::process::id())
        .unwrap();
    st["pools"]["build"]["held"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (
                h["lane"].as_str().unwrap().to_string(),
                h["pid"].as_u64().unwrap() as u32,
            )
        })
        .collect()
}

#[test]
fn unregistered_caller_gets_a_slot_under_the_daemons_label_only() {
    let (_root, shared) = daemon();
    let peer = Peer::forged();
    let r = shared
        .rpc_slot_acquire(&run_params("build", "r1", peer.0), peer.0)
        .unwrap();
    assert_eq!(r["granted"], true, "{r}");
    // The hold is the daemon's label — never the forged lane/alias.
    assert_eq!(held(&shared), vec![(label(), peer.0)]);
    assert!(!held(&shared).iter().any(|(l, _)| l == "cc13-pm"));
}

#[test]
fn unregistered_caller_is_refused_everything_but_build_and_test_run() {
    let (_root, shared) = daemon();
    let peer = Peer::forged();
    for kind in ["suite", "check"] {
        let e = shared
            .rpc_slot_acquire(&run_params(kind, "k", peer.0), peer.0)
            .unwrap_err();
        assert!(
            e.to_string().contains("only a build or test slot"),
            "{kind}: {e}"
        );
    }
    // A hand-held acquire (no exec) is refused too — it could bind an ancestor.
    let mut hand = run_params("build", "h", peer.0);
    hand["exec"] = json!(false);
    let e = shared.rpc_slot_acquire(&hand, peer.0).unwrap_err();
    assert!(e.to_string().contains("hand-held acquire"), "{e}");
    // A foreign or ancestor pid is never claimable.
    let foreign = run_params("build", "f", std::process::id());
    assert!(shared.rpc_slot_acquire(&foreign, peer.0).is_err());
    // No hold was minted by any refusal.
    assert!(held(&shared).is_empty());
}

#[test]
fn unregistered_caller_cannot_release_and_a_grant_changes_no_identity() {
    let (_root, shared) = daemon();
    let peer = Peer::forged();
    let before = format!("{:?}", shared.connection_caller(peer.0).unwrap());
    let r = shared
        .rpc_slot_acquire(&run_params("build", "r1", peer.0), peer.0)
        .unwrap();
    let token = r["token"].as_str().unwrap();
    // No release verb for a lane the daemon cannot name.
    let e = shared
        .rpc_slot_release(&json!({"token": token, "pid": peer.0}), peer.0)
        .unwrap_err();
    assert!(
        e.to_string()
            .contains("registered pane or managed endpoint"),
        "{e}"
    );
    assert_eq!(
        held(&shared).len(),
        1,
        "the hold survived the refused release"
    );
    // Holding a slot derived no pane identity and proved no operator.
    assert!(shared.slot_identity(peer.0).unwrap().is_none());
    assert_eq!(
        format!("{:?}", shared.connection_caller(peer.0).unwrap()),
        before
    );
    assert!(shared.operator_evidence(peer.0).is_err());
    // The operator-only verb still refuses it.
    let e = shared
        .rpc_slot_reconcile(
            &json!({"enrollment_id": "x", "token": token, "evidence": {}}),
            peer.0,
        )
        .unwrap_err();
    assert!(e.to_string().contains("operator action"), "{e}");
}

#[test]
fn detached_child_gets_the_same_slot_and_the_same_nothing() {
    let (_root, shared) = daemon();
    let peer = Peer::detached();
    let r = shared
        .rpc_slot_acquire(&run_params("test", "d1", peer.0), peer.0)
        .unwrap();
    assert_eq!(r["granted"], true, "{r}");
    assert_eq!(held(&shared), vec![(label(), peer.0)]);
    assert!(shared.slot_identity(peer.0).unwrap().is_none());
    assert!(shared.operator_evidence(peer.0).is_err());
}

#[test]
fn concurrent_unregistered_callers_never_exceed_the_pool() {
    let (_root, shared) = daemon();
    let peers: Vec<Peer> = (0..8).map(|_| Peer::forged()).collect();
    let granted = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for (i, p) in peers.iter().enumerate() {
            let (shared, granted) = (&shared, &granted);
            s.spawn(move || {
                let r = shared
                    .rpc_slot_acquire(&run_params("build", &format!("c{i}"), p.0), p.0)
                    .unwrap();
                if r["granted"] == true {
                    granted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            });
        }
    });
    // The build pool is 3 but CAD-1268's combined default cap is 2:
    // exactly two grants, six queued, no double-grant.
    assert_eq!(granted.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(held(&shared).len(), 2);
    let st = shared.rpc_slot_status(&json!({}), peers[0].0).unwrap();
    assert_eq!(st["total_capacity"], 2);
    assert_eq!(st["config"]["total_slots"], 2);
    assert_eq!(st["waiting"].as_array().unwrap().len(), 6);
    assert!(st["waiting"]
        .as_array()
        .unwrap()
        .iter()
        .all(|w| w["unregistered"] == true && w["wait_reason"] == "capacity"));
}

#[test]
fn an_unreadable_ancestry_still_refuses() {
    let (_root, shared) = daemon();
    // pid 4194304+ cannot exist: no /proc ancestry means NO label.
    let e = shared
        .rpc_slot_acquire(&run_params("build", "x", 4_999_999), 4_999_999)
        .unwrap_err();
    assert!(e.to_string().contains("underivable"), "{e}");
}

fn rid(n: u32) -> String {
    format!("run-{n:032x}")
}

fn intent(kind: SlotKind, env: &[&str]) -> crate::runner::Intent {
    crate::runner::Intent {
        runner_id: "r-test".into(),
        project: "p".into(),
        recipe: "rec".into(),
        kind,
        argv: vec!["true".into()],
        worktree: "/tmp".into(),
        cwd: "/tmp".into(),
        env: env.iter().map(|s| s.to_string()).collect(),
        head_sha: "0".repeat(40),
        dirty: false,
        digest: "d".into(),
    }
}

#[test]
fn unregistered_launch_is_an_allowlist_of_envless_build_and_test_recipes() {
    assert!(Shared::unregistered_launch_allowed(&intent(SlotKind::Build, &[])).is_ok());
    assert!(Shared::unregistered_launch_allowed(&intent(SlotKind::Test, &[])).is_ok());
    for kind in [SlotKind::Suite, SlotKind::Check] {
        assert!(Shared::unregistered_launch_allowed(&intent(kind, &[])).is_err());
    }
    let e =
        Shared::unregistered_launch_allowed(&intent(SlotKind::Build, &["GH_TOKEN"])).unwrap_err();
    assert!(e.to_string().contains("GH_TOKEN"), "{e}");
}

#[test]
fn launch_requester_labels_an_unproven_caller_unregistered_never_operator() {
    let (_root, shared) = daemon();
    let peer = Peer::forged();
    let r = shared.launch_requester(peer.0).unwrap();
    assert_eq!(
        (r.kind.as_str(), r.lane.as_str()),
        ("unregistered", label().as_str())
    );
    assert_ne!(r.lane, "(operator)");
}

#[test]
fn an_unregistered_caller_reads_only_its_own_runner_receipt() {
    let (root, shared) = daemon();
    let peer = Peer::forged();
    let write = |id: &str, kind: &str, lane: &str| {
        let mut i = intent(SlotKind::Build, &[]);
        i.runner_id = id.into();
        let log = crate::runner::log_path(root.path(), id);
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        let rec = crate::runner::Receipt::pending(
            &i,
            crate::runner::Requester {
                kind: kind.into(),
                lane: lane.into(),
            },
            &log,
            1.0,
        );
        crate::runner::write_receipt(root.path(), &rec).unwrap();
    };
    write(&rid(1), "unregistered", &label());
    write(&rid(2), "unregistered", "unregistered:0");
    write(&rid(3), "pane", "dev-1");
    write(&rid(4), "pane", &label());
    let read = |id: &str| shared.rpc_slot_runner(&json!({"runner_id": id}), peer.0);
    assert_eq!(read(&rid(1)).unwrap()["runner_id"], rid(1));
    for id in [rid(2), rid(3), rid(4)] {
        let e = read(&id).unwrap_err();
        assert!(e.to_string().contains("only its own runner"), "{id}: {e}");
    }
}

#[test]
fn peer_label_is_minted_from_the_connection_never_a_field() {
    let (_root, shared) = daemon();
    let peer = Peer::forged();
    match shared.slot_or_unregistered(peer.0).unwrap() {
        SlotPeer::Unregistered { lane, peer: p } => {
            assert_eq!(lane, label());
            assert_eq!(p, peer.0);
        }
        SlotPeer::Known(_) => panic!("a forged env must not derive a pane"),
    }
}

/// The blocker on PR #779: a runner launched by an unregistered caller is
/// enrolled under `unregistered:<uid>`; its process tree must derive NO
/// identity — never `Who::Agent(label)` — so no attributed gate admits it.
#[test]
fn an_unregistered_runner_tree_is_no_agent_to_any_gate() {
    let (_root, shared) = daemon();
    let peer = Peer::forged();
    let clk = crate::slots::SlotClock::at((shared.slot_clock)(), epoch_secs());
    shared
        .slots
        .lock()
        .unwrap()
        .enroll_runner(&label(), &rid(7), "digest", peer.0, clk)
        .unwrap();
    assert!(shared.slot_identity(peer.0).unwrap().is_none());
    assert!(matches!(
        shared.connection_caller(peer.0).unwrap(),
        caller_rule::Who::Unproven(_)
    ));
    for method in ["test_submit", "monitor_register", "agent_ready"] {
        shared
            .caller_gate(method, &json!({}), peer.0)
            .expect_err(method);
    }
    // It still gets what the label gets: a queue position.
    let r = shared.rpc_slot_acquire(&run_params("build", "t1", peer.0), peer.0);
    assert!(r.is_ok(), "{r:?}");
}

#[test]
fn a_registered_style_runner_tree_is_still_that_agent() {
    // The guard is the unregistered label only: a pane-lane runner tree
    // keeps acting as its launcher (existing CAD-230b design).
    let (_root, shared) = daemon();
    let peer = Peer::forged();
    let clk = crate::slots::SlotClock::at((shared.slot_clock)(), epoch_secs());
    shared
        .slots
        .lock()
        .unwrap()
        .enroll_runner("dev-1", &rid(8), "digest", peer.0, clk)
        .unwrap();
    assert!(shared.slot_identity(peer.0).unwrap().is_some());
}
