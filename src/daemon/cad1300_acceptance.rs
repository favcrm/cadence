//! CAD-1300 independent acceptance checks — written by the check author
//! from the ticket and the PM decision of 2026-10-09, not by the
//! implementer (AGENTS.md "Gates and security work"). The implementer may
//! remove the `#[ignore]`s below and nothing else; the cases, their order
//! and their assertions are the contract.
//!
//! They reuse the CAD-1218 fixture (`cad1218_acceptance`: a real daemon
//! and board, a fake `gh`, a fixture AgenticOS platform) and follow its
//! rule: every refused case differs from an accepted one in exactly the
//! property under test, so only that guard refuses.
//!
//! | test | rule |
//! |---|---|
//! | `cad1300_publish_after_a_revoked_board_approval_is_refused` | item 4: one approval record per head on every board path; Publish reuses a standing one, refuses a revoked one; the CLI path is unchanged |
//! | `cad1300_publish_first_then_approve_is_refused` | item 4, converse: a head Publish recorded cannot be approved again |
//! | `cad1300_owner_with_a_long_actor_is_admitted` | item 3: every platform owner is an approver; the relayed actor is bounded, deterministic and attributable |
//! | `cad1300_owner_with_a_non_ascii_name_is_admitted` | item 3 (PM, 2026-10-09): an owner whose name has no ASCII characters is an approver too |
//! | `cad1300_one_record_per_head_whatever_the_repo_case` | item 4, Spec/security REVISE on PR #913: the one-record rule holds when the PR URL and the registered project spell the repo in different letter case |
//! | `cad1300_cli_records_in_any_case_count_as_on_record` | item 4, Spec/security REVISE r2 on PR #913: an approval the operator's CLI recorded in mixed case counts for the one-record rule |
//! | `cad1300_approver_refusal_is_403_and_no_repo_oracle` | item 7 (`rpc_err` 403 mapping on Approve) and item 9 (no registered-repo oracle) |
//! | `cad1300_reachable_second_layers` | item 7 survivors that can be reached: forged verb fields, `board_revocable`'s `recorded_via` term |
//!
//! The design note (approach, the refused cases, the actor abbreviation,
//! and what stays a manual probe) is in the CAD-1300 lane's
//! `design-note.md`; its contract is restated above each test.

use super::cad1218_acceptance::{
    delivery_state, merges, state_path, Board, APPROVE, COMPANY, HEAD, KEY_SEED, MALLORY,
    PUBLIC_HOST, PUBLISH_ISSUE, PUBLISH_ISSUE_2, PUBLISH_PR, PUBLISH_PR_2, REPO, STATE_VERB, VERB,
};
use crate::test_seam::{scoped, Asserted};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::signature::Ed25519KeyPair;
use serde_json::{json, Value};

/// The source limit the store applies to every approval record
/// (`approval_source`: 1-200 non-control characters).
const SOURCE_MAX: usize = 200;

fn publish_path(issue: &str) -> String {
    format!("/api/delivery/{issue}/merge")
}

fn publish_body() -> String {
    json!({"sha": HEAD}).to_string()
}

fn approve_pr(pr: u64) -> String {
    json!({"repo": REPO, "pr": pr, "head": HEAD}).to_string()
}

fn as_operator(board: &Board, method: &str, params: Value) -> crate::Result<Value> {
    scoped(Asserted::Operator, || {
        crate::client::rpc(&board.state, method, params)
    })
}

/// A public (AgenticOS) session through the daemon's real assertion
/// exchange, for any verified identity the platform may sign: `sub`,
/// `name`, `email` within the assertion's own limits (`sub` ≤ 200, email
/// non-empty ≤ 320), `role` `owner` or `member`. `jti` is single-use. A
/// new session for the same `sub` replaces the old one.
fn platform_session(
    board: &Board,
    sub: &str,
    name: &str,
    email: &str,
    role: &str,
    jti: &str,
) -> String {
    let key = Ed25519KeyPair::from_seed_unchecked(&KEY_SEED).unwrap();
    let now = crate::issue::time::now_epoch();
    let part = |v: Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(&v).unwrap());
    let signed = format!(
        "{}.{}",
        part(json!({"alg": "EdDSA", "typ": "JWT", "kid": "c1218"})),
        part(json!({
            "iss": board.issuer, "aud": PUBLIC_HOST, "sub": sub,
            "email": email, "name": name,
            "company": COMPANY, "role": role,
            "iat": now, "exp": now + 30, "jti": jti
        }))
    );
    let assertion = format!(
        "{signed}.{}",
        URL_SAFE_NO_PAD.encode(key.sign(signed.as_bytes()).as_ref())
    );
    let opened = as_operator(board, "board_session_open", json!({"assertion": assertion}))
        .expect("the fixture platform's assertion opens a session");
    opened["token"].as_str().unwrap().to_string()
}

/// Setup, not the rule under test: the session is live on the public host.
#[track_caller]
fn signed_in(board: &Board, token: &str, who: &str) {
    let (status, meta) = board.public("/api/meta", token, None);
    assert_eq!(
        (status, &meta["signed_in"]),
        (200, &json!(true)),
        "setup: {who}'s public session is not live: {meta}"
    );
}

// ---------------------------------------------------------------------
// Item 4 (PM decision, 2026-10-09: refuse). One approval record per
// repo/PR/head, from every board path — Approve and Publish alike.
//
// Contract for a board-relayed Publish (`delivery_approve` WITH a
// `request_actor`):
// - the head has no approval record: record one and enqueue (as today);
// - the head has a STANDING merge approval (Approve, then Publish; or a
//   retry after a failed enqueue): enqueue against that record — no new
//   record, the reply's `approval_id` is the standing one;
// - the head's record was REVOKED (and nothing stands): refuse with a
//   plain reason naming the revoke; no record, no observation that moves
//   the row, no `gh pr merge`, the row stays `passed`.
// The CLI path (`delivery_approve` WITHOUT `request_actor`, i.e.
// `cadence delivery merge`) is unchanged: source `operator`, and it may
// re-approve a revoked head (a fresh `<id>-2`), as `cadence audit
// approve` may.
// ---------------------------------------------------------------------

#[test]
fn cad1300_publish_after_a_revoked_board_approval_is_refused() {
    let root = tempfile::Builder::new().prefix("c1300r").tempdir().unwrap();
    let board = Board::start(root.path());
    let operator = board.session();

    // --- positive control: Approve, then Publish the same standing
    // approval — enqueued against it, no second record ---
    let (status, body) = board.post(
        "operator",
        Some(&operator),
        APPROVE,
        &approve_pr(PUBLISH_PR),
    );
    assert_eq!(status, 200, "setup: Approve on the board: {body}");
    let standing = body["approval_id"].as_str().unwrap().to_string();
    let events = board.approval_events();
    let (status, body) = board.post(
        "operator",
        Some(&operator),
        &publish_path(PUBLISH_ISSUE),
        &publish_body(),
    );
    assert_eq!(
        status, 200,
        "Publish of a head with a standing board approval must enqueue: {body}"
    );
    assert_eq!(
        body["approval_id"],
        standing.as_str(),
        "Publish reuses the standing approval: {body}"
    );
    assert_eq!(
        board.approval_events(),
        events,
        "Publish of an approved head wrote a second approval record"
    );
    let seen = board.audit_pr(PUBLISH_PR, HEAD);
    assert_eq!(seen["state"], "in-force", "{seen}");
    assert_eq!(seen["approval_id"], standing.as_str(), "{seen}");
    assert!(merges(&board).contains(&format!("merge {PUBLISH_PR} ")));

    // --- the rule: Approve, revoke, then Publish the same head. Only the
    // revoke differs from the accepted pair above ---
    let (status, body) = board.post(
        "operator",
        Some(&operator),
        APPROVE,
        &approve_pr(PUBLISH_PR_2),
    );
    assert_eq!(status, 200, "setup: Approve on the board: {body}");
    let revoked = body["approval_id"].as_str().unwrap().to_string();
    let (status, body) = board.post(
        "operator",
        Some(&operator),
        &format!("/api/approvals/{revoked}/revoke"),
        &json!({"reason": "re-review"}).to_string(),
    );
    assert_eq!(status, 200, "setup: revoke on the board: {body}");
    assert_eq!(board.audit_pr(PUBLISH_PR_2, HEAD)["state"], "revoked");
    let events = board.approval_events();
    let (status, body) = board.post(
        "operator",
        Some(&operator),
        &publish_path(PUBLISH_ISSUE_2),
        &publish_body(),
    );
    assert!(
        (400..500).contains(&status),
        "Publish after a revoked board approval must be refused: {status} {body}"
    );
    assert!(
        body.to_string().to_ascii_lowercase().contains("revoked"),
        "the refusal says plainly that the head's approval was revoked: {body}"
    );
    assert_eq!(
        board.approval_events(),
        events,
        "a refused Publish wrote to the approval stream"
    );
    let seen = board.audit_pr(PUBLISH_PR_2, HEAD);
    assert_eq!(seen["state"], "revoked", "{seen}");
    assert_eq!(seen["approval_id"], revoked.as_str(), "{seen}");
    assert!(
        !merges(&board).contains(&format!("merge {PUBLISH_PR_2} ")),
        "a refused Publish enqueued a merge: {}",
        merges(&board)
    );
    assert_eq!(delivery_state(&board, PUBLISH_ISSUE_2), "passed");

    // The same refusal for a platform owner's Publish (the rule is per
    // head, not per actor).
    let owner = platform_session(
        &board,
        "usr_owner_r",
        "Revoke Owner",
        "revoke-owner@example.com",
        "owner",
        "c1300-r-owner",
    );
    signed_in(&board, &owner, "the owner");
    let (status, body) = board.public(
        &publish_path(PUBLISH_ISSUE_2),
        &owner,
        Some(&publish_body()),
    );
    assert!(
        (400..500).contains(&status),
        "an owner's Publish after the revoke must be refused too: {status} {body}"
    );
    assert_eq!(board.approval_events(), events);
    assert_eq!(board.audit_pr(PUBLISH_PR_2, HEAD)["state"], "revoked");
    assert!(!merges(&board).contains(&format!("merge {PUBLISH_PR_2} ")));
    assert_eq!(delivery_state(&board, PUBLISH_ISSUE_2), "passed");

    // --- the CLI path is unchanged: `cadence delivery merge` (no
    // request_actor) re-approves the revoked head with a fresh record ---
    let out = as_operator(
        &board,
        "delivery_approve",
        json!({"issue": PUBLISH_ISSUE_2, "sha": HEAD}),
    )
    .expect("the CLI's delivery merge still re-approves a revoked head");
    let fresh = out["approval_id"].as_str().unwrap_or_default().to_string();
    assert!(
        !fresh.is_empty() && fresh != revoked,
        "the CLI records a fresh approval id: {out}"
    );
    let seen = board.audit_pr(PUBLISH_PR_2, HEAD);
    assert_eq!(seen["state"], "in-force", "{seen}");
    assert_eq!(seen["approval_id"], fresh.as_str(), "{seen}");
    assert_eq!(seen["source"], "operator", "the CLI source stays: {seen}");
    assert_eq!(seen["recorded_via"], "operator-connection", "{seen}");
    assert!(merges(&board).contains(&format!("merge {PUBLISH_PR_2} ")));
}

#[test]
fn cad1300_publish_first_then_approve_is_refused() {
    let root = tempfile::Builder::new().prefix("c1300c").tempdir().unwrap();
    let board = Board::start(root.path());
    let operator = board.session();

    // The ordinary first Publish still records and enqueues.
    let (status, body) = board.post(
        "operator",
        Some(&operator),
        &publish_path(PUBLISH_ISSUE),
        &publish_body(),
    );
    assert_eq!(status, 200, "a first Publish records and enqueues: {body}");
    let id = body["approval_id"].as_str().unwrap().to_string();
    let seen = board.audit_pr(PUBLISH_PR, HEAD);
    assert_eq!(seen["state"], "in-force", "{seen}");
    assert_eq!(seen["approval_id"], id.as_str(), "{seen}");
    assert!(merges(&board).contains(&format!("merge {PUBLISH_PR} ")));

    // Approve of the head Publish recorded: already on record.
    let events = board.approval_events();
    let (status, body) = board.post(
        "operator",
        Some(&operator),
        APPROVE,
        &approve_pr(PUBLISH_PR),
    );
    assert!(
        (400..500).contains(&status),
        "Approve of a head Publish already recorded must be refused: {status} {body}"
    );
    assert_eq!(board.approval_events(), events, "a refused Approve wrote");
    assert_eq!(board.audit_pr(PUBLISH_PR, HEAD)["approval_id"], id.as_str());
    // ... and after that record is revoked, neither board path re-records.
    let (status, body) = board.post(
        "operator",
        Some(&operator),
        &format!("/api/approvals/{id}/revoke"),
        &json!({"reason": "re-review"}).to_string(),
    );
    assert_eq!(status, 200, "setup: revoke on the board: {body}");
    let events = board.approval_events();
    let (status, body) = board.post(
        "operator",
        Some(&operator),
        APPROVE,
        &approve_pr(PUBLISH_PR),
    );
    assert!((400..500).contains(&status), "{status} {body}");
    assert_eq!(board.approval_events(), events);
    assert_eq!(board.audit_pr(PUBLISH_PR, HEAD)["state"], "revoked");
}

// ---------------------------------------------------------------------
// Item 3. Every platform-verified owner is an approver with no config,
// whatever the length of their verified name and email (an assertion's
// email is non-empty and at most 320 characters; the name is unbounded).
//
// Contract:
// - The actor the board relays for a public session (`BoardUser::actor`,
//   the one string `board_caller` derives) keeps its shape
//   `<name> <<email>> (board)` and is at most 190 characters, so the
//   record's source `"<actor> via board"` fits the store's 200. When the
//   full actor would be longer it is abbreviated deterministically (the
//   same verified identity always yields the same actor) and stays
//   attributable: it begins with the name's start, keeps the email's
//   local part and its `@`, and two owners whose emails differ yield
//   different actors (e.g. a short digest of the full email in the
//   abbreviated part — see the design note).
// - The daemon's `(board)` shape rule stays: a malformed `(board)` actor
//   is refused at the verb even from an operator connection; a member is
//   refused at admission; an agent or unproven caller at the verb.
// ---------------------------------------------------------------------

/// The record's source for an admitted long-actor owner: bounded,
/// printable, board-revocable in shape, attributable.
#[track_caller]
fn bounded_owner_source(seen: &Value, name: &str, email: &str) -> String {
    let source = seen["source"].as_str().unwrap_or_default().to_string();
    let n = source.chars().count();
    assert!(
        (1..=SOURCE_MAX).contains(&n),
        "the source must be 1-{SOURCE_MAX} characters, is {n}: {seen}"
    );
    assert!(!source.chars().any(char::is_control), "{seen}");
    assert!(source.ends_with(" (board) via board"), "{seen}");
    assert!(
        source.starts_with(&name[..10]),
        "the source begins with the owner's name: {seen}"
    );
    let local = &email[..=email.find('@').unwrap()];
    assert!(
        source.contains(&format!("<{local}")),
        "the source keeps the email's local part and '@': {seen}"
    );
    source
}

#[test]
fn cad1300_owner_with_a_long_actor_is_admitted() {
    let root = tempfile::Builder::new().prefix("c1300o").tempdir().unwrap();
    let board = Board::start(root.path());

    // --- kept refusals first (each passes today and must keep passing) ---
    for actor in [
        "x (board)",
        "Platform Owner (board)",
        "Name <a@b>> (board)",
        "N <a@b> (board) via board",
        "Name <a@b> (BOARD)",
        "operator (ui) (board)",
        "x <a@b> (tailscale)",
    ] {
        let err = as_operator(
            &board,
            VERB,
            json!({"repo": REPO, "pr": 59, "head": HEAD, "request_actor": actor}),
        )
        .expect_err("a malformed (board) actor recorded an approval");
        assert_eq!(
            err.code(),
            Some("approver_not_allowed"),
            "'{actor}' must be refused by the approver rule: {err}"
        );
    }
    assert_eq!(board.audit_pr(59, HEAD)["state"], "missing");
    let member = platform_session(
        &board,
        "usr_member_long",
        &format!("Member {}", "M".repeat(150)),
        "member@example.com",
        "member",
        "c1300-member",
    );
    signed_in(&board, &member, "the member");
    let (status, reply) = board.public(APPROVE, &member, Some(&approve_pr(59)));
    assert_eq!(status, 403, "a member approves nothing: {reply}");
    assert_eq!(reply["check"], "member_role", "{reply}");
    assert_eq!(board.audit_pr(59, HEAD)["state"], "missing");

    // The owners. `long`: a 150-character name and a 255-character email
    // with a 64-character local part (rendered today: 251 characters).
    // `twin`: the same name and local part, an email differing only at
    // its end. `window`: rendered today at 195 characters — inside the
    // `request_actor` cap, outside the source cap once " via board" is
    // appended.
    let long_name = format!("Owner {}", "L".repeat(144));
    let local = format!("longowner{}", "o".repeat(55));
    let domain = format!("{}.{}.{}", "a".repeat(60), "b".repeat(60), "c".repeat(60));
    let long_email = format!("{local}@{domain}.example");
    let twin_email = format!("{local}@{domain}.exampld");
    let window_name = format!("Window {}", "W".repeat(93));
    let window_email = format!("{}@window.example", "w".repeat(69));
    assert_eq!(long_name.len(), 150);
    assert_eq!(long_email.len(), 255);
    assert_eq!(
        window_name.len() + window_email.len() + " <> (board)".len(),
        195
    );

    // The verbs refuse a direct dial even with a long (board) actor.
    let long_actor = format!("{long_name} <{long_email}> (board)");
    for who in [Asserted::Agent("worker".into()), Asserted::Unproven] {
        let text = scoped(who.clone(), || {
            crate::client::rpc(
                &board.state,
                VERB,
                json!({"repo": REPO, "pr": 59, "head": HEAD, "request_actor": long_actor}),
            )
        })
        .expect_err("a non-operator recorded by naming a (board) actor")
        .to_string();
        assert!(text.contains("operator action"), "{who:?}: {text}");
    }
    assert_eq!(board.audit_pr(59, HEAD)["state"], "missing");

    // --- the long owner: read, Approve, read, revoke, Publish ---
    let owner = platform_session(
        &board,
        "usr_owner_long",
        &long_name,
        &long_email,
        "owner",
        "c1300-long-1",
    );
    signed_in(&board, &owner, "the long-actor owner");
    let read = state_path(REPO, 60, HEAD);
    let (status, body) = board.public(&read, &owner, None);
    assert_eq!(status, 200, "a long-actor owner reads the state: {body}");
    assert_eq!(body["state"], "missing", "{body}");
    let (status, body) = board.public(APPROVE, &owner, Some(&approve_pr(60)));
    assert_eq!(status, 200, "a long-actor owner approves: {body}");
    let id = body["approval_id"].as_str().unwrap().to_string();
    let seen = board.audit_pr(60, HEAD);
    assert_eq!(seen["state"], "in-force", "{seen}");
    assert_eq!(seen["recorded_via"], "operator-connection", "{seen}");
    let long_source = bounded_owner_source(&seen, &long_name, &long_email);
    assert!(!long_source.contains("tailscale"), "{seen}");
    let (status, body) = board.public(&read, &owner, None);
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["state"], "in-force", "{body}");
    assert_eq!(body["board_revocable"], true, "{body}");
    let (status, body) = board.public(
        &format!("/api/approvals/{id}/revoke"),
        &owner,
        Some(&json!({"reason": "owner re-review"}).to_string()),
    );
    assert_eq!(status, 200, "the long-actor owner revokes: {body}");
    assert_eq!(board.audit_pr(60, HEAD)["state"], "revoked");
    let (status, body) = board.public(&publish_path(PUBLISH_ISSUE), &owner, Some(&publish_body()));
    assert_eq!(status, 200, "the long-actor owner Publishes: {body}");
    let seen = board.audit_pr(PUBLISH_PR, HEAD);
    assert_eq!(seen["state"], "in-force", "{seen}");
    assert_eq!(
        bounded_owner_source(&seen, &long_name, &long_email),
        long_source,
        "Approve and Publish attribute the same owner identically"
    );
    assert!(merges(&board).contains(&format!("merge {PUBLISH_PR} ")));

    // --- deterministic: a new session for the same verified identity ---
    let again = platform_session(
        &board,
        "usr_owner_long",
        &long_name,
        &long_email,
        "owner",
        "c1300-long-2",
    );
    signed_in(&board, &again, "the long-actor owner's second session");
    let (status, body) = board.public(APPROVE, &again, Some(&approve_pr(61)));
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        bounded_owner_source(&board.audit_pr(61, HEAD), &long_name, &long_email),
        long_source,
        "the abbreviation is deterministic"
    );

    // --- attributable: another owner differing only at the email's end ---
    let twin = platform_session(
        &board,
        "usr_owner_twin",
        &long_name,
        &twin_email,
        "owner",
        "c1300-twin",
    );
    signed_in(&board, &twin, "the twin owner");
    let (status, body) = board.public(APPROVE, &twin, Some(&approve_pr(62)));
    assert_eq!(status, 200, "{body}");
    let twin_source = bounded_owner_source(&board.audit_pr(62, HEAD), &long_name, &twin_email);
    assert_ne!(
        twin_source, long_source,
        "two owners with different emails must not share one attribution"
    );

    // --- the 191-200 window: Approve and Publish ---
    let window = platform_session(
        &board,
        "usr_owner_window",
        &window_name,
        &window_email,
        "owner",
        "c1300-window",
    );
    signed_in(&board, &window, "the window owner");
    let (status, body) = board.public(APPROVE, &window, Some(&approve_pr(63)));
    assert_eq!(status, 200, "a 195-character owner actor approves: {body}");
    bounded_owner_source(&board.audit_pr(63, HEAD), &window_name, &window_email);
    let (status, body) = board.public(
        &publish_path(PUBLISH_ISSUE_2),
        &window,
        Some(&publish_body()),
    );
    assert_eq!(status, 200, "a 195-character owner actor Publishes: {body}");
    bounded_owner_source(
        &board.audit_pr(PUBLISH_PR_2, HEAD),
        &window_name,
        &window_email,
    );
    assert!(merges(&board).contains(&format!("merge {PUBLISH_PR_2} ")));
}

// ---------------------------------------------------------------------
// Item 7 (the `rpc_err` 403 mapping on Approve) and item 9 (no
// registered-repo oracle).
//
// Contract:
// - The daemon's approver refusal (`approver_not_allowed`) reaches the
//   browser as `403 {"check": "approver_not_allowed"}` on Approve, state,
//   revoke and Publish. Over HTTP, the one actor the board admits and the
//   daemon refuses is a platform owner whose verified email has no `@`
//   (the `(board)` shape rule stays, deliberately).
// - For an actor that fails the approver rule, `approval_state` and
//   `approval_record_shown` answer a registered and an unregistered repo
//   identically — same code, same message, nothing about registration —
//   i.e. the approver rule runs before the repo lookup.
// ---------------------------------------------------------------------

#[test]
fn cad1300_approver_refusal_is_403_and_no_repo_oracle() {
    let root = tempfile::Builder::new().prefix("c1300m").tempdir().unwrap();
    let board = Board::start(root.path());
    let operator = board.session();
    let unregistered = "acme/not-a-project";

    // --- the 403 mapping over HTTP (passes today) ---
    let no_at = platform_session(
        &board,
        "usr_owner_noat",
        "No At Owner",
        "no-at-owner.example.com",
        "owner",
        "c1300-noat",
    );
    signed_in(&board, &no_at, "the owner without an '@'");
    let refused = |what: &str, (status, body): (u16, Value)| {
        assert_eq!(status, 403, "{what}: {body}");
        assert_eq!(body["check"], "approver_not_allowed", "{what}: {body}");
        assert!(body.get("state").is_none(), "{what}: {body}");
        body
    };
    refused(
        "Approve",
        board.public(APPROVE, &no_at, Some(&approve_pr(70))),
    );
    refused(
        "state",
        board.public(&state_path(REPO, 70, HEAD), &no_at, None),
    );
    refused(
        "revoke",
        board.public(
            "/api/approvals/merge-pr70-x/revoke",
            &no_at,
            Some(&json!({"reason": "r"}).to_string()),
        ),
    );
    refused(
        "Publish",
        board.public(&publish_path(PUBLISH_ISSUE), &no_at, Some(&publish_body())),
    );
    assert_eq!(board.audit_pr(70, HEAD)["state"], "missing");
    assert_eq!(board.audit_pr(PUBLISH_PR, HEAD)["state"], "missing");
    assert_eq!(merges(&board), "", "a refused Publish enqueued a merge");
    // Positive control for the same route and PR: the operator approves.
    let (status, body) = board.post("operator", Some(&operator), APPROVE, &approve_pr(70));
    assert_eq!(status, 200, "{body}");

    // --- no registered-repo oracle at the verbs (off-list tailnet login) ---
    let pm_yaml = board.state.join("pm").join("pm.yaml");
    let mut yaml = std::fs::read_to_string(&pm_yaml).unwrap();
    yaml.push_str("approvals:\n  tailnet_logins:\n    - chris@example.com\n");
    std::fs::write(&pm_yaml, yaml).unwrap();
    let events = board.approval_events();
    for method in [STATE_VERB, VERB] {
        let call = |repo: &str| {
            as_operator(
                &board,
                method,
                json!({"repo": repo, "pr": 70, "head": HEAD, "request_actor": MALLORY}),
            )
            .expect_err("an off-list login was answered")
        };
        let (known, unknown) = (call(REPO), call(unregistered));
        assert_eq!(
            unknown.code(),
            known.code(),
            "{method}: an off-list actor learns whether a repo is registered: \
             registered → {known}; unregistered → {unknown}"
        );
        assert_eq!(known.code(), Some("approver_not_allowed"), "{known}");
        assert_eq!(
            unknown.to_string(),
            known.to_string(),
            "{method}: the two refusals must be identical"
        );
        for text in [known.to_string(), unknown.to_string()] {
            assert!(
                !text.contains("registered") && !text.contains("in-force"),
                "{method}: the refusal reveals state: {text}"
            );
        }
    }
    // ... and over HTTP, for the owner the daemon refuses.
    for (what, path, body) in [
        ("state", state_path(unregistered, 70, HEAD), None),
        (
            "Approve",
            APPROVE.to_string(),
            Some(json!({"repo": unregistered, "pr": 70, "head": HEAD}).to_string()),
        ),
    ] {
        let known_path = path.replace(unregistered, REPO);
        let known_body = body.as_deref().map(|b| b.replace(unregistered, REPO));
        let known = board.public(&known_path, &no_at, known_body.as_deref());
        let unknown = board.public(&path, &no_at, body.as_deref());
        assert_eq!(
            unknown, known,
            "{what} over HTTP: registered and unregistered repos must answer alike"
        );
        refused(what, unknown);
    }
    assert_eq!(board.approval_events(), events, "a refusal wrote");
}

// ---------------------------------------------------------------------
// Item 7 survivors that a guard-only state can reach.
//
// - Forged fields at the verb (`approval_record_shown` from an operator
//   connection): item 6 lets the implementer drop the daemon's denylist
//   as redundant with the route's `deny_unknown_fields`, so the property
//   is pinned, not the mechanism: a forged field is refused (nothing
//   recorded) or ignored (the record carries only what the daemon
//   derives). It never lands.
// - `board_revocable`'s `recorded_via` term: no current writer records a
//   `merge` with another `recorded_via`, so the predicate itself is
//   checked on records that differ only in that field.
// ---------------------------------------------------------------------

#[test]
fn cad1300_reachable_second_layers() {
    // --- the predicate, one field at a time ---
    let board_record = json!({
        "action": "merge",
        "recorded_via": "operator-connection",
        "source": "operator (ui) via board",
    });
    assert!(crate::store::board_revocable(&board_record));
    for via in [json!("delegated:pm-d"), json!("agent:x"), Value::Null] {
        let mut other = board_record.clone();
        other["recorded_via"] = via.clone();
        assert!(
            !crate::store::board_revocable(&other),
            "a record recorded_via {via} is not the board's to revoke"
        );
    }

    // --- forged fields at the verb never land ---
    let root = tempfile::Builder::new().prefix("c1300f").tempdir().unwrap();
    let board = Board::start(root.path());
    for (pr, field, value) in [
        (80u64, "source", json!("chris in chat")),
        (81, "action", json!("deploy")),
        (82, "id", json!("merge-pr82-chosen")),
        (83, "recorded_via", json!("delegated:pm-d")),
        (84, "delegated", json!(true)),
        (85, "by", json!("chris")),
    ] {
        let mut params =
            json!({"repo": REPO, "pr": pr, "head": HEAD, "request_actor": "operator (ui)"});
        params[field] = value.clone();
        let out = as_operator(&board, VERB, params);
        let seen = board.audit_pr(pr, HEAD);
        match out {
            Err(_) => assert_eq!(
                seen["state"], "missing",
                "a refused forged '{field}' recorded: {seen}"
            ),
            Ok(out) => {
                assert_eq!(seen["state"], "in-force", "forged '{field}': {out} {seen}");
                assert_eq!(seen["source"], "operator (ui) via board", "{field}: {seen}");
                assert_eq!(
                    seen["recorded_via"], "operator-connection",
                    "{field}: {seen}"
                );
                assert_eq!(
                    seen["approval_id"],
                    crate::store::default_approval_id("merge", pr, HEAD).as_str(),
                    "{field}: {seen}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------
// Item 3, PM decision of 2026-10-09: ALL platform-verified owners may
// approve with no config — including an owner whose verified name has no
// ASCII characters (陳大文).
//
// Why it is refused today: `BoardUser::actor` keeps printable ASCII only,
// so the name cleans to "" and the actor falls back to `<handle> (board)`
// (`handle_of(sub, email)`), which drops the email. The daemon's `(board)`
// shape rule (`platform_owner_actor`) needs `<name> <<email>> (board)`, so
// `approver_source` refuses it: 403 `approver_not_allowed` on every route.
//
// Contract:
// - The relayed actor for such an owner keeps the `(board)` shape with a
//   non-empty name part and the verified email: `<name> <<email>> (board)`.
//   The name part is deterministic and stays within the store's source
//   rule (1-200 bytes, no control characters, `"<actor> via board"`). The
//   recommended rendering keeps `actor()` printable-ASCII (it also feeds
//   commit `Actor:` trailers): when the cleaned name is empty, use the
//   session's `handle` as the name part — `usr_chan <dawen.chan@example.hk>
//   (board)`. The check pins the properties, not that string.
// - Attributable: the source keeps `<local@` of the email, and two owners
//   with the same non-ASCII name but different emails get different
//   sources. Deterministic: a second session for the same identity
//   yields the same source.
// - The daemon's shape rule is NOT relaxed: the bare `<handle> (board)`
//   that today's board relays, a bare non-ASCII `陳大文 (board)`, and the
//   other malformed shapes stay refused at the verb; a member with a
//   non-ASCII name is still refused at admission.
// ---------------------------------------------------------------------

/// The record's source for an admitted non-ASCII-named owner.
#[track_caller]
fn non_ascii_owner_source(seen: &Value, email: &str) -> String {
    let source = seen["source"].as_str().unwrap_or_default().to_string();
    assert!(
        (1..=SOURCE_MAX).contains(&source.len()),
        "the source must be 1-{SOURCE_MAX} bytes (the store's rule), is {}: {seen}",
        source.len()
    );
    assert!(!source.chars().any(char::is_control), "{seen}");
    assert!(
        source.ends_with(&format!("<{email}> (board) via board")),
        "{seen}"
    );
    let name_part = source
        .strip_suffix(&format!(" <{email}> (board) via board"))
        .unwrap_or_default();
    assert!(
        !name_part.trim().is_empty(),
        "the actor keeps a non-empty name part: {seen}"
    );
    source
}

#[test]
fn cad1300_owner_with_a_non_ascii_name_is_admitted() {
    let root = tempfile::Builder::new().prefix("c1300u").tempdir().unwrap();
    let board = Board::start(root.path());
    let name = "\u{9673}\u{5927}\u{6587}"; // 陳大文
    let email = "dawen.chan@example.hk";
    let twin_email = "dawen.chan2@example.hk";

    // --- kept refusals first (each passes today and must keep passing) ---
    for actor in [
        "usr_chan (board)".to_string(),
        format!("{name} (board)"),
        "x (board)".to_string(),
        format!("{name} <{email}>> (board)"),
        format!("{name} <{email}> (board) via board"),
        format!("{name} <{email}> (BOARD)"),
        format!("<{email}> (board)"),
    ] {
        let err = as_operator(
            &board,
            VERB,
            json!({"repo": REPO, "pr": 89, "head": HEAD, "request_actor": actor}),
        )
        .expect_err("a malformed (board) actor recorded an approval");
        assert_eq!(
            err.code(),
            Some("approver_not_allowed"),
            "'{actor}' must be refused by the approver rule: {err}"
        );
    }
    assert_eq!(board.audit_pr(89, HEAD)["state"], "missing");
    let member = platform_session(
        &board,
        "usr_chan_member",
        name,
        "member.chan@example.hk",
        "member",
        "c1300-u-member",
    );
    signed_in(&board, &member, "the non-ASCII-named member");
    for (path, body) in [
        (APPROVE.to_string(), Some(approve_pr(89))),
        (state_path(REPO, 89, HEAD), None),
        (publish_path(PUBLISH_ISSUE), Some(publish_body())),
    ] {
        let (status, reply) = board.public(&path, &member, body.as_deref());
        assert_eq!(status, 403, "a member on {path}: {reply}");
        assert_eq!(reply["check"], "member_role", "a member on {path}: {reply}");
    }
    assert_eq!(board.audit_pr(89, HEAD)["state"], "missing");
    assert_eq!(merges(&board), "", "a member's Publish enqueued a merge");

    // --- the owner: read, Approve, read, revoke, Publish ---
    let owner = platform_session(&board, "usr_chan", name, email, "owner", "c1300-u-1");
    signed_in(&board, &owner, "the non-ASCII-named owner");
    let read = state_path(REPO, 90, HEAD);
    let (status, body) = board.public(&read, &owner, None);
    assert_eq!(
        status, 200,
        "a non-ASCII-named owner reads the state: {body}"
    );
    assert_eq!(body["state"], "missing", "{body}");
    let (status, body) = board.public(APPROVE, &owner, Some(&approve_pr(90)));
    assert_eq!(status, 200, "a non-ASCII-named owner approves: {body}");
    let id = body["approval_id"].as_str().unwrap().to_string();
    let seen = board.audit_pr(90, HEAD);
    assert_eq!(seen["state"], "in-force", "{seen}");
    assert_eq!(seen["recorded_via"], "operator-connection", "{seen}");
    let source = non_ascii_owner_source(&seen, email);
    assert!(!source.contains("tailscale"), "{seen}");
    let (status, body) = board.public(&read, &owner, None);
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["state"], "in-force", "{body}");
    assert_eq!(body["board_revocable"], true, "{body}");
    let (status, body) = board.public(
        &format!("/api/approvals/{id}/revoke"),
        &owner,
        Some(&json!({"reason": "owner re-review"}).to_string()),
    );
    assert_eq!(status, 200, "the non-ASCII-named owner revokes: {body}");
    assert_eq!(board.audit_pr(90, HEAD)["state"], "revoked");
    let (status, body) = board.public(&publish_path(PUBLISH_ISSUE), &owner, Some(&publish_body()));
    assert_eq!(status, 200, "the non-ASCII-named owner Publishes: {body}");
    let seen = board.audit_pr(PUBLISH_PR, HEAD);
    assert_eq!(seen["state"], "in-force", "{seen}");
    assert_eq!(
        non_ascii_owner_source(&seen, email),
        source,
        "Approve and Publish attribute the same owner identically"
    );
    assert!(merges(&board).contains(&format!("merge {PUBLISH_PR} ")));

    // --- deterministic: a new session for the same verified identity ---
    let again = platform_session(&board, "usr_chan", name, email, "owner", "c1300-u-2");
    signed_in(&board, &again, "the owner's second session");
    let (status, body) = board.public(APPROVE, &again, Some(&approve_pr(91)));
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        non_ascii_owner_source(&board.audit_pr(91, HEAD), email),
        source,
        "the rendering is deterministic"
    );

    // --- attributable: another owner with the same name, another email ---
    let twin = platform_session(
        &board,
        "usr_chan_two",
        name,
        twin_email,
        "owner",
        "c1300-u-twin",
    );
    signed_in(&board, &twin, "the same-named owner");
    let (status, body) = board.public(APPROVE, &twin, Some(&approve_pr(92)));
    assert_eq!(status, 200, "{body}");
    let twin_source = non_ascii_owner_source(&board.audit_pr(92, HEAD), twin_email);
    assert_ne!(
        twin_source, source,
        "two same-named owners must not share one attribution"
    );
}

// ---------------------------------------------------------------------
// Item 4, Spec/security REVISE on PR #913 (head 60c54b3c): one approval
// record per head must hold whatever letter case the repo is spelled in.
// GitHub owner/repo names are case-insensitive; Approve takes the repo of
// the registered project (lowercased), while Publish takes it from the
// delivery row's PR URL as the worker reported it (GitHub prints the
// canonical case, e.g. `Acme/Widgets`). Matching the stored scope by
// exact JSON let a revoked head be re-approved by Publish and let one
// head carry two records.
//
// Contract (properties, not the mechanism — lowercasing at record time
// and a case-insensitive compare both satisfy it):
// - With the row URL `Acme/Widgets` and the project `acme/widgets` (and
//   the reverse), the board's Approve and Publish share exactly ONE
//   record per head, in either order: Approve → revoke → Publish is
//   refused ("revoked", nothing recorded, no `gh pr merge`, the row stays
//   `passed`); Approve → Publish reuses the standing record with no new
//   event; Publish → Approve is refused as already on record.
// - Every board-recorded approval of the repo stores the same canonical
//   lowercase `scope.repo`, on both paths.
// - `gh pr merge` still names the row's own slug (`-R Acme/Widgets`).
// - The CLI path (`delivery_approve` without `request_actor`) is
//   unchanged: it still records (a fresh id after a revoke), source
//   `operator`, and enqueues.
// ---------------------------------------------------------------------

/// `scope.repo` of every recorded approval for `pr`, oldest first.
fn recorded_repos(board: &Board, pr: u64) -> Vec<String> {
    let db = rusqlite::Connection::open_with_flags(
        board.state.join("cadence.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut stmt = db
        .prepare(
            "SELECT payload FROM events WHERE alias = 'audit:approvals' \
             AND kind = 'approval_recorded' ORDER BY seq",
        )
        .unwrap();
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|raw| serde_json::from_str::<Value>(&raw.unwrap()).unwrap())
        .filter(|p| p["scope"]["pr"] == json!(pr))
        .map(|p| p["scope"]["repo"].as_str().unwrap_or_default().to_string())
        .collect();
    rows
}

/// The audit reader agrees, whichever case the repo is asked in.
#[track_caller]
fn audit_state(board: &Board, pr: u64, want: &str, id: &str) {
    for repo in [REPO, "Acme/Widgets"] {
        let seen = crate::audit::approval_check(&board.state, repo, pr, HEAD).0;
        assert_eq!(seen["state"], want, "audit for {repo}#{pr}: {seen}");
        assert_eq!(seen["approval_id"], id, "audit for {repo}#{pr}: {seen}");
    }
}

/// Approve → revoke → Publish refused, then Approve → Publish reuses, on
/// a board whose project is `origin` and whose rows name `row`.
fn one_record_both_orders(origin: &str, row: &str) {
    let root = tempfile::Builder::new().prefix("c1300k").tempdir().unwrap();
    let board = Board::start_cased(root.path(), origin, row);
    let operator = board.session();
    let what = format!("project {origin}, row URL {row}");

    // --- (1) Approve, revoke, Publish: refused, whatever the case ---
    let (status, body) = board.post(
        "operator",
        Some(&operator),
        APPROVE,
        &approve_pr(PUBLISH_PR_2),
    );
    assert_eq!(status, 200, "setup ({what}): Approve: {body}");
    let revoked = body["approval_id"].as_str().unwrap().to_string();
    let (status, body) = board.post(
        "operator",
        Some(&operator),
        &format!("/api/approvals/{revoked}/revoke"),
        &json!({"reason": "re-review"}).to_string(),
    );
    assert_eq!(status, 200, "setup ({what}): revoke: {body}");
    let events = board.approval_events();
    let (status, body) = board.post(
        "operator",
        Some(&operator),
        &publish_path(PUBLISH_ISSUE_2),
        &publish_body(),
    );
    assert!(
        (400..500).contains(&status),
        "{what}: Publish after a revoked board approval must be refused: {status} {body}"
    );
    assert!(
        body.to_string().to_ascii_lowercase().contains("revoked"),
        "{what}: the refusal names the revoke: {body}"
    );
    assert_eq!(
        board.approval_events(),
        events,
        "{what}: a refused Publish wrote to the approval stream"
    );
    assert_eq!(recorded_repos(&board, PUBLISH_PR_2).len(), 1, "{what}");
    audit_state(&board, PUBLISH_PR_2, "revoked", &revoked);
    assert!(
        !merges(&board).contains(&format!("merge {PUBLISH_PR_2} ")),
        "{what}: a refused Publish enqueued a merge: {}",
        merges(&board)
    );
    assert_eq!(delivery_state(&board, PUBLISH_ISSUE_2), "passed", "{what}");

    // --- (2) Approve, then Publish: the one standing record is reused ---
    let (status, body) = board.post(
        "operator",
        Some(&operator),
        APPROVE,
        &approve_pr(PUBLISH_PR),
    );
    assert_eq!(status, 200, "setup ({what}): Approve: {body}");
    let standing = body["approval_id"].as_str().unwrap().to_string();
    let events = board.approval_events();
    let (status, body) = board.post(
        "operator",
        Some(&operator),
        &publish_path(PUBLISH_ISSUE),
        &publish_body(),
    );
    assert_eq!(status, 200, "{what}: Publish of an approved head: {body}");
    assert_eq!(
        body["approval_id"],
        standing.as_str(),
        "{what}: Publish reuses the standing approval: {body}"
    );
    assert_eq!(
        board.approval_events(),
        events,
        "{what}: Publish of an approved head wrote a second record"
    );
    assert_eq!(
        recorded_repos(&board, PUBLISH_PR),
        vec![REPO.to_string()],
        "{what}: exactly one record, in the canonical lowercase repo"
    );
    audit_state(&board, PUBLISH_PR, "in-force", &standing);
    // The merge still names the row's own slug.
    assert!(
        merges(&board).contains(&format!("merge {PUBLISH_PR} -R {row} ")),
        "{what}: gh pr merge must keep the row's slug: {}",
        merges(&board)
    );

    // --- (5) the CLI path is unchanged: a fresh record, and it merges ---
    let out = as_operator(
        &board,
        "delivery_approve",
        json!({"issue": PUBLISH_ISSUE_2, "sha": HEAD}),
    )
    .expect("the CLI's delivery merge still re-approves a revoked head");
    let fresh = out["approval_id"].as_str().unwrap_or_default().to_string();
    assert!(
        !fresh.is_empty() && fresh != revoked,
        "{what}: the CLI records a fresh id: {out}"
    );
    let seen = crate::audit::approval_check(&board.state, REPO, PUBLISH_PR_2, HEAD).0;
    assert_eq!(seen["state"], "in-force", "{what}: {seen}");
    assert_eq!(seen["source"], "operator", "{what}: {seen}");
    assert!(
        merges(&board).contains(&format!("merge {PUBLISH_PR_2} -R {row} ")),
        "{what}: {}",
        merges(&board)
    );
}

#[test]
fn cad1300_one_record_per_head_whatever_the_repo_case() {
    // The row URL in GitHub's canonical mixed case, the project lowercase.
    one_record_both_orders(REPO, "Acme/Widgets");
    // The reverse: the project registered in mixed case, the URL lowercase.
    one_record_both_orders("Acme/Widgets", REPO);

    // --- (2, converse) Publish first, then Approve: already on record ---
    let root = tempfile::Builder::new().prefix("c1300l").tempdir().unwrap();
    let board = Board::start_cased(root.path(), REPO, "Acme/Widgets");
    let operator = board.session();
    let (status, body) = board.post(
        "operator",
        Some(&operator),
        &publish_path(PUBLISH_ISSUE),
        &publish_body(),
    );
    assert_eq!(status, 200, "a first Publish records and enqueues: {body}");
    let id = body["approval_id"].as_str().unwrap().to_string();
    assert!(merges(&board).contains(&format!("merge {PUBLISH_PR} -R Acme/Widgets ")));
    let events = board.approval_events();
    let (status, body) = board.post(
        "operator",
        Some(&operator),
        APPROVE,
        &approve_pr(PUBLISH_PR),
    );
    assert!(
        (400..500).contains(&status),
        "Approve of a head Publish recorded under the row's case must be refused: {status} {body}"
    );
    assert_eq!(board.approval_events(), events, "a refused Approve wrote");
    assert_eq!(
        recorded_repos(&board, PUBLISH_PR),
        vec![REPO.to_string()],
        "Publish stores the canonical lowercase repo"
    );
    audit_state(&board, PUBLISH_PR, "in-force", &id);
}

// ---------------------------------------------------------------------
// Item 4, Spec/security REVISE round 2 on PR #913 (head 02ca64c0): the
// one-record-per-head and revoke-sticks rule must also hold against a
// record the operator's CLI wrote. `cadence audit approve` defaults
// `--repo` to the checkout's origin slug as spelled (`Acme/Widgets`) and
// `approval_record` stores it as sent, while the board paths look the
// head up as `acme/widgets`. `audit::approval_in_force` already compares
// the repo case-insensitively; the write-side lookups must agree.
//
// Contract (properties; a case-insensitive repo compare, PR equal, in
// the store's head lookups — `record_approval_once`, the standing and
// on-record lookups of the board Publish, and `record_approval`'s dedupe
// — is the recommended mechanism; it also covers mixed-case records
// already stored):
// - (1) a CLI record in any case, then a CLI revoke, then board Publish:
//   refused ("revoked"), no new event, no `gh pr merge`, row `passed`;
// - (2) a standing CLI record in any case: board Publish reuses it (same
//   id, no new event) and enqueues;
// - (3) board Approve after a CLI record in any case: refused, no event;
// - (4) the CLI's own `delivery_approve` (no `request_actor`) retried
//   against a standing `operator` record of the same head dedupes to it in
//   any case, exactly as it does for the lowercase record;
// - (5) each case runs first with a lowercase CLI record as the control
//   (today's behaviour), then with `Acme/Widgets`; the outcome must not
//   depend on the letter case.
// ---------------------------------------------------------------------

fn cli_record(board: &Board, repo: &str, pr: u64, source: &str) -> String {
    as_operator(
        board,
        "approval_record",
        json!({"repo": repo, "pr": pr, "head": HEAD, "source": source}),
    )
    .expect("setup: the CLI's approval_record")["approval_id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Cases (1)-(3) with the CLI record spelled `repo`.
fn cli_record_against_board_paths(repo: &str) {
    let root = tempfile::Builder::new().prefix("c1300x").tempdir().unwrap();
    let board = Board::start(root.path());
    let operator = board.session();
    let what = format!("CLI record for {repo}");

    // --- (1) CLI record, CLI revoke, board Publish: refused ---
    let revoked = cli_record(&board, repo, PUBLISH_PR_2, "chris in chat");
    as_operator(
        &board,
        "approval_revoke",
        json!({"id": revoked, "source": "operator", "reason": "re-review"}),
    )
    .expect("setup: the CLI's approval_revoke");
    audit_state(&board, PUBLISH_PR_2, "revoked", &revoked);
    let events = board.approval_events();
    let (status, body) = board.post(
        "operator",
        Some(&operator),
        &publish_path(PUBLISH_ISSUE_2),
        &publish_body(),
    );
    assert!(
        (400..500).contains(&status),
        "{what}: board Publish after the CLI record was revoked must be refused: {status} {body}"
    );
    assert!(
        body.to_string().to_ascii_lowercase().contains("revoked"),
        "{what}: the refusal names the revoke: {body}"
    );
    assert_eq!(
        board.approval_events(),
        events,
        "{what}: a refused Publish wrote"
    );
    audit_state(&board, PUBLISH_PR_2, "revoked", &revoked);
    assert!(
        !merges(&board).contains(&format!("merge {PUBLISH_PR_2} ")),
        "{what}: a refused Publish enqueued a merge: {}",
        merges(&board)
    );
    assert_eq!(delivery_state(&board, PUBLISH_ISSUE_2), "passed", "{what}");

    // --- (2) a standing CLI record: board Publish reuses it ---
    let standing = cli_record(&board, repo, PUBLISH_PR, "chris in chat");
    let events = board.approval_events();
    let (status, body) = board.post(
        "operator",
        Some(&operator),
        &publish_path(PUBLISH_ISSUE),
        &publish_body(),
    );
    assert_eq!(
        status, 200,
        "{what}: board Publish of a CLI-approved head: {body}"
    );
    assert_eq!(
        body["approval_id"],
        standing.as_str(),
        "{what}: Publish reuses the standing CLI record: {body}"
    );
    assert_eq!(
        board.approval_events(),
        events,
        "{what}: Publish wrote a second record for the head"
    );
    assert_eq!(recorded_repos(&board, PUBLISH_PR).len(), 1, "{what}");
    audit_state(&board, PUBLISH_PR, "in-force", &standing);
    assert!(merges(&board).contains(&format!("merge {PUBLISH_PR} ")));

    // --- (3) board Approve after a CLI record: already on record ---
    let cli = cli_record(&board, repo, 60, "chris in chat");
    let events = board.approval_events();
    let (status, body) = board.post("operator", Some(&operator), APPROVE, &approve_pr(60));
    assert!(
        (400..500).contains(&status),
        "{what}: board Approve of a CLI-approved head must be refused: {status} {body}"
    );
    assert_eq!(
        board.approval_events(),
        events,
        "{what}: a refused Approve wrote"
    );
    audit_state(&board, 60, "in-force", &cli);
}

#[test]
fn cad1300_cli_records_in_any_case_count_as_on_record() {
    // (5) the lowercase control first, then GitHub's canonical case.
    cli_record_against_board_paths(REPO);
    cli_record_against_board_paths("Acme/Widgets");

    // --- (4) the CLI's delivery merge retried against a standing
    // `operator` record dedupes to it: lowercase control, then mixed ---
    let root = tempfile::Builder::new().prefix("c1300y").tempdir().unwrap();
    let board = Board::start(root.path());
    for (repo, issue, pr) in [
        (REPO, PUBLISH_ISSUE, PUBLISH_PR),
        ("Acme/Widgets", PUBLISH_ISSUE_2, PUBLISH_PR_2),
    ] {
        let earlier = cli_record(&board, repo, pr, "operator");
        let events = board.approval_events();
        let out = as_operator(
            &board,
            "delivery_approve",
            json!({"issue": issue, "sha": HEAD}),
        )
        .expect("the CLI's delivery merge of an approved head");
        assert_eq!(
            out["approval_id"],
            earlier.as_str(),
            "{repo}: the CLI retry dedupes to the standing record: {out}"
        );
        assert_eq!(
            board.approval_events(),
            events,
            "{repo}: the CLI retry minted a second record for the head"
        );
        audit_state(&board, pr, "in-force", &earlier);
        assert!(merges(&board).contains(&format!("merge {pr} ")), "{repo}");
    }
}
