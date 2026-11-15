//! CAD-616 independent negative security acceptance tests, authored from the
//! ticket BEFORE production code lands (`AGENTS.md` "Gates and security work").
//!
//! Drives the real `Shared::dispatch`, `wiki`, `master_perm`, `/proc`
//! enrollment identity (`slot_identity` + `operator_evidence`), and loopback
//! HTTP board (`crate::ui::serve`) guards with zero `test-seam` overrides:
//!
//! 1. Operator-only boundary on daemon RPCs (`master_permission_*`) and HTTP
//!    `/api/master/permissions*` routes against an enrolled worker, an
//!    anonymous caller, and a signed-in non-operator board member.
//! 2. Per-agent `agents/<alias>/permissions.yaml` (and `SOUL.md` / `AGENT.md`)
//!    write/delete refusal over `wiki_write` / `wiki_rm` for both agent and
//!    operator callers ("only a permission decision writes it").
//! 3. `role == "reviewer"` refusal on `request_open` (`kind: "approval"`),
//!    even with `broker_approvals: true` and a planted allow rule.
//! 4. Foreign worker spoofing (`w2` claiming `alias: "w1"`) on `request_open`
//!    and `master_permission_use` confinement against worker requests, forged
//!    state grants, worker rules, and non-master/`Class::Never` commands.
//! 5. Immediate `Class::Never` refusal on brokered worker `request_open`
//!    (`agents/w1/permissions.yaml`, `agents/w1/SOUL.md`, `agents/w1/AGENT.md`,
//!    `agents/master/permissions.yaml`, daemon `state_dir`, `cadence audit
//!    approve`, `gh pr merge`) even with planted allow rules.
//! 6. Fail-closed refusal on symlinked or malformed `agents/w1/permissions.yaml`
//!    when evaluating a worker `request_open` (`kind: "approval"`).

use super::{ServeOptions, Shared};
use crate::adapter::Identity;
use crate::issue::Pm;
use crate::master_perm::{self, Class, Effect, Request, Risk, Rule, Scope};
use crate::store::NewAgent;
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

fn path_var() -> String {
    std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string())
}

fn spawn_sleeper() -> Child {
    Command::new("sleep")
        .arg("300")
        .env_clear()
        .env("PATH", path_var())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn clean child for enrolled agent identity")
}

/// Live enrolled managed agent backed by a real child process verified
/// through `/proc/<pid>` (`slot_identity` + `strict_caller`).
struct EnrolledSeat {
    alias: String,
    child: Child,
}

impl EnrolledSeat {
    fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for EnrolledSeat {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    state_dir: PathBuf,
    pm_dir: PathBuf,
    repo_dir: PathBuf,
    shared: Arc<Shared>,
}

impl Fixture {
    fn new(prefix: &str) -> Self {
        let dir = tempfile::Builder::new().prefix(prefix).tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();

        let state_dir = dir.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();
        fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700)).unwrap();

        let pm_dir = dir.path().join("pm");
        let pm = Pm::init(&pm_dir).unwrap();

        let repo_dir = dir.path().join("repo");
        fs::create_dir_all(repo_dir.join("master-ok")).unwrap();
        fs::create_dir_all(repo_dir.join("forged")).unwrap();
        fs::create_dir_all(repo_dir.join("worker-rule")).unwrap();
        let repo_canon = fs::canonicalize(&repo_dir).unwrap();

        let proj_dir = pm_dir.join("demo");
        fs::create_dir_all(&proj_dir).unwrap();
        let proj_yaml = proj_dir.join("project.yaml");
        fs::write(
            &proj_yaml,
            format!(
                "key: demo\nprefix: DEMO\nrepos:\n  - path: {}\n",
                repo_canon.display()
            ),
        )
        .unwrap();
        crate::issue::write::commit(
            &pm,
            std::slice::from_ref(&proj_yaml),
            "demo: register test checkout",
            &[],
            "operator",
        )
        .unwrap();

        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
        let shared = Shared::new(&state_dir, &opts).unwrap();

        Self {
            _dir: dir,
            state_dir,
            pm_dir,
            repo_dir: repo_canon,
            shared,
        }
    }

    fn enroll_managed(&self, alias: &str, role: &str, broker_approvals: bool) -> EnrolledSeat {
        let child = spawn_sleeper();
        let seat = EnrolledSeat {
            alias: alias.to_string(),
            child,
        };
        self.bind_child_seat(&seat.alias, role, broker_approvals, seat.pid());
        seat
    }

    fn bind_child_seat(&self, alias: &str, role: &str, broker_approvals: bool, pid: u32) {
        let cwd = self.repo_dir.to_str().unwrap();
        let params = json!({ "broker_approvals": broker_approvals }).to_string();
        self.shared
            .store
            .register_agent(&NewAgent {
                alias,
                provider: "claude",
                endpoint_kind: "managed",
                role,
                cwd,
                sandbox: "read-only",
                instructions: None,
                params: Some(&params),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        self.shared
            .store
            .set_identity(
                alias,
                &Identity {
                    thread_id: format!("thread-{alias}"),
                    session_id: format!("session-{alias}"),
                    model: None,
                    effort: None,
                    pid,
                    endpoint: Some(format!("managed://{alias}")),
                    generation: Some(format!("gen-{alias}")),
                    attach: None,
                },
            )
            .unwrap();
        self.shared.enroll_endpoint(alias);
        let derived = self
            .shared
            .slot_identity(pid)
            .unwrap()
            .expect("enrolled managed child must derive slot_identity");
        assert_eq!(derived.lane(), alias);
    }

    fn operator_pid(&self) -> u32 {
        let pid = std::process::id();
        self.shared
            .operator_evidence(pid)
            .expect("test process must pass positive operator proof on isolated fixture");
        pid
    }

    fn dispatch(&self, method: &str, params: Value, peer_pid: u32) -> crate::error::Result<Value> {
        self.shared.dispatch(method, &params, peer_pid)
    }

    fn pending_requests_for(&self, alias: &str) -> Vec<String> {
        let mut ids: Vec<String> = self
            .shared
            .pending
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, req)| req.alias == alias)
            .map(|(id, _)| id.clone())
            .collect();
        if let Ok(rows) = master_perm::board_requests(&self.state_dir, master_perm::clock()) {
            for r in rows {
                if r.requested_by == alias && r.status == "pending" {
                    ids.push(r.id);
                }
            }
        }
        ids
    }
}

fn exact_allow_rule(id: &str, argv: Vec<String>, cwd: &Path) -> Rule {
    Rule {
        id: id.to_string(),
        effect: Effect::Allow,
        scope: Scope::Exact,
        argv,
        tail: Vec::new(),
        cwd: cwd.to_string_lossy().into_owned(),
        by: "operator".to_string(),
        at: master_perm::clock(),
    }
}

fn seed_master_perm_doc(state_dir: &Path, doc: Value) -> PathBuf {
    let path = state_dir.join("master-permissions.json");
    fs::write(&path, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
    path
}

fn request_record(
    id: &str,
    requested_by: &str,
    argv: &[String],
    cwd: &Path,
    status: &str,
) -> Request {
    let now = master_perm::clock();
    Request {
        id: id.to_string(),
        argv: argv.to_vec(),
        cwd: cwd.to_string_lossy().into_owned(),
        reason: "acceptance seed".to_string(),
        requested_by: requested_by.to_string(),
        created: now,
        expires_at: now + 600,
        status: status.to_string(),
        risk: Risk::Low,
        decision: String::new(),
        decision_label: String::new(),
    }
}

fn request_open_bash(alias: &str, handle: &str, command: &str) -> Value {
    json!({
        "alias": alias,
        "kind": "approval",
        "tool": "Bash",
        "request": handle,
        "input_summary": command,
        "input": { "command": command },
    })
}

fn request_open_write(alias: &str, handle: &str, file_path: &str, content: &str) -> Value {
    json!({
        "alias": alias,
        "kind": "approval",
        "tool": "Write",
        "request": handle,
        "input_summary": file_path,
        "input": { "file_path": file_path, "content": content },
    })
}

#[test]
fn operator_only_master_permission_rpcs_refuse_worker_caller() {
    let fx = Fixture::new("c616rpc");
    let w1 = fx.enroll_managed("w1", "worker", true);
    let _master = fx.enroll_managed("master", "worker", false);
    let op_pid = fx.operator_pid();

    let ls_argv = vec!["ls".into(), fx.repo_dir.to_string_lossy().into_owned()];
    let w1_req = request_record("perm-w1-1", "w1", &ls_argv, &fx.repo_dir, "pending");
    let master_req = request_record("perm-m-1", "master", &ls_argv, &fx.repo_dir, "pending");
    let doc_path = seed_master_perm_doc(
        &fx.state_dir,
        json!({
            "requests": [w1_req, master_req],
            "grants": [],
        }),
    );

    let w1_rule_path = master_perm::write_rules(
        &fx.pm_dir,
        "w1",
        &[exact_allow_rule("rule-w1-1", ls_argv.clone(), &fx.repo_dir)],
    )
    .unwrap();
    let master_rule_path = master_perm::write_rules(
        &fx.pm_dir,
        "master",
        &[exact_allow_rule("rule-m-1", ls_argv, &fx.repo_dir)],
    )
    .unwrap();

    // Positive control: the real operator lists permissions cleanly.
    fx.dispatch("master_permission_list", json!({}), op_pid)
        .expect("operator must be allowed to call master_permission_list");

    // Snapshot after the read baseline so we prove refused agent calls make zero mutations.
    let doc_before = fs::read(&doc_path).unwrap();
    let w1_rules_before = fs::read(&w1_rule_path).unwrap();
    let master_rules_before = fs::read(&master_rule_path).unwrap();

    let agent_calls = [
        ("master_permission_list", json!({})),
        ("master_permission_allow_once", json!({"id": "perm-w1-1"})),
        ("master_permission_allow_once", json!({"id": "perm-m-1"})),
        (
            "master_permission_always",
            json!({"id": "perm-w1-1", "scope": "exact"}),
        ),
        (
            "master_permission_always",
            json!({"id": "perm-m-1", "scope": "exact"}),
        ),
        (
            "master_permission_reject",
            json!({"id": "perm-w1-1", "dont_ask_again": true}),
        ),
        (
            "master_permission_reject",
            json!({"id": "perm-m-1", "dont_ask_again": true}),
        ),
        ("master_permission_revoke", json!({"id": "rule-w1-1"})),
        ("master_permission_revoke", json!({"id": "rule-m-1"})),
    ];

    for (method, params) in agent_calls {
        let err = fx
            .dispatch(method, params.clone(), w1.pid())
            .expect_err(&format!("worker w1 must be refused {method} {params}"));
        let msg = err.to_string();
        assert!(
            msg.contains("operator"),
            "{method} refusal must name operator boundary, got: {msg}"
        );
    }

    // Even when an agent supplies an explicit `alias` targeting worker rules, it must be refused
    // (either as connection-bound on origin/main or as operator-only once worker alias routing exists).
    for (method, params) in [
        ("master_permission_list", json!({"alias": "w1"})),
        (
            "master_permission_revoke",
            json!({"id": "rule-w1-1", "alias": "w1"}),
        ),
    ] {
        let err = fx
            .dispatch(method, params.clone(), w1.pid())
            .expect_err(&format!("worker w1 must be refused {method} {params}"));
        let msg = err.to_string();
        assert!(
            msg.contains("operator") || msg.contains("connection-bound"),
            "{method} {params} refusal must hit operator/connection-bound boundary, got: {msg}"
        );
    }

    assert_eq!(fs::read(&doc_path).unwrap(), doc_before);
    assert_eq!(fs::read(&w1_rule_path).unwrap(), w1_rules_before);
    assert_eq!(fs::read(&master_rule_path).unwrap(), master_rules_before);
}

#[test]
fn wiki_write_and_rm_refuse_per_agent_permissions_yaml_for_agent_and_operator() {
    let fx = Fixture::new("c616wiki");
    let w1 = fx.enroll_managed("w1", "worker", true);
    let w2 = fx.enroll_managed("w2", "worker", true);
    let op_pid = fx.operator_pid();

    // Positive controls: wiki_write works for own knowledge/ so refusals are ACL-driven.
    fx.dispatch(
        "wiki_write",
        json!({"path": "agents/w1/knowledge/ok.md", "text": "# worker note\n"}),
        w1.pid(),
    )
    .expect("agent writes own knowledge/");
    fx.dispatch(
        "wiki_write",
        json!({"path": "agents/w1/knowledge/op.md", "text": "# operator note\n"}),
        op_pid,
    )
    .expect("operator writes agent knowledge/");

    let perm_path = fx.pm_dir.join("agents/w1/permissions.yaml");
    assert!(!perm_path.exists());

    for (who, pid) in [("w1", w1.pid()), ("w2", w2.pid()), ("operator", op_pid)] {
        let err = fx
            .dispatch(
                "wiki_write",
                json!({
                    "path": "agents/w1/permissions.yaml",
                    "text": "rules:\n  - id: forged\n",
                }),
                pid,
            )
            .expect_err(&format!(
                "{who} must be refused wiki_write on agents/w1/permissions.yaml"
            ));
        let msg = err.to_string();
        assert!(
            msg.contains("only a permission decision writes it"),
            "{who} wiki_write on agents/w1/permissions.yaml must hit permission-decision protection, got: {msg}"
        );
        assert!(
            !perm_path.exists(),
            "{who} created permissions.yaml on disk"
        );
    }

    // Seed a real committed permissions.yaml file and test wiki_rm refusal for agent and operator.
    let seeded = master_perm::write_rules(
        &fx.pm_dir,
        "w1",
        &[exact_allow_rule(
            "rule-keep",
            vec!["ls".into(), fx.repo_dir.to_string_lossy().into_owned()],
            &fx.repo_dir,
        )],
    )
    .unwrap();
    let pm = Pm::at(&fx.pm_dir).unwrap();
    crate::issue::write::commit(
        &pm,
        std::slice::from_ref(&seeded),
        "seed agents/w1/permissions.yaml",
        &[],
        "operator",
    )
    .unwrap();
    let before = fs::read(&seeded).unwrap();

    for (who, pid) in [("w1", w1.pid()), ("w2", w2.pid()), ("operator", op_pid)] {
        let err = fx
            .dispatch(
                "wiki_rm",
                json!({"path": "agents/w1/permissions.yaml"}),
                pid,
            )
            .expect_err(&format!(
                "{who} must be refused wiki_rm on agents/w1/permissions.yaml"
            ));
        let msg = err.to_string();
        assert!(
            msg.contains("only a permission decision writes it"),
            "{who} wiki_rm on agents/w1/permissions.yaml must hit permission-decision protection, got: {msg}"
        );
        assert_eq!(
            fs::read(&seeded).unwrap(),
            before,
            "{who} altered permissions.yaml"
        );
    }

    // SOUL.md and AGENT.md across worker aliases are also protected from wiki writes.
    for path in [
        "agents/w1/profile/SOUL.md",
        "agents/w1/profile/AGENT.md",
        "agents/w1/SOUL.md",
        "agents/w1/AGENT.md",
    ] {
        for (who, pid) in [("w1", w1.pid()), ("operator", op_pid)] {
            fx.dispatch("wiki_write", json!({"path": path, "text": "tampered"}), pid)
                .expect_err(&format!("{who} must be refused wiki_write on {path}"));
            assert!(!fx.pm_dir.join(path).exists(), "{who} wrote {path}");
        }
    }
}

#[test]
fn reviewer_role_is_refused_approval_requests_even_when_brokered_with_allow_rule() {
    let fx = Fixture::new("c616rev");
    let rev = fx.enroll_managed("rev1", "reviewer", true);

    let cmd = format!("ls {}", fx.repo_dir.display());
    let argv = vec!["ls".into(), fx.repo_dir.to_string_lossy().into_owned()];
    master_perm::write_rules(
        &fx.pm_dir,
        "rev1",
        &[exact_allow_rule("rev-allow-ls", argv, &fx.repo_dir)],
    )
    .unwrap();

    let err = fx
        .dispatch(
            "request_open",
            request_open_bash("rev1", "perm-rev-1", &cmd),
            rev.pid(),
        )
        .expect_err("role == reviewer must be unconditionally refused request_open(approval)");
    assert!(
        !err.to_string().is_empty(),
        "reviewer refusal returned an empty error"
    );
    assert!(
        fx.pending_requests_for("rev1").is_empty(),
        "refused reviewer request must not leave a pending request"
    );
}

#[test]
fn foreign_worker_and_master_permission_use_confinement() {
    let fx = Fixture::new("c616use");
    let w1 = fx.enroll_managed("w1", "worker", true);
    let w2 = fx.enroll_managed("w2", "worker", true);
    let master = fx.enroll_managed("master", "pm", false);
    fx.shared
        .store
        .enqueue(
            "master",
            "home permission turn",
            None,
            "msg-master-home-1",
            "operator",
        )
        .unwrap();
    assert!(matches!(
        fx.shared.store.take_queued("master").unwrap(),
        crate::store::Take::Message(_)
    ));

    // 1. Foreign worker w2 cannot open a permission request claiming alias w1.
    let w1_cmd = format!("ls {}", fx.repo_dir.display());
    fx.dispatch(
        "request_open",
        request_open_bash("w1", "perm-spoof-1", &w1_cmd),
        w2.pid(),
    )
    .expect_err("foreign worker w2 must be refused request_open for w1");
    assert!(fx.pending_requests_for("w1").is_empty());

    // 2. Worker w1 cannot call master_permission_use directly.
    let worker_argv = vec!["ls".into(), fx.repo_dir.to_string_lossy().into_owned()];
    fx.dispatch(
        "master_permission_use",
        json!({"argv": worker_argv, "cwd": fx.repo_dir}),
        w1.pid(),
    )
    .expect_err("worker w1 must be refused master_permission_use");

    // 3. Even when master calls master_permission_use, it cannot redeem:
    //    - a grant tied to a worker request (`requested_by: "w1"`)
    //    - a forged orphan grant without a master request
    //    - a grant for a Class::Never / non-master command (`gh pr merge`)
    //    - a worker allow rule in agents/w1/permissions.yaml
    let forged_dir = fx.repo_dir.join("forged");
    let worker_rule_dir = fx.repo_dir.join("worker-rule");
    let master_ok_dir = fx.repo_dir.join("master-ok");

    let forged_argv = vec!["ls".into(), forged_dir.to_string_lossy().into_owned()];
    let worker_rule_argv = vec!["ls".into(), worker_rule_dir.to_string_lossy().into_owned()];
    let master_ok_argv = vec!["ls".into(), master_ok_dir.to_string_lossy().into_owned()];
    let merge_argv: Vec<String> = vec!["gh".into(), "pr".into(), "merge".into(), "1".into()];

    let now = master_perm::clock();
    let exp = now + 600;
    let w1_req = request_record("perm-w1-req", "w1", &worker_argv, &fx.repo_dir, "allowed");
    let master_req = request_record(
        "perm-m-ctrl",
        "master",
        &master_ok_argv,
        &fx.repo_dir,
        "allowed",
    );
    let doc_path = seed_master_perm_doc(
        &fx.state_dir,
        json!({
            "requests": [w1_req, master_req],
            "grants": [
                {
                    "id": "grant-worker",
                    "request_id": "perm-w1-req",
                    "argv": worker_argv,
                    "cwd": fx.repo_dir,
                    "uses_left": 1,
                    "expires_at": exp,
                },
                {
                    "id": "grant-forged-orphan",
                    "request_id": "perm-does-not-exist",
                    "argv": forged_argv,
                    "cwd": fx.repo_dir,
                    "uses_left": 1,
                    "expires_at": exp,
                },
                {
                    "id": "grant-never-merge",
                    "request_id": "perm-m-ctrl",
                    "argv": merge_argv,
                    "cwd": fx.repo_dir,
                    "uses_left": 1,
                    "expires_at": exp,
                },
                {
                    "id": "grant-master-ok",
                    "request_id": "perm-m-ctrl",
                    "argv": master_ok_argv,
                    "cwd": fx.repo_dir,
                    "uses_left": 1,
                    "expires_at": exp,
                }
            ]
        }),
    );

    master_perm::write_rules(
        &fx.pm_dir,
        "w1",
        &[exact_allow_rule(
            "rule-w1-only",
            worker_rule_argv.clone(),
            &fx.repo_dir,
        )],
    )
    .unwrap();

    // Positive control: master CAN redeem its own genuine master grant.
    let ctrl = fx
        .dispatch(
            "master_permission_use",
            json!({"argv": master_ok_argv, "cwd": fx.repo_dir}),
            master.pid(),
        )
        .expect("positive control master grant must execute");
    assert_eq!(
        ctrl["applied"],
        json!(true),
        "master positive control: {ctrl}"
    );

    // Worker-owned request's grant must NOT be redeemed by master.
    let res_worker = fx.dispatch(
        "master_permission_use",
        json!({"argv": worker_argv, "cwd": fx.repo_dir}),
        master.pid(),
    );
    assert!(
        res_worker.is_err()
            || res_worker
                .as_ref()
                .is_ok_and(|v| v["applied"] == json!(false)),
        "master_permission_use must not redeem a grant belonging to worker w1, got: {res_worker:?}"
    );

    // Forged orphan grant with no matching master request must NOT be redeemed.
    let res_forged = fx.dispatch(
        "master_permission_use",
        json!({"argv": forged_argv, "cwd": fx.repo_dir}),
        master.pid(),
    );
    assert!(
        res_forged.is_err()
            || res_forged
                .as_ref()
                .is_ok_and(|v| v["applied"] == json!(false)),
        "master_permission_use must not redeem a forged orphan grant, got: {res_forged:?}"
    );

    // Non-master / Class::Never command must be refused even with a matching grant.
    fx.dispatch(
        "master_permission_use",
        json!({"argv": merge_argv, "cwd": fx.repo_dir}),
        master.pid(),
    )
    .expect_err("Class::Never non-master command must be refused even with planted grant");

    // Worker's permissions.yaml allow rule must NOT apply to master_permission_use.
    let res_w_rule = fx.dispatch(
        "master_permission_use",
        json!({"argv": worker_rule_argv, "cwd": fx.repo_dir}),
        master.pid(),
    );
    assert!(
        res_w_rule.is_err()
            || res_w_rule
                .as_ref()
                .is_ok_and(|v| v["applied"] == json!(false)),
        "master_permission_use must not consume worker w1 allow rules, got: {res_w_rule:?}"
    );

    // Confirm the unredeemed worker grant was not consumed in master-permissions.json.
    let after: Value = serde_json::from_slice(&fs::read(&doc_path).unwrap()).unwrap();
    let worker_grant = after["grants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["id"] == json!("grant-worker"))
        .unwrap();
    assert_eq!(
        worker_grant["uses_left"],
        json!(1),
        "worker grant must stay unconsumed: {after}"
    );
}

#[test]
fn worker_class_never_targets_are_refused_even_with_planted_allow_rules() {
    let fx = Fixture::new("c616nvr");
    let w1 = fx.enroll_managed("w1", "worker", true);

    let pm_w1_perm = fx.pm_dir.join("agents/w1/permissions.yaml");
    let pm_w1_soul = fx.pm_dir.join("agents/w1/SOUL.md");
    let pm_w1_agent = fx.pm_dir.join("agents/w1/AGENT.md");
    let pm_master_perm = fx.pm_dir.join("agents/master/permissions.yaml");
    let state_target = fx.state_dir.join("cadence.sqlite3");

    let bash_cmds: Vec<(&str, Vec<String>)> = vec![
        (
            "cat agents/w1/permissions.yaml",
            vec!["cat".into(), "agents/w1/permissions.yaml".into()],
        ),
        (
            "abs agents/w1/permissions.yaml",
            vec!["cat".into(), pm_w1_perm.to_string_lossy().into_owned()],
        ),
        (
            "cat agents/w1/SOUL.md",
            vec!["cat".into(), "agents/w1/SOUL.md".into()],
        ),
        (
            "abs agents/w1/SOUL.md",
            vec!["cat".into(), pm_w1_soul.to_string_lossy().into_owned()],
        ),
        (
            "cat agents/w1/AGENT.md",
            vec!["cat".into(), pm_w1_agent.to_string_lossy().into_owned()],
        ),
        (
            "cat agents/master/permissions.yaml",
            vec!["cat".into(), pm_master_perm.to_string_lossy().into_owned()],
        ),
        (
            "cat state_dir sqlite",
            vec!["cat".into(), state_target.to_string_lossy().into_owned()],
        ),
        (
            "ls state_dir",
            vec!["ls".into(), fx.state_dir.to_string_lossy().into_owned()],
        ),
        (
            "cadence audit approve",
            vec![
                "cadence".into(),
                "audit".into(),
                "approve".into(),
                "CAD-1".into(),
                "--sha".into(),
                "0123456789abcdef0123456789abcdef01234567".into(),
            ],
        ),
        (
            "gh pr merge",
            vec!["gh".into(), "pr".into(), "merge".into(), "123".into()],
        ),
    ];

    // Also verify master_perm::classify directly marks worker protected files as Class::Never
    // for the protected-file reason (even when the files reside inside a registered checkout),
    // not merely because of an unknown checkout path.
    fs::create_dir_all(fx.pm_dir.join("agents/w1/sub")).unwrap();
    fs::create_dir_all(fx.pm_dir.join("agents/master")).unwrap();
    fs::write(&pm_w1_perm, "rules: []\n").unwrap();
    fs::write(&pm_w1_soul, "# w1 soul\n").unwrap();
    fs::write(&pm_w1_agent, "# w1 agent\n").unwrap();
    fs::write(&pm_master_perm, "rules: []\n").unwrap();
    let states = [fx.state_dir.as_path()];
    let pm_canon = fs::canonicalize(&fx.pm_dir).unwrap();
    let checkouts = [pm_canon];
    for rel in [
        "agents/w1/permissions.yaml",
        "agents/w1/sub/../permissions.yaml",
        "agents/w1/SOUL.md",
        "agents/w1/AGENT.md",
        "agents/master/permissions.yaml",
    ] {
        let cls =
            master_perm::classify(&["cat".into(), rel.into()], &fx.pm_dir, &checkouts, &states);
        assert!(
            matches!(cls, Class::Never { why } if !why.contains("only a cadence verb")),
            "master_perm::classify must mark {rel} as protected Class::Never, got {cls:?}"
        );
    }

    // Plant matching Exact Allow rules in agents/w1/permissions.yaml for every command and tool path.
    let mut planted: Vec<Rule> = bash_cmds
        .iter()
        .enumerate()
        .map(|(i, (_, argv))| {
            exact_allow_rule(&format!("allow-bash-{i}"), argv.clone(), &fx.repo_dir)
        })
        .collect();
    for (i, p) in [
        "agents/w1/permissions.yaml".to_string(),
        pm_w1_perm.to_string_lossy().into_owned(),
        "agents/w1/SOUL.md".to_string(),
        pm_w1_soul.to_string_lossy().into_owned(),
        "agents/master/permissions.yaml".to_string(),
        pm_master_perm.to_string_lossy().into_owned(),
        fx.state_dir.join("evil.txt").to_string_lossy().into_owned(),
    ]
    .iter()
    .enumerate()
    {
        planted.push(exact_allow_rule(
            &format!("allow-write-{i}"),
            vec!["Write".into(), p.clone()],
            &fx.repo_dir,
        ));
    }
    master_perm::write_rules(&fx.pm_dir, "w1", &planted).unwrap();

    for (idx, (label, argv)) in bash_cmds.iter().enumerate() {
        let cmd = argv.join(" ");
        fx.dispatch(
            "request_open",
            request_open_bash("w1", &format!("perm-never-bash-{idx}"), &cmd),
            w1.pid(),
        )
        .expect_err(&format!(
            "Class::Never Bash target '{label}' ({cmd}) must be refused on request_open even with planted allow rule"
        ));
    }

    let write_targets = [
        "agents/w1/permissions.yaml".to_string(),
        pm_w1_perm.to_string_lossy().into_owned(),
        "agents/w1/sub/../SOUL.md".to_string(),
        pm_w1_soul.to_string_lossy().into_owned(),
        pm_w1_agent.to_string_lossy().into_owned(),
        "agents/master/permissions.yaml".to_string(),
        pm_master_perm.to_string_lossy().into_owned(),
        fx.state_dir.join("evil.txt").to_string_lossy().into_owned(),
    ];
    for (idx, target) in write_targets.iter().enumerate() {
        fx.dispatch(
            "request_open",
            request_open_write("w1", &format!("perm-never-write-{idx}"), target, "tampered"),
            w1.pid(),
        )
        .expect_err(&format!(
            "Class::Never Write target '{target}' must be refused on request_open even with planted allow rule"
        ));
    }

    assert!(
        fx.pending_requests_for("w1").is_empty(),
        "refused Class::Never requests must never park pending entries"
    );
}

#[test]
fn malformed_or_symlinked_worker_permissions_yaml_fails_closed() {
    let fx = Fixture::new("c616yaml");
    let w1 = fx.enroll_managed("w1", "worker", true);

    let benign_cmd = format!("ls {}", fx.repo_dir.display());
    let benign_argv = vec!["ls".into(), fx.repo_dir.to_string_lossy().into_owned()];

    // Case A: agents/w1/permissions.yaml is a symlink to another valid allow-rule file.
    let outside = fx._dir.path().join("outside-rules.yaml");
    let yaml_ok = serde_yaml::to_string(&json!({
        "rules": [exact_allow_rule("sym-rule", benign_argv, &fx.repo_dir)]
    }))
    .unwrap();
    fs::write(&outside, yaml_ok).unwrap();

    let w1_dir = fx.pm_dir.join("agents/w1");
    fs::create_dir_all(&w1_dir).unwrap();
    let w1_perm = w1_dir.join("permissions.yaml");
    std::os::unix::fs::symlink(&outside, &w1_perm).unwrap();

    fx.dispatch(
        "request_open",
        request_open_bash("w1", "perm-sym-1", &benign_cmd),
        w1.pid(),
    )
    .expect_err("symlinked agents/w1/permissions.yaml must fail closed on request_open");
    assert!(fx.pending_requests_for("w1").is_empty());

    // Case B: agents/w1/permissions.yaml is a regular file with malformed YAML.
    fs::remove_file(&w1_perm).unwrap();
    fs::write(&w1_perm, "rules: [\n  unclosed: : :\n").unwrap();

    fx.dispatch(
        "request_open",
        request_open_bash("w1", "perm-bad-yaml-1", &benign_cmd),
        w1.pid(),
    )
    .expect_err("malformed agents/w1/permissions.yaml must fail closed on request_open");
    assert!(fx.pending_requests_for("w1").is_empty());
}

#[cfg(test)]
mod http_acceptance {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;

    const JWK_KID: &str = "cad616-key-1";
    const COMPANY_ID: &str = "comp-cad616";

    fn free_port() -> u16 {
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    }

    struct HttpRig {
        fx: Fixture,
        ui_port: u16,
        local_host: String,
        public_host: String,
        keypair: Ed25519KeyPair,
        issuer: String,
        stop: Arc<AtomicBool>,
        jwks_server: Arc<tiny_http::Server>,
        threads: Vec<thread::JoinHandle<()>>,
    }

    impl HttpRig {
        fn start() -> Self {
            let fx = Fixture::new("c616ui");
            let sock_path = crate::client::socket_path(&fx.state_dir);
            let listener = UnixListener::bind(&sock_path).unwrap();
            let stop = Arc::new(AtomicBool::new(false));

            // Spin up a real daemon socket thread dispatching to our Shared.
            let daemon_handle = {
                let shared = Arc::clone(&fx.shared);
                let stop = Arc::clone(&stop);
                thread::spawn(move || loop {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if stop.load(Ordering::Acquire) {
                                break;
                            }
                            let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
                            super::super::serve::handle_conn(Arc::clone(&shared), stream);
                        }
                        Err(_) if stop.load(Ordering::Acquire) => break,
                        Err(_) => break,
                    }
                })
            };

            // Spin up a loopback JWKS server for PublicBoard signed assertions.
            let keypair = Ed25519KeyPair::from_seed_unchecked(&[0x61; 32]).unwrap();
            let pub_b64 = URL_SAFE_NO_PAD.encode(keypair.public_key().as_ref());
            let jwks_server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
            let jwks_port = jwks_server.server_addr().to_ip().unwrap().port();
            let issuer = format!("http://127.0.0.1:{jwks_port}");
            let jwks_body = json!({
                "keys": [{
                    "kty": "OKP",
                    "crv": "Ed25519",
                    "kid": JWK_KID,
                    "x": pub_b64,
                }]
            })
            .to_string();

            let jwks_handle = {
                let srv = Arc::clone(&jwks_server);
                let stop = Arc::clone(&stop);
                thread::spawn(move || {
                    for req in srv.incoming_requests() {
                        if stop.load(Ordering::Acquire) {
                            break;
                        }
                        let resp = tiny_http::Response::from_string(jwks_body.clone())
                            .with_status_code(200)
                            .with_header(
                                tiny_http::Header::from_bytes("Content-Type", "application/json")
                                    .unwrap(),
                            );
                        let _ = req.respond(resp);
                    }
                })
            };

            let ui_port = free_port();
            let local_host = format!("cadence-{ui_port}.localhost:{ui_port}");
            let public_host = format!("cad616.board.localhost:{ui_port}");
            crate::board_identity::write_config(
                &fx.state_dir,
                &crate::board_identity::Config {
                    host: public_host.clone(),
                    issuer: issuer.clone(),
                    company: COMPANY_ID.to_string(),
                },
            )
            .unwrap();

            let (startup_tx, startup_rx) = std::sync::mpsc::channel();
            let mut opts = crate::ui::ServeOpts {
                host: "127.0.0.1".to_string(),
                port: ui_port,
                allow_hosts: vec![public_host.clone()],
                allow_origins: vec![format!("http://{public_host}")],
                stop: Some(Arc::clone(&stop)),
                startup: Some(startup_tx),
                public: Some(crate::ui::PublicBoard {
                    host: public_host.clone(),
                    issuer: issuer.clone(),
                    company: COMPANY_ID.to_string(),
                    authorize_url: format!("{issuer}/v2/board/authorize"),
                }),
                ..Default::default()
            };
            let _ = &mut opts;

            let ui_handle = {
                let state = fx.state_dir.clone();
                let pm = fx.pm_dir.clone();
                thread::spawn(move || {
                    let _ = crate::ui::serve(&state, &pm, &opts);
                })
            };
            startup_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("ui::serve timed out starting")
                .expect("ui::serve failed to bind");

            Self {
                fx,
                ui_port,
                local_host,
                public_host,
                keypair,
                issuer,
                stop,
                jwks_server,
                threads: vec![daemon_handle, jwks_handle, ui_handle],
            }
        }

        fn send_raw_http(&self, raw_request: &str) -> (u16, Vec<(String, String)>, Value) {
            let mut stream = TcpStream::connect(("127.0.0.1", self.ui_port)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(15)))
                .unwrap();
            stream.write_all(raw_request.as_bytes()).unwrap();
            stream.flush().unwrap();
            let mut raw = Vec::new();
            stream.read_to_end(&mut raw).unwrap();
            parse_http_response(&raw)
        }

        fn request(
            &self,
            method: &str,
            path: &str,
            host: &str,
            headers: &[(&str, &str)],
            body: Option<&str>,
        ) -> (u16, Vec<(String, String)>, Value) {
            let raw = build_http_request(method, path, host, headers, body);
            self.send_raw_http(&raw)
        }

        fn mint_local_operator_session(&self) -> (String, String) {
            crate::operator_auth::ensure_secret(&self.fx.state_dir).unwrap();
            let secret = crate::operator_auth::read_secret(&self.fx.state_dir).unwrap();
            let minted = self
                .fx
                .dispatch(
                    "operator_link_mint",
                    json!({"secret": secret, "origin": "loopback"}),
                    self.fx.operator_pid(),
                )
                .unwrap();
            let nonce = minted["nonce"].as_str().unwrap();
            let origin = format!("http://{}", self.local_host);
            let body = json!({"nonce": nonce}).to_string();
            let (status, hdrs, json_body) = self.request(
                "POST",
                "/api/session",
                &self.local_host,
                &[
                    ("Content-Type", "application/json"),
                    ("X-Cadence-Board", "1"),
                    ("Origin", &origin),
                    ("Sec-Fetch-Site", "same-origin"),
                ],
                Some(&body),
            );
            assert_eq!(status, 200, "POST /api/session failed: {json_body}");
            let cookie = extract_cookie(&hdrs).expect("Set-Cookie missing from /api/session");
            let session_key = json_body["session_key"]
                .as_str()
                .expect("session_key missing")
                .to_string();
            (cookie, session_key)
        }

        fn mint_public_board_cookie(&self, sub: &str, role: &str) -> String {
            let now = crate::issue::time::now_epoch();
            let header = URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&json!({
                    "alg": "EdDSA",
                    "typ": "JWT",
                    "kid": JWK_KID,
                }))
                .unwrap(),
            );
            let claims = URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&json!({
                    "iss": self.issuer,
                    "aud": self.public_host,
                    "sub": sub,
                    "email": format!("{sub}@example.com"),
                    "name": sub,
                    "company": COMPANY_ID,
                    "role": role,
                    "iat": now,
                    "exp": now + 30,
                    "jti": format!("jti-{}-{}", sub, uuid::Uuid::new_v4()),
                }))
                .unwrap(),
            );
            let signed = format!("{header}.{claims}");
            let sig = URL_SAFE_NO_PAD.encode(self.keypair.sign(signed.as_bytes()).as_ref());
            let assertion = format!("{signed}.{sig}");
            let origin = format!("http://{}", self.public_host);
            let body = json!({"assertion": assertion}).to_string();
            let (status, hdrs, resp_body) = self.request(
                "POST",
                "/__platform/session",
                &self.public_host,
                &[
                    ("Content-Type", "application/json"),
                    ("Origin", &origin),
                    ("Sec-Fetch-Site", "same-origin"),
                ],
                Some(&body),
            );
            assert_eq!(
                status, 200,
                "POST /__platform/session for {role} failed: {resp_body}"
            );
            extract_cookie(&hdrs).expect("Set-Cookie missing from /__platform/session")
        }
    }

    impl Drop for HttpRig {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            let _ = UnixStream::connect(crate::client::socket_path(&self.fx.state_dir));
            let _ = TcpStream::connect(("127.0.0.1", self.ui_port));
            self.jwks_server.unblock();
            for handle in self.threads.drain(..) {
                let _ = handle.join();
            }
        }
    }

    fn build_http_request(
        method: &str,
        path: &str,
        host: &str,
        headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> String {
        let mut out = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
        for (k, v) in headers {
            out.push_str(&format!("{k}: {v}\r\n"));
        }
        if let Some(payload) = body {
            out.push_str(&format!(
                "Content-Length: {}\r\n\r\n{payload}",
                payload.len()
            ));
        } else {
            out.push_str("\r\n");
        }
        out
    }

    fn extract_cookie(headers: &[(String, String)]) -> Option<String> {
        headers.iter().find_map(|(k, v)| {
            if k.eq_ignore_ascii_case("set-cookie") {
                v.split(';').next().map(|pair| pair.trim().to_string())
            } else {
                None
            }
        })
    }

    fn parse_http_response(raw: &[u8]) -> (u16, Vec<(String, String)>, Value) {
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("HTTP response missing header terminator");
        let head = std::str::from_utf8(&raw[..split]).unwrap();
        let mut lines = head.lines();
        let status_line = lines.next().unwrap();
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let headers: Vec<(String, String)> = lines
            .filter_map(|line| {
                let (k, v) = line.split_once(':')?;
                Some((k.trim().to_string(), v.trim().to_string()))
            })
            .collect();
        let body = serde_json::from_slice(&raw[split + 4..]).unwrap_or_else(|_| {
            Value::String(String::from_utf8_lossy(&raw[split + 4..]).into_owned())
        });
        (status, headers, body)
    }

    /// Child process running under an enrolled worker identity that opens raw
    /// loopback TCP requests against `ui::serve` so `/proc/net/tcp` ties its
    /// socket to the worker's `Attribution::Agent("w1")`.
    struct AgentHttpClient {
        child: Child,
        stdin: std::process::ChildStdin,
        stdout: BufReader<std::process::ChildStdout>,
    }

    impl AgentHttpClient {
        fn spawn(port: u16) -> Self {
            let script = r#"
import base64, socket, sys
port = int(sys.argv[1])
for line in sys.stdin:
    raw = base64.b64decode(line.strip())
    with socket.create_connection(("127.0.0.1", port), timeout=15) as s:
        s.sendall(raw)
        chunks = []
        while True:
            b = s.recv(65536)
            if not b:
                break
            chunks.append(b)
    sys.stdout.write(base64.b64encode(b"".join(chunks)).decode() + "\n")
    sys.stdout.flush()
"#;
            let mut child = Command::new("python3")
                .args(["-u", "-c", script, &port.to_string()])
                .env_clear()
                .env("PATH", path_var())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn python3 HTTP peer for enrolled agent");
            let stdin = child.stdin.take().unwrap();
            let stdout = BufReader::new(child.stdout.take().unwrap());
            Self {
                child,
                stdin,
                stdout,
            }
        }

        fn send(&mut self, raw_http: &str) -> (u16, Vec<(String, String)>, Value) {
            let std_b64 = base64::engine::general_purpose::STANDARD.encode(raw_http.as_bytes());
            writeln!(self.stdin, "{std_b64}").unwrap();
            self.stdin.flush().unwrap();
            let mut line = String::new();
            self.stdout.read_line(&mut line).unwrap();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(line.trim())
                .unwrap();
            parse_http_response(&bytes)
        }
    }

    impl Drop for AgentHttpClient {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[test]
    fn http_master_permission_routes_refuse_agent_anonymous_and_member_callers() {
        let rig = HttpRig::start();

        let ls_argv = vec!["ls".into(), rig.fx.repo_dir.to_string_lossy().into_owned()];
        let w1_req = request_record("perm-http-w1", "w1", &ls_argv, &rig.fx.repo_dir, "pending");
        let ctrl_req = request_record(
            "perm-http-ctrl",
            "master",
            &ls_argv,
            &rig.fx.repo_dir,
            "pending",
        );
        let doc_path = seed_master_perm_doc(
            &rig.fx.state_dir,
            json!({
                "requests": [w1_req, ctrl_req],
                "grants": [],
            }),
        );
        let w1_rule_path = master_perm::write_rules(
            &rig.fx.pm_dir,
            "w1",
            &[exact_allow_rule(
                "rule-http-w1",
                ls_argv.clone(),
                &rig.fx.repo_dir,
            )],
        )
        .unwrap();
        let master_rule_path = master_perm::write_rules(
            &rig.fx.pm_dir,
            "master",
            &[exact_allow_rule("rule-http-m", ls_argv, &rig.fx.repo_dir)],
        )
        .unwrap();

        let doc_before = fs::read(&doc_path).unwrap();
        let w1_rules_before = fs::read(&w1_rule_path).unwrap();
        let master_rules_before = fs::read(&master_rule_path).unwrap();

        let mut agent_peer = AgentHttpClient::spawn(rig.ui_port);
        rig.fx
            .bind_child_seat("w1", "worker", true, agent_peer.child.id());

        let (op_cookie, op_key) = rig.mint_local_operator_session();
        let member_cookie = rig.mint_public_board_cookie("usr_member", "member");
        let owner_cookie = rig.mint_public_board_cookie("usr_owner", "owner");

        let routes: [(&str, &str, Option<&str>); 8] = [
            ("GET", "/api/master/permissions", None),
            (
                "POST",
                "/api/master/permissions/perm-http-w1/allow-once",
                Some("{}"),
            ),
            (
                "POST",
                "/api/master/permissions/perm-http-w1/always",
                Some(r#"{"scope":"exact"}"#),
            ),
            (
                "POST",
                "/api/master/permissions/perm-http-w1/reject",
                Some(r#"{"dont_ask_again":true}"#),
            ),
            (
                "POST",
                "/api/master/permissions/perm-http-ctrl/allow-once",
                Some("{}"),
            ),
            (
                "POST",
                "/api/master/permissions/rules/rule-http-w1/revoke",
                Some("{}"),
            ),
            (
                "POST",
                "/api/master/permissions/rules/rule-http-m/revoke",
                Some("{}"),
            ),
            (
                "POST",
                "/api/master/permissions/perm-http-ctrl/reject",
                Some(r#"{"dont_ask_again":false}"#),
            ),
        ];

        let local_origin = format!("http://{}", rig.local_host);
        let public_origin = format!("http://{}", rig.public_host);

        fn request_headers<'a>(method: &str, origin: &'a str) -> Vec<(&'a str, &'a str)> {
            if method == "POST" {
                vec![
                    ("Content-Type", "application/json"),
                    ("X-Cadence-Board", "1"),
                    ("Origin", origin),
                    ("Sec-Fetch-Site", "same-origin"),
                ]
            } else {
                vec![]
            }
        }

        for (method, path, body) in routes {
            // 1. Anonymous caller refused without an operator session.
            let anon_hdrs = request_headers(method, &local_origin);
            let (st, _, resp) = rig.request(method, path, &rig.local_host, &anon_hdrs, body);
            if method == "GET" {
                assert_eq!(st, 401, "anonymous {method} {path} must return 401: {resp}");
                assert_eq!(
                    resp["code"],
                    json!("operator_session_required"),
                    "anonymous {method} {path} refused with unexpected code: {resp}"
                );
            } else {
                assert_eq!(st, 403, "anonymous {method} {path} must return 403: {resp}");
                assert_eq!(
                    resp["check"],
                    json!("operator_session_required"),
                    "anonymous {method} {path} refused with unexpected check: {resp}"
                );
            }

            // 2. PublicBoard signed-in member refused on member_role.
            let mut mem_hdrs = request_headers(method, &public_origin);
            mem_hdrs.push(("Cookie", &member_cookie));
            let (st, _, resp) = rig.request(method, path, &rig.public_host, &mem_hdrs, body);
            assert_eq!(st, 403, "member {method} {path} must return 403: {resp}");
            assert_eq!(
                resp["check"],
                json!("member_role"),
                "member {method} {path} refused with unexpected check: {resp}"
            );

            // 3. Enrolled worker TCP peer refused (GET hits read-session gate 401 before route,
            //    POST hits operator_only 403 even with valid browser write headers).
            let raw_agent = build_http_request(
                method,
                path,
                &rig.local_host,
                &request_headers(method, &local_origin),
                body,
            );
            let (st, _, resp) = agent_peer.send(&raw_agent);
            if method == "GET" {
                assert_eq!(st, 401, "agent w1 {method} {path} must return 401: {resp}");
                assert_eq!(
                    resp["code"],
                    json!("operator_session_required"),
                    "agent w1 {method} {path} refused with unexpected code: {resp}"
                );
            } else {
                assert_eq!(st, 403, "agent w1 {method} {path} must return 403: {resp}");
                assert_eq!(
                    resp["check"],
                    json!("operator_only"),
                    "agent w1 {method} {path} refused with unexpected check: {resp}"
                );
            }
        }

        // 4a. Enrolled worker presenting a stolen operator cookie on GET /api/master/permissions
        //     passes `admit_read`, hits `admit_operator_read`, is refused `session_from_agent`, and revokes it.
        let (op_cookie_get, op_key_get) = rig.mint_local_operator_session();
        let raw_stolen_get = build_http_request(
            "GET",
            "/api/master/permissions",
            &rig.local_host,
            &[
                ("Cookie", op_cookie_get.as_str()),
                ("X-Cadence-Session", op_key_get.as_str()),
            ],
            None,
        );
        let (st, _, resp) = agent_peer.send(&raw_stolen_get);
        assert_eq!(st, 403, "stolen cookie GET by w1 must return 403: {resp}");
        assert_eq!(resp["check"], json!("session_from_agent"), "{resp}");

        // 4b. Enrolled worker presenting a stolen operator cookie+session_key on POST is refused and revokes it.
        let mut stolen_hdrs = vec![
            ("Cookie", op_cookie.as_str()),
            ("X-Cadence-Session", op_key.as_str()),
            ("Content-Type", "application/json"),
            ("X-Cadence-Board", "1"),
            ("Origin", local_origin.as_str()),
            ("Sec-Fetch-Site", "same-origin"),
        ];
        let raw_stolen = build_http_request(
            "POST",
            "/api/master/permissions/perm-http-w1/allow-once",
            &rig.local_host,
            &stolen_hdrs,
            Some("{}"),
        );
        let (st, _, resp) = agent_peer.send(&raw_stolen);
        assert_eq!(st, 403, "stolen cookie by w1 must return 403: {resp}");
        assert_eq!(resp["check"], json!("session_from_agent"), "{resp}");

        // State files remain completely unchanged by all refused requests.
        assert_eq!(fs::read(&doc_path).unwrap(), doc_before);
        assert_eq!(fs::read(&w1_rule_path).unwrap(), w1_rules_before);
        assert_eq!(fs::read(&master_rule_path).unwrap(), master_rules_before);

        // Positive controls: fresh local operator session AND PublicBoard owner reach the handler.
        let (fresh_cookie, fresh_key) = rig.mint_local_operator_session();
        stolen_hdrs.clear();
        stolen_hdrs.push(("Cookie", fresh_cookie.as_str()));
        stolen_hdrs.push(("X-Cadence-Session", fresh_key.as_str()));
        let (st, _, body) = rig.request(
            "GET",
            "/api/master/permissions",
            &rig.local_host,
            &stolen_hdrs,
            None,
        );
        assert_eq!(
            st, 200,
            "local operator GET /api/master/permissions: {body}"
        );

        let (st, _, body) = rig.request(
            "GET",
            "/api/master/permissions",
            &rig.public_host,
            &[("Cookie", owner_cookie.as_str())],
            None,
        );
        assert_eq!(
            st, 200,
            "PublicBoard owner GET /api/master/permissions: {body}"
        );

        let (st, _, body) = rig.request(
            "POST",
            "/api/master/permissions/perm-http-ctrl/allow-once",
            &rig.public_host,
            &[
                ("Cookie", owner_cookie.as_str()),
                ("Content-Type", "application/json"),
                ("X-Cadence-Board", "1"),
                ("Origin", public_origin.as_str()),
                ("Sec-Fetch-Site", "same-origin"),
            ],
            Some("{}"),
        );
        assert_eq!(
            st, 200,
            "PublicBoard owner POST allow-once must pass gate and decide ctrl request: {body}"
        );
    }
}
