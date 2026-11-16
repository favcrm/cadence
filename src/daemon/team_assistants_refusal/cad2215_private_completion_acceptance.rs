//! CAD-2215 independent private-COMPLETION acceptance. Authored by a worker
//! that is not the implementer; the implementer must not edit or weaken this
//! file (AGENTS.md "Gates and security work").
//!
//! The seam under test is the daemon's real worker-result path, not a
//! mirrored fake and not a bare `Store::finish`:
//!
//!   * a genuine signed `member_job_request` (Ed25519 envelope, live
//!     authority and issue scope) is promoted by the real sweep into a
//!     member-owned job and draft task;
//!   * the operator dispatches that task with the real `task_dispatch`
//!     RPC to a registered `tui-stub/pty` endpoint, whose `job_dispatch`
//!     kickoff is claimed and marked running exactly as the actor does;
//!   * the result is reported through the daemon RPC `message_report`
//!     (`kind=result`), which owns `finish_running`/`finish_in` and the
//!     CAD-2215 private-ledger settlement. No direct store write stands
//!     in for that path.
//!
//! What this file requires:
//!
//!   1. The ledger row exists the moment the completion is reported —
//!      BEFORE any verdict, accept or operator reconcile. It holds the
//!      exact dispatch message, the exact result text and SHA, the digest
//!      over that identity, and no publication. The owner's projection
//!      (`member_job_list.private_deliveries`) and the operator's
//!      (`delivery_private_list`) must already carry it. A recorder that
//!      waits for a verdict fails here.
//!   2. Completion is isolated: no shared issue comment, no ordinary
//!      `delivery.json` write and no tracker byte moves.
//!   3. An explicitly null SHA stays null. After a real `task_sha`
//!      repair, a real `task_verdict`, a real `reopen` + re-dispatch and
//!      the operator `delivery_private_reconcile`, the ORIGINAL result's
//!      null SHA and digest are byte-identical and never duplicated; the
//!      repaired `task.head_sha` must not leak into the stored result.
//!   4. A genuinely unavailable private ledger (its final path is a
//!      directory) never yields a private success claim and never
//!      fabricates a native completion. The exact later reconcile over
//!      the restored original ledger evidence recovers one identical
//!      result.
//!   5. An ordinary (non-assist) task completed through the same RPC is
//!      never recorded privately and never moves the private ledger;
//!      private and ordinary state stay byte-isolated.
//!   6. Only the proven operator may publish, and only an exact,
//!      authorized selection — every relay/forged/ambiguous attempt is
//!      refused without a shared effect.
//!   7. Result bytes are not a rendered summary: a whitespace-significant
//!      body is stored, digested and projected verbatim through the owner
//!      and operator projections, a real repair + verdict, and the
//!      operator reconcile.
//!   8. `agent_unfence --status completed` is a real completion too. With
//!      an unavailable private ledger it must keep the committed native
//!      reconcile and report the per-message private state as
//!      `unavailable` in `private_deliveries`, never silently drop it;
//!      after the exact original ledger evidence is restored, the
//!      operator reconcile recovers one exact result from the durable
//!      note.
//!   9. A member-private job, and an ambiguous queue-only `assist-` job
//!      with no unique provenance row, may not be dispatched to a
//!      sandbox endpoint at all: the refusal (code
//!      `cloud_sandbox_private_job`) precedes the hosted/HTTPS
//!      entitlement probe and leaves the task revision/kickoff/events,
//!      the private ledger and the tracker untouched. The recovery side
//!      refuses a previously dispatched kickoff before its claim and
//!      settles it failed with the same code and no remote work. No
//!      ordinary RPC sandbox positive is exercised — no fixture admits
//!      that without a hosted company and a real entitlement call — so
//!      the control is the same pure guard on a genuinely ordinary job.
//!  10. Dispatch A, operator fail/reopen/redispatch B while A can still
//!      finish: A's late completion writes its own report evidence and
//!      private result but never mutates B's task state/revision/head/
//!      dispatch pointer; B's later completion owns the task edge and
//!      its own result, and A's historical result stays bound to A.
//!  11. The read-only owner (`member_job_list`) and operator
//!      (`delivery_private_list`) projections carry
//!      `private_deliveries_state`: `available` only when every scoped
//!      native completed dispatch has its exact ledger result. A
//!      readable but stale dispatch-time ledger reports `unavailable`
//!      and withholds the stale row; after the explicit
//!      `delivery_private_reconcile` it reports `available` with the
//!      exact bytes. The answer is recomputed from durable rows/files
//!      on each read, so repeated reads are identical.
//!  12. Drift defense: if the native completed message is synthetically
//!      moved out of current task enumeration (`source`/`task_id`
//!      drift) while the sidecar stays a readable Running entry, both
//!      projections still report `unavailable` and withhold the stale
//!      row; restoring the exact native row resumes normal verified
//!      handling (the stale mismatch is again `unavailable`, and the
//!      explicit reconcile is the repair).
//!  13. A non-owner agent's `delivery_private_list` answer is identical
//!      — an empty delivery list with no status field — whether the
//!      private ledger is healthy, unreadable or malformed. No agent
//!      learns the private sidecar's state.

use super::*;

/// A live pty endpoint generation: 32 lowercase hex, the shape the pty
/// adapter mints per pane `open` and `turn_token_current` checks.
const PTY_GEN: &str = "0123456789abcdef0123456789abcdef";
/// The explicit manual repair value (`job task sha`), distinct from the
/// result's own null SHA.
const REPAIRED_SHA: &str = "1234567890abcdef1234567890abcdef12345678";
/// A second real commit, used by the ordinary control's result.
const CONTROL_SHA: &str = "fedcba0987654321fedcba0987654321fedcba09";

/// One registered explicit-reporting endpoint plus the turn token its
/// pty adapter would mint under [`PTY_GEN`].
struct PtyWorker {
    alias: String,
    token: String,
}

/// Register a real `tui-stub/pty` worker in `upstream`'s group with a
/// proven live generation — the endpoint kind whose result reports are
/// explicit (`message_report`), not adapter-owned.
fn pty_worker(fx: &Fx, alias: &str, upstream: &str) -> PtyWorker {
    let cwd = fx.dir.path().join(format!("{alias}-cwd"));
    std::fs::create_dir_all(&cwd).unwrap();
    let params = json!({"upstream": upstream}).to_string();
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias,
            provider: "tui-stub",
            endpoint_kind: "pty",
            role: "worker",
            cwd: cwd.to_str().unwrap(),
            sandbox: "workspace-write",
            instructions: None,
            params: Some(params.as_str()),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    fx.shared
        .store
        .set_identity(
            alias,
            &crate::adapter::Identity {
                thread_id: format!("thread-{alias}"),
                session_id: format!("session-{alias}"),
                model: None,
                effort: None,
                pid: 0,
                endpoint: None,
                generation: Some(PTY_GEN.to_string()),
                attach: None,
            },
        )
        .unwrap();
    PtyWorker {
        alias: alias.to_string(),
        token: format!("pty-{PTY_GEN}-{}", "c".repeat(32)),
    }
}

/// Register a real `sandbox/sandbox` endpoint in `upstream`'s group. No
/// identity/turn is minted: the dispatch must be refused before any
/// admission, so there is never a remote operation to drive.
fn sandbox_worker(fx: &Fx, alias: &str, upstream: &str) {
    let cwd = fx.dir.path().join(format!("{alias}-cwd"));
    std::fs::create_dir_all(&cwd).unwrap();
    let params = json!({"upstream": upstream}).to_string();
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias,
            provider: "sandbox",
            endpoint_kind: "sandbox",
            role: "worker",
            cwd: cwd.to_str().unwrap(),
            sandbox: "workspace-write",
            instructions: None,
            params: Some(params.as_str()),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
}

/// Claim the worker's queued kickoff the way the actor does and file the
/// result through the real daemon RPC. Idempotent for a retry of the same
/// turn: an already-running kickoff keeps its token, a terminal one is
/// refused loudly rather than silently re-reported.
fn report_result(
    fx: &Fx,
    worker: &PtyWorker,
    message: &str,
    text: &str,
    sha: Option<&str>,
) -> crate::error::Result<Value> {
    report_result_with_token(fx, worker, &worker.token, message, text, sha)
}

/// The same real RPC under an explicit turn token — a second dispatch to
/// the same endpoint mints its own token, as the pty adapter does.
fn report_result_with_token(
    fx: &Fx,
    worker: &PtyWorker,
    token: &str,
    message: &str,
    text: &str,
    sha: Option<&str>,
) -> crate::error::Result<Value> {
    let current = fx
        .shared
        .store
        .message(message)
        .unwrap()
        .expect("kickoff row disappeared");
    match current.state.as_str() {
        "queued" => {
            let crate::store::Take::Message(claimed) = fx
                .shared
                .store
                .take_queued(&worker.alias)
                .expect("kickoff claim")
            else {
                panic!("{} has no claimable kickoff", worker.alias);
            };
            assert_eq!(claimed.id, message, "claimed a different delivery");
            fx.shared.store.mark_running(message, token).unwrap();
        }
        "submitting" | "running" => {
            if current.turn_id.as_deref() != Some(token) {
                fx.shared.store.mark_running(message, token).unwrap();
            }
        }
        other => panic!("kickoff {message} is {other}, not awaiting a result"),
    }
    let mut params = json!({
        "message": message,
        "token": token,
        "kind": "result",
        "text": text,
    });
    if let Some(sha) = sha {
        params["sha"] = json!(sha);
    }
    fx.call(
        Asserted::Agent(worker.alias.clone()),
        "message_report",
        params,
    )
}

/// The operator's private projection, verbatim.
fn deliveries(fx: &Fx) -> Vec<Value> {
    fx.op("delivery_private_list", json!({}))["deliveries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The rows for one exact (job, task, dispatch) identity.
fn delivery_for(fx: &Fx, job: &str, task: &str, dispatch: &str) -> Vec<Value> {
    deliveries(fx)
        .into_iter()
        .filter(|row| {
            row["origin"]["job"] == job
                && row["origin"]["task"] == task
                && row["origin"]["dispatch"] == dispatch
        })
        .collect()
}

/// How many shared issue comments carry `marker`.
fn shared_comments(fx: &Fx, marker: &str) -> usize {
    crate::issue::board::find_issue(&fx.pm(), FIXTURE_ISSUE)
        .expect("fixture issue missing")
        .comments
        .iter()
        .filter(|comment| comment.body.contains(marker))
        .count()
}

#[test]
fn private_completion_records_the_exact_result_before_verdict_and_null_sha_survives() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-completion");
    with_deferred_service(&fx);
    let worker = pty_worker(&fx, "completion-pty", &rt.alias);
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope("sub-completion"),
            "title": "private completion acceptance",
            "request_key": "private-completion-acceptance",
            "issue": FIXTURE_ISSUE,
            "body": "complete private work",
        }),
    );
    let job = promote_member_fixture_job(&fx, "sub-completion", &request);
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .expect("promoted private job has no draft task")
        .id
        .clone();
    let dispatch = fx.op(
        "task_dispatch",
        json!({"task": task, "to": worker.alias, "by": "operator"}),
    );
    assert_eq!(
        dispatch["private_delivery"]["state"], "opened",
        "real private dispatch did not open its ledger entry: {dispatch}"
    );
    let message = dispatch["message"]
        .as_str()
        .expect("real dispatch answered no kickoff id")
        .to_string();
    let kickoff = fx
        .shared
        .store
        .message(&message)
        .unwrap()
        .expect("dispatched kickoff is not a native row");
    assert_eq!(kickoff.source, "job_dispatch");
    assert_eq!(kickoff.task_id.as_deref(), Some(task.as_str()));
    assert_eq!(kickoff.issue.as_deref(), Some(FIXTURE_ISSUE));

    let marker = "PRIVATE_COMPLETION_RESULT_2215";
    let tracker_before = fx.tree();
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();
    let ledger_before = std::fs::read(fx.dir.path().join("delivery-private.json")).unwrap();
    assert_eq!(shared_comments(&fx, marker), 0);

    // The genuine completed kickoff, reported with an EXPLICITLY null SHA:
    // the worker result text is the only body and `sha` is absent.
    let completed = report_result(&fx, &worker, &message, marker, None)
        .expect("the real daemon result path refused a genuine worker result");
    assert_eq!(completed["state"], "reported");
    assert_eq!(
        completed["private_delivery"]["state"], "recorded",
        "completion did not settle the private ledger: {completed}"
    );

    // Native completion is durable and honest.
    let done = fx
        .shared
        .store
        .message(&message)
        .unwrap()
        .expect("kickoff vanished after completion");
    assert_eq!(done.state, "completed");
    let stored = done
        .result
        .as_ref()
        .expect("completed kickoff has no result");
    assert_eq!(stored["text"], marker);
    assert!(
        stored["sha"].is_null(),
        "the daemon invented a SHA: {stored}"
    );
    let task_after = fx.shared.store.task(&task).unwrap();
    assert_eq!(task_after.state, "review");
    assert!(task_after.head_sha.is_none());

    // BEFORE any verdict/accept/reconcile: the delivery is already there,
    // exactly once, with the exact dispatch/result and no publication.
    let expected_digest = crate::delivery::private::result_digest(&message, None, marker);
    let rows = delivery_for(&fx, &job, &task, &message);
    assert_eq!(
        rows.len(),
        1,
        "completion did not project exactly one private delivery before verdict: {rows:?}"
    );
    let row = rows[0].clone();
    assert_eq!(row["state"], "completed");
    assert_eq!(row["origin"]["issue"], FIXTURE_ISSUE);
    assert_eq!(row["origin"]["project"], "fixture");
    assert_eq!(row["origin"]["owner_org"], ORG);
    assert_eq!(row["origin"]["owner_sub"], "sub-completion");
    assert_eq!(row["origin"]["revision"].as_i64(), Some(1));
    let results = row["results"]
        .as_array()
        .expect("private delivery has no results array");
    assert_eq!(results.len(), 1, "completion recorded {results:?}");
    assert_eq!(results[0]["message"], message);
    assert_eq!(results[0]["body"], marker);
    assert!(
        results[0]["sha"].is_null(),
        "completion fabricated a SHA for a null-SHA result: {results:?}"
    );
    assert_eq!(results[0]["digest"], expected_digest);
    assert!(
        row["publications"].as_array().unwrap().is_empty(),
        "completion published private work without authorization: {row}"
    );

    // The exact owner sees the same row; a non-owner agent sees nothing.
    let owner = fx.op(
        "member_job_list",
        json!({"member_envelope": fx.member_envelope("sub-completion")}),
    );
    let owner_rows: Vec<&Value> = owner["private_deliveries"]
        .as_array()
        .expect("owner projection has no private deliveries")
        .iter()
        .filter(|candidate| candidate["id"] == row["id"])
        .collect();
    assert_eq!(
        owner_rows.len(),
        1,
        "the exact owner did not see the completed delivery: {owner}"
    );
    assert_eq!(
        owner_rows[0], &row,
        "owner and operator projections disagree"
    );
    let agent_rows = fx
        .call(
            Asserted::Agent(worker.alias.clone()),
            "delivery_private_list",
            json!({}),
        )
        .expect("agent private list");
    assert_eq!(
        agent_rows["deliveries"],
        json!([]),
        "an agent observed private work: {agent_rows}"
    );

    // Completion is isolated from the shared loop.
    let shared_list = fx.op("delivery_list", json!({}));
    assert!(
        !shared_list.to_string().contains(marker)
            && !shared_list.to_string().contains(job.as_str()),
        "private completion leaked into shared delivery: {shared_list}"
    );
    assert_eq!(fx.tree(), tracker_before, "completion changed the tracker");
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "completion changed the ordinary PR loop"
    );
    assert_eq!(
        shared_comments(&fx, marker),
        0,
        "completion published to the issue"
    );
    assert_ne!(
        std::fs::read(fx.dir.path().join("delivery-private.json")).unwrap(),
        ledger_before,
        "fixture did not observe any private write"
    );

    // ---- explicit null SHA: repair, verdict, reopen/re-dispatch ----
    let repaired = fx.op(
        "task_sha",
        json!({"task": task, "sha": REPAIRED_SHA, "by": "operator"}),
    );
    assert_eq!(repaired["task"]["head_sha"], REPAIRED_SHA);
    let verdict = fx.op(
        "task_verdict",
        json!({"task": task, "sha": REPAIRED_SHA, "verdict": "pass"}),
    );
    assert_eq!(
        verdict["private_delivery"]["state"], "recorded",
        "a healthy ledger was reported unavailable after an explicit SHA repair: {verdict}"
    );
    let after_verdict = delivery_for(&fx, &job, &task, &message);
    assert_eq!(after_verdict.len(), 1);
    assert_eq!(
        after_verdict[0], row,
        "verdict rewrote the original null-SHA result or its digest: {after_verdict:?}"
    );

    fx.op("task_reopen", json!({"task": task, "by": "operator"}));
    let second = fx.op(
        "task_dispatch",
        json!({"task": task, "to": worker.alias, "by": "operator"}),
    );
    let second_message = second["message"]
        .as_str()
        .expect("re-dispatch answered no kickoff id")
        .to_string();
    assert_ne!(
        second_message, message,
        "reopen/re-dispatch reused the original dispatch identity"
    );
    assert_eq!(
        second["private_delivery"]["state"], "opened",
        "the fresh private kickoff did not open a new delivery: {second}"
    );
    let fresh = delivery_for(&fx, &job, &task, &second_message);
    assert_eq!(
        fresh.len(),
        1,
        "fresh kickoff delivery missing or duplicated"
    );
    assert_eq!(fresh[0]["state"], "running");
    assert!(
        fresh[0]["results"].as_array().unwrap().is_empty(),
        "a fresh kickoff invented a result: {fresh:?}"
    );

    // Operator reconcile over native rows only: the original result keeps
    // its null SHA and digest, the fresh running delivery gains nothing,
    // and a replay is byte-identical.
    let message_before = fx
        .shared
        .store
        .message(&message)
        .unwrap()
        .unwrap()
        .to_json();
    let second_before = fx
        .shared
        .store
        .message(&second_message)
        .unwrap()
        .unwrap()
        .to_json();
    let tracker_after = fx.tree();
    let reconciled = fx.op("delivery_private_reconcile", json!({"job": job}));
    assert!(
        reconciled["unavailable"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "reconcile reported unavailable work over a healthy ledger: {reconciled}"
    );
    let original: Vec<Value> = delivery_for(&fx, &job, &task, &message);
    assert_eq!(
        original.len(),
        1,
        "reconcile duplicated the original result"
    );
    assert_eq!(
        original[0], row,
        "reconcile changed the original null SHA/digest after repair: {original:?}"
    );
    let original_results = original[0]["results"].as_array().unwrap();
    assert_eq!(
        original_results.len(),
        1,
        "reconcile duplicated the result row"
    );
    assert!(original_results[0]["sha"].is_null());
    assert_eq!(original_results[0]["digest"], expected_digest);
    let fresh_after = delivery_for(&fx, &job, &task, &second_message);
    assert_eq!(fresh_after.len(), 1);
    assert_eq!(fresh_after[0]["state"], "running");
    assert!(fresh_after[0]["results"].as_array().unwrap().is_empty());
    let replay = fx.op("delivery_private_reconcile", json!({"job": job}));
    assert_eq!(
        replay["deliveries"], reconciled["deliveries"],
        "a replayed reconcile rewrote the ledger"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&message)
            .unwrap()
            .unwrap()
            .to_json(),
        message_before,
        "reconcile rewrote the completed kickoff"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&second_message)
            .unwrap()
            .unwrap()
            .to_json(),
        second_before,
        "reconcile rewrote the fresh kickoff"
    );
    assert_eq!(
        fx.tree(),
        tracker_after,
        "reconcile wrote shared tracker state"
    );

    // ---- shared publication requires the exact authorized selection ----
    let delivery_id = row["id"].as_str().unwrap().to_string();
    let publish = json!({
        "delivery": delivery_id,
        "issue": FIXTURE_ISSUE,
        "project": "fixture",
        "results": [message],
    });
    let ledger_before_publish = std::fs::read(fx.dir.path().join("delivery-private.json")).unwrap();
    assert!(
        fx.call(
            Asserted::Agent(worker.alias.clone()),
            "delivery_publish",
            publish.clone(),
        )
        .is_err(),
        "an agent published the private result"
    );
    for (field, value) in [
        ("public_session", json!(true)),
        (
            "member_envelope",
            json!(fx.member_envelope("sub-completion")),
        ),
        ("actor", json!("operator")),
        ("owner", json!("sub-completion")),
        ("explicit", json!(true)),
    ] {
        let mut forged = publish.clone();
        forged[field] = value;
        assert!(
            fx.op_err("delivery_publish", forged.clone()).is_err(),
            "operator transport relay forged {field}"
        );
        assert!(
            fx.call(
                Asserted::Agent(worker.alias.clone()),
                "delivery_publish",
                forged,
            )
            .is_err(),
            "agent forged {field}"
        );
    }
    for bad in [
        json!({"delivery": delivery_id, "issue": "FIX-2", "project": "fixture", "results": [message]}),
        json!({"delivery": delivery_id, "issue": FIXTURE_ISSUE, "project": "wrong-project", "results": [message]}),
        json!({"delivery": delivery_id, "issue": FIXTURE_ISSUE, "project": "fixture", "results": []}),
        json!({"delivery": delivery_id, "issue": FIXTURE_ISSUE, "project": "fixture", "results": [message, message]}),
        json!({"delivery": delivery_id, "issue": FIXTURE_ISSUE, "project": "fixture", "results": ["wrong-result"]}),
        json!({"delivery": delivery_id, "issue": FIXTURE_ISSUE, "project": "fixture", "results": [message], "digest": "wrong-digest"}),
        json!({"delivery": delivery_id, "issue": FIXTURE_ISSUE, "project": "fixture", "results": [message], "target": "pr"}),
    ] {
        assert!(
            fx.op_err("delivery_publish", bad.clone()).is_err(),
            "an unauthorized/ambiguous selection published: {bad}"
        );
    }
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery-private.json")).unwrap(),
        ledger_before_publish,
        "a refused publication changed the private ledger"
    );
    assert_eq!(
        fx.tree(),
        tracker_after,
        "a refused publication changed the tracker"
    );
    assert_eq!(shared_comments(&fx, marker), 0);
    assert!(
        delivery_for(&fx, &job, &task, &message)[0]["publications"]
            .as_array()
            .unwrap()
            .is_empty(),
        "a refused publication recorded a receipt"
    );
}

#[test]
fn private_completion_preserves_exact_result_bytes_through_reconcile() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-bytes");
    with_deferred_service(&fx);
    let worker = pty_worker(&fx, "bytes-pty", &rt.alias);
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope("sub-bytes"),
            "title": "byte-exact private completion",
            "request_key": "byte-exact-private-completion",
            "issue": FIXTURE_ISSUE,
            "body": "prove the result bytes are kept",
        }),
    );
    let job = promote_member_fixture_job(&fx, "sub-bytes", &request);
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    let dispatch = fx.op(
        "task_dispatch",
        json!({"task": task, "to": worker.alias, "by": "operator"}),
    );
    assert_eq!(dispatch["private_delivery"]["state"], "opened");
    let message = dispatch["message"].as_str().unwrap().to_string();
    let kickoff = fx.shared.store.message(&message).unwrap().unwrap();
    assert_eq!(kickoff.source, "job_dispatch");

    // Whitespace-significant result: leading/trailing spaces, an inner
    // tab and a trailing newline. The daemon must store, digest and
    // project THIS exact byte sequence, not a trimmed rendering.
    let body = "  leading bytes kept  \n\tinner tab and trailing spaces  \n";
    let human = "leading bytes kept";
    let tracker_before = fx.tree();
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();

    let completed = report_result(&fx, &worker, &message, body, None)
        .expect("the real daemon result path refused a whitespace-bearing result");
    assert_eq!(
        completed["private_delivery"]["state"], "recorded",
        "completion did not settle the private ledger: {completed}"
    );
    let done = fx.shared.store.message(&message).unwrap().unwrap();
    let stored = done
        .result
        .as_ref()
        .expect("completed kickoff has no result");
    assert_eq!(stored["text"], body, "the native result text was altered");

    let expected_digest = crate::delivery::private::result_digest(&message, None, body);
    let rows = delivery_for(&fx, &job, &task, &message);
    assert_eq!(rows.len(), 1, "byte-exact delivery missing or duplicated");
    let row = rows[0].clone();
    let results = row["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0]["body"], body,
        "the private result body was trimmed or rewritten: {results:?}"
    );
    assert!(results[0]["sha"].is_null());
    assert_eq!(
        results[0]["digest"], expected_digest,
        "the digest was not computed over the exact bytes"
    );

    // The owner's projection carries the same exact bytes.
    let owner = fx.op(
        "member_job_list",
        json!({"member_envelope": fx.member_envelope("sub-bytes")}),
    );
    let owner_rows: Vec<&Value> = owner["private_deliveries"]
        .as_array()
        .expect("owner projection missing")
        .iter()
        .filter(|candidate| candidate["id"] == row["id"])
        .collect();
    assert_eq!(owner_rows.len(), 1);
    assert_eq!(
        owner_rows[0], &row,
        "owner and operator projections disagree on the result bytes"
    );

    // A real repair + verdict must not re-derive the stored bytes.
    fx.op(
        "task_sha",
        json!({"task": task, "sha": REPAIRED_SHA, "by": "operator"}),
    );
    fx.op(
        "task_verdict",
        json!({"task": task, "sha": REPAIRED_SHA, "verdict": "pass"}),
    );
    let after_verdict = delivery_for(&fx, &job, &task, &message);
    assert_eq!(after_verdict.len(), 1);
    assert_eq!(
        after_verdict[0], row,
        "verdict rewrote the exact result body or digest"
    );

    // Operator reconciliation over native rows preserves the same bytes
    // and is replay-identical.
    let recovered = fx.op("delivery_private_reconcile", json!({"job": job}));
    assert!(
        recovered["unavailable"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "reconcile reported unavailable over a healthy ledger: {recovered}"
    );
    let reconciled = delivery_for(&fx, &job, &task, &message);
    assert_eq!(reconciled.len(), 1);
    assert_eq!(
        reconciled[0], row,
        "reconciliation altered the exact result bytes or digest"
    );
    let reconciled_results = reconciled[0]["results"].as_array().unwrap();
    assert_eq!(reconciled_results[0]["body"], body);
    assert_eq!(reconciled_results[0]["digest"], expected_digest);
    let replay = fx.op("delivery_private_reconcile", json!({"job": job}));
    assert_eq!(
        replay["deliveries"], recovered["deliveries"],
        "a replayed reconcile altered the exact result"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&message)
            .unwrap()
            .unwrap()
            .to_json()["result"]["text"],
        body,
        "reconcile rewrote the native result text"
    );

    assert_eq!(
        fx.tree(),
        tracker_before,
        "byte-exact completion wrote the tracker"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "byte-exact completion wrote the ordinary loop"
    );
    assert_eq!(
        shared_comments(&fx, human),
        0,
        "private bytes reached the issue"
    );
}

#[test]
fn unfence_completion_surfaces_unavailable_private_state_and_recovers_exactly() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-unfence");
    with_deferred_service(&fx);
    let worker = pty_worker(&fx, "unfence-pty", &rt.alias);
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope("sub-unfence"),
            "title": "unfence private completion",
            "request_key": "unfence-private-completion",
            "issue": FIXTURE_ISSUE,
            "body": "settle a fenced private kickoff",
        }),
    );
    let job = promote_member_fixture_job(&fx, "sub-unfence", &request);
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    let dispatch = fx.op(
        "task_dispatch",
        json!({"task": task, "to": worker.alias, "by": "operator"}),
    );
    assert_eq!(dispatch["private_delivery"]["state"], "opened");
    let message = dispatch["message"].as_str().unwrap().to_string();

    // A genuine running turn whose endpoint dies: the daemon's own
    // orphan sweep fences it `unknown`, exactly what `agent unfence`
    // reconciles. The agent-unfence call is the real completion path;
    // no result is invented here.
    let kickoff = fx.shared.store.message(&message).unwrap().unwrap();
    assert_eq!(kickoff.state, "queued");
    let crate::store::Take::Message(claimed) = fx
        .shared
        .store
        .take_queued(&worker.alias)
        .expect("kickoff claim")
    else {
        panic!("{} has no claimable kickoff", worker.alias);
    };
    assert_eq!(claimed.id, message);
    fx.shared
        .store
        .mark_running(&message, &worker.token)
        .unwrap();
    fx.shared
        .store
        .orphan_running(&worker.alias, "fixture endpoint died before its report")
        .unwrap();
    let fenced = fx.shared.store.message(&message).unwrap().unwrap();
    assert_eq!(fenced.state, "unknown", "fixture did not fence the kickoff");

    // Break only the fixture-owned private ledger final path.
    let ledger = fx.dir.path().join("delivery-private.json");
    let original = std::fs::read(&ledger).unwrap();
    std::fs::remove_file(&ledger).unwrap();
    std::fs::create_dir(&ledger).unwrap();
    let tracker_before = fx.tree();
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();

    // Whitespace-significant operator note and no SHA line: the reconcile
    // records an explicit null SHA and the exact note bytes as the body.
    let note = "  unfence result kept  \n\twith tab and trailing spaces  \n";
    let human = "unfence result kept";
    let unfenced = fx.op(
        "agent_unfence",
        json!({
            "alias": worker.alias,
            "status": "completed",
            "note": note,
            "resume": false,
        }),
    );
    assert!(
        unfenced["reconciled"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(message.as_str()))),
        "unfence did not reconcile the native kickoff: {unfenced}"
    );
    let private = unfenced["private_deliveries"]
        .as_array()
        .expect("agent_unfence dropped the per-message private state");
    assert_eq!(
        private.len(),
        1,
        "expected one private settlement row for the reconciled kickoff: {unfenced}"
    );
    assert_eq!(private[0]["message"], message);
    assert_eq!(
        private[0]["state"], "unavailable",
        "a broken ledger was not surfaced per message: {unfenced}"
    );
    assert!(
        private[0]["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()),
        "unavailable private state carried no reason: {unfenced}"
    );

    // The native completion itself is truthful and durable.
    let done = fx.shared.store.message(&message).unwrap().unwrap();
    assert_eq!(done.state, "completed");
    let recorded = done
        .result
        .as_ref()
        .expect("reconciled kickoff has no result");
    assert_eq!(recorded["note"], note, "the native note bytes were altered");
    assert!(recorded["sha"].is_null(), "unfence fabricated a SHA");
    let task_done = fx.shared.store.task(&task).unwrap();
    assert_eq!(task_done.state, "review");
    assert!(task_done.head_sha.is_none());

    // While the ledger is broken nothing may claim a private success.
    assert!(
        fx.op_err("delivery_private_list", json!({})).is_err(),
        "a broken private ledger was listed as available"
    );
    match fx.op_err("delivery_private_reconcile", json!({"job": job})) {
        Err(error) => assert!(
            !error.to_string().to_lowercase().contains("unknown method"),
            "reconcile entry point is not registered: {error}"
        ),
        Ok(value) => {
            assert_eq!(
                value["reconciled"].as_u64(),
                Some(0),
                "a broken ledger reported a successful reconcile: {value}"
            );
            assert!(
                value["unavailable"]
                    .as_array()
                    .is_some_and(|rows| rows.iter().any(|row| row["job"] == job)),
                "a broken reconcile did not name the unavailable identity: {value}"
            );
        }
    }
    assert_eq!(
        fx.tree(),
        tracker_before,
        "a sidecar failure wrote the tracker"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "a sidecar failure changed the ordinary loop"
    );

    // Restoring the exact original ledger evidence makes the same native
    // completion recoverable — one exact result, no replay.
    std::fs::remove_dir(&ledger).unwrap();
    std::fs::write(&ledger, &original).unwrap();
    let message_before = done.to_json();
    let task_before = task_done.to_json();
    let tracker_after = fx.tree();
    let recovered = fx.op("delivery_private_reconcile", json!({"job": job}));
    assert!(
        recovered["unavailable"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "restored ledger still reported unavailable work: {recovered}"
    );
    let rows = delivery_for(&fx, &job, &task, &message);
    assert_eq!(rows.len(), 1, "recovery duplicated or missed the delivery");
    let row = &rows[0];
    assert_eq!(row["state"], "completed");
    let results = row["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "recovery duplicated the result: {row}");
    assert_eq!(results[0]["message"], message);
    assert_eq!(
        results[0]["body"], note,
        "recovery trimmed the operator-reconciled note: {results:?}"
    );
    assert!(results[0]["sha"].is_null());
    assert_eq!(
        results[0]["digest"],
        crate::delivery::private::result_digest(&message, None, note)
    );
    assert!(row["publications"].as_array().unwrap().is_empty());
    let replay = fx.op("delivery_private_reconcile", json!({"job": job}));
    assert_eq!(
        replay["deliveries"], recovered["deliveries"],
        "a replayed recovery altered the exact unfence result"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&message)
            .unwrap()
            .unwrap()
            .to_json(),
        message_before,
        "recovery rewrote the native reconciled kickoff"
    );
    assert_eq!(
        fx.shared.store.task(&task).unwrap().to_json(),
        task_before,
        "recovery reran or rewrote the reconciled task"
    );
    assert_eq!(
        fx.tree(),
        tracker_after,
        "recovery wrote shared tracker state"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "recovery changed the ordinary loop"
    );
    assert_eq!(
        shared_comments(&fx, human),
        0,
        "recovery published private work"
    );
}

#[test]
fn private_or_ambiguous_job_to_sandbox_is_refused_before_any_effect() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-sandbox");
    with_deferred_service(&fx);
    sandbox_worker(&fx, "sandbox-private", &rt.alias);
    sandbox_worker(&fx, "sandbox-ambiguous", "ordinary-worker");
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope("sub-sandbox"),
            "title": "sandbox refusal acceptance",
            "request_key": "sandbox-refusal-acceptance",
            "issue": FIXTURE_ISSUE,
            "body": "never dispatch private work to a cloud sandbox",
        }),
    );
    let job = promote_member_fixture_job(&fx, "sub-sandbox", &request);
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    let db = rusqlite::Connection::open(fx.dir.path().join("cadence.sqlite3")).unwrap();
    let dispatch_events = |task: &str| -> i64 {
        db.query_row(
            "SELECT COUNT(*) FROM events WHERE task_id=?1 AND kind='task_dispatched'",
            [task],
            |row| row.get(0),
        )
        .unwrap()
    };
    // The external sandbox admission is a hosted-company + HTTPS
    // entitlement probe. The refusal must precede it: the fixture has no
    // hosted company, so a post-probe refusal would carry this code.
    let before_remote_admission = |error: &str| {
        let lower = error.to_lowercase();
        assert!(
            !lower.contains("hosted agenticos context")
                && !lower.contains("cloud_sandbox_not_hosted")
                && !lower.contains("entitle"),
            "refused only after the external sandbox admission probe: {error}"
        );
    };

    let tracker_before = fx.tree();
    let private_before = fx.shared.store.task(&task).unwrap().to_json();
    let refused = fx.op_err(
        "task_dispatch",
        json!({"task": task, "to": "sandbox-private", "by": "operator"}),
    );
    let error = refused.expect_err("a member-private job was dispatched to a sandbox endpoint");
    assert_eq!(
        error.code(),
        Some(crate::issue::cloud::PRIVATE_JOB),
        "private-job sandbox refusal used the wrong code: {error}"
    );
    let text = error.to_string();
    assert!(
        text.contains("cloud sandbox dispatch is unsupported"),
        "refusal did not name the sandbox lane: {text}"
    );
    assert!(
        text.contains(job.as_str()),
        "refusal did not name the job: {text}"
    );
    assert!(
        text.contains("member-private"),
        "refusal did not name the private provenance: {text}"
    );
    before_remote_admission(&text);
    assert_eq!(
        fx.shared.store.task(&task).unwrap().to_json(),
        private_before,
        "sandbox refusal mutated the private task revision/state/head/dispatch"
    );
    assert!(
        fx.shared
            .store
            .task_dispatch_messages(&task)
            .unwrap()
            .is_empty(),
        "sandbox refusal enqueued a native kickoff"
    );
    assert_eq!(
        dispatch_events(&task),
        0,
        "sandbox refusal wrote a dispatch event"
    );
    assert_eq!(
        fx.tree(),
        tracker_before,
        "sandbox refusal mutated the issue"
    );
    assert!(
        deliveries(&fx)
            .iter()
            .all(|row| row["origin"]["job"] != job),
        "sandbox refusal created a private delivery"
    );

    // Ambiguous provenance: a REAL accepted queue row whose planned job
    // has no ledger bind yet (the pre-promotion crash window). The id
    // prefix alone is not authority, so the guard must refuse it before
    // the remote probe and before any dispatch write.
    let queued = fx
        .shared
        .store
        .assist_request_enqueue(
            ORG,
            "sub-sandbox",
            "sandbox-queue-only",
            FIXTURE_ISSUE,
            "Queue-only sandbox job",
            "body",
            "",
            "cred-sub-sandbox",
        )
        .unwrap();
    let ambiguous_job = queued.planned_job_id.clone();
    let ambiguous_task = format!("{ambiguous_job}-t1");
    db.execute(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,repo,state,created,updated) \
         VALUES(?1,'Ambiguous queue job','spec.md','ordinary-worker',?2,NULL,'open',1,1)",
        rusqlite::params![ambiguous_job.as_str(), FIXTURE_ISSUE],
    )
    .unwrap();
    db.execute(
        "INSERT INTO tasks(id,job_id,title,assignee,state,created,updated) \
         VALUES(?1,?2,'Ambiguous task',NULL,'draft',1,1)",
        rusqlite::params![ambiguous_task.as_str(), ambiguous_job.as_str()],
    )
    .unwrap();
    assert!(
        fx.shared
            .store
            .assist_request_for_job(&ambiguous_job)
            .unwrap()
            .is_none(),
        "fixture created a ledger bind, not a queue-only crash window"
    );
    assert!(
        fx.shared
            .store
            .assist_job_has_request(&ambiguous_job)
            .unwrap(),
        "the queue-only provenance is invisible to the private-job predicate"
    );
    let ambiguous_before = fx.shared.store.task(&ambiguous_task).unwrap().to_json();
    let refused = fx.op_err(
        "task_dispatch",
        json!({"task": ambiguous_task, "to": "sandbox-ambiguous", "by": "operator"}),
    );
    let error =
        refused.expect_err("an ambiguous queue-only job was dispatched to a sandbox endpoint");
    assert_eq!(
        error.code(),
        Some(crate::issue::cloud::PRIVATE_JOB),
        "ambiguous sandbox refusal used the wrong code: {error}"
    );
    let text = error.to_string();
    assert!(
        text.contains("cloud sandbox dispatch is unsupported"),
        "refusal did not name the sandbox lane: {text}"
    );
    assert!(
        text.contains(ambiguous_job.as_str()),
        "refusal did not name the ambiguous job: {text}"
    );
    before_remote_admission(&text);
    assert_eq!(
        fx.shared.store.task(&ambiguous_task).unwrap().to_json(),
        ambiguous_before,
        "sandbox refusal mutated the ambiguous task"
    );
    assert!(
        fx.shared
            .store
            .task_dispatch_messages(&ambiguous_task)
            .unwrap()
            .is_empty(),
        "sandbox refusal enqueued an ambiguous kickoff"
    );
    assert_eq!(dispatch_events(&ambiguous_task), 0);
    assert_eq!(
        fx.tree(),
        tracker_before,
        "sandbox refusal mutated the issue"
    );

    // Recovery-side guard: a pre-fix sandbox kickoff that is ALREADY
    // dispatched must be refused before the runner claims it. The native
    // kickoff row is real (enqueued then bound to the ambiguous task);
    // the guard settles it failed with the same code and no remote work.
    let legacy_kickoff = "legacy-private-sandbox-kickoff";
    fx.shared
        .store
        .enqueue(
            "sandbox-ambiguous",
            "legacy kickoff",
            None,
            legacy_kickoff,
            "job_dispatch",
        )
        .unwrap();
    db.execute(
        "UPDATE messages SET task_id=?1, issue=?2 WHERE id=?3",
        rusqlite::params![ambiguous_task.as_str(), FIXTURE_ISSUE, legacy_kickoff],
    )
    .unwrap();
    let legacy = fx.shared.store.message(legacy_kickoff).unwrap().unwrap();
    let error = crate::issue::cloud::refuse_dispatched_kickoff(&fx.shared.store, &legacy)
        .expect_err("a previously dispatched private sandbox kickoff was claimed");
    assert_eq!(
        error.code(),
        Some(crate::issue::cloud::PRIVATE_JOB),
        "recovery refusal used the wrong code: {error}"
    );
    let settled = fx.shared.store.message(legacy_kickoff).unwrap().unwrap();
    assert_eq!(
        settled.state, "failed",
        "an unclaimed private sandbox kickoff was not settled failed: {}",
        settled.state
    );
    assert!(
        settled.error.as_deref().is_some_and(|error| {
            error.contains("unsupported") || error.contains("member-private")
        }),
        "recovery refusal left no truthful error: {:?}",
        settled.error
    );
    assert_eq!(
        fx.shared.store.task(&ambiguous_task).unwrap().to_json(),
        ambiguous_before,
        "recovery refusal mutated the task"
    );
    assert_eq!(
        fx.tree(),
        tracker_before,
        "recovery refusal mutated the issue"
    );

    // Positive control at the guard's own boundary. An ordinary job has no
    // ledger bind, no queue row and no assistant binding for its PM or
    // worker, so the same pure guard admits it. This does NOT exercise the
    // ordinary RPC: no fixture can admit that without a hosted company and
    // a real HTTPS entitlement call.
    let ordinary_job = fx.shared.store.job(&rt.foreign_job).unwrap();
    assert!(
        !fx.shared
            .store
            .assist_job_has_request(&rt.foreign_job)
            .unwrap(),
        "the control job is not ordinary"
    );
    assert!(
        crate::issue::cloud::refuse_private_job(
            &fx.shared.store,
            &ordinary_job,
            "sandbox-private",
        )
        .is_ok(),
        "the sandbox private-job guard refused a genuinely ordinary job"
    );
}

#[test]
fn late_completion_of_a_failed_dispatch_never_mutates_redispatched_b() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-late-a");
    with_deferred_service(&fx);
    let worker = pty_worker(&fx, "late-pty", &rt.alias);
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope("sub-late-a"),
            "title": "late completion acceptance",
            "request_key": "late-completion-acceptance",
            "issue": FIXTURE_ISSUE,
            "body": "A may finish after B starts",
        }),
    );
    let job = promote_member_fixture_job(&fx, "sub-late-a", &request);
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    let sha_a = "1111111111111111111111111111111111111111";
    let sha_b = "2222222222222222222222222222222222222222";
    let body_a = "A late result 2215";
    let body_b = "B current result 2215";

    // Dispatch A and hold its turn running — A may finish later.
    let dispatch_a = fx.op(
        "task_dispatch",
        json!({"task": task, "to": worker.alias, "by": "operator"}),
    );
    assert_eq!(dispatch_a["private_delivery"]["state"], "opened");
    let message_a = dispatch_a["message"].as_str().unwrap().to_string();
    let crate::store::Take::Message(held_a) = fx
        .shared
        .store
        .take_queued(&worker.alias)
        .expect("A kickoff claim")
    else {
        panic!("{} has no claimable A kickoff", worker.alias);
    };
    assert_eq!(held_a.id, message_a);
    fx.shared
        .store
        .mark_running(&message_a, &worker.token)
        .unwrap();

    // Operator replaces A: fail, reopen, dispatch B. A's native message
    // stays running; its ledger row is truthfully settled `failed`.
    fx.op(
        "task_fail",
        json!({"task": task, "reason": "operator replaced the worker", "by": "operator"}),
    );
    let a_failed = delivery_for(&fx, &job, &task, &message_a);
    assert_eq!(a_failed.len(), 1, "A's delivery vanished on fail");
    assert_eq!(a_failed[0]["state"], "failed");
    fx.op("task_reopen", json!({"task": task, "by": "operator"}));
    let dispatch_b = fx.op(
        "task_dispatch",
        json!({"task": task, "to": worker.alias, "by": "operator"}),
    );
    let message_b = dispatch_b["message"]
        .as_str()
        .expect("re-dispatch answered no kickoff id")
        .to_string();
    assert_ne!(message_b, message_a);
    assert_eq!(dispatch_b["private_delivery"]["state"], "opened");
    let task_b = fx.shared.store.task(&task).unwrap();
    assert_eq!(task_b.state, "dispatched");
    assert_eq!(task_b.revision, 1);
    assert_eq!(task_b.dispatch_message.as_deref(), Some(message_b.as_str()));
    assert!(task_b.head_sha.is_none());
    let task_b_before = task_b.to_json();
    let b_message_before = fx
        .shared
        .store
        .message(&message_b)
        .unwrap()
        .unwrap()
        .to_json();
    let tracker_before = fx.tree();
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();

    // A's late real completion: native result is durable, the private
    // result binds to A's own dispatch, and B's task is untouched.
    let a_done = report_result(&fx, &worker, &message_a, body_a, Some(sha_a))
        .expect("A's late result was refused");
    assert_eq!(
        a_done["private_delivery"]["state"], "recorded",
        "A's historical result did not bind to A: {a_done}"
    );
    assert_eq!(
        fx.shared.store.task(&task).unwrap().to_json(),
        task_b_before,
        "A's late completion mutated B's task state/revision/head/dispatch"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&message_b)
            .unwrap()
            .unwrap()
            .to_json(),
        b_message_before,
        "A's late completion mutated B's kickoff"
    );
    let a_rows = delivery_for(&fx, &job, &task, &message_a);
    assert_eq!(a_rows.len(), 1);
    assert_eq!(a_rows[0]["state"], "completed");
    assert_eq!(a_rows[0]["origin"]["dispatch"], message_a);
    let a_result = &a_rows[0]["results"].as_array().unwrap()[0];
    assert_eq!(a_result["message"], message_a);
    assert_eq!(a_result["body"], body_a);
    assert_eq!(a_result["sha"], sha_a);
    assert_eq!(
        a_result["digest"],
        crate::delivery::private::result_digest(&message_a, Some(sha_a), body_a)
    );
    let b_rows = delivery_for(&fx, &job, &task, &message_b);
    assert_eq!(b_rows.len(), 1);
    assert_eq!(b_rows[0]["state"], "running");
    assert!(b_rows[0]["results"].as_array().unwrap().is_empty());

    // B's later real completion moves the task and records B — A's
    // historical result must remain exactly as recorded.
    let token_b = format!("pty-{PTY_GEN}-{}", "d".repeat(32));
    let b_done = report_result_with_token(&fx, &worker, &token_b, &message_b, body_b, Some(sha_b))
        .expect("B's real completion was refused");
    assert_eq!(b_done["private_delivery"]["state"], "recorded");
    let task_after = fx.shared.store.task(&task).unwrap();
    assert_eq!(task_after.state, "review");
    assert_eq!(task_after.revision, 1);
    assert_eq!(task_after.head_sha.as_deref(), Some(sha_b));
    let b_after = delivery_for(&fx, &job, &task, &message_b);
    assert_eq!(b_after.len(), 1);
    assert_eq!(b_after[0]["state"], "completed");
    let b_result = &b_after[0]["results"].as_array().unwrap()[0];
    assert_eq!(b_result["message"], message_b);
    assert_eq!(b_result["body"], body_b);
    assert_eq!(b_result["sha"], sha_b);
    assert_eq!(
        b_result["digest"],
        crate::delivery::private::result_digest(&message_b, Some(sha_b), body_b)
    );
    let a_after_b = delivery_for(&fx, &job, &task, &message_a);
    assert_eq!(
        a_after_b[0], a_rows[0],
        "B's completion rewrote A's historical result"
    );

    // Each dispatch owns exactly one report event with its own SHA.
    let db = rusqlite::Connection::open(fx.dir.path().join("cadence.sqlite3")).unwrap();
    let reports = |message: &str| -> Vec<Value> {
        let mut stmt = db
            .prepare(
                "SELECT payload FROM events WHERE task_id=?1 AND kind='task_reported' ORDER BY seq",
            )
            .unwrap();
        stmt.query_map([task.as_str()], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|payload| serde_json::from_str::<Value>(&payload.unwrap()).unwrap())
            .filter(|event| event["message"].as_str() == Some(message))
            .collect()
    };
    for (message, sha) in [(message_a.as_str(), sha_a), (message_b.as_str(), sha_b)] {
        let rows = reports(message);
        assert_eq!(
            rows.len(),
            1,
            "dispatch {message} has {} report events",
            rows.len()
        );
        assert_eq!(rows[0]["sha"], sha);
        assert_eq!(rows[0]["revision"].as_i64(), Some(1));
    }

    let reconciled = fx.op("delivery_private_reconcile", json!({"job": job}));
    assert!(
        reconciled["unavailable"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "reconcile reported unavailable work: {reconciled}"
    );
    let a_final = delivery_for(&fx, &job, &task, &message_a);
    let b_final = delivery_for(&fx, &job, &task, &message_b);
    assert_eq!(a_final.len(), 1);
    assert_eq!(b_final.len(), 1);
    assert_eq!(
        a_final[0], a_rows[0],
        "reconcile changed A's historical result"
    );
    assert_eq!(b_final[0], b_after[0], "reconcile changed B's result");
    let replay = fx.op("delivery_private_reconcile", json!({"job": job}));
    assert_eq!(replay["deliveries"], reconciled["deliveries"]);
    assert_eq!(
        fx.tree(),
        tracker_before,
        "late completion wrote the tracker"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "late completion wrote the ordinary loop"
    );
}

#[test]
fn stale_readable_private_ledger_reports_unavailable_until_exact_reconcile() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-consistency");
    with_deferred_service(&fx);
    let worker = pty_worker(&fx, "consistency-pty", &rt.alias);
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope("sub-consistency"),
            "title": "consistency projection acceptance",
            "request_key": "consistency-projection-acceptance",
            "issue": FIXTURE_ISSUE,
            "body": "the sidecar may lag a durable completion",
        }),
    );
    let job = promote_member_fixture_job(&fx, "sub-consistency", &request);
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    let dispatch = fx.op(
        "task_dispatch",
        json!({"task": task, "to": worker.alias, "by": "operator"}),
    );
    assert_eq!(dispatch["private_delivery"]["state"], "opened");
    let message = dispatch["message"].as_str().unwrap().to_string();
    let ledger = fx.dir.path().join("delivery-private.json");
    // The exact dispatch-time bytes are what a crash before settlement
    // leaves readable: a Running entry with zero results.
    let dispatch_time_ledger = std::fs::read(&ledger).unwrap();
    let tracker_before = fx.tree();
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();

    let marker = "CONSISTENCY_RESULT_2215";
    let sha = "3333333333333333333333333333333333333333";
    let completed = report_result(&fx, &worker, &message, marker, Some(sha))
        .expect("genuine completion refused");
    assert_eq!(completed["private_delivery"]["state"], "recorded");
    let expected_digest = crate::delivery::private::result_digest(&message, Some(sha), marker);

    // Healthy native completion + exact ledger result: both read-only
    // projections report available, and expose the exact result.
    let owner = fx.op(
        "member_job_list",
        json!({"member_envelope": fx.member_envelope("sub-consistency")}),
    );
    assert_eq!(
        owner["private_deliveries_state"], "available",
        "a consistent completion was reported unavailable: {owner}"
    );
    let operator = fx.op("delivery_private_list", json!({}));
    assert_eq!(
        operator["private_deliveries_state"], "available",
        "operator view hid a consistent completion: {operator}"
    );
    let rows = delivery_for(&fx, &job, &task, &message);
    assert_eq!(rows.len(), 1);
    let delivery_id = rows[0]["id"].clone();
    assert_eq!(rows[0]["results"][0]["body"], marker);
    assert_eq!(rows[0]["results"][0]["sha"], sha);
    assert_eq!(rows[0]["results"][0]["digest"], expected_digest);

    // Synthetic crash window: the native completion is durable, but the
    // sidecar is the exact pre-completion, readable Running entry. No
    // store write follows. The projection must say unavailable — it must
    // not present the stale entry as running/available success.
    std::fs::write(&ledger, &dispatch_time_ledger).unwrap();
    let owner_stale = fx.op(
        "member_job_list",
        json!({"member_envelope": fx.member_envelope("sub-consistency")}),
    );
    assert_eq!(
        owner_stale["private_deliveries_state"], "unavailable",
        "owner view accepted a stale readable ledger as success: {owner_stale}"
    );
    let operator_stale = fx.op("delivery_private_list", json!({}));
    assert_eq!(
        operator_stale["private_deliveries_state"], "unavailable",
        "operator view accepted a stale readable ledger as success: {operator_stale}"
    );
    for view in [&owner_stale, &operator_stale] {
        assert!(
            !view["private_deliveries"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["id"] == delivery_id)),
            "a stale readable entry for the completed dispatch was not withheld: {view}"
        );
        assert!(
            !view.to_string().contains(marker),
            "a stale projection fabricated the private result: {view}"
        );
    }

    // Durable rows, not memory: a second fresh read answers the same.
    assert_eq!(
        fx.op(
            "member_job_list",
            json!({"member_envelope": fx.member_envelope("sub-consistency")}),
        ),
        owner_stale,
        "a repeated owner read changed the stale projection"
    );
    assert_eq!(
        fx.op("delivery_private_list", json!({})),
        operator_stale,
        "a repeated operator read changed the stale projection"
    );

    // Corruption/drift defense: the native completed row is synthetically
    // moved out of current dispatch enumeration (its source no longer
    // reads `job_dispatch`) while the sidecar stays a readable Running
    // entry. The projection must still report unavailable and withhold
    // the stale row rather than treating it as normal running work.
    let native_before_drift = fx
        .shared
        .store
        .message(&message)
        .unwrap()
        .unwrap()
        .to_json();
    assert_eq!(native_before_drift["source"], "job_dispatch");
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE messages SET source='drift' WHERE id=?1",
                rusqlite::params![message.as_str()],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(
        fx.shared
            .store
            .task_dispatch_messages(&task)
            .unwrap()
            .iter()
            .all(|row| row.id != message),
        "the drifted native row was still enumerable as a dispatch"
    );
    let owner_drift = fx.op(
        "member_job_list",
        json!({"member_envelope": fx.member_envelope("sub-consistency")}),
    );
    assert_eq!(
        owner_drift["private_deliveries_state"], "unavailable",
        "owner view accepted a drifted native row as normal work: {owner_drift}"
    );
    let operator_drift = fx.op("delivery_private_list", json!({}));
    assert_eq!(
        operator_drift["private_deliveries_state"], "unavailable",
        "operator view accepted a drifted native row as normal work: {operator_drift}"
    );
    for view in [&owner_drift, &operator_drift] {
        assert!(
            !view["private_deliveries"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["id"] == delivery_id)),
            "a drifted native row did not suppress the stale Running entry: {view}"
        );
        assert!(
            !view.to_string().contains(marker),
            "a drifted native row fabricated the private result: {view}"
        );
    }

    // Restoring the exact native row resumes normal verified handling:
    // the stale ledger is again reported unavailable and withheld, and
    // the explicit reconcile is the only repair.
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE messages SET source='job_dispatch' WHERE id=?1",
                rusqlite::params![message.as_str()],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        fx.shared
            .store
            .message(&message)
            .unwrap()
            .unwrap()
            .to_json(),
        native_before_drift,
        "the restore did not return the exact native completed row"
    );
    let owner_restored = fx.op(
        "member_job_list",
        json!({"member_envelope": fx.member_envelope("sub-consistency")}),
    );
    assert_eq!(
        owner_restored["private_deliveries_state"], "unavailable",
        "normal stale handling did not resume after the native restore: {owner_restored}"
    );
    let operator_restored = fx.op("delivery_private_list", json!({}));
    assert_eq!(
        operator_restored["private_deliveries_state"], "unavailable",
        "normal stale handling did not resume after the native restore: {operator_restored}"
    );
    for view in [&owner_restored, &operator_restored] {
        assert!(
            !view["private_deliveries"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["id"] == delivery_id)),
            "the restored stale entry was not withheld: {view}"
        );
    }

    // The explicit operator reconcile appends the exact durable result;
    // both projections then report available with the exact bytes.
    let recovered = fx.op("delivery_private_reconcile", json!({"job": job}));
    assert!(
        recovered["unavailable"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "reconcile reported unavailable over a readable ledger: {recovered}"
    );
    let owner_ok = fx.op(
        "member_job_list",
        json!({"member_envelope": fx.member_envelope("sub-consistency")}),
    );
    assert_eq!(
        owner_ok["private_deliveries_state"], "available",
        "owner view did not recover after the exact reconcile: {owner_ok}"
    );
    let operator_ok = fx.op("delivery_private_list", json!({}));
    assert_eq!(
        operator_ok["private_deliveries_state"], "available",
        "operator view did not recover after the exact reconcile: {operator_ok}"
    );
    let recovered_rows = delivery_for(&fx, &job, &task, &message);
    assert_eq!(recovered_rows.len(), 1);
    assert_eq!(recovered_rows[0]["state"], "completed");
    let result = &recovered_rows[0]["results"].as_array().unwrap()[0];
    assert_eq!(result["message"], message);
    assert_eq!(result["body"], marker);
    assert_eq!(result["sha"], sha);
    assert_eq!(result["digest"], expected_digest);
    assert_eq!(
        fx.op("delivery_private_list", json!({})),
        operator_ok,
        "a repeated available read changed"
    );
    assert_eq!(
        fx.tree(),
        tracker_before,
        "consistency projection wrote the tracker"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "consistency projection wrote the ordinary loop"
    );
    assert_eq!(
        shared_comments(&fx, marker),
        0,
        "private result reached the issue"
    );
}

#[test]
fn agent_private_list_never_leaks_private_sidecar_state() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-agent-leak");
    with_deferred_service(&fx);
    let worker = pty_worker(&fx, "leak-pty", &rt.alias);
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope("sub-agent-leak"),
            "title": "agent leak acceptance",
            "request_key": "agent-leak-acceptance",
            "issue": FIXTURE_ISSUE,
            "body": "an agent must not learn the sidecar state",
        }),
    );
    let job = promote_member_fixture_job(&fx, "sub-agent-leak", &request);
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    let dispatch = fx.op(
        "task_dispatch",
        json!({"task": task, "to": worker.alias, "by": "operator"}),
    );
    assert_eq!(dispatch["private_delivery"]["state"], "opened");
    let tracker_before = fx.tree();
    let agent = || Asserted::Agent(worker.alias.clone());

    // A healthy readable ledger already answers the non-owner with an
    // empty list — no rows, no counts, no ids.
    let healthy = fx
        .call(agent(), "delivery_private_list", json!({}))
        .expect("an agent private list must remain readable");
    assert_eq!(healthy, json!({"deliveries": []}));

    // Unreadable and malformed sidecars must be INDISTINGUISHABLE from
    // healthy: identical empty success, never an error or a status field.
    let ledger = fx.dir.path().join("delivery-private.json");
    let original = std::fs::read(&ledger).unwrap();
    std::fs::remove_file(&ledger).unwrap();
    std::fs::create_dir(&ledger).unwrap();
    let unreadable = fx
        .call(agent(), "delivery_private_list", json!({}))
        .expect("an unreadable private ledger must not change an agent's empty answer");
    assert_eq!(
        unreadable, healthy,
        "an agent learned the private sidecar is unreadable: {unreadable}"
    );
    std::fs::remove_dir(&ledger).unwrap();
    std::fs::write(&ledger, b"not-json").unwrap();
    let malformed = fx
        .call(agent(), "delivery_private_list", json!({}))
        .expect("a malformed private ledger must not change an agent's empty answer");
    assert_eq!(
        malformed, healthy,
        "an agent learned the private sidecar is malformed: {malformed}"
    );
    std::fs::write(&ledger, &original).unwrap();
    let restored = fx
        .call(agent(), "delivery_private_list", json!({}))
        .expect("the restored ledger must not change an agent's empty answer");
    assert_eq!(restored, healthy, "the restored answer changed");
    assert_eq!(fx.tree(), tracker_before, "agent reads wrote the tracker");
}

#[test]
fn ordinary_completion_is_never_private_and_private_state_stays_isolated() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-ordinary-control");
    with_deferred_service(&fx);
    let private_worker = pty_worker(&fx, "isolation-private-pty", &rt.alias);
    let control_worker = pty_worker(&fx, "isolation-ordinary-pty", "ordinary-worker");

    // A genuine private completion first, then the ordinary control.
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope("sub-ordinary-control"),
            "title": "isolation private work",
            "request_key": "isolation-private-work",
            "issue": FIXTURE_ISSUE,
            "body": "private work",
        }),
    );
    let job = promote_member_fixture_job(&fx, "sub-ordinary-control", &request);
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    let dispatch = fx.op(
        "task_dispatch",
        json!({"task": task, "to": private_worker.alias, "by": "operator"}),
    );
    let message = dispatch["message"].as_str().unwrap().to_string();
    let private_marker = "ISOLATED_PRIVATE_RESULT_2215";
    let completed = report_result(
        &fx,
        &private_worker,
        &message,
        private_marker,
        Some(CONTROL_SHA),
    )
    .expect("private completion refused");
    assert_eq!(completed["private_delivery"]["state"], "recorded");

    let ledger_after_private = std::fs::read(fx.dir.path().join("delivery-private.json")).unwrap();
    let ordinary_after_private = std::fs::read(fx.dir.path().join("delivery.json")).ok();
    let tracker_after_private = fx.tree();

    // The control: a real draft task in a NON-assist job, dispatched and
    // completed through the same daemon result path.
    let ordinary = fx.op(
        "task_dispatch",
        json!({"task": rt.foreign_task, "to": control_worker.alias, "by": "operator"}),
    );
    assert_eq!(
        ordinary["private_delivery"]["state"], "not_private",
        "ordinary dispatch was recorded as private: {ordinary}"
    );
    let ordinary_message = ordinary["message"].as_str().unwrap().to_string();
    let control_marker = "ORDINARY_CONTROL_RESULT_2215";
    let control = report_result(
        &fx,
        &control_worker,
        &ordinary_message,
        control_marker,
        Some(CONTROL_SHA),
    )
    .expect("ordinary completion refused");
    assert_eq!(
        control["private_delivery"]["state"], "not_private",
        "ordinary completion was recorded as private: {control}"
    );
    let control_done = fx.shared.store.message(&ordinary_message).unwrap().unwrap();
    assert_eq!(
        control_done.state, "completed",
        "ordinary control did not complete"
    );

    // Nothing ordinary touched the private ledger, and nothing private
    // touched the ordinary loop or the tracker.
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery-private.json")).unwrap(),
        ledger_after_private,
        "ordinary completion wrote the private ledger"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_after_private,
        "private completion wrote the ordinary loop"
    );
    assert_eq!(
        fx.tree(),
        tracker_after_private,
        "completion moved the tracker"
    );

    let rows = deliveries(&fx);
    assert_eq!(
        rows.len(),
        1,
        "ordinary work entered the private projection: {rows:?}"
    );
    assert_eq!(rows[0]["origin"]["job"], job);
    assert!(
        !format!("{rows:?}").contains(control_marker),
        "an ordinary result appeared in the private projection"
    );
    let reconcile = fx.op("delivery_private_reconcile", json!({"job": rt.foreign_job}));
    assert!(
        reconcile["deliveries"].as_array().unwrap().is_empty(),
        "reconcile invented a private delivery for ordinary work: {reconcile}"
    );
    assert!(!reconcile.to_string().contains(control_marker));
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery-private.json")).unwrap(),
        ledger_after_private,
        "ordinary reconcile changed the private ledger"
    );

    // The owner projection is equally exact.
    let owner = fx.op(
        "member_job_list",
        json!({"member_envelope": fx.member_envelope("sub-ordinary-control")}),
    );
    let owner_rows = owner["private_deliveries"].as_array().unwrap();
    assert_eq!(owner_rows.len(), 1);
    assert_eq!(owner_rows[0]["origin"]["job"], job);
    assert!(!owner.to_string().contains(control_marker));
}

#[test]
fn unavailable_private_storage_never_claims_success_and_exact_recovery_works() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-sidecar");
    with_deferred_service(&fx);
    let worker = pty_worker(&fx, "sidecar-pty", &rt.alias);
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope("sub-sidecar"),
            "title": "sidecar failure acceptance",
            "request_key": "sidecar-failure-acceptance",
            "issue": FIXTURE_ISSUE,
            "body": "private work under a broken sidecar",
        }),
    );
    let job = promote_member_fixture_job(&fx, "sub-sidecar", &request);
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    let dispatch = fx.op(
        "task_dispatch",
        json!({"task": task, "to": worker.alias, "by": "operator"}),
    );
    assert_eq!(dispatch["private_delivery"]["state"], "opened");
    let message = dispatch["message"].as_str().unwrap().to_string();

    // The ledger was genuinely written at dispatch; keep its exact bytes
    // and make only the fixture-owned final path unwritable (a directory,
    // not a global permission/umask mutation).
    let ledger = fx.dir.path().join("delivery-private.json");
    let original = std::fs::read(&ledger).unwrap();
    std::fs::remove_file(&ledger).unwrap();
    std::fs::create_dir(&ledger).unwrap();
    let tracker_before = fx.tree();
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();

    let marker = "SIDECAR_FAILURE_RESULT_2215";
    let outcome = report_result(&fx, &worker, &message, marker, Some(REPAIRED_SHA));
    let after = fx
        .shared
        .store
        .message(&message)
        .unwrap()
        .expect("kickoff vanished during a sidecar failure");
    match &outcome {
        Ok(value) => {
            assert_eq!(
                value["private_delivery"]["state"], "unavailable",
                "unavailable private storage claimed success: {value}"
            );
            assert_eq!(
                after.state, "completed",
                "a committed completion must be honest about the native result"
            );
            let stored = after
                .result
                .as_ref()
                .expect("completed kickoff has no result");
            assert_eq!(stored["text"], marker);
            assert_eq!(stored["sha"], REPAIRED_SHA);
        }
        Err(error) => {
            let text = error.to_string().to_lowercase();
            assert!(
                !text.contains("recorded") && !text.contains("success"),
                "an unavailable ledger was reported as success: {error}"
            );
            match after.state.as_str() {
                "completed" => {
                    let stored = after
                        .result
                        .as_ref()
                        .expect("completed kickoff has no result");
                    assert_eq!(stored["text"], marker);
                    assert_eq!(stored["sha"], REPAIRED_SHA);
                }
                "running" => assert!(
                    after.result.is_none(),
                    "a failed completion invented a native result"
                ),
                other => panic!("a failed completion reached unexpected state {other}"),
            }
        }
    }

    // While the ledger is broken nothing may be reported as recorded.
    assert!(
        fx.op_err("delivery_private_list", json!({})).is_err(),
        "a broken private ledger was listed as available"
    );
    assert!(
        fx.op_err("delivery_private_list", json!({"issue": FIXTURE_ISSUE}))
            .is_err(),
        "a broken private ledger answered a scoped list"
    );
    match fx.op_err("delivery_private_reconcile", json!({"job": job})) {
        Err(error) => assert!(
            !error.to_string().to_lowercase().contains("unknown method"),
            "reconcile entry point is not registered: {error}"
        ),
        Ok(value) => {
            assert_eq!(
                value["reconciled"].as_u64(),
                Some(0),
                "a broken ledger reported a successful reconcile: {value}"
            );
            assert!(
                value["unavailable"]
                    .as_array()
                    .is_some_and(|rows| rows.iter().any(|row| row["job"] == job)),
                "a broken reconcile did not name the unavailable identity: {value}"
            );
        }
    }
    assert_eq!(
        fx.tree(),
        tracker_before,
        "a sidecar failure wrote the tracker"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "a sidecar failure changed the ordinary loop"
    );

    // Restore the EXACT original ledger evidence — never a fabricated one.
    std::fs::remove_dir(&ledger).unwrap();
    std::fs::write(&ledger, &original).unwrap();

    // If the failure also refused the durable completion, the exact same
    // result — same text, same SHA — is filed once the storage is healthy.
    if after.state != "completed" {
        let retried = report_result(&fx, &worker, &message, marker, Some(REPAIRED_SHA))
            .expect("the exact completion was refused after the ledger was restored");
        assert_eq!(
            retried["private_delivery"]["state"], "recorded",
            "restored storage did not settle the completion: {retried}"
        );
        let completed = fx.shared.store.message(&message).unwrap().unwrap();
        assert_eq!(completed.state, "completed");
        let stored = completed
            .result
            .as_ref()
            .expect("completed kickoff has no result");
        assert_eq!(stored["text"], marker);
        assert_eq!(stored["sha"], REPAIRED_SHA);
    }

    let message_before = fx
        .shared
        .store
        .message(&message)
        .unwrap()
        .unwrap()
        .to_json();
    let task_before = fx.shared.store.task(&task).unwrap().to_json();
    let tracker_after = fx.tree();
    let recovered = fx.op("delivery_private_reconcile", json!({"job": job}));
    assert!(
        recovered["unavailable"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "restored ledger still reported unavailable work: {recovered}"
    );
    let rows = delivery_for(&fx, &job, &task, &message);
    assert_eq!(
        rows.len(),
        1,
        "recovery duplicated or missed the delivery: {rows:?}"
    );
    let row = &rows[0];
    assert_eq!(row["state"], "completed");
    let results = row["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "recovery duplicated the result: {row}");
    assert_eq!(results[0]["message"], message);
    assert_eq!(results[0]["body"], marker);
    assert_eq!(results[0]["sha"], REPAIRED_SHA);
    assert_eq!(
        results[0]["digest"],
        crate::delivery::private::result_digest(&message, Some(REPAIRED_SHA), marker)
    );
    assert!(row["publications"].as_array().unwrap().is_empty());
    let replay = fx.op("delivery_private_reconcile", json!({"job": job}));
    assert_eq!(
        replay["deliveries"], recovered["deliveries"],
        "a replayed recovery rewrote the exact result"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&message)
            .unwrap()
            .unwrap()
            .to_json(),
        message_before,
        "recovery rewrote the native kickoff"
    );
    assert_eq!(
        fx.shared.store.task(&task).unwrap().to_json(),
        task_before,
        "recovery reran or rewrote the task"
    );
    assert_eq!(
        fx.tree(),
        tracker_after,
        "recovery wrote shared tracker state"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "recovery changed the ordinary loop"
    );
    assert_eq!(
        shared_comments(&fx, marker),
        0,
        "recovery published private work"
    );
}
