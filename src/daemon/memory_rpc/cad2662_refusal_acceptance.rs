//! CAD-2662 independent refusal acceptance. Authored from the ticket, not the
//! implementation, by a session other than the implementer: a caller cannot
//! widen `memory_match` to another project or role through arguments, and a
//! request that names another project is refused. The project and identity
//! come from the daemon-derived caller and its own running turn, never from
//! request fields. A refused lookup supplies nothing — no other project's
//! lesson text — and records no supply receipt.
//!
//! The caller is an asserted native worker whose running turn is bound to
//! issue A-1 in project `alpha`; project `beta` holds a project-wide lesson
//! that must never reach it. Every call goes through `Shared::dispatch`.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::daemon::{ServeOptions, Shared};
use crate::store::{NewAgent, Sender, Steer, Take};
use crate::test_seam::{scoped, Asserted};

/// `beta`'s lesson body and slug (`supply::fixture::two_projects`).
const BETA_BODY: &str = "BETA_WIDE_FACT";
const BETA_SLUG: &str = "beta-wide";
const RECEIPT: &str = "lessons_supplied";

struct Fx {
    _dir: tempfile::TempDir,
    shared: Arc<Shared>,
}

fn files_under(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().is_some_and(|name| name == ".git") {
            continue;
        }
        if path.is_dir() {
            files_under(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Register `alias` as a managed worker holding one running turn bound to
/// `issue`. Returns that turn's message id and token.
fn worker(
    shared: &Shared,
    cwd: &str,
    alias: &str,
    generation: &str,
    issue: &str,
) -> (String, String) {
    shared
        .store
        .register_agent(&NewAgent {
            alias,
            provider: "claude",
            endpoint_kind: "managed",
            role: "worker",
            cwd,
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    shared
        .store
        .set_identity(
            alias,
            &crate::adapter::Identity {
                thread_id: format!("{alias}-thread"),
                session_id: format!("{alias}-session"),
                model: None,
                effort: None,
                pid: std::process::id(),
                endpoint: None,
                generation: Some(generation.into()),
                attach: None,
            },
        )
        .unwrap();
    shared
        .store
        .enqueue_steered(
            alias,
            "kickoff",
            None,
            &format!("m-{alias}"),
            "test",
            None,
            Some(issue),
            None,
            &Sender::Unattributed,
            &Steer::NONE,
            None,
            None,
            None,
        )
        .unwrap();
    let Take::Message(taken) = shared.store.take_queued(alias).unwrap() else {
        panic!("{alias} must claim its queued turn");
    };
    let token = crate::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint(generation);
    shared.store.mark_running(&taken.id, &token).unwrap();
    (taken.id.clone(), token)
}

/// `w1` holds a turn on alpha's A-1. `w2` holds a turn on beta's B-1; its
/// message id and token are returned so `w1` can try to borrow them.
fn fx() -> (Fx, (String, String)) {
    let dir = tempfile::Builder::new().prefix("c2662r").tempdir().unwrap();
    let pm_dir = dir.path().join("pm");
    let pm = crate::issue::Pm::init(&pm_dir).unwrap();
    crate::memory::supply::fixture::two_projects(&pm_dir);
    for (key, id) in [("alpha", "A-1"), ("beta", "B-1")] {
        let issue_dir = pm_dir.join(key).join(id);
        std::fs::create_dir_all(&issue_dir).unwrap();
        let front = crate::issue::model::Front::new(id, "t", "2026-11-22T00:00:00Z");
        std::fs::write(
            issue_dir.join("issue.md"),
            format!(
                "---\n{}---\n\nbody\n",
                serde_yaml::to_string(&front).unwrap()
            ),
        )
        .unwrap();
    }
    let mut files = Vec::new();
    files_under(&pm_dir, &mut files);
    pm.commit(&files, "CAD-2662 refusal fixture\n\nActor: test\n")
        .unwrap();
    let opts = ServeOptions {
        provider_env: crate::adapter::ProviderEnv::refusing_providers(),
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    opts.provider_env
        .set("CADENCE_HOME", dir.path().to_str().unwrap());
    let shared = Shared::new(dir.path(), &opts).unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    worker(&shared, &cwd, "w1", "g1", "A-1");
    let beta_turn = worker(&shared, &cwd, "w2", "g2", "B-1");
    (Fx { _dir: dir, shared }, beta_turn)
}

/// `memory_match` as the alpha-bound worker `w1`.
fn lookup(fx: &Fx, params: Value) -> crate::error::Result<Value> {
    scoped(Asserted::Agent("w1".into()), || {
        fx.shared
            .dispatch("memory_match", &params, std::process::id())
    })
}

/// The refusal text of a lookup that must be refused.
fn refused(fx: &Fx, params: Value) -> String {
    match lookup(fx, params.clone()) {
        Err(error) => error.to_string(),
        Ok(answer) => panic!("lookup {params} must be refused, got {answer}"),
    }
}

fn receipt(fx: &Fx, alias: &str) -> Option<Value> {
    fx.shared
        .store
        .last_event_of(alias, &[RECEIPT])
        .unwrap()
        .map(|event| event.payload)
}

fn assert_no_beta(text: &str, what: &str) {
    assert!(
        !text.contains(BETA_BODY),
        "{what} leaked beta's lesson: {text}"
    );
    assert!(
        !text.contains(BETA_SLUG),
        "{what} named beta's lesson: {text}"
    );
}

#[test]
fn cad2662_refusal_acceptance_arguments_cannot_widen_project_or_role() {
    let (fx, (beta_message, beta_token)) = fx();

    // 1. Naming another project is refused, alone or beside valid axes, and
    //    the refusal carries none of that project's lessons.
    for params in [
        json!({"project": "beta"}),
        json!({"project": "beta", "paths": ["src/x.rs"], "tags": ["t"]}),
    ] {
        let error = refused(&fx, params);
        assert!(
            error.contains("project"),
            "the refusal names the field: {error}"
        );
        assert_no_beta(&error, "the refusal");
    }

    // 2. No other argument picks the role, provider, identity or issue:
    //    each is refused, never honoured as a wider selection.
    for (field, value) in [
        ("role", json!("pm")),
        ("roles", json!(["pm", "qa"])),
        ("team_role", json!("pm")),
        ("provider", json!("codex")),
        ("alias", json!("w2")),
        ("issue", json!("B-1")),
    ] {
        let error = refused(&fx, json!({field: value}));
        assert_no_beta(&error, field);
    }

    // 3. Another agent's beta-bound turn cannot be borrowed by passing its
    //    message id and token.
    let error = refused(&fx, json!({"message": beta_message, "token": beta_token}));
    assert_no_beta(&error, "the borrowed-turn refusal");

    // 4. A refused lookup records no supply receipt, for the caller or the
    //    agent whose turn it tried to borrow.
    assert!(
        receipt(&fx, "w1").is_none(),
        "a refused lookup wrote a receipt"
    );
    assert!(
        receipt(&fx, "w2").is_none(),
        "a refused lookup wrote w2 a receipt"
    );

    // 5. The same caller with no widening argument is answered from its own
    //    turn's project, and that answer records a receipt — so the
    //    refusals above are the guard, not a broken fixture.
    let own = lookup(&fx, json!({})).expect("the caller's own lookup is answered");
    assert_eq!(own["project"], "alpha", "{own}");
    assert_no_beta(&own.to_string(), "the caller's own answer");
    let recorded = receipt(&fx, "w1").expect("an answered lookup records a receipt");
    assert_eq!(recorded["project"], "alpha", "{recorded}");
    assert_no_beta(&recorded.to_string(), "the receipt");

    // 6. The extra axes the tool does accept only select within the turn's
    //    project: aiming them at beta never reaches beta's lessons.
    for params in [
        json!({"paths": ["../beta/**"]}),
        json!({"paths": ["beta/**"], "components": ["beta"], "tags": ["beta"]}),
    ] {
        match lookup(&fx, params.clone()) {
            Err(error) => assert_no_beta(&error.to_string(), "an axis refusal"),
            Ok(answer) => {
                assert_eq!(answer["project"], "alpha", "{params}: {answer}");
                let supplied = answer["supplied"].to_string();
                assert!(!supplied.contains(BETA_SLUG), "{params}: {answer}");
                assert!(
                    !answer["lessons"].to_string().contains(BETA_BODY),
                    "{params}: {answer}"
                );
            }
        }
    }
    let last = receipt(&fx, "w1").expect("the receipt is still w1's");
    assert_eq!(last["project"], "alpha", "{last}");
    assert!(
        receipt(&fx, "w2").is_none(),
        "w1's lookups never write w2 a receipt"
    );
}
