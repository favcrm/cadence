//! CAD-1241 acceptance check — authored independently of the
//! implementation (the "Gates and security work" rule: the check
//! proving the refused/fixed case is written by someone other than
//! the implementer, and the implementer may not edit or weaken it).
//!
//! What it proves, against the REAL `run_inner` classification and
//! write path — synthetic pm dir, real git repo, injected PR lookups,
//! never live GitHub:
//!
//! - A batch of persistently unresolved older candidates (open,
//!   stalled, held) sorts ahead of a genuinely merged later issue. On
//!   the pre-CAD-1241 repeated-prefix scan the merged issue is never
//!   reached. With the daemon cursor, repeated bounded runs resume
//!   after the last evidence-bearing classified id, wrap, and the
//!   merged issue's file status becomes `done` within a bounded
//!   number of ticks.
//! - Every run's `classified` count stays <= the batch limit, and the
//!   candidates that must not move keep their outcomes: `open`
//!   (unmerged), `stalled` (closed-unmerged PR), `held` (open PR,
//!   post-merge claim, unverifiable PR state) all remain `doing`.
//!
//! INTEGRATION NOTE (for the parent / whoever registers this module):
//! this file was written against the anticipated cursor API —
//!
//!     run_inner(pm, project, dry_run, actor, state_dir, limit,
//!               cursor: Option<&str>,   // last classified id of the prior tick
//!               pr_list, pr_view, close_lock_wait)
//!
//! returning the report JSON with a `"cursor"` field holding the id of
//! the last evidence-bearing candidate this run classified, or `null`
//! when the run reached the end of the candidate list (the next tick
//! wraps to the head). The ONLY adaptation points are the two helpers
//! [`daemon_sweep`] and [`next_cursor`]: if the landed API instead
//! takes `&mut Option<String>`, returns `(Value, Option<String>)`, or
//! names the field differently, change those two helpers and nothing
//! else — the assertions below must not be weakened.
//!
//! Register with `mod cad1241_acceptance;` inside `reconcile.rs` — the
//! `#![cfg(test)]` below already confines it to test builds, gated or
//! not.

#![cfg(test)]

use super::*;
use crate::issue::model::{Claim, Front, Ref};
use std::fs;
use tempfile::TempDir;

/// The per-tick bound under test — stands in for `RECONCILE_BATCH`.
const BATCH: usize = 3;

/// A throwaway pm dir + one project + one real git repo, shaped like
/// the rig in `reconcile.rs`'s own tests but self-contained here so a
/// refactor of that test module cannot move this check.
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
    // Board views read the notes dir — a throwaway path, never the
    // default /var/www/agent-notes.
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
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    fs::write(repo.join("f"), "one").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-m", "one"]);
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

/// `cadence/<name>` with one commit past main; merged into main when
/// `merged`. Returns the branch tip.
fn lane(repo: &Path, name: &str, merged: bool) -> String {
    let branch = crate::worktree::layout::branch(name);
    git(repo, &["checkout", "-q", "-b", &branch]);
    fs::write(repo.join("f"), format!("work on {name}")).unwrap();
    git(repo, &["commit", "-qam", name]);
    git(repo, &["checkout", "-q", "main"]);
    if merged {
        git(repo, &["merge", "-q", "--no-ff", "-m", "m", &branch]);
    }
    git(repo, &["rev-parse", &branch])
}

/// One `doing` issue whose refs are the worktree+branch pair
/// `issue start` would mint for lane `name` under `rig.repo`.
fn put_lane_issue(rig: &Rig, n: u32, name: &str, claim_at: Option<&str>) -> String {
    let id = format!("CAD-{n}");
    let dir = rig.pm.dir.join(&rig.key).join(&id);
    fs::create_dir_all(&dir).unwrap();
    let mut front = Front::new(&id, "t", "2026-09-28T00:00:00Z");
    front.status = "doing".to_string();
    if let Some(at) = claim_at {
        front.claim = Some(Claim {
            by: "op".to_string(),
            at: at.to_string(),
            session: None,
            last_seen: None,
            note: None,
            stale: None,
        });
    }
    for (kind, path) in [
        (
            "worktree",
            crate::worktree::layout::worktree_dir(&rig.repo, name)
                .display()
                .to_string(),
        ),
        ("branch", crate::worktree::layout::branch(name)),
    ] {
        front.refs.push(Ref {
            kind: kind.to_string(),
            path: Some(path),
            url: None,
            label: None,
            closed: None,
            worktree: None,
            cargo_target: None,
            agent: None,
        });
    }
    write::save_front(&dir, &front, "").unwrap();
    id
}

/// ADAPTATION POINT 1 — one daemon-equivalent bounded run: no state
/// dir (skips the finish sweep, like `run_daemon`), no lock wait.
/// `cursor` is the in-memory resume position the stall-watch keeps
/// between ticks — `None` scans from the first candidate.
fn daemon_sweep(rig: &Rig, cursor: Option<&str>, pl: PrLookup<'_>, pv: PrView<'_>) -> Value {
    run_inner(
        &rig.pm,
        None,
        false,
        "op",
        None,
        BATCH,
        cursor,
        pl,
        pv,
        Duration::ZERO,
    )
    .unwrap()
}

/// ADAPTATION POINT 2 — where the report carries the resume position
/// for the next tick: the last evidence-bearing candidate classified,
/// or `None` once a run reached the end of the list (wrap next tick).
fn next_cursor(out: &Value) -> Option<String> {
    out["cursor"].as_str().map(str::to_string)
}

fn no_view(_: &Path, _: &str) -> GhProbe {
    GhProbe::Unreachable
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

/// THE acceptance check: six unresolved older candidates ahead of a
/// merged CAD-99, batch of three. Pre-cursor code starves CAD-99 on
/// every tick; with the cursor, three bounded runs close it and a
/// fourth wraps to the head without moving anything that must stay.
#[test]
fn bounded_daemon_runs_eventually_close_a_later_merged_issue() {
    let rig = rig();

    // Older candidates, natural-id sorted ahead of CAD-99 — each one
    // unresolved on EVERY tick, so a repeated-prefix scan never
    // advances past them:
    //   CAD-10, CAD-11 — committed lanes, never merged → open/unmerged
    //   CAD-12         — lane's only PR closed unmerged → stalled
    //   CAD-13         — merged lane + an OPEN follow-up PR → held/open-pr
    //   CAD-40         — merged + claim recorded AFTER the merge →
    //                    held/post-merge-claim
    //   CAD-50         — merged, but GitHub unreachable →
    //                    held/pr-state-unverified (never close on a guess)
    let mut unresolved = Vec::new();
    for (n, name) in [(10, "cad-10-x"), (11, "cad-11-x"), (12, "cad-12-x")] {
        unresolved.push(put_lane_issue(&rig, n, name, None));
        lane(&rig.repo, name, false);
    }
    unresolved.push(put_lane_issue(&rig, 13, "cad-13-x", None));
    lane(&rig.repo, "cad-13-x", true);
    unresolved.push(put_lane_issue(
        &rig,
        40,
        "cad-40-x",
        Some("2026-09-27T10:00:00Z"), // claimed AFTER the merge below
    ));
    let cad40_tip = lane(&rig.repo, "cad-40-x", true);
    unresolved.push(put_lane_issue(&rig, 50, "cad-50-x", None));
    lane(&rig.repo, "cad-50-x", true);

    // The later ticket the prefix scan starves: genuinely merged,
    // no claim, no open PR — the sweep must close it.
    let merged = put_lane_issue(&rig, 99, "cad-99-x", None);
    lane(&rig.repo, "cad-99-x", true);

    // Injected `gh pr list --head` answers, per branch.
    let pl = move |_: &Path, branch: &str| -> GhProbe {
        match branch {
            "cadence/cad-12-x" => GhProbe::Ok(vec![Pr {
                number: 12,
                state: "closed".to_string(),
                merged_at: None,
                head_oid: "x".to_string(),
                base: "main".to_string(),
            }]),
            "cadence/cad-13-x" => GhProbe::Ok(vec![
                Pr {
                    number: 13,
                    state: "merged".to_string(),
                    merged_at: Some(1),
                    head_oid: "x".to_string(),
                    base: "main".to_string(),
                },
                Pr {
                    number: 14,
                    state: "open".to_string(),
                    merged_at: None,
                    head_oid: "x".to_string(),
                    base: "main".to_string(),
                },
            ]),
            "cadence/cad-40-x" => GhProbe::Ok(vec![Pr {
                number: 40,
                state: "merged".to_string(),
                merged_at: Some(time::parse_iso("2026-09-26T10:00:00Z").unwrap()),
                head_oid: cad40_tip.clone(),
                base: "main".to_string(),
            }]),
            "cadence/cad-50-x" => GhProbe::Unreachable,
            _ => GhProbe::Ok(vec![]),
        }
    };

    // DEFECT PROOF (the old-code reproducer, in place): with no
    // continuation at all — the pre-CAD-1241 repeated-prefix scan —
    // the same fixture starves CAD-99 on every tick forever.
    for tick in 1..=4 {
        let out = daemon_sweep(&rig, None, &pl, &no_view);
        assert_eq!(
            out["classified"].as_u64().unwrap() as usize,
            BATCH,
            "prefix scan tick {tick}: {out}"
        );
        assert_eq!(
            status(&rig, &merged),
            "doing",
            "prefix scan reached the merged ticket — the fixture is wrong"
        );
    }

    // The verdict every persistent candidate must keep, checked on
    // whichever tick classifies it — the protected invariant is the
    // outcome, not which batch the cursor lands it in (exclusive or
    // overlapping resume are both conforming designs).
    let expect: &[(&str, &str, &str)] = &[
        ("CAD-10", "open", "unmerged"),
        ("CAD-11", "open", "unmerged"),
        ("CAD-12", "stalled", "pr-closed-unmerged"),
        ("CAD-13", "held", "open-pr"),
        ("CAD-40", "held", "post-merge-claim"),
        ("CAD-50", "held", "pr-state-unverified"),
    ];

    // Repeated bounded runs with the cursor carried across ticks —
    // exactly what the stall-watch does with its in-memory cursor.
    // Six persistent candidates at batch 3 fill the first tick under
    // every design; the merged ticket must be reached and closed
    // within three more — the deadline for eventual coverage.
    let mut cursor: Option<String> = None;
    let mut closed_tick = None;
    for tick in 1..=4 {
        let out = daemon_sweep(&rig, cursor.as_deref(), &pl, &no_view);
        let classified = out["classified"].as_u64().unwrap() as usize;
        assert!(
            classified <= BATCH,
            "tick {tick} classified {classified} > batch {BATCH}: {out}"
        );
        if tick == 1 {
            // A cold start scans the natural-order prefix.
            assert_eq!(classified, BATCH, "{out}");
            assert_eq!(row(&out, "CAD-10")["issue"], "CAD-10", "{out}");
            assert_eq!(row(&out, "CAD-11")["issue"], "CAD-11", "{out}");
            assert_eq!(row(&out, "CAD-12")["issue"], "CAD-12", "{out}");
        }
        // Every persistent candidate this tick classifies keeps its
        // current outcome — the cursor changes who is examined, never
        // the verdicts.
        for (id, outcome, reason) in expect {
            if let Some(r) = out["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["issue"] == json!(id))
            {
                assert_eq!(r["outcome"], json!(outcome), "{id} on tick {tick}: {out}");
                assert_eq!(r["reason"], json!(reason), "{id} on tick {tick}: {out}");
            }
        }
        cursor = next_cursor(&out);
        if status(&rig, &merged) == "done" {
            closed_tick = Some(tick);
            break;
        }
    }
    assert_eq!(
        status(&rig, &merged),
        "done",
        "merged later issue starved — the repeated-prefix defect"
    );
    assert!(closed_tick.is_some());

    // After the list end the scan wraps: one more bounded tick
    // re-examines persistent candidates and none of them move.
    let out = daemon_sweep(&rig, cursor.as_deref(), &pl, &no_view);
    assert!(
        out["classified"].as_u64().unwrap() as usize <= BATCH,
        "post-wrap tick broke the bound: {out}"
    );

    // Everything unresolved stayed unresolved on file.
    for id in &unresolved {
        assert_eq!(status(&rig, id), "doing", "{id} moved");
    }
}
