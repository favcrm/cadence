//! CAD-2215 independent milestone/epic and hidden-job status acceptance.
//! The product implementer must not edit or weaken this check.
//! Signed-board HTTP only; this does not prove local-agent isolation.

use super::*;

fn put_issue(rig: &Rig, project: &str, id: &str, title: &str, extra: &str) {
    let dir = rig.pm.join(project).join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("issue.md"), format!(
        "---\nid: {id}\ntitle: {title}\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n{extra}---\n"
    )).unwrap();
}

fn row<'a>(payload: &'a Value, collection: &str, id: &str) -> &'a Value {
    payload[collection]
        .as_array()
        .expect("response collection missing")
        .iter()
        .find(|v| v["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("{collection} missing {id}"))
}

#[test]
fn signed_board_milestones_and_epics_scope_projects_and_private_job_outcomes() {
    let _flag = AssistantFlagOn::arm();
    let (rig, events) = queue_rig(&["ana", "ben"]);
    let _keys = initialize_queue_keyring(&rig);
    seed(&rig);
    // Seed real declared milestones, epic parents and children on both sides
    // of the existing workspace-visible/private project boundary.
    std::fs::write(
        rig.pm.join("visible/PROJECT.md"),
        "---\nmilestones:\n  - id: m-visible\n    title: CAD2215_VISIBLE_MILESTONE\n---\n",
    )
    .unwrap();
    std::fs::write(
        rig.pm.join("secret/PROJECT.md"),
        "---\nmilestones:\n  - id: m-secret\n    title: CAD2215_PRIVATE_MILESTONE\n---\n",
    )
    .unwrap();
    put_issue(
        &rig,
        "visible",
        "VIS-1",
        "CAD2215_VISIBLE_EPIC",
        "type: epic\nmilestone: m-visible\n",
    );
    put_issue(
        &rig,
        "visible",
        "VIS-2",
        "CAD2215_VISIBLE_CHILD",
        "parent: VIS-1\n",
    );
    put_issue(
        &rig,
        "secret",
        "SEC-1",
        "CAD2215_PRIVATE_EPIC",
        "type: epic\nmilestone: m-secret\n",
    );
    put_issue(
        &rig,
        "secret",
        "SEC-2",
        "CAD2215_PRIVATE_CHILD",
        "parent: SEC-1\n",
    );
    let ana = login(&rig, "ana", "member");
    let ben = login(&rig, "ben", "member");
    let operator = login(&rig, "cad2215-epic-operator", "owner");

    for path in ["/api/milestones", "/api/epics"] {
        let owner = get(&rig, &operator, path).to_string();
        let member = get(&rig, &ben, path).to_string();
        for marker in [
            "CAD2215_VISIBLE_MILESTONE",
            "m-visible",
            "VIS-1",
            "VIS-2",
            "CAD2215_VISIBLE_EPIC",
        ] {
            // The epic endpoint does not render the configured milestone title.
            if path == "/api/epics" && marker == "CAD2215_VISIBLE_MILESTONE" {
                continue;
            }
            assert!(
                owner.contains(marker),
                "operator {path} omitted visible {marker}"
            );
            assert!(
                member.contains(marker),
                "member {path} omitted visible {marker}"
            );
        }
        for marker in ["m-secret", "SEC-1", "SEC-2", "CAD2215_PRIVATE_EPIC"] {
            assert!(
                owner.contains(marker),
                "operator {path} cannot recover private {marker}"
            );
            assert!(
                !member.contains(marker),
                "member {path} leaked private {marker}"
            );
        }
        if path == "/api/milestones" {
            assert!(
                owner.contains("CAD2215_PRIVATE_MILESTONE"),
                "operator lost private milestone title"
            );
            assert!(
                !member.contains("CAD2215_PRIVATE_MILESTONE"),
                "member saw private milestone title"
            );
        }
        assert!(
            !member.contains("CAD2215_PRIVATE_CHILD"),
            "member {path} saw private child title"
        );
    }

    // Capture the member's visible epic and milestone before any hidden job
    // exists. One real ordinary child and its count must remain visible.
    let before_epics = get(&rig, &ben, "/api/epics");
    let before_milestones = get(&rig, &ben, "/api/milestones");
    let before_epic = row(&before_epics, "epics", "VIS-1").clone();
    let before_milestone = row(&before_milestones, "milestones", "m-visible").clone();
    assert_eq!(before_epic["total"], 1, "ordinary child count is not one");
    assert!(
        before_epic.to_string().contains("VIS-2"),
        "ordinary child missing"
    );
    let operator_before = get(&rig, &operator, "/api/epics");
    let operator_before_epic = row(&operator_before, "epics", "VIS-1").clone();

    let env = rig.envelope("ana", json!({}), json!({}));
    let (status, requested) = rig.call("job_request", &env,
        &json!({"title":"CAD2215_HIDDEN_STATUS_JOB", "request_key":"cad2215-hidden-status", "issue":"VIS-9"}));
    assert_eq!(
        status, 200,
        "real signed private job request failed: {requested}"
    );
    let job = promoted_job(&rig, &events, "ana", &requested);
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    assert!(shared.store.assist_request_owns(ORG, "ana", &job).unwrap());
    assert!(!shared.store.job_is_subscriber(&job, ORG, "ben").unwrap());
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    // Synthetic issue transition, not a supported rebind API; owner
    // provenance remains the real signed assistant-request ledger, and that
    // ledger's accepted row moves with the job so the two agree. The hidden
    // task is blocked so operator derived status must actually change.
    assert_eq!(
        db.execute("UPDATE jobs SET issue_id='VIS-2' WHERE id=?1", [&job])
            .unwrap(),
        1
    );
    assert_eq!(
        db.execute(
            "UPDATE assist_request_queue SET issue='VIS-2' WHERE job_id=?1",
            [&job]
        )
        .unwrap(),
        1
    );
    assert!(
        db.execute("UPDATE tasks SET state='blocked' WHERE job_id=?1", [&job])
            .unwrap()
            > 0
    );

    let after_epics = get(&rig, &ben, "/api/epics");
    let after_milestones = get(&rig, &ben, "/api/milestones");
    let after_epic = row(&after_epics, "epics", "VIS-1");
    let after_milestone = row(&after_milestones, "milestones", "m-visible");
    let operator_after = get(&rig, &operator, "/api/epics");
    let operator_after_epic = row(&operator_after, "epics", "VIS-1");
    assert_ne!(
        operator_before_epic["blocked"], operator_after_epic["blocked"],
        "operator control did not observe the hidden blocked job outcome"
    );
    for field in ["total", "counts", "blocked", "done_ratio"] {
        assert_eq!(
            before_epic[field], after_epic[field],
            "hidden job altered member epic {field}"
        );
    }
    assert_eq!(
        before_epic["work"]["progress"], after_epic["work"]["progress"],
        "hidden job altered member epic progress"
    );
    assert_eq!(
        before_epic["work"]["health"], after_epic["work"]["health"],
        "hidden job altered member epic health"
    );
    assert_eq!(
        before_milestone["tasks"], after_milestone["tasks"],
        "hidden job altered member milestone task status"
    );
    assert_eq!(
        before_milestone["health"], after_milestone["health"],
        "hidden job altered member milestone health"
    );
    for payload in [&after_epics, &after_milestones] {
        let text = payload.to_string();
        assert!(
            !text.contains(&job),
            "member derived view leaked private job ID"
        );
        assert!(
            !text.contains("CAD2215_HIDDEN_STATUS_JOB"),
            "member derived view leaked private job title"
        );
    }
    // Ana is the actual owner and can still read the job itself; a private
    // job's project visibility is not the same as ownership.
    let env = rig.envelope("ana", json!({}), json!({}));
    let (status, evidence) = rig.call("job_evidence", &env, &json!({"job":job}));
    assert_eq!(status, 200, "job owner lost evidence: {evidence}");
    let _ = ana;
}
