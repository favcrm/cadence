//! CAD-2215: scoped agent visibility races over the real daemon/store.
//!
//! Two scoped-read leaks are pinned behaviourally, with real store mutations
//! and pre-response barriers (never source text):
//!
//! 1. `agent_show`'s `turn_project`: an agent with a readable cwd reports its
//!    cwd project when idle, but hidden-only queued/running work used to make
//!    that field disappear. A scoped reader must not see that transition; the
//!    proven operator keeps the raw stored behaviour.
//! 2. `agent_show`/`agent_list` post-selection reproof: inserting an unrelated
//!    hidden issue/private job between selection and reproof used to change the
//!    whole issue/denial snapshot and refuse the read. The visible page/list
//!    must survive unchanged; a selected visible task/message that becomes
//!    hidden/denied must still refuse.
//!
//! The fixtures are a workspace-visible project and a private project, each
//! with a real git checkout so an agent's cwd resolves to one project. The
//! test-only selection hooks fire on the scoped path after selection and before
//! reproof; every mutation is a real SQLite writer called at that barrier.

use super::*;
use crate::test_seam::{scoped, Asserted};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn private_dir(path: &Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn git_init(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("init")
        .arg("-q")
        .status()
        .expect("git must be available for the cwd fixture");
    assert!(status.success(), "git init failed for {}", dir.display());
}

fn write_project(pm: &Path, key: &str, prefix: &str, issue: &str, extra: &str) {
    let project = pm.join(key);
    let issue_dir = project.join(issue);
    std::fs::create_dir_all(&issue_dir).unwrap();
    std::fs::write(
        project.join("project.yaml"),
        format!("key: {key}\nprefix: {prefix}\n{extra}"),
    )
    .unwrap();
    std::fs::write(
        issue_dir.join("issue.md"),
        format!(
            "---\nid: {issue}\ntitle: {issue} fixture\nstatus: backlog\n\
             priority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n"
        ),
    )
    .unwrap();
}

struct World {
    _state: tempfile::TempDir,
    pm: PathBuf,
    repo_open: PathBuf,
}

impl World {
    fn state(&self) -> &Path {
        self._state.path()
    }

    fn boot(&self) -> Arc<Shared> {
        self.boot_with(|_| {})
    }

    fn boot_with(&self, configure: impl FnOnce(&mut ServeOptions)) -> Arc<Shared> {
        let mut opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", self.pm.to_str().unwrap());
        configure(&mut opts);
        Shared::new(self.state(), &opts).unwrap()
    }
}

/// A workspace-visible project and a private project, each a real git
/// checkout, a scoped reader, a worker whose cwd is the readable repo, one open
/// task and one private task, and one visible completed message on the open
/// task.
fn world() -> World {
    let state = tempfile::tempdir().unwrap();
    private_dir(state.path());
    let pm = state.path().join("tracker");
    crate::issue::Pm::init(&pm).unwrap();
    std::fs::set_permissions(&pm, std::fs::Permissions::from_mode(0o700)).unwrap();
    let repo_open = state.path().join("repo-open");
    let repo_hidden = state.path().join("repo-hidden");
    git_init(&repo_open);
    git_init(&repo_hidden);
    write_project(
        &pm,
        "open",
        "OPEN",
        "OPEN-1",
        &format!("repos:\n  - path: {}\n", repo_open.display()),
    );
    write_project(&pm, "second", "SEC", "SEC-1", "");
    write_project(
        &pm,
        "hidden",
        "HID",
        "HID-1",
        &format!(
            "policy:\n  version: 1\n  visibility: private\nrepos:\n  - path: {}\n",
            repo_hidden.display()
        ),
    );
    let world = World {
        _state: state,
        pm,
        repo_open,
    };
    {
        let shared = world.boot();
        shared
            .store
            .register_agent(&NewAgent {
                alias: "reader",
                provider: "fake",
                endpoint_kind: "fake",
                role: "worker",
                cwd: world.state().to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        shared
            .store
            .register_agent(&NewAgent {
                alias: "worker",
                provider: "fake",
                endpoint_kind: "fake",
                role: "worker",
                cwd: world.repo_open.to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        let db = rusqlite::Connection::open(world.state().join("cadence.sqlite3")).unwrap();
        // Terminal history belongs to this worker registration, not an older
        // registration with the same alias.
        db.execute_batch(
            "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated)
             VALUES('open-job','Open','spec.md','manager','SEC-1','open',1,1),
                   ('hidden-job','Hidden','spec.md','manager','HID-1','open',2,2),
                   ('binding-job','Binding','spec.md','manager','OPEN-1','open',3,3);
             INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
             VALUES('open-task','open-job','Open','worker','running',1,1),
                   ('hidden-task','hidden-job','Hidden','worker','running',2,2),
                   ('binding-task','binding-job','Binding','worker','running',3,3);
             INSERT INTO messages(id,alias,body,source,state,task_id,issue,created)
             SELECT 'open-visible','worker','VISIBLE_OPEN_BODY','user','completed',
                    'open-task','SEC-1',created FROM agents WHERE alias='worker';",
        )
        .unwrap();
    }
    world
}

fn show(shared: &Arc<Shared>, who: Asserted, alias: &str) -> Result<Value> {
    let shared = Arc::clone(shared);
    scoped(who, || {
        shared.dispatch("agent_show", &json!({"alias": alias}), std::process::id())
    })
}

fn list(shared: &Arc<Shared>, who: Asserted) -> Result<Value> {
    let shared = Arc::clone(shared);
    scoped(who, || {
        shared.dispatch("agent_list", &json!({}), std::process::id())
    })
}

fn append_message(state: &Path, id: &str, issue: &str, task: &str, body: &str) {
    let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO messages(id,alias,body,source,state,task_id,issue,created)
         VALUES(?1,'worker',?2,'user','running',?3,?4,2)",
        rusqlite::params![id, body, task, issue],
    )
    .unwrap();
}

/// Unrelated private-job activity for the show/list barriers: a new job and
/// task under the existing private issue, with one settled request owner, so
/// the denial snapshot grows while no selected row changes.
fn insert_unrelated_private_job(state: &Path) {
    let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    db.execute_batch(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated)
         VALUES('hidden-job-2','Hidden 2','spec.md','manager','HID-1','open',9,9);
         INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
         VALUES('hidden-task-2','hidden-job-2','Hidden 2','manager','running',9,9);",
    )
    .unwrap();
    db.execute(
        "INSERT INTO assist_requests(org,sub,request_key,job,created)
         VALUES('cadence-org','owner','req-hidden-2','hidden-job-2',1)",
        [],
    )
    .unwrap();
}

/// Rebind the selected visible job to member-private provenance (single owner,
/// PM and assignee are neither the reader nor its assistant), so the selected
/// task/message stops being visible to the reader.
fn make_open_job_private(state: &Path) {
    let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO assist_requests(org,sub,request_key,job,created)
         VALUES('cadence-org','owner','req-open','open-job',1)",
        [],
    )
    .unwrap();
}

/// Move the selected task-binding's job to the private project: the task now
/// resolves to HID-1 (unreadable), while the serialized page still carries the
/// stale OPEN-1 issue for that binding.
fn rebind_binding_job(state: &Path) {
    let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    db.execute(
        "UPDATE jobs SET issue_id='HID-1' WHERE id='binding-job'",
        [],
    )
    .unwrap();
}

/// The isolated stale-lane-issues race: move the ONLY selected OPEN-1 task to
/// the readable SEC-1, then remove OPEN-1 from the tracker so the serialized
/// `lane_issues` entry is stale while every current selected task stays
/// readable (so a current-only task reproof passes).
fn rebind_binding_task_and_retire_open_issue(pm: &Path, state: &Path) {
    let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    db.execute(
        "UPDATE jobs SET issue_id='SEC-1' WHERE id='binding-job'",
        [],
    )
    .unwrap();
    drop(db);
    std::fs::rename(pm.join("open").join("OPEN-1"), state.join("retired-OPEN-1"))
        .expect("the OPEN-1 issue must leave the tracker");
}

fn no_hidden_needles(value: &Value, what: &str) {
    let rendered = value.to_string();
    for needle in ["HID-1", "hidden-job", "hidden-task", "hidden-job-2"] {
        assert!(
            !rendered.contains(needle),
            "{what} exposed hidden activity {needle}: {rendered}"
        );
    }
}

/// The visible row shape both worlds can compare: stable, reader-visible
/// fields only (never stored state, timings or liveness).
fn stable_agents(list: &Value) -> Vec<Value> {
    let mut rows = list["agents"]
        .as_array()
        .unwrap_or_else(|| panic!("agent list has no agents array: {list}"))
        .iter()
        .map(|agent| {
            json!({
                "alias": agent["alias"],
                "state": agent["state"],
                "tasks": agent["tasks"],
                "lane_issues": agent["lane_issues"],
                "capabilities": agent["capabilities"],
                "turn_project": agent["turn_project"],
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left["alias"]
            .as_str()
            .unwrap_or_default()
            .cmp(right["alias"].as_str().unwrap_or_default())
    });
    rows
}

/// The chosen scoped contract: `turn_project` is omitted entirely (absent or
/// null), never a cwd fallback and never a hidden project.
fn assert_turn_project_omitted(page: &Value, what: &str) {
    match page["agent"].get("turn_project") {
        None | Some(Value::Null) => {}
        Some(other) => panic!("{what} exposed turn_project to a scoped reader: {other} in {page}"),
    }
}

#[test]
fn scoped_show_does_not_reveal_hidden_work_moving_turn_project() {
    let world = world();
    let shared = world.boot();

    // The chosen safe scoped contract is to OMIT `turn_project`: a scoped
    // reader cannot be told a project while an unseen turn may be running in
    // another one. It is omitted while idle and after hidden work arrives, so
    // the presence/absence of the field never reveals hidden activity.
    let idle = show(&shared, Asserted::Agent("reader".to_string()), "worker")
        .expect("the readable worker must show");
    assert_turn_project_omitted(&idle, "idle scoped show");

    // Operator control keeps the raw stored behaviour while idle.
    let operator_idle = show(&shared, Asserted::Operator, "worker").expect("operator show");
    assert_eq!(
        operator_idle["agent"]["turn_project"],
        json!("open"),
        "the operator lost the raw idle turn project: {operator_idle}"
    );

    // Hidden-only running work arrives: the raw resolver now points at the
    // private project, but the scoped reader still sees no field at all.
    append_message(
        world.state(),
        "hidden-running",
        "HID-1",
        "hidden-task",
        "HIDDEN_RUNNING_BODY",
    );
    let busy = show(&shared, Asserted::Agent("reader".to_string()), "worker")
        .expect("the scoped show must still succeed after hidden work starts");
    assert_turn_project_omitted(&busy, "scoped show after hidden work");
    no_hidden_needles(&busy, "scoped show");

    // The operator control keeps the raw stored behaviour.
    let operator = show(&shared, Asserted::Operator, "worker").expect("operator show");
    assert_eq!(
        operator["agent"]["turn_project"],
        json!("hidden"),
        "the operator lost the raw turn project: {operator}"
    );
}

#[test]
fn scoped_show_survives_an_unrelated_hidden_job_insertion_before_reproof() {
    let world = world();
    let state = world.state().to_path_buf();
    let control = world.boot();
    let control_page = show(&control, Asserted::Agent("reader".to_string()), "worker")
        .expect("the control scoped show");

    let fired = Arc::new(AtomicBool::new(false));
    let hook_state = state.clone();
    let shared = world.boot_with(|opts| {
        let fired = Arc::clone(&fired);
        let hook_state = hook_state.clone();
        opts.agent_show_selected_hook = Some(Arc::new(move |alias: &str| {
            if alias != "worker" || fired.swap(true, Ordering::SeqCst) {
                return;
            }
            insert_unrelated_private_job(&hook_state);
        }));
    });

    let page = show(&shared, Asserted::Agent("reader".to_string()), "worker")
        .expect("an unrelated hidden insertion must not refuse the scoped show");
    assert!(
        fired.load(Ordering::SeqCst),
        "the show selection hook never fired"
    );
    assert_eq!(
        page["agent"]["turn_project"], control_page["agent"]["turn_project"],
        "hidden insertion changed the visible turn_project: {page}"
    );
    assert_eq!(
        page["agent"]["tasks"], control_page["agent"]["tasks"],
        "hidden insertion changed the visible tasks: {page}"
    );
    no_hidden_needles(&page, "scoped show after hidden insertion");
    assert!(
        page.to_string().contains("open-visible"),
        "the visible message disappeared: {page}"
    );
}

#[test]
fn scoped_show_refuses_when_a_selected_job_becomes_private() {
    let world = world();
    let state = world.state().to_path_buf();
    let fired = Arc::new(AtomicBool::new(false));
    let hook_state = state.clone();
    let shared = world.boot_with(|opts| {
        let fired = Arc::clone(&fired);
        let hook_state = hook_state.clone();
        opts.agent_show_selected_hook = Some(Arc::new(move |alias: &str| {
            if alias != "worker" || fired.swap(true, Ordering::SeqCst) {
                return;
            }
            make_open_job_private(&hook_state);
        }));
    });

    let refused = show(&shared, Asserted::Agent("reader".to_string()), "worker")
        .expect_err("a selected job that became private must refuse the page");
    assert!(
        fired.load(Ordering::SeqCst),
        "the show selection hook never fired"
    );
    assert_ne!(refused.kind(), "internal", "{refused:?}");
    let rendered = format!("{refused} {refused:?}");
    for needle in ["open-visible", "VISIBLE_OPEN_BODY", "HID-1", "hidden-job"] {
        assert!(
            !rendered.contains(needle),
            "the refusal leaked {needle}: {rendered}"
        );
    }
}

/// A selected `task_bindings` row with NO selected message: the hook rebinds
/// that task's job to an unreadable project. The serialized page still names
/// the old readable issue, so a reproof that trusts the serialized issue would
/// leak the task; the current task -> job -> issue binding must refuse.
#[test]
fn scoped_show_refuses_when_a_selected_task_binding_rebinds_to_an_unreadable_project() {
    let world = world();
    let state = world.state().to_path_buf();

    // Control: the binding is selected, and no message selects that task.
    let control = world.boot();
    let control_page = show(&control, Asserted::Agent("reader".to_string()), "worker")
        .expect("the control scoped show");
    let bindings = control_page["task_bindings"]
        .as_array()
        .unwrap_or_else(|| panic!("the control show has no task_bindings: {control_page}"));
    assert!(
        bindings
            .iter()
            .any(|binding| binding["id"] == json!("binding-task")),
        "the fixture binding task is not selected: {control_page}"
    );
    assert!(
        control_page["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|message| message["task_id"] != json!("binding-task")),
        "the fixture binding task must have no selected message: {control_page}"
    );

    let fired = Arc::new(AtomicBool::new(false));
    let hook_state = state.clone();
    let shared = world.boot_with(|opts| {
        let fired = Arc::clone(&fired);
        let hook_state = hook_state.clone();
        opts.agent_show_selected_hook = Some(Arc::new(move |alias: &str| {
            if alias != "worker" || fired.swap(true, Ordering::SeqCst) {
                return;
            }
            rebind_binding_job(&hook_state);
        }));
    });

    let refused = show(&shared, Asserted::Agent("reader".to_string()), "worker")
        .expect_err("a selected task binding that rebinds to an unreadable project must refuse");
    assert!(
        fired.load(Ordering::SeqCst),
        "the show selection hook never fired"
    );
    assert_ne!(refused.kind(), "internal", "{refused:?}");
    let rendered = format!("{refused} {refused:?}");
    for needle in [
        "binding-task",
        "binding-job",
        "HID-1",
        "open-task",
        "VISIBLE_OPEN_BODY",
    ] {
        assert!(
            !rendered.contains(needle),
            "the refusal leaked {needle}: {rendered}"
        );
    }
}

#[test]
fn scoped_list_survives_an_unrelated_hidden_job_insertion_before_reproof() {
    let world = world();
    let state = world.state().to_path_buf();
    let control = world.boot();
    let control_rows = stable_agents(
        &list(&control, Asserted::Agent("reader".to_string())).expect("the control scoped list"),
    );

    let fired = Arc::new(AtomicBool::new(false));
    let hook_state = state.clone();
    let shared = world.boot_with(|opts| {
        let fired = Arc::clone(&fired);
        let hook_state = hook_state.clone();
        opts.agent_list_selected_hook = Some(Arc::new(move || {
            if fired.swap(true, Ordering::SeqCst) {
                return;
            }
            insert_unrelated_private_job(&hook_state);
        }));
    });

    let page = list(&shared, Asserted::Agent("reader".to_string()))
        .expect("an unrelated hidden insertion must not refuse the scoped list");
    assert!(
        fired.load(Ordering::SeqCst),
        "the list selection hook never fired"
    );
    assert_eq!(
        stable_agents(&page),
        control_rows,
        "hidden insertion changed the visible list: {page}"
    );
    assert_eq!(
        page["live"],
        Value::Null,
        "scoped live is not unavailable: {page}"
    );
    no_hidden_needles(&page, "scoped list after hidden insertion");
}

#[test]
fn scoped_list_refuses_when_a_selected_task_becomes_private() {
    let world = world();
    let state = world.state().to_path_buf();
    let fired = Arc::new(AtomicBool::new(false));
    let hook_state = state.clone();
    let shared = world.boot_with(|opts| {
        let fired = Arc::clone(&fired);
        let hook_state = hook_state.clone();
        opts.agent_list_selected_hook = Some(Arc::new(move || {
            if fired.swap(true, Ordering::SeqCst) {
                return;
            }
            make_open_job_private(&hook_state);
        }));
    });

    let refused = list(&shared, Asserted::Agent("reader".to_string()))
        .expect_err("a selected task that became private must refuse the list");
    assert!(
        fired.load(Ordering::SeqCst),
        "the list selection hook never fired"
    );
    assert_ne!(refused.kind(), "internal", "{refused:?}");
    let rendered = format!("{refused} {refused:?}");
    for needle in ["open-task", "open-visible", "VISIBLE_OPEN_BODY", "HID-1"] {
        assert!(
            !rendered.contains(needle),
            "the refusal leaked {needle}: {rendered}"
        );
    }
}

/// Replace the scoped caller's durable registration through the real store
/// path: stop it, remove the row (force), then register the same alias again,
/// which mints a NEW durable uid while the project grants (policy spelling) and
/// the worker's visible work stay untouched.
fn replace_reader_registration(store: &crate::store::Store, state: &Path) {
    let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    db.execute(
        "UPDATE agents SET state='stopped', endpoint=NULL WHERE alias='reader'",
        [],
    )
    .unwrap();
    drop(db);
    store
        .remove_agent(
            "reader",
            true,
            &json!({"by": "operator", "by_kind": "operator"}),
        )
        .expect("the fixture reader must be removable");
    store
        .register_agent(&NewAgent {
            alias: "reader",
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: state.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .expect("the replacement registration");
}

#[test]
fn scoped_show_refuses_when_the_callers_registration_is_replaced_before_reproof() {
    let world = world();
    let state = world.state().to_path_buf();
    let control = world.boot();
    let control_page = show(&control, Asserted::Agent("reader".to_string()), "worker")
        .expect("the control scoped show");
    assert!(
        control_page.to_string().contains("open-visible"),
        "the control fixture lost its visible message: {control_page}"
    );

    // A Store handle outlives the setup Shared; the hook uses the REAL remove
    // and register path, not a raw transaction.
    let store = {
        let setup = world.boot();
        Arc::clone(&setup.store)
    };
    let before = store.agent("reader").expect("the original reader row").uid;
    let fired = Arc::new(AtomicBool::new(false));
    let hook_store = Arc::clone(&store);
    let hook_state = state.clone();
    let shared = world.boot_with(|opts| {
        let fired = Arc::clone(&fired);
        opts.agent_show_selected_hook = Some(Arc::new(move |alias: &str| {
            if alias != "worker" || fired.swap(true, Ordering::SeqCst) {
                return;
            }
            replace_reader_registration(&hook_store, &hook_state);
        }));
    });

    let refused = show(&shared, Asserted::Agent("reader".to_string()), "worker")
        .expect_err("a replaced caller registration must refuse the scoped show");
    assert!(
        fired.load(Ordering::SeqCst),
        "the show selection hook never fired"
    );
    assert_eq!(refused.kind(), "rejected", "{refused:?}");
    let rendered = format!("{refused} {refused:?}");
    for needle in ["VISIBLE_OPEN_BODY", "open-task", "binding-task", "worker"] {
        assert!(
            !rendered.contains(needle),
            "the refusal leaked {needle}: {rendered}"
        );
    }

    let after = shared
        .store
        .agent("reader")
        .expect("the replacement reader row")
        .uid;
    assert_ne!(
        before, after,
        "the fixture did not replace the registration"
    );

    // The replacement keeps the same project grants and the same visible work:
    // a fresh read under the stable new registration succeeds.
    let fresh = show(&shared, Asserted::Agent("reader".to_string()), "worker")
        .expect("the replacement registration keeps its grants");
    assert!(
        fresh.to_string().contains("open-visible"),
        "the replacement lost the visible work: {fresh}"
    );
}

#[test]
fn scoped_list_refuses_when_the_callers_registration_is_replaced_before_reproof() {
    let world = world();
    let state = world.state().to_path_buf();
    let control = world.boot();
    let control_page =
        list(&control, Asserted::Agent("reader".to_string())).expect("the control scoped list");
    assert!(
        control_page.to_string().contains("worker"),
        "the control fixture lost the worker row: {control_page}"
    );

    let store = {
        let setup = world.boot();
        Arc::clone(&setup.store)
    };
    let before = store.agent("reader").expect("the original reader row").uid;
    let fired = Arc::new(AtomicBool::new(false));
    let hook_store = Arc::clone(&store);
    let hook_state = state.clone();
    let shared = world.boot_with(|opts| {
        let fired = Arc::clone(&fired);
        opts.agent_list_selected_hook = Some(Arc::new(move || {
            if fired.swap(true, Ordering::SeqCst) {
                return;
            }
            replace_reader_registration(&hook_store, &hook_state);
        }));
    });

    let refused = list(&shared, Asserted::Agent("reader".to_string()))
        .expect_err("a replaced caller registration must refuse the scoped list");
    assert!(
        fired.load(Ordering::SeqCst),
        "the list selection hook never fired"
    );
    assert_eq!(refused.kind(), "rejected", "{refused:?}");
    let rendered = format!("{refused} {refused:?}");
    for needle in ["VISIBLE_OPEN_BODY", "open-task", "binding-task", "worker"] {
        assert!(
            !rendered.contains(needle),
            "the refusal leaked {needle}: {rendered}"
        );
    }

    let after = shared
        .store
        .agent("reader")
        .expect("the replacement reader row")
        .uid;
    assert_ne!(
        before, after,
        "the fixture did not replace the registration"
    );

    let fresh = list(&shared, Asserted::Agent("reader".to_string()))
        .expect("the replacement registration keeps its grants");
    assert!(
        fresh.to_string().contains("worker"),
        "the replacement lost the visible worker row: {fresh}"
    );
}

fn agent_row<'a>(page: &'a Value, alias: &str) -> &'a Value {
    page["agents"]
        .as_array()
        .unwrap_or_else(|| panic!("agent list has no agents array: {page}"))
        .iter()
        .find(|agent| agent["alias"] == json!(alias))
        .unwrap_or_else(|| panic!("agent '{alias}' is missing from the list: {page}"))
}

fn string_list(value: &Value, field: &str, what: &str) -> Vec<String> {
    value[field]
        .as_array()
        .unwrap_or_else(|| panic!("{what} has no {field} array: {value}"))
        .iter()
        .map(|item| {
            item.as_str()
                .unwrap_or_else(|| panic!("{what} {field} has a non-string item: {value}"))
                .to_string()
        })
        .collect()
}

/// The isolated stale-`lane_issues` race: exactly one selected task binds the
/// readable OPEN-1 at selection; at the barrier it moves to the still-readable
/// SEC-1 and OPEN-1 leaves the tracker. A current-only task reproof passes,
/// but the serialized lane issue is now hidden, so the list must refuse rather
/// than leak it.
#[test]
fn scoped_list_refuses_when_a_selected_lane_issue_becomes_hidden_while_the_task_stays_readable() {
    let world = world();
    let state = world.state().to_path_buf();
    let pm = world.pm.clone();

    // Control preconditions: exactly one selected task is OPEN-1-bound
    // (`binding-task`), no other selected binding is, and no running message
    // is bound to OPEN-1, so only the stale lane issue can be the leak.
    let control = world.boot();
    let control_page =
        list(&control, Asserted::Agent("reader".to_string())).expect("the control scoped list");
    let worker = agent_row(&control_page, "worker");
    let tasks = string_list(worker, "tasks", "the control worker row");
    assert!(
        tasks.iter().any(|task| task == "binding-task"),
        "{control_page}"
    );
    assert!(
        tasks.iter().any(|task| task == "open-task"),
        "{control_page}"
    );
    let lane_issues = string_list(worker, "lane_issues", "the control worker row");
    assert_eq!(
        lane_issues
            .iter()
            .filter(|issue| *issue == "OPEN-1")
            .count(),
        1,
        "the fixture must serialize exactly one OPEN-1 lane issue: {control_page}"
    );
    let task_refs: Vec<&str> = tasks.iter().map(String::as_str).collect();
    let bindings = control
        .store
        .task_issue_bindings(&task_refs)
        .expect("the selected task bindings");
    let open_bound = task_refs
        .iter()
        .copied()
        .filter(|task| bindings.get(*task).and_then(|issue| issue.as_deref()) == Some("OPEN-1"))
        .collect::<Vec<_>>();
    assert_eq!(
        open_bound,
        vec!["binding-task"],
        "another selected task bound OPEN-1 would mask the lane_issues race: {control_page}"
    );
    let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    let running_open: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE alias='worker' \
             AND state IN ('queued','running','submitting') AND issue='OPEN-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        running_open, 0,
        "a running OPEN-1 message would mask the lane_issues race"
    );
    drop(db);

    let fired = Arc::new(AtomicBool::new(false));
    let hook_pm = pm.clone();
    let hook_state = state.clone();
    let shared = world.boot_with(|opts| {
        let fired = Arc::clone(&fired);
        opts.agent_list_selected_hook = Some(Arc::new(move || {
            if fired.swap(true, Ordering::SeqCst) {
                return;
            }
            rebind_binding_task_and_retire_open_issue(&hook_pm, &hook_state);
        }));
    });

    let refused = list(&shared, Asserted::Agent("reader".to_string()))
        .expect_err("a serialized lane issue that became hidden must refuse the list");
    assert!(
        fired.load(Ordering::SeqCst),
        "the list selection hook never fired"
    );
    assert_eq!(refused.kind(), "rejected", "{refused:?}");
    let rendered = format!("{refused} {refused:?}");
    for needle in [
        "OPEN-1",
        "open-task",
        "binding-task",
        "worker",
        "VISIBLE_OPEN_BODY",
    ] {
        assert!(
            !rendered.contains(needle),
            "the refusal leaked {needle}: {rendered}"
        );
    }

    // The race left every selected task readable (both now SEC-1): a fresh
    // read succeeds and can only carry the current lane issue.
    let fresh =
        list(&shared, Asserted::Agent("reader".to_string())).expect("the current state must list");
    let worker = agent_row(&fresh, "worker");
    let lanes = string_list(worker, "lane_issues", "the fresh worker row");
    assert!(
        !lanes.iter().any(|issue| issue == "OPEN-1"),
        "the fresh list still carries the retired OPEN-1: {fresh}"
    );
    assert!(
        lanes.iter().all(|issue| issue == "SEC-1") && lanes.iter().any(|issue| issue == "SEC-1"),
        "the fresh list lost the current lane issue: {fresh}"
    );
    assert!(
        string_list(worker, "tasks", "the fresh worker row")
            .iter()
            .any(|task| task == "binding-task"),
        "the fresh list lost the rebounded task: {fresh}"
    );
}
