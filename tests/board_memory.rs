//! board_memory: area tests split from tests/board.rs (CAD-537).
//! Board e2e: the `cadence issue` CLI against a temp PM dir, and the
//! `cadence ui` HTTP server in-process.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod board_common;
use board_common::*;

use cadence_agent::issue::time;
use serde_json::json;
use serde_json::Value;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use std::time::Instant;
use tempfile::TempDir;

// ==== project memory (CAD-68) ====

/// A fixture with a project repo + components for memory tests.
fn mem_fx() -> (TempDir, PathBuf, PathBuf, PathBuf) {
    let (_t, pm, state, repo) = start_fx();
    let repo_s = repo.to_str().unwrap().to_string();
    // start_fx's project is `demo` with no components — add the
    // component-bearing project `mem` alongside it.
    assert!(
        cli(
            &pm,
            &state,
            &[
                "issue",
                "project",
                "add",
                "mem",
                "--prefix",
                "M",
                "--repo",
                &repo_s,
                "--component",
                "daemon",
                "--component",
                "other",
            ]
        )
        .0
    );
    (_t, pm, state, repo)
}

fn mem_body(fact: &str) -> String {
    format!("{fact}\n\n**Why:** the test reason.\n\n**How to apply:** apply the fact.\n")
}

/// `git` with the just-built cadence first on PATH — needed for `git
/// commit` inside a pm that has a `memory/` dir, where the tracker's
/// pre-commit lint hook shells out to `cadence` and an older release
/// binary on PATH would refuse the memory dir.
fn git_hooked(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        // Same identity pm.commit injects — CI runners have no global
        // git config, so a plain `git commit` refuses without it.
        .args(["-c", "user.name=test", "-c", "user.email=t@t"])
        .args(args)
        .env(
            "PATH",
            format!(
                "{}:{}",
                Path::new(bin()).parent().unwrap().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    )
}

/// `memory <verb>` against the fixture.
fn mem_cli(pm: &Path, state: &Path, args: &[&str]) -> (bool, Value) {
    let mut full = vec!["memory"];
    full.extend_from_slice(args);
    cli(pm, state, &full)
}

fn mem_cli_env(pm: &Path, state: &Path, args: &[&str], env: &[(&str, &str)]) -> (bool, Value) {
    let mut full = vec!["memory"];
    full.extend_from_slice(args);
    cli_env(pm, state, &full, env)
}

fn propose(pm: &Path, state: &Path, slug: &str, kind: &str, extra: &[&str]) -> (bool, Value) {
    let body = mem_body(&format!("fact for {slug}"));
    let mut args = vec![
        "propose",
        "--project",
        "mem",
        "--type",
        kind,
        "--id",
        slug,
        "-m",
        &body,
    ];
    args.extend_from_slice(extra);
    mem_cli(pm, state, &args)
}

/// Write an explicitly legacy/unverified fixture for reader tests.  It is
/// intentionally not an authority-bearing setup: accepted records without
/// native proposer/review/finalization receipts must remain visible but
/// blocked from retrieval.  Native write coverage belongs to the daemon
/// integration tests, where the socket can prove a live PTY endpoint.
fn legacy_memory(
    pm: &Path,
    slug: &str,
    kind: &str,
    extra: &[&str],
    status: &str,
    verified_at: Option<&str>,
) {
    let mut project = false;
    let mut components = Vec::new();
    let mut paths = Vec::new();
    let mut providers = Vec::new();
    let mut tags = Vec::new();
    let mut confidence = "medium";
    let mut source = None;
    let mut i = 0;
    while i < extra.len() {
        match extra[i] {
            "--scope-project" => {
                project = true;
                i += 1;
            }
            "--scope-component" => {
                components.push(extra[i + 1]);
                i += 2;
            }
            "--scope-path" => {
                paths.push(extra[i + 1]);
                i += 2;
            }
            "--scope-provider" => {
                providers.push(extra[i + 1]);
                i += 2;
            }
            "--scope-tag" => {
                tags.push(extra[i + 1]);
                i += 2;
            }
            "--confidence" => {
                confidence = extra[i + 1];
                i += 2;
            }
            "--source" => {
                source = Some(extra[i + 1]);
                i += 2;
            }
            _ => i += 1,
        }
    }
    let mut yaml = format!(
        "id: {slug}\ntype: {kind}\nstatus: {status}\nconfidence: {confidence}\ncreated: 2026-01-01T00:00:00Z\n"
    );
    if let Some(source) = source {
        yaml.push_str(&format!("source: {source}\n"));
    }
    if let Some(verified_at) = verified_at {
        yaml.push_str(&format!("verified_at: {verified_at}\n"));
    }
    if project
        || !components.is_empty()
        || !paths.is_empty()
        || !providers.is_empty()
        || !tags.is_empty()
    {
        yaml.push_str("scope:\n");
        if project {
            yaml.push_str("  project: true\n");
        }
        if !components.is_empty() {
            yaml.push_str("  components:\n");
            for value in components {
                yaml.push_str(&format!("    - {value}\n"));
            }
        }
        if !paths.is_empty() {
            yaml.push_str("  paths:\n");
            for value in paths {
                yaml.push_str(&format!("    - \"{value}\"\n"));
            }
        }
        if !providers.is_empty() {
            yaml.push_str("  providers:\n");
            for value in providers {
                yaml.push_str(&format!("    - {value}\n"));
            }
        }
        if !tags.is_empty() {
            yaml.push_str("  tags:\n");
            for value in tags {
                yaml.push_str(&format!("    - {value}\n"));
            }
        }
    } else {
        yaml.push_str("scope: {}\n");
    }
    std::fs::create_dir_all(pm.join("mem/memory")).unwrap();
    std::fs::write(
        pm.join(format!("mem/memory/{slug}.md")),
        format!(
            "---\n{yaml}---\n\n{}",
            mem_body(&format!("fact for {slug}"))
        ),
    )
    .unwrap();
}

#[test]
fn memory_writes_require_native_identity() {
    let (_t, pm, state, _repo) = mem_fx();
    let before = commits(&pm);

    // A CLI process is not itself an enrolled native endpoint.  Every
    // authority-bearing write therefore fails before creating a file or
    // tracker commit; the native daemon integration owns the positive path.
    let (ok, err) = propose(
        &pm,
        &state,
        "pipe-drain",
        "gotcha",
        &["--scope-path", "src/**"],
    );
    assert!(!ok, "{err}");
    assert!(
        err["error"]
            .as_str()
            .unwrap()
            .contains("Daemon is not reachable"),
        "{err}"
    );
    assert!(!pm.join("mem/memory/pipe-drain.md").exists());
    assert_eq!(
        commits(&pm),
        before,
        "failed native write changed the tracker"
    );

    legacy_memory(
        &pm,
        "pipe-drain",
        "gotcha",
        &["--scope-path", "src/**"],
        "proposed",
        None,
    );
    for args in [
        vec!["accept", "pipe-drain", "--project", "mem"],
        vec!["reject", "pipe-drain", "--project", "mem"],
        vec!["verify", "pipe-drain", "--project", "mem"],
        vec![
            "supersede",
            "pipe-drain",
            "pipe-drain-new",
            "--project",
            "mem",
        ],
    ] {
        let (ok, err) = mem_cli(&pm, &state, &args);
        assert!(!ok, "{args:?} unexpectedly wrote: {err}");
        assert!(
            err["error"]
                .as_str()
                .unwrap()
                .contains("Daemon is not reachable"),
            "{args:?}: {err}"
        );
    }
    let text = std::fs::read_to_string(pm.join("mem/memory/pipe-drain.md")).unwrap();
    assert!(text.contains("status: proposed"), "{text}");
}

#[test]
fn memory_write_guards() {
    let (_t, pm, state, _repo) = mem_fx();
    legacy_memory(
        &pm,
        "curated",
        "rule",
        &["--scope-project"],
        "proposed",
        None,
    );

    // An operator or environment alias is not a native endpoint proof.
    // Every authority-bearing action fails closed while the daemon is down.
    for args in [
        vec!["accept", "curated", "--project", "mem"],
        vec!["reject", "curated", "--project", "mem"],
        vec!["verify", "curated", "--project", "mem"],
    ] {
        let (ok, err) = mem_cli_env(&pm, &state, &args, &[("CADENCE_ALIAS", "w1")]);
        assert!(!ok, "{args:?} should refuse: {err}");
        assert!(
            err["error"]
                .as_str()
                .unwrap()
                .contains("Daemon is not reachable"),
            "{err}"
        );
    }
    let text = std::fs::read_to_string(pm.join("mem/memory/curated.md")).unwrap();
    assert!(text.contains("status: proposed"), "{text}");

    // …and the same alias cannot turn a CLI process into a proposer.
    let (ok, out) = mem_cli_env(
        &pm,
        &state,
        &[
            "propose",
            "--project",
            "mem",
            "--type",
            "gotcha",
            "--id",
            "w-lesson",
            "--scope-project",
            "-m",
            &mem_body("worker learned a thing"),
        ],
        &[("CADENCE_ALIAS", "w1")],
    );
    assert!(!ok, "{out}");
    assert!(
        out["error"]
            .as_str()
            .unwrap()
            .contains("Daemon is not reachable"),
        "{out}"
    );
    assert!(!pm.join("mem/memory/w-lesson.md").exists());

    // The daemon is consulted before write-time validation, so malformed
    // request content cannot use the CLI as a local validation bypass.
    let (ok, err) = mem_cli(
        &pm,
        &state,
        &[
            "propose",
            "--project",
            "mem",
            "--type",
            "rule",
            "--id",
            "no-why",
            "--scope-project",
            "-m",
            "just a fact, no sections",
        ],
    );
    assert!(
        !ok && err["error"]
            .as_str()
            .unwrap()
            .contains("Daemon is not reachable"),
        "{err}"
    );
    let (ok, err) = mem_cli(
        &pm,
        &state,
        &[
            "propose",
            "--project",
            "mem",
            "--type",
            "rule",
            "--id",
            "bad-comp",
            "--scope-component",
            "nope",
            "-m",
            &mem_body("x"),
        ],
    );
    assert!(
        !ok && err["error"]
            .as_str()
            .unwrap()
            .contains("Daemon is not reachable"),
        "{err}"
    );
    // Lint flags a dangling supersedes on a hand-edited file.
    let bad = pm.join("mem/memory/dangling.md");
    std::fs::write(
        &bad,
        format!(
            "---\nid: dangling\ntype: rule\nstatus: accepted\nconfidence: high\ncreated: 2026-01-01T00:00:00Z\nsupersedes: ghost\nscope:\n  project: true\n---\n\n{}",
            mem_body("dangling link")
        ),
    )
    .unwrap();
    let (ok, out) = mem_cli(&pm, &state, &["lint"]);
    assert!(!ok, "{out}");
    assert!(
        out["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e.as_str().unwrap().contains("supersedes 'ghost'")),
        "{out}"
    );
    std::fs::remove_file(&bad).unwrap();
}

#[test]
fn memory_match_blocks_legacy_records() {
    let (_t, pm, state, repo) = mem_fx();

    // The ten-memory fixture across every scope axis. These are deliberately
    // legacy accepted records: their scope metadata remains readable, but
    // retrieval must refuse them because they have no native proposer and
    // PM finalization proof.
    let specs: [(&str, &str, &[&str]); 10] = [
        (
            "r-project",
            "rule",
            &["--scope-project", "--confidence", "high"],
        ),
        ("r-comp", "rule", &["--scope-component", "daemon"]),
        ("r-low", "rule", &["--scope-project", "--confidence", "low"]),
        (
            "g-comp-hi",
            "gotcha",
            &["--scope-component", "daemon", "--confidence", "high"],
        ),
        ("g-tag", "gotcha", &["--scope-tag", "flaky"]),
        (
            "g-comp-lo",
            "gotcha",
            &["--scope-component", "daemon", "--confidence", "low"],
        ),
        ("c-path", "recipe", &["--scope-path", "src/**"]),
        ("c-prov", "recipe", &["--scope-provider", "claude"]),
        (
            "d-prov",
            "decision",
            &["--scope-provider", "claude", "--confidence", "high"],
        ),
        ("x-other", "gotcha", &["--scope-component", "other"]),
    ];
    for (slug, kind, extra) in &specs {
        legacy_memory(
            &pm,
            slug,
            kind,
            extra,
            "accepted",
            Some("2026-01-01T00:00:00Z"),
        );
    }
    // A proposed (not yet accepted) twin must never match.
    legacy_memory(
        &pm,
        "pending",
        "rule",
        &["--scope-project"],
        "proposed",
        None,
    );

    // Issue M-1: component daemon + tags [flaky] + a code commit
    // touching src/adapter/x.rs — path/tag/component/provider axes
    // all live.
    assert!(
        cli(
            &pm,
            &state,
            &[
                "issue",
                "new",
                "Scoped Work",
                "--project",
                "mem",
                "--component",
                "daemon"
            ]
        )
        .0
    );
    // tags isn't a writable field on this base — hand-edit it in;
    // the loose reader picks it up.
    let issue_md = pm.join("mem/M-1/issue.md");
    let text = std::fs::read_to_string(&issue_md).unwrap();
    std::fs::write(
        &issue_md,
        text.replace("created:", "tags: [flaky]\ncreated:"),
    )
    .unwrap();
    git_hooked(&pm, &["add", "-A"]);
    git_hooked(&pm, &["commit", "-qm", "tag fixture"]);
    // A code commit the issue discovery will pick up (subject names M-1).
    std::fs::create_dir_all(repo.join("src/adapter")).unwrap();
    std::fs::write(repo.join("src/adapter/x.rs"), "x").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "M-1 adapter work"]);
    // D-2-equivalent: M-2 has no component/tags/commits.
    assert!(cli(&pm, &state, &["issue", "new", "Bare", "--project", "mem"]).0);

    let (ok, out) = mem_cli(
        &pm,
        &state,
        &["match", "--issue", "M-1", "--provider", "claude", "--json"],
    );
    assert!(ok, "{out}");
    let slugs: Vec<&str> = out["matched"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["slug"].as_str().unwrap())
        .collect();
    assert!(
        slugs.is_empty(),
        "legacy records must stay blocked: {slugs:?}"
    );

    // M-2 also has no eligible native records.
    let (ok, out) = mem_cli(&pm, &state, &["match", "--issue", "M-2", "--json"]);
    assert!(ok, "{out}");
    let slugs: Vec<&str> = out["matched"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["slug"].as_str().unwrap())
        .collect();
    assert!(slugs.is_empty(), "{slugs:?}");

    // Explicit-axis match without an issue.
    let (ok, out) = mem_cli(
        &pm,
        &state,
        &[
            "match",
            "--project",
            "mem",
            "--component",
            "other",
            "--json",
        ],
    );
    assert!(ok, "{out}");
    let slugs: Vec<&str> = out["matched"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["slug"].as_str().unwrap())
        .collect();
    assert!(slugs.is_empty(), "{slugs:?}");
}

/// Give a `legacy_memory` fixture a PM-finalized verify cycle at `at` —
/// the receipt retrieval reads as "last verified". Freshness readers do
/// not require quorum; the raw `verified_at` field is not the receipt.
fn verify_fixture(pm: &Path, slug: &str, at: &str) {
    let path = pm.join(format!("mem/memory/{slug}.md"));
    let text = std::fs::read_to_string(&path).unwrap();
    let (mut front, body) = cadence_agent::memory::parse_memory(&text).unwrap();
    front.review_cycle = 2;
    front.finalizations.retain(|r| r.operation != "verify");
    front
        .finalizations
        .push(cadence_agent::memory::FinalizationReceipt {
            operation: "verify".to_string(),
            cycle: 2,
            digest: "fixture".to_string(),
            finalizer: cadence_agent::memory::IdentityProof {
                alias: "fixture-pm".to_string(),
                registration: 4,
                generation: "fixture-pm-4".to_string(),
                process_start: 104,
                role: "pm".to_string(),
            },
            finalized_at: at.to_string(),
        });
    std::fs::write(
        &path,
        cadence_agent::issue::parse::render(&front, &body).unwrap(),
    )
    .unwrap();
}

/// Set (or clear) a fixture's explicit `stale:` mark.
fn mark_stale_fixture(pm: &Path, slug: &str, why: Option<&str>) {
    let path = pm.join(format!("mem/memory/{slug}.md"));
    let text = std::fs::read_to_string(&path).unwrap();
    let (mut front, body) = cadence_agent::memory::parse_memory(&text).unwrap();
    front.stale = why.map(str::to_string);
    std::fs::write(
        &path,
        cadence_agent::issue::parse::render(&front, &body).unwrap(),
    )
    .unwrap();
}

fn stale_entry<'a>(out: &'a Value, slug: &str) -> Option<&'a Value> {
    out["stale"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["slug"] == slug)
}

#[test]
fn memory_stale_flags_changed_paths() {
    let (_t, pm, state, repo) = mem_fx();
    let verified_epoch = time::now_epoch();
    let verified = time::iso(verified_epoch);
    for (slug, glob) in [("src-watch", "src/**"), ("docs-watch", "docs/**")] {
        legacy_memory(
            &pm,
            slug,
            "rule",
            &["--scope-path", glob],
            "accepted",
            Some(&verified),
        );
        verify_fixture(&pm, slug, &verified);
    }
    // The change must land strictly after the verify. Both stamps are
    // whole seconds: wait for the clock to pass the verified second
    // (often already true after the CLI calls).
    let deadline = Instant::now() + Duration::from_secs(5);
    while time::now_epoch() <= verified_epoch {
        assert!(Instant::now() < deadline, "clock never passed {verified}");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/changed.rs"), "x").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "late src change"]);

    let (ok, out) = mem_cli(&pm, &state, &["ls", "--stale", "--json"]);
    assert!(ok, "{out}");
    let slugs: Vec<&str> = out["stale"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, vec!["src-watch"], "{out}");
    assert_eq!(
        out["stale"][0]["reason"].as_str().unwrap(),
        "paths changed after verified_at"
    );
    assert_eq!(out["stale"][0]["evidence"]["state"], "verified", "{out}");
    assert_eq!(
        out["stale"][0]["changed"],
        json!(["src/changed.rs"]),
        "{out}"
    );
}

/// CAD-395: `ls --stale` reads freshness through the same `Freshness`
/// retrieval uses — past the window reads "unverified (last verified
/// <date>)", a stale mark reads withheld with its reason, a raw
/// `verified_at` with no verify receipt reads unverified, and none of
/// them is ever "verified".
#[test]
fn memory_stale_reads_retrieval_freshness() {
    let (_t, pm, state, _repo) = mem_fx();
    let now = time::iso(time::now_epoch());
    for slug in ["fresh", "aged", "marked", "raw-stamp"] {
        legacy_memory(
            &pm,
            slug,
            "rule",
            &["--scope-project"],
            "accepted",
            Some(&now),
        );
    }
    verify_fixture(&pm, "fresh", &now);
    verify_fixture(&pm, "aged", "2020-01-01T00:00:00Z");
    verify_fixture(&pm, "marked", &now);
    mark_stale_fixture(&pm, "marked", Some("M-9 reverted the cited fix"));

    let (ok, out) = mem_cli(&pm, &state, &["ls", "--stale", "--json"]);
    assert!(ok, "{out}");
    assert!(stale_entry(&out, "fresh").is_none(), "{out}");
    let aged = stale_entry(&out, "aged").expect("past-window is stale");
    assert_eq!(aged["reason"], "not verified within the window");
    assert_eq!(aged["verified_at"], "2020-01-01T00:00:00Z");
    assert_eq!(aged["evidence"]["state"], "unverified");
    assert_eq!(
        aged["evidence"]["label"],
        "unverified (last verified 2020-01-01)"
    );
    let marked = stale_entry(&out, "marked").expect("stale mark is stale");
    assert_eq!(
        marked["reason"],
        "evidence marked stale: M-9 reverted the cited fix"
    );
    assert_eq!(marked["evidence"]["state"], "withheld");
    assert_eq!(
        marked["evidence"]["reason"],
        "evidence marked stale: M-9 reverted the cited fix"
    );
    // A raw verified_at of now is not a verify receipt.
    let raw = stale_entry(&out, "raw-stamp").expect("raw stamp is not a verify");
    assert_eq!(raw["verified_at"], Value::Null);
    assert_eq!(raw["evidence"]["label"], "unverified");

    // The text view carries the same labels, never "verified".
    let (ok, text, _) = cli_out_err(&pm, &state, &["memory", "ls", "--stale"]);
    assert!(ok, "{text}");
    assert!(
        text.contains(
            "mem/aged\tunverified (last verified 2020-01-01)\tnot verified within the window"
        ),
        "{text}"
    );
    assert!(
        text.contains("mem/marked\twithheld\tevidence marked stale: M-9 reverted the cited fix"),
        "{text}"
    );
    assert!(text.contains("mem/raw-stamp\tunverified\t"), "{text}");
    assert!(!text.contains("\tverified"), "{text}");

    // The window is the project's `memory.stale_days`; `--days` overrides.
    let yaml = pm.join("mem/project.yaml");
    let mut conf = std::fs::read_to_string(&yaml).unwrap();
    conf.push_str("memory:\n  stale_days: 100000\n");
    std::fs::write(&yaml, conf).unwrap();
    let (ok, out) = mem_cli(&pm, &state, &["ls", "--stale", "--json"]);
    assert!(ok, "{out}");
    assert!(stale_entry(&out, "aged").is_none(), "{out}");
    let (ok, out) = mem_cli(&pm, &state, &["ls", "--stale", "--days", "30", "--json"]);
    assert!(ok, "{out}");
    assert!(stale_entry(&out, "aged").is_some(), "{out}");
}

#[test]
fn memory_seed_lessons_require_native_identity() {
    let (_t, pm, state, _repo) = mem_fx();
    // The kickoff's seed list — ten lessons, verbatim.
    let lessons: [(&str, &str); 10] = [
        ("detached-reviews", "Reviews happen on detached checkouts only."),
        ("isolate-before-blame", "Compare a failure in isolation on the PR head and on the base before blaming a PR."),
        ("stress-state-waits", "Stress new state-waiting tests before trusting them."),
        ("gate-moved-base", "Gate the merge result when the base moved under the PR."),
        ("pipe-draining", "4 KB pipes require draining while waiting on a child process."),
        ("devin-prompts", "The Devin busy prompt is `Guide Devin while it works` vs idle `Ask Devin to build…`."),
        ("worktree-idle-probe", "Never remove a worktree until the owner probes idle."),
        ("restart-quiet-queue", "Queue nothing immediately before a daemon restart."),
        ("claude-default-model", "Managed Claude workers inherit the host default model unless --model is provided."),
        ("rebase-closing-brace", "After a keep-both rebase at end of file, re-insert the closing brace Git matched as context."),
    ];
    for (slug, fact) in &lessons {
        let (ok, out) = mem_cli(
            &pm,
            &state,
            &[
                "propose", "--project", "mem", "--type", "rule", "--id", slug,
                "--scope-project", "--source", "CAD-68", "-m",
                &format!(
                    "{fact}\n\n**Why:** learned the hard way in cadence development.\n\n**How to apply:** check this before the relevant step.\n"
                ),
            ],
        );
        assert!(
            !ok,
            "{slug} unexpectedly imported without native identity: {out}"
        );
        assert!(
            out["error"]
                .as_str()
                .unwrap()
                .contains("Daemon is not reachable"),
            "{slug}: {out}"
        );
    }
    let (ok, out) = mem_cli(&pm, &state, &["lint"]);
    assert!(ok && out["ok"] == true, "{out}");
    let (ok, out) = cli(&pm, &state, &["issue", "lint"]);
    assert!(ok && out["ok"] == true, "{out}");
    let (ok, out) = mem_cli(&pm, &state, &["ls", "--project", "mem", "--json"]);
    assert!(ok);
    assert_eq!(out["memories"].as_array().unwrap().len(), 0, "{out}");
}

#[test]
fn memory_ui_lists_detail_and_refuses_memory_write() {
    let (_t, pm, state, _repo) = mem_fx();
    legacy_memory(
        &pm,
        "ui-mem",
        "rule",
        &["--scope-project"],
        "proposed",
        None,
    );
    let (port, _ui) = spawn_ui(&pm, &state);
    let host = format!("127.0.0.1:{port}");

    // List + filter.
    let (status, body) = http(port, "GET", "/api/memories", &host);
    assert_eq!(status, 200, "{body}");
    let list: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(list["memories"].as_array().unwrap().len(), 1);
    let (status, body) = http(port, "GET", "/api/memories?status=accepted", &host);
    assert_eq!(status, 200);
    let list: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(list["memories"].as_array().unwrap().len(), 0);

    // Detail.
    let (status, body) = http(port, "GET", "/api/memories/mem/ui-mem", &host);
    assert_eq!(status, 200, "{body}");
    let detail: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(detail["slug"], "ui-mem");
    assert!(detail["body"].as_str().unwrap().contains("**Why:**"));

    // Unguarded write refused (no X-Cadence-Board header).
    let (status, _, _) = http_write(
        port,
        "POST",
        "/api/memories/mem/ui-mem/accept",
        &host,
        &["Content-Type: application/json"],
        b"{}",
    );
    assert_eq!(status, 403);

    // The CSRF guard permits the request shape, but HTTP still cannot prove
    // the native PTY identity required for memory authority. The route must
    // refuse without changing the legacy record.
    let before = std::fs::read_to_string(pm.join("mem/memory/ui-mem.md")).unwrap();
    let (status, _, body) =
        write_json(port, "POST", "/api/memories/mem/ui-mem/accept", &host, "{}");
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("native agent endpoint"), "{body}");
    let text = std::fs::read_to_string(pm.join("mem/memory/ui-mem.md")).unwrap();
    assert_eq!(text, before);

    // Reject is equally authority-bearing and therefore equally refused.
    legacy_memory(
        &pm,
        "ui-no",
        "gotcha",
        &["--scope-project"],
        "proposed",
        None,
    );
    let before = std::fs::read_to_string(pm.join("mem/memory/ui-no.md")).unwrap();
    let (status, _, body) = write_json(port, "POST", "/api/memories/mem/ui-no/reject", &host, "{}");
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("native agent endpoint"), "{body}");
    assert_eq!(
        std::fs::read_to_string(pm.join("mem/memory/ui-no.md")).unwrap(),
        before
    );
}

/// CAD-395: the board's memory view reads a lesson's freshness through
/// the same `Freshness` retrieval uses, per the lesson's project window:
/// past-window → "unverified (last verified <date>)", stale-marked →
/// withheld with its reason, raw `verified_at` alone → unverified.
#[test]
fn memory_board_reads_retrieval_freshness() {
    let (_t, pm, state, _repo) = mem_fx();
    let now = time::iso(time::now_epoch());
    for slug in ["fresh", "aged", "marked", "raw-stamp"] {
        legacy_memory(
            &pm,
            slug,
            "rule",
            &["--scope-project"],
            "accepted",
            Some(&now),
        );
    }
    verify_fixture(&pm, "fresh", &now);
    verify_fixture(&pm, "aged", "2020-01-01T00:00:00Z");
    verify_fixture(&pm, "marked", &now);
    mark_stale_fixture(&pm, "marked", Some("M-9 reverted the cited fix"));
    let (port, _ui) = spawn_ui(&pm, &state);
    let host = format!("127.0.0.1:{port}");

    let (status, body) = http(port, "GET", "/api/memories", &host);
    assert_eq!(status, 200, "{body}");
    let list: Value = serde_json::from_str(&body).unwrap();
    let evidence = |slug: &str| -> Value {
        list["memories"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["slug"] == slug)
            .unwrap()["evidence"]
            .clone()
    };
    let fresh = evidence("fresh");
    assert_eq!(fresh["state"], "verified", "{fresh}");
    assert_eq!(fresh["label"], format!("verified {}", &now[..10]));
    let aged = evidence("aged");
    assert_eq!(aged["state"], "unverified", "{aged}");
    assert_eq!(aged["label"], "unverified (last verified 2020-01-01)");
    assert_eq!(aged["window_days"], 30);
    let marked = evidence("marked");
    assert_eq!(marked["state"], "withheld", "{marked}");
    assert_eq!(marked["label"], "withheld");
    assert_eq!(
        marked["reason"],
        "evidence marked stale: M-9 reverted the cited fix"
    );
    let raw = evidence("raw-stamp");
    assert_eq!(raw["state"], "unverified", "{raw}");
    assert_eq!(raw["label"], "unverified");
    assert_eq!(raw["last_verified"], Value::Null);

    // The detail and `memory ls --json` carry the same reading.
    let (status, body) = http(port, "GET", "/api/memories/mem/aged", &host);
    assert_eq!(status, 200, "{body}");
    let detail: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(detail["evidence"], aged);
    let (ok, out) = mem_cli(&pm, &state, &["ls", "--json"]);
    assert!(ok, "{out}");
    let card = out["memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["slug"] == "marked")
        .unwrap();
    assert_eq!(card["evidence"], marked);

    // The window is the lesson's project's.
    let yaml = pm.join("mem/project.yaml");
    let mut conf = std::fs::read_to_string(&yaml).unwrap();
    conf.push_str("memory:\n  stale_days: 100000\n");
    std::fs::write(&yaml, conf).unwrap();
    let (status, body) = http(port, "GET", "/api/memories/mem/aged", &host);
    assert_eq!(status, 200, "{body}");
    let detail: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(detail["evidence"]["label"], "verified 2020-01-01");
}

/// A memory file that fails to load does not sink the readers:
/// `memory ls` still lists the good ones, warns once on stderr and
/// reports `load_errors`; `/api/memories` carries `memory_errors`.
#[test]
fn memory_load_errors_surface_in_cli_and_api() {
    let (_t, pm, state, _repo) = mem_fx();
    legacy_memory(
        &pm,
        "good-mem",
        "rule",
        &["--scope-project"],
        "proposed",
        None,
    );
    std::fs::write(
        pm.join("mem/memory/broken.md"),
        "---\nid: [unclosed\n---\nbody\n",
    )
    .unwrap();

    // CLI: one good memory lists, stderr names the broken file.
    let (ok, out, err) = cli_out_err(&pm, &state, &["memory", "ls", "--project", "mem", "--json"]);
    assert!(ok, "{err}");
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["memories"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["load_errors"].as_array().unwrap().len(), 1, "{v}");
    assert!(err.contains("1 file(s) failed to load"), "{err}");
    assert!(err.contains("broken.md"), "{err}");

    // `ls --stale` takes the same path.
    let (ok, out, err) = cli_out_err(&pm, &state, &["memory", "ls", "--stale", "--json"]);
    assert!(ok, "{err}");
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["load_errors"].as_array().unwrap().len(), 1, "{v}");

    // API: same split — good payload, errors alongside.
    let (port, _ui) = spawn_ui(&pm, &state);
    let host = format!("127.0.0.1:{port}");
    let (status, body) = http(port, "GET", "/api/memories?project=mem", &host);
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["memories"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["memory_errors"].as_array().unwrap().len(), 1, "{v}");
    assert!(
        v["memory_errors"][0]
            .as_str()
            .unwrap()
            .contains("broken.md"),
        "{v}"
    );
}

/// An absent memory dir is a valid empty store; a path that exists
/// but cannot be enumerated (here: a file where the dir should be)
/// is an explicit load error — never silently empty.
#[test]
fn memory_absent_dir_empty_unreadable_dir_errors() {
    let (_t, pm, state, _repo) = mem_fx();

    // Absent: `mem` has no memory/ dir yet — clean empty, no errors.
    let (ok, out, err) = cli_out_err(&pm, &state, &["memory", "ls", "--project", "mem", "--json"]);
    assert!(ok, "{err}");
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["memories"].as_array().unwrap().len(), 0, "{v}");
    assert_eq!(v["load_errors"].as_array().unwrap().len(), 0, "{v}");

    // A file where the dir should be: read_dir fails ENOTDIR → error.
    std::fs::write(pm.join("mem/memory"), "not a dir").unwrap();
    let (ok, out, err) = cli_out_err(&pm, &state, &["memory", "ls", "--project", "mem", "--json"]);
    assert!(ok, "{err}");
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["memories"].as_array().unwrap().len(), 0, "{v}");
    let errs = v["load_errors"].as_array().unwrap();
    assert_eq!(errs.len(), 1, "{v}");
    assert!(errs[0].as_str().unwrap().contains("cannot list"), "{v}");
    assert!(err.contains("failed to load"), "{err}");

    // The API surfaces the same split — HTTP stays 200, the error is
    // in-band next to the (empty) records.
    let (port, _ui) = spawn_ui(&pm, &state);
    let host = format!("127.0.0.1:{port}");
    let (status, body) = http(port, "GET", "/api/memories?project=mem", &host);
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["memories"].as_array().unwrap().len(), 0, "{v}");
    assert!(
        v["memory_errors"][0]
            .as_str()
            .unwrap()
            .contains("cannot list"),
        "{v}"
    );
}

/// CAD-437: `memory ls` shares the grammar — repeatable any-of value
/// flags, AND across them (with component/path scope semantics kept),
/// unknown values error, sort/limit/fields tail.
#[test]
fn memory_ls_cad437_grammar() {
    let (_t, pm, state, _repo) = mem_fx();
    legacy_memory(
        &pm,
        "a-rule",
        "rule",
        &["--scope-project"],
        "accepted",
        None,
    );
    legacy_memory(
        &pm,
        "a-gotcha",
        "gotcha",
        &["--scope-component", "daemon"],
        "accepted",
        None,
    );
    legacy_memory(
        &pm,
        "old-rule",
        "rule",
        &["--scope-component", "other"],
        "superseded",
        None,
    );
    let slugs = |v: &Value| -> Vec<String> {
        let mut s: Vec<String> = v["memories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["slug"].as_str().unwrap().to_string())
            .collect();
        s.sort();
        s
    };

    // Any-of within a flag (repeat or comma-join); AND across flags.
    let (ok, out) = mem_cli(&pm, &state, &["ls", "--type", "rule,gotcha", "--json"]);
    assert!(ok, "{out}");
    assert_eq!(slugs(&out).len(), 3, "{out}");
    let (ok, out) = mem_cli(
        &pm,
        &state,
        &["ls", "--type", "rule", "--status", "accepted", "--json"],
    );
    assert!(ok, "{out}");
    assert_eq!(slugs(&out), ["a-rule"], "{out}");
    // Scoped axes keep their retrieval meaning: a component filter
    // still passes project-wide memories.
    let (ok, out) = mem_cli(&pm, &state, &["ls", "--component", "daemon", "--json"]);
    assert!(ok, "{out}");
    assert_eq!(slugs(&out), ["a-gotcha", "a-rule"], "{out}");
    let (ok, out) = mem_cli(
        &pm,
        &state,
        &[
            "ls",
            "--component",
            "daemon",
            "--status",
            "superseded",
            "--json",
        ],
    );
    assert!(ok, "{out}");
    assert_eq!(slugs(&out), Vec::<String>::new(), "{out}");

    // Unknown vocabularies error; so do unknown sorts and fields.
    let (ok, _, err) = cli_out_err(&pm, &state, &["memory", "ls", "--type", "zzz"]);
    assert!(!ok && err.contains("gotcha"), "{err}");
    let (ok, _, err) = cli_out_err(&pm, &state, &["memory", "ls", "--status", "zzz"]);
    assert!(!ok && err.contains("accepted"), "{err}");
    let (ok, _, err) = cli_out_err(&pm, &state, &["memory", "ls", "--sort", "zzz", "--json"]);
    assert!(!ok && err.contains("--sort"), "{err}");
    let (ok, _, err) = cli_out_err(&pm, &state, &["memory", "ls", "--fields", "id"]);
    assert!(!ok && err.contains("--json"), "{err}");

    // The tail: sort, limit, fields.
    let (ok, out) = mem_cli(
        &pm,
        &state,
        &["ls", "--sort", "-slug", "--limit", "1", "--json"],
    );
    assert!(ok, "{out}");
    assert_eq!(slugs(&out), ["old-rule"], "{out}");
    let (ok, out) = mem_cli(&pm, &state, &["ls", "--fields", "slug,type", "--json"]);
    assert!(ok, "{out}");
    let keys: Vec<&String> = out["memories"][0].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["slug", "type"], "{out}");
}

/// A hand-edited over-complex path glob bypasses write-time
/// validation — so load quarantines it with an error before matching
/// can ever run it. Valid siblings still list and match.
#[test]
fn memory_overcomplex_glob_quarantined_at_load() {
    let (_t, pm, state, _repo) = mem_fx();
    legacy_memory(
        &pm,
        "good-rule",
        "rule",
        &["--scope-project"],
        "accepted",
        Some("2026-01-01T00:00:00Z"),
    );
    // The evil file is accepted and project-scoped — it would match
    // everything; its hand-edited glob quarantines it instead. The valid
    // sibling remains readable, but both legacy records stay ineligible for
    // retrieval without native receipts.
    std::fs::write(
        pm.join("mem/memory/evil-glob.md"),
        "---\nid: evil-glob\ntype: rule\nstatus: accepted\nconfidence: medium\ncreated: 2026-01-01T00:00:00Z\nscope:\n  project: true\n  paths:\n    - \"**a**a**a**\"\n---\nfact\n\n**Why:** w\n\n**How to apply:** h\n",
    )
    .unwrap();

    let (ok, out) = mem_cli(&pm, &state, &["ls", "--project", "mem", "--json"]);
    assert!(ok, "{out}");
    let slugs: Vec<&str> = out["memories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, vec!["good-rule"], "{out}");
    let errs = out["load_errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(errs.contains("evil-glob"), "{errs}");
    assert!(errs.contains("too complex"), "{errs}");
    assert!(errs.contains("quarantined"), "{errs}");

    // Match — `evil-glob` never reaches glob_match. The valid legacy rule
    // is visible but blocked by retrieval proof, so the result is empty;
    // the test still proves the adversarial glob is quarantined before any
    // matcher can execute it.
    let (ok, out) = mem_cli(
        &pm,
        &state,
        &["match", "--project", "mem", "--path", "src/x.rs", "--json"],
    );
    assert!(ok, "{out}");
    let slugs: Vec<&str> = out["matched"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["slug"].as_str().unwrap())
        .collect();
    assert!(slugs.is_empty(), "{out}");
    assert!(
        out["load_errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e.as_str().unwrap().contains("evil-glob")),
        "{out}"
    );
}

/// Lint bounds the fact block in bytes, not just lines, and refuses
/// path scopes whose `**` recursion could backtrack exponentially.
#[test]
fn memory_lint_bounds_fact_bytes_and_glob() {
    let (_t, pm, state, _repo) = mem_fx();
    let dir = pm.join("mem/memory");
    std::fs::create_dir_all(&dir).unwrap();
    let fat = "x".repeat(600);
    std::fs::write(
        dir.join("fat-fact.md"),
        format!(
            "---\nid: fat-fact\ntype: rule\nstatus: proposed\nconfidence: medium\ncreated: 2026-01-01T00:00:00Z\n---\n{fat}\n\n**Why:** w\n\n**How to apply:** h\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("evil-glob.md"),
        "---\nid: evil-glob\ntype: rule\nstatus: proposed\nconfidence: medium\ncreated: 2026-01-01T00:00:00Z\nscope:\n  paths:\n    - \"**a**a**a**\"\n---\nfact\n\n**Why:** w\n\n**How to apply:** h\n",
    )
    .unwrap();

    let (ok, out) = mem_cli(&pm, &state, &["lint"]);
    assert!(!ok && out["ok"] == false, "{out}");
    let errs = out["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(errs.contains("fat-fact: fact is 600 bytes"), "{errs}");
    assert!(errs.contains("evil-glob"), "{errs}");
    assert!(errs.contains("too complex"), "{errs}");
}

/// A verify time that isn't ASCII — e.g. `abcé-01-01` — must not
/// panic the stale scan (the old slicer cut mid-char); it reads as not
/// current.
#[test]
fn memory_malformed_timestamp_is_safe() {
    let (_t, pm, state, _repo) = mem_fx();
    legacy_memory(
        &pm,
        "bad-date",
        "rule",
        &["--scope-project"],
        "accepted",
        Some("2026-01-01T00:00:00Z"),
    );
    verify_fixture(&pm, "bad-date", "abc\u{e9}-01-01");

    let (ok, out) = mem_cli(&pm, &state, &["ls", "--stale", "--json"]);
    assert!(ok, "{out}");
    let stale = out["stale"].as_array().unwrap();
    assert_eq!(stale.len(), 1, "{out}");
    assert_eq!(stale[0]["slug"], "bad-date");
    assert_eq!(
        stale[0]["reason"].as_str().unwrap(),
        "not verified within the window"
    );
}
