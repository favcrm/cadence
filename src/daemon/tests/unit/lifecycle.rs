use super::*;

#[test]
fn serve_refuses_after_an_interrupted_restore_without_touching_state() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    let aside = state.join("cadence.sqlite3.replaced-20260923T000000Z-deadbeef");
    std::fs::write(&aside, b"previous store").unwrap();
    let marker = state.join(SHUTDOWN_FILE);
    std::fs::write(&marker, b"{}").unwrap();

    // On a thread: a serve that wrongly starts must fail the test,
    // not hang it.
    let owned = state.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(serve_with(&owned, ServeOptions::default()));
    });
    let err = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("serve started over an interrupted restore")
        .unwrap_err()
        .to_string();

    assert!(err.contains("interrupted restore"), "{err}");
    assert!(err.contains(&aside.display().to_string()), "{err}");
    let put_back = format!(
        "mv '{}' '{}'",
        aside.display(),
        state.join("cadence.sqlite3").display()
    );
    assert!(err.contains(&put_back), "{err}");
    assert!(!state.join("cadence.sqlite3").exists());
    assert!(!state.join("cadence.sock").exists());
    assert_eq!(std::fs::read(&marker).unwrap(), b"{}");
    assert_eq!(std::fs::read(&aside).unwrap(), b"previous store");

    // Once it is moved away the check passes.
    std::fs::remove_file(&aside).unwrap();
    crate::backup::refuse_interrupted_restore(state).unwrap();
}

#[test]
fn cad561_a_pending_update_drains_only_while_the_lease_holds_it() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    // Create the store first, so the marker is read on a real dir.
    Shared::new(state, &ServeOptions::default()).unwrap();
    let pending = crate::update::PendingUpdate {
        phase: "draining".into(),
        target: "b".repeat(40),
        from: Some("a".repeat(40)),
        by: "operator:ada".into(),
        since: crate::rollout::unix_now(),
    };
    // A fresh marker no live lease backs — anyone with state-dir
    // access can write this file — is not authority.
    crate::update::write_pending(state, &pending).unwrap();
    let shared = Shared::new(state, &ServeOptions::default()).unwrap();
    assert!(!shared.draining(), "an unbacked marker never drains");
    // The update claims the lease, then drains: a restart mid-update
    // stays drained from boot.
    hold_lease(state, "operator:ada");
    assert!(shared.draining(), "the lease holder's marker drains");
    assert_eq!(
        shared.pending_update().map(|p| p.target),
        Some("b".repeat(40))
    );
    // Another writer's fresh marker is still not authority.
    let mut forged = pending.clone();
    forged.by = "operator:mallory".into();
    crate::update::write_pending(state, &forged).unwrap();
    assert!(!shared.draining(), "only the lease holder's marker drains");
    crate::update::write_pending(state, &pending).unwrap();
    assert!(shared.draining());
    // The update finishing (marker removed) lifts it.
    crate::update::clear_pending(state);
    assert!(shared.pending_update().is_none());
    assert!(!shared.draining());
    // A marker whose heartbeat stopped (an update that died) is
    // ignored: the file's mtime, not `since`, decides.
    crate::update::write_pending(state, &pending).unwrap();
    let old = std::time::SystemTime::now()
        - Duration::from_secs_f64(crate::update::UPDATE_STALE_SECS + 60.0);
    let file = std::fs::File::options()
        .write(true)
        .open(crate::update::update_file(state))
        .unwrap();
    file.set_modified(old).unwrap();
    drop(file);
    assert!(crate::update::pending_update(state).is_none());
    let shared = Shared::new(state, &ServeOptions::default()).unwrap();
    assert!(!shared.draining(), "a stale marker does not drain");
    // A live marker drains again.
    crate::update::write_pending(state, &pending).unwrap();
    assert!(shared.draining());
    assert_eq!(
        shared.pending_update().map(|p| p.phase),
        Some("draining".into())
    );
}

#[test]
fn cad561_update_drain_is_operator_gated_in_the_dispatch() {
    let src = include_str!("../../../daemon.rs");
    let arm = src
        .find("\"update_drain\" => {")
        .expect("the update_drain arm");
    let body = &src[arm..arm + 400];
    assert!(
        body.contains("operator_connection(\"update_drain\""),
        "update_drain must prove the operator connection: {body}"
    );
}

#[test]
fn stopping_reservation_blocks_relaunch() {
    let (dir, shared) = shared();
    register(&shared, dir.path(), "w1");
    shared
        .lifecycle
        .lock()
        .unwrap()
        .stopping
        .insert("w1".to_string());
    let err = shared.launch_actor("w1").unwrap_err();
    assert!(err.to_string().contains("running or stopping"));
    // Reservation dropped (stop finalized): launch succeeds again.
    shared.lifecycle.lock().unwrap().stopping.remove("w1");
    shared.launch_actor("w1").unwrap();
    // Let the spawned actor exit instead of leaking it into other tests.
    shared.store.set_enabled("w1", false).unwrap();
    let lc = shared.lifecycle.lock().unwrap();
    if let Some(ctl) = lc.agents.get("w1") {
        ctl.wake.notify_all();
    }
}

#[test]
fn session_minted_persist_failure_is_evented() {
    let (dir, shared) = shared();
    shared.on_provider_event(
        "ghost",
        "cadence/session_minted",
        json!({"session": "chat-1"}),
    );
    register(&shared, dir.path(), "ghost");
    let kinds: Vec<String> = shared
        .store
        .events("ghost", 0, 50)
        .unwrap()
        .iter()
        .map(|e| e.kind.clone())
        .collect();
    assert!(kinds.iter().any(|k| k == "session_minted"), "{kinds:?}");
    assert!(
        kinds.iter().any(|k| k == "session_persist_failed"),
        "{kinds:?}"
    );
}

#[test]
fn session_resume_failed_never_clears_nondisposable() {
    let (dir, shared) = shared();
    shared
        .store
        .register_agent(&NewAgent {
            alias: "cl1",
            provider: "claude",
            endpoint_kind: "pty",
            role: "worker",
            cwd: dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    shared
        .store
        .set_params("cl1", &json!({"session": "claude-session-1"}))
        .unwrap();
    shared.on_provider_event(
        "cl1",
        "cadence/session_resume_failed",
        json!({"session": "claude-session-1", "reason": "timed out"}),
    );
    let agent = shared.store.agent("cl1").unwrap();
    assert_eq!(
        agent
            .params
            .as_ref()
            .and_then(|p| p.get("session"))
            .and_then(Value::as_str),
        Some("claude-session-1"),
        "non-disposable endpoint must keep its stored session"
    );
    let kinds: Vec<String> = shared
        .store
        .events("cl1", 0, 50)
        .unwrap()
        .iter()
        .map(|e| e.kind.clone())
        .collect();
    assert!(
        kinds.iter().any(|k| k == "session_resume_failed"),
        "{kinds:?}"
    );
}

#[test]
fn overlapping_stop_is_rejected_without_mutation() {
    let (dir, shared) = shared();
    register(&shared, dir.path(), "w1");
    shared
        .lifecycle
        .lock()
        .unwrap()
        .stopping
        .insert("w1".to_string());
    let before = shared.store.agent("w1").unwrap().state;
    let err = shared.rpc_stop(&json!({"alias": "w1"})).unwrap_err();
    assert!(err.to_string().contains("already stopping"));
    // Zero mutations: agent still enabled and in its prior state.
    let agent = shared.store.agent("w1").unwrap();
    assert_eq!(agent.state, before);
    assert!(agent.enabled);
    // Once the owning stop finalizes and releases, a sequential
    // stop proceeds — repeat stop stays idempotent.
    shared.lifecycle.lock().unwrap().stopping.remove("w1");
    let stopped = shared.rpc_stop(&json!({"alias": "w1"})).unwrap();
    assert_eq!(stopped["state"], "stopped");
    let again = shared.rpc_stop(&json!({"alias": "w1"})).unwrap();
    assert_eq!(again["state"], "stopped");
}
