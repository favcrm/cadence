#[cfg(feature = "test-seam")]
mod native_chain_acceptance {}

use super::*;
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::thread;

const ORG: &str = "acme";
const ORG_ID: &str = "ws_real";
const BRIDGE: &str = "hct_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

fn target(listener: &TcpListener) -> RemoteTarget {
    RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint: format!("http://{}", listener.local_addr().unwrap()),
        org_id: ORG_ID.into(),
    }
}

/// Write the credential the login slice writes — the same on-disk
/// layout (`cli-<slug>.json`, the `hct_` bound to org id + audience).
fn store_credential_at(dir: &Path, org: &str, org_id: &str, endpoint: &str, expires_at: u64) {
    let body = json!({"version": "hosted-cadence-auth.v1",
        "organization_id": org_id, "audience": endpoint,
        "access_token": BRIDGE, "expires_at": expires_at});
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join(format!("cli-{org}.json")))
        .unwrap();
    file.write_all(body.to_string().as_bytes()).unwrap();
}
fn store_credential(dir: &Path, endpoint: &str) {
    store_credential_at(dir, ORG, ORG_ID, endpoint, now().unwrap() + 300);
}

#[test]
fn audit_verdict_is_a_registered_mutating_write() {
    use super::{cli_requested_scopes, lost_mutation_note, mutating_cli_verb, REMOTE_VERBS};

    assert!(REMOTE_VERBS.contains(&"audit_verdict"));
    assert_eq!(cli_requested_scopes("audit_verdict"), &["cli.write"]);
    assert!(mutating_cli_verb("audit_verdict"));
    assert!(
        lost_mutation_note("audit_verdict").contains("audit verdicts"),
        "a lost verdict write must send the caller back to the read, not a blind retry"
    );
}

#[test]
fn the_project_scoped_gate_verbs_are_registered_with_their_scopes() {
    // CAD-2001: the review evidence is project-scoped, so this build sends the
    // project verbs — and only those — for the gate and the operator.
    use super::{
        cli_requested_scopes, lost_mutation_note, mutating_cli_verb, PROJECT_SCOPED_GATE_VERBS,
        REMOTE_VERBS,
    };

    for verb in [
        "audit_approval_project",
        "audit_verdicts_project",
        "audit_verdict_project",
        "audit_approve_project",
        "audit_reviewers_project",
        "audit_reviewers_project_set",
        "audit_identities",
        "audit_identities_set",
    ] {
        assert!(REMOTE_VERBS.contains(&verb), "{verb}");
        assert!(PROJECT_SCOPED_GATE_VERBS.contains(&verb), "{verb}");
    }
    for verb in [
        "audit_approval_project",
        "audit_verdicts_project",
        "audit_reviewers_project",
        "audit_identities",
    ] {
        assert_eq!(cli_requested_scopes(verb), &["cli.read"], "{verb}");
        assert!(!mutating_cli_verb(verb), "{verb}");
    }
    assert_eq!(
        cli_requested_scopes("audit_verdict_project"),
        &["cli.write"]
    );
    for verb in [
        "audit_approve_project",
        "audit_reviewers_project_set",
        "audit_identities_set",
    ] {
        assert_eq!(
            cli_requested_scopes(verb),
            &["cli.write", "audit.approve"],
            "{verb}"
        );
        assert!(mutating_cli_verb(verb), "{verb}");
    }
    // A lost write sends the caller back to the matching project read.
    assert!(lost_mutation_note("audit_reviewers_project_set").contains("same project"));
    assert!(lost_mutation_note("audit_identities_set").contains("audit identities"));
    assert!(lost_mutation_note("audit_verdict_project").contains("audit verdicts"));
}

#[test]
fn a_tenant_that_cannot_serve_a_project_verb_names_the_release_order() {
    // An older tenant refuses the name (501 / 400 / 409 / 503) rather than
    // answering, and the client must say so — never read it as "no evidence"
    // and never fall back to the repo-scoped verbs.
    use super::{mutating_cli_verb, tenant_cannot_serve, PROJECT_SCOPED_GATE_VERBS};

    for verb in PROJECT_SCOPED_GATE_VERBS {
        for (status, body) in [
            (
                501u16,
                json!({"ok": false, "error": {"code": "not_implemented"}}),
            ),
            (
                400,
                json!({"ok": false, "error": {"code": "invalid_request"}}),
            ),
            (
                409,
                json!({"ok": false, "error": {"code": "container_required"}}),
            ),
        ] {
            let why = tenant_cannot_serve(verb, status, Some(&body)).expect("a version signal");
            assert!(why.contains("tenant"), "{why}");
            assert!(why.contains("first"), "{why}");
            assert!(why.contains("repo-scoped"), "{why}");
            assert!(why.contains(verb), "{why}");
        }
        // A waking reply is the retry loop's, not a version problem.
        assert!(tenant_cannot_serve(verb, 503, Some(&json!({"state": "waking"}))).is_none());
        // A bare availability failure keeps its own decoder.
        assert!(
            tenant_cannot_serve(verb, 503, Some(&json!({"state": "board_unreachable"}))).is_none()
        );
        // An unrelated refusal is not blamed on the release order.
        assert!(tenant_cannot_serve(
            verb,
            403,
            Some(&json!({"ok": false, "error": {"code": "not_permitted"}}))
        )
        .is_none());
    }
    // A tenant that predates the project verbs answers 503 for a capability
    // it lacks. A read keeps the release-order refusal; a write with the same
    // code is the mutation classifier's unknown outcome, so it is asserted
    // separately (and must not be read as "nothing was recorded").
    for verb in PROJECT_SCOPED_GATE_VERBS
        .iter()
        .copied()
        .filter(|verb| !mutating_cli_verb(verb))
    {
        let why = tenant_cannot_serve(
            verb,
            503,
            Some(&json!({"ok": false, "error": {"code": "capability_unavailable"}})),
        )
        .expect("a version signal");
        assert!(why.contains("Release order"), "{verb}: {why}");
        assert!(why.contains("repo-scoped"), "{verb}: {why}");
        assert!(why.contains(verb), "{verb}: {why}");
    }
    // A project with no proven policy is a real refusal with its own remedy.
    let why = tenant_cannot_serve(
        "audit_verdicts_project",
        409,
        Some(&json!({"ok": false, "error": {"code": "project_policy_unavailable"}})),
    )
    .expect("a refusal");
    assert!(why.contains("access policy"), "{why}");
    assert!(!why.contains("Release order"), "{why}");
}

#[test]
fn audit_reviewers_set_is_a_registered_owner_write() {
    // AOS-473: the attestation is the owner's own act, so its write asks for
    // the same `audit.approve` scope as the merge approval — never a plain
    // `cli.write` that an agent's login could carry.
    use super::{cli_requested_scopes, lost_mutation_note, mutating_cli_verb, REMOTE_VERBS};

    assert!(REMOTE_VERBS.contains(&"audit_reviewers"));
    assert!(REMOTE_VERBS.contains(&"audit_reviewers_set"));
    assert_eq!(cli_requested_scopes("audit_reviewers"), &["cli.read"]);
    assert_eq!(
        cli_requested_scopes("audit_reviewers_set"),
        &["cli.write", "audit.approve"]
    );
    assert!(mutating_cli_verb("audit_reviewers_set"));
    assert!(!mutating_cli_verb("audit_reviewers"));
    assert!(
        lost_mutation_note("audit_reviewers_set").contains("audit reviewers"),
        "a lost attestation write must send the caller back to the read, not a blind retry"
    );
}

#[test]
fn timeline_on_an_older_board_is_a_release_order_refusal() {
    use super::timeline_board_too_old;
    let args = |before: bool| {
        let mut m = serde_json::Map::new();
        m.insert("project".into(), serde_json::json!("atlas"));
        if before {
            m.insert("before".into(), serde_json::json!("cursor"));
        }
        m
    };
    let old = timeline_board_too_old(
        "project_timeline",
        501,
        Some("not_implemented"),
        &args(false),
    )
    .unwrap();
    assert!(
        old.contains("cannot serve the project timeline yet"),
        "{old}"
    );
    assert!(old.contains("501") && !old.contains("retry"), "{old}");
    let relay = timeline_board_too_old(
        "project_timeline",
        400,
        Some("invalid_request"),
        &args(false),
    )
    .unwrap();
    assert!(
        relay.contains("cannot serve the project timeline yet"),
        "{relay}"
    );
    // A bad cursor is a real 400 from a current board; other verbs are untouched.
    assert!(timeline_board_too_old(
        "project_timeline",
        400,
        Some("invalid_request"),
        &args(true)
    )
    .is_none());
    assert!(timeline_board_too_old("project_ls", 501, None, &args(false)).is_none());
}

/// CAD-2260. The statuses and codes are the ones the tenant board gives in
/// agenticos-v2 `apps/api/src/tenant-board/cli.ts` (`tenantWikiCall`): a
/// `write` carrying `transfer` fails the argument check, `job_status` is not
/// implemented, and a `read` of a missing path is `404 not_found`.
#[test]
fn wiki_cp_on_a_board_without_transfers_names_the_limit_and_what_works() {
    use super::wiki_copy_unserved;
    let transfer = |op: &str| {
        let mut m = serde_json::Map::new();
        m.insert("path".into(), json!("projects/demo/notes"));
        m.insert("transfer".into(), json!({ "op": op }));
        m
    };
    for (verb, op, status, code, nothing) in [
        // local → wiki, into a new or an existing folder
        ("write", "begin", 400, Some("invalid_request"), "copied"),
        // the same, from a board that refuses the transfer by one stable code
        ("write", "begin", 501, Some("not_implemented"), "copied"),
        // wiki → local
        (
            "job_status",
            "export",
            501,
            Some("not_implemented"),
            "copied",
        ),
        // --resume reads the admitted job first
        ("job_status", "status", 501, Some("not_implemented"), "read"),
        // a 501 whose code this client does not know: the code is not echoed
        ("job_status", "export", 501, Some("unknown_verb"), "copied"),
    ] {
        let error = wiki_copy_unserved(verb, status, code, &transfer(op)).unwrap();
        let text = error.to_string();
        assert_eq!(error.code(), Some("not_implemented"), "{text}");
        assert_eq!(error.kind(), "rejected", "{text}");
        assert!(text.contains("does not serve `wiki cp`"), "{text}");
        assert!(
            text.contains("wiki put") && text.contains("wiki upload"),
            "{text}"
        );
        assert!(text.contains(&format!("HTTP {status}")), "{text}");
        assert!(text.ends_with(&format!("Nothing was {nothing}")), "{text}");
        assert!(
            !text.contains("unknown") && !text.contains("retry"),
            "a healthy board must not read as an outage or an unrecognised refusal: {text}"
        );
    }
    // After `begin` was admitted a refusal is not proof that nothing moved, and
    // a board that serves transfers answers 400 to a bad chunk: both keep the
    // decoded refusal.
    for (op, status, code) in [
        ("chunk", 400, Some("invalid_request")),
        ("chunk", 501, Some("not_implemented")),
        ("apply", 501, Some("not_implemented")),
        ("finish", 501, Some("not_implemented")),
        ("begin", 400, Some("unknown_verb")),
        ("begin", 400, None),
        ("begin", 409, Some("path_conflict")),
    ] {
        assert!(
            wiki_copy_unserved("write", status, code, &transfer(op)).is_none(),
            "{op} {status} {code:?}"
        );
    }
    // A per-entry export read pins a `rev`; earlier entries may already be on
    // disk, so it keeps the decoded refusal too.
    let mut pinned = transfer("export");
    pinned["transfer"]["rev"] = json!("rev-1");
    assert!(wiki_copy_unserved("job_status", 501, Some("not_implemented"), &pinned).is_none());
    // A text `wiki put` carries no transfer and keeps its own refusals.
    let mut put = serde_json::Map::new();
    put.insert("path".into(), json!("projects/demo/notes/a.md"));
    put.insert("text".into(), json!("hello"));
    assert!(wiki_copy_unserved("write", 400, Some("invalid_request"), &put).is_none());
    assert!(wiki_copy_unserved("write", 501, Some("not_implemented"), &put).is_none());
}

/// CAD-2261: every code a transfer-serving board (AOS-566) refuses a `wiki cp`
/// operation with becomes one plain sentence that keeps the code.
#[test]
fn wiki_cp_refusals_from_a_board_that_serves_transfers_are_plain() {
    use super::wiki_copy_refusal;
    let with = |op: &str| {
        let mut m = serde_json::Map::new();
        m.insert("path".into(), json!("projects/demo/notes"));
        m.insert("transfer".into(), json!({ "op": op }));
        m
    };
    for (verb, op, status, code) in [
        ("write", "begin", 409, "destination_changed"),
        ("write", "begin", 413, "payload_too_large"),
        ("write", "begin", 409, "job_conflict"),
        ("write", "begin", 400, "manifest_invalid"),
        ("write", "finish", 400, "transfer_invalid"),
        ("job_status", "status", 404, "not_found"),
        ("job_status", "export", 404, "not_found"),
        ("write", "chunk", 404, "not_found"),
        ("write", "apply", 404, "not_found"),
        ("write", "finish", 404, "not_found"),
        ("write", "apply", 409, "stage_incomplete"),
        ("write", "finish", 409, "entries_pending"),
        ("write", "chunk", 409, "chunk_conflict"),
        ("write", "chunk", 409, "job_closed"),
        ("write", "chunk", 409, "entry_decided"),
        ("job_status", "export", 409, "revision_changed"),
    ] {
        let error = wiki_copy_refusal(verb, status, Some(code), &with(op)).unwrap();
        let text = error.to_string();
        assert_eq!(error.code(), Some(code), "{text}");
        assert_eq!(error.kind(), "rejected", "{text}");
        assert!(
            text.ends_with(&format!("({code}, HTTP {status})")),
            "{text}"
        );
        for banned in [
            "unknown",
            "did not match",
            "retry later",
            "MiB",
            "Tenant remote call",
        ] {
            assert!(!text.contains(banned), "{banned}: {text}");
        }
    }
    let missing_job = wiki_copy_refusal("job_status", 404, Some("not_found"), &with("status"));
    assert!(missing_job
        .unwrap()
        .to_string()
        .contains("no such copy job"));
    let missing_page = wiki_copy_refusal("job_status", 404, Some("not_found"), &with("export"));
    assert!(missing_page
        .unwrap()
        .to_string()
        .contains("no readable page"));
    // Mid-job the wrapper gives the next step, so no "without `--resume`" here.
    let gone = wiki_copy_refusal("write", 404, Some("not_found"), &with("chunk")).unwrap();
    assert!(
        gone.to_string().contains("no longer has this copy job"),
        "{gone}"
    );
    assert!(!gone.to_string().contains("--resume"), "{gone}");
    // A `begin` never meets a job that does not exist yet.
    assert!(wiki_copy_refusal("write", 404, Some("not_found"), &with("begin")).is_none());
    // A board without transfers: the CAD-2260 refusal now says what unblocks it.
    let old = super::wiki_copy_unserved("write", 501, Some("not_implemented"), &with("begin"));
    assert!(old.unwrap().to_string().contains("has to be updated first"));
    assert!(
        wiki_copy_refusal("write", 409, Some("destination_changed"), &with("begin"))
            .unwrap()
            .to_string()
            .contains("--update")
    );
    assert!(
        wiki_copy_refusal("write", 413, Some("payload_too_large"), &with("begin"))
            .unwrap()
            .to_string()
            .contains("wiki upload")
    );
    // A text `wiki put` shares the verb and keeps its own refusals.
    let mut put = serde_json::Map::new();
    put.insert("path".into(), json!("projects/demo/notes/a.md"));
    put.insert("text".into(), json!("hello"));
    assert!(wiki_copy_refusal("write", 413, Some("payload_too_large"), &put).is_none());
    // A status or code the board does not send for a transfer is left alone.
    assert!(wiki_copy_refusal("write", 409, Some("revision_conflict"), &with("begin")).is_none());
    assert!(wiki_copy_refusal("write", 400, Some("invalid_request"), &with("begin")).is_none());
    assert!(wiki_copy_refusal("write", 409, None, &with("begin")).is_none());
    assert!(wiki_copy_refusal("read", 404, Some("not_found"), &with("status")).is_none());
}

/// CAD-2261: the refusals in the board's own fixture (a byte-identical copy of
/// agenticos-v2 `apps/api/src/board/fixtures/wiki-transfer.contract.json` at
/// `67a3839ae1222b2e6a9be7904aca270f5a449aea`) replayed through the real
/// request path: the board's exact bytes in, the board's code out.
#[test]
fn wiki_cp_refusals_in_the_board_fixture_reach_the_user_plainly() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/agenticos/wiki-transfer.contract.json"
    ))
    .unwrap();
    let mut replayed = 0;
    for step in fixture["steps"].as_array().unwrap() {
        let status = step["response"]["status"].as_u64().unwrap() as u16;
        if status < 400 {
            continue;
        }
        let label = step["label"].as_str().unwrap();
        let verb = step["action"].as_str().unwrap();
        let args = step["arguments"].as_object().unwrap().clone();
        let body = step["response"]["body"].clone();
        let code = body["error"]["code"].as_str().unwrap().to_string();
        if verb != "read" && wiki_transfer_shape(verb, &args).is_err() {
            // The client refuses this one before any request; the board's
            // answer to it is still mapped.
            let error = super::wiki_copy_refusal(verb, status, Some(code.as_str()), &args).unwrap();
            assert_eq!(error.code(), Some(code.as_str()), "{label}");
            replayed += 1;
            continue;
        }
        let line: &'static str = match status {
            400 => "400 Bad Request",
            404 => "404 Not Found",
            409 => "409 Conflict",
            413 => "413 Payload Too Large",
            other => panic!("{label}: no status line for {other}"),
        };
        let answer = body.clone();
        let (listener, seen, server) = finite_worker(2, move |i, socket, _req| {
            if i == 0 {
                upload_authorize_answer(socket);
            } else {
                write_answer(socket, line, "", &answer);
            }
        });
        let (target, _root, dir) = credentialed(&listener);
        let error = call_verb_in(&target, verb, args, 5, &dir, |_| {}, |_| {}).unwrap_err();
        server.join().unwrap();
        assert_eq!(seen.lock().unwrap().len(), 2, "{label}: sent once");
        let text = error.to_string();
        assert_eq!(error.code(), Some(code.as_str()), "{label}: {text}");
        assert_eq!(error.kind(), "rejected", "{label}: {text}");
        assert!(!text.contains("unknown"), "{label}: {text}");
        assert!(!text.contains("Tenant remote call"), "{label}: {text}");
        // The sentence the user reads, not only the code.
        let expected = match (verb, code.as_str()) {
            ("read", "not_found") => "no readable page at that path",
            (_, "job_conflict") => "already holds this job id for a different copy",
            (_, "destination_changed") => "Copy again with `--update`",
            (_, "not_found") => "no such copy job for this login",
            (_, other) => panic!("{label}: no expected sentence for {other}"),
        };
        assert!(text.contains(expected), "{label}: {text}");
        replayed += 1;
    }
    assert!(replayed > 0, "the fixture holds no refusal");
}

#[test]
fn a_missing_wiki_page_on_a_hosted_board_is_named_by_code() {
    let error = message_family_refusal("read", 404, Some("not_found"), reject("generic"));
    assert_eq!(error.code(), Some("not_found"));
    let text = error.to_string();
    assert!(text.contains("no readable page at that path"), "{text}");
    assert!(!text.contains("generic"), "{text}");
    // Another verb's 404 keeps the decoded refusal.
    let other = message_family_refusal("enumerate", 404, Some("not_found"), reject("generic"));
    assert!(other.to_string().contains("generic"));
}

/// CAD-2260: one wiki call against a fake board that authorizes it and then
/// refuses it in the body shape the tenant board's `fail()` writes. The board
/// serves exactly those two requests. Returns the refusal and the paths it saw.
fn wiki_call_refused(
    status_line: &'static str,
    code: &'static str,
    verb: &str,
    args: Value,
) -> (Error, Vec<String>) {
    let (listener, seen, server) = finite_worker(2, move |i, socket, _req| {
        if i == 0 {
            upload_authorize_answer(socket);
        } else {
            write_answer(
                socket,
                status_line,
                "",
                &json!({"ok": false, "error": {
                    "code": code, "message": "Tenant remote call refused."}}),
            );
        }
    });
    let (target, _root, dir) = credentialed(&listener);
    let out = call_verb_in(
        &target,
        verb,
        args.as_object().unwrap().clone(),
        5,
        &dir,
        |_| {},
        |_| {},
    );
    server.join().unwrap();
    let paths = seen
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.path.clone())
        .collect();
    (out.unwrap_err(), paths)
}

/// CAD-2260, through the request path: the requests `wiki cp` sends to a board
/// without transfers each come back as one clear refusal, sent once.
#[test]
fn wiki_cp_against_a_board_without_transfers_is_one_clear_refusal_sent_once() {
    let manifest = json!({"version": 1, "source_id": "local:demo",
        "root": "projects/demo/notes", "entries": []});
    for (verb, args, status_line, code) in [
        (
            "write",
            json!({"path": "projects/demo/notes",
                "transfer": {"op": "begin", "job_id": "job-1", "manifest": manifest}}),
            "400 Bad Request",
            "invalid_request",
        ),
        (
            "job_status",
            json!({"path": "projects/demo/notes/a.md",
                "transfer": {"op": "export", "path": "projects/demo/notes/a.md"}}),
            "501 Not Implemented",
            "not_implemented",
        ),
    ] {
        let (error, seen) = wiki_call_refused(status_line, code, verb, args);
        let text = error.to_string();
        assert_eq!(error.code(), Some("not_implemented"), "{verb}: {text}");
        assert!(text.contains("does not serve `wiki cp`"), "{verb}: {text}");
        assert!(
            !text.contains("unknown") && !text.contains("retry"),
            "{verb}: {text}"
        );
        assert!(!text.contains("Tenant remote call"), "{verb}: {text}");
        assert_eq!(
            seen,
            ["/__platform/wiki/authorize", "/__platform/wiki/call"],
            "{verb}: one request, never replayed"
        );
    }
    // The discovery read of a destination that does not exist yet.
    let (error, _) = wiki_call_refused(
        "404 Not Found",
        "not_found",
        "read",
        json!({"path": "projects/demo/notes", "offset": 0, "bytes": 0}),
    );
    assert_eq!(error.code(), Some("not_found"), "{error}");
}

#[test]
fn project_verbs_are_a_registered_read_and_owner_write() {
    // CAD-1999: the client refuses a verb missing from REMOTE_VERBS before a
    // request exists, so the registry read and the owner write must both be
    // registered — and the write must ask only for cli.write, while a lost
    // answer reconciles with the read instead of a blind retry.
    use super::{cli_requested_scopes, lost_mutation_note, mutating_cli_verb, REMOTE_VERBS};

    assert!(REMOTE_VERBS.contains(&"project_ls"));
    assert!(REMOTE_VERBS.contains(&"project_new"));
    assert!(REMOTE_VERBS.contains(&"project_timeline"));
    assert_eq!(cli_requested_scopes("project_timeline"), &["cli.read"]);
    assert!(!mutating_cli_verb("project_timeline"));
    assert_eq!(cli_requested_scopes("project_ls"), &["cli.read"]);
    assert_eq!(cli_requested_scopes("project_new"), &["cli.write"]);
    assert!(!mutating_cli_verb("project_ls"));
    assert!(mutating_cli_verb("project_new"));
    assert!(
        lost_mutation_note("project_new").contains("issue project ls"),
        "a lost project create must send the caller back to the registry read"
    );
}

#[test]
fn project_new_without_a_correlated_receipt_is_unknown_and_not_retried() {
    // CAD-1999: a 2xx proves the create only as the requested project echoed
    // back — key, name and a valid prefix that matches the requested
    // `--prefix`. A `null`, `{ok:true}`, foreign or unrelated body, or a
    // missing/invalid/different prefix cannot establish that THIS create
    // landed under the intended namespace — unknown, reconciled by the
    // registry read, never a rejection and never a replay. The
    // empty/204/non-JSON answer is the same check on a `None` body.
    let args = || {
        json!({"key": "ops", "name": "Operations", "prefix": "OPS"})
            .as_object()
            .unwrap()
            .clone()
    };
    let derived_args = || {
        json!({"key": "ops", "name": "Operations"})
            .as_object()
            .unwrap()
            .clone()
    };
    for (receipt, arguments) in [
        (Value::Null, args()),
        (json!({"ok": true}), args()),
        (
            json!({"project": {"key": "other", "name": "Operations", "prefix": "OPS"}}),
            args(),
        ),
        (
            json!({"project": {"key": "ops", "name": "Other", "prefix": "OPS"}}),
            args(),
        ),
        (json!({"project": "ops"}), args()),
        // The stored prefix is missing entirely.
        (
            json!({"project": {"key": "ops", "name": "Operations"}}),
            args(),
        ),
        // The stored prefix is another project's namespace.
        (
            json!({"project": {"key": "ops", "name": "Operations", "prefix": "OTHER"}}),
            args(),
        ),
        // The stored prefix is not a valid hosted prefix (starts with a digit).
        (
            json!({"project": {"key": "ops", "name": "Operations", "prefix": "9X"}}),
            args(),
        ),
        // No requested prefix, but the receipt still must carry a real one.
        (
            json!({"project": {"key": "ops", "name": "Operations"}}),
            derived_args(),
        ),
    ] {
        let (out, requests) = run(vec![(200, "", receipt)], "project_new", arguments, 10);
        let err = out.unwrap_err();
        assert_eq!(err.kind(), "unknown", "{err}");
        let text = err.to_string();
        assert!(text.contains("issue project ls"), "{text}");
        assert!(!text.contains("retry later"), "{text}");
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "a project create was replayed"
        );
    }

    // The requested prefix echoed as stored (upper case) is the same project.
    let (out, _) = run(
        vec![(
            200,
            "",
            json!({"project": {"key": "ops", "name": "Operations", "prefix": "OPS"},
                "duplicate": false}),
        )],
        "project_new",
        args(),
        10,
    );
    assert!(out.is_ok(), "{:?}", out.err().map(|e| e.to_string()));

    // A caller-typed lower-case --prefix matches the stored upper-case one.
    let lower = || {
        json!({"key": "ops", "name": "Operations", "prefix": "ops"})
            .as_object()
            .unwrap()
            .clone()
    };
    let (out, _) = run(
        vec![(
            200,
            "",
            json!({"project": {"key": "ops", "name": "Operations", "prefix": "OPS"}}),
        )],
        "project_new",
        lower(),
        10,
    );
    assert!(out.is_ok(), "{:?}", out.err().map(|e| e.to_string()));

    // No --prefix: the board derives it, so any valid stored prefix is
    // accepted rather than duplicating the board's derivation client-side.
    let (out, _) = run(
        vec![(
            200,
            "",
            json!({"project": {"key": "ops", "name": "Operations", "prefix": "OPS"}}),
        )],
        "project_new",
        derived_args(),
        10,
    );
    assert!(out.is_ok(), "{:?}", out.err().map(|e| e.to_string()));
}

#[test]
fn project_new_ambiguous_5xx_is_unknown_but_pre_forward_5xx_stays_definite() {
    // CAD-1999, narrowed by CAD-2009: a 5xx without proof it preceded the verb
    // may follow a committed create, and a `state` the relay does not document
    // is no such proof. A failed wake, a tracker refusal and a board gate
    // refusal (`hop: board`) are decided before the verb ran and stay definite.
    let args = || {
        json!({"key": "ops", "name": "Operations"})
            .as_object()
            .unwrap()
            .clone()
    };
    for (status, body) in [
        (500u16, json!({"ok": false})),
        (503, json!({"ok": false})),
        (500, json!({"state": "unavailable"})),
        (
            503,
            json!({"ok": false, "error": {"code": "capability_unavailable"}}),
        ),
    ] {
        let (out, requests) = run(vec![(status, "", body)], "project_new", args(), 1);
        let err = out.unwrap_err();
        assert_eq!(err.kind(), "unknown", "{status}: {err}");
        let text = err.to_string();
        assert!(text.contains("issue project ls"), "{status}: {text}");
        assert!(!text.contains("retry later"), "{status}: {text}");
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "{status}: a project create was replayed"
        );
    }

    for body in [
        json!({"state": "wake_failed", "code": "wake_failed"}),
        json!({"ok": false, "error": {"code": "tracker_unavailable"}}),
    ] {
        let (out, _) = run(vec![(503, "", body)], "project_new", args(), 1);
        assert_eq!(out.unwrap_err().kind(), "rejected");
    }

    let (out, _) = run(
        vec![(
            500,
            "",
            json!({"ok": false, "error": {"code": "runtime_unavailable", "hop": "board"}}),
        )],
        "project_new",
        args(),
        1,
    );
    assert_eq!(out.unwrap_err().kind(), "rejected");
}

#[test]
fn assignment_identity_survives_saved_login_reloads() {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut frozen = target(&listener);
    frozen.identity = Some(CliIdentityPin {
        subject_id: "user_original".into(),
        issuer: "https://original.invalid".into(),
    });
    store_credential(dir.path(), &frozen.endpoint);
    let path = dir.path().join(format!("cli-{ORG}.json"));
    let mut original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    original["subject_id"] = json!("user_original");
    original["issuer"] = json!("https://original.invalid");
    fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(
        cli_material(dir.path(), &frozen).unwrap().access_token,
        BRIDGE
    );

    for (subject, issuer) in [
        (json!("user_replacement"), json!("https://original.invalid")),
        (json!("user_original"), json!("https://replacement.invalid")),
        (Value::Null, Value::Null),
        (json!("user_original"), Value::Null),
    ] {
        let mut replacement = original.clone();
        replacement["subject_id"] = subject;
        replacement["issuer"] = issuer;
        fs::write(&path, serde_json::to_vec(&replacement).unwrap()).unwrap();
        assert!(cli_material(dir.path(), &frozen).is_err());
        assert_eq!(
            listener.accept().err().unwrap().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
    fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(
        cli_material(dir.path(), &frozen).unwrap().access_token,
        BRIDGE
    );
}

fn read_request(socket: std::net::TcpStream) -> (std::net::TcpStream, Request) {
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut parts = line.trim_end().split(' ');
    let (method, path) = (
        parts.next().unwrap().to_string(),
        parts.next().unwrap().to_string(),
    );
    assert_eq!(method, "POST");
    let mut bearer = String::new();
    let mut length = 0_usize;
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        let (key, value) = line.split_once(':').unwrap();
        if key.eq_ignore_ascii_case("authorization") {
            bearer = value.trim().trim_start_matches("Bearer ").to_string();
        }
        if key.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse().unwrap();
        }
    }
    let mut raw = vec![0; length];
    reader.read_exact(&mut raw).unwrap();
    (
        socket,
        Request {
            path,
            bearer,
            body: serde_json::from_slice(&raw).unwrap(),
        },
    )
}

/// One request the fake accepted — path, bearer and parsed body.
struct Request {
    path: String,
    bearer: String,
    body: Value,
}

fn answer(mut socket: std::net::TcpStream, status: &str, extra: &str, body: &Value) {
    let bytes = serde_json::to_vec(body).unwrap();
    write!(
        socket,
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n{extra}\r\n",
        bytes.len()
    )
    .unwrap();
    socket.write_all(&bytes).unwrap();
}

/// The strict `/__platform/cli/authorize` gate (mirrors
/// `hostedCadenceCliAuthorizeRequestSchema`): every key required, no key
/// extra, org and audience pinned to this board, scopes from the CLI
/// authorization contract, and ttl within 1..=300.
fn check_authorize(req: &Request) -> bool {
    req.path == "/__platform/cli/authorize"
        && req.bearer == BRIDGE
        && req.body.as_object().map(Map::len) == Some(5)
        && req.body["version"] == json!(CLI_VERSION)
        && req.body["organization_id"] == json!(ORG_ID)
        && req.body["audience"].as_str().is_some()
        && req.body["requested_scopes"].as_array().is_some_and(|s| {
            !s.is_empty()
                && s.iter().all(|v| {
                    matches!(
                        v.as_str(),
                        Some(
                            "cli.read"
                                | "cli.write"
                                | "settings.read"
                                | "settings.write"
                                | "audit.approve"
                        )
                    )
                })
        })
        && req.body["ttl_seconds"]
            .as_u64()
            .is_some_and(|t| (1..=300).contains(&t))
}

/// The strict `/__platform/cli/call` gate (mirrors
/// `hostedCadenceCliCallRequestSchema` + the forbidden-key rule):
/// version, a compact-JWS envelope, an allowlisted verb, optional
/// arguments object — and no authority-shaped top-level key.
fn check_call(req: &Request) -> bool {
    const FORBIDDEN: &[&str] = &["wiki_as", "actor", "role", "org", "user", "as", "principal"];
    let keys_ok = req
        .body
        .as_object()
        .is_some_and(|o| o.keys().all(|k| !FORBIDDEN.contains(&k.as_str())));
    let envelope_ok = req.body["envelope"].as_str().is_some_and(|e| {
        e.split('.').count() == 3
            && e.split('.').all(|p| {
                !p.is_empty()
                    && p.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            })
    });
    req.path == "/__platform/cli/call"
        && req.bearer == BRIDGE
        && keys_ok
        && req.body["version"] == json!(CLI_VERSION)
        && envelope_ok
        && REMOTE_VERBS.contains(&req.body["verb"].as_str().unwrap_or_default())
        && req.body.get("arguments").is_none_or(|a| a.is_object())
}

/// A loopback fake worker: one authorize (strict schema → mint a fixed
/// envelope shape), then `calls` call-attempts answered in order.
/// Returns the bound listener plus a `Vec` the server records every
/// request path + body into — so a test can prove nothing was sent.
fn fake_worker(
    answers: Vec<(u16, &'static str, Value)>,
) -> (
    TcpListener,
    std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = listener.try_clone().unwrap();
    let seen: std::sync::Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
    let requests = seen.clone();
    // The container (#744) consumes each cli envelope's `jti` once —
    // the fake does the same so a reused envelope is refused, not
    // silently accepted.
    let spent: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let consumed = spent.clone();
    let handle = thread::spawn(move || {
        loop {
            let Ok((socket, _)) = server.accept() else {
                return;
            };
            let req = read_request(socket);
            let (socket, req) = (req.0, req.1);
            requests.lock().unwrap().push(json!({
                "path": req.path, "body": req.body,
            }));
            if req.path == "/__platform/cli/authorize" {
                if !check_authorize(&req) {
                    answer(
                        socket,
                        "400 Bad Request",
                        "",
                        &json!({"code": "invalid_request"}),
                    );
                    continue;
                }
                // Envelope shape only — the worker signs the real one.
                // A distinct envelope per mint lets a test prove the
                // client never reuses one across commands/retries (#744:
                // the container treats each cli envelope as single-use).
                let mint = requests
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|r| r["path"] == json!("/__platform/cli/authorize"))
                    .count();
                let envelope = format!("h.e.{mint}");
                answer(
                    socket,
                    "200 OK",
                    "",
                    &json!({"version": CLI_VERSION, "envelope": envelope,
                        "bearer": format!("wikienv_{envelope}"), "expires_at": 0,
                        "actor": "users/o", "scope": ["cli.read"], "issuer": "i"}),
                );
                continue;
            }
            if req.path == "/__platform/cli/call" {
                let n = requests
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|r| r["path"] == json!("/__platform/cli/call"))
                    .count();
                if !check_call(&req) {
                    answer(
                        socket,
                        "400 Bad Request",
                        "",
                        &json!({"code": "invalid_request"}),
                    );
                    continue;
                }
                // Single-use: a replayed envelope is unauthorized —
                // the jti was already spent by an earlier call.
                let env = req.body["envelope"].as_str().unwrap().to_string();
                {
                    let mut spent = consumed.lock().unwrap();
                    if spent.contains(&env) {
                        answer(
                            socket,
                            "401 Unauthorized",
                            "",
                            &json!({"code": "envelope_reused"}),
                        );
                        continue;
                    }
                    spent.push(env);
                }
                let Some((status, extra, body)) = answers.get(n - 1) else {
                    // More attempts than answers — a test bug or an
                    // unbounded retry loop; refuse loudly.
                    answer(socket, "500", "", &json!({"code": "unexpected_call"}));
                    continue;
                };
                let line = match status {
                    200 => "200 OK",
                    201 => "201 Created",
                    202 => "202 Accepted",
                    302 => "302 Found",
                    401 => "401 Unauthorized",
                    403 => "403 Forbidden",
                    404 => "404 Not Found",
                    409 => "409 Conflict",
                    500 => "500 Internal Server Error",
                    501 => "501 Not Implemented",
                    503 => "503 Service Unavailable",
                    _ => "400 Bad Request",
                };
                answer(socket, line, extra, &body.clone());
                continue;
            }
            answer(socket, "404 Not Found", "", &json!({"code": "not_found"}));
        }
    });
    (listener, seen, handle)
}

/// Run one command against the strict fake: returns the call result
/// and every request the worker saw (to count mints / reads).
fn run(
    answers: Vec<(u16, &'static str, Value)>,
    verb: &str,
    args: Map<String, Value>,
    wake_timeout: u64,
) -> (Result<Value>, Vec<Value>) {
    let (listener, calls, _server) = fake_worker(answers);
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    let out = call_verb_in(&target, verb, args, wake_timeout, &dir, |_| {}, |_| {});
    let calls_out = calls.lock().unwrap().clone();
    // Tests assert on the verb's JSON body — a conflict answer
    // still carries one, so unwrap it the same way.
    let out = out.map(|a| match a {
        RemoteAnswer::Ok(b) | RemoteAnswer::Conflict(b) => b,
    });
    (out, calls_out)
}

/// Number of requests the fake accepted at `path` — 0 proves no byte
/// for that route ever left the client.
fn hits(requests: &[Value], path: &str) -> usize {
    requests.iter().filter(|r| r["path"] == json!(path)).count()
}

#[test]
fn cad1434_blocker_authorize_refusals_never_forward_or_retry_user_writes() {
    let request_id = "12345678-1234-4234-8234-123456789abc";
    let unavailable = json!({"ok": false, "error": {
        "code": "runtime_unavailable", "hop": "board", "request_id": request_id,
        "message": "do-not-print-server-text"}})
    .to_string();
    let unsafe_code = json!({"code": "do-not-print-server-text /private/token",
        "message": "do-not-print-server-text"})
    .to_string();
    for (status, body, expected) in [
        (
            401,
            "<html>do-not-print-server-text</html>",
            "uncertain, HTTP 401",
        ),
        (
            403,
            "<html>do-not-print-server-text</html>",
            "authorization, HTTP 403",
        ),
        (503, unavailable.as_str(), "availability, HTTP 503"),
        (400, unsafe_code.as_str(), "unknown, HTTP 400"),
        (
            302,
            "<html>do-not-print-server-text</html>",
            "remote redirect refused",
        ),
        (
            200,
            "<html>do-not-print-server-text</html>",
            "remote authorize answered non-JSON",
        ),
    ] {
        for operation in ["issue_new", "write", "upload"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let target = target(&listener);
            let server = listener.try_clone().unwrap();
            let response = body.to_owned();
            let (stop, stopped) = std::sync::mpsc::channel();
            let worker = thread::spawn(move || {
                let mut paths = Vec::new();
                let mut stopping = false;
                let deadline = Instant::now() + Duration::from_secs(10);
                while Instant::now() < deadline {
                    stopping |= stopped.try_recv().is_ok();
                    let (mut socket, _) = match server.accept() {
                        Ok(connection) => connection,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            if stopping {
                                break;
                            }
                            thread::sleep(Duration::from_millis(2));
                            continue;
                        }
                        Err(e) => panic!("refusal worker accept: {e}"),
                    };
                    socket.set_nonblocking(false).unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(10)))
                        .unwrap();
                    let mut reader = BufReader::new(socket.try_clone().unwrap());
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    paths.push(line.split(' ').nth(1).unwrap().to_owned());
                    let mut length = 0;
                    loop {
                        line.clear();
                        reader.read_line(&mut line).unwrap();
                        if line == "\r\n" {
                            break;
                        }
                        if let Some((key, value)) = line.split_once(':') {
                            if key.eq_ignore_ascii_case("content-length") {
                                length = value.trim().parse::<usize>().unwrap();
                            }
                        }
                    }
                    // Read bytes, not JSON: an incorrectly forwarded multipart
                    // upload must also be recorded and fail the path assertion.
                    let mut raw = vec![0; length];
                    reader.read_exact(&mut raw).unwrap();
                    write!(socket, "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}", response.len()).unwrap();
                }
                paths
            });
            let root = tempfile::tempdir().unwrap();
            let dir = root.path();
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
            store_credential(dir, &target.endpoint);
            let saved = fs::read(dir.join(format!("cli-{ORG}.json"))).unwrap();
            let result = if operation == "upload" {
                upload_file_in(
                    dir,
                    &target,
                    "global/test.txt",
                    "test.txt",
                    b"test",
                    true,
                    5,
                    |_| panic!("authorize refusal retried"),
                )
            } else {
                let arguments = if operation == "write" {
                    json!({"path": "global/test", "text": "test"})
                } else {
                    json!({"project": "test", "title": "test"})
                };
                call_verb_in(
                    &target,
                    operation,
                    arguments.as_object().unwrap().clone(),
                    5,
                    dir,
                    |_| panic!("authorize refusal retried"),
                    |_| panic!("authorize refusal slept"),
                )
                .map(|_| json!(null))
            };
            stop.send(()).unwrap();
            let paths = worker.join().unwrap();
            let relay = if operation == "issue_new" {
                "cli"
            } else {
                "wiki"
            };
            assert_eq!(
                paths,
                [format!("/__platform/{relay}/authorize")],
                "{operation}, HTTP {status}"
            );
            let text = result.unwrap_err().to_string();
            assert!(text.contains(expected), "{operation}: {text}");
            assert!(!text.contains("do-not-print-server-text"), "{text}");
            assert!(!text.contains("/private/token"), "{text}");
            assert!(!text.contains("cadence login"), "{text}");
            assert!(!text.contains(BRIDGE), "{text}");
            if status == 503 {
                assert!(
                    text.contains(&format!("hop board, request {request_id}")),
                    "{text}"
                );
            }
            assert_eq!(
                fs::read(dir.join(format!("cli-{ORG}.json"))).unwrap(),
                saved
            );
        }
    }
}

#[test]
fn wiki_refuses_forged_authority_before_mint_and_never_uses_cli_relay() {
    // CAD-1401 independent refusal acceptance. The worker deliberately
    // accepts every payload: only the real client guard can refuse it.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let target = target(&listener);
    let server = listener.try_clone().unwrap();
    let seen: std::sync::Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
    let requests = seen.clone();
    let (stop, stopped) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        while stopped.try_recv().is_err() {
            let socket = match server.accept() {
                Ok((socket, _)) => socket,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(e) => panic!("fake worker accept: {e}"),
            };
            let (socket, req) = read_request(socket);
            let version = if req.path.starts_with("/__platform/wiki/") {
                "hosted-cadence-wiki.v1"
            } else {
                "hosted-cadence-cli.v1"
            };
            let response = if req.path.ends_with("/authorize") {
                json!({"version": version, "envelope": "h.e.1"})
            } else if req.body["action"] == "write" {
                json!({"path": "global/acceptance.md", "rev": "fnv1a:0123456789abcdef",
                    "committed": true})
            } else {
                json!({"ok": true})
            };
            requests.lock().unwrap().push(json!({
                "path": req.path, "bearer": req.bearer, "body": req.body,
            }));
            answer(socket, "200 OK", "", &response);
        }
    });
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(
        root.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    store_credential(root.path(), &target.endpoint);
    let mut failures = Vec::new();

    for dir in [root.path().to_path_buf(), root.path().join("missing")] {
        let before = seen.lock().unwrap().len();
        let out = call_verb_in(&target, "delete", Map::new(), 10, &dir, |_| {}, |_| {});
        if !matches!(out, Err(ref e) if e.kind() == "rejected" && e.to_string().contains("allowlist"))
            || seen.lock().unwrap().len() != before
        {
            failures.push("delete was not refused before credential access/request".to_string());
        }
    }
    // Observe successful calls too, so a missing route or a wrong schema
    // cannot masquerade as a local refusal. Include an empty arguments map.
    for (action, args) in [
        ("enumerate", json!({"path": "global/", "q": "acceptance"})),
        ("read", json!({"path": "global/acceptance.md"})),
        (
            "write",
            json!({"path": "global/acceptance.md", "text": "acceptance", "if_rev": "none"}),
        ),
        ("enumerate", json!({})),
    ] {
        let args = args.as_object().unwrap().clone();
        let before = seen.lock().unwrap().len();
        let out = call_verb_in(
            &target,
            action,
            args.clone(),
            10,
            root.path(),
            |_| {},
            |_| {},
        );
        let observed = seen.lock().unwrap();
        let pair = &observed[before..];
        let scope = if action == "write" {
            "wiki.write"
        } else {
            "wiki.read"
        };
        if !matches!(out, Ok(RemoteAnswer::Ok(_)))
            || pair.len() != 2
            || pair[0]["path"] != "/__platform/wiki/authorize"
            || pair[0]["bearer"] != BRIDGE
            || pair[0]["body"]["version"] != "hosted-cadence-wiki.v1"
            || pair[0]["body"]["organization_id"] != ORG_ID
            || pair[0]["body"]["audience"] != target.endpoint
            || pair[0]["body"]["requested_scopes"] != json!([scope])
            || pair[1]["path"] != "/__platform/wiki/call"
            || pair[1]["bearer"] != BRIDGE
            || !exact_object(
                &pair[1]["body"],
                &["version", "envelope", "action", "arguments"],
            )
            || pair[1]["body"]["version"] != "hosted-cadence-wiki.v1"
            || pair[1]["body"]["envelope"] != "h.e.1"
            || pair[1]["body"]["action"] != action
            || pair[1]["body"]["arguments"] != Value::Object(args)
        {
            failures.push(format!(
                "{action}: wrong wiki route, scope, or exact call body"
            ));
        }
    }
    for action in ["enumerate", "read", "write"] {
        for field in ["actor", "role", "org", "user", "wiki_as", "as", "principal"] {
            let mut args = json!({"path": "global/acceptance.md", "text": "acceptance"})
                .as_object()
                .unwrap()
                .clone();
            args.insert(field.into(), json!("forged"));
            let before = seen.lock().unwrap().len();
            let out = call_verb_in(&target, action, args, 10, root.path(), |_| {}, |_| {});
            if !matches!(out, Err(ref e) if e.kind() == "rejected")
                || seen.lock().unwrap().len() != before
            {
                failures.push(format!(
                    "{action}: {field} was not refused before mint/request"
                ));
            }
        }
    }
    stop.send(()).unwrap();
    worker.join().unwrap();
    let requests = seen.lock().unwrap();
    if requests.iter().any(|r| r["path"] == "/__platform/cli/call") {
        failures.push("a wiki action reached /__platform/cli/call".into());
    }
    for request in requests
        .iter()
        .filter(|r| r["path"] == "/__platform/wiki/call")
    {
        if request["body"]["arguments"]
            .as_object()
            .is_some_and(|args| {
                args.keys().any(|key| {
                    ["actor", "role", "org", "user", "wiki_as", "as", "principal"]
                        .contains(&key.as_str())
                })
            })
        {
            failures.push("a wiki call carried authority in arguments".into());
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn credential_for_another_org_is_never_sent_to_this_host() {
    // The stored file names ws_real/acme's endpoint; the resolved org is
    // beta — the token must not ride to a foreign host (contract I1).
    let (listener, _c, _s) = fake_worker(vec![]);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, "https://acme.cadencecloud.app");
    let foreign = RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint: format!("http://{}", listener.local_addr().unwrap()),
        org_id: "ws_other".into(),
    };
    let out = call_verb_in(&foreign, "status", Map::new(), 10, &dir, |_| {}, |_| {});
    assert!(out.unwrap_err().to_string().contains("different workspace"));
}

/// Envelopes the calls presented, in order — the value the container
/// would consume as single-use `jti`.
fn envelopes(requests: &[Value]) -> Vec<String> {
    requests
        .iter()
        .filter(|r| r["path"] == json!("/__platform/cli/call"))
        .map(|r| r["body"]["envelope"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn every_command_mints_a_fresh_envelope() {
    // Single-use contract (#744): the container consumes the envelope's
    // `jti`, so two commands must present two different mints — an
    // envelope is never reused across calls.
    let (listener, requests, _s) =
        fake_worker(vec![(200, "", json!({"a": 1})), (200, "", json!({"b": 2}))]);
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    for verb in ["status", "agent_list"] {
        call_verb_in(&target, verb, Map::new(), 10, &dir, |_| {}, |_| {}).unwrap();
    }
    let envs = envelopes(&requests.lock().unwrap());
    assert_eq!(envs.len(), 2);
    assert_ne!(envs[0], envs[1], "two commands reused one envelope");
}

#[test]
fn a_wake_retry_remints_the_envelope() {
    // The waking 503 came from the worker before any forward — the
    // envelope's jti was verified there but never consumed by a
    // container. Retrying still mints a fresh one: a 503 that fired
    // later in the pipeline would have burned the first.
    let (out, requests) = run(
        vec![
            (503, "", json!({"state": "waking", "retry_after_s": 1})),
            (200, "", json!({"ok": true})),
        ],
        "status",
        Map::new(),
        120,
    );
    assert!(out.is_ok());
    assert_eq!(hits(&requests, "/__platform/cli/authorize"), 2);
    let envs = envelopes(&requests);
    assert_eq!(envs.len(), 2);
    assert_ne!(envs[0], envs[1], "a wake retry reused the envelope");
}

/// The complete correlated receipt a `message_send` 2xx must carry
/// (CAD-1568 revision): the request's stable `message` id and canonical
/// `alias` echoed, a known lifecycle `state`, a real `duplicate` bool,
/// the immutable `workspace_id`, the sender's canonical
/// `sender_subject` and the persisted `conversation` Home thread id.
fn send_receipt(id: &str) -> Value {
    json!({
        "message": id,
        "alias": "master",
        "state": "queued",
        "duplicate": false,
        "workspace_id": ORG_ID,
        "sender_subject": "sub_owner_1",
        "sender": "cli-owner",
        "conversation": "conv_home_1",
    })
}

#[test]
fn successful_send_without_a_receipt_is_unknown_and_not_retried() {
    for receipt in [
        Value::Null,
        json!({"message":"lost-receipt","state":"waking","duplicate":false}),
        json!({"message":"another-request","state":"queued","duplicate":false}),
    ] {
        let (out, requests) = run(
            vec![(200, "", receipt)],
            "message_send",
            json!({"alias": "master", "text": "coordinate", "message": "lost-receipt"})
                .as_object()
                .unwrap()
                .clone(),
            10,
        );
        assert_eq!(out.unwrap_err().kind(), "unknown");
        assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
    }
}

#[test]
fn send_with_a_missing_or_malformed_binding_is_unknown_and_not_retried() {
    // Each corruption removes or falsifies one correlation field of the
    // complete receipt: a 2xx carrying it cannot prove THIS send landed
    // as this caller's message in this workspace's conversation, so the
    // outcome is `unknown` — and the send is still never replayed.
    let tampered = |damage: &dyn Fn(&mut Map<String, Value>)| {
        let mut receipt = send_receipt("lost-receipt");
        damage(receipt.as_object_mut().unwrap());
        receipt
    };
    let receipts = [
        tampered(&|r| {
            r.remove("workspace_id");
        }),
        tampered(&|r| {
            r["workspace_id"] = json!("ws_foreign");
        }),
        tampered(&|r| {
            r["workspace_id"] = json!("");
        }),
        tampered(&|r| {
            r.remove("sender_subject");
        }),
        tampered(&|r| {
            r["sender_subject"] = json!("");
        }),
        tampered(&|r| {
            r.remove("conversation");
        }),
        tampered(&|r| {
            r["conversation"] = json!("");
        }),
        tampered(&|r| {
            r["alias"] = json!("assistant-personal");
        }),
        tampered(&|r| {
            r.remove("sender");
        }),
        tampered(&|r| {
            r["sender"] = json!("");
        }),
        tampered(&|r| {
            r["duplicate"] = json!("yes");
        }),
        tampered(&|r| {
            r["state"] = json!("delivered");
        }),
    ];
    for receipt in receipts {
        let (out, requests) = run(
            vec![(200, "", receipt)],
            "message_send",
            json!({"alias": "master", "text": "coordinate", "message": "lost-receipt"})
                .as_object()
                .unwrap()
                .clone(),
            10,
        );
        assert_eq!(out.unwrap_err().kind(), "unknown");
        assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
    }
}

#[test]
fn send_without_expected_message_identity_is_unknown_and_not_retried() {
    let mut receipt = send_receipt("lost-receipt");
    receipt.as_object_mut().unwrap().remove("message");
    let (out, requests) = run(
        vec![(200, "", receipt)],
        "message_send",
        json!({"alias": "master", "text": "coordinate"})
            .as_object()
            .unwrap()
            .clone(),
        10,
    );
    assert_eq!(out.unwrap_err().kind(), "unknown");
    assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
}

#[test]
fn send_receipt_for_another_subject_is_unknown_and_not_retried() {
    // A login that recorded its canonical subject binds the receipt to
    // it: a 2xx answering a different `sender_subject` cannot be this
    // send's enqueue proof.
    let (listener, requests, _s) = fake_worker(vec![(200, "", send_receipt("lost-receipt"))]);
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    let path = dir.join(format!("cli-{ORG}.json"));
    let mut cred: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    cred["subject_id"] = json!("sub_the_login");
    fs::write(&path, serde_json::to_vec(&cred).unwrap()).unwrap();
    let out = call_verb_in(
        &target,
        "message_send",
        json!({"alias": "master", "text": "coordinate", "message": "lost-receipt"})
            .as_object()
            .unwrap()
            .clone(),
        10,
        &dir,
        |_| {},
        |_| {},
    );
    assert_eq!(out.unwrap_err().kind(), "unknown");
    assert_eq!(
        hits(&requests.lock().unwrap(), "/__platform/cli/call"),
        1,
        "an unknown send outcome was replayed"
    );
}

#[test]
fn a_send_wake_retry_keeps_its_durable_message_id() {
    // A pre-forward wake retry needs a fresh envelope but must not
    // mint another durable message id.
    let args: Map<String, Value> = json!({
        "alias": "master",
        "text": "please coordinate",
        "message": "cad1568-proof-1",
    })
    .as_object()
    .unwrap()
    .clone();
    let (out, requests) = run(
        vec![
            (503, "", json!({"state": "waking", "retry_after_s": 1})),
            (200, "", send_receipt("cad1568-proof-1")),
        ],
        "message_send",
        args,
        120,
    );
    out.unwrap();
    let mut calls = 0;
    for request in requests
        .iter()
        .filter(|r| r["path"] == json!("/__platform/cli/call"))
    {
        calls += 1;
        assert_eq!(request["body"]["arguments"]["message"], "cad1568-proof-1");
    }
    assert_eq!(calls, 2);
    let envs = envelopes(&requests);
    assert_ne!(envs[0], envs[1], "a send retry reused the envelope");
}

#[test]
fn cli_message_refusals_map_bounded_codes_never_server_text() {
    // CAD-1568: the container's `cli_fail` body is
    // `{ok:false,error:{code,message}}` and `message` is arbitrary
    // server text — the client answers the bounded code with its own
    // actionable text, and the server string never surfaces.
    let poisonous = |code: &str| {
        json!({"ok": false, "error": {
            "code": code,
            "message": "internal-server-text /private/path"}})
    };
    for (verb, status, code) in [
        ("message_send", 400u16, "recipient_class_unsupported"),
        ("message_send", 400, "message_id_conflict"),
        ("message_send", 403, "not_permitted"),
        ("message_read", 404, "not_found"),
    ] {
        let (out, requests) = run(vec![(status, "", poisonous(code))], verb, Map::new(), 10);
        let err = out.unwrap_err();
        let shown = err.to_string();
        assert_eq!(
            err.code(),
            Some(code),
            "{verb}: machine-readable refusal code changed"
        );
        assert!(
            !shown.contains("internal-server-text") && !shown.contains("/private/path"),
            "{verb} {code} leaked server text: {shown}"
        );
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "a refusal was retried"
        );
    }
    // An unlisted code on a message verb keeps the generic refusal —
    // never a fabricated action.
    let (out, _) = run(
        vec![(400, "", poisonous("some_future_code"))],
        "message_send",
        Map::new(),
        10,
    );
    let shown = out.unwrap_err().to_string();
    assert!(shown.contains("unknown"), "{shown}");
    assert!(!shown.contains("internal-server-text"), "{shown}");
}

#[test]
fn ticket_message_verbs_name_each_refusal_and_never_retry_it() {
    let refusal =
        |code: &str| json!({"ok": false, "error": {"code": code, "message": "server-text"}});
    for (verb, status, code) in [
        ("issue_message_send", 409u16, "message_id_conflict"),
        ("issue_message_send", 404, "not_found"),
        ("issue_messages", 404, "not_found"),
        ("issue_messages_read", 404, "not_found"),
        ("issue_message_send", 400, "invalid_request"),
        ("issue_messages", 400, "invalid_request"),
        ("issue_messages_read", 400, "invalid_request"),
        // A tenant that predates the verbs must not read as an outage.
        ("issue_message_send", 501, "not_implemented"),
        ("issue_messages", 501, "not_implemented"),
        ("issue_messages_read", 501, "not_implemented"),
    ] {
        let (out, requests) = run(vec![(status, "", refusal(code))], verb, Map::new(), 10);
        let err = out.unwrap_err();
        assert_eq!(err.code(), Some(code), "{verb} {status}: {err}");
        assert_ne!(err.kind(), "unknown", "{verb} {status}");
        assert!(!err.to_string().contains("server-text"), "{verb}: {err}");
        assert_eq!(hits(&requests, "/__platform/cli/call"), 1, "{verb} retried");
    }
}

#[test]
fn a_lost_ticket_message_answer_is_unknown_and_says_how_to_retry() {
    for (verb, note) in [
        ("issue_message_send", "same --message"),
        ("issue_messages_read", "only moves forward"),
    ] {
        let (out, requests) = run(vec![(500, "", json!({}))], verb, Map::new(), 10);
        let err = out.unwrap_err();
        assert_eq!(err.kind(), "unknown", "{verb}: {err}");
        assert!(err.to_string().contains(note), "{verb}: {err}");
        assert_eq!(hits(&requests, "/__platform/cli/call"), 1, "{verb} retried");
    }
}

#[test]
fn a_ticket_message_receipt_passes_through_as_stored() {
    let receipt = json!({
        "message": "tm-1", "id": "CAD-1", "to": "developer", "from": null,
        "sender_subject": "usr_1", "sender": "member", "text": "hi", "seq": 3,
        "created_at": 1, "duplicate": false, "state": "stored",
    });
    let sent = json!({"id": "CAD-1", "to": "developer", "message": "tm-1", "text": "hi"});
    let (out, _) = run(
        vec![(200, "", receipt.clone())],
        "issue_message_send",
        sent.as_object().unwrap().clone(),
        10,
    );
    assert_eq!(out.unwrap(), receipt);
}

#[test]
fn a_verb_a_hosted_workspace_cannot_serve_is_named_plainly() {
    // CAD-2063: these once read as an unrecognized refusal, like a fault.
    for verb in ["agent_show", "message_inbox"] {
        let body = json!({"ok": false, "error": {"code": "container_required"}});
        let (out, requests) = run(vec![(409, "", body)], verb, Map::new(), 10);
        let err = out.unwrap_err();
        assert_eq!(err.code(), Some("container_required"), "{verb}");
        assert!(err.to_string().contains("cannot serve"), "{verb}: {err}");
        assert_eq!(hits(&requests, "/__platform/cli/call"), 1, "{verb} retried");
    }
    // These two need settings.read before a call is made, so test the mapping.
    for verb in ["master_status", "settings_agents"] {
        let code = Some("container_required");
        let err = message_family_refusal(verb, 409, code, reject("generic"));
        assert_eq!(err.code(), code, "{verb}");
        assert!(err.to_string().contains("cannot serve"), "{verb}: {err}");
    }
    // Any other verb keeps the generic refusal.
    let body = json!({"ok": false, "error": {"code": "container_required"}});
    let (out, _) = run(vec![(409, "", body)], "issue_ls", Map::new(), 10);
    let shown = out.unwrap_err().to_string();
    assert!(!shown.contains("cannot serve"), "{shown}");
}

#[test]
fn a_checked_conflict_rides_up_as_the_conflict_answer() {
    // A 409 `{conflict, current_rev, card}` is the remote's checked
    // "not written" verdict — the body must reach the caller (the
    // CLI prints it and exits 5), not collapse into a generic
    // refusal. One attempt only: a conflict is never retried.
    let body = json!({"conflict": "if_rev",
        "current_rev": "fnv1a:829ba77079943a03",
        "card": {"id": "CAD-1"},
        "error": "if_rev does not match issue.md — re-read and retry"});
    let (listener, requests, _s) = fake_worker(vec![(409, "", body)]);
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    let out = call_verb_in(&target, "issue_set", Map::new(), 10, &dir, |_| {}, |_| {}).unwrap();
    let RemoteAnswer::Conflict(body) = out else {
        panic!("a 409 conflict answer must surface as Conflict, got {out:?}");
    };
    assert_eq!(body["current_rev"], json!("fnv1a:829ba77079943a03"));
    assert_eq!(body["card"]["id"], json!("CAD-1"));
    assert_eq!(
        hits(&requests.lock().unwrap(), "/__platform/cli/call"),
        1,
        "a conflict was retried"
    );
}

#[test]
fn a_bare_409_without_the_conflict_shape_is_a_generic_refusal() {
    // Only the contract's `{conflict: …}` payload is a verdict; a
    // 409 carrying anything else stays the generic refusal.
    let (out, requests) = run(
        vec![(409, "", json!({"error": "midway collision"}))],
        "issue_set",
        Map::new(),
        10,
    );
    // CAD-1434: decoded, not a bare generic message. It shows the safe
    // category, never the raw body, and is never retried.
    let shown = out.unwrap_err().to_string();
    assert!(shown.contains("unknown"), "{shown}");
    assert!(!shown.contains("midway collision"), "{shown}");
    assert!(!shown.contains("cadence login"), "{shown}");
    assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
}

#[test]
fn a_bare_503_is_availability_and_is_not_retried_or_login() {
    // A 503 without a waking state is terminal for this command and keeps the
    // saved login. A read changed nothing, so it is availability: retry later.
    // A write may have committed behind the same answer (CAD-2009), so it is
    // an unknown outcome to reconcile, never advice to retry.
    for body in [
        json!({"state": "board_unreachable"}),
        json!({"ok": false, "error": {"code": "capability_unavailable"}}),
    ] {
        let (out, requests) = run(vec![(503, "", body.clone())], "issue_ls", Map::new(), 10);
        let shown = out.unwrap_err().to_string();
        assert!(shown.contains("unavailable"), "{shown}");
        assert!(shown.contains("retry later"), "{shown}");
        assert!(!shown.contains("cadence login"), "{shown}");
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "a bare 503 on a read was retried"
        );

        let (out, requests) = run(vec![(503, "", body)], "issue_set", Map::new(), 10);
        let err = out.unwrap_err();
        assert_eq!(err.kind(), "unknown", "{err}");
        let shown = err.to_string();
        assert!(shown.contains("issue show"), "{shown}");
        assert!(!shown.contains("retry later"), "{shown}");
        assert!(!shown.contains("cadence login"), "{shown}");
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "a bare 503 on a write was retried"
        );
    }
}

/// The exact Cadence container refusal (`src/ui/wiki_envelope.rs`): a path
/// outside the signed grant is a 403 `insufficient_scope` with a message.
/// On the call path it is authorization, shows its safe code, asks for no
/// login, is sent once, and never echoes the body.
#[test]
fn a_container_403_insufficient_scope_is_authorization_on_the_call_path() {
    let body = json!({"ok": false, "error": {"code": "insufficient_scope",
        "message": "the path is outside this envelope's wiki write prefix do-not-print"}});
    let (out, requests) = run(vec![(403, "", body)], "issue_set", Map::new(), 10);
    let shown = out.unwrap_err().to_string();
    assert!(shown.contains("authorization"), "{shown}");
    assert!(shown.contains("insufficient_scope"), "{shown}");
    assert!(!shown.contains("cadence login"), "{shown}");
    assert!(!shown.contains("do-not-print"), "{shown}");
    assert_eq!(
        hits(&requests, "/__platform/cli/call"),
        1,
        "a container 403 was retried"
    );
}

/// A generic 403 (no safe code) is still an access denial: conservative
/// authorization guidance, never a login claim or an unknown-shape answer.
#[test]
fn a_generic_403_is_conservative_access_denial_not_login() {
    let (refusal, reason) = decode_refusal(403, Some(&json!({"error": {"code": "forbidden"}})));
    assert_eq!(refusal, Refusal::Authorization);
    assert_eq!(reason, None);
    let text = refusal_message(refusal, reason.as_deref(), 403);
    assert!(text.contains("not permitted"), "{text}");
    assert!(!text.contains("cadence login"), "{text}");
    let bare = decode_refusal(403, None);
    assert_eq!(bare.0, Refusal::Authorization);
    assert!(!refusal_message(bare.0, bare.1.as_deref(), 403).contains("cadence login"));
}

/// The container's `insufficient_scope` is authorization only on 403. The same
/// code on a 401 is not trusted as a policy proof, so it stays uncertain.
#[test]
fn insufficient_scope_is_authorization_only_on_403() {
    let body = json!({"error": {"code": "insufficient_scope"}});
    assert_eq!(decode_refusal(403, Some(&body)).0, Refusal::Authorization);
    assert_eq!(decode_refusal(401, Some(&body)).0, Refusal::Unknown);
}

/// The same container refusal on the blob upload path: one upload request,
/// safe code shown, no login advice, the raw body never printed.
#[test]
fn a_container_403_insufficient_scope_is_authorization_on_the_upload_path() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();
    std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let (listener, handle) = upload_fake(
        403,
        r#"{"ok":false,"error":{"code":"insufficient_scope","message":"outside prefix do-not-print"}}"#,
    );
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    store_credential(dir, &endpoint);
    let target = RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint,
        org_id: ORG_ID.into(),
    };
    let err = upload_file_in(
        dir,
        &target,
        "projects/x/a.pdf",
        "a.pdf",
        b"%PDF-bytes",
        true,
        5,
        |_| {},
    )
    .unwrap_err();
    let _ = handle.join().unwrap();
    let shown = err.to_string();
    assert!(shown.contains("authorization"), "{shown}");
    assert!(shown.contains("insufficient_scope"), "{shown}");
    assert!(!shown.contains("cadence login"), "{shown}");
    assert!(!shown.contains("do-not-print"), "{shown}");
}

#[test]
fn a_400_shows_its_safe_code_and_is_not_retried() {
    // A payload/validation refusal keeps its safe code so the caller sees
    // what was refused. It is not a login or availability claim.
    let (out, requests) = run(
        vec![(
            400,
            "",
            json!({"ok": false, "error": {"code": "payload_too_large"}}),
        )],
        "issue_set",
        Map::new(),
        10,
    );
    let shown = out.unwrap_err().to_string();
    assert!(shown.contains("payload_too_large"), "{shown}");
    assert!(!shown.contains("cadence login"), "{shown}");
    assert_eq!(
        hits(&requests, "/__platform/cli/call"),
        1,
        "a 400 was retried"
    );
}

/// The exact AOS gateway refusal (`routes.ts` `gateRefusal`): 401, code
/// `unauthorized`, plus the proven policy reason. Each is driven through the
/// real call path (`call_verb_in` -> `call_once` -> fake relay), not only the
/// decoder.
fn aos_refusal(reason: &str) -> Value {
    json!({"ok": false, "error": {"code": "unauthorized",
        "message": "Wiki call refused.", "reason": reason}})
}

#[test]
fn a_policy_401_scope_not_granted_never_recommends_login_and_is_not_retried() {
    // Paired AOS shape: 401 unauthorized + reason scope_not_granted is a
    // policy outcome. It must not say "cadence login", and exactly one
    // attempt is sent (no retry, no mutation replay).
    let (out, requests) = run(
        vec![(401, "", aos_refusal("scope_not_granted"))],
        "issue_set",
        Map::new(),
        10,
    );
    let shown = out.unwrap_err().to_string();
    assert!(!shown.contains("cadence login"), "{shown}");
    assert!(shown.contains("authorization"), "{shown}");
    assert!(shown.contains("scope_not_granted"), "{shown}");
    assert_eq!(
        hits(&requests, "/__platform/cli/call"),
        1,
        "a policy 401 was retried"
    );
}

#[test]
fn every_allowlisted_policy_reason_is_authorization_on_the_real_path() {
    for reason in ["scope_not_granted", "grant_narrowed", "binding_mismatch"] {
        let (out, requests) = run(
            vec![(401, "", aos_refusal(reason))],
            "issue_set",
            Map::new(),
            10,
        );
        let shown = out.unwrap_err().to_string();
        assert!(!shown.contains("cadence login"), "{reason}: {shown}");
        assert!(shown.contains(reason), "{reason}: {shown}");
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "{reason} retried"
        );
    }
}

#[test]
fn envelope_invalid_is_not_terminal_session_loss_and_is_not_retried() {
    // envelope_invalid (expired/legacy envelope) is neither policy nor a
    // proven session loss. It must not claim expiry or tell the user to log in.
    let (out, requests) = run(
        vec![(401, "", aos_refusal("envelope_invalid"))],
        "status",
        Map::new(),
        10,
    );
    let shown = out.unwrap_err().to_string();
    assert!(!shown.contains("cadence login"), "{shown}");
    assert!(shown.contains("did not confirm"), "{shown}");
    assert_eq!(
        hits(&requests, "/__platform/cli/call"),
        1,
        "an envelope refusal was retried"
    );
}

#[test]
fn a_generic_unauthorized_without_reason_is_uncertain_not_confirmed_expiry() {
    // A 401 unauthorized with no reason means the bearer did not resolve, but
    // the server cannot tell expiry from revocation. It is uncertain: no
    // confirmed-expiry claim, one attempt, read-only diagnosis advised.
    let (out, requests) = run(
        vec![(
            401,
            "",
            json!({"ok": false, "error": {"code": "unauthorized"}}),
        )],
        "status",
        Map::new(),
        10,
    );
    let shown = out.unwrap_err().to_string();
    assert!(shown.contains("did not confirm"), "{shown}");
    assert!(shown.contains("read-only diagnosis"), "{shown}");
    assert!(!shown.contains("has expired"), "{shown}");
    assert_eq!(
        hits(&requests, "/__platform/cli/call"),
        1,
        "a generic 401 was retried"
    );
}

#[test]
fn a_non_json_401_is_uncertain_and_never_echoes_the_body() {
    // A gateway or ingress 401 that is not the envelope (HTML, empty) must not
    // be read as confirmed expiry, and its body must not be echoed.
    let (refusal, reason) = decode_refusal(401, None);
    assert_eq!(refusal, Refusal::Uncertain);
    assert_eq!(reason, None);
    let text = refusal_message(refusal, reason.as_deref(), 401);
    assert!(text.contains("did not confirm"), "{text}");
    assert!(!text.contains("<html"), "{text}");
}

#[test]
fn only_terminal_issuer_and_session_codes_ask_for_login() {
    for code in [
        "invalid_token",
        "invalid_grant",
        "session_expired",
        "session_revoked",
        "lineage_terminal",
    ] {
        let (refusal, _) = decode_refusal(401, Some(&json!({"error": {"code": code}})));
        assert_eq!(refusal, Refusal::Authentication, "{code}");
        let text = refusal_message(refusal, None, 401);
        assert!(text.contains("cadence login"), "{code}: {text}");
    }
}

#[test]
fn an_unknown_policy_looking_reason_is_not_trusted() {
    // Only the allowlisted reasons count. A free-form or unknown reason is
    // dropped, so it can neither assert policy nor leak into the message.
    let (refusal, reason) = decode_refusal(
        401,
        Some(&json!({"error": {"code": "unauthorized", "reason": "some raw server text"}})),
    );
    assert_eq!(reason, None);
    assert_eq!(refusal, Refusal::Uncertain);
}

#[test]
fn a_board_refusal_shows_its_hop_and_request_id_and_never_the_raw_body() {
    // The AOS board names its hop and a fresh UUID. Both reach the message;
    // the raw body and any extra server text never do.
    let id = "0b2e1f4a-7c3d-4e5f-8a9b-1c2d3e4f5a6b";
    let body = json!({"ok": false, "error": {"code": "unauthorized",
        "reason": "scope_not_granted", "hop": "board", "request_id": id,
        "message": "raw server text do-not-print"}});
    let (out, requests) = run(vec![(401, "", body)], "issue_set", Map::new(), 10);
    let shown = out.unwrap_err().to_string();
    assert!(shown.contains("hop board"), "{shown}");
    assert!(shown.contains(id), "{shown}");
    assert!(!shown.contains("do-not-print"), "{shown}");
    assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
}

#[test]
fn an_unlisted_hop_or_a_non_uuid_request_id_is_dropped() {
    for body in [
        json!({"error": {"code": "unauthorized", "hop": "evil.example",
            "request_id": "0b2e1f4a-7c3d-4e5f-8a9b-1c2d3e4f5a6b"}}),
        json!({"error": {"code": "unauthorized", "hop": "board",
            "request_id": "not a uuid <script>"}}),
    ] {
        assert_eq!(refusal_correlation(Some(&body)), None, "{body}");
    }
    // A legacy envelope with no correlation still decodes exactly as before.
    assert_eq!(
        refusal_correlation(Some(&json!({"error": {"code": "unauthorized"}}))),
        None
    );
}

#[test]
fn a_renewal_refusal_uses_the_issuer_shape_not_the_gateway_envelope() {
    // Renewal reads the issuer's {error:"<code>"}. Only its terminal
    // refresh-grant refusals ask for login; a transient 503 keeps the login.
    let terminal = renewal_refusal(401, &json!({"error": "invalid_token"}), None);
    assert!(terminal.to_string().contains("cadence login"), "{terminal}");
    let transient = renewal_refusal(503, &json!({"error": "authority_unavailable"}), None);
    assert!(
        !transient.to_string().contains("cadence login"),
        "{transient}"
    );
    assert!(transient.to_string().contains("kept"), "{transient}");
}

#[test]
fn a_team_commit_uncertainty_is_an_unknown_outcome_not_a_refusal() {
    let answer = classify(
        503,
        None,
        Some(json!({"error": {"code": "mutation_outcome_uncertain"}})),
    );
    assert!(matches!(
        answer,
        Verdict::Done(Err(Error::OutcomeUnknown(_)))
    ));
}

/// A relay that answers the authorize mint, then drops the connection
/// without answering the call — the response is lost after the request
/// was dispatched.
fn dropping_worker() -> (TcpListener, std::sync::Arc<std::sync::Mutex<Vec<Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = listener.try_clone().unwrap();
    let seen: std::sync::Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
    let requests = seen.clone();
    thread::spawn(move || {
        loop {
            let Ok((socket, _)) = server.accept() else {
                return;
            };
            let (socket, req) = read_request(socket);
            requests.lock().unwrap().push(json!({
                "path": req.path, "body": req.body,
            }));
            if req.path == "/__platform/cli/authorize" {
                answer(
                    socket,
                    "200 OK",
                    "",
                    &json!({"version": CLI_VERSION, "envelope": "h.e.1",
                        "bearer": "wikienv_h.e.1", "expires_at": 0,
                        "actor": "users/o", "scope": ["cli.read"], "issuer": "i"}),
                );
                continue;
            }
            // The call was dispatched; the answer never comes.
            drop(socket);
        }
    });
    (listener, seen)
}

/// CAD-1504: a lost response to a team mutation is an unknown outcome,
/// not a rejection. The Worker may have committed the write, so the CLI
/// must not report it as refused and must not invite a blind retry.
#[test]
fn a_lost_response_to_a_team_mutation_is_unknown_not_a_refusal() {
    let (listener, _seen) = dropping_worker();
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    let err = call_verb_in(
        &target,
        "team_invite",
        json!({"email": "new@example.test", "role": "member"})
            .as_object()
            .cloned()
            .unwrap(),
        10,
        &dir,
        |_| {},
        |_| {},
    )
    .unwrap_err();
    assert_eq!(err.kind(), "unknown", "{err}");
    let text = err.to_string();
    assert!(text.contains("team list"), "{text}");
    assert!(!text.contains("retry later"), "{text}");
}

/// AOS-528: a link write answers the card it changed. A 200 naming another
/// ticket, or naming none, cannot prove this link landed, so it is an unknown
/// outcome rather than a success the caller would trust.
#[test]
fn aos528_link_receipts_must_name_the_ticket_they_changed() {
    let args = |id: &str| json!({"id": id, "kind": "blocked_by", "target": "CAD-9"});
    let receipt = |id: &str| json!({"issue": {"id": id}, "card": {"id": id}});
    for verb in ["issue_link", "issue_unlink"] {
        let sent = args("CAD-16");
        let sent = sent.as_object().unwrap();
        assert!(
            issue_receipt_correlates(verb, &receipt("CAD-16"), sent),
            "{verb}"
        );
        assert!(
            !issue_receipt_correlates(verb, &receipt("CAD-99"), sent),
            "{verb} accepted a foreign card"
        );
        assert!(
            !issue_receipt_correlates(verb, &json!({}), sent),
            "{verb} accepted an empty body"
        );
        assert!(
            !issue_receipt_correlates(verb, &json!({"card": {"id": "CAD-16"}}), sent),
            "{verb} accepted a receipt with no issue"
        );
        assert!(
            !issue_receipt_correlates(verb, &json!({"issue": {"id": "CAD-16"}}), sent),
            "{verb} accepted a receipt with no card"
        );
    }
}

/// A team 2xx whose receipt does not name the row this call changed proves
/// nothing about the write: it is the same unknown outcome a lost answer is,
/// with the directory read named and the call never replayed.
fn team_receipt_unknown(status: u16, verb: &str, args: Value, receipt: Value) {
    let (result, requests) = run(
        vec![(status, "", receipt.clone())],
        verb,
        args.as_object().unwrap().clone(),
        10,
    );
    let error = result.unwrap_err();
    assert_eq!(error.kind(), "unknown", "{verb} {receipt}: {error}");
    let text = error.to_string();
    assert!(text.contains("team list"), "{verb} {receipt}: {text}");
    assert!(!text.contains("retry later"), "{verb} {receipt}: {text}");
    assert_eq!(
        hits(&requests, "/__platform/cli/call"),
        1,
        "{verb} must not replay a write whose receipt did not prove it"
    );
}

/// CAD-2009: each platform team write answers a receipt naming the row it
/// changed — the normalized invite address, the revoked invite, the member's
/// role, the removed id. The documented success shapes ride the real
/// transport seam; a foreign or unusable receipt does not become success.
#[test]
fn cad2009_team_receipts_must_name_the_member_or_invite_they_changed() {
    let invite = |id: &str, email: &str, role: &str, status: &str| {
        json!({"invite": {
            "id": id, "email": email, "role": role, "status": status,
            "createdAt": "2026-01-01T00:00:00.000Z",
            "expiresAt": "2026-01-08T00:00:00.000Z"}})
    };
    let member = |user: &str, role: &str| {
        json!({"member": {
            "userId": user, "name": "Synthetic", "email": "synthetic@example.test",
            "role": role, "joinedAt": "2026-01-01T00:00:00.000Z"}})
    };
    let invite_args = json!({"email": " New@Example.TEST ", "role": "owner"});
    let revoke_args = json!({"invite_id": "inv_1"});
    let role_args = json!({"user_id": "usr_1", "role": "owner"});
    let remove_args = json!({"user_id": "usr_1"});

    for (status, verb, args, receipt) in [
        (
            201u16,
            "team_invite",
            invite_args.clone(),
            invite("inv_1", "new@example.test", "owner", "pending"),
        ),
        // A live pending invite already exists: the server returns it
        // unchanged, so its stored role may legitimately differ from the
        // role this call asked for.
        (
            200,
            "team_invite",
            json!({"email": "NEW@example.test", "role": "owner"}),
            invite("inv_0", "new@example.test", "member", "pending"),
        ),
        (
            200,
            "team_revoke_invite",
            revoke_args.clone(),
            invite("inv_1", "new@example.test", "member", "revoked"),
        ),
        (
            200,
            "team_set_role",
            role_args.clone(),
            member("usr_1", "owner"),
        ),
        (
            200,
            "team_remove",
            remove_args.clone(),
            json!({"removed": "usr_1"}),
        ),
    ] {
        let (result, requests) = run(
            vec![(status, "", receipt.clone())],
            verb,
            args.as_object().unwrap().clone(),
            10,
        );
        assert_eq!(result.unwrap(), receipt, "{verb} HTTP {status}");
        assert_eq!(hits(&requests, "/__platform/cli/call"), 1, "{verb}");
    }

    for (status, verb, args, receipt) in [
        (200u16, "team_invite", invite_args.clone(), json!({})),
        (
            200,
            "team_invite",
            invite_args.clone(),
            invite("inv_1", "other@example.test", "owner", "pending"),
        ),
        // A new invite must record the requested role and stay pending.
        (
            201,
            "team_invite",
            invite_args.clone(),
            invite("inv_1", "new@example.test", "member", "pending"),
        ),
        (
            201,
            "team_invite",
            invite_args.clone(),
            invite("inv_1", "new@example.test", "owner", "accepted"),
        ),
        (
            200,
            "team_invite",
            invite_args.clone(),
            invite("inv_1", "new@example.test", "owner", "accepted"),
        ),
        // Only 200 and 201 are the documented invite answers; a 202 with a
        // valid pending receipt is not proof the invite was created.
        (
            202,
            "team_invite",
            invite_args.clone(),
            invite("inv_1", "new@example.test", "owner", "pending"),
        ),
        (200, "team_revoke_invite", revoke_args.clone(), json!({})),
        (
            200,
            "team_revoke_invite",
            revoke_args.clone(),
            invite("inv_2", "new@example.test", "member", "revoked"),
        ),
        (
            200,
            "team_revoke_invite",
            revoke_args.clone(),
            invite("inv_1", "new@example.test", "member", "pending"),
        ),
        (200, "team_set_role", role_args.clone(), json!({})),
        (
            200,
            "team_set_role",
            role_args.clone(),
            member("usr_2", "owner"),
        ),
        (
            200,
            "team_set_role",
            role_args.clone(),
            member("usr_1", "member"),
        ),
        (200, "team_remove", remove_args.clone(), json!({})),
        (
            200,
            "team_remove",
            remove_args.clone(),
            json!({"removed": "usr_2"}),
        ),
        (
            200,
            "team_remove",
            remove_args.clone(),
            json!({"removed": 7}),
        ),
    ] {
        team_receipt_unknown(status, verb, args, receipt);
    }
}

fn settings_args(operation: &str, payload: Value) -> Map<String, Value> {
    json!({"operation": operation, "expected_revision": null, "payload": payload})
        .as_object()
        .unwrap()
        .clone()
}

// CAD-2009: independently authored by the fresh-session Standards reviewer.
// Authorization scope is deliberately unchanged: the hosted schema does not
// list fleet.check, but that does not make this native read a settings write.
// Current AgenticOS origin/staging 36dd174ea668e30cebe92a6fd0f6a57cc1902e98
// refuses settings_agents with 409 container_required before execution.
// These loopback peers exercise the real HTTP client, not current deployment.
fn cad2009_assert_fleet_read_failure(error: &Error, requests: &[Value]) {
    assert_eq!(error.kind(), "rejected", "{error}");
    assert_eq!(crate::error::exit_code_for_kind(error.kind()), 3);
    let diagnostic = error.to_string();
    for false_claim in [
        "outcome_unknown",
        "may already have committed",
        "may or may not have been applied",
        "settings agents show",
        "settings agents history",
    ] {
        assert!(
            !diagnostic.contains(false_claim),
            "a read failure must not claim an uncertain settings write: {diagnostic}"
        );
    }
    assert_eq!(requests.len(), 2, "no automatic replay: {requests:?}");
    assert_eq!(requests[0]["path"], "/__platform/cli/authorize");
    assert_eq!(
        requests[0]["body"]["requested_scopes"],
        json!(["settings.write"]),
        "effect classification must not change the hosted authorization contract"
    );
    assert_eq!(requests[1]["path"], "/__platform/cli/call");
    assert_eq!(requests[1]["body"]["verb"], "settings_agents");
    assert_eq!(
        requests[1]["body"]["arguments"],
        json!({"operation": "fleet.check", "expected_revision": null, "payload": {}})
    );
}

#[test]
fn cad2009_fleet_read_503_is_availability_not_mutation_uncertainty() {
    let (result, requests) = run(
        vec![(
            503,
            "",
            json!({"ok": false, "error": {"code": "capability_unavailable"}}),
        )],
        "settings_agents",
        settings_args("fleet.check", json!({})),
        10,
    );
    let error = result.unwrap_err();
    cad2009_assert_fleet_read_failure(&error, &requests);
    let diagnostic = error.to_string();
    assert!(diagnostic.contains("availability"), "{diagnostic}");
    assert!(diagnostic.contains("retry later"), "{diagnostic}");
}

#[test]
fn cad2009_fleet_read_container_required_remains_a_definite_refusal() {
    let (result, requests) = run(
        vec![(
            409,
            "",
            json!({"ok": false, "error": {"code": "container_required"}}),
        )],
        "settings_agents",
        settings_args("fleet.check", json!({})),
        10,
    );
    let error = result.unwrap_err();
    cad2009_assert_fleet_read_failure(&error, &requests);
    assert_eq!(error.code(), Some("container_required"));
}

#[test]
fn cad2009_fleet_read_lost_answer_is_not_mutation_uncertainty() {
    use std::os::fd::AsRawFd;

    fn accept_bounded(listener: &TcpListener) -> std::net::TcpStream {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "fleet HTTP request deadline elapsed");
            let mut descriptor = libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: descriptor is initialized and the listener remains live.
            let ready = unsafe {
                libc::poll(
                    &mut descriptor,
                    1,
                    remaining.as_millis().clamp(1, i32::MAX as u128) as i32,
                )
            };
            if ready < 0 {
                let error = std::io::Error::last_os_error();
                assert_eq!(error.kind(), std::io::ErrorKind::Interrupted, "{error}");
                continue;
            }
            assert_eq!(ready, 1, "CLI did not send the fleet HTTP request");
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            return socket;
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let target = target(&listener);
    let worker = thread::spawn(move || {
        let mut requests = Vec::new();
        let (socket, request) = read_request(accept_bounded(&listener));
        assert!(check_authorize(&request));
        assert_eq!(request.body["requested_scopes"], json!(["settings.write"]));
        requests.push(json!({"path": request.path, "body": request.body}));
        answer(
            socket,
            "200 OK",
            "",
            &json!({"version": CLI_VERSION, "envelope": "h.e.1"}),
        );

        let (socket, request) = read_request(accept_bounded(&listener));
        assert!(check_call(&request));
        assert_eq!(request.body["verb"], "settings_agents");
        assert_eq!(
            request.body["arguments"],
            json!({"operation": "fleet.check", "expected_revision": null, "payload": {}})
        );
        requests.push(json!({"path": request.path, "body": request.body}));
        // Complete request observed, then close without an HTTP response.
        drop(socket);
        requests
    });
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    store_credential(root.path(), &target.endpoint);
    let result = call_verb_in(
        &target,
        "settings_agents",
        settings_args("fleet.check", json!({})),
        10,
        root.path(),
        |_| {},
        |_| {},
    );
    let requests = worker.join().unwrap();
    let error = result.unwrap_err();
    cad2009_assert_fleet_read_failure(&error, &requests);
    assert!(
        error.to_string().contains("remote request failed"),
        "{error}"
    );
}

/// The native config-write result (`build_snapshot`): a revision token plus
/// the whole config document. A 2xx that lacks either is not that receipt.
fn config_snapshot(revision: &str) -> Value {
    json!({
        "revision": revision,
        "config": {
            "harnesses": [], "providers": [], "models": [], "profiles": [],
            "scope": {},
            "roleDefaults": {
                "pm": "", "research": "", "architect": "", "dev": "",
                "qa": "", "devops": "", "worker": ""
            },
            "pins": []
        },
        "observations": {"harnesses": {}, "models": {}, "profiles": {}},
        "catalogs": {},
        "history": [],
        "retiredSources": []
    })
}

fn settings_receipt_unknown(operation: &str, payload: Value, receipt: Value) {
    let (result, requests) = run(
        vec![(200, "", receipt.clone())],
        "settings_agents",
        settings_args(operation, payload),
        10,
    );
    let error = result.unwrap_err();
    assert_eq!(error.kind(), "unknown", "{operation} {receipt}: {error}");
    let text = error.to_string();
    assert!(
        !text.contains("retry later"),
        "{operation} {receipt}: {text}"
    );
    assert_eq!(
        hits(&requests, "/__platform/cli/call"),
        1,
        "{operation} must not replay a write whose receipt did not prove it"
    );
}

/// CAD-2009: a settings answer that does not prove the write must name an
/// executable read for that operation class, with the same --org, and never
/// invite a blind retry. It stays an unknown outcome.
#[test]
fn cad2009_settings_unknown_guidance_names_an_executable_read() {
    for (operation, payload, read) in [
        (
            "harness.put",
            json!({"id": "pi", "label": "Pi", "cli": "pi"}),
            "settings agents show",
        ),
        (
            "grant.revoke",
            json!({"grant_id": "sg_1"}),
            "settings agents grant list",
        ),
        ("master.resume", json!({}), "master status"),
    ] {
        let (result, requests) = run(
            vec![(200, "", json!({}))],
            "settings_agents",
            settings_args(operation, payload),
            10,
        );
        let error = result.unwrap_err();
        assert_eq!(error.kind(), "unknown", "{operation}: {error}");
        let text = error.to_string();
        assert!(text.contains(read), "{operation}: {text}");
        assert!(text.contains("--org"), "{operation}: {text}");
        assert!(!text.contains("retry later"), "{operation}: {text}");
        assert_eq!(hits(&requests, "/__platform/cli/call"), 1, "{operation}");
    }
}

/// CAD-2009: `grant.revoke` answers `{grant_id, status:"revoked"}`. A 2xx
/// that omits the id, names another grant, claims another status or carries
/// a different operation family proves nothing about this revocation.
#[test]
fn cad2009_settings_grant_revoke_requires_the_grant_it_revoked() {
    let payload = json!({"grant_id": "sg_1"});
    let revoked = json!({"grant_id": "sg_1", "status": "revoked"});
    let (result, requests) = run(
        vec![(200, "", revoked.clone())],
        "settings_agents",
        settings_args("grant.revoke", payload.clone()),
        10,
    );
    assert_eq!(result.unwrap(), revoked);
    assert_eq!(hits(&requests, "/__platform/cli/call"), 1);

    for receipt in [
        json!({}),
        json!({"grant_id": "sg_2", "status": "revoked"}),
        json!({"grant_id": "sg_1", "status": "active"}),
        config_snapshot("rev-2"),
    ] {
        settings_receipt_unknown("grant.revoke", payload.clone(), receipt);
    }
}

/// CAD-2009: a config write answers the whole config snapshot (`revision`
/// plus `config`). A bare object, a decision result or a revision without
/// the document is not that receipt.
#[test]
fn cad2009_settings_config_writes_require_a_config_snapshot() {
    let payload = json!({"id": "pi", "label": "Pi", "cli": "pi"});
    let mut snapshot = config_snapshot("rev-2");
    snapshot["config"]["harnesses"] = json!([
        {"id": "pi", "label": "Pi", "cli": "pi", "note": ""}
    ]);
    let (result, requests) = run(
        vec![(200, "", snapshot.clone())],
        "settings_agents",
        settings_args("harness.put", payload.clone()),
        10,
    );
    assert_eq!(result.unwrap(), snapshot);
    assert_eq!(hits(&requests, "/__platform/cli/call"), 1);

    for receipt in [
        json!({}),
        json!({"grant_id": "sg_1", "status": "revoked"}),
        json!({"revision": "rev-2"}),
        // A snapshot that omits the harness this call wrote contradicts it.
        config_snapshot("rev-2"),
    ] {
        settings_receipt_unknown("harness.put", payload.clone(), receipt);
    }

    // harness.remove: a full snapshot without the removed row is the
    // documented answer; a truncated `config:{}` is not proof the row is
    // absent, so it stays unknown.
    let remove_payload = json!({"id": "pi"});
    settings_receipt_ok(
        "harness.remove",
        remove_payload.clone(),
        config_snapshot("rev-3"),
    );
    settings_receipt_unknown(
        "harness.remove",
        remove_payload,
        json!({"revision": "rev-2", "config": {}}),
    );

    // harness.put binds the fields it wrote: a snapshot that still names
    // the old cli contradicts the request.
    let cli_payload = json!({"id": "claude", "label": "Claude", "cli": "tmux"});
    let mut cli_saved = config_snapshot("rev-3");
    cli_saved["config"]["harnesses"] = json!([
        {"id": "claude", "label": "Claude", "cli": "tmux", "note": ""}
    ]);
    settings_receipt_ok("harness.put", cli_payload.clone(), cli_saved);
    let mut stale_cli = config_snapshot("rev-3");
    stale_cli["config"]["harnesses"] = json!([
        {"id": "claude", "label": "Claude", "cli": "claude", "note": ""}
    ]);
    settings_receipt_unknown("harness.put", cli_payload, stale_cli);

    // provider.remove: a full snapshot without the provider is the answer;
    // a malformed row is not proof of absence.
    let provider_payload = json!({"id": "synthetic"});
    settings_receipt_ok(
        "provider.remove",
        provider_payload.clone(),
        config_snapshot("rev-3"),
    );
    let mut malformed_providers = config_snapshot("rev-3");
    malformed_providers["config"]["providers"] = json!([null]);
    settings_receipt_unknown("provider.remove", provider_payload, malformed_providers);

    // pin.put: the snapshot must carry the requested pin, not another version.
    let pin_payload = json!({"pkg": "synthetic", "pinned": "1.2.3"});
    let mut pinned = config_snapshot("rev-3");
    pinned["config"]["pins"] = json!([{"pkg": "synthetic", "pinned": "1.2.3"}]);
    settings_receipt_ok("pin.put", pin_payload.clone(), pinned);
    let mut stale_pin = config_snapshot("rev-3");
    stale_pin["config"]["pins"] = json!([{"pkg": "synthetic", "pinned": "9.9.9"}]);
    settings_receipt_unknown("pin.put", pin_payload, stale_pin);

    // model.put trims the id at the native boundary: `" m1 "` persists as
    // `m1`, and that documented snapshot row must remain a usable receipt.
    let model_payload = json!({
        "id": " m1 ", "provider": "synthetic", "ctx": "", "out": "",
        "thinking": null, "images": null
    });
    let mut model_snapshot = config_snapshot("rev-3");
    model_snapshot["config"]["providers"] = json!([{
        "id": "synthetic", "label": "Synthetic", "kind": "native CLI",
        "pin": "", "gate": false, "paid": false, "note": ""
    }]);
    model_snapshot["config"]["models"] = json!([{
        "id": "m1", "provider": "synthetic", "ctx": "", "out": "",
        "thinking": null, "images": null
    }]);
    settings_receipt_ok("model.put", model_payload, model_snapshot);
}

/// CAD-2009: `apply` commits the whole config document, so its snapshot must
/// still carry the rows the request wrote; policy-projected extras are
/// allowed, but a contradicting requested row is unknown.
#[test]
fn cad2009_settings_apply_binds_the_requested_config() {
    let role_defaults = json!({
        "pm": "", "research": "", "architect": "", "dev": "",
        "qa": "", "devops": "", "worker": ""
    });
    let config = json!({
        "harnesses": [{"id": "claude", "label": "Claude", "cli": "tmux", "note": ""}],
        "providers": [{
            "id": "synthetic", "label": "Synthetic", "kind": "Pi extension",
            "pin": "synthetic@1.2.3", "gate": true, "paid": false, "note": ""
        }],
        "models": [],
        "profiles": [],
        "scope": {},
        "roleDefaults": role_defaults.clone(),
        "pins": []
    });
    let payload = json!({"config": config});
    let mut applied = config_snapshot("rev-3");
    applied["config"]["harnesses"] = json!([
        {"id": "claude", "label": "Claude", "cli": "tmux", "note": ""}
    ]);
    // The sent explicit pins stay empty; the Pi-extension provider's pin is
    // projected into the snapshot as the derived extra pin.
    applied["config"]["providers"] = json!([{
        "id": "synthetic", "label": "Synthetic", "kind": "Pi extension",
        "pin": "synthetic@1.2.3", "gate": true, "paid": false, "note": ""
    }]);
    applied["config"]["pins"] = json!([{"pkg": "synthetic", "pinned": "1.2.3"}]);
    applied["observations"]["harnesses"] = json!({
        "claude": {"installed": null, "version": null, "live": 0}
    });
    settings_receipt_ok("apply", payload.clone(), applied);

    let mut contradicted = config_snapshot("rev-3");
    contradicted["config"]["harnesses"] = json!([
        {"id": "claude", "label": "Claude", "cli": "claude", "note": ""}
    ]);
    settings_receipt_unknown("apply", payload.clone(), contradicted);

    // An unchanged empty document is the documented apply answer.
    let empty_config = json!({
        "harnesses": [],
        "providers": [],
        "models": [],
        "profiles": [],
        "scope": {},
        "roleDefaults": role_defaults,
        "pins": []
    });
    let empty_payload = json!({"config": empty_config});
    settings_receipt_ok("apply", empty_payload.clone(), config_snapshot("rev-3"));

    // `present_overlay` never adds harnesses or profiles, so retaining an
    // omitted one contradicts the applied document.
    let mut retained_harness = config_snapshot("rev-3");
    retained_harness["config"]["harnesses"] = json!([
        {"id": "claude", "label": "Claude", "cli": "tmux", "note": ""}
    ]);
    retained_harness["observations"]["harnesses"] = json!({
        "claude": {"installed": null, "version": null, "live": 0}
    });
    settings_receipt_unknown("apply", empty_payload.clone(), retained_harness);

    let mut retained_profile = config_snapshot("rev-3");
    retained_profile["config"]["profiles"] = json!([{
        "id": "p1", "role": "dev", "model": "inherit", "job": "Synthetic job"
    }]);
    retained_profile["observations"]["profiles"] = json!({
        "p1": {"version": "abcdef123456", "usedBy": [], "history": [], "files": []}
    });
    settings_receipt_unknown("apply", empty_payload.clone(), retained_profile);

    // Two returned pins may share a package with different versions; the
    // requested full (pkg,pinned) spec must match, not the first entry with
    // that package.
    let two_pin_config = json!({
        "harnesses": [{"id": "claude", "label": "Claude", "cli": "tmux", "note": ""}],
        "providers": [{
            "id": "synthetic", "label": "Synthetic", "kind": "Pi extension",
            "pin": "synthetic@1.2.3", "gate": true, "paid": false, "note": ""
        }],
        "models": [],
        "profiles": [],
        "scope": {},
        "roleDefaults": role_defaults.clone(),
        "pins": [{"pkg": "synthetic", "pinned": "9.9.9"}]
    });
    let two_pin_payload = json!({"config": two_pin_config});
    let mut two_pins = config_snapshot("rev-3");
    two_pins["config"]["harnesses"] = json!([
        {"id": "claude", "label": "Claude", "cli": "tmux", "note": ""}
    ]);
    two_pins["config"]["providers"] = json!([{
        "id": "synthetic", "label": "Synthetic", "kind": "Pi extension",
        "pin": "synthetic@1.2.3", "gate": true, "paid": false, "note": ""
    }]);
    two_pins["config"]["pins"] = json!([
        {"pkg": "synthetic", "pinned": "1.2.3"},
        {"pkg": "synthetic", "pinned": "9.9.9"}
    ]);
    two_pins["observations"]["harnesses"] = json!({
        "claude": {"installed": null, "version": null, "live": 0}
    });
    settings_receipt_ok("apply", two_pin_payload, two_pins);

    // An extra pin with no sent explicit pin and no provider source is not a
    // documented projection.
    let mut unsourced_pin = config_snapshot("rev-3");
    unsourced_pin["config"]["pins"] = json!([{"pkg": "synthetic", "pinned": "1.2.3"}]);
    settings_receipt_unknown("apply", empty_payload.clone(), unsourced_pin);

    // A provider pin with the same package but a different version is not
    // proof of the requested full spec.
    let mut wrong_version = config_snapshot("rev-3");
    wrong_version["config"]["providers"] = json!([{
        "id": "synthetic", "label": "Synthetic", "kind": "Pi extension",
        "pin": "synthetic@1.2.3", "gate": true, "paid": false, "note": ""
    }]);
    wrong_version["config"]["pins"] = json!([{"pkg": "synthetic", "pinned": "9.9.9"}]);
    settings_receipt_unknown("apply", payload.clone(), wrong_version);

    // Exact apply: the returned document may not add a provider, model,
    // role default or scope entry the sent document did not carry.
    let exact_config = json!({
        "harnesses": [{"id": "claude", "label": "Claude", "cli": "tmux", "note": ""}],
        "providers": [{
            "id": "synthetic", "label": "Synthetic", "kind": "native CLI",
            "pin": "", "gate": false, "paid": false, "note": ""
        }],
        "models": [{
            "id": "m1", "provider": "synthetic", "ctx": "", "out": "",
            "thinking": null, "images": null
        }],
        "profiles": [],
        "scope": {},
        "roleDefaults": role_defaults.clone(),
        "pins": []
    });
    let exact_payload = json!({"config": exact_config});
    let mut exact = config_snapshot("rev-3");
    exact["config"]["harnesses"] = json!([
        {"id": "claude", "label": "Claude", "cli": "tmux", "note": ""}
    ]);
    exact["config"]["providers"] = json!([{
        "id": "synthetic", "label": "Synthetic", "kind": "native CLI",
        "pin": "", "gate": false, "paid": false, "note": ""
    }]);
    exact["config"]["models"] = json!([{
        "id": "m1", "provider": "synthetic", "ctx": "", "out": "",
        "thinking": null, "images": null
    }]);
    exact["observations"]["harnesses"] = json!({
        "claude": {"installed": null, "version": null, "live": 0}
    });
    exact["observations"]["models"] = json!({"m1": {"live": 0}});
    settings_receipt_ok("apply", exact_payload.clone(), exact.clone());

    let mut extra_provider = exact.clone();
    extra_provider["config"]["providers"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": "other", "label": "Other", "kind": "native CLI",
            "pin": "", "gate": false, "paid": false, "note": ""
        }));
    settings_receipt_unknown("apply", exact_payload.clone(), extra_provider);

    let mut extra_model = exact.clone();
    extra_model["config"]["models"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": "m2", "provider": "synthetic", "ctx": "", "out": "",
            "thinking": null, "images": null
        }));
    settings_receipt_unknown("apply", exact_payload.clone(), extra_model);

    let mut wrong_role = exact.clone();
    wrong_role["config"]["roleDefaults"]["worker"] = json!("m1");
    settings_receipt_unknown("apply", exact_payload.clone(), wrong_role);

    let mut extra_scope = exact;
    extra_scope["config"]["scope"] = json!({"m1": "off"});
    settings_receipt_unknown("apply", exact_payload, extra_scope);

    // Every provider-sourced pin must appear; a duplicate of one spec cannot
    // stand in for the missing one.
    let two_provider_config = json!({
        "harnesses": [],
        "providers": [
            {"id": "alpha", "label": "Alpha", "kind": "Pi extension",
             "pin": "alpha@1", "gate": true, "paid": false, "note": ""},
            {"id": "beta", "label": "Beta", "kind": "Pi extension",
             "pin": "beta@2", "gate": true, "paid": false, "note": ""}
        ],
        "models": [],
        "profiles": [],
        "scope": {},
        "roleDefaults": role_defaults.clone(),
        "pins": []
    });
    let two_provider_payload = json!({"config": two_provider_config});
    let mut both_pins = config_snapshot("rev-3");
    both_pins["config"]["providers"] = json!([
        {"id": "alpha", "label": "Alpha", "kind": "Pi extension",
         "pin": "alpha@1", "gate": true, "paid": false, "note": ""},
        {"id": "beta", "label": "Beta", "kind": "Pi extension",
         "pin": "beta@2", "gate": true, "paid": false, "note": ""}
    ]);
    both_pins["config"]["pins"] = json!([
        {"pkg": "alpha", "pinned": "1"},
        {"pkg": "beta", "pinned": "2"}
    ]);
    settings_receipt_ok("apply", two_provider_payload.clone(), both_pins);

    let mut duplicated_pin = config_snapshot("rev-3");
    duplicated_pin["config"]["providers"] = json!([
        {"id": "alpha", "label": "Alpha", "kind": "Pi extension",
         "pin": "alpha@1", "gate": true, "paid": false, "note": ""},
        {"id": "beta", "label": "Beta", "kind": "Pi extension",
         "pin": "beta@2", "gate": true, "paid": false, "note": ""}
    ]);
    duplicated_pin["config"]["pins"] = json!([
        {"pkg": "alpha", "pinned": "1"},
        {"pkg": "alpha", "pinned": "1"}
    ]);
    settings_receipt_unknown("apply", two_provider_payload, duplicated_pin);
}

/// CAD-2009: `pin.remove` proves the pin is gone, or that a provider row
/// still sources it; a leftover pin with no source contradicts the removal.
#[test]
fn cad2009_settings_pin_remove_proves_absence_or_a_provider_source() {
    let payload = json!({"pkg": "synthetic"});

    // The documented removal answer: the pin is absent.
    settings_receipt_ok("pin.remove", payload.clone(), config_snapshot("rev-3"));

    // A provider row carrying the same pin sources a legitimate retention.
    let mut sourced = config_snapshot("rev-3");
    sourced["config"]["providers"] = json!([{
        "id": "synthetic", "label": "Synthetic", "kind": "Pi extension",
        "pin": "synthetic@1.2.3", "gate": true, "paid": false, "note": ""
    }]);
    sourced["config"]["pins"] = json!([{"pkg": "synthetic", "pinned": "1.2.3"}]);
    settings_receipt_ok("pin.remove", payload.clone(), sourced);

    // A leftover pin with no provider source is not proof of removal.
    let mut unsourced = config_snapshot("rev-3");
    unsourced["config"]["pins"] = json!([{"pkg": "synthetic", "pinned": "1.2.3"}]);
    settings_receipt_unknown("pin.remove", payload, unsourced);
}

/// CAD-2009: the non-snapshot families are distinct — `profile.sync` nests
/// its snapshot beside `files`, so a bare snapshot or a bare `files` array
/// proves nothing about the sync.
#[test]
fn cad2009_settings_nested_and_decision_results_are_distinct_families() {
    let payload = json!({});
    let synced = json!({"snapshot": config_snapshot("rev-3"), "files": []});
    let (result, requests) = run(
        vec![(200, "", synced.clone())],
        "settings_agents",
        settings_args("profile.sync", payload.clone()),
        10,
    );
    assert_eq!(result.unwrap(), synced);
    assert_eq!(hits(&requests, "/__platform/cli/call"), 1);

    for receipt in [json!({}), json!({"files": []}), config_snapshot("rev-3")] {
        settings_receipt_unknown("profile.sync", payload.clone(), receipt);
    }

    // A named profile must exist and yield exactly its pi and claude file
    // rows; an empty or single-harness file list proves nothing.
    let named_payload = json!({"profile": "p1"});
    let mut named_snapshot = config_snapshot("rev-3");
    named_snapshot["config"]["profiles"] = json!([{
        "id": "p1", "role": "dev", "model": "inherit", "job": "Synthetic job"
    }]);
    named_snapshot["observations"]["profiles"] = json!({
        "p1": {"version": "abcdef123456", "usedBy": [], "history": [], "files": []}
    });
    let named = json!({
        "snapshot": named_snapshot.clone(),
        "files": [
            {"profile": "p1", "harness": "pi",
             "path": "/tmp/synthetic/pi/p1/AGENT.md", "status": "unmanaged"},
            {"profile": "p1", "harness": "claude",
             "path": "/tmp/synthetic/claude/p1/CLAUDE.md", "status": "in sync"}
        ]
    });
    settings_receipt_ok("profile.sync", named_payload.clone(), named);

    settings_receipt_unknown(
        "profile.sync",
        named_payload.clone(),
        json!({"snapshot": config_snapshot("rev-3"), "files": []}),
    );
    settings_receipt_unknown(
        "profile.sync",
        named_payload,
        json!({
            "snapshot": named_snapshot,
            "files": [{"profile": "p1", "harness": "pi",
                "path": "/tmp/synthetic/pi/p1/AGENT.md", "status": "unmanaged"}]
        }),
    );

    // An all-profile receipt cannot duplicate a profile row or its file rows:
    // duplicated totals must not stand in for distinct target coverage.
    let mut duplicated_profiles = config_snapshot("rev-3");
    duplicated_profiles["config"]["profiles"] = json!([
        {"id": "p1", "role": "dev", "model": "inherit", "job": "Synthetic job"},
        {"id": "p1", "role": "dev", "model": "inherit", "job": "Synthetic job"}
    ]);
    duplicated_profiles["observations"]["profiles"] = json!({
        "p1": {"version": "abcdef123456", "usedBy": [], "history": [], "files": []}
    });
    settings_receipt_unknown(
        "profile.sync",
        json!({}),
        json!({
            "snapshot": duplicated_profiles,
            "files": [
                {"profile": "p1", "harness": "pi",
                 "path": "/tmp/synthetic/pi/p1/AGENT.md", "status": "unmanaged"},
                {"profile": "p1", "harness": "claude",
                 "path": "/tmp/synthetic/claude/p1/CLAUDE.md", "status": "in sync"},
                {"profile": "p1", "harness": "pi",
                 "path": "/tmp/synthetic/pi/p1/AGENT.md", "status": "unmanaged"},
                {"profile": "p1", "harness": "claude",
                 "path": "/tmp/synthetic/claude/p1/CLAUDE.md", "status": "in sync"}
            ]
        }),
    );
}

fn settings_receipt_ok(operation: &str, payload: Value, receipt: Value) {
    let (result, requests) = run(
        vec![(200, "", receipt.clone())],
        "settings_agents",
        settings_args(operation, payload),
        10,
    );
    assert_eq!(result.unwrap(), receipt, "{operation}");
    assert_eq!(hits(&requests, "/__platform/cli/call"), 1, "{operation}");
}

/// CAD-2009: the remaining native settings families answer their own
/// documented shapes. A 2xx from another family proves nothing about the
/// operation it answered, so it stays unknown.
#[test]
fn cad2009_settings_remaining_native_result_families_are_distinct() {
    // grant.request: the server allocates the request id, so only its shape
    // is provable.
    let request_payload = json!({
        "operation": "harness.put", "expected_revision": null,
        "payload": {"id": "pi"}, "reason": "synthetic"
    });
    settings_receipt_ok(
        "grant.request",
        request_payload.clone(),
        json!({"request_id": "sr_1"}),
    );
    settings_receipt_unknown("grant.request", request_payload, json!({}));

    // grant.allow: native echoes the new grant id, its expiry and the bound
    // operation/digest — never the request id it was asked under.
    let allow_payload = json!({"request_id": "sr_1"});
    settings_receipt_ok(
        "grant.allow",
        allow_payload.clone(),
        json!({
            "grant_id": "sg_1",
            "expires_at": 1_800_000_000u64,
            "bound": {
                "operation": "harness.put",
                "payload_sha256": "0".repeat(64),
                "expected_revision": null,
                "subject": {"kind": "agent", "alias": "synthetic", "generation": "g1", "uid": 1},
                "org": "local"
            }
        }),
    );
    settings_receipt_unknown("grant.allow", allow_payload, json!({"grant_id": "sg_1"}));

    // grant.reject: the answer echoes the request id it rejected.
    let reject_payload = json!({"request_id": "sr_1"});
    settings_receipt_ok(
        "grant.reject",
        reject_payload.clone(),
        json!({"request_id": "sr_1", "status": "rejected"}),
    );
    settings_receipt_unknown(
        "grant.reject",
        reject_payload,
        json!({"request_id": "sr_2", "status": "rejected"}),
    );

    // catalog.refresh: a listing, or the documented error-shaped answer the
    // daemon returns when the provider catalog is unavailable.
    let refresh_payload = json!({"provider": "synthetic"});
    for receipt in [
        json!({
            "source": "omp models synthetic --json --no-extensions",
            "fetchedAt": "2026-01-01T00:00:00Z",
            "models": [{"id": "m1"}]
        }),
        json!({
            "source": "omp models synthetic --json --no-extensions",
            "fetchedAt": "2026-01-01T00:00:00Z",
            "models": [],
            "error": "no supported native catalog"
        }),
    ] {
        settings_receipt_ok("catalog.refresh", refresh_payload.clone(), receipt);
    }
    // The source names the provider the call asked for; another provider's
    // catalog is not this refresh's answer.
    settings_receipt_unknown(
        "catalog.refresh",
        refresh_payload.clone(),
        json!({
            "source": "omp models other --json --no-extensions",
            "fetchedAt": "2026-01-01T00:00:00Z",
            "models": []
        }),
    );
    settings_receipt_unknown(
        "catalog.refresh",
        refresh_payload,
        json!({"source": "omp models synthetic --json --no-extensions"}),
    );

    // catalog.import: the saved snapshot plus the ids it added.
    let import_payload = json!({"provider": "synthetic", "models": ["m1"]});
    let mut imported_snapshot = config_snapshot("rev-4");
    imported_snapshot["config"]["models"] = json!([{
        "id": "m1", "provider": "synthetic", "ctx": "", "out": "",
        "thinking": null, "images": null
    }]);
    settings_receipt_ok(
        "catalog.import",
        import_payload.clone(),
        json!({"snapshot": imported_snapshot, "imported": ["m1"]}),
    );
    // The ids it claims must be in the snapshot it returns, under the
    // requested provider; a valid snapshot without the row proves nothing.
    settings_receipt_unknown(
        "catalog.import",
        import_payload.clone(),
        json!({"snapshot": config_snapshot("rev-4"), "imported": ["m1"]}),
    );
    let mut foreign_provider = config_snapshot("rev-4");
    foreign_provider["config"]["models"] = json!([{
        "id": "m1", "provider": "other", "ctx": "", "out": "",
        "thinking": null, "images": null
    }]);
    settings_receipt_unknown(
        "catalog.import",
        import_payload.clone(),
        json!({"snapshot": foreign_provider, "imported": ["m1"]}),
    );

    // An id the request did not ask for is not this import's answer.
    let mut wrong_import = config_snapshot("rev-4");
    wrong_import["config"]["models"] = json!([{
        "id": "m2", "provider": "synthetic", "ctx": "", "out": "",
        "thinking": null, "images": null
    }]);
    settings_receipt_unknown(
        "catalog.import",
        import_payload.clone(),
        json!({"snapshot": wrong_import, "imported": ["m2"]}),
    );
    settings_receipt_unknown(
        "catalog.import",
        import_payload,
        json!({"imported": ["m1"]}),
    );

    // master.resume: the canonical alias, either a start or the documented
    // attention answer (a failed start is still a definite answer).
    let resume_payload = json!({});
    for receipt in [
        json!({"alias": "master", "state": "starting"}),
        json!({"alias": "master", "state": "attention"}),
    ] {
        settings_receipt_ok("master.resume", resume_payload.clone(), receipt);
    }
    settings_receipt_unknown(
        "master.resume",
        resume_payload,
        json!({"alias": "other", "state": "starting"}),
    );
}

// CAD-2009: independently authored by the ticket author; implementers must not edit.
#[test]
fn cad2009_lost_responses_to_issue_and_wiki_writes_remain_unknown() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let target = target(&listener);
    let server = listener.try_clone().unwrap();
    let worker = thread::spawn(move || {
        let mut paths = Vec::new();
        for _ in 0..6 {
            let (socket, _) = server.accept().unwrap();
            let (socket, req) = read_request(socket);
            paths.push(req.path.clone());
            match req.path.as_str() {
                "/__platform/cli/authorize" => answer(
                    socket,
                    "200 OK",
                    "",
                    &json!({"version": CLI_VERSION, "envelope": "h.e.1"}),
                ),
                "/__platform/wiki/authorize" => answer(
                    socket,
                    "200 OK",
                    "",
                    &json!({"version": WIKI_VERSION, "envelope": "h.e.1"}),
                ),
                "/__platform/cli/call" | "/__platform/wiki/call" => drop(socket),
                path => panic!("unexpected request: {path}"),
            }
        }
        paths
    });
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    store_credential(root.path(), &target.endpoint);
    for (verb, args) in [
        (
            "issue_new",
            json!({"project": "synthetic", "title": "Synthetic"}),
        ),
        ("issue_comment", json!({"id": "SYN-1", "body": "Synthetic"})),
        (
            "write",
            json!({"path": "global/synthetic.md", "text": "Synthetic", "if_rev": "none"}),
        ),
    ] {
        let error = call_verb_in(
            &target,
            verb,
            args.as_object().unwrap().clone(),
            10,
            root.path(),
            |_| {},
            |_| {},
        )
        .unwrap_err();
        assert_eq!(error.kind(), "unknown", "{verb}: {error}");
    }
    assert_eq!(
        worker.join().unwrap(),
        [
            "/__platform/cli/authorize",
            "/__platform/cli/call",
            "/__platform/cli/authorize",
            "/__platform/cli/call",
            "/__platform/wiki/authorize",
            "/__platform/wiki/call",
        ]
    );
}

// CAD-2009: a reply that proves neither success nor a pre-effect refusal
// must not be reported as success or a definite rejection.
#[test]
fn cad2009_unconfirmed_issue_writes_are_unknown_and_not_replayed() {
    for (verb, args) in [
        (
            "issue_new",
            json!({"project": "synthetic", "title": "Synthetic"}),
        ),
        ("issue_comment", json!({"id": "SYN-1", "body": "Synthetic"})),
    ] {
        for (status, body) in [(200, Value::Null), (200, json!({})), (503, json!({}))] {
            let (result, requests) = run(
                vec![(status, "", body)],
                verb,
                args.as_object().unwrap().clone(),
                10,
            );
            let error = result.unwrap_err();
            assert_eq!(error.kind(), "unknown", "{verb} HTTP {status}: {error}");
            assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
        }
    }
}

#[test]
fn cad2009_project_mutation_capability_failure_remains_unknown() {
    let body = json!({"ok": false, "error": {"code": "capability_unavailable"}});
    let args = json!({
        "project": "synthetic", "issue": "SYN-1", "repo": "example/synthetic",
        "pr": 7, "head": "0123456789abcdef0123456789abcdef01234567", "source": "synthetic"
    });
    let (result, requests) = run(
        vec![(503, "", body)],
        "audit_approve_project",
        args.as_object().unwrap().clone(),
        10,
    );
    let error = result.unwrap_err();
    assert_eq!(error.kind(), "unknown", "{error}");
    assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
}

/// CAD-2150: a board that predates `include_body` refuses the field as an
/// invalid request. Only a request that asked for the text gets the typed
/// code, so the caller can re-read without it; the plain read keeps the
/// release-order refusal.
#[test]
fn cad2150_board_without_review_text_gets_a_typed_refusal_only_when_asked() {
    let refused = json!({"ok": false, "error": {"code": "invalid_request"}});
    let mut asked = json!({"project": "synthetic"}).as_object().unwrap().clone();
    asked.insert("include_body".to_string(), json!(true));
    let (result, _) = run(
        vec![(400, "", refused.clone())],
        "audit_verdicts_project",
        asked,
        10,
    );
    match result.unwrap_err() {
        crate::error::Error::Structured(s) => {
            assert_eq!(s.code, super::VERDICT_BODY_UNSUPPORTED);
        }
        other => panic!("expected the typed code, got {other}"),
    }
    let plain = json!({"project": "synthetic"}).as_object().unwrap().clone();
    let (result, _) = run(
        vec![(400, "", refused)],
        "audit_verdicts_project",
        plain,
        10,
    );
    assert!(result.unwrap_err().to_string().contains("Release order"));
}

/// CAD-2001: a tenant that cannot serve a project-scoped *read* (503
/// capability_unavailable) is a release-order refusal — never "no evidence"
/// and never a fallback to the older repo-scoped verb. The matching write
/// stays unknown (above), because it may already have run.
#[test]
fn cad2009_project_read_capability_failure_names_the_release_order() {
    let body = json!({"ok": false, "error": {"code": "capability_unavailable"}});
    let args = json!({"project": "synthetic"});
    for verb in [
        "audit_approval_project",
        "audit_verdicts_project",
        "audit_reviewers_project",
        "audit_identities",
    ] {
        let (result, requests) = run(
            vec![(503, "", body.clone())],
            verb,
            args.as_object().unwrap().clone(),
            10,
        );
        let error = result.unwrap_err();
        assert_eq!(error.kind(), "rejected", "{verb}: {error}");
        let text = error.to_string();
        assert!(text.contains("Release order"), "{verb}: {text}");
        assert!(text.contains("repo-scoped"), "{verb}: {text}");
        assert!(!text.contains("no evidence"), "{verb}: {text}");
        assert!(!text.contains("retry later"), "{verb}: {text}");
        assert_eq!(hits(&requests, "/__platform/cli/call"), 1, "{verb}");
        for request in &requests {
            if request["path"] == json!("/__platform/cli/call") {
                assert_eq!(
                    request["body"]["verb"].as_str(),
                    Some(verb),
                    "never fall back to the older repo-scoped verb"
                );
            }
        }
    }
}

#[test]
fn cad2009_foreign_issue_receipt_does_not_confirm_a_comment() {
    let receipt = json!({"issue": {"id": "SYN-2"}, "card": {"id": "SYN-2"}, "warnings": []});
    let args = json!({"id": "SYN-1", "body": "Synthetic"});
    let (result, requests) = run(
        vec![(200, "", receipt)],
        "issue_comment",
        args.as_object().unwrap().clone(),
        10,
    );
    let error = result.unwrap_err();
    assert_eq!(error.kind(), "unknown", "{error}");
    assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
}

#[test]
fn cad2009_native_and_tenant_issue_receipts_remain_compatible() {
    let comment = json!({"id": "SYN-1", "body": "Synthetic"});
    for receipt in [
        json!({"issue": {"id": "SYN-1"}, "card": {"id": "SYN-1"}, "warnings": []}),
        json!({"issue": {"id": "SYN-1"}, "comment": {"body": "Synthetic"}}),
    ] {
        let (result, requests) = run(
            vec![(200, "", receipt.clone())],
            "issue_comment",
            comment.as_object().unwrap().clone(),
            10,
        );
        assert_eq!(result.unwrap(), receipt);
        assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
    }
    let unchanged =
        json!({"issue": {"id": "SYN-1"}, "card": {"id": "SYN-1", "rev": "same"}, "warnings": []});
    let args = json!({"ids": ["SYN-1"], "set": ["status=ready"], "if_rev": "same"});
    let (result, requests) = run(
        vec![(200, "", unchanged.clone())],
        "issue_set",
        args.as_object().unwrap().clone(),
        10,
    );
    assert_eq!(result.unwrap(), unchanged);
    assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
}

#[test]
fn cad2009_policy_receipts_must_match_the_requested_policy() {
    let args = json!({"project": "synthetic", "visibility": "workspace-visible",
        "grants": ["users/synthetic"]});
    let args = args.as_object().unwrap();
    let tenant = json!({"state": "recorded", "project": "synthetic",
        "visibility": "workspace-visible", "grants": ["users/synthetic"], "revision": 1});
    let native = json!({"project": "synthetic", "committed": true, "policy": {
        "visibility": "workspace_visible", "members": ["users/synthetic"], "version": 1}});
    assert!(policy_receipt_correlates(&tenant, args));
    assert!(policy_receipt_correlates(&native, args));
    let mut wrong_visibility = tenant.clone();
    wrong_visibility["visibility"] = json!("private");
    assert!(!policy_receipt_correlates(&wrong_visibility, args));
    let mut wrong_grants = native.clone();
    wrong_grants["policy"]["members"] = json!(["users/other"]);
    assert!(!policy_receipt_correlates(&wrong_grants, args));
    let empty = json!({"project": "synthetic", "visibility": "workspace-visible", "grants": []});
    let native_empty = json!({"project": "synthetic", "committed": true,
        "policy": {"visibility": "workspace_visible", "version": 1}});
    assert!(policy_receipt_correlates(
        &native_empty,
        empty.as_object().unwrap()
    ));
}

#[test]
fn cad2009_tracker_unavailable_before_write_is_definite() {
    let body = json!({"ok": false, "error": {"code": "tracker_unavailable"}});
    let args = json!({"id": "SYN-1", "body": "Synthetic"});
    let (result, requests) = run(
        vec![(503, "", body)],
        "issue_comment",
        args.as_object().unwrap().clone(),
        10,
    );
    let error = result.unwrap_err();
    assert_eq!(error.kind(), "rejected", "{error}");
    assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
}

// CAD-2009: independently authored by the fresh-session Standards reviewer.
#[test]
fn cad2009_unproved_issue_write_5xx_markers_are_unknown_without_replay() {
    let args = json!({"id": "SYN-1", "body": "Synthetic guard acceptance"});
    for (status, body) in [
        (503, json!({"state": "mystery"})),
        (500, json!({"state": "waking"})),
        (
            500,
            json!({"ok": false, "error": {"code": "tracker_unavailable"}}),
        ),
    ] {
        let (result, requests) = run(
            vec![(status, "", body.clone())],
            "issue_comment",
            args.as_object().unwrap().clone(),
            10,
        );
        let error = result.expect_err("an unproved 5xx cannot confirm a write or its refusal");
        assert_eq!(error.kind(), "unknown", "HTTP {status} {body}: {error}");
        let text = error.to_string();
        assert!(text.contains("issue show"), "HTTP {status} {body}: {text}");
        assert!(
            !text.contains("retry later"),
            "HTTP {status} {body}: {text}"
        );
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "HTTP {status} {body}: an unproved write must not be replayed"
        );
    }
}

// The recognized pre-forward wake keeps its authorized retry and fresh envelope.
#[test]
fn cad2009_proven_issue_write_wake_still_retries_to_correlated_success() {
    let args = json!({"id": "SYN-1", "body": "Synthetic guard acceptance"});
    let receipt = json!({
        "issue": {"id": "SYN-1"},
        "comment": {"body": "Synthetic guard acceptance"}
    });
    let (result, requests) = run(
        vec![
            (503, "", json!({"state": "waking", "retry_after_s": 0})),
            (200, "", receipt.clone()),
        ],
        "issue_comment",
        args.as_object().unwrap().clone(),
        10,
    );
    assert_eq!(
        result.expect("a proven pre-forward wake may retry"),
        receipt
    );
    assert_eq!(hits(&requests, "/__platform/cli/call"), 2);
    assert_eq!(hits(&requests, "/__platform/cli/authorize"), 2);
    let envelopes: Vec<&str> = requests
        .iter()
        .filter(|request| request["path"] == json!("/__platform/cli/call"))
        .map(|request| request["body"]["envelope"].as_str().unwrap())
        .collect();
    assert_ne!(
        envelopes[0], envelopes[1],
        "a wake retry must mint a fresh envelope"
    );
}

// CAD-2009: independently authored by the fresh-session Standards reviewer.
#[test]
fn cad2009_explicit_issue_write_uncertainty_overrides_a_wake_hint() {
    let args = json!({"id": "SYN-1", "body": "Synthetic uncertainty acceptance"});
    let receipt = json!({
        "issue": {"id": "SYN-1"},
        "comment": {"body": "Synthetic uncertainty acceptance"}
    });
    let (result, requests) = run(
        vec![
            (
                503,
                "",
                json!({
                    "state": "waking",
                    "retry_after_s": 0,
                    "error": {"code": "mutation_outcome_uncertain"}
                }),
            ),
            // A forbidden replay gets a valid answer so the failure is bounded.
            (200, "", receipt),
        ],
        "issue_comment",
        args.as_object().unwrap().clone(),
        10,
    );
    assert_eq!(
        hits(&requests, "/__platform/cli/call"),
        1,
        "explicit mutation uncertainty must prevent a wake replay"
    );
    let error = result.expect_err("explicit mutation uncertainty cannot confirm a comment");
    assert_eq!(error.kind(), "unknown", "{error}");
    let text = error.to_string();
    assert!(text.contains("issue show"), "{text}");
    assert!(!text.contains("retry later"), "{text}");
}

#[test]
fn cad2009_upload_receipts_bind_destination_and_bytes() {
    let digest = Sha256::digest(b"abc");
    let hex = format!("{digest:x}");
    let path = "global/synthetic.txt";
    let valid_blob = json!({"path": path, "sha256": hex, "size": 3});
    let valid_text = json!({"path": path, "kind": "text", "rev": "r1", "size": 3});
    let valid_native = json!({"path": path, "committed": true, "rev": "r1"});
    for receipt in [valid_blob, valid_text, valid_native] {
        assert!(upload_receipt_correlates(&receipt, path, &digest, 3));
    }
    for receipt in [
        json!({"committed": true, "rev": "r1"}),
        json!({"path": "global/other.txt", "sha256": hex}),
        json!({"path": path, "kind": "text", "rev": "r1", "size": 3, "sha256": "00"}),
        json!({"path": path, "kind": "text", "rev": "r1", "size": 3, "sha256": 3}),
        json!({"path": path, "committed": true, "rev": "r1", "size": 4}),
    ] {
        assert!(!upload_receipt_correlates(&receipt, path, &digest, 3));
    }
}

#[test]
fn cad2009_wiki_receipts_accept_server_normalized_paths() {
    let input = "global/cafe\u{301}.md";
    let canonical = "global/caf\u{e9}.md";
    let args = json!({"path": input, "text": "abc"})
        .as_object()
        .unwrap()
        .clone();
    let receipt = json!({"path": canonical, "rev": "r1", "committed": true});
    assert!(wiki_write_receipt_correlates(&receipt, &args));
    let digest = Sha256::digest(b"abc");
    assert!(upload_receipt_correlates(&receipt, input, &digest, 3));
    let foreign = json!({"path": "global/other.md", "rev": "r1", "committed": true});
    assert!(!wiki_write_receipt_correlates(&foreign, &args));
    assert!(!upload_receipt_correlates(&foreign, input, &digest, 3));
}

/// A generic 5xx with no worker `state` and no board hop follows a
/// possible commit: the platform may have written the membership change
/// and then failed to answer. It is an unknown outcome, never an
/// availability error that says "retry later".
#[test]
fn a_bare_5xx_to_a_team_mutation_is_unknown_not_availability() {
    let body = json!({"ok": false, "error": {"code": "internal_error"}});
    let (listener, _seen, _server) = fake_worker(vec![(503, "", body)]);
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    let err = call_verb_in(
        &target,
        "team_remove",
        json!({"user_id": "usr_x"}).as_object().cloned().unwrap(),
        10,
        &dir,
        |_| {},
        |_| {},
    )
    .unwrap_err();
    assert_eq!(err.kind(), "unknown", "{err}");
    let text = err.to_string();
    assert!(text.contains("team list"), "{text}");
    assert!(!text.contains("retry later"), "{text}");
}

// Independently authored from CAD-1678's lost-outcome requirement.
#[test]
fn hosted_platform_call_ambiguous_replies_require_same_key_reconciliation() {
    let args = json!({
        "operation": "call",
        "tool": "generate_image",
        "body": {"prompt": "synthetic acceptance input"},
        "key": "cad1678-acceptance-run-1"
    })
    .as_object()
    .cloned()
    .unwrap();

    for (status, body) in [
        // The tenant relay can return this after the capability request was
        // dispatched and its response was lost. It proves no non-execution.
        (
            503,
            json!({"ok": false, "error": {"code": "capability_unavailable"}}),
        ),
        // A syntactically valid JSON null is not a usable success envelope.
        // This distinct malformed-2xx case uses the existing JSON-only fake.
        (200, Value::Null),
    ] {
        let (out, requests) = run(vec![(status, "", body)], "platform_call", args.clone(), 10);
        let err = out.expect_err("an ambiguous paid call must not become a successful receipt");
        assert_eq!(err.kind(), "unknown", "HTTP {status}: {err}");
        let text = err.to_string();
        assert!(
            text.contains("charg"),
            "possible charge must be disclosed: {text}"
        );
        assert!(
            text.contains("same"),
            "reconcile the same invocation: {text}"
        );
        assert!(
            text.contains("key") || text.contains("request"),
            "reconciliation must retain the existing replay handle: {text}"
        );
        assert!(
            text.contains("replay") || text.contains("retry"),
            "the user needs same-handle reconciliation guidance: {text}"
        );
        assert!(
            !text.contains("team list"),
            "wrong operation's remedy: {text}"
        );
        assert!(
            !text.contains("team invites"),
            "wrong operation's remedy: {text}"
        );
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "an ambiguous platform call must not be automatically replayed"
        );
    }

    let (out, requests) = run(
        vec![(
            503,
            "",
            json!({"ok": false, "error": {"code": "capability_unavailable"}}),
        )],
        "platform_call",
        json!({"operation": "tools"}).as_object().cloned().unwrap(),
        10,
    );
    let err = out.unwrap_err();
    assert_eq!(err.kind(), "rejected", "listing cannot have charged: {err}");
    assert!(err.to_string().contains("availability"), "{err}");
    assert!(
        !err.to_string().contains("charg"),
        "listing is not a paid call: {err}"
    );
    assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
}

// Independently authored from the capability door's explicit uncertainty contract.
#[test]
fn hosted_platform_explicit_uncertainty_preserves_same_key_reconciliation() {
    let args = json!({
        "operation": "call",
        "tool": "generate_image",
        "body": {"prompt": "synthetic acceptance input"},
        "key": "test-key"
    })
    .as_object()
    .cloned()
    .unwrap();

    let (out, requests) = run(
        vec![(
            409,
            "",
            json!({"ok": false, "error": {"code": "uncertain"}}),
        )],
        "platform_call",
        args.clone(),
        10,
    );
    let err = out.expect_err("explicit provider uncertainty cannot be a successful receipt");
    assert_eq!(err.kind(), "unknown", "{err}");
    let text = err.to_string();
    assert!(
        text.contains("charg"),
        "possible charge must be disclosed: {text}"
    );
    assert!(
        text.contains("same"),
        "reconcile the same invocation: {text}"
    );
    assert!(
        text.contains("key") || text.contains("request"),
        "reconciliation must retain the existing replay handle: {text}"
    );
    assert!(
        text.contains("replay") || text.contains("retry"),
        "the user needs same-handle reconciliation guidance: {text}"
    );
    assert!(
        !text.contains("team list"),
        "wrong operation's remedy: {text}"
    );
    assert!(
        !text.contains("team invites"),
        "wrong operation's remedy: {text}"
    );
    assert_eq!(
        hits(&requests, "/__platform/cli/call"),
        1,
        "an uncertain platform call must not be automatically replayed"
    );

    let (out, requests) = run(
        vec![(
            409,
            "",
            json!({"ok": false, "error": {"code": "key_conflict"}}),
        )],
        "platform_call",
        args,
        10,
    );
    // The existing key-conflict response is a definite refusal, unlike a
    // revision-conflict response carrying a resynchronization document.
    let err = out.expect_err("a conflicting key must remain a definite refusal");
    assert_eq!(err.kind(), "rejected", "{err}");
    let text = err.to_string();
    assert!(text.contains("key_conflict"), "{text}");
    assert!(
        !text.contains("charg"),
        "a definite refusal must not claim possible execution: {text}"
    );
    assert_eq!(
        hits(&requests, "/__platform/cli/call"),
        1,
        "a conflicting key must not be automatically replayed"
    );
}

/// The board's own gate refusal is decided before the verb ran, so it
/// stays a definite refusal — it must not be blurred into "may have
/// committed".
#[test]
fn a_board_gate_refusal_to_a_team_mutation_stays_a_refusal() {
    let request_id = "12345678-1234-4234-8234-123456789abc";
    let body = json!({"ok": false, "error": {
        "code": "runtime_unavailable", "hop": "board", "request_id": request_id,
        "message": "do-not-print-server-text"}});
    let (listener, _seen, _server) = fake_worker(vec![(503, "", body)]);
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    let err = call_verb_in(
        &target,
        "team_set_role",
        json!({"user_id": "usr_x", "role": "member"})
            .as_object()
            .cloned()
            .unwrap(),
        10,
        &dir,
        |_| {},
        |_| {},
    )
    .unwrap_err();
    assert_eq!(err.kind(), "rejected", "{err}");
    let text = err.to_string();
    assert!(!text.contains("may already have committed"), "{text}");
}

/// A read-only team verb commits nothing, so a 5xx is plain
/// availability — the unknown-outcome treatment is for writes only.
#[test]
fn a_5xx_to_a_team_read_stays_availability() {
    let body = json!({"ok": false, "error": {"code": "internal_error"}});
    let (listener, _seen, _server) = fake_worker(vec![(503, "", body)]);
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    let err = call_verb_in(&target, "team_list", Map::new(), 10, &dir, |_| {}, |_| {}).unwrap_err();
    assert_ne!(err.kind(), "unknown", "{err}");
}

/// The platform's own explicit uncertainty code keeps its decoded
/// message — it names the code the Worker returned, which is more
/// precise than the generic ambiguous-outcome text.
#[test]
fn an_explicit_uncertainty_code_to_a_team_mutation_keeps_its_code() {
    let body = json!({"ok": false, "error": {"code": "invite_email_uncertain"}});
    let (listener, _seen, _server) = fake_worker(vec![(503, "", body)]);
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    let err = call_verb_in(
        &target,
        "team_invite",
        json!({"email": "new@example.test", "role": "member"})
            .as_object()
            .cloned()
            .unwrap(),
        10,
        &dir,
        |_| {},
        |_| {},
    )
    .unwrap_err();
    assert_eq!(err.kind(), "unknown", "{err}");
    let text = err.to_string();
    assert!(text.contains("invite_email_uncertain"), "{text}");
}

#[test]
fn local_status_is_offline_redacted_and_never_claims_live_verification() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let endpoint = "https://acme.cadencecloud.app";
    let target = RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint: endpoint.into(),
        org_id: ORG_ID.into(),
    };
    let at = now().unwrap();
    let body = json!({"version": "hosted-cadence-auth.v1", "organization_id": ORG_ID,
        "audience": endpoint, "access_token": BRIDGE, "expires_at": at + 300,
        "capabilities": ["cli.read", "cli.write"],
        "cli_session": {"session_id": "hcs_12345678-1234-4234-8234-123456789abc",
            "refresh_token": "hcr_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "generation": 1, "device_label": "cadence CLI",
            "idle_expires_at": at + 1000, "absolute_expires_at": at + 10_000}});
    std::fs::write(dir.join(format!("cli-{ORG}.json")), body.to_string()).unwrap();
    std::fs::set_permissions(
        dir.join(format!("cli-{ORG}.json")),
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    let status = local_status_at(&dir, &target);
    assert_eq!(status["state"], "renewable");
    assert_eq!(status["live_verified"], false);
    assert_eq!(status["scopes"], json!(["cli.read", "cli.write"]));
    let text = status.to_string();
    assert!(!text.contains(BRIDGE), "{text}");
    assert!(!text.contains("hcr_AAAA"), "{text}");
    assert!(!text.contains("hct_"), "{text}");
    // Freshness is a local observation, not renewal health.
    assert!(status["observed_at"].as_u64().is_some());
    assert!(status["stored_at"].as_u64().is_some());
    assert!(status.get("renewal_health").is_none(), "{text}");
}

#[test]
fn cad1434_blocker_local_status_legacy_expiry_keeps_command_refusal() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.join(format!("cli-{ORG}.json"));
    let at = now().unwrap();
    store_credential_at(dir, ORG, ORG_ID, &target.endpoint, at + 300);
    assert_eq!(
        local_status_at(dir, &target)["state"],
        "access_valid_until_expiry"
    );
    assert!(load_credential(dir, &target).is_ok());

    store_credential_at(dir, ORG, ORG_ID, &target.endpoint, at - 1);
    let mut body: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    body["capabilities"] = json!(["cli.read", "cli.write"]);
    fs::write(&path, body.to_string()).unwrap();
    let status = local_status_at(dir, &target);
    assert_eq!(status["state"], "reauthentication_required");
    assert_eq!(status["access_expires_at"], at - 1);
    assert_eq!(status["scopes"], json!(["cli.read", "cli.write"]));
    assert_eq!(status["live_verified"], false);
    assert!(status["observed_at"].as_u64().is_some());
    let text = status.to_string();
    assert!(!text.contains("hct_"), "{text}");
    assert!(!text.contains("hcr_"), "{text}");
    assert!(!text.contains(dir.to_string_lossy().as_ref()), "{text}");
    assert_eq!(
        load_credential(dir, &target).err().unwrap().to_string(),
        "the remote credential has expired — run `cadence login` again"
    );

    for (key, value) in [
        ("version", json!("invalid")),
        ("organization_id", json!("ws_other")),
        ("audience", json!("https://other.cadencecloud.app")),
        ("access_token", json!("invalid")),
        ("unexpected", json!(true)),
    ] {
        let mut invalid = body.clone();
        invalid[key] = value;
        fs::write(&path, invalid.to_string()).unwrap();
        assert_eq!(
            local_status_at(dir, &target)["state"],
            "invalid_local_credential",
            "{key}"
        );
        assert!(load_credential(dir, &target).is_err());
    }
    fs::write(&path, "not JSON").unwrap();
    assert_eq!(
        local_status_at(dir, &target)["state"],
        "invalid_local_credential"
    );
    fs::write(&path, body.to_string()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        local_status_at(dir, &target)["state"],
        "invalid_local_credential"
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        local_status_at(dir, &target)["state"],
        "invalid_local_credential"
    );
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
    let original = dir.join("saved.json");
    fs::rename(&path, &original).unwrap();
    std::os::unix::fs::symlink(&original, &path).unwrap();
    assert_eq!(
        local_status_at(dir, &target)["state"],
        "invalid_local_credential"
    );
    assert!(load_credential(dir, &target).is_err());
    assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
}

#[test]
fn a_stat_failure_is_unavailable_never_no_credential() {
    // Only a missing file is absence. Any other stat failure is unavailable,
    // not proof that nothing is stored. A file in the directory's place forces
    // ENOTDIR for every user; chmod 000 does not, because root and a process
    // holding CAP_DAC_READ_SEARCH pass it and get a plain ENOENT.
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::write(&dir, b"").unwrap();
    let target = RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint: "https://acme.cadencecloud.app".into(),
        org_id: ORG_ID.into(),
    };
    let status = local_status_at(&dir, &target);
    assert_eq!(status["state"], "unavailable", "{status}");
}

#[test]
fn an_expired_session_keeps_its_expiry_fields() {
    // The expired branch must still report the session's idle/absolute
    // expiry, so a reader sees why renewal is no longer possible.
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let endpoint = "https://acme.cadencecloud.app";
    let target = RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint: endpoint.into(),
        org_id: ORG_ID.into(),
    };
    let at = now().unwrap();
    let body = json!({"version": "hosted-cadence-auth.v1", "organization_id": ORG_ID,
        "audience": endpoint, "access_token": BRIDGE, "expires_at": at - 10,
        "capabilities": ["cli.read", "cli.write"],
        "cli_session": {"session_id": "hcs_12345678-1234-4234-8234-123456789abc",
            "refresh_token": "hcr_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "generation": 1, "device_label": "cadence CLI",
            "idle_expires_at": at - 5, "absolute_expires_at": at - 1}});
    let path = dir.join(format!("cli-{ORG}.json"));
    std::fs::write(&path, body.to_string()).unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let status = local_status_at(&dir, &target);
    assert_eq!(status["state"], "reauthentication_required");
    assert_eq!(status["session_idle_expires_at"], json!(at - 5));
    assert_eq!(status["session_absolute_expires_at"], json!(at - 1));
    assert_eq!(status["live_verified"], false);
}

/// CAD-1853: cloud-purpose admission must not be added to the default CLI
/// renewal entrypoint. This drives its real lock/load/due/authority guard;
/// the transport closure is an observable boundary, never a refusal oracle.
#[test]
fn default_cli_renewal_refuses_cloud_or_missing_cli_consent_before_any_post() {
    for scopes in [
        json!(["bridge.enroll", "cloud_agent.read", "cloud_agent.write"]),
        json!(["cli.read", "cli.write", "cloud_agent.write"]),
        json!(["cli.read"]),
    ] {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let endpoint = "https://acme.cadencecloud.app";
        let issuer = "https://api-v2.agenticos.hk";
        let target = RemoteTarget {
            identity: Some(CliIdentityPin {
                issuer: issuer.into(),
                subject_id: "user_owner".into(),
            }),
            org: ORG.into(),
            org_id: ORG_ID.into(),
            endpoint: endpoint.into(),
        };
        let at = now().unwrap();
        store_credential_at(root.path(), ORG, ORG_ID, endpoint, at - 1);
        let path = root.path().join(format!("cli-{ORG}.json"));
        let mut saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        saved["issuer"] = json!(issuer);
        saved["subject_id"] = json!("user_owner");
        saved["organization_slug"] = json!(ORG);
        saved["capabilities"] = scopes.clone();
        saved["cli_session"] = json!({
            "session_id": "hcs_12345678-1234-4234-8234-123456789abc",
            "refresh_token": format!("hcr_{}", "A".repeat(43)),
            "generation": 1,
            "device_label": "cadence CLI",
            "idle_expires_at": at + 1000,
            "absolute_expires_at": at + 10_000
        });
        fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
        fs::write(root.path().join("trusted-issuer"), format!("{issuer}\n")).unwrap();
        fs::set_permissions(
            root.path().join("trusted-issuer"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let before = fs::read(&path).unwrap();
        let posts = std::cell::Cell::new(0);
        let result = renewed_credential_with(
            root.path(),
            &target,
            Instant::now() + Duration::from_secs(5),
            |_dir, _issuer, _path, _bearer, _body, _deadline| {
                posts.set(posts.get() + 1);
                Err(Error::unknown("default CLI must not send this consent"))
            },
        );
        assert!(result.is_err(), "default CLI renewed {scopes}");
        assert_eq!(
            posts.get(),
            0,
            "default CLI sent cloud or incomplete consent"
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            before,
            "refusal published intent or authority"
        );
    }
}

#[test]
fn an_unsafe_server_code_is_dropped_from_the_decoded_reason() {
    // Free text or wrong case from a server never becomes the shown reason.
    let (refusal, reason) = decode_refusal(
        418,
        Some(&json!({"error": {"code": "Bad Code With Spaces"}})),
    );
    assert_eq!(refusal, Refusal::Unknown);
    assert_eq!(reason, None);
    let text = refusal_message(refusal, reason.as_deref(), 418);
    assert!(!text.contains("Bad Code"), "{text}");
}

#[test]
fn a_cli_only_login_stays_valid_and_a_wiki_verb_needs_its_scope() {
    // CAD-1401 repair: the stored CLI pair is a valid login and its renewal
    // keeps exactly that set; a wiki verb on it is refused before any request.
    let cli: Vec<String> = vec!["cli.read".into(), "cli.write".into()];
    assert!(valid_capabilities(&cli));
    assert!(!valid_capabilities(&["cli.read".into()]));
    assert!(same_capabilities_json(
        &json!(["cli.read", "cli.write"]),
        &cli
    ));
    assert!(!same_capabilities_json(
        &json!(["cli.read", "cli.write", "wiki.read"]),
        &cli
    ));

    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(
        root.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let target = RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint: "http://127.0.0.1:9".into(),
        org_id: ORG_ID.into(),
    };
    let body = json!({"version": "hosted-cadence-auth.v1", "organization_id": ORG_ID,
        "audience": target.endpoint, "capabilities": cli,
        "access_token": BRIDGE, "expires_at": now().unwrap() + 300});
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.path().join(format!("cli-{ORG}.json")))
        .unwrap();
    file.write_all(body.to_string().as_bytes()).unwrap();
    let mut args = Map::new();
    args.insert("path".into(), json!("global/"));
    let err =
        call_verb_in(&target, "enumerate", args, 10, root.path(), |_| {}, |_| {}).unwrap_err();
    assert!(err.to_string().contains("wiki.read capability"), "{err}");
    assert!(err.to_string().contains("nothing was sent"), "{err}");
}

/// CAD-913: a fake relay that answers one authorize and one upload. It
/// records the upload's raw multipart bytes and bearer so the test checks
/// the wire form itself: the contract fields, one `file` part, the bearer,
/// and a refusal that prints only status and code.
fn upload_fake(
    upload_status: u16,
    upload_body: &'static str,
) -> (TcpListener, thread::JoinHandle<(Vec<u8>, String, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = listener.try_clone().unwrap();
    server.set_nonblocking(true).unwrap();
    let handle = thread::spawn(move || {
        let mut seen_upload = (Vec::new(), String::new(), String::new());
        // Bounded: the client may refuse before the second request, so the
        // fake gives up on its own instead of hanging the test.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut answered = 0;
        while answered < 2 && Instant::now() < deadline {
            let socket = match server.accept() {
                Ok((socket, _)) => socket,
                Err(_) => {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let path = line.split(' ').nth(1).unwrap().to_string();
            let mut bearer = String::new();
            let mut envelope_header = String::new();
            let mut length = 0_usize;
            let mut content_type = String::new();
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let (key, value) = line.split_once(':').unwrap();
                if key.eq_ignore_ascii_case("authorization") {
                    bearer = value.trim().trim_start_matches("Bearer ").to_string();
                }
                if key.eq_ignore_ascii_case("x-cadence-wiki-envelope") {
                    envelope_header = value.trim().to_string();
                }
                if key.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap();
                }
                if key.eq_ignore_ascii_case("content-type") {
                    content_type = value.trim().to_string();
                }
            }
            let mut raw = vec![0; length];
            reader.read_exact(&mut raw).unwrap();
            answered += 1;
            if path == "/__platform/wiki/authorize" {
                let env = "eyJhIjoxfQ.eyJiIjoyfQ.c2ln";
                answer(
                    socket,
                    "200 OK",
                    "",
                    &json!({"version": WIKI_VERSION, "envelope": env}),
                );
            } else {
                assert_eq!(path, "/__platform/wiki/upload");
                assert!(content_type.starts_with("multipart/form-data; boundary="));
                seen_upload = (raw, bearer, envelope_header);
                let body: Value = serde_json::from_str(upload_body).unwrap_or(json!({}));
                answer(socket, &format!("{upload_status} X"), "", &body);
            }
        }
        seen_upload
    });
    (listener, handle)
}

#[test]
fn upload_sends_one_multipart_body_and_a_refusal_shows_only_the_code() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();
    std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    // The overwrite pre-check is off here: the test drives the upload
    // request itself, and the listing path is covered by `overwrite`.
    let (listener, handle) = upload_fake(409, r#"{"code":"too_large","secret":"do-not-print"}"#);
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    store_credential(dir, &endpoint);
    let target = RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint,
        org_id: ORG_ID.into(),
    };
    let err = upload_file_in(
        dir,
        &target,
        "sources/a.pdf",
        "a.pdf",
        b"%PDF-bytes",
        true,
        5,
        |_| {},
    )
    .unwrap_err();
    let (raw, bearer, envelope_header) = handle.join().unwrap();
    // The bearer rides the Authorization header and the envelope rides
    // X-Cadence-Wiki-Envelope. The body carries the contract fields and
    // the single file part with the raw bytes, and no envelope field.
    assert_eq!(bearer, BRIDGE);
    assert_eq!(envelope_header, "eyJhIjoxfQ.eyJiIjoyfQ.c2ln");
    let text = String::from_utf8_lossy(&raw);
    for needle in [
        "name=\"version\"\r\n\r\nhosted-cadence-wiki.v1",
        "name=\"action\"\r\n\r\nwrite_blob",
        "name=\"path\"\r\n\r\nsources/a.pdf",
        "filename=\"a.pdf\"",
    ] {
        assert!(text.contains(needle), "missing {needle}");
    }
    assert!(!text.contains("name=\"envelope\""), "envelope in body");
    assert!(!text.contains("eyJhIjoxfQ"), "envelope in body");
    assert_eq!(text.matches("name=\"file\"").count(), 1);
    // A refusal names the status and code and never the raw body.
    let shown = err.to_string();
    assert!(
        shown.contains("409") && shown.contains("too_large"),
        "{shown}"
    );
    assert!(!shown.contains("do-not-print"), "{shown}");
}

/// CAD-913: the overwrite check fails closed on any listing that may be
/// partial, and reads a plain listing exactly.
#[test]
fn overwrite_check_fails_closed_on_a_possibly_partial_listing() {
    let plain = json!({"path": "sources", "entries": [{"name": "a.pdf"}]});
    assert_eq!(listing_taken(&plain, "a.pdf"), Ok(true));
    assert_eq!(listing_taken(&plain, "b.pdf"), Ok(false));
    for paged in [
        json!({"path": "sources", "entries": [], "truncated": true}),
        json!({"path": "sources", "entries": [], "next_cursor": "c2"}),
        json!({"path": "sources", "entries": [], "has_more": true}),
    ] {
        assert!(listing_taken(&paged, "a.pdf").is_err(), "{paged}");
    }
    // A missing or non-array `entries` cannot prove the name is free.
    assert!(listing_taken(&json!({"path": "sources"}), "a.pdf").is_err());
    // Any extra key is refused, even one that reads as "complete".
    let done = json!({"path": "sources", "entries": [], "truncated": false});
    assert!(listing_taken(&done, "a.pdf").is_err());
    // An unknown paging key that carries an existing name is refused too.
    let token = json!({"path": "sources", "entries": [], "next_token": "t2"});
    assert!(listing_taken(&token, "a.pdf").is_err());
    // A non-object body cannot prove the name is free.
    assert!(listing_taken(&json!([{"name": "a.pdf"}]), "a.pdf").is_err());
}

/// CAD-913 overwrite guard: an existing destination without `--overwrite`
/// is refused from the listing alone. The fake records every request path.
/// Authorize and the listing are reads and may be sent; the test asserts
/// the upload endpoint is never reached.
#[test]
fn overwrite_refusal_sends_no_upload_request() {
    let (message, order) = refused_listing_fixture(json!({
        "path": "sources",
        "entries": [{"name": "a.pdf"}]
    }));
    assert!(message.contains("already exists"), "{message}");
    assert!(message.contains("Nothing was sent"), "{message}");
    // Order: authorize, then the listing call. No upload request at all.
    assert_eq!(
        order.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(),
        ["/__platform/wiki/authorize", "/__platform/wiki/call"]
    );
    assert_eq!(order[1].1, "enumerate");
}

/// CAD-913: an unknown paging key on the listing refuses the upload even
/// when the existing name is on that page, and sends no upload request.
#[test]
fn unknown_paging_key_refuses_without_sending_upload() {
    let (message, order) = refused_listing_fixture(json!({
        "path": "sources",
        "entries": [{"name": "a.pdf"}],
        "next_token": "page-2"
    }));
    assert!(message.contains("may be incomplete"), "{message}");
    assert!(message.contains("Nothing was sent"), "{message}");
    assert!(!message.contains("page-2"), "{message}");
    assert!(
        !order.iter().any(|(p, _)| p.ends_with("/upload")),
        "upload reached: {order:?}"
    );
}

/// Runs `upload_file_in` without `--overwrite` against a fake whose listing
/// answer is `listing`. Returns the refusal message and every request seen.
fn refused_listing_fixture(listing: Value) -> (String, Vec<(String, String)>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();
    std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = listener.try_clone().unwrap();
    server.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_fake = Arc::clone(&stop);
    let fake = thread::spawn(move || {
        let mut paths: Vec<(String, String)> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !stop_fake.load(Ordering::SeqCst) && Instant::now() < deadline {
            let socket = match server.accept() {
                Ok((socket, _)) => socket,
                Err(_) => {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let path = line.split(' ').nth(1).unwrap().to_string();
            let mut length = 0_usize;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let (key, value) = line.split_once(':').unwrap();
                if key.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut raw = vec![0; length];
            reader.read_exact(&mut raw).unwrap();
            let action = serde_json::from_slice::<Value>(&raw)
                .ok()
                .and_then(|b| b["action"].as_str().map(str::to_string))
                .unwrap_or_default();
            paths.push((path.clone(), action));
            if path == "/__platform/wiki/authorize" {
                answer(
                    socket,
                    "200 OK",
                    "",
                    &json!({"version": WIKI_VERSION, "envelope": "eyJhIjoxfQ.eyJiIjoyfQ.c2ln"}),
                );
            } else if path == "/__platform/wiki/call" {
                answer(socket, "200 OK", "", &listing);
            } else {
                answer(socket, "500 X", "", &json!({}));
            }
        }
        paths
    });
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    store_credential(dir, &endpoint);
    let target = RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint,
        org_id: ORG_ID.into(),
    };
    let err = upload_file_in(
        dir,
        &target,
        "sources/a.pdf",
        "a.pdf",
        b"%PDF-bytes",
        false,
        5,
        |_| {},
    )
    .unwrap_err();
    stop.store(true, Ordering::SeqCst);
    let paths = fake.join().unwrap();
    (err.to_string(), paths)
}
fn credentialed(listener: &TcpListener) -> (RemoteTarget, tempfile::TempDir, PathBuf) {
    let target = target(listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    (target, root, dir)
}

fn write_answer(socket: &mut std::net::TcpStream, status: &str, extra: &str, body: &Value) {
    let bytes = serde_json::to_vec(body).unwrap();
    let _ = write!(
        socket,
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n{extra}\r\n",
        bytes.len()
    );
    let _ = socket.write_all(&bytes);
    let _ = socket.flush();
}

fn finite_worker(
    n: usize,
    serve: impl Fn(usize, &mut std::net::TcpStream, &Request) + Send + 'static,
) -> (
    TcpListener,
    std::sync::Arc<std::sync::Mutex<Vec<Request>>>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = listener.try_clone().unwrap();
    let seen: std::sync::Arc<std::sync::Mutex<Vec<Request>>> = Default::default();
    let requests = seen.clone();
    let handle = thread::spawn(move || {
        for i in 0..n {
            let Ok((socket, _)) = server.accept() else {
                return;
            };
            let (mut socket, req) = {
                let (s, r) = read_request(socket);
                (s, r)
            };
            requests.lock().unwrap().push(clone_request(&req));
            serve(i, &mut socket, &req);
        }
    });
    (listener, seen, handle)
}

fn clone_request(req: &Request) -> Request {
    Request {
        path: req.path.clone(),
        bearer: req.bearer.clone(),
        body: req.body.clone(),
    }
}

/// Read an HTTP request without assuming its body is JSON; uploads are
/// multipart and the server fixture must observe the bytes as sent.
fn read_raw_request(socket: std::net::TcpStream) -> (std::net::TcpStream, String, Vec<u8>) {
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let path = line.split(' ').nth(1).unwrap().to_string();
    let mut length = 0_usize;
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap();
            }
        }
    }
    let mut raw = vec![0; length];
    reader.read_exact(&mut raw).unwrap();
    (socket, path, raw)
}

/// Bounded loopback fixture for upload deadline tests. A quiet half-second
/// after the last request ends the fixture, including when the client
/// correctly refuses before sending a later phase.
fn upload_deadline_worker(
    serve: impl Fn(&str, &mut std::net::TcpStream, &[u8]) + Send + 'static,
) -> (
    TcpListener,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = listener.try_clone().unwrap();
    server.set_nonblocking(true).unwrap();
    let paths = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = paths.clone();
    let handle = thread::spawn(move || {
        let overall = Instant::now() + Duration::from_secs(8);
        let mut last_request = Instant::now();
        while Instant::now() < overall {
            match server.accept() {
                Ok((socket, _)) => {
                    let (mut socket, path, raw) = read_raw_request(socket);
                    observed.lock().unwrap().push(path.clone());
                    serve(&path, &mut socket, &raw);
                    last_request = Instant::now();
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if !observed.lock().unwrap().is_empty()
                        && last_request.elapsed() >= Duration::from_millis(500)
                    {
                        break;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });
    (listener, paths, handle)
}

fn upload_authorize_answer(socket: &mut std::net::TcpStream) {
    write_answer(
        socket,
        "200 OK",
        "",
        &json!({"version": WIKI_VERSION, "envelope": "h.e.1"}),
    );
}

#[test]
fn upload_total_deadline_covers_authorization() {
    let (listener, paths, server) = upload_deadline_worker(|path, socket, _raw| {
        if path == "/__platform/wiki/authorize" {
            thread::sleep(Duration::from_secs(3));
            upload_authorize_answer(socket);
        } else {
            write_answer(socket, "200 OK", "", &json!({"ok": true}));
        }
    });
    let (target, _root, dir) = credentialed(&listener);
    let start = Instant::now();
    let out = upload_file_in(&dir, &target, "a.pdf", "a.pdf", b"pdf", true, 1, |_| {});
    let elapsed = start.elapsed();
    assert!(
        out.is_err(),
        "slow upload authorization unexpectedly succeeded"
    );
    assert!(
        elapsed < Duration::from_millis(2300),
        "1s upload budget took {elapsed:?}"
    );
    server.join().unwrap();
    assert_eq!(*paths.lock().unwrap(), ["/__platform/wiki/authorize"]);
}

#[test]
fn upload_total_deadline_covers_response_body() {
    let (listener, paths, server) = upload_deadline_worker(|path, socket, _raw| {
        if path == "/__platform/wiki/authorize" {
            upload_authorize_answer(socket);
        } else {
            let _ = write!(socket, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{{");
            let _ = socket.flush();
            thread::sleep(Duration::from_secs(3));
            let _ = socket.write_all(b"\"ok\":true}");
        }
    });
    let (target, _root, dir) = credentialed(&listener);
    let start = Instant::now();
    let out = upload_file_in(&dir, &target, "a.pdf", "a.pdf", b"pdf", true, 1, |_| {});
    let elapsed = start.elapsed();
    assert!(
        out.is_err(),
        "slow upload response body unexpectedly succeeded"
    );
    assert!(
        elapsed < Duration::from_millis(2300),
        "1s upload budget took {elapsed:?}"
    );
    server.join().unwrap();
    assert_eq!(
        *paths.lock().unwrap(),
        ["/__platform/wiki/authorize", "/__platform/wiki/upload"]
    );
}

#[test]
fn upload_total_deadline_includes_overwrite_preflight() {
    let wiki_authorizes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let authorizes = wiki_authorizes.clone();
    let (listener, paths, server) = upload_deadline_worker(move |path, socket, _raw| match path {
        "/__platform/wiki/authorize"
            if authorizes.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 =>
        {
            upload_authorize_answer(socket);
        }
        "/__platform/wiki/authorize" => {
            thread::sleep(Duration::from_secs(3));
            upload_authorize_answer(socket);
        }
        "/__platform/wiki/call" => {
            thread::sleep(Duration::from_millis(1200));
            write_answer(socket, "200 OK", "", &json!({"path":"a", "entries":[]}));
        }
        _ => write_answer(socket, "200 OK", "", &json!({"ok": true})),
    });
    let (target, _root, dir) = credentialed(&listener);
    let start = Instant::now();
    let out = upload_file_in(&dir, &target, "a/a.pdf", "a.pdf", b"pdf", false, 2, |_| {});
    let elapsed = start.elapsed();
    assert!(out.is_err(), "upload exceeded the shared preflight budget");
    assert!(
        elapsed < Duration::from_millis(2700),
        "2s total upload budget took {elapsed:?}"
    );
    server.join().unwrap();
    let paths = paths.lock().unwrap();
    assert_eq!(paths[0], "/__platform/wiki/authorize");
    assert_eq!(paths[1], "/__platform/wiki/call");
    assert_eq!(paths[2], "/__platform/wiki/authorize");
    assert_eq!(paths.len(), 3, "timeout must not send or replay an upload");
}

#[test]
fn a_slow_authorize_is_capped_by_the_wake_budget() {
    let (listener, seen, server) = finite_worker(1, |_i, socket, _req| {
        thread::sleep(Duration::from_secs(3));
        write_answer(
            socket,
            "200 OK",
            "",
            &json!({"version": CLI_VERSION, "envelope": "h.e.1"}),
        );
    });
    let (target, _root, dir) = credentialed(&listener);
    let start = Instant::now();
    let out = call_verb_in(&target, "status", Map::new(), 1, &dir, |_| {}, |_| {});
    let elapsed = start.elapsed();
    assert!(out.is_err());
    assert!(
        elapsed < Duration::from_secs(2),
        "a 1s budget let authorize run {elapsed:?}"
    );
    server.join().unwrap();
    let reqs = seen.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].path, "/__platform/cli/authorize");
}

#[test]
fn authorize_and_call_share_one_deadline() {
    let (listener, seen, server) = finite_worker(2, |i, socket, _req| {
        if i == 0 {
            thread::sleep(Duration::from_secs(1));
            write_answer(
                socket,
                "200 OK",
                "",
                &json!({"version": CLI_VERSION, "envelope": "h.e.1"}),
            );
        } else {
            thread::sleep(Duration::from_secs(3));
            write_answer(socket, "200 OK", "", &json!({"ok": true}));
        }
    });
    let (target, _root, dir) = credentialed(&listener);
    let start = Instant::now();
    let out = call_verb_in(&target, "status", Map::new(), 2, &dir, |_| {}, |_| {});
    let elapsed = start.elapsed();
    assert!(out.is_err());
    assert!(
        elapsed < Duration::from_millis(2750),
        "authorize+call did not share the 2s deadline: {elapsed:?}"
    );
    server.join().unwrap();
    let reqs = seen.lock().unwrap();
    assert_eq!(reqs.len(), 2);
    assert!(check_authorize(&reqs[0]), "authorize schema");
    assert!(check_call(&reqs[1]), "call schema");
}

#[test]
fn call_response_body_is_bounded_by_remaining_deadline() {
    let (listener, _seen, server) = finite_worker(2, |i, socket, _req| {
        if i == 0 {
            write_answer(
                socket,
                "200 OK",
                "",
                &json!({"version": CLI_VERSION, "envelope": "h.e.1"}),
            );
        } else {
            let body = serde_json::to_vec(&json!({"ok": true})).unwrap();
            let _ = write!(
                socket,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(&body[..1]);
            let _ = socket.flush();
            thread::sleep(Duration::from_secs(3));
            let _ = socket.write_all(&body[1..]);
        }
    });
    let (target, _root, dir) = credentialed(&listener);
    let start = Instant::now();
    let out = call_verb_in(&target, "status", Map::new(), 1, &dir, |_| {}, |_| {});
    let elapsed = start.elapsed();
    assert!(out.is_err());
    assert!(
        elapsed < Duration::from_secs(2),
        "a 1s budget let the body read run {elapsed:?}"
    );
    server.join().unwrap();
}

#[test]
fn a_zero_budget_never_sends_a_byte() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let (target, _root, dir) = credentialed(&listener);
    let out = call_verb_in(&target, "status", Map::new(), 0, &dir, |_| {}, |_| {});
    assert!(out.is_err());
    assert!(
        matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "a dead budget opened a socket"
    );
}

#[test]
fn a_shared_expired_deadline_cannot_restart_the_wake_budget() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let (target, _root, dir) = credentialed(&listener);
    let deadline = Instant::now() - Duration::from_secs(1);
    let error = call_verb_until(
        &target,
        "read",
        Map::new(),
        deadline,
        60,
        &dir,
        |_| {},
        |_| {},
    )
    .unwrap_err();
    assert_eq!(error.kind(), "busy");
    assert_eq!(error.code(), Some("waking"));
    assert!(
        matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "a continuation restarted its expired budget"
    );
}

#[test]
fn a_wake_sleep_that_would_pass_the_deadline_is_the_retryable_busy() {
    let (out, _requests) = run(
        vec![
            (503, "", json!({"state": "waking", "retry_after_s": 5})),
            (503, "", json!({"state": "waking", "retry_after_s": 5})),
        ],
        "status",
        Map::new(),
        2,
    );
    let err = out.unwrap_err();
    assert_eq!(err.kind(), "busy");
    assert_eq!(err.code(), Some("waking"));
}

#[test]
fn transfer_ops_stay_on_write_or_job_status_and_refuse_authority() {
    let begin = json!({
        "path": "projects/example",
        "transfer": {
            "op": "begin",
            "manifest": {"version": 1, "root": "projects/example", "entries": []}
        }
    });
    assert!(wiki_transfer_shape("write", begin.as_object().unwrap()).is_ok());
    let mut mixed = begin.as_object().unwrap().clone();
    mixed.insert("text".into(), json!("no"));
    let err = wiki_transfer_shape("write", &mixed).unwrap_err();
    assert!(err.to_string().contains("never sent"), "{err}");
    let export = json!({
        "path": "projects/example/a.md",
        "transfer": {"op": "export", "path": "projects/example/a.md"}
    });
    assert!(wiki_transfer_shape("job_status", export.as_object().unwrap()).is_ok());
    let outside = json!({
        "path": "projects/example",
        "transfer": {"op": "export", "path": "projects/example/a.md"}
    });
    let err = wiki_transfer_shape("job_status", outside.as_object().unwrap()).unwrap_err();
    assert!(err.to_string().contains("outer path"), "{err}");
    let binding = json!({
        "path": "projects/example",
        "transfer": {"op": "status", "root": "projects/example", "_transfer_binding": {}}
    });
    let err = wiki_transfer_shape("job_status", binding.as_object().unwrap()).unwrap_err();
    assert!(err.to_string().contains("never sent"), "{err}");
    let forged = json!({
        "path": "projects/example",
        "transfer": {"op": "status", "actor": "operator"}
    });
    let err = wiki_transfer_shape("job_status", forged.as_object().unwrap()).unwrap_err();
    assert!(err.to_string().contains("never sent"), "{err}");
    let err = wiki_transfer_shape(
        "job_status",
        json!({"path": "projects/example", "transfer": {"op": "begin"}})
            .as_object()
            .unwrap(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("not allowed"), "{err}");
}

#[test]
fn hosted_platform_in_progress_replay_preserves_same_key_reconciliation() {
    let args = json!({
        "operation": "call",
        "tool": "generate_image",
        "body": {"prompt": "synthetic in-progress acceptance input"},
        "key": "cad1678-in-progress-acceptance-1"
    })
    .as_object()
    .cloned()
    .unwrap();
    // The door can answer this while another attempt with this key still
    // owns the funded dispatch. It does not prove the operation was rejected.
    let body = json!({
        "ok": false,
        "error": {"code": "idempotency_in_progress"}
    });
    let (listener, requests, _server) = fake_worker(vec![(409, "", body)]);
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);

    let out = call_verb_in(
        &target,
        "platform_call",
        args.clone(),
        10,
        &dir,
        |_| {},
        |_| panic!("an in-progress replay must not be automatically retried"),
    );
    let requests = requests.lock().unwrap().clone();
    assert_eq!(
        hits(&requests, "/__platform/cli/call"),
        1,
        "an in-progress replay must make exactly one forwarding attempt"
    );

    let err = out.expect_err("in-progress funding is not a completed receipt");
    assert_eq!(
        err.kind(),
        "unknown",
        "the original call can still complete and charge; it is not rejected: {err}"
    );
    let text = err.to_string().to_ascii_lowercase();
    assert!(
        text.contains("charg"),
        "the possible charge must be disclosed: {text}"
    );
    assert!(
        text.contains("same"),
        "reconciliation must preserve the same invocation: {text}"
    );
    assert!(
        text.contains("key") || text.contains("request"),
        "reconciliation must retain the original replay handle: {text}"
    );
    assert!(
        text.contains("replay") || text.contains("retry"),
        "the user needs same-handle replay guidance: {text}"
    );
    assert!(
        !text.contains("the remote refused the request"),
        "a possible funded dispatch must not be described as definitely refused: {text}"
    );
    assert!(
        !text.contains("team list") && !text.contains("team invites"),
        "the remedy must reconcile this tool call, not an unrelated team mutation: {text}"
    );
}

#[test]
fn hosted_connected_read_in_progress_is_an_unknown_outcome() {
    let args = json!({
        "operation": "connected_read",
        "tool": "search_console_report",
        "connectionId": "con_synthetic",
        "body": {"site_url": "sc-domain:example.test"},
        "key": "synthetic-connected-read-1"
    })
    .as_object()
    .cloned()
    .unwrap();
    let body = json!({"ok": false, "error": {"code": "idempotency_in_progress"}});
    let (listener, requests, _server) = fake_worker(vec![(409, "", body)]);
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);

    let err = call_verb_in(
        &target,
        "platform_call",
        args,
        10,
        &dir,
        |_| {},
        |_| panic!("an in-progress read must not be automatically retried"),
    )
    .expect_err("an in-progress read has no final receipt");
    assert_eq!(err.kind(), "unknown", "{err}");
    assert!(err.to_string().contains("platform status"), "{err}");
    assert_eq!(hits(&requests.lock().unwrap(), "/__platform/cli/call"), 1);
}

#[test]
fn hosted_connected_read_without_a_receipt_is_an_unknown_outcome() {
    let args = json!({
        "operation": "connected_read",
        "tool": "search_console_report",
        "connectionId": "con_synthetic",
        "body": {"site_url": "sc-domain:example.test"},
        "key": "synthetic-connected-read-2"
    })
    .as_object()
    .cloned()
    .unwrap();
    for body in [
        json!({"ok": true}),
        json!({"ok": true, "data": {}}),
        json!({"ok": true, "data": {"repeated": true, "result": null}}),
        json!({"ok": true, "data": {"repeated": true, "result": {"rows": []}}}),
        json!({"ok": true, "data": {
            "repeated": true, "result": {"rows": []}, "effect": "read",
            "price": {"currency": "USD", "scale": 6, "amount": "invalid"}
        }}),
    ] {
        let (listener, requests, _server) = fake_worker(vec![(200, "", body)]);
        let target = target(&listener);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        store_credential(&dir, &target.endpoint);

        let err = call_verb_in(
            &target,
            "platform_call",
            args.clone(),
            10,
            &dir,
            |_| {},
            |_| panic!("a missing receipt must not be automatically retried"),
        )
        .expect_err("a missing receipt does not prove the read failed");
        assert_eq!(err.kind(), "unknown", "{err}");
        assert!(err.to_string().contains("platform status"), "{err}");
        assert_eq!(hits(&requests.lock().unwrap(), "/__platform/cli/call"), 1);
    }
}

#[test]
fn hosted_connected_read_accepts_a_complete_settled_receipt_and_replay() {
    let args = json!({
        "operation": "connected_read",
        "tool": "search_console_report",
        "connectionId": "con_synthetic",
        "body": {"site_url": "sc-domain:example.test"},
        "key": "synthetic-connected-read-3"
    })
    .as_object()
    .cloned()
    .unwrap();
    for repeated in [false, true] {
        let receipt = json!({"ok": true, "data": {
            "repeated": repeated,
            "result": {"rows": []},
            "effect": "read",
            "price": {"currency": "USD", "scale": 6, "amount": "0.000025"}
        }});
        let (listener, requests, _server) = fake_worker(vec![(200, "", receipt.clone())]);
        let target = target(&listener);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        store_credential(&dir, &target.endpoint);
        let answer = call_verb_in(
            &target,
            "platform_call",
            args.clone(),
            10,
            &dir,
            |_| {},
            |_| panic!("a settled read must not be automatically retried"),
        )
        .expect("a complete settled receipt is accepted");
        assert!(matches!(answer, RemoteAnswer::Ok(body) if body == receipt));
        assert_eq!(hits(&requests.lock().unwrap(), "/__platform/cli/call"), 1);
    }
}

/// CAD-2226: the three owner-enrollment operations ride `platform_call`. A 2xx
/// is printed only when it carries the complete documented row shape; an
/// incomplete read is a refusal, never an invented empty list. Unknown additive
/// fields stay accepted so a newer server is not frozen out.
#[test]
fn hosted_connection_reads_require_the_complete_documented_rows() {
    let provider = json!({
        "provider": "google_search_console",
        "descriptor_available": false,
        "manifest_status": "missing",
        "reviewed_pin": null,
        "reported_pin": null,
        "enrollment": "available",
        "read": {
            "slug": "google_search_console",
            "qualified": true,
            "pinned": true,
            "projected": true
        }
    });
    let connection = json!({
        "id": "con_synthetic",
        "provider": "google_search_console",
        "account": "",
        "kind": "enrolled",
        "revision": null,
        "registration_digest": null,
        "scopes": [],
        "status": {
            "adapter_registered": true,
            "descriptor_available": true,
            "custody_available": true,
            "manifest_status": "matched",
            "reviewed_pin": "google-search-console@1",
            "reported_pin": "google-search-console@1",
            "execution_authority": true,
            "network_checked": false
        }
    });
    let without = |row: &Value, key: &str| {
        let mut row = row.clone();
        row.as_object_mut().unwrap().remove(key);
        row
    };
    let mut bad_read = provider.clone();
    bad_read["read"]
        .as_object_mut()
        .unwrap()
        .remove("projected");
    let mut bad_descriptor = provider.clone();
    bad_descriptor["descriptor_available"] = json!("yes");
    let mut bad_status = connection.clone();
    bad_status["status"]
        .as_object_mut()
        .unwrap()
        .remove("custody_available");
    let mut bad_kind = connection.clone();
    bad_kind["kind"] = json!("enrolled_elsewhere");
    let mut bad_scopes = connection.clone();
    bad_scopes["scopes"] = json!([1]);
    let mut additive_provider = provider.clone();
    additive_provider
        .as_object_mut()
        .unwrap()
        .insert("future".into(), json!(true));
    let mut additive_connection = connection.clone();
    additive_connection
        .as_object_mut()
        .unwrap()
        .insert("future".into(), json!(true));

    let cases: [(&str, &str, Value, Vec<Value>); 2] = [
        (
            "connection_providers",
            "providers",
            additive_provider,
            vec![
                json!({}),
                without(&provider, "enrollment"),
                bad_read,
                bad_descriptor,
                json!("google_search_console"),
            ],
        ),
        (
            "connection_list",
            "connections",
            additive_connection,
            vec![
                json!({}),
                without(&connection, "revision"),
                bad_status,
                bad_kind,
                bad_scopes,
                json!("con_synthetic"),
            ],
        ),
    ];
    for (operation, field, good, bad) in cases {
        let args = json!({ "operation": operation })
            .as_object()
            .cloned()
            .unwrap();
        let call = |body: Value| {
            let (listener, requests, _server) = fake_worker(vec![(200, "", body)]);
            let target = target(&listener);
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("a");
            std::fs::create_dir(&dir).unwrap();
            std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
                .unwrap();
            store_credential(&dir, &target.endpoint);
            let answer = call_verb_in(
                &target,
                "platform_call",
                args.clone(),
                10,
                &dir,
                |_| {},
                |_| panic!("a read is never retried"),
            );
            assert_eq!(hits(&requests.lock().unwrap(), "/__platform/cli/call"), 1);
            answer
        };
        for row in bad {
            let err = call(json!({"ok": true, "data": { field: [row] }}))
                .expect_err("an incomplete row proves nothing");
            assert_eq!(err.kind(), "rejected", "{err}");
            let text = err.to_string().to_ascii_lowercase();
            assert!(text.contains("incomplete"), "{text}");
            assert!(
                !text.contains("no connections") && !text.contains("no providers"),
                "an unusable answer must not become an empty list: {text}"
            );
        }
        // A legitimate empty list and a row with tolerable additive fields
        // both stay valid.
        for body in [
            json!({"ok": true, "data": { field: [] }}),
            json!({"ok": true, "data": { field: [good.clone()], "future": true }}),
        ] {
            let answer = call(body.clone()).expect("a complete answer is accepted");
            assert!(matches!(answer, RemoteAnswer::Ok(received) if received == body));
        }
    }
}

/// Enrollment is a mutation: a 2xx without a verified HTTPS link leaves the
/// outcome unknown, so this client neither claims a link nor repeats the call.
#[test]
fn hosted_connection_start_requires_an_https_link_and_never_retries() {
    let args = json!({
        "operation": "connection_start",
        "body": {"toolkit": "google_search_console"}
    })
    .as_object()
    .cloned()
    .unwrap();
    for body in [
        json!({"ok": true}),
        json!({"ok": true, "data": {}}),
        json!({"ok": true, "data": {"url": "http://provider.example.test/oauth"}}),
        json!({"ok": true, "data": {"url": "/oauth"}}),
        json!({"ok": true, "data": {"url": "https://"}}),
        json!({"ok": true, "data": {"url": "https://provider.example.test:999999/oauth"}}),
        json!({"ok": true, "data": {"url": "https://provider.example.test:notaport/x"}}),
        json!({"ok": true, "data": {"url": "https://user:secret@provider.example.test/x"}}),
        json!({"ok": true, "data": {"url": "not-a-url"}}),
        json!({"ok": false, "data": {"url": "https://provider.example.test/oauth"}}),
    ] {
        let (listener, requests, _server) = fake_worker(vec![(200, "", body.clone())]);
        let target = target(&listener);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        store_credential(&dir, &target.endpoint);

        let err = call_verb_in(
            &target,
            "platform_call",
            args.clone(),
            10,
            &dir,
            |_| {},
            |_| panic!("an enrollment start is never retried automatically"),
        )
        .expect_err("an unverified link is not a success");
        assert_eq!(err.kind(), "unknown", "{err}");
        let text = err.to_string().to_ascii_lowercase();
        assert!(
            text.contains("connection ls"),
            "the link that may exist must be reconciled by a read: {text}"
        );
        assert!(
            !text.contains("https://provider"),
            "a failed answer must not echo a link it did not verify: {text}"
        );
        assert_eq!(hits(&requests.lock().unwrap(), "/__platform/cli/call"), 1);
    }

    let receipt = json!({"ok": true, "data": {
        "url": "https://provider.example.test/oauth?synthetic=1"
    }});
    let (listener, requests, _server) = fake_worker(vec![(200, "", receipt.clone())]);
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);
    let answer = call_verb_in(
        &target,
        "platform_call",
        args,
        10,
        &dir,
        |_| {},
        |_| panic!("an enrollment start is never retried automatically"),
    )
    .expect("a verified HTTPS link is the success");
    assert!(matches!(answer, RemoteAnswer::Ok(body) if body == receipt));
    assert_eq!(hits(&requests.lock().unwrap(), "/__platform/cli/call"), 1);
}

/// A hosted server that predates these operations answers the unknown
/// operation. That is a release-order refusal, not an empty list and not a
/// missing link, and it never falls back to the local daemon.
#[test]
fn hosted_connection_operations_refuse_honestly_on_an_older_server() {
    for (operation, field) in [
        ("connection_providers", "providers"),
        ("connection_list", "connections"),
        ("connection_start", "url"),
    ] {
        let args = if operation == "connection_start" {
            json!({"operation": operation, "body": {"toolkit": "google_search_console"}})
        } else {
            json!({ "operation": operation })
        }
        .as_object()
        .cloned()
        .unwrap();
        for (status, code) in [
            (501u16, None),
            (400, Some("invalid_request")),
            (400, Some("unknown_verb")),
        ] {
            let body = match code {
                Some(code) => json!({"ok": false, "error": {"code": code}}),
                None => json!({"ok": false, "error": {"code": "not_implemented"}}),
            };
            let (listener, requests, _server) = fake_worker(vec![(status, "", body)]);
            let target = target(&listener);
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("a");
            std::fs::create_dir(&dir).unwrap();
            std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
                .unwrap();
            store_credential(&dir, &target.endpoint);

            let err = call_verb_in(
                &target,
                "platform_call",
                args.clone(),
                10,
                &dir,
                |_| {},
                |_| panic!("an older server's refusal is not retried"),
            )
            .expect_err("an older server cannot serve the operation");
            assert_eq!(err.kind(), "rejected", "{err}");
            let text = err.to_string().to_ascii_lowercase();
            assert!(
                text.contains("update the hosted server first"),
                "{operation} {status}: {text}"
            );
            assert!(
                text.contains(operation),
                "the refusal names the operation the server lacks: {text}"
            );
            if operation == "connection_start" {
                assert!(
                    text.contains("no authorization link"),
                    "an older server must not yield an invented link: {text}"
                );
            } else {
                assert!(
                    text.contains("not an empty list"),
                    "an older server must not yield an invented empty {field}: {text}"
                );
            }
            assert_eq!(hits(&requests.lock().unwrap(), "/__platform/cli/call"), 1);
        }
    }
}

/// An uncertain enrollment start is the same unresolved outcome a lost
/// response is: never a link, never an automatic second attempt.
#[test]
fn hosted_connection_start_uncertainty_is_an_unknown_outcome() {
    let args = json!({
        "operation": "connection_start",
        "body": {"toolkit": "google_search_console"}
    })
    .as_object()
    .cloned()
    .unwrap();
    for (status, body) in [
        (409u16, json!({"ok": false, "error": {"code": "uncertain"}})),
        (
            409,
            json!({"ok": false, "error": {"code": "idempotency_in_progress"}}),
        ),
        (
            500,
            json!({"ok": false, "error": {"code": "provider_unavailable"}}),
        ),
    ] {
        let (listener, requests, _server) = fake_worker(vec![(status, "", body)]);
        let target = target(&listener);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        store_credential(&dir, &target.endpoint);

        let err = call_verb_in(
            &target,
            "platform_call",
            args.clone(),
            10,
            &dir,
            |_| {},
            |_| panic!("an uncertain start must not be automatically retried"),
        )
        .expect_err("an uncertain start has no verified link");
        assert_eq!(err.kind(), "unknown", "{err}");
        let text = err.to_string().to_ascii_lowercase();
        assert!(text.contains("connection ls"), "{text}");
        assert!(
            text.contains("does not prove"),
            "an empty connection list does not prove no link was issued: {text}"
        );
        assert!(
            text.contains("deliberate new enrollment"),
            "a second start must be described as a deliberate new act, not a retry: {text}"
        );
        assert_eq!(hits(&requests.lock().unwrap(), "/__platform/cli/call"), 1);
    }
}

#[test]
fn hosted_platform_catalog_in_progress_reply_remains_a_read_refusal() {
    let args = json!({"operation": "tools"}).as_object().cloned().unwrap();
    let body = json!({
        "ok": false,
        "error": {"code": "idempotency_in_progress"}
    });
    let (listener, requests, _server) = fake_worker(vec![(409, "", body)]);
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    store_credential(&dir, &target.endpoint);

    let out = call_verb_in(
        &target,
        "platform_call",
        args.clone(),
        10,
        &dir,
        |_| {},
        |_| panic!("a catalog refusal must not be automatically retried"),
    );
    let requests = requests.lock().unwrap().clone();
    assert_eq!(
        hits(&requests, "/__platform/cli/call"),
        1,
        "the refused catalog read must make exactly one forwarding attempt"
    );

    let err = out.expect_err("HTTP 409 is not a successful catalog response");
    assert_eq!(
        err.kind(),
        "rejected",
        "listing cannot dispatch a paid tool, even with the same error code: {err}"
    );
    let text = err.to_string().to_ascii_lowercase();
    assert!(
        text.contains("idempotency_in_progress"),
        "the read refusal must preserve the safe server code: {text}"
    );
    assert!(
        !text.contains("charg"),
        "a catalog read must not claim a possible paid dispatch: {text}"
    );
}

#[test]
fn artifact_verbs_are_reads_and_a_store_refusal_is_named() {
    use super::{artifact_refusal, artifact_tenant_too_old, mutating_cli_verb, REMOTE_VERBS};
    for verb in ["artifact_find", "artifact_show", "artifact_read"] {
        assert!(REMOTE_VERBS.contains(&verb), "{verb}");
        assert!(!mutating_cli_verb(verb), "{verb}");
    }
    let refused = artifact_refusal(
        400,
        Some(&serde_json::json!({"error": {"code": "invalid_request", "message": "not a pdf"}})),
    );
    assert!(refused.to_string().contains("not a pdf"));
    // Only the store's own codes are echoed; any other body goes through the safe decoder.
    let other = artifact_refusal(
        403,
        Some(&serde_json::json!({"error": {"code": "secret", "message": "leak-me"}})),
    );
    assert!(!other.to_string().contains("leak-me"));
    // A success that does not name these bytes is not a proven success.
    use super::artifact_receipt;
    let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    let good = serde_json::json!({"file": {"id": "art-1", "version": 1, "digest": digest}});
    assert!(artifact_receipt(good, b"abc").is_ok());
    for bad in [
        serde_json::json!({}),
        serde_json::json!({"file": {"id": "art-1", "version": 1, "digest": "00"}}),
    ] {
        assert_eq!(artifact_receipt(bad, b"abc").unwrap_err().kind(), "unknown");
    }
    let old = artifact_tenant_too_old("artifact_find", 501, "read");
    assert!(old.contains("update the tenant server first"));
    assert!(!old.contains("unavailable"));
}

// CAD-2083 acceptance, written from the ticket by the reviewer rather than the
// implementer. It drives the real client path (`call_verb_in`, or the real local
// dispatch) with the statuses and bodies the tenant's `tenantCliCall` answers.
mod cad2083_acceptance {
    use super::*;

    fn tenant_refusal(code: &str) -> Value {
        json!({"ok": false, "error": {"code": code, "message": "Tenant remote call refused."}})
    }

    fn arguments(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    fn say() -> Map<String, Value> {
        arguments(json!({"id": "CAD-1", "to": "developer", "message": "tm-1", "text": "hi"}))
    }

    fn mark() -> Map<String, Value> {
        arguments(json!({"id": "CAD-1", "role": "developer", "through": 7}))
    }

    fn list() -> Map<String, Value> {
        arguments(json!({"id": "CAD-1", "to": "developer"}))
    }

    fn call_body(requests: &[Value]) -> Value {
        let calls: Vec<_> = requests
            .iter()
            .filter(|r| r["path"] == json!("/__platform/cli/call"))
            .collect();
        assert_eq!(calls.len(), 1, "exactly one call, never a retry");
        calls[0]["body"].clone()
    }

    fn minted_scopes(requests: &[Value]) -> Vec<String> {
        requests
            .iter()
            .find(|r| r["path"] == json!("/__platform/cli/authorize"))
            .unwrap()["body"]["requested_scopes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn each_verb_reaches_the_tenant_and_its_answer_is_printed_as_sent() {
        let stored = json!({
            "message": "tm-1", "id": "CAD-1", "to": "developer", "from": null,
            "sender_subject": "usr_1", "sender": "member", "text": "hi", "seq": 3,
            "created_at": 1, "duplicate": true, "state": "stored",
        });
        // `read_through` is what the tenant returned, not the `through` asked for.
        let marked = json!({"id": "CAD-1", "role": "developer", "read_through": 3});
        let page = json!({
            "id": "CAD-1", "messages": [], "more": true,
            "unread": {"developer": 2}, "unread_more": true,
        });
        for (verb, args, answer, scope) in [
            ("issue_message_send", say(), stored, "cli.write"),
            ("issue_messages_read", mark(), marked, "cli.write"),
            ("issue_messages", list(), page, "cli.read"),
        ] {
            let (out, requests) = run(vec![(200, "", answer.clone())], verb, args.clone(), 10);
            assert_eq!(out.unwrap(), answer, "{verb}: the answer was changed");
            let body = call_body(&requests);
            assert_eq!(body["verb"], json!(verb));
            assert_eq!(body["arguments"], Value::Object(args), "{verb}");
            // The tenant admits a verb only inside its required scope.
            assert!(
                minted_scopes(&requests).iter().any(|s| s == scope),
                "{verb} never asked for {scope}"
            );
        }
    }

    #[test]
    fn each_refusal_is_named_and_is_never_retried() {
        for (verb, args, status, code) in [
            ("issue_message_send", say(), 409u16, "message_id_conflict"),
            ("issue_message_send", say(), 404, "not_found"),
            ("issue_messages", list(), 404, "not_found"),
            ("issue_messages_read", mark(), 404, "not_found"),
            ("issue_message_send", say(), 403, "insufficient_scope"),
            ("issue_messages_read", mark(), 403, "insufficient_scope"),
            // What a relay that predates the verbs answers.
            ("issue_message_send", say(), 400, "invalid_request"),
            ("issue_messages", list(), 400, "invalid_request"),
            ("issue_messages_read", mark(), 400, "invalid_request"),
        ] {
            let (out, requests) = run(vec![(status, "", tenant_refusal(code))], verb, args, 10);
            let err = out.unwrap_err();
            assert_eq!(err.code(), Some(code), "{verb} {status}: {err}");
            assert_eq!(err.kind(), "rejected", "{verb} {status}: {err}");
            call_body(&requests);
        }
    }

    #[test]
    fn a_tenant_that_predates_the_verbs_is_named_and_is_not_an_outage() {
        for (verb, args) in [
            ("issue_message_send", say()),
            ("issue_messages", list()),
            ("issue_messages_read", mark()),
        ] {
            let (out, requests) = run(
                vec![(501, "", tenant_refusal("not_implemented"))],
                verb,
                args,
                10,
            );
            let err = out.unwrap_err();
            assert_eq!(err.code(), Some("not_implemented"), "{verb}: {err}");
            assert_eq!(err.kind(), "rejected", "{verb}: {err}");
            let shown = err.to_string();
            for outage in ["unavailable", "retry", "coordination"] {
                assert!(
                    !shown.contains(outage),
                    "{verb} reads as an outage: {shown}"
                );
            }
            call_body(&requests);
        }
    }

    #[test]
    fn a_lost_write_answer_is_unknown_and_is_never_retried() {
        for (verb, args) in [
            ("issue_message_send", say()),
            ("issue_messages_read", mark()),
        ] {
            let (out, requests) = run(vec![(500, "", json!({}))], verb, args.clone(), 10);
            let err = out.unwrap_err();
            assert_eq!(err.kind(), "unknown", "{verb} 500: {err}");
            call_body(&requests);
            let (kind, calls) = dropped_call_answer(verb, args);
            assert_eq!(kind, "unknown", "{verb}: a dropped connection");
            assert_eq!(calls, 1, "{verb}: a dropped connection was retried");
        }
    }

    /// A worker that mints an envelope, then hangs up instead of answering.
    fn dropped_call_answer(verb: &str, args: Map<String, Value>) -> (&'static str, usize) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = listener.try_clone().unwrap();
        let calls = std::sync::Arc::new(std::sync::Mutex::new(0usize));
        let seen = calls.clone();
        thread::spawn(move || loop {
            let Ok((socket, _)) = server.accept() else {
                return;
            };
            let (socket, req) = read_request(socket);
            if req.path == "/__platform/cli/authorize" {
                answer(
                    socket,
                    "200 OK",
                    "",
                    &json!({"version": CLI_VERSION, "envelope": "h.e.1"}),
                );
            } else {
                *seen.lock().unwrap() += 1;
                drop(socket);
            }
        });
        let target = target(&listener);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        store_credential(&dir, &target.endpoint);
        let err = call_verb_in(&target, verb, args, 10, &dir, |_| {}, |_| {}).unwrap_err();
        let calls = *calls.lock().unwrap();
        (err.kind(), calls)
    }

    #[test]
    fn a_refusal_decided_before_the_verb_ran_never_reads_as_unknown() {
        let board_gate = json!({"ok": false, "error": {
            "code": "board_unreachable", "hop": "board",
            "request_id": "00000000-0000-4000-8000-000000000000",
        }});
        for (verb, args) in [
            ("issue_message_send", say()),
            ("issue_messages_read", mark()),
        ] {
            for (status, body) in [
                // The board's own gate.
                (503u16, board_gate.clone()),
                // The worker's pre-forward state.
                (503, json!({"state": "wake_failed", "code": "wake_failed"})),
                // A read-only login over the real relay: the mint narrows the
                // envelope to `cli.read`, then the call answers a bare 401.
                (401, tenant_refusal("unauthorized")),
            ] {
                let (out, requests) = run(vec![(status, "", body)], verb, args.clone(), 10);
                let err = out.unwrap_err();
                assert_ne!(err.kind(), "unknown", "{verb} {status}: {err}");
                call_body(&requests);
            }
        }
        // A read changes nothing, so its lost answer is never a possible write.
        let (out, requests) = run(vec![(500, "", json!({}))], "issue_messages", list(), 10);
        assert_ne!(out.unwrap_err().kind(), "unknown");
        call_body(&requests);
    }

    /// The real relay, for what the shared fake cannot show: the mint narrows the
    /// requested scopes to the live grant (403 `forbidden` when none is left), and
    /// the call answers `reply` byte for byte.
    fn relay_answer(
        verb: &str,
        args: Map<String, Value>,
        grant: &'static [&'static str],
        status: &'static str,
        reply: &'static str,
    ) -> Result<RemoteAnswer> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = listener.try_clone().unwrap();
        thread::spawn(move || loop {
            let Ok((socket, _)) = server.accept() else {
                return;
            };
            let (mut socket, req) = read_request(socket);
            if req.path == "/__platform/cli/authorize" {
                let scope: Vec<_> = req.body["requested_scopes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|s| grant.iter().any(|g| s.as_str() == Some(g)))
                    .cloned()
                    .collect();
                if scope.is_empty() {
                    answer(socket, "403 Forbidden", "", &tenant_refusal("forbidden"));
                } else {
                    answer(
                        socket,
                        "200 OK",
                        "",
                        &json!({"version": CLI_VERSION, "envelope": "h.e.1", "scope": scope}),
                    );
                }
            } else {
                write!(
                    socket,
                    "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                )
                .unwrap();
            }
        });
        let target = target(&listener);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        store_credential(&dir, &target.endpoint);
        call_verb_in(&target, verb, args, 10, &dir, |_| {}, |_| {})
    }

    #[test]
    fn a_read_only_login_is_named_over_the_real_relay() {
        for (verb, args) in [
            ("issue_message_send", say()),
            ("issue_messages_read", mark()),
        ] {
            let err = relay_answer(
                verb,
                args,
                &["cli.read"],
                "401 Unauthorized",
                r#"{"ok":false,"error":{"code":"unauthorized","message":"refused"}}"#,
            )
            .unwrap_err();
            assert_eq!(err.code(), Some("insufficient_scope"), "{verb}: {err}");
            assert!(!err.to_string().contains("expired"), "{verb}: {err}");
        }
    }

    #[test]
    fn a_write_answer_without_its_receipt_is_unknown_never_a_refusal_or_a_success() {
        for (verb, args) in [
            ("issue_message_send", say()),
            ("issue_messages_read", mark()),
        ] {
            for reply in ["", "<html>ok</html>", "{}"] {
                let kind = relay_answer(
                    verb,
                    args.clone(),
                    &["cli.read", "cli.write"],
                    "200 OK",
                    reply,
                )
                .err()
                .map(|e| e.kind());
                assert_eq!(kind, Some("unknown"), "{verb} answered {reply:?}");
            }
        }
    }

    #[test]
    fn a_local_org_refuses_all_three_verbs_and_touches_nothing() {
        use crate::issue::cli::{run as issue_run, IssueAction};
        for action in [
            IssueAction::Say {
                id: "CAD-1".into(),
                to: "developer".into(),
                from: None,
                message: "tm-1".into(),
                text: Some("hi".into()),
                file: None,
            },
            IssueAction::Messages {
                id: "CAD-1".into(),
                to: None,
                after: None,
            },
            IssueAction::MessagesRead {
                id: "CAD-1".into(),
                role: "developer".into(),
                through: 1,
            },
        ] {
            let state = tempfile::tempdir().unwrap();
            let err = issue_run(&action, state.path()).unwrap_err();
            assert_eq!(err.kind(), "rejected", "{err}");
            assert!(err.to_string().contains("hosted"), "{err}");
            assert_eq!(std::fs::read_dir(state.path()).unwrap().count(), 0);
        }
    }
}

/// CAD-2009: the platform-call seam keeps the two answers that prove the funded
/// dispatch never ran a *definite* refusal. `501 not_implemented` means the
/// tenant does not serve the verb, and `503 tracker_unavailable` is the native
/// route's `pm_at` gate, opened before any verb runs — both precede the provider
/// call, so neither may become the possible-charge unknown of a dispatched but
/// unanswered call, and neither may be replayed.
#[test]
fn hosted_platform_call_pre_forward_refusals_stay_definite() {
    const KEY: &str = "cad2009-pre-forward-acceptance-1";
    for (status, line, code) in [
        (501_u16, "501 Not Implemented", "not_implemented"),
        (503, "503 Service Unavailable", "tracker_unavailable"),
    ] {
        let body = json!({"ok": false, "error": {"code": code}});
        let (listener, seen, server) = finite_worker(2, move |i, socket, _req| {
            if i == 0 {
                write_answer(
                    socket,
                    "200 OK",
                    "",
                    &json!({"version": CLI_VERSION, "envelope": "h.e.1"}),
                );
            } else {
                write_answer(socket, line, "", &body);
            }
        });
        let (target, _root, dir) = credentialed(&listener);
        let args = json!({
            "operation": "call",
            "tool": "generate_image",
            "body": {"prompt": "synthetic pre-forward acceptance input"},
            "key": KEY
        })
        .as_object()
        .cloned()
        .unwrap();
        let out = call_verb_in(
            &target,
            "platform_call",
            args,
            10,
            &dir,
            |_| {},
            |_| panic!("a pre-forward refusal must not be automatically retried"),
        );
        server.join().unwrap();
        let reqs = seen.lock().unwrap();
        assert_eq!(
            reqs.iter()
                .filter(|req| req.path == "/__platform/cli/call")
                .count(),
            1,
            "HTTP {status} ({code}): one forwarding attempt, never a replay"
        );

        let err = out.expect_err("a pre-effect refusal is not a successful tool receipt");
        assert_eq!(
            err.kind(),
            "rejected",
            "HTTP {status} ({code}) proves the dispatch never ran, so it is definite: {err}"
        );
        let text = err.to_string();
        assert!(
            !text.contains("charg"),
            "an undispatched call cannot have charged: {text}"
        );
        assert!(
            !text.contains("outcome_unknown") && !text.contains("may already have committed"),
            "a proven pre-forward refusal is not an unknown outcome: {text}"
        );
    }
}

/// Exercise the real credential, authorize and multipart upload orchestration.
/// Client completion ends the fixture; timeouts are loud failure bounds only.
fn cad2009_artifact_upload_answer(status: &'static str, answer: Value) -> Result<Value> {
    use std::os::fd::AsRawFd;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let worker = listener.try_clone().unwrap();
    let (complete_tx, complete_rx) = std::sync::mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let mut paths = Vec::new();
        for _ in 0..2 {
            let mut pending = libc::pollfd {
                fd: worker.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: pending is one initialized pollfd, and the count is one.
            let ready = unsafe { libc::poll(&mut pending, 1, 5000) };
            assert_eq!(
                ready, 1,
                "expected artifact request did not arrive within 5s"
            );
            assert_ne!(pending.revents & libc::POLLIN, 0);
            let (socket, _) = worker.accept().unwrap();
            let (mut socket, path, raw) = read_raw_request(socket);
            match path.as_str() {
                "/__platform/cli/authorize" => {
                    let request: Value = serde_json::from_slice(&raw).unwrap();
                    assert_eq!(request["version"], CLI_VERSION);
                    assert_eq!(request["requested_scopes"], json!(["cli.write"]));
                    write_answer(
                        &mut socket,
                        "200 OK",
                        "",
                        &json!({"version": CLI_VERSION, "envelope": "h.e.1"}),
                    );
                }
                "/__platform/artifacts/upload" => {
                    let multipart = std::str::from_utf8(&raw).unwrap();
                    assert!(
                        multipart.contains("name=\"id\"\r\n\r\nart-A\r\n"),
                        "the actual upload must explicitly request art-A: {multipart}"
                    );
                    assert!(multipart.contains("name=\"file\"; filename=\"acceptance.txt\""));
                    assert!(
                        multipart.contains("\r\n\r\nabc\r\n--"),
                        "the receipt's known digest must describe the transmitted bytes"
                    );
                    write_answer(&mut socket, status, "", &answer);
                }
                _ => panic!("unexpected artifact upload request: {path}"),
            }
            paths.push(path);
        }
        complete_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("artifact client did not finish within 5s of the response");
        match worker.accept() {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Ok(_) => panic!("artifact upload attempted an extra request after its final answer"),
            Err(error) => panic!("checking for an artifact replay failed: {error}"),
        }
        paths
    });
    let (target, _root, dir) = credentialed(&listener);
    let upload = ArtifactUpload {
        kind: "md",
        title: "Synthetic acceptance document",
        id: Some("art-A"),
        issue: None,
        label: None,
        customer: None,
        file_name: "acceptance.txt",
        bytes: b"abc",
    };
    let result = upload_artifact_in(&dir, &target, &upload, 2, |_| {
        panic!("a final artifact receipt or refusal must never trigger a wake retry")
    });
    complete_tx.send(()).unwrap();
    let paths = server.join().unwrap();
    assert_eq!(
        paths.as_slice(),
        ["/__platform/cli/authorize", "/__platform/artifacts/upload"],
        "authorize then exactly one upload, with no automatic replay"
    );
    result
}

#[test]
fn cad2009_artifact_upload_wrong_requested_id_is_unknown_without_replay() {
    // SHA-256 of the literal payload "abc", independent of the production hash.
    let answer = json!({"file": {
        "id": "art-B",
        "version": 1,
        "digest": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    }});
    let error = cad2009_artifact_upload_answer("200 OK", answer)
        .expect_err("a receipt for art-B cannot prove the explicitly requested art-A upload");
    assert_eq!(
        error.kind(),
        "unknown",
        "matching bytes alone cannot turn a mismatched id into success or definite refusal: {error}"
    );
    assert!(
        error.to_string().contains("artifact find"),
        "an ambiguous upload must name its reconciliation read: {error}"
    );
}

#[test]
fn cad2009_artifact_upload_matching_requested_id_succeeds_once() {
    let answer = json!({"file": {
        "id": "art-A",
        "version": 1,
        "digest": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    }});
    let result = cad2009_artifact_upload_answer("200 OK", answer.clone())
        .expect("a receipt naming art-A and its transmitted bytes must succeed");
    assert_eq!(result, answer);
}

#[test]
fn cad2009_artifact_upload_board_hop_503_is_definite_without_replay() {
    let answer = json!({"ok": false, "error": {
        "code": "capability_unavailable",
        "hop": "board",
        "request_id": "9cb6440e-f363-4dca-8f85-bc652696bc6a"
    }});
    let error = cad2009_artifact_upload_answer("503 Service Unavailable", answer)
        .expect_err("a board gate refusal cannot produce an artifact receipt");
    assert_eq!(
        error.kind(),
        "rejected",
        "the board hop proves this upload was refused before forwarding: {error}"
    );
}

#[test]
fn cad2009_artifact_upload_unproven_tracker_503_is_unknown_without_replay() {
    // Native /cli/call's tracker gate is not proof for /artifacts/upload.
    let answer = json!({"ok": false, "error": {"code": "tracker_unavailable"}});
    let error = cad2009_artifact_upload_answer("503 Service Unavailable", answer)
        .expect_err("an unproven 503 cannot produce an artifact receipt");
    assert_eq!(
        error.kind(),
        "unknown",
        "this endpoint provides no proof that tracker_unavailable preceded storage: {error}"
    );
}

#[test]
fn cad2009_artifact_upload_unproven_worker_states_are_unknown_without_replay() {
    for (status, state) in [
        ("503 Service Unavailable", "wake_failed"),
        ("503 Service Unavailable", "board_unreachable"),
        ("503 Service Unavailable", "runtime_unavailable"),
        ("503 Service Unavailable", "mystery"),
        ("500 Internal Server Error", "waking"),
    ] {
        let answer = json!({"state": state});
        let error = cad2009_artifact_upload_answer(status, answer)
            .expect_err("an unproven worker state cannot establish an artifact upload outcome");
        assert_eq!(
            error.kind(),
            "unknown",
            "{status} with state {state} is not pre-forward proof for the artifact endpoint: {error}"
        );
    }
}

// Independently authored CAD-2009 acceptance by the fresh-session Standards
// reviewer. Append to src/remote_cli/tests.rs; reuse its existing strict
// loopback HTTP `run` helper, not the private receipt predicate.
// Producer evidence: AgenticOS apps/api/src/tenant-board/cli.ts:
// requiredText (396-403), PROJECT_KEY (66), and issue_new (614-641).
// The producer trims project input but does not lowercase it.
// Formatting-only changes to this independently authored test are authorized;
// changing its cases, expectations, seam, or assertions is not authorized.
#[test]
fn cad2009_issue_create_accepts_trimmed_project_but_not_a_foreign_project() {
    for sent_project in [" synthetic ", "\u{feff}synthetic\u{feff}"] {
        for (received_project, confirmed) in [("synthetic", true), ("foreign", false)] {
            let args = json!({"project": sent_project, "title": "Synthetic receipt acceptance"});
            let receipt = json!({
                "issue": {"id": "SYN-1"},
                "card": {
                    "id": "SYN-1",
                    "project": received_project,
                    "title": "Synthetic receipt acceptance"
                },
                "warnings": []
            });
            let (result, requests) = run(
                vec![(200, "", receipt.clone())],
                "issue_new",
                args.as_object().unwrap().clone(),
                10,
            );
            // Both branches use the real authorize/call transport and the
            // original credential-bound fixture; neither may bypass auth or
            // replay a mutation after receiving its synthetic server answer.
            assert_eq!(hits(&requests, "/__platform/cli/authorize"), 1);
            assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
            let authorize = requests
                .iter()
                .find(|request| request["path"] == "/__platform/cli/authorize")
                .unwrap();
            assert_eq!(authorize["body"]["organization_id"], ORG_ID);
            assert!(authorize["body"]["requested_scopes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|scope| scope == "cli.write"));
            if confirmed {
                assert_eq!(
                    result.unwrap(),
                    receipt,
                    "the producer-normalized project must confirm this create: {sent_project:?}"
                );
            } else {
                let error = result.expect_err("a foreign project cannot confirm this create");
                assert_eq!(error.kind(), "unknown", "{error}");
                assert!(error.to_string().contains("issue ls"), "{error}");
            }
        }
    }
}

// JS String.trim removes U+FEFF but not U+0085. The native IssueNew path
// passes req.title to issue_write::new_issue (src/ui/cli_route.rs:704-719),
// then Front::new stores title.to_string() (src/issue/model.rs:154-157).
// Preserve that exact-title receipt too; do not impose tenant-only title
// normalization on the native producer. These are synthetic native-shaped
// replies at the HTTP client seam, not an execution of the native route.
#[test]
fn cad2009_issue_create_correlates_producer_title_without_overtrimming() {
    for (case, sent_project, sent_title, received_title, confirmed) in [
        (
            "tenant trims BOM title",
            "synthetic",
            "\u{feff}Synthetic receipt acceptance\u{feff}",
            "Synthetic receipt acceptance",
            true,
        ),
        (
            "NEL project cannot correlate with a trimmed project",
            "\u{0085}synthetic\u{0085}",
            "Synthetic receipt acceptance",
            "Synthetic receipt acceptance",
            false,
        ),
        (
            "NEL title cannot correlate with a trimmed title",
            "synthetic",
            "\u{0085}Synthetic receipt acceptance\u{0085}",
            "Synthetic receipt acceptance",
            false,
        ),
        (
            "native preserves exact BOM title",
            "synthetic",
            "\u{feff}Synthetic receipt acceptance\u{feff}",
            "\u{feff}Synthetic receipt acceptance\u{feff}",
            true,
        ),
        (
            "native and tenant preserve exact NEL title",
            "synthetic",
            "\u{0085}Synthetic receipt acceptance\u{0085}",
            "\u{0085}Synthetic receipt acceptance\u{0085}",
            true,
        ),
    ] {
        let args = json!({"project": sent_project, "title": sent_title});
        let receipt = json!({
            "issue": {"id": "SYN-1"},
            "card": {
                "id": "SYN-1",
                "project": "synthetic",
                "title": received_title
            },
            "warnings": []
        });
        let (result, requests) = run(
            vec![(200, "", receipt.clone())],
            "issue_new",
            args.as_object().unwrap().clone(),
            10,
        );
        assert_eq!(hits(&requests, "/__platform/cli/authorize"), 1, "{case}");
        assert_eq!(hits(&requests, "/__platform/cli/call"), 1, "{case}");
        let authorize = requests
            .iter()
            .find(|request| request["path"] == "/__platform/cli/authorize")
            .unwrap();
        assert_eq!(authorize["body"]["organization_id"], ORG_ID, "{case}");
        assert!(
            authorize["body"]["requested_scopes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|scope| scope == "cli.write"),
            "{case}"
        );
        if confirmed {
            assert_eq!(result.unwrap(), receipt, "{case}");
        } else {
            let error = result.expect_err(case);
            assert_eq!(error.kind(), "unknown", "{case}: {error}");
            assert!(error.to_string().contains("issue ls"), "{case}: {error}");
        }
    }
}

// Independently authored CAD-2009 Spec/security acceptance.
// Append to src/remote_cli/tests.rs beside the existing hosted receipt tests.
// Formatting-only changes are authorized. Do not change cases, expectations,
// transport seam, scope checks, or no-replay assertions without reviewer agreement.
//
// Producer evidence read from the available local AgenticOS source:
// packages/contracts/src/members.ts:47-51 applies JS trim().toLowerCase()
// before z.email(); hosted-cadence-auth.ts:1240-1243 reuses that email schema;
// apps/api/src/board/team-cli.ts:200-215 passes parsed.data.email to the write.
// ECMAScript trim removes U+FEFF but does not remove U+0085. The latter is
// neither part of the producer's canonical ASCII address nor a valid address
// wrapper. These synthetic replies exercise the real authorize/call client,
// not a live producer or the private receipt predicate.
#[test]
fn cad2009_team_invite_receipts_match_ecmascript_whitespace_without_overtrimming() {
    let canonical = "new@example.test";
    for (case, sent_email, received_email, confirmed) in [
        (
            "canonical ASCII control",
            "New@Example.TEST",
            canonical,
            true,
        ),
        (
            "producer strips surrounding BOM",
            "\u{feff}New@Example.TEST\u{feff}",
            canonical,
            true,
        ),
        (
            "producer trims spaces and BOM together",
            " \u{feff} New@Example.TEST \u{feff} ",
            canonical,
            true,
        ),
        (
            "foreign address cannot confirm BOM-wrapped input",
            "\u{feff}New@Example.TEST\u{feff}",
            "other@example.test",
            false,
        ),
        (
            "NEL-wrapped receipt is not the producer canonical address",
            "New@Example.TEST",
            "\u{0085}new@example.test\u{0085}",
            false,
        ),
    ] {
        // Cover the producer's two success statuses: 201 allocates a pending
        // invite; 200 returns an existing pending invite. Use the requested
        // role in both, so this test changes no role-correlation expectations.
        for status in [200_u16, 201] {
            let args = json!({"email": sent_email, "role": "member"});
            let receipt = json!({"invite": {
                "id": "inv_cad2009_unicode",
                "email": received_email,
                "role": "member",
                "status": "pending",
                "createdAt": "2026-01-01T00:00:00.000Z",
                "expiresAt": "2026-01-08T00:00:00.000Z"
            }});
            let (result, requests) = run(
                vec![(status, "", receipt.clone())],
                "team_invite",
                args.as_object().unwrap().clone(),
                10,
            );
            assert_eq!(
                requests.len(),
                2,
                "{case}/HTTP {status}: no extra reads or replay"
            );
            assert_eq!(
                hits(&requests, "/__platform/cli/authorize"),
                1,
                "{case}/HTTP {status}"
            );
            assert_eq!(
                hits(&requests, "/__platform/cli/call"),
                1,
                "{case}/HTTP {status}"
            );
            let authorize = &requests[0];
            assert_eq!(authorize["path"], "/__platform/cli/authorize", "{case}");
            assert_eq!(authorize["body"]["version"], CLI_VERSION, "{case}");
            assert_eq!(authorize["body"]["organization_id"], ORG_ID, "{case}");
            assert_eq!(
                authorize["body"]["requested_scopes"],
                json!(["cli.write"]),
                "{case}"
            );
            assert!(
                authorize["body"]["audience"]
                    .as_str()
                    .is_some_and(|audience| audience.starts_with("http://127.0.0.1:")),
                "{case}"
            );
            assert!(
                authorize["body"]["ttl_seconds"]
                    .as_u64()
                    .is_some_and(|ttl| (1..=300).contains(&ttl)),
                "{case}"
            );
            assert_eq!(authorize["body"].as_object().unwrap().len(), 5, "{case}");
            let call = &requests[1];
            assert_eq!(call["path"], "/__platform/cli/call", "{case}");
            assert_eq!(call["body"]["version"], CLI_VERSION, "{case}");
            assert_eq!(call["body"]["verb"], "team_invite", "{case}");
            assert_eq!(
                call["body"]["arguments"], args,
                "{case}: outgoing input changed"
            );
            assert_eq!(
                call["body"].as_object().unwrap().len(),
                4,
                "{case}: authority-shaped extra field"
            );
            assert!(
                call["body"]["envelope"].as_str().is_some_and(|envelope| {
                    envelope.split('.').count() == 3
                        && envelope.split('.').all(|part| {
                            !part.is_empty()
                                && part.bytes().all(|byte| {
                                    byte.is_ascii_alphanumeric() || b"_-".contains(&byte)
                                })
                        })
                }),
                "{case}: missing single-use authorization envelope"
            );
            if confirmed {
                assert_eq!(result.expect(case), receipt, "{case}/HTTP {status}");
            } else {
                let error = result.expect_err(case);
                assert_eq!(error.kind(), "unknown", "{case}/HTTP {status}: {error}");
                assert_eq!(crate::error::exit_code_for_kind(error.kind()), 1, "{case}");
                let guidance = error.to_string();
                assert!(
                    guidance.contains("team list") || guidance.contains("team invites"),
                    "{case}: {guidance}"
                );
                assert!(!guidance.contains("retry later"), "{case}: {guidance}");
            }
        }
    }
}

// U+0085 input never reaches the producer: it is a control character under
// the existing client team_argument_shape. Keep that predispatch guard; do
// not "fix" normalization by trimming invalid input before admission.
#[test]
fn cad2009_team_invite_nel_input_keeps_definite_predispatch_refusal() {
    let args = json!({
        "email": "\u{0085}New@Example.TEST\u{0085}",
        "role": "member"
    });
    let (result, requests) = run(
        Vec::new(),
        "team_invite",
        args.as_object().unwrap().clone(),
        10,
    );
    assert!(
        requests.is_empty(),
        "NEL input must send neither authorization nor mutation"
    );
    let error = result.expect_err("the existing control-character guard must reject NEL input");
    assert_eq!(error.kind(), "rejected", "{error}");
    assert_eq!(crate::error::exit_code_for_kind(error.kind()), 3);
    assert!(!error.to_string().contains("outcome is unknown"), "{error}");
}

// CAD-2009: a sync can update one export before another fails. Only a
// documented pre-effect refusal proves that the dispatched sync did not apply.
#[test]
fn cad2009_profile_sync_postdispatch_503_is_unknown_but_invalid_request_is_definite() {
    for (status, code, expected_kind) in [
        (503, "capability_unavailable", "unknown"),
        // cli_route::rpc maps a native rejected error to this pre-effect
        // refusal (for example, selecting a profile that does not exist).
        (400, "invalid_request", "rejected"),
    ] {
        let arguments = settings_args("profile.sync", json!({"profile": "p1"}));
        let (result, requests) = run(
            vec![(status, "", json!({"ok": false, "error": {"code": code}}))],
            "settings_agents",
            arguments.clone(),
            10,
        );
        let error = result.expect_err("the failed sync must not return success");
        assert_eq!(error.kind(), expected_kind, "HTTP {status}: {error}");
        assert_eq!(
            crate::error::exit_code_for_kind(error.kind()),
            if status == 503 { 1 } else { 3 },
            "HTTP {status}"
        );
        let guidance = error.to_string();
        if status == 503 {
            for read in ["settings agents show", "settings agents history", "--org"] {
                assert!(guidance.contains(read), "HTTP {status}: {guidance}");
            }
            assert!(!guidance.contains("retry later"), "{guidance}");
        } else {
            assert!(!guidance.contains("settings agents show"), "{guidance}");
            assert!(!guidance.contains("settings agents history"), "{guidance}");
        }
        assert_eq!(
            hits(&requests, "/__platform/cli/authorize"),
            1,
            "HTTP {status}: must not reauthorize: {requests:?}"
        );
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "HTTP {status}: must not replay the sync: {requests:?}"
        );
        assert_eq!(requests.len(), 2, "HTTP {status}: {requests:?}");
        assert_eq!(requests[0]["path"], "/__platform/cli/authorize");
        assert_eq!(requests[1]["path"], "/__platform/cli/call");
        assert_eq!(requests[1]["body"]["verb"], "settings_agents");
        assert_eq!(requests[1]["body"]["arguments"], json!(arguments));
    }
}

#[test]
fn cad2101_profile_verbs_are_registered_with_their_scopes() {
    use super::{cli_requested_scopes, lost_mutation_note, mutating_cli_verb, REMOTE_VERBS};
    for verb in [
        "agent_profile_show",
        "agent_profile_create",
        "agent_profile_update",
    ] {
        assert!(REMOTE_VERBS.contains(&verb), "{verb}");
    }
    assert_eq!(cli_requested_scopes("agent_profile_show"), &["cli.read"]);
    assert_eq!(cli_requested_scopes("agent_profile_create"), &["cli.write"]);
    assert_eq!(cli_requested_scopes("agent_profile_update"), &["cli.write"]);
    assert!(!mutating_cli_verb("agent_profile_show"));
    assert!(mutating_cli_verb("agent_profile_create"));
    assert!(mutating_cli_verb("agent_profile_update"));
    assert!(lost_mutation_note("agent_profile_update").contains("agent profile get"));
    assert!(lost_mutation_note("agent_profile_create").contains("agent profiles"));
    // CAD-2009: the classifier is total; the read must not be a possible write.
    assert!(super::mutation_reconcile("agent_profile_show").is_none());
    assert!(super::mutation_reconcile("agent_profile_create").is_some());
}

#[test]
fn cad2101_profile_refusals_keep_their_code_and_are_never_retried() {
    // Server text is arbitrary; the client answers the bounded code with its
    // own text. Synthetic bodies only.
    let refusal = |code: &str| json!({"error": {"code": code, "message": "server-text /private"}});
    for (verb, status, code) in [
        ("agent_profile_show", 404u16, "not_found"),
        ("agent_profile_update", 404, "not_found"),
        ("agent_profile_create", 403, "not_permitted"),
        ("agent_profile_update", 403, "not_permitted"),
        // The call gate answers a member or a non-user principal this way.
        ("agent_profile_create", 401, "unauthorized"),
        ("agent_profile_update", 401, "unauthorized"),
        ("agent_profile_create", 409, "alias_taken"),
        ("agent_profile_create", 400, "invalid_request"),
        ("agent_profile_update", 400, "invalid_request"),
        ("agent_profile_show", 503, "capability_unavailable"),
    ] {
        let (out, requests) = run(vec![(status, "", refusal(code))], verb, Map::new(), 10);
        let err = out.unwrap_err();
        let shown = err.to_string();
        assert_eq!(err.code(), Some(code), "{verb} {status}: {shown}");
        assert_eq!(err.kind(), "rejected", "{verb} {status}: {shown}");
        assert!(
            !shown.contains("server-text") && !shown.contains("/private"),
            "{shown}"
        );
        assert!(
            !shown.contains("coordination"),
            "a profile refusal read as an outage: {shown}"
        );
        assert_eq!(hits(&requests, "/__platform/cli/call"), 1, "{verb} {code}");
    }
}

#[test]
fn cad2101_stale_revision_is_a_conflict_answer_that_writes_nothing() {
    // The 409 is a verdict, not an error: the caller prints it and exits 5.
    let (out, requests) = run(
        vec![(
            409,
            "",
            json!({"error": {"code": "revision_conflict", "message": "x"}}),
        )],
        "agent_profile_update",
        json!({"id": "ap_synthetic-0001", "expectedRevision": 1, "role": "r"})
            .as_object()
            .unwrap()
            .clone(),
        10,
    );
    let body = out.expect("a conflict is an answer, not an error");
    assert_eq!(body["error"]["code"], "revision_conflict");
    assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
}

#[test]
fn cad2101_a_board_without_the_verbs_says_to_update_it() {
    // The tenant board answers 501 not_implemented for a verb it lacks.
    for verb in [
        "agent_profile_show",
        "agent_profile_create",
        "agent_profile_update",
    ] {
        let (out, requests) = run(
            vec![(501, "", json!({"error": {"code": "not_implemented"}}))],
            verb,
            Map::new(),
            10,
        );
        let shown = out.unwrap_err().to_string();
        assert!(shown.contains("update the board first"), "{verb}: {shown}");
        assert!(!shown.contains("coordination"), "{shown}");
        assert!(shown.contains("Nothing was"), "{shown}");
        assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
    }
    // A bare 400 from an older relay is also what a bad value gets, so the
    // text names both and still says to update the board.
    let (out, _) = run(
        vec![(400, "", json!({"error": {"code": "invalid_request"}}))],
        "agent_profile_update",
        Map::new(),
        10,
    );
    assert!(out
        .unwrap_err()
        .to_string()
        .contains("update the board first"));
}

#[test]
fn cad2101_profile_write_without_a_matching_receipt_is_unknown() {
    let update = || {
        json!({"id": "ap_synthetic-0001", "expectedRevision": 2, "role": "r"})
            .as_object()
            .unwrap()
            .clone()
    };
    for receipt in [
        Value::Null,
        json!({"ok": true}),
        json!({"profile": {"id": "ap_other", "revision": 3}}),
        json!({"profile": {"id": "ap_synthetic-0001"}}),
    ] {
        let (out, requests) = run(
            vec![(200, "", receipt)],
            "agent_profile_update",
            update(),
            10,
        );
        let err = out.unwrap_err();
        assert_eq!(err.kind(), "unknown", "{err}");
        assert!(err.to_string().contains("agent profile get"), "{err}");
        assert_eq!(hits(&requests, "/__platform/cli/call"), 1, "replayed");
    }
    let (out, _) = run(
        vec![(
            200,
            "",
            json!({"profile": {"id": "ap_synthetic-0001", "revision": 3, "role": "r"}}),
        )],
        "agent_profile_update",
        update(),
        10,
    );
    assert_eq!(out.unwrap()["profile"]["revision"], 3);
    // Create: any profile id with a revision matches; no profile does not.
    let create = || {
        json!({"alias": "ledger", "catalogue": "accountant"})
            .as_object()
            .unwrap()
            .clone()
    };
    let (out, _) = run(
        vec![(200, "", json!({"profile": {}}))],
        "agent_profile_create",
        create(),
        10,
    );
    assert_eq!(out.unwrap_err().kind(), "unknown");
    let (out, _) = run(
        vec![(500, "", json!({"error": {"code": "internal"}}))],
        "agent_profile_create",
        create(),
        10,
    );
    let err = out.unwrap_err();
    assert_eq!(err.kind(), "unknown", "{err}");
    assert!(err.to_string().contains("agent profiles"), "{err}");
    // A write's 503 is an unknown outcome too, not a refusal with an exit 3.
    let (out, _) = run(
        vec![(
            503,
            "",
            json!({"error": {"code": "capability_unavailable"}}),
        )],
        "agent_profile_update",
        json!({"id": "ap_synthetic-0001", "expectedRevision": 2, "role": "r"})
            .as_object()
            .unwrap()
            .clone(),
        10,
    );
    assert_eq!(out.unwrap_err().kind(), "unknown");
}

/// CAD-2111: answer each hosted doctor request by its path.
fn doctor_worker(
    n: usize,
    answer: impl Fn(&str) -> (&'static str, Value) + Send + 'static,
) -> (
    TcpListener,
    std::sync::Arc<std::sync::Mutex<Vec<Request>>>,
    thread::JoinHandle<()>,
) {
    finite_worker(n, move |_, socket, req| {
        let (status, body) = answer(&req.path);
        write_answer(socket, status, "", &body);
    })
}

fn doctor_envelope(version: &str) -> Value {
    json!({"version": version, "envelope": "aa.SECRETENVELOPE.cc", "scope": ["cli.read"]})
}

fn with_scopes(dir: &Path, scopes: &[&str]) {
    let path = dir.join(format!("cli-{ORG}.json"));
    let mut saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    saved["capabilities"] = json!(scopes);
    fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
}

fn check<'a>(checks: &'a [probe::Check], id: &str) -> &'a probe::Check {
    checks.iter().find(|c| c.id == id).unwrap()
}

/// No renewal is due in these, so the issuer must never be called.
fn doctor_checks(dir: &Path, target: &RemoteTarget, offline: bool) -> Vec<probe::Check> {
    probe::checks_at(dir, target, offline, |_, _, _, _, _, _| {
        panic!("doctor renewed a login that was not due")
    })
}

const DEFAULT_SCOPES: &[&str] = &["cli.read", "cli.write", "wiki.read", "wiki.write"];

fn doctor_healthy(path: &str) -> (&'static str, Value) {
    match path {
        "/__platform/cli/authorize" => ("200 OK", doctor_envelope(CLI_VERSION)),
        "/__platform/wiki/authorize" => ("200 OK", doctor_envelope(WIKI_VERSION)),
        "/__platform/cli/call" => ("200 OK", json!({"projects": []})),
        other => panic!("doctor sent an unexpected request: {other}"),
    }
}

#[test]
fn hosted_doctor_passes_a_working_login_with_only_reads() {
    let (listener, seen, handle) = doctor_worker(3, doctor_healthy);
    let (target, _root, dir) = credentialed(&listener);
    with_scopes(&dir, DEFAULT_SCOPES);
    let checks = doctor_checks(&dir, &target, false);
    handle.join().unwrap();

    assert_eq!(probe::exit_code(&checks), 0, "{checks:?}");
    for id in ["credential", "auth", "read", "wiki", "scopes"] {
        assert_eq!(check(&checks, id).status, probe::Status::Ok, "{id}");
    }
    // Optional access a default login never asked for is listed, not a fault.
    let scopes = &check(&checks, "scopes").summary;
    assert!(
        scopes.contains("--settings") && scopes.contains("--audit-approve"),
        "{scopes}"
    );

    let seen = seen.lock().unwrap();
    let paths: Vec<&str> = seen.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/__platform/cli/authorize",
            "/__platform/cli/call",
            "/__platform/wiki/authorize"
        ]
    );
    assert_eq!(seen[0].body["requested_scopes"], json!(["cli.read"]));
    assert_eq!(seen[1].body["verb"], "project_ls");
}

#[test]
fn hosted_doctor_never_prints_a_credential_or_envelope() {
    let (listener, _seen, handle) = doctor_worker(3, doctor_healthy);
    let (target, _root, dir) = credentialed(&listener);
    with_scopes(&dir, DEFAULT_SCOPES);
    let checks = doctor_checks(&dir, &target, false);
    handle.join().unwrap();
    let printed = format!(
        "{}{}",
        probe::report_text(&target, &checks),
        probe::report_json(&target, &checks)
    );
    assert!(!printed.contains(BRIDGE), "{printed}");
    assert!(!printed.contains("SECRETENVELOPE"), "{printed}");
}

#[test]
fn hosted_doctor_names_login_only_for_a_proven_ended_session() {
    let (listener, _seen, handle) = doctor_worker(1, |_| {
        (
            "401 Unauthorized",
            json!({"ok": false, "error": {"code": "session_revoked"}}),
        )
    });
    let (target, _root, dir) = credentialed(&listener);
    let checks = doctor_checks(&dir, &target, false);
    handle.join().unwrap();

    assert_eq!(probe::exit_code(&checks), 2);
    let auth = check(&checks, "auth");
    assert_eq!(auth.status, probe::Status::Fail);
    assert!(
        auth.remedy
            .as_deref()
            .unwrap()
            .contains("cadence login acme"),
        "{auth:?}"
    );
    assert_eq!(check(&checks, "read").status, probe::Status::Skipped);
}

#[test]
fn hosted_doctor_does_not_call_a_bare_401_an_expired_login() {
    let (listener, _seen, handle) = doctor_worker(1, |_| {
        (
            "401 Unauthorized",
            json!({"ok": false, "error": {"code": "unauthorized"}}),
        )
    });
    let (target, _root, dir) = credentialed(&listener);
    let checks = doctor_checks(&dir, &target, false);
    handle.join().unwrap();

    assert_eq!(probe::exit_code(&checks), 2);
    let auth = check(&checks, "auth");
    assert!(
        auth.summary.contains("does not prove it expired"),
        "{auth:?}"
    );
    assert!(
        auth.remedy
            .as_deref()
            .unwrap()
            .contains("cadence login acme"),
        "{auth:?}"
    );
}

#[test]
fn hosted_doctor_says_a_server_fault_is_not_the_login() {
    let (listener, _seen, handle) = doctor_worker(1, |_| {
        (
            "503 Service Unavailable",
            json!({"ok": false, "error": {"code": "capability_unavailable"}}),
        )
    });
    let (target, _root, dir) = credentialed(&listener);
    let checks = doctor_checks(&dir, &target, false);
    handle.join().unwrap();

    assert_eq!(probe::exit_code(&checks), 2);
    let auth = check(&checks, "auth");
    assert!(
        auth.summary.contains("your login is not the problem"),
        "{auth:?}"
    );
    assert!(
        !auth.remedy.as_deref().unwrap().contains("cadence login"),
        "{auth:?}"
    );
}

#[test]
fn hosted_doctor_reports_an_unreachable_workspace_without_blaming_the_login() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (target, _root, dir) = credentialed(&listener);
    drop(listener);
    let checks = doctor_checks(&dir, &target, false);

    assert_eq!(probe::exit_code(&checks), 2);
    let auth = check(&checks, "auth");
    assert!(
        auth.summary.contains("says nothing about your login"),
        "{auth:?}"
    );
}

#[test]
fn hosted_doctor_warns_on_a_login_that_cannot_write() {
    let (listener, _seen, handle) = doctor_worker(3, doctor_healthy);
    let (target, _root, dir) = credentialed(&listener);
    with_scopes(&dir, &["cli.read", "wiki.read", "wiki.write"]);
    let checks = doctor_checks(&dir, &target, false);
    handle.join().unwrap();

    assert_eq!(probe::exit_code(&checks), 1);
    let scopes = check(&checks, "scopes");
    assert_eq!(scopes.status, probe::Status::Warn);
    assert!(
        scopes
            .remedy
            .as_deref()
            .unwrap()
            .contains("cadence login acme"),
        "{scopes:?}"
    );
}

#[test]
fn hosted_doctor_skips_the_wiki_for_a_login_without_it() {
    let (listener, seen, handle) = doctor_worker(2, doctor_healthy);
    let (target, _root, dir) = credentialed(&listener);
    with_scopes(&dir, &["cli.read", "cli.write"]);
    let checks = doctor_checks(&dir, &target, false);
    handle.join().unwrap();

    assert_eq!(probe::exit_code(&checks), 0, "{checks:?}");
    assert_eq!(check(&checks, "wiki").status, probe::Status::Skipped);
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[test]
fn hosted_doctor_offline_sends_nothing() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let (target, _root, dir) = credentialed(&listener);
    with_scopes(&dir, DEFAULT_SCOPES);
    let checks = doctor_checks(&dir, &target, true);

    assert_eq!(probe::exit_code(&checks), 0);
    for id in ["auth", "read", "wiki"] {
        assert_eq!(check(&checks, id).status, probe::Status::Skipped, "{id}");
    }
    assert_eq!(
        listener.accept().err().unwrap().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn hosted_doctor_without_a_login_stops_before_the_network() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let target = target(&listener);
    let root = tempfile::tempdir().unwrap();
    let checks = doctor_checks(root.path(), &target, false);

    assert_eq!(probe::exit_code(&checks), 2);
    assert!(check(&checks, "credential")
        .remedy
        .as_deref()
        .unwrap()
        .contains("cadence login acme"));
    assert_eq!(check(&checks, "auth").status, probe::Status::Skipped);
    assert_eq!(
        listener.accept().err().unwrap().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

/// A saved login whose access is due, so doctor must renew before it can sign
/// in. Most real runs start here: access lasts minutes, sessions last days.
fn due_login(target: &RemoteTarget) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let issuer = "https://api-v2.agenticos.hk";
    let at = now().unwrap();
    store_credential_at(root.path(), ORG, ORG_ID, &target.endpoint, at - 1);
    let path = root.path().join(format!("cli-{ORG}.json"));
    let mut saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    saved["issuer"] = json!(issuer);
    saved["subject_id"] = json!("user_owner");
    saved["organization_slug"] = json!(ORG);
    saved["capabilities"] = json!(DEFAULT_SCOPES);
    saved["cli_session"] = json!({
        "session_id": "hcs_12345678-1234-4234-8234-123456789abc",
        "refresh_token": format!("hcr_{}", "A".repeat(43)),
        "generation": 1,
        "device_label": "cadence CLI",
        "idle_expires_at": at + 1000,
        "absolute_expires_at": at + 10_000
    });
    fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
    let pin = root.path().join("trusted-issuer");
    fs::write(&pin, format!("{issuer}\n")).unwrap();
    fs::set_permissions(&pin, fs::Permissions::from_mode(0o600)).unwrap();
    root
}

/// The doctor result when renewal is due and the issuer does `answer`. The
/// workspace listener must see nothing: a failed renewal stops the live pass.
fn doctor_with_renewal(
    answer: Result<crate::remote_enrollment::IssuerAnswer>,
) -> Vec<probe::Check> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let target = target(&listener);
    let root = due_login(&target);
    let checks = probe::checks_at(root.path(), &target, false, |_, _, _, _, _, _| answer);
    assert_eq!(
        listener.accept().err().unwrap().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(check(&checks, "read").status, probe::Status::Skipped);
    assert_eq!(check(&checks, "wiki").status, probe::Status::Skipped);
    checks
}

fn issuer_answer(status: u16, body: Value) -> Result<crate::remote_enrollment::IssuerAnswer> {
    Ok(crate::remote_enrollment::IssuerAnswer {
        status,
        body,
        correlation: None,
    })
}

#[test]
fn hosted_doctor_due_renewal_with_the_issuer_unreachable_does_not_blame_the_login() {
    let checks = doctor_with_renewal(Err(reject("Hosted CLI session request failed")));
    assert_eq!(probe::exit_code(&checks), 2);
    let auth = check(&checks, "auth");
    assert!(
        auth.summary.contains("says nothing about your login"),
        "{auth:?}"
    );
    assert!(
        !auth.remedy.as_deref().unwrap().contains("cadence login"),
        "{auth:?}"
    );
}

#[test]
fn hosted_doctor_due_renewal_with_an_issuer_outage_does_not_blame_the_login() {
    let checks = doctor_with_renewal(issuer_answer(
        503,
        json!({"error": "authority_unavailable"}),
    ));
    assert_eq!(probe::exit_code(&checks), 2);
    let auth = check(&checks, "auth");
    assert!(
        auth.summary
            .contains("does not show a problem with your login"),
        "{auth:?}"
    );
    assert!(
        !auth.remedy.as_deref().unwrap().contains("cadence login"),
        "{auth:?}"
    );
}

#[test]
fn hosted_doctor_due_renewal_of_an_ended_session_names_login_with_the_org() {
    for (status, code) in [(401, "invalid_token"), (400, "invalid_grant")] {
        let checks = doctor_with_renewal(issuer_answer(status, json!({"error": code})));
        assert_eq!(probe::exit_code(&checks), 2);
        let auth = check(&checks, "auth");
        assert!(auth.summary.contains("session has ended"), "{auth:?}");
        assert_eq!(auth.remedy.as_deref(), Some("run `cadence login acme`"));
    }
}

#[test]
fn hosted_doctor_due_renewal_with_an_unreadable_success_is_not_called_healthy() {
    let checks = doctor_with_renewal(issuer_answer(200, json!({"unexpected": true})));
    assert_eq!(probe::exit_code(&checks), 1);
    assert_eq!(check(&checks, "auth").status, probe::Status::Unknown);
}

// CAD-2127: `platform status` and `platform artifact` are read-only lookups of
// a request the workspace already holds. A lost read proves nothing ran, so an
// availability failure must stay a definite refusal; a tool call (`call`)
// remains a possible mutation and keeps its "may have been charged" unknown.
#[test]
fn cad2127_platform_status_and_artifact_are_read_only_effects() {
    for operation in ["status", "artifact"] {
        let arguments = json!({"operation": operation, "key": "cad2127-run-1"})
            .as_object()
            .cloned()
            .unwrap();
        let effect = call_effect(Surface::Cli, "platform_call", &arguments);
        assert!(
            matches!(effect, Effect::Read),
            "`platform {operation}` must not be treated as a mutation"
        );
        assert!(!effect.is_mutation(), "{operation}");
        assert!(
            effect.reconciliation().is_none(),
            "a read has no effect to reconcile"
        );
    }

    // A paid tool call stays a mutation, and an operation outside the known
    // read set fails closed as a possible mutation.
    for operation in ["call", "unmapped_operation"] {
        let arguments = json!({"operation": operation, "tool": "generate_image"})
            .as_object()
            .cloned()
            .unwrap();
        assert!(
            call_effect(Surface::Cli, "platform_call", &arguments).is_mutation(),
            "`platform {operation}` must stay a possible mutation"
        );
    }
}

// CAD-2226: the owner-enrollment reads change nothing, while starting an
// enrollment mints a one-time provider link — a possible mutation whose lost
// answer is reconciled by the connection read, never by `platform status`.
#[test]
fn hosted_connection_operations_are_reads_or_a_mutation() {
    for operation in ["connection_providers", "connection_list"] {
        let arguments = json!({"operation": operation})
            .as_object()
            .cloned()
            .unwrap();
        let effect = call_effect(Surface::Cli, "platform_call", &arguments);
        assert!(
            matches!(effect, Effect::Read),
            "`{operation}` must not be treated as a mutation"
        );
        assert!(effect.reconciliation().is_none(), "{operation}");
    }

    let arguments = json!({
        "operation": "connection_start",
        "body": {"toolkit": "google_search_console"}
    })
    .as_object()
    .cloned()
    .unwrap();
    let effect = call_effect(Surface::Cli, "platform_call", &arguments);
    assert!(effect.is_mutation(), "starting an enrollment mints a link");
    let note = effect
        .reconciliation()
        .expect("an uncertain start has a read that reconciles it");
    assert!(note.contains("connection ls"), "{note}");
    assert!(
        !note.contains("platform status"),
        "an enrollment link is not a platform request: {note}"
    );
}

// CAD-2127: a 503 to a read cannot have started or billed anything, so it is
// `rejected` availability — never the paid-call unknown that says the caller
// may have been charged.
#[test]
fn cad2127_platform_read_503_is_rejected_availability_not_charged_unknown() {
    for operation in ["status", "artifact"] {
        let arguments = json!({"operation": operation, "key": "cad2127-run-1"})
            .as_object()
            .cloned()
            .unwrap();
        let (out, requests) = run(
            vec![(
                503,
                "",
                json!({"ok": false, "error": {"code": "capability_unavailable"}}),
            )],
            "platform_call",
            arguments,
            10,
        );
        let error = out.expect_err("a refused read is not a successful lookup");
        assert_eq!(error.kind(), "rejected", "{operation}: {error}");
        assert_eq!(
            crate::error::exit_code_for_kind(error.kind()),
            3,
            "{operation}"
        );
        let text = error.to_string();
        assert!(text.contains("availability"), "{operation}: {text}");
        assert!(
            !text.contains("charg"),
            "{operation}: a read cannot be charged: {text}"
        );
        assert!(
            !text.contains("may already have"),
            "{operation}: a read must not claim a lost mutation: {text}"
        );
        assert!(!text.contains("outcome_unknown"), "{operation}: {text}");
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "{operation}: an availability refusal is not replayed"
        );
    }
}

// CAD-2127: a 200 `status` lookup is usable only when it proves one of the
// three outcome states. Anything else cannot be read as `recorded`, so it is
// refused rather than reported as a confirmed call.
#[test]
fn cad2127_platform_status_requires_a_known_outcome() {
    let arguments = || {
        json!({"operation": "status", "key": "cad2127-run-1"})
            .as_object()
            .cloned()
            .unwrap()
    };

    for outcome in ["recorded", "unrecorded", "unknown"] {
        let (out, _) = run(
            vec![(
                200,
                "",
                json!({"ok": true, "data": {"outcome": outcome, "key": "cad2127-run-1"}}),
            )],
            "platform_call",
            arguments(),
            10,
        );
        let body = out.unwrap_or_else(|error| panic!("{outcome}: {error}"));
        assert_eq!(body["data"]["outcome"], json!(outcome), "{outcome}");
    }

    for body in [
        json!({"ok": true, "data": {}}),
        json!({"ok": true, "data": {"outcome": "pending"}}),
        json!({"ok": true, "data": {"outcome": "RECORDED"}}),
        json!({"ok": true, "data": {"outcome": null}}),
        json!({"ok": false, "data": {"outcome": "recorded"}}),
        json!({"ok": true}),
        json!({"ok": true, "data": []}),
        Value::Null,
    ] {
        let (out, requests) = run(
            vec![(200, "", body.clone())],
            "platform_call",
            arguments(),
            10,
        );
        let error = out.expect_err("an uncorrelated status answer is not a lookup result");
        assert_eq!(error.kind(), "rejected", "{body}: {error}");
        let text = error.to_string();
        assert!(text.contains("status lookup"), "{body}: {text}");
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "{body}: the read is not retried"
        );
    }

    // The artifact window has its own usable shape: `ok` plus an object `data`.
    let artifact = || {
        json!({"operation": "artifact", "ref": format!("med_job-1.{}", "a".repeat(64)), "offset": 0})
            .as_object()
            .cloned()
            .unwrap()
    };
    let (out, _) = run(
        vec![(
            200,
            "",
            json!({"ok": true, "data": {"ref": format!("med_job-1.{}", "a".repeat(64)), "offset": 0}}),
        )],
        "platform_call",
        artifact(),
        10,
    );
    assert!(out.is_ok(), "{:?}", out.err().map(|e| e.to_string()));
    let (out, _) = run(
        vec![(200, "", json!({"ok": true}))],
        "platform_call",
        artifact(),
        10,
    );
    let error = out.expect_err("an artifact reply without a data window is unusable");
    assert_eq!(error.kind(), "rejected", "{error}");
    assert!(error.to_string().contains("artifact read"), "{error}");
}

// CAD-2127: a tenant older than `operation: status|artifact` must be reported
// as a release-order problem, not as a retryable availability blip and not as
// "the request was never made".
#[test]
fn cad2127_older_tenant_refuses_platform_status_and_artifact_by_release_order() {
    for operation in ["status", "artifact"] {
        for (status, code) in [
            (501u16, "not_implemented"),
            (400, "invalid_request"),
            (400, "unknown_verb"),
        ] {
            let arguments = json!({"operation": operation, "key": "cad2127-run-1"})
                .as_object()
                .cloned()
                .unwrap();
            let (out, requests) = run(
                vec![(status, "", json!({"ok": false, "error": {"code": code}}))],
                "platform_call",
                arguments,
                10,
            );
            let error = out.expect_err("an old tenant cannot serve the read");
            assert_eq!(
                error.kind(),
                "rejected",
                "{operation} {status} {code}: {error}"
            );
            assert_eq!(
                crate::error::exit_code_for_kind(error.kind()),
                3,
                "{operation} {status} {code}"
            );
            let text = error.to_string();
            assert!(
                text.contains("update the tenant server first"),
                "{operation} {status} {code}: {text}"
            );
            assert!(
                text.contains(&format!("platform {operation}")),
                "{operation} {status} {code}: {text}"
            );
            assert!(
                text.contains("nothing was started or billed"),
                "{operation} {status} {code}: {text}"
            );
            assert!(
                !text.contains("retry later"),
                "{operation} {status} {code}: {text}"
            );
            assert_eq!(
                hits(&requests, "/__platform/cli/call"),
                1,
                "{operation} {status} {code}: no replay"
            );
        }
    }
}

// CAD-2127: an ambiguous paid call keeps the server's own proof (`code`,
// `stage`, `recorded`) and points at the read-only lookup before a repeat that
// could start and charge a second call.
#[test]
fn cad2127_platform_call_maintenance_preserves_proof_and_reconciliation() {
    let arguments = json!({
        "operation": "call",
        "tool": "generate_image",
        "key": "cad2127-acceptance-run-1",
        "body": {"prompt": "synthetic acceptance input"},
    })
    .as_object()
    .cloned()
    .unwrap();
    let (out, requests) = run(
        vec![(
            503,
            "",
            json!({"ok": false, "error": {
                "code": "maintenance",
                "stage": "delivery_admission",
                "recorded": false,
                "message": "do-not-print-server-text",
            }}),
        )],
        "platform_call",
        arguments,
        10,
    );
    let error = out.expect_err("an ambiguous paid call is never a success receipt");
    assert_eq!(error.kind(), "unknown", "{error}");
    assert_eq!(crate::error::exit_code_for_kind(error.kind()), 1, "{error}");
    let text = error.to_string();
    assert!(text.contains("HTTP 503"), "{text}");
    assert!(text.contains("code maintenance"), "{text}");
    assert!(text.contains("stage delivery_admission"), "{text}");
    assert!(text.contains("recorded=false"), "{text}");
    assert!(
        text.contains("platform status"),
        "the caller must reconcile read-only first: {text}"
    );
    assert!(text.contains("--request"), "{text}");
    assert!(
        text.contains("start and charge a new call"),
        "a replay can charge a new call: {text}"
    );
    assert!(text.contains("unrecorded"), "{text}");
    assert!(
        text.contains("get_media_job"),
        "polling the existing job is the alternative to replaying: {text}"
    );
    assert!(!text.contains("retry later"), "{text}");
    assert!(
        !text.contains("do-not-print-server-text"),
        "only bounded diagnostics may be echoed: {text}"
    );
    assert_eq!(
        hits(&requests, "/__platform/cli/call"),
        1,
        "an ambiguous paid call is not automatically replayed"
    );
}

// CAD-2127 follow-up (independent security review): a 200 `status` answer is
// evidence only for the selector this call sent. The media lookup contract
// (`GET /capabilities/media/lookup`, apps/api/src/media/routes.ts) echoes the
// row's caller key as `data.key` and its job as `data.job` (`jobView.id`); a
// job-scoped lookup always answers a job, and an unrecorded/unknown key
// answer keeps the key with no job. An answer that drops or substitutes a
// requested selector cannot reconcile this request.
#[test]
fn cad2127_platform_status_correlates_a_requested_key() {
    let arguments = json!({"operation": "status", "key": "cad2127-run-1"})
        .as_object()
        .cloned()
        .unwrap();

    // The requested key is echoed. Its job may be present (a recorded row),
    // absent (unrecorded) or null (a multi-namespace unknown) — none of those
    // may be read as a different request.
    for data in [
        json!({
            "outcome": "recorded",
            "recorded": true,
            "key": "cad2127-run-1",
            "job": {"id": "med_job-a", "status": "succeeded"},
            "billing": {"state": "settled", "chargeMinor": 123_456},
        }),
        json!({
            "outcome": "unrecorded",
            "recorded": false,
            "key": "cad2127-run-1",
            "job": null,
            "billing": null,
        }),
        json!({
            "outcome": "unknown",
            "recorded": false,
            "key": "cad2127-run-1",
            "job": null,
            "billing": null,
            "note": "The key is present or uncertain in multiple tool namespaces.",
        }),
    ] {
        let body = json!({"ok": true, "data": data.clone()});
        let (out, requests) = run(
            vec![(200, "", body.clone())],
            "platform_call",
            arguments.clone(),
            10,
        );
        let answered = out.unwrap_or_else(|error| panic!("{body}: {error}"));
        assert_eq!(answered["data"]["key"], json!("cad2127-run-1"), "{body}");
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "{body}: the read is not retried"
        );
    }

    for data in [
        // The key is missing entirely: nothing ties the answer to this request.
        json!({"outcome": "recorded"}),
        json!({"outcome": "recorded", "key": null}),
        json!({"outcome": "unrecorded", "key": null, "job": null}),
        // Another request's key: the row is real, but it is not this lookup.
        json!({
            "outcome": "recorded",
            "key": "key-b",
            "job": {"id": "med_job-b", "status": "succeeded"},
        }),
        // A non-string key is not the requested handle.
        json!({"outcome": "recorded", "key": 7, "job": null}),
    ] {
        let body = json!({"ok": true, "data": data.clone()});
        let (out, requests) = run(
            vec![(200, "", body.clone())],
            "platform_call",
            arguments.clone(),
            10,
        );
        let error = out.expect_err("an uncorrelated key answer is not evidence");
        assert_eq!(error.kind(), "rejected", "{body}: {error}");
        assert_eq!(crate::error::exit_code_for_kind(error.kind()), 3, "{body}");
        let text = error.to_string();
        assert!(text.contains("status"), "{body}: {text}");
        assert!(
            !text.contains("may already have"),
            "{body}: a read never reports a lost mutation: {text}"
        );
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "{body}: no replay"
        );
    }
}

#[test]
fn cad2127_platform_status_correlates_a_requested_job() {
    let arguments = json!({"operation": "status", "jobId": "med_job-1"})
        .as_object()
        .cloned()
        .unwrap();

    // A job lookup answers the requested job. The row's caller key is not a
    // selector here, so it may be any key the workspace stored.
    for data in [
        json!({
            "outcome": "recorded",
            "recorded": true,
            "key": "another-caller-key",
            "job": {"id": "med_job-1", "status": "succeeded"},
            "billing": {"state": "settled", "chargeMinor": 60_000},
        }),
        json!({
            "outcome": "unknown",
            "recorded": true,
            "key": "another-caller-key",
            "job": {"id": "med_job-1", "status": "uncertain"},
            "billing": {"state": "uncertain"},
        }),
    ] {
        let body = json!({"ok": true, "data": data.clone()});
        let (out, requests) = run(
            vec![(200, "", body.clone())],
            "platform_call",
            arguments.clone(),
            10,
        );
        let answered = out.unwrap_or_else(|error| panic!("{body}: {error}"));
        assert_eq!(answered["data"]["job"]["id"], json!("med_job-1"), "{body}");
        assert_eq!(hits(&requests, "/__platform/cli/call"), 1, "{body}");
    }

    for data in [
        // No job at all: a key-shaped answer cannot reconcile a job lookup.
        json!({"outcome": "recorded", "key": "another-caller-key"}),
        json!({"outcome": "unrecorded", "key": "another-caller-key", "job": null}),
        // A job row without the id it was asked for.
        json!({"outcome": "recorded", "key": "another-caller-key", "job": {"status": "succeeded"}}),
        // Another job's row.
        json!({"outcome": "recorded", "key": "another-caller-key", "job": {"id": "med_job-2"}}),
        // A job that is not a row object cannot carry the selector.
        json!({"outcome": "recorded", "key": "another-caller-key", "job": "med_job-1"}),
    ] {
        let body = json!({"ok": true, "data": data.clone()});
        let (out, requests) = run(
            vec![(200, "", body.clone())],
            "platform_call",
            arguments.clone(),
            10,
        );
        let error = out.expect_err("an uncorrelated job answer is not evidence");
        assert_eq!(error.kind(), "rejected", "{body}: {error}");
        let text = error.to_string();
        assert!(text.contains("status"), "{body}: {text}");
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "{body}: no replay"
        );
    }
}

#[test]
fn cad2127_platform_status_correlates_both_selectors() {
    let arguments = json!({
        "operation": "status",
        "key": "cad2127-run-1",
        "jobId": "med_job-1",
    })
    .as_object()
    .cloned()
    .unwrap();

    // With both selectors, both must be echoed: the media door only answers
    // 200 when the key owns that job (a mismatch is 409 key_conflict).
    let matched = json!({"ok": true, "data": {
        "outcome": "recorded",
        "recorded": true,
        "key": "cad2127-run-1",
        "job": {"id": "med_job-1", "status": "succeeded"},
        "billing": {"state": "settled", "chargeMinor": 123_456},
    }});
    let (out, requests) = run(
        vec![(200, "", matched.clone())],
        "platform_call",
        arguments.clone(),
        10,
    );
    let answered = out.unwrap_or_else(|error| panic!("{matched}: {error}"));
    assert_eq!(answered["data"]["key"], json!("cad2127-run-1"));
    assert_eq!(answered["data"]["job"]["id"], json!("med_job-1"));
    assert_eq!(hits(&requests, "/__platform/cli/call"), 1);

    for data in [
        // Job matches, key is beyond the request.
        json!({"outcome": "recorded", "key": "cad2127-run-1", "job": {"id": "med_job-2"}}),
        // Key matches, job is beyond the request.
        json!({"outcome": "recorded", "key": "key-b", "job": {"id": "med_job-1"}}),
        // One selector missing each way.
        json!({"outcome": "recorded", "key": "cad2127-run-1"}),
        json!({"outcome": "recorded", "key": "cad2127-run-1", "job": null}),
        json!({"outcome": "recorded", "job": {"id": "med_job-1"}}),
        // Neither selector echoed.
        json!({"outcome": "unrecorded"}),
    ] {
        let body = json!({"ok": true, "data": data.clone()});
        let (out, requests) = run(
            vec![(200, "", body.clone())],
            "platform_call",
            arguments.clone(),
            10,
        );
        let error = out.expect_err("both selectors are required when both were sent");
        assert_eq!(error.kind(), "rejected", "{body}: {error}");
        let text = error.to_string();
        assert!(text.contains("status"), "{body}: {text}");
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "{body}: no replay"
        );
    }
}

// CAD-2149: a managed session's hosted verbs ride its own credential. These
// drive the real client (`call_verb_until_retry`) against a synthetic board and
// read what the board saw: which bearer carried each request.
mod agent_credential_acceptance {
    use super::*;
    use std::sync::Mutex;

    const AGENT: &str = "hct_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    const ASSIGNMENT: &str = "hla_00000000-0000-4000-8000-000000000001";
    const CLAIM: &str = "hlc_00000000-0000-4000-8000-000000000002";
    const ISSUER: &str = "https://issuer.invalid";
    const SUBJECT: &str = "usr_member";

    /// What the synthetic board answers, by route and bearer.
    struct Board {
        mint: (u16, Value),
        agent_authorize: (u16, Value),
        agent_call: (u16, Value),
        login_call: (u16, Value),
    }

    fn minted() -> (u16, Value) {
        (
            200,
            json!({
                "version": "hosted-cadence-local-assignment.v1",
                "assignment_id": ASSIGNMENT,
                "generation": 1,
                "claim_id": CLAIM,
                "principal": {"kind": "agent", "agent_id": CLAIM, "actor": format!("agents/{CLAIM}")},
                "capabilities": ["cli.read", "cli.write", "wiki.read"],
                "credential": {
                    "credential_id": "hca_00000000-0000-4000-8000-000000000003",
                    "access_token": AGENT,
                    "token_type": "Bearer",
                    "issued_at": now().unwrap(),
                    "expires_at": now().unwrap() + 900,
                    "renewal": "remint",
                },
            }),
        )
    }

    fn envelope(n: usize) -> Value {
        json!({"version": CLI_VERSION, "envelope": format!("h.e.{n}"),
            "bearer": "x", "expires_at": 0, "actor": "users/o", "scope": ["cli.read"], "issuer": "i"})
    }

    fn comment_receipt() -> Value {
        json!({"issue": {"id": "SYN-1"}, "comment": {"body": "Synthetic"}})
    }

    fn board() -> Board {
        Board {
            mint: minted(),
            agent_authorize: (200, envelope(0)),
            agent_call: (200, comment_receipt()),
            login_call: (200, comment_receipt()),
        }
    }

    fn status_line(status: u16) -> &'static str {
        match status {
            200 => "200 OK",
            401 => "401 Unauthorized",
            403 => "403 Forbidden",
            404 => "404 Not Found",
            409 => "409 Conflict",
            500 => "500 Internal Server Error",
            _ => "400 Bad Request",
        }
    }

    struct Session {
        target: RemoteTarget,
        dir: PathBuf,
        slot: AgentSlot,
        _root: tempfile::TempDir,
        _listener: TcpListener,
        seen: std::sync::Arc<Mutex<Vec<Request>>>,
        notes: Mutex<Vec<String>>,
    }

    /// `n` is the exact number of requests the board will serve.
    fn session(board: Board, n: usize) -> Session {
        let counter = std::sync::atomic::AtomicUsize::new(0);
        let (listener, seen, _server) = finite_worker(n, move |_, socket, req| {
            let (status, body) = if req.path.ends_with("/agent-credential") {
                board.mint.clone()
            } else if req.path.ends_with("/authorize") {
                if req.bearer == AGENT {
                    board.agent_authorize.clone()
                } else {
                    let version = if req.path.contains("/wiki/") {
                        WIKI_VERSION
                    } else {
                        CLI_VERSION
                    };
                    let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    (
                        200,
                        json!({"version": version, "envelope": format!("h.l.{n}")}),
                    )
                }
            } else if req.bearer == AGENT {
                board.agent_call.clone()
            } else {
                board.login_call.clone()
            };
            write_answer(socket, status_line(status), "", &body);
        });
        let target = RemoteTarget {
            identity: Some(CliIdentityPin {
                subject_id: SUBJECT.into(),
                issuer: ISSUER.into(),
            }),
            ..target(&listener)
        };
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("auth");
        let assignments = root.path().join("hosted-assignments");
        for path in [&dir, &assignments] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let login = json!({"version": "hosted-cadence-auth.v1", "organization_id": ORG_ID,
            "organization_slug": ORG, "audience": target.endpoint, "issuer": ISSUER,
            "subject_id": SUBJECT, "capabilities": ["cli.read", "cli.write", "wiki.read"],
            "access_token": BRIDGE, "expires_at": now().unwrap() + 300});
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(dir.join(format!("cli-{ORG}.json")))
            .unwrap();
        file.write_all(login.to_string().as_bytes()).unwrap();
        let slot = AgentSlot::new(
            assignments.join("msg_1.agent"),
            &target,
            ASSIGNMENT.into(),
            CLAIM.into(),
            1,
        );
        Session {
            target,
            dir,
            slot,
            _root: root,
            _listener: listener,
            seen,
            notes: Mutex::new(Vec::new()),
        }
    }

    impl Session {
        fn send(&self, managed: bool, verb: &str, args: Value) -> Result<Value> {
            let deadline = Instant::now() + Duration::from_secs(10);
            call_verb_until_retry(
                &self.target,
                verb,
                args.as_object().unwrap().clone(),
                deadline,
                10,
                &self.dir,
                true,
                managed.then_some(&self.slot),
                |line| self.notes.lock().unwrap().push(line.to_string()),
                |_| {},
            )
            .map(|answer| match answer {
                RemoteAnswer::Ok(body) | RemoteAnswer::Conflict(body) => body,
            })
        }

        fn comment(&self, managed: bool) -> Result<Value> {
            self.send(
                managed,
                "issue_comment",
                json!({"id": "SYN-1", "body": "Synthetic"}),
            )
        }

        /// (route, which bearer) for every request the board served.
        fn trail(&self) -> Vec<(String, &'static str)> {
            self.seen
                .lock()
                .unwrap()
                .iter()
                .map(|req| {
                    let route = req.path.rsplit('/').next().unwrap().to_string();
                    let bearer = match req.bearer.as_str() {
                        AGENT => "agent",
                        BRIDGE => "login",
                        _ => "other",
                    };
                    (route, bearer)
                })
                .collect()
        }

        fn lines(&self) -> Vec<String> {
            self.notes.lock().unwrap().clone()
        }
    }

    fn step(route: &str, bearer: &'static str) -> (String, &'static str) {
        (route.to_string(), bearer)
    }

    #[test]
    fn a_managed_comment_is_sent_on_the_agent_credential_and_it_is_cached_private() {
        let s = session(board(), 5);
        s.comment(true).unwrap();
        s.comment(true).unwrap();
        // One mint on the member's login, then only the agent bearer; the second
        // comment reuses the cached credential instead of minting again.
        assert_eq!(
            s.trail(),
            vec![
                step("agent-credential", "login"),
                step("authorize", "agent"),
                step("call", "agent"),
                step("authorize", "agent"),
                step("call", "agent"),
            ]
        );
        let mint = s.seen.lock().unwrap()[0].body.clone();
        assert_eq!(
            mint,
            json!({"version": "hosted-cadence-local-assignment.v1", "organization_id": ORG_ID,
                "expected_generation": 1, "claim_id": CLAIM})
        );
        let meta = std::fs::metadata(&s.slot.path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert!(std::fs::read_to_string(&s.slot.path)
            .unwrap()
            .contains(AGENT));
        assert!(s.lines().is_empty(), "{:?}", s.lines());
    }

    #[test]
    fn a_board_without_the_mint_keeps_the_login_with_one_line() {
        for (status, body) in [
            (404, json!({"ok": false, "code": "not_found"})),
            (
                409,
                json!({"ok": false, "code": "renewable_login_required"}),
            ),
        ] {
            let mut b = board();
            b.mint = (status, body);
            let s = session(b, 3);
            s.comment(true).unwrap();
            assert_eq!(
                s.trail(),
                vec![
                    step("agent-credential", "login"),
                    step("authorize", "login"),
                    step("call", "login"),
                ],
                "HTTP {status}"
            );
            assert_eq!(s.lines().len(), 1, "{:?}", s.lines());
            assert!(!s.slot.path.exists());
        }
    }

    #[test]
    fn wiki_and_the_audit_verdicts_stay_on_the_login_and_a_review_rides_the_credential() {
        let mut b = board();
        b.agent_call = (
            200,
            json!({"review": {"artifact": "a", "digest": "d", "decision": "approve"}}),
        );
        let s = session(b, 9);
        s.send(true, "read", json!({"path": "notes"})).unwrap();
        s.send(true, "audit_verdict_project", json!({"project": "p"}))
            .unwrap();
        s.send(true, "audit_verdict", json!({"repo": "r"})).unwrap();
        assert_eq!(
            s.trail(),
            vec![
                step("authorize", "login"),
                step("call", "login"),
                step("authorize", "login"),
                step("call", "login"),
                step("authorize", "login"),
                step("call", "login"),
            ]
        );
        // `issue review` is judged against an artifact's author, and an attach
        // from this session is recorded as the agent: it must be the agent too.
        s.send(
            true,
            "issue_review",
            json!({"artifact": "a", "digest": "d", "decision": "approve"}),
        )
        .unwrap();
        let trail = s.trail();
        assert_eq!(
            trail[6..],
            [
                step("agent-credential", "login"),
                step("authorize", "agent"),
                step("call", "agent"),
            ]
        );
        assert!(s.lines().is_empty());
    }

    // The board's own `issued_at` bounds the lifetime; a slow mint (the client
    // reads the clock after the answer) is not aged out of it.
    #[test]
    fn a_slow_mint_is_judged_from_the_boards_issued_at() {
        let target = RemoteTarget {
            org: ORG.into(),
            endpoint: "http://127.0.0.1:1".into(),
            org_id: ORG_ID.into(),
            identity: None,
        };
        let slot = AgentSlot::new(
            PathBuf::from("/nonexistent/x.agent"),
            &target,
            ASSIGNMENT.into(),
            CLAIM.into(),
            1,
        );
        let answer = |issued: u64, expires: u64| {
            let mut value = minted().1;
            value["credential"]["issued_at"] = json!(issued);
            value["credential"]["expires_at"] = json!(expires);
            value
        };
        let at = 1_000_000;
        let check = |issued, expires, at| {
            agent_credential::validated(&slot, &target, Some(&answer(issued, expires)), at)
        };
        // Issued 8 s before the client looked at the clock: still usable.
        assert!(check(at - 8, at - 8 + 900, at).is_ok());
        // Past its own lifetime, issued in the future, or longer than 900 s.
        assert!(check(at - 901, at - 1, at).is_err());
        assert!(check(at + 60, at + 960, at).is_err());
        assert!(check(at, at + 100_000, at).is_err());
    }
}

// CAD-2149 / AOS-502: independent refusal acceptance for the managed session's
// own credential, written from the two tickets and the server contract, not
// from the implementation. Each check drives the real client
// (`call_verb_until_retry`) against a synthetic loopback board and reads which
// bearer carried every request. The implementer must not edit it.
mod agent_credential_refusals {
    use super::*;
    use std::net::Shutdown;
    use std::sync::{atomic::AtomicUsize, Arc, Mutex};

    const ASSIGNMENT: &str = "hla_00000000-0000-4000-8000-000000000001";
    const CLAIM: &str = "hlc_00000000-0000-4000-8000-000000000002";
    const OTHER_CLAIM: &str = "hlc_00000000-0000-4000-8000-000000000009";
    const ISSUER: &str = "https://issuer.invalid";
    const SUBJECT: &str = "usr_member";
    const LOGIN: &str = "login";

    fn tok(c: char) -> String {
        format!("hct_{}", c.to_string().repeat(43))
    }

    /// Which token the board mints for a claim and generation, so a request
    /// shows exactly which mint it rode on.
    fn tok_for(claim: &str, generation: u64) -> String {
        tok(match (claim == CLAIM, generation) {
            (true, 1) => 'A',
            (true, _) => 'C',
            (false, _) => 'D',
        })
    }

    fn minted(claim: &str, generation: u64, ttl: u64) -> Value {
        json!({
            "version": "hosted-cadence-local-assignment.v1",
            "assignment_id": ASSIGNMENT,
            "generation": generation,
            "claim_id": claim,
            "principal": {"kind": "agent", "agent_id": claim, "actor": format!("agents/{claim}")},
            "capabilities": ["cli.read", "cli.write", "wiki.read"],
            "credential": {
                "credential_id": "hca_00000000-0000-4000-8000-000000000003",
                "access_token": tok_for(claim, generation),
                "token_type": "Bearer",
                "issued_at": now().unwrap(),
                "expires_at": now().unwrap() + ttl,
                "renewal": "remint",
            },
        })
    }

    fn refused(code: &str) -> Value {
        json!({"ok": false, "error": {"code": code}})
    }

    enum Reply {
        Json(u16, Value),
        Raw(u16, &'static str),
        Hangup,
    }

    fn reason(status: u16) -> &'static str {
        match status {
            200 => "200 OK",
            401 => "401 Unauthorized",
            403 => "403 Forbidden",
            409 => "409 Conflict",
            500 => "500 Internal Server Error",
            _ => "400 Bad Request",
        }
    }

    fn deliver(socket: &mut std::net::TcpStream, reply: Reply) {
        match reply {
            Reply::Json(status, body) => write_answer(socket, reason(status), "", &body),
            Reply::Raw(status, text) => {
                let _ = write!(
                    socket,
                    "HTTP/1.1 {}\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                    reason(status),
                    text.len()
                );
            }
            // The request was read; the answer is lost.
            Reply::Hangup => {
                let _ = socket.shutdown(Shutdown::Both);
            }
        }
    }

    fn agent(req: &Request) -> bool {
        req.bearer != BRIDGE
    }

    struct Rig {
        target: RemoteTarget,
        dir: PathBuf,
        root: tempfile::TempDir,
        seen: Arc<Mutex<Vec<Request>>>,
        notes: Mutex<Vec<String>>,
        _listener: TcpListener,
    }

    /// A synthetic board serving at most `n` requests. `script` may override
    /// the answer to a route (the last path segment); the default board mints
    /// what was asked for, authorizes and answers every call.
    fn rig(n: usize, script: impl Fn(&str, &Request) -> Option<Reply> + Send + 'static) -> Rig {
        let envelopes = AtomicUsize::new(0);
        let (listener, seen, _server) = finite_worker(n, move |_, socket, req| {
            let route = req.path.rsplit('/').next().unwrap().to_owned();
            let reply = script(&route, req).unwrap_or_else(|| match route.as_str() {
                "agent-credential" => ok200(minted(
                    req.body["claim_id"].as_str().unwrap_or_default(),
                    req.body["expected_generation"].as_u64().unwrap_or_default(),
                    900,
                )),
                "authorize" => ok200(json!({"version": CLI_VERSION, "envelope": format!(
                    "h.e.{}", envelopes.fetch_add(1, std::sync::atomic::Ordering::SeqCst))})),
                _ => ok200(json!({"issue": {"id": "SYN-1"}, "comment": {"body": "Synthetic"}})),
            });
            deliver(socket, reply);
        });
        let target = RemoteTarget {
            identity: Some(CliIdentityPin {
                subject_id: SUBJECT.into(),
                issuer: ISSUER.into(),
            }),
            ..target(&listener)
        };
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("auth");
        for path in [&dir, &root.path().join("hosted-assignments")] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let login = json!({"version": "hosted-cadence-auth.v1", "organization_id": ORG_ID,
            "organization_slug": ORG, "audience": target.endpoint, "issuer": ISSUER,
            "subject_id": SUBJECT, "capabilities": ["cli.read", "cli.write", "wiki.read"],
            "access_token": BRIDGE, "expires_at": now().unwrap() + 300});
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(dir.join(format!("cli-{ORG}.json")))
            .unwrap();
        file.write_all(login.to_string().as_bytes()).unwrap();
        Rig {
            target,
            dir,
            root,
            seen,
            notes: Mutex::new(Vec::new()),
            _listener: listener,
        }
    }

    fn ok200(body: Value) -> Reply {
        Reply::Json(200, body)
    }

    impl Rig {
        fn slot(&self, claim: &str, generation: u64) -> AgentSlot {
            AgentSlot::new(
                self.root.path().join("hosted-assignments/msg_1.agent"),
                &self.target,
                ASSIGNMENT.into(),
                claim.into(),
                generation,
            )
        }

        /// `None` is a human shell: no managed session was bound.
        fn send(&self, slot: Option<&AgentSlot>, verb: &str, args: Value) -> Result<Value> {
            call_verb_until_retry(
                &self.target,
                verb,
                args.as_object().unwrap().clone(),
                Instant::now() + Duration::from_secs(10),
                10,
                &self.dir,
                true,
                slot,
                |line| self.notes.lock().unwrap().push(line.to_string()),
                |_| {},
            )
            .map(|answer| match answer {
                RemoteAnswer::Ok(body) | RemoteAnswer::Conflict(body) => body,
            })
        }

        fn comment(&self, slot: Option<&AgentSlot>) -> Result<Value> {
            self.send(
                slot,
                "issue_comment",
                json!({"id": "SYN-1", "body": "Synthetic"}),
            )
        }

        /// `route:bearer` for every request the board served, where the bearer
        /// is `login` or the first letter of the minted token.
        fn trail(&self) -> Vec<String> {
            self.seen
                .lock()
                .unwrap()
                .iter()
                .map(|req| {
                    let who = if req.bearer == BRIDGE {
                        LOGIN.to_string()
                    } else {
                        req.bearer[4..5].to_string()
                    };
                    format!("{}:{who}", req.path.rsplit('/').next().unwrap())
                })
                .collect()
        }

        fn notes(&self) -> Vec<String> {
            self.notes.lock().unwrap().clone()
        }
    }

    const MINT: &str = "agent-credential:login";

    // 1. The agent's own refusals stand.
    #[test]
    fn a_refusal_on_the_agent_credential_is_never_retried_on_the_login() {
        let profile = json!({"id": "ap_synthetic", "expectedRevision": 1, "model": "m"});
        for (verb, args, stage, status, code) in [
            ("team_invites", json!({}), "authorize", 401, "unauthorized"),
            ("team_list", json!({}), "authorize", 403, "forbidden"),
            (
                "agent_profile_update",
                profile.clone(),
                "authorize",
                403,
                "forbidden",
            ),
            ("agent_profile_update", profile, "call", 403, "forbidden"),
        ] {
            let r = rig(8, move |route, req| {
                (agent(req) && route == stage).then(|| Reply::Json(status, refused(code)))
            });
            let slot = r.slot(CLAIM, 1);
            r.send(Some(&slot), verb, args).unwrap_err();
            let mut want = vec![MINT, "authorize:A"];
            if stage == "call" {
                want.push("call:A");
            }
            assert_eq!(r.trail(), want, "{verb} {status} {code} at {stage}");
            assert!(r.notes().is_empty(), "{verb}: {:?}", r.notes());
        }
    }

    // 1b. A cached bearer the board stops accepting is dropped and re-minted
    // once — the cache must not fail every verb until it ages out.
    #[test]
    fn a_cached_credential_401_is_dropped_and_re_minted_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let authorizes = AtomicUsize::new(0);
        let r = rig(12, move |route, req| {
            // The first send mints and authorizes; the second send's cached
            // bearer is the one the board refuses.
            if agent(req)
                && route == "authorize"
                && authorizes.fetch_add(1, Ordering::SeqCst) + 1 == 2
            {
                return Some(Reply::Json(401, refused("unauthorized")));
            }
            None
        });
        let slot = r.slot(CLAIM, 1);
        r.comment(Some(&slot)).unwrap();
        r.comment(Some(&slot)).unwrap();
        assert_eq!(
            r.trail(),
            vec![
                MINT,
                "authorize:A",
                "call:A",
                "authorize:A",
                MINT,
                "authorize:A",
                "call:A",
            ],
            "cached 401 was not re-minted exactly once"
        );
        assert!(r.notes().is_empty(), "{:?}", r.notes());
    }

    // 1c. A policy 401 is authorization, not a stale bearer: no re-mint.
    #[test]
    fn a_policy_401_on_a_cached_credential_is_not_re_minted() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let authorizes = AtomicUsize::new(0);
        let r = rig(8, move |route, req| {
            if agent(req)
                && route == "authorize"
                && authorizes.fetch_add(1, Ordering::SeqCst) + 1 == 2
            {
                return Some(Reply::Json(
                    401,
                    json!({"ok": false, "error": {
                        "code": "unauthorized", "reason": "scope_not_granted"}}),
                ));
            }
            None
        });
        let slot = r.slot(CLAIM, 1);
        r.comment(Some(&slot)).unwrap();
        r.comment(Some(&slot)).unwrap_err();
        assert_eq!(
            r.trail(),
            vec![MINT, "authorize:A", "call:A", "authorize:A"],
            "a policy 401 was re-minted"
        );
    }

    // 2. Exactly `human_session_required` falls back, once; nothing else does.
    #[test]
    fn only_exactly_human_session_required_falls_back_and_says_so_once() {
        for stage in ["authorize", "call"] {
            let r = rig(12, move |route, req| {
                (agent(req) && route == stage)
                    .then(|| Reply::Json(403, refused("human_session_required")))
            });
            let slot = r.slot(CLAIM, 1);
            r.comment(Some(&slot)).unwrap();
            r.comment(Some(&slot)).unwrap();
            let mut want = vec![MINT, "authorize:A"];
            if stage == "call" {
                want.push("call:A");
            }
            want.extend([
                "authorize:login",
                "call:login",
                "authorize:login",
                "call:login",
            ]);
            assert_eq!(r.trail(), want, "fallback at {stage}");
            let notes = r.notes();
            assert_eq!(notes.len(), 1, "{notes:?}");
            assert!(!notes[0].contains("hct_"), "{notes:?}");
        }
        let lookalikes = [
            (
                "authorize",
                Reply::Json(
                    403,
                    json!({"ok": false, "error": {
                "code": "forbidden", "message": "human_session_required"}}),
                ),
            ),
            (
                "authorize",
                Reply::Json(403, refused("human_session_required_x")),
            ),
            ("authorize", Reply::Raw(403, "human_session_required")),
            (
                "authorize",
                Reply::Raw(403, "<html>human_session_required</html>"),
            ),
            ("call", Reply::Raw(403, "human_session_required")),
            (
                "call",
                Reply::Json(403, refused("human_session_required_x")),
            ),
        ];
        for (i, (stage, reply)) in lookalikes.into_iter().enumerate() {
            let reply = std::sync::Mutex::new(Some(reply));
            let r = rig(8, move |route, req| {
                if agent(req) && route == stage {
                    reply.lock().unwrap().take()
                } else {
                    None
                }
            });
            let slot = r.slot(CLAIM, 1);
            r.comment(Some(&slot)).unwrap_err();
            let mut want = vec![MINT, "authorize:A"];
            if stage == "call" {
                want.push("call:A");
            }
            assert_eq!(r.trail(), want, "lookalike {i} at {stage} fell back");
            assert!(r.notes().is_empty(), "lookalike {i}: {:?}", r.notes());
        }
    }

    // 3. A write whose answer is lost is unknown: never replayed, never moved.
    #[test]
    fn a_lost_answer_to_a_write_under_the_agent_credential_is_unknown_with_no_replay() {
        let r = rig(8, |route, req| {
            (agent(req) && route == "call").then_some(Reply::Hangup)
        });
        let slot = r.slot(CLAIM, 1);
        let error = r.comment(Some(&slot)).unwrap_err();
        assert_eq!(error.kind(), "unknown", "{error}");
        assert_eq!(r.trail(), [MINT, "authorize:A", "call:A"]);
        assert!(r.notes().is_empty(), "{:?}", r.notes());
    }

    // 4. A mint that is not exactly this claim's refuses before anything is sent.
    #[test]
    fn a_bad_mint_refuses_with_nothing_sent() {
        type Case = (&'static str, Box<dyn Fn() -> Reply + Send>);
        let cases: Vec<Case> = vec![
            (
                "403",
                Box::new(|| Reply::Json(403, json!({"ok": false, "code": "forbidden"}))),
            ),
            (
                "409 fenced",
                Box::new(|| Reply::Json(409, json!({"ok": false, "code": "fenced"}))),
            ),
            (
                "409 terminal",
                Box::new(|| Reply::Json(409, json!({"ok": false, "code": "terminal"}))),
            ),
            (
                "500",
                Box::new(|| Reply::Json(500, json!({"ok": false, "code": "internal"}))),
            ),
            ("malformed", Box::new(|| ok200(json!({"unexpected": true})))),
            (
                "no credential",
                Box::new(|| {
                    let mut v = minted(CLAIM, 1, 900);
                    v.as_object_mut().unwrap().remove("credential");
                    ok200(v)
                }),
            ),
            (
                "oversized lifetime",
                Box::new(|| ok200(minted(CLAIM, 1, 100_000))),
            ),
            (
                "wrong assignment",
                Box::new(|| {
                    let mut v = minted(CLAIM, 1, 900);
                    v["assignment_id"] = json!("hla_00000000-0000-4000-8000-0000000000ff");
                    ok200(v)
                }),
            ),
            (
                "wrong claim",
                Box::new(|| ok200(minted(OTHER_CLAIM, 1, 900))),
            ),
            (
                "wrong generation",
                Box::new(|| ok200(minted(CLAIM, 2, 900))),
            ),
        ];
        for (name, reply) in cases {
            let reply = Mutex::new(Some(reply()));
            let r = rig(4, move |route, _| {
                if route == "agent-credential" {
                    reply.lock().unwrap().take()
                } else {
                    None
                }
            });
            let slot = r.slot(CLAIM, 1);
            r.comment(Some(&slot)).unwrap_err();
            assert_eq!(r.trail(), [MINT], "{name}");
            assert!(r.notes().is_empty(), "{name}: {:?}", r.notes());
            assert!(!slot.path.exists(), "{name} left a cache file");
        }
    }

    #[test]
    fn a_credential_cached_for_another_claim_or_generation_is_never_used() {
        let r = rig(12, |_, _| None);
        r.comment(Some(&r.slot(CLAIM, 1))).unwrap();
        // The same file path now holds A's credential. A different claim, then
        // a different generation of the first claim, must each mint afresh.
        r.comment(Some(&r.slot(OTHER_CLAIM, 1))).unwrap();
        r.comment(Some(&r.slot(CLAIM, 2))).unwrap();
        assert_eq!(
            r.trail(),
            [
                MINT,
                "authorize:A",
                "call:A",
                MINT,
                "authorize:D",
                "call:D",
                MINT,
                "authorize:C",
                "call:C",
            ]
        );
    }

    // 5. The credential file.
    #[test]
    fn the_credential_file_is_private_not_followed_and_never_printed() {
        let r = rig(12, |_, _| None);
        let slot = r.slot(CLAIM, 1);
        let out = r.comment(Some(&slot)).unwrap();
        let meta = std::fs::symlink_metadata(&slot.path).unwrap();
        assert!(meta.is_file());
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        // The bearer rides only in the Authorization header: never a URL, a
        // body, a note (stderr) or the verb's answer (stdout). The client
        // spawns no process, so there is no argv for it to appear on.
        assert!(!out.to_string().contains("hct_"), "{out}");
        assert!(r.notes().is_empty());
        for req in r.seen.lock().unwrap().iter() {
            assert!(!req.path.contains("hct_"), "{}", req.path);
            assert!(!req.body.to_string().contains("hct_"), "{}", req.path);
        }
        // Replace the file with a symlink to a perfectly good credential.
        let planted = r.root.path().join("planted.json");
        std::fs::rename(&slot.path, &planted).unwrap();
        let before = std::fs::read(&planted).unwrap();
        std::os::unix::fs::symlink(&planted, &slot.path).unwrap();
        r.comment(Some(&slot)).unwrap();
        assert_eq!(
            r.trail()[3..],
            [MINT, "authorize:A", "call:A"],
            "the symlink was read through instead of minting"
        );
        assert_eq!(std::fs::read(&planted).unwrap(), before, "written through");
    }

    // 6. A human shell is unchanged.
    #[test]
    fn a_human_shell_never_reads_the_credential_file() {
        let r = rig(8, |_, _| None);
        let slot = r.slot(CLAIM, 1);
        r.comment(Some(&slot)).unwrap();
        let cached = std::fs::read(&slot.path).unwrap();
        r.comment(None).unwrap();
        assert_eq!(r.trail()[3..], ["authorize:login", "call:login"]);
        assert_eq!(std::fs::read(&slot.path).unwrap(), cached);
        assert!(r.notes().is_empty());
    }
}
