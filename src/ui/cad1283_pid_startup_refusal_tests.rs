// CAD-1283 acceptance: forged `ui.startup-failed-{pid}` frames are refused.
//
// Independent acceptance author; the CAD-1283 implementer owns
// `ui.rs` / `ui/serve.rs` / `sandbox.rs` / `error.rs` and does not edit
// these bytes. Registration is one frozen patch applied by the parent:
//
//     #[cfg(test)]
//     #[path = "ui/cad1283_pid_startup_refusal_tests.rs"]
//     mod cad1283_pid_startup_refusal_tests;
//
// It pins the agreed consumer contract — the real
// `super::startup_bind_in_use(state_dir, pid, nonce) -> bool` reads the
// per-pid `ui.startup-failed-<pid>` frame (name pinned by the ticket,
// independently of any source helper) in `state_dir` and answers
// `true` only
// for a regular (non-symlink) file holding an exact
// `{"pid":<own child pid>,"nonce":<parent nonce>,"kind":"addr_in_use"}`
// match — and it must fail to compile if the implementation does not
// provide that signature. It proves ONLY this
// function-level refusal; the real consumer must call it just after
// its own `child.try_wait()` returns `Some(..)` and must never delete
// the frame it reads. Separate evidence must still cover:
//   * the producer writes the pid-scoped frame only on a real
//     `Server::http` `AddrInUse` bind failure, via direct exclusive
//     `create_new` + no-follow, mode 0600, never following a symlink
//     and never unlinking a foreign frame;
//   * the consumer retries only a fresh automatic allocation (never an
//     explicit `--port` or persisted `ui.json` port), bounded, failing
//     closed on exhaustion;
//   * end-to-end fallback with a real owned busy listener, and that
//     `ui.ready` success semantics stay unchanged.
// No nonce value is printed or logged by this module.

use serde_json::json;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::Command;

/// The pid-scoped frame path the ticket pins for the typed failure
/// record: `ui.startup-failed-{pid}` in `state_dir`, derived here
/// independently of the implementation so this fixture cannot mirror
/// a wrong helper.
fn frame_path(dir: &tempfile::TempDir, pid: u32) -> PathBuf {
    dir.path().join(format!("ui.startup-failed-{pid}"))
}

fn state_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("cad1283-refusal-")
        .tempdir_in("/tmp")
        .unwrap()
}

/// Spawn a real child of our own and wait it out: the returned pid
/// belongs to a process this test spawned and reaped, matching how the
/// consumer learns `child.id()` before `try_wait` says it exited.
fn exited_own_child() -> std::process::Child {
    let mut child = Command::new("true").spawn().unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(0));
    child
}

fn write_frame(dir: &tempfile::TempDir, pid: u32, frame: &serde_json::Value) {
    std::fs::write(frame_path(dir, pid), frame.to_string()).unwrap();
}

#[test]
fn startup_bind_in_use_refuses_forged_frames_and_accepts_the_bound_one() {
    let dir = state_dir();
    // Same nonce source the real `ui start` uses, minted locally.
    let nonce = crate::operator_auth::random_credential().unwrap();
    let wrong_nonce = crate::operator_auth::random_credential().unwrap();
    assert_ne!(nonce, wrong_nonce);
    let child = exited_own_child();
    let own_pid = child.id();

    let frame =
        |pid: u32, nonce: &str, kind: &str| json!({"pid": pid, "nonce": nonce, "kind": kind});

    // Positive control first: the valid bound frame for our own exited
    // child classifies as a bind collision. If this fails, every
    // refusal below is vacuous, so it is deliberately first.
    write_frame(&dir, own_pid, &frame(own_pid, &nonce, "addr_in_use"));
    assert!(
        super::startup_bind_in_use(dir.path(), own_pid, &nonce),
        "real helper must accept the valid bound startup-failed frame"
    );

    // Wrong nonce (correct pid): a foreign `ui start`'s leftover frame,
    // or a replay under another nonce, is not our collision.
    write_frame(&dir, own_pid, &frame(own_pid, &wrong_nonce, "addr_in_use"));
    assert!(!super::startup_bind_in_use(dir.path(), own_pid, &nonce));

    // Wrong pid (correct nonce): the frame names a stranger's pid while
    // the consumer asks about its OWN child pid — refused. The helper
    // is always called with our own pid here, exactly as the real
    // caller passes `child.id()`; only the frame's pid field is forged,
    // and the forged payload still lands at the own-pid path a real
    // racing producer or attacker would have had to write.
    // A pid adjacent to ours and an impossible one are both covered.
    for forged_pid in [own_pid.saturating_add(1), u32::MAX] {
        assert_ne!(forged_pid, own_pid);
        write_frame(&dir, own_pid, &frame(forged_pid, &nonce, "addr_in_use"));
        assert!(
            !super::startup_bind_in_use(dir.path(), own_pid, &nonce),
            "frame naming foreign pid {forged_pid} must be refused"
        );
    }

    // Unknown kind: only a real `AddrInUse` earns this classification.
    for kind in ["error", "crashed", "ADDR_IN_USE", "addr_in_use ", ""] {
        write_frame(&dir, own_pid, &frame(own_pid, &nonce, kind));
        assert!(
            !super::startup_bind_in_use(dir.path(), own_pid, &nonce),
            "kind {kind:?} must not classify as a bind collision"
        );
    }

    // Malformed records: not JSON, missing bound fields, wrong types.
    for raw in [
        "not json",
        "{\"pid\":",
        "{\"pid\":1}",
        "{\"nonce\":\"x\"}",
        "{\"kind\":\"addr_in_use\"}",
        "{\"pid\":\"1\",\"nonce\":\"x\",\"kind\":\"addr_in_use\"}",
        "[1,2,3]",
        "null",
        "",
    ] {
        std::fs::write(frame_path(&dir, own_pid), raw).unwrap();
        assert!(
            !super::startup_bind_in_use(dir.path(), own_pid, &nonce),
            "malformed frame {raw:?} must be refused"
        );
    }

    // Absent frame: an exit that wrote nothing is an unknown failure.
    let _ = std::fs::remove_file(frame_path(&dir, own_pid));
    assert!(!super::startup_bind_in_use(dir.path(), own_pid, &nonce));

    // Non-regular records carry no authority (design requires a regular
    // file): a symlink pointing at an otherwise valid bound frame, and
    // a directory standing in the frame's place, both refuse — never
    // follow, never classify a collision.
    let real = tempfile::Builder::new()
        .prefix("cad1283-real-")
        .tempdir_in("/tmp")
        .unwrap();
    write_frame(&real, own_pid, &frame(own_pid, &nonce, "addr_in_use"));
    symlink(frame_path(&real, own_pid), frame_path(&dir, own_pid)).unwrap();
    assert!(
        !super::startup_bind_in_use(dir.path(), own_pid, &nonce),
        "a symlinked startup-failed frame must be refused"
    );
    std::fs::remove_file(frame_path(&dir, own_pid)).unwrap();
    std::fs::create_dir(frame_path(&dir, own_pid)).unwrap();
    assert!(
        !super::startup_bind_in_use(dir.path(), own_pid, &nonce),
        "a directory in the frame's place must be refused"
    );
    std::fs::remove_dir(frame_path(&dir, own_pid)).unwrap();

    // Truncated producer output: a partially written frame that never
    // fully completed the direct exclusive write yields no
    // classification.
    let mut f = std::fs::File::create(frame_path(&dir, own_pid)).unwrap();
    f.write_all(b"{\"pid\":").unwrap();
    drop(f);
    assert!(!super::startup_bind_in_use(dir.path(), own_pid, &nonce));
}
