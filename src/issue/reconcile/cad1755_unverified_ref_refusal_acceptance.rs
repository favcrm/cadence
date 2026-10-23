//! CAD-1755 refusal acceptance check — authored independently of the
//! implementation (AGENTS.md "Gates and security work": the check
//! proving the bad case is refused is written from the ticket by
//! someone other than the implementer, and the implementer may not
//! edit or weaken it).
//!
//! The rule under test: reconcile never closes a ticket while ANY of
//! its relevant PR/branch refs has an unverified GitHub probe
//! (`GhProbe::Unreachable`), even when another ref already supplied
//! merge evidence. The verdict is `held` with reason
//! `pr-state-unverified`, and the row's `detail.unverified` names the
//! ref that could not be verified.
//!
//! It drives the REAL `run_inner` sweep (probe -> classify -> write
//! path) non-dry-run against a synthetic pm dir and a real git repo,
//! with injected `pr_list` / `pr_view` probes — never live GitHub —
//! and reads the ticket file afterwards: a refused ticket must still
//! be `doing` on disk. A control with every probe answering proves
//! the same shapes DO close, so the refusal is not vacuous.

#![cfg(test)]

use super::*;
use crate::issue::model::{Front, Ref};
use std::fs;
use tempfile::TempDir;

struct Rig {
    _tmp: TempDir,
    pm: Pm,
    repo: PathBuf,
    key: String,
}

fn rig() -> Rig {
    let tmp = TempDir::new().unwrap();
    let t = tmp.path();
    let pm_dir = t.join("pm");
    let mut pm = Pm::init(&pm_dir).unwrap();
    pm.config.notes_dir = t.join("notes").display().to_string();
    let key = "cad".to_string();
    let pdir = pm_dir.join(&key);
    fs::create_dir_all(&pdir).unwrap();
    fs::write(
        pdir.join("project.yaml"),
        "key: cad\nprefix: CAD\nrepos: []\n",
    )
    .unwrap();
    let repo = t.join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    fs::write(repo.join("f"), "one").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-qm", "one"]);
    Rig {
        _tmp: tmp,
        pm,
        repo,
        key,
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// `cadence/<name>` with one commit past main, merged into main when
/// `merged`. Returns the branch tip.
fn lane(repo: &Path, name: &str, merged: bool) -> String {
    let branch = crate::worktree::layout::branch(name);
    git(repo, &["checkout", "-q", "-b", &branch]);
    fs::write(repo.join(format!("f-{name}")), name).unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", name]);
    git(repo, &["checkout", "-q", "main"]);
    if merged {
        git(repo, &["merge", "-q", "--no-ff", "-m", "m", &branch]);
    }
    git(repo, &["rev-parse", &branch])
}

fn mk_ref(kind: &str, path: Option<String>, url: Option<String>) -> Ref {
    Ref {
        kind: kind.to_string(),
        path,
        url,
        label: None,
        closed: None,
        worktree: None,
        cargo_target: None,
        agent: None,
    }
}

/// One unclaimed `doing` issue. `worktree_lane` records the worktree
/// ref `issue start` would mint (its implied branch is a lane, and it
/// gives the repo root); `branches` adds explicit branch refs under
/// that root; `pr_urls` adds `pr` url refs.
fn put_issue(
    rig: &Rig,
    n: u32,
    worktree_lane: Option<&str>,
    branches: &[&str],
    pr_urls: &[&str],
) -> String {
    let id = format!("CAD-{n}");
    let dir = rig.pm.dir.join(&rig.key).join(&id);
    fs::create_dir_all(&dir).unwrap();
    let mut front = Front::new(&id, "t", "2026-09-28T00:00:00Z");
    front.status = "doing".to_string();
    if let Some(name) = worktree_lane {
        front.refs.push(mk_ref(
            "worktree",
            Some(
                crate::worktree::layout::worktree_dir(&rig.repo, name)
                    .display()
                    .to_string(),
            ),
            None,
        ));
    }
    for b in branches {
        front.refs.push(mk_ref(
            "branch",
            Some(crate::worktree::layout::branch(b)),
            None,
        ));
    }
    for u in pr_urls {
        front.refs.push(mk_ref("pr", None, Some(u.to_string())));
    }
    write::save_front(&dir, &front, "").unwrap();
    id
}

fn merged_pr(number: u64, head_oid: &str) -> Pr {
    Pr {
        number,
        state: "merged".to_string(),
        merged_at: Some(time::parse_iso("2026-09-26T10:00:00Z").unwrap()),
        head_oid: head_oid.to_string(),
        base: "main".to_string(),
    }
}

/// The real sweep, non-dry-run, unbounded, no state dir.
fn sweep(rig: &Rig, pl: PrLookup<'_>, pv: PrView<'_>) -> Value {
    run_inner(&rig.pm, None, false, "op", None, 0, None, pl, pv, None).unwrap()
}

fn status(rig: &Rig, id: &str) -> String {
    let dir = rig.pm.dir.join(&rig.key).join(id);
    let (f, _) = write::load_front(&dir).unwrap();
    f.status
}

fn row<'a>(out: &'a Value, id: &str) -> &'a Value {
    out["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["issue"] == json!(id))
        .unwrap_or_else(|| panic!("{id} not classified in {out}"))
}

fn assert_refused(rig: &Rig, out: &Value, id: &str, unverified_ref: &str) {
    let r = row(out, id);
    assert_eq!(
        r["outcome"],
        json!("held"),
        "{id}: partial GitHub success must never close (CAD-1755): {r}"
    );
    assert_eq!(r["reason"], json!("pr-state-unverified"), "{id}: {r}");
    let named = r["detail"]["unverified"]
        .as_array()
        .unwrap_or_else(|| panic!("{id}: detail.unverified missing: {r}"));
    assert!(
        named.iter().any(|v| v == &json!(unverified_ref)),
        "{id}: detail.unverified must name {unverified_ref}: {r}"
    );
    assert_eq!(
        status(rig, id),
        "doing",
        "{id}: ticket file moved although a ref was unverified"
    );
}

fn assert_closed(rig: &Rig, out: &Value, id: &str) {
    let r = row(out, id);
    assert_eq!(r["outcome"], json!("closed"), "{id} control: {r}");
    assert!(
        r["detail"].get("unverified").is_none(),
        "{id} control: nothing unverified expected: {r}"
    );
    assert_eq!(status(rig, id), "done", "{id} control did not close");
}

fn url(n: u32) -> String {
    format!("https://github.com/favcrm/cadence/pull/{n}")
}

/// Builds six ticket shapes and sweeps them once. In the refusal
/// case (`reach == false`) each ticket has exactly one ref whose probe
/// is `Unreachable` while ANOTHER ref of the same ticket answers
/// merged; in the control (`reach == true`) that one probe answers
/// merged too and nothing else differs.
///
/// - CAD-21: lane branch (pr_list) answers merged; its `pr` url
///   (pr_view) is the unverified ref.
/// - CAD-22: `pr` url (pr_view) answers merged; the lane branch
///   (pr_list) is the unverified ref.
/// - CAD-23: two lane branches (both merged locally). `probe` lists
///   explicit branch refs before a worktree's implied branch, so the
///   branch ref `cad-23-b` (unverified) is probed BEFORE the worktree
///   lane `cad-23-a` (answers merged): unreachable, then success.
/// - CAD-26: the same two-lane shape the other way round: the branch
///   ref `cad-26-b` answers merged first, then the worktree lane
///   `cad-26-a` is the unverified ref: success, then unreachable.
/// - CAD-24: two `pr` urls; the first answers merged, the second is
///   the unverified ref.
/// - CAD-25: two `pr` urls in the reverse order: the first is the
///   unverified ref, the second answers merged.
fn run_case(reach: bool) {
    let rig = rig();

    let a21 = put_issue(&rig, 21, Some("cad-21-a"), &[], &[&url(2101)]);
    let tip21 = lane(&rig.repo, "cad-21-a", true);

    let a22 = put_issue(&rig, 22, Some("cad-22-a"), &[], &[&url(2201)]);
    let tip22 = lane(&rig.repo, "cad-22-a", true);

    let a23 = put_issue(&rig, 23, Some("cad-23-a"), &["cad-23-b"], &[]);
    let tip23a = lane(&rig.repo, "cad-23-a", true);
    let tip23b = lane(&rig.repo, "cad-23-b", true);

    let a26 = put_issue(&rig, 26, Some("cad-26-a"), &["cad-26-b"], &[]);
    let tip26a = lane(&rig.repo, "cad-26-a", true);
    let tip26b = lane(&rig.repo, "cad-26-b", true);

    let a24 = put_issue(&rig, 24, None, &[], &[&url(2401), &url(2402)]);
    let a25 = put_issue(&rig, 25, None, &[], &[&url(2501), &url(2502)]);

    // The single probe per ticket that is Unreachable in the refusal
    // case and answers in the control.
    let gate = |answer: GhProbe| {
        if reach {
            answer
        } else {
            GhProbe::Unreachable
        }
    };

    let pl = |_: &Path, branch: &str| -> GhProbe {
        match branch {
            "cadence/cad-21-a" => GhProbe::Ok(vec![merged_pr(21, &tip21)]),
            "cadence/cad-22-a" => gate(GhProbe::Ok(vec![merged_pr(22, &tip22)])),
            "cadence/cad-23-a" => GhProbe::Ok(vec![merged_pr(23, &tip23a)]),
            "cadence/cad-23-b" => gate(GhProbe::Ok(vec![merged_pr(230, &tip23b)])),
            "cadence/cad-26-a" => gate(GhProbe::Ok(vec![merged_pr(26, &tip26a)])),
            "cadence/cad-26-b" => GhProbe::Ok(vec![merged_pr(260, &tip26b)]),
            other => panic!("unexpected pr_list probe for {other}"),
        }
    };
    let pv = |_: &Path, u: &str| -> GhProbe {
        let n: u64 = u.rsplit('/').next().unwrap().parse().unwrap();
        let answer = GhProbe::Ok(vec![merged_pr(n, "0000000")]);
        match n {
            2201 | 2401 | 2502 => answer,
            2101 | 2402 | 2501 => gate(answer),
            _ => panic!("unexpected pr_view probe for {u}"),
        }
    };

    let out = sweep(&rig, &pl, &pv);
    if reach {
        for id in [&a21, &a22, &a23, &a24, &a25, &a26] {
            assert_closed(&rig, &out, id);
        }
    } else {
        assert_refused(&rig, &out, &a21, &url(2101));
        assert_refused(&rig, &out, &a22, "cadence/cad-22-a");
        assert_refused(&rig, &out, &a23, "cadence/cad-23-b");
        assert_refused(&rig, &out, &a26, "cadence/cad-26-a");
        assert_refused(&rig, &out, &a24, &url(2402));
        assert_refused(&rig, &out, &a25, &url(2501));
    }
}

/// THE refusal check: partial GitHub success then an unreachable ref
/// must never close — in both orders and across both ref kinds.
#[test]
fn partial_success_then_unreachable_ref_never_closes() {
    run_case(false);
}

/// Control: identical shapes with every probe answering merged close,
/// so the refusal above is caused by the unverified ref alone.
#[test]
fn control_every_ref_answering_merged_closes() {
    run_case(true);
}
