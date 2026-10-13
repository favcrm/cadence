// Appended to the existing native CAD-1337 acceptance namespace. These
// independent checks are ticket/review-derived and leave its existing
// fixtures and assertions unchanged.

const REPAIR_STALE_BYTES: &[u8] = b"must not be retained after realm change\n";

#[test]
fn cad1337_public_attachment_refuses_stale_realm_on_native_upload_and_read() {
    let fx = Fx::new();
    let owner = fx.public_session("usr-cad1337-stale-realm", "owner");
    let account = owner["session"]["account_id"].as_str().unwrap();
    let proof = json!({"token":owner["token"],"key":"","origin":"public"});

    // Positive controls use the real assertion verifier, account binding,
    // upload path, and session-scoped read before the current realm changes.
    let uploaded = fx.upload(&owner, SECRET_BYTES, json!({})).unwrap();
    let id = uploaded["id"].as_str().unwrap();
    let original = fx
        ._dir
        .path()
        .join("workspace/users")
        .join(account)
        .join("threads")
        .join(&fx.master_home)
        .join("uploads")
        .join(id)
        .join("ticket.txt");
    assert_eq!(std::fs::read(&original).unwrap(), SECRET_BYTES);
    let read = fx
        .call(
            Asserted::Operator,
            "chat_file_read",
            json!({"id":id,"session":proof}),
        )
        .unwrap();
    assert_eq!(
        read["text"],
        json!(String::from_utf8_lossy(SECRET_BYTES).as_ref())
    );

    crate::board_identity::write_config(
        fx._dir.path(),
        &crate::board_identity::Config {
            host: "cad1337-changed.board.localhost".into(),
            issuer: fx.issuer.origin.clone(),
            company: COMPANY.into(),
        },
    )
    .unwrap();
    let before = fx.row_count();

    let read_error = fx
        .call(
            Asserted::Operator,
            "chat_file_read",
            json!({
                "id":id,
                "session":{"token":owner["token"],"key":"","origin":"public"},
            }),
        )
        .expect_err("native read accepted a public session from a stale realm")
        .to_string();
    assert!(
        !read_error.contains(String::from_utf8_lossy(SECRET_BYTES).as_ref()),
        "stale native read refusal disclosed retained bytes"
    );

    let upload_error = fx
        .upload(&owner, REPAIR_STALE_BYTES, json!({}))
        .expect_err("native upload accepted a public session from a stale realm")
        .to_string();
    assert!(
        !upload_error.contains("not provably the operator"),
        "stale-proof case failed at caller attribution instead of the attachment authority guard: {upload_error}"
    );
    assert_eq!(
        fx.row_count(),
        before,
        "stale native proof changed attachment rows"
    );
    assert_eq!(std::fs::read(&original).unwrap(), SECRET_BYTES);
    assert_eq!(
        std::fs::read_dir(original.parent().unwrap().parent().unwrap())
            .unwrap()
            .count(),
        1,
        "stale upload published another original"
    );
}

fn cad1337_repair_audit_rows(fx: &Fx) -> Vec<(String, Value)> {
    let db = rusqlite::Connection::open(crate::rollout::db_file(fx._dir.path())).unwrap();
    let mut statement = db
        .prepare("SELECT kind,payload FROM events ORDER BY seq")
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|row| {
            let (kind, payload) = row.unwrap();
            (kind, serde_json::from_str(&payload).unwrap())
        })
        .collect()
}

fn cad1337_repair_has_subject(payload: &Value, subject: &str) -> bool {
    payload["targets"]
        .as_array()
        .is_some_and(|targets| targets.iter().any(|target| target["sub"] == subject))
}

fn cad1337_repair_has_session(payload: &Value, session: &str) -> bool {
    payload["targets"]
        .as_array()
        .is_some_and(|targets| targets.iter().any(|target| target["session"] == session))
}

fn cad1337_repair_is_compensation(payload: &Value) -> bool {
    ["action", "reason"].iter().any(|field| {
        payload[*field]
            .as_str()
            .is_some_and(|value| value.to_ascii_lowercase().contains("compensat"))
    })
}

#[test]
fn cad1337_post_open_account_failure_is_uncertain_and_compensated() {
    let fx = Fx::new();
    let subject = "usr-cad1337-account-failure";
    let assertion = fx.issuer.assertion(subject, "owner");
    let db = rusqlite::Connection::open(crate::rollout::db_file(fx._dir.path())).unwrap();
    db.execute_batch(
        "CREATE TRIGGER cad1337_repair_account_insert_failure
         BEFORE INSERT ON chat_upload_accounts
         BEGIN SELECT RAISE(ABORT, 'cad1337 fixture account registration failure'); END;",
    )
    .unwrap();
    drop(db);

    let failure = fx
        .call(
            Asserted::Operator,
            "board_session_open",
            json!({"assertion":assertion.clone()}),
        )
        .expect_err("post-open account-registration failure issued a session")
        .to_string();
    assert!(
        !failure.contains(&assertion),
        "sign-in refusal leaked the verified assertion"
    );

    // The prepared open spent its one-use assertion even though registration
    // failed. Replaying it must refuse rather than minting or retrying.
    let replay = fx
        .call(
            Asserted::Operator,
            "board_session_open",
            json!({"assertion":assertion}),
        )
        .expect_err("failed open did not spend its one-use assertion")
        .to_string();
    assert!(
        replay.contains("already exchanged") || replay.contains("replayed"),
        "unexpected replay refusal: {replay}"
    );

    let sessions: Value = serde_json::from_slice(
        &std::fs::read(fx._dir.path().join("operator/sessions.json")).unwrap(),
    )
    .unwrap();
    assert!(
        !sessions["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| { row["user"]["sub"].as_str() == Some(subject) }),
        "failed open left its compensated public session persisted"
    );
    let db = rusqlite::Connection::open(crate::rollout::db_file(fx._dir.path())).unwrap();
    let accounts: i64 = db
        .query_row("SELECT count(*) FROM chat_upload_accounts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        accounts, 0,
        "failed account registration persisted a binding"
    );
    drop(db);

    let records = cad1337_repair_audit_rows(&fx);
    let prepared = records
        .iter()
        .find(|(kind, payload)| {
            kind == "board_session_mutation_prepared"
                && payload["action"] == "board_session_opened"
                && payload["outcome"] == "prepared"
                && cad1337_repair_has_subject(payload, subject)
        })
        .map(|(_, payload)| payload)
        .expect("post-open failure must retain the prepared session audit");
    let session = prepared["targets"][0]["session"]
        .as_str()
        .expect("audit references the noncredential session id");
    let open_outcomes = records
        .iter()
        .filter(|(kind, payload)| {
            kind == "board_session_mutation_finished"
                && payload["action"] == "board_session_opened"
                && cad1337_repair_has_session(payload, session)
        })
        .map(|(_, payload)| payload)
        .collect::<Vec<_>>();
    assert_eq!(
        open_outcomes.len(),
        1,
        "prepared open needs one honest outcome"
    );
    assert_eq!(
        open_outcomes[0]["outcome"], "uncertain",
        "post-open account failure is not a clean no-op"
    );

    assert!(
        records.iter().any(|(kind, payload)| {
            kind == "board_session_mutation_prepared"
                && payload["outcome"] == "prepared"
                && cad1337_repair_has_session(payload, session)
                && cad1337_repair_is_compensation(payload)
        }),
        "session cleanup must have a prepared server-compensation audit"
    );
    let completed = records
        .iter()
        .filter(|(kind, payload)| {
            kind == "board_session_mutation_finished"
                && payload["outcome"] == "completed"
                && cad1337_repair_has_session(payload, session)
                && cad1337_repair_is_compensation(payload)
        })
        .map(|(_, payload)| payload)
        .collect::<Vec<_>>();
    assert_eq!(
        completed.len(),
        1,
        "successful compensation needs one completed outcome"
    );
    let compensation = completed[0];
    assert!(
        matches!(
            compensation["actor_kind"].as_str(),
            Some("system" | "server" | "daemon")
        ) && compensation["human"].is_null()
            && compensation["agent"].is_null(),
        "compensation audit must identify server action, not human/operator/agent: {compensation}"
    );
    assert_ne!(compensation["action"], "board_session_logged_out");
    assert_ne!(compensation["reason"], "session_logout");
    assert_ne!(compensation["reason"], "presented_by_agent");
    for (_, payload) in records.iter().filter(|(_, payload)| {
        cad1337_repair_has_session(payload, session)
            && (payload["action"] == "board_session_opened"
                || cad1337_repair_is_compensation(payload))
    }) {
        let serialized = payload.to_string();
        assert!(!serialized.contains(&assertion));
        for credential_field in [
            "\"token\"",
            "\"key\"",
            "\"jti\"",
            "\"assertion\"",
            "\"user_agent\"",
        ] {
            assert!(
                !serialized.contains(credential_field),
                "audit retained credential field {credential_field}: {payload}"
            );
        }
    }
    assert!(
        !records
            .iter()
            .any(|(kind, payload)| { kind == "board_session_opened" && payload["sub"] == subject }),
        "failed open must not emit the success notice"
    );
}

const CAD1337_ACCOUNT_SETUP_COMPENSATION_REASON: &str = "upload_account_setup_compensation";

fn cad1337_system_compensation_receipt() -> Value {
    let request = uuid::Uuid::new_v4().to_string();
    json!({
        "schema_version":1,
        "at":crate::issue::time::now_epoch() as f64,
        "company":COMPANY,
        "actor_kind":"system",
        "human":null,
        "agent":null,
        "action":"board_session_revoked",
        "resource":"session:1337cafe",
        "reason":CAD1337_ACCOUNT_SETUP_COMPENSATION_REASON,
        "outcome":"completed",
        "request_id":request.clone(),
        "correlation_id":request,
        "causation_id":null,
        "revision_or_digest":null,
        "revision_availability":"session_state_has_no_content_revision",
        "targets":[{
            "session":"1337cafe",
            "origin":"public",
            "sub":"usr-cad1337-account-setup",
            "role":"operator",
            "role_basis":"signed_session_snapshot",
            "company":COMPANY,
        }],
    })
}

fn cad1337_activity_page(
    store: &crate::store::Store,
    dir: &std::path::Path,
) -> crate::Result<Value> {
    store.activity_query(
        dir,
        &crate::store::activity::ActivityQuery {
            company: COMPANY.into(),
            ..Default::default()
        },
    )
}

fn cad1337_activity_refuses(payload: Value, label: &str) {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(&dir.path().join("receipt.sqlite3")).unwrap();
    store
        .event_public(
            crate::store::Store::DAEMON_STREAM,
            "board_session_mutation_finished",
            payload,
        )
        .unwrap();
    let error = cad1337_activity_page(&store, dir.path())
        .expect_err(&format!(
            "activity guard accepted forbidden {label} receipt"
        ))
        .to_string();
    assert!(
        error.contains("activity session receipt is malformed"),
        "{label} failed outside the real session-receipt guard: {error}"
    );
}

#[test]
fn cad1337_activity_guard_allows_only_system_account_setup_compensation() {
    let valid = cad1337_system_compensation_receipt();
    let dir = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(&dir.path().join("receipt.sqlite3")).unwrap();
    store
        .event_public(
            crate::store::Store::DAEMON_STREAM,
            "board_session_mutation_finished",
            valid.clone(),
        )
        .unwrap();
    let page = cad1337_activity_page(&store, dir.path()).unwrap();
    let projected = page["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["target"] == valid["resource"])
        .expect("actual activity guard must project the narrow system compensation");
    assert_eq!(projected["action"], "board_session_revoked");
    assert_eq!(projected["outcome"], "completed");
    assert!(
        projected["human"].is_null(),
        "system compensation is not a human actor"
    );
    assert_eq!(projected["receipt"]["actor_kind"], "system");
    assert_eq!(
        projected["receipt"]["reason"],
        CAD1337_ACCOUNT_SETUP_COMPENSATION_REASON
    );
    assert!(projected["receipt"]["agent"].is_null());
    assert_eq!(
        projected["receipt"]["targets"][0]["sub"], "usr-cad1337-account-setup",
        "the signed-session target remains metadata, not the acting human"
    );

    // A system actor is not valid for another board-session mutation, even
    // when that action/reason pair is otherwise on the existing allowlist.
    let mut other_action = valid.clone();
    other_action["action"] = json!("board_session_opened");
    other_action["reason"] = json!("platform_sign_in");
    cad1337_activity_refuses(other_action, "system actor on sign-in");
    let mut other_reason = valid.clone();
    other_reason["reason"] = json!("operator_revoke");
    cad1337_activity_refuses(other_reason, "system actor on operator-revoke reason");

    // Existing actor classes cannot borrow the new system-only compensation
    // reason. In particular, the ordinary operator-revoke branch must not
    // silently accept it.
    let mut operator = valid.clone();
    operator["actor_kind"] = json!("operator");
    cad1337_activity_refuses(operator, "operator actor on compensation reason");
    let mut human = valid.clone();
    human["actor_kind"] = json!("human");
    human["human"] = json!({
        "sub":"usr-cad1337-account-setup",
        "company":COMPANY,
        "session":"1337cafe",
        "role":"operator",
        "role_basis":"signed_session_snapshot",
    });
    cad1337_activity_refuses(human, "human actor on compensation reason");
    let mut agent = valid.clone();
    agent["actor_kind"] = json!("agent");
    agent["agent"] = json!("worker");
    cad1337_activity_refuses(agent, "agent actor on compensation reason");
    let mut unknown = valid;
    unknown["actor_kind"] = json!("unknown");
    cad1337_activity_refuses(unknown, "unknown actor on compensation reason");
}

#[test]
fn cad1337_compensation_audit_prepare_failure_remains_uncertain_and_safe() {
    let fx = Fx::new();
    let subject = "usr-cad1337-compensation-audit-failure";
    let assertion = fx.issuer.assertion(subject, "owner");
    let db = rusqlite::Connection::open(crate::rollout::db_file(fx._dir.path())).unwrap();
    db.execute_batch(
        "CREATE TRIGGER cad1337_fail_account_registration
         BEFORE INSERT ON chat_upload_accounts
         BEGIN SELECT RAISE(ABORT, 'cad1337 fixture account failure'); END;
         CREATE TRIGGER cad1337_fail_compensation_prepare
         BEFORE INSERT ON events
         WHEN NEW.kind='board_session_mutation_prepared'
              AND instr(NEW.payload, 'board_session_revoked') > 0
         BEGIN SELECT RAISE(ABORT, 'cad1337 fixture compensation audit failure'); END;",
    )
    .unwrap();
    drop(db);

    let failure = fx
        .call(
            Asserted::Operator,
            "board_session_open",
            json!({"assertion":assertion.clone()}),
        )
        .expect_err("open returned a credential after compensation audit refusal")
        .to_string();
    assert!(
        !failure.contains(&assertion)
            && !failure.contains("cad1337 fixture account failure")
            && !failure.contains("cad1337 fixture compensation audit failure"),
        "post-open uncertainty leaked credentials or raw storage/audit errors: {failure}"
    );
    assert!(
        failure.contains("outcome") || failure.contains("audit_unavailable"),
        "cleanup audit refusal was not surfaced as static uncertainty: {failure}"
    );

    let replay = fx
        .call(
            Asserted::Operator,
            "board_session_open",
            json!({"assertion":assertion}),
        )
        .expect_err("compensation-audit failure did not preserve JTI spend")
        .to_string();
    assert!(
        replay.contains("already exchanged") || replay.contains("replayed"),
        "unexpected one-use replay result: {replay}"
    );
    let sessions: Value = serde_json::from_slice(
        &std::fs::read(fx._dir.path().join("operator/sessions.json")).unwrap(),
    )
    .unwrap();
    assert!(
        sessions["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| { row["user"]["sub"].as_str() == Some(subject) }),
        "failed compensation audit must not silently claim the session was revoked"
    );
    let records = cad1337_repair_audit_rows(&fx);
    assert!(
        records.iter().any(|(kind, payload)| {
            kind == "board_session_mutation_finished"
                && payload["action"] == "board_session_opened"
                && payload["outcome"] == "uncertain"
                && cad1337_repair_has_subject(payload, subject)
        }),
        "prepared open needs an uncertain outcome when compensation cannot be audited"
    );
    assert!(
        !records.iter().any(|(kind, payload)| {
            kind == "board_session_mutation_prepared"
                && payload["action"] == "board_session_revoked"
                && cad1337_repair_has_subject(payload, subject)
        }),
        "failed compensation prepare must not claim an audited cleanup intent"
    );
}
