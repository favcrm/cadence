
    #[test]
    fn approval_evidence_is_idempotent_and_separate_from_delivery() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        reg(&s, "a1", &cwd);
        let via = "operator-connection";
        let op = "chris via chat";
        assert!(
            s.record_approval(&approval("ap-1", op, SHA40_A, 84), via)
                .unwrap()
                .0
        );
        // Identical retry dedupes; the same id naming another head,
        // source or PR is refused.
        assert!(
            !s.record_approval(&approval("ap-1", op, SHA40_A, 84), via)
                .unwrap()
                .0
        );
        for conflicting in [
            approval("ap-1", op, SHA40_B, 84),
            approval("ap-1", "someone else", SHA40_A, 84),
            approval("ap-1", op, SHA40_A, 85),
        ] {
            let err = s.record_approval(&conflicting, via).unwrap_err();
            assert!(err.to_string().contains("different evidence"), "{err}");
        }
        assert!(
            s.record_approval(&approval("ap-2", op, SHA40_B, 70), via)
                .unwrap()
                .0
        );
        assert!(s.revoke_approval("ap-1", op, "head changed", via).unwrap());
        assert!(!s.revoke_approval("ap-1", op, "head changed", via).unwrap());
        assert!(s.revoke_approval("ap-1", op, "other reason", via).is_err());
        // A revoked id is never reused — not even for identical evidence.
        let err = s
            .record_approval(&approval("ap-1", op, SHA40_A, 84), via)
            .unwrap_err();
        assert!(
            err.to_string().contains("was revoked") && err.to_string().contains("--id"),
            "{err}"
        );
        assert!(s.revoke_approval("missing", op, "no record", via).is_err());
        // `user`/`daemon` are delivery/stream identities, not operators;
        // abbreviated heads, bad scopes and bad ids are refused.
        assert!(s
            .record_approval(&approval("ap-3", "user", SHA40_A, 1), via)
            .is_err());
        assert!(s
            .record_approval(&approval("ap-3", "Daemon", SHA40_A, 1), via)
            .is_err());
        assert!(s
            .record_approval(&approval("ap-3", op, "aaaaaaa", 1), via)
            .is_err());
        assert!(s
            .record_approval(&approval("ap-3", op, SHA40_A, 0), via)
            .is_err());
        assert!(s
            .record_approval(&approval("AP 3", op, SHA40_A, 1), via)
            .is_err());
        let mut bad_repo = approval("ap-3", op, SHA40_A, 1);
        bad_repo.repo = "no-slash";
        assert!(s.record_approval(&bad_repo, via).is_err());

        // A cancelled delivery carrying an approval phrase writes nothing
        // to the approval stream, and the daemon stream's retention prune
        // does not reach it.
        s.enqueue(
            "a1",
            &format!("OPERATOR APPROVED #84 at {SHA40_A}"),
            None,
            "m-cancel",
            "user",
        )
        .unwrap();
        s.cancel("m-cancel", "operator", Some("delivery withdrawn"))
            .unwrap();
        s.prune_stream(Store::DAEMON_STREAM, 0).unwrap();
        assert_eq!(
            approval_rows(&s),
            vec![
                (APPROVAL_RECORDED_EVENT.to_string(), "ap-1".to_string()),
                (APPROVAL_RECORDED_EVENT.to_string(), "ap-2".to_string()),
                (APPROVAL_REVOKED_EVENT.to_string(), "ap-1".to_string()),
            ]
        );
        // The stream name can never be an agent alias, so `agent rm`'s
        // per-alias event delete cannot reach it either.
        assert!(identifier(APPROVAL_STREAM, "alias").is_err());
    }

    /// CAD-217 review: re-approving after a revoke with the default id
    /// records a FRESH approval (`<base>-2`), never a silent duplicate of
    /// the revoked one; identical re-sends of the live one still dedupe.
    #[test]
    fn approval_default_id_counts_past_a_revoke() {
        let (_dir, s) = store();
        let via = "operator-connection";
        let mut a = approval("unused", "op", SHA40_A, 84);
        a.id = None;
        let base = default_approval_id("merge", 84, SHA40_A);
        assert_eq!(base, "merge-pr84-aaaaaaaaaaaa");
        assert_eq!(s.record_approval(&a, via).unwrap(), (true, base.clone()));
        assert_eq!(s.record_approval(&a, via).unwrap(), (false, base.clone()));
        assert!(s.revoke_approval(&base, "op", "head moved", via).unwrap());
        let fresh = format!("{base}-2");
        assert_eq!(s.record_approval(&a, via).unwrap(), (true, fresh.clone()));
        assert_eq!(s.record_approval(&a, via).unwrap(), (false, fresh.clone()));
        // A different source for the same head takes the next free id
        // instead of colliding with the default.
        let mut b = approval("unused", "someone else", SHA40_A, 84);
        b.id = None;
        assert_eq!(
            s.record_approval(&b, via).unwrap(),
            (true, format!("{base}-3"))
        );
        assert_eq!(
            approval_rows(&s),
            vec![
                (APPROVAL_RECORDED_EVENT.to_string(), base.clone()),
                (APPROVAL_REVOKED_EVENT.to_string(), base),
                (APPROVAL_RECORDED_EVENT.to_string(), fresh),
                (
                    APPROVAL_RECORDED_EVENT.to_string(),
                    "merge-pr84-aaaaaaaaaaaa-3".to_string()
                ),
            ]
        );
    }
