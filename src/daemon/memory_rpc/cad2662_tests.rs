//! CAD-2662: `memory_match` and an agent's `memory_propose` through the
//! daemon's own dispatch, as an asserted native worker whose running turn is
//! bound to issue A-1 in project `alpha`. Lessons are synthetic fixtures.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::daemon::{ServeOptions, Shared};
use crate::store::{NewAgent, Sender, Steer, Take};
use crate::test_seam::{scoped, Asserted};

struct Fx {
    _dir: tempfile::TempDir,
    pm_dir: std::path::PathBuf,
    shared: Arc<Shared>,
    message: String,
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

/// A worker `w1` holding one running turn. `issue` is the turn's bound
/// issue; `None` leaves the turn with no issue scope.
fn fx(issue: Option<&str>) -> Fx {
    let dir = tempfile::tempdir().unwrap();
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
    pm.commit(&files, "CAD-2662 fixture\n\nActor: test\n")
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
    shared
        .store
        .register_agent(&NewAgent {
            alias: "w1",
            provider: "claude",
            endpoint_kind: "managed",
            role: "worker",
            cwd: &cwd,
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
            "w1",
            &crate::adapter::Identity {
                thread_id: "fixture-thread".into(),
                session_id: "fixture-session".into(),
                model: None,
                effort: None,
                pid: std::process::id(),
                endpoint: None,
                generation: Some("g1".into()),
                attach: None,
            },
        )
        .unwrap();
    shared
        .store
        .enqueue_steered(
            "w1",
            "kickoff",
            None,
            "m-cad2662",
            "test",
            None,
            issue,
            None,
            &Sender::Unattributed,
            &Steer::NONE,
            None,
            None,
            None,
        )
        .unwrap();
    let Take::Message(taken) = shared.store.take_queued("w1").unwrap() else {
        panic!("w1 must claim its queued turn");
    };
    let token = crate::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint("g1");
    shared.store.mark_running(&taken.id, &token).unwrap();
    Fx {
        _dir: dir,
        pm_dir,
        shared,
        message: taken.id.clone(),
    }
}

fn call(fx: &Fx, method: &str, params: Value) -> crate::error::Result<Value> {
    scoped(Asserted::Agent("w1".into()), || {
        fx.shared.dispatch(method, &params, std::process::id())
    })
}

fn supplied(response: &Value) -> Vec<String> {
    let mut slugs: Vec<String> = response["supplied"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["slug"].as_str().unwrap().to_owned())
        .collect();
    slugs.sort_unstable();
    slugs
}

fn receipts(fx: &Fx) -> Option<crate::store::Event> {
    fx.shared
        .store
        .last_event_of("w1", &[crate::memory::supply::SUPPLY_EVENT])
        .unwrap()
}

#[test]
fn cad2662_match_supplies_the_turn_projects_lessons_and_records_a_receipt() {
    let fx = fx(Some("A-1"));
    let response = call(&fx, "memory_match", json!({})).unwrap();
    assert_eq!(response["project"], "alpha", "{response}");
    assert_eq!(response["issue"], "A-1");
    assert_eq!(supplied(&response), vec!["alpha-wide"], "{response}");
    assert_eq!(
        response["withheld"],
        json!([{"slug": "alpha-stale", "reason": "evidence marked stale: CAD-9 reverted the fix"}])
    );
    let text = response.to_string();
    assert!(!text.contains("ALPHA_STALE_BODY"), "{text}");
    assert!(!text.contains("beta"), "{text}");
    assert_eq!(response["receipt"], crate::memory::supply::SUPPLY_EVENT);

    let receipt = receipts(&fx).expect("a lookup records its receipt").payload;
    assert_eq!(receipt["message"], fx.message.as_str(), "{receipt}");
    assert_eq!(receipt["project"], "alpha");
    assert_eq!(receipt["issue"], "A-1");
    assert_eq!(receipt["provider"], "claude");
    assert_eq!(receipt["supplied"], response["supplied"]);
    assert_eq!(receipt["withheld"], response["withheld"]);
    // Not the dispatch `context_supplied` row, which drives restore.
    assert!(fx
        .shared
        .store
        .last_event_of("w1", &[crate::context_pack::SUPPLY_EVENT])
        .unwrap()
        .is_none());

    // A scope change widens the selection inside the same project.
    let widened = call(
        &fx,
        "memory_match",
        json!({"paths": ["src/adapter/codex.rs"]}),
    )
    .unwrap();
    assert_eq!(supplied(&widened), vec!["alpha-adapter", "alpha-wide"]);
}

#[test]
fn cad2662_match_refuses_a_named_project_role_provider_or_alias() {
    let fx = fx(Some("A-1"));
    for (field, value) in [
        ("project", "beta"),
        ("role", "pm"),
        ("provider", "codex"),
        ("alias", "w2"),
        ("issue", "B-1"),
    ] {
        let error = call(&fx, "memory_match", json!({field: value})).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("memory request field '{field}' is not accepted"),
            "{field}"
        );
    }
    assert!(receipts(&fx).is_none(), "a refused lookup records nothing");
    // A path outside the repository is refused before any selection.
    assert!(call(&fx, "memory_match", json!({"paths": ["../beta/x"]})).is_err());
    assert!(receipts(&fx).is_none());
}

#[test]
fn cad2662_match_needs_an_issue_bound_turn_and_an_agent_caller() {
    let fx = fx(None);
    let error = call(&fx, "memory_match", json!({})).unwrap_err();
    assert_eq!(
        error.to_string(),
        "lesson lookup message has no trusted issue scope"
    );
    let operator = scoped(Asserted::Operator, || {
        fx.shared
            .dispatch("memory_match", &json!({}), std::process::id())
    })
    .unwrap_err();
    assert_eq!(
        operator.to_string(),
        "lesson lookup requires the assigned native agent"
    );
    assert!(receipts(&fx).is_none());
}

#[test]
fn cad2662_agent_propose_without_a_project_files_into_the_turn_project() {
    let fx = fx(Some("A-1"));
    let request = |project: Option<&str>, id: &str, components: &[&str]| {
        let scope = crate::memory::Scope {
            components: components.iter().map(|c| (*c).to_string()).collect(),
            paths: if components.is_empty() {
                vec!["src/adapter/**".into()]
            } else {
                vec![]
            },
            ..crate::memory::Scope::default()
        };
        crate::memory::cli::propose_params(&crate::memory::cli::Proposal {
            project,
            kind: "gotcha",
            scope: &scope,
            source: Some("CAD-2662"),
            confidence: None,
            from: None,
            text: Some("Lookups record a receipt\n\n**Why:** w\n\n**How to apply:** h\n"),
            id: Some(id),
            nature: "judgement",
            audience: "project",
            roles: &[],
            evidence_refs: &[],
            verification_condition: None,
        })
    };
    let derived = call(&fx, "memory_propose", request(None, "derived-one", &[])).unwrap();
    let named = call(
        &fx,
        "memory_propose",
        request(Some("alpha"), "named-one", &[]),
    )
    .unwrap();
    assert!(
        fx.pm_dir.join("alpha/memory/derived-one.md").is_file(),
        "{derived}"
    );
    assert!(
        fx.pm_dir.join("alpha/memory/named-one.md").is_file(),
        "{named}"
    );
    assert!(!fx.pm_dir.join("beta/memory/derived-one.md").exists());
    // Same path, same answer shape.
    let keys = |value: &Value| {
        let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort_unstable();
        keys
    };
    assert_eq!(keys(&derived), keys(&named));
    let text = std::fs::read_to_string(fx.pm_dir.join("alpha/memory/derived-one.md")).unwrap();
    assert!(text.contains("status: proposed"), "{text}");

    // A proposal retrieval validation refuses (a whitespace component is no
    // retrieval axis) is refused the same way, named or derived.
    let derived = call(&fx, "memory_propose", request(None, "bad-one", &[" "])).unwrap_err();
    let named = call(
        &fx,
        "memory_propose",
        request(Some("alpha"), "bad-two", &[" "]),
    )
    .unwrap_err();
    assert!(
        derived.to_string().contains("scope has no retrieval axis"),
        "{derived}"
    );
    assert_eq!(
        derived.to_string().replace("bad-one", "<id>"),
        named.to_string().replace("bad-two", "<id>")
    );
}
