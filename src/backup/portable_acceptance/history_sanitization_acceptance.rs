use super::*;

use crate::backup::portable::inspect_portable;
use rusqlite::OpenFlags;

#[test]
fn cad1579_acc_history_redaction_preserves_source_and_refuses_retained_secrets() {
    let root = root("cad1579-independent-history-export-boundary");
    let tracker = make_tracker(&root);
    let state = make_state(&root, false);
    let source_db = db_file(&state);

    // Construct a synthetic GitHub PAT shape at runtime. This is neither a
    // real credential nor a bare hexadecimal value exempted by the scanner.
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let suffix: String = (0..36)
        .map(|i| alphabet[(i * 17 + 11) % alphabet.len()] as char)
        .collect();
    let credential = format!("{}{}", ["gh", "p_"].concat(), suffix);
    let findings =
        crate::secret::scan(&credential, None).expect("the synthetic credential must be scannable");
    assert!(
        findings.iter().any(|finding| {
            finding.rule == "github-pat" && finding.severity == crate::secret::Severity::Block
        }),
        "the fixture must exercise an actual blocking credential rule"
    );

    let original_messages = vec![
        (
            "msg-0001".to_string(),
            format!(
                "ticket CAD-1163 decision recorded\nhistorical paste: {credential}\nrepeat: {credential}\nkeep café 🧭"
            ),
        ),
        (
            "msg-0002".to_string(),
            format!(
                "handoff for CAD-1163 review\nquoted: {credential}\nreview context retained"
            ),
        ),
        (
            "msg-0003".to_string(),
            "review complete for CAD-1163".to_string(),
        ),
    ];
    let safe_instructions = "keep synthetic operator instructions unchanged";
    {
        let conn = Connection::open(&source_db).unwrap();
        let message_count: i64 = conn
            .query_row("SELECT count(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            message_count, 3,
            "the parent fixture starts with three messages"
        );
        for (id, body) in &original_messages[..2] {
            assert_eq!(
                conn.execute("UPDATE messages SET body=?1 WHERE id=?2", params![body, id],)
                    .unwrap(),
                1
            );
        }
        assert_eq!(
            conn.execute(
                "UPDATE agents SET instructions=?1 WHERE alias='cad-worker'",
                params![safe_instructions],
            )
            .unwrap(),
            1
        );
    }

    let read_messages = |db: &Path| -> Vec<(String, String)> {
        let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut stmt = conn
            .prepare("SELECT id, body FROM messages ORDER BY id")
            .unwrap();
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<(String, String)>>>()
            .unwrap();
        rows
    };
    assert_eq!(read_messages(&source_db), original_messages);
    assert!(
        contains(&fs::read(&source_db).unwrap(), &credential),
        "the source SQLite file really contains the synthetic credential"
    );
    let source_before = tree(&state);
    let tracker_before = tree(&tracker);

    // The same historical messages must refuse without explicit consent.
    let mut options = opts(&tracker);
    options.redact_history_secrets = false;
    let refused_out = root.join("history-without-consent");
    let error = export_portable(&state, &refused_out, &options)
        .expect_err("credential-bearing history must refuse without opt-in");
    assert_eq!(error.code(), Some("secret_detected"));
    assert!(error.to_string().contains("messages.body"));
    assert!(!error.to_string().contains(&credential));
    assert!(!refused_out.exists(), "refusal must not publish an output");
    assert!(
        no_partial(&refused_out),
        "refusal must remove private staging"
    );
    assert_eq!(
        tree(&state),
        source_before,
        "refusal must not edit source state"
    );
    assert_eq!(
        tree(&tracker),
        tracker_before,
        "refusal must not edit the tracker"
    );

    // Opt-in transforms only the exported copy, without deleting messages or
    // counting repeated occurrences as separately changed message bodies.
    options.redact_history_secrets = true;
    let out = root.join("history-with-consent");
    export_portable(&state, &out, &options)
        .expect("opt-in must sanitize the same credential-bearing history");
    assert!(out.is_dir());
    assert!(no_partial(&out));
    assert_eq!(
        tree(&state),
        source_before,
        "successful export must preserve source state"
    );
    assert_eq!(
        tree(&tracker),
        tracker_before,
        "successful export must preserve the tracker"
    );

    let packaged_db = out.join("cadence.sqlite3");
    let expected_messages = vec![
        (
            "msg-0001".to_string(),
            "ticket CAD-1163 decision recorded\nhistorical paste: [redacted:github-pat]\nrepeat: [redacted:github-pat]\nkeep café 🧭".to_string(),
        ),
        (
            "msg-0002".to_string(),
            "handoff for CAD-1163 review\nquoted: [redacted:github-pat]\nreview context retained".to_string(),
        ),
        (
            "msg-0003".to_string(),
            "review complete for CAD-1163".to_string(),
        ),
    ];
    assert_eq!(
        read_messages(&packaged_db),
        expected_messages,
        "all message IDs, nonsecret text and the untouched third body must survive"
    );
    {
        let conn =
            Connection::open_with_flags(&packaged_db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let instructions: String = conn
            .query_row(
                "SELECT instructions FROM agents WHERE alias='cad-worker'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(instructions, safe_instructions);
    }
    let packaged_bytes = fs::read(&packaged_db).unwrap();
    assert!(
        !contains(&packaged_bytes, &credential),
        "the packaged SQLite file must not retain old credential bytes in freed pages"
    );

    let manifest = read_manifest(&out);
    assert_eq!(manifest["redacted_message_bodies"].as_u64(), Some(2));
    let package_before_inspection = tree(&out);
    let inspection = inspect_portable(&out)
        .expect("the sanitized package must pass the real receiver inspection");
    assert_eq!(inspection["status"], "verified_coordination_package");
    assert_eq!(inspection["redacted_message_bodies"].as_u64(), Some(2));
    assert_eq!(
        inspection["manifest_sha256"].as_str(),
        Some(sha_hex(&fs::read(out.join("manifest.json")).unwrap()).as_str())
    );
    assert_eq!(
        tree(&out),
        package_before_inspection,
        "inspection must be read-only"
    );
    assert_eq!(read_messages(&source_db), original_messages);
    assert_eq!(
        tree(&state),
        source_before,
        "inspection must not affect source state"
    );
    assert_eq!(tree(&tracker), tracker_before);

    // Retained configuration is not history. Reuse the same credential and
    // leave the credential-bearing source messages intact: enabling body
    // sanitization must not sanitize, omit or exempt agents.instructions.
    {
        let conn = Connection::open(&source_db).unwrap();
        assert_eq!(
            conn.execute(
                "UPDATE agents SET instructions=?1 WHERE alias='cad-worker'",
                params![format!("retained configuration {credential}")],
            )
            .unwrap(),
            1
        );
    }
    assert_eq!(read_messages(&source_db), original_messages);
    let source_with_retained_secret = tree(&state);
    let retained_out = root.join("retained-field-still-refused");
    let error = export_portable(&state, &retained_out, &options)
        .expect_err("history opt-in must not authorize retained-field secrets");
    assert_eq!(error.code(), Some("secret_detected"));
    assert!(
        error.to_string().contains("agents.instructions"),
        "the retained field must remain subject to the final secret scan"
    );
    assert!(!error.to_string().contains(&credential));
    assert!(
        !retained_out.exists(),
        "retained-field refusal must not publish"
    );
    assert!(no_partial(&retained_out));
    assert_eq!(
        tree(&state),
        source_with_retained_secret,
        "refusal must preserve both the retained field and original message history"
    );
    assert_eq!(tree(&tracker), tracker_before);
    assert_eq!(
        tree(&out),
        package_before_inspection,
        "a later refusal must not alter the already published sanitized package"
    );
}
