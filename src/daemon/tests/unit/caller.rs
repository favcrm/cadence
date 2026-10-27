use super::*;

/// CAD-385 acceptance 2: a pane row whose pid is now held by an
/// unrelated process — a real one, recorded with an EARLIER start
/// time than the process holding the pid now — maps the caller to
/// no alias in any derivation, answering exactly as when the row is
/// not registered at all. The same row with the matching start
/// time does name the caller's pane (the check is not blanket).

#[test]
fn cad385_reused_pane_pid_maps_no_alias_like_an_unregistered_process() {
    let tree = PaneTree::spawn();
    let start = crate::peer::proc_starttime(tree.pane_pid()).unwrap();

    let (bare_dir, bare) = shared();
    register(&bare, bare_dir.path(), "elsewhere");
    let unregistered = identity_answers(&bare, tree.caller);
    assert!(unregistered[0].contains("Ok(None)"), "{unregistered:?}");

    let (dir, stale) = shared();
    register(&stale, dir.path(), "elsewhere");
    record_pane(&stale, dir.path(), "pm", tree.pane_pid(), Some(start - 1));
    assert_eq!(identity_answers(&stale, tree.caller), unregistered);

    let (live_dir, live) = shared();
    register(&live, live_dir.path(), "elsewhere");
    record_pane(&live, live_dir.path(), "pm", tree.pane_pid(), Some(start));
    assert!(matches!(
        live.slot_identity(tree.caller).unwrap(),
        Some(SlotWho::Pane { ref lane, .. }) if lane == "pm"
    ));
    assert_eq!(
        live.derived_caller("elsewhere", tree.caller, "answer")
            .unwrap(),
        ("pm".to_string(), "agent")
    );
}

/// CAD-385 acceptance 3: a pane row with a pid but no recorded start
/// time (written before schema v14) fails closed — no derivation
/// names its alias or lets the caller pass as unregistered: slot and
/// caller identity refuse naming the remedy, and operator proof
/// still counts the row as a pane.

#[test]
fn cad385_pane_row_without_a_start_time_fails_closed_naming_the_remedy() {
    let tree = PaneTree::spawn();
    let (dir, shared) = shared();
    register(&shared, dir.path(), "elsewhere");
    record_pane(&shared, dir.path(), "pm", tree.pane_pid(), None);

    let slot = shared.slot_identity(tree.caller).err().unwrap().to_string();
    let caller = match shared.caller_identity(tree.caller) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("a legacy row must not resolve a caller"),
    };
    let answer = shared
        .derived_caller("elsewhere", tree.caller, "answer")
        .unwrap_err()
        .to_string();
    for refusal in [&slot, &caller, &answer] {
        assert!(refusal.contains("'pm'"), "{refusal}");
        assert!(refusal.contains("process start time"), "{refusal}");
        assert!(refusal.contains("cadence daemon restart"), "{refusal}");
        assert!(refusal.contains("doctor --host"), "{refusal}");
    }
    let proof = shared.operator_evidence(tree.caller).unwrap_err();
    assert!(proof.contains("registered pane 'pm'"), "{proof}");
}

/// The unmatched-caller tail: a broken walk refuses only while the
/// target lives; a clean walk stamps `operator` solely on positive
/// terminal evidence — a detached caller is `unknown`, never
/// `operator`.

#[test]
fn unmatched_caller_is_fail_closed() {
    // Broken walk: refused while the target pane lives.
    assert!(unmatched_caller(false, true, false, "answer").is_err());
    assert!(unmatched_caller(false, true, true, "answer").is_err());
    // …and `unknown` once it is gone.
    assert_eq!(
        unmatched_caller(false, false, false, "answer").unwrap(),
        ("unknown".to_string(), "unknown")
    );
    // Clean walk + foreign pty → operator.
    assert_eq!(
        unmatched_caller(true, true, true, "answer").unwrap(),
        ("operator".to_string(), "operator")
    );
    // Clean walk + no terminal (a full setsid detach) → unknown.
    assert_eq!(
        unmatched_caller(true, true, false, "answer").unwrap(),
        ("unknown".to_string(), "unknown")
    );
    assert_eq!(
        unmatched_caller(true, false, false, "answer").unwrap(),
        ("unknown".to_string(), "unknown")
    );
}
