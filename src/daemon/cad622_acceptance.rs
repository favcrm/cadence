//! ACCEPTANCE CHECK (CAD-622): an app's own team agents cannot author
//! changes to that app (the self-escalation guard).
//!
//! Written from the ticket, not from the implementation. Operator
//! decision (2026-09-26): app changes are ordinary work — the master or
//! any connected agent may author them (plan → branch → independent
//! review → merge → `cadence app update` → operator re-approval for
//! structural changes). The one refusal is the app's own team: an agent
//! the operator assigned to an installation's team (the saved roles, or
//! a run's frozen role inputs) never authors a change to that app's
//! source, and never reviews one.
//!
//! Cases, each with a positive control so a test cannot pass by
//! refusing everything: a team agent dispatched to edit its own app is
//! refused before anything is created; a non-team agent is allowed;
//! renaming the role key does not launder the author (membership is by
//! alias); editing the app through a ticket of another project is
//! refused; a plan naming the app's source is refused to its team agent
//! and admitted for another agent; the review loop refuses a done
//! report whose PR's own changed files touch the app the worker is on;
//! an app's team reviewer is never routed the review and cannot file a
//! verdict if it was; and an unreadable diff flags the guard rather
//! than passing silently.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use super::*;
use crate::issue::{self, app};
use crate::store::{NewAgent, Store};
use crate::test_seam::{scoped, Asserted};

const PROJECT: &str = "widgets";
const OTHER: &str = "other";
const ISSUE: &str = "CAD-622";
const APP: &str = "social-content";
const REPO: &str = "acme/widgets";
const PR: u64 = 622;
const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

fn pr_url() -> String {
    format!("https://github.com/{REPO}/pull/{PR}")
}

/// The `gh` every loop test uses: `pr view` answers this PR at `HEAD`
/// (or whatever `head` names) with the app's file in the diff. Any
/// other call is a failure the test should see.
fn write_gh(dir: &Path) {
    let script = format!(
        r##"#!/bin/sh
if [ "$1" = pr ] && [ "$2" = view ]; then
  printf '{{"number":{PR},"state":"OPEN","headRefOid":"{HEAD}","baseRefName":"main","baseRefOid":"{HEAD}","changedFiles":1,"additions":2,"deletions":0,"files":[{{"path":"apps/{APP}/app.md"}}]}}\n'
  exit 0
fi
echo "unhandled gh fixture: $*" >&2
exit 1
"##
    );
    let gh = dir.join("gh");
    std::fs::write(&gh, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A private tracker with one project, one issue, one installed app and
/// its saved team, plus an in-process daemon with a scripted `gh`.
struct Fx {
    _root: tempfile::TempDir,
    pm_dir: PathBuf,
    state: PathBuf,
    shared: Arc<Shared>,
    /// The issue the dispatch cases work on — the ticket's own project.
    issue: String,
}

impl Fx {
    /// `team` is the saved team on the install record; `reviewers` are
    /// the registered reviewer-role agents (all idle).
    fn new(team: &[(&str, &str)], reviewers: &[&str]) -> Self {
        Self::with_extra_project(team, reviewers, false)
    }

    /// With `other`, a second project exists and the issue lives there —
    /// the "edit the app through a different project" bypass.
    fn with_extra_project(team: &[(&str, &str)], reviewers: &[&str], other: bool) -> Self {
        let root = tempfile::Builder::new().prefix("c622d").tempdir().unwrap();
        let pm_dir = root.path().join("pm");
        let pm = issue::Pm::init(&pm_dir).unwrap();
        let checkout = root.path().join("widgets");
        std::fs::create_dir_all(&checkout).unwrap();
        for args in [
            vec!["init", "-q"],
            vec![
                "remote",
                "add",
                "origin",
                "https://github.com/acme/widgets.git",
            ],
        ] {
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(&checkout)
                .args(&args)
                .status()
                .unwrap()
                .success());
        }
        issue::write::project_add(
            &pm,
            PROJECT,
            "CAD",
            &[checkout.to_str().unwrap().into()],
            &[],
            &[],
            None,
        )
        .unwrap();
        let (home, ticket) = if other {
            issue::write::project_add(&pm, OTHER, "OTH", &[], &[], &[], None).unwrap();
            (OTHER, "OTH-1")
        } else {
            (PROJECT, ISSUE)
        };
        issue::write::new_issue(
            &pm,
            &checkout,
            Some(home),
            "An app change",
            None,
            None,
            &[],
            None,
            None,
            &[],
            Some(ticket),
            None,
            "operator",
        )
        .unwrap();
        install_app(&pm, team);
        write_gh(root.path());
        let opts = crate::daemon::ServeOptions {
            test_seam: true,
            delivery_gh: Some(root.path().join("gh")),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
        let shared = Shared::new(root.path(), &opts).unwrap();
        let cwd = root.path().to_str().unwrap().to_string();
        let mut agents: Vec<(&str, &str)> = vec![("lead", "pm"), ("writer", "worker")];
        agents.extend(reviewers.iter().map(|r| (*r, "reviewer")));
        for (alias, role) in agents {
            shared
                .store
                .register_agent(&NewAgent {
                    alias,
                    provider: "fake",
                    endpoint_kind: "fake",
                    role,
                    cwd: &cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params: (alias == "writer").then_some(r#"{"upstream":"lead"}"#),
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
            shared.store.set_agent_state(alias, "idle", None).unwrap();
        }
        let state = root.path().to_path_buf();
        Fx {
            _root: root,
            pm_dir,
            state,
            shared,
            issue: ticket.to_string(),
        }
    }

    fn pm(&self) -> crate::issue::Pm {
        crate::issue::Pm::at(&self.pm_dir).unwrap()
    }

    /// Open the delivery loop for `worker` at `HEAD` with a done report
    /// already filed — the state `route_delivery` judges.
    fn seed_loop(&self, worker: &str) {
        let mut rec = delivery::Record::new(ISSUE, PROJECT, worker, 0);
        rec.dispatched_at = 0;
        rec.pr = Some(pr_url());
        let mut records = std::collections::BTreeMap::new();
        records.insert(ISSUE.to_string(), rec);
        delivery::save(&self.state, &records).unwrap();
        self.done_report(worker);
    }

    fn done_report(&self, worker: &str) {
        let dir = self.pm_dir.join(PROJECT).join(ISSUE).join("reports");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("19700101T000000Z-done.md"),
            format!(
                "---\nschema: cadence.report/2\nkind: done\ntask: {ISSUE}\nagent: {worker}\n\
                 sha: {HEAD}\npr: {}\n---\n\n## Expected\n\nx\n\n## Evidence\n\nx\n\n\
                 ## Cause\n\nx\n\n## Correction\n\nx\n\n## Lesson\n\nx\n\n## Next\n\nx\n",
                pr_url()
            ),
        )
        .unwrap();
    }

    fn record(&self) -> delivery::Record {
        delivery::load(&self.state)
            .unwrap()
            .get(ISSUE)
            .cloned()
            .expect("fixture record")
    }

    fn save_record(&self, rec: delivery::Record) {
        let mut records = delivery::load(&self.state).unwrap();
        records.insert(ISSUE.to_string(), rec);
        delivery::save(&self.state, &records).unwrap();
    }

    fn route(&self) -> Result<()> {
        self.shared.route_delivery().map(|_| ())
    }

    fn why(&self) -> String {
        self.shared
            .store
            .last_event_of(Store::DAEMON_STREAM, &["review_done_refused"])
            .unwrap()
            .map(|e| e.payload["why"].as_str().unwrap_or_default().to_string())
            .unwrap_or_default()
    }
}

/// `apps/<APP>/` with a one-workflow bundle and the saved team.
fn install_app(pm: &issue::Pm, team: &[(&str, &str)]) {
    let dir = pm.dir.join(PROJECT).join(app::DIR);
    std::fs::create_dir_all(dir.join(APP).join("workflows")).unwrap();
    std::fs::write(
        dir.join(APP).join(app::MANIFEST),
        format!(
            "---\napp: {APP}\ntitle: Social content\nversion: '1'\nneeds:\n  connections: []\n---\n\nGuide\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join(APP).join("workflows").join("post.md"),
        "---\ntitle: \"Post\"\ngoal: \"Write one post\"\ninputs:\n  writer: { ask: \"Who writes?\" }\n---\n\n## Draft\nagent: {{writer}}\n\nWrite it.\n\n### Acceptance\n- [ ] written\n",
    )
    .unwrap();
    let record = app::Record {
        schema: 1,
        app: APP.to_string(),
        install_id: format!("{PROJECT}-{APP}"),
        source: app::Source::Path {
            path: format!("tests/fixtures/apps/{APP}"),
        },
        bindings: Default::default(),
        team: team
            .iter()
            .map(|(role, alias)| (role.to_string(), alias.to_string()))
            .collect(),
        installed_at: "2026-01-01T00:00:00Z".into(),
        installed_by: "operator".into(),
        updated_at: None,
        removed: None,
        restore_after: None,
        purge_after: None,
        catalogue: None,
    };
    std::fs::write(
        dir.join(format!("{APP}.yaml")),
        serde_yaml::to_string(&record).unwrap(),
    )
    .unwrap();
}

/// A workspace installation: `.apps/installations/<id>/record.yaml` with
/// its team in the daemon's `app-teams.json`.
fn workspace_install_from(fx: &Fx, id: &str, url: &str, roles: &[(&str, &str)]) {
    let dir = fx.pm_dir.join(".apps").join("installations").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let record = app::Record {
        schema: 1,
        app: APP.to_string(),
        install_id: id.to_string(),
        source: app::Source::Git {
            url: url.to_string(),
            sha: "0".repeat(40),
            dir: None,
        },
        bindings: Default::default(),
        team: Default::default(),
        installed_at: "2026-01-01T00:00:00Z".into(),
        installed_by: "operator".into(),
        updated_at: None,
        removed: None,
        restore_after: None,
        purge_after: None,
        catalogue: None,
    };
    std::fs::write(
        dir.join("record.yaml"),
        serde_yaml::to_string(&record).unwrap(),
    )
    .unwrap();
    let mut teams: Value = std::fs::read_to_string(fx.state.join("app-teams.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({"schema": 1, "teams": {}}));
    teams["teams"][id] = json!({
        "owner_pm": "lead",
        "roles": roles.iter().map(|(r, a)| (r.to_string(), a.to_string()))
            .collect::<std::collections::BTreeMap<_, _>>(),
        "revision": 1,
    });
    std::fs::write(fx.state.join("app-teams.json"), teams.to_string()).unwrap();
}

fn workspace_install(fx: &Fx, id: &str, roles: &[(&str, &str)]) {
    workspace_install_from(fx, id, "https://github.com/acme/app-repo", roles)
}

fn dispatch_args(to: &str, repo: PathBuf) -> crate::issue::dispatch::DispatchArgs {
    crate::issue::dispatch::DispatchArgs {
        to: to.into(),
        note: None,
        name: None,
        base: None,
        repo: Some(repo),
        reply_to: Some("lead".into()),
        summary: None,
        job_spec: None,
        no_lessons: true,
        force: false,
        take_over: None,
    }
}

fn set_paths(pm: &issue::Pm, id: &str, paths: &str) {
    crate::issue::edit::edit(
        pm,
        id,
        &crate::issue::edit::EditSpec {
            set: vec![format!("paths={paths}")],
            ..Default::default()
        },
        "operator",
        None,
    )
    .unwrap();
}

/// Case 1: A team agent dispatched to edit its own app is refused, with the
/// plain reason, and nothing is created.
#[test]
fn cad622_a_team_agent_cannot_be_dispatched_to_edit_its_own_app() {
    let fx = Fx::new(&[("writer", "writer")], &[]);
    let pm = fx.pm();
    set_paths(&pm, &fx.issue, &format!("apps/{APP}/app.md,src/other.rs"));
    let err = crate::issue::dispatch::run(
        &pm,
        &fx.issue,
        &dispatch_args("writer", fx._root.path().join("widgets")),
        "operator",
        &fx.state,
        None,
        crate::issue::dispatch::DispatchOrigin::Explicit,
    )
    .expect_err("a team agent must not be dispatched to edit its own app");
    let why = err.to_string();
    assert!(
        why.contains("writer is on app 'social-content's team (project widgets)")
            && why.contains("cannot author a change to that app's source (apps/social-content/)"),
        "the refusal names the agent, the app and the path: {why}"
    );
    assert!(
        !fx._root.path().join("widgets/.cadence").exists(),
        "a refused dispatch creates no worktree"
    );
}

/// Case 2: A non-team agent is allowed: the guard refuses the app's own team,
/// not everyone. The dispatch gets past the guard to the next check.
#[test]
fn cad622_an_agent_off_the_team_may_be_dispatched_to_edit_the_app() {
    let fx = Fx::new(&[("writer", "writer")], &[]);
    let pm = fx.pm();
    set_paths(&pm, &fx.issue, &format!("apps/{APP}/app.md"));
    let err = crate::issue::dispatch::run(
        &pm,
        &fx.issue,
        &dispatch_args("lead", fx._root.path().join("widgets")),
        "operator",
        &fx.state,
        None,
        crate::issue::dispatch::DispatchOrigin::Explicit,
    )
    .expect_err("the fixture daemon has no seat for this worker");
    assert!(
        !err.to_string().contains("is on app"),
        "a non-team author is not refused by the app guard: {err}"
    );
}

/// Case 3: Renaming the role key does not launder the author — membership is
/// by agent alias.
#[test]
fn cad622_renaming_the_team_role_does_not_launder_the_author() {
    let fx = Fx::new(&[("author", "writer")], &[]);
    let pm = fx.pm();
    set_paths(&pm, &fx.issue, &format!("apps/{APP}/workflows/post.md"));
    let err = crate::issue::dispatch::run(
        &pm,
        &fx.issue,
        &dispatch_args("writer", fx._root.path().join("widgets")),
        "operator",
        &fx.state,
        None,
        crate::issue::dispatch::DispatchOrigin::Explicit,
    )
    .expect_err("a renamed role still names the same agent");
    assert!(
        err.to_string()
            .contains("writer is on app 'social-content's team"),
        "{err}"
    );
}

/// Case 4: Editing the app through a ticket of another project is refused:
/// the team is read across every project and `apps/<name>/` is
/// repo-relative.
#[test]
fn cad622_another_project_cannot_launder_the_author() {
    let fx = Fx::with_extra_project(&[("writer", "writer")], &[], true);
    let pm = fx.pm();
    set_paths(&pm, &fx.issue, &format!("apps/{APP}/app.md"));
    let err = crate::issue::dispatch::run(
        &pm,
        &fx.issue,
        &dispatch_args("writer", fx._root.path().join("widgets")),
        "operator",
        &fx.state,
        None,
        crate::issue::dispatch::DispatchOrigin::Explicit,
    )
    .expect_err("another project cannot hide the app's team");
    assert!(
        err.to_string()
            .contains("writer is on app 'social-content's team (project widgets)"),
        "{err}"
    );
}

/// Case 5: A plan that names the app's source is refused to its team agent
/// and admitted for another agent.
#[test]
fn cad622_a_plan_touching_its_own_app_is_refused() {
    let fx = Fx::new(&[("writer", "writer")], &[]);
    let plan = format!(
        "---\ntitle: \"Improve the app\"\ngoal: \"Change the app\"\n---\n\n\
         ## Rework the guide\nsize: S\n\nRewrite `apps/{APP}/app.md`.\n\n\
         ### Acceptance\n- [ ] the guide is reworked\n"
    );
    let err = scoped(Asserted::Agent("writer".into()), || {
        fx.shared.dispatch(
            "plan_propose",
            &json!({"project": PROJECT, "text": plan}),
            std::process::id(),
        )
    })
    .expect_err("a team agent cannot plan a change to its own app");
    assert!(
        err.to_string()
            .contains("writer is on app 'social-content's team"),
        "{err}"
    );
    // Positive control: the same plan from an agent off the team.
    let out = scoped(Asserted::Agent("lead".into()), || {
        fx.shared.dispatch(
            "plan_propose",
            &json!({"project": PROJECT, "text": plan}),
            std::process::id(),
        )
    })
    .expect("an agent off the team may plan the change");
    assert_eq!(out["tickets"].as_array().map(Vec::len), Some(1), "{out}");
}

/// Case 6: The review loop refuses a done report whose PR's own changed files
/// touch the app its author is on — and admits the same PR from an agent
/// off the team.
#[test]
fn cad622_the_review_loop_refuses_a_team_agent_authors_app_pr() {
    let fx = Fx::new(&[("writer", "writer")], &["reviewer"]);
    fx.seed_loop("writer");
    fx.route().expect("the router pass answers");
    let rec = fx.record();
    assert_eq!(
        rec.state,
        delivery::State::Working,
        "the head never entered review"
    );
    assert!(rec.head.is_none(), "a refused done report records no head");
    let why = fx.why();
    assert!(
        why.contains("writer is on app 'social-content's team (project widgets)")
            && why.contains("apps/social-content/"),
        "the refusal is plain and specific: {why}"
    );

    // Positive control: an agent off the app's team authors the same PR.
    let fx = Fx::new(&[("writer", "writer")], &["reviewer"]);
    fx.seed_loop("lead");
    fx.route().expect("the router pass answers");
    let rec = fx.record();
    assert_eq!(
        rec.state,
        delivery::State::Reviewing,
        "{}",
        rec.note.clone().unwrap_or_default()
    );
    assert_eq!(rec.head.as_deref(), Some(HEAD));
    assert_eq!(rec.reviewer.as_deref(), Some("reviewer"));
}

/// 7a. An app's own team reviewer is never routed the review.
#[test]
fn cad622_an_apps_team_reviewer_is_not_routed_the_review() {
    // `reviewer` is on the app's team; `outsider` is not. Both idle.
    let fx = Fx::new(
        &[("writer", "writer"), ("reviewer", "reviewer")],
        &["reviewer", "outsider"],
    );
    fx.seed_loop("lead");
    fx.route().expect("the router pass answers");
    let rec = fx.record();
    assert_eq!(rec.state, delivery::State::Reviewing, "{:?}", rec.note);
    assert_eq!(
        rec.reviewer.as_deref(),
        Some("outsider"),
        "the app's own team reviewer must not take the app's review"
    );

    // With only the team reviewer available, the review stays unstaffed
    // and the note says why.
    let fx = Fx::new(
        &[("writer", "writer"), ("reviewer", "reviewer")],
        &["reviewer"],
    );
    fx.seed_loop("lead");
    fx.route().expect("the router pass answers");
    let rec = fx.record();
    assert_eq!(rec.state, delivery::State::Unstaffed);
    let note = rec.note.clone().unwrap_or_default();
    assert!(
        note.contains("excluded as members of the app's own team: reviewer"),
        "{note}"
    );
}

/// 7b. An app's own team reviewer cannot file a verdict on the app's PR.
#[test]
fn cad622_an_apps_team_reviewer_cannot_file_a_verdict() {
    let fx = Fx::new(
        &[("writer", "writer"), ("reviewer", "reviewer")],
        &["reviewer", "outsider"],
    );
    let mut rec = delivery::Record::new(ISSUE, PROJECT, "lead", 0);
    rec.pr = Some(pr_url());
    rec.head = Some(HEAD.into());
    rec.reviewer = Some("reviewer".into());
    rec.touched_apps = Some(vec![APP.into()]);
    rec.enter(delivery::State::Reviewing, 1);
    fx.save_record(rec);
    let body = format!(
        "---\nkind: verdict\ntask: {ISSUE}\nverdict: pass\nsha: {HEAD}\n---\n\nLooks good.\n"
    );
    let err = scoped(Asserted::Agent("reviewer".into()), || {
        fx.shared
            .rpc_report_verdict(&json!({"issue": ISSUE, "text": body}), std::process::id())
    })
    .expect_err("an app's team reviewer must not judge the app's change");
    assert!(
        err.to_string()
            .contains("reviewer is on app 'social-content's team")
            && err.to_string().contains("cannot review"),
        "{err}"
    );

    // Positive control: an independent reviewer's verdict is accepted.
    let mut rec = fx.record();
    rec.reviewer = Some("outsider".into());
    fx.save_record(rec);
    scoped(Asserted::Agent("outsider".into()), || {
        fx.shared
            .rpc_report_verdict(&json!({"issue": ISSUE, "text": body}), std::process::id())
    })
    .expect("an independent reviewer files the verdict");
}

/// When the PR's own files cannot be read, an own-team author's report is
/// HELD (refused with the plain reason) — never admitted — and a ticket
/// with no app-team author is unaffected (positive control).
#[test]
fn cad622_unreadable_pr_files_hold_an_own_team_authors_report() {
    let fx = Fx::new(&[("writer", "writer")], &["reviewer"]);
    std::fs::write(fx._root.path().join("gh"), "#!/bin/sh\nexit 1\n").unwrap();
    fx.seed_loop("writer");
    fx.route().expect("the router pass answers");
    let rec = fx.record();
    assert_eq!(
        rec.state,
        delivery::State::Working,
        "an unreadable diff never admits an own-team author"
    );
    assert!(rec.head.is_none());
    let why = fx.why();
    assert!(
        why.contains("held until the PR's own changed files can be read"),
        "{why}"
    );
    let event = fx
        .shared
        .store
        .last_event_of(Store::DAEMON_STREAM, &["review_app_guard_unavailable"])
        .unwrap()
        .expect("the unreadable diff is flagged");
    assert_eq!(event.payload["issue"].as_str(), Some(ISSUE));

    // Positive control: the same outage does not stall a ticket in a
    // workspace where nobody is on an app's team.
    let fx = Fx::new(&[], &["reviewer"]);
    std::fs::write(fx._root.path().join("gh"), "#!/bin/sh\nexit 1\n").unwrap();
    fx.seed_loop("lead");
    fx.route().expect("the router pass answers");
    assert_eq!(fx.record().state, delivery::State::Reviewing);
}

/// A workspace installation (`.apps/installations/<id>/record.yaml` with
/// its team in the daemon's `app-teams.json`) is refused exactly like a
/// legacy project install — with and without a run.
#[test]
fn cad622_a_workspace_installation_team_is_refused() {
    let fx = Fx::new(&[], &[]);
    workspace_install(&fx, "inst-ws", &[("writer", "writer")]);
    let pm = fx.pm();
    set_paths(&pm, &fx.issue, &format!("apps/{APP}/app.md"));
    let err = crate::issue::dispatch::run(
        &pm,
        &fx.issue,
        &dispatch_args("writer", fx._root.path().join("widgets")),
        "operator",
        &fx.state,
        None,
        crate::issue::dispatch::DispatchOrigin::Explicit,
    )
    .expect_err("a workspace installation's team is a team");
    assert!(
        err.to_string()
            .contains("writer is on app 'social-content's team"),
        "{err}"
    );

    // A frozen run names the agent even though the saved team is empty.
    let fx = Fx::new(&[], &[]);
    workspace_install(&fx, "inst-ws2", &[]);
    fx.shared
        .store
        .write_tx(|tx| {
            tx.execute(
                "INSERT INTO app_runs(id,install_id,epoch,bundle_digest,snapshot,snapshot_digest, \
                 project_link,owner_pm,request_id,state,approved_digest,created,updated) \
                 VALUES('run-ws','inst-ws2',1,'b',?,'d',NULL,'lead','r1','approved',NULL,0,0)",
                rusqlite::params![
                    json!({"inputs": {}, "assignments": {"1": {"alias": "writer"}}}).to_string()
                ],
            )?;
            Ok(())
        })
        .unwrap();
    set_paths(&fx.pm(), &fx.issue, &format!("apps/{APP}/app.md"));
    // Run membership is store-backed: the daemon's own admission reads it.
    let err = scoped(Asserted::Agent("lead".into()), || {
        fx.shared.dispatch(
            "dispatch_send",
            &json!({"alias": "writer", "issue": ISSUE, "text": "do it", "reply_to": "lead"}),
            std::process::id(),
        )
    })
    .expect_err("a run's assignment is membership");
    assert!(
        err.to_string()
            .contains("writer is on app 'social-content's team"),
        "{err}"
    );
}

/// The daemon's own dispatch admission refuses a direct `dispatch_send`
/// from a team PM to a team worker on a ticket that plans the app.
#[test]
fn cad622_dispatch_send_refuses_at_the_daemon_boundary() {
    let fx = Fx::new(&[("writer", "writer")], &[]);
    set_paths(&fx.pm(), &fx.issue, &format!("apps/{APP}/app.md"));
    let err = scoped(Asserted::Agent("lead".into()), || {
        fx.shared.dispatch(
            "dispatch_send",
            &json!({"alias": "writer", "issue": ISSUE, "text": "do it", "reply_to": "lead"}),
            std::process::id(),
        )
    })
    .expect_err("the daemon refuses an own-team kickoff");
    assert!(
        err.to_string()
            .contains("writer is on app 'social-content's team"),
        "{err}"
    );
    // Positive control: a worker off the app's team is not refused by the
    // guard (it fails later, on the missing lane, not on the team).
    let fx = Fx::new(&[("writer", "writer")], &[]);
    set_paths(&fx.pm(), &fx.issue, &format!("apps/{APP}/app.md"));
    let err = scoped(Asserted::Agent("lead".into()), || {
        fx.shared.dispatch(
            "dispatch_send",
            &json!({"alias": "lead", "issue": ISSUE, "text": "do it", "reply_to": "lead"}),
            std::process::id(),
        )
    })
    .expect_err("the fixture has no lane for this issue");
    assert!(!err.to_string().contains("is on app"), "{err}");
}

/// A plan naming the app with the ordinary `./apps/<name>/` spelling is
/// refused too, and an agent off the team may propose it.
#[test]
fn cad622_a_plan_spelling_the_app_as_dot_slash_is_refused() {
    let fx = Fx::new(&[("writer", "writer")], &[]);
    let plan = format!(
        "---\ntitle: \"T\"\ngoal: \"G\"\n---\n\n## Rework\nsize: S\n\nRewrite \
         ./apps/{APP}/app.md.\n\n### Acceptance\n- [ ] done\n"
    );
    let err = scoped(Asserted::Agent("writer".into()), || {
        fx.shared.dispatch(
            "plan_propose",
            &json!({"project": PROJECT, "text": plan}),
            std::process::id(),
        )
    })
    .expect_err("./apps is the same source path");
    assert!(
        err.to_string()
            .contains("writer is on app 'social-content's team"),
        "{err}"
    );
    scoped(Asserted::Agent("lead".into()), || {
        fx.shared.dispatch(
            "plan_propose",
            &json!({"project": PROJECT, "text": plan}),
            std::process::id(),
        )
    })
    .expect("an agent off the team may plan it");
    // A genuinely nested path is not the tracker's apps/.
    let nested = "---\ntitle: \"T\"\ngoal: \"G\"\n---\n\n## Rework\nsize: S\n\nEdit \
                  src/apps/x/app.md.\n\n### Acceptance\n- [ ] done\n";
    scoped(Asserted::Agent("writer".into()), || {
        fx.shared.dispatch(
            "plan_propose",
            &json!({"project": PROJECT, "text": nested}),
            std::process::id(),
        )
    })
    .expect("src/apps/ is not the tracker's apps/");
}

/// An app installed from the project's own repository is refused at
/// dispatch even though the ticket names no `apps/<name>/` path — the
/// pinned git source is the evidence.
#[test]
fn cad622_a_git_source_app_is_refused_at_dispatch() {
    let fx = Fx::new(&[], &[]);
    workspace_install_from(
        &fx,
        "inst-git",
        "https://github.com/acme/widgets",
        &[("writer", "writer")],
    );
    let pm = fx.pm();
    set_paths(&pm, &fx.issue, "app.md,workflows/post.md");
    let err = crate::issue::dispatch::run(
        &pm,
        &fx.issue,
        &dispatch_args("writer", fx._root.path().join("widgets")),
        "operator",
        &fx.state,
        None,
        crate::issue::dispatch::DispatchOrigin::Explicit,
    )
    .expect_err("the app's git source is its source");
    assert!(err.to_string().contains("its pinned git source"), "{err}");
}

/// Membership is read fresh at the verdict: a reviewer that joins the
/// app's team while the review is outstanding can no longer file PASS.
#[test]
fn cad622_membership_changing_before_the_verdict_is_refused() {
    let fx = Fx::new(&[("writer", "writer")], &["reviewer", "outsider"]);
    let mut rec = delivery::Record::new(ISSUE, PROJECT, "lead", 0);
    rec.pr = Some(pr_url());
    rec.head = Some(HEAD.into());
    rec.reviewer = Some("outsider".into());
    rec.enter(delivery::State::Reviewing, 1);
    fx.save_record(rec);
    let body =
        format!("---\nkind: verdict\ntask: {ISSUE}\nverdict: pass\nsha: {HEAD}\n---\n\nFine.\n");
    scoped(Asserted::Agent("outsider".into()), || {
        fx.shared
            .rpc_report_verdict(&json!({"issue": ISSUE, "text": body}), std::process::id())
    })
    .expect("an independent reviewer files the verdict");

    // Reopen the review, then put the same reviewer on the app's team.
    let mut rec = delivery::Record::new(ISSUE, PROJECT, "lead", 0);
    rec.pr = Some(pr_url());
    rec.head = Some(HEAD.into());
    rec.reviewer = Some("outsider".into());
    rec.enter(delivery::State::Reviewing, 2);
    fx.save_record(rec);
    install_app(&fx.pm(), &[("writer", "writer"), ("reviewer", "outsider")]);
    let err = scoped(Asserted::Agent("outsider".into()), || {
        fx.shared
            .rpc_report_verdict(&json!({"issue": ISSUE, "text": body}), std::process::id())
    })
    .expect_err("a reviewer that joined the team is refused");
    assert!(
        err.to_string()
            .contains("outsider is on app 'social-content's team")
            && err.to_string().contains("cannot review"),
        "{err}"
    );
}

/// The guard reads a run's frozen role inputs too, not only the saved
/// team: an agent a run named as a team role is on that app's team.
#[test]
fn cad622_a_runs_role_input_puts_the_agent_on_the_apps_team() {
    let fx = Fx::new(&[], &[]);
    let pm = fx.pm();
    fx.shared
        .store
        .write_tx(|tx| {
            tx.execute(
                "INSERT INTO app_runs(id,install_id,epoch,bundle_digest,snapshot,snapshot_digest, \
                 project_link,owner_pm,request_id,state,approved_digest,created,updated) \
                 VALUES('run-c622','widgets-social-content',1,'b',?,'d',NULL,'lead','r1', \
                 'approved',NULL,0,0)",
                rusqlite::params![
                    json!({"inputs": {"writer": "writer"}, "assignments": {}}).to_string()
                ],
            )?;
            Ok(())
        })
        .unwrap();
    let guard = crate::issue::app_guard::Ctx {
        pm_dir: &pm.dir,
        state_dir: Some(&fx.state),
        store: Some(&fx.shared.store),
    };
    let teams = crate::issue::app_guard::teams(guard).unwrap();
    assert!(
        teams.get(APP).is_some_and(|t| t.contains("writer")),
        "a run's role input is the app's team: {teams:?}"
    );
    assert!(
        crate::issue::app_guard::teams(crate::issue::app_guard::Ctx::tracker(&pm.dir)).unwrap()
            [APP]
            .is_empty(),
        "the saved team alone is empty here"
    );
}
