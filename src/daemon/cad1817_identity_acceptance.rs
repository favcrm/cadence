// CAD-1817 Phase 1 independent guard acceptance, authored from the ticket
// (TEST-003 / REQ-003 / REQ-004 / REQ-006 / REQ-011) and the frozen contract,
// independently of the source writer. Product implementers must not edit or
// weaken this file. It is included inside `daemon::team_assistants_refusal`
// so it reuses that module's REAL fixtures: a live signed member envelope and
// revalidate platform (`Fx`), the production stamp writer (`enqueue_sent`),
// a registry-minted current turn token, `Asserted::Agent` callers, the real
// `member_assist_state` / `message_report` dispatch, the real protected
// master file writer, and a real isolated daemon + board for the HTTP peer.
//
// What is exercised:
//   * personal identity is a bounded read of the private profile files and
//     an unreadable/symlinked/oversized profile ERRORS instead of silently
//     reading as unset; one principal's broken profile does not affect
//     another's;
//   * a foreign assistant carrying the victim's exact running message and
//     current token cannot ACK the victim profile, including the new
//     `user_name` / `look` target fields; an operator-derived ACK is still
//     refused; a stale revision, stale turn, platform-revoked member and
//     locally revoked binding each refuse with DB, private files, PM tree
//     and the identity projection byte-identical;
//   * an unknown/disabled avatar look, non-string value and unknown profile
//     field refuse before any effect;
//   * a legitimate later `update` in complete/skipped/deferred preserves the
//     lifecycle state, other fields, stable alias/conversation and pinned
//     look, and clearing the name returns the unset fallback without
//     reopening setup;
//   * `master_identity_set` is a proven-operator verb: agent (including the
//     master itself), unproven and forged-actor calls refuse; identity
//     selectors, stale revision, unknown field and unknown look refuse;
//     a proven operator writes IDENTITY.md (name), SOUL.md (tone) and
//     params.look, keeps the internal `master` alias and leaves the existing
//     operator-only `agent_file_write` protection intact;
//   * `POST /api/master/identity` is no weaker: agent, unproven, stolen
//     session, forged actor field, stale revision and unknown look all
//     refuse without state/file effects, while the operator's session +
//     proof succeeds.

// The canonical AgentAvatar looks (ui/src/features/agents/AgentAvatar.tsx).
const CAD1817_AVATAR_LOOKS: [&str; 11] = [
    "builder",
    "guard",
    "bookkeeper",
    "content",
    "designer",
    "reception",
    "operations",
    "support",
    "tester",
    "robot",
    "custom",
];

fn cad1817_profile_files(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    [
        "BOOTSTRAP.md",
        "IDENTITY.md",
        "SOUL.md",
        "USER.md",
        "AGENTS.md",
    ]
    .into_iter()
    .map(|name| (name.to_string(), std::fs::read(root.join(name)).ok()))
    .collect()
}

fn cad1817_profile_text(files: &BTreeMap<String, Option<Vec<u8>>>, name: &str) -> String {
    files
        .get(name)
        .and_then(|bytes| bytes.as_ref())
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default()
}

/// The consumer-visible identity projection from `member_assist_state` —
/// read through the real signed-envelope dispatch, never the store row.
fn cad1817_identity(fx: &Fx, sub: &str) -> Value {
    let state = fx.op(
        "member_assist_state",
        json!({"member_envelope": fx.member_envelope(sub)}),
    );
    let identity = state
        .get("assistant")
        .and_then(|assistant| assistant.get("identity"))
        .cloned()
        .unwrap_or_else(|| panic!("member_assist_state has no assistant.identity: {state}"));
    assert!(
        identity.is_object(),
        "assistant.identity is not an object: {identity}"
    );
    identity
}

/// The DB half of "no effect": lifecycle state, revision and the stable
/// binding identity. `AssistantBinding`'s other columns (`updated`) are not
/// compared because a refusal must not even touch them, but the SQL UPDATE
/// itself is guarded by the revision CAS.
fn cad1817_binding_snapshot(fx: &Fx, sub: &str) -> Value {
    let b = fx
        .shared
        .store
        .assistant_binding(ORG, sub)
        .unwrap()
        .unwrap_or_else(|| panic!("no binding for {sub}"));
    json!({
        "bootstrap_state": b.bootstrap_state,
        "bootstrap_revision": b.bootstrap_revision,
        "assistant_id": b.assistant_id,
        "alias": b.alias,
        "conversation_id": b.conversation_id,
        "authority_rev": b.authority_rev,
        "state": b.state,
        "bootstrap_template_version": b.bootstrap_template_version,
    })
}

/// Files + binding + PM tree, without the identity projection. Used where
/// the authority itself was withdrawn and `member_assist_state` must refuse
/// (a "no effect" probe must not require a successful projection).
fn cad1817_storage_snapshot(fx: &Fx, sub: &str, root: &Path) -> Value {
    let files: BTreeMap<String, Option<String>> = cad1817_profile_files(root)
        .into_iter()
        .map(|(name, bytes)| {
            (
                name,
                bytes.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
            )
        })
        .collect();
    json!({
        "binding": cad1817_binding_snapshot(fx, sub),
        "files": files,
        "tree": fx.tree(),
    })
}

fn cad1817_profile_snapshot(fx: &Fx, sub: &str, root: &Path) -> Value {
    let mut snapshot = cad1817_storage_snapshot(fx, sub, root);
    snapshot["identity"] = cad1817_identity(fx, sub);
    snapshot
}

/// Seed the victim's stamped running turn exactly as the production send
/// path does (verified-member stamp + registry-minted current token). No
/// RPC sends and no actor launch: this is the same quiescent fixture the
/// existing CAD-1420 ACK acceptance uses.
fn cad1817_running_turn(fx: &Fx, sub: &str) -> (String, String, String, PathBuf) {
    let rt = fx.member_runtime(sub);
    let binding = fx
        .shared
        .store
        .assistant_binding(ORG, sub)
        .unwrap()
        .unwrap_or_else(|| panic!("no binding for {sub}"));
    let root = fx.shared.assistant_principal_cwd(ORG, sub);
    let generation = "0123456789abcdef0123456789abcdef";
    let token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(generation);
    let message = format!("cad1817-{sub}-turn");
    fx.shared
        .store
        .enqueue_sent(
            &rt.alias,
            "cad1817 identity turn",
            None,
            &message,
            &format!("member-{sub}"),
            None,
            &crate::store::Sender::VerifiedMember(crate::store::MemberAuthority {
                org: ORG.to_string(),
                sub: sub.to_string(),
                assistant_id: binding.assistant_id.clone(),
                credential_id: format!("cred-{sub}"),
            }),
        )
        .unwrap();
    fx.shared
        .store
        .fixture_write(|tx| {
            // Preserve the runtime's existing params (including the REQ-011
            // look pin) while making this turn long-lived.
            let stored: Option<String> = tx.query_row(
                "SELECT params FROM agents WHERE alias=?",
                [rt.alias.as_str()],
                |row| row.get(0),
            )?;
            let mut params = stored
                .as_deref()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .unwrap_or_else(|| json!({}));
            params["report_timeout_secs"] = json!(3600);
            tx.execute(
                "UPDATE agents SET generation=?, state='idle', params=? WHERE alias=?",
                rusqlite::params![generation, params.to_string(), rt.alias.as_str()],
            )?;
            Ok(())
        })
        .unwrap();
    fx.shared.store.mark_running(&message, &token).unwrap();
    let agent = fx.shared.store.agent(&rt.alias).unwrap();
    assert!(
        crate::adapter::registry::turn_token_current(
            &agent.provider,
            &agent.endpoint_kind,
            agent.generation.as_deref(),
            &token,
        ),
        "fixture token is not current for the stored provider/endpoint/generation"
    );
    let running = fx.shared.store.message(&message).unwrap().unwrap();
    assert_eq!(running.state, "running", "victim turn is not running");
    assert_eq!(running.turn_id.as_deref(), Some(token.as_str()));
    (rt.alias, message, token, root)
}

fn cad1817_ack(
    fx: &Fx,
    alias: &str,
    message: &str,
    token: &str,
    bootstrap: Value,
) -> crate::Result<Value> {
    fx.as_assistant(
        alias,
        "message_report",
        json!({"message": message, "token": token, "kind": "ack", "bootstrap": bootstrap}),
    )
}

// ---------------------------------------------------------------------------
// Personal identity read: files are the authority; an unreadable profile is
// an error, never a silent unset.
// ---------------------------------------------------------------------------

#[test]
fn cad1817_personal_identity_read_is_canonical_and_fails_closed() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let sub = "sub-cad1817-identity-read";
    let _rt = fx.member_runtime(sub);
    let _control = fx.member_runtime("sub-cad1817-identity-control");
    let binding = fx
        .shared
        .store
        .assistant_binding(ORG, sub)
        .unwrap()
        .unwrap();
    let root = fx.shared.assistant_principal_cwd(ORG, sub);

    // Unset starter profile: name/tone unset, the pinned look is the stable
    // alias-derived canonical look, revision is the committed binding one.
    let identity = cad1817_identity(&fx, sub);
    assert_eq!(identity["name"], Value::Null, "unset name: {identity}");
    assert_eq!(identity["tone"], Value::Null, "unset tone: {identity}");
    assert_eq!(
        identity["revision"],
        json!(binding.bootstrap_revision),
        "identity revision is not the committed bootstrap revision: {identity}"
    );
    // Independently worked literal, NOT a copy of the projection helper:
    // fixture alias "assist-agent-sub-cad1817-identity-read" with
    // h = h*31 + UTF-16 code unit (u32), h = 4081543250, 4081543250 % 11 = 4,
    // and canonical list index 4 is "designer".
    assert_eq!(
        identity["look"],
        json!("designer"),
        "REQ-011: the initial look must derive from the immutable alias {} (expected pinned designer): {identity}",
        binding.alias
    );
    assert!(identity.get("preferences").is_none());
    assert!(identity.get("user_name").is_none());
    let raw = fx
        .op(
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope(sub)}),
        )
        .to_string();
    assert!(
        !raw.contains("user_name"),
        "the private user address leaked into member_assist_state: {raw}"
    );

    let before_files = cad1817_profile_files(&root);
    assert!(
        before_files["IDENTITY.md"].is_some(),
        "fixture seeded no IDENTITY.md"
    );

    // A symlink is never followed: the read errors truthfully and the target
    // is not read or overwritten.
    let outside = fx.dir.path().join("cad1817-outside-identity.md");
    std::fs::write(&outside, b"CAD1817-OUTSIDE-SECRET").unwrap();
    std::fs::remove_file(root.join("IDENTITY.md")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("IDENTITY.md")).unwrap();
    let refused = fx
        .op_err(
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope(sub)}),
        )
        .unwrap_err()
        .to_string();
    assert!(
        !refused.is_empty(),
        "a symlinked IDENTITY.md was read instead of refused"
    );
    assert!(
        !refused.contains("CAD1817-OUTSIDE-SECRET"),
        "the refusal leaked the symlink target: {refused}"
    );
    assert_eq!(
        std::fs::read(&outside).unwrap().as_slice(),
        &b"CAD1817-OUTSIDE-SECRET"[..],
        "the refused read touched the symlink target"
    );
    // The broken profile is this principal's: another member still reads.
    let control_identity = cad1817_identity(&fx, "sub-cad1817-identity-control");
    assert!(
        control_identity["revision"].is_i64(),
        "another member's identity read broke with the victim's profile: {control_identity}"
    );

    // An oversized profile refuses at the bounded read, not as unset.
    std::fs::remove_file(root.join("IDENTITY.md")).unwrap();
    std::fs::write(root.join("IDENTITY.md"), vec![b'x'; 70 * 1024]).unwrap();
    let oversized = fx
        .op_err(
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope(sub)}),
        )
        .unwrap_err()
        .to_string();
    assert!(
        !oversized.is_empty(),
        "an oversized IDENTITY.md was read instead of refused"
    );

    // Restore the exact pre-test bytes so the fixture leaves no trace.
    if let Some(bytes) = before_files["IDENTITY.md"].as_ref() {
        std::fs::write(root.join("IDENTITY.md"), bytes).unwrap();
    }
    assert_eq!(
        cad1817_profile_files(&root),
        before_files,
        "identity read failure left a file effect"
    );
}

// ---------------------------------------------------------------------------
// Personal profile ACK: foreign/operator/stale/revoked/unknown-look refusals
// never reach files, DB, PM tree or the identity projection.
// ---------------------------------------------------------------------------

#[test]
fn cad1817_profile_ack_refusals_have_no_effect() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let victim_sub = "sub-cad1817-ack-victim";
    let thief_sub = "sub-cad1817-ack-thief";
    let thief = fx.member_runtime(thief_sub);
    // `cad1817_running_turn` provisions the victim once; a second
    // `member_runtime` would reuse its per-sub foreign message id.
    let (alias, message, token, root) = cad1817_running_turn(&fx, victim_sub);
    let revision = fx
        .shared
        .store
        .assistant_binding(ORG, victim_sub)
        .unwrap()
        .unwrap()
        .bootstrap_revision;
    let before = cad1817_profile_snapshot(&fx, victim_sub, &root);

    // Bad 1 — a foreign assistant carrying the victim's exact running
    // message/token and a full target effect (name, tone, user_name, look,
    // preferences) is refused by ownership before any effect.
    let stolen = cad1817_ack(
        &fx,
        &thief.alias,
        &message,
        &token,
        json!({"action": "update", "expected_revision": revision,
               "profile": {"name": "Stolen", "tone": "stolen", "preferences": "stolen",
                           "user_name": "Stolen user", "look": "guard"}}),
    );
    assert!(
        stolen.is_err(),
        "a foreign assistant's ACK of the victim profile was accepted"
    );
    assert_eq!(
        cad1817_profile_snapshot(&fx, victim_sub, &root),
        before,
        "a foreign ACK changed profile/identity state"
    );

    // Bad 2 — a proven operator connection carrying the same valid running
    // turn is not the assistant's own connection.
    let operator = fx.op_err(
        "message_report",
        json!({"message": message, "token": token, "kind": "ack",
               "bootstrap": {"action": "update", "expected_revision": revision,
                             "profile": {"name": "Operator", "look": "guard"}}}),
    );
    assert!(
        operator.is_err(),
        "an operator-derived ACK of the assistant profile was accepted"
    );
    assert_eq!(cad1817_profile_snapshot(&fx, victim_sub, &root), before);

    // Bad 3 — stale revision with a current turn/token.
    let stale_revision = cad1817_ack(
        &fx,
        &alias,
        &message,
        &token,
        json!({"action": "update", "expected_revision": revision + 1,
               "profile": {"name": "Stale", "tone": "stale", "look": "guard"}}),
    );
    assert!(
        stale_revision.is_err(),
        "an ACK with a mismatched expected_revision was accepted"
    );
    assert_eq!(cad1817_profile_snapshot(&fx, victim_sub, &root), before);

    // Bad 4 — unknown avatar look refuses before any effect.
    let bad_look = cad1817_ack(
        &fx,
        &alias,
        &message,
        &token,
        json!({"action": "update", "expected_revision": revision,
               "profile": {"name": "Look", "look": "dragon"}}),
    );
    assert!(bad_look.is_err(), "an unknown avatar look was accepted");
    assert_eq!(cad1817_profile_snapshot(&fx, victim_sub, &root), before);

    // Bad 5 — non-string value and an unknown profile field refuse.
    for (case, profile) in [
        ("non-string look", json!({"look": 7})),
        ("unknown profile field", json!({"nickname": "nick"})),
        (
            "reserved marker",
            json!({"name": "x <!-- cadence:managed-name -->"}),
        ),
    ] {
        let refused = cad1817_ack(
            &fx,
            &alias,
            &message,
            &token,
            json!({"action": "update", "expected_revision": revision, "profile": profile}),
        );
        assert!(refused.is_err(), "{case} was accepted");
        assert_eq!(
            cad1817_profile_snapshot(&fx, victim_sub, &root),
            before,
            "{case} changed profile/identity state"
        );
    }

    // Bad 6 — a stale turn (no longer running) refuses even with the stored
    // revision and a current-looking token.
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE messages SET state='completed' WHERE id=?",
                [&message],
            )?;
            Ok(())
        })
        .unwrap();
    let stale_turn = cad1817_ack(
        &fx,
        &alias,
        &message,
        &token,
        json!({"action": "update", "expected_revision": revision,
               "profile": {"name": "Stale turn", "look": "guard"}}),
    );
    assert!(
        stale_turn.is_err(),
        "a completed turn accepted a profile update"
    );
    assert_eq!(cad1817_profile_snapshot(&fx, victim_sub, &root), before);
    // Restore the running turn for the remaining (revocation) probes below.
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE messages SET state='running', turn_id=? WHERE id=?",
                rusqlite::params![token, message],
            )?;
            Ok(())
        })
        .unwrap();

    // Bad 7 — the platform authority is withdrawn after the turn was
    // queued; the live re-prove refuses the update.
    let revoked_sub = "sub-cad1817-ack-platform-revoked";
    let (revoked_alias, revoked_message, revoked_token, revoked_root) =
        cad1817_running_turn(&fx, revoked_sub);
    let revoked_revision = fx
        .shared
        .store
        .assistant_binding(ORG, revoked_sub)
        .unwrap()
        .unwrap()
        .bootstrap_revision;
    fx.remove_member(revoked_sub);
    // Snapshot AFTER the deliberate authority withdrawal: the guard's
    // invariant is that the refused ACK itself changes nothing, not that the
    // fixture's intentional revocation is invisible.
    let revoked_before = cad1817_storage_snapshot(&fx, revoked_sub, &revoked_root);
    let denied = cad1817_ack(
        &fx,
        &revoked_alias,
        &revoked_message,
        &revoked_token,
        json!({"action": "update", "expected_revision": revoked_revision,
               "profile": {"name": "Revoked", "look": "guard"}}),
    );
    assert!(
        denied.is_err(),
        "a platform-revoked member's profile ACK was accepted"
    );
    assert_eq!(
        cad1817_storage_snapshot(&fx, revoked_sub, &revoked_root),
        revoked_before,
        "a platform-revoked ACK changed profile/identity state"
    );
    assert!(
        fx.op_err(
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope(revoked_sub)})
        )
        .is_err(),
        "a platform-revoked member still projected an identity"
    );

    // Bad 8 — the LOCAL binding is revoked; the ACK refuses at the
    // active-binding CAS with the profile untouched.
    let local_sub = "sub-cad1817-ack-local-revoked";
    let (local_alias, local_message, local_token, local_root) =
        cad1817_running_turn(&fx, local_sub);
    let local_revision = fx
        .shared
        .store
        .assistant_binding(ORG, local_sub)
        .unwrap()
        .unwrap()
        .bootstrap_revision;
    fx.revoke(local_sub);
    // Snapshot AFTER the deliberate local revocation (which legitimately
    // flips binding.state to 'revoked'); the refused ACK must add no further
    // profile/file/identity effect.
    let local_before = cad1817_storage_snapshot(&fx, local_sub, &local_root);
    let local_refused = cad1817_ack(
        &fx,
        &local_alias,
        &local_message,
        &local_token,
        json!({"action": "update", "expected_revision": local_revision,
               "profile": {"name": "Local revoked", "look": "guard"}}),
    );
    assert!(
        local_refused.is_err(),
        "a locally revoked binding accepted a profile update"
    );
    assert_eq!(
        cad1817_storage_snapshot(&fx, local_sub, &local_root),
        local_before,
        "a locally revoked ACK changed profile/identity state"
    );
    // The revoked binding is not silently unset: the state read refuses.
    assert!(
        fx.op_err(
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope(local_sub)})
        )
        .is_err(),
        "a revoked binding still projected an identity"
    );
}

// ---------------------------------------------------------------------------
// A legitimate later update in complete/skipped/deferred preserves the
// lifecycle, other fields, stable identity and the pinned look; clearing
// the name returns the unset fallback.
// ---------------------------------------------------------------------------

#[test]
fn cad1817_profile_update_preserves_lifecycle_and_other_fields() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);

    for (action, expected_state) in [
        ("complete", "complete"),
        ("skip", "skipped"),
        ("defer", "deferred"),
    ] {
        let sub = format!("sub-cad1817-life-{action}");
        let (alias, message, token, root) = cad1817_running_turn(&fx, &sub);
        let before = fx
            .shared
            .store
            .assistant_binding(ORG, &sub)
            .unwrap()
            .unwrap();
        let conversation = before
            .conversation_id
            .clone()
            .expect("the member runtime must bind a conversation");
        let history_before = fx
            .shared
            .store
            .thread_entries_of(&conversation, 0, 100)
            .unwrap()
            .len();
        let agent_before = fx.shared.store.agent(&alias).unwrap();
        // The owning row's pin is the authority the projection must echo;
        // this does not recompute the alias-look algorithm.
        let initial_pin = agent_before
            .params
            .as_ref()
            .and_then(|params| params.get("look"))
            .and_then(Value::as_str)
            .expect("ensure_assistant_runtime must pin the initial alias look")
            .to_string();
        assert_eq!(
            cad1817_identity(&fx, &sub)["look"],
            json!(initial_pin),
            "the identity projection did not echo the owning assistant's pin"
        );
        // A different canonical pin proves rename/tone/reload never reroll it.
        let pinned_look = *CAD1817_AVATAR_LOOKS
            .iter()
            .find(|look| **look != initial_pin)
            .expect("the canonical list has more than one look");
        let rev0 = before.bootstrap_revision;

        // Enter the lifecycle state through the real ACK, no profile.
        cad1817_ack(
            &fx,
            &alias,
            &message,
            &token,
            json!({"action": action, "expected_revision": rev0}),
        )
        .unwrap_or_else(|e| panic!("{action} refused: {e}"));
        let after_transition = fx
            .shared
            .store
            .assistant_binding(ORG, &sub)
            .unwrap()
            .unwrap();
        assert_eq!(
            after_transition.bootstrap_state, expected_state,
            "{action} did not record {expected_state}"
        );
        let rev1 = after_transition.bootstrap_revision;
        assert!(rev1 > rev0, "{action} did not advance the revision");

        // A later explicit update (rename + tone + address + look +
        // preferences) is allowed in every state and keeps the state.
        cad1817_ack(
            &fx,
            &alias,
            &message,
            &token,
            json!({"action": "update", "expected_revision": rev1,
                   "profile": {"name": "First Assistant", "tone": "warm",
                               "preferences": "keep this preference",
                               "user_name": "Avery", "look": pinned_look}}),
        )
        .unwrap_or_else(|e| panic!("{expected_state} update refused: {e}"));
        let after_update = fx
            .shared
            .store
            .assistant_binding(ORG, &sub)
            .unwrap()
            .unwrap();
        assert_eq!(
            after_update.bootstrap_state, expected_state,
            "an update changed the {expected_state} lifecycle state"
        );
        assert_eq!(
            after_update.assistant_id, before.assistant_id,
            "an update changed the stable assistant id"
        );
        assert_eq!(
            after_update.alias, before.alias,
            "an update changed the stable alias"
        );
        assert_eq!(
            after_update.conversation_id, before.conversation_id,
            "an update changed the bound conversation"
        );
        assert!(after_update.bootstrap_revision > rev1);
        assert_eq!(
            fx.shared
                .store
                .thread_entries_of(&conversation, 0, 100)
                .unwrap()
                .len(),
            history_before,
            "an identity update changed the assistant history"
        );
        let agent_after = fx.shared.store.agent(&alias).unwrap();
        assert_eq!(
            (
                agent_after.alias.clone(),
                agent_after.provider.clone(),
                agent_after.cwd.clone()
            ),
            (
                agent_before.alias.clone(),
                agent_before.provider.clone(),
                agent_before.cwd.clone()
            ),
            "an identity update changed the assistant runtime ownership"
        );
        let rev2 = after_update.bootstrap_revision;

        let identity = cad1817_identity(&fx, &sub);
        assert_eq!(identity["name"], json!("First Assistant"));
        assert_eq!(identity["tone"], json!("warm"));
        assert_eq!(identity["revision"], json!(rev2));
        assert_eq!(
            identity["look"],
            json!(pinned_look),
            "the explicit look was not recorded: {identity}"
        );
        let owning_pin = |fx: &Fx| -> Option<String> {
            fx.shared
                .store
                .agent(&alias)
                .unwrap()
                .params
                .and_then(|params| {
                    params
                        .get("look")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
        };
        assert_eq!(
            owning_pin(&fx).as_deref(),
            Some(pinned_look),
            "the explicit pick was not persisted on the owning assistant row"
        );
        assert!(
            identity.get("user_name").is_none() && identity.get("preferences").is_none(),
            "private address/preferences leaked into identity: {identity}"
        );

        let files = cad1817_profile_files(&root);
        let identity_md = cad1817_profile_text(&files, "IDENTITY.md");
        let soul_md = cad1817_profile_text(&files, "SOUL.md");
        let user_md = cad1817_profile_text(&files, "USER.md");
        assert!(identity_md.contains("First Assistant"));
        assert!(!identity_md.contains("Avery"));
        assert!(!identity_md.contains("keep this preference"));
        assert!(soul_md.contains("warm"));
        assert!(user_md.contains("Avery"));
        assert!(user_md.contains("keep this preference"));
        assert_eq!(
            files["BOOTSTRAP.md"].is_none(),
            action == "complete",
            "BOOTSTRAP presence does not follow the lifecycle: {action}"
        );

        // A genuinely old revision (rev1, before this update) is stale even
        // with the current turn/token: it refuses with the profile, look,
        // lifecycle and identity projection unchanged.
        let before_stale = cad1817_profile_snapshot(&fx, &sub, &root);
        let stale = cad1817_ack(
            &fx,
            &alias,
            &message,
            &token,
            json!({"action": "update", "expected_revision": rev1,
                   "profile": {"name": "Stale Assistant", "look": "robot"}}),
        );
        assert!(
            stale.is_err(),
            "{expected_state}: an ACK with a genuinely old revision was accepted"
        );
        assert_eq!(
            cad1817_profile_snapshot(&fx, &sub, &root),
            before_stale,
            "{expected_state} stale revision changed profile/identity state"
        );

        // The model-context projection uses the same effective identity.
        let running = fx.shared.store.message(&message).unwrap().unwrap();
        let context = fx
            .shared
            .member_profile_context(&running)
            .unwrap()
            .unwrap_or_default();
        assert!(
            context.contains("state: ") && context.contains(&format!("state: {expected_state}")),
            "{expected_state} context lacks its state: {context}"
        );
        assert!(context.contains(&format!("revision: {rev2}")));
        assert!(context.contains("First Assistant"));
        assert!(context.contains("warm"));
        if action != "pending" {
            assert!(
                !context.contains("## Bootstrap (pending)"),
                "{expected_state} context carried the pending guide"
            );
        }

        // Rename later: only the named field changes, look and address stay.
        cad1817_ack(
            &fx,
            &alias,
            &message,
            &token,
            json!({"action": "update", "expected_revision": rev2,
                   "profile": {"name": "Second Assistant"}}),
        )
        .unwrap_or_else(|e| panic!("{expected_state} rename refused: {e}"));
        let renamed = cad1817_identity(&fx, &sub);
        assert_eq!(renamed["name"], json!("Second Assistant"));
        assert_eq!(renamed["tone"], json!("warm"));
        assert_eq!(renamed["look"], json!(pinned_look));
        assert_eq!(
            owning_pin(&fx).as_deref(),
            Some(pinned_look),
            "a rename rerolled or cleared the owning pin"
        );
        let files = cad1817_profile_files(&root);
        assert!(cad1817_profile_text(&files, "USER.md").contains("Avery"));
        assert!(cad1817_profile_text(&files, "USER.md").contains("keep this preference"));
        let rev3 = fx
            .shared
            .store
            .assistant_binding(ORG, &sub)
            .unwrap()
            .unwrap()
            .bootstrap_revision;

        // Tone-only update keeps name/look/address.
        cad1817_ack(
            &fx,
            &alias,
            &message,
            &token,
            json!({"action": "update", "expected_revision": rev3,
                   "profile": {"tone": "direct"}}),
        )
        .unwrap_or_else(|e| panic!("{expected_state} tone update refused: {e}"));
        let toned = cad1817_identity(&fx, &sub);
        assert_eq!(toned["name"], json!("Second Assistant"));
        assert_eq!(toned["tone"], json!("direct"));
        assert_eq!(toned["look"], json!(pinned_look));
        assert_eq!(
            owning_pin(&fx).as_deref(),
            Some(pinned_look),
            "a tone update rerolled or cleared the owning pin"
        );

        // Clearing the name explicitly returns the unset fallback, keeps
        // everything else and does not reopen setup.
        let rev4 = fx
            .shared
            .store
            .assistant_binding(ORG, &sub)
            .unwrap()
            .unwrap()
            .bootstrap_revision;
        cad1817_ack(
            &fx,
            &alias,
            &message,
            &token,
            json!({"action": "update", "expected_revision": rev4,
                   "profile": {"name": ""}}),
        )
        .unwrap_or_else(|e| panic!("{expected_state} clear-name refused: {e}"));
        let cleared = cad1817_identity(&fx, &sub);
        assert_eq!(
            cleared["name"],
            Value::Null,
            "clearing the name did not return the unset fallback: {cleared}"
        );
        assert_eq!(cleared["tone"], json!("direct"));
        assert_eq!(cleared["look"], json!(pinned_look));
        // Every identity read also runs the runtime (re)entry path, so this
        // is the pin surviving a reload as well as all profile updates.
        assert_eq!(
            owning_pin(&fx).as_deref(),
            Some(pinned_look),
            "a runtime reload/clear rerolled or cleared the owning pin"
        );
        let cleared_binding = fx
            .shared
            .store
            .assistant_binding(ORG, &sub)
            .unwrap()
            .unwrap();
        assert_eq!(
            cleared_binding.bootstrap_state, expected_state,
            "clearing the name changed the lifecycle"
        );
        assert_eq!(cleared_binding.alias, before.alias);
        assert_eq!(
            fx.shared
                .store
                .thread_entries_of(&conversation, 0, 100)
                .unwrap()
                .len(),
            history_before,
            "the full update sequence changed the assistant history"
        );
        let agent_final = fx.shared.store.agent(&alias).unwrap();
        assert_eq!(
            (agent_final.alias, agent_final.provider, agent_final.cwd),
            (agent_before.alias, agent_before.provider, agent_before.cwd),
            "the full update sequence changed the assistant runtime ownership"
        );
    }
}

// ---------------------------------------------------------------------------
// Coordinator identity — native verb.
// ---------------------------------------------------------------------------

/// Register a non-master agent so an `Asserted::Agent` caller resolves from
/// the registry (the seam never invents an identity).
fn cad1817_register_agent(shared: &Shared, alias: &str) {
    if shared.store.agent_opt(alias).unwrap().is_some() {
        return;
    }
    let cwd = shared.state_dir.join("cad1817-agents").join(alias);
    std::fs::create_dir_all(&cwd).unwrap();
    shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias,
            provider: "inbox",
            endpoint_kind: "inbox",
            role: "worker",
            cwd: cwd.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
}

fn cad1817_register_master(shared: &Shared, cwd: &Path) {
    if shared
        .store
        .agent_opt(crate::master::ALIAS)
        .unwrap()
        .is_some()
    {
        return;
    }
    std::fs::create_dir_all(cwd).unwrap();
    shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: crate::master::ALIAS,
            provider: "inbox",
            endpoint_kind: "inbox",
            role: "worker",
            cwd: cwd.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
}

/// Seed the protected master files through the production writer/template
/// path and record their digests, so a later identity write is judged by the
/// same digest guard `master start` uses. Returns the PM for byte reads.
fn cad1817_prepare_master(shared: &Shared, cwd: &Path) -> crate::issue::Pm {
    cad1817_register_master(shared, cwd);
    let pm = shared.pm().unwrap();
    crate::master::install_defaults(&pm, "operator").unwrap();
    for name in crate::master::FILES {
        let path = pm.dir.join("agents").join(crate::master::ALIAS).join(name);
        if let Ok(text) = std::fs::read_to_string(&path) {
            crate::master::record(
                &shared.state_dir,
                crate::master::ALIAS,
                name,
                &crate::master::digest(&text),
            )
            .unwrap();
        }
    }
    pm
}

fn cad1817_master_files(pm_dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    ["IDENTITY.md", "SOUL.md", "AGENT.md"]
        .into_iter()
        .map(|name| {
            (
                name.to_string(),
                std::fs::read(pm_dir.join("agents").join(crate::master::ALIAS).join(name)).ok(),
            )
        })
        .collect()
}

fn cad1817_owning_look(shared: &Shared, alias: &str) -> Option<String> {
    shared
        .store
        .agent(alias)
        .unwrap()
        .params
        .and_then(|params| {
            params
                .get("look")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

fn cad1817_master_text(files: &BTreeMap<String, Option<Vec<u8>>>, name: &str) -> String {
    files
        .get(name)
        .and_then(|bytes| bytes.as_ref())
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default()
}

#[test]
fn cad1817_master_identity_native_requires_operator_and_writes_protected_files() {
    let fx = Fx::new();
    cad1817_register_agent(&fx.shared, "cad1817-intruder");
    let pm = cad1817_prepare_master(&fx.shared, &fx.dir.path().join("master-cwd"));
    // A Home-scope active turn: without one, `master_policy` fail-closes an
    // agent-master call to the APP allowlist before the operator guard, which
    // is not the guard under test. No thread entry means Home (CAD-1098).
    let (duplicate, _) = fx
        .shared
        .store
        .enqueue_steered(
            crate::master::ALIAS,
            "cad1817 master home turn",
            None,
            "cad1817-master-home-turn",
            "chat",
            None,
            None,
            None,
            &crate::store::Sender::Unattributed,
            &crate::store::Steer::NONE,
            None,
            None,
            None,
        )
        .unwrap();
    assert!(!duplicate, "the master home turn id collided");
    fx.shared
        .store
        .mark_running("cad1817-master-home-turn", "cad1817-master-home")
        .unwrap();
    let state_before = fx.op("master_state", json!({}));
    assert_eq!(
        state_before["turn"]["state"],
        json!("working"),
        "the fixture master turn is not the running Home turn: {state_before}"
    );
    assert!(
        state_before["turn"]["conversation"].is_null(),
        "the fixture master turn must be Home scope (no app conversation): {state_before}"
    );
    assert_eq!(state_before["alias"], json!(crate::master::ALIAS));
    let identity = state_before["identity"].clone();
    assert_eq!(
        identity["name"],
        Value::Null,
        "unset master name: {state_before}"
    );
    assert_eq!(
        identity["tone"],
        Value::Null,
        "unset master tone: {state_before}"
    );
    assert_eq!(
        identity["look"],
        json!("robot"),
        "REQ-011: an unpinned master defaults to the existing robot: {state_before}"
    );
    let revision = identity["revision"]
        .as_str()
        .expect("master identity revision must be a string")
        .to_string();
    let files_before = cad1817_master_files(&pm.dir);
    let body = json!({"expected_revision": revision, "name": "Workspace Guide",
                      "tone": "direct", "look": "guard"});

    // Agent callers may not self-edit. The registered master's own
    // connection is refused by the master policy (Home scope established
    // above, so this can never be the app-turn outer guard); a non-master
    // agent reaches the operator connection guard. Each refusal leaves
    // state and files untouched.
    let master_refused = fx.call(
        Asserted::Agent(crate::master::ALIAS.to_string()),
        "master_identity_set",
        body.clone(),
    );
    assert!(
        master_refused.is_err(),
        "the registered master's self-edit was allowed"
    );
    assert_eq!(fx.op("master_state", json!({})), state_before);
    assert_eq!(cad1817_master_files(&pm.dir), files_before);

    let intruder_refused = fx.call(
        Asserted::Agent("cad1817-intruder".to_string()),
        "master_identity_set",
        body.clone(),
    );
    assert!(
        intruder_refused.is_err(),
        "a non-master agent write to the master identity was accepted"
    );
    assert_eq!(fx.op("master_state", json!({})), state_before);
    assert_eq!(cad1817_master_files(&pm.dir), files_before);

    // An unattributable caller is not the operator either.
    let unproven = fx.call(Asserted::Unproven, "master_identity_set", body.clone());
    assert!(
        unproven.is_err(),
        "an unattributable caller wrote the master identity"
    );
    assert_eq!(fx.op("master_state", json!({})), state_before);
    assert_eq!(cad1817_master_files(&pm.dir), files_before);

    // Identity selectors are refused whole — the target is connection-bound,
    // never a request field.
    for field in [
        "by", "as", "actor", "caller", "operator", "alias", "agent", "slug", "sub",
    ] {
        let mut forged = body.clone();
        forged[field] = json!("cad1817-victim");
        let refused = fx.op_err("master_identity_set", forged);
        assert!(
            refused.is_err(),
            "identity selector '{field}' was read instead of refused"
        );
        assert_eq!(fx.op("master_state", json!({})), state_before);
        assert_eq!(cad1817_master_files(&pm.dir), files_before);
    }

    // Unknown field, stale revision and unknown look each refuse before a
    // write.
    for (case, forged) in [
        (
            "unknown field",
            json!({"expected_revision": revision, "name": "X", "nickname": "n"}),
        ),
        (
            "stale revision",
            json!({"expected_revision": format!("{revision}-stale"), "name": "X"}),
        ),
        (
            "unknown look",
            json!({"expected_revision": revision, "look": "dragon"}),
        ),
        (
            "non-string look",
            json!({"expected_revision": revision, "look": 7}),
        ),
    ] {
        let refused = fx.op_err("master_identity_set", forged);
        assert!(refused.is_err(), "{case} was accepted");
        assert_eq!(
            fx.op("master_state", json!({})),
            state_before,
            "{case} changed master state"
        );
        assert_eq!(
            cad1817_master_files(&pm.dir),
            files_before,
            "{case} changed protected master files"
        );
    }

    // Proven operator: the supported write succeeds and both projections
    // and protected files move together.
    let written = fx.op("master_identity_set", body.clone());
    assert_eq!(written["identity"]["name"], json!("Workspace Guide"));
    assert_eq!(written["identity"]["tone"], json!("direct"));
    assert_eq!(written["identity"]["look"], json!("guard"));
    assert_ne!(
        written["identity"]["revision"].as_str(),
        Some(revision.as_str()),
        "the identity revision did not advance"
    );
    let applies = written["applies"].as_str().unwrap_or_default();
    assert!(
        matches!(applies, "next_turn" | "next_session"),
        "unexpected applies contract: {written}"
    );
    let after = fx.op("master_state", json!({}));
    assert_eq!(after["alias"], json!(crate::master::ALIAS));
    assert_eq!(after["identity"], written["identity"]);
    let files_after = cad1817_master_files(&pm.dir);
    assert!(
        cad1817_master_text(&files_after, "IDENTITY.md").contains("Workspace Guide"),
        "master IDENTITY.md lacks the operator's name: {files_after:?}"
    );
    assert!(
        cad1817_master_text(&files_after, "SOUL.md").contains("direct"),
        "master SOUL.md lacks the operator's tone: {files_after:?}"
    );
    assert_eq!(
        files_after["AGENT.md"], files_before["AGENT.md"],
        "master AGENT.md changed on an identity write"
    );

    // Avatar-only boundary: the public revision MUST fence the look too.
    // Otherwise a stale editor holding the pre-look revision could overwrite
    // a newer avatar even though the name/tone files did not change.
    let rev_before_look = after["identity"]["revision"]
        .as_str()
        .expect("master identity revision must be a string")
        .to_string();
    let files_before_look = cad1817_master_files(&pm.dir);
    let look_only = fx.op(
        "master_identity_set",
        json!({"expected_revision": rev_before_look, "look": "tester"}),
    );
    assert_eq!(
        look_only["identity"]["look"],
        json!("tester"),
        "the look-only operator write did not land: {look_only}"
    );
    assert_eq!(
        look_only["applies"],
        json!("next_turn"),
        "a look-only write must be next_turn presentation: {look_only}"
    );
    let rev_after_look = look_only["identity"]["revision"]
        .as_str()
        .expect("master identity revision must be a string")
        .to_string();
    assert_ne!(
        rev_after_look, rev_before_look,
        "a look-only mutation left the public identity revision unchanged — a stale \
         editor could overwrite a newer avatar: before={rev_before_look} after={rev_after_look}"
    );
    let state_after_look = fx.op("master_state", json!({}));
    assert_eq!(
        state_after_look["identity"], look_only["identity"],
        "master_state did not project the look-only write"
    );
    assert_eq!(
        cad1817_master_files(&pm.dir),
        files_before_look,
        "a look-only identity write changed protected files"
    );
    assert_eq!(
        cad1817_owning_look(&fx.shared, crate::master::ALIAS).as_deref(),
        Some("tester"),
        "the look-only write did not persist the owning pin"
    );

    // The pre-look revision is now stale: an old editor trying to restore
    // the previous avatar (or rename) refuses with no state/file/params move.
    let stale_avatar = fx.op_err(
        "master_identity_set",
        json!({"expected_revision": rev_before_look, "look": "guard",
               "name": "Stale avatar"}),
    );
    assert!(
        stale_avatar.is_err(),
        "an old revision overwrote a newer avatar"
    );
    assert_eq!(
        fx.op("master_state", json!({})),
        state_after_look,
        "a refused stale avatar overwrite changed master state"
    );
    assert_eq!(
        cad1817_master_files(&pm.dir),
        files_before_look,
        "a refused stale avatar overwrite changed protected files"
    );
    assert_eq!(
        cad1817_owning_look(&fx.shared, crate::master::ALIAS).as_deref(),
        Some("tester"),
        "a refused stale avatar overwrite changed the owning pin"
    );

    // The old revision is now stale; a retry refuses and changes nothing.
    let current = look_only["identity"]["revision"]
        .as_str()
        .unwrap()
        .to_string();
    let before_retry = (
        fx.op("master_state", json!({})),
        cad1817_master_files(&pm.dir),
    );
    let retried = fx.op_err("master_identity_set", body);
    assert!(retried.is_err(), "a stale identity retry was accepted");
    assert_eq!(
        (
            fx.op("master_state", json!({})),
            cad1817_master_files(&pm.dir)
        ),
        before_retry
    );

    // The protected-writer file defenses are unchanged: a symlinked
    // IDENTITY.md is never followed, and an oversized file refuses at its
    // cap. Neither can turn into a partial identity write.
    let identity_path = pm
        .dir
        .join("agents")
        .join(crate::master::ALIAS)
        .join("IDENTITY.md");
    let original_identity = std::fs::read(&identity_path).unwrap();
    let before_defense = fx.op("master_state", json!({}));
    let outside = fx.dir.path().join("cad1817-master-outside.md");
    std::fs::write(&outside, b"# outside the tracker\n").unwrap();
    std::fs::remove_file(&identity_path).unwrap();
    std::os::unix::fs::symlink(&outside, &identity_path).unwrap();
    let symlinked = fx.op_err(
        "master_identity_set",
        json!({"expected_revision": current, "name": "Symlinked"}),
    );
    assert!(
        symlinked.is_err(),
        "a symlinked master IDENTITY.md was written through"
    );
    assert_eq!(
        std::fs::read(&outside).unwrap().as_slice(),
        &b"# outside the tracker\n"[..],
        "the refused write touched the symlink target"
    );
    assert!(
        fx.op_err("master_state", json!({})).is_err(),
        "master_state silently read a symlinked identity as unset"
    );
    std::fs::remove_file(&identity_path).unwrap();
    std::fs::write(&identity_path, &original_identity).unwrap();
    assert_eq!(
        fx.op("master_state", json!({})),
        before_defense,
        "the refused symlink write left a state effect"
    );

    std::fs::write(&identity_path, "# oversized\n".repeat(200)).unwrap();
    let oversized = fx.op_err(
        "master_identity_set",
        json!({"expected_revision": current, "name": "Oversized"}),
    );
    assert!(
        oversized.is_err(),
        "an oversized master identity file was read as an identity"
    );
    assert!(
        fx.op_err("master_state", json!({})).is_err(),
        "master_state silently read an oversized identity as unset"
    );
    std::fs::write(&identity_path, original_identity).unwrap();
    assert_eq!(
        fx.op("master_state", json!({})),
        before_defense,
        "the refused oversized write left a state effect"
    );

    // Self-edit stays refused after the write, and the existing protected
    // file writer keeps its operator-only gate (the master cannot write its
    // own SOUL/AGENT from chat).
    let self_edit = fx.call(
        Asserted::Agent(crate::master::ALIAS.to_string()),
        "master_identity_set",
        json!({"expected_revision": current, "name": "Master itself"}),
    );
    assert!(
        self_edit.is_err(),
        "the registered master edited its own identity"
    );
    let soul_mid = cad1817_master_files(&pm.dir);
    let file_write = fx.call(
        Asserted::Agent(crate::master::ALIAS.to_string()),
        "agent_file_write",
        json!({"agent": crate::master::ALIAS, "file": "SOUL.md",
               "text": "# hijacked\n\nwritten from chat\n"}),
    );
    assert!(
        file_write.is_err(),
        "agent_file_write lost its operator-only gate"
    );
    assert_eq!(cad1817_master_files(&pm.dir), soul_mid);

    // Operator control: the same protected writer still works for a proven
    // operator, so the refusals above are identity, not a blanket deny.
    fx.op(
        "agent_file_write",
        json!({"agent": crate::master::ALIAS, "file": "AGENT.md",
               "text": "# Master instructions\n\nOperator-approved fixture text.\n"}),
    );
    let final_files = cad1817_master_files(&pm.dir);
    assert!(
        cad1817_master_text(&final_files, "AGENT.md").contains("Operator-approved fixture text"),
        "the proven operator's agent_file_write did not land"
    );
}

// ---------------------------------------------------------------------------
// Coordinator identity — real HTTP peer, no weaker than the native verb.
// ---------------------------------------------------------------------------

struct Cad1817HttpRig {
    // Field order: stop servers/threads before the temp dir is removed.
    root: tempfile::TempDir,
    state: PathBuf,
    pm: PathBuf,
    port: u16,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Cad1817HttpRig {
    fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("cad1817-http-")
            .tempdir()
            .unwrap();
        let state = root.path().join("s");
        let pm = root.path().join("pm");
        std::fs::create_dir_all(&state).unwrap();
        crate::issue::Pm::init(&pm).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();

        let opts = crate::daemon::ServeOptions {
            provider_env: crate::adapter::ProviderEnv::refusing_providers(),
            test_seam: true,
            stop: Some(stop.clone()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let dir = state.clone();
        threads.push(std::thread::spawn(move || {
            crate::daemon::serve_with(&dir, opts).unwrap();
        }));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !state.join("cadence.sock").exists()
            || crate::test_seam::Seam::token_at(&state).is_none()
        {
            assert!(
                std::time::Instant::now() < deadline,
                "owned daemon did not start"
            );
            assert!(!threads[0].is_finished(), "owned daemon exited");
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            crate::test_seam::Seam::token_at(&state).is_some(),
            "the fixture daemon minted no seam token"
        );

        let first = (std::process::id() % 90) as u16;
        let mut port = None;
        for offset in 0..90u16 {
            let candidate = 3110 + (first + offset) % 90;
            if candidate == 3162 {
                continue;
            }
            let (ready, rx) = std::sync::mpsc::channel();
            let board = crate::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port: candidate,
                stop: Some(stop.clone()),
                startup: Some(ready),
                test_seam: true,
                ..Default::default()
            };
            let (board_state, board_pm) = (state.clone(), pm.clone());
            let thread = std::thread::spawn(move || {
                drop(crate::ui::serve(&board_state, &board_pm, &board));
            });
            match rx.recv_timeout(Duration::from_secs(20)) {
                Ok(Ok(())) => {
                    threads.push(thread);
                    port = Some(candidate);
                    break;
                }
                Ok(Err(std::io::ErrorKind::AddrInUse)) => {
                    let _ = thread.join();
                }
                other => {
                    threads.push(thread);
                    panic!("board startup failed: {other:?}");
                }
            }
        }
        let port = port.expect("no isolated board port in 3110..=3199");
        Self {
            root,
            state,
            pm,
            port,
            stop,
            threads,
        }
    }

    fn rpc(&self, method: &str, params: Value) -> crate::Result<Value> {
        crate::test_seam::scoped(Asserted::Operator, || {
            crate::client::rpc_timeout(&self.state, method, params, Duration::from_secs(20))
        })
    }

    fn store(&self) -> Arc<Shared> {
        let opts = crate::daemon::ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", self.pm.to_str().unwrap());
        Shared::new(&self.state, &opts).unwrap()
    }

    /// The board's own loopback name — the only Host a local operator
    /// session is bound to (`operator::board_host`).
    fn host(&self) -> String {
        format!("cadence-{}.localhost:{}", self.port, self.port)
    }

    fn origin(&self) -> String {
        format!("http://{}", self.host())
    }

    /// A raw HTTP request. `who` is the seam assertion (`operator`,
    /// `unproven`, `agent:<alias>`); `session` is a live (cookie, key).
    fn request(
        &self,
        method: &str,
        path: &str,
        who: Option<&str>,
        session: Option<&(String, String)>,
        body: &Value,
    ) -> (u16, Vec<(String, String)>, Value) {
        let payload = serde_json::to_vec(body).unwrap();
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nOrigin: {}\r\nX-Cadence-Board: 1\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.host(),
            self.origin(),
            payload.len()
        );
        if let Some(who) = who {
            // Read the fixture credential per request: a later `Shared::new`
            // on this armed state re-arms and mints a replacement token
            // (`Shared::new` uses `arm_if_requested`), so a cached token would
            // fail the seam before the intended board guard.
            let seam = crate::test_seam::Seam::token_at(&self.state)
                .expect("the fixture state lost its seam token");
            head.push_str(&format!(
                "{}: {who}\r\n{}: {seam}\r\n",
                crate::test_seam::AS_HEADER,
                crate::test_seam::TOKEN_HEADER,
            ));
        }
        if let Some((cookie, key)) = session {
            head.push_str(&format!("Cookie: {cookie}\r\nX-Cadence-Session: {key}\r\n"));
        }
        head.push_str("\r\n");
        let mut socket = std::net::TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        std::io::Write::write_all(&mut socket, head.as_bytes()).unwrap();
        std::io::Write::write_all(&mut socket, &payload).unwrap();
        let mut response = Vec::new();
        std::io::Read::read_to_end(&mut socket, &mut response).unwrap();
        let split = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("HTTP response has no header/body split");
        let header = std::str::from_utf8(&response[..split]).unwrap();
        let mut lines = header.lines();
        let status: u16 = lines
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let headers: Vec<(String, String)> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
            .collect();
        let body = serde_json::from_slice(&response[split + 4..]).unwrap_or(Value::Null);
        (status, headers, body)
    }

    fn session(&self) -> (String, String) {
        crate::operator_auth::ensure_secret(&self.state).unwrap();
        let secret = crate::operator_auth::read_secret(&self.state).unwrap();
        let minted = self
            .rpc(
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )
            .unwrap_or_else(|e| panic!("operator_link_mint: {e}"));
        let nonce = minted["nonce"].as_str().unwrap().to_string();
        let (status, headers, body) = self.request(
            "POST",
            "/api/session",
            Some("operator"),
            None,
            &json!({"nonce": nonce}),
        );
        assert_eq!(status, 200, "session bootstrap refused: {body}");
        let cookie = headers
            .iter()
            .find(|(name, _)| name.as_str() == "set-cookie")
            .map(|(_, value)| value.split(';').next().unwrap().to_string())
            .unwrap_or_else(|| panic!("no session cookie: {headers:?}"));
        let key = body["session_key"]
            .as_str()
            .unwrap_or_else(|| panic!("no session key: {body}"))
            .to_string();
        assert!(!cookie.is_empty() && !key.is_empty());
        (cookie, key)
    }

    /// `POST /api/master/identity` under an optional session/proof. Returns
    /// (status, body) — the caller asserts the refusal and no effect.
    fn post_identity(
        &self,
        session: Option<&(String, String)>,
        who: Option<&str>,
        body: &Value,
    ) -> (u16, Value) {
        let (status, _, body) = self.request("POST", "/api/master/identity", who, session, body);
        (status, body)
    }
}

impl Drop for Cad1817HttpRig {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for thread in self.threads.drain(..).rev() {
            let _ = thread.join();
        }
    }
}

#[test]
fn cad1817_master_identity_http_is_no_weaker_than_native() {
    let rig = Cad1817HttpRig::start();
    let shared = rig.store();
    let pm = cad1817_prepare_master(&shared, &rig.root.path().join("master-cwd"));
    let state_before = rig.rpc("master_state", json!({})).unwrap();
    let revision = state_before["identity"]["revision"]
        .as_str()
        .expect("master identity revision must be a string")
        .to_string();
    assert_eq!(state_before["identity"]["look"], json!("robot"));
    let files_before = cad1817_master_files(&pm.dir);
    let valid = json!({"expected_revision": revision, "name": "HTTP Workspace",
                       "tone": "calm", "look": "operations"});

    let unchanged = |rig: &Cad1817HttpRig| {
        assert_eq!(
            rig.rpc("master_state", json!({})).unwrap(),
            state_before,
            "a refused HTTP identity write changed master state"
        );
        assert_eq!(
            cad1817_master_files(&pm.dir),
            files_before,
            "a refused HTTP identity write changed protected files"
        );
    };

    // Agent/peer caller with no session: the operator-only peer guard refuses
    // before the daemon is asked.
    let (status, body) = rig.post_identity(None, Some("agent:cad1817-http-agent"), &valid);
    assert!(
        (400u16..500).contains(&status),
        "an agent HTTP identity write was not refused: {status} {body}"
    );
    unchanged(&rig);

    // Provably no-one is not the operator.
    let (status, body) = rig.post_identity(None, Some("unproven"), &valid);
    assert!(
        (400u16..500).contains(&status),
        "an unproven HTTP identity write was not refused: {status} {body}"
    );
    unchanged(&rig);

    // A stolen/live operator session presented BY an agent is refused and
    // revoked, exactly like the native session rule.
    let stolen = rig.session();
    let (status, body) = rig.post_identity(Some(&stolen), Some("agent:cad1817-http-agent"), &valid);
    assert!(
        (400u16..500).contains(&status),
        "an agent presenting an operator session was not refused: {status} {body}"
    );
    unchanged(&rig);

    // Forged actor and unknown field with a legitimate operator session.
    let session = rig.session();
    for (case, forged) in [
        (
            "forged actor",
            json!({"expected_revision": revision, "name": "X", "by": "owner"}),
        ),
        (
            "unknown field",
            json!({"expected_revision": revision, "name": "X", "agent": "someone"}),
        ),
        (
            "stale revision",
            json!({"expected_revision": format!("{revision}-stale"), "name": "X"}),
        ),
        (
            "unknown look",
            json!({"expected_revision": revision, "look": "dragon"}),
        ),
    ] {
        let (status, body) = rig.post_identity(Some(&session), Some("operator"), &forged);
        assert!(
            (400u16..500).contains(&status),
            "{case} HTTP identity write was not refused: {status} {body}"
        );
        unchanged(&rig);
    }

    // Positive control: the same real operator session + proof is admitted
    // and the HTTP peer reaches the same protected write as the native verb.
    let (status, body) = rig.post_identity(Some(&session), Some("operator"), &valid);
    assert_eq!(status, 200, "operator HTTP identity write refused: {body}");
    assert_eq!(body["identity"]["name"], json!("HTTP Workspace"));
    assert_eq!(body["identity"]["tone"], json!("calm"));
    assert_eq!(body["identity"]["look"], json!("operations"));
    assert_ne!(
        body["identity"]["revision"].as_str(),
        Some(revision.as_str()),
        "the HTTP identity revision did not advance"
    );
    let after = rig.rpc("master_state", json!({})).unwrap();
    assert_eq!(after["alias"], json!(crate::master::ALIAS));
    assert_eq!(after["identity"], body["identity"]);
    let files_after = cad1817_master_files(&pm.dir);
    assert!(
        cad1817_master_text(&files_after, "IDENTITY.md").contains("HTTP Workspace"),
        "HTTP identity write did not reach IDENTITY.md"
    );
    assert!(
        cad1817_master_text(&files_after, "SOUL.md").contains("calm"),
        "HTTP identity write did not reach SOUL.md"
    );
    assert_eq!(
        files_after["AGENT.md"], files_before["AGENT.md"],
        "an HTTP identity write changed AGENT.md"
    );

    // Avatar-only boundary over the real HTTP peer: the look-only mutation
    // must advance the public revision, and the pre-look revision must then
    // be refused so a stale browser cannot overwrite the newer avatar.
    let rev_before_look = body["identity"]["revision"]
        .as_str()
        .expect("master identity revision must be a string")
        .to_string();
    let files_before_look = files_after.clone();
    let (status, look_out) = rig.post_identity(
        Some(&session),
        Some("operator"),
        &json!({"expected_revision": rev_before_look, "look": "tester"}),
    );
    assert_eq!(
        status, 200,
        "look-only HTTP identity write refused: {look_out}"
    );
    assert_eq!(look_out["identity"]["look"], json!("tester"));
    assert_eq!(look_out["applies"], json!("next_turn"));
    let rev_after_look = look_out["identity"]["revision"]
        .as_str()
        .expect("master identity revision must be a string")
        .to_string();
    assert_ne!(
        rev_after_look, rev_before_look,
        "a look-only HTTP mutation left the public identity revision unchanged — \n         a stale browser could overwrite a newer avatar"
    );
    let state_after_look = rig.rpc("master_state", json!({})).unwrap();
    assert_eq!(state_after_look["identity"], look_out["identity"]);
    assert_eq!(cad1817_master_files(&pm.dir), files_before_look);
    assert_eq!(
        cad1817_owning_look(&shared, crate::master::ALIAS).as_deref(),
        Some("tester"),
        "the look-only HTTP write did not persist the owning pin"
    );

    let (status, stale_avatar) = rig.post_identity(
        Some(&session),
        Some("operator"),
        &json!({"expected_revision": rev_before_look, "look": "guard",
               "name": "Stale avatar"}),
    );
    assert!(
        (400u16..500).contains(&status),
        "an old HTTP revision overwrote a newer avatar: {status} {stale_avatar}"
    );
    assert_eq!(
        rig.rpc("master_state", json!({})).unwrap(),
        state_after_look,
        "a refused stale avatar overwrite changed master state"
    );
    assert_eq!(
        cad1817_master_files(&pm.dir),
        files_before_look,
        "a refused stale avatar overwrite changed protected files"
    );
    assert_eq!(
        cad1817_owning_look(&shared, crate::master::ALIAS).as_deref(),
        Some("tester"),
        "a refused stale avatar overwrite changed the owning pin"
    );
}
