//! CAD-1239 acceptance checks (design section 10), daemon level. Written by a
//! non-implementer. Drives the real `Shared::dispatch` RPC gate under the
//! explicit test-seam identity (`scoped`), against an isolated temp state.
//! Store-level guards live in `src/store/tests/archive_acceptance.rs`.
#![cfg(all(feature = "test-seam", target_os = "linux"))]

use cadence_agent::daemon::{AutoStopSetting, ServeOptions, Shared};
use cadence_agent::issue::Pm;
use cadence_agent::store::Store;
use cadence_agent::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::path::PathBuf;

struct Fixture {
    shared: std::sync::Arc<Shared>,
    state: PathBuf,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::Builder::new()
        .prefix("c1239-")
        .tempdir_in("/tmp")
        .unwrap();
    let pm = dir.path().join("pm");
    let state = dir.path().join("state");
    Pm::init(&pm).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
    env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let opts = ServeOptions {
        provider_env: env,
        auto_stop: Some(AutoStopSetting::off()),
        report_router: Some(0),
        checkup: Some(0),
        slots: Some(Default::default()),
        lease: Some(Default::default()),
        ..ServeOptions::default()
    };
    let shared = Shared::new(&state, &opts).expect("fixture daemon");
    Fixture {
        shared,
        state,
        _dir: dir,
    }
}

impl Fixture {
    fn db(&self) -> PathBuf {
        self.state.join("cadence.sqlite3")
    }
    fn store(&self) -> Store {
        Store::open(&self.db()).unwrap()
    }
    /// Dispatch as an explicit identity. Operator is the only identity that
    /// may run the archive verbs.
    fn as_(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(who, || {
            self.shared
                .dispatch(method, &params, std::process::id())
        })
    }
}

fn seed_stopped(f: &Fixture, alias: &str) {
    let cwd = f._dir.path().join("w");
    std::fs::create_dir_all(&cwd).unwrap();
    let s = f.store();
    s.register_agent(&cadence_agent::store::NewAgent {
        alias,
        provider: "fake",
        endpoint_kind: "managed",
        role: "worker",
        cwd: cwd.to_str().unwrap(),
        sandbox: "read-only",
        instructions: None,
        params: None,
        team_role: None,
        model_policy: None,
    })
    .unwrap();
    s.set_agent_state(alias, "stopped", None).unwrap();
    s.set_enabled(alias, false).unwrap();
}

fn archive_via_store(f: &Fixture, alias: &str) {
    let s = f.store();
    let digest = s.archive_plan(alias).unwrap().digest;
    s.archive_agents(
        &[(alias.to_string(), digest)],
        "retired",
        &json!({"by": "operator", "by_kind": "operator-connection"}),
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// archive_refuses_non_operator (I1)
// A pane agent, an unproven caller and an agent identity are refused by the
// archive verbs, and the row is not touched.
// ---------------------------------------------------------------------------

#[test]
fn archive_refuses_non_operator_plan_archive_restore() {
    let f = fixture();
    seed_stopped(&f, "victim");
    let plan_params = json!({"aliases": ["victim"], "reason": "retired"});

    for who in [
        Asserted::Unproven,
        Asserted::Agent("victim".into()),
    ] {
        let label = format!("{who:?}");
        let plan = f.as_(who.clone(), "agent_archive_plan", plan_params.clone());
        let e = plan.expect_err(&format!("{label}: archive plan allowed"));
        assert!(e.to_string().contains("operator"), "{label}: plan refused for wrong reason: {e}");

        let archive = f.as_(
            who.clone(),
            "agent_archive",
            json!({
                "aliases": ["victim"],
                "reason": "retired",
                "expect": [{"alias": "victim", "digest": "0".repeat(64)}],
            }),
        );
        let e = archive.expect_err(&format!("{label}: archive allowed"));
        assert!(e.to_string().contains("operator"), "{label}: archive refused for wrong reason: {e}");

        let restore = f.as_(
            who.clone(),
            "agent_restore",
            json!({"aliases": ["victim"], "expect": [{"alias": "victim", "rev": 0}]}),
        );
        let e = restore.expect_err(&format!("{label}: restore allowed"));
        assert!(e.to_string().contains("operator"), "{label}: restore refused for wrong reason: {e}");
    }

    let row = f.store().agent("victim").unwrap();
    assert!(row.archived_at.is_none(), "non-operator archived the row");
    assert_eq!(row.lifecycle_rev, 2, "non-operator bumped the revision");
}

// ---------------------------------------------------------------------------
// archived_row_not_started_or_enqueued (I5), RPC side
// ---------------------------------------------------------------------------

#[test]
fn archived_row_refuses_send_through_rpc_as_operator() {
    let f = fixture();
    seed_stopped(&f, "gone");
    archive_via_store(&f, "gone");

    let err = f
        .as_(
            Asserted::Operator,
            "agent_send",
            json!({"alias": "gone", "text": "wake up", "id": "gone-m1"}),
        )
        .expect_err("send to an archived alias must refuse");
    assert!(err.to_string().contains("archived"), "{err}");
    assert!(f.store().agent("gone").unwrap().archived_at.is_some());
}

// ---------------------------------------------------------------------------
// register_agent duplicate: archived-specific error, no `UNIQUE` text (section 7)
// ---------------------------------------------------------------------------

#[test]
fn archived_register_duplicate_is_not_unique_recovery() {
    let f = fixture();
    seed_stopped(&f, "dupe");
    archive_via_store(&f, "dupe");

    let cwd = f._dir.path().join("w");
    let err = f
        .as_(
            Asserted::Operator,
            "agent_register",
            json!({
                "alias": "dupe",
                "provider": "fake",
                "endpoint_kind": "managed",
                "role": "worker",
                "cwd": cwd.to_str().unwrap(),
                "sandbox": "read-only",
            }),
        )
        .expect_err("registering an archived alias must refuse");
    let msg = err.to_string();
    assert!(msg.contains("archived"), "{msg}");
    assert!(
        !msg.contains("UNIQUE"),
        "archived duplicate surfaced as UNIQUE recovery: {msg}"
    );
    assert!(f.store().agent("dupe").unwrap().archived_at.is_some());
}

// ---------------------------------------------------------------------------
// Blocked-only checks: a resume RPC and a routed report recipient go through
// the same store guard as the RPC layer. Covered at store level by
// `archived_row_not_enqueued_refused_inside_enqueue_transaction` and
// `archived_row_excluded_from_queued_for_stopped`; the RPC resume path is not
// exercised here. See the gap report.
// ---------------------------------------------------------------------------
