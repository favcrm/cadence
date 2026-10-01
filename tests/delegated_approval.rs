//! CAD-918: each test attacks one safeguard of delegated approvals.
#![allow(clippy::disallowed_methods)]
mod common;
use common::*;

use serde_json::{json, Value};
use std::path::PathBuf;
use tempfile::TempDir;

const HEAD: &str = "1111111111111111111111111111111111111111";
const OLD: &str = "2222222222222222222222222222222222222222";
const REPO: &str = "acme/app";
const LANE: &str = "cadence/cad-1-demo";

struct Fx {
    f: PlanFixture,
    gh_dir: PathBuf,
    notes: PathBuf,
    home: TempDir,
    _gh_tmp: TempDir,
}

impl Fx {
    /// Project `cad` on github.com/Acme/app; ticket CAD-1 owned by `w1`
    /// with lane branch LANE; PR #7 at HEAD from that branch, touching
    /// one delegable file, with the base branch's required check green;
    /// and a notes dir the daemon reads via pm.yaml.
    fn start() -> Fx {
        let gh_tmp = TempDir::new().unwrap();
        let bin = gh_tmp.path().join("gh");
        std::fs::write(&bin, include_str!("fixtures/delegated-gh.py")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut opts = daemon_opts();
        opts.delivery_gh = Some(bin);
        let f = PlanFixture::start_with(opts);
        // `audit::parse_note` binds `CAD-` ids, so the tickets are those.
        let repo = f.tmp.path().join("repo").canonicalize().unwrap();
        let repo = repo.to_str().unwrap();
        let add = [
            "issue", "project", "add", "cad", "--prefix", "CAD", "--repo", repo,
        ];
        let (ok, out) = f.cli(&add);
        assert!(ok, "{out}");
        let yaml = |path: PathBuf, edit: &dyn Fn(&mut serde_yaml::Value)| {
            let mut v: serde_yaml::Value =
                serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            edit(&mut v);
            std::fs::write(&path, serde_yaml::to_string(&v).unwrap()).unwrap();
        };
        yaml(f.pm_dir.join("cad/project.yaml"), &|p| {
            p["repos"][0]["remote"] = "https://github.com/Acme/app.git".into();
        });
        for title in ["demo work", "other work"] {
            let (ok, out) = f.cli(&["issue", "new", title, "--project", "cad"]);
            assert!(ok, "{out}");
        }
        let issue = f.pm_dir.join("cad/CAD-1/issue.md");
        let text = std::fs::read_to_string(&issue).unwrap();
        let (front, body) = text["---\n".len()..].split_once("\n---\n").unwrap();
        let mut front: serde_yaml::Value = serde_yaml::from_str(front).unwrap();
        front["owner"] = "w1".into();
        front["refs"] = serde_yaml::from_str(&format!("[{{kind: branch, path: {LANE}}}]")).unwrap();
        let front = serde_yaml::to_string(&front).unwrap();
        std::fs::write(&issue, format!("---\n{front}---\n{body}")).unwrap();
        let notes = f.tmp.path().join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        yaml(f.pm_dir.join("pm.yaml"), &|c| {
            c["notes_dir"] = notes.to_str().unwrap().into();
        });
        let fx = Fx {
            gh_dir: gh_tmp.path().to_path_buf(),
            f,
            notes,
            home: TempDir::new().unwrap(),
            _gh_tmp: gh_tmp,
        };
        fx.pr(&["src/cli/status.rs"]);
        fx
    }

    /// PR #7 at HEAD touching `files`; CI green.
    fn pr(&self, files: &[&str]) {
        let diff: String = files
            .iter()
            .map(|p| format!("diff --git a/{p} b/{p}\n@@ -1 +1 @@\n-a\n+b\n"))
            .collect();
        let files: Vec<Value> = files.iter().map(|p| json!({"path": p})).collect();
        let view = json!({
            "headRefOid": HEAD, "headRefName": LANE, "baseRefName": "main", "state": "OPEN",
            "title": "CAD-1: demo work", "author": {"login": "gh-bot"},
            "changedFiles": files.len(), "files": files, "statusCheckRollup": [],
        });
        let state = json!({
            "pr": "7", "view": view, "diff": diff,
            "branch": {"protection": {"required_status_checks":
                {"checks": [{"context": "test", "app_id": 15368}]}}},
            "runs": {"total_count": 1, "check_runs": [{"name": "test", "status": "completed",
                "conclusion": "success", "app": {"id": 15368, "slug": "github-actions"},
                "check_suite": {"id": 500}, "id": 1}]},
            "workflows": {"total_count": 1, "workflow_runs": [{"path": ".github/workflows/ci.yml",
                "event": "pull_request", "head_sha": HEAD, "check_suite_id": 500,
                "pull_requests": [{"number": 7, "base": {"ref": "main"}}]}]},
        });
        self.write_gh(&state);
    }

    fn gh_state(&self) -> Value {
        let text = std::fs::read_to_string(self.gh_dir.join("delegated-gh.json"));
        serde_json::from_str(&text.unwrap()).unwrap()
    }

    fn write_gh(&self, state: &Value) {
        std::fs::write(self.gh_dir.join("delegated-gh.json"), state.to_string()).unwrap();
    }

    /// Set the fake gh's state at a JSON pointer.
    fn set(&self, pointer: &str, value: Value) {
        let (mut s, (parent, key)) = (self.gh_state(), pointer.rsplit_once('/').unwrap());
        s.pointer_mut(parent).unwrap()[key] = value;
        self.write_gh(&s);
    }

    /// A verdict note in the AGENTS.md shape, for PR #7.
    fn note(&self, name: &str, from: &str, head: &str, risk: &str) -> PathBuf {
        self.note_pr(name, from, head, risk, 7)
    }

    fn note_pr(&self, name: &str, from: &str, head: &str, risk: &str, pr: u64) -> PathBuf {
        let path = self.notes.join(format!("20261001-0000{name}-verdict.md"));
        let text = format!(
            "# Verdict: CAD-1 review — pass\n> Issue: CAD-1\n> From: {from}\n\n\
             ## Verdict\npass — PR #{pr}, head {head}\n\nRisk: {risk}\n"
        );
        std::fs::write(&path, text).unwrap();
        path
    }

    fn pane(&self, alias: &str) -> LaneShell {
        let sh = LaneShell::spawn(self.home.path());
        plant_pane(&self.f.d, alias, sh.pid());
        sh
    }

    fn designate(&self, alias: &str, active: bool) -> Value {
        let r = self.f.d.operator_rpc(
            "approval_designate",
            json!({"alias": alias, "project": "cad", "source": "op", "active": active}),
        );
        r.unwrap()
    }

    fn approve_pr(&self, sh: &mut LaneShell, pr: u64, head: &str, extra: &str) -> (i64, String) {
        let args = format!(
            "audit approve --delegated --pr {pr} --head {head} --repo {REPO} --source s {extra}"
        );
        sh.cadence(&self.f.d.state, &args)
    }

    fn approve(&self, sh: &mut LaneShell, extra: &str) -> (i64, String) {
        self.approve_pr(sh, 7, HEAD, extra)
    }

    fn refuse(&self, sh: &mut LaneShell, extra: &str, why: &str) {
        let (rc, out) = self.approve(sh, extra);
        refused(rc, &out, why);
    }

    /// `audit digest`, with the fake gh first on the CLI's PATH.
    fn digest(&self, sh: &mut LaneShell) -> Value {
        let (rc, out) = sh.run(&format!(
            "PATH={}:$PATH {} --state-dir {} audit digest",
            self.gh_dir.display(),
            env!("CARGO_BIN_EXE_cadence"),
            self.f.d.state.display()
        ));
        assert_eq!(rc, 0, "{out}");
        serde_json::from_str(&out).unwrap()
    }

    /// Delegated records on the approval stream.
    fn delegated(&self) -> Vec<Value> {
        let conn = rusqlite::Connection::open(self.f.d.state.join("cadence.sqlite3")).unwrap();
        let sql = "SELECT payload FROM events WHERE alias='audit:approvals' ORDER BY seq";
        let mut st = conn.prepare(sql).unwrap();
        let rows = st.query_map([], |r| r.get::<_, String>(0)).unwrap();
        let rows = rows.map(|p| serde_json::from_str::<Value>(&p.unwrap()).unwrap());
        rows.filter(|p| p["action"] == "delegated-merge").collect()
    }
}

fn refused(rc: i64, out: &str, why: &str) {
    assert_ne!(rc, 0, "{out}");
    assert!(out.contains(why), "expected '{why}' in: {out}");
}

/// The designated agent records once, under its derived identity — two
/// concurrent calls with different sources leave one record. The
/// digest shows both reviewers, notices an edited note, and offers a
/// revert only once merged. A revoke sticks.
#[test]
fn cad918_delegated_approval_records_once_under_the_derived_identity() {
    let fx = Fx::start();
    let mut pm = fx.pane("pm-d");
    fx.designate("pm-d", true);
    let r1 = fx.note("01-r1", "r1 (claude opus)", HEAD, "delegated (5)");
    fx.note("02-r2", "r2", HEAD, "delegated (2)");
    let cmd = |src: &str| {
        format!(
            "{} --state-dir {} audit approve --delegated --pr 7 --head {HEAD} --repo Acme/App \
             --source {src}",
            env!("CARGO_BIN_EXE_cadence"),
            fx.f.d.state.display()
        )
    };
    let (rc, out) = pm.run(&format!("{} & {} & wait", cmd("a"), cmd("b")));
    assert_eq!(rc, 0, "{out}");
    let recs = fx.delegated();
    assert_eq!(recs.len(), 1, "exactly once: {recs:?}");
    assert_eq!(recs[0]["approver"], "delegated:pm-d", "{recs:?}");
    assert_eq!(recs[0]["recorded_via"], "delegated:pm-d", "{recs:?}");
    assert_eq!(recs[0]["scope"]["repo"], REPO, "{recs:?}");
    assert_eq!(recs[0]["reviewers"], json!(["r2", "r1"]), "{recs:?}");
    assert_eq!(recs[0]["verdict_sha256"].as_array().unwrap().len(), 2);
    let (rc, out) = fx.approve(&mut pm, "");
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["duplicate"],
        true
    );
    assert_eq!(fx.delegated().len(), 1);

    let row = fx.digest(&mut pm)["delegated"][0].clone();
    assert_eq!(row["reviewers"], json!(["r2", "r1"]), "{row}");
    assert_eq!(row["verdicts_intact"], true, "{row}");
    assert_eq!(
        row["revert"],
        Value::Null,
        "an open PR has no revert: {row}"
    );
    assert!(row["revoke"]
        .as_str()
        .unwrap()
        .contains("cadence audit revoke"));
    std::fs::write(&r1, "edited after the approval").unwrap();
    fx.set(
        "/merge",
        json!({"state": "MERGED", "mergeCommit": {"oid": OLD}}),
    );
    let row = fx.digest(&mut pm)["delegated"][0].clone();
    assert_eq!(row["verdicts_intact"], false, "{row}");
    assert_eq!(row["revert"], format!("git revert {OLD}"), "{row}");

    fx.note("01-r1", "r1", HEAD, "delegated (5)");
    let id = recs[0]["approval_id"].as_str().unwrap();
    let revoke = json!({"id": id, "source": "op", "reason": "x"});
    fx.f.d.operator_rpc("approval_revoke", revoke).unwrap();
    assert_eq!(fx.digest(&mut pm)["delegated"][0]["revoked"], true);
    fx.refuse(&mut pm, "", "was revoked");
    assert_eq!(fx.delegated().len(), 1);
}

/// Only a designated agent, for its project's own repo — checked before
/// any gh call — never an author or the operator, never after the
/// designation is withdrawn or the alias re-registered, until the
/// operator designates it again.
#[test]
fn cad918_delegated_approval_refuses_undesignated_author_and_operator() {
    let fx = Fx::start();
    fx.note("01-r1", "r1", HEAD, "delegated (5)");
    fx.note("02-r2", "r2", HEAD, "delegated (5)");
    let mut pm = fx.pane("pm-d");
    fx.designate("pm-d", true);
    let (rc, out) = pm.cadence(
        &fx.f.d.state,
        &format!("audit approve --delegated --pr 7 --head {HEAD} --repo other/app --source s"),
    );
    refused(rc, &out, "whose repo is other/app");
    let (rc, out) = pm.cadence(
        &fx.f.d.state,
        &format!(
            "audit approve --delegated --pr 7 --head {HEAD} --repo h.example/acme/app --source s"
        ),
    );
    refused(rc, &out, "not a plain owner/name");
    assert!(
        !fx.gh_dir.join("gh.log").exists(),
        "gh ran before the repo check"
    );
    let mut stranger = fx.pane("pm-x");
    fx.refuse(&mut stranger, "", "not designated");
    let mut author = fx.pane("w1");
    fx.designate("w1", true);
    fx.refuse(&mut author, "", "an author never approves");
    fx.designate("pm-late", true);
    let mut late = fx.pane("pm-late");
    fx.refuse(&mut late, "", "not designated");
    fx.designate("pm-d", false);
    fx.refuse(&mut pm, "", "not designated");
    let params = json!({"pr": 7, "head": HEAD, "repo": REPO, "source": "op"});
    let err =
        fx.f.d
            .operator_rpc("approval_delegate", params)
            .unwrap_err();
    assert!(err.to_string().contains("designated agent's act"), "{err}");
    assert!(fx.delegated().is_empty());
    // Re-registered: the old designation is void until designated again.
    fx.designate("pm-d", true);
    drop(pm);
    // The planted pane's endpoint, as `plant_pane` wrote it, goes first.
    let db = rusqlite::Connection::open(fx.f.d.state.join("cadence.sqlite3")).unwrap();
    db.execute(
        "UPDATE agents SET pid=NULL, endpoint_kind='inbox' WHERE alias='pm-d'",
        [],
    )
    .unwrap();
    let remove = json!({"alias": "pm-d", "force": true});
    fx.f.d.operator_rpc("agent_remove", remove).unwrap();
    let mut again = fx.pane("pm-d");
    fx.refuse(&mut again, "", "not designated");
    assert_eq!(fx.designate("pm-d", true)["changed"], true);
    let (rc, out) = fx.approve(&mut again, "");
    assert_eq!(rc, 0, "{out}");
}

/// Two PASS notes on the exact head from distinct non-author reviewers,
/// compared by alias however the `From:` line is decorated, and no
/// reviewer on that head saying human.
#[test]
fn cad918_delegated_approval_needs_two_independent_passes_on_the_head() {
    let fx = Fx::start();
    let mut pm = fx.pane("pm-d");
    fx.designate("pm-d", true);
    let r1 = fx.note("01-r1", "r1", HEAD, "delegated (5)");
    fx.refuse(&mut pm, "", "found 1");
    for (name, from, head) in [
        ("02-r1", "r1", HEAD),
        ("02-r1d", "`r1` (claude opus)", HEAD),
        ("03-w1", "w1", HEAD),
        ("04-pm", "pm-d", HEAD),
        ("04-pmd", "pm-d (claude opus)", HEAD),
        ("04-pmc", "pm-d, standards", HEAD),
        ("05-r2", "r2", OLD),
    ] {
        let note = fx.note(name, from, head, "delegated (5)");
        fx.refuse(&mut pm, "", "found 1");
        std::fs::remove_file(note).unwrap();
    }
    // A symlinked note is skipped, never followed (as `audit verdicts`).
    let outside = fx.home.path().join("20261001-000008-r2-verdict.md");
    let note = fx.note("08-r2", "r2", HEAD, "delegated (5)");
    std::fs::rename(&note, &outside).unwrap();
    std::os::unix::fs::symlink(&outside, &note).unwrap();
    fx.refuse(&mut pm, "", "found 1");
    std::fs::remove_file(note).unwrap();
    // One reader (CAD-959): a note whose title and section disagree, or
    // whose section and inline line disagree, is a conflict, not a pass.
    let head_line = format!("pass — PR #7, head {HEAD}");
    for (name, title, tail) in [
        ("06-c1", "revise", ""),
        ("06-c2", "pass", "\nVerdict: revise\n"),
    ] {
        let path = fx.notes.join(format!("20261001-0000{name}-verdict.md"));
        let text = format!(
            "# Verdict: CAD-1 Standards review — {title}\n> Issue: CAD-1\n> From: r2\n\n\
             ## Verdict\n{head_line}\n\nRisk: delegated (5)\n{tail}"
        );
        std::fs::write(&path, text).unwrap();
        fx.refuse(&mut pm, "", "is a conflict");
        std::fs::remove_file(path).unwrap();
    }
    let (rc, out) = fx.approve_pr(&mut pm, 7, OLD, "");
    refused(rc, &out, "an approval binds the exact head");
    fx.note("06-r2", "r2", HEAD, "delegated (5)");
    let human = fx.note("07-r3", "r3", HEAD, "human (1)");
    fx.refuse(&mut pm, "", "Risk: human");
    assert!(fx.delegated().is_empty());
    std::fs::remove_file(human).unwrap();
    let (rc, out) = fx.approve(&mut pm, "");
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains(r1.to_str().unwrap()), "{out}");
}

/// CI counts only check runs from the required app, for every check
/// the base branch requires; a status never stands in, and qa-verdict
/// in any state but success refuses.
#[test]
fn cad918_delegated_approval_ci_counts_only_required_actions_checks() {
    let fx = Fx::start();
    let mut pm = fx.pane("pm-d");
    fx.designate("pm-d", true);
    fx.note("01-r1", "r1", HEAD, "delegated (5)");
    fx.note("02-r2", "r2", HEAD, "delegated (5)");
    let run = |name: &str, status: &str, app: u64| json!({"name": name, "status": status, "conclusion": "success", "app": {"id": app}, "check_suite": {"id": 500}});
    let runs = |r: Vec<Value>| json!({"total_count": r.len(), "check_runs": r});
    let status = json!([{"__typename": "StatusContext", "context": "test", "state": "SUCCESS"}]);
    fx.set("/runs", runs(vec![]));
    fx.set("/view/statusCheckRollup", status);
    fx.refuse(&mut pm, "", "'test' has no run");
    fx.set("/runs", runs(vec![run("test", "completed", 99)]));
    fx.refuse(&mut pm, "", "'test' has no run");
    fx.set("/runs", runs(vec![run("test", "in_progress", 15368)]));
    fx.refuse(&mut pm, "", "not a completed success");
    fx.set(
        "/runs",
        json!({"total_count": 5, "check_runs": [run("test", "completed", 15368)]}),
    );
    fx.refuse(&mut pm, "", "did not list every check run");
    fx.set("/runs", runs(vec![run("test", "completed", 15368)]));
    let mut checks =
        fx.gh_state()["branch"]["protection"]["required_status_checks"]["checks"].clone();
    checks
        .as_array_mut()
        .unwrap()
        .push(json!({"context": "fmt", "app_id": 15368}));
    fx.set("/branch/protection/required_status_checks/checks", checks);
    fx.refuse(&mut pm, "", "'fmt' has no run");
    fx.set("/branch", json!({}));
    fx.refuse(&mut pm, "", "declares no required checks");
    fx.pr(&["src/cli/status.rs"]);
    let qa = json!([{"__typename": "StatusContext", "context": "qa-verdict", "state": "FAILURE"}]);
    fx.set("/view/statusCheckRollup", qa);
    fx.refuse(&mut pm, "", "qa-verdict is FAILURE");
    // A forged Actions run: right name and app, but not from a
    // pull_request run of ci.yml for this head.
    fx.pr(&["src/cli/status.rs"]);
    fx.set("/runs/check_runs/0/check_suite/id", json!(900));
    fx.refuse(
        &mut pm,
        "",
        "in a pull_request run of .github/workflows/ci.yml",
    );
    fx.pr(&["src/cli/status.rs"]);
    fx.set("/workflows/workflow_runs/0/event", json!("push"));
    fx.refuse(
        &mut pm,
        "",
        "in a pull_request run of .github/workflows/ci.yml",
    );
    fx.pr(&["src/cli/status.rs"]);
    fx.set(
        "/workflows/workflow_runs/0/path",
        json!(".github/workflows/mine.yml"),
    );
    fx.refuse(
        &mut pm,
        "",
        "in a pull_request run of .github/workflows/ci.yml",
    );
    fx.pr(&["src/cli/status.rs"]);
    fx.set("/workflows/workflow_runs/0/head_sha", json!(OLD));
    fx.refuse(
        &mut pm,
        "",
        "in a pull_request run of .github/workflows/ci.yml",
    );
    fx.pr(&["src/cli/status.rs"]);
    // A run of another PR on the same head; then this PR's number on an
    // attacker-made base; then a fork run that names no PR.
    let other = json!([{"number": 99, "base": {"ref": "main"}}]);
    fx.set("/workflows/workflow_runs/0/pull_requests", other);
    fx.refuse(
        &mut pm,
        "",
        "in a pull_request run of .github/workflows/ci.yml",
    );
    fx.pr(&["src/cli/status.rs"]);
    fx.set(
        "/workflows/workflow_runs/0/pull_requests/0/base/ref",
        json!("evil-base"),
    );
    fx.refuse(
        &mut pm,
        "",
        "in a pull_request run of .github/workflows/ci.yml",
    );
    fx.pr(&["src/cli/status.rs"]);
    fx.set("/workflows/workflow_runs/0/pull_requests", json!([]));
    fx.refuse(
        &mut pm,
        "",
        "in a pull_request run of .github/workflows/ci.yml",
    );
    // A real failure, then a later same-name success elsewhere.
    fx.pr(&["src/cli/status.rs"]);
    let real = json!({"id": 1, "name": "test", "status": "completed", "conclusion": "failure",
                      "app": {"id": 15368}, "check_suite": {"id": 500}});
    let forged = json!({"id": 2, "name": "test", "status": "completed", "conclusion": "success",
                        "app": {"id": 15368}, "check_suite": {"id": 900}});
    fx.set(
        "/runs",
        json!({"total_count": 2, "check_runs": [real, forged]}),
    );
    fx.refuse(&mut pm, "", "'test' is not a completed success");
    fx.pr(&["src/cli/status.rs"]);
    fx.set("/workflows/total_count", json!(3));
    fx.refuse(&mut pm, "", "did not list every workflow run");
    assert!(fx.delegated().is_empty());
    fx.pr(&["src/cli/status.rs"]);
    let (rc, out) = fx.approve(&mut pm, "");
    assert_eq!(rc, 0, "{out}");
}

/// Identity is the connection's: a forged field is refused, and a
/// detached child of the designated pane, still carrying its
/// CADENCE_ALIAS, derives no agent and records nothing.
#[test]
fn cad918_delegated_approval_refuses_forged_identity_and_detached_child() {
    let fx = Fx::start();
    let mut pm = fx.pane("pm-d");
    fx.designate("pm-d", true);
    fx.note("01-r1", "r1", HEAD, "delegated (5)");
    fx.note("02-r2", "r2", HEAD, "delegated (5)");
    for (field, value) in [
        ("approver", "delegated:pm-z"),
        ("as", "pm-z"),
        ("by", "pm-z"),
    ] {
        let mut p = json!({"pr": 7, "head": HEAD, "repo": REPO, "source": "s"});
        p[field] = json!(value);
        let r = pm.rpc(&fx.f.d.state, "approval_delegate", p);
        let msg = r["error"]["message"].as_str().unwrap_or_default();
        assert!(msg.contains(&format!("'{field}'")), "{field}: {r}");
    }
    fx.refuse(&mut pm, "--as pm-z", "--as");
    let dir = fx.home.path().join("detached");
    std::fs::create_dir_all(&dir).unwrap();
    let inner = format!(
        "{} --state-dir {} audit approve --delegated --pr 7 --head {HEAD} --repo {REPO} \
         --source s > {d}/out 2>&1; echo $? > {d}/rc",
        env!("CARGO_BIN_EXE_cadence"),
        fx.f.d.state.display(),
        d = dir.display()
    );
    let (rc, out) = pm.run(&format!(
        "CADENCE_ALIAS=pm-d setsid -f sh -c '{inner}' </dev/null >/dev/null 2>&1; \
         for i in $(seq 200); do [ -s {d}/rc ] && break; sleep 0.05; done; \
         cat {d}/rc {d}/out",
        d = dir.display()
    ));
    assert_eq!(rc, 0, "{out}");
    assert!(
        !out.starts_with("0\n"),
        "the detached child was accepted: {out}"
    );
    assert!(
        out.contains("derives no agent identity") || out.contains("operator"),
        "{out}"
    );
    assert!(fx.delegated().is_empty());
}

/// The path check is an allowlist whatever the reviewers wrote: every
/// path, both sides of a rename, must be delegable. Only a schema path
/// passes, and only under a live, single-use scope pre-approval bound to
/// the ticket's text and lane branch.
#[test]
fn cad918_delegated_approval_path_allowlist_overrides_reviewers() {
    let fx = Fx::start();
    let mut pm = fx.pane("pm-d");
    fx.designate("pm-d", true);
    let scope = json!({"issue": "CAD-1", "source": "op"});
    let scope = fx.f.d.operator_rpc("approval_scope", scope).unwrap();
    let scope_id = scope["approval_id"].as_str().unwrap().to_string();
    let risk = format!("delegated (scope {scope_id})");
    fx.note("01-r1", "r1", HEAD, &risk);
    fx.note("02-r2", "r2", HEAD, &risk);
    let with_scope = format!("--scope {scope_id}");
    for (path, why) in [
        (".github/workflows/ci.yml", "trigger 4"),
        ("docs/roles/risk-classes.md", "trigger 7"),
        ("src/peer.rs", "trigger 1"),
        ("src/daemon/jobs_rpc.rs", "outside the delegable allowlist"),
    ] {
        fx.pr(&["src/cli/status.rs", path]);
        fx.refuse(
            &mut pm,
            &with_scope,
            &format!("{path} needs operator ({why}"),
        );
    }
    fx.pr(&["src/cli/status.rs"]);
    fx.set(
        "/diff",
        json!(
            "diff --git a/src/audit.rs b/src/cli/status.rs\nsimilarity index 98%\n\
               rename from src/audit.rs\nrename to src/cli/status.rs\n"
        ),
    );
    fx.refuse(
        &mut pm,
        &with_scope,
        "src/audit.rs needs operator (trigger 7",
    );
    fx.pr(&["src/store/schema.rs"]);
    fx.refuse(&mut pm, "", "a schema path needs operator");
    assert!(fx.delegated().is_empty());
    let issue = fx.f.pm_dir.join("cad/CAD-1/issue.md");
    let text = std::fs::read_to_string(&issue).unwrap();
    std::fs::write(&issue, format!("{text}\nwidened scope\n")).unwrap();
    fx.refuse(&mut pm, &with_scope, "no live scope pre-approval");
    std::fs::write(&issue, &text).unwrap();
    fx.set("/view/title", json!("CAD-2: other work"));
    fx.refuse(&mut pm, &with_scope, "no live scope pre-approval of CAD-2");
    fx.set("/view/title", json!("CAD-1: demo work"));
    fx.set("/view/headRefName", json!("cadence/other-lane"));
    fx.refuse(&mut pm, &with_scope, "is not a lane branch of CAD-1");
    fx.set("/view/headRefName", json!(LANE));
    let (rc, out) = fx.approve(&mut pm, &with_scope);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(fx.delegated()[0]["scope_approval"], scope_id.as_str());
    // Single use: a second PR cannot ride the same pre-approval.
    fx.set("/pr", json!("8"));
    fx.set("/view/headRefOid", json!(OLD));
    fx.set("/workflows/workflow_runs/0/head_sha", json!(OLD));
    fx.set(
        "/workflows/workflow_runs/0/pull_requests/0/number",
        json!(8),
    );
    fx.note_pr("03-r1", "r1", OLD, &risk, 8);
    fx.note_pr("04-r2", "r2", OLD, &risk, 8);
    let (rc, out) = fx.approve_pr(&mut pm, 8, OLD, &with_scope);
    refused(rc, &out, "already consumed");
    assert_eq!(fx.delegated().len(), 1);
}

/// Designation and scope pre-approval are the operator's, and the
/// operator's generic verb cannot forge a delegated record.
#[test]
fn cad918_designation_and_scope_are_operator_only() {
    let fx = Fx::start();
    let mut pm = fx.pane("pm-d");
    let (rc, out) = pm.cadence(
        &fx.f.d.state,
        "audit designate pm-d --project cad --source me",
    );
    refused(rc, &out, "operator action");
    let designate = json!({"alias": "pm-d", "project": "cad", "source": "me"});
    let r = pm.rpc(&fx.f.d.state, "approval_designate", designate);
    let msg = r["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("operator action"), "{r}");
    let (rc, out) = pm.cadence(
        &fx.f.d.state,
        "audit approve --issue CAD-1 --action scope --source me",
    );
    refused(rc, &out, "operator action");
    let (_, out) = pm.cadence(&fx.f.d.state, "audit designations");
    let listed = serde_json::from_str::<Value>(&out).unwrap();
    assert_eq!(listed["designations"], json!([]), "{out}");
    let forged = json!({"source": "op", "action": "delegated-merge", "head": HEAD,
                        "repo": REPO, "pr": 7});
    let err = fx.f.d.operator_rpc("approval_record", forged).unwrap_err();
    assert!(err.to_string().contains("its own verb"), "{err}");
    assert!(fx.delegated().is_empty());
}

/// No board (HTTP) path can be weaker than the RPC: it relays none.
#[test]
fn cad918_board_relays_no_approval_verb() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let dir = std::fs::read_dir(root.join("ui"))
        .unwrap()
        .map(|e| e.unwrap().path());
    let verbs = [
        "approval_record",
        "approval_delegate",
        "approval_designate",
        "approval_scope",
    ];
    for f in dir
        .chain([root.join("ui.rs")])
        .filter(|f| f.extension().is_some_and(|e| e == "rs"))
    {
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(
            !verbs.iter().any(|v| text.contains(v)),
            "{} relays one",
            f.display()
        );
    }
}
