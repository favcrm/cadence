//! CAD-2323 independent refusal acceptance. Authored from the ticket, not the
//! implementation; the implementer must not edit or weaken it (AGENTS.md
//! "Gates and security work").
//!
//! The rule: two optional flags on a scoped read keep fields the scoped
//! filter strips, and only for the agent itself or its group root on its own
//! connection:
//!
//! * `agent_show {include_launch_state}` keeps the row's `state` and
//!   `endpoint`, and nothing else;
//! * `agent_list {include_group}` keeps an admitted row's `params`, and
//!   nothing else.
//!
//! Refused (the answer equals today's answer to the same read without the
//! flag): a stranger agent from another group, a peer worker in the same
//! group, and a relayed reader (`read_as`) on an operator connection, even
//! one naming the group root or the member itself. The positive control
//! sends the same reads from the group root, so the refusals are not vacuous.

use crate::daemon::Shared;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::sync::Arc;

const MEMBER: &str = "w1";
const ENDPOINT: &str = "native://zq-cad2323-w1-endpoint";
const JOB: &str = "zq-cad2323-stored-job: reconcile the vendor ledger";
const LANE: &str = "lane-cad2323-owner-cwd";
const UPSTREAM: &str = r#""upstream":"root""#;

struct World {
    _dir: tempfile::TempDir,
    shared: Arc<Shared>,
}

impl World {
    /// Group root `root` with members `w1` (open endpoint, stored job,
    /// distinctive cwd) and `w2`; a second group root `other` with `ow`.
    fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("c2323r").tempdir().unwrap();
        let pm = dir.path().join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let opts = crate::daemon::ServeOptions {
            test_seam: true,
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let shared = Shared::new(dir.path(), &opts).unwrap();
        let base = dir.path().to_str().unwrap().to_string();
        let lane = dir.path().join(LANE);
        std::fs::create_dir_all(&lane).unwrap();
        let lane = lane.to_str().unwrap().to_string();
        let rows: [(&str, &str, Option<&str>, Option<&str>); 5] = [
            ("root", base.as_str(), None, None),
            ("other", base.as_str(), None, None),
            (
                MEMBER,
                lane.as_str(),
                Some(JOB),
                Some(r#"{"upstream":"root"}"#),
            ),
            ("w2", base.as_str(), None, Some(r#"{"upstream":"root"}"#)),
            ("ow", base.as_str(), None, Some(r#"{"upstream":"other"}"#)),
        ];
        for (alias, cwd, instructions, params) in rows {
            shared
                .store
                .register_agent(&NewAgent {
                    alias,
                    provider: "fake",
                    endpoint_kind: "fake",
                    role: if params.is_none() { "pm" } else { "worker" },
                    cwd,
                    sandbox: "read-only",
                    instructions,
                    params,
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
        }
        shared
            .store
            .set_identity(
                MEMBER,
                &crate::adapter::Identity {
                    thread_id: "thread-cad2323".to_string(),
                    session_id: "session-cad2323".to_string(),
                    model: None,
                    effort: None,
                    pid: 4242,
                    endpoint: Some(ENDPOINT.to_string()),
                    generation: Some("g1".to_string()),
                    attach: None,
                },
            )
            .unwrap();
        Self { _dir: dir, shared }
    }

    fn call(&self, who: &Asserted, method: &str, params: Value) -> Value {
        scoped(who.clone(), || {
            self.shared.dispatch(method, &params, std::process::id())
        })
        .unwrap_or_else(|e| panic!("{method} {params}: the read itself must succeed: {e}"))
    }

    /// `agent_show` of the member, with and without the flag.
    fn show_pair(&self, who: &Asserted, extra: &Value) -> (Value, Value) {
        let mut base = json!({"alias": MEMBER, "limit": 0});
        merge(&mut base, extra);
        let mut flagged = base.clone();
        flagged["include_launch_state"] = json!(true);
        (
            self.call(who, "agent_show", base)["agent"].clone(),
            self.call(who, "agent_show", flagged)["agent"].clone(),
        )
    }

    /// `agent_list` rows by alias, with and without the flag.
    fn list_pair(&self, who: &Asserted, extra: &Value) -> (Value, Value) {
        let mut base = json!({});
        merge(&mut base, extra);
        let mut flagged = base.clone();
        flagged["include_group"] = json!(true);
        (
            rows(&self.call(who, "agent_list", base)),
            rows(&self.call(who, "agent_list", flagged)),
        )
    }
}

fn merge(into: &mut Value, extra: &Value) {
    for (key, value) in extra.as_object().unwrap() {
        into[key] = value.clone();
    }
}

fn rows(list: &Value) -> Value {
    let mut by_alias = serde_json::Map::new();
    for row in list["agents"].as_array().expect("agents array") {
        by_alias.insert(row["alias"].as_str().unwrap().to_string(), row.clone());
    }
    Value::Object(by_alias)
}

fn assert_no_owner_text(case: &str, got: &Value) {
    let text = got.to_string();
    for secret in [ENDPOINT, JOB, LANE, UPSTREAM] {
        assert!(!text.contains(secret), "{case}: `{secret}` leaked: {got}");
    }
}

/// The member's `agent_show` with `include_launch_state` equals today's
/// stripped row, and the group's `agent_list` rows with `include_group`
/// equal today's rows, except the rows in `own` (the reader's own row, its
/// own group), which the rule admits.
fn assert_refused(world: &World, case: &str, who: Asserted, extra: Value, own: &[&str]) {
    let (base, got) = world.show_pair(&who, &extra);
    assert_eq!(base["alias"], MEMBER, "{case} show: {base}");
    assert_eq!(
        got, base,
        "{case} show: include_launch_state changed an unadmitted reader's row"
    );
    assert_no_owner_text(&format!("{case} show"), &got);

    let (base, got) = world.list_pair(&who, &extra);
    assert!(
        base.get(MEMBER).is_some(),
        "{case} list: the reader must see the member's row for this to bite: {base}"
    );
    for alias in ["root", MEMBER, "w2"] {
        if own.contains(&alias) {
            continue;
        }
        assert_eq!(
            got[alias], base[alias],
            "{case} list: include_group changed the unadmitted row `{alias}`"
        );
        assert_no_owner_text(&format!("{case} list row {alias}"), &got[alias]);
    }
}

#[test]
fn a_stranger_or_a_peer_on_its_own_connection_gets_nothing_extra() {
    let world = World::new();
    for (case, reader, own) in [
        ("stranger group root", "other", &[][..]),
        ("stranger worker", "ow", &[][..]),
        ("same-group peer", "w2", &["w2"][..]),
    ] {
        assert_refused(&world, case, Asserted::Agent(reader.into()), json!({}), own);
    }
}

#[test]
fn a_relayed_reader_gets_nothing_extra_even_naming_the_root_or_the_member() {
    let world = World::new();
    for actor in ["agents/root", "agents/w1", "users/subject-member"] {
        assert_refused(
            &world,
            &format!("read_as {actor}"),
            Asserted::Operator,
            json!({"read_as": actor}),
            &[],
        );
    }
}

#[test]
fn each_flag_keeps_only_its_own_fields_for_an_admitted_reader() {
    let world = World::new();
    for reader in ["root", MEMBER] {
        let who = Asserted::Agent(reader.into());
        let (_, shown) = world.show_pair(&who, &json!({}));
        for field in ["params", "cwd", "instructions"] {
            assert!(
                shown.get(field).is_none_or(Value::is_null),
                "{reader}: include_launch_state added `{field}`: {shown}"
            );
        }
        for secret in [JOB, LANE, UPSTREAM] {
            assert!(
                !shown.to_string().contains(secret),
                "{reader}: include_launch_state leaked `{secret}`: {shown}"
            );
        }

        let (base, listed) = world.list_pair(&who, &json!({}));
        let row = &listed[MEMBER];
        assert!(
            row.get("endpoint").is_none_or(Value::is_null),
            "{reader}: include_group added `endpoint`: {row}"
        );
        assert_eq!(
            row["state"], base[MEMBER]["state"],
            "{reader}: include_group restored the stored state"
        );
        for field in ["cwd", "instructions"] {
            assert!(
                row.get(field).is_none_or(Value::is_null),
                "{reader}: include_group added `{field}`: {row}"
            );
        }
        for secret in [ENDPOINT, JOB, LANE] {
            assert!(
                !listed.to_string().contains(secret),
                "{reader}: include_group leaked `{secret}`: {listed}"
            );
        }
    }
}

#[test]
fn control_the_group_root_on_its_own_connection_gets_both() {
    let world = World::new();
    let who = Asserted::Agent("root".into());
    let (base, shown) = world.show_pair(&who, &json!({}));
    assert!(base.get("endpoint").is_none(), "today's row: {base}");
    assert_eq!(shown["endpoint"], ENDPOINT, "{shown}");
    assert_eq!(shown["state"], "idle", "{shown}");

    let (base, listed) = world.list_pair(&who, &json!({}));
    assert!(base[MEMBER].get("params").is_none(), "today's row: {base}");
    for member in [MEMBER, "w2"] {
        assert_eq!(
            listed[member]["params"]["upstream"], "root",
            "{member}: {listed}"
        );
    }
}
