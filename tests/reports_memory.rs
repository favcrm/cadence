//! reports_memory: area tests split from tests/integration.rs (CAD-426).
//! End-to-end tests: real socket daemon in-process, fake provider.
//! These exercise the observable contract — queue order, idempotency,
//! restart fencing, approval brokering, serialization — without model calls.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod common;
use common::*;

use cadence_agent::memory;
use cadence_agent::memory::FinalizationReceipt;
use cadence_agent::memory::Front;
use cadence_agent::memory::IdentityProof;
use cadence_agent::memory::Memory;
use cadence_agent::memory::ReviewReceipt;
use cadence_agent::memory::Scope;
use serde_json::json;
use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tempfile::TempDir;

// ---- CAD-23: result text must never truncate on the store/route ----

#[test]
fn long_result_text_survives_store_and_inbox_route() {
    let d = TestDaemon::start();
    let _mock = d.mock_claude("ok", None);
    d.register_inbox("pm");
    d.register_claude("w1", json!({"upstream": "pm"}));
    d.wait_agent("w1", "idle", 15);
    // The claude result echoes the prompt ("MOCK_OK:<prompt>") — the
    // same single-line result text at each probed size.
    for size in [200usize, 2_000, 8_000, 39_000] {
        let id = format!("m{size}");
        d.send("w1", json!({"text": "x".repeat(size), "message": id}))
            .unwrap();
        let m = d.wait_message("w1", &id, &["completed"], 30);
        assert_eq!(
            m["result"]["text"].as_str().unwrap().len(),
            size + 8,
            "store truncated the {size}-char result"
        );
    }
    // An inbox PM receives the full text in every routed body — nothing
    // bounds a non-pty delivery.
    let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    let routed: Vec<&Value> = pm["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["source"] == "worker_result")
        .collect();
    assert_eq!(routed.len(), 4, "{pm}");
    for (m, size) in routed.iter().zip([200usize, 2_000, 8_000, 39_000]) {
        assert!(
            m["body"].as_str().unwrap().contains(&"x".repeat(size)),
            "routed body truncated at {size}: {}",
            m["body"].as_str().unwrap().len()
        );
    }
}

#[test]
fn long_result_bounded_only_for_pty_paste() {
    // The pty paste gate refuses bodies over its size bound — the routed
    // delivery would FAIL outright. The store keeps the record whole;
    // only the paste is bounded, with a pointer to the full record.
    let d = TestDaemon::start();
    let pm_mock = d.mock_devin();
    let _worker_mock = d.mock_claude("ok", None);
    d.register_devin("pm", None);
    d.wait_agent("pm", "idle", 20);
    d.register_claude("w1", json!({"upstream": "pm"}));
    d.wait_agent("w1", "idle", 15);
    d.operator_rpc("agent_ready", json!({"alias": "pm"}))
        .unwrap();
    d.send("w1", json!({"text": "x".repeat(39_000), "message": "m1"}))
        .unwrap();
    let m1 = d.wait_message("w1", "m1", &["completed"], 30);
    assert_eq!(m1["result"]["text"].as_str().unwrap().len(), 39_008);
    // The routed delivery pastes (completes) — before the bound it
    // failed at the pty pre-write gate.
    let routed = {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
            if let Some(m) = pm["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["source"].as_str() == Some("worker_result"))
            {
                break m.clone();
            }
            assert!(Instant::now() < deadline, "no routed result on pm");
            thread::sleep(Duration::from_millis(50));
        }
    };
    let body = routed["body"].as_str().unwrap();
    assert!(body.len() < 4000, "routed body not bounded: {}", body.len());
    assert!(body.contains("agent show w1"), "{body}");
    assert!(
        !body.contains(&"x".repeat(3900)),
        "routed body carried the full text"
    );
    d.wait_message("pm", routed["id"].as_str().unwrap(), &["completed"], 20);
    let screen = std::fs::read_to_string(d.pane_file(&pm_mock, "pm", "screen")).unwrap_or_default();
    assert!(screen.contains("agent show w1"), "{screen}");
}

#[test]
fn long_reconcile_note_survives_store_and_route() {
    let d = TestDaemon::start();
    d.register("w1");
    d.register_inbox("pm");
    d.wait_agent("w1", "idle", 10);
    // An unknown message with reply_to reconciled completed routes its
    // note — a 40k single-line note keeps whole in store and inbox.
    d.send(
        "w1",
        json!({"text": "DISCONNECT", "message": "u1", "reply_to": "pm"}),
    )
    .unwrap();
    d.wait_message("w1", "u1", &["unknown"], 15);
    let note = "n".repeat(40_000);
    d.operator_rpc(
        "message_reconcile",
        json!({"message": "u1", "status": "completed", "note": note}),
    )
    .unwrap();
    let m = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "u1")
        .unwrap()
        .clone();
    assert_eq!(m["result"]["note"].as_str().unwrap().len(), 40_000);
    let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    let routed = pm["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["source"] == "worker_result")
        .cloned()
        .expect("no routed result on pm");
    assert!(
        routed["body"].as_str().unwrap().contains(&note),
        "routed body truncated the note"
    );
}

#[test]
fn long_report_text_survives_pty_report_to_inbox() {
    // The pty report path: `message result --text` carries the full
    // single-line text into the store, and an inbox PM's routed
    // worker_result body carries it whole.
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("w1", None);
    d.register_inbox("pm");
    d.wait_agent("w1", "idle", 20);
    for size in [200usize, 2_000, 8_000, 40_000] {
        let id = format!("r{size}");
        d.send("w1", json!({"text": "w", "message": id, "reply_to": "pm"}))
            .unwrap();
        d.operator_rpc("agent_ready", json!({"alias": "w1"}))
            .unwrap();
        let token = pty_token(&d, "w1", &id);
        d.report(&id, &token, "result", &"x".repeat(size)).unwrap();
        let m = d.wait_message("w1", &id, &["completed"], 15);
        assert_eq!(
            m["result"]["text"].as_str().unwrap().len(),
            size,
            "store truncated the {size}-char report"
        );
    }
    let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    let routed: Vec<&Value> = pm["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["source"] == "worker_result")
        .collect();
    assert_eq!(routed.len(), 4, "{pm}");
    for (m, size) in routed.iter().zip([200usize, 2_000, 8_000, 40_000]) {
        assert!(
            m["body"].as_str().unwrap().contains(&"x".repeat(size)),
            "inbox routed body truncated at {size}"
        );
    }
}

/// Write an accepted memory fixture with the same authenticated evidence
/// shape produced by the native daemon path. Dispatch/match tests use this
/// fixture so they exercise retrieval eligibility without pretending that a
/// CLI child outside every agent endpoint can author or review a memory.
fn write_reviewed_memory(
    pm_dir: &Path,
    project: &str,
    slug: &str,
    kind: &str,
    scope: Scope,
    fact: &str,
) -> PathBuf {
    let identity = |alias: &str, registration: u64, role: &str| IdentityProof {
        alias: alias.to_string(),
        registration,
        generation: format!("fixture-{alias}-{registration}"),
        process_start: 100 + registration,
        role: role.to_string(),
    };
    let author = identity("fixture-author", 1, "worker");
    let reviewer_a = identity("fixture-reviewer-a", 2, "worker");
    let reviewer_b = identity("fixture-reviewer-b", 3, "worker");
    let pm = identity("fixture-pm", 4, "pm");
    let body = format!(
        "{fact}\n\n**Why:** reviewed integration fixture.\n\n**How to apply:** apply the fixture rule.\n"
    );
    let mut front = Front {
        id: slug.to_string(),
        kind: kind.to_string(),
        status: "accepted".to_string(),
        scope,
        source: Some("CAD-191".to_string()),
        confidence: "high".to_string(),
        created: "2026-01-01T00:00:00Z".to_string(),
        verified_at: Some("2026-01-02T00:00:00Z".to_string()),
        stale: None,
        supersedes: None,
        author: Some(author.alias.clone()),
        author_proof: Some(author.clone()),
        contributors: Vec::new(),
        review_cycle: 1,
        active_operation: None,
        reviews: Vec::new(),
        finalizations: Vec::new(),
    };
    let path = pm_dir
        .join(project)
        .join("memory")
        .join(format!("{slug}.md"));
    let mut memory = Memory {
        project: project.to_string(),
        front: front.clone(),
        body,
        path: path.clone(),
    };
    let digest = memory::semantic_digest(&memory);
    let receipt_digest = digest.clone();
    let receipt = move |reviewer: &IdentityProof, evidence: &str| ReviewReceipt {
        reviewer: reviewer.alias.clone(),
        identity: reviewer.stable_id(),
        generation: reviewer.generation.clone(),
        process_start: reviewer.process_start,
        role: reviewer.role.clone(),
        operation: "accept".to_string(),
        cycle: 1,
        digest: receipt_digest.clone(),
        verdict: "pass".to_string(),
        evidence: evidence.to_string(),
        recorded_at: "2026-01-02T00:00:00Z".to_string(),
    };
    front.reviews = vec![
        receipt(&reviewer_a, "fixture reviewer A evidence"),
        receipt(&reviewer_b, "fixture reviewer B evidence"),
    ];
    front.finalizations.push(FinalizationReceipt {
        operation: "accept".to_string(),
        cycle: 1,
        digest,
        finalizer: pm,
        finalized_at: "2026-01-02T00:00:00Z".to_string(),
    });
    memory.front = front;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        cadence_agent::issue::parse::render(&memory.front, &memory.body).unwrap(),
    )
    .unwrap();
    path
}

/// Give a `write_reviewed_memory` fixture a PM-finalized verify cycle 2
/// at `verified_at`, reviewed by the same two fixture reviewers.
fn mark_memory_verified(path: &Path, verified_at: &str) {
    let text = std::fs::read_to_string(path).unwrap();
    let (mut front, body) = memory::parse_memory(&text).unwrap();
    let verify: Vec<ReviewReceipt> = front
        .reviews
        .iter()
        .map(|r| ReviewReceipt {
            operation: "verify".to_string(),
            cycle: 2,
            recorded_at: verified_at.to_string(),
            ..r.clone()
        })
        .collect();
    front.reviews.extend(verify);
    let accept = front.finalizations[0].clone();
    front.finalizations.push(FinalizationReceipt {
        operation: "verify".to_string(),
        cycle: 2,
        finalized_at: verified_at.to_string(),
        ..accept
    });
    front.review_cycle = 2;
    front.verified_at = Some(verified_at.to_string());
    std::fs::write(
        path,
        cadence_agent::issue::parse::render(&front, &body).unwrap(),
    )
    .unwrap();
}

/// The positive CAD-191 path uses four real mock Devin panes. Each bridge
/// request is opened by the lockholding provider process itself, so the
/// daemon must resolve the actual Unix peer pid through the pane's /proc
/// ancestry and native flock ownership before allowing the write.
#[test]
fn memory_native_socket_identity_requires_distinct_reviewers() {
    let tmp = TempDir::new().unwrap();
    let pm_dir = tmp.path().join("pm");
    let pm = cadence_agent::issue::Pm::init(&pm_dir).unwrap();
    std::fs::create_dir_all(pm_dir.join("demo")).unwrap();
    std::fs::write(
        pm_dir.join("demo/project.yaml"),
        "key: demo\nprefix: D\ncomponents: []\n",
    )
    .unwrap();
    pm.commit(
        &[pm_dir.join("demo/project.yaml")],
        "project fixture\n\nActor: test\n",
    )
    .unwrap();

    let mock_dir = TempDir::new().unwrap();
    let mock = install_mock_devin(mock_dir.path());
    // The daemon's own env, not the process's: concurrent tests never
    // share (or fall back to the host's ~/pm for) a tracker dir.
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let d = TestDaemon::start();
    let cwd = d.dir.path().to_str().unwrap().to_string();
    for (alias, role) in [
        ("author", "worker"),
        ("reviewer-a", "worker"),
        ("reviewer-b", "worker"),
        ("pm", "pm"),
    ] {
        d.operator_rpc(
            "agent_register",
            json!({
                "alias": alias,
                "provider": "devin",
                "endpoint_kind": "pty",
                "cwd": cwd,
                "role": role,
                "params": "{\"auto_ready\":\"verified\"}"
            }),
        )
        .unwrap();
    }
    for alias in ["author", "reviewer-a", "reviewer-b", "pm"] {
        d.wait_agent(alias, "idle", 25);
    }

    // Keep the author in a real running turn while its provider socket
    // performs the proposal. This proves the resolver accepts a live,
    // owned endpoint in the ordinary worker state, not only an idle pane.
    d.send(
        "author",
        json!({"text": "hold native identity", "message": "memory-busy"}),
    )
    .unwrap();
    let busy_token = pty_token(&d, "author", "memory-busy");
    d.wait_agent("author", "busy", 10);

    let body = "\nsocket-bound memory claims native identity\n\n**Why:** the provider socket is the authority.\n\n**How to apply:** use only reviewed native memory.\n";
    let proposal = json!({
        "project": "demo",
        "kind": "rule",
        "scope": {"project": true},
        "source": "CAD-191",
        "confidence": "high",
        "text": body,
        "id": "native-socket-rule"
    });

    // Request identity claims are rejected before any PM write. The
    // author pane remains the only possible source of the later proposal.
    let err = d
        .memory_rpc(
            &mock,
            "author",
            "memory_propose",
            json!({"alias": "pm", "reviewer": "pm", "pane": "pm", "inner": proposal.clone()}),
        )
        .unwrap_err();
    assert!(err.contains("connection-bound"), "{err}");
    assert!(!pm_dir.join("demo/memory/native-socket-rule.md").exists());

    let proposed = d
        .memory_rpc(&mock, "author", "memory_propose", proposal)
        .unwrap();
    assert_eq!(proposed["status"], "proposed", "{proposed}");
    let digest = proposed["digest"].as_str().unwrap().to_string();
    assert_eq!(digest.len(), 64, "{proposed}");

    let read_memory = || std::fs::read(pm_dir.join("demo/memory/native-socket-rule.md")).unwrap();
    let before_author_review = read_memory();
    let err = d
        .memory_rpc(
            &mock,
            "author",
            "memory_review",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "verdict": "pass",
                "evidence": "author cannot review",
                "digest": digest,
            }),
        )
        .unwrap_err();
    assert!(err.contains("author cannot review"), "{err}");
    assert_eq!(before_author_review, read_memory());

    let err = d
        .memory_rpc(
            &mock,
            "reviewer-a",
            "memory_review",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "verdict": "pass",
                "evidence": "spoofed reviewer",
                "digest": digest,
                "reviewer": "reviewer-b",
                "pid": 1,
            }),
        )
        .unwrap_err();
    assert!(err.contains("connection-bound"), "{err}");

    let review_a = d
        .memory_rpc(
            &mock,
            "reviewer-a",
            "memory_review",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "verdict": "pass",
                "evidence": "reviewer A inspected the native socket claim",
                "digest": digest,
            }),
        )
        .unwrap();
    assert_eq!(review_a["quorum"]["eligible"], false, "{review_a}");
    assert!(review_a["quorum"]["reason"]
        .as_str()
        .unwrap()
        .contains("1/2"));

    let before_repeat = read_memory();
    let err = d
        .memory_rpc(
            &mock,
            "reviewer-a",
            "memory_review",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "verdict": "pass",
                "evidence": "same reviewer twice",
                "digest": digest,
            }),
        )
        .unwrap_err();
    assert!(err.contains("already reviewed"), "{err}");
    assert_eq!(before_repeat, read_memory());

    // A detached integration-test RPC has no pane or enrolled-endpoint
    // ancestor: it has no agent identity (CAD-381) and cannot borrow an
    // alias from its params.
    let err = d
        .rpc(
            "memory_review",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "verdict": "pass",
                "evidence": "outside all panes",
                "digest": digest,
            }),
        )
        .unwrap_err();
    assert!(err.to_string().contains("has no agent identity"), "{err}");

    let before_missing_quorum = read_memory();
    let err = d
        .memory_rpc(
            &mock,
            "pm",
            "memory_finalize",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "digest": digest,
            }),
        )
        .unwrap_err();
    assert!(err.contains("1/2"), "{err}");
    assert_eq!(before_missing_quorum, read_memory());

    let before_missing_evidence = read_memory();
    let err = d
        .memory_rpc(
            &mock,
            "reviewer-b",
            "memory_review",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "verdict": "pass",
                "evidence": "",
                "digest": digest,
            }),
        )
        .unwrap_err();
    assert!(err.contains("evidence must be nonempty"), "{err}");
    assert_eq!(before_missing_evidence, read_memory());

    let before_stale = read_memory();
    let err = d
        .memory_rpc(
            &mock,
            "reviewer-b",
            "memory_review",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "verdict": "pass",
                "evidence": "stale digest",
                "digest": "0000000000000000000000000000000000000000000000000000000000000000",
            }),
        )
        .unwrap_err();
    assert!(err.contains("revision changed"), "{err}");
    assert_eq!(before_stale, read_memory());

    // A live pane with a stale stored generation is still refused: the
    // endpoint must match the adapter's current native session proof.
    let reviewer_b_generation = d.rpc("agent_show", json!({"alias": "reviewer-b"})).unwrap()
        ["agent"]["generation"]
        .as_str()
        .unwrap()
        .to_string();
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE agents SET generation=?1 WHERE alias=?2",
        rusqlite::params!["stale-native-generation", "reviewer-b"],
    )
    .unwrap();
    let before_generation_mismatch = read_memory();
    let err = d
        .memory_rpc(
            &mock,
            "reviewer-b",
            "memory_review",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "verdict": "pass",
                "evidence": "stale endpoint generation",
                "digest": digest,
            }),
        )
        .unwrap_err();
    assert!(err.contains("generation changed"), "{err}");
    assert_eq!(before_generation_mismatch, read_memory());
    conn.execute(
        "UPDATE agents SET generation=?1 WHERE alias=?2",
        rusqlite::params![reviewer_b_generation, "reviewer-b"],
    )
    .unwrap();

    let review_b = d
        .memory_rpc(
            &mock,
            "reviewer-b",
            "memory_review",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "verdict": "pass",
                "evidence": "reviewer B independently inspected the native socket claim",
                "digest": digest,
            }),
        )
        .unwrap();
    assert_eq!(review_b["quorum"]["eligible"], true, "{review_b}");

    let finalized = d
        .memory_rpc(
            &mock,
            "pm",
            "memory_finalize",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "digest": digest,
            }),
        )
        .unwrap();
    assert_eq!(finalized["status"], "accepted", "{finalized}");
    assert_eq!(finalized["finalized"], true, "{finalized}");
    assert_eq!(finalized["quorum"]["eligible"], true, "{finalized}");

    let pm = cadence_agent::issue::Pm::at(&pm_dir).unwrap();
    let (_, accepted) = memory::find(&pm, Some("demo"), "native-socket-rule").unwrap();
    assert!(memory::retrieval_status(&accepted).0);
    assert_eq!(accepted.body, body);
    assert_eq!(
        accepted.front.author_proof.as_ref().unwrap().alias,
        "author"
    );
    assert_eq!(
        accepted
            .front
            .reviews
            .iter()
            .map(|r| r.reviewer.as_str())
            .collect::<Vec<_>>(),
        vec!["reviewer-a", "reviewer-b"]
    );

    // Native PM rejection is an ordinary authenticated mutation too. It
    // preserves an earlier review receipt and reports the tracker commit;
    // an HTTP/CLI caller cannot manufacture this result.
    let rejected_proposal = json!({
        "project": "demo",
        "kind": "gotcha",
        "scope": {"project": true},
        "source": "CAD-191",
        "confidence": "medium",
        "text": "native rejection keeps review history\n\n**Why:** the PM rejected it.\n\n**How to apply:** do not use it.\n",
        "id": "native-rejected-rule"
    });
    let proposed_rejected = d
        .memory_rpc(&mock, "author", "memory_propose", rejected_proposal)
        .unwrap();
    let rejected_digest = proposed_rejected["digest"].as_str().unwrap().to_string();
    let review = d
        .memory_rpc(
            &mock,
            "reviewer-a",
            "memory_review",
            json!({
                "slug": "native-rejected-rule",
                "project": "demo",
                "operation": "accept",
                "verdict": "pass",
                "evidence": "reviewer A recorded a retained rejection review",
                "digest": rejected_digest,
            }),
        )
        .unwrap();
    assert_eq!(review["quorum"]["eligible"], false, "{review}");
    let rejected = d
        .memory_rpc(
            &mock,
            "pm",
            "memory_finalize",
            json!({
                "slug": "native-rejected-rule",
                "project": "demo",
                "operation": "reject",
            }),
        )
        .unwrap();
    assert_eq!(rejected["status"], "rejected", "{rejected}");
    assert_eq!(rejected["committed"], true, "{rejected}");
    let (_, rejected_memory) = memory::find(&pm, Some("demo"), "native-rejected-rule").unwrap();
    assert_eq!(rejected_memory.front.status, "rejected");
    assert_eq!(rejected_memory.front.reviews.len(), 1);
    assert_eq!(rejected_memory.front.reviews[0].reviewer, "reviewer-a");
    assert!(!memory::retrieval_status(&rejected_memory).0);

    d.report("memory-busy", &busy_token, "result", "done")
        .unwrap();
    d.wait_message("author", "memory-busy", &["completed"], 10);
}

/// `dispatch` renders matching accepted memories into
/// `<state>/dispatch/<msg>-lessons.md`, names the file in the kickoff
/// and the issue comment, and reports slugs in JSON. `--no-lessons`
/// skips the whole path.
#[test]
fn dispatch_injects_project_memory_lessons() {
    // w1 is `inbox` — the queued kickoff stays inspectable.
    let (_seeded, state) = seeded_state(
        &[
            ("pm", None, "fake", "worker"),
            ("w1", Some("{\"upstream\":\"pm\"}"), "inbox", "worker"),
        ],
        |_, _| {},
    );
    let (tmp, pm_dir, repo, home) = pm_lab_dirs();
    // dispatch_send reads the tracker daemon-side (claim + lane refs) —
    // bind CADENCE_PM_DIR into the daemon's env before it starts.
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let d = TestDaemon::start_on(state);

    let git = git_ok();
    git_f_repo(&repo, &git, |_| {});
    let cli = cadence_cli_json(&d.state, &pm_dir, &home);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    demo_project_init(&cli, &repo_s);
    demo_issue_news(&cli, &["One", "Two", "Three"]);

    // Three reviewed memories on the project: one project-wide rule
    // (matches), one component-scoped gotcha (no component on the issue
    // -> no match), and one provider-scoped rule for a different provider.
    // They are written as authenticated reviewed fixtures because an
    // ordinary CLI child is deliberately not a memory authority.
    write_reviewed_memory(
        &pm_dir,
        "demo",
        "always-drain",
        "rule",
        Scope {
            project: true,
            ..Scope::default()
        },
        "always drain the pipe before send",
    );
    write_reviewed_memory(
        &pm_dir,
        "demo",
        "comp-only",
        "gotcha",
        Scope {
            components: vec!["daemon".to_string()],
            ..Scope::default()
        },
        "daemon-only gotcha",
    );
    write_reviewed_memory(
        &pm_dir,
        "demo",
        "claude-only",
        "rule",
        Scope {
            providers: vec!["claude".to_string()],
            ..Scope::default()
        },
        "claude provider rule",
    );
    // CAD-203: a project-wide rule whose last verify is years old decays
    // to unverified — still injected, labelled — while one explicitly
    // marked stale is withheld from the lessons and the briefing, with
    // its reason.
    let aged = write_reviewed_memory(
        &pm_dir,
        "demo",
        "aged-rule",
        "rule",
        Scope {
            project: true,
            ..Scope::default()
        },
        "aged rule nobody re-checked",
    );
    mark_memory_verified(&aged, "2020-01-01T00:00:00Z");
    let marked = write_reviewed_memory(
        &pm_dir,
        "demo",
        "marked-rule",
        "rule",
        Scope {
            project: true,
            ..Scope::default()
        },
        "rule whose cited fix was reverted",
    );
    let text = std::fs::read_to_string(&marked).unwrap();
    let (mut front, body) = memory::parse_memory(&text).unwrap();
    front.stale = Some("D-9 reverted the cited fix".to_string());
    std::fs::write(
        &marked,
        cadence_agent::issue::parse::render(&front, &body).unwrap(),
    )
    .unwrap();
    // A still-proposed memory never injects. It intentionally has no
    // authenticated proof, so it also documents legacy/proposed withholding.
    let pending = pm_dir.join("demo/memory/pending-one.md");
    std::fs::write(
        pending,
        "---\nid: pending-one\ntype: rule\nstatus: proposed\nconfidence: medium\ncreated: 2026-01-01T00:00:00Z\nscope:\n  project: true\n---\nnot yet accepted\n\n**Why:** pending.\n\n**How to apply:** do not inject.\n",
    )
    .unwrap();

    let note = tmp.path().join("kickoff.md");
    std::fs::write(&note, "# kickoff").unwrap();
    let note_s = note.canonicalize().unwrap().to_str().unwrap().to_string();

    // The dispatch: exactly the project-wide rule lands.
    let (ok, out) = cli(&[
        "dispatch",
        "D-1",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(ok && out["dispatched"] == true, "{out}");
    assert_eq!(
        out["lessons"],
        json!(["aged-rule", "always-drain"]),
        "{out}"
    );
    assert_eq!(
        out["lessons_withheld"],
        json!([{"slug": "marked-rule", "reason": "evidence marked stale: D-9 reverted the cited fix"}]),
        "{out}"
    );
    let lessons_path = PathBuf::from(out["lessons_file"].as_str().unwrap());
    let msg_id = out["message"].as_str().unwrap().to_string();
    assert_eq!(
        lessons_path,
        d.state
            .join("dispatch")
            .join(format!("{msg_id}-lessons.md"))
    );
    let text = std::fs::read_to_string(&lessons_path).unwrap();
    assert!(text.contains("always drain the pipe before send"), "{text}");
    assert!(
        text.contains("- `always-drain` (rule, unverified):"),
        "{text}"
    );
    assert!(
        text.contains(
            "- `aged-rule` (rule, unverified (last verified 2020-01-01)): aged rule nobody re-checked"
        ),
        "{text}"
    );
    assert!(!text.contains("daemon-only gotcha"), "{text}");
    assert!(
        !text.contains("rule whose cited fix was reverted"),
        "{text}"
    );
    assert!(
        text.contains("- `marked-rule`: evidence marked stale: D-9 reverted the cited fix"),
        "{text}"
    );
    assert!(text.len() <= 4096);
    // The kickoff names the file; the comment records the injection.
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let kick = &show["messages"].as_array().unwrap()[0];
    assert_eq!(kick["id"].as_str().unwrap(), msg_id);
    let body_s = kick["body"].as_str().unwrap();
    assert!(
        body_s.contains(&format!("Lessons: {}.", lessons_path.display())),
        "{body_s}"
    );
    let issue = cli(&["issue", "show", "D-1", "--json"]).1;
    assert!(
        issue["comments"].as_array().unwrap().iter().any(|c| {
            c["body"]
                .as_str()
                .unwrap()
                .contains("Lessons injected: aged-rule, always-drain")
        }),
        "{issue}"
    );
    assert!(
        issue["comments"].as_array().unwrap().iter().any(|c| {
            c["body"].as_str().unwrap().contains(
                "Lessons withheld: marked-rule (evidence marked stale: D-9 reverted the cited fix)",
            )
        }),
        "{issue}"
    );

    // --no-lessons: nothing rendered, nothing appended.
    let (ok, out) = cli(&[
        "dispatch",
        "D-2",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
        "--no-lessons",
    ]);
    assert!(ok && out["dispatched"] == true, "{out}");
    assert_eq!(out["lessons"], json!([]), "{out}");
    assert_eq!(out["lessons_file"], Value::Null, "{out}");
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let kick2 = show["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"].as_str() == out["message"].as_str())
        .unwrap();
    assert!(
        !kick2["body"].as_str().unwrap().contains("Lessons:"),
        "{}",
        kick2["body"]
    );
    assert_eq!(
        std::fs::read_dir(d.state.join("dispatch")).unwrap().count(),
        1
    );

    // The bootstrap briefing carries the project's accepted rules for
    // an agent whose cwd sits inside the project repo — proposed and
    // non-matching scopes stay out.
    d.fixture_rpc(
        "agent_register",
        json!({"alias": "w2", "provider": "fake", "endpoint_kind": "fake",
               "cwd": repo, "params": json!({"upstream": "pm"}).to_string()}),
    )
    .unwrap();
    d.wait_agent("w2", "idle", 15);
    let (ok, out) = cli(&["agent", "bootstrap", "w2"]);
    assert!(ok, "{out}");
    let briefing = d.state.join("briefings").join("pm").join("BRIEFING-w2.md");
    let text = std::fs::read_to_string(&briefing).unwrap();
    assert!(
        text.contains("## Project memory — accepted rules (demo)"),
        "{text}"
    );
    assert!(text.contains("always-drain"), "{text}");
    assert!(text.contains("`always-drain` (unverified)"), "{text}");
    assert!(!text.contains("pending-one"), "{text}");
    assert!(!text.contains("claude-only"), "{text}");
    assert!(
        text.contains("`aged-rule` (unverified (last verified 2020-01-01))"),
        "{text}"
    );
    // CAD-395: the stale-marked rule is not applied, but the briefing
    // names it and why.
    assert!(
        !text.contains("rule whose cited fix was reverted"),
        "{text}"
    );
    assert!(
        text.contains(
            "Withheld — stale evidence, not applied:\n\n- `marked-rule`: evidence marked stale: D-9 reverted the cited fix"
        ),
        "{text}"
    );

    // CAD-395: when every match is withheld the lessons file is still
    // written, carrying only its Withheld section.
    let mark = |slug: &str, why: &str| {
        let path = pm_dir.join(format!("demo/memory/{slug}.md"));
        let text = std::fs::read_to_string(&path).unwrap();
        let (mut front, body) = memory::parse_memory(&text).unwrap();
        front.stale = Some(why.to_string());
        std::fs::write(
            &path,
            cadence_agent::issue::parse::render(&front, &body).unwrap(),
        )
        .unwrap();
    };
    mark("always-drain", "D-8 replaced the pipe");
    mark("aged-rule", "D-7 removed the code it cites");
    let (ok, out) = cli(&[
        "dispatch",
        "D-3",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(ok && out["dispatched"] == true, "{out}");
    assert_eq!(out["lessons"], json!([]), "{out}");
    assert_eq!(
        out["lessons_withheld"].as_array().unwrap().len(),
        3,
        "{out}"
    );
    let lessons_path = PathBuf::from(out["lessons_file"].as_str().expect("file written"));
    let text = std::fs::read_to_string(&lessons_path).unwrap();
    assert_eq!(
        text,
        "# Lessons — matched project memories\n\n\
         \n## Withheld — stale evidence, not applied\n\n\
         - `aged-rule`: evidence marked stale: D-7 removed the code it cites\n\
         - `always-drain`: evidence marked stale: D-8 replaced the pipe\n\
         - `marked-rule`: evidence marked stale: D-9 reverted the cited fix\n"
    );
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let kick = show["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"].as_str() == out["message"].as_str())
        .unwrap();
    assert!(
        kick["body"]
            .as_str()
            .unwrap()
            .contains(&format!("Lessons: {}.", lessons_path.display())),
        "{kick}"
    );
    // The briefing likewise applies none and names all three.
    let (ok, out) = cli(&["agent", "bootstrap", "w2"]);
    assert!(ok, "{out}");
    let text = std::fs::read_to_string(&briefing).unwrap();
    assert!(text.contains("(none applied)"), "{text}");
    assert!(
        text.contains("- `always-drain`: evidence marked stale: D-8 replaced the pipe"),
        "{text}"
    );
}

/// Explicit-axis matching resolves the current project from cwd and never
/// searches sibling projects. An explicit `--project` remains available for
/// callers whose cwd is outside a registered repo.
#[test]
fn memory_match_explicit_axes_stay_in_current_project() {
    let tmp = TempDir::new().unwrap();
    let pm_dir = tmp.path().join("pm");
    let repo_a = tmp.path().join("repo-a");
    let repo_b = tmp.path().join("repo-b");
    let home = tmp.path().join("home");
    for dir in [&pm_dir, &repo_a, &repo_b, &home] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let git = |dir: &Path| {
        for args in [
            &["init", "-b", "main"][..],
            &["config", "user.email", "test@example.invalid"][..],
            &["config", "user.name", "test"][..],
        ] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {:?}: {:?}", args, out);
        }
    };
    git(&repo_a);
    git(&repo_b);
    let bin = Path::new(env!("CARGO_BIN_EXE_cadence"));
    let run = |cwd: &Path, args: &[&str]| -> (bool, Value) {
        let out = std::process::Command::new(bin)
            .arg("--state-dir")
            .arg(tmp.path().join("state"))
            .args(args)
            .current_dir(cwd)
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin.parent().unwrap().display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap();
        let text = if out.stdout.is_empty() {
            String::from_utf8_lossy(&out.stderr).to_string()
        } else {
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        (
            out.status.success(),
            serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
        )
    };
    assert!(run(&repo_a, &["issue", "init"]).0);
    let repo_a_s = repo_a.to_str().unwrap();
    let repo_b_s = repo_b.to_str().unwrap();
    assert!(
        run(
            &repo_a,
            &[
                "issue",
                "project",
                "add",
                "alpha",
                "--prefix",
                "A",
                "--repo",
                repo_a_s,
                "--component",
                "daemon",
            ],
        )
        .0
    );
    assert!(
        run(
            &repo_a,
            &[
                "issue",
                "project",
                "add",
                "beta",
                "--prefix",
                "B",
                "--repo",
                repo_b_s,
                "--component",
                "daemon",
            ],
        )
        .0
    );
    // Both records carry a real acceptance quorum; the test is about
    // project resolution, so legacy accepted text must not be enough.
    for (project, id, fact) in [
        ("alpha", "alpha-daemon", "alpha fact"),
        ("beta", "beta-daemon", "beta fact"),
    ] {
        write_reviewed_memory(
            &pm_dir,
            project,
            id,
            "rule",
            Scope {
                components: vec!["daemon".to_string()],
                ..Scope::default()
            },
            fact,
        );
    }

    let (ok, out) = run(
        &repo_a,
        &["memory", "match", "--component", "daemon", "--json"],
    );
    assert!(ok, "{out}");
    assert_eq!(out["context"]["project"], "alpha", "{out}");
    assert_eq!(out["matched"].as_array().unwrap().len(), 1, "{out}");
    assert_eq!(out["matched"][0]["project"], "alpha", "{out}");
    assert_eq!(out["matched"][0]["slug"], "alpha-daemon", "{out}");
    assert_eq!(out["matched"][0]["fact"], "alpha fact", "{out}");

    let (ok, out) = run(
        &home,
        &[
            "memory",
            "match",
            "--project",
            "beta",
            "--component",
            "daemon",
            "--json",
        ],
    );
    assert!(ok, "{out}");
    assert_eq!(out["context"]["project"], "beta", "{out}");
    assert_eq!(out["matched"].as_array().unwrap().len(), 1, "{out}");
    assert_eq!(out["matched"][0]["slug"], "beta-daemon", "{out}");
}

/// Memory failures degrade, never sink a dispatch: a malformed memory
/// file fails matching → no lessons + `lessons_error`; a `Lessons:`
/// suffix that pushes the kickoff body over the pty cap is dropped
/// (original body restored, no file written). Briefings bound their
/// rule section to ≤8 entries and 4 KiB.
#[test]
fn dispatch_degrades_on_memory_failures() {
    let (_seeded, state) = seeded_state(
        &[
            ("pm", None, "fake", "worker"),
            ("w1", Some("{\"upstream\":\"pm\"}"), "inbox", "worker"),
        ],
        |_, _| {},
    );
    let (tmp, pm_dir, repo, home) = pm_lab_dirs();
    // dispatch_send reads the tracker daemon-side (claim + lane refs) —
    // bind CADENCE_PM_DIR into the daemon's env before it starts.
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let d = TestDaemon::start_on(state);

    let git = |dir: &Path, args: &[&str]| {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "git {:?}", args);
    };
    git_f_repo(&repo, &git, |_| {});
    let cli = cadence_cli_json(&d.state, &pm_dir, &home);
    assert!(cli(&["issue", "init"]).0);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    assert!(cli(&["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s]).0);
    // D-2's title is sized so the kickoff body sits just under the
    // 4000-byte cap — the `Lessons:` suffix is what tips it over.
    let long_title = "x".repeat(3720);
    for title in ["One".to_string(), long_title, "Three".to_string()] {
        assert!(cli(&["issue", "new", &title, "--project", "demo"]).0);
    }
    // One good reviewed rule — matching works until the broken file.
    write_reviewed_memory(
        &pm_dir,
        "demo",
        "good-rule",
        "rule",
        Scope {
            project: true,
            ..Scope::default()
        },
        "a good fact",
    );

    let note = tmp.path().join("kickoff.md");
    std::fs::write(&note, "# kickoff").unwrap();
    let note_s = note.canonicalize().unwrap().to_str().unwrap().to_string();

    // Malformed memory file → excluded from the match and named;
    // the valid rule still reaches the kickoff.
    std::fs::write(
        pm_dir.join("demo/memory/broken.md"),
        "---\nid: [unclosed\n---\nbody\n",
    )
    .unwrap();
    let (ok, out) = cli(&[
        "dispatch",
        "D-1",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(ok && out["dispatched"] == true, "{out}");
    assert_eq!(out["lessons"], json!(["good-rule"]), "{out}");
    let lessons_file = out["lessons_file"].as_str().unwrap_or_default();
    assert!(lessons_file.ends_with("-lessons.md"), "{out}");
    let err = out["lessons_error"].as_str().unwrap_or_default();
    assert!(err.contains("broken.md"), "{out}");
    assert!(!out["message"].as_str().unwrap().is_empty());
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let kick = show["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"].as_str() == out["message"].as_str())
        .unwrap();
    assert!(kick["body"].as_str().unwrap().contains("Lessons:"));
    assert!(Path::new(lessons_file).is_file());

    // Over-cap: the good rule matches, but the `Lessons:` suffix would
    // push the kickoff body past the 4000-byte pty cap → the suffix
    // and the file are dropped, the original body still sends.
    std::fs::remove_file(pm_dir.join("demo/memory/broken.md")).unwrap();
    let (ok, out) = cli(&[
        "dispatch",
        "D-2",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(ok && out["dispatched"] == true, "{out}");
    assert_eq!(out["lessons"], json!([]), "{out}");
    assert_eq!(out["lessons_file"], Value::Null, "{out}");
    let err = out["lessons_error"].as_str().unwrap_or_default();
    assert!(err.contains("body limit"), "{out}");
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let kick = show["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"].as_str() == out["message"].as_str())
        .unwrap();
    let sent = kick["body"].as_str().unwrap();
    assert!(!sent.contains("Lessons:"), "suffix dropped: {sent}");
    assert!(
        sent.len() > 3900 && sent.len() <= 4000,
        "original long body sent: {}",
        sent.len()
    );
    // No new lessons file — D-1's remains the only one — and no
    // half-written .tmp residue.
    let names: Vec<String> = std::fs::read_dir(d.state.join("dispatch"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(
        names.iter().filter(|n| n.ends_with("-lessons.md")).count(),
        1,
        "{names:?}"
    );
    assert!(!names.iter().any(|n| n.ends_with(".tmp")), "{names:?}");

    // Unwritable lessons dir: `<state>/dispatch` as a plain file →
    // create_dir_all fails → dispatch still lands, the error is
    // named, and nothing that looks like a lessons artifact exists.
    std::fs::remove_dir_all(d.state.join("dispatch")).unwrap();
    std::fs::write(d.state.join("dispatch"), "not a dir").unwrap();
    let (ok, out) = cli(&[
        "dispatch",
        "D-3",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(ok && out["dispatched"] == true, "{out}");
    assert_eq!(out["lessons"], json!([]), "{out}");
    assert_eq!(out["lessons_file"], Value::Null, "{out}");
    let err = out["lessons_error"].as_str().unwrap_or_default();
    assert!(err.contains("unwritable"), "{out}");
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let kick = show["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"].as_str() == out["message"].as_str())
        .unwrap();
    assert!(!kick["body"].as_str().unwrap().contains("Lessons:"));
    assert!(
        d.state.join("dispatch").is_file(),
        "the placeholder is untouched — no dir or file replaced it"
    );

    // Briefing cap: an oversized first rule is skipped, not a stop —
    // later smaller rules still list, ≤8 entries and ≤4 KiB hold,
    // and the omission is counted. fat-rule-00's hand-edited 5 KiB
    // fact alone exceeds the byte budget: under the old `break` it
    // hid every rule after it.
    // These are reviewed fixtures too: accepted legacy text is deliberately
    // withheld from dispatch, so the briefing-cap assertion must use the
    // same authenticated evidence shape as the ordinary dispatch fixtures.
    write_reviewed_memory(
        &pm_dir,
        "demo",
        "fat-rule-00",
        "rule",
        Scope {
            project: true,
            ..Scope::default()
        },
        &"z".repeat(5 * 1024),
    );
    write_reviewed_memory(
        &pm_dir,
        "demo",
        "fat-rule-01-tiny",
        "rule",
        Scope {
            project: true,
            ..Scope::default()
        },
        "t",
    );
    for i in 2..10 {
        write_reviewed_memory(
            &pm_dir,
            "demo",
            &format!("fat-rule-{i:02}"),
            "rule",
            Scope {
                project: true,
                ..Scope::default()
            },
            &"y".repeat(700),
        );
    }
    d.fixture_rpc(
        "agent_register",
        json!({"alias": "w2", "provider": "fake", "endpoint_kind": "fake",
               "cwd": repo, "params": json!({"upstream": "pm"}).to_string()}),
    )
    .unwrap();
    d.wait_agent("w2", "idle", 15);
    let (ok, out) = cli(&["agent", "bootstrap", "w2"]);
    assert!(ok, "{out}");
    let briefing = d.state.join("briefings").join("pm").join("BRIEFING-w2.md");
    let text = std::fs::read_to_string(&briefing).unwrap();
    let section = text.split("## Project memory").nth(1).unwrap_or_default();
    let items = section.split("\n\n`cadence memory match").next().unwrap();
    let listed = items.matches("- `fat-rule-").count();
    assert!((1..=8).contains(&listed), "{listed} rules in section");
    assert!(items.len() <= 4 * 1024 + 128, "{} bytes", items.len());
    // The oversized rule never listed; the tiny rule after it did —
    // proof the budget skip keeps scanning. The omission is counted.
    assert!(!items.contains("fat-rule-00`"), "{items}");
    assert!(items.contains("- `fat-rule-01-tiny`"), "{items}");
    assert!(items.contains("accepted rule(s) omitted"), "{items}");
}

/// The routing contract: `question`, `feedback` and `bug` file into
/// `cadence` from any cwd; `idea` files into the cwd's project (or
/// --project) and refuses when neither resolves. Priorities default
/// P3 except `bug` (P2); every issue carries `intake` + kind tags.
#[test]
fn report_routes_by_kind_and_defaults() {
    let s = ReportFx::new();

    // bug/question/feedback from the product repo all land in cadence.
    for (kind, want_id) in [("bug", "C-1"), ("question", "C-2"), ("feedback", "C-3")] {
        let (ok, out) = s.cli_at(&s.product_repo, &["report", "--kind", kind, "-m", "x"]);
        assert!(ok && out["id"] == want_id, "{kind}: {out}");
        assert_eq!(out["project"], "cadence");
    }
    // bug defaults P2, the rest P3; --priority overrides.
    let (_, out) = s.cli(&["report", "show", "C-1"]);
    assert_eq!(out["priority"], "P2");
    let (_, out) = s.cli(&["report", "show", "C-2"]);
    assert_eq!(out["priority"], "P3");
    let (ok, out) = s.cli_at(
        &s.product_repo,
        &["report", "--kind", "bug", "--priority", "P0", "-m", "sev"],
    );
    assert!(ok && out["priority"] == "P0", "{out}");

    // idea from the product repo lands in product.
    let (ok, out) = s.cli_at(
        &s.product_repo,
        &["report", "--kind", "idea", "-m", "a product idea"],
    );
    assert!(
        ok && out["id"] == "P-1" && out["project"] == "product",
        "{out}"
    );

    // idea from a foreign cwd refuses, naming --project.
    let (_, stderr, _) = s.cli_at_env(
        &s.foreign_cwd,
        &["report", "--kind", "idea", "-m", "stray idea"],
        &[],
    );
    assert!(stderr.contains("--project"), "{stderr}");

    // --project wins even for kinds that would otherwise route by cwd.
    let (ok, out) = s.cli_at(
        &s.product_repo,
        &[
            "report",
            "--kind",
            "idea",
            "--project",
            "cadence",
            "-m",
            "a cadence idea",
        ],
    );
    assert!(ok && out["project"] == "cadence", "{out}");

    // Tags: intake + kind on every issue; the commit carries the
    // Actor trailer.
    let body = s.issue_body("cadence", "C-1");
    assert!(
        body.contains("- bug") && body.contains("- intake"),
        "{body}"
    );
    let log = s.tracker_log(6);
    assert!(log.contains("Actor:"), "{log}");

    // The default kind is feedback — `cadence report -m` files into
    // cadence's project.
    let (ok, out) = s.cli_at(&s.product_repo, &["report", "-m", "no kind"]);
    assert!(
        ok && out["kind"] == "feedback" && out["project"] == "cadence",
        "{out}"
    );
}

/// `--issue` attaches the report as a comment on the named issue and
/// creates nothing new; the comment carries the kind and context.
#[test]
fn report_issue_attaches_comment() {
    let s = ReportFx::new();
    let (ok, out) = s.cli(&["issue", "new", "Target", "--project", "product"]);
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap().to_string();

    let (ok, out) = s.cli(&[
        "report",
        "--issue",
        &id,
        "--kind",
        "question",
        "-m",
        "what does the flag do?",
    ]);
    assert!(ok && out["id"] == id && out["kind"] == "question", "{out}");
    let comments = s.pm_dir.join("product").join(&id).join("comments");
    let comment = std::fs::read_dir(&comments)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let text = std::fs::read_to_string(comment).unwrap();
    assert!(text.contains("what does the flag do?"), "{text}");
    assert!(text.contains("## Report context"), "{text}");
}

/// A credential-shaped string in the report body never lands: a known
/// provider prefix is a blocking scan finding and the write is refused
/// (CAD-440), while a shape only the scrubber knows — a bare
/// high-entropy token — is stored redacted.
#[test]
fn report_redacts_credential_shapes() {
    let s = ReportFx::new();
    let secret = "ghp_".to_string() + &"a".repeat(36);
    let (ok, stderr, _) = s.cli_at_env(
        &s.product_repo,
        &[
            "report",
            "--kind",
            "bug",
            "-m",
            &format!("leaked {secret} in CI log"),
        ],
        &[],
    );
    assert!(!ok, "{stderr}");
    assert!(
        stderr.contains("secret_detected") || stderr.contains("refused"),
        "{stderr}"
    );
    assert!(!stderr.contains(&secret), "{stderr}");

    let s = ReportFx::new();
    let token = format!("{}{}", "x9K", "mQ2").repeat(10);
    let (ok, out) = s.cli(&[
        "report",
        "--kind",
        "bug",
        "-m",
        &format!("saw {token} in CI log"),
    ]);
    assert!(ok, "{out}");
    let body = s.issue_body("cadence", out["id"].as_str().unwrap());
    assert!(!body.contains(&token), "{body}");
    assert!(body.contains("[REDACTED]"), "{body}");
}

/// The Overview needs-me row appears while the issue sits in backlog
/// and clears when it leaves — and `report ls` filters by kind and
/// project.
#[test]
fn report_needs_me_row_and_ls_filters() {
    let s = ReportFx::new();
    let (ok, _) = s.cli_at(
        &s.product_repo,
        &["report", "--kind", "idea", "-m", "an idea"],
    );
    assert!(ok);
    let (ok, _) = s.cli_at(&s.product_repo, &["report", "--kind", "bug", "-m", "a bug"]);
    assert!(ok);

    let view = overview_at(&s.home, &s.state, Some(&s.pm_dir), &[]);
    let needs = view["needs_me"].as_array().unwrap();
    let intake: Vec<&Value> = needs.iter().filter(|n| n["kind"] == "intake").collect();
    assert_eq!(intake.len(), 2, "{needs:?}");
    let commands: Vec<&str> = intake
        .iter()
        .filter_map(|n| n["command"].as_str())
        .collect();
    assert!(
        commands.contains(&"cadence report show P-1"),
        "{commands:?}"
    );
    assert!(
        commands.contains(&"cadence report show C-1"),
        "{commands:?}"
    );

    // `ls` filters: by kind and by project.
    let (_, out) = s.cli(&["report", "ls", "--kind", "idea"]);
    assert_eq!(out["count"], 1);
    assert_eq!(out["reports"][0]["id"], "P-1");
    let (_, out) = s.cli(&["report", "ls", "--project", "cadence"]);
    assert_eq!(out["count"], 1);
    assert_eq!(out["reports"][0]["id"], "C-1");

    // Moving the issue off backlog clears the row.
    let (ok, _) = s.cli(&["issue", "set", "P-1", "status=ready"]);
    assert!(ok);
    let view = overview_at(&s.home, &s.state, Some(&s.pm_dir), &[]);
    let needs = view["needs_me"].as_array().unwrap();
    assert_eq!(
        needs.iter().filter(|n| n["kind"] == "intake").count(),
        1,
        "{needs:?}"
    );
}

/// CAD-437: `report ls` shares the grammar — any-of value flags, AND
/// across them, `--open`/`--all` scopes, unknown values error, and the
/// sort/limit/fields tail.
#[test]
fn report_ls_cad437_grammar() {
    let s = ReportFx::new();
    // Bugs route to the `cadence` project wherever they are filed.
    for (cwd, kind, msg) in [
        (&s.product_repo, "idea", "idea one"), // P-1
        (&s.product_repo, "bug", "bug one"),   // C-1
        (&s.cadence_repo, "bug", "cad bug"),   // C-2
    ] {
        let (ok, out) = s.cli_at(cwd, &["report", "--kind", kind, "-m", msg]);
        assert!(ok, "{out}");
    }
    let ids = |v: &Value| -> Vec<String> {
        let mut ids: Vec<String> = v["reports"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids
    };

    // Comma-join and repeat are the same any-of; AND across flags.
    let (_, out) = s.cli(&["report", "ls", "--kind", "bug,idea"]);
    assert_eq!(ids(&out).len(), 3, "{out}");
    let (_, out) = s.cli(&["report", "ls", "--kind", "bug", "--kind", "idea"]);
    assert_eq!(ids(&out).len(), 3, "{out}");
    let (_, out) = s.cli(&["report", "ls", "--kind", "bug", "--project", "cadence"]);
    assert_eq!(ids(&out), ["C-1", "C-2"], "{out}");
    let (_, out) = s.cli(&["report", "ls", "--ticket", "P-1"]);
    assert_eq!(ids(&out), ["P-1"], "{out}");
    let (_, out) = s.cli(&["report", "ls", "--source", "task"]);
    assert_eq!(ids(&out), Vec::<String>::new(), "{out}");

    // --open drops resolved intake rows; --all brings them back.
    let (ok, _) = s.cli(&["issue", "set", "P-1", "status=done"]);
    assert!(ok);
    let (_, out) = s.cli(&["report", "ls"]);
    assert_eq!(ids(&out), ["C-1", "C-2"], "{out}");
    let (_, out) = s.cli(&["report", "ls", "--all"]);
    assert_eq!(ids(&out), ["C-1", "C-2", "P-1"], "{out}");
    // clap refuses --open with --all before the handler runs (plain
    // text on stderr, not the JSON error channel).
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&s.state)
        .args(["report", "ls", "--open", "--all"])
        .env("CADENCE_PM_DIR", &s.pm_dir)
        .env("HOME", &s.home)
        .env_remove("CADENCE_ALIAS")
        .output()
        .unwrap();
    assert!(!out.status.success());

    // Unknown values and bad sorts are errors.
    for args in [
        &["report", "ls", "--kind", "zzz"][..],
        &["report", "ls", "--source", "zzz"][..],
        &["report", "ls", "--project", "zzz"][..],
        &["report", "ls", "--ticket", "not an id"][..],
        &["report", "ls", "--sort", "zzz"][..],
    ] {
        let (ok, err, _) = s.cli_at_env(&s.product_repo, args, &[]);
        assert!(!ok, "{args:?} must fail: {err}");
    }

    // sort asc + limit + fields.
    let (_, out) = s.cli(&["report", "ls", "--all", "--sort", "id", "--limit", "2"]);
    assert_eq!(ids(&out), ["C-1", "C-2"], "{out}");
    let (_, out) = s.cli(&[
        "report", "ls", "--kind", "bug", "--fields", "id,kind", "--json",
    ]);
    let keys: Vec<&String> = out["reports"][0].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["id", "kind"], "{out}");
}

/// A report notifies the project's PM inbox — `team.yaml`
/// `roles.pm.alias` names it (ADR 0001); absent any resolvable PM the
/// report still files, with `notified` recording the miss.
#[test]
fn report_notifies_pm_inbox() {
    let s = ReportFx::new();
    let d = TestDaemon::start_on(s.state.clone());
    d.register_inbox("pm");
    // The daemon's own state dir is s.state — report and daemon agree.

    // No team.yaml yet — no resolvable PM, still files fine.
    let (ok, out) = s.cli(&["report", "--kind", "bug", "-m", "first"]);
    assert!(ok && out["notified"].is_null(), "{out}");

    // team.yaml declares the PM inbox — the report sends one line.
    std::fs::write(
        s.pm_dir.join("cadence").join("team.yaml"),
        "roles:\n  pm:\n    kind: inbox\n    alias: pm\n",
    )
    .unwrap();
    let (ok, out) = s.cli(&["report", "--kind", "bug", "-m", "second"]);
    assert!(
        ok && out["notified"]["sent"] == true && out["notified"]["to"] == "pm",
        "{out}"
    );
    let show = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    assert_eq!(show["queued"].as_i64().unwrap(), 1);
    let drained = d.rpc("agent_inbox", json!({"alias": "pm"})).unwrap();
    let msgs = drained["messages"].as_array().unwrap();
    assert!(
        msgs[0]["body"].as_str().unwrap().contains("C-2"),
        "{msgs:?}"
    );
}

/// `cli_at_env` without the JSON parse — for clap-level refusals
/// (`--issue --project`) that exit 2 with plain-text usage.
fn cli_raw_at(s: &ReportFx, cwd: &Path, args: &[&str]) -> (bool, String) {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.arg("--state-dir")
        .arg(&s.state)
        .args(args)
        .env("CADENCE_PM_DIR", &s.pm_dir)
        .env("HOME", &s.home)
        .env_remove("CADENCE_ALIAS")
        .current_dir(cwd);
    let out = cmd.output().unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// Round 2: a multi-line body keeps its line structure — blank lines
/// and indentation survive the prose scrubber — and the title is the
/// first line only, never the collapsed body.
#[test]
fn report_preserves_multiline_body() {
    let s = ReportFx::new();
    let body = "Steps to reproduce:\n\n    1. run `cadence status`\n\t2. see error\n";
    let (ok, out) = s.cli_at(&s.product_repo, &["report", "--kind", "bug", "-m", body]);
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap().to_string();
    let stored = s.issue_body("cadence", &id);
    assert!(stored.contains("    1. run `cadence status`"), "{stored}");
    assert!(stored.contains("\t2. see error"), "{stored}");
    let (_, out) = s.cli(&["report", "show", &id]);
    assert_eq!(out["title"], "Steps to reproduce:", "{out}");
}

/// Round 2: the prose leak rows — each secret asserted absent from
/// the stored issue file.
#[test]
fn report_redacts_prose_secret_forms() {
    let s = ReportFx::new();
    let rows: [(&str, &str); 9] = [
        (
            "Auth header\nAuthorization: Basic dXNlcjpwYXNzd29yZA==",
            "dXNlcjpwYXNzd29yZA==",
        ),
        ("quoted flag\n--password \"correct horse battery\"", "horse"),
        ("user pair\n-u \"admin:hunter 2\"", "admin:hunter"),
        ("prose\nnote: the db password is hunter2 ok", "hunter2"),
        (
            "url\ncall https://api/x?api_key=abcd1234&page=2 done",
            "abcd1234",
        ),
        (
            "pem\n-----BEGIN RSA PRIVATE KEY-----\nMIIabc123\n-----END RSA PRIVATE KEY-----\ntail", // gitleaks:allow — synthetic redaction fixture
            "MIIabc123",
        ),
        (
            "password:\n  synthetic_boundary_value",
            "synthetic_boundary_value",
        ),
        ("Example\n--password=\"first second third\"", "second"),
        (
            "Example\n--password \"first\n synthetic_quote_tail\" ordinary tail",
            "synthetic_quote_tail",
        ),
    ];
    for (i, (body, gone)) in rows.iter().enumerate() {
        let (ok, out) = s.cli_at(&s.product_repo, &["report", "--kind", "bug", "-m", body]);
        assert!(ok, "row {i}: {out}");
        let stored = s.issue_body("cadence", &format!("C-{}", i + 1));
        assert!(!stored.contains(gone), "row {i}: {stored}");
        assert!(stored.contains("[REDACTED]"), "row {i}: {stored}");
    }
    // A PEM marker may itself be the title, so exercise the `--file` path
    // because clap treats a leading `-----` inline value as an option.
    let pem_file = s._tmp.path().join("pem-title.txt");
    std::fs::write(
        &pem_file,
        "-----BEGIN RSA PRIVATE KEY-----\nsynthetic_pem_payload\n-----END RSA PRIVATE KEY-----", // gitleaks:allow — synthetic redaction fixture
    )
    .unwrap();
    let (ok, out) = s.cli_at(
        &s.product_repo,
        &[
            "report",
            "--kind",
            "bug",
            "--file",
            pem_file.to_str().unwrap(),
        ],
    );
    assert!(ok, "{out}");
    let stored = s.issue_body("cadence", "C-10");
    assert!(!stored.contains("synthetic_pem_payload"), "{stored}");
    assert!(stored.contains("[REDACTED]"), "{stored}");
    // The URL keeps its non-secret query params and path.
    let stored = s.issue_body("cadence", "C-5");
    assert!(
        stored.contains("https://api/x?api_key=[REDACTED]&page=2"),
        "{stored}"
    );
}

/// Boundary redaction also applies to comments on existing issues, not just
/// newly filed intake rows.
#[test]
fn report_comment_redacts_boundary_secret_forms() {
    let s = ReportFx::new();
    let (ok, out) = s.cli(&["issue", "new", "Target", "--project", "product"]);
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap().to_string();
    let body = r#"password:
  synthetic_comment_boundary

Example
--password="first synthetic_comment_glued"

-----BEGIN RSA PRIVATE KEY-----
synthetic_comment_pem
-----END RSA PRIVATE KEY-----

Example
--password "first
 synthetic_comment_quote" ordinary tail"#;
    let (ok, out) = s.cli(&["report", "--issue", &id, "--kind", "bug", "-m", body]);
    assert!(ok, "{out}");
    let comments = s.pm_dir.join("product").join(&id).join("comments");
    let comment = std::fs::read_dir(&comments)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let stored = std::fs::read_to_string(comment).unwrap();
    for gone in [
        "synthetic_comment_boundary",
        "synthetic_comment_glued",
        "synthetic_comment_pem",
        "synthetic_comment_quote",
    ] {
        assert!(!stored.contains(gone), "{gone}: {stored}");
    }
    assert!(stored.contains("[REDACTED]"), "{stored}");
}

/// Round 2: control characters reach neither the stored issue nor
/// the PM's inbox line.
#[test]
fn report_strips_control_chars() {
    let s = ReportFx::new();
    let d = TestDaemon::start_on(s.state.clone());
    d.register_inbox("pm");
    std::fs::write(
        s.pm_dir.join("cadence").join("team.yaml"),
        "roles:\n  pm:\n    kind: inbox\n    alias: pm\n",
    )
    .unwrap();
    // --file: argv cannot carry `\x00` at all — the OS rejects it
    // before cadence reads it.
    let body_file = s._tmp.path().join("body.txt");
    std::fs::write(
        &body_file,
        "crash\x1b[2J here\n\x1b]0;pwned\x07second\x00l\n",
    )
    .unwrap();
    let (ok, out) = s.cli(&[
        "report",
        "--kind",
        "bug",
        "--file",
        body_file.to_str().unwrap(),
    ]);
    assert!(ok, "{out}");
    let stored = s.issue_body("cadence", out["id"].as_str().unwrap());
    assert!(
        !stored.contains('\x1b') && !stored.contains('\x07') && !stored.contains('\x00'),
        "{stored}"
    );
    let msgs = d.rpc("agent_inbox", json!({"alias": "pm"})).unwrap();
    let line = msgs["messages"][0]["body"].as_str().unwrap().to_string();
    assert!(!line.chars().any(|c| c.is_control()), "{line:?}");
}

/// Round 2: a body over the 32 KB cap is refused with the cap named;
/// a first line over 200 chars becomes a capped title, not a 300-char
/// board row.
#[test]
fn report_caps_body_and_title() {
    let s = ReportFx::new();
    let big = "x".repeat(33 * 1024);
    let (_, stderr, (ok, _)) = s.cli_at_env(&s.product_repo, &["report", "-m", &big], &[]);
    assert!(!ok && stderr.contains("32 KB"), "{stderr}");

    let long_title = "a".repeat(300);
    let (ok, out) = s.cli(&["report", "-m", &format!("{long_title}\nrest")]);
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap().to_string();
    let (_, out) = s.cli(&["report", "show", &id]);
    assert_eq!(out["title"].as_str().unwrap().chars().count(), 200);
}

/// Round 2: `intake` + kind are system vocabulary — a project with a
/// declared `tags:` allowlist still takes reports.
#[test]
fn report_ignores_project_tag_allowlist() {
    let s = ReportFx::new();
    let repo = s
        .product_repo
        .canonicalize()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let (ok, out) = s.cli(&[
        "issue", "project", "add", "strict", "--prefix", "S", "--repo", &repo, "--tag", "triage",
    ]);
    assert!(ok, "{out}");
    let (ok, out) = s.cli(&[
        "report",
        "--kind",
        "idea",
        "--project",
        "strict",
        "-m",
        "an idea",
    ]);
    assert!(ok && out["project"] == "strict", "{out}");
    let stored = s.issue_body("strict", "S-1");
    assert!(
        stored.contains("- intake") && stored.contains("- idea"),
        "{stored}"
    );
}

/// Round 2: the kind is its own frontmatter field — an extra tag
/// sorting ahead of it cannot mislabel `ls`/`show`.
#[test]
fn report_kind_survives_extra_tags() {
    let s = ReportFx::new();
    let (ok, out) = s.cli_at(&s.product_repo, &["report", "--kind", "bug", "-m", "x"]);
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap().to_string();
    let (ok, out) = s.cli(&["issue", "tag", &id, "add", "aaa-first"]);
    assert!(ok, "{out}");
    let (_, out) = s.cli(&["report", "ls", "--kind", "bug"]);
    assert_eq!(out["count"], 1, "{out}");
    assert_eq!(out["reports"][0]["kind"], "bug");
    let (_, out) = s.cli(&["report", "show", &id]);
    assert_eq!(out["kind"], "bug", "{out}");
    let stored = s.issue_body("cadence", &id);
    assert!(stored.contains("kind: bug"), "{stored}");
}

/// Round 2: `report ls` reads the derived status — a verdict note
/// derives `done` while frontmatter still says `backlog`, and `ls`
/// agrees with the overview.
#[test]
fn report_ls_uses_derived_status() {
    let s = ReportFx::new();
    let (ok, out) = s.cli_at(&s.product_repo, &["report", "--kind", "bug", "-m", "x"]);
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap().to_string();
    std::fs::write(
        s.notes_dir.join("20260101-000000-t-verdict.md"),
        format!("# Close-out\n> Issue: `{id}`\n\n## Verdict\npass\n"),
    )
    .unwrap();
    let (_, out) = s.cli(&["report", "ls"]);
    assert_eq!(out["count"], 0, "{out}");
    // The file still says backlog — `ls` followed the derived status.
    let stored = s.issue_body("cadence", &id);
    assert!(stored.contains("status: backlog"), "{stored}");
}

/// Round 2: `needs_me` caps intake rows at NEEDS_ME_CAP plus one
/// summary row — a flood cannot bury real work.
#[test]
fn report_needs_me_caps_intake_rows() {
    let s = ReportFx::new();
    for i in 0..12 {
        let (ok, out) = s.cli_at(
            &s.product_repo,
            &["report", "--kind", "bug", "-m", &format!("bug {i}")],
        );
        assert!(ok, "{out}");
    }
    let view = overview_at(&s.home, &s.state, Some(&s.pm_dir), &[]);
    let intake: Vec<&Value> = view["needs_me"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["kind"] == "intake")
        .collect();
    assert_eq!(intake.len(), 11, "{intake:?}");
    assert!(
        intake.iter().any(|n| n["title"]
            .as_str()
            .unwrap()
            .contains("2 more intake reports")),
        "{intake:?}"
    );
}

/// Round 2 nits: `--issue` rejects the flags it would ignore;
/// `report show` refuses non-intake issues.
#[test]
fn report_issue_conflicts_and_show_scope() {
    let s = ReportFx::new();
    let (ok, out) = s.cli(&["issue", "new", "Target", "--project", "product"]);
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap().to_string();

    let (ok, text) = cli_raw_at(
        &s,
        &s.product_repo,
        &["report", "--issue", &id, "--project", "cadence", "-m", "x"],
    );
    assert!(!ok && text.contains("--project"), "{text}");
    let (ok, text) = cli_raw_at(
        &s,
        &s.product_repo,
        &["report", "--issue", &id, "--priority", "P0", "-m", "x"],
    );
    assert!(!ok && text.contains("--priority"), "{text}");

    let (ok, _, (_, out)) = s.cli_at_env(&s.product_repo, &["report", "show", &id], &[]);
    assert!(!ok, "{out}");
}

/// Run the real binary with PATH led by the build dir (the tracker's
/// pre-commit hook calls `cadence`); HOME/XDG/TMPDIR inside the fixture;
/// stdout+stderr, raw.
fn task_report_cli(s: &ReportFx, state: &Path, args: &[&str]) -> (bool, String) {
    task_report_cli_env(s, state, args, &[])
}

/// [`task_report_cli`] with extra env (e.g. `CADENCE_ALIAS`).
fn task_report_cli_env(
    s: &ReportFx,
    state: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> (bool, String) {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.arg("--state-dir")
        .arg(state)
        .args(args)
        .env("CADENCE_PM_DIR", &s.pm_dir)
        .env("HOME", &s.home)
        .env("XDG_STATE_HOME", s.home.join(".state"))
        .env("XDG_CONFIG_HOME", s.home.join(".config"))
        .env("TMPDIR", &s.home)
        .env(
            "PATH",
            format!(
                "{}:{}",
                s.bin_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env_remove("CADENCE_ALIAS")
        .current_dir(&s.product_repo);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// Report files on a ticket plus the tracker's commit count — the
/// "nothing was filed" probe.
fn task_report_footprint(s: &ReportFx, id: &str) -> (usize, String) {
    let reports = s.pm_dir.join("product").join(id).join("reports");
    let n = std::fs::read_dir(&reports).map(|d| d.count()).unwrap_or(0);
    (n, git_at(&s.pm_dir, &["rev-list", "--count", "HEAD"]))
}

fn task_report_issue(s: &ReportFx) -> String {
    let (ok, out) = s.cli(&["issue", "new", "Target", "--project", "product"]);
    assert!(ok, "{out}");
    out["id"].as_str().unwrap().to_string()
}

/// `report file` stores the normalised record under the ticket's
/// `reports/`, commits it through the tracker writer, and `issue show`
/// (text and JSON — the board's detail payload) lists it; lint passes;
/// filing the same content again is a no-op duplicate.
#[test]
fn task_report_file_stores_and_show_lists() {
    let s = ReportFx::new();
    let id = task_report_issue(&s);
    let src = s.home.join("done.md");
    let sha = "a".repeat(40);
    std::fs::write(
        &src,
        task_report_text(&format!(
            "agent: dev-1\nsha: {sha}\nconstraints: [no UI work]\ncontext_feedback:\n  \
             used: [{{id: L-1, helpful: true}}]\n  wrong: [{{id: L-2, why: stale flag}}]\n  \
             reread: [src/issue/write.rs]\n"
        )),
    )
    .unwrap();
    let src_s = src.to_str().unwrap();
    let (ok, out) = s.cli(&[
        "report", "file", "--task", &id, "--kind", "done", "--file", src_s,
    ]);
    assert!(
        ok && out["committed"] == true && out["kind"] == "done",
        "{out}"
    );
    let path = out["path"].as_str().unwrap().to_string();
    assert!(path.starts_with(&format!("{id}/reports/")) && path.ends_with("-dev-1.md"));
    let stored = std::fs::read_to_string(s.pm_dir.join("product").join(&path)).unwrap();
    for want in [
        "schema: cadence.report/2",
        "kind: done",
        &format!("task: {id}"),
        "agent: dev-1",
        "## Lesson",
    ] {
        assert!(stored.contains(want), "{want}: {stored}");
    }
    let log = s.tracker_log(1);
    assert!(
        log.contains(&format!("{id}: report done by dev-1"))
            && log.contains(&format!("Issue: {id}"))
            && log.contains("Actor: dev-1"),
        "{log}"
    );

    let (ok, show) = s.cli(&["issue", "show", &id, "--json"]);
    assert!(ok, "{show}");
    let r = &show["reports"][0];
    assert_eq!(r["kind"], "done", "{show}");
    assert_eq!(r["agent"], "dev-1");
    assert_eq!(r["sha"], sha.as_str());
    assert_eq!(r["context_feedback"]["wrong"][0]["why"], "stale flag");
    assert_eq!(r["path"], path.as_str());
    assert!(r["body"].as_str().unwrap().contains("## Evidence"), "{r}");
    let (ok, text) = task_report_cli(&s, &s.state, &["issue", "show", &id]);
    assert!(ok && text.contains("report done by dev-1"), "{text}");
    assert!(text.contains(&path), "{text}");

    let (ok, lint) = s.cli(&["issue", "lint"]);
    assert!(ok && lint["ok"] == true, "{lint}");

    // Retry with the same file: the stored report is reused.
    let (ok, again) = s.cli(&[
        "report", "file", "--task", &id, "--kind", "done", "--file", src_s,
    ]);
    assert!(
        ok && again["duplicate"] == true && again["path"] == path.as_str(),
        "{again}"
    );
    let (_, show) = s.cli(&["issue", "show", &id, "--json"]);
    assert_eq!(show["reports"].as_array().unwrap().len(), 1, "{show}");
}

/// A question carries options/impact and is stored `input-required`;
/// malformed reports and credential-shaped text are refused with
/// nothing written; the intake kinds of `cadence report` are untouched.
#[test]
fn task_report_refuses_malformed_and_secrets() {
    let s = ReportFx::new();
    let id = task_report_issue(&s);
    let file = |name: &str, text: &str| {
        let p = s.home.join(name);
        std::fs::write(&p, text).unwrap();
        p.to_str().unwrap().to_string()
    };
    let q = file(
        "q.md",
        &task_report_text("options: [ship now, wait for CAD-392]\nimpact: blocks the merge\n"),
    );
    let (ok, out) = s.cli(&[
        "report", "file", "--task", &id, "--kind", "question", "--file", &q,
    ]);
    assert!(ok, "{out}");
    let (_, show) = s.cli(&["issue", "show", &id, "--json"]);
    assert_eq!(show["reports"][0]["state"], "input-required", "{show}");
    assert_eq!(show["reports"][0]["options"][1], "wait for CAD-392");

    let reports = s.pm_dir.join("product").join(&id).join("reports");
    let count = || std::fs::read_dir(&reports).unwrap().count();
    let before = count();
    let secret = cad109_token(&["gh", "p_"].concat(), "cad341", 36);
    for (name, text, kind, want) in [
        (
            "missing.md",
            task_report_text("").replace("## Lesson", "## Lessons"),
            "done",
            "## Lesson",
        ),
        (
            "noopts.md",
            task_report_text("impact: x\n"),
            "question",
            "options",
        ),
        (
            "extra.md",
            task_report_text("mood: fine\n"),
            "blocked",
            "unknown field",
        ),
        (
            "mismatch.md",
            task_report_text("kind: done\n"),
            "blocked",
            "disagrees",
        ),
        (
            "secret.md",
            task_report_text("").replace("Evidence text.", &format!("{secret}\n")),
            "done",
            "credential-shaped",
        ),
    ] {
        let p = file(name, &text);
        let (ok, text) = task_report_cli(
            &s,
            &s.state,
            &[
                "report", "file", "--task", &id, "--kind", kind, "--file", &p,
            ],
        );
        assert!(!ok && text.contains(want), "{name}: {text}");
        assert!(!text.contains(&secret), "{name}: {text}");
    }
    assert_eq!(count(), before, "a refused report wrote a file");
    // Unknown ticket refuses too.
    let (ok, _) = task_report_cli(
        &s,
        &s.state,
        &[
            "report", "file", "--task", "P-99", "--kind", "question", "--file", &q,
        ],
    );
    assert!(!ok);

    // Intake kinds keep working unchanged.
    let (ok, out) = s.cli(&["report", "--kind", "question", "-m", "how?"]);
    assert!(
        ok && out["kind"] == "question" && out["status"] == "backlog",
        "{out}"
    );
}

/// `issue lint` validates report files a writer did not produce: a
/// hand-dropped report missing headings or filed under another task
/// fails the lint with the path named.
#[test]
fn task_report_lint_validates_files() {
    let s = ReportFx::new();
    let id = task_report_issue(&s);
    let reports = s.pm_dir.join("product").join(&id).join("reports");
    std::fs::create_dir_all(&reports).unwrap();
    std::fs::write(
        reports.join("20260923T000000Z-x.md"),
        "---\nschema: cadence.report/2\nkind: done\ntask: P-77\nagent: x\n---\n\n## Expected\n",
    )
    .unwrap();
    let (ok, lint) = s.cli(&["issue", "lint"]);
    assert!(!ok && lint["ok"] == false, "{lint}");
    let errors = lint["errors"].to_string();
    assert!(
        errors.contains(&format!("{id}/reports/20260923T000000Z-x.md"))
            && errors.contains("does not match its ticket"),
        "{errors}"
    );
    // The detail payload still lists it, flagged, instead of hiding it.
    let (_, show) = s.cli(&["issue", "show", &id, "--json"]);
    assert!(show["reports"][0]["error"].is_string(), "{show}");
}

/// A dispatched task-bound pty turn on the stub, bound to board issue
/// `issue`: returns (daemon, mock guard, message id, token).
fn task_report_bound_turn(issue: &str) -> (TestDaemon, MockStub, String, String) {
    let d = TestDaemon::start();
    let mock = d.mock_stub();
    d.register_inbox("pm");
    d.register_stub("st", json!({"upstream": "pm", "auto_ready": "verified"}));
    d.wait_agent("st", "idle", 20);
    let (spec, sha) = d.spec_file("spec.md", "report me");
    d.rpc(
        "job_new",
        json!({"pm": "pm", "job": "j1", "spec": spec, "spec_sha256": sha,
               "issue": issue, "task_assignee": "st"}),
    )
    .unwrap();
    let r = d.job_dispatch("j1-t1", json!({})).unwrap();
    let k = r["message"].as_str().unwrap().to_string();
    let token = pty_token(&d, "st", &k);
    (d, mock, k, token)
}

/// A pty worker attaches a report to `message result --report`. The
/// daemon's gates run before anything is filed: a malformed report, a
/// wrong token, or a report for another ticket refuses the result and
/// leaves no report file and no tracker commit. The accepted result
/// carries the `Report:` line; a retry is idempotent on both sides.
#[test]
fn task_report_attaches_to_message_result() {
    let s = ReportFx::new();
    let id = task_report_issue(&s);
    let other = task_report_issue(&s);
    let (d, _mock, k, token) = task_report_bound_turn(&id);
    let result = |token: &str, file: &Path| {
        task_report_cli(
            &s,
            &d.state,
            &[
                "message",
                "result",
                &k,
                "--token",
                token,
                "--text",
                "done",
                "--report",
                file.to_str().unwrap(),
            ],
        )
    };
    let write = |name: &str, text: String| {
        let p = s.home.join(name);
        std::fs::write(&p, text).unwrap();
        p
    };
    let good = write(
        "good.md",
        task_report_text(&format!("kind: done\ntask: {id}\nagent: st\n")),
    );
    let before = task_report_footprint(&s, &id);

    // Malformed report: refused locally.
    let bad = write(
        "bad.md",
        task_report_text(&format!("kind: done\ntask: {id}\n")).replace("## Next", ""),
    );
    let (ok, text) = result(&token, &bad);
    assert!(!ok && text.contains("## Next"), "{text}");
    // Wrong token: the daemon refuses before anything is filed.
    let (ok, text) = result("pty-0-not-the-token", &good);
    assert!(!ok && text.contains("Token"), "{text}");
    // A report for another ticket than the message's bound issue.
    let stray = write(
        "stray.md",
        task_report_text(&format!("kind: done\ntask: {other}\n")),
    );
    let (ok, text) = result(&token, &stray);
    assert!(!ok && text.contains(&format!("is not {id}")), "{text}");
    assert_eq!(
        task_report_footprint(&s, &id),
        before,
        "a refused result filed"
    );
    assert_eq!(task_report_footprint(&s, &other).0, 0);
    assert_eq!(d.message_state("st", &k), "running");

    let (ok, text) = result(&token, &good);
    assert!(ok, "{text}");
    let m = d.wait_message("st", &k, &["completed"], 10);
    let stored = m["result"]["text"].as_str().unwrap_or_default().to_string();
    assert!(
        stored.starts_with("done\n\nReport: ") && stored.contains(&format!("{id}/reports/")),
        "{m}"
    );
    let (_, show) = s.cli(&["issue", "show", &id, "--json"]);
    assert_eq!(show["reports"][0]["agent"], "st", "{show}");
    // Retry: nothing new is filed; the daemon sees an idempotent duplicate.
    let after = task_report_footprint(&s, &id);
    let (ok, text) = result(&token, &good);
    assert!(ok && text.contains("duplicate"), "{text}");
    assert_eq!(task_report_footprint(&s, &id), after);
}

/// With `CADENCE_ALIAS` set the report's author is the caller: a
/// frontmatter `agent`/`author` naming someone else is refused, and a
/// traversal-shaped agent name is refused whoever files it.
#[test]
fn task_report_author_is_the_caller() {
    let s = ReportFx::new();
    let id = task_report_issue(&s);
    let file = |name: &str, front: &str| {
        let p = s.home.join(name);
        std::fs::write(&p, task_report_text(front)).unwrap();
        p.to_str().unwrap().to_string()
    };
    let args = |f: &str| {
        vec![
            "report".to_string(),
            "file".into(),
            "--task".into(),
            id.clone(),
            "--kind".into(),
            "done".into(),
            "--file".into(),
            f.to_string(),
        ]
    };
    let run = |f: &str, env: &[(&str, &str)]| {
        let a = args(f);
        let a: Vec<&str> = a.iter().map(String::as_str).collect();
        task_report_cli_env(&s, &s.state, &a, env)
    };
    let before = task_report_footprint(&s, &id);
    for (name, front) in [("a.md", "agent: qa-1\n"), ("b.md", "author: qa-1\n")] {
        let (ok, text) = run(&file(name, front), &[("CADENCE_ALIAS", "dev-1")]);
        assert!(
            !ok && text.contains("is not the caller 'dev-1'"),
            "{name}: {text}"
        );
    }
    for bad in ["../x", "a/b", "..", "x y"] {
        let (ok, text) = run(&file("t.md", &format!("agent: '{bad}'\n")), &[]);
        assert!(!ok && text.contains("Bad report agent"), "{bad}: {text}");
    }
    assert_eq!(task_report_footprint(&s, &id), before);
    // The matching claim (or none) files under the alias.
    let (ok, text) = run(
        &file("c.md", "agent: dev-1\n"),
        &[("CADENCE_ALIAS", "dev-1")],
    );
    assert!(ok && text.contains("-dev-1.md"), "{text}");
    assert!(s.tracker_log(1).contains("Actor: dev-1"));
}

/// A question stays open until an `answer` report names it; the answer
/// must name an existing question on the same ticket, and the question
/// file is never edited.
#[test]
fn task_report_answer_closes_a_question() {
    let s = ReportFx::new();
    let id = task_report_issue(&s);
    let file = |name: &str, text: String| {
        let p = s.home.join(name);
        std::fs::write(&p, text).unwrap();
        p.to_str().unwrap().to_string()
    };
    let q = file(
        "q.md",
        task_report_text("options: [a, b]\nimpact: blocks merge\n"),
    );
    let (ok, out) = s.cli(&[
        "report", "file", "--task", &id, "--kind", "question", "--file", &q,
    ]);
    assert!(ok, "{out}");
    let qname = out["report"].as_str().unwrap().to_string();
    let qpath = s
        .pm_dir
        .join("product")
        .join(&id)
        .join("reports")
        .join(&qname);
    let qbytes = std::fs::read(&qpath).unwrap();
    let (_, show) = s.cli(&["issue", "show", &id, "--json"]);
    assert_eq!(show["reports"][0]["open"], true, "{show}");

    // Answers must name a question on this ticket.
    let done = file("d.md", task_report_text(""));
    let (ok, out) = s.cli(&[
        "report", "file", "--task", &id, "--kind", "done", "--file", &done,
    ]);
    assert!(ok, "{out}");
    let done_name = out["report"].as_str().unwrap().to_string();
    for target in ["20990101T000000Z-nobody.md", done_name.as_str(), "../x.md"] {
        let a = file(
            "a.md",
            format!("---\nanswers: {target}\n---\n\nGo with a.\n"),
        );
        let (ok, text) = task_report_cli(
            &s,
            &s.state,
            &[
                "report", "file", "--task", &id, "--kind", "answer", "--file", &a,
            ],
        );
        assert!(!ok, "{target}: {text}");
    }
    let a = file(
        "a.md",
        format!("---\nanswers: {qname}\n---\n\nGo with a.\n"),
    );
    let (ok, out) = s.cli(&[
        "report", "file", "--task", &id, "--kind", "answer", "--file", &a,
    ]);
    assert!(ok, "{out}");
    let aname = out["report"].as_str().unwrap().to_string();
    let (_, show) = s.cli(&["issue", "show", &id, "--json"]);
    let reports = show["reports"].as_array().unwrap();
    let question = reports
        .iter()
        .find(|r| r["name"] == qname.as_str())
        .unwrap();
    assert_eq!(question["open"], false, "{show}");
    assert_eq!(question["answered_by"], json!([aname]), "{show}");
    assert_eq!(
        std::fs::read(&qpath).unwrap(),
        qbytes,
        "question was edited"
    );
    let (ok, lint) = s.cli(&["issue", "lint"]);
    assert!(ok && lint["ok"] == true, "{lint}");
}

/// A symlinked `reports/` is never written through and fails lint.
#[test]
fn task_report_refuses_symlinked_reports_dir() {
    let s = ReportFx::new();
    let id = task_report_issue(&s);
    let outside = s.home.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, s.pm_dir.join("product").join(&id).join("reports"))
        .unwrap();
    let f = s.home.join("r.md");
    std::fs::write(&f, task_report_text("")).unwrap();
    let (ok, text) = task_report_cli(
        &s,
        &s.state,
        &[
            "report",
            "file",
            "--task",
            &id,
            "--kind",
            "done",
            "--file",
            f.to_str().unwrap(),
        ],
    );
    assert!(!ok && text.contains("symlink"), "{text}");
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    let (ok, lint) = s.cli(&["issue", "lint"]);
    assert!(
        !ok && lint["errors"].to_string().contains("reports/ is a symlink"),
        "{lint}"
    );
}

/// CAD-381 ACCEPTANCE: memory resolves its caller through the one
/// identity verifier — the daemon's CAD-230 enrollment of a managed
/// provider it launched — so headless endpoints author, review and
/// finalize exactly like panes. A proposal by one managed endpoint is
/// reviewed by two others and finalized by a managed pm to `accepted`;
/// the author still cannot review its own proposal; a spoofed identity
/// param is refused; the test process and a managed tool's detached
/// grandchild have no agent identity (never the endpoint's); and an
/// owner-generation drift revokes the enrollment for
/// good — restoring the generation does not revive it.
#[test]
fn memory_managed_endpoints_authenticate_through_daemon_enrollment() {
    let tmp = TempDir::new().unwrap();
    let pm_dir = tmp.path().join("pm");
    let pm = cadence_agent::issue::Pm::init(&pm_dir).unwrap();
    std::fs::create_dir_all(pm_dir.join("demo")).unwrap();
    std::fs::write(
        pm_dir.join("demo/project.yaml"),
        "key: demo\nprefix: D\ncomponents: []\n",
    )
    .unwrap();
    pm.commit(
        &[pm_dir.join("demo/project.yaml")],
        "project fixture\n\nActor: test\n",
    )
    .unwrap();
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let d = TestDaemon::start_opts(slot_opts(2, 1, 900, &[]));
    let mut author = ManagedWorker::start(&d, "author");
    let mut reviewer_a = ManagedWorker::start(&d, "reviewer-a");
    let mut reviewer_b = ManagedWorker::start(&d, "reviewer-b");
    let mut lead = ManagedWorker::start_role(&d, "lead", "pm");
    for alias in ["author", "reviewer-a", "reviewer-b", "lead"] {
        d.wait_agent(alias, "idle", 25);
    }
    let ok = |r: Value, what: &str| -> Value {
        assert_eq!(r["ok"], true, "{what}: {r}");
        r["result"].clone()
    };
    let refused = |r: &Value, needle: &str, what: &str| {
        assert_eq!(r["ok"], false, "{what}: {r}");
        let msg = r["error"]["message"].as_str().unwrap_or_default();
        assert!(msg.contains(needle), "{what}: want '{needle}' in {r}");
    };
    let memory_path = pm_dir.join("demo/memory/managed-rule.md");
    let read_memory = || std::fs::read(&memory_path).unwrap();

    let proposal = json!({
        "project": "demo",
        "kind": "rule",
        "scope": {"project": true},
        "source": "CAD-381",
        "confidence": "high",
        "text": "\nmanaged endpoints carry daemon-minted identity\n\n**Why:** the enrollment is the authority.\n\n**How to apply:** trust only the daemon's record.\n",
        "id": "managed-rule"
    });
    // A self-asserted identity is refused before any PM write, even
    // from a genuinely enrolled endpoint.
    let r = author.rpc(
        "self",
        "memory_propose",
        json!({"alias": "lead", "inner": proposal.clone()}),
    );
    refused(&r, "connection-bound", "spoofed alias");
    assert!(!memory_path.exists());

    // The provider's tool subprocess (a verified descendant) proposes
    // as the endpoint.
    let proposed = ok(
        author.rpc("child", "memory_propose", proposal.clone()),
        "managed propose",
    );
    assert_eq!(proposed["status"], "proposed", "{proposed}");
    let digest = proposed["digest"].as_str().unwrap().to_string();
    let review = |verdict_evidence: &str| {
        json!({
            "slug": "managed-rule",
            "project": "demo",
            "operation": "accept",
            "verdict": "pass",
            "evidence": verdict_evidence,
            "digest": digest,
        })
    };

    let before = read_memory();
    let r = author.rpc("self", "memory_review", review("self review"));
    refused(&r, "author cannot review", "author reviews own proposal");
    assert_eq!(before, read_memory());

    // Spoofed reviewer identity from a real endpoint: refused.
    let mut spoof = review("spoofed reviewer");
    spoof["reviewer"] = json!("reviewer-b");
    let r = reviewer_a.rpc("self", "memory_review", spoof);
    refused(&r, "connection-bound", "spoofed reviewer");

    // No agent identity: the test process itself, and a managed tool's
    // setsid double-forked grandchild (off the provider's ancestry).
    let err = d
        .operator_rpc("memory_review", review("outside every agent tree"))
        .unwrap_err();
    assert!(err.to_string().contains("has no agent identity"), "{err}");
    let r = reviewer_a.rpc("detached", "memory_review", review("detached"));
    refused(&r, "has no agent identity", "detached grandchild");
    assert_eq!(before, read_memory());

    let a = ok(
        reviewer_a.rpc("self", "memory_review", review("reviewer A checked it")),
        "reviewer A",
    );
    assert_eq!(a["quorum"]["eligible"], false, "{a}");
    let b = ok(
        reviewer_b.rpc("child", "memory_review", review("reviewer B checked it")),
        "reviewer B",
    );
    assert_eq!(b["quorum"]["eligible"], true, "{b}");
    let finalized = ok(
        lead.rpc(
            "self",
            "memory_finalize",
            json!({"slug": "managed-rule", "project": "demo",
                   "operation": "accept", "digest": digest}),
        ),
        "managed pm finalize",
    );
    assert_eq!(finalized["status"], "accepted", "{finalized}");

    let pm = cadence_agent::issue::Pm::at(&pm_dir).unwrap();
    let (_, accepted) = memory::find(&pm, Some("demo"), "managed-rule").unwrap();
    assert!(memory::retrieval_status(&accepted).0);
    let author_proof = accepted.front.author_proof.clone().unwrap();
    assert_eq!(author_proof.alias, "author");
    let agent = d.rpc("agent_show", json!({"alias": "author"})).unwrap()["agent"].clone();
    assert!(
        author_proof
            .generation
            .ends_with(&format!(":{}", author.pid)),
        "the proof carries the enrolled owner generation: {author_proof:?} {agent}"
    );
    assert_eq!(
        accepted
            .front
            .reviews
            .iter()
            .map(|r| r.reviewer.as_str())
            .collect::<Vec<_>>(),
        vec!["reviewer-a", "reviewer-b"]
    );
    assert_eq!(accepted.front.finalizations[0].finalizer.alias, "lead");

    // Owner generation drift revokes the author's enrollment: the old
    // enrollment no longer authenticates — and never falls through to
    // "no agent identity" — even once the row's generation is restored.
    let original = agent["generation"].as_str().map(str::to_string);
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE agents SET generation='regenerated' WHERE alias='author'",
        [],
    )
    .unwrap();
    let second = json!({
        "project": "demo",
        "kind": "gotcha",
        "scope": {"project": true},
        "source": "CAD-381",
        "confidence": "low",
        "text": "stale enrollments vouch for no one\n\n**Why:** drift revokes.\n\n**How to apply:** reopen the endpoint.\n",
        "id": "stale-rule"
    });
    let r = author.rpc("self", "memory_propose", second.clone());
    refused(&r, "is revoked", "drifted enrollment");
    conn.execute(
        "UPDATE agents SET generation=?1 WHERE alias='author'",
        [original],
    )
    .unwrap();
    drop(conn);
    let r = author.rpc("child", "memory_propose", second);
    refused(&r, "is revoked", "revoked enrollment after restore");
    assert!(!pm_dir.join("demo/memory/stale-rule.md").exists());
}

/// CAD-381 review: a managed endpoint's identity outlives the build-slot
/// TTL (an `expired` enrollment still vouches — renewal is only at the
/// next open); and the ambiguous branches fail closed — two agent endpoints on one
/// ancestry, and one pid that is both a pane and an enrolled root.
#[test]
fn memory_managed_identity_survives_ttl_and_refuses_ambiguity() {
    let tmp = TempDir::new().unwrap();
    let pm_dir = tmp.path().join("pm");
    let pm = cadence_agent::issue::Pm::init(&pm_dir).unwrap();
    std::fs::create_dir_all(pm_dir.join("demo")).unwrap();
    std::fs::write(
        pm_dir.join("demo/project.yaml"),
        "key: demo\nprefix: D\ncomponents: []\n",
    )
    .unwrap();
    pm.commit(
        &[pm_dir.join("demo/project.yaml")],
        "project fixture\n\nActor: test\n",
    )
    .unwrap();
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let clock = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1_000));
    let d = TestDaemon::start_opts(slot_opts_clock(
        2,
        1,
        900,
        &[],
        Some(std::sync::Arc::clone(&clock)),
    ));
    let mut wk = ManagedWorker::start(&d, "wk");
    let mut twin = ManagedWorker::start(&d, "twin");
    for alias in ["wk", "twin"] {
        d.wait_agent(alias, "idle", 25);
    }
    let proposal = |id: &str| {
        json!({
            "project": "demo",
            "kind": "rule",
            "scope": {"project": true},
            "source": "CAD-381",
            "confidence": "high",
            "text": "identity is not a build lease\n\n**Why:** the TTL caps slots.\n\n**How to apply:** revalidate per call.\n",
            "id": id
        })
    };
    let refused = |r: &Value, needle: &str, what: &str| {
        assert_eq!(r["ok"], false, "{what}: {r}");
        let msg = r["error"]["message"].as_str().unwrap_or_default();
        assert!(msg.contains(needle), "{what}: want '{needle}' in {r}");
    };

    // Past the enrollment TTL: no new build work, identity intact.
    clock.store(
        1_000 + cadence_agent::slots::ENROLLMENT_TTL_SECS as u64 + 1,
        std::sync::atomic::Ordering::Relaxed,
    );
    let s = wk.rpc("self", "slot_status", json!({}));
    let mine = s["result"]["enrollments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["owner_actor"] == "wk")
        .cloned()
        .unwrap();
    assert_eq!(mine["auth_state"], "expired", "{s}");
    // A freshly enrolled (active) endpoint alongside the expired one:
    // revalidation must still read the expired owner's row, not treat
    // it as gone (review round 3).
    let mut fresh = ManagedWorker::start(&d, "fresh");
    d.wait_agent("fresh", "idle", 25);
    let r = wk.rpc("child", "memory_propose", proposal("after-ttl"));
    assert_eq!(r["ok"], true, "expired enrollment still vouches: {r}");
    assert_eq!(r["result"]["status"], "proposed", "{r}");
    let r = fresh.rpc("self", "memory_propose", proposal("fresh-rule"));
    assert_eq!(r["ok"], true, "active enrollment vouches: {r}");

    // Real drift of the expired owner's row still revokes it.
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE agents SET generation='regenerated' WHERE alias='wk'",
        [],
    )
    .unwrap();
    drop(conn);
    let r = wk.rpc("child", "memory_propose", proposal("drifted-rule"));
    refused(&r, "is revoked", "drifted expired owner");

    // One pid both a registered pane and an enrolled root: ambiguous.
    plant_pane(&d, "twin-pane", twin.pid);
    let r = twin.rpc("self", "memory_propose", proposal("twin-rule"));
    refused(
        &r,
        "both a registered pane and an enrolled endpoint",
        "pane==root",
    );

    // Two agent endpoints on one ancestry (the test process planted as
    // a pane above the provider): ambiguous, never nearest-wins.
    plant_self(&d);
    let r = fresh.rpc("self", "memory_propose", proposal("nested-rule"));
    refused(&r, "caller identity ambiguous", "nested endpoints");
    for id in ["drifted-rule", "twin-rule", "nested-rule"] {
        assert!(!pm_dir.join(format!("demo/memory/{id}.md")).exists());
    }
}

/// CAD-214: email-style flags on `message send` / `send` fail with the
/// real usage line, not clap's `-- --to` value tip.
#[test]
fn send_email_flags_print_real_usage() {
    let state = TempDir::new().unwrap();
    for verb in [&["message", "send"][..], &["send"][..]] {
        let usage = format!("cadence {} <ALIAS> --text <body>", verb.join(" "));
        for flag in ["--to", "--subject", "--body", "--cc"] {
            for tail in [&[flag, "x"][..], &["pm", "--text", "hi", flag, "x"][..]] {
                let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
                    .arg("--state-dir")
                    .arg(state.path())
                    .args(verb)
                    .args(tail)
                    .output()
                    .unwrap();
                let stderr = String::from_utf8_lossy(&out.stderr);
                assert_eq!(out.status.code(), Some(2), "{verb:?} {tail:?}: {stderr}");
                assert!(stderr.contains(&usage), "{verb:?} {tail:?}: {stderr}");
                assert!(
                    stderr.contains(&format!("no `{flag}` flag")),
                    "{verb:?} {tail:?}: {stderr}"
                );
                assert!(!stderr.contains("-- --"), "{verb:?} {tail:?}: {stderr}");
                assert!(stderr.contains("SUBJECT:"), "{verb:?} {tail:?}: {stderr}");
            }
        }
    }
    // Other unknown flags keep clap's own error.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(state.path())
        .args(["send", "pm", "--bogus"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unexpected argument '--bogus'"));
    // `--help` documents the SUBJECT convention.
    for verb in [&["message", "send"][..], &["send"][..]] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .args(verb)
            .arg("--help")
            .output()
            .unwrap();
        assert!(out.status.success());
        let help = String::from_utf8_lossy(&out.stdout);
        assert!(help.contains("SUBJECT: <topic>"), "{verb:?}: {help}");
    }
}

/// CAD-214: `--text` and `-m` carry the body on `message send`, `send`
/// and `issue comment` alike.
#[test]
fn body_flags_match_on_send_and_comment() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    let sends: [(&[&str], &str, &str); 4] = [
        (&["message", "send"], "--text", "b-ms-text"),
        (&["message", "send"], "-m", "b-ms-m"),
        (&["send"], "--text", "b-s-text"),
        (&["send"], "-m", "b-s-m"),
    ];
    for (verb, flag, id) in sends {
        let mut args = verb.to_vec();
        args.extend(["w1", flag, id, "--message", id]);
        let out = launch_cli(&d, &args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let m = d.wait_message("w1", id, &["completed"], 15);
        assert_eq!(m["body"], id, "{args:?}");
    }

    let pm_dir = d.dir.path().join("pm");
    let home = d.dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence")).parent().unwrap();
    let issue = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&d.state)
            .args(args)
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    issue(&["issue", "init"]);
    issue(&["issue", "project", "add", "demo", "--prefix", "D"]);
    issue(&["issue", "new", "One", "--project", "demo"]);
    issue(&["issue", "comment", "D-1", "-m", "comment-via-m"]);
    issue(&["issue", "comment", "D-1", "--text", "comment-via-text"]);
    let show = issue(&["issue", "show", "D-1"]);
    for want in ["comment-via-m", "comment-via-text"] {
        assert!(show.contains(want), "{want}: {show}");
    }
}

/// `cadence --state-dir <state> secret scan <args>` with `stdin` piped in.
fn cad109_scan(state: &Path, args: &[&str], stdin: &str) -> (i32, Value, String) {
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(state)
        .args(["secret", "scan"])
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let v = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("not json: {stdout} / {stderr}"));
    (
        out.status.code().unwrap_or(-1),
        v,
        format!("{stdout}{stderr}"),
    )
}

/// Exit 0 when clean or warn-only, 1 on a blocking finding. A bare token
/// alone on its line is found with its rule, line and a redacted prefix, and
/// the value appears nowhere in the output. The operator allowlist in the
/// state dir drops what it names.
#[test]
fn secret_scan_exit_codes_and_redaction() {
    let tmp = TempDir::new().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();

    let (code, v, _) = cad109_scan(&state, &[], "Ran the focused tests; all green.\n");
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["findings"], json!([]));

    let warn = format!(
        "config:\n  api_key = \"{}\"\n",
        cad109_token("", "warn", 24)
    );
    let (code, v, _) = cad109_scan(&state, &[], &warn);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["findings"][0]["rule"], "generic-api-key");
    assert_eq!(v["findings"][0]["severity"], "warn");

    let ant = ["sk", "-ant-", "api03-"].concat();
    let pat = ["github", "_pat_"].concat();
    for (prefix, rule) in [
        ("figd_", "cadence-figma-token"),
        (ant.as_str(), "cadence-anthropic-key"),
        (pat.as_str(), "cadence-github-fine-grained-pat"),
    ] {
        let tok = cad109_token(prefix, rule, 64);
        let (code, v, all) = cad109_scan(&state, &[], &format!("Result\n\n{tok}\n"));
        assert_eq!(code, 1, "{v}");
        let f = &v["findings"][0];
        assert_eq!(f["rule"], rule, "{v}");
        assert_eq!(
            (f["line"].as_u64(), f["column"].as_u64()),
            (Some(3), Some(1))
        );
        assert_eq!(f["severity"], "block");
        assert!(f["redacted"].as_str().unwrap().ends_with('…'), "{v}");
        assert!(!all.contains(&tok[prefix.len()..]), "{all}");
    }

    let tok = cad109_token("figd_", "file", 40);
    let file = tmp.path().join("pr-body.md");
    std::fs::write(&file, format!("# Summary\n\ntoken {tok}\n")).unwrap();
    let (code, v, all) = cad109_scan(&state, &["--file", file.to_str().unwrap()], "");
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["blocking"], 1);
    assert!(!all.contains(&tok[5..]), "{all}");

    std::fs::write(
        state.join("secret-allowlist.toml"),
        "[[allow]]\nrule = \"cadence-figma-token\"\nreason = \"test fixture\"\n",
    )
    .unwrap();
    let (code, v, _) = cad109_scan(&state, &["--file", file.to_str().unwrap()], "");
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["allowlisted"], 1);

    std::fs::write(state.join("secret-allowlist.toml"), "not toml [").unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&state)
        .args(["secret", "scan", "--file", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Refusing to write"));
}
