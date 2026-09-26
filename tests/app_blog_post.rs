//! CAD-548 — `apps/blog-post` is the first installable app; this test
//! is the ticket's documented demo, run end to end against a real
//! `cadence sandbox` (throwaway state, a board port in 3110-3199, the
//! production daemon and `~/pm` untouched):
//!
//!   install `apps/blog-post` → `app approve` → `plan propose
//!   --workflow blog-post/blog-post` → `plan approve` → the five
//!   workflow lanes dispatch in dependency order (Brief → Draft →
//!   Images → Review → Publish) → the Publish lane stages the
//!   `publish` send on the `publish` slot → the request sits waiting
//!   in Needs-you → the PM and the requesting agent are both refused
//!   the release → the operator presses → the post lands in the local
//!   outbox → the PM receives the verified outcome naming the board
//!   link.
//!
//! Every step is a real `cadence` command or daemon RPC; the operator
//! steps go through the suite's detached-operator helper — the same
//! shape an operator shell outside every pane presents (CAD-291,
//! CAD-384) — so the gate assertions stay honest even when this test
//! itself runs inside an agent pane. `cargo test --test app_blog_post
//! -- --nocapture` prints each command and its result: that output is
//! the transcript attached to the ticket.
#![allow(clippy::disallowed_methods)]

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use cadence_agent::{client, proto};
use serde_json::{json, Value};
use tempfile::TempDir;

use common::{operator_cadence_at, plant_member_pane, test_env, LaneShell, TestDaemon};

mod common;

/// The placeholder credential `local` enrolls — the adapter never
/// reads the bytes.
const TOKEN: &str = "cad548-demo-placeholder";

// ---------- the sandbox host ----------

/// A `cadence sandbox up` host: a tmp HOME/XDG the daemon and board
/// run under, the sandbox root fenced off by `CADENCE_SANDBOX_ROOT`,
/// the port pick honouring the suite's 3110-3199 lease dir. `Drop`
/// runs `sandbox down`.
struct Host {
    tmp: TempDir,
    name: Option<String>,
}

impl Host {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        for d in ["home", "xdg-state", "xdg-data", "sandboxes"] {
            std::fs::create_dir_all(tmp.path().join(d)).unwrap();
        }
        Self { tmp, name: None }
    }

    fn home(&self) -> PathBuf {
        self.tmp.path().join("home")
    }

    fn xdg_data(&self) -> PathBuf {
        self.tmp.path().join("xdg-data")
    }

    /// `cadence <args>` on the host, env-pointed at this sandbox's
    /// roots — mirrors `tests/sandbox.rs`'s `Host::run`.
    fn run(&self, args: &[&str]) -> Output {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
        cmd.args(args)
            .env("HOME", self.home())
            .env("XDG_STATE_HOME", self.tmp.path().join("xdg-state"))
            .env("XDG_DATA_HOME", self.xdg_data())
            .env("CADENCE_SANDBOX_ROOT", self.tmp.path().join("sandboxes"))
            .env(
                cadence_agent::sandbox::TEST_PORT_LOCK_DIR,
                "/tmp/cadence-test-ports",
            );
        for var in [
            "CADENCE_STATE_DIR",
            "CADENCE_PM_DIR",
            "CADENCE_PROFILE",
            "CADENCE_ALIAS",
            "CADENCE_ROLLOUT_AS",
            "CADENCE_SANDBOX_ALLOW_GLOBAL",
        ] {
            cmd.env_remove(var);
        }
        cmd.output().unwrap()
    }

    /// `sandbox up <name>` — no `--port`, so the sandbox picks a free
    /// port in 3110-3199 itself (the lease dir keeps parallel tests
    /// out of the pick).
    fn up(&mut self, name: &str) -> Value {
        say(&format!("$ cadence sandbox up {name}"));
        let out = self.run(&["sandbox", "up", name]);
        assert!(out.status.success(), "sandbox up: {}", text(&out));
        self.name = Some(name.to_string());
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        let port = v["port"].as_u64().unwrap() as u16;
        assert!(
            (3110..=3199).contains(&port),
            "sandbox port {port} outside 3110-3199"
        );
        say(&format!(
            "    sandbox {name} up: {}",
            v["url"].as_str().unwrap()
        ));
        v
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        if let Some(name) = self.name.take() {
            let _ = self.run(&["sandbox", "down", name.as_str()]);
        }
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// One transcript line — the demo's narration.
fn say(line: &str) {
    println!("{line}");
}

/// The operator-side `cadence <args>` — detached, env-cleared, proven
/// the operator however this test is reached; prints the command into
/// the transcript and asserts success.
fn op(home: &Path, state: &Path, args: &[&str]) -> String {
    say(&format!("$ (operator) cadence {}", args.join(" ")));
    let out = operator_cadence_at(home, state, args);
    assert!(
        out.status.success(),
        "operator cadence {}: {}",
        args.join(" "),
        text(&out)
    );
    let out = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.is_empty() {
        say(&indent(&out));
    }
    out
}

fn indent(s: &str) -> String {
    s.lines()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn banner(step: &str) {
    println!("\n=== {step} ===");
}

/// The outcome message the daemon delivers to the staged effect's
/// upstream — dedupe id `daemon_message_id("effect", effect_id)`.
fn outcome_message(state: &Path, effect_id: &str) -> Option<(String, String)> {
    let conn = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    conn.query_row(
        "SELECT alias, body FROM messages WHERE id=?1",
        rusqlite::params![proto::daemon_message_id("effect", effect_id)],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    )
    .ok()
}

/// One `agent_register` for a group member — `upstream` binds it to
/// the PM's group (dispatch's `--reply-to` check and the outcome's
/// route both read it).
fn register_member(d: &TestDaemon, alias: &str, upstream: &str, cwd: &Path) {
    d.operator_rpc(
        "agent_register",
        json!({"alias": alias, "provider": "inbox", "endpoint_kind": "inbox",
               "cwd": cwd.to_str().unwrap(),
               "params": json!({"upstream": upstream}).to_string()}),
    )
    .unwrap_or_else(|e| panic!("register {alias}: {e}"));
    say(&format!(
        "    agent {alias} registered (inbox, upstream {upstream})"
    ));
}

/// The git demo repo the project points at: one commit on `main`, a
/// local user for the lane commits.
fn init_repo(home: &Path) -> PathBuf {
    let repo = home.join("site");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "cad548-demo"],
        vec!["config", "user.email", "demo@localhost"],
    ] {
        let out = std::process::Command::new("git")
            .args(&args)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {}: {}",
            args.join(" "),
            text(&out)
        );
    }
    std::fs::write(repo.join("README.md"), "# site\n").unwrap();
    let out = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(out.status.success());
    let out = std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "git commit: {}", text(&out));
    repo
}

/// Merge a ticket branch into the demo repo's main — what the PR merge
/// does in the real flow. Runs as the operator's git; the sandbox has
/// no `gh`, so the merge is local.
fn merge(home: &Path, repo: &Path, branch: &str, id: &str) {
    say(&format!(
        "$ (operator) git merge {branch}   # the merged PR"
    ));
    let out = std::process::Command::new("git")
        .args(["merge", "--no-edit", branch])
        .current_dir(repo)
        .env("HOME", home)
        .output()
        .unwrap();
    assert!(out.status.success(), "merge {id}: {}", text(&out));
}

/// The run of one workflow ticket: dispatch to `agent`, let its lane
/// run `work` inside the fresh worktree, file the done report, merge
/// into the project's repo, close. Returns `(worktree, branch)`.
fn run_ticket(
    host: &Host,
    d: &TestDaemon,
    repo: &Path,
    lane: &mut LaneShell,
    agent: &str,
    id: &str,
    work: &str,
) -> (PathBuf, String) {
    let out = op(
        &host.home(),
        &d.state,
        &["dispatch", id, "--to", agent, "--reply-to", "pm"],
    );
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["dispatched"], json!(true), "{v}");
    let wt = PathBuf::from(v["worktree"].as_str().unwrap());
    let branch = v["branch"].as_str().unwrap().to_string();
    say(&format!(
        "    {id} -> {agent}: worktree {}, branch {branch}",
        wt.display()
    ));

    // The lane's persistent shell lives in the ticket's worktree —
    // `cd` first or `git add`/scripts land in whatever cwd the lane
    // inherited.
    let (rc, out) = lane.run(&format!("cd {}", wt.display()));
    assert_eq!(rc, 0, "cd {}: {out}", wt.display());
    lane_work(lane, &wt, id, work);

    let report = wt.join("report.md");
    write_report(&report, id, agent);
    let (rc, out) = lane.run(&format!(
        "git add -A && git commit -qm '{id}: {agent} work'"
    ));
    assert_eq!(rc, 0, "{agent} commit on {id}: {out}");
    let (rc, out) = lane.cadence(
        &d.state,
        &format!(
            "report file --task {id} --kind done --file {}",
            report.display()
        ),
    );
    assert_eq!(rc, 0, "{agent} report file {id}: {out}");
    say(&format!("    {agent} filed the done report on {id}"));

    merge(&host.home(), repo, &branch, id);
    op(&host.home(), &d.state, &["issue", "set", id, "status=done"]);
    (wt, branch)
}

// ---------- the demo ----------

/// The operator-side read of one `platform_effects` row by request id.
fn effect_row(d: &TestDaemon, request: &str) -> Option<Value> {
    let out = d
        .operator_rpc("platform_effects", json!({}))
        .expect("operator platform_effects");
    out["effects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["request"] == request)
        .cloned()
}

/// A `done` report body — the schema requires the six reflection
/// headings.
fn write_report(path: &Path, id: &str, agent: &str) {
    std::fs::write(
        path,
        format!(
            "# {id} done\n\n\
             ## Expected\n\n{id} produces its artifact.\n\n\
             ## Evidence\n\n{agent} committed it on the ticket branch.\n\n\
             ## Cause\n\nThe workflow step ran.\n\n\
             ## Correction\n\nNone needed.\n\n\
             ## Lesson\n\nTemplate + rubric keep the lane small.\n\n\
             ## Next\n\nThe dependent ticket unblocks.\n"
        ),
    )
    .unwrap();
}

/// Write `script` into `lane`'s scratch dir and run it under the lane
/// with the worktree as argv[1] — one `bash file` line on the lane's
/// stdin, so heredocs in the script stay well-formed.
fn lane_work(lane: &mut LaneShell, wt: &Path, id: &str, script: &str) {
    let file = lane.dir.path().join(format!("work-{id}.sh"));
    std::fs::write(&file, format!("set -euo pipefail\ncd \"$1\"\n{script}\n")).unwrap();
    let (rc, out) = lane.run(&format!("bash {} {}", file.display(), wt.display()));
    assert_eq!(rc, 0, "{id} lane work: {out}");
    if !out.trim().is_empty() {
        say(&indent(out.trim()));
    }
}

#[test]
fn blog_post_app_demo() {
    common::suite_slot();
    // The pm repo's `pre-commit` hook runs `cadence issue lint` from
    // PATH — `sandbox up`'s daemon inherits this process's env, so the
    // binary under test must be on PATH before `up` spawns it (the
    // installed release lints `apps/` as an issue dir and refuses the
    // commit).
    common::hook_bin_on_path();
    let slug = "ship-safely";
    let topic = "staging-safe deploys";
    let request = "cad548-publish";

    banner("0. sandbox up (temp state, port 3110-3199)");
    let mut host = Host::new();
    let v = host.up("blog548");
    let state = PathBuf::from(v["state_dir"].as_str().unwrap());
    let pm_dir = PathBuf::from(v["pm_dir"].as_str().unwrap());
    let board = v["url"].as_str().unwrap().to_string();
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let d = TestDaemon {
        dir: TempDir::new().unwrap(),
        state: state.clone(),
        handle: None,
        process: None,
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(2)).is_ok() {
        assert!(std::time::Instant::now() < deadline, "daemon never healthy");
        std::thread::sleep(Duration::from_millis(100));
    }
    say(&format!("    daemon healthy on {board}, state {state:?}"));

    banner("1. project + repo");
    let repo = init_repo(&host.home());
    let repo_s = repo.to_str().unwrap().to_string();
    op(
        &host.home(),
        &state,
        &[
            "issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s,
        ],
    );

    banner("2. install + approve apps/blog-post");
    let app_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("apps/blog-post");
    let app_src_s = app_src.to_str().unwrap().to_string();
    op(
        &host.home(),
        &state,
        &[
            "workflow",
            "check",
            &app_src.join("workflows/blog-post.md").to_string_lossy(),
        ],
    );
    op(
        &host.home(),
        &state,
        &["app", "install", &app_src_s, "--project", "demo"],
    );
    op(
        &host.home(),
        &state,
        &["app", "show", "blog-post", "--project", "demo"],
    );
    op(
        &host.home(),
        &state,
        &["app", "approve", "blog-post", "--project", "demo"],
    );

    banner("3. agents: pm + the five lanes");
    let lanes_home = TempDir::new().unwrap();
    let mut pm_lane = LaneShell::spawn(lanes_home.path());
    plant_member_pane(&d, "pm", "inbox", None, pm_lane.pid());
    for a in ["strat", "draft", "img", "rev"] {
        register_member(&d, a, "pm", &repo);
    }
    // `pub` needs pane-bound identity for `platform_call`: a planted
    // pty lane like the suite's CAD-149 fixtures, upstream `pm`.
    let mut pub_lane = LaneShell::spawn(lanes_home.path());
    plant_member_pane(&d, "pub", "claude", Some("pm"), pub_lane.pid());
    say("    agent pub registered (pty lane, upstream pm)");
    let mut strat = LaneShell::spawn(lanes_home.path());
    let mut draft = LaneShell::spawn(lanes_home.path());
    let mut img = LaneShell::spawn(lanes_home.path());
    let mut rev = LaneShell::spawn(lanes_home.path());
    for (lane, alias) in [
        (&mut strat, "strat"),
        (&mut draft, "draft"),
        (&mut img, "img"),
        (&mut rev, "rev"),
        (&mut pub_lane, "pub"),
    ] {
        let (rc, out) = lane.run(&format!("export CADENCE_ALIAS={alias}"));
        assert_eq!(rc, 0, "{out}");
    }
    op(&host.home(), &state, &["agent", "list"]);

    banner("4. publish slot: enroll local/outbox, grant pub");
    let out = d
        .operator_rpc(
            "platform_enroll",
            json!({"accept_same_uid_risk": true, "platform": "local",
                   "account": "outbox", "scopes": ["publish"], "shape": "token",
                   "token": TOKEN}),
        )
        .unwrap_or_else(|e| panic!("enroll: {e}"));
    say(&format!(
        "    platform_enroll local/outbox: {}",
        indent(&serde_json::to_string(&out).unwrap())
    ));
    d.operator_rpc(
        "platform_grant",
        json!({"agent": "pub", "platform": "local",
               "account": "outbox", "scopes": ["publish"]}),
    )
    .unwrap_or_else(|e| panic!("grant: {e}"));
    say("    platform_grant pub local/outbox publish");

    banner("5. propose + approve the run");
    let out = op(
        &host.home(),
        &state,
        &[
            "plan",
            "propose",
            "--project",
            "demo",
            "--workflow",
            "blog-post/blog-post",
            "--input",
            &format!("topic={topic}"),
            "--input",
            &format!("slug={slug}"),
            "--input",
            "keyword=deploy safety",
            "--input",
            "strategist=strat",
            "--input",
            "writer=draft",
            "--input",
            "designer=img",
            "--input",
            "reviewer=rev",
            "--input",
            "publisher=pub",
        ],
    );
    let v: Value = serde_json::from_str(&out).unwrap();
    let epic = v["epic"].as_str().unwrap().to_string();
    let tickets: Vec<String> = v["tickets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect();
    assert_eq!(tickets.len(), 5, "{v}");
    say(&format!("    plan {epic}: tickets {tickets:?}"));
    op(&host.home(), &state, &["plan", "approve", &epic]);
    op(&host.home(), &state, &["plan", "show", &epic]);

    banner("6. Brief -> Draft -> Images -> Review");
    run_ticket(
        &host,
        &d,
        &repo,
        &mut strat,
        "strat",
        &tickets[0],
        &format!(
            "mkdir -p posts/{slug} && cat > posts/{slug}/brief.md <<'EOF'\n\
             # Brief: {topic}\n\n\
             audience: platform engineers\n\
             angle: deploys that cannot touch production until staging proves them\n\
             keywords: deploy safety, staging parity, release gates\n\n\
             sources:\n\
             - https://example.com/deploy-gates\n\
             - https://example.com/staging-parity\n\
             - https://example.com/release-checklists\nEOF"
        ),
    );
    run_ticket(
        &host,
        &d,
        &repo,
        &mut draft,
        "draft",
        &tickets[1],
        &format!(
            "cat > posts/{slug}/post.md <<'EOF'\n\
             # {topic}\n\n\
             A deploy only earns production after staging has proved it. The\n\
             release gate is the contract: staging evidence first, the publish\n\
             second. Posts that name numbers cite the brief's sources.\n\n\
             ![hero](images/hero.svg)\nEOF"
        ),
    );
    run_ticket(
        &host, &d, &repo, &mut img, "img", &tickets[2],
        &format!(
            "mkdir -p posts/{slug}/images && \
             printf '%s' '<svg xmlns=\"http://www.w3.org/2000/svg\"/>' > posts/{slug}/images/hero.svg && \
             printf '%s' '<svg xmlns=\"http://www.w3.org/2000/svg\"/>' > posts/{slug}/images/social.svg"
        ),
    );
    run_ticket(
        &host,
        &d,
        &repo,
        &mut rev,
        "rev",
        &tickets[3],
        &format!(
            "cat > posts/{slug}/review.md <<'EOF'\n\
             # Review: {topic}\n\n\
             rubric: rubrics/blog.md\n\
             verdict: PASS\n\n\
             The draft follows the brief, claims cite the listed sources, and\n\
             both images carry alt text. Reviewed by an agent that neither\n\
             wrote nor illustrated the post.\nEOF"
        ),
    );

    banner("7. Publish: stage the send on the publish slot");
    // Dispatch checks a pty worker's pane cwd sits inside the issue's
    // project repo (CAD-202) — home the lane in the demo repo first.
    let (rc, out) = pub_lane.run(&format!("cd {}", repo.display()));
    assert_eq!(rc, 0, "cd {}: {out}", repo.display());
    let out = op(
        &host.home(),
        &d.state,
        &["dispatch", &tickets[4], "--to", "pub", "--reply-to", "pm"],
    );
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["dispatched"], json!(true), "{v}");
    let pub_wt = PathBuf::from(v["worktree"].as_str().unwrap());
    let pub_branch = v["branch"].as_str().unwrap().to_string();
    say(&format!(
        "    {} -> pub: worktree {}, branch {pub_branch}",
        tickets[4],
        pub_wt.display()
    ));
    // Attachment confinement reads the requesting agent's registered
    // `cwd`: pin pub's row at its ticket worktree (the same fields
    // `plant_member_pane` writes).
    let conn = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE agents SET cwd=?1 WHERE alias='pub'",
        rusqlite::params![pub_wt.to_str().unwrap()],
    )
    .unwrap();
    let (rc, out) = pub_lane.run(&format!("cd {}", pub_wt.display()));
    assert_eq!(rc, 0, "cd {}: {out}", pub_wt.display());
    lane_work(
        &mut pub_lane,
        &pub_wt,
        &tickets[4],
        &format!(
            "grep -q 'verdict: PASS' posts/{slug}/review.md && echo 'review verdict: PASS'\n\
             ls posts/{slug} posts/{slug}/images"
        ),
    );
    let frame = pub_lane.rpc(
        &state,
        "platform_call",
        json!({"platform": "local", "account": "outbox", "tool": "publish",
               "input": {"project": "demo",
                         "title": topic,
                         "body": "A deploy only earns production after staging has proved it.\n",
                         "attachments": [format!("posts/{slug}/post.md"),
                                         format!("posts/{slug}/images/hero.svg"),
                                         format!("posts/{slug}/images/social.svg")]},
               "request": request}),
    );
    assert_eq!(frame["ok"], json!(true), "stage refused: {frame}");
    let eid = frame["result"]["effect_id"].as_str().unwrap().to_string();
    assert_eq!(frame["result"]["result"], json!("staged"), "{frame}");
    say(&format!("    pub staged the publish send — effect {eid}"));

    banner("8. Needs-you holds the staged send; the gate refuses non-operators");
    let row = effect_row(&d, request).expect("staged row");
    assert_eq!(row["state"], json!("waiting"), "{row}");
    assert_eq!(row["effect"], json!("send"), "{row}");
    assert!(row["preview"].as_str().unwrap().contains(topic), "{row}");
    say("    effect row: state=waiting effect=send agent=pub");
    let overview = common::overview_at(&host.home(), &state, Some(&pm_dir), &[]);
    let needs = serde_json::to_string(&overview["needs_me"]).unwrap();
    assert!(
        needs.contains(request) || needs.contains(&eid),
        "Needs-you lacks the staged send: {needs}"
    );
    say(&format!("    Needs-you row: {needs}"));

    let frame = pm_lane.rpc(
        &state,
        "agent_respond",
        json!({"alias": "pub", "request": request, "decision": "accept"}),
    );
    assert_eq!(frame["ok"], json!(false), "PM release admitted: {frame}");
    say(&format!("    pm press refused: {}", frame["error"]));
    let frame = pub_lane.rpc(
        &state,
        "agent_respond",
        json!({"alias": "pub", "request": request, "decision": "accept"}),
    );
    assert_eq!(
        frame["ok"],
        json!(false),
        "requester release admitted: {frame}"
    );
    say(&format!("    pub press refused: {}", frame["error"]));

    banner("9. operator releases; the post lands in the outbox");
    let out = d
        .operator_rpc(
            "agent_respond",
            json!({"alias": "pub", "request": request, "decision": "accept"}),
        )
        .unwrap_or_else(|e| panic!("release: {e}"));
    let row = out["effect"].clone();
    assert_eq!(row["state"], json!("done"), "{row}");
    assert_eq!(row["outcome"]["verified"], json!(true), "{row}");
    say("    released: state=done verified=true");

    let outbox = host.xdg_data().join("cadence").join("outbox");
    let item = outbox.join("demo").join(&eid);
    let post = std::fs::read_to_string(item.join("post.md")).unwrap();
    assert!(post.contains(topic), "{post}");
    assert!(item.join("attachments/post.md").exists(), "post attachment");
    assert!(
        item.join("attachments/hero.svg").exists(),
        "hero attachment"
    );
    let index: Value =
        serde_json::from_str(&std::fs::read_to_string(item.join("index.json")).unwrap()).unwrap();
    assert_eq!(index["effect_id"], json!(eid));
    assert_eq!(index["project"], json!("demo"));
    assert_eq!(
        index["result"]["board_url"].as_str().unwrap(),
        format!("{board}/outbox?item={eid}")
    );
    say(&format!("    outbox item: {}", item.display()));

    banner("10. the PM receives the verified outcome");
    let (alias, body) = outcome_message(&state, &eid).expect("outcome message to pm");
    assert_eq!(alias, "pm");
    assert!(body.contains("done"), "{body}");
    assert!(
        body.contains(&format!("board: {board}/outbox?item={eid}")),
        "{body}"
    );
    say(&format!("    pm inbox: {}", indent(body.trim())));

    // The publish ticket closes the plan.
    let report = pub_wt.join("report.md");
    write_report(&report, &tickets[4], "pub");
    let (rc, out) = pub_lane.run("git add -A && git commit -qm 'publish staged'");
    assert_eq!(rc, 0, "{out}");
    let (rc, out) = pub_lane.cadence(
        &state,
        &format!(
            "report file --task {} --kind done --file {}",
            tickets[4],
            report.display()
        ),
    );
    assert_eq!(rc, 0, "{out}");
    merge(&host.home(), &repo, &pub_branch, &tickets[4]);
    op(
        &host.home(),
        &state,
        &["issue", "set", &tickets[4], "status=done"],
    );
    op(&host.home(), &state, &["plan", "show", &epic]);
    op(&host.home(), &state, &["platform", "outbox"]);

    say("\n    demo complete: install -> approve -> run -> staged -> released -> outbox -> pm");
}

// ---------- CAD-577: installed means ready to run ----------

/// CAD-577's own demo, from a **clean** state: no CLI setup beyond the
/// install, no hand-enrolled credential, no hand-made grant, no agent
/// inputs typed one by one. The operator's board path is:
///
///   install `apps/blog-post` → approve → set the app's default team →
///   New post with only a topic → the plan waits for approval →
///   approve → the Publish step stages on the built-in `local` account
///   the approval derived a grant for → release in Needs-you → the
///   outbox item lands.
///
/// Every step is a real `cadence` command or daemon RPC; the operator
/// steps go through the suite's detached-operator helper. The team is
/// set through the board's `app_set_team` relay (`app set-team` on the
/// CLI is the same RPC), and the propose names only `topic` and `slug`
/// — the saved team fills the five agent roles, which is what makes a
/// fresh install runnable with no CLI.
#[test]
fn blog_post_installed_is_ready_to_run() {
    common::suite_slot();
    common::hook_bin_on_path();
    let topic = "installed means ready";
    let slug = "installed-means-ready";
    let request = "cad577-publish";

    banner("0. sandbox up (temp state, port 3110-3199)");
    let mut host = Host::new();
    let v = host.up("blog577");
    let state = PathBuf::from(v["state_dir"].as_str().unwrap());
    let pm_dir = PathBuf::from(v["pm_dir"].as_str().unwrap());
    let board = v["url"].as_str().unwrap().to_string();
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let d = TestDaemon {
        dir: TempDir::new().unwrap(),
        state: state.clone(),
        handle: None,
        process: None,
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(2)).is_ok() {
        assert!(std::time::Instant::now() < deadline, "daemon never healthy");
        std::thread::sleep(Duration::from_millis(100));
    }
    say(&format!("    daemon healthy on {board}, state {state:?}"));

    banner("1. project + repo");
    let repo = init_repo(&host.home());
    let repo_s = repo.to_str().unwrap().to_string();
    op(
        &host.home(),
        &state,
        &[
            "issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s,
        ],
    );

    banner("2. install + approve apps/blog-post (no enroll, no grant)");
    let app_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("apps/blog-post");
    let app_src_s = app_src.to_str().unwrap().to_string();
    op(
        &host.home(),
        &state,
        &["app", "install", &app_src_s, "--project", "demo"],
    );
    op(
        &host.home(),
        &state,
        &["app", "approve", "blog-post", "--project", "demo"],
    );
    // The built-in account is always listed — no enrollment happened.
    let accounts = d.operator_rpc("platform_accounts", json!({})).unwrap();
    let builtin = accounts["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["platform"] == "local" && a["account"] == "local")
        .cloned()
        .expect("built-in local/local listed with no enrollment");
    assert_eq!(builtin["custody"], "built-in", "{builtin}");
    say("    platform accounts lists local/local as built-in");

    banner("3. agents: pm + the five lanes the team will name");
    let lanes_home = TempDir::new().unwrap();
    let pm_lane = LaneShell::spawn(lanes_home.path());
    plant_member_pane(&d, "pm", "inbox", None, pm_lane.pid());
    for a in ["strat", "draft", "img", "rev"] {
        register_member(&d, a, "pm", &repo);
    }
    let mut pub_lane = LaneShell::spawn(lanes_home.path());
    plant_member_pane(&d, "pub", "claude", Some("pm"), pub_lane.pid());
    let mut strat = LaneShell::spawn(lanes_home.path());
    let mut draft = LaneShell::spawn(lanes_home.path());
    let mut img = LaneShell::spawn(lanes_home.path());
    let mut rev = LaneShell::spawn(lanes_home.path());
    for (lane, alias) in [
        (&mut strat, "strat"),
        (&mut draft, "draft"),
        (&mut img, "img"),
        (&mut rev, "rev"),
        (&mut pub_lane, "pub"),
    ] {
        let (rc, out) = lane.run(&format!("export CADENCE_ALIAS={alias}"));
        assert_eq!(rc, 0, "{out}");
    }
    op(&host.home(), &state, &["agent", "list"]);

    banner("4. the app's default team (the operator's board write)");
    let out = op(
        &host.home(),
        &state,
        &[
            "app",
            "set-team",
            "blog-post",
            "--project",
            "demo",
            "--role",
            "strategist=strat",
            "--role",
            "writer=draft",
            "--role",
            "designer=img",
            "--role",
            "reviewer=rev",
            "--role",
            "publisher=pub",
        ],
    );
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["team"]["publisher"], json!("pub"), "{v}");
    say("    app set-team: five roles saved with the install record");

    banner("5. New post with only a topic — the team fills the rest");
    let out = op(
        &host.home(),
        &state,
        &[
            "plan",
            "propose",
            "--project",
            "demo",
            "--workflow",
            "blog-post/blog-post",
            "--input",
            &format!("topic={topic}"),
            "--input",
            &format!("slug={slug}"),
        ],
    );
    let v: Value = serde_json::from_str(&out).unwrap();
    let epic = v["epic"].as_str().unwrap().to_string();
    let tickets: Vec<String> = v["tickets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect();
    assert_eq!(tickets.len(), 5, "{v}");
    // The saved team reached the rendered plan: each ticket's owner is
    // the agent the operator picked, not a missing-input refusal.
    for (id, who) in [
        (&tickets[0], "strat"),
        (&tickets[1], "draft"),
        (&tickets[2], "img"),
        (&tickets[3], "rev"),
        (&tickets[4], "pub"),
    ] {
        let show = op(&host.home(), &state, &["issue", "show", id]);
        assert!(show.contains(who), "{id} owner should be {who}: {show}");
    }
    say(&format!(
        "    plan {epic}: tickets {tickets:?} (owners from the saved team)"
    ));
    // The approval derived the Publish step's grant on the built-in
    // account — no hand-made grant anywhere.
    let grants = d
        .operator_rpc("platform_grants", json!({"agent": "pub"}))
        .unwrap();
    let g = grants["grants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["platform"] == "local" && g["account"] == "local")
        .cloned()
        .expect("approval derived pub's local/local grant");
    assert_eq!(g["scopes"], json!(["publish"]), "{g}");
    say("    app approval derived pub's grant: local/local publish");

    banner("6. approve the plan; the run's stopped team agents resume");
    op(&host.home(), &state, &["plan", "approve", &epic]);
    op(&host.home(), &state, &["plan", "show", &epic]);

    banner("7. Brief -> Draft -> Images -> Review");
    run_ticket(
        &host,
        &d,
        &repo,
        &mut strat,
        "strat",
        &tickets[0],
        &format!(
            "mkdir -p posts/{slug} && cat > posts/{slug}/brief.md <<'EOF'\n\
             # Brief: {topic}\n\n\
             audience: app operators\n\
             angle: an install runs from the board with no CLI\n\
             keywords: ready to run, default team, built-in outbox\n\n\
             sources:\n\
             - https://example.com/ready-to-run\n\
             - https://example.com/default-team\n\
             - https://example.com/builtin-outbox\nEOF"
        ),
    );
    run_ticket(
        &host,
        &d,
        &repo,
        &mut draft,
        "draft",
        &tickets[1],
        &format!(
            "cat > posts/{slug}/post.md <<'EOF'\n\
             # {topic}\n\n\
             An install is ready to run when the board can start it: the\n\
             operator approves, sets the team once, and types a topic. The\n\
             built-in outbox needs no enrollment, and the approval derives\n\
             exactly the scope the Publish step declared.\n\n\
             ![hero](images/hero.svg)\nEOF"
        ),
    );
    run_ticket(
        &host, &d, &repo, &mut img, "img", &tickets[2],
        &format!(
            "mkdir -p posts/{slug}/images && \
             printf '%s' '<svg xmlns=\"http://www.w3.org/2000/svg\"/>' > posts/{slug}/images/hero.svg && \
             printf '%s' '<svg xmlns=\"http://www.w3.org/2000/svg\"/>' > posts/{slug}/images/social.svg"
        ),
    );
    run_ticket(
        &host,
        &d,
        &repo,
        &mut rev,
        "rev",
        &tickets[3],
        &format!(
            "cat > posts/{slug}/review.md <<'EOF'\n\
             # Review: {topic}\n\n\
             rubric: rubrics/blog.md\n\
             verdict: PASS\n\n\
             The draft follows the brief, claims cite the listed sources, and\n\
             both images carry alt text. Reviewed by an agent that neither\n\
             wrote nor illustrated the post.\nEOF"
        ),
    );

    banner("8. Publish: stage on the built-in account the grant covers");
    let (rc, out) = pub_lane.run(&format!("cd {}", repo.display()));
    assert_eq!(rc, 0, "cd {}: {out}", repo.display());
    let out = op(
        &host.home(),
        &d.state,
        &["dispatch", &tickets[4], "--to", "pub", "--reply-to", "pm"],
    );
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["dispatched"], json!(true), "{v}");
    let pub_wt = PathBuf::from(v["worktree"].as_str().unwrap());
    let pub_branch = v["branch"].as_str().unwrap().to_string();
    say(&format!(
        "    {} -> pub: worktree {}, branch {pub_branch}",
        tickets[4],
        pub_wt.display()
    ));
    let conn = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE agents SET cwd=?1 WHERE alias='pub'",
        rusqlite::params![pub_wt.to_str().unwrap()],
    )
    .unwrap();
    let (rc, out) = pub_lane.run(&format!("cd {}", pub_wt.display()));
    assert_eq!(rc, 0, "cd {}: {out}", pub_wt.display());
    lane_work(
        &mut pub_lane,
        &pub_wt,
        &tickets[4],
        &format!(
            "grep -q 'verdict: PASS' posts/{slug}/review.md && echo 'review verdict: PASS'\n\
             ls posts/{slug} posts/{slug}/images"
        ),
    );
    // The workflow names the built-in account (`local`); no account is
    // passed, so the call resolves the default and the derived grant
    // covers it.
    let frame = pub_lane.rpc(
        &state,
        "platform_call",
        json!({"platform": "local", "tool": "publish",
               "input": {"project": "demo",
                         "title": topic,
                         "body": "An install is ready to run when the board can start it.\n",
                         "attachments": [format!("posts/{slug}/post.md"),
                                         format!("posts/{slug}/images/hero.svg"),
                                         format!("posts/{slug}/images/social.svg")]},
               "request": request}),
    );
    assert_eq!(frame["ok"], json!(true), "stage refused: {frame}");
    let eid = frame["result"]["effect_id"].as_str().unwrap().to_string();
    assert_eq!(frame["result"]["result"], json!("staged"), "{frame}");
    say(&format!("    pub staged the publish send — effect {eid}"));

    banner("9. Needs-you holds the staged send; release lands the item");
    let row = effect_row(&d, request).expect("staged row");
    assert_eq!(row["state"], json!("waiting"), "{row}");
    let overview = common::overview_at(&host.home(), &state, Some(&pm_dir), &[]);
    let needs = serde_json::to_string(&overview["needs_me"]).unwrap();
    assert!(
        needs.contains(request) || needs.contains(&eid),
        "Needs-you lacks the staged send: {needs}"
    );
    let out = d
        .operator_rpc(
            "agent_respond",
            json!({"alias": "pub", "request": request, "decision": "accept"}),
        )
        .unwrap_or_else(|e| panic!("release: {e}"));
    let row = out["effect"].clone();
    assert_eq!(row["state"], json!("done"), "{row}");
    assert_eq!(row["outcome"]["verified"], json!(true), "{row}");

    let outbox = host.xdg_data().join("cadence").join("outbox");
    let item = outbox.join("demo").join(&eid);
    let post = std::fs::read_to_string(item.join("post.md")).unwrap();
    assert!(post.contains(topic), "{post}");
    assert!(
        item.join("attachments/hero.svg").exists(),
        "hero attachment"
    );
    say(&format!("    outbox item: {}", item.display()));

    let (alias, body) = outcome_message(&state, &eid).expect("outcome message to pm");
    assert_eq!(alias, "pm");
    assert!(body.contains("done"), "{body}");
    say(&format!("    pm inbox: {}", indent(body.trim())));

    say(
        "\n    clean-state demo complete: install -> approve -> set team -> topic -> \
          approve -> staged -> released -> outbox",
    );
}
