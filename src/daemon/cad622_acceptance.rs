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
/// The head an app-team worker pushes onto an already-admitted PR.
const HEAD2: &str = "1111111111111111111111111111111111111111";
/// A second installed app: its own team, its own source path.
const APP_B: &str = "blog-post";

fn pr_url() -> String {
    format!("https://github.com/{REPO}/pull/{PR}")
}

/// The `gh` every loop test uses: `pr view` answers the head and files
/// recorded for that PR's number in `gh-view-<n>.json` (falling back to
/// the fixture's own PR). Any other call is a failure the test should
/// see.
fn write_gh(dir: &Path) {
    let script = format!(
        r##"#!/bin/sh
if [ "$1" = pr ] && [ "$2" = view ]; then
  n=$(printf '%s' "$3" | sed 's#.*/##')
  view="{}/gh-view-$n.json"
  [ -f "$view" ] || view="{}/gh-view-{PR}.json"
  cat "$view"
  exit 0
fi
echo "unhandled gh fixture: $*" >&2
exit 1
"##,
        dir.display(),
        dir.display()
    );
    let gh = dir.join("gh");
    std::fs::write(&gh, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    set_view(dir, PR, HEAD, &[&format!("apps/{APP}/app.md")]);
}

/// What `gh pr view` answers for `pr`: this exact head and these changed
/// files.
fn set_view(dir: &Path, pr: u64, head: &str, files: &[&str]) {
    let files: Vec<Value> = files.iter().map(|path| json!({"path": path})).collect();
    let view = json!({
        "number": pr,
        "state": "OPEN",
        "headRefOid": head,
        "baseRefName": "main",
        "baseRefOid": head,
        "changedFiles": files.len(),
        "additions": 2,
        "deletions": 0,
        "files": files,
    });
    std::fs::write(
        dir.join(format!("gh-view-{pr}.json")),
        view.to_string(),
    )
    .unwrap();
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
        Self::build(team, reviewers, other, false)
    }

    /// A project that registers a second, unrelated repository as well —
    /// the app's git source is only one of them.
    fn with_second_repo(team: &[(&str, &str)], reviewers: &[&str]) -> Self {
        Self::build(team, reviewers, false, true)
    }

    fn build(
        team: &[(&str, &str)],
        reviewers: &[&str],
        other: bool,
        second_repo: bool,
    ) -> Self {
        let root = tempfile::Builder::new().prefix("c622d").tempdir().unwrap();
        let pm_dir = root.path().join("pm");
        let pm = issue::Pm::init(&pm_dir).unwrap();
        let checkout = root.path().join("widgets");
        std::fs::create_dir_all(&checkout).unwrap();
        let mut repos: Vec<String> = vec![checkout.to_str().unwrap().into()];
        let mut remotes = vec!["https://github.com/acme/widgets.git"];
        if second_repo {
            let tools = root.path().join("tools");
            std::fs::create_dir_all(&tools).unwrap();
            repos.push(tools.to_str().unwrap().into());
            remotes.push("https://github.com/acme/tools.git");
        }
        for (repo, remote) in repos.iter().zip(remotes.iter()) {
            for args in [
                vec!["init", "-q"],
                vec!["remote", "add", "origin", remote],
            ] {
                assert!(std::process::Command::new("git")
                    .arg("-C")
                    .arg(repo)
                    .args(&args)
                    .status()
                    .unwrap()
                    .success());
            }
        }
        issue::write::project_add(&pm, PROJECT, "CAD", &repos, &[], &[], None).unwrap();
        let (home, ticket) = if other {
            // The second project works in the same checkout — the bypass
            // this fixture models is the ticket's project, not its repo.
            issue::write::project_add(
                &pm,
                OTHER,
                "OTH",
                &[checkout.to_str().unwrap().into()],
                &[],
                &[],
                None,
            )
            .unwrap();
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

    /// Register one more idle fake agent.
    fn register(&self, alias: &str, role: &str) {
        let cwd = self._root.path().to_str().unwrap().to_string();
        self.shared
            .store
            .register_agent(&NewAgent {
                alias,
                provider: "fake",
                endpoint_kind: "fake",
                role,
                cwd: &cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        self.shared.store.set_agent_state(alias, "idle", None).unwrap();
    }

    /// What `gh pr view` answers for `pr` from now on.
    fn set_view(&self, pr: u64, head: &str, files: &[&str]) {
        set_view(self._root.path(), pr, head, files);
    }

    /// A run of `install` whose snapshot froze `team_roles` and assigned
    /// nothing — the run-time half of the app's team.
    fn insert_run(&self, id: &str, install: &str, snapshot: Value) {
        self.shared
            .store
            .write_tx(|tx| {
                tx.execute(
                    "INSERT INTO app_runs(id,install_id,epoch,bundle_digest,snapshot, \
                     snapshot_digest,project_link,owner_pm,request_id,state,approved_digest, \
                     created,updated) \
                     VALUES(?1,?2,1,'b',?3,'d',NULL,'lead','r1','approved',NULL,0,0)",
                    rusqlite::params![id, install, snapshot.to_string()],
                )?;
                Ok(())
            })
            .unwrap();
    }

    /// The delivery record of any issue in the fixture's project.
    fn record_of(&self, issue: &str) -> delivery::Record {
        delivery::load(&self.state)
            .unwrap()
            .get(issue)
            .cloned()
            .unwrap_or_else(|| panic!("fixture record {issue}"))
    }

    fn save_records(&self, records: &[delivery::Record]) {
        let mut all = delivery::load(&self.state).unwrap();
        for rec in records {
            all.insert(rec.issue.clone(), rec.clone());
        }
        delivery::save(&self.state, &all).unwrap();
    }

    /// A done report for `worker` on `issue`, at `head`/`pr`.
    fn report(&self, issue: &str, worker: &str, name: &str, head: &str, pr: u64) {
        let dir = self.pm_dir.join(PROJECT).join(issue).join("reports");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(name),
            format!(
                "---\nschema: cadence.report/2\nkind: done\ntask: {issue}\nagent: {worker}\n\
                 sha: {head}\npr: https://github.com/{REPO}/pull/{pr}\n---\n\n\
                 ## Expected\n\nx\n\n## Evidence\n\nx\n\n## Cause\n\nx\n\n\
                 ## Correction\n\nx\n\n## Lesson\n\nx\n\n## Next\n\nx\n"
            ),
        )
        .unwrap();
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
    install_named_app(pm, APP, team);
}

/// `apps/<name>/` with a one-workflow bundle and the saved team.
fn install_named_app(pm: &issue::Pm, name: &str, team: &[(&str, &str)]) {
    let dir = pm.dir.join(PROJECT).join(app::DIR);
    std::fs::create_dir_all(dir.join(name).join("workflows")).unwrap();
    std::fs::write(
        dir.join(name).join(app::MANIFEST),
        format!(
            "---\napp: {name}\ntitle: Social content\nversion: '1'\nneeds:\n  connections: []\n---\n\nGuide\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join(name).join("workflows").join("post.md"),
        "---\ntitle: \"Post\"\ngoal: \"Write one post\"\ninputs:\n  writer: { ask: \"Who writes?\" }\n---\n\n## Draft\nagent: {{writer}}\n\nWrite it.\n\n### Acceptance\n- [ ] written\n",
    )
    .unwrap();
    let record = app::Record {
        schema: 1,
        app: name.to_string(),
        install_id: format!("{PROJECT}-{name}"),
        source: app::Source::Path {
            path: format!("tests/fixtures/apps/{name}"),
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
        dir.join(format!("{name}.yaml")),
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
    rec.app_evidence = Some(delivery::AppEvidence {
        pr: pr_url(),
        head: HEAD.into(),
        apps: vec![APP.into()],
        from_git: vec![],
    });
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

/// The guard reads a run's frozen team-role bindings too, not only the
/// saved team: an agent a run bound to a team role is on that app's team.
#[test]
fn cad622_a_runs_frozen_team_role_puts_the_agent_on_the_apps_team() {
    let fx = Fx::new(&[], &[]);
    let pm = fx.pm();
    fx.insert_run(
        "run-c622",
        "widgets-social-content",
        json!({"team_roles": {"writer": "writer"}, "assignments": {}}),
    );
    let guard = crate::issue::app_guard::Ctx {
        pm_dir: &pm.dir,
        state_dir: Some(&fx.state),
        store: Some(&fx.shared.store),
    };
    let teams = crate::issue::app_guard::teams(guard).unwrap();
    assert!(
        teams.get(APP).is_some_and(|t| t.contains("writer")),
        "a run's frozen team role is the app's team: {teams:?}"
    );
    assert!(
        crate::issue::app_guard::teams(crate::issue::app_guard::Ctx::tracker(&pm.dir)).unwrap()
            [APP]
            .is_empty(),
        "the saved team alone is empty here"
    );
}

/// F4: role keys are not aliases. A saved role `writer → topic` makes
/// `topic` a team member; the ordinary content input `topic: lead` is
/// content, so `lead` never joins the app's team.
#[test]
fn cad622_a_content_input_never_makes_an_agent_a_team_member() {
    let fx = Fx::new(&[], &[]);
    workspace_install(&fx, "inst-ws3", &[("writer", "topic")]);
    fx.insert_run(
        "run-ws3",
        "inst-ws3",
        json!({
            "team_roles": {"writer": "topic"},
            "inputs": {"writer": "topic", "topic": "lead"},
            "assignments": {},
        }),
    );
    let guard = crate::issue::app_guard::Ctx {
        pm_dir: &fx.pm_dir,
        state_dir: Some(&fx.state),
        store: Some(&fx.shared.store),
    };
    let teams = crate::issue::app_guard::teams(guard).unwrap();
    let team = teams.get(APP).expect("the workspace install's app");
    assert!(team.contains("topic"), "the bound role alias is a member: {team:?}");
    assert!(
        !team.contains("lead"),
        "a content input's value is not a team member: {team:?}"
    );
    // And the independent agent is not refused at the daemon boundary.
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

/// R1: the operator replaces the saved role's alias, and the run bound
/// the old alias. The frozen provenance keeps the old alias barred even
/// though no current team or step assignment names it.
#[test]
fn cad622_a_replaced_saved_role_keeps_the_frozen_alias_barred() {
    let fx = Fx::new(&[], &[]);
    // The current team: the role was re-pointed at a new agent.
    workspace_install(&fx, "inst-ws4", &[("writer", "new-writer")]);
    // The run froze the old binding; its steps assigned nobody.
    fx.insert_run(
        "run-ws4",
        "inst-ws4",
        json!({"team_roles": {"writer": "old-writer"}, "assignments": {}}),
    );
    fx.register("old-writer", "worker");
    fx.register("new-writer", "worker");
    let guard = crate::issue::app_guard::Ctx {
        pm_dir: &fx.pm_dir,
        state_dir: Some(&fx.state),
        store: Some(&fx.shared.store),
    };
    let teams = crate::issue::app_guard::teams(guard).unwrap();
    let team = teams.get(APP).expect("the workspace install's app");
    assert!(
        team.contains("old-writer"),
        "the frozen role binding stays a member: {team:?}"
    );
    assert!(team.contains("new-writer"), "the current alias too: {team:?}");
    // The daemon's own admission refuses the historical member.
    set_paths(&fx.pm(), &fx.issue, &format!("apps/{APP}/app.md"));
    let err = scoped(Asserted::Agent("lead".into()), || {
        fx.shared.dispatch(
            "dispatch_send",
            &json!({"alias": "old-writer", "issue": ISSUE, "text": "do it",
                    "reply_to": "lead"}),
            std::process::id(),
        )
    })
    .expect_err("the run's frozen role alias is still the app's team");
    assert!(
        err.to_string()
            .contains("old-writer is on app 'social-content's team"),
        "{err}"
    );
}

/// F1: the native `task_dispatch` path — an issue-linked job the caller's
/// own PM created, assigned to an app-team worker — is refused by the
/// same store-backed guard `dispatch_send` runs, before the queue write.
/// An independent worker on the same ticket is not refused by it.
#[test]
fn cad622_task_dispatch_refuses_an_own_team_worker() {
    let fx = Fx::new(&[("writer", "writer")], &[]);
    set_paths(&fx.pm(), &fx.issue, &format!("apps/{APP}/app.md"));
    let checkout = fx._root.path().join("widgets");
    let job = |id: &str, assignee: &str| {
        fx.shared
            .store
            .create_job(
                id,
                Some("kickoff"),
                "spec.md",
                &"0".repeat(64),
                "lead",
                Some(ISSUE),
                Some(checkout.to_str().unwrap()),
                None,
                2,
                None,
                None,
                None,
                None,
                None,
                Some(assignee),
                Some("the work is done"),
            )
            .expect("issue-bound job");
        format!("{id}-t1")
    };
    let task = job("c622-job", "writer");
    let err = scoped(Asserted::Agent("lead".into()), || {
        fx.shared.dispatch(
            "task_dispatch",
            &json!({"task": task, "to": "writer", "by": "lead"}),
            std::process::id(),
        )
    })
    .expect_err("the native kickoff must run the app-team guard");
    assert!(
        err.to_string()
            .contains("writer is on app 'social-content's team"),
        "{err}"
    );
    // Nothing was queued: the task is still a draft with no kickoff.
    let task_row = fx.shared.store.task(&task).expect("task reads");
    assert_eq!(task_row.state, "draft");
    assert!(task_row.dispatch_message.is_none());

    // Positive control: the same ticket and job, an independent worker.
    let task = job("c622-job2", "lead");
    let err = scoped(Asserted::Agent("lead".into()), || {
        fx.shared.dispatch(
            "task_dispatch",
            &json!({"task": task, "to": "lead", "by": "lead"}),
            std::process::id(),
        )
    })
    .expect_err("the fixture has no lane for this issue");
    assert!(!err.to_string().contains("is on app"), "{err}");
}

/// F1: both monitor kickoffs reach the same guard. The automatic watcher
/// path refuses an own-team worker before its store transaction.
#[test]
fn cad622_monitor_dispatch_refuses_an_own_team_worker() {
    let fx = Fx::new(&[("writer", "writer")], &[]);
    set_paths(&fx.pm(), &fx.issue, &format!("apps/{APP}/app.md"));
    let checkout = fx._root.path().join("widgets");
    fx.shared
        .store
        .create_job(
            "c622-mon",
            Some("kickoff"),
            "spec.md",
            &"0".repeat(64),
            "lead",
            Some(ISSUE),
            Some(checkout.to_str().unwrap()),
            None,
            2,
            None,
            None,
            None,
            None,
            None,
            Some("writer"),
            Some("the work is done"),
        )
        .expect("issue-bound job");
    let err = fx
        .shared
        .monitor_dispatch_task("c622-watch", "c622-mon-t1", true)
        .expect_err("the automatic monitor kickoff must run the app-team guard");
    assert!(
        err.to_string()
            .contains("writer is on app 'social-content's team"),
        "{err}"
    );
    let task = fx.shared.store.task("c622-mon-t1").expect("task reads");
    assert_eq!(task.state, "draft");
    assert!(task.dispatch_message.is_none());
}

/// R2-HEAD: a clean admitted head is reviewed; the worker then pushes an
/// own-app change onto the same PR. The observed head change re-runs the
/// author authorization for the NEW head and holds the review — an
/// independent reviewer being available does not repair it.
#[test]
fn cad622_an_own_app_push_after_admission_is_refused() {
    let fx = Fx::new(&[("writer", "writer")], &["reviewer", "outsider"]);
    fx.set_view(PR, HEAD, &["src/lib.rs"]);
    fx.seed_loop("writer");
    fx.route().expect("the router pass answers");
    let rec = fx.record();
    assert_eq!(
        rec.state,
        delivery::State::Reviewing,
        "a clean head from an own-team worker is admitted: {:?}",
        rec.note
    );
    assert!(
        rec.reviewer.is_some(),
        "an independent reviewer was available and routed"
    );
    // The worker pushes its own app's file onto the same PR.
    fx.set_view(PR, HEAD2, &[&format!("apps/{APP}/app.md")]);
    scoped(Asserted::Operator, || {
        fx.shared.rpc_delivery_observe(
            &json!({"issue": ISSUE, "head": HEAD2, "pr_state": "OPEN"}),
            std::process::id(),
        )
    })
    .expect("the observation answers");
    let rec = fx.record();
    assert_eq!(
        rec.state,
        delivery::State::Escalated,
        "the moved head is not routed: {:?}",
        rec.note
    );
    assert!(
        rec.note
            .as_deref()
            .unwrap_or_default()
            .contains("writer is on app 'social-content's team"),
        "{:?}",
        rec.note
    );
    assert!(
        rec.reviewer.is_none(),
        "no reviewer is handed the own-team head"
    );
    assert!(
        fx.shared
            .store
            .last_event_of(Store::DAEMON_STREAM, &["review_app_author_refused"])
            .unwrap()
            .is_some(),
        "the refusal is on the daemon stream"
    );
    // The independent reviewer that took the clean head never judged this one.
    assert_ne!(rec.passed_sha(), Some(HEAD2));
}

/// R6: a refused report for another PR/head must not rewrite the accepted
/// head's app evidence — not even when another record's movement saves the
/// whole record map. The reviewer who joins the app's team is still
/// refused at the verdict.
#[test]
fn cad622_a_refused_report_cannot_poison_the_accepted_heads_evidence() {
    // `writer` is on blog-post's team only; social-content is unrelated.
    let fx = Fx::new(&[], &["reviewer", "outsider"]);
    install_named_app(&fx.pm(), APP_B, &[("writer", "writer")]);
    let pm = fx.pm();
    crate::issue::write::new_issue(
        &pm,
        &fx._root.path().join("widgets"),
        Some(PROJECT),
        "A second ticket",
        None,
        None,
        &[],
        None,
        None,
        &[],
        Some("CAD-623"),
        None,
        "operator",
    )
    .unwrap();
    fx.set_view(PR, HEAD, &[&format!("apps/{APP}/app.md")]);
    fx.set_view(623, HEAD, &[&format!("apps/{APP_B}/app.md")]);
    fx.set_view(624, HEAD, &["src/lib.rs"]);

    // CAD-622: the worker's PR touches the app it is NOT on — admitted.
    let mut a = delivery::Record::new(ISSUE, PROJECT, "writer", 0);
    a.pr = Some(pr_url());
    // CAD-623: a second record that will move in the same router pass.
    let mut b = delivery::Record::new("CAD-623", PROJECT, "lead", 0);
    b.pr = Some(format!("https://github.com/{REPO}/pull/624"));
    fx.save_records(&[a, b]);
    fx.report(ISSUE, "writer", "19700101T000000Z-done.md", HEAD, PR);
    fx.route().expect("the router pass answers");
    let a = fx.record_of(ISSUE);
    assert_eq!(
        a.state,
        delivery::State::Reviewing,
        "an unrelated app's worker authors the app's PR: {:?}",
        a.note
    );
    let evidence = a.app_evidence.clone().expect("the head's evidence");
    assert_eq!(evidence.head, HEAD);
    assert_eq!(evidence.apps, vec![APP.to_string()]);
    let reviewer = a.reviewer.clone().expect("an independent reviewer");

    // The worker files a second report naming an app IT is on. The same
    // pass moves CAD-623, so the whole record map is written.
    fx.report(
        ISSUE,
        "writer",
        "19700101T000001Z-done.md",
        HEAD,
        623,
    );
    fx.report("CAD-623", "lead", "19700101T000000Z-done.md", HEAD, 624);
    fx.route().expect("the router pass answers");
    assert_eq!(
        fx.record_of("CAD-623").state,
        delivery::State::Reviewing,
        "the other record moved, so the map was saved"
    );
    let why = fx.why();
    assert!(
        why.contains("writer is on app 'blog-post's team"),
        "the refused report names the app it touches: {why}"
    );
    let a = fx.record_of(ISSUE);
    let evidence = a.app_evidence.clone().expect("the head's evidence survives");
    assert_eq!(
        evidence.head, HEAD,
        "the accepted head's evidence is untouched"
    );
    assert_eq!(
        evidence.apps,
        vec![APP.to_string()],
        "a refused report for another app never rewrites it"
    );
    assert_eq!(a.pr, Some(pr_url()));

    // The reviewer joins the app's team: the verdict on this head is refused
    // because the evidence still names social-content.
    install_app(
        &fx.pm(),
        &[("writer", "writer"), ("reviewer", reviewer.as_str())],
    );
    let body = format!(
        "---\nkind: verdict\ntask: {ISSUE}\nverdict: pass\nsha: {HEAD}\n---\n\nFine.\n"
    );
    let err = scoped(Asserted::Agent(reviewer.clone()), || {
        fx.shared
            .rpc_report_verdict(&json!({"issue": ISSUE, "text": body}), std::process::id())
    })
    .expect_err("a reviewer that joined the app's team cannot judge its head");
    assert!(
        err.to_string()
            .contains(&format!("{reviewer} is on app 'social-content's team"))
            && err.to_string().contains("cannot review"),
        "{err}"
    );
}

/// F2: a live grant admits a workflow render, never an app's own team
/// authoring a change to that app. The proposal is refused before any
/// tracker allocation, epic/ticket or admission mark — and the same
/// grant admits the render from an independent proposer.
#[test]
fn cad622_a_grant_never_admits_an_own_team_proposal() {
    let fx = Fx::new(&[("writer", "writer")], &[]);
    let pm = fx.pm();
    let text = format!(
        "---\ntitle: \"Improve the app\"\ngoal: \"Change the app\"\n---\n\n\
         ## Rework the guide\nagent: writer\nsize: S\n\nRewrite \
         `apps/{APP}/app.md`.\n\n### Acceptance\n- [ ] reworked\n"
    );
    crate::issue::workflow::write_file(
        &pm,
        PROJECT,
        "improve",
        &text,
        true,
        fx._root.path(),
        "operator",
    )
    .unwrap();
    let approval = scoped(Asserted::Operator, || {
        fx.shared.rpc_workflow_approve(
            &json!({
                "project": PROJECT,
                "name": "improve",
                "unattended": {
                    "repo": REPO,
                    "paths": [format!("apps/{APP}/app.md")],
                    "inputs": {},
                },
            }),
            std::process::id(),
        )
    })
    .expect("the operator's live grant");
    assert!(
        approval["unattended"]["grant_id"].as_str().is_some(),
        "the grant is live: {approval}"
    );
    let issues = |fx: &Fx| -> std::collections::BTreeSet<String> {
        let mut ids = std::collections::BTreeSet::new();
        let Ok(entries) = std::fs::read_dir(fx.pm_dir.join(PROJECT)) else {
            return ids;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("CAD-") && entry.path().join("issue.md").is_file() {
                ids.insert(name);
            }
        }
        ids
    };
    let before = issues(&fx);
    let marks = fx.shared.store.unattended_run_payloads().unwrap().len();
    let propose = |who: &str| {
        scoped(Asserted::Agent(who.to_string()), || {
            fx.shared.rpc_plan_propose(
                &json!({"project": PROJECT, "workflow": "improve", "inputs": {}}),
                std::process::id(),
            )
        })
    };
    // The app's own team agent proposes the grant's exact render.
    let err = propose("writer").expect_err("a live grant never admits an own-team author");
    assert!(
        err.to_string()
            .contains("writer is on app 'social-content's team"),
        "{err}"
    );
    assert_eq!(issues(&fx), before, "no ticket or epic was allocated");
    assert_eq!(
        fx.shared.store.unattended_run_payloads().unwrap().len(),
        marks,
        "no admission mark was written"
    );

    // Positive control: an independent proposer's same render is admitted.
    let out = propose("lead").expect("an independent proposer is admitted");
    assert_eq!(out["state"].as_str(), Some("approved"), "{out}");
    assert!(
        issues(&fx).len() > before.len(),
        "the admitted plan allocated its tickets"
    );
    assert!(
        fx.shared.store.unattended_run_payloads().unwrap().len() > marks,
        "the admitted plan recorded its mark"
    );
}

/// F3: the git-source evidence is the repository the dispatch SELECTED.
/// A project that also registers an unrelated repo does not refuse work
/// that never touches the app's own repository.
#[test]
fn cad622_dispatch_matches_the_selected_repository() {
    let fx = Fx::with_second_repo(&[], &[]);
    workspace_install_from(
        &fx,
        "inst-git2",
        "https://github.com/acme/widgets",
        &[("writer", "writer")],
    );
    let pm = fx.pm();
    set_paths(&pm, &fx.issue, "app.md,workflows/post.md");
    let tools = fx._root.path().join("tools");
    // `--repo tools`: only tools' paths, so the app's git source is not
    // this dispatch's evidence.
    let err = crate::issue::dispatch::run(
        &pm,
        &fx.issue,
        &dispatch_args("writer", tools),
        "operator",
        &fx.state,
        None,
        crate::issue::dispatch::DispatchOrigin::Explicit,
    )
    .expect_err("the fixture has no seat for this worker");
    assert!(
        !err.to_string().contains("pinned git source"),
        "an unrelated repository is not the app's source: {err}"
    );
    // Positive control: the app's own repository IS refused.
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
