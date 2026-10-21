use super::*;

/// CAD-162 acceptance 2/3: on every endpoint kind with a scheme, the
/// token minted under the live generation is current; the same
/// shape from an EARLIER generation, another endpoint kind's token
/// carrying the LIVE generation, and any token while the generation
/// is unproven (cleared at store open) are each refused and change
/// nothing.

#[test]
fn message_report_judges_tokens_by_the_endpoints_own_scheme() {
    let (dir, shared) = shared();
    for (i, (pair, own, foreign)) in schemed_endpoints().into_iter().enumerate() {
        let what = format!("{}/{}", pair.0, pair.1);
        // Current: accepted, and the ack keeps the message running.
        let token = format!("{own}-{GEN}-nonce{i}");
        let id = running_turn(
            &shared,
            dir.path(),
            &format!("cur{i}"),
            pair,
            Some(GEN),
            &token,
        );
        report(&shared, &id, &token, "ack")
            .unwrap_or_else(|e| panic!("{what}: current token refused: {e}"));
        let m = shared.store.message(&id).unwrap().unwrap();
        assert_eq!(m.state, "running", "{what}");
        assert_eq!(
            m.result.as_ref().unwrap()["ack"]["text"],
            "reported",
            "{what}"
        );

        // Earlier generation, same scheme.
        let stale = format!("{own}-{OLD_GEN}-nonce{i}");
        let id = running_turn(
            &shared,
            dir.path(),
            &format!("old{i}"),
            pair,
            Some(GEN),
            &stale,
        );
        assert_refused_untouched(&shared, &id, &stale, &format!("{what} stale"));

        // Another endpoint kind's token under the LIVE generation.
        let cross = format!("{foreign}-{GEN}-nonce{i}");
        let id = running_turn(
            &shared,
            dir.path(),
            &format!("xk{i}"),
            pair,
            Some(GEN),
            &cross,
        );
        assert_refused_untouched(&shared, &id, &cross, &format!("{what} cross-kind"));

        // Generation not proven: nothing is current.
        let id = running_turn(&shared, dir.path(), &format!("ng{i}"), pair, None, &token);
        assert_refused_untouched(&shared, &id, &token, &format!("{what} no generation"));
    }
}

/// CAD-162 acceptance 2/3, fail closed: an endpoint whose token
/// carries nothing cadence can check against its generation (codex:
/// provider turn ids; devin cloud: the message id; fake; mailbox)
/// accepts NO report — not its own token, not a pty- or claude-
/// shaped token forged under its live generation.

#[test]
fn message_report_refuses_every_token_on_endpoints_without_a_scheme() {
    let (dir, shared) = shared();
    let pairs = [
        ("codex", "managed"),
        ("codex", "managed-ws"),
        ("devin", "cloud"),
        ("fake", "fake"),
        ("inbox", "inbox"),
    ];
    for (i, pair) in pairs.into_iter().enumerate() {
        let alias = format!("ns{i}");
        let id = format!("m-{alias}");
        let tokens = [
            // What each adapter mints today: codex a provider turn
            // id, devin cloud the message id, fake a counter.
            "turn-0001".to_string(),
            id.clone(),
            "fake-turn-1".to_string(),
            // Forgeries under the live generation.
            format!("pty-{GEN}-n"),
            format!("claude-{GEN}-n"),
            format!("{}-{GEN}-n", pair.0),
            format!("{}-{GEN}-n", pair.1),
        ];
        let first = running_turn(&shared, dir.path(), &alias, pair, Some(GEN), &tokens[0]);
        assert_eq!(first, id);
        for token in &tokens {
            shared.store.mark_running(&id, token).unwrap();
            assert_refused_untouched(
                &shared,
                &id,
                token,
                &format!("{}/{} {token}", pair.0, pair.1),
            );
        }
    }
}

/// CAD-162 acceptance 4: an ack on a managed endpoint keeps the
/// message running without the pty `submitted` marker (so it is
/// never `awaiting_report`), a reported `result` is refused — the
/// adapter's turn result is the one writer of the outcome — and that
/// later turn result completes the message.

#[test]
fn managed_ack_keeps_running_and_the_turn_result_completes() {
    let (dir, shared) = shared();
    let token = format!("claude-{GEN}-n");
    let id = running_turn(
        &shared,
        dir.path(),
        "mc1",
        ("claude", "managed"),
        Some(GEN),
        &token,
    );
    report(&shared, &id, &token, "ack").unwrap();
    let m = shared.store.message(&id).unwrap().unwrap();
    assert_eq!(m.state, "running");
    let result = m.result.clone().unwrap();
    assert_eq!(result["status"], "acknowledged", "{result}");
    assert_eq!(result["ack"]["text"], "reported", "{result}");
    assert!(!m.awaiting_report(), "a managed ack owes no report");
    assert!(m.holds_turn());

    let err = report(&shared, &id, &token, "result")
        .unwrap_err()
        .to_string();
    assert!(err.contains("report `ack` only"), "{err}");
    let m = shared.store.message(&id).unwrap().unwrap();
    assert_eq!(m.state, "running");
    assert_eq!(m.result.as_ref().unwrap()["status"], "acknowledged");

    shared
        .complete(
            &m,
            TurnResult {
                turn_id: token.clone(),
                status: "completed".into(),
                text: "done".into(),
                stop_reason: Some("end_turn".into()),
                error: None,
            },
        )
        .unwrap();
    let m = shared.store.message(&id).unwrap().unwrap();
    assert_eq!(m.state, "completed");
    assert_eq!(m.result.as_ref().unwrap()["text"], "done");
    // An ack after completion is refused like on pty.
    assert!(report(&shared, &id, &token, "ack").is_err());
}
