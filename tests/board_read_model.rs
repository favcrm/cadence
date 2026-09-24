//! CAD-325: the board read model under a tracker the size of the live
//! one. A fixture of 420 issues (containers, blockers, comments, tagged
//! notes) plus an in-process daemon carrying agents, issue-bound jobs and
//! message history; `/api/overview`, `/api/issues` and `/api/agents` must
//! answer with p95 under 500 ms while board streams are connected, and a
//! tracker write through `cadence issue …` must show on the next request.

// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use cadence_agent::{client, daemon, ui};
use serde_json::{json, Value};
use tempfile::TempDir;

const ISSUES: usize = 420;
const AGENTS: usize = 12;
const JOBS: usize = 160;
const SAMPLES: usize = 20;
const P95_BUDGET: Duration = Duration::from_millis(500);

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_cadence")
}

fn cli(pm: &Path, state: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(bin())
        .arg("--state-dir")
        .arg(state)
        .args(args)
        .env("CADENCE_PM_DIR", pm)
        // The tracker's hooks run `cadence` from PATH — the build under test.
        .env(
            "PATH",
            format!(
                "{}:{}",
                Path::new(bin()).parent().unwrap().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env_remove("CADENCE_ALIAS")
        .output()
        .unwrap();
    let text = if out.stdout.is_empty() {
        String::from_utf8_lossy(&out.stderr).to_string()
    } else {
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    (out.status.success(), text)
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn get(port: u16, path: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(120))).unwrap();
    write!(s, "GET {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n").unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let buf = String::from_utf8_lossy(&raw).to_string();
    let status = buf
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = buf.split_once("\r\n\r\n").map_or("", |x| x.1).to_string();
    (status, body)
}

fn get_json(port: u16, path: &str) -> Value {
    let (code, body) = get(port, path);
    assert_eq!(code, 200, "{path}: {body}");
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("{path}: {e}: {body}"))
}

/// The in-process daemon — `daemon::serve` on a caller-owned state dir.
struct Daemon {
    state: PathBuf,
    handle: Option<thread::JoinHandle<()>>,
}

impl Daemon {
    fn start(state: PathBuf) -> Self {
        let owned = state.clone();
        let handle = thread::spawn(move || {
            let _ = daemon::serve(&owned);
        });
        let d = Self {
            state,
            handle: Some(handle),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while client::rpc(&d.state, "health", json!({})).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become healthy");
            thread::sleep(Duration::from_millis(50));
        }
        d
    }

    fn rpc(&self, method: &str, params: Value) -> Value {
        client::rpc(&self.state, method, params.clone())
            .unwrap_or_else(|e| panic!("{method} {params}: {e}"))
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = client::rpc(&self.state, "shutdown", json!({}));
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn start_ui(pm: &Path, state: &Path) -> u16 {
    let overall = Instant::now() + Duration::from_secs(30);
    loop {
        let port = free_port();
        let (sd, pd) = (state.to_path_buf(), pm.to_path_buf());
        thread::spawn(move || {
            let opts = ui::ServeOpts {
                host: "127.0.0.1".to_string(),
                port,
                ..Default::default()
            };
            let _ = ui::serve(&sd, &pd, &opts);
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) {
                s.set_read_timeout(Some(Duration::from_secs(30))).ok();
                let probe = format!("GET /api/meta HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n");
                let _ = s.write_all(probe.as_bytes());
                let mut buf = String::new();
                if s.read_to_string(&mut buf).is_ok() && buf.contains("200") {
                    return port;
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            assert!(Instant::now() < overall, "ui server did not start");
            thread::sleep(Duration::from_millis(50));
        }
    }
}

/// A tracker the size of the live one, written straight to disk and
/// committed once — 420 `issue new` round trips would dominate the run.
/// Every 10th issue is an epic over the next nine; every 7th is blocked
/// by its predecessor; a third carry a comment; notes tag ~150 issues.
fn seed_tracker(pm: &Path, state: &Path, notes: &Path, count: usize) {
    assert!(cli(pm, state, &["issue", "init"]).0);
    assert!(
        cli(
            pm,
            state,
            &["issue", "project", "add", "cadence", "--prefix", "CAD"]
        )
        .0
    );
    let yaml = pm.join("pm.yaml");
    let text = std::fs::read_to_string(&yaml).unwrap();
    let text = text
        .lines()
        .map(|l| {
            if l.starts_with("notes_dir:") {
                format!("notes_dir: {}", notes.display())
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&yaml, format!("{text}\n")).unwrap();
    std::fs::create_dir_all(notes).unwrap();
    let statuses = ["backlog", "ready", "doing", "review", "done", "dropped"];
    for n in 1..=count {
        let dir = pm.join("cadence").join(format!("CAD-{n}"));
        std::fs::create_dir_all(dir.join("comments")).unwrap();
        std::fs::create_dir_all(dir.join("artifacts")).unwrap();
        let mut front = format!(
            "---\nid: CAD-{n}\ntitle: fixture issue {n}\nstatus: {}\npriority: P{}\n",
            statuses[n % statuses.len()],
            n % 4
        );
        if n % 3 == 0 {
            front.push_str(&format!("owner: wk{}\n", n % AGENTS));
        }
        if n % 10 != 1 {
            let epic = n - (n - 1) % 10;
            front.push_str(&format!("parent: CAD-{epic}\n"));
        }
        if n % 7 == 0 && n > 1 {
            front.push_str(&format!("blocked_by:\n- CAD-{}\n", n - 1));
        }
        if n % 5 == 0 {
            front.push_str("tags:\n- mvp\n");
        }
        front.push_str("created: 2026-09-01T00:00:00Z\n---\n\n");
        let body =
            format!("fixture body {n}\n\n## Acceptance\n\n- [x] first check\n- [ ] second check\n");
        std::fs::write(dir.join("issue.md"), format!("{front}{body}")).unwrap();
        if n % 3 == 0 {
            std::fs::write(
                dir.join("comments").join("20260901T000000Z-operator.md"),
                "---\nauthor: operator\nat: 2026-09-01T00:00:00Z\n---\n\na comment\n",
            )
            .unwrap();
        }
        if n % 3 == 1 {
            let kind = ["kickoff", "qa", "note"][n % 3];
            std::fs::write(
                notes.join(format!("20260901-{:06}-cad-{n}-{kind}.md", n % 240000)),
                format!("# note for CAD-{n}\n> Issue: `CAD-{n}`\n\nbody\n"),
            )
            .unwrap();
        }
    }
    git(pm, &["add", "-A"]);
    git(
        pm,
        &["commit", "-q", "--no-verify", "-m", "fixture tracker"],
    );
}

/// Agents, issue-bound jobs with dispatched tasks, and message history.
fn seed_daemon(d: &Daemon, pm: &Path, jobs: usize) {
    let cwd = pm.to_str().unwrap();
    d.rpc(
        "agent_register",
        json!({"alias": "pm", "provider": "fake", "endpoint_kind": "fake", "cwd": cwd}),
    );
    for a in 0..AGENTS {
        d.rpc(
            "agent_register",
            json!({"alias": format!("wk{a}"), "provider": "fake",
                   "endpoint_kind": "fake", "cwd": cwd,
                   "params": "{\"upstream\":\"pm\"}"}),
        );
    }
    let spec = pm.join("spec.md");
    std::fs::write(&spec, "# spec\n").unwrap();
    for j in 0..jobs {
        let issue = format!("CAD-{}", 2 + j * 2);
        let job = d.rpc(
            "job_new",
            json!({"pm": "pm", "spec": spec, "spec_sha256": "test",
                   "issue": issue, "title": format!("job {j}")}),
        );
        let job_id = job["job"]["id"].as_str().unwrap().to_string();
        let task = d.rpc(
            "task_new",
            json!({"job": job_id, "assignee": format!("wk{}", j % AGENTS),
                   "title": format!("task {j}"), "acceptance": "done"}),
        );
        if j % 4 == 0 {
            let tid = task["task"]["id"].as_str().unwrap();
            d.rpc("task_dispatch", json!({"task": tid, "by": "operator"}));
        }
    }
    for a in 0..AGENTS {
        for m in 0..25 {
            d.rpc(
                "agent_send",
                json!({"alias": format!("wk{a}"), "text": format!("message {m} {}", "x".repeat(400))}),
            );
        }
    }
}

/// A connected board stream, read in the background so the server
/// never blocks on a full socket — the board keeps one open per tab.
fn open_stream(port: u16) -> thread::JoinHandle<()> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        s,
        "GET /api/stream HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n"
    )
    .unwrap();
    s.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let end = Instant::now() + Duration::from_secs(600);
        while Instant::now() < end {
            match s.read(&mut buf) {
                Ok(0) => return,
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(_) => return,
            }
        }
    })
}

fn p95(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    let idx = ((samples.len() as f64) * 0.95).ceil() as usize - 1;
    samples[idx.min(samples.len() - 1)]
}

/// Fields drop in order: the daemon shuts down before the temp dir that
/// holds its socket goes away (a deleted socket leaves `shutdown`
/// unreachable and the join waiting).
struct Fixture {
    daemon: Daemon,
    pm: PathBuf,
    state: PathBuf,
    port: u16,
    _tmp: TempDir,
}

fn fixture(issues: usize, jobs: usize) -> Fixture {
    let tmp = TempDir::new().unwrap();
    let pm = tmp.path().join("pm");
    let state = tmp.path().join("st");
    let notes = tmp.path().join("notes");
    std::fs::create_dir_all(&pm).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    seed_tracker(&pm, &state, &notes, issues);
    let daemon = Daemon::start(state.clone());
    seed_daemon(&daemon, &pm, jobs);
    let port = start_ui(&pm, &state);
    Fixture {
        _tmp: tmp,
        pm,
        state,
        daemon,
        port,
    }
}

/// p50/p95 of `SAMPLES` sequential reads per route, after one warm read.
fn measure(port: u16, label: &str) -> Vec<(&'static str, Duration)> {
    let routes = ["/api/overview", "/api/issues", "/api/agents"];
    for r in routes {
        let t = Instant::now();
        let v = get_json(port, r);
        eprintln!("{label} first {r}: {:?}", t.elapsed());
        if r == "/api/issues" {
            assert_eq!(v["issues"].as_array().unwrap().len(), ISSUES);
        }
    }
    let mut report = Vec::new();
    for r in routes {
        let mut samples = Vec::new();
        for _ in 0..SAMPLES {
            let t = Instant::now();
            let _ = get_json(port, r);
            samples.push(t.elapsed());
        }
        let p = p95(samples.clone());
        samples.sort();
        eprintln!(
            "{label} {r}: p50 {:?} p95 {p:?} over {SAMPLES} requests",
            samples[samples.len() / 2]
        );
        report.push((r, p));
    }
    report
}

/// The read model's cost meters for the fixture's board.
fn stats(fx: &Fixture) -> (u64, u64) {
    let s = ui::read_model_stats(&fx.state, &fx.pm);
    (
        s["parses"].as_u64().unwrap(),
        s["overview_builds"].as_u64().unwrap(),
    )
}

#[test]
fn board_reads_p95_under_budget_on_a_400_issue_tracker() {
    let fx = fixture(ISSUES, JOBS);
    let port = fx.port;
    // No stream open: every read fetches the daemon snapshot itself.
    let mut report = measure(port, "no stream");
    // The caches the timings rest on, asserted directly: an unchanged
    // tracker is never re-parsed, and back-to-back overview reads share
    // builds (a background refresh past 2 s may add one or two).
    let (parses, builds) = stats(&fx);
    assert!(
        parses >= ISSUES as u64,
        "the first read indexes every folder"
    );
    for _ in 0..SAMPLES {
        get_json(port, "/api/issues");
        get_json(port, "/api/issues/CAD-7");
        get_json(port, "/api/overview");
    }
    let (parses_after, builds_after) = stats(&fx);
    assert_eq!(
        parses_after, parses,
        "an unchanged tracker is never re-parsed"
    );
    assert!(
        builds_after - builds <= (SAMPLES / 4) as u64,
        "{} overview builds for {SAMPLES} reads — the overview cache is not serving",
        builds_after - builds
    );
    // Two open board tabs, as on the live host: the shared watcher keeps
    // the snapshot fresh and reads are served from it.
    let _streams = [open_stream(port), open_stream(port)];
    report.extend(measure(port, "two streams"));
    for (r, p) in report {
        assert!(p < P95_BUDGET, "{r} p95 {p:?} ≥ {P95_BUDGET:?}");
    }
}

/// Collect a board stream's bytes in the background.
fn stream_into(port: u16) -> std::sync::Arc<std::sync::Mutex<String>> {
    let buf = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        s,
        "GET /api/stream HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n"
    )
    .unwrap();
    s.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    let out = buf.clone();
    thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        let end = Instant::now() + Duration::from_secs(300);
        while Instant::now() < end {
            match s.read(&mut chunk) {
                Ok(0) => return,
                Ok(n) => out
                    .lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&chunk[..n])),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => return,
            }
        }
    });
    buf
}

/// The `data:` payloads of every `event: <kind>` frame seen so far.
fn frames(buf: &std::sync::Mutex<String>, kind: &str) -> Vec<Value> {
    let text = buf.lock().unwrap().clone();
    text.split("\n\n")
        .filter_map(|f| {
            let rest = f
                .trim_start_matches('\n')
                .strip_prefix(&format!("event: {kind}\n"))?;
            serde_json::from_str(rest.strip_prefix("data: ")?).ok()
        })
        .collect()
}

fn wait_for(what: &str, secs: u64, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !ok() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(100));
    }
}

fn card(port: u16, id: &str) -> Value {
    get_json(port, "/api/issues")["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .cloned()
        .unwrap_or(Value::Null)
}

fn open_review(port: u16) -> i64 {
    get_json(port, "/api/overview")["projects"][0]["open_by_status"]["review"]
        .as_i64()
        .unwrap_or(0)
}

/// The cache is never staler than one read: a `cadence issue …` write
/// shows on the very next `/api/issues`, `/api/issues/<id>` and
/// `/api/overview`, and the stream carries it as an `issue` upsert next
/// to the unchanged legacy `issues` frame. Plans and agents diff too.
#[test]
fn tracker_writes_show_on_the_next_read_and_stream_as_entity_diffs() {
    let fx = fixture(ISSUES, 24);
    let port = fx.port;
    let stream = stream_into(port);
    wait_for("the stream to go live", 10, || {
        stream.lock().unwrap().contains(": ping")
    });

    // Warm every cache the write must get past.
    assert_eq!(card(port, "CAD-3")["status"], "review");
    let review_before = open_review(port);
    assert_eq!(get_json(port, "/api/issues/CAD-3")["status"], "review");
    let (ok, out) = cli(&fx.pm, &fx.state, &["issue", "set", "CAD-3", "status=done"]);
    assert!(ok, "{out}");
    // The very next reads — no sleep, no refresh wait.
    assert_eq!(card(port, "CAD-3")["status"], "done");
    assert_eq!(get_json(port, "/api/issues/CAD-3")["status"], "done");
    assert_eq!(
        open_review(port),
        review_before - 1,
        "overview projects row"
    );

    let (ok, out) = cli(
        &fx.pm,
        &fx.state,
        &["issue", "new", "fresh from the cli", "--project", "cadence"],
    );
    assert!(ok, "{out}");
    let fresh = format!("CAD-{}", ISSUES + 1);
    assert_eq!(card(port, &fresh)["title"], "fresh from the cli");

    // The stream: the legacy frame, unchanged, and the entity diffs.
    const ISSUES_FRAME: &str =
        "event: issues\ndata: {\"resources\":[\"issues\",\"projects\",\"issue\",\"overview\"]}\n\n";
    wait_for("an issue upsert for CAD-3 and the new issue", 10, || {
        let upserts = frames(&stream, "issue");
        upserts
            .iter()
            .any(|f| f["op"] == "upsert" && f["id"] == "CAD-3" && f["issue"]["status"] == "done")
            && upserts
                .iter()
                .any(|f| f["op"] == "upsert" && f["id"] == fresh && f["issue"]["id"] == fresh)
    });
    assert!(stream.lock().unwrap().contains(ISSUES_FRAME));
    // Only what changed is sent — no upsert for an untouched issue.
    assert!(
        !frames(&stream, "issue").iter().any(|f| f["id"] == "CAD-5"),
        "untouched issues are not re-sent"
    );

    // A plan epic: its block upserts with the epic's id, and moves with
    // its tickets' status.
    let epic = fx.pm.join("cadence/CAD-1/issue.md");
    let text = std::fs::read_to_string(&epic).unwrap();
    let text = text.replacen(
        "created:",
        "plan:\n  state: approved\n  proposed_by: operator\n  proposed_at: 2026-09-01T00:00:00Z\n  tickets:\n  - CAD-2\n  - CAD-3\ncreated:",
        1,
    );
    std::fs::write(&epic, text).unwrap();
    wait_for("a plan upsert for CAD-1", 10, || {
        frames(&stream, "plan")
            .iter()
            .any(|f| f["op"] == "upsert" && f["id"] == "CAD-1" && f["plan"]["state"] == "approved")
    });
    assert_eq!(
        get_json(port, "/api/issues/CAD-1")["plan"]["state"],
        "approved"
    );

    // An agent: the legacy `agents` frame plus an `agent` upsert.
    fx.daemon.rpc(
        "agent_register",
        json!({"alias": "late", "provider": "fake", "endpoint_kind": "fake",
               "cwd": fx.pm.to_str().unwrap()}),
    );
    wait_for("an agent upsert for late", 10, || {
        frames(&stream, "agent")
            .iter()
            .any(|f| f["op"] == "upsert" && f["id"] == "late" && f["agent"]["alias"] == "late")
    });
    assert!(stream
        .lock()
        .unwrap()
        .contains("event: agents\ndata: {\"resources\":[\"agents\",\"issue\",\"overview\"]}\n\n"));
    assert!(get_json(port, "/api/agents")["agents"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["alias"] == "late"));
}

/// The `needs_me` rows of an overview, as text — enough to find one.
fn needs(ov: &Value) -> Vec<String> {
    ov["needs_me"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.to_string())
        .collect()
}

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Review round 1, item 1: the overview is never served past a change.
/// Idle (no stream), a read is never older than the 10 s cap; with a
/// stream open, the refetch an `agents` frame triggers already shows
/// the change that caused the frame.
#[test]
fn overview_is_never_served_past_a_change_or_the_age_cap() {
    let fx = fixture(40, 2);
    let port = fx.port;
    fx.daemon.rpc(
        "agent_register",
        json!({"alias": "box", "provider": "inbox", "endpoint_kind": "inbox"}),
    );
    let unread = |ov: &Value, n: u32| {
        needs(ov)
            .iter()
            .any(|r| r.contains(&format!("{n} unread for box")))
    };

    // Idle: a change the board never hears about shows once the cached
    // value passes the cap — and the value served is never older.
    let ov = get_json(port, "/api/overview");
    assert!(!unread(&ov, 1), "{ov}");
    fx.daemon
        .rpc("agent_send", json!({"alias": "box", "text": "first"}));
    thread::sleep(Duration::from_millis(10_500));
    let ov = get_json(port, "/api/overview");
    assert!(unread(&ov, 1), "past the cap the change shows: {ov}");
    let age = now_epoch() - ov["generated_at"].as_i64().unwrap();
    assert!(age <= 11, "served overview is {age}s old");

    // Streamed: the `agents` frame's refetch sees the change.
    let stream = stream_into(port);
    wait_for("the stream to go live", 10, || {
        stream.lock().unwrap().contains(": ping")
    });
    let ov = get_json(port, "/api/overview");
    assert!(unread(&ov, 1) && !unread(&ov, 2), "{ov}");
    let agents_frames = || frames(&stream, "agents").len();
    let before = agents_frames();
    fx.daemon
        .rpc("agent_send", json!({"alias": "box", "text": "second"}));
    wait_for("an agents frame", 10, || agents_frames() > before);
    let ov = get_json(port, "/api/overview");
    assert!(unread(&ov, 2), "the refetch after the frame shows it: {ov}");
}

/// Review round 1, item 3: the git-clock cache follows history, not
/// just the file. An uncommitted status edit read once, then committed,
/// must report the commit's time — not the time cached before it.
#[test]
fn status_clock_follows_the_commit_not_the_cached_edit() {
    let fx = fixture(40, 2);
    let port = fx.port;
    let since_of = |id: &str| -> Option<i64> {
        get_json(port, "/api/overview")["needs_me"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| {
                r["title"]
                    .as_str()
                    .is_some_and(|t| t.starts_with(&format!("{id} in review")))
            })
            .map(|r| r["since"].as_i64().unwrap_or(-1))
    };
    let last_status_commit = || -> i64 {
        let out = Command::new("git")
            .arg("-C")
            .arg(&fx.pm)
            .args([
                "log",
                "-1",
                "--format=%at",
                "-G",
                "^status:",
                "--",
                "cadence/CAD-8/issue.md",
            ])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    };
    // CAD-8: a leaf in `doing`, no job, no note — its status is the file's.
    let _ = get_json(port, "/api/overview");
    let file = fx.pm.join("cadence/CAD-8/issue.md");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, text.replacen("status: doing", "status: review", 1)).unwrap();
    // Read between the write and its commit: history still says the
    // fixture commit.
    let seeded = last_status_commit();
    assert_eq!(since_of("CAD-8"), Some(seeded));
    // Commit a second later; the next read reports the commit.
    thread::sleep(Duration::from_millis(1_100));
    git(
        &fx.pm,
        &["commit", "-q", "--no-verify", "-am", "CAD-8 to review"],
    );
    let committed = last_status_commit();
    assert!(committed > seeded);
    assert_eq!(
        since_of("CAD-8"),
        Some(committed),
        "HEAD moved: the clock re-reads"
    );
}
