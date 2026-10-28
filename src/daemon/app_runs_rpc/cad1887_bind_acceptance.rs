//! CAD-1887: `app_run_ticket_bind` records an issue only when that id is
//! the reservation's selected ticket and the issue file is already
//! committed and clean. A missing id, another committed issue, and the
//! same body still staged never bind. The reservation stays unbound, no
//! second issue is allocated, and the staged leftover stays in the index.

use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::daemon::{ServeOptions, Shared};
use crate::issue::write::{app_ticket_body, app_ticket_title, materialize_app_run_ticket};
use crate::issue::Pm;
use crate::store::{NewAgent, Take};
use crate::test_seam::{scoped, Asserted};

const WORKFLOW: &str = "---\ntitle: Local\ngoal: Reviewed text\n---\n## Write\nagent: writer\naction: local.text.produce\n\nWrite Markdown.\n\n### Acceptance\n- [ ] Markdown artifact exists\n\n## Review\nagent: reviewer\ndepends_on: 1\naction: local.text.review\n\nReview the exact artifact.\n\n### Acceptance\n- [ ] Exact artifact reviewed\n";

const TWO_PROPOSALS: &str = "\
## Recorded decisions

The group agreed to keep the current lunch hours.

## Proposed next steps

- Publish the Tuesday menu on the board.
- Ask the vendor for a written quote.

## Notes

This line is not a proposal.
";

fn fixture() -> (tempfile::TempDir, Arc<Shared>, Pm) {
    let root = tempfile::tempdir().expect("temporary CAD-1887 daemon root");
    let pm_dir = root.path().join("pm");
    let pm = Pm::init(&pm_dir).expect("initialize isolated PM");
    crate::issue::write::project_add(&pm, "cadence", "CAD", &[], &[], &[], None)
        .expect("project cadence");
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm_dir.to_str().expect("PM path is UTF-8"));
    let shared = Shared::new(root.path(), &opts).expect("initialize isolated daemon store");
    for (alias, role) in [("lead", "pm"), ("writer", "worker"), ("reviewer", "worker")] {
        shared
            .store
            .register_agent(&NewAgent {
                alias,
                provider: "claude",
                endpoint_kind: "managed",
                role,
                cwd: root.path().to_str().expect("fixture path is UTF-8"),
                sandbox: "read-only",
                instructions: None,
                params: Some("{\"upstream\":\"lead\"}"),
                team_role: None,
                model_policy: None,
            })
            .expect("register fixture agent");
        shared
            .store
            .set_identity(
                alias,
                &crate::adapter::Identity {
                    thread_id: "t".into(),
                    session_id: "s".into(),
                    model: None,
                    effort: None,
                    pid: 4242,
                    endpoint: None,
                    generation: Some("g1".into()),
                    attach: None,
                },
            )
            .expect("set fixture identity");
    }
    shared
        .store
        .app_capability_decide("install-1", "sha256:bundle", true)
        .expect("approve the local bundle");
    (root, shared, pm)
}

fn reviewed_run(shared: &Shared, request_id: &str) -> (String, String, String) {
    let inputs = std::collections::BTreeMap::new();
    let workflow = crate::store::app_runs::LocalWorkflow::parse(WORKFLOW, &inputs)
        .expect("local review workflow");
    let run = shared
        .store
        .app_run_create(crate::store::app_runs::LocalRunRequest {
            install_id: "install-1",
            bundle_digest: "sha256:bundle",
            workflow: &workflow,
            inputs: &inputs,
            request_id,
            owner_pm: Some("lead"),
            project_link: None,
        })
        .expect("create the local run");
    let id = run["id"].as_str().expect("run id").to_string();
    shared
        .store
        .app_run_decide(
            &id,
            run["snapshot_digest"].as_str(),
            false,
            Some("sha256:bundle"),
        )
        .expect("approve the run");
    let dispatched = shared
        .store
        .app_run_dispatch(&id, "sha256:bundle")
        .expect("dispatch the writer");
    let produce_step = dispatched["steps"][0]["step_id"]
        .as_str()
        .expect("writer step")
        .to_string();
    let writer = start_step(shared, &dispatched, 0, "writer");
    let envelope = json!({
        "schema": 1,
        "kind": "produce_text",
        "run_id": id,
        "step_id": produce_step,
        "revision": 1,
        "outcome": "succeeded",
        "artifacts": [{"media_type": "text/markdown", "text": TWO_PROPOSALS}],
    });
    shared
        .store
        .finish(
            &writer,
            "completed",
            &json!({"turn_id": writer.turn_id, "text": envelope.to_string()}),
            None,
        )
        .expect("writer finishes");
    let next = shared
        .store
        .app_run_dispatch(&id, "sha256:bundle")
        .expect("dispatch the reviewer");
    let review_step = next["steps"][1]["step_id"]
        .as_str()
        .expect("reviewer step")
        .to_string();
    let reviewer = start_step(shared, &next, 1, "reviewer");
    let artifact = next["artifacts"][0]["id"]
        .as_str()
        .expect("artifact id")
        .to_string();
    let fetched = shared
        .store
        .app_artifact_with_digest(
            &artifact,
            Some((&reviewer.id, reviewer.turn_id.as_deref().expect("turn"))),
            "sha256:bundle",
        )
        .expect("reviewer reads the artifact");
    let digest = fetched["digest"].as_str().expect("digest").to_string();
    let response = json!({
        "schema": 1,
        "kind": "review_text",
        "run_id": id,
        "step_id": review_step,
        "revision": 1,
        "producer_step_id": produce_step,
        "producer_revision": 1,
        "artifact_sha256": digest,
        "decision": "approve",
        "rationale": "The proposals are in the source.",
    });
    shared
        .store
        .finish(
            &reviewer,
            "completed",
            &json!({"turn_id": reviewer.turn_id, "text": response.to_string()}),
            None,
        )
        .expect("reviewer approves");
    assert_eq!(
        shared.store.app_run_show(&id).unwrap()["state"],
        "succeeded"
    );
    (id, artifact, digest)
}

fn start_step(shared: &Shared, run: &Value, step: usize, alias: &str) -> crate::store::Message {
    let message = run["steps"][step]["message_id"].as_str().expect("message");
    let Take::Message(taken) = shared
        .store
        .take_queued_app_proven(alias, Some((message, "sha256:bundle")))
        .expect("claim the app step")
    else {
        panic!("app kickoff must be claimed");
    };
    let token = crate::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint("g1");
    shared
        .store
        .mark_running(&taken.id, &token)
        .expect("mark the step running");
    shared.store.message(&taken.id).unwrap().unwrap()
}

fn reserve(
    shared: &Shared,
    run_id: &str,
    decision_id: &str,
    request_id: &str,
    artifact: &str,
    digest: &str,
) -> Value {
    shared
        .store
        .app_run_disposition(run_id, decision_id, artifact, digest, "accept", None)
        .expect("accept the reviewed artifact");
    let reserved = shared
        .store
        .app_run_ticket_reserve(run_id, request_id, artifact, digest, 1, None)
        .expect("reserve the first proposal");
    assert!(reserved["issue_id"].is_null());
    let shown = shared.store.app_run_show(run_id).unwrap();
    json!({
        "run_id": run_id,
        "request_id": shown["ticket"]["request_id"],
        "artifact_id": shown["ticket"]["artifact_id"],
        "artifact_digest": shown["ticket"]["artifact_digest"],
        "index": shown["ticket"]["index"],
        "count": shown["ticket"]["count"],
        "suggestion": shown["ticket"]["suggestion"],
    })
}

fn bind(
    shared: &Arc<Shared>,
    run_id: &str,
    request_id: &str,
    issue_id: &str,
) -> crate::Result<Value> {
    scoped(Asserted::Operator, || {
        shared.dispatch(
            "app_run_ticket_bind",
            &json!({
                "run_id": run_id,
                "request_id": request_id,
                "issue_id": issue_id,
            }),
            std::process::id(),
        )
    })
}

fn head(pm_dir: &Path) -> String {
    crate::issue::git(pm_dir, &["rev-parse", "HEAD"]).expect("HEAD")
}

fn bound_events(shared: &Shared) -> i64 {
    let conn = shared.store.try_conn().expect("store connection");
    conn.query_row(
        "SELECT COUNT(*) FROM events WHERE alias=?1 AND kind=?2",
        [
            crate::store::app_runs::APP_RUN_APPROVAL_STREAM,
            "app_run_ticket_bound",
        ],
        |row| row.get(0),
    )
    .expect("count bound events")
}

fn stage_exact(pm: &Pm, id: &str, reserved: &Value) -> Vec<u8> {
    let body = app_ticket_body(reserved).expect("reservation body");
    let title = app_ticket_title(reserved["suggestion"].as_str().expect("suggestion"));
    let dir = pm.dir.join("cadence").join(id);
    std::fs::create_dir_all(dir.join("comments")).unwrap();
    std::fs::create_dir_all(dir.join("artifacts")).unwrap();
    let front = crate::issue::model::Front::new(
        id,
        &title,
        &crate::issue::time::iso(crate::issue::time::now_epoch()),
    );
    crate::issue::write::save_front(&dir, &front, &body).expect("stage the issue text");
    let rel = format!("cadence/{id}/issue.md");
    crate::issue::git(&pm.dir, &["add", "--", &rel]).expect("stage the issue");
    std::fs::read(dir.join("issue.md")).unwrap()
}

#[test]
fn cad1887_bind_accepts_only_the_committed_selected_issue() {
    let (root, shared, pm) = fixture();
    let (ok_run, ok_artifact, ok_digest) = reviewed_run(&shared, "request-ok");
    let ok_reserved = reserve(
        &shared,
        &ok_run,
        "decision-ok",
        "ticket-ok",
        &ok_artifact,
        &ok_digest,
    );
    let created = materialize_app_run_ticket(&pm, root.path(), "cadence", &ok_reserved)
        .expect("commit the selected issue");
    assert_eq!(created["created"], true);
    let ok_issue = created["id"].as_str().expect("issue id").to_string();
    assert_eq!(ok_issue, "CAD-1");
    let head_after_commit = head(&pm.dir);

    let bound = bind(&shared, &ok_run, "ticket-ok", &ok_issue).expect("bind the committed issue");
    assert_eq!(bound["issue_id"], ok_issue);
    assert_eq!(bound["replayed"], false);
    assert_eq!(head(&pm.dir), head_after_commit, "bind must not commit");
    assert_eq!(bound_events(&shared), 1);

    let replay = bind(&shared, &ok_run, "ticket-ok", &ok_issue).expect("replay the same bind");
    assert_eq!(replay["issue_id"], ok_issue);
    assert_eq!(replay["replayed"], true);
    assert_eq!(
        bound_events(&shared),
        1,
        "replay must not write another bound event"
    );
    assert_eq!(
        shared.store.app_run_show(&ok_run).unwrap()["ticket"]["issue_id"],
        ok_issue
    );

    let (no_run, no_artifact, no_digest) = reviewed_run(&shared, "request-no");
    let no_reserved = reserve(
        &shared,
        &no_run,
        "decision-no",
        "ticket-no",
        &no_artifact,
        &no_digest,
    );
    let head_before_refusal = head(&pm.dir);
    let events_before_refusal = bound_events(&shared);
    assert!(!pm.dir.join("cadence/CAD-2").exists());

    let missing = bind(&shared, &no_run, "ticket-no", "CAD-999999")
        .expect_err("a missing issue must not bind")
        .to_string();
    assert!(
        missing.contains("not the committed selected issue"),
        "{missing}"
    );
    let other = bind(&shared, &no_run, "ticket-no", &ok_issue)
        .expect_err("another reservation's issue must not bind")
        .to_string();
    assert!(
        other.contains("not the committed selected issue"),
        "{other}"
    );
    assert!(
        !pm.dir.join("cadence/CAD-2").exists(),
        "a refusal allocated an issue"
    );
    assert!(shared.store.app_run_show(&no_run).unwrap()["ticket"]["issue_id"].is_null());

    let staged_bytes = stage_exact(&pm, "CAD-2", &no_reserved);
    let staged = bind(&shared, &no_run, "ticket-no", "CAD-2")
        .expect_err("a staged issue must not bind")
        .to_string();
    assert!(
        staged.contains("not a committed reservation") && staged.contains("left unchanged"),
        "{staged}"
    );
    assert_eq!(
        std::fs::read(pm.dir.join("cadence/CAD-2/issue.md")).unwrap(),
        staged_bytes,
        "the staged issue was rewritten"
    );
    let index = crate::issue::git(&pm.dir, &["diff", "--cached", "--name-only"]).unwrap();
    assert!(
        index.contains("cadence/CAD-2/issue.md"),
        "staged evidence was cleared: {index}"
    );
    assert!(!pm.dir.join("cadence/CAD-3").exists());
    assert!(shared.store.app_run_show(&no_run).unwrap()["ticket"]["issue_id"].is_null());
    assert_eq!(bound_events(&shared), events_before_refusal);
    assert_eq!(head(&pm.dir), head_before_refusal);
    assert_eq!(
        shared.store.app_run_show(&ok_run).unwrap()["ticket"]["issue_id"],
        ok_issue
    );
}
