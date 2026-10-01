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

struct Fx {
    f: PlanFixture,
    gh: PathBuf,
    notes: PathBuf,
    home: TempDir,
    _gh_dir: TempDir,
}

impl Fx {
    /// Ticket CAD-1 owned by `w1`, PR #7 at HEAD with green CI touching
    /// one ordinary file, and a notes dir the daemon reads via pm.yaml.
    fn start() -> Fx {
        let gh_dir = TempDir::new().unwrap();
        let bin = gh_dir.path().join("gh");
        std::fs::write(&bin, include_str!("fixtures/delegated-gh.py")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut opts = daemon_opts();
        opts.delivery_gh = Some(bin);
        let f = PlanFixture::start_with(opts);
        // `audit::parse_note` binds `CAD-` ids, so the ticket is one.
        let repo = f.tmp.path().join("repo").canonicalize().unwrap();
        let (ok, out) = f.cli(&[
            "issue",
            "project",
            "add",
            "cad",
            "--prefix",
            "CAD",
            "--repo",
            repo.to_str().unwrap(),
        ]);
        assert!(ok, "{out}");
        let (ok, out) = f.cli(&["issue", "new", "demo work", "--project", "cad"]);
        assert!(ok, "{out}");
        let issue = f.pm_dir.join("cad/CAD-1/issue.md");
        let text = std::fs::read_to_string(&issue).unwrap();
        assert!(
            text.starts_with("---\n") && !text.contains("\nowner:"),
            "{text}"
        );
        std::fs::write(&issue, text.replacen("---\n", "---\nowner: w1\n", 1)).unwrap();
        let notes = f.tmp.path().join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        let yaml = f.pm_dir.join("pm.yaml");
        let mut cfg: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&yaml).unwrap()).unwrap();
        cfg["notes_dir"] = notes.to_str().unwrap().into();
        std::fs::write(&yaml, serde_yaml::to_string(&cfg).unwrap()).unwrap();
        let fx = Fx {
            gh: gh_dir.path().join("delegated-gh.json"),
            f,
            notes,
            home: TempDir::new().unwrap(),
            _gh_dir: gh_dir,
        };
        fx.pr(&["src/cli/job.rs"], "@@ -1 +1 @@\n-a\n+b\n");
        fx
    }

    fn pr(&self, files: &[&str], diff: &str) {
        let files: Vec<Value> = files.iter().map(|p| json!({"path": p})).collect();
        let view = json!({
            "headRefOid": HEAD, "state": "OPEN", "title": "CAD-1: demo work",
            "author": {"login": "gh-bot"}, "changedFiles": files.len(), "files": files,
            "statusCheckRollup": [{"__typename": "CheckRun", "name": "ci",
                                   "status": "COMPLETED", "conclusion": "SUCCESS"}],
        });
        std::fs::write(&self.gh, json!({"view": view, "diff": diff}).to_string()).unwrap();
    }

    /// A verdict note in the AGENTS.md shape.
    fn note(&self, name: &str, from: &str, head: &str, risk: &str) -> PathBuf {
        let path = self.notes.join(format!("20261001-0000{name}-verdict.md"));
        let text = format!(
            "# Verdict: CAD-1 review — pass\n> Issue: CAD-1\n> From: {from}\n\n\
             ## Verdict\npass — PR #7, head {head}\n\nRisk: {risk}\n"
        );
        std::fs::write(&path, text).unwrap();
        path
    }

    fn pane(&self, alias: &str) -> LaneShell {
        let sh = LaneShell::spawn(self.home.path());
        plant_pane(&self.f.d, alias, sh.pid());
        sh
    }

    fn designate(&self, alias: &str, active: bool) {
        let r = self.f.d.operator_rpc(
            "approval_designate",
            json!({"alias": alias, "project": "cad", "source": "op", "active": active}),
        );
        assert!(r.is_ok(), "{r:?}");
    }

    fn approve(&self, sh: &mut LaneShell, extra: &str) -> (i64, String) {
        sh.cadence(
            &self.f.d.state,
            &format!(
                "audit approve --delegated --pr 7 --head {HEAD} --repo {REPO} \
                 --source 'pm in lane' {extra}"
            ),
        )
    }

    fn refuse(&self, sh: &mut LaneShell, extra: &str, why: &str) {
        let (rc, out) = self.approve(sh, extra);
        refused(rc, &out, why);
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

/// The designated agent records once, under its derived identity —
/// two concurrent calls with different sources leave one record — and
/// the digest lists it with its undo commands.
#[test]
fn cad918_delegated_approval_records_once_under_the_derived_identity() {
    let fx = Fx::start();
    let mut pm = fx.pane("pm-d");
    fx.designate("pm-d", true);
    fx.note("01-r1", "r1", HEAD, "delegated (5)");
    fx.note("02-r2", "r2", HEAD, "delegated (2)");
    let cmd = |src: &str| {
        format!(
            "{} --state-dir {} audit approve --delegated --pr 7 --head {HEAD} --repo {REPO} \
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
    assert_eq!(recs[0]["verdicts"].as_array().unwrap().len(), 2, "{recs:?}");
    let (rc, out) = fx.approve(&mut pm, "");
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["duplicate"],
        true,
        "{out}"
    );
    assert_eq!(fx.delegated().len(), 1);

    let (rc, out) = pm.cadence(&fx.f.d.state, "audit digest");
    assert_eq!(rc, 0, "{out}");
    let j: Value = serde_json::from_str(&out).unwrap();
    let row = &j["delegated"][0];
    assert_eq!(row["approver"], "delegated:pm-d", "{j}");
    assert!(
        row["revoke"]
            .as_str()
            .unwrap()
            .contains("cadence audit revoke"),
        "{j}"
    );
    assert!(
        row["revert"].as_str().unwrap().contains("git revert"),
        "{j}"
    );
    let id = recs[0]["approval_id"].as_str().unwrap();
    fx.f.d
        .operator_rpc(
            "approval_revoke",
            json!({"id": id, "source": "op", "reason": "x"}),
        )
        .unwrap();
    let (_, out) = pm.cadence(&fx.f.d.state, "audit digest");
    let j: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(j["delegated"][0]["revoked"], true, "{j}");
}

/// Only a designated agent, never an author or the operator, and
/// never after the designation is withdrawn.
#[test]
fn cad918_delegated_approval_refuses_undesignated_author_and_operator() {
    let fx = Fx::start();
    fx.note("01-r1", "r1", HEAD, "delegated (5)");
    fx.note("02-r2", "r2", HEAD, "delegated (5)");
    let mut stranger = fx.pane("pm-x");
    fx.refuse(&mut stranger, "", "not designated");
    let mut author = fx.pane("w1");
    fx.designate("w1", true);
    fx.refuse(&mut author, "", "an author never approves");
    // A designation never passes to an alias registered after it.
    fx.designate("pm-late", true);
    let mut late = fx.pane("pm-late");
    fx.refuse(&mut late, "", "not designated");
    let mut pm = fx.pane("pm-d");
    fx.designate("pm-d", true);
    fx.designate("pm-d", false);
    fx.refuse(&mut pm, "", "not designated");
    let params = json!({"pr": 7, "head": HEAD, "repo": REPO, "source": "op"});
    let err = fx.f.d.operator_rpc("approval_delegate", params);
    let err = err.unwrap_err();
    assert!(err.to_string().contains("designated agent's act"), "{err}");
    assert!(fx.delegated().is_empty());
}

/// Two PASS notes on the exact head from distinct non-author reviewers,
/// and no reviewer on that head saying human.
#[test]
fn cad918_delegated_approval_needs_two_independent_passes_on_the_head() {
    let fx = Fx::start();
    let mut pm = fx.pane("pm-d");
    fx.designate("pm-d", true);
    let r1 = fx.note("01-r1", "r1", HEAD, "delegated (5)");
    fx.refuse(&mut pm, "", "found 1");
    // The same reviewer twice, the author, the approver, a stale head.
    for (name, from, head) in [
        ("02-r1", "r1", HEAD),
        ("03-w1", "w1", HEAD),
        ("04-pm", "pm-d", HEAD),
        ("05-r2", "r2", OLD),
    ] {
        let note = fx.note(name, from, head, "delegated (5)");
        fx.refuse(&mut pm, "", "found 1");
        std::fs::remove_file(note).unwrap();
    }
    let (rc, out) = pm.cadence(
        &fx.f.d.state,
        &format!("audit approve --delegated --pr 7 --head {OLD} --repo {REPO} --source s"),
    );
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

/// The mechanical path check holds whatever the reviewers wrote:
/// triggers 4 and 7 always refuse, trigger 1 and schema paths only
/// pass under a live scope pre-approval bound to the ticket's text.
#[test]
fn cad918_delegated_approval_path_check_overrides_reviewers() {
    let fx = Fx::start();
    let mut pm = fx.pane("pm-d");
    fx.designate("pm-d", true);
    let scope =
        fx.f.d
            .operator_rpc("approval_scope", json!({"issue": "CAD-1", "source": "op"}))
            .unwrap();
    let scope_id = scope["approval_id"].as_str().unwrap().to_string();
    let risk = format!("delegated (scope {scope_id})");
    fx.note("01-r1", "r1", HEAD, &risk);
    fx.note("02-r2", "r2", HEAD, &risk);
    let with_scope = format!("--scope {scope_id}");
    for path in [
        ".github/workflows/ci.yml",
        "docs/roles/risk-classes.md",
        "Cargo.lock",
    ] {
        fx.pr(&["src/cli/job.rs", path], "");
        fx.refuse(&mut pm, &with_scope, "never delegable");
    }
    fx.pr(&["src/store/schema.rs"], "");
    fx.refuse(&mut pm, "", "trigger schema");
    fx.pr(
        &["src/ui.rs"],
        "@@ -9 +9 @@ fn write_caller(r: &Request) {\n-a\n+b\n",
    );
    fx.refuse(&mut pm, "", "symbol write_caller");
    assert!(fx.delegated().is_empty());
    let issue = fx.f.pm_dir.join("cad/CAD-1/issue.md");
    let text = std::fs::read_to_string(&issue).unwrap();
    std::fs::write(&issue, format!("{text}\nwidened scope\n")).unwrap();
    fx.refuse(&mut pm, &with_scope, "no live scope pre-approval");
    std::fs::write(&issue, text).unwrap();
    let (rc, out) = fx.approve(&mut pm, &with_scope);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(fx.delegated()[0]["scope_approval"], scope_id.as_str());
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
    let r = pm.rpc(
        &fx.f.d.state,
        "approval_designate",
        json!({"alias": "pm-d", "project": "cad", "source": "me"}),
    );
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("operator action"),
        "{r}"
    );
    let (rc, out) = pm.cadence(
        &fx.f.d.state,
        "audit approve --issue CAD-1 --action scope --source me",
    );
    refused(rc, &out, "operator action");
    let (_, out) = pm.cadence(&fx.f.d.state, "audit designations");
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["designations"],
        json!([]),
        "{out}"
    );
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
    for f in dir
        .chain([root.join("ui.rs")])
        .filter(|f| f.extension().is_some_and(|e| e == "rs"))
    {
        let text = std::fs::read_to_string(&f).unwrap();
        let verbs = [
            "approval_record",
            "approval_delegate",
            "approval_designate",
            "approval_scope",
        ];
        assert!(
            !verbs.iter().any(|v| text.contains(v)),
            "{} relays an approval verb",
            f.display()
        );
    }
}
