//! CAD-2323: two PM-run reads that CAD-2215's scoped agent filter left
//! empty. A group root's `join` polls its new member with
//! `briefing::launch_poll_read` and briefs it only once
//! `briefing::endpoint_opened` holds; the member's briefing lists its group
//! from `briefing::roster_read` through `briefing::group_roster`. Both must
//! work for the agent itself and its group root, and give a stranger or a
//! relayed reader nothing more than before.

use crate::daemon::Shared;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::sync::Arc;

struct World {
    _dir: tempfile::TempDir,
    shared: Arc<Shared>,
}

impl World {
    /// Group root `root` with members `w1` and `w2`; a second group root
    /// `other` with its member `ow`.
    fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("c2323").tempdir().unwrap();
        let pm = dir.path().join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let opts = crate::daemon::ServeOptions {
            test_seam: true,
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let shared = Shared::new(dir.path(), &opts).unwrap();
        let cwd = dir.path().to_str().unwrap().to_string();
        for root in ["root", "other"] {
            shared
                .store
                .register_agent(&NewAgent {
                    alias: root,
                    provider: "fake",
                    endpoint_kind: "fake",
                    role: "pm",
                    cwd: &cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params: None,
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
        }
        let world = Self { _dir: dir, shared };
        for (member, root) in [("w1", "root"), ("w2", "root"), ("ow", "other")] {
            world
                .call(
                    Asserted::Agent(root.into()),
                    "agent_register",
                    json!({
                        "alias": member, "provider": "fake", "endpoint_kind": "fake",
                        "cwd": cwd, "params": json!({"upstream": root}).to_string(),
                    }),
                )
                .expect("a group root registers its member");
        }
        world
    }

    fn call(&self, who: Asserted, method: &str, params: Value) -> crate::error::Result<Value> {
        scoped(who, || {
            self.shared.dispatch(method, &params, std::process::id())
        })
    }

    /// The endpoint opens, as the adapter records it: `state` becomes idle.
    fn open_endpoint(&self, alias: &str) {
        self.shared
            .store
            .set_identity(
                alias,
                &crate::adapter::Identity {
                    thread_id: format!("thread-{alias}"),
                    session_id: format!("session-{alias}"),
                    model: None,
                    effort: None,
                    pid: 4242,
                    endpoint: Some(format!("native://{alias}")),
                    generation: Some("g1".to_string()),
                    attach: None,
                },
            )
            .unwrap();
    }

    fn roster(&self, who: Asserted) -> String {
        let list = self
            .call(who, "agent_list", crate::briefing::roster_read())
            .expect("the roster read succeeds");
        crate::briefing::group_roster(&list, "root")
    }
}

fn relayed(mut params: Value) -> Value {
    params["read_as"] = json!("agents/root");
    params
}

#[test]
fn a_pm_run_join_sees_its_members_endpoint_open() {
    let world = World::new();
    world.open_endpoint("w1");
    for reader in ["root", "w1"] {
        let got = world
            .call(
                Asserted::Agent(reader.into()),
                "agent_show",
                crate::briefing::launch_poll_read("w1"),
            )
            .expect("the join poll reads the member");
        let agent = &got["agent"];
        assert_eq!(agent["endpoint"], "native://w1", "{reader}: {got}");
        assert_eq!(agent["state"], "idle", "{reader}: {got}");
        for attachable in [false, true] {
            assert!(
                crate::briefing::endpoint_opened(agent, attachable),
                "{reader}: the join must brief once the endpoint opens: {got}"
            );
        }
        // The poll's flag carries the launch state only.
        for field in ["params", "cwd", "instructions"] {
            assert!(
                agent.get(field).is_none_or(Value::is_null),
                "{reader}: the poll read got `{field}`: {got}"
            );
        }
        // And the briefing read's flags do not carry the launch state.
        let briefing = world
            .call(
                Asserted::Agent(reader.into()),
                "agent_show",
                crate::briefing::briefing_read("w1"),
            )
            .expect("the briefing read succeeds");
        assert!(
            briefing["agent"].get("endpoint").is_none(),
            "{reader}: {briefing}"
        );
        assert!(briefing["agent"]["state"].is_null(), "{reader}: {briefing}");
    }

    // Refused: a stranger agent, a peer in the same group, and a relayed
    // reader naming the group root all see today's stripped row.
    for (case, who, params) in [
        (
            "stranger",
            Asserted::Agent("other".into()),
            crate::briefing::launch_poll_read("w1"),
        ),
        (
            "peer",
            Asserted::Agent("w2".into()),
            crate::briefing::launch_poll_read("w1"),
        ),
        (
            "read_as agents/root",
            Asserted::Operator,
            relayed(crate::briefing::launch_poll_read("w1")),
        ),
    ] {
        let got = world
            .call(who, "agent_show", params)
            .unwrap_or_else(|e| panic!("{case}: the read itself succeeds: {e}"));
        let agent = &got["agent"];
        assert_eq!(agent["alias"], "w1", "{case}: {got}");
        assert!(agent.get("endpoint").is_none(), "{case}: {got}");
        assert!(agent["state"].is_null(), "{case}: {got}");
        assert!(!got.to_string().contains("native://w1"), "{case}: {got}");
    }
}

#[test]
fn a_pm_run_briefing_roster_lists_the_group() {
    let world = World::new();
    let lines = |roster: &str| roster.lines().map(str::to_string).collect::<Vec<_>>();

    let by_root = world.roster(Asserted::Agent("root".into()));
    let by_root = lines(&by_root);
    assert_eq!(by_root.len(), 3, "{by_root:?}");
    let listed = |lines: &[String], line: &str| lines.iter().any(|l| l.starts_with(line));
    assert!(listed(&by_root, "- `root` — group root"), "{by_root:?}");
    for member in ["w1", "w2"] {
        assert!(
            listed(&by_root, &format!("- `{member}` — worker")),
            "{member} missing from the group root's roster: {by_root:?}"
        );
    }

    // A member reads its own row, not its peer's.
    let by_member = world.roster(Asserted::Agent("w1".into()));
    let by_member = lines(&by_member);
    assert_eq!(by_member.len(), 2, "{by_member:?}");
    assert!(listed(&by_member, "- `w1` — worker"), "{by_member:?}");

    // Refused: a stranger and a relayed reader naming the group root learn
    // no one's upstream.
    for (case, who, params) in [
        (
            "stranger",
            Asserted::Agent("other".into()),
            crate::briefing::roster_read(),
        ),
        (
            "read_as agents/root",
            Asserted::Operator,
            relayed(crate::briefing::roster_read()),
        ),
    ] {
        let list = world
            .call(who, "agent_list", params)
            .unwrap_or_else(|e| panic!("{case}: the list itself succeeds: {e}"));
        let roster = crate::briefing::group_roster(&list, "root");
        assert_eq!(lines(&roster).len(), 1, "{case}: {roster}");
        for row in list["agents"].as_array().unwrap() {
            if ["root", "w1", "w2"].contains(&row["alias"].as_str().unwrap_or_default()) {
                assert!(row.get("params").is_none(), "{case}: {row}");
            }
        }
    }
}
