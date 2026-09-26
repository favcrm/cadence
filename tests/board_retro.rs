//! board_retro: area tests split from tests/board.rs (CAD-537).
//! Board e2e: the `cadence issue` CLI against a temp PM dir, and the
//! `cadence ui` HTTP server in-process.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod board_common;
use board_common::*;

use std::path::Path;
use tempfile::TempDir;

// ---------- issue retro ----------

/// Point pm.yaml's `notes_dir` at a scratch dir so the retro never
/// reads the host's real agent-notes.
fn set_notes_dir(pm: &Path, notes: &Path) {
    let file = pm.join("pm.yaml");
    let text = std::fs::read_to_string(&file).unwrap();
    let mut out = String::new();
    for line in text.lines() {
        if line.starts_with("notes_dir:") {
            out.push_str(&format!("notes_dir: {}\n", notes.display()));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    std::fs::write(&file, out).unwrap();
}

fn write_note(notes: &Path, name: &str, text: &str) {
    std::fs::write(notes.join(name), text).unwrap();
}

#[test]
fn retro_reports_rounds_defects_flakes_and_unknowns() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let notes = TempDir::new().unwrap();
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "project", "add", "cadence", "--prefix", "CAD"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "new", "the work", "--project", "cadence"]
        )
        .0
    );
    for s in ["ready", "doing", "review"] {
        assert!(
            cli(
                pm.path(),
                state.path(),
                &["issue", "set", "CAD-1", &format!("status={s}")]
            )
            .0
        );
    }
    // A comment mentioning a flake becomes flake evidence.
    assert!(
        cli(
            pm.path(),
            state.path(),
            &[
                "issue",
                "comment",
                "CAD-1",
                "-m",
                "one transient failure on the lock probe, green on rerun",
                "--author",
                "qa-1"
            ]
        )
        .0
    );
    set_notes_dir(pm.path(), notes.path());

    // Header-tagged qa + verdict notes; a kickoff that only matches
    // by filename (no `Issue:` header) — the weaker evidence class.
    write_note(
        notes.path(),
        "20260920-100000-60747-cad-1-the-work-kickoff.md",
        "# CAD-1 kickoff\n\nNo Issue header — filename match only.\n",
    );
    write_note(
        notes.path(),
        "20260920-110000-60747-x-qa.md",
        "# QA report\n> Issue: `CAD-1`\n\nRound 1 done; gates green.\n",
    );
    write_note(
        notes.path(),
        "20260920-120000-60747-x-verdict.md",
        "# Verdict: CAD-1\n> Issue: `CAD-1`\n\n## Verdict\n**Blocked.**\n\n## Blocking finding\nThe join matched pid alone — stale rows could manufacture ownership.\n",
    );
    write_note(
        notes.path(),
        "20260920-130000-60747-x-verdict.md",
        "# Verdict: CAD-1 round 2\n> Issue: `CAD-1`\n\n## Verdict\n**Pass.** Ship it.\n",
    );

    let before = commits(pm.path());
    let (ok, v) = cli(
        pm.path(),
        state.path(),
        &["issue", "retro", "CAD-1", "--json"],
    );
    assert!(ok, "{v}");
    // Read-only: the tracker gained no commits.
    assert_eq!(commits(pm.path()), before, "retro must not write");

    assert_eq!(v["schema"], "cadence.retro/1");
    assert_eq!(v["id"], "CAD-1");
    // Two verdict notes = two review rounds; round 1 blocked.
    assert_eq!(v["review"]["rounds"], 2, "{v}");
    let verdicts = v["review"]["verdict_notes"].as_array().unwrap();
    assert_eq!(verdicts[0]["outcome"], "not-pass");
    assert_eq!(verdicts[1]["outcome"], "pass");
    assert_eq!(v["review"]["qa_reports"], 1);
    assert_eq!(v["review"]["kickoffs"], 1);
    assert_eq!(v["review"]["comments"], 1);
    // The filename-only kickoff is labelled with the weaker match.
    let fnote = v["review"]["verdict_notes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|n| n["match"] == "header");
    assert!(fnote, "verdict notes here are all header-tagged: {v}");

    // The blocked round surfaces as a defect with its finding text.
    let defects = v["defects"].as_array().unwrap();
    assert_eq!(defects.len(), 1, "{v}");
    assert!(defects[0]["summary"]
        .as_str()
        .unwrap()
        .contains("join matched pid alone"));
    assert!(defects[0]["source"]
        .as_str()
        .unwrap()
        .contains("verdict.md"));

    // Flake keyword hit from the comment.
    let flakes = v["flakes"].as_array().unwrap();
    assert!(!flakes.is_empty());
    assert!(flakes
        .iter()
        .any(|f| f["text"].as_str().unwrap().contains("transient")));

    // Timings from set-transitions; done_at falls back to the
    // passing-verdict note while no `set status=done` exists.
    assert!(v["timings"]["ready_at"].is_string());
    assert!(v["timings"]["doing_at"].is_string());
    assert!(v["timings"]["review_at"].is_string());
    assert_eq!(v["timings"]["done_at"], "2026-09-20T13:00:00Z");

    // With status=done the tracker commit supplies done_at and the
    // lead time becomes a real (small) number.
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "set", "CAD-1", "status=done"]
        )
        .0
    );
    let (ok, v) = cli(
        pm.path(),
        state.path(),
        &["issue", "retro", "CAD-1", "--json"],
    );
    assert!(ok, "{v}");
    assert!(v["timings"]["done_at"].is_string());
    assert_ne!(v["timings"]["done_at"], "2026-09-20T13:00:00Z");
    assert!(v["timings"]["lead_hours_ready_to_done"].is_number());

    // Explicit unknowns: merged_at (no code commits), human_minutes,
    // defect classes, and the absent store.
    let unknown_fields: Vec<&str> = v["unknowns"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|u| u["field"].as_str())
        .collect();
    for f in [
        "merged_at",
        "human_minutes",
        "defect_classes",
        "store_verdicts",
    ] {
        assert!(unknown_fields.contains(&f), "missing unknown {f}: {v}");
    }
    assert_eq!(v["sources_state"]["store"], "absent");

    // Lessons are proposed only — promotion text is pinned.
    let lessons = v["proposed_lessons"].as_array().unwrap();
    assert!(!lessons.is_empty());
    assert!(lessons
        .iter()
        .all(|l| l["promotion"].as_str().unwrap().contains("manual")));
}

#[test]
fn retro_minimal_issue_marks_everything_unknown() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let notes = TempDir::new().unwrap();
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "project", "add", "cadence", "--prefix", "CAD"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "new", "bare", "--project", "cadence"]
        )
        .0
    );
    set_notes_dir(pm.path(), notes.path());
    let (ok, v) = cli(
        pm.path(),
        state.path(),
        &["issue", "retro", "CAD-1", "--json"],
    );
    assert!(ok, "{v}");
    assert_eq!(v["review"]["rounds"], 0);
    assert!(v["defects"].as_array().unwrap().is_empty());
    assert!(v["flakes"].as_array().unwrap().is_empty());
    assert!(v["proposed_lessons"].as_array().unwrap().is_empty());
    // Never invented: every unproven field is a named unknown.
    let fields: Vec<&str> = v["unknowns"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|u| u["field"].as_str())
        .collect();
    for f in [
        "ready_at",
        "doing_at",
        "review_at",
        "done_at",
        "merged_at",
        "human_minutes",
    ] {
        assert!(fields.contains(&f), "missing unknown {f}: {v}");
    }
}

#[test]
fn retro_rejects_unknown_issue_and_stays_read_only() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let notes = TempDir::new().unwrap();
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    set_notes_dir(pm.path(), notes.path());
    let (ok, err) = cli(
        pm.path(),
        state.path(),
        &["issue", "retro", "CAD-9", "--json"],
    );
    assert!(!ok);
    assert!(err["error"].as_str().unwrap().contains("CAD-9"));
}

#[test]
fn retro_joins_store_verdicts_for_the_issue() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let notes = TempDir::new().unwrap();
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "project", "add", "cadence", "--prefix", "CAD"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "new", "stored", "--project", "cadence"]
        )
        .0
    );
    set_notes_dir(pm.path(), notes.path());

    // Minimal daemon store: one job bound to CAD-1, one task, two
    // verdicts (a not-pass then a pass). Another job's verdict on a
    // different issue must NOT join.
    let db = state.path().join("cadence.sqlite3");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE jobs(id TEXT PRIMARY KEY, issue_id TEXT);
         CREATE TABLE tasks(id TEXT PRIMARY KEY, job_id TEXT NOT NULL);
         CREATE TABLE verdicts(seq INTEGER PRIMARY KEY AUTOINCREMENT,
             task_id TEXT NOT NULL, revision INTEGER NOT NULL,
             sha TEXT NOT NULL, verdict TEXT NOT NULL,
             reviewer TEXT NOT NULL, created REAL NOT NULL);
         INSERT INTO jobs VALUES('j1','CAD-1'),('j2','CAD-99');
         INSERT INTO tasks VALUES('t1','j1'),('t2','j2');
         INSERT INTO verdicts(task_id,revision,sha,verdict,reviewer,created)
             VALUES('t1',0,'aaa111','changes-requested','qa-1',1758400000.0),
                   ('t1',1,'bbb222','pass','qa-1',1758403600.0),
                   ('t2',0,'ccc333','fail','qa-1',1758400000.0);",
    )
    .unwrap();
    drop(conn);

    let (ok, v) = cli(
        pm.path(),
        state.path(),
        &["issue", "retro", "CAD-1", "--json"],
    );
    assert!(ok, "{v}");
    assert_eq!(v["sources_state"]["store"], "ok");
    let sv = v["review"]["store_verdicts"].as_array().unwrap();
    assert_eq!(sv.len(), 2, "{sv:?}"); // t2/CAD-99 must not join
    assert_eq!(sv[0]["sha"], "aaa111");
    assert_eq!(sv[0]["verdict"], "changes-requested");
    assert_eq!(sv[1]["verdict"], "pass");
    assert!(sv[0]["at"].as_str().unwrap().starts_with("2025-09-20"));
    assert_eq!(v["review"]["store_failed"], 1);
    // The non-pass store verdict surfaces as a defect with its sha.
    assert!(v["defects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["summary"].as_str().unwrap().contains("aaa111")));
    // Store verdicts corroborate review rounds too.
    assert!(v["review"]["store_verdicts"]
        .as_array()
        .unwrap()
        .iter()
        .all(|x| x["task"] == "t1"));
}
