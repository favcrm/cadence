//! CAD-2310 independent refusal acceptance. Authored from the ticket, not the
//! implementation; the implementer must not edit or weaken it (AGENTS.md
//! "Gates and security work").
//!
//! The rule: a scoped `agent_show` carries the target's `instructions`,
//! `params` and `cwd` only for the briefing read (an include flag set), only
//! when the reader is the connection itself, and only when the caller rule
//! admits that connection as the agent or its group root. A relayed reader
//! (`read_as`, `public_session`) is someone other than the connection, so it
//! gets none of the three, even when the relayed identity names the group root
//! and the connection is the operator. Each relayed read must succeed (the
//! guard under test strips fields; it does not refuse the read) and leave the
//! tracker tree unchanged. The positive control sends the same briefing read
//! from the group root and from the member itself, so the refusals cannot pass
//! vacuously.

use crate::daemon::Shared;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const JOB: &str = "zq-relayed-briefing-job-2310: settle the vendor invoices";
const MEMBER: &str = "member";
const LANE: &str = "lane-cad2310-relay-cwd";
const HOST: &str = "relay.board.localhost:3112";

struct World {
    dir: tempfile::TempDir,
    shared: Arc<Shared>,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("c2310r").tempdir().unwrap();
        let pm = dir.path().join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        crate::board_identity::write_config(
            dir.path(),
            &crate::board_identity::Config {
                host: HOST.to_string(),
                issuer: "http://127.0.0.1:3998".to_string(),
                company: "synthetic-company".to_string(),
            },
        )
        .unwrap();
        let opts = crate::daemon::ServeOptions {
            test_seam: true,
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let shared = Shared::new(dir.path(), &opts).unwrap();
        let root_cwd = dir.path().to_str().unwrap().to_string();
        shared
            .store
            .register_agent(&NewAgent {
                alias: "root",
                provider: "fake",
                endpoint_kind: "fake",
                role: "pm",
                cwd: &root_cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        let world = Self { dir, shared };
        std::fs::create_dir_all(world.lane()).unwrap();
        world
            .call(
                Asserted::Operator,
                "agent_profile_create",
                json!({"alias": "ledger", "catalogue": "developer", "job": JOB}),
            )
            .expect("operator creates the profile");
        world
            .call(
                Asserted::Agent("root".into()),
                "agent_register",
                json!({
                    "alias": MEMBER, "provider": "fake", "endpoint_kind": "fake",
                    "cwd": world.lane(), "profile": "ledger",
                    "params": "{\"upstream\":\"root\"}",
                }),
            )
            .expect("the group root registers its member against the profile");
        world
    }

    fn lane(&self) -> PathBuf {
        self.dir.path().join(LANE)
    }

    fn call(&self, who: Asserted, method: &str, params: Value) -> crate::error::Result<Value> {
        scoped(who, || {
            self.shared.dispatch(method, &params, std::process::id())
        })
    }

    /// The briefing read, with one relay field added.
    fn relayed(&self, key: &str, value: Value) -> Value {
        let mut params = crate::briefing::briefing_read(MEMBER);
        params[key] = value;
        params
    }

    fn tree(&self) -> BTreeMap<String, Vec<u8>> {
        fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let rel = path
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                if path.is_dir() {
                    out.insert(format!("{rel}/"), vec![]);
                    walk(base, &path, out);
                } else {
                    out.insert(rel, std::fs::read(&path).unwrap());
                }
            }
        }
        let pm = self.dir.path().join("pm");
        let mut out = BTreeMap::new();
        walk(&pm, &pm, &mut out);
        out
    }

    /// A relayed briefing read on an operator connection: the read succeeds,
    /// names the member, and carries no job, `params` or `cwd`.
    fn assert_relay_stripped(&self, case: &str, params: Value) {
        let before = self.tree();
        let got = self
            .call(Asserted::Operator, "agent_show", params)
            .unwrap_or_else(|e| panic!("{case}: the relayed read itself must succeed: {e}"));
        let agent = &got["agent"];
        assert_eq!(agent["alias"], MEMBER, "{case}: {got}");
        for field in ["instructions", "params", "cwd"] {
            assert!(
                agent.get(field).is_none_or(Value::is_null),
                "{case}: a relayed reader got `{field}`: {got}"
            );
        }
        let text = got.to_string();
        assert!(!text.contains(JOB), "{case}: the job leaked: {got}");
        assert!(!text.contains(LANE), "{case}: the lane cwd leaked: {got}");
        // `upstream` alone is a capability's launch-param name; the stored
        // value is what must not leak.
        assert!(
            !text.contains(r#""upstream":"root""#),
            "{case}: params leaked: {got}"
        );
        assert_eq!(before, self.tree(), "{case}: a read moved the tracker tree");
    }
}

#[test]
fn a_read_as_board_member_gets_no_job_params_or_cwd() {
    let world = World::new();
    let params = world.relayed("read_as", json!("users/subject-member"));
    world.assert_relay_stripped("read_as a board member", params);
}

#[test]
fn a_read_as_relay_naming_the_group_root_or_the_member_gets_nothing() {
    let world = World::new();
    for actor in ["agents/root", "agents/member"] {
        let params = world.relayed("read_as", json!(actor));
        world.assert_relay_stripped(&format!("read_as {actor}"), params);
    }
}

#[test]
fn a_public_session_member_gets_no_job_params_or_cwd() {
    use crate::operator_auth::{BoardUser, PublicRealm};

    let world = World::new();
    let config = crate::board_identity::read_config(world.dir.path()).unwrap();
    let now = world.shared.operator_now();
    let token = world
        .shared
        .operator_auth()
        .open_public_bound(
            BoardUser {
                sub: "subject-member".to_string(),
                email: "member@example.test".to_string(),
                name: "Synthetic member".to_string(),
                role: "member".to_string(),
                handle: "member-user".to_string(),
            },
            "jti-cad2310-member",
            now + 60,
            "synthetic-test",
            PublicRealm {
                host: config.host,
                issuer: config.issuer,
                company: config.company,
            },
            now,
        )
        .unwrap()
        .unwrap()
        .token;
    let params = world.relayed("public_session", json!(token));
    world.assert_relay_stripped("public_session member", params);
}

#[test]
fn control_the_group_root_and_the_member_itself_get_the_job_params_and_cwd() {
    let world = World::new();
    for who in ["root", MEMBER] {
        let got = world
            .call(
                Asserted::Agent(who.into()),
                "agent_show",
                crate::briefing::briefing_read(MEMBER),
            )
            .unwrap_or_else(|e| panic!("{who}: the briefing read must succeed: {e}"));
        let agent = &got["agent"];
        assert!(
            agent["instructions"]
                .as_str()
                .is_some_and(|text| text.contains(JOB)),
            "{who}: the authorized reader lost the job: {got}"
        );
        assert_eq!(agent["params"]["upstream"], "root", "{who}: {got}");
        assert_eq!(
            agent["cwd"],
            json!(world.lane().to_str().unwrap()),
            "{who}: {got}"
        );
    }
    // Without an include flag the same reader gets none of the three.
    let plain = world
        .call(
            Asserted::Agent("root".into()),
            "agent_show",
            json!({"alias": MEMBER, "limit": 0}),
        )
        .expect("a plain scoped read succeeds");
    for field in ["instructions", "params", "cwd"] {
        assert!(
            plain["agent"].get(field).is_none_or(Value::is_null),
            "a read without an include flag got `{field}`: {plain}"
        );
    }
}
