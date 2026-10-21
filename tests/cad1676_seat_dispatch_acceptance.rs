//! File: tests/cad1676_seat_dispatch_acceptance.rs
//! Independently authored by standards-reviewer from CAD-1676 acceptance.
//! Exercises real daemon RPC and authenticated board HTTP boundaries.
//! Rejected requests must not create a claim, lane, registration or message;
//! a subsequent valid request must dispatch to the original seat unchanged.
//!
//! The fake endpoint exercises the real actor/queue without provider calls.
//! The live-PTY foreign-cwd scenario requires a tmux-backed pane, which this
//! fixture does not provision. The bounded real-boundary refusal cases here
//! are a provider mismatch and a genuinely fenced fake endpoint, followed by
//! a successful dispatch to a different existing fake seat on the same issue.
#![cfg(feature = "test-seam")]

use cadence_agent::issue::{parse, write, Pm};
use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
use cadence_agent::{client, daemon, operator_auth};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::{Duration, Instant};

const TEST: &str = "cad1676_rejected_seats_leave_no_side_effects_then_existing_seat_dispatches";
const ISOLATED: &str = "CADENCE_CAD1676_ACCEPT_ROOT";
const PROJECT: &str = "seat";
const ISSUE: &str = "S1676-1";
const GROUP: &str = "seat-pm";
const GOOD: &str = "seat-good";
const FENCED: &str = "seat-fenced";

type Session = (String, String);

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = cadence_agent::reaper::output(Command::new("git").arg("-C").arg(cwd).args(args))
        .expect("run fixture git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn init_repo(repo: &Path) {
    fs::create_dir_all(repo).unwrap();
    git(repo, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(repo.join(".gitignore"), ".cadence/\ntarget/\n").unwrap();
    fs::write(repo.join("tracked.txt"), "CAD-1676 acceptance fixture\n").unwrap();
    git(repo, &["add", ".gitignore", "tracked.txt"]);
    git(repo, &["commit", "--quiet", "-m", "fixture base"]);
    let origin = repo.with_file_name("origin.git");
    fs::create_dir_all(&origin).unwrap();
    git(
        &origin,
        &["init", "--quiet", "--bare", "--initial-branch=main"],
    );
    git(repo, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(
        repo,
        &["push", "--quiet", "--set-upstream", "origin", "main"],
    );
    git(repo, &["remote", "set-head", "origin", "main"]);
}

struct Fixture {
    root: PathBuf,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
    port: u16,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        for thread in self.threads.drain(..).rev() {
            let result = thread.join();
            if !std::thread::panicking() {
                result.expect("fixture service panicked");
            }
        }
    }
}

impl Fixture {
    fn start(root: PathBuf) -> Self {
        fs::create_dir_all(&root).unwrap();
        let mut fx = Self {
            root,
            stop: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
            port: 0,
        };
        init_repo(&fx.repo());
        let pm = Pm::init(&fx.pm()).unwrap();
        write::project_add(
            &pm,
            PROJECT,
            "S1676",
            &[fx.repo().display().to_string()],
            &[],
            &[],
            None,
        )
        .unwrap();
        write::new_issue(
            &pm,
            &fx.repo(),
            Some(PROJECT),
            "Existing seat acceptance",
            Some("P2"),
            None,
            &[],
            None,
            None,
            &[],
            Some(ISSUE),
            Some(
                "Exercise the existing seat.\n\n## Acceptance\n\n\
                 - [ ] One lane-bound kickoff reaches the existing seat.\n",
            ),
            "standards-reviewer",
        )
        .unwrap();

        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", fx.pm().to_str().unwrap());
        let opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(Arc::clone(&fx.stop)),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        let state = fx.state();
        fx.threads.push(std::thread::spawn(move || {
            daemon::serve_with(&state, opts).expect("fixture daemon");
        }));
        let deadline = Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&fx.state(), "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&fx.state()).is_none()
        {
            assert!(Instant::now() < deadline, "fixture daemon startup timeout");
            std::thread::sleep(Duration::from_millis(50));
        }

        for port in 3110..=3199 {
            let (tx, rx) = std::sync::mpsc::channel();
            let opts = cadence_agent::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                stop: Some(Arc::clone(&fx.stop)),
                startup: Some(tx),
                test_seam: true,
                ..Default::default()
            };
            let (state, pm) = (fx.state(), fx.pm());
            let thread = std::thread::spawn(move || {
                let _ = cadence_agent::ui::serve(&state, &pm, &opts);
            });
            match rx.recv_timeout(Duration::from_secs(20)).unwrap() {
                Ok(()) => {
                    fx.port = port;
                    fx.threads.push(thread);
                    break;
                }
                Err(_) => thread.join().unwrap(),
            }
        }
        assert_ne!(fx.port, 0, "no fixture board port in 3110-3199");

        fx.op(
            "agent_register",
            json!({
                "alias": GROUP, "provider": "inbox",
                "endpoint_kind": "inbox", "role": "pm",
            }),
        );
        for alias in [GOOD, FENCED] {
            fx.op(
                "agent_register",
                json!({
                    "alias": alias, "provider": "fake", "endpoint_kind": "fake",
                    "role": "worker", "cwd": fx.repo(),
                }),
            );
            fx.wait_state(alias, "idle");
        }
        // Produce the fence through a real actor failure, not a store edit
        // or a fabricated tuple passed to kickoff_register.
        fx.op(
            "agent_send",
            json!({
                "alias": FENCED, "text": "CAD225_QUOTA_ERROR",
                "message": "cad1676-fence",
            }),
        );
        fx.wait_state(FENCED, "attention");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let show = fx.op("agent_show", json!({"alias": FENCED}));
            if show["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| message["id"] == "cad1676-fence" && message["state"] == "failed")
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "fencing turn never settled: {show}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        fx
    }

    fn state(&self) -> PathBuf {
        self.root.join("s")
    }

    fn pm(&self) -> PathBuf {
        self.root.join("pm")
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    fn issue_file(&self) -> PathBuf {
        self.pm().join(PROJECT).join(ISSUE).join("issue.md")
    }

    fn op(&self, method: &str, params: Value) -> Value {
        scoped(Asserted::Operator, || {
            client::rpc(&self.state(), method, params)
        })
        .unwrap_or_else(|error| panic!("{method}: {error}"))
    }

    fn wait_state(&self, alias: &str, state: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let show = self.op("agent_show", json!({"alias": alias}));
            if show["agent"]["state"] == state {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{alias} never became {state}: {show}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn post(
        &self,
        who: &str,
        route: &str,
        body: Value,
        session: Option<&Session>,
    ) -> (u16, String, Option<String>) {
        let host = format!("cadence-{}.localhost:{}", self.port, self.port);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let mut request = agent
            .post(format!("http://127.0.0.1:{}{route}", self.port))
            .header("Host", &host)
            .header("Origin", format!("http://{host}"))
            .header("X-Cadence-Board", "1")
            .header("Content-Type", "application/json")
            .header(AS_HEADER, who)
            .header(TOKEN_HEADER, Seam::token_at(&self.state()).unwrap());
        if let Some((cookie, key)) = session {
            request = request
                .header("Cookie", cookie)
                .header("X-Cadence-Session", key);
        }
        let mut response = request.send(body.to_string()).unwrap();
        let status = response.status().as_u16();
        let cookie = response
            .headers()
            .get("set-cookie")
            .and_then(|header| header.to_str().ok())
            .map(|header| header.split(';').next().unwrap().to_string());
        let text = response.body_mut().read_to_string().unwrap();
        (status, text, cookie)
    }

    fn session(&self) -> Session {
        operator_auth::ensure_secret(&self.state()).unwrap();
        let secret = operator_auth::read_secret(&self.state()).unwrap();
        let nonce = self.op(
            "operator_link_mint",
            json!({"secret": secret, "origin": "loopback"}),
        )["nonce"]
            .clone();
        let (status, text, cookie) =
            self.post("operator", "/api/session", json!({"nonce": nonce}), None);
        assert_eq!(status, 200, "operator session exchange: {text}");
        let body: Value = serde_json::from_str(&text).unwrap();
        (
            cookie.unwrap(),
            body["session_key"].as_str().unwrap().to_string(),
        )
    }

    fn kickoff(
        &self,
        http: bool,
        session: &Session,
        alias: &str,
        provider: &str,
    ) -> Result<Value, String> {
        let mut request = json!({"group": GROUP, "provider": provider, "alias": alias});
        if http {
            let (status, text, _) = self.post(
                "operator",
                &format!("/api/issues/{ISSUE}/kickoff"),
                request,
                Some(session),
            );
            let body: Value = serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("HTTP {status}: invalid JSON {error}: {text}"));
            if status == 200 {
                Ok(body["kickoff"].clone())
            } else {
                assert_eq!(
                    status, 400,
                    "expected a seat refusal, not an unrelated HTTP error: {text}"
                );
                Err(text)
            }
        } else {
            request["issue"] = json!(ISSUE);
            scoped(Asserted::Operator, || {
                client::rpc(&self.state(), "issue_kickoff", request)
            })
            .map_err(|error| error.to_string())
        }
    }

    // Public agent JSON intentionally omits uid. Observe actual registration
    // events instead, so an unchanged alias cannot hide re-registration.
    fn registration_events(&self, alias: &str) -> Vec<i64> {
        let mut after = 0;
        let mut registered = Vec::new();
        loop {
            let page = self.op(
                "agent_events",
                json!({"alias": alias, "after": after, "wait": 0}),
            );
            let events = page["events"].as_array().unwrap();
            if events.is_empty() {
                return registered;
            }
            for event in events {
                if event["kind"] == "registered" {
                    registered.push(event["seq"].as_i64().unwrap());
                }
            }
            let cursor = page["cursor"].as_i64().unwrap();
            assert!(cursor > after, "event pagination did not advance: {page}");
            after = cursor;
        }
    }

    fn identities(&self) -> Vec<Value> {
        let listing = self.op("agent_list", json!({}));
        let mut rows: Vec<Value> = listing["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|agent| {
                json!({
                    "alias": agent["alias"],
                    "registration_events": self.registration_events(agent["alias"].as_str().unwrap()),
                    "provider": agent["provider"], "endpoint_kind": agent["endpoint_kind"],
                    "cwd": agent["cwd"], "params": agent["params"],
                    "model": agent["model"], "effort": agent["effort"],
                })
            })
            .collect();
        rows.sort_by(|left, right| left["alias"].as_str().cmp(&right["alias"].as_str()));
        rows
    }

    fn message_ids(&self) -> BTreeMap<String, Vec<String>> {
        [GROUP, GOOD, FENCED]
            .into_iter()
            .map(|alias| {
                let page = self.op("message_list", json!({"to": alias, "limit": 50}));
                assert_eq!(
                    page["truncated"], false,
                    "fixture message inventory must be complete"
                );
                let mut ids: Vec<String> = page["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|message| message["id"].as_str().unwrap().to_string())
                    .collect();
                ids.sort();
                (alias.to_string(), ids)
            })
            .collect()
    }

    fn snapshot(&self) -> Snapshot {
        let lanes = self.repo().join(".cadence/wt");
        let mut lane_dirs: Vec<PathBuf> = match fs::read_dir(&lanes) {
            Ok(entries) => entries.map(|entry| entry.unwrap().path()).collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("read fixture lane directory: {error}"),
        };
        lane_dirs.sort();
        Snapshot {
            issue: fs::read(self.issue_file()).unwrap(),
            tracker_head: git(&self.pm(), &["rev-parse", "HEAD"]),
            worktrees: git(&self.repo(), &["worktree", "list", "--porcelain"]),
            branches: git(
                &self.repo(),
                &[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/heads",
                ],
            ),
            lane_dirs,
            agents: self.identities(),
            messages: self.message_ids(),
        }
    }
}

#[derive(Debug, PartialEq)]
struct Snapshot {
    issue: Vec<u8>,
    tracker_head: String,
    worktrees: String,
    branches: String,
    lane_dirs: Vec<PathBuf>,
    agents: Vec<Value>,
    messages: BTreeMap<String, Vec<String>>,
}

#[test]
fn cad1676_rejected_seats_leave_no_side_effects_then_existing_seat_dispatches() {
    let Some(root) = std::env::var_os(ISOLATED) else {
        let sandbox = tempfile::Builder::new()
            .prefix("c1676a-")
            .tempdir_in("/tmp")
            .unwrap();
        let home = sandbox.path().join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join(".gitconfig"),
            "[user]\nname = CAD-1676 acceptance\nemail = cad1676-acceptance@example.invalid\n",
        )
        .unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", TEST, "--test-threads", "1", "--nocapture"])
            .env(ISOLATED, sandbox.path())
            .env("HOME", &home)
            .env("TMPDIR", sandbox.path())
            .env("XDG_CONFIG_HOME", sandbox.path().join("config"))
            .env("XDG_DATA_HOME", sandbox.path().join("data"))
            .env("XDG_STATE_HOME", sandbox.path().join("state"))
            .env("XDG_CACHE_HOME", sandbox.path().join("cache"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"));
        for key in [
            "CADENCE_ALIAS",
            "CADENCE_PM_DIR",
            "CADENCE_STATE_DIR",
            "CADENCE_ORG",
            "CADENCE_PROFILE",
            "CADENCE_TEST_AS",
            "CADENCE_TEST_SEAM",
            "CADENCE_GRANT_TOKEN",
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET_DIR",
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_COUNT",
        ] {
            command.env_remove(key);
        }
        let output = cadence_agent::reaper::output(&mut command).unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success(),
            "isolated CAD-1676 acceptance failed: {text}"
        );
        assert!(
            text.contains("1 passed"),
            "isolated child did not execute the acceptance check: {text}"
        );
        return;
    };
    let root = PathBuf::from(root);
    for http in [false, true] {
        let fixture = Fixture::start(root.join(if http { "http" } else { "rpc" }));
        let session = fixture.session();
        let baseline = fixture.snapshot();
        let (front, _) =
            parse::parse_issue(&String::from_utf8(baseline.issue.clone()).unwrap()).unwrap();
        assert!(front.claim.is_none());
        assert!(front
            .refs
            .iter()
            .all(|reference| reference.kind != "worktree"));
        assert_eq!(baseline.agents.len(), 3);
        let seat = baseline
            .agents
            .iter()
            .find(|agent| agent["alias"] == GOOD)
            .unwrap();
        assert_eq!(seat["registration_events"].as_array().unwrap().len(), 1);

        let mismatch = fixture.kickoff(http, &session, GOOD, "claude").unwrap_err();
        assert!(
            mismatch.contains("the seat runs fake, not claude"),
            "wrong refusal: {mismatch}"
        );
        assert_eq!(
            fixture.snapshot(),
            baseline,
            "provider mismatch mutated fixture state (HTTP={http})"
        );

        let fenced = fixture.kickoff(http, &session, FENCED, "fake").unwrap_err();
        assert!(fenced.contains("fenced"), "wrong refusal: {fenced}");
        assert_eq!(
            fixture.snapshot(),
            baseline,
            "fenced seat mutated fixture state (HTTP={http})"
        );

        // The same issue must remain usable: a lane-only duplicate receipt
        // is not successful dispatch, and would fail these assertions.
        let receipt = fixture.kickoff(http, &session, GOOD, "fake").unwrap();
        assert_eq!(
            receipt["dispatched"], true,
            "existing seat was not dispatched: {receipt}"
        );
        let message = receipt["message"]
            .as_str()
            .filter(|id| !id.is_empty())
            .expect("kickoff message id");
        let lane = PathBuf::from(receipt["worktree"].as_str().expect("kickoff worktree"));
        assert!(lane.is_dir(), "successful dispatch did not create its lane");
        assert_eq!(
            fixture.identities(),
            baseline.agents,
            "existing seats were registered again or reconfigured"
        );
        let page = fixture.op(
            "message_list",
            json!({"to": GOOD, "source": "dispatch", "limit": 50}),
        );
        assert_eq!(
            page["messages"].as_array().unwrap().len(),
            1,
            "expected exactly one kickoff: {page}"
        );
        assert_eq!(page["messages"][0]["id"], message);
        let show = fixture.op("agent_show", json!({"alias": GOOD}));
        let stored = show["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == message)
            .expect("durable kickoff on the existing seat");
        assert_eq!(stored["source"], "dispatch");
        assert_eq!(stored["issue"], ISSUE);
        assert_eq!(stored["worktree"].as_str(), lane.to_str());
        let (front, _) =
            parse::parse_issue(&fs::read_to_string(fixture.issue_file()).unwrap()).unwrap();
        assert_eq!(front.status, "doing");
        assert_eq!(front.owner.as_deref(), Some(GOOD));
        assert_eq!(
            front.claim.as_ref().map(|claim| claim.by.as_str()),
            Some(GROUP)
        );
    }
}
