//! audit_review: area tests split from tests/integration.rs (CAD-426).
//! End-to-end tests: real socket daemon in-process, fake provider.
//! These exercise the observable contract — queue order, idempotency,
//! restart fencing, approval brokering, serialization — without model calls.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod common;
use common::*;

use cadence_agent::store::{NewAgent, Store};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// `cadence review` — the mechanical review routine against a temp repo
// with a fake `gh` and trivial gate commands.
// ---------------------------------------------------------------------------

fn review_git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn review_git_sha(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A fixture repo: `origin` (bare) + `repo` (clone) whose main moved
/// after the PR branches cut, four open PR heads under refs/pull/N/head
/// (7 = clean merge + a new wait-test, 8 = pairwise conflict with 7,
/// 9 = clean, 10 = merge conflict with main), a `cadence-review.toml`
/// driving trivial commands, and a fake `gh` answering from fixtures.
struct ReviewFixture {
    repo: PathBuf,
    state: PathBuf,
    fakebin: PathBuf,
    fakedir: PathBuf,
    gate_log: PathBuf,
    suite_ran: PathBuf,
    suite_lock: PathBuf,
    head7: String,
}

fn review_fixture(base: &Path) -> ReviewFixture {
    let origin = base.join("origin.git");
    let repo = base.join("repo");
    let state = base.join("state");
    let fakedir = base.join("gh-fixtures");
    let fakebin = base.join("bin");
    std::fs::create_dir_all(&fakedir).unwrap();
    std::fs::create_dir_all(&fakebin).unwrap();
    std::fs::create_dir_all(&state).unwrap();

    review_git(base, &["init", "-q", "--bare", &origin.to_string_lossy()]);
    review_git(
        base,
        &[
            "clone",
            "-q",
            &origin.to_string_lossy(),
            &repo.to_string_lossy(),
        ],
    );
    review_git(&repo, &["config", "user.email", "t@t"]);
    review_git(&repo, &["config", "user.name", "t"]);
    review_git(&repo, &["checkout", "-qb", "main"]);

    let put = |rel: &str, text: &str| {
        let p = repo.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };

    put("marker.txt", "a");
    put("shared.txt", "a");
    put("shared2.txt", "a");
    put("other.txt", "a");
    put(
        "tests/test_old.rs",
        "#[test]\nfn old_test() {}\n#[test]\nfn new_flaky() {}\n",
    );
    put(
        "cadence-review.toml",
        r#"prepare = ["echo prepared >> \"$GATE_LOG\""]
gates = [
    "echo gate1 >> \"$GATE_LOG\"",
    "echo gate2 >> \"$GATE_LOG\"",
    "sh gate_fail.sh",
    "echo gate4-never >> \"$GATE_LOG\"",
]
full_suite = "sh suite.sh"
test_globs = ["tests/**"]
test_command = "sh one_test.sh {test}"
stress_pattern = ["wait_"]
"#,
    );
    put(
        "gate_fail.sh",
        "echo gate3-output-line1\n\
         echo \"test new_flaky ... FAILED\"\n\
         echo \"test ghost_test ... FAILED\"\n\
         echo \"test bad;touch_pwn ... FAILED\"\n\
         echo \"\"\n\
         echo \"failures:\"\n\
         echo \"\"\n\
         echo \"    new_flaky\"\n\
         echo \"    ghost_test\"\n\
         echo \"    bad;touch_pwn\"\n\
         echo \"\"\n\
         echo \"test result: FAILED. 0 passed; 3 failed\"\n\
         exit 1\n",
    );
    put(
        "suite.sh",
        "touch \"$SUITE_RAN\"\necho \"test result: ok. 5 passed\"\nexit 0\n",
    );
    put(
        "one_test.sh",
        "case \"$1\" in\n\
         \x20 new_flaky) [ \"$(cat shared.txt)\" = \"a\" ] && exit 0 || exit 1 ;;\n\
         \x20 *) exit 0 ;;\n\
         esac\n",
    );
    review_git(&repo, &["add", "-A"]);
    review_git(&repo, &["commit", "-qm", "base A"]);
    let sha_a = review_git_sha(&repo, &["rev-parse", "HEAD"]);

    // PR 7: touches shared.txt and adds a test that waits — merges
    // cleanly with the moved base.
    review_git(&repo, &["checkout", "-qb", "pr-7", "main"]);
    put("shared.txt", "pr7");
    put(
        "tests/test_new.rs",
        "#[test]\nfn new_daemon_wait() {\n    let _ = wait_agent;\n}\n",
    );
    review_git(&repo, &["add", "-A"]);
    review_git(&repo, &["commit", "-qm", "pr7"]);
    let head7 = review_git_sha(&repo, &["rev-parse", "HEAD"]);
    review_git(&repo, &["push", "-q", "origin", "HEAD:refs/pull/7/head"]);

    // PR 8: same file, other content — pairwise conflict with PR 7.
    review_git(&repo, &["checkout", "-qb", "pr-8", "main"]);
    put("shared.txt", "pr8");
    review_git(&repo, &["commit", "-qam", "pr8"]);
    let head8 = review_git_sha(&repo, &["rev-parse", "HEAD"]);
    review_git(&repo, &["push", "-q", "origin", "HEAD:refs/pull/8/head"]);

    // PR 9: adds a file — clean against everything.
    review_git(&repo, &["checkout", "-qb", "pr-9", "main"]);
    put("extra9.txt", "nine");
    review_git(&repo, &["add", "-A"]);
    review_git(&repo, &["commit", "-qm", "pr9"]);
    let head9 = review_git_sha(&repo, &["rev-parse", "HEAD"]);
    review_git(&repo, &["push", "-q", "origin", "HEAD:refs/pull/9/head"]);

    // PR 10: touches shared2.txt, which main is about to change too —
    // a merge-result conflict. Also adds a wait-test.
    review_git(&repo, &["checkout", "-qb", "pr-10", "main"]);
    put("shared2.txt", "pr10");
    put(
        "tests/test_wait10.rs",
        "#[test]\nfn wait_thing() {\n    let _ = wait_agent;\n}\n",
    );
    review_git(&repo, &["add", "-A"]);
    review_git(&repo, &["commit", "-qm", "pr10"]);
    let head10 = review_git_sha(&repo, &["rev-parse", "HEAD"]);
    review_git(&repo, &["push", "-q", "origin", "HEAD:refs/pull/10/head"]);

    // main moves past every merge-base (commit B on disjoint files for
    // PR 7's clean merge; shared2.txt for PR 10's conflict).
    review_git(&repo, &["checkout", "-q", "main"]);
    put("other.txt", "b");
    put("shared2.txt", "b");
    review_git(&repo, &["commit", "-qam", "base B"]);
    review_git(&repo, &["push", "-q", "origin", "main"]);
    let _ = sha_a;

    // Fake gh + fixtures.
    let gh = fakebin.join("gh");
    std::fs::write(
        &gh,
        "#!/bin/sh\n\
         case \"$1 $2\" in\n\
         \x20 \"pr view\") cat \"$FAKE_GH_DIR/pr-view-$3.json\" ;;\n\
         \x20 \"pr list\") cat \"$FAKE_GH_DIR/pr-list.json\" ;;\n\
         \x20 \"repo view\") echo \"o/r\" ;;\n\
         \x20 *) echo \"fake gh unhandled: $*\" >&2; exit 1 ;;\n\
         esac\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let view = |n: i64, sha: &str, branch: &str| {
        serde_json::json!({
            "number": n, "title": format!("PR {n}"),
            "url": format!("https://example/{n}"),
            "headRefName": branch, "headRefOid": sha,
            "baseRefName": "main",
            "files": [{"path": "shared.txt"}, {"path": "tests/test_new.rs"}],
            "state": "OPEN",
        })
    };
    std::fs::write(
        fakedir.join("pr-view-7.json"),
        serde_json::to_string(&view(7, &head7, "pr-7")).unwrap(),
    )
    .unwrap();
    std::fs::write(
        fakedir.join("pr-view-10.json"),
        serde_json::to_string(&view(10, &head10, "pr-10")).unwrap(),
    )
    .unwrap();
    std::fs::write(
        fakedir.join("pr-list.json"),
        serde_json::to_string(&serde_json::json!([
            {"number": 7, "title": "PR 7", "headRefOid": head7},
            {"number": 8, "title": "PR 8", "headRefOid": head8},
            {"number": 9, "title": "PR 9", "headRefOid": head9},
            {"number": 10, "title": "PR 10", "headRefOid": head10},
        ]))
        .unwrap(),
    )
    .unwrap();

    ReviewFixture {
        gate_log: base.join("gate.log"),
        suite_ran: base.join("suite.ran"),
        suite_lock: base.join("suite.lock"),
        repo,
        state,
        fakebin,
        fakedir,
        head7,
    }
}

fn review_cmd(f: &ReviewFixture) -> std::process::Command {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.arg("--state-dir")
        .arg(&f.state)
        .arg("review")
        .arg("--repo")
        .arg("o/r")
        .current_dir(&f.repo)
        .env(
            "PATH",
            format!("{}:{}", f.fakebin.display(), std::env::var("PATH").unwrap()),
        )
        .env("FAKE_GH_DIR", &f.fakedir)
        .env("GATE_LOG", &f.gate_log)
        .env("SUITE_RAN", &f.suite_ran)
        .env("CADENCE_SUITE_LOCK", &f.suite_lock);
    cmd
}

fn review_report(f: &ReviewFixture, pr: i64) -> Value {
    let dir = f.state.join("reviews");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&format!("review-o_r-pr{pr}-")) && n.ends_with(".json"))
        .collect();
    names.sort();
    let last = names
        .last()
        .unwrap_or_else(|| panic!("no review report for pr {pr} in {}", dir.display()));
    serde_json::from_str(&std::fs::read_to_string(dir.join(last)).unwrap()).unwrap()
}

#[test]
fn review_verb_end_to_end() {
    let base = TempDir::new().unwrap();
    let f = review_fixture(base.path());
    let out = review_cmd(&f).arg("7").output().unwrap();
    // blocked → exit 2
    assert_eq!(
        out.status.code(),
        Some(2),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Gates ran in order; the failing gate stopped the sequence. The
    // second "prepared" is the base-head checkout prepared for the
    // equal-conditions compare.
    let log = std::fs::read_to_string(&f.gate_log).unwrap();
    assert_eq!(log, "prepared\ngate1\ngate2\nprepared\n", "{log}");
    // The full suite ran once.
    assert!(f.suite_ran.exists());
    // Both checkouts are gone afterwards.
    assert!(!f.repo.join(".cadence/wt/review-7").exists());
    assert!(!f.repo.join(".cadence/wt/review-7-base").exists());
    let wts = review_git_sha(&f.repo, &["worktree", "list"]);
    assert!(!wts.contains("review-7"), "{wts}");

    let r = review_report(&f, 7);
    assert_eq!(r["pr"], 7);
    assert_eq!(r["head"], json!(f.head7));
    assert_eq!(r["base"]["moved_since_merge_base"], true);
    assert_eq!(r["gated_tree"], json!("merge-result"));
    assert_eq!(r["merge"]["result"], json!("clean"));

    let gates = r["gates"].as_array().unwrap();
    let outcomes: Vec<&str> = gates
        .iter()
        .map(|g| g["outcome"].as_str().unwrap())
        .collect();
    assert_eq!(outcomes, vec!["ok", "ok", "fail", "skipped"], "{gates:?}");
    // The failing gate's tail is kept.
    assert!(
        gates[2]["tail"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l.as_str().unwrap_or("").contains("new_flaky")),
        "{:?}",
        gates[2]["tail"]
    );

    // New test detected and stressed 5 times.
    let stress = r["stress"].as_array().unwrap();
    assert_eq!(stress.len(), 1);
    assert_eq!(stress[0]["test"], json!("new_daemon_wait"));
    assert_eq!(stress[0]["runs"], 5);
    assert_eq!(stress[0]["failures"], 0);
    assert_eq!(stress[0]["detail"].as_array().unwrap().len(), 5);

    assert_eq!(r["full_suite"]["outcome"], json!("ok"));
    assert!(r["suite_lock"]["path"].is_string(), "{:?}", r["suite_lock"]);

    // Equal-conditions compare, sorted by name:
    // - `bad;touch_pwn` fails name validation — never executed,
    //   unknown on both sides → inconclusive.
    // - `ghost_test` cannot be located — never executed → inconclusive.
    // - `new_flaky` fails the gated tree, passes on the base →
    //   regression.
    let fails = r["failures"].as_array().unwrap();
    assert_eq!(fails.len(), 3, "{fails:?}");
    assert_eq!(fails[0]["test"], json!("bad;touch_pwn"));
    assert_eq!(fails[0]["verdict"], json!("inconclusive"));
    assert_eq!(
        fails[0]["isolated_gated"]["outcome"],
        json!("unknown"),
        "{fails:?}"
    );
    assert!(
        fails[0].get("cmd").is_none(),
        "invalid name must never reach a command: {fails:?}"
    );
    assert_eq!(fails[1]["test"], json!("ghost_test"));
    assert_eq!(fails[1]["verdict"], json!("inconclusive"));
    assert_eq!(fails[1]["isolated_base"]["outcome"], json!("unknown"));
    assert!(
        fails[1]["isolated_base"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("not found"),
        "{fails:?}"
    );
    assert_eq!(fails[2]["test"], json!("new_flaky"));
    assert_eq!(fails[2]["isolated_gated"]["outcome"], json!("fail"));
    assert_eq!(fails[2]["isolated_base"]["outcome"], json!("pass"));
    assert_eq!(fails[2]["verdict"], json!("regression"));
    // Base prepare ran and was recorded.
    let bp = r["base_prepare"].as_array().unwrap();
    assert_eq!(bp.len(), 1, "{bp:?}");
    assert_eq!(bp[0]["outcome"], json!("ok"));

    // Pairwise open-PR conflicts: PR 8 conflicts on shared.txt; PR 9
    // merges clean. PR 10 does not merge into main, so its overlap is
    // not assessed — and that is a verdict reason (CAD-295).
    let conflicts = r["open_pr_conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    assert_eq!(conflicts[0]["pr"], 8);
    assert_eq!(conflicts[0]["files"], json!(["shared.txt"]));
    assert_eq!(
        r["open_pr_not_assessed"],
        json!([{"pr": 10, "title": "PR 10",
                "reason": "it does not merge into the current base"}])
    );
    let reasons: Vec<&str> = r["verdict_reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    assert!(
        reasons
            .contains(&"overlap not assessed with #10 — it does not merge into the current base"),
        "{reasons:?}"
    );

    assert_eq!(r["schema_migration"], false);
    // PR 7 leaves cadence-review.toml alone; the config came from the
    // base head.
    assert_eq!(
        r["config"],
        json!({"source": "base", "base_sha": r["base"]["sha"], "changed_by_pr": false})
    );
    assert_eq!(r["suggested_verdict"], json!("blocked"));
    assert!(r["report_md"].as_str().unwrap().ends_with(".md"));

    // The Markdown report exists and names the verdict.
    let md_path = PathBuf::from(r["report_md"].as_str().unwrap());
    let md = std::fs::read_to_string(&md_path).unwrap();
    assert!(md.contains("suggested verdict: blocked"), "{md}");
    assert!(md.contains("regression"), "{md}");
    assert!(md.contains("changed by this PR: **no**"), "{md}");
}

/// CAD-261: another process fetching in the same checkout between the
/// review's fetch and its resolve must not change what the review
/// resolves. A `git` wrapper on PATH follows every fetch with a fetch
/// of PR 8's head — exactly what a concurrent fetch does to the shared
/// FETCH_HEAD — so any FETCH_HEAD read would see PR 8's commit.
#[test]
fn review_verb_resolves_its_own_refs_despite_a_concurrent_fetch() {
    use std::os::unix::fs::PermissionsExt;
    let base = TempDir::new().unwrap();
    let f = review_fixture(base.path());
    let real_git = String::from_utf8(
        std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    let wrapper = f.fakebin.join("git");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n\"{real_git}\" \"$@\"\nrc=$?\n\
             if [ -z \"$CLOBBERING\" ] && [ \"$1\" = -C ]; then\n\
             case \" $* \" in *\" fetch \"*)\n\
             CLOBBERING=1 \"{real_git}\" -C \"$2\" fetch -q origin refs/pull/8/head >/dev/null 2>&1 ;;\n\
             esac\nfi\nexit $rc\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let main_sha = review_git_sha(&f.repo, &["ls-remote", "origin", "refs/heads/main"])
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();

    let out = review_cmd(&f).arg("7").output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("head moved while resolving"),
        "a concurrent fetch was mistaken for a moved head: {stderr}"
    );
    let r = review_report(&f, 7);
    assert_eq!(
        r["head"],
        json!(f.head7),
        "head resolved through FETCH_HEAD"
    );
    assert_eq!(
        r["base"]["sha"],
        json!(main_sha),
        "base resolved through FETCH_HEAD"
    );
    // The overlap scan still attributes each PR's own tree: only PR 8
    // conflicts, not PR 9 and 10 read as PR 8's commit.
    let conflicts = r["open_pr_conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    assert_eq!(conflicts[0]["pr"], 8);
    // The run's private refs are gone afterwards.
    let left = review_git_sha(
        &f.repo,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/cadence/review/",
        ],
    );
    assert!(left.is_empty(), "review refs left behind: {left}");
}

/// CAD-277: two PRs cut from different base commits must not "overlap"
/// on a file only the base changed between the cuts. PR 10 was cut from
/// base A and changes shared2.txt; main then rewrote shared2.txt (B). A
/// PR 11 cut from B that changes an unrelated file carries B's
/// shared2.txt, so a head-vs-head merge-tree conflicts there — but
/// neither PR 11 nor that line of history is PR 11's change.
#[test]
fn review_verb_ignores_base_drift_between_pr_cut_points() {
    let base = TempDir::new().unwrap();
    let f = review_fixture(base.path());
    review_git(&f.repo, &["checkout", "-qb", "pr-11", "main"]);
    std::fs::write(f.repo.join("eleven.txt"), "eleven").unwrap();
    review_git(&f.repo, &["add", "-A"]);
    review_git(&f.repo, &["commit", "-qm", "pr11"]);
    let head11 = review_git_sha(&f.repo, &["rev-parse", "HEAD"]);
    review_git(&f.repo, &["push", "-q", "origin", "HEAD:refs/pull/11/head"]);
    review_git(&f.repo, &["checkout", "-q", "main"]);
    std::fs::write(
        f.fakedir.join("pr-view-11.json"),
        json!({"number": 11, "title": "PR 11", "url": "https://example/11",
               "headRefName": "pr-11", "headRefOid": head11,
               "baseRefName": "main", "files": [{"path": "eleven.txt"}],
               "state": "OPEN"})
        .to_string(),
    )
    .unwrap();
    let list: Value =
        serde_json::from_str(&std::fs::read_to_string(f.fakedir.join("pr-list.json")).unwrap())
            .unwrap();
    let mut list = list.as_array().unwrap().clone();
    list.push(json!({"number": 11, "title": "PR 11", "headRefOid": head11}));
    std::fs::write(f.fakedir.join("pr-list.json"), json!(list).to_string()).unwrap();
    let main_sha = review_git_sha(&f.repo, &["rev-parse", "main"]);

    let _ = review_cmd(&f).arg("11").output().unwrap();
    let r = review_report(&f, 11);
    let conflicts = r["open_pr_conflicts"].as_array().unwrap();
    assert!(
        !conflicts.iter().any(|c| c["pr"] == 10),
        "base drift reported as an overlap with PR 10: {conflicts:?}"
    );
    assert!(conflicts.is_empty(), "{conflicts:?}");
    // PR 10 itself does not merge into the moved base, so there is no
    // as-landed tree to compare: listed, never dropped or counted.
    let skipped = r["open_pr_not_assessed"].as_array().unwrap();
    assert!(
        skipped.iter().any(|c| c["pr"] == 10
            && c["reason"]
                .as_str()
                .unwrap_or("")
                .contains("does not merge")),
        "{skipped:?}"
    );
    // ...and named in the verdict, so the unknown overlap is not
    // missed (CAD-295).
    assert!(
        r["verdict_reasons"].as_array().unwrap().iter().any(|s| s
            == "overlap not assessed with #10 — it does not merge into the current base"),
        "{:?}",
        r["verdict_reasons"]
    );
    // CAD-297: PR 10 (shared2.txt, a wait-test) shares no file with
    // PR 11 (eleven.txt), so its advisory hint is empty and omitted.
    let ten = skipped.iter().find(|c| c["pr"] == 10).unwrap();
    assert!(ten.get("advisory_overlap").is_none(), "{ten:?}");
    assert!(ten.get("advisory_overlap_error").is_none(), "{ten:?}");
    assert!(
        !r["verdict_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m.as_str().unwrap_or("").contains("advisory")),
        "{:?}",
        r["verdict_reasons"]
    );
    assert_eq!(r["open_pr_conflicts_base"], json!(main_sha));
    let md = std::fs::read_to_string(r["report_md"].as_str().unwrap()).unwrap();
    assert!(md.contains("would land on base"), "{md}");
    assert!(md.contains("Not assessed"), "{md}");
    // No conflict among the assessed PRs is not "no conflicting open
    // PRs" while one was never assessed.
    assert!(!md.contains("no conflicting open PRs"), "{md}");
    assert!(
        md.contains("none among assessed PRs — 1 not assessed, listed below"),
        "{md}"
    );
    assert!(!md.contains("advisory"), "{md}");
}

/// CAD-297: a PR that does not merge into the base is not assessed, but
/// when it changes a file the reviewed PR also changes, the report says
/// so as an advisory hint — across a rename on either side. PR 15 moves
/// big.txt to moved.txt and edits it; PR 14 edits big.txt in place and
/// PR 16 moves it to elsewhere.txt, and both of those also edit a file
/// main changed after they were cut, so neither merges into the base.
#[test]
fn review_verb_hints_a_not_assessed_overlap_across_a_rename() {
    let base = TempDir::new().unwrap();
    let f = review_fixture(base.path());
    let lines: Vec<String> = (1..=10).map(|n| format!("line {n}")).collect();
    let body = |at: usize, text: &str| {
        let mut v = lines.clone();
        v[at] = text.to_string();
        v.join("\n") + "\n"
    };
    review_git(&f.repo, &["checkout", "-q", "main"]);
    std::fs::write(f.repo.join("big.txt"), body(0, "line 1")).unwrap();
    std::fs::write(f.repo.join("drift.txt"), "a").unwrap();
    review_git(&f.repo, &["add", "-A"]);
    review_git(&f.repo, &["commit", "-qm", "add big.txt"]);
    review_git(&f.repo, &["push", "-q", "origin", "main"]);
    let push_pr = |n: i64, edit: &dyn Fn()| -> String {
        let branch = format!("pr-{n}");
        review_git(&f.repo, &["checkout", "-qb", &branch, "main"]);
        edit();
        review_git(&f.repo, &["add", "-A"]);
        review_git(&f.repo, &["commit", "-qm", &branch]);
        let head = review_git_sha(&f.repo, &["rev-parse", "HEAD"]);
        review_git(
            &f.repo,
            &["push", "-q", "origin", &format!("HEAD:refs/pull/{n}/head")],
        );
        review_git(&f.repo, &["checkout", "-q", "main"]);
        head
    };
    let head14 = push_pr(14, &|| {
        std::fs::write(f.repo.join("big.txt"), body(8, "line 9 by pr14")).unwrap();
        std::fs::write(f.repo.join("drift.txt"), "pr14").unwrap();
    });
    let head16 = push_pr(16, &|| {
        review_git(&f.repo, &["mv", "big.txt", "elsewhere.txt"]);
        std::fs::write(f.repo.join("elsewhere.txt"), body(9, "line 10 by pr16")).unwrap();
        std::fs::write(f.repo.join("drift.txt"), "pr16").unwrap();
    });
    // main moves on drift.txt: PR 14 and PR 16 no longer merge into it.
    std::fs::write(f.repo.join("drift.txt"), "main").unwrap();
    review_git(&f.repo, &["commit", "-qam", "drift"]);
    review_git(&f.repo, &["push", "-q", "origin", "main"]);
    let head15 = push_pr(15, &|| {
        review_git(&f.repo, &["mv", "big.txt", "moved.txt"]);
        std::fs::write(f.repo.join("moved.txt"), body(4, "line 5 by pr15")).unwrap();
    });
    std::fs::write(
        f.fakedir.join("pr-view-15.json"),
        json!({"number": 15, "title": "PR 15", "url": "https://example/15",
               "headRefName": "pr-15", "headRefOid": head15,
               "baseRefName": "main", "files": [{"path": "moved.txt"}],
               "state": "OPEN"})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        f.fakedir.join("pr-list.json"),
        json!([
            {"number": 14, "title": "PR 14", "headRefOid": head14},
            {"number": 15, "title": "PR 15", "headRefOid": head15},
            {"number": 16, "title": "PR 16", "headRefOid": head16},
        ])
        .to_string(),
    )
    .unwrap();

    let _ = review_cmd(&f).arg("15").output().unwrap();
    let r = review_report(&f, 15);
    assert!(
        r["open_pr_conflicts"].as_array().unwrap().is_empty(),
        "{:?}",
        r["open_pr_conflicts"]
    );
    let skipped = r["open_pr_not_assessed"].as_array().unwrap();
    let hint = |n: i64| {
        let c = skipped
            .iter()
            .find(|c| c["pr"] == n)
            .unwrap_or_else(|| panic!("PR {n} not listed as not assessed: {skipped:?}"));
        assert!(
            c["reason"]
                .as_str()
                .unwrap_or("")
                .contains("does not merge"),
            "{c:?}"
        );
        c["advisory_overlap"].clone()
    };
    // The reviewed side renamed; PR 14 edits the old name in place.
    assert_eq!(hint(14), json!(["big.txt"]));
    // Both sides renamed the same file to different names.
    assert_eq!(hint(16), json!(["big.txt"]));

    let reasons: Vec<&str> = r["verdict_reasons"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    // One reason for both not-assessed PRs (CAD-295), each carrying its
    // advisory overlap; no second, separate line per PR (CAD-297).
    assert!(
        reasons.contains(
            &"overlap not assessed with \
              #14 (advisory, not a conflict: both PRs change big.txt), \
              #16 (advisory, not a conflict: both PRs change big.txt) \
              — they do not merge into the current base"
        ),
        "{reasons:?}"
    );
    assert_eq!(
        reasons.iter().filter(|m| m.contains("#14")).count(),
        1,
        "{reasons:?}"
    );
    let md = std::fs::read_to_string(r["report_md"].as_str().unwrap()).unwrap();
    assert!(
        md.contains("none among assessed PRs — 2 not assessed, listed below"),
        "{md}"
    );
    assert!(!md.contains("no conflicting open PRs"), "{md}");
    assert_eq!(
        md.matches("  - advisory, not a conflict: both PRs change big.txt\n")
            .count(),
        2,
        "{md}"
    );
}

/// CAD-277 QA finding: a conflict that involves a rename must still be
/// reported. PR 12 moves big.txt to moved.txt and edits line 5; PR 13
/// edits line 5 of big.txt. Comparing both as landed on the base lets
/// git's rename detection pair the two edits.
#[test]
fn review_verb_reports_a_rename_overlap() {
    let base = TempDir::new().unwrap();
    let f = review_fixture(base.path());
    let lines: Vec<String> = (1..=10).map(|n| format!("line {n}")).collect();
    let body = |five: &str| {
        let mut v = lines.clone();
        v[4] = five.to_string();
        v.join("\n") + "\n"
    };
    // The file lives on the current base, so both PRs are cut after it.
    review_git(&f.repo, &["checkout", "-q", "main"]);
    std::fs::write(f.repo.join("big.txt"), body("line 5")).unwrap();
    review_git(&f.repo, &["add", "-A"]);
    review_git(&f.repo, &["commit", "-qm", "add big.txt"]);
    review_git(&f.repo, &["push", "-q", "origin", "main"]);
    let push_pr = |n: i64, branch: &str, edit: &dyn Fn()| -> String {
        review_git(&f.repo, &["checkout", "-qb", branch, "main"]);
        edit();
        review_git(&f.repo, &["add", "-A"]);
        review_git(&f.repo, &["commit", "-qm", branch]);
        let head = review_git_sha(&f.repo, &["rev-parse", "HEAD"]);
        review_git(
            &f.repo,
            &["push", "-q", "origin", &format!("HEAD:refs/pull/{n}/head")],
        );
        review_git(&f.repo, &["checkout", "-q", "main"]);
        head
    };
    let head12 = push_pr(12, "pr-12", &|| {
        review_git(&f.repo, &["mv", "big.txt", "moved.txt"]);
        std::fs::write(f.repo.join("moved.txt"), body("line 5 by pr12")).unwrap();
    });
    let head13 = push_pr(13, "pr-13", &|| {
        std::fs::write(f.repo.join("big.txt"), body("line 5 by pr13")).unwrap();
    });
    std::fs::write(
        f.fakedir.join("pr-view-13.json"),
        json!({"number": 13, "title": "PR 13", "url": "https://example/13",
               "headRefName": "pr-13", "headRefOid": head13,
               "baseRefName": "main", "files": [{"path": "big.txt"}],
               "state": "OPEN"})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        f.fakedir.join("pr-list.json"),
        json!([
            {"number": 12, "title": "PR 12", "headRefOid": head12},
            {"number": 13, "title": "PR 13", "headRefOid": head13},
        ])
        .to_string(),
    )
    .unwrap();

    let _ = review_cmd(&f).arg("13").output().unwrap();
    let r = review_report(&f, 13);
    let conflicts = r["open_pr_conflicts"].as_array().unwrap();
    assert_eq!(
        conflicts.len(),
        1,
        "rename overlap was dropped: {conflicts:?}"
    );
    assert_eq!(conflicts[0]["pr"], 12);
    assert!(
        !conflicts[0]["files"].as_array().unwrap().is_empty(),
        "{conflicts:?}"
    );
}

#[test]
fn review_verb_merge_conflict_blocks() {
    let base = TempDir::new().unwrap();
    let f = review_fixture(base.path());
    let out = review_cmd(&f).arg("10").output().unwrap();
    // blocked → exit 2
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = review_report(&f, 10);
    assert_eq!(r["base"]["moved_since_merge_base"], true);
    assert_eq!(r["merge"]["result"], json!("conflict"));
    assert_eq!(r["merge"]["conflict_files"], json!(["shared2.txt"]));
    // The gates still ran, on the bare PR head.
    assert_eq!(r["gated_tree"], json!("pr-head"));
    let outcomes: Vec<&str> = r["gates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["outcome"].as_str().unwrap())
        .collect();
    assert_eq!(outcomes, vec!["ok", "ok", "fail", "skipped"]);
    // Its wait-test was stressed too.
    assert_eq!(r["stress"][0]["test"], json!("wait_thing"));
    assert_eq!(r["suggested_verdict"], json!("blocked"));
    assert!(
        r["verdict_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s.as_str().unwrap_or("").contains("does not merge")),
        "{:?}",
        r["verdict_reasons"]
    );
    // Every other PR is not assessed because PR 10 itself does not
    // merge; that is already the blocking reason above, so no
    // "overlap not assessed" reason is added (CAD-295).
    let skipped = r["open_pr_not_assessed"].as_array().unwrap();
    assert_eq!(skipped.len(), 3, "{skipped:?}");
    assert!(
        skipped
            .iter()
            .all(|c| c["reason"] == "this PR does not merge into the current base"),
        "{skipped:?}"
    );
    assert!(
        !r["verdict_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s.as_str().unwrap_or("").contains("overlap not assessed")),
        "{:?}",
        r["verdict_reasons"]
    );
}

#[test]
fn review_verb_lock_refuses_a_second_run() {
    use std::os::unix::io::AsRawFd;
    let base = TempDir::new().unwrap();
    let f = review_fixture(base.path());
    let reviews = f.state.join("reviews");
    std::fs::create_dir_all(&reviews).unwrap();
    let held = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(reviews.join("o_r.review.lock"))
        .unwrap();
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
    let out = review_cmd(&f).arg("7").output().unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("already running"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    drop(held);
}

#[test]
fn review_verb_suite_lock_serializes() {
    use std::os::unix::io::AsRawFd;
    let base = TempDir::new().unwrap();
    let f = review_fixture(base.path());
    // Hold the suite slot: the run must wait on it before `full_suite`.
    let held = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&f.suite_lock)
        .unwrap();
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);

    let mut child = review_cmd(&f).arg("7").spawn().unwrap();
    // Everything before the suite takes well under 4s here; a still-
    // running child with no suite marker is waiting on the lock.
    // CAD-184 kept sleep: absence window — the review prints nothing while
    // it waits on the flock.
    std::thread::sleep(Duration::from_secs(4));
    assert!(
        child.try_wait().unwrap().is_none(),
        "review finished while the suite lock was held"
    );
    assert!(!f.suite_ran.exists(), "suite ran while its lock was held");
    drop(held);
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(f.suite_ran.exists());
}

#[test]
fn review_verb_refuses_to_adopt_an_existing_dir() {
    let base = TempDir::new().unwrap();
    let f = review_fixture(base.path());
    let dir = f.repo.join(".cadence/wt/review-7");

    // A plain directory with an uncommitted file at the path — the
    // run refuses and leaves it byte-for-byte untouched.
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("precious.txt"), "keep me").unwrap();
    let out = review_cmd(&f).arg("7").output().unwrap();
    assert!(
        !out.status.success(),
        "expected refusal, got {:?}",
        out.status
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("already exists"), "{err}");
    assert!(err.contains("review-7"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.join("precious.txt")).unwrap(),
        "keep me"
    );

    // A foreign worktree at the same path — same refusal, still
    // registered and untouched afterwards.
    std::fs::remove_dir_all(&dir).unwrap();
    review_git(
        &f.repo,
        &[
            "worktree",
            "add",
            "--detach",
            &dir.to_string_lossy(),
            &f.head7,
        ],
    );
    std::fs::write(dir.join("precious.txt"), "keep me").unwrap();
    let out = review_cmd(&f).arg("7").output().unwrap();
    assert!(
        !out.status.success(),
        "expected refusal, got {:?}",
        out.status
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("already exists"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.join("precious.txt")).unwrap(),
        "keep me"
    );
    assert!(dir.join("marker.txt").exists(), "foreign checkout intact");
    let wts = review_git_sha(&f.repo, &["worktree", "list"]);
    assert!(wts.contains("review-7"), "{wts}");
    review_git(
        &f.repo,
        &["worktree", "remove", "--force", &dir.to_string_lossy()],
    );
}

#[test]
fn review_verb_base_prepare_failure_marks_inconclusive() {
    let base = TempDir::new().unwrap();
    let f = review_fixture(base.path());
    // Prepare succeeds on the gated tree, fails on the base tree. The
    // config is read from the base head, so it lands on origin/main.
    std::fs::write(
        f.repo.join("cadence-review.toml"),
        r#"prepare = ["if [ \"$CADENCE_REVIEW_TREE\" = \"base\" ]; then echo base-prep-broke; exit 1; else echo prepared >> \"$GATE_LOG\"; fi"]
gates = ["echo gate1 >> \"$GATE_LOG\"", "sh gate_fail.sh"]
full_suite = "sh suite.sh"
test_globs = ["tests/**"]
test_command = "sh one_test.sh {test}"
stress_pattern = ["wait_"]
"#,
    )
    .unwrap();
    review_git(
        &f.repo,
        &["commit", "-qam", "base config: base prepare breaks"],
    );
    review_git(&f.repo, &["push", "-q", "origin", "main"]);
    let out = review_cmd(&f).arg("7").output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = review_report(&f, 7);
    // The base changed the config after PR 7 branched — that is not a
    // change by the PR.
    assert_eq!(
        r["config"]["changed_by_pr"],
        json!(false),
        "{:?}",
        r["config"]
    );
    // The failed prepare is recorded as its own step.
    let bp = r["base_prepare"].as_array().unwrap();
    assert_eq!(bp.len(), 1, "{bp:?}");
    assert_eq!(bp[0]["outcome"], json!("fail"));
    // Every comparison's base side is unknown — nothing laundered into
    // a fake "pre-existing".
    let fails = r["failures"].as_array().unwrap();
    assert_eq!(fails.len(), 3, "{fails:?}");
    for c in fails {
        assert_eq!(c["isolated_base"]["outcome"], json!("unknown"), "{c:?}");
        assert_eq!(c["verdict"], json!("inconclusive"), "{c:?}");
    }
    // new_flaky still ran on the gated tree and failed there.
    let nf = fails.iter().find(|c| c["test"] == "new_flaky").unwrap();
    assert_eq!(nf["isolated_gated"]["outcome"], json!("fail"));
    assert_eq!(r["suggested_verdict"], json!("blocked"));
}

#[test]
fn review_verb_gates_with_the_base_config_when_the_pr_rewrites_it() {
    let base = TempDir::new().unwrap();
    let f = review_fixture(base.path());
    // PR 11 rewrites its own gates to a no-op. The reviewer's checkout
    // stays on the PR branch, so the working tree holds the weakened
    // copy too — neither may be read.
    review_git(&f.repo, &["checkout", "-qb", "pr-11", "main"]);
    std::fs::write(
        f.repo.join("cadence-review.toml"),
        r#"prepare = []
gates = ["true"]
full_suite = "true"
test_globs = ["tests/**"]
test_command = "true {test}"
"#,
    )
    .unwrap();
    review_git(&f.repo, &["commit", "-qam", "pr11 weakens the gates"]);
    let head11 = review_git_sha(&f.repo, &["rev-parse", "HEAD"]);
    review_git(&f.repo, &["push", "-q", "origin", "HEAD:refs/pull/11/head"]);
    std::fs::write(
        f.fakedir.join("pr-view-11.json"),
        serde_json::to_string(&json!({
            "number": 11, "title": "PR 11", "url": "https://example/11",
            "headRefName": "pr-11", "headRefOid": head11,
            "baseRefName": "main",
            "files": [{"path": "cadence-review.toml"}],
            "state": "OPEN",
        }))
        .unwrap(),
    )
    .unwrap();

    let out = review_cmd(&f).args(["11", "--no-full"]).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // The base head's gates ran and its failing gate still failed.
    let log = std::fs::read_to_string(&f.gate_log).unwrap();
    assert!(log.starts_with("prepared\ngate1\ngate2\n"), "{log}");
    let r = review_report(&f, 11);
    let gates = r["gates"].as_array().unwrap();
    let cmds: Vec<&str> = gates.iter().map(|g| g["cmd"].as_str().unwrap()).collect();
    assert_eq!(cmds.len(), 4, "{gates:?}");
    assert_eq!(cmds[2], "sh gate_fail.sh");
    assert_eq!(gates[2]["outcome"], json!("fail"), "{gates:?}");
    assert_eq!(
        r["config"],
        json!({"source": "base", "base_sha": r["base"]["sha"], "changed_by_pr": true})
    );
    assert_ne!(r["suggested_verdict"], json!("pass"));
    assert!(
        r["verdict_reasons"].as_array().unwrap().iter().any(|s| s
            .as_str()
            .unwrap_or("")
            .contains("changes cadence-review.toml")),
        "{:?}",
        r["verdict_reasons"]
    );
    let md = std::fs::read_to_string(r["report_md"].as_str().unwrap()).unwrap();
    assert!(md.contains("changed by this PR: **yes**"), "{md}");
}

/// CAD-301: GitHub CI has no git identity, so a test that commits
/// without `-c user.name/-c user.email` fails there with "Author
/// identity unknown". The review must fail it too, however the caller's
/// host supplies an identity: a global `~/.gitconfig`, `GIT_CONFIG_*`
/// entries, `GIT_CONFIG_PARAMETERS` (what `git -c user.name=… <alias>`
/// hands its children, CAD-307), `EMAIL` / `GIT_AUTHOR_*` /
/// `GIT_COMMITTER_*`, or git's own user@hostname guess.
#[test]
fn review_verb_gates_run_without_a_git_identity() {
    let base = TempDir::new().unwrap();
    let f = review_fixture(base.path());
    let caller_home = base.path().join("caller-home");
    std::fs::create_dir_all(&caller_home).unwrap();
    std::fs::write(
        caller_home.join(".gitconfig"),
        "[user]\n\tname = host\n\temail = host@example.com\n",
    )
    .unwrap();
    // The base config gates and suites with a probe that records the
    // env it sees, then commits in a scratch repo with no identity.
    std::fs::write(
        f.repo.join("cadence-review.toml"),
        r#"prepare = []
gates = ["sh commit_probe.sh gate"]
full_suite = "sh commit_probe.sh suite"
test_globs = ["tests/**"]
test_command = "sh one_test.sh {test}"
stress_pattern = ["wait_"]
"#,
    )
    .unwrap();
    std::fs::write(
        f.repo.join("commit_probe.sh"),
        "set -e\n\
         probe=$(git config --get cadence.probe || true)\n\
         echo \"$1|$HOME|$CARGO_HOME|$RUSTUP_HOME|$XDG_DATA_HOME|$probe\" >> \"$GATE_LOG\"\n\
         d=$(mktemp -d)\n\
         trap 'rm -rf \"$d\"' EXIT\n\
         git -C \"$d\" init -q\n\
         git -C \"$d\" commit -q --allow-empty -m \"$1\"\n\
         echo \"committed-$1\" >> \"$GATE_LOG\"\n",
    )
    .unwrap();
    review_git(&f.repo, &["add", "-A"]);
    review_git(
        &f.repo,
        &["commit", "-qm", "base config: commit without an identity"],
    );
    review_git(&f.repo, &["push", "-q", "origin", "main"]);

    let out = review_cmd(&f)
        .arg("7")
        .env("HOME", &caller_home)
        .env("CARGO_HOME", "/caller/cargo")
        .env_remove("RUSTUP_HOME")
        .env_remove("XDG_DATA_HOME")
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "user.email")
        .env("GIT_CONFIG_VALUE_0", "cfg@example.com")
        .env("GIT_CONFIG_KEY_1", "cadence.probe")
        .env("GIT_CONFIG_VALUE_1", "kept")
        .env(
            "GIT_CONFIG_PARAMETERS",
            "'user.name'='param' 'user.email'='param@example.com'",
        )
        .env("EMAIL", "env@example.com")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "c")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let r = review_report(&f, 7);
    let gates = r["gates"].as_array().unwrap();
    assert_eq!(gates[0]["outcome"], json!("fail"), "{gates:?}");
    let identity_error = |tail: &Value| {
        tail.as_array()
            .unwrap()
            .iter()
            .any(|l| l.as_str().unwrap_or("").contains("Author identity unknown"))
    };
    assert!(identity_error(&gates[0]["tail"]), "{:?}", gates[0]["tail"]);
    assert_eq!(
        r["full_suite"]["outcome"],
        json!("fail"),
        "{:?}",
        r["full_suite"]
    );
    assert_eq!(r["suggested_verdict"], json!("blocked"));

    // Neither commit landed; both commands saw the same scratch HOME,
    // the caller's real tool homes, and the caller's non-identity
    // GIT_CONFIG entry.
    let log = std::fs::read_to_string(&f.gate_log).unwrap();
    assert!(!log.contains("committed-"), "{log}");
    let rows: Vec<Vec<&str>> = log.lines().map(|l| l.split('|').collect()).collect();
    assert_eq!(rows.len(), 2, "{log}");
    assert_eq!(rows[0][0], "gate");
    assert_eq!(rows[1][0], "suite");
    let gate_home = rows[0][1];
    assert_eq!(rows[1][1], gate_home, "{log}");
    assert_ne!(Path::new(gate_home), caller_home.as_path(), "{log}");
    assert!(!gate_home.is_empty(), "{log}");
    let caller = caller_home.display();
    for row in &rows {
        assert_eq!(row[2], "/caller/cargo", "{log}");
        assert_eq!(row[3], format!("{caller}/.rustup"), "{log}");
        assert_eq!(row[4], format!("{caller}/.local/share"), "{log}");
        assert_eq!(row[5], "kept", "{log}");
    }
    // The scratch HOME is removed with the review.
    assert!(!Path::new(gate_home).exists(), "{gate_home} left behind");
}

/// CAD-264: `vite build` strips TypeScript types without checking them,
/// so the repo's own `cadence-review.toml` must type-check ui/src as a
/// gate. The gate command is taken from that file and run by
/// `cadence review` against the real `tsc` in ui/node_modules: a ui/src
/// type error fails the gate with tsc's message, a clean tree passes.
#[test]
fn review_verb_ui_typecheck_gate_catches_a_ui_type_error() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let real: toml::Value =
        toml::from_str(&std::fs::read_to_string(root.join("cadence-review.toml")).unwrap())
            .unwrap();
    let typecheck = "cd ui && node_modules/.bin/tsc --noEmit";
    let gates: Vec<&str> = real["gates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g.as_str().unwrap())
        .collect();
    assert!(gates.contains(&typecheck), "{gates:?}");
    let node_modules = root.join("ui/node_modules");
    assert!(
        node_modules.join(".bin/tsc").exists(),
        "{} has no tsc — run `pnpm install` in ui/ (or link the main \
         checkout's ui/node_modules, as the review's prepare does)",
        node_modules.display()
    );

    let base = TempDir::new().unwrap();
    let f = review_fixture(base.path());
    // Base config: link the real node_modules, then gate with the
    // repo's own typecheck command and nothing else.
    std::fs::write(
        f.repo.join("cadence-review.toml"),
        format!(
            r#"prepare = ["cd ui && ln -sfn '{}' node_modules"]
gates = ["{typecheck}"]
full_suite = "sh suite.sh"
test_globs = ["tests/**"]
test_command = "sh one_test.sh {{test}}"
stress_pattern = ["wait_"]
"#,
            node_modules.display()
        ),
    )
    .unwrap();
    std::fs::create_dir_all(f.repo.join("ui/src")).unwrap();
    std::fs::copy(
        root.join("ui/tsconfig.json"),
        f.repo.join("ui/tsconfig.json"),
    )
    .unwrap();
    std::fs::write(f.repo.join(".gitignore"), "/ui/node_modules\n").unwrap();
    std::fs::write(
        f.repo.join("ui/src/count.ts"),
        "export const count: number = 1;\n",
    )
    .unwrap();
    review_git(&f.repo, &["add", "-A"]);
    review_git(&f.repo, &["commit", "-qm", "base: a typed ui"]);
    review_git(&f.repo, &["push", "-q", "origin", "main"]);

    let open_pr = |n: i64, file: &str, text: &str| {
        review_git(&f.repo, &["checkout", "-qb", &format!("pr-{n}"), "main"]);
        std::fs::write(f.repo.join(file), text).unwrap();
        review_git(&f.repo, &["add", "-A"]);
        review_git(&f.repo, &["commit", "-qm", &format!("pr{n}")]);
        let head = review_git_sha(&f.repo, &["rev-parse", "HEAD"]);
        review_git(
            &f.repo,
            &["push", "-q", "origin", &format!("HEAD:refs/pull/{n}/head")],
        );
        std::fs::write(
            f.fakedir.join(format!("pr-view-{n}.json")),
            serde_json::to_string(&json!({
                "number": n, "title": format!("PR {n}"),
                "url": format!("https://example/{n}"),
                "headRefName": format!("pr-{n}"), "headRefOid": head,
                "baseRefName": "main",
                "files": [{"path": file}],
                "state": "OPEN",
            }))
            .unwrap(),
        )
        .unwrap();
        review_git(&f.repo, &["checkout", "-q", "main"]);
    };
    // PR 12 assigns a string to a number — vite would build it.
    open_pr(
        12,
        "ui/src/label.ts",
        "export const label: number = \"not a number\";\n",
    );
    // PR 13 is the same file, well typed.
    open_pr(
        13,
        "ui/src/label.ts",
        "export const label: string = \"ok\";\n",
    );

    let typecheck_gate = |pr: i64| {
        let out = review_cmd(&f)
            .args([&pr.to_string(), "--no-full"])
            .output()
            .unwrap();
        let r = review_report(&f, pr);
        assert!(
            r["prepare"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["outcome"] == json!("ok")),
            "{:?}\nstderr: {}",
            r["prepare"],
            String::from_utf8_lossy(&out.stderr)
        );
        let gates = r["gates"].as_array().unwrap().clone();
        assert_eq!(gates.len(), 1, "{gates:?}");
        assert_eq!(gates[0]["cmd"], json!(typecheck));
        (out, r, gates[0].clone())
    };

    let (out, r, gate) = typecheck_gate(12);
    assert_eq!(out.status.code(), Some(2), "{r}");
    assert_eq!(gate["outcome"], json!("fail"), "{gate:?}");
    let tail: Vec<&str> = gate["tail"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap_or(""))
        .collect();
    assert!(
        tail.iter()
            .any(|l| l.contains("src/label.ts") && l.contains("error TS2322")),
        "{tail:?}"
    );
    assert_eq!(r["suggested_verdict"], json!("blocked"));
    assert!(
        r["verdict_reasons"]
            .as_array()
            .unwrap()
            .contains(&json!(format!("gate `{typecheck}` fail"))),
        "{:?}",
        r["verdict_reasons"]
    );

    let (_, r, gate) = typecheck_gate(13);
    assert_eq!(gate["outcome"], json!("ok"), "{gate:?}");
    assert!(
        !r["verdict_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s.as_str().unwrap_or("").contains("gate `")),
        "{:?}",
        r["verdict_reasons"]
    );
}

// ---------- CAD-153: cadence audit -------------------------------------

/// A repo whose default branch holds squash-merge subjects `… (#N)`
/// plus the landed-head commits, so `contains_head` can patch-id
/// compare. Returns (repo, notes, report, heads) — `heads[i]` is the
/// headRefOid for PR i+1.
fn audit_repo(dir: &TempDir) -> (PathBuf, PathBuf, PathBuf, Vec<String>) {
    let repo = dir.path().join("repo");
    let notes = dir.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    git_repo(&repo);
    let g = |args: &[&str]| -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {:?}", out.stderr);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    // No global identity on CI runners.
    g(&["config", "user.email", "t@t"]);
    g(&["config", "user.name", "t"]);
    let branch = g(&["rev-parse", "--abbrev-ref", "HEAD"]);
    let mut heads = Vec::new();
    let mut prs = Vec::new();
    for n in 1..=3u8 {
        // The PR head: same change on a side branch.
        g(&["checkout", "-qb", &format!("pr{n}")]);
        std::fs::write(repo.join(format!("f{n}.txt")), format!("change {n}")).unwrap();
        g(&["add", "."]);
        g(&["commit", "-qm", &format!("work {n}")]);
        let head = g(&["rev-parse", "HEAD"]);
        // The squash merge: identical change on the default branch.
        g(&["checkout", "-q", &branch]);
        std::fs::write(repo.join(format!("f{n}.txt")), format!("change {n}")).unwrap();
        g(&["add", "."]);
        g(&["commit", "-qm", &format!("work {n} (CAD-{n}) (#{n})")]);
        let merge = g(&["rev-parse", "HEAD"]);
        prs.push(format!(
            r#"{{"number":{n},"title":"work {n} (CAD-{n})","headRefOid":"{head}",
              "mergeCommit":{{"oid":"{merge}"}},"mergedBy":{{"login":"ops-1"}},
              "mergedAt":"2026-09-20T12:00:0{n}Z"}}"#
        ));
        heads.push(head);
    }
    let report = dir.path().join("merge-report.json");
    std::fs::write(
        &report,
        format!("{{\"prs\":[{}],\"statuses\":{{}}}}", prs.join(",")),
    )
    .unwrap();
    (repo, notes, report, heads)
}

/// `cadence audit` fully fixtured — `--merge-report` + `--notes-dir`
/// replace gh and the notes tree, an empty state dir and PM dir keep
/// the daemon store and tracker out.
fn run_audit(
    state: &Path,
    pm: &Path,
    repo: &Path,
    notes: &Path,
    report: &Path,
    extra: &[&str],
) -> std::process::Output {
    run_audit_full(state, pm, repo, Some(notes), Some(report), extra, None)
}

/// The plumbing behind `run_audit`: optional fixture paths (a `None`
/// `--merge-report` exercises the live `gh` path) plus an optional
/// PATH override so tests can remove `gh` entirely.
fn run_audit_full(
    state: &Path,
    pm: &Path,
    repo: &Path,
    notes: Option<&Path>,
    report: Option<&Path>,
    extra: &[&str],
    path_env: Option<&Path>,
) -> std::process::Output {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.arg("--state-dir")
        .arg(state)
        .arg("audit")
        .arg("--repo")
        .arg(repo);
    if let Some(n) = notes {
        cmd.arg("--notes-dir").arg(n);
    }
    if let Some(r) = report {
        cmd.arg("--merge-report").arg(r);
    }
    cmd.args(extra).env("CADENCE_PM_DIR", pm);
    if let Some(p) = path_env {
        cmd.env("PATH", p);
    }
    cmd.output().unwrap()
}

/// A PATH that resolves `git` (the audit shells it constantly) but
/// has no `gh` — proving neither fixture mode nor the live path can
/// accidentally reach the real CLI.
fn path_without_gh(dir: &TempDir) -> PathBuf {
    let bin = dir.path().join("no-gh-bin");
    std::fs::create_dir_all(&bin).unwrap();
    for p in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let git = p.join("git");
        if git.is_file() {
            std::os::unix::fs::symlink(&git, bin.join("git")).unwrap();
            return bin;
        }
    }
    panic!("no git on PATH to link into {bin:?}");
}

fn verdict_note(notes: &Path, name: &str, head: &str, from: &str, class: &str) {
    std::fs::write(
        notes.join(name),
        format!(
            "# Verdict: pass\n> From: `{from}`\n\n## Verdict\npass — head `{head}`\n\n\
             **Risk: {class} (test trigger)**\n\n**What an auditor should check:** the row.\n"
        ),
    )
    .unwrap();
}

#[test]
fn audit_reconstructs_clean_merge() {
    let dir = TempDir::new().unwrap();
    let (repo, notes, report, heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&pm).unwrap();
    // Every PR head carries a pass verdict note + SUCCESS status —
    // the fully clean run. Note filenames (11:59:xx) and status
    // `created_at` predate `mergedAt` 12:00:0n — a post-merge verdict
    // is post-hoc evidence, not a merge-time gate.
    let mut report_json: Value =
        serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    for (i, h) in heads.iter().enumerate() {
        verdict_note(
            &notes,
            &format!("20260920-1159{i}0-x-p{n}-verdict.md", i = i, n = i + 1),
            h,
            "qa-1",
            "auto",
        );
        report_json["statuses"][h] = json!({
            "statuses": [{
                "context": "qa-verdict", "state": "SUCCESS",
                "created_at": "2026-09-20T11:59:30Z",
                "creator": {"login": "qa-bot"}
            }]
        });
    }
    std::fs::write(&report, report_json.to_string()).unwrap();

    let out = run_audit(&state, &pm, &repo, &notes, &report, &[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "clean rows must not flag:\n{text}"
    );
    assert!(text.contains("#1"), "{text}");
    assert!(text.contains("reviewer qa-1"), "{text}");
    assert!(text.contains("merger ops-1"), "{text}");
    assert!(text.contains("contains_head yes"), "{text}");
    assert!(text.contains("class auto"), "{text}");
    assert!(text.contains("trigger test trigger"), "{text}");
    assert!(!text.contains("FLAG"), "{text}");
    // The root commit shows as a `?` row but is exempt from flags —
    // it predates the PR process. Any *later* direct push would flag.
    assert!(text.contains("? init"), "{text}");
    // Read-only: the repo must be byte-identical afterwards.
    assert_eq!(git_porcelain(&repo), "", "audit must not dirty the repo");
}

#[test]
fn audit_flags_reviewer_equals_merger() {
    let dir = TempDir::new().unwrap();
    let (repo, notes, report, heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&pm).unwrap();
    // The note's `From:` is an agent alias — it happens to spell the
    // same string as the merger's GitHub login, which must NOT flag:
    // the namespaces differ.
    verdict_note(
        &notes,
        "20260920-115900-x-p3-verdict.md",
        &heads[2],
        "ops-1",
        "auto",
    );
    let mut report_json: Value =
        serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    let status = |login: &str| {
        json!({
            heads[2].clone(): {"statuses":[{
                "context":"qa-verdict","state":"SUCCESS",
                "created_at":"2026-09-20T11:59:30Z",
                "creator":{"login":login}
            }]}
        })
    };
    report_json["statuses"] = status("qa-1");
    std::fs::write(&report, report_json.to_string()).unwrap();

    // Alias collision alone: no flag.
    let out = run_audit(&state, &pm, &repo, &notes, &report, &[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !text.contains("reviewer==merger"),
        "note From: alias must not feed the flag:\n{text}"
    );

    // Same GitHub identity posted qa-verdict and merged, while the run
    // shows the fleet has a separate QA identity (`qa-bot` on #1): the
    // row deviates from the norm — flag.
    report_json["statuses"] = status("ops-1");
    report_json["statuses"][heads[0].clone()] = json!({"statuses":[{
        "context":"qa-verdict","state":"SUCCESS",
        "created_at":"2026-09-20T11:59:30Z",
        "creator":{"login":"qa-bot"}
    }]});
    std::fs::write(&report, report_json.to_string()).unwrap();
    let out = run_audit(&state, &pm, &repo, &notes, &report, &[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "flag must exit 1:\n{text}");
    assert!(text.contains("FLAG[reviewer==merger]"), "{text}");
    assert!(text.contains("reviewer@gh ops-1"), "{text}");
    assert!(!text.contains("structural:"), "{text}");

    // CAD-207: when ops-1 is the ONLY GitHub identity in the run, the
    // match is structural — one summary line, no per-row flag.
    report_json["statuses"] = status("ops-1");
    std::fs::write(&report, report_json.to_string()).unwrap();
    let out = run_audit(&state, &pm, &repo, &notes, &report, &[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains("FLAG[reviewer==merger]"), "{text}");
    assert!(
        text.contains("structural: 1 merge(s) share GitHub identity ops-1"),
        "{text}"
    );
}

#[test]
fn audit_flags_merge_with_no_verdict_on_head() {
    let dir = TempDir::new().unwrap();
    let (repo, notes, report, _heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&pm).unwrap();
    // Empty notes dir, empty statuses — nothing proves a pass.
    let out = run_audit(&state, &pm, &repo, &notes, &report, &["--limit", "1"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "verdict-less head must flag:\n{text}"
    );
    assert!(text.contains("FLAG[no-passing-verdict]"), "{text}");
    assert!(text.contains("verdict unknown"), "{text}");
    assert!(text.contains("reviewer unknown"), "{text}");
    // The reasons must accompany the unknowns.
    let jout = run_audit(
        &state,
        &pm,
        &repo,
        &notes,
        &report,
        &["--limit", "1", "--json"],
    );
    let j: Value = serde_json::from_str(&String::from_utf8_lossy(&jout.stdout)).unwrap();
    let unknowns = j["merges"][0]["unknowns"].as_array().unwrap();
    assert!(
        unknowns.iter().any(|u| u["field"] == "verdict"),
        "unknown verdict needs a reason: {j}"
    );
}

#[test]
fn audit_json_shape_is_stable() {
    let dir = TempDir::new().unwrap();
    let (repo, notes, report, heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&pm).unwrap();
    verdict_note(
        &notes,
        "20260920-115900-x-p1-verdict.md",
        &heads[0],
        "qa-1",
        "auto",
    );

    let out = run_audit(
        &state,
        &pm,
        &repo,
        &notes,
        &report,
        &["--since", "24h", "--json"],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let j: Value = serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("--json not one document: {e}\n{text}"));
    assert_eq!(j["schema"].as_str().unwrap(), "cadence.audit/1");
    for key in [
        "repo",
        "default_ref",
        "since",
        "filters",
        "merges",
        "summary",
    ] {
        assert!(j.get(key).is_some(), "missing top-level {key}: {j}");
    }
    let m = &j["merges"][0];
    for key in [
        "pr",
        "title",
        "merge_sha",
        "landed_head",
        "reviewed_head",
        "contains_head",
        "qa_verdict_status",
        "qa_verdict_creator",
        "status_post_hoc",
        "verdict",
        "verdict_post_hoc",
        "reviewer",
        "merger",
        "class",
        "trigger",
        "gate_summary",
        "auditor_check",
        "residue",
        "outcome",
        "flags",
        "evidence_unavailable",
        "unknowns",
    ] {
        assert!(m.get(key).is_some(), "missing merges[].{key}: {m}");
    }
    for key in ["tree_match", "smoke", "daemon_restart", "revert"] {
        assert!(
            m["outcome"].get(key).is_some(),
            "missing outcome.{key}: {m}"
        );
    }
    assert!(j["summary"]["rows"].as_u64().unwrap() >= 3);
}

#[test]
fn audit_filters_since_class_project_limit() {
    let dir = TempDir::new().unwrap();
    let (repo, notes, report, heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    // Tracker: CAD-1 lives under project `alpha`.
    std::fs::create_dir_all(pm.join("alpha").join("CAD-1")).unwrap();
    std::fs::write(pm.join("alpha/CAD-1/issue.md"), "---\nid: CAD-1\n---\n").unwrap();
    verdict_note(
        &notes,
        "20260920-115800-x-p1-verdict.md",
        &heads[0],
        "qa-1",
        "auto",
    );
    verdict_note(
        &notes,
        "20260920-115810-x-p2-verdict.md",
        &heads[1],
        "qa-1",
        "human",
    );

    // --since far future → no rows.
    let out = run_audit(
        &state,
        &pm,
        &repo,
        &notes,
        &report,
        &["--since", "2999-01-01"],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("0 merges") || text.contains("no merges"),
        "{text}"
    );

    // --limit 2 → exactly two rows.
    let out = run_audit(&state, &pm, &repo, &notes, &report, &["--limit", "2"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("(2 merges"), "{text}");

    // --class auto → only the auto-classified row.
    let out = run_audit(&state, &pm, &repo, &notes, &report, &["--class", "auto"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("#1"), "{text}");
    assert!(!text.contains("#2"), "{text}");
    // --class human → the human row only.
    let out = run_audit(&state, &pm, &repo, &notes, &report, &["--class", "human"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("#2"), "{text}");
    assert!(!text.contains("#1"), "{text}");

    // --project alpha → only CAD-1's row.
    let out = run_audit(&state, &pm, &repo, &notes, &report, &["--project", "alpha"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("#1"), "{text}");
    assert!(!text.contains("#2"), "{text}");
}

#[test]
fn audit_fixture_never_shells_gh() {
    let dir = TempDir::new().unwrap();
    let (repo, notes, report, _heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&pm).unwrap();
    // PATH really has no gh: if fixture mode shelled out anyway the
    // spawn would fail and every row would report evidence gaps.
    let path = path_without_gh(&dir);
    let out = run_audit_full(
        &state,
        &pm,
        &repo,
        Some(&notes),
        Some(&report),
        &["--json"],
        Some(&path),
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let j: Value = serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("fixture mode must not call gh: {e}\n{text}"));
    // 3 merge subjects + the init commit.
    assert_eq!(j["merges"].as_array().unwrap().len(), 4);
    let pr_rows = j["merges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["pr"].as_u64().is_some())
        .count();
    assert_eq!(pr_rows, 3);
    // No row reports a failed channel — the fixture answered everything.
    assert!(
        j["merges"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["evidence_unavailable"].is_null()),
        "a spawn attempt would surface as evidence_unavailable: {j}"
    );
}

/// gh absent from PATH on the live path: every row is
/// `unknown — evidence unavailable`, nothing flags, exit 0. Missing
/// evidence is never an accusation.
#[test]
fn audit_gh_unavailable_is_unknown_not_flag() {
    let dir = TempDir::new().unwrap();
    let (repo, notes, report, _heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&pm).unwrap();
    // A github origin so the audit resolves a slug and really tries gh;
    // a verdict note proves notes answered (fail) — the row must still
    // not flag while gh itself is unreachable.
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success());
    };
    g(&["remote", "add", "origin", "https://github.com/x/y"]);
    // One non-PR direct push too — even it must not flag with gh down.
    std::fs::write(repo.join("direct.txt"), "d").unwrap();
    g(&["add", "."]);
    g(&["commit", "-qm", "direct push"]);

    let path = path_without_gh(&dir);
    let out = run_audit_full(
        &state,
        &pm,
        &repo,
        Some(&notes),
        None, // no --merge-report: the live gh path, with gh absent
        &["--json"],
        Some(&path),
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "unavailable evidence must not flag:\n{text}"
    );
    let j: Value = serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("--json not one document: {e}\n{text}"));
    let merges = j["merges"].as_array().unwrap();
    assert!(!merges.is_empty());
    for m in merges {
        assert!(
            m["flags"].as_array().unwrap().is_empty(),
            "no flags on missing evidence: {m}"
        );
    }
    // Every row that needed gh reports the gap with its reason.
    let gap_rows = merges
        .iter()
        .filter(|m| !m["evidence_unavailable"].is_null())
        .count();
    assert!(
        gap_rows >= merges.iter().filter(|m| m["pr"].is_u64()).count(),
        "gh-down PR rows must carry evidence_unavailable: {j}"
    );
    assert_eq!(j["summary"]["flagged"].as_u64().unwrap(), 0);
    // Suppress the unused-fixture warning — this test runs live.
    let _ = report;
}

#[test]
fn audit_evidence_unavailable_is_unknown_not_flag() {
    let dir = TempDir::new().unwrap();
    let (repo, _notes, report, _heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&pm).unwrap();
    // The notes directory does not exist — a verdict note could be in
    // it. `no-passing-verdict` must not fire: the row is unknown, and
    // unknown rows exit 0.
    let missing_notes = dir.path().join("no-such-notes");
    let out = run_audit(
        &state,
        &pm,
        &repo,
        &missing_notes,
        &report,
        &["--limit", "1"],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "missing evidence is unknown, not a flag:\n{text}"
    );
    assert!(!text.contains("FLAG["), "{text}");
    assert!(text.contains("evidence unavailable"), "{text}");

    // A PR absent from the fixture's `prs` list is likewise a data
    // gap, not a verdict failure.
    let mut report_json: Value =
        serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    report_json["prs"] = json!([]);
    std::fs::write(&report, report_json.to_string()).unwrap();
    let notes = dir.path().join("notes");
    let out = run_audit(
        &state,
        &pm,
        &repo,
        &notes,
        &report,
        &["--limit", "1", "--json"],
    );
    let j: Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(out.status.code(), Some(0), "{j}");
    let m = &j["merges"][0];
    assert!(
        m["flags"].as_array().unwrap().is_empty(),
        "evidence gaps must not flag: {m}"
    );
    assert!(
        m["evidence_unavailable"]
            .as_str()
            .is_some_and(|s| s.contains("no merged PR")),
        "gap reason must surface: {m}"
    );
}

#[test]
fn audit_post_hoc_verdict_does_not_clear_flag() {
    let dir = TempDir::new().unwrap();
    let (repo, notes, report, heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&pm).unwrap();
    // The verdict note's filename timestamp is *after* the merge —
    // evidence that arrived post-merge, not a merge-time review.
    verdict_note(
        &notes,
        "20260920-130000-x-p3-verdict.md",
        &heads[2],
        "qa-1",
        "auto",
    );
    let out = run_audit(
        &state,
        &pm,
        &repo,
        &notes,
        &report,
        &["--limit", "1", "--json"],
    );
    let j: Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(out.status.code(), Some(1), "post-hoc pass must flag: {j}");
    let m = &j["merges"][0];
    assert_eq!(m["pr"].as_u64(), Some(3), "{j}");
    assert_eq!(m["verdict_post_hoc"].as_bool(), Some(true), "{j}");
    assert!(
        m["flags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "no-passing-verdict"),
        "post-hoc pass must not clear the flag: {m}"
    );
}

// ---------- CAD-217: operator approval evidence ------------------------

/// `mergedAt` of `audit_repo`'s PR n is 2026-09-20T12:00:0nZ.
const AUDIT_MERGE_EPOCH: f64 = 1_789_905_600.0;

/// Record an approval through the store's writer, then pin its event
/// time — the fixture merges are in the past, so "before the merge"
/// needs an explicit clock.
fn seed_approval(state: &Path, id: &str, head: &str, pr: u64, at: f64) {
    seed_approval_in(state, "x/y", id, head, pr, at);
}

/// `seed_approval` scoped to another repository.
fn seed_approval_in(state: &Path, repo: &str, id: &str, head: &str, pr: u64, at: f64) {
    let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
    let (new, recorded) = store
        .record_approval(
            &cadence_agent::store::NewApproval {
                id: Some(id),
                source: "operator in chat",
                action: "merge",
                head_sha: head,
                repo,
                pr,
            },
            "operator-connection",
        )
        .unwrap();
    assert!(new);
    assert_eq!(recorded, id);
    drop(store);
    set_approval_at(state, "approval_recorded", id, at);
}

fn set_approval_at(state: &Path, kind: &str, id: &str, at: f64) {
    let conn = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    let n = conn
        .execute(
            "UPDATE events SET at=?1 WHERE alias='audit:approvals' AND kind=?2 \
             AND json_extract(payload,'$.approval_id')=?3",
            rusqlite::params![at, kind, id],
        )
        .unwrap();
    assert_eq!(n, 1);
}

/// CAD-217: a human-class merge binds to the operator approval in force
/// at merge time for the EXACT landed head. Approved, revoked-before-
/// merge, post-merge-only and older-head approvals each render with
/// their provenance; a cancelled queue message carrying the approval
/// phrase is never an approval; no readable store is `unknown`, not a
/// flag.
#[test]
fn audit_binds_human_merge_to_exact_head_approval() {
    let dir = TempDir::new().unwrap();
    let (repo, notes, report, heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&pm).unwrap();
    for (i, h) in heads.iter().enumerate() {
        verdict_note(
            &notes,
            &format!("20260920-1159{i}0-x-p{n}-verdict.md", n = i + 1),
            h,
            "qa-1",
            "human",
        );
    }
    // The checkout's origin scopes approvals even in fixture runs.
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["remote", "add", "origin", "https://github.com/x/y"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let before = AUDIT_MERGE_EPOCH - 600.0;
    let after = AUDIT_MERGE_EPOCH + 3600.0;
    // #1: approved for its landed head before the merge.
    seed_approval(&state, "ap-1", &heads[0], 1, before);
    // #2: an approval for an OLDER head before the merge, and one for
    // the landed head only after it.
    let old_head = "1111111111111111111111111111111111111111";
    seed_approval(&state, "ap-2-old", old_head, 2, before);
    seed_approval(&state, "ap-2-late", &heads[1], 2, after);
    // …and one for the landed head before the merge, but scoped to
    // another repository: out of scope, never binds.
    seed_approval_in(&state, "other/z", "ap-2-foreign", &heads[1], 2, before);
    // #3: approved, then explicitly revoked before the merge.
    seed_approval(&state, "ap-3", &heads[2], 3, before);
    let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
    assert!(store
        .revoke_approval(
            "ap-3",
            "operator in chat",
            "head moved",
            "operator-connection"
        )
        .unwrap());
    // The PR #84 shape: the approval phrase for #2's landed head sits
    // only in a queue message that was then CANCELLED.
    let cwd = dir.path().to_string_lossy().to_string();
    store
        .register_agent(&NewAgent {
            alias: "ops-1",
            provider: "inbox",
            endpoint_kind: "inbox",
            role: "worker",
            cwd: &cwd,
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    store
        .enqueue(
            "ops-1",
            &format!("OPERATOR APPROVED #2 at {}", heads[1]),
            None,
            "m-approval",
            "user",
        )
        .unwrap();
    store
        .cancel("m-approval", "operator", Some("competing executor"))
        .unwrap();
    drop(store);
    set_approval_at(&state, "approval_revoked", "ap-3", before + 60.0);

    let out = run_audit(&state, &pm, &repo, &notes, &report, &["--json"]);
    let j: Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(out.status.code(), Some(1), "{j}");
    let row = |pr: u64| {
        j["merges"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["pr"].as_u64() == Some(pr))
            .unwrap()
            .clone()
    };
    let r1 = row(1);
    // Bound, but only an operator CLAIM until CAD-280.
    assert_eq!(r1["approval"]["state"], "operator-claimed", "{r1}");
    assert_eq!(r1["approval"]["verified"], false, "{r1}");
    assert_eq!(r1["approval"]["note"], "unverified until CAD-280", "{r1}");
    assert_eq!(r1["approval"]["required"], true, "{r1}");
    assert_eq!(r1["approval"]["before_merge"], true, "{r1}");
    assert_eq!(r1["approval"]["record"]["id"], "ap-1", "{r1}");
    assert_eq!(
        r1["approval"]["record"]["source"], "operator in chat",
        "{r1}"
    );
    assert_eq!(r1["approval"]["record"]["head_sha"], heads[0], "{r1}");
    assert_eq!(r1["approval"]["record"]["scope"]["pr"], 1, "{r1}");
    assert_eq!(
        r1["approval"]["record"]["recorded_via"], "operator-connection",
        "{r1}"
    );
    assert_eq!(r1["flags"], json!([]), "{r1}");

    let r2 = row(2);
    assert_eq!(r2["approval"]["state"], "missing", "{r2}");
    assert_eq!(r2["approval"]["before_merge"], false, "{r2}");
    assert!(
        r2["approval"]["reason"]
            .as_str()
            .unwrap()
            .contains("post-merge"),
        "{r2}"
    );
    assert_eq!(
        r2["approval"]["other_heads"][0]["head_sha"], old_head,
        "{r2}"
    );
    assert_eq!(r2["flags"], json!(["approval-missing"]), "{r2}");

    let r3 = row(3);
    assert_eq!(r3["approval"]["state"], "revoked", "{r3}");
    assert_eq!(r3["approval"]["revocation"]["reason"], "head moved", "{r3}");
    assert_eq!(r3["approval"]["revocation"]["before_merge"], true, "{r3}");
    assert_eq!(r3["flags"], json!(["approval-revoked"]), "{r3}");
    assert_eq!(j["summary"]["approvals"]["operator_claimed"], 1, "{j}");
    assert_eq!(j["summary"]["approvals"]["verified"], false, "{j}");

    let out = run_audit(&state, &pm, &repo, &notes, &report, &[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("approval operator-claimed · id ap-1 · source \"operator in chat\""),
        "{text}"
    );
    assert!(text.contains("unverified until CAD-280"), "{text}");
    assert!(
        text.contains("1 approval(s) operator-claimed (unverified until CAD-280)"),
        "{text}"
    );
    assert!(text.contains("(before merge)"), "{text}");
    assert!(text.contains("FLAG[approval-revoked]"), "{text}");

    // With no daemon store the approval question is unanswerable:
    // `unknown`, reported with its reason, never flagged.
    let bare = dir.path().join("bare-state");
    std::fs::create_dir_all(&bare).unwrap();
    let out = run_audit(&bare, &pm, &repo, &notes, &report, &["--json"]);
    let j: Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(out.status.code(), Some(0), "{j}");
    for pr in 1..=3u64 {
        let m = j["merges"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["pr"].as_u64() == Some(pr))
            .unwrap();
        assert_eq!(m["approval"]["state"], "unknown", "{m}");
        assert!(
            m["approval"]["reason"]
                .as_str()
                .unwrap()
                .contains("no daemon store"),
            "{m}"
        );
        assert_eq!(m["flags"], json!([]), "{m}");
    }
}

/// CAD-217: only an operator connection records or revokes approval
/// evidence. A pane (and every process descended from it — the RPC
/// and the `cadence audit approve` CLI alike) is refused, as is an
/// identity-shaped request field; the operator's writes dedupe and a
/// conflicting reuse of an id is refused.
#[test]
fn audit_approval_record_is_operator_only() {
    let d = TestDaemon::start();
    let home = TempDir::new().unwrap();
    let mut pane = LaneShell::spawn(home.path());
    plant_pane(&d, "pane-1", pane.pid());
    let head = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
    let params = json!({"id": "ap-7", "source": "operator in chat",
                        "head": head, "repo": "x/y", "pr": 7});

    let r = pane.rpc(&d.state, "approval_record", params.clone());
    let msg = r["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("operator action") && msg.contains("pane-1"),
        "{r}"
    );
    let (rc, out) = pane.cadence(
        &d.state,
        &format!("audit approve --pr 7 --head {head} --source op --repo x/y"),
    );
    assert_ne!(rc, 0, "{out}");
    assert!(out.contains("operator action"), "{out}");

    let mut forged = params.clone();
    forged["by"] = json!("operator");
    let err = d.operator_rpc("approval_record", forged).unwrap_err();
    assert!(err.to_string().contains("'by'"), "{err}");

    let r = d.operator_rpc("approval_record", params.clone()).unwrap();
    assert_eq!(r["state"], "recorded", "{r}");
    assert_eq!(r["duplicate"], false, "{r}");
    assert_eq!(r["recorded_via"], "operator-connection", "{r}");
    let r = d.operator_rpc("approval_record", params.clone()).unwrap();
    assert_eq!(r["duplicate"], true, "{r}");
    let mut other = params.clone();
    other["head"] = json!("0123456789012345678901234567890123456789");
    let err = d.operator_rpc("approval_record", other).unwrap_err();
    assert!(err.to_string().contains("different evidence"), "{err}");
    let mut short = params.clone();
    short["id"] = json!("ap-short");
    short["head"] = json!("abcdefa");
    assert!(d.operator_rpc("approval_record", short).is_err());

    let revoke = json!({"id": "ap-7", "source": "operator", "reason": "moved"});
    let r = pane.rpc(&d.state, "approval_revoke", revoke.clone());
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("operator action"),
        "{r}"
    );
    let r = d.operator_rpc("approval_revoke", revoke).unwrap();
    assert_eq!(r["state"], "revoked", "{r}");
    // A revoked id is never re-recorded — the operator is told why.
    let err = d
        .operator_rpc("approval_record", params.clone())
        .unwrap_err();
    assert!(
        err.to_string().contains("was revoked") && err.to_string().contains("--id"),
        "{err}"
    );

    // Revoke → re-approve with the DEFAULT id: a fresh record under
    // `<base>-2`, never a silent duplicate of the revoked one.
    let auto = json!({"source": "operator in chat", "head": head,
                      "repo": "x/y", "pr": 9});
    let r = d.operator_rpc("approval_record", auto.clone()).unwrap();
    assert_eq!(r["approval_id"], "merge-pr9-abcdefabcdef", "{r}");
    assert_eq!(r["duplicate"], false, "{r}");
    let r = d.operator_rpc("approval_record", auto.clone()).unwrap();
    assert_eq!(r["duplicate"], true, "{r}");
    let r = d
        .operator_rpc(
            "approval_revoke",
            json!({"id": "merge-pr9-abcdefabcdef", "source": "operator", "reason": "moved"}),
        )
        .unwrap();
    assert_eq!(r["state"], "revoked", "{r}");
    let r = d.operator_rpc("approval_record", auto.clone()).unwrap();
    assert_eq!(r["approval_id"], "merge-pr9-abcdefabcdef-2", "{r}");
    assert_eq!(r["duplicate"], false, "{r}");
    let r = d.operator_rpc("approval_record", auto).unwrap();
    assert_eq!(r["approval_id"], "merge-pr9-abcdefabcdef-2", "{r}");
    assert_eq!(r["duplicate"], true, "{r}");

    // The operator's CLI cannot be exercised as a child here: the test
    // daemon runs in this process, so every child descends from the
    // daemon and `operator_proof` refuses it (the CLI's argument
    // shaping is unit-tested in main.rs). A second operator record:
    let mut second = params.clone();
    second["id"] = json!("ap-8");
    second["pr"] = json!(8);
    let r = d.operator_rpc("approval_record", second).unwrap();
    assert_eq!(r["duplicate"], false, "{r}");

    // Exactly the operator's writes reached the approval stream.
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    let kinds: Vec<String> = conn
        .prepare("SELECT kind FROM events WHERE alias='audit:approvals' ORDER BY seq")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "approval_recorded",
            "approval_revoked",
            "approval_recorded",
            "approval_revoked",
            "approval_recorded",
            "approval_recorded"
        ]
    );
}

// ---------- CAD-207: the digest discriminates -------------------------

/// The live CAD-207 run: 36 merges all pushed through ONE GitHub
/// account (`cc-syntax`), 34 of them with a `qa-verdict` status that
/// same account posted, and 2 (#43, #50) with no passing verdict on
/// the landed head. The shared identity is reported once as a summary
/// line; only the two real findings flag.
#[test]
fn audit_digest_reports_shared_identity_once() {
    let dir = TempDir::new().unwrap();
    let repo = dir.path().join("repo");
    git_repo(&repo);
    let mut prs = Vec::new();
    let mut statuses = serde_json::Map::new();
    for n in 15..=50u64 {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q"])
            .args(["--allow-empty", "-m", &format!("work {n} (#{n})")])
            .output()
            .unwrap();
        assert!(out.status.success());
        let head = format!("{n:040x}");
        prs.push(
            json!({"number": n, "title": format!("work {n}"), "headRefOid": head,
                        "mergedBy": {"login": "cc-syntax"},
                        "mergedAt": "2026-09-20T12:00:00Z"}),
        );
        if n != 43 && n != 50 {
            statuses.insert(
                head,
                json!({"statuses": [{"context": "qa-verdict", "state": "success",
                                     "created_at": "2026-09-20T11:00:00Z",
                                     "creator": {"login": "cc-syntax"}}]}),
            );
        }
    }
    let report = dir.path().join("report.json");
    let write_report = |statuses: &serde_json::Map<String, Value>| {
        std::fs::write(
            &report,
            json!({"prs": prs, "statuses": statuses}).to_string(),
        )
        .unwrap();
    };
    write_report(&statuses);
    let notes = dir.path().join("notes");
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    for d in [&notes, &state, &pm] {
        std::fs::create_dir_all(d).unwrap();
    }

    let out = run_audit(&state, &pm, &repo, &notes, &report, &[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{text}");
    let flagged: Vec<&str> = text.lines().filter(|l| l.contains("FLAG[")).collect();
    assert_eq!(
        flagged,
        [
            "#50 work 50  FLAG[no-passing-verdict]",
            "#43 work 43  FLAG[no-passing-verdict]"
        ],
        "{text}"
    );
    assert_eq!(text.matches("reviewer==merger").count(), 1, "{text}");
    assert!(
        text.contains("structural: 34 merge(s) share GitHub identity cc-syntax"),
        "{text}"
    );

    let out = run_audit(&state, &pm, &repo, &notes, &report, &["--json"]);
    let j: Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(j["summary"]["flagged"], 2, "{j}");
    let st = &j["summary"]["structural"];
    assert_eq!(st.as_array().unwrap().len(), 1, "{j}");
    assert_eq!(st[0]["code"], "shared-github-identity", "{j}");
    assert_eq!(st[0]["identity"], "cc-syntax", "{j}");
    assert_eq!(st[0]["rows"], 34, "{j}");
    let structural_rows = j["merges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["structural"] == json!(["reviewer==merger"]))
        .count();
    assert_eq!(structural_rows, 34, "{j}");

    // A clean run against the shared-token fleet exits 0: pass the
    // two real findings and nothing is left to flag.
    for n in [43u64, 50] {
        statuses.insert(
            format!("{n:040x}"),
            json!({"statuses": [{"context": "qa-verdict", "state": "success",
                                 "created_at": "2026-09-20T11:00:00Z",
                                 "creator": {"login": "cc-syntax"}}]}),
        );
    }
    write_report(&statuses);
    let out = run_audit(&state, &pm, &repo, &notes, &report, &[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(!text.contains("FLAG["), "{text}");
    assert!(
        text.contains("structural: 36 merge(s) share GitHub identity cc-syntax"),
        "{text}"
    );
}

/// CAD-207 review: "one shared identity" is decided over every merged
/// PR the fetch returned, not the rendered window. The fleet has a
/// separate QA identity (`qa-bot` posted #1's status); a `--limit 1`
/// window holding only #3 — posted and merged by `ops-1` — must still
/// flag the self-review instead of calling it structural.
#[test]
fn audit_digest_decides_identity_over_the_fleet_not_the_window() {
    let dir = TempDir::new().unwrap();
    let (repo, notes, report, heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&pm).unwrap();
    let mut report_json: Value =
        serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    let status = |login: &str| {
        json!({"statuses":[{"context":"qa-verdict","state":"SUCCESS",
                            "created_at":"2026-09-20T11:59:30Z",
                            "creator":{"login":login}}]})
    };
    report_json["statuses"][heads[0].clone()] = status("qa-bot");
    report_json["statuses"][heads[2].clone()] = status("ops-1");
    std::fs::write(&report, report_json.to_string()).unwrap();

    let out = run_audit(&state, &pm, &repo, &notes, &report, &["--limit", "1"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("#3 work 3"), "{text}");
    assert!(text.contains("FLAG[reviewer==merger]"), "{text}");
    assert!(!text.contains("structural:"), "{text}");
    assert_eq!(out.status.code(), Some(1), "{text}");
}

/// A fake `gh` for the live audit path (CAD-287): `pr list` answers
/// `$FAKE_GH_DIR/prs.json`, `statuses/<sha>` answers
/// `status-<sha>.json` (or `[]`), the combined endpoint has nothing.
/// Every call is logged to `calls.log`. `audit_live_gh` prepends the
/// shebang and `FAKE_GH_DIR` — baked in, not a process-wide env var
/// that parallel tests would race on.
const AUDIT_FAKE_GH: &str = r#"
printf '%s\n' "$*" >> "$FAKE_GH_DIR/calls.log"
case "$1 $2" in
  "pr list"*) cat "$FAKE_GH_DIR/prs.json" ;;
  "api repos/x/y/statuses/"*)
    sha=${2#repos/x/y/statuses/}; sha=${sha%%\?*}
    if [ -f "$FAKE_GH_DIR/status-$sha.json" ]; then cat "$FAKE_GH_DIR/status-$sha.json"; else echo '[]'; fi ;;
  "api repos/x/y/commits/"*) echo '{"statuses": []}' ;;
  *) echo "fake gh: unexpected call: $*" >&2; exit 64 ;;
esac
"#;

/// `audit_repo` wired for the live path: a github.com origin, the
/// fake gh first on PATH, `prs.json` from the fixture's PR list, and
/// one `statuses/<head>` answer per `(pr index, creator)` in `posted`.
/// Returns the PATH to run under and the gh call log.
fn audit_live_gh(
    dir: &TempDir,
    repo: &Path,
    report: &Path,
    heads: &[String],
    posted: &[(usize, &str)],
) -> (PathBuf, PathBuf) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["remote", "add", "origin", "https://github.com/x/y"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let ghdir = dir.path().join("fake-gh");
    std::fs::create_dir_all(&ghdir).unwrap();
    let fixture: Value = serde_json::from_str(&std::fs::read_to_string(report).unwrap()).unwrap();
    std::fs::write(ghdir.join("prs.json"), fixture["prs"].to_string()).unwrap();
    for (i, login) in posted {
        let list = json!([{"context": "qa-verdict", "state": "success",
                           "created_at": "2026-09-20T11:59:30Z",
                           "creator": {"login": login}}]);
        std::fs::write(
            ghdir.join(format!("status-{}.json", heads[*i])),
            list.to_string(),
        )
        .unwrap();
    }
    let gh = ghdir.join("gh");
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\nFAKE_GH_DIR='{}'{AUDIT_FAKE_GH}",
            ghdir.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!(
        "{}:{}",
        ghdir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    (PathBuf::from(path), ghdir.join("calls.log"))
}

/// CAD-287: a live run fetches statuses only for rendered rows, so its
/// in-hand creators are the window's. QA posts #1's qa-verdict with its
/// own token (outside a `--limit 1` window); in-window #3 was posted
/// and merged by `ops-1` — a real self-review. The mergers alone (all
/// `ops-1`) must not make it structural: flagged, and #1's status is
/// still never fetched (`--limit` keeps bounding the fan-out).
#[test]
fn audit_live_self_review_outside_qa_token_window_is_flagged() {
    let dir = TempDir::new().unwrap();
    let (repo, notes, report, heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&pm).unwrap();
    let (path, calls) = audit_live_gh(&dir, &repo, &report, &heads, &[(0, "qa-bot"), (2, "ops-1")]);

    let out = run_audit_full(
        &state,
        &pm,
        &repo,
        Some(&notes),
        None, // live gh path
        &["--limit", "1"],
        Some(&path),
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let log = std::fs::read_to_string(&calls).unwrap_or_default();
    assert!(text.contains("#3 work 3"), "{text}");
    assert!(
        text.contains("reviewer@gh ops-1"),
        "live status read: {text}"
    );
    assert!(text.contains("FLAG[reviewer==merger]"), "{text}\n{log}");
    assert!(!text.contains("structural:"), "{text}");
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(log.contains(&format!("statuses/{}", heads[2])), "{log}");
    assert!(
        !log.contains(&heads[0]),
        "out-of-window head fetched: {log}"
    );
}

/// CAD-287, the other side: a shared-token fleet (every merge and every
/// qa-verdict by `ops-1`). A window that holds every fetched PR decides
/// the fleet live — structural, exit 0. A narrower window cannot see
/// the rest of the fleet's creators, so it keeps the per-row flag.
#[test]
fn audit_live_structural_only_when_every_fetched_pr_is_in_hand() {
    let dir = TempDir::new().unwrap();
    let (repo, notes, report, heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&pm).unwrap();
    let posted = [(0, "ops-1"), (1, "ops-1"), (2, "ops-1")];
    let (path, _calls) = audit_live_gh(&dir, &repo, &report, &heads, &posted);
    let run =
        |extra: &[&str]| run_audit_full(&state, &pm, &repo, Some(&notes), None, extra, Some(&path));

    let out = run(&["--limit", "0"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("structural: 3 merge(s) share GitHub identity ops-1"),
        "{text}"
    );
    assert!(!text.contains("FLAG["), "{text}");
    assert_eq!(out.status.code(), Some(0), "{text}");

    let out = run(&["--limit", "1"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("FLAG[reviewer==merger]"), "{text}");
    assert!(!text.contains("structural:"), "{text}");
    assert_eq!(out.status.code(), Some(1), "{text}");
}

/// CAD-287: an operator-claimed approval satisfies the human-class gate
/// in the exit code — the one channel that cannot carry the
/// disclosure — and docs/AUDIT.md must say so until CAD-280.
#[test]
fn audit_docs_state_exit_code_accepts_operator_claimed() {
    let doc =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/AUDIT.md")).unwrap();
    let doc = doc.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        doc.contains(
            "The exit code treats an `operator-claimed` approval as satisfied: \
             text and JSON disclose the claim, the exit code cannot."
        ),
        "docs/AUDIT.md must state the exit-code limitation"
    );
}
