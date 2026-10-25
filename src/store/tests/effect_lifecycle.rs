// Drafted retained adversarial checks for the CAD-1128 encrypted Store
// lifecycle — UNCOMPILED/UNEXECUTED. Mount as a new file
// `src/store/tests/effect_lifecycle.rs` and add `mod effect_lifecycle;` to
// `src/store/tests/mod.rs`. `use super::*` supplies `store()`, `reg`,
// `seeded_task`, `TempDir`, `Store`, `params`, `json`, `Connection`,
// `OptionalExtension`, `Uuid`, and every `crate::store::*` type the
// submodule reaches.
//
// Each check drives the REAL typed Store methods against a real sqlite
// store — no daemon, socket, key, vault or fake state machine. Every
// counterexample pairs a working happy setup with a concrete wrong-result
// assertion: the guard it names is the only thing standing between the
// seeded state and a wrong claim/closure/residue. No test asserts red
// from a setup or compile failure.
//
// Surfaces under test:
//   Store::effect_executing(effect_id, expected)   — src/store/effects.rs
//   Store::platform_revoke_encrypted(...)          — src/store/platform.rs
//   Store::platform_revoke_inner close loop        — src/store/platform.rs
//   respond_effect / screen_reason_or_refuse       — src/daemon/effect_rpc.rs
//     (the store-level residue assertions; the daemon helper is reached
//      via its typed inputs in the dedicated reason check)

use super::*;

// ---------- fixture builders ----------

/// A legacy (non-encrypted) credential record: `custody:"file"` so the
/// unsupported-custody policy does NOT mask the execution-claim guard —
/// these are eligible legacy targets, exactly the wrong-claim surface R2
/// names.
fn legacy_record(platform: &str, account: &str, conn: &str, rev: u64) -> CredentialRecord {
    CredentialRecord {
        connection_id: conn.to_string(),
        credential_revision: rev,
        platform: platform.to_string(),
        account: account.to_string(),
        scopes: vec!["widgets:write".to_string()],
        fingerprint: format!("fp-{account}"),
        custody: "file".to_string(),
        exchange: "token".to_string(),
        enrolled_at: 0.0,
        by: "operator".to_string(),
    }
}

/// Build a staged `waiting` EffectRow against `(platform, account)` via the
/// real `effect_stage` path (authorization_kind defaults to `agent_grant`).
fn stage_row(eff: &str, platform: &str, account: &str) -> EffectRow {
    EffectRow {
        effect_id: eff.to_string(),
        request: format!("req-{eff}"),
        agent: "w1".to_string(),
        platform: platform.to_string(),
        account: account.to_string(),
        tool: "widgets.publish".to_string(),
        label: None,
        input: json!({"body": "hello"}),
        input_summary: "post hello".to_string(),
        preview: "publish".to_string(),
        source_name: None,
        source_hash: None,
        scopes: vec!["widgets:write".to_string()],
        task: None,
        state: "waiting".to_string(),
        close_reason: None,
        decision: None,
        outcome: None,
        needs_you: false,
        staged_at: 0.0,
        updated_at: 0.0,
    }
}

/// Stage then operator-accept → the durable `decided` row (the clone the
/// executor holds before it claims).
fn decided_row(s: &Store, eff: &str, platform: &str, account: &str) -> EffectRow {
    let row = stage_row(eff, platform, account);
    s.effect_stage(&row).unwrap();
    let decision = json!({"by": {"member":"operator","role":"operator","rule":"operator-only"},
                        "at":"t"});
    s.effect_decide(&row.request, true, &decision)
        .unwrap()
        .unwrap()
}

fn effect_state(s: &Store, eff: &str) -> String {
    s.fixture_write(|conn| {
        Ok(conn.query_row(
            "SELECT state FROM platform_effects WHERE effect_id=?1",
            params![eff],
            |r| r.get::<_, String>(0),
        )?)
    })
    .unwrap()
}

fn credential_present(s: &Store, platform: &str, account: &str) -> bool {
    s.platform_credential(platform, account).unwrap().is_some()
}

// =====================================================================
// 1. Exact-row execution claim: stale / foreign / zero-row refuse on an
//    ELIGIBLE LEGACY target — the unsupported_custody policy is not what
//    stops these, so the CAS is the named guard.
// =====================================================================

/// A `decided` row claimed with a structurally valid but STALE clone
/// (one whose fields no longer match the durable row) must refuse
/// `effect_not_claimable` — never execute under another row's identity.
/// Mutation point: dropping the immutable-field rebind in
/// `effect_executing` lets a clone supply alternate routing/input and
/// passes this test red.
#[test]
fn execution_claim_refuses_stale_clone_on_legacy_target() {
    let (dir, s) = store();
    let cwd = dir.path().join("w");
    reg(&s, "w1", &cwd);
    s.platform_enroll(&legacy_record("fixture", "acct", "conn-1", 1), false, None)
        .unwrap();
    let row = decided_row(&s, "eff-1", "fixture", "acct");
    assert_eq!(row.state, "decided");

    // A stale clone: same effect_id, mutated routing field. The CAS must
    // catch the divergence and refuse.
    let mut stale = row.clone();
    stale.input = json!({"body": "MUTATED-ALTERNATE-INPUT"});
    let err = s.effect_executing("eff-1", &stale).unwrap_err();
    assert_eq!(err.code(), Some("effect_not_claimable"));
    // The row is still `decided`, not executing — no claim happened.
    assert_eq!(effect_state(&s, "eff-1"), "decided");
}

/// A foreign clone — a different request's EffectRow presented against
/// `eff-1`'s id — must refuse. The clone is a routing hint, never a
/// receipt.
#[test]
fn execution_claim_refuses_foreign_row_identity() {
    let (dir, s) = store();
    let cwd = dir.path().join("w");
    reg(&s, "w1", &cwd);
    s.platform_enroll(&legacy_record("fixture", "acct", "conn-1", 1), false, None)
        .unwrap();
    let real = decided_row(&s, "eff-1", "fixture", "acct");
    // A foreign row: same id, different request/agent/tool.
    let mut foreign = real.clone();
    foreign.request = "req-OTHER".to_string();
    foreign.tool = "widgets.delete".to_string();
    let err = s.effect_executing("eff-1", &foreign).unwrap_err();
    assert_eq!(err.code(), Some("effect_not_claimable"));
    assert_eq!(effect_state(&s, "eff-1"), "decided");
}

/// Zero-row claim: a `waiting`/`closed`/non-`decided` row cannot transition
/// to `executing` — the `n != 1` guard. Mutation point: removing the row
/// count check lets a closed or never-decided row claim execution and the
/// platform call fire.
#[test]
fn execution_claim_requires_exactly_one_decided_row() {
    let (dir, s) = store();
    let cwd = dir.path().join("w");
    reg(&s, "w1", &cwd);
    s.platform_enroll(&legacy_record("fixture", "acct", "conn-1", 1), false, None)
        .unwrap();

    // waiting (never decided): the conditional UPDATE matches 0 rows.
    let waiting = stage_row("eff-wait", "fixture", "acct");
    s.effect_stage(&waiting).unwrap();
    let err = s.effect_executing("eff-wait", &waiting).unwrap_err();
    assert_eq!(err.code(), Some("effect_not_claimable"));
    assert_eq!(effect_state(&s, "eff-wait"), "waiting");

    // closed (revoke already won): a decided row closed by the credential
    // revoke cannot then be claimed — this is the R2 linearization.
    let decided = decided_row(&s, "eff-cl", "fixture", "acct");
    // Close it via the encrypted revoke's waiting/decided closure.
    s.platform_revoke_encrypted("fixture", "acct", "operator", "conn-1", 1)
        .unwrap();
    assert_eq!(effect_state(&s, "eff-cl"), "closed");
    let err = s.effect_executing("eff-cl", &decided).unwrap_err();
    assert_eq!(err.code(), Some("effect_not_claimable"));
}

/// The positive control: an unchanged `decided` clone DOES claim — proves
/// the guard admits the legitimate transition so the refusals above are
/// guard-specific, not a broken fixture.
#[test]
fn execution_claim_accepts_matching_decided_clone() {
    let (dir, s) = store();
    let cwd = dir.path().join("w");
    reg(&s, "w1", &cwd);
    s.platform_enroll(&legacy_record("fixture", "acct", "conn-1", 1), false, None)
        .unwrap();
    let row = decided_row(&s, "eff-ok", "fixture", "acct");
    let claimed = s.effect_executing("eff-ok", &row).unwrap();
    assert_eq!(claimed.state, "executing");
    assert_eq!(claimed.effect_id, "eff-ok");
}

// =====================================================================
// 2. At-most-one same-revision revoke/rotate winner.
// =====================================================================

/// Two `platform_revoke_encrypted` calls carrying the SAME
/// expected (connection_id, revision): the first commits and retires the
/// record; the second sees `None`/absent and cannot re-publish. Mutation
/// point: dropping the expected-CAS lets a stale caller retire a record a
/// concurrent winner already retired — or worse, a re-enrolled
/// incarnation.
#[test]
fn encrypted_revoke_at_most_one_same_revision_winner() {
    let (_dir, s) = store();
    s.platform_enroll(&legacy_record("fixture", "acct", "conn-1", 1), false, None)
        .unwrap();
    assert!(credential_present(&s, "fixture", "acct"));

    let first = s
        .platform_revoke_encrypted("fixture", "acct", "operator", "conn-1", 1)
        .unwrap();
    assert!(first.is_some(), "first revoke must retire the record");
    assert!(!credential_present(&s, "fixture", "acct"));

    // Second caller with the same expectation sees the record already
    // gone — `None`, not a second retirement.
    let second = s
        .platform_revoke_encrypted("fixture", "acct", "operator", "conn-1", 1)
        .unwrap();
    assert!(
        second.is_none(),
        "stale same-revision revoke must not re-retire"
    );
}

/// A stale `expected_revision` (the caller read an older `connection_show`)
/// against a rotated-forward record refuses `stale_revision` and retires
/// NOTHING. Mutation point: skipping the expectation check lets an
/// out-of-date caller revoke the newer incarnation.
#[test]
fn encrypted_revoke_refuses_stale_expected_revision() {
    let (_dir, s) = store();
    // A fresh record enrolls at rev 1; a real rotate carries it to rev 2 —
    // `platform_enroll` refuses a NEW record at rev > 1, so build the
    // rev-2 state via the typed rotate path, not a forged initial row.
    s.platform_enroll(&legacy_record("fixture", "acct", "conn-1", 1), false, None)
        .unwrap();
    s.platform_enroll(&legacy_record("fixture", "acct", "conn-1", 2), true, None)
        .unwrap();
    let err = s
        .platform_revoke_encrypted("fixture", "acct", "operator", "conn-1", 1)
        .unwrap_err();
    assert_eq!(err.code(), Some("stale_revision"));
    assert!(
        credential_present(&s, "fixture", "acct"),
        "stale revoke must not retire"
    );
}

/// A stale `expected_connection_id` (old incarnation after re-enroll)
/// refuses `stale_revision` and retires nothing — the CAS binds the exact
/// incarnation, not just the account.
#[test]
fn encrypted_revoke_refuses_stale_connection_incarnation() {
    let (_dir, s) = store();
    // A re-enroll replaces the account's incarnation: enroll conn-old at
    // rev 1, REVOKE it (the durable delete), then enroll conn-new fresh at
    // rev 1. The source guard refuses a connection_id change on a plain
    // rotate, so the real path is revoke-then-reenroll — the CAS must still
    // bind conn-old out.
    s.platform_enroll(
        &legacy_record("fixture", "acct", "conn-old", 1),
        false,
        None,
    )
    .unwrap();
    let _ = s
        .platform_revoke_encrypted("fixture", "acct", "operator", "conn-old", 1)
        .unwrap();
    s.platform_enroll(
        &legacy_record("fixture", "acct", "conn-new", 1),
        false,
        None,
    )
    .unwrap();
    let err = s
        .platform_revoke_encrypted("fixture", "acct", "operator", "conn-old", 1)
        .unwrap_err();
    assert_eq!(err.code(), Some("stale_revision"));
    assert!(credential_present(&s, "fixture", "acct"));
}

// =====================================================================
// 3. Atomic waiting/decided closure preserving executing/reconcile, plus
//    grants/default/approvals retirement in the same transaction.
// =====================================================================

/// Revoke must close ONLY `waiting`/`decided` generic rows with
/// `credential_revoked`, and preserve `executing`/`reconcile` uncertainty —
/// ciphertext cleanup never cancels a possibly transmitted effect.
/// Mutation point: closing reconcile/executing (the OLD
/// `close_credential_effects` behavior) would flip them to `closed` and
/// fail the preserve assertion.
#[test]
fn encrypted_revoke_closes_only_unexecuted_preserving_uncertain() {
    let (dir, s) = store();
    let cwd = dir.path().join("w");
    reg(&s, "w1", &cwd);
    s.platform_enroll(&legacy_record("fixture", "acct", "conn-1", 1), false, None)
        .unwrap();

    let waiting = stage_row("eff-w", "fixture", "acct");
    s.effect_stage(&waiting).unwrap();
    let _decided = decided_row(&s, "eff-d", "fixture", "acct");

    // executing: claim it for real so it is genuinely in-flight.
    let exec = decided_row(&s, "eff-x", "fixture", "acct");
    s.effect_executing("eff-x", &exec).unwrap();
    assert_eq!(effect_state(&s, "eff-x"), "executing");

    // reconcile: force a durable reconcile row (the uncertain survivor).
    s.fixture_write(|conn| {
        conn.execute(
            "UPDATE platform_effects SET state='reconcile' WHERE effect_id='eff-x'",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    assert_eq!(effect_state(&s, "eff-x"), "reconcile");

    let (_, _, closed) = s
        .platform_revoke_encrypted("fixture", "acct", "operator", "conn-1", 1)
        .unwrap()
        .unwrap();
    assert!(closed.contains(&"eff-w".to_string()));
    assert!(closed.contains(&"eff-d".to_string()));
    assert!(!closed.contains(&"eff-x".to_string()));
    assert_eq!(effect_state(&s, "eff-w"), "closed");
    assert_eq!(effect_state(&s, "eff-d"), "closed");
    // executing/reconcile uncertainty is preserved — never falsely closed.
    assert_eq!(effect_state(&s, "eff-x"), "reconcile");
    assert!(!credential_present(&s, "fixture", "acct"));
}

/// Grants, the project default and the credential row retire atomically
/// with the revoke — a grant outliving its credential would silently
/// reactivate on re-enroll (the closed default).
#[test]
fn encrypted_revoke_retires_grants_default_and_record_atomically() {
    let (_dir, s) = store();
    s.platform_enroll(&legacy_record("fixture", "acct", "conn-1", 1), false, None)
        .unwrap();
    s.platform_grant_add(
        "keeper",
        "fixture",
        "acct",
        &["widgets:write".to_string()],
        "operator",
    )
    .unwrap();
    // A second grant on a DIFFERENT account must survive — the revoke is
    // account-scoped, not a blanket wipe.
    // A real project default bound to the account — via the typed
    // `platform_default_set(project, platform, account, by)` facade.
    s.platform_default_set("proj-1", "fixture", "acct", "operator")
        .unwrap();
    // A second grant on a DIFFERENT account must survive — the revoke is
    // account-scoped, not a blanket wipe.
    s.platform_enroll(&legacy_record("fixture", "other", "conn-9", 1), false, None)
        .unwrap();
    s.platform_grant_add(
        "keeper",
        "fixture",
        "other",
        &["widgets:read".to_string()],
        "operator",
    )
    .unwrap();
    s.platform_default_set("proj-2", "fixture", "other", "operator")
        .unwrap();

    s.platform_revoke_encrypted("fixture", "acct", "operator", "conn-1", 1)
        .unwrap();

    assert!(!credential_present(&s, "fixture", "acct"));
    // Grants bound to the revoked account are gone.
    let grants = s.platform_grants(Some("keeper")).unwrap();
    assert!(grants.iter().all(|g| g.account != "acct"));
    // The unrelated account's grant survives.
    assert!(grants.iter().any(|g| g.account == "other"));
    // platform_default(project, platform): the revoked account's default
    // row is gone for the project that pointed at it; the other project's
    // default is untouched.
    let gone = s.platform_default("proj-1", "fixture").unwrap();
    assert!(
        gone.is_none() || gone.unwrap().account != "acct",
        "revoked account must not remain a project default"
    );
    let kept = s.platform_default("proj-2", "fixture").unwrap();
    assert_eq!(kept.unwrap().account, "other");
}

/// A `decided` row closed by the revoke is provably unexecuted — the
/// paired half of R2: the in-transaction closure + the row-count CAS mean
/// a closed row can never reach `executing`. Re-attempting the claim after
/// the revoke commits must refuse.
#[test]
fn closed_decided_row_cannot_be_claimed_after_revoke() {
    let (dir, s) = store();
    let cwd = dir.path().join("w");
    reg(&s, "w1", &cwd);
    s.platform_enroll(&legacy_record("fixture", "acct", "conn-1", 1), false, None)
        .unwrap();
    let clone = decided_row(&s, "eff-rev", "fixture", "acct");

    // Revoke wins the race: closes the still-unexecuted decided row.
    s.platform_revoke_encrypted("fixture", "acct", "operator", "conn-1", 1)
        .unwrap();
    assert_eq!(effect_state(&s, "eff-rev"), "closed");

    // The stale clone's claim must now lose — zero `decided` rows match.
    let err = s.effect_executing("eff-rev", &clone).unwrap_err();
    assert_eq!(err.code(), Some("effect_not_claimable"));
    assert_eq!(effect_state(&s, "eff-rev"), "closed");
}

// =====================================================================
// 4. The reason boundary — CORE-only residue evidence.
//
// The unscreenable-reason REFUSAL (`reason_unscreenable`) is a daemon
// boundary — `rpc_platform_effect_close` → `screen_reason_or_refuse` → a
// refused write BEFORE any durable decision/close_reason/event. A core-only
// test cannot invoke it, so this file does not fake it; the real daemon-RPC
// refusal lives in the separate rig check (see integration note). What
// belongs here are the core guarantees the refusal protects:
// =====================================================================

/// An encrypted-tagged credential's revoke still retires authority AND
/// closes its unexecuted `waiting`/`decided` generic rows in one core
/// transaction — deauthorization never depends on decrypting custody. A
/// `reconcile` row (uncertain, possibly transmitted) is preserved.
#[test]
fn encrypted_revoke_retires_authority_without_custody_bytes() {
    let (dir, s) = store();
    let cwd = dir.path().join("w");
    reg(&s, "w1", &cwd);
    // An encrypted-tagged record — custody bytes are NOT loadable here (no
    // real vault), so the revoke exercises the core-authority path only.
    let mut rec = legacy_record("fixture", "acct", "conn-1", 1);
    rec.custody = crate::platform::encrypted_sqlite::TAG_PREFIX.to_string() + "deadbeef";
    s.platform_enroll(&rec, false, None).unwrap();

    let waiting = stage_row("eff-w", "fixture", "acct");
    s.effect_stage(&waiting).unwrap();
    let _decided = decided_row(&s, "eff-d", "fixture", "acct");
    let exec = decided_row(&s, "eff-x", "fixture", "acct");
    s.effect_executing("eff-x", &exec).unwrap();
    s.fixture_write(|conn| {
        conn.execute(
            "UPDATE platform_effects SET state='reconcile' WHERE effect_id='eff-x'",
            [],
        )?;
        Ok(())
    })
    .unwrap();

    let (_, _, closed) = s
        .platform_revoke_encrypted("fixture", "acct", "operator", "conn-1", 1)
        .unwrap()
        .unwrap();
    // waiting/decided provably-unexecuted rows closed credential_revoked;
    // reconcile stays uncertain — ciphertext cleanup never cancels it.
    assert!(closed.contains(&"eff-w".to_string()));
    assert!(closed.contains(&"eff-d".to_string()));
    assert!(!closed.contains(&"eff-x".to_string()));
    assert_eq!(effect_state(&s, "eff-x"), "reconcile");
    assert!(!credential_present(&s, "fixture", "acct"));
}

/// A `waiting` row closes with the fixed `credential_revoked`/`cancelled`
/// cause and carries NO caller-supplied text — the store's typed close
/// writes the given cause verbatim into `close_reason`, never a caller
/// string, so an omitted reason yields the fixed cause (the authorized
/// safe outcome). This is the core's fixed-cause guarantee the daemon's
//  screen relies on before it refuses caller text.
#[test]
fn fixed_cause_close_carries_no_caller_text() {
    let (dir, s) = store();
    let cwd = dir.path().join("w");
    reg(&s, "w1", &cwd);
    s.platform_enroll(&legacy_record("fixture", "acct", "conn-1", 1), false, None)
        .unwrap();
    let waiting = stage_row("eff-fx", "fixture", "acct");
    s.effect_stage(&waiting).unwrap();

    s.effect_close(EffectKey::Id("eff-fx".to_string()), "credential_revoked")
        .unwrap();
    let (state, close_reason): (String, Option<String>) = s
        .fixture_write(|conn| {
            conn.query_row(
                "SELECT state, close_reason FROM platform_effects WHERE effect_id='eff-fx'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(state, "closed");
    assert_eq!(close_reason.as_deref(), Some("credential_revoked"));
}
