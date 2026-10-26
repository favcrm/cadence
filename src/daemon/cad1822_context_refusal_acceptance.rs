//! CAD-1822 independent adversarial acceptance, authored from the ticket by
//! the acceptance author, NOT by the implementer. The implementer may not
//! edit or weaken this file.
//!
//! Ticket expectation, not implementation behavior: before any adapter
//! submission an issue-bound dispatch must refuse when its mandatory context
//! cannot be supplied whole. A required source that is absent at the pinned
//! revision, a manifest that binds another project's scope, and a mandatory
//! combined payload over the local enqueue ceiling are refusals — never a
//! silent omission, truncation of acceptance, or a queued turn. A valid
//! required source on the same live path is the positive control: the guard
//! must not blanket-refuse.
//!
//! This exercises the real daemon RPC and the real board HTTP peer over a
//! real fixture Git repository and the real tracker; the provider endpoint is
//! a registered `fake` seat, so a refusal is observed as *no message row*
//! (no adapter submission), not as a fake answer. No planner function is
//! called directly and no request field is trusted to carry sources.

#![cfg(feature = "test-seam")]

use crate::issue::{write, Pm};
use crate::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PROJECT: &str = "ctxp";
const PROJECT_PREFIX: &str = "C1822";
const OTHER_PROJECT: &str = "otherp";
const OTHER_PREFIX: &str = "O1822";
const GROUP: &str = "ctxp-pm";

const CONTROL: &str = "C1822-1";
const SCOPE_BAD: &str = "C1822-2";
const MISSING_BAD: &str = "C1822-3";
const OVERFLOW_BAD: &str = "C1822-4";
const RESTORED: &str = "C1822-5";
const PIN_A: &str = "C1822-6";
const PIN_B: &str = "C1822-7";

const CONTROL_WORKER: &str = "ctxp-control";
const SCOPE_WORKER: &str = "ctxp-scope";
const MISSING_WORKER: &str = "ctxp-missing";
const OVERFLOW_WORKER: &str = "ctxp-overflow";
const RESTORED_WORKER: &str = "ctxp-restored";
const PIN_WORKER: &str = "ctxp-pin";

const MANIFEST: &str = "docs/cadence/project-context.yaml";
const POLICY: &str = "docs/policy.md";
const ABSENT: &str = "docs/absent.md";
const BIG_POLICY: &str = "docs/big-policy.md";

/// The fixture state dir is also the daemon socket root; every fixture gets
/// its own temp dir under the process temp root.
fn git(cwd: &Path, args: &[&str]) -> String {
    let output = crate::reaper::output(Command::new("git").arg("-C").arg(cwd).args(args))
        .unwrap_or_else(|error| panic!("git {args:?} in {}: {error}", cwd.display()));
    assert!(
        output.status.success(),
        "git {args:?} in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn commit_all(repo: &Path, message: &str) {
    git(repo, &["add", "-A"]);
    let output = crate::reaper::output(Command::new("git").arg("-C").arg(repo).args([
        "-c",
        "user.name=CAD-1822 acceptance",
        "-c",
        "user.email=cad1822-acceptance@example.invalid",
        "commit",
        "--quiet",
        "-m",
        message,
    ]))
    .expect("run fixture commit");
    assert!(
        output.status.success(),
        "fixture commit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    git(repo, &["push", "--quiet", "origin", "main"]);
}

/// A local fixture repository with a real origin, so `issue start` and the
/// project reader resolve an immutable HEAD exactly as in production.
fn init_repo(repo: &Path) {
    fs::create_dir_all(repo).expect("create fixture repo");
    git(repo, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(repo.join(".gitignore"), ".cadence/\ntarget/\n").unwrap();
    fs::write(repo.join("tracked.txt"), "CAD-1822 context acceptance\n").unwrap();
    git(repo, &["add", ".gitignore", "tracked.txt"]);
    let output = crate::reaper::output(Command::new("git").arg("-C").arg(repo).args([
        "-c",
        "user.name=CAD-1822 acceptance",
        "-c",
        "user.email=cad1822-acceptance@example.invalid",
        "commit",
        "--quiet",
        "-m",
        "fixture base",
    ]))
    .expect("run fixture base commit");
    assert!(
        output.status.success(),
        "fixture base commit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let origin = PathBuf::from(format!("{}-origin.git", repo.display()));
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

fn manifest_yaml(project: &str, documents: &[(&str, &str, &str, bool)]) -> String {
    let mut text = format!("schema: 1\nproject: {project}\nagent_delivery: true\ndocuments:\n");
    for (id, path, title, required) in documents {
        text.push_str(&format!(
            "  - id: {id}\n    kind: scope\n    path: {path}\n    title: {title}\n    \
             required: {required}\n    roles: []\n"
        ));
    }
    text
}

/// Write one manifest revision plus its documents, commit it, and push it so
/// the fixture's `origin/main` and local HEAD agree. Only real tracked files
/// feed the reader; nothing here is injected into the daemon request.
fn set_sources(
    repo: &Path,
    project: &str,
    documents: &[(&str, &str, &str, bool)],
    files: &[(&str, &str)],
) {
    for (path, content) in files {
        let path = repo.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
    }
    let manifest = repo.join(MANIFEST);
    fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    fs::write(&manifest, manifest_yaml(project, documents)).unwrap();
    commit_all(repo, &format!("context {project} {} docs", documents.len()));
}

/// 50_000 bytes of multibyte policy text (< the reader's 64 KiB document
/// cap) — large enough that the mandatory rendered payload crosses the
/// 48_000-byte enqueue ceiling once acceptance and the contract are added.
fn big_policy() -> String {
    let unit = "政策不可截断，上下文必须整体拒绝。";
    let mut text = String::new();
    while text.len() < 50_000 {
        text.push_str(unit);
    }
    text
}

fn acceptance_body(items: &[&str]) -> String {
    let mut body =
        String::from("Exercise CAD-1822 mandatory context delivery.\n\n## Acceptance\n\n");
    for item in items {
        body.push_str(&format!("- [ ] {item}\n"));
    }
    body
}

struct Fixture {
    _dir: tempfile::TempDir,
    pm: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    port: u16,
    token: String,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
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
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("c1822-context-")
            .tempdir()
            .expect("isolated CAD-1822 root");
        let pm = dir.path().join("pm");
        let repo = dir.path().join("repo");
        let other = dir.path().join("other-repo");
        let state = dir.path().join("state");
        init_repo(&repo);
        init_repo(&other);

        let pm_handle = Pm::init(&pm).expect("initialize isolated PM");
        write::project_add(
            &pm_handle,
            PROJECT,
            PROJECT_PREFIX,
            &[repo.display().to_string()],
            &[],
            &[],
            None,
        )
        .expect("register the context project");
        write::project_add(
            &pm_handle,
            OTHER_PROJECT,
            OTHER_PREFIX,
            &[other.display().to_string()],
            &[],
            &[],
            None,
        )
        .expect("register the conflicting project");

        // Initial revision: a valid required policy, so the control dispatch
        // on this exact live path must succeed.
        set_sources(
            &repo,
            PROJECT,
            &[("policy", POLICY, "Required policy", true)],
            &[(POLICY, "The required policy body is present.\n")],
        );

        for (id, title, items) in [
            (
                CONTROL,
                "CAD-1822 control dispatch",
                vec!["The supplied required policy is delivered whole."],
            ),
            (
                SCOPE_BAD,
                "CAD-1822 cross-project scope",
                vec!["A source bound to another project is refused."],
            ),
            (
                MISSING_BAD,
                "CAD-1822 absent required source",
                vec!["An absent required source blocks the turn."],
            ),
            (
                OVERFLOW_BAD,
                "CAD-1822 mandatory overflow",
                vec![
                    "Combined mandatory payload over the limit is refused whole.",
                    "验收条目不得被截断，全部原样拒绝。",
                ],
            ),
            (
                RESTORED,
                "CAD-1822 restored dispatch",
                vec!["A restored required source dispatches again."],
            ),
            (
                PIN_A,
                "CAD-1822 pin isolation A",
                vec!["The pin for this message names task A."],
            ),
            (
                PIN_B,
                "CAD-1822 pin isolation B",
                vec!["The pin for this message names task B."],
            ),
        ] {
            let body = acceptance_body(&items);
            write::new_issue(
                &pm_handle,
                &repo,
                Some(PROJECT),
                title,
                Some("P2"),
                None,
                &[],
                None,
                None,
                &[],
                Some(id),
                Some(body.as_str()),
                "cad1822-acceptance",
            )
            .expect("create fixture issue");
        }

        let env = crate::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let opts = super::ServeOptions {
            provider_env: env,
            stop: Some(Arc::clone(&stop)),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(super::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };

        let mut fixture = Self {
            _dir: dir,
            pm,
            repo,
            state,
            port: 0,
            token: String::new(),
            stop,
            threads: Vec::new(),
        };

        let daemon_state = fixture.state.clone();
        let daemon = std::thread::spawn(move || {
            super::serve_with(&daemon_state, opts).expect("fixture daemon");
        });
        fixture.threads.push(daemon);
        let deadline = Instant::now() + Duration::from_secs(30);
        while crate::client::rpc_timeout(
            &fixture.state,
            "health",
            json!({}),
            Duration::from_secs(2),
        )
        .is_err()
            || Seam::token_at(&fixture.state).is_none()
        {
            assert!(Instant::now() < deadline, "fixture daemon startup timeout");
            std::thread::sleep(Duration::from_millis(50));
        }
        fixture.token = Seam::token_at(&fixture.state).expect("fixture seam token");

        fixture.register_agents();
        fixture
    }

    fn start_board(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        for port in 3110..=3199u16 {
            let (tx, rx) = std::sync::mpsc::channel();
            let opts = crate::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                stop: Some(Arc::clone(&self.stop)),
                startup: Some(tx),
                test_seam: true,
                ..Default::default()
            };
            let (state, pm) = (self.state.clone(), self.pm.clone());
            let thread = std::thread::spawn(move || {
                let _ = crate::ui::serve(&state, &pm, &opts);
            });
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(remaining) {
                Ok(Ok(())) => {
                    self.port = port;
                    self.threads.push(thread);
                    return;
                }
                Ok(Err(std::io::ErrorKind::AddrInUse)) => {
                    thread.join().expect("fixture board thread panicked");
                }
                Ok(Err(kind)) => panic!("fixture board failed on {port}: {kind:?}"),
                Err(error) => panic!("fixture board startup failed on {port}: {error}"),
            }
        }
        panic!("no fixture board port in 3110-3199");
    }

    fn register_agents(&self) {
        self.op(
            "agent_register",
            json!({
                "alias": GROUP, "provider": "inbox",
                "endpoint_kind": "inbox", "role": "pm",
            }),
        );
        for alias in [
            CONTROL_WORKER,
            SCOPE_WORKER,
            MISSING_WORKER,
            OVERFLOW_WORKER,
            RESTORED_WORKER,
            PIN_WORKER,
        ] {
            self.op(
                "agent_register",
                json!({
                    "alias": alias, "provider": "fake", "endpoint_kind": "fake",
                    "role": "worker", "cwd": self.repo,
                    "params": r#"{"fake_generation":true}"#,
                }),
            );
            self.wait_state(alias, "idle");
        }
    }

    fn op(&self, method: &str, params: Value) -> Value {
        self.try_op(method, params)
            .unwrap_or_else(|error| panic!("{method}: {error}"))
    }

    fn try_op(&self, method: &str, params: Value) -> crate::error::Result<Value> {
        scoped(Asserted::Operator, || {
            crate::client::rpc(&self.state, method, params)
        })
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

    fn issue_file(&self, id: &str) -> PathBuf {
        self.pm.join(PROJECT).join(id).join("issue.md")
    }

    /// The acceptance section alone: the ticket requires that refusal never
    /// truncates it. Frontmatter may legitimately change if a lane is opened
    /// before the guard (the ticket bounds submission, not the lane), so it
    /// is excluded from the byte comparison.
    fn acceptance(&self, id: &str) -> String {
        let text = fs::read_to_string(self.issue_file(id)).expect("read issue body");
        let start = text
            .find("## Acceptance")
            .unwrap_or_else(|| panic!("{id} has no acceptance section"));
        let rest = &text[start..];
        let end = rest[3..]
            .find("\n## ")
            .map(|offset| offset + 3)
            .unwrap_or(rest.len());
        rest[..end].to_string()
    }

    fn kickoff_rpc(&self, issue: &str, alias: &str) -> crate::error::Result<Value> {
        scoped(Asserted::Operator, || {
            crate::client::rpc(
                &self.state,
                "issue_kickoff",
                json!({"issue": issue, "group": GROUP, "provider": "fake", "alias": alias}),
            )
        })
    }

    fn message_ids(&self, alias: &str, source: Option<&str>) -> Vec<String> {
        let mut params = json!({"to": alias, "limit": 50});
        if let Some(source) = source {
            params["source"] = json!(source);
        }
        let page = self.op("message_list", params);
        assert_eq!(
            page["truncated"], false,
            "fixture message inventory must be complete: {page}"
        );
        let mut ids: Vec<String> = page["messages"]
            .as_array()
            .expect("message_list rows")
            .iter()
            .map(|message| message["id"].as_str().expect("message id").to_string())
            .collect();
        ids.sort();
        ids
    }

    /// The native pin event for exactly this kickoff message, read through
    /// the store query the change adds. Opening a second connection to the
    /// fixture's SQLite file only observes what the served daemon recorded;
    /// it cannot fabricate a pin or borrow another message's pin.
    fn context_pin(&self, alias: &str, message: &str) -> Value {
        // open_side preserves the live daemon's identity and in-flight turns;
        // Store::open is the owner boot path and would run restart recovery.
        let store = crate::store::Store::open_side(&self.state.join("cadence.sqlite3"))
            .expect("open fixture store for the pin receipt");
        store
            .context_pin(alias, message)
            .expect("read context pin")
            .unwrap_or_else(|| panic!("no context pin for message {message}"))
    }

    fn post_kickoff(&self, session: &(String, String), issue: &str, alias: &str) -> (u16, String) {
        let host = format!("cadence-{}.localhost:{}", self.port, self.port);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let mut response = agent
            .post(format!(
                "http://127.0.0.1:{}/api/issues/{issue}/kickoff",
                self.port
            ))
            .header("Host", &host)
            .header("Origin", format!("http://{host}"))
            .header("X-Cadence-Board", "1")
            .header(AS_HEADER, "operator")
            .header(TOKEN_HEADER, &self.token)
            .header("Cookie", &session.0)
            .header("X-Cadence-Session", &session.1)
            .header("Content-Type", "application/json")
            .send(json!({"group": GROUP, "provider": "fake", "alias": alias}).to_string())
            .unwrap();
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().unwrap_or_default();
        (status, text)
    }

    fn session(&self) -> (String, String) {
        crate::operator_auth::ensure_secret(&self.state).expect("operator secret");
        let secret = crate::operator_auth::read_secret(&self.state).expect("read operator secret");
        let nonce = self.op(
            "operator_link_mint",
            json!({"secret": secret, "origin": "loopback"}),
        )["nonce"]
            .clone();
        let host = format!("cadence-{}.localhost:{}", self.port, self.port);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let mut response = agent
            .post(format!("http://127.0.0.1:{}/api/session", self.port))
            .header("Host", &host)
            .header("Origin", format!("http://{host}"))
            .header("X-Cadence-Board", "1")
            .header(AS_HEADER, "operator")
            .header(TOKEN_HEADER, &self.token)
            .header("Content-Type", "application/json")
            .send(json!({"nonce": nonce}).to_string())
            .unwrap();
        let status = response.status().as_u16();
        let cookie = response
            .headers()
            .get("set-cookie")
            .and_then(|header| header.to_str().ok())
            .and_then(|header| header.split(';').next())
            .map(str::to_string);
        let text = response.body_mut().read_to_string().unwrap();
        assert_eq!(status, 200, "operator session exchange: {text}");
        let body: Value = serde_json::from_str(&text).expect("operator session json");
        (
            cookie.expect("operator session cookie"),
            body["session_key"]
                .as_str()
                .expect("operator session key")
                .to_string(),
        )
    }
}

/// Refusal must name the ticket's reason and leave no adapter submission:
/// no message row on the target, and the acceptance section byte-identical.
fn expect_refused(
    fixture: &Fixture,
    issue: &str,
    worker: &str,
    code: &str,
    baseline: &[String],
    response: std::result::Result<Value, String>,
) {
    let error = match response {
        Ok(value) => panic!("{issue} was dispatched instead of refused: {value}"),
        Err(error) => error,
    };
    assert!(
        error.contains(&format!("context refused: {code}")),
        "{issue}: expected the mandatory-context guard ({code}), got: {error}"
    );
    assert_eq!(
        fixture.message_ids(worker, None),
        baseline,
        "{issue}: a refused dispatch changed {worker}'s message inventory"
    );
    assert!(
        fixture.message_ids(worker, Some("dispatch")).is_empty(),
        "{issue}: a refused dispatch left a dispatch-shaped row on {worker}"
    );
    assert!(
        !fixture.acceptance(issue).is_empty(),
        "{issue}: acceptance text disappeared"
    );
}

#[test]
fn cad1822_mandatory_issue_context_refuses_before_adapter_submission() {
    let mut fixture = Fixture::new();
    fixture.start_board();
    let session = fixture.session();

    // Positive control: the same live path, the same worker shape and a
    // valid required source must dispatch exactly one kickoff. Without this
    // a blanket refusal could pass the adversarial cases.
    let control_before = fixture.acceptance(CONTROL);
    let dispatched = fixture
        .kickoff_rpc(CONTROL, CONTROL_WORKER)
        .expect("a valid required source must dispatch, not be refused");
    assert_eq!(
        dispatched["dispatched"],
        json!(true),
        "control did not dispatch: {dispatched}"
    );
    let control_messages = fixture.message_ids(CONTROL_WORKER, Some("dispatch"));
    assert_eq!(
        control_messages.len(),
        1,
        "control must queue exactly one kickoff: {control_messages:?}"
    );
    assert_eq!(
        dispatched["message"].as_str(),
        control_messages.first().map(String::as_str),
        "control receipt must name the queued kickoff"
    );
    assert_eq!(
        fixture.acceptance(CONTROL),
        control_before,
        "a successful dispatch must not rewrite acceptance"
    );

    // A. A manifest that binds another project's scope is refused. The
    // required document is present; only the scope binding is wrong.
    set_sources(
        &fixture.repo,
        OTHER_PROJECT,
        &[("policy", POLICY, "Required policy", true)],
        &[(POLICY, "Foreign project policy; must not be supplied.\n")],
    );
    let scope_before = fixture.acceptance(SCOPE_BAD);
    let scope_baseline = fixture.message_ids(SCOPE_WORKER, None);
    let scope = fixture
        .kickoff_rpc(SCOPE_BAD, SCOPE_WORKER)
        .map_err(|error| error.to_string());
    expect_refused(
        &fixture,
        SCOPE_BAD,
        SCOPE_WORKER,
        "scope-conflict",
        &scope_baseline,
        scope,
    );
    assert_eq!(
        fixture.acceptance(SCOPE_BAD),
        scope_before,
        "scope refusal rewrote acceptance"
    );

    // B. A required source that is not tracked at HEAD is refused over the
    // real board HTTP peer; the manifest itself is valid.
    set_sources(
        &fixture.repo,
        PROJECT,
        &[("absent", ABSENT, "Absent required source", true)],
        &[],
    );
    let missing_before = fixture.acceptance(MISSING_BAD);
    let missing_baseline = fixture.message_ids(MISSING_WORKER, None);
    let (status, body) = fixture.post_kickoff(&session, MISSING_BAD, MISSING_WORKER);
    assert_eq!(
        status, 400,
        "board must refuse an absent required source: {body}"
    );
    expect_refused(
        &fixture,
        MISSING_BAD,
        MISSING_WORKER,
        "missing-required-source",
        &missing_baseline,
        Err(body),
    );
    assert_eq!(
        fixture.acceptance(MISSING_BAD),
        missing_before,
        "missing-source refusal rewrote acceptance"
    );

    // C. A required source large enough that the mandatory combined payload
    // crosses the local enqueue ceiling is refused whole, including
    // multibyte text; acceptance is never truncated to make it fit.
    let big = big_policy();
    set_sources(
        &fixture.repo,
        PROJECT,
        &[("big-policy", BIG_POLICY, "Large required policy", true)],
        &[(BIG_POLICY, big.as_str())],
    );
    let overflow_before = fixture.acceptance(OVERFLOW_BAD);
    let overflow_baseline = fixture.message_ids(OVERFLOW_WORKER, None);
    let (status, body) = fixture.post_kickoff(&session, OVERFLOW_BAD, OVERFLOW_WORKER);
    assert_eq!(
        status, 400,
        "board must refuse an over-limit mandatory payload: {body}"
    );
    expect_refused(
        &fixture,
        OVERFLOW_BAD,
        OVERFLOW_WORKER,
        "mandatory-overflow",
        &overflow_baseline,
        Err(body),
    );
    assert_eq!(
        fixture.acceptance(OVERFLOW_BAD),
        overflow_before,
        "overflow refusal must not truncate acceptance"
    );

    // The refusal is not a blanket outage: the same project still dispatches
    // once a valid required source is restored.
    set_sources(
        &fixture.repo,
        PROJECT,
        &[("policy", POLICY, "Required policy", true)],
        &[(POLICY, "The required policy body is restored.\n")],
    );
    let restored = fixture
        .kickoff_rpc(RESTORED, RESTORED_WORKER)
        .expect("restored required source must dispatch again");
    assert_eq!(restored["dispatched"], json!(true), "{restored}");
    assert_eq!(
        fixture.message_ids(RESTORED_WORKER, Some("dispatch")).len(),
        1,
        "restored dispatch must queue exactly one kickoff"
    );

    // Isolation: two real kickoffs on one seat must each bind their own exact
    // kickoff message id. A newest-event-by-alias lookup (or a shared pin)
    // would make one task inherit the other's contract, so both receipts are
    // read by id and must disagree in task and manifest digest.
    let pin_a = fixture
        .kickoff_rpc(PIN_A, PIN_WORKER)
        .expect("pin A kickoff must dispatch");
    let pin_b = fixture
        .kickoff_rpc(PIN_B, PIN_WORKER)
        .expect("pin B kickoff must dispatch");
    assert_eq!(pin_a["dispatched"], json!(true), "{pin_a}");
    assert_eq!(pin_b["dispatched"], json!(true), "{pin_b}");
    let mid_a = pin_a["message"]
        .as_str()
        .expect("pin A kickoff message id")
        .to_string();
    let mid_b = pin_b["message"]
        .as_str()
        .expect("pin B kickoff message id")
        .to_string();
    assert_ne!(mid_a, mid_b, "two issues must mint two distinct kickoffs");
    let receipt_a = fixture.context_pin(PIN_WORKER, &mid_a);
    let receipt_b = fixture.context_pin(PIN_WORKER, &mid_b);
    assert_eq!(
        receipt_a["message"].as_str(),
        Some(mid_a.as_str()),
        "pin A is bound to another message id: {receipt_a}"
    );
    assert_eq!(
        receipt_b["message"].as_str(),
        Some(mid_b.as_str()),
        "pin B is bound to another message id: {receipt_b}"
    );
    assert_eq!(
        receipt_a["manifest"]["task"]["task"].as_str(),
        Some(PIN_A),
        "pin A names the wrong task: {receipt_a}"
    );
    assert_eq!(
        receipt_b["manifest"]["task"]["task"].as_str(),
        Some(PIN_B),
        "pin B names the wrong task: {receipt_b}"
    );
    assert_ne!(
        receipt_a["manifest"]["digest"], receipt_b["manifest"]["digest"],
        "two tasks must not share one pinned manifest"
    );
    assert_ne!(
        fixture.context_pin(PIN_WORKER, &mid_a),
        receipt_b,
        "the seat's newest pin must not answer for an earlier message"
    );
}

#[test]
fn cad1822_compaction_restores_the_bound_task_on_plain_worker_followups() {
    let fixture = Fixture::new();
    // Required context fits by itself; optional continuity must yield after
    // compaction rather than making the same task impossible to resume.
    let policy = format!(
        "The required policy body is present.\n{}\nEnd of mandatory policy appendix.\n",
        "Reference policy clause.\n".repeat(1699)
    );
    set_sources(
        &fixture.repo,
        PROJECT,
        &[("policy", POLICY, "Required policy", true)],
        &[(POLICY, policy.as_str())],
    );
    let dispatched = fixture.kickoff_rpc(CONTROL, CONTROL_WORKER).unwrap();
    let expected_generation = fixture.op("agent_show", json!({"alias": CONTROL_WORKER}))["agent"]
        ["generation"]
        .as_str()
        .expect("the fixture must launch a generation-reporting endpoint")
        .to_string();
    let store = crate::store::Store::open_side(&fixture.state.join("cadence.sqlite3")).unwrap();
    assert_eq!(
        store.agent(CONTROL_WORKER).unwrap().generation.as_deref(),
        Some(expected_generation.as_str()),
        "opening an observation handle must not recover or invalidate the live endpoint"
    );
    let supplied = |message: &str| {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let events = store.events_tail(CONTROL_WORKER, 100).unwrap();
            if let Some(event) = events.iter().find(|event| {
                event.kind == "context_supplied" && event.payload["message"] == message
            }) {
                return event.payload.clone();
            }
            if let Some(event) = events.iter().find(|event| {
                event.kind == "context_refused" && event.payload["message"] == message
            }) {
                panic!("valid required context was refused: {}", event.payload);
            }
            assert!(Instant::now() < deadline, "no supply receipt for {message}");
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let send = |text: &str| {
        fixture.op("agent_send", json!({"alias": CONTROL_WORKER, "text": text}))["message"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let first = supplied(dispatched["message"].as_str().unwrap());
    fixture.wait_state(CONTROL_WORKER, "idle");
    let active = fixture.op("agent_show", json!({"alias": CONTROL_WORKER}));
    assert_eq!(
        first["session"], active["agent"]["thread_id"],
        "retention requires the same confirmed live provider session: {active}"
    );
    assert_eq!(
        first["generation"], active["agent"]["generation"],
        "retention requires the same confirmed live endpoint generation: {active}"
    );
    let followup = send("Continue the bound issue without changing its policy.");
    let retained = supplied(&followup);
    assert_eq!(
        retained["appended_bytes"],
        0,
        "first={first}; retained={retained}; current={}",
        fixture.op("agent_show", json!({"alias": CONTROL_WORKER}))
    );
    let another = send("Continue the same task with the same acceptance.");
    assert_eq!(supplied(&another)["appended_bytes"], 0);
    let compact = send("COMPACT");
    supplied(&compact);
    let resumed = send("Continue the same issue after provider compaction.");
    let restored = supplied(&resumed);
    assert_eq!(restored["task"], CONTROL);
    assert_eq!(restored["pin"], first["pin"]);
    let events = store.events_tail(CONTROL_WORKER, 100).unwrap();
    let result = events
        .iter()
        .find(|event| event.kind == "turn_finished" && event.payload["message"] == resumed)
        .expect("restored turn must complete");
    let text = result.payload["result"]["text"].as_str().unwrap();
    assert!(text.contains("The supplied required policy is delivered whole."));
    assert!(text.contains("The required policy body is present."));
    assert!(text.contains("End of mandatory policy appendix."));
}

#[test]
fn cad1822_receipt_loss_preserves_task_without_replaying_the_turn() {
    let fixture = Fixture::new();
    let path = fixture.state.join("cadence.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER cad1822_drop_supply BEFORE INSERT ON events
         WHEN NEW.alias = 'ctxp-control' AND NEW.kind = 'context_supplied'
         BEGIN SELECT RAISE(FAIL, 'CAD1822 injected receipt failure'); END;",
        )
        .unwrap();
    let store = crate::store::Store::open_side(&path).unwrap();
    let event = |kind: &str, message: &str| {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let events = store.events_tail(CONTROL_WORKER, 100).unwrap();
            if let Some(found) = events
                .iter()
                .find(|entry| entry.kind == kind && entry.payload["message"] == message)
            {
                return found.payload.clone();
            }
            assert!(Instant::now() < deadline, "missing {kind} for {message}");
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let first = fixture.kickoff_rpc(CONTROL, CONTROL_WORKER).unwrap();
    let first_id = first["message"].as_str().unwrap();
    event("context_supply_failed", first_id);
    connection
        .execute_batch("DROP TRIGGER cad1822_drop_supply;")
        .unwrap();
    let next = fixture.op(
        "agent_send",
        json!({"alias": CONTROL_WORKER, "text": "Continue the same task after receipt recovery."}),
    );
    let next_id = next["message"].as_str().unwrap();
    let finished = event("turn_finished", next_id);
    let input = finished["result"]["text"].as_str().unwrap();
    assert!(
        input.contains("The supplied required policy is delivered whole."),
        "receipt loss dropped the task contract: {finished}"
    );
    assert!(
        input.contains("The required policy body is present."),
        "receipt loss dropped the required source: {finished}"
    );
    let restored = event("context_supplied", next_id);
    assert_eq!(restored["task"], CONTROL);
    assert_eq!(restored["pin"], first_id);
    let entries = store.events_tail(CONTROL_WORKER, 100).unwrap();
    assert_eq!(
        entries
            .iter()
            .filter(|entry| {
                entry.kind == "turn_finished" && entry.payload["message"] == first_id
            })
            .count(),
        1,
        "a failed receipt must not replay the completed turn"
    );
    assert!(
        !entries.iter().any(|entry| {
            entry.kind == "context_supplied" && entry.payload["message"] == first_id
        }),
        "failed receipt storage must not establish supplied state"
    );
}

#[test]
fn cad1822_uncertain_delivery_requires_reconciliation_not_replay() {
    let fixture = Fixture::new();
    let first = fixture.kickoff_rpc(CONTROL, CONTROL_WORKER).unwrap();
    let lost = fixture.op(
        "agent_send",
        json!({"alias": CONTROL_WORKER, "text": "DISCONNECT"}),
    );
    let lost_id = lost["message"].as_str().unwrap();
    let store = crate::store::Store::open_side(&fixture.state.join("cadence.sqlite3")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while store.message(lost_id).unwrap().unwrap().state != "unknown" {
        assert!(
            Instant::now() < deadline,
            "accepted disconnect did not remain unknown"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    fixture.wait_state(CONTROL_WORKER, "attention");
    assert_eq!(store.message(lost_id).unwrap().unwrap().state, "unknown");
    let events = store.events_tail(CONTROL_WORKER, 100).unwrap();
    assert!(events.iter().any(|entry| {
        entry.kind == "context_supplied" && entry.payload["message"] == first["message"]
    }));
    assert!(
        !events.iter().any(|entry| {
            entry.kind == "context_supplied" && entry.payload["message"] == lost_id
        }),
        "an accepted but uncertain turn must not establish supplied status"
    );
    assert!(
        fixture
            .try_op("agent_resume", json!({"alias": CONTROL_WORKER}))
            .is_err(),
        "resume must not silently settle or resend an unknown turn"
    );
    let refused = scoped(Asserted::Agent(CONTROL_WORKER.into()), || {
        crate::client::rpc(
            &fixture.state,
            "agent_unfence",
            json!({"alias": CONTROL_WORKER, "status": "interrupted", "resume": true}),
        )
    });
    let error = refused
        .expect_err("a worker cannot authorize its own reconciliation")
        .to_string();
    assert!(
        error.contains("operator"),
        "unexpected refusal instead of the operator guard: {error}"
    );
    assert_eq!(store.message(lost_id).unwrap().unwrap().state, "unknown");
    let reconciled = fixture.try_op("agent_unfence", json!({
        "alias": CONTROL_WORKER, "status": "interrupted", "resume": true,
        "note": "Isolated fake disconnected after acceptance; no external actions; settle without resend."
    }));
    assert!(
        reconciled.is_ok(),
        "operator reconciliation={reconciled:?}; message={:?}; current={}",
        store.message(lost_id).unwrap(),
        fixture.op("agent_show", json!({"alias": CONTROL_WORKER}))
    );
    let next = fixture.op("agent_send", json!({
        "alias": CONTROL_WORKER, "text": "Continue the current task after reconciliation; do not retry the disconnected turn."
    }));
    let next_id = next["message"].as_str().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let receipt = loop {
        let events = store.events_tail(CONTROL_WORKER, 100).unwrap();
        if let Some(entry) = events
            .iter()
            .find(|entry| entry.kind == "context_supplied" && entry.payload["message"] == next_id)
        {
            break entry.payload.clone();
        }
        assert!(
            Instant::now() < deadline,
            "reconciled continuation did not restore context"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(receipt["task"], CONTROL);
    assert_eq!(receipt["pin"], first["message"]);
    assert!(
        receipt["appended_bytes"].as_u64().unwrap() > 0,
        "a fresh generation cannot inherit old confirmed residency"
    );
    let finished = store.message(next_id).unwrap().unwrap();
    let text = finished.result.as_ref().unwrap()["text"].as_str().unwrap();
    assert!(text.contains("The supplied required policy is delivered whole."));
    assert!(text.contains("The required policy body is present."));
    let events = store.events_tail(CONTROL_WORKER, 100).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|entry| { entry.kind == "turn_started" && entry.payload["message"] == lost_id })
            .count(),
        1,
        "reconciliation must not replay the accepted turn"
    );
    assert_eq!(
        store.message(lost_id).unwrap().unwrap().state,
        "interrupted"
    );
}

#[test]
fn cad1822_activation_write_failure_refuses_before_provider_acceptance() {
    let fixture = Fixture::new();
    let path = fixture.state.join("cadence.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER cad1822_drop_activation BEFORE INSERT ON events
         WHEN NEW.alias = 'ctxp-control' AND NEW.kind = 'context_activated'
         BEGIN SELECT RAISE(FAIL, 'CAD1822 injected activation failure'); END;",
        )
        .unwrap();
    let kickoff = fixture.kickoff_rpc(CONTROL, CONTROL_WORKER).unwrap();
    let id = kickoff["message"].as_str().unwrap();
    let store = crate::store::Store::open_side(&path).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let failed = loop {
        let message = store.message(id).unwrap().unwrap();
        if message.state == "failed" {
            break message;
        }
        assert!(
            Instant::now() < deadline,
            "activation failure was not refused"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(failed.result.as_ref().unwrap()["error"]
        .as_str()
        .unwrap()
        .contains("context refused: activation-failed:"));
    assert!(
        !store
            .events_tail(CONTROL_WORKER, 100)
            .unwrap()
            .iter()
            .any(|entry| {
                entry.payload["message"] == id
                    && ["turn_started", "context_supplied"].contains(&entry.kind.as_str())
            }),
        "failed durable task binding must not reach the provider"
    );
}

#[test]
fn cad1822_non_context_turn_keeps_legacy_continuity_above_pilot_cap() {
    let fixture = Fixture::new();
    let store = crate::store::Store::open_side(&fixture.state.join("cadence.sqlite3")).unwrap();
    let completed = |id: &str| {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let message = store.message(id).unwrap().unwrap();
            if message.state == "completed" {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "legacy turn did not complete: {}",
                message.to_json()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let requirement = "Legacy task contract: keep accepted actions non-replayed.";
    let first = fixture.op(
        "thread_send",
        json!({
            "alias": CONTROL_WORKER, "text": requirement
        }),
    );
    let first_id = first["message"].as_str().unwrap();
    completed(first_id);
    assert!(
        store
            .context_pin(CONTROL_WORKER, first_id)
            .unwrap()
            .is_none(),
        "this scenario must exercise the legacy non-context path"
    );
    let compact = fixture.op(
        "thread_send",
        json!({"alias": CONTROL_WORKER, "text": "COMPACT"}),
    );
    completed(compact["message"].as_str().unwrap());
    assert!(
        store
            .events_tail(CONTROL_WORKER, 100)
            .unwrap()
            .iter()
            .any(|entry| entry.kind == "session_compacted"),
        "the actual provider compaction must make legacy continuity due"
    );
    let baseline = store
        .events_tail(CONTROL_WORKER, 100)
        .unwrap()
        .last()
        .unwrap()
        .seq;
    let next = fixture.op("thread_send", json!({
        "alias": CONTROL_WORKER,
        "text": format!("Continue under the previously recorded task contract.\n{}", "x".repeat(47_900))
    }));
    completed(next["message"].as_str().unwrap());
    let events = store.events_tail(CONTROL_WORKER, 100).unwrap();
    let received = events
        .iter()
        .find(|entry| entry.seq > baseline && entry.kind == "fake_pack");
    assert!(
        received
            .and_then(|entry| entry.payload["pack"].as_str())
            .is_some_and(|text| text.contains(requirement)),
        "pilot-only cap dropped actual legacy continuity; decision: {:?}",
        events
            .iter()
            .find(|entry| entry.seq > baseline && entry.kind == "context_gap")
            .map(|entry| &entry.payload)
    );
}

fn opt_out_assignment_does_not_restore_prior_context(
    same_issue: bool,
    blocked_event: Option<&str>,
) {
    let fixture = Fixture::new();
    let store = crate::store::Store::open_side(&fixture.state.join("cadence.sqlite3")).unwrap();
    let event = |kind: &str, id: &str| {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let entries = store.events_tail(CONTROL_WORKER, 100).unwrap();
            if let Some(entry) = entries
                .iter()
                .find(|entry| entry.kind == kind && entry.payload["message"] == id)
            {
                return entry.payload.clone();
            }
            assert!(Instant::now() < deadline, "missing {kind} for {id}");
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let first = fixture.kickoff_rpc(CONTROL, CONTROL_WORKER).unwrap();
    event("context_supplied", first["message"].as_str().unwrap());
    let manifest = fixture.repo.join("docs/cadence/project-context.yaml");
    let current = fs::read_to_string(&manifest).unwrap();
    fs::write(
        &manifest,
        current.replacen("agent_delivery: true", "agent_delivery: false", 1),
    )
    .unwrap();
    commit_all(
        &fixture.repo,
        "disable pilot delivery for the next native assignment",
    );
    if let Some(kind) = blocked_event {
        let connection = rusqlite::Connection::open(fixture.state.join("cadence.sqlite3")).unwrap();
        connection
            .execute_batch(&format!(
                "CREATE TRIGGER cad1822_transition_failure BEFORE INSERT ON events
             WHEN NEW.alias = 'ctxp-control' AND NEW.kind = '{kind}'
             BEGIN SELECT RAISE(FAIL, 'CAD1822 injected transition failure'); END;"
            ))
            .unwrap();
    }
    let next = if same_issue {
        fixture.try_op("dispatch_send", json!({
            "alias": CONTROL_WORKER, "issue": CONTROL, "reply_to": GROUP,
            "text": format!("Explicit legacy re-assignment of this issue.\n{}", fixture.acceptance(CONTROL))
        }))
    } else {
        fixture.kickoff_rpc(PIN_A, CONTROL_WORKER)
    };
    if blocked_event == Some("context_disabled") {
        next.expect_err("a failed exact-assignment selection must refuse admission");
        assert!(
            fixture.message_ids(CONTROL_WORKER, None).iter().all(|id| {
                id == first["message"].as_str().unwrap()
                    || store.message(id).unwrap().unwrap().state == "failed"
            }),
            "a missing assignment selection must not leave executable work"
        );
        assert_eq!(
            store
                .events_tail(CONTROL_WORKER, 100)
                .unwrap()
                .iter()
                .filter(|entry| entry.kind == "turn_started")
                .count(),
            1,
            "only the initial valid task may have reached the provider"
        );
        return;
    }
    let next = next.unwrap();
    if blocked_event == Some("context_deactivated") {
        let id = next["message"].as_str().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let message = store.message(id).unwrap().unwrap();
            if message.state == "failed" {
                break;
            }
            assert_ne!(
                message.state, "completed",
                "failed scope invalidation reached the provider"
            );
            assert!(
                Instant::now() < deadline,
                "scope invalidation did not fail closed"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !store
                .events_tail(CONTROL_WORKER, 100)
                .unwrap()
                .iter()
                .any(|entry| { entry.kind == "turn_started" && entry.payload["message"] == id }),
            "an uncleared prior task must block provider acceptance of the new assignment"
        );
        return;
    }
    event("turn_finished", next["message"].as_str().unwrap());
    let compact = fixture.op(
        "agent_send",
        json!({"alias": CONTROL_WORKER, "text": "COMPACT"}),
    );
    event("turn_finished", compact["message"].as_str().unwrap());
    let followup = fixture.op("agent_send", json!({
        "alias": CONTROL_WORKER, "text": "Continue the most recently assigned issue after compaction."
    }));
    let finished = event("turn_finished", followup["message"].as_str().unwrap());
    let text = finished["result"]["text"].as_str().unwrap();
    assert!(!text.contains("The supplied required policy is delivered whole."),
        "an opted-out native assignment restored the previous pilot contract (same issue: {same_issue}): {finished}");
    assert!(!text.contains("The required policy body is present."),
        "an opted-out native assignment automatically restored the previous policy (same issue: {same_issue}): {finished}");
}

#[test]
fn cad1822_opt_out_other_issue_does_not_restore_prior_task() {
    opt_out_assignment_does_not_restore_prior_context(false, None);
}

#[test]
fn cad1822_explicit_opt_out_same_issue_does_not_restore_prior_task() {
    opt_out_assignment_does_not_restore_prior_context(true, None);
}

#[test]
fn cad1822_failed_disabled_selection_cannot_enqueue_unbound_work() {
    opt_out_assignment_does_not_restore_prior_context(false, Some("context_disabled"));
}

#[test]
fn cad1822_failed_scope_invalidation_cannot_submit_the_new_assignment() {
    opt_out_assignment_does_not_restore_prior_context(false, Some("context_deactivated"));
}

#[test]
fn cad1822_rejected_message_collision_cannot_retarget_queued_context() {
    let fixture = Fixture::new();
    let store = crate::store::Store::open_side(&fixture.state.join("cadence.sqlite3")).unwrap();
    let event = |kind: &str, id: &str| {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let entries = store.events_tail(CONTROL_WORKER, 100).unwrap();
            if let Some(entry) = entries
                .iter()
                .find(|entry| entry.kind == kind && entry.payload["message"] == id)
            {
                return entry.payload.clone();
            }
            assert!(Instant::now() < deadline, "missing {kind} for {id}");
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let first = fixture.kickoff_rpc(CONTROL, CONTROL_WORKER).unwrap();
    event("context_supplied", first["message"].as_str().unwrap());
    let manifest = fixture.repo.join("docs/cadence/project-context.yaml");
    let current = fs::read_to_string(&manifest).unwrap();
    fs::write(
        &manifest,
        current.replacen("agent_delivery: true", "agent_delivery: false", 1),
    )
    .unwrap();
    commit_all(&fixture.repo, "opt out only future accepted assignments");
    fixture.op("agent_send", json!({
        "alias": CONTROL_WORKER, "text": "NEED_INPUT:hold the actor while testing a queued message collision"
    }));
    let deadline = Instant::now() + Duration::from_secs(30);
    let handle = loop {
        let pending = fixture.op("agent_requests", json!({"alias": CONTROL_WORKER}));
        if let Some(handle) = pending["requests"]
            .as_array()
            .unwrap()
            .first()
            .and_then(|entry| entry["request"].as_str())
        {
            break handle.to_string();
        }
        assert!(
            Instant::now() < deadline,
            "the provider did not hold for the collision scenario"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let queued_id = uuid::Uuid::new_v4().simple().to_string();
    fixture.op(
        "agent_send",
        json!({
            "alias": CONTROL_WORKER, "text": "COMPACT", "message": queued_id
        }),
    );
    let rejected = fixture.try_op(
        "dispatch_send",
        json!({
            "alias": CONTROL_WORKER, "issue": CONTROL, "reply_to": GROUP,
            "text": "A conflicting assignment must not replace the queued message.",
            "message": queued_id
        }),
    );
    fixture.op(
        "agent_respond",
        json!({
            "alias": CONTROL_WORKER, "request": handle, "decision": "accept"
        }),
    );
    assert!(
        rejected.is_err(),
        "the conflicting envelope must be refused"
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let queued = store.message(&queued_id).unwrap().unwrap();
        if queued.result.is_some() {
            assert_eq!(
                queued.state,
                "completed",
                "a rejected dispatch poisoned a valid queued turn: {}",
                queued.to_json()
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the original queued turn did not finish"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let followup = fixture.op("agent_send", json!({
        "alias": CONTROL_WORKER, "text": "Continue the original accepted task after compaction; the conflicting assignment was refused."
    }));
    let restored = event("turn_finished", followup["message"].as_str().unwrap());
    let input = restored["result"]["text"].as_str().unwrap();
    assert!(
        input.contains("The supplied required policy is delivered whole."),
        "a rejected same-id request changed the accepted task contract: {restored}"
    );
    assert!(
        input.contains("The required policy body is present."),
        "a rejected same-id request suppressed the accepted task's required policy: {restored}"
    );
}

#[test]
fn cad1822_accepted_legacy_retry_does_not_recapture_new_project_context() {
    let fixture = Fixture::new();
    let manifest = fixture.repo.join("docs/cadence/project-context.yaml");
    let enabled = fs::read_to_string(&manifest).unwrap();
    fs::write(
        &manifest,
        enabled.replacen("agent_delivery: true", "agent_delivery: false", 1),
    )
    .unwrap();
    commit_all(
        &fixture.repo,
        "accept the original request without pilot context",
    );
    let first = fixture.kickoff_rpc(CONTROL, CONTROL_WORKER).unwrap();
    let id = first["message"].as_str().unwrap();
    let store = crate::store::Store::open_side(&fixture.state.join("cadence.sqlite3")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let original = loop {
        let message = store.message(id).unwrap().unwrap();
        if message.result.is_some() {
            break message;
        }
        assert!(
            Instant::now() < deadline,
            "the original legacy request did not settle"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(original.state, "completed");
    fs::write(&manifest, enabled).unwrap();
    fs::remove_file(fixture.repo.join(POLICY)).unwrap();
    commit_all(
        &fixture.repo,
        "enable context with an unavailable required source",
    );
    let retry = fixture.try_op("dispatch_send", json!({
        "alias": CONTROL_WORKER, "issue": CONTROL, "message": id,
        "text": original.body, "reply_to": original.reply_to.as_deref().unwrap()
    })).expect("a retry of an accepted envelope must return its existing outcome, not a new source refusal");
    assert_eq!(retry["duplicate"], true);
    assert_eq!(retry["state"], "completed");
    let fresh = fixture.try_op("dispatch_send", json!({
        "alias": CONTROL_WORKER, "issue": CONTROL,
        "text": "A genuinely new assignment still needs the required source.", "reply_to": GROUP
    })).expect_err("a new assignment must still refuse the unavailable required source");
    assert!(fresh.to_string().contains("missing-required-source"));
    assert_eq!(
        store
            .events_tail(CONTROL_WORKER, 100)
            .unwrap()
            .iter()
            .filter(|entry| entry.kind == "turn_started")
            .count(),
        1,
        "a retry cannot replay an accepted task and the new invalid task cannot run"
    );
}

fn context_event(fixture: &Fixture, alias: &str, kind: &str, message: &str) -> Value {
    let store = crate::store::Store::open_side(&fixture.state.join("cadence.sqlite3")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(entry) = store
            .events_tail(alias, 100)
            .unwrap()
            .iter()
            .find(|entry| entry.kind == kind && entry.payload["message"] == message)
        {
            return entry.payload.clone();
        }
        assert!(Instant::now() < deadline, "missing {kind} for {message}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn cad1822_custom_dispatch_carries_the_canonical_acceptance() {
    let fixture = Fixture::new();
    // Open the genuine lane on another seat; this target has never received
    // the contract, so retained history cannot conceal an omitted criterion.
    fixture.kickoff_rpc(CONTROL, RESTORED_WORKER).unwrap();
    let sent = fixture.op(
        "dispatch_send",
        json!({
            "alias": CONTROL_WORKER, "issue": CONTROL, "reply_to": GROUP,
            "text": "Execute the assigned task using the supplied policy."
        }),
    );
    let finished = context_event(
        &fixture,
        CONTROL_WORKER,
        "turn_finished",
        sent["message"].as_str().unwrap(),
    );
    assert!(
        finished["result"]["text"]
            .as_str()
            .unwrap()
            .contains("The supplied required policy is delivered whole."),
        "dispatch provenance is not proof that the actual input carries acceptance: {finished}"
    );
}

#[test]
fn cad1822_retained_peer_envelope_overflow_refuses_before_provider() {
    let fixture = Fixture::new();
    let first = fixture.kickoff_rpc(CONTROL, CONTROL_WORKER).unwrap();
    context_event(
        &fixture,
        CONTROL_WORKER,
        "context_supplied",
        first["message"].as_str().unwrap(),
    );
    let sent = scoped(Asserted::Agent(CONTROL_WORKER.into()), || {
        crate::client::rpc(
            &fixture.state,
            "agent_send",
            json!({"alias": CONTROL_WORKER, "text": "\"".repeat(30_000)}),
        )
    })
    .unwrap();
    let id = sent["message"].as_str().unwrap();
    let store = crate::store::Store::open_side(&fixture.state.join("cadence.sqlite3")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let settled = loop {
        let message = store.message(id).unwrap().unwrap();
        if message.result.is_some() {
            break message;
        }
        assert!(
            Instant::now() < deadline,
            "expanded peer turn did not settle"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(
        settled.state, "failed",
        "the expanded mandatory body must refuse even when source bodies are retained"
    );
    assert!(settled.result.as_ref().unwrap()["error"]
        .as_str()
        .unwrap()
        .contains("mandatory-overflow"));
    assert!(
        !store
            .events_tail(CONTROL_WORKER, 100)
            .unwrap()
            .iter()
            .any(|entry| { entry.kind == "turn_started" && entry.payload["message"] == id }),
        "expanded overflow must be refused before provider acceptance"
    );
}

#[test]
fn cad1822_context_join_is_inside_the_combined_byte_cap() {
    let fixture = Fixture::new();
    let sent = fixture.kickoff_rpc(CONTROL, RESTORED_WORKER).unwrap();
    let manifest: crate::context_pack::Manifest = serde_json::from_value(
        fixture.context_pin(RESTORED_WORKER, sent["message"].as_str().unwrap())["manifest"].clone(),
    )
    .unwrap();
    let mut reader = |source: &crate::context_pack::Source| {
        crate::issue::context::read_pinned_blob(
            &fixture.repo,
            &source.revision,
            &source.path,
            &source.digest,
        )
    };
    let baseline = crate::context_pack::plan(&manifest, None, false, "", 0, &mut reader).unwrap();
    let exact_body = 48_000 - baseline.text.len() - 2;
    let body = "x".repeat(exact_body + 1);
    let exact =
        crate::context_pack::plan(&manifest, None, false, &body[..exact_body], 0, &mut reader)
            .unwrap();
    assert_eq!(exact_body + 2 + exact.text.len(), 48_000);
    assert!(
        matches!(
            crate::context_pack::plan(&manifest, None, false, &body, 0, &mut reader),
            Err(crate::context_pack::Refusal::MandatoryOverflow { .. })
        ),
        "one byte beyond the actual body + join + context cap must refuse, not reach the adapter"
    );
}

fn receipt_from_plan(
    manifest: &crate::context_pack::Manifest,
    plan: crate::context_pack::Plan,
) -> crate::context_pack::Receipt {
    crate::context_pack::Receipt {
        task: manifest.task.task.clone(),
        task_revision: manifest.task.revision.clone(),
        manifest: manifest.digest.clone(),
        sources: plan.included,
        withheld: plan.withheld,
        ..Default::default()
    }
}

#[test]
fn cad1822_omitted_optional_source_is_not_resident_when_promoted() {
    let fixture = Fixture::new();
    let guide = format!(
        "REQUIRED_GUIDE_BEGIN{}REQUIRED_GUIDE_END",
        "g".repeat(10_000)
    );
    set_sources(
        &fixture.repo,
        PROJECT,
        &[
            ("policy", POLICY, "Required policy", true),
            ("guide", "docs/guide.md", "Guide", false),
        ],
        &[
            (POLICY, "The required policy body is present.\n"),
            ("docs/guide.md", &guide),
        ],
    );
    let first = fixture.kickoff_rpc(CONTROL, RESTORED_WORKER).unwrap();
    let original: crate::context_pack::Manifest = serde_json::from_value(
        fixture.context_pin(RESTORED_WORKER, first["message"].as_str().unwrap())["manifest"]
            .clone(),
    )
    .unwrap();
    let mut reader = |source: &crate::context_pack::Source| {
        crate::issue::context::read_pinned_blob(
            &fixture.repo,
            &source.revision,
            &source.path,
            &source.digest,
        )
    };
    let mut initial_body = String::with_capacity(40_000);
    initial_body.push_str(r#"Acceptance: "The supplied required policy is delivered whole.""#);
    initial_body.extend(std::iter::repeat_n('x', 40_000 - initial_body.len()));
    let full =
        crate::context_pack::plan(&original, None, false, &initial_body, 0, &mut reader).unwrap();
    assert!(full
        .withheld
        .iter()
        .any(|gap| gap["id"] == "guide" && gap["state"] == "overflow"));
    let previous = receipt_from_plan(&original, full);
    let retained = crate::context_pack::plan(
        &original,
        Some(&previous),
        true,
        "Continue the unchanged task.",
        0,
        &mut reader,
    )
    .unwrap();
    assert!(
        !retained
            .included
            .iter()
            .any(|source| source["id"] == "guide"),
        "a retained receipt cannot claim a never-delivered document is unchanged/resident"
    );
    assert!(
        retained
            .withheld
            .iter()
            .any(|gap| gap["id"] == "guide" && gap["state"] == "overflow"),
        "the actual overflow reason must survive an unchanged turn"
    );
    let previous = receipt_from_plan(&original, retained);
    set_sources(
        &fixture.repo,
        PROJECT,
        &[
            ("policy", POLICY, "Required policy", true),
            ("guide", "docs/guide.md", "Guide", true),
        ],
        &[
            (POLICY, "The required policy body is present.\n"),
            ("docs/guide.md", &guide),
        ],
    );
    let revised = fixture.op(
        "dispatch_send",
        json!({
            "alias": RESTORED_WORKER, "issue": CONTROL, "reply_to": GROUP,
            "text": fixture.acceptance(CONTROL)
        }),
    );
    let current: crate::context_pack::Manifest = serde_json::from_value(
        fixture.context_pin(RESTORED_WORKER, revised["message"].as_str().unwrap())["manifest"]
            .clone(),
    )
    .unwrap();
    let promoted = crate::context_pack::plan(
        &current,
        Some(&previous),
        true,
        "Apply the current task.",
        0,
        &mut reader,
    )
    .unwrap();
    assert!(
        promoted.text.contains(&guide),
        "promoting a never-supplied optional guide to required must send its complete body"
    );
}

#[test]
fn cad1822_legacy_summary_change_keeps_the_reported_duplicate() {
    let fixture = Fixture::new();
    let manifest = fixture.repo.join("docs/cadence/project-context.yaml");
    let current = fs::read_to_string(&manifest).unwrap();
    fs::write(
        &manifest,
        current.replacen("agent_delivery: true", "agent_delivery: false", 1),
    )
    .unwrap();
    commit_all(&fixture.repo, "leave the legacy dispatch path opted out");
    let first = fixture.kickoff_rpc(CONTROL, CONTROL_WORKER).unwrap();
    context_event(
        &fixture,
        CONTROL_WORKER,
        "turn_finished",
        first["message"].as_str().unwrap(),
    );
    let pm = crate::issue::Pm::at(&fixture.pm).unwrap();
    let args = crate::issue::dispatch::DispatchArgs {
        to: CONTROL_WORKER.into(),
        note: None,
        name: None,
        base: None,
        repo: Some(fixture.repo.clone()),
        reply_to: Some(GROUP.into()),
        summary: Some("A changed summary is not a new legacy task assignment.".into()),
        job_spec: None,
        no_lessons: true,
        force: false,
        take_over: None,
    };
    let repeated = scoped(Asserted::Operator, || {
        crate::issue::dispatch::run(
            &pm,
            CONTROL,
            &args,
            "operator",
            &fixture.state,
            None,
            crate::issue::dispatch::DispatchOrigin::Explicit,
        )
    })
    .unwrap();
    assert_eq!(repeated["dispatched"], false,
        "changing summary alone must not execute another task in a non-opted-in project: {repeated}");
    assert_eq!(repeated["duplicate_kind"], "reported");
    assert_eq!(repeated["message"], first["message"]);
}

#[test]
fn cad1822_pi_guard_is_inside_the_full_combined_cap() {
    let fixture = Fixture::new();
    let sent = fixture.kickoff_rpc(CONTROL, RESTORED_WORKER).unwrap();
    let manifest: crate::context_pack::Manifest = serde_json::from_value(
        fixture.context_pin(RESTORED_WORKER, sent["message"].as_str().unwrap())["manifest"].clone(),
    )
    .unwrap();
    let mut reader = |source: &crate::context_pack::Source| {
        crate::issue::context::read_pinned_blob(
            &fixture.repo,
            &source.revision,
            &source.path,
            &source.digest,
        )
    };
    let guard = crate::adapter::pi::TASK_INPUT_PREFIX;
    let reserved = crate::context_pack::adapter_reserved_bytes("omp");
    let baseline =
        crate::context_pack::plan(&manifest, None, false, "", reserved, &mut reader).unwrap();
    let body_bytes = 48_000 - guard.len() - baseline.text.len() - 2;
    let mut body = "界".repeat(body_bytes / 3);
    body.extend(std::iter::repeat_n('x', body_bytes % 3));
    let exact =
        crate::context_pack::plan(&manifest, None, false, &body, reserved, &mut reader).unwrap();
    assert_eq!(guard.len() + body.len() + 2 + exact.text.len(), 48_000);
    body.push('x');
    assert!(
        matches!(
            crate::context_pack::plan(&manifest, None, false, &body, reserved, &mut reader),
            Err(crate::context_pack::Refusal::MandatoryOverflow { .. })
        ),
        "one byte above the complete local Pi text, including its real guard and Unicode, must refuse"
    );
}

#[test]
fn cad1822_pi_guard_is_inside_the_retained_cap() {
    let fixture = Fixture::new();
    let sent = fixture.kickoff_rpc(CONTROL, RESTORED_WORKER).unwrap();
    let manifest: crate::context_pack::Manifest = serde_json::from_value(
        fixture.context_pin(RESTORED_WORKER, sent["message"].as_str().unwrap())["manifest"].clone(),
    )
    .unwrap();
    let mut reader = |source: &crate::context_pack::Source| {
        crate::issue::context::read_pinned_blob(
            &fixture.repo,
            &source.revision,
            &source.path,
            &source.digest,
        )
    };
    let guard = crate::adapter::pi::TASK_INPUT_PREFIX;
    let full =
        crate::context_pack::plan(&manifest, None, false, "", guard.len(), &mut reader).unwrap();
    let previous = receipt_from_plan(&manifest, full);
    let body_bytes = 48_000 - guard.len();
    let mut body = "界".repeat(body_bytes / 3);
    body.extend(std::iter::repeat_n('x', body_bytes % 3));
    let exact = crate::context_pack::plan(
        &manifest,
        Some(&previous),
        true,
        &body,
        guard.len(),
        &mut reader,
    )
    .unwrap();
    assert_eq!(guard.len() + body.len() + exact.text.len(), 48_000);
    body.push('x');
    assert!(
        matches!(
            crate::context_pack::plan(
                &manifest,
                Some(&previous),
                true,
                &body,
                guard.len(),
                &mut reader,
            ),
            Err(crate::context_pack::Refusal::MandatoryOverflow { .. })
        ),
        "retained context must not hide the known Pi guard overhead from the hard cap"
    );
}

#[test]
fn cad1822_context_revision_and_contract_share_the_selected_issue_snapshot() {
    let fixture = Fixture::new();
    let pm = Pm::at(&fixture.pm).unwrap();
    let original_source = fs::read_to_string(fixture.issue_file(CONTROL)).unwrap();
    let (original_front, original_body) =
        crate::issue::parse::parse_issue(&original_source).unwrap();
    let original_items = crate::issue::parse::acceptance_items(&original_body);
    let expected_title = original_front.title;
    let expected_acceptance: Vec<String> = original_items
        .iter()
        .map(|item| item.text.clone())
        .collect();
    let expected_revision = crate::delegation::sha256_hex(original_source.as_bytes());

    // Capture the same parsed issue snapshot that preflight passes to the
    // shared guard, before another ordinary tracker writer changes it.
    let gated = crate::issue::dispatch::gate_issue(&pm, CONTROL, true).unwrap();
    assert_eq!(gated.front.title, expected_title);
    assert_eq!(gated.items, original_items);

    let select = || {
        crate::issue::dispatch::context_guard(
            &pm,
            &gated.project,
            &gated.front,
            crate::issue::dispatch::ContextTarget {
                team_role: None,
                provider: "fake",
                endpoint_kind: "fake",
            },
            true,
            || {
                Ok((
                    crate::context_pack::TaskContract::new(
                        &gated.front.id,
                        &gated.front.title,
                        gated.source.as_bytes(),
                        &gated.items,
                        gated.request.clone(),
                    ),
                    std::borrow::Cow::Borrowed(
                        "Execute the originally selected task using its required project policy.",
                    ),
                ))
            },
        )
        .expect("the real required Git policy must remain deliverable")
        .expect("the fixture project is explicitly opted in")
        .manifest
    };

    // Positive control: without an intervening edit the real guard agrees
    // with the selected source, including its exact byte revision.
    let stable = select();
    assert_eq!(stable.task.title, expected_title);
    assert_eq!(stable.task.acceptance, expected_acceptance);
    assert_eq!(stable.task.revision, expected_revision);

    // These are normal tracker writes, completed exactly between the two
    // reads. They are not an authorized revision of the captured dispatch.
    let revised_title = "CAD-1822 independently revised task title";
    write::set_fields(
        &pm,
        &[CONTROL.to_string()],
        &[format!("title={revised_title}")],
        "cad1822-acceptance",
        None,
    )
    .unwrap();
    let input = fixture._dir.path().join("revised-acceptance.md");
    fs::write(
        &input,
        "- [ ] The revised task has a different canonical acceptance.\n",
    )
    .unwrap();
    write::set_acceptance(&pm, CONTROL, &input, "cad1822-acceptance").unwrap();

    let revised = crate::issue::dispatch::gate_issue(&pm, CONTROL, true).unwrap();
    assert_eq!(revised.front.title, revised_title);
    assert_ne!(revised.items, original_items);
    let revised_source = fs::read(fixture.issue_file(CONTROL)).unwrap();
    assert_ne!(
        crate::delegation::sha256_hex(&revised_source),
        expected_revision,
        "the controlled normal edits must actually change the issue revision"
    );

    let pinned = select();
    assert_eq!(pinned.task.title, expected_title);
    assert_eq!(pinned.task.acceptance, expected_acceptance);
    assert_eq!(
        pinned.task.revision, expected_revision,
        "the original title and criteria must retain their original source revision; \
         a later issue edit cannot relabel an already selected contract"
    );
}

#[test]
fn cad1822_empty_project_context_still_requires_a_prompt_capable_endpoint() {
    let fixture = Fixture::new();
    fixture.kickoff_rpc(CONTROL, RESTORED_WORKER).unwrap();
    let target = "ctxp-empty-contract-inbox";
    fixture.op(
        "agent_register",
        json!({
            "alias": target, "provider": "inbox", "endpoint_kind": "inbox",
            "role": "worker", "cwd": fixture.repo
        }),
    );
    let (_, issue_body) = write::load_front(fixture.issue_file(CONTROL).parent().unwrap()).unwrap();
    assert!(
        !crate::issue::parse::acceptance_items(&issue_body).is_empty(),
        "the empty document set must still have a mandatory canonical task contract"
    );
    let acceptance_before = fixture.acceptance(CONTROL);
    let manifest = fixture.repo.join(MANIFEST);
    let enabled = format!("schema: 1\nproject: {PROJECT}\nagent_delivery: true\ndocuments: []\n");
    fs::write(&manifest, &enabled).unwrap();
    commit_all(
        &fixture.repo,
        "opt in with a mandatory task contract and no document bodies",
    );

    let id = "cad1822-empty-contract-inbox";
    let request = json!({
        "alias": target, "issue": CONTROL, "reply_to": GROUP,
        "message": id, "text": "Execute the assigned task."
    });
    let baseline = fixture.message_ids(target, None);
    let error = fixture.try_op("dispatch_send", request.clone()).expect_err(
        "an opted-in canonical task contract cannot be silently discarded by an Inbox endpoint",
    );
    assert_eq!(error.kind(), "rejected");
    assert!(
        error
            .to_string()
            .contains("context refused: unsupported-endpoint:"),
        "expected the native mandatory-contract endpoint refusal, got: {error}"
    );
    assert_eq!(fixture.message_ids(target, None), baseline);
    assert_eq!(fixture.acceptance(CONTROL), acceptance_before);
    let store = crate::store::Store::open_side(&fixture.state.join("cadence.sqlite3")).unwrap();
    assert!(store.message(id).unwrap().is_none());
    assert!(store.context_pin(target, id).unwrap().is_none());

    fs::write(
        &manifest,
        enabled.replacen("agent_delivery: true", "agent_delivery: false", 1),
    )
    .unwrap();
    commit_all(
        &fixture.repo,
        "leave the legacy Inbox assignment explicitly opted out",
    );
    let accepted = fixture
        .try_op("dispatch_send", request)
        .expect("an opted-out legacy Inbox dispatch must remain supported");
    assert_eq!(accepted["message"], id);
    assert_eq!(accepted["state"], "queued");
    assert_eq!(accepted["duplicate"], false);
    assert!(store.message(id).unwrap().is_some());
    assert!(store.context_pin(target, id).unwrap().is_none());
    assert_eq!(fixture.acceptance(CONTROL), acceptance_before);
}

#[test]
fn cad1846_complete_issue_request_overflow_refuses_native_admission() {
    // One bad-case oracle, exercised through both native entry points.
    for via_http in [false, true] {
        let mut fixture = Fixture::new();
        let session = if via_http {
            fixture.start_board();
            Some(fixture.session())
        } else {
            None
        };

        let control = if let Some(session) = session.as_ref() {
            let (status, body) = fixture.post_kickoff(session, CONTROL, CONTROL_WORKER);
            assert_eq!(status, 200, "valid HTTP control: {body}");
            serde_json::from_str::<Value>(&body).unwrap()["kickoff"].clone()
        } else {
            fixture
                .kickoff_rpc(CONTROL, CONTROL_WORKER)
                .expect("a concise opted-in issue must remain dispatchable")
        };
        assert_eq!(control["dispatched"], json!(true), "{control}");
        let control_id = control["message"].as_str().expect("control message id");
        assert_eq!(
            fixture.message_ids(CONTROL_WORKER, Some("dispatch")),
            vec![control_id.to_string()]
        );
        let store = crate::store::Store::open_side(&fixture.state.join("cadence.sqlite3"))
            .expect("observe the live fixture without restart recovery");
        assert!(store
            .context_pin(CONTROL_WORKER, control_id)
            .unwrap()
            .is_some());

        // Mandatory, ordinary canonical prose: no large acceptance, title,
        // request body, project document, attachment or frontmatter field.
        let mut narrative = String::from(
            "## Request\n\nRead input.csv and write complete-output.csv. \
             Keep every following record-specific requirement verbatim.\n",
        );
        let mut record = 0usize;
        while narrative.len() <= crate::store::ENQUEUE_BYTES + 1024 {
            narrative.push_str(&format!(
                "For input record {record:04}, preserve identifier CAD1846-{record:04} exactly.\n"
            ));
            record += 1;
        }
        assert!(narrative.len() > crate::store::ENQUEUE_BYTES);
        let body = format!("{narrative}\n## Acceptance\n\n- [ ] Produce the requested output.\n");
        let pm = Pm::at(&fixture.pm).unwrap();
        let create_issue = |id: &str| {
            write::new_issue(
                &pm,
                &fixture.repo,
                Some(PROJECT),
                "CAD-1846 complete request overflow",
                Some("P2"),
                None,
                &[],
                None,
                None,
                &[],
                Some(id),
                Some(body.as_str()),
                "cad1846-acceptance",
            )
            .expect("create a valid canonical issue through the tracker writer");
        };
        let bad_issue = "C1822-8";
        create_issue(bad_issue);
        let source = fs::read_to_string(fixture.issue_file(bad_issue)).unwrap();
        assert!(source.len() < 64 * 1024);
        let (_, canonical_body) = crate::issue::parse::parse_issue(&source).unwrap();
        assert!(canonical_body.contains(narrative.as_str()));
        let items = crate::issue::parse::acceptance_items(&canonical_body);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "Produce the requested output.");
        let acceptance_before = fixture.acceptance(bad_issue);

        // dispatch_send requires an authentic issue lane. Establish only
        // that prerequisite, without first admitting a kickoff for this issue.
        if !via_http {
            let args = crate::issue::start::StartArgs {
                repo: None,
                name: None,
                base: None,
                owner: Some(OVERFLOW_WORKER.to_string()),
                job: None,
                by: Some(GROUP.to_string()),
                take_over: None,
            };
            scoped(Asserted::Operator, || {
                crate::issue::start::run(&pm, bad_issue, &args, "operator", &fixture.state)
            })
            .expect("establish the native lane required by dispatch_send");
        }
        let message_id = "cad1846-complete-request-overflow";
        let request = json!({
            "alias": OVERFLOW_WORKER,
            "issue": bad_issue,
            "reply_to": GROUP,
            "message": message_id,
            "text": "Execute the complete assigned request."
        });
        let baseline = fixture.message_ids(OVERFLOW_WORKER, None);
        assert!(baseline.is_empty(), "the adversarial target must be fresh");
        let response = if let Some(session) = session.as_ref() {
            let (status, body) = fixture.post_kickoff(session, bad_issue, OVERFLOW_WORKER);
            assert_eq!(status, 400, "HTTP mandatory narrative overflow: {body}");
            Err(body)
        } else {
            fixture
                .try_op("dispatch_send", request.clone())
                .map_err(|error| error.to_string())
        };
        expect_refused(
            &fixture,
            bad_issue,
            OVERFLOW_WORKER,
            "mandatory-overflow",
            &baseline,
            response,
        );
        assert_eq!(fixture.acceptance(bad_issue), acceptance_before);
        let (_, after_body) =
            write::load_front(fixture.issue_file(bad_issue).parent().unwrap()).unwrap();
        assert_eq!(
            after_body, canonical_body,
            "refusal must not truncate the request"
        );
        let events = store.events_tail(OVERFLOW_WORKER, 1000).unwrap();
        assert!(
            events.len() < 1000,
            "the selection inventory must be complete"
        );
        assert!(
            events.iter().all(|event| {
                event.kind != crate::context_pack::PIN_EVENT
                    && event.kind != crate::context_pack::DISABLED_EVENT
            }),
            "a refused kickoff must admit neither a pin nor a disabled selection"
        );
        if !via_http {
            assert!(store.message(message_id).unwrap().is_none());
            assert!(store
                .context_pin(OVERFLOW_WORKER, message_id)
                .unwrap()
                .is_none());
        }

        // Explicit committed opt-out preserves legacy behavior for the same
        // oversized canonical request; it is not a blanket refusal control.
        let manifest = fixture.repo.join(MANIFEST);
        let enabled = fs::read_to_string(&manifest).unwrap();
        fs::write(
            &manifest,
            enabled.replacen("agent_delivery: true", "agent_delivery: false", 1),
        )
        .unwrap();
        commit_all(&fixture.repo, "CAD-1846 explicit legacy opt-out control");
        let accepted = if let Some(session) = session.as_ref() {
            let opt_out_issue = "C1822-9";
            create_issue(opt_out_issue);
            let (status, body) = fixture.post_kickoff(session, opt_out_issue, OVERFLOW_WORKER);
            assert_eq!(status, 200, "explicit HTTP opt-out control: {body}");
            let accepted = serde_json::from_str::<Value>(&body).unwrap()["kickoff"].clone();
            assert_eq!(accepted["dispatched"], json!(true), "{accepted}");
            accepted
        } else {
            let accepted = fixture
                .try_op("dispatch_send", request)
                .expect("explicit opt-out must admit the same legacy envelope");
            assert_eq!(accepted["message"], json!(message_id));
            assert_eq!(accepted["duplicate"], json!(false));
            accepted
        };
        let accepted_id = accepted["message"].as_str().expect("opt-out message id");
        assert_eq!(
            fixture.message_ids(OVERFLOW_WORKER, Some("dispatch")),
            vec![accepted_id.to_string()]
        );
        assert!(store.message(accepted_id).unwrap().is_some());
        assert!(store
            .context_pin(OVERFLOW_WORKER, accepted_id)
            .unwrap()
            .is_none());
    }
}

#[test]
fn cad1846_native_job_task_refuses_missing_pinned_required_document() {
    use crate::issue::start::{JobArgs, StartArgs};
    use sha2::{Digest, Sha256};

    const WORKER: &str = "ctxp-native-job";
    const CONTROL_MESSAGE: &str = "cad1846-native-job-control";
    const BAD_MESSAGE: &str = "cad1846-native-job-missing";
    const SPEC_PATH: &str = "docs/native-job-spec.md";
    const SPEC: &str = "Produce native-job-result.txt from tracked.txt.\n\
        Preserve the input bytes and report the commit produced.\n";

    let fixture = Fixture::new();
    let pm = Pm::at(&fixture.pm).expect("open isolated fixture PM");
    let repo = fixture.repo.canonicalize().unwrap();
    let spec = repo.join(SPEC_PATH);
    let enabled_manifest = manifest_yaml(
        PROJECT,
        &[("absent", ABSENT, "Absent required source", true)],
    );
    let disabled_manifest =
        enabled_manifest.replacen("agent_delivery: true", "agent_delivery: false", 1);
    fs::write(repo.join(MANIFEST), &disabled_manifest).unwrap();
    fs::write(&spec, SPEC).unwrap();
    commit_all(
        &repo,
        "CAD-1846 explicit opt-out with missing required source",
    );
    assert!(
        git(&repo, &["ls-tree", "HEAD", "--", ABSENT]).is_empty(),
        "the positive control must have the same missing tracked document"
    );

    // A real managed, prompt-capable endpoint. The fixture already seals
    // provider commands to false; do not replace that with a real provider.
    // Its failed open leaves the enabled worker detached before either
    // dispatch, so admission rows cannot race an adapter claim.
    fixture.op(
        "agent_register",
        json!({
            "alias": WORKER,
            "provider": "codex",
            "endpoint_kind": "managed",
            "role": "worker",
            "team_role": "dev",
            "model_policy": "provider_default",
            "cwd": repo,
            "params": json!({"upstream": GROUP}).to_string(),
        }),
    );
    fixture.wait_state(WORKER, "attention");
    let store = crate::store::Store::open_side(&fixture.state.join("cadence.sqlite3"))
        .expect("observe the served store without restart recovery");
    let worker = store.agent(WORKER).unwrap();
    assert!(worker.enabled, "the target must not be disabled");
    assert_eq!(worker.endpoint_kind, "managed");
    assert!(worker.endpoint.is_none());

    // Native issue start performs the normal spec read/hash, PM membership,
    // plan, claim and real worktree setup, then creates the issue-linked job
    // through its authenticated RPC. No task/binding rows are planted.
    let start_job = |issue: &str| {
        let started = scoped(Asserted::Operator, || {
            crate::issue::start::run(
                &pm,
                issue,
                &StartArgs {
                    repo: Some(repo.clone()),
                    name: None,
                    base: Some("main".to_string()),
                    owner: Some(WORKER.to_string()),
                    job: Some(JobArgs {
                        id: None,
                        pm: GROUP.to_string(),
                        spec: spec.clone(),
                        assignee: Some(WORKER.to_string()),
                        force: false,
                    }),
                    by: Some(GROUP.to_string()),
                    take_over: None,
                },
                "cad1846-acceptance",
                &fixture.state,
            )
        })
        .unwrap_or_else(|error| panic!("native job setup for {issue}: {error}"));
        assert!(
            Path::new(started["worktree"].as_str().unwrap()).is_dir(),
            "native job setup must create its real lane: {started}"
        );
        started
    };

    // Positive control comes first, so the baseline RED run still proves
    // that this native endpoint/setup can admit an explicitly opted-out job.
    let control_started = start_job(CONTROL);
    let control_task = control_started["task"].as_str().unwrap();
    let control = fixture.op(
        "task_dispatch",
        json!({
            "task": control_task,
            "by": GROUP,
            "message": CONTROL_MESSAGE,
        }),
    );
    let control_row = store.message(CONTROL_MESSAGE).unwrap().unwrap();
    let control_task_row = store.task(control_task).unwrap();
    eprintln!(
        "CAD-1846 native job opt-out control: {}",
        json!({
            "rpc": control,
            "task": control_task_row.to_json(),
            "message": {
                "id": control_row.id,
                "state": control_row.state,
                "source": control_row.source,
                "issue": control_row.issue,
                "task": control_row.task_id,
            },
        })
    );
    assert_eq!(control["message"], CONTROL_MESSAGE);
    assert_eq!(control["duplicate"], false);
    assert_eq!(control_row.state, "queued");
    assert_eq!(control_row.source, "job_dispatch");
    assert_eq!(control_row.issue.as_deref(), Some(CONTROL));
    assert_eq!(control_row.task_id.as_deref(), Some(control_task));
    assert_eq!(control_task_row.state, "dispatched");
    assert_eq!(control_task_row.revision, 1);
    assert_eq!(
        control_task_row.dispatch_message.as_deref(),
        Some(CONTROL_MESSAGE)
    );
    assert_eq!(store.messages_for_task(control_task).unwrap().len(), 1);
    assert!(store
        .context_pin(WORKER, CONTROL_MESSAGE)
        .unwrap()
        .is_none());

    // Only the committed opt-in changes. The required document remains
    // absent, while the spec stays readable and byte-identical.
    fs::write(repo.join(MANIFEST), &enabled_manifest).unwrap();
    commit_all(&repo, "CAD-1846 opt in with required source absent");
    let source_revision = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    assert!(
        git(&repo, &["ls-tree", &source_revision, "--", ABSENT]).is_empty(),
        "the required document must be absent at the immutable project revision"
    );
    let bad_started = start_job(MISSING_BAD);
    let task_id = bad_started["task"].as_str().unwrap();
    let job_id = bad_started["job"].as_str().unwrap();
    let lane = Path::new(bad_started["worktree"].as_str().unwrap());
    assert_eq!(
        git(lane, &["rev-parse", "HEAD"]).trim(),
        source_revision.as_str(),
        "the native lane must start at the same committed project snapshot"
    );
    let job = store.job(job_id).unwrap();
    assert_eq!(job.issue_id.as_deref(), Some(MISSING_BAD));
    assert_eq!(job.pm_alias, GROUP);
    assert_eq!(
        job.spec_sha256,
        Some(format!("{:x}", Sha256::digest(fs::read(&spec).unwrap())))
    );
    let task_before = store.task(task_id).unwrap();
    assert_eq!(task_before.state, "draft");
    assert_eq!(task_before.revision, 0);
    assert_eq!(task_before.assignee.as_deref(), Some(WORKER));
    assert!(task_before.dispatch_message.is_none());
    assert!(store.messages_for_task(task_id).unwrap().is_empty());
    let messages_before = fixture.message_ids(WORKER, None);
    let event_cursor = store
        .events_tail(WORKER, 1)
        .unwrap()
        .last()
        .map(|event| event.seq)
        .unwrap_or(0);

    let response = fixture
        .try_op(
            "task_dispatch",
            json!({"task": task_id, "by": GROUP, "message": BAD_MESSAGE}),
        )
        .map_err(|error| error.to_string());
    let task_after = store.task(task_id).unwrap().to_json();
    let messages_after = fixture.message_ids(WORKER, None);
    let task_messages = store.messages_for_task(task_id).unwrap();
    let new_events = store.events(WORKER, event_cursor, 100).unwrap();
    let job_events = store.job_events(job_id, 0, 100).unwrap();
    assert!(
        new_events.len() < 100,
        "worker observation must not be truncated"
    );
    assert!(
        job_events.len() < 100,
        "job observation must not be truncated"
    );
    let evidence = json!({
        "project_revision": source_revision,
        "required_document": ABSENT,
        "response": &response,
        "task_before": task_before.to_json(),
        "task_after": &task_after,
        "messages_before": &messages_before,
        "messages_after": &messages_after,
        "task_messages": task_messages.iter().map(|message| json!({
            "id": message.id,
            "state": message.state,
            "source": message.source,
            "issue": message.issue,
            "task": message.task_id,
        })).collect::<Vec<_>>(),
        "pin": store.context_pin(WORKER, BAD_MESSAGE).unwrap(),
        "worker_events": new_events.iter().map(|event| event.to_json()).collect::<Vec<_>>(),
        "job_events": job_events.iter().map(|event| event.to_json()).collect::<Vec<_>>(),
    });
    eprintln!("CAD-1846 native missing-context admission observation: {evidence}");

    assert!(
        response
            .as_ref()
            .err()
            .is_some_and(|error| { error.contains("context refused: missing-required-source") }),
        "native task dispatch must refuse for the required-context reason: {evidence}"
    );
    assert_eq!(
        task_after,
        task_before.to_json(),
        "refusal changed the task: {evidence}"
    );
    assert_eq!(
        messages_after, messages_before,
        "refusal changed the queue: {evidence}"
    );
    assert!(
        task_messages.is_empty(),
        "refusal admitted a task message: {evidence}"
    );
    assert!(
        store.message(BAD_MESSAGE).unwrap().is_none(),
        "refusal wrote a kickoff: {evidence}"
    );
    assert!(
        store.context_pin(WORKER, BAD_MESSAGE).unwrap().is_none(),
        "refusal wrote a context pin: {evidence}"
    );
    assert!(
        new_events.iter().all(|event| event.kind != "context_pin"),
        "refusal wrote a pin under another message id: {evidence}"
    );
    assert!(
        job_events
            .iter()
            .all(|event| event.kind != "task_dispatched"),
        "refusal recorded a dispatched transition: {evidence}"
    );
}

const CAD1846_JOB_GUARD_WORKER: &str = "ctxp-job-guard";

fn cad1846_job_guard_fixture(opted_in: bool) -> (Fixture, Value) {
    use crate::issue::start::{JobArgs, StartArgs};

    let fixture = Fixture::new();
    let spec = fixture.repo.join("docs/cad1846-guard-spec.md");
    fs::write(
        &spec,
        "Read tracked.txt and produce guard-result.txt without changing input bytes.\n",
    )
    .unwrap();
    fs::write(
        fixture.repo.join("docs/cad1846-guard-alt.md"),
        "Read tracked.txt and produce alternate-guard-result.txt.\n",
    )
    .unwrap();
    let manifest = manifest_yaml(PROJECT, &[("policy", POLICY, "Required policy", true)]);
    fs::write(
        fixture.repo.join(MANIFEST),
        if opted_in {
            manifest
        } else {
            manifest.replacen("agent_delivery: true", "agent_delivery: false", 1)
        },
    )
    .unwrap();
    commit_all(&fixture.repo, "CAD-1846 native guard fixture sources");
    fixture.op(
        "agent_register",
        json!({
            "alias": CAD1846_JOB_GUARD_WORKER,
            "provider": "codex",
            "endpoint_kind": "managed",
            "role": "worker",
            "team_role": "dev",
            "model_policy": "provider_default",
            "cwd": fixture.repo,
            "params": json!({"upstream": GROUP}).to_string(),
        }),
    );
    fixture.wait_state(CAD1846_JOB_GUARD_WORKER, "attention");
    let pm = Pm::at(&fixture.pm).unwrap();
    let started = scoped(Asserted::Operator, || {
        crate::issue::start::run(
            &pm,
            CONTROL,
            &StartArgs {
                repo: Some(fixture.repo.clone()),
                name: None,
                base: Some("main".to_string()),
                owner: Some(CAD1846_JOB_GUARD_WORKER.to_string()),
                job: Some(JobArgs {
                    id: None,
                    pm: GROUP.to_string(),
                    spec,
                    assignee: Some(CAD1846_JOB_GUARD_WORKER.to_string()),
                    force: false,
                }),
                by: Some(GROUP.to_string()),
                take_over: None,
            },
            "cad1846-acceptance",
            &fixture.state,
        )
    })
    .expect("establish the real native issue-linked job and lane");
    assert!(Path::new(started["worktree"].as_str().unwrap()).is_dir());
    (fixture, started)
}

#[test]
fn cad1846_planned_job_dispatch_refuses_changed_material_snapshot() {
    use crate::store::{DispatchPlan, IssueSelection};

    let (fixture, started) = cad1846_job_guard_fixture(false);
    let store = crate::store::Store::open_side(&fixture.state.join("cadence.sqlite3")).unwrap();
    let job_id = started["job"].as_str().unwrap();
    let control_task = started["task"].as_str().unwrap();
    let worker = CAD1846_JOB_GUARD_WORKER;
    assert!(store.agent(worker).unwrap().enabled);

    // This is the actual production kickoff renderer and an honest disabled
    // selection for the committed explicit opt-out, not a fabricated pin.
    let delivery = |plan: &DispatchPlan| {
        (
            crate::store::job_kickoff(
                &plan.job,
                &plan.task,
                plan.revision,
                &plan.message_id,
                &plan.worker.provider,
                &plan.worker.endpoint_kind,
                false,
            )
            .unwrap(),
            IssueSelection::Disabled {
                task: plan.task.id.clone(),
                reason: "the assigned project is not opted in to agent delivery".to_string(),
            },
        )
    };

    let control = store
        .plan_task_dispatch(control_task, None, Some("cad1846-snapshot-control"))
        .unwrap();
    let accepted = store
        .dispatch_task_planned(&control, Some(delivery(&control)), "operator", None, None)
        .expect("an unchanged native snapshot must be admitted");
    assert_eq!(accepted.1, control.message_id);
    assert_eq!(accepted.0.revision, control.revision);
    assert!(!accepted.2);
    assert_eq!(store.message(&accepted.1).unwrap().unwrap().state, "queued");

    // A live duplicate needs no new body/selection and keeps its revision.
    let duplicate_plan = store.plan_task_dispatch(control_task, None, None).unwrap();
    assert!(duplicate_plan.duplicate);
    let duplicate = store
        .dispatch_task_planned(&duplicate_plan, None, "operator", None, None)
        .expect("a still-live duplicate must return its original admission");
    assert!(duplicate.2);
    assert_eq!(duplicate.1, accepted.1);
    assert_eq!(duplicate.0.revision, accepted.0.revision);

    // If that accepted kickoff ceases to be live, a no-delivery retry may
    // not mint the next turn from the obsolete duplicate snapshot.
    fixture.op("task_cancel", json!({"task": control_task, "by": GROUP}));
    let ceased_task = store.task(control_task).unwrap().to_json();
    let ceased_messages = fixture.message_ids(worker, None);
    let ceased = store.dispatch_task_planned(&duplicate_plan, None, "operator", None, None);
    assert!(
        ceased.is_err(),
        "a ceased duplicate minted a new turn: {ceased:?}"
    );
    assert_eq!(store.task(control_task).unwrap().to_json(), ceased_task);
    assert_eq!(fixture.message_ids(worker, None), ceased_messages);
    assert_eq!(
        store.message(&accepted.1).unwrap().unwrap().state,
        "cancelled"
    );

    let alt_spec = fixture.repo.join("docs/cad1846-guard-alt.md");
    let foreign_repo = fixture._dir.path().join("other-repo");
    let foreign_sha = git(&foreign_repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let variants: Vec<(&str, &str, String)> = vec![
        (
            "tasks",
            "acceptance",
            "Produce a materially different result.".to_string(),
        ),
        (
            "tasks",
            "title",
            "A materially changed task objective".to_string(),
        ),
        ("tasks", "spec_path", alt_spec.display().to_string()),
        ("tasks", "worktree", foreign_repo.display().to_string()),
        ("tasks", "branch", "main".to_string()),
        ("tasks", "base_sha", foreign_sha),
        // The PM itself is a legitimate native group assignee. The planned
        // adapter must not silently override this changed stored assignment.
        ("tasks", "assignee", GROUP.to_string()),
        (
            "jobs",
            "title",
            "A materially changed job objective".to_string(),
        ),
        ("jobs", "spec_path", alt_spec.display().to_string()),
        ("jobs", "spec_sha256", "b".repeat(64)),
        ("jobs", "issue_id", MISSING_BAD.to_string()),
        ("jobs", "repo", foreign_repo.display().to_string()),
        ("jobs", "base_ref", "main".to_string()),
    ];

    for (index, (table, column, replacement)) in variants.into_iter().enumerate() {
        let task_id = format!("cad1846-stale-material-{index}");
        fixture.op(
            "task_new",
            json!({
                "job": job_id,
                "task": task_id,
                "title": "Original scoped task",
                "assignee": worker,
                "acceptance": "Produce guard-result.txt.",
                "worktree": started["worktree"],
                "branch": started["branch"],
                "base_sha": started["base"]["sha"],
            }),
        );
        let message_id = format!("cad1846-stale-material-message-{index}");
        let plan = store
            .plan_task_dispatch(&task_id, None, Some(&message_id))
            .unwrap();
        let prepared_delivery = delivery(&plan);
        let row_id = if table == "tasks" {
            task_id.as_str()
        } else {
            job_id
        };
        let update_sql = format!("UPDATE {table} SET {column}=? WHERE id=?");
        let select_sql = format!("SELECT {column} FROM {table} WHERE id=?");

        // Deterministic consume-time race injection at the actual Store seam.
        // The fixture write does not update timestamps: comparing only updated
        // cannot stand in for fencing the captured material inputs.
        let previous: Option<String> = store
            .fixture_write(|tx| {
                let previous: Option<String> =
                    tx.query_row(&select_sql, [row_id], |row| row.get(0))?;
                assert_ne!(previous.as_deref(), Some(replacement.as_str()));
                tx.execute(&update_sql, rusqlite::params![&replacement, row_id])?;
                Ok(previous)
            })
            .unwrap();
        let task_before = store.task(&task_id).unwrap().to_json();
        let job_before = store.job(job_id).unwrap().to_json();
        let messages_before = fixture.message_ids(worker, None);
        let cursor = store
            .events_tail(worker, 1)
            .unwrap()
            .last()
            .map(|e| e.seq)
            .unwrap_or(0);
        let result =
            store.dispatch_task_planned(&plan, Some(prepared_delivery), "operator", None, None);
        let task_after = store.task(&task_id).unwrap().to_json();
        let job_after = store.job(job_id).unwrap().to_json();
        let events = store.events(worker, cursor, 1000).unwrap();
        assert!(events.len() < 1000);
        let scoped_events = store.job_events(job_id, 0, 1000).unwrap();
        assert!(scoped_events.len() < 1000);
        eprintln!(
            "CAD-1846 stale {table}.{column}: result={result:?}; task={task_after}; job={job_after}"
        );
        assert!(
            result.is_err(),
            "stale {table}.{column} admitted old delivery: {result:?}"
        );
        assert_eq!(task_after, task_before, "stale refusal changed the task");
        assert_eq!(job_after, job_before, "stale refusal changed the job");
        assert_eq!(fixture.message_ids(worker, None), messages_before);
        assert!(store.messages_for_task(&task_id).unwrap().is_empty());
        assert!(store.message(&message_id).unwrap().is_none());
        assert!(store.context_pin(worker, &message_id).unwrap().is_none());
        assert!(store
            .context_disabled(worker, &message_id)
            .unwrap()
            .is_none());
        assert!(events
            .iter()
            .all(|e| { e.kind != "context_pin" && e.kind != "context_disabled" }));
        assert!(scoped_events
            .iter()
            .all(|e| { e.kind != "task_dispatched" || e.payload["task"] != task_id }));
        store
            .fixture_write(|tx| {
                tx.execute(&update_sql, rusqlite::params![previous, row_id])?;
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn cad1846_native_job_task_refuses_scope_outside_its_issue_lane() {
    use crate::issue::start::StartArgs;

    let (fixture, started) = cad1846_job_guard_fixture(true);
    let worker = CAD1846_JOB_GUARD_WORKER;
    let pm = Pm::at(&fixture.pm).unwrap();
    let store = crate::store::Store::open_side(&fixture.state.join("cadence.sqlite3")).unwrap();
    let job_id = started["job"].as_str().unwrap();
    let control_task = started["task"].as_str().unwrap();
    let spec = fixture
        .repo
        .join("docs/cad1846-guard-spec.md")
        .canonicalize()
        .unwrap();
    assert!(store.agent(worker).unwrap().enabled);

    // Complete required context, real native scope and readable spec must
    // admit. Do not require a pin here: pin delivery is a separate rule, and
    // must not make this scope oracle RED for an unrelated missing feature.
    assert!(!git(&fixture.repo, &["show", "HEAD:docs/policy.md"]).is_empty());
    assert!(!fs::read_to_string(&spec).unwrap().is_empty());
    let control = fixture.op(
        "task_dispatch",
        json!({"task": control_task, "by": GROUP, "message": "cad1846-scope-control"}),
    );
    assert_eq!(control["duplicate"], false);
    assert_eq!(control["message"], "cad1846-scope-control");
    assert_eq!(
        store
            .message("cad1846-scope-control")
            .unwrap()
            .unwrap()
            .state,
        "queued"
    );
    assert_eq!(store.task(control_task).unwrap().revision, 1);

    let foreign_repo = fixture._dir.path().join("other-repo");
    set_sources(
        &foreign_repo,
        OTHER_PROJECT,
        &[("policy", POLICY, "Required policy", true)],
        &[(POLICY, "Complete foreign-project required policy.\n")],
    );
    write::new_issue(
        &pm,
        &foreign_repo,
        Some(OTHER_PROJECT),
        "CAD-1846 foreign native lane",
        Some("P2"),
        None,
        &[],
        None,
        None,
        &[],
        Some("O1822-1"),
        Some("Create a real foreign lane.\n\n## Acceptance\n\n- [ ] Keep its scope separate.\n"),
        "cad1846-acceptance",
    )
    .unwrap();
    let start_lane = |issue: &str, repo: &Path| {
        scoped(Asserted::Operator, || {
            crate::issue::start::run(
                &pm,
                issue,
                &StartArgs {
                    repo: Some(repo.to_path_buf()),
                    name: None,
                    base: Some("main".to_string()),
                    owner: Some(worker.to_string()),
                    job: None,
                    by: Some(GROUP.to_string()),
                    take_over: None,
                },
                "cad1846-acceptance",
                &fixture.state,
            )
        })
        .expect("create a contradictory candidate through real native issue start")
    };
    let foreign_lane = start_lane("O1822-1", &foreign_repo);
    let sibling_lane = start_lane(MISSING_BAD, &fixture.repo);

    for (index, (case, candidate)) in [
        ("foreign-project", foreign_lane),
        ("other-issue-same-project", sibling_lane),
    ]
    .into_iter()
    .enumerate()
    {
        let lane = Path::new(candidate["worktree"].as_str().unwrap());
        assert!(lane.is_dir());
        assert_ne!(candidate["worktree"], started["worktree"]);
        assert_eq!(
            git(lane, &["rev-parse", "HEAD"]).trim(),
            candidate["base"]["sha"].as_str().unwrap()
        );
        assert!(!git(lane, &["show", "HEAD:docs/policy.md"]).is_empty());
        let task_id = format!("cad1846-wrong-scope-{index}");
        let message_id = format!("cad1846-wrong-scope-message-{index}");
        // task_new is the supported native scope-bearing mutation. The actor
        // is authorized; the contradictory scope must still fail admission.
        fixture.op(
            "task_new",
            json!({
                "job": job_id,
                "task": task_id,
                "title": "Scope-bound job task",
                "assignee": worker,
                "spec": spec,
                "acceptance": "Produce guard-result.txt without changing input bytes.",
                "worktree": candidate["worktree"],
                "branch": candidate["branch"],
                "base_sha": candidate["base"]["sha"],
            }),
        );
        let before = store.task(&task_id).unwrap().to_json();
        let messages_before = fixture.message_ids(worker, None);
        let cursor = store
            .events_tail(worker, 1)
            .unwrap()
            .last()
            .map(|e| e.seq)
            .unwrap_or(0);
        let response = fixture
            .try_op(
                "task_dispatch",
                json!({"task": task_id, "by": GROUP, "message": message_id}),
            )
            .map_err(|error| error.to_string());
        let after = store.task(&task_id).unwrap().to_json();
        let events = store.events(worker, cursor, 1000).unwrap();
        let job_events = store.job_events(job_id, 0, 1000).unwrap();
        assert!(events.len() < 1000 && job_events.len() < 1000);
        let evidence = json!({
            "case": case,
            "candidate": candidate,
            "actual_issue_lane": started["worktree"],
            "response": &response,
            "task_before": &before,
            "task_after": &after,
            "messages_before": &messages_before,
            "messages_after": fixture.message_ids(worker, None),
            "pin": store.context_pin(worker, &message_id).unwrap(),
            "events": events.iter().map(|e| e.to_json()).collect::<Vec<_>>(),
        });
        eprintln!("CAD-1846 native scope observation: {evidence}");
        assert!(
            response
                .as_ref()
                .err()
                .is_some_and(|error| { error.contains("context refused: scope-conflict") }),
            "contradictory native scope was not refused for scope-conflict: {evidence}"
        );
        assert_eq!(
            after, before,
            "scope refusal changed task state: {evidence}"
        );
        assert_eq!(fixture.message_ids(worker, None), messages_before);
        assert!(store.messages_for_task(&task_id).unwrap().is_empty());
        assert!(store.message(&message_id).unwrap().is_none());
        assert!(store.context_pin(worker, &message_id).unwrap().is_none());
        assert!(store
            .context_disabled(worker, &message_id)
            .unwrap()
            .is_none());
        assert!(
            events
                .iter()
                .all(|e| { e.kind != "context_pin" && e.kind != "context_disabled" }),
            "scope refusal admitted selection metadata: {evidence}"
        );
        assert!(
            job_events
                .iter()
                .all(|e| { e.kind != "task_dispatched" || e.payload["task"] != task_id }),
            "scope refusal recorded a dispatched transition: {evidence}"
        );
    }
}

#[test]
fn cad1846_native_job_spec_refuses_external_file_and_own_lane_symlink_escape() {
    use std::os::unix::fs::symlink;

    let (fixture, started) = cad1846_job_guard_fixture(true);
    let worker = CAD1846_JOB_GUARD_WORKER;
    let store = crate::store::Store::open_side(&fixture.state.join("cadence.sqlite3")).unwrap();
    let job_id = started["job"].as_str().unwrap();
    let control_task = started["task"].as_str().unwrap();
    let own_lane = Path::new(started["worktree"].as_str().unwrap())
        .canonicalize()
        .unwrap();
    let repo = fixture.repo.canonicalize().unwrap();
    let issue_dir = fixture
        .issue_file(CONTROL)
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap();
    assert!(store.agent(worker).unwrap().enabled);
    assert_eq!(store.agent(worker).unwrap().endpoint_kind, "managed");

    // This real native control has valid issue/job/lane relationships and
    // the readable tracked spec established by the shared fixture. A pin
    // proves admission used the opted-in full-context path, not legacy mail.
    let control_message = "cad1846-owned-spec-control";
    let control = fixture.op(
        "task_dispatch",
        json!({"task": control_task, "by": GROUP, "message": control_message}),
    );
    assert_eq!(control["message"], control_message);
    assert_eq!(control["duplicate"], false);
    assert_eq!(
        store.message(control_message).unwrap().unwrap().state,
        "queued"
    );
    assert_eq!(store.task(control_task).unwrap().revision, 1);
    let control_pin = store
        .context_pin(worker, control_message)
        .unwrap()
        .expect("the valid opted-in tracked spec must produce a context pin");
    assert!(control_pin["manifest"]["task"]["request"]
        .as_str()
        .unwrap()
        .contains("Read tracked.txt and produce guard-result.txt without changing input bytes."));

    let add_task = |task_id: &str, spec_path: &Path| {
        fixture.op(
            "task_new",
            json!({
                "job": job_id,
                "task": task_id,
                "title": "Spec ownership acceptance",
                "assignee": worker,
                "spec": spec_path,
                "acceptance": "Produce the requested artifact without changing the input.",
                "worktree": started["worktree"],
                "branch": started["branch"],
                "base_sha": started["base"]["sha"],
            }),
        )
    };

    // An untracked spec inside this task's OWN genuine issue lane is an
    // allowed source. Requiring every spec to be tracked would be a blanket
    // restriction outside the operator's approved source policy.
    const OWN_SPEC_BODY: &str = "Read tracked.txt and write own-lane-output.txt.\n\
        Preserve CAD1846_OWN_UNTRACKED_SPEC_SENTINEL exactly.\n";
    let own_spec = own_lane.join("cad1846-own-untracked-spec.md");
    fs::write(&own_spec, OWN_SPEC_BODY).unwrap();
    assert!(git(
        &own_lane,
        &["ls-tree", "HEAD", "--", "cad1846-own-untracked-spec.md"],
    )
    .is_empty());
    let own_task = "cad1846-own-untracked-spec-task";
    let own_message = "cad1846-own-untracked-spec-message";
    add_task(own_task, &own_spec);
    let own_control = fixture.op(
        "task_dispatch",
        json!({"task": own_task, "by": GROUP, "message": own_message}),
    );
    assert_eq!(own_control["message"], own_message);
    assert_eq!(own_control["duplicate"], false);
    assert_eq!(store.message(own_message).unwrap().unwrap().state, "queued");
    assert_eq!(store.task(own_task).unwrap().revision, 1);
    let own_pin = store
        .context_pin(worker, own_message)
        .unwrap()
        .expect("an allowed own-lane untracked spec must be pinned");
    assert!(own_pin["manifest"]["task"]["request"]
        .as_str()
        .unwrap()
        .contains(OWN_SPEC_BODY));

    // Harmless, fixture-owned text outside ALL allowed source roots. Never
    // use an actual credential, private host file, or another user's data.
    let external = fixture
        ._dir
        .path()
        .join("cad1846-external-sentinel-spec.md");
    fs::write(
        &external,
        "Write external-output.txt.\nCAD1846_HARMLESS_EXTERNAL_SPEC_SENTINEL\n",
    )
    .unwrap();
    let external = external.canonicalize().unwrap();
    assert!(!external.starts_with(&repo));
    assert!(!external.starts_with(&own_lane));
    assert!(!external.starts_with(&issue_dir));
    let escaped_link = own_lane.join("cad1846-escaped-spec.md");
    symlink(&external, &escaped_link).unwrap();
    assert_eq!(escaped_link.canonicalize().unwrap(), external);

    // Both requests keep the authentic primary job/issue/lane and use task
    // spec overrides, so a parent job-SHA mismatch cannot mask ownership.
    // The second spec's lexical path is in the correct lane, but its real
    // target is the same disallowed external fixture file.
    for (index, (case, spec_path)) in [
        ("external-host-file", external.as_path()),
        ("own-lane-symlink-escape", escaped_link.as_path()),
    ]
    .into_iter()
    .enumerate()
    {
        let task_id = format!("cad1846-spec-ownership-task-{index}");
        let message_id = format!("cad1846-spec-ownership-message-{index}");
        add_task(&task_id, spec_path);
        let before = store.task(&task_id).unwrap().to_json();
        assert_eq!(before["state"], "draft");
        assert_eq!(before["revision"], 0);
        assert!(before["dispatch_message"].is_null());
        let messages_before = fixture.message_ids(worker, None);
        let cursor = store
            .events_tail(worker, 1)
            .unwrap()
            .last()
            .map(|event| event.seq)
            .unwrap_or(0);
        let response = fixture
            .try_op(
                "task_dispatch",
                json!({"task": task_id, "by": GROUP, "message": message_id}),
            )
            .map_err(|error| error.to_string());
        let after = store.task(&task_id).unwrap().to_json();
        let messages_after = fixture.message_ids(worker, None);
        let events = store.events(worker, cursor, 1000).unwrap();
        let job_events = store.job_events(job_id, 0, 1000).unwrap();
        assert!(events.len() < 1000 && job_events.len() < 1000);
        let evidence = json!({
            "case": case,
            "requested_spec": spec_path,
            "real_spec": spec_path.canonicalize().unwrap(),
            "actual_issue_lane": &own_lane,
            "response": &response,
            "task_before": &before,
            "task_after": &after,
            "messages_before": &messages_before,
            "messages_after": &messages_after,
            "pin": store.context_pin(worker, &message_id).unwrap(),
            "disabled": store.context_disabled(worker, &message_id).unwrap(),
            "worker_events": events.iter().map(|event| event.to_json()).collect::<Vec<_>>(),
            "job_events": job_events.iter().map(|event| event.to_json()).collect::<Vec<_>>(),
        });
        eprintln!("CAD-1846 native spec ownership observation: {evidence}");
        assert!(
            response
                .as_ref()
                .err()
                .is_some_and(|error| { error.contains("context refused: scope-conflict") }),
            "disallowed spec ownership was not refused for scope-conflict: {evidence}"
        );
        assert_eq!(
            after, before,
            "ownership refusal changed the task: {evidence}"
        );
        assert_eq!(
            messages_after, messages_before,
            "ownership refusal changed the queue: {evidence}"
        );
        assert!(store.messages_for_task(&task_id).unwrap().is_empty());
        assert!(store.message(&message_id).unwrap().is_none());
        assert!(store.context_pin(worker, &message_id).unwrap().is_none());
        assert!(store
            .context_disabled(worker, &message_id)
            .unwrap()
            .is_none());
        assert!(
            events
                .iter()
                .all(|event| { event.kind != "context_pin" && event.kind != "context_disabled" }),
            "ownership refusal admitted selection metadata: {evidence}"
        );
        assert!(
            job_events.iter().all(|event| {
                event.kind != "task_dispatched" || event.payload["task"] != task_id
            }),
            "ownership refusal recorded a dispatched transition: {evidence}"
        );
    }
}

#[test]
fn cad1846_task_bound_followup_cannot_adopt_an_undispatched_siblings_context() {
    use crate::issue::start::{JobArgs, StartArgs};

    let fixture = Fixture::new();
    // Fixture's stock CONTROL_WORKER has no params.upstream, and the native
    // agent_set allowlist explicitly rejects changing upstream. Register a
    // separate live fake through the supported native registration API rather
    // than weakening the group guard or planting membership rows. This uses
    // the same fake endpoint/session-generation behavior as CONTROL_WORKER.
    const WORKER: &str = "ctxp-job-followup";
    const TASK_B: &str = "cad1846-undispatched-sibling-b";
    const KICKOFF_A: &str = "cad1846-followup-kickoff-a";
    const SAME_A: &str = "cad1846-followup-same-a";
    let spec_a = fixture.repo.join("docs/cad1846-followup-a.md");
    let spec_b = fixture.repo.join("docs/cad1846-followup-b.md");
    fs::write(
        &spec_a,
        "Read tracked.txt and write sibling-a-result.txt.\n",
    )
    .unwrap();
    fs::write(
        &spec_b,
        "Read tracked.txt and write sibling-b-result.txt.\n",
    )
    .unwrap();
    commit_all(&fixture.repo, "CAD-1846 tracked sibling task specs");
    fixture.op(
        "agent_register",
        json!({
            "alias": WORKER,
            "provider": "fake",
            "endpoint_kind": "fake",
            "role": "worker",
            "team_role": "dev",
            "cwd": fixture.repo,
            "params": json!({"fake_generation": true, "upstream": GROUP}).to_string(),
        }),
    );
    fixture.wait_state(WORKER, "idle");
    let pm = Pm::at(&fixture.pm).unwrap();
    let started = scoped(Asserted::Operator, || {
        crate::issue::start::run(
            &pm,
            CONTROL,
            &StartArgs {
                repo: Some(fixture.repo.clone()),
                name: None,
                base: Some("main".to_string()),
                owner: Some(WORKER.to_string()),
                job: Some(JobArgs {
                    id: None,
                    pm: GROUP.to_string(),
                    spec: spec_a,
                    assignee: Some(WORKER.to_string()),
                    force: false,
                }),
                by: Some(GROUP.to_string()),
                take_over: None,
            },
            "cad1846-acceptance",
            &fixture.state,
        )
    })
    .expect("create the authentic issue-linked job and lane");
    let job_id = started["job"].as_str().unwrap();
    let task_a = started["task"].as_str().unwrap();
    let store = crate::store::Store::open_side(&fixture.state.join("cadence.sqlite3")).unwrap();

    // Wait for actual native supply receipts, never infer supply from an
    // enqueue response or a fake echo. The fake produces ordinary completed
    // turns without a SHA directive: kickoff A reaches review, which is NOT
    // terminal for compose_task_message; adhoc messages do not drive task QA.
    let supplied = |message: &str| {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let events = store.events_tail(WORKER, 1000).unwrap();
            assert!(events.len() < 1000, "supply observation must be complete");
            if let Some(event) = events.iter().find(|event| {
                event.kind == "context_supplied" && event.payload["message"] == message
            }) {
                return event.payload.clone();
            }
            if let Some(event) = events.iter().find(|event| {
                event.kind == "context_refused" && event.payload["message"] == message
            }) {
                panic!("legitimate task supply was refused: {}", event.payload);
            }
            assert!(
                Instant::now() < deadline,
                "no confirmed supply for {message}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let settled = |message: &str| {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let row = store
                .message(message)
                .unwrap()
                .expect("native message exists");
            if matches!(
                row.state.as_str(),
                "completed" | "failed" | "interrupted" | "cancelled"
            ) {
                return row;
            }
            assert!(
                Instant::now() < deadline,
                "native turn did not settle: {}",
                row.to_json()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };

    let kickoff = fixture.op(
        "task_dispatch",
        json!({"task": task_a, "by": GROUP, "message": KICKOFF_A}),
    );
    assert_eq!(kickoff["message"], KICKOFF_A);
    let first = supplied(KICKOFF_A);
    assert_eq!(settled(KICKOFF_A).state, "completed");
    fixture.wait_state(WORKER, "idle");
    assert_eq!(first["task"], task_a);
    assert_eq!(first["pin"], KICKOFF_A);
    assert!(first["appended_bytes"].as_u64().unwrap() > 0);
    assert_eq!(store.task(task_a).unwrap().state, "review");
    let conversation = first["conversation"].as_str().unwrap().to_string();
    let live = fixture.op("agent_show", json!({"alias": WORKER}));
    assert_eq!(first["session"], live["agent"]["thread_id"]);
    assert_eq!(first["generation"], live["agent"]["generation"]);
    let activation_a = store
        .last_context_activation(WORKER, &conversation)
        .unwrap()
        .expect("the confirmed kickoff must establish its own active binding");
    assert_eq!(activation_a["task"], task_a);
    assert_eq!(activation_a["pin"], KICKOFF_A);

    // Positive control: an explicitly task-bound adhoc message for the SAME
    // assigned task must complete in the confirmed session with zero repeated
    // mandatory source-body bytes. No second dispatch is performed.
    fixture.op(
        "agent_send",
        json!({
            "alias": WORKER,
            "task": task_a,
            "message": SAME_A,
            "text": "Continue the already assigned task without changing its contract.",
        }),
    );
    let retained = supplied(SAME_A);
    assert_eq!(settled(SAME_A).state, "completed");
    fixture.wait_state(WORKER, "idle");
    assert_eq!(retained["task"], task_a);
    assert_eq!(retained["pin"], first["pin"]);
    assert_eq!(retained["session"], first["session"]);
    assert_eq!(retained["generation"], first["generation"]);
    assert_eq!(retained["appended_bytes"], 0);
    assert_eq!(
        store
            .last_context_activation(WORKER, &conversation)
            .unwrap(),
        Some(activation_a.clone())
    );

    // B is a legitimate sibling with the SAME issue/job/assignee/lane, but
    // distinct spec and acceptance. Creating it does not dispatch it, pin it,
    // or authorize borrowing A's currently active task contract.
    fixture.op(
        "task_new",
        json!({
            "job": job_id,
            "task": TASK_B,
            "title": "Undispatched sibling B",
            "assignee": WORKER,
            "spec": spec_b,
            "acceptance": "Write sibling-b-result.txt according to sibling B's spec.",
            "worktree": started["worktree"],
            "branch": started["branch"],
            "base_sha": started["base"]["sha"],
        }),
    );
    let task_b_before = store.task(TASK_B).unwrap().to_json();
    assert_eq!(task_b_before["state"], "draft");
    assert_eq!(task_b_before["revision"], 0);
    assert!(task_b_before["dispatch_message"].is_null());
    assert!(store.messages_for_task(TASK_B).unwrap().is_empty());
    assert_eq!(
        store
            .last_context_activation(WORKER, &conversation)
            .unwrap(),
        Some(activation_a.clone())
    );

    // The source tag is not task identity: both the default native send and
    // a permitted caller tag must respect explicit recipient-task binding.
    for (index, source) in [None, Some("task-note")].into_iter().enumerate() {
        let message_id = format!("cad1846-followup-wrong-sibling-{index}");
        let cursor = store
            .events_tail(WORKER, 1)
            .unwrap()
            .last()
            .map(|event| event.seq)
            .unwrap_or(0);
        let supply_before = store.last_context_supply(WORKER, &conversation).unwrap();
        let mut request = json!({
            "alias": WORKER,
            "task": TASK_B,
            "message": message_id,
            "text": "Continue explicitly named sibling B, not sibling A.",
        });
        if let Some(source) = source {
            request["source"] = json!(source);
        }
        fixture.op("agent_send", request);
        let row = settled(&message_id);
        fixture.wait_state(WORKER, "idle");
        // On the known bad baseline a completed turn carries A's confirmed
        // supply. Wait for that receipt so RED records the actual adoption,
        // rather than racing completion against its later receipt write.
        if row.state == "completed" {
            supplied(&message_id);
        }
        let events = store.events(WORKER, cursor, 1000).unwrap();
        let job_events = store.job_events(job_id, 0, 1000).unwrap();
        assert!(events.len() < 1000 && job_events.len() < 1000);
        let activation_after = store
            .last_context_activation(WORKER, &conversation)
            .unwrap();
        let evidence = json!({
            "message": row.to_json(),
            "task_a": task_a,
            "explicit_task_b": TASK_B,
            "task_b_before": &task_b_before,
            "task_b_after": store.task(TASK_B).unwrap().to_json(),
            "activation_before": &activation_a,
            "activation_after": &activation_after,
            "events": events.iter().map(|event| event.to_json()).collect::<Vec<_>>(),
        });
        eprintln!("CAD-1846 explicit sibling followup observation: {evidence}");
        assert_eq!(row.task_id.as_deref(), Some(TASK_B));
        assert_eq!(
            row.state, "failed",
            "wrong sibling reached provider instead of refusal: {evidence}"
        );
        assert!(
            row.error
                .as_deref()
                .is_some_and(|error| { error.contains("context refused: scope-conflict") }),
            "wrong sibling did not fail for context scope: {evidence}"
        );
        assert!(
            row.turn_id.is_none(),
            "a refused sibling started a provider turn: {evidence}"
        );
        assert!(
            events.iter().any(|event| {
                event.kind == "context_refused"
                    && event.payload["message"] == message_id
                    && event.payload["code"] == "scope-conflict"
            }),
            "no native scope refusal event: {evidence}"
        );
        assert!(
            events.iter().all(|event| {
                event.kind != "context_supplied" || event.payload["message"] != message_id
            }),
            "wrong sibling was supplied A or B context: {evidence}"
        );
        assert!(
            events.iter().all(|event| !matches!(
                event.kind.as_str(),
                "context_pin" | "context_disabled" | "context_activated" | "context_deactivated"
            )),
            "adhoc sibling refusal admitted or retargeted context: {evidence}"
        );
        assert!(store.context_pin(WORKER, &message_id).unwrap().is_none());
        assert!(store
            .context_disabled(WORKER, &message_id)
            .unwrap()
            .is_none());
        assert_eq!(activation_after, Some(activation_a.clone()));
        assert_eq!(
            store.last_context_supply(WORKER, &conversation).unwrap(),
            supply_before
        );
        assert_eq!(store.task(TASK_B).unwrap().to_json(), task_b_before);
        assert!(
            job_events.iter().all(|event| {
                event.kind != "task_dispatched" || event.payload["task"] != TASK_B
            }),
            "an adhoc sibling followup silently dispatched B: {evidence}"
        );
    }
}

#[test]
fn cad1846_plain_kickoff_cannot_suppress_explicit_issue_job_dispatch() {
    use crate::issue::dispatch::{DispatchArgs, DispatchOrigin};

    const WORKER: &str = "ctxp-entrypoint-parity";
    const SPEC_PATH: &str = "docs/cad1846-entrypoint-spec.md";
    const QA_PATH: &str = "docs/cad1846-entrypoint-qa.md";
    const SPEC: &str = "Read tracked.txt and write entrypoint-job-result.csv.\n\
        Preserve the input row order and every identifier byte-for-byte.\n\
        Emit exactly the columns identifier,source_revision.\n";
    const DEV_POLICY: &str = "CAD1846_DEV_POLICY: preserve the input without normalization.\n";
    const QA_POLICY: &str = "CAD1846_QA_ONLY_POLICY: not instructions for the assigned dev.\n";

    let fixture = Fixture::new();
    let spec = fixture.repo.join(SPEC_PATH);
    fs::write(&spec, SPEC).unwrap();
    fs::write(fixture.repo.join(POLICY), DEV_POLICY).unwrap();
    fs::write(fixture.repo.join(QA_PATH), QA_POLICY).unwrap();
    fs::write(
        fixture.repo.join(MANIFEST),
        format!(
            "schema: 1\nproject: {PROJECT}\nagent_delivery: true\ndocuments:\n\
             \x20 - id: dev-policy\n    kind: scope\n    path: {POLICY}\n\
             \x20   title: Required dev policy\n    required: true\n    roles: [dev]\n\
             \x20 - id: qa-policy\n    kind: scope\n    path: {QA_PATH}\n\
             \x20   title: QA-only policy\n    required: false\n    roles: [qa]\n"
        ),
    )
    .unwrap();
    commit_all(
        &fixture.repo,
        "CAD-1846 tracked entrypoint spec and role-scoped policies",
    );
    fixture.op(
        "agent_register",
        json!({
            "alias": WORKER,
            "provider": "fake",
            "endpoint_kind": "fake",
            "role": "worker",
            "team_role": "dev",
            "cwd": fixture.repo,
            "params": json!({"fake_generation": true, "upstream": GROUP}).to_string(),
        }),
    );
    fixture.wait_state(WORKER, "idle");
    let store = crate::store::Store::open_side(&fixture.state.join("cadence.sqlite3")).unwrap();
    let settled = |message: &str| {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let row = store.message(message).unwrap().expect("native kickoff row");
            if matches!(
                row.state.as_str(),
                "completed" | "failed" | "interrupted" | "cancelled" | "unknown"
            ) {
                assert_eq!(row.state, "completed", "kickoff failed: {}", row.to_json());
                return row;
            }
            assert!(
                Instant::now() < deadline,
                "kickoff did not settle: {}",
                row.to_json()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };

    // Real native positive control, with the group membership that --job
    // subsequently requires. No fixture row or sender identity is planted.
    let plain = fixture.kickoff_rpc(CONTROL, WORKER).unwrap();
    assert_eq!(plain["dispatched"], json!(true), "{plain}");
    let plain_id = plain["message"].as_str().unwrap().to_string();
    let plain_row = settled(&plain_id);
    fixture.wait_state(WORKER, "idle");
    let plain_supply = context_event(&fixture, WORKER, "context_supplied", &plain_id);
    assert_eq!(plain_supply["task"], CONTROL);
    assert_eq!(plain_row.source, "dispatch");
    assert_eq!(plain_row.issue.as_deref(), Some(CONTROL));
    assert!(plain_row.task_id.is_none());
    assert_eq!(plain_row.worktree.as_deref(), plain["worktree"].as_str());
    let plain_before = plain_row.to_json();
    let plain_pin = fixture.context_pin(WORKER, &plain_id);
    let plain_manifest: crate::context_pack::Manifest =
        serde_json::from_value(plain_pin["manifest"].clone()).unwrap();
    assert_eq!(plain_manifest.task.task, CONTROL);
    assert_eq!(plain_manifest.role, "dev");
    assert_eq!(
        plain_manifest
            .documents
            .iter()
            .map(|document| document.id.as_str())
            .collect::<Vec<_>>(),
        vec!["dev-policy"]
    );
    assert!(store.jobs(None, true).unwrap().is_empty());

    // Exercise exactly the public explicit `dispatch --job --spec` path.
    // The plain completed row is still present on the SAME issue/lane/seat.
    let pm = Pm::at(&fixture.pm).unwrap();
    let args = DispatchArgs {
        to: WORKER.to_string(),
        note: None,
        name: None,
        base: None,
        repo: None,
        reply_to: Some(GROUP.to_string()),
        summary: None,
        job_spec: Some(spec.clone()),
        no_lessons: false,
        force: false,
        take_over: None,
    };
    let dispatch_job = || {
        scoped(Asserted::Operator, || {
            crate::issue::dispatch::run(
                &pm,
                CONTROL,
                &args,
                "operator",
                &fixture.state,
                Some(GROUP),
                DispatchOrigin::Explicit,
            )
        })
        .expect("explicit job dispatch must not adopt the plain kickoff")
    };
    let dispatched = dispatch_job();
    let jobs_after_attempt = store.jobs(None, true).unwrap();
    let native_jobs = jobs_after_attempt
        .iter()
        .map(|job| {
            let tasks = store.tasks_for_job(&job.id).unwrap();
            json!({
                "job": job.to_json(),
                "tasks": tasks.iter().map(|task| task.to_json()).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    let evidence = json!({
        "requested_entry_point": "explicit issue dispatch --job --spec",
        "receipt": dispatched,
        "original_plain_message": plain_id,
        "native_jobs_after_attempt": native_jobs,
        "worker_message_ids": fixture.message_ids(WORKER, None),
    });
    eprintln!("CAD-1846 plain-to-job entrypoint observation: {evidence}");
    assert_eq!(
        store.message(&plain_id).unwrap().unwrap().to_json(),
        plain_before,
        "the job-mode attempt changed the completed plain message: {evidence}"
    );
    assert_eq!(
        fixture.context_pin(WORKER, &plain_id),
        plain_pin,
        "the job-mode attempt retargeted the original plain pin: {evidence}"
    );
    assert_eq!(
        dispatched["dispatched"],
        json!(true),
        "a reported plain kickoff is not a job-mode duplicate: {evidence}"
    );
    let job_message = dispatched["message"].as_str().unwrap().to_string();
    assert_ne!(
        job_message, plain_id,
        "job mode reused the plain kickoff: {dispatched}"
    );
    assert_eq!(dispatched["worktree"], plain["worktree"]);
    let job_row = settled(&job_message);
    fixture.wait_state(WORKER, "idle");
    assert_eq!(job_row.source, "job_dispatch");
    assert_eq!(job_row.issue.as_deref(), Some(CONTROL));
    let recorded_job_lane = job_row
        .worktree
        .as_deref()
        .expect("native job message must record its task lane");
    let resolved_job_lane = crate::worktree::layout::worktree_dir(&fixture.repo, recorded_job_lane)
        .canonicalize()
        .expect("the recorded native job lane must resolve to a real worktree");
    let resolved_plain_lane = Path::new(
        plain_row
            .worktree
            .as_deref()
            .expect("native plain kickoff must record its issue lane"),
    )
    .canonicalize()
    .expect("the original plain issue lane must remain a real worktree");
    assert_eq!(
        resolved_job_lane, resolved_plain_lane,
        "the native job kickoff must resolve to the exact original issue lane"
    );
    let task_id = job_row
        .task_id
        .as_deref()
        .expect("job kickoff's native task binding");
    let task = store.task(task_id).unwrap();
    let job = store.job(&task.job_id).unwrap();
    assert_eq!(dispatched["task"], task_id);
    assert_eq!(dispatched["job"], job.id.as_str());
    assert_eq!(task.assignee.as_deref(), Some(WORKER));
    assert_eq!(task.dispatch_message.as_deref(), Some(job_message.as_str()));
    assert_eq!(task.revision, 1);
    assert_eq!(task.state, "review");
    assert_eq!(job.issue_id.as_deref(), Some(CONTROL));
    assert_eq!(job.pm_alias, GROUP);
    assert_eq!(
        Path::new(&job.spec_path),
        spec.canonicalize().unwrap().as_path()
    );
    assert_eq!(store.jobs(None, true).unwrap().len(), 1);
    assert_eq!(store.tasks_for_job(&job.id).unwrap().len(), 1);
    let task_messages = store.messages_for_task(task_id).unwrap();
    assert_eq!(task_messages.len(), 1);
    assert_eq!(task_messages[0].id, job_message);

    let job_pin = fixture.context_pin(WORKER, &job_message);
    assert_eq!(job_pin["task"], task_id);
    let job_manifest: crate::context_pack::Manifest =
        serde_json::from_value(job_pin["manifest"].clone()).unwrap();
    assert_eq!(job_manifest.task.task, task_id);
    assert_eq!(job_manifest.task.issue, CONTROL);
    assert!(
        job_manifest.task.request.contains(SPEC),
        "complete effective spec was not pinned"
    );
    assert_eq!(job_manifest.project, plain_manifest.project);
    assert_eq!(
        job_manifest.project_revision,
        plain_manifest.project_revision
    );
    assert_eq!(job_manifest.role, plain_manifest.role);
    assert_eq!(job_manifest.assigned_role, plain_manifest.assigned_role);
    assert_eq!(job_manifest.documents, plain_manifest.documents);
    let supply = context_event(&fixture, WORKER, "context_supplied", &job_message);
    assert_eq!(supply["task"], task_id);
    assert_eq!(supply["pin"], job_message);
    let finished = context_event(&fixture, WORKER, "turn_finished", &job_message);
    let input = finished["result"]["text"].as_str().unwrap();
    for line in SPEC.lines() {
        assert!(
            input.contains(line),
            "native job turn omitted spec line {line:?}: {finished}"
        );
    }
    assert!(input.contains(DEV_POLICY.trim_end()));
    assert!(!input.contains(QA_POLICY.trim_end()));

    // Reported retry of the SAME explicit job entry point is not a new task,
    // a new revision or a second action, and must never fall back to plain_id.
    let messages_before = fixture.message_ids(WORKER, None);
    let task_before = store.task(task_id).unwrap().to_json();
    let job_before = store.job(&job.id).unwrap().to_json();
    let retry = dispatch_job();
    assert_eq!(retry["dispatched"], json!(false), "{retry}");
    assert_eq!(retry["duplicate"], json!(true), "{retry}");
    assert_eq!(
        retry["message"], job_message,
        "retry adopted the wrong entry point: {retry}"
    );
    assert_ne!(retry["message"], plain_id);
    assert_eq!(fixture.message_ids(WORKER, None), messages_before);
    assert_eq!(store.jobs(None, true).unwrap().len(), 1);
    assert_eq!(store.tasks_for_job(&job.id).unwrap().len(), 1);
    assert_eq!(store.messages_for_task(task_id).unwrap().len(), 1);
    assert_eq!(store.task(task_id).unwrap().to_json(), task_before);
    assert_eq!(store.job(&job.id).unwrap().to_json(), job_before);
    assert_eq!(fixture.context_pin(WORKER, &job_message), job_pin);
    assert_eq!(
        store.message(&plain_id).unwrap().unwrap().to_json(),
        plain_before
    );
    assert_eq!(fixture.context_pin(WORKER, &plain_id), plain_pin);
    let job_events = store.job_events(&job.id, 0, 1000).unwrap();
    assert!(
        job_events.len() < 1000,
        "job transition inventory must be complete"
    );
    assert_eq!(
        job_events
            .iter()
            .filter(|event| { event.kind == "task_dispatched" && event.payload["task"] == task_id })
            .count(),
        1,
        "retry recorded a second task dispatch"
    );
}

#[test]
fn cad1846_synthetic_eligible_lesson_reaches_all_three_native_entrypoints_once() {
    use crate::issue::dispatch::{DispatchArgs, DispatchOrigin};
    use crate::issue::start::{JobArgs, StartArgs};
    use crate::memory::{
        FinalizationReceipt, Front as MemoryFront, IdentityProof, Memory, ReviewReceipt, Scope,
    };

    const ELIGIBLE_ID: &str = "synthetic-parity-eligible";
    const STALE_ID: &str = "synthetic-parity-stale";
    const FACT: &str = "CAD1846_SYNTHETIC_ELIGIBLE_FACT preserve every input identifier.";
    const APPLY: &str =
        "CAD1846_SYNTHETIC_ELIGIBLE_APPLY compare output identifiers byte-for-byte.";
    const STALE_FACT: &str = "CAD1846_SYNTHETIC_STALE_FACT must never become task instructions.";
    const STALE_REASON: &str = "synthetic fixture evidence was explicitly invalidated";
    const PLAIN_WORKER: &str = "ctxp-lesson-plain";
    const WRAPPER_WORKER: &str = "ctxp-lesson-wrapper";
    const NATIVE_WORKER: &str = "ctxp-lesson-native";

    let fixture = Fixture::new();
    let pm = Pm::at(&fixture.pm).unwrap();
    let spec = fixture.repo.join("docs/cad1846-lesson-spec.md");
    fs::write(
        &spec,
        "Read tracked.txt and write lesson-delivery-result.txt.\n",
    )
    .unwrap();
    commit_all(
        &fixture.repo,
        "CAD-1846 tracked lesson consumer fixture spec",
    );
    let memory_dir = crate::memory::memory_dir(&pm, PROJECT);
    fs::create_dir_all(&memory_dir).unwrap();

    // GIVEN synthetic accepted inputs, exactly like memory::tests::finalized.
    // These names/receipts are not registered identities or real approvals.
    // No record escapes this test's disposable PM, and no memory-governance
    // RPC or production authority is exercised or asserted here.
    let proof = |alias: &str, registration: u64, role: &str| IdentityProof {
        alias: alias.to_string(),
        registration,
        generation: format!("synthetic-generation-{registration}"),
        process_start: registration + 100,
        role: role.to_string(),
    };
    let make_memory = |id: &str, fact: &str, apply: &str, stale: Option<&str>| {
        let author = proof("synthetic-author", 1, "worker");
        let mut memory = Memory {
            project: PROJECT.to_string(),
            front: MemoryFront {
                id: id.to_string(),
                kind: "rule".to_string(),
                status: "accepted".to_string(),
                scope: Scope {
                    providers: vec!["fake".to_string()],
                    ..Scope::default()
                },
                source: Some("SYNTHETIC CONSUMER TEST INPUT; no operational approval".to_string()),
                confidence: "medium".to_string(),
                created: "2026-01-01T00:00:00Z".to_string(),
                verified_at: None,
                stale: stale.map(str::to_string),
                supersedes: None,
                author: Some(author.alias.clone()),
                author_proof: Some(author),
                contributors: Vec::new(),
                review_cycle: 1,
                active_operation: None,
                reviews: Vec::new(),
                finalizations: Vec::new(),
            },
            body: format!(
                "{fact}\n\n**Why:** synthetic consumer-input evidence only\n\n**How to apply:** {apply}\n"
            ),
            path: memory_dir.join(format!("{id}.md")),
        };
        let digest = crate::memory::semantic_digest(&memory);
        for reviewer in [
            proof("synthetic-reviewer-a", 2, "worker"),
            proof("synthetic-reviewer-b", 3, "worker"),
        ] {
            memory.front.reviews.push(ReviewReceipt {
                reviewer: reviewer.alias.clone(),
                identity: reviewer.stable_id(),
                generation: reviewer.generation,
                process_start: reviewer.process_start,
                role: reviewer.role,
                operation: "accept".to_string(),
                cycle: 1,
                digest: digest.clone(),
                verdict: "pass".to_string(),
                evidence: "Synthetic test input, not an operational review receipt".to_string(),
                recorded_at: "2026-01-01T00:00:00Z".to_string(),
            });
        }
        memory.front.finalizations.push(FinalizationReceipt {
            operation: "accept".to_string(),
            cycle: 1,
            digest,
            finalizer: proof("synthetic-finalizer", 4, "pm"),
            finalized_at: "2026-01-01T00:00:00Z".to_string(),
        });
        let eligibility = crate::memory::retrieval_status(&memory);
        assert!(
            eligibility.0,
            "invalid synthetic accepted input: {}",
            eligibility.1
        );
        assert!(crate::memory::last_verified(&memory).is_none());
        memory
    };
    let eligible = make_memory(ELIGIBLE_ID, FACT, APPLY, None);
    let stale = make_memory(
        STALE_ID,
        STALE_FACT,
        "Do not apply this stale fixture.",
        Some(STALE_REASON),
    );
    for memory in [&eligible, &stale] {
        fs::write(
            &memory.path,
            format!(
                "---\n{}---\n{}",
                serde_yaml::to_string(&memory.front).unwrap(),
                memory.body
            ),
        )
        .unwrap();
    }
    let loaded = crate::memory::load_project(&fixture.pm, PROJECT).unwrap();
    assert_eq!(loaded.len(), 2);
    for memory in &loaded {
        assert!(crate::memory::retrieval_status(memory).0);
        assert!(crate::memory::last_verified(memory).is_none());
    }
    eprintln!("CAD-1846 lesson fixture: synthetic accepted inputs only; no native approval claim");

    for worker in [PLAIN_WORKER, WRAPPER_WORKER, NATIVE_WORKER] {
        fixture.op(
            "agent_register",
            json!({
                "alias": worker,
                "provider": "fake",
                "endpoint_kind": "fake",
                "role": "worker",
                "team_role": "dev",
                "cwd": fixture.repo,
                "params": json!({"fake_generation": true, "upstream": GROUP}).to_string(),
            }),
        );
        fixture.wait_state(worker, "idle");
    }
    let store = crate::store::Store::open_side(&fixture.state.join("cadence.sqlite3")).unwrap();

    let plain = fixture.kickoff_rpc(CONTROL, PLAIN_WORKER).unwrap();
    assert_eq!(plain["dispatched"], true);
    let plain_message = plain["message"].as_str().unwrap().to_string();
    context_event(&fixture, PLAIN_WORKER, "context_supplied", &plain_message);
    fixture.wait_state(PLAIN_WORKER, "idle");

    let wrapper = scoped(Asserted::Operator, || {
        crate::issue::dispatch::run(
            &pm,
            MISSING_BAD,
            &DispatchArgs {
                to: WRAPPER_WORKER.to_string(),
                note: None,
                name: None,
                base: Some("main".to_string()),
                repo: Some(fixture.repo.clone()),
                reply_to: Some(GROUP.to_string()),
                summary: None,
                job_spec: Some(spec.clone()),
                no_lessons: false,
                force: false,
                take_over: None,
            },
            "operator",
            &fixture.state,
            Some(GROUP),
            DispatchOrigin::Explicit,
        )
    })
    .expect("exercise the actual explicit issue --job entry point");
    assert_eq!(wrapper["dispatched"], true);
    let wrapper_message = wrapper["message"].as_str().unwrap().to_string();
    context_event(
        &fixture,
        WRAPPER_WORKER,
        "context_supplied",
        &wrapper_message,
    );
    fixture.wait_state(WRAPPER_WORKER, "idle");

    let native_started = scoped(Asserted::Operator, || {
        crate::issue::start::run(
            &pm,
            SCOPE_BAD,
            &StartArgs {
                repo: Some(fixture.repo.clone()),
                name: None,
                base: Some("main".to_string()),
                owner: Some(NATIVE_WORKER.to_string()),
                job: Some(JobArgs {
                    id: None,
                    pm: GROUP.to_string(),
                    spec: spec.clone(),
                    assignee: Some(NATIVE_WORKER.to_string()),
                    force: false,
                }),
                by: Some(GROUP.to_string()),
                take_over: None,
            },
            "cad1846-acceptance",
            &fixture.state,
        )
    })
    .expect("establish authentic native issue-linked job scope");
    let native = fixture.op(
        "task_dispatch",
        json!({
            "task": native_started["task"],
            "by": GROUP,
            "message": "cad1846-native-lesson-message",
        }),
    );
    let native_message = native["message"].as_str().unwrap().to_string();
    context_event(&fixture, NATIVE_WORKER, "context_supplied", &native_message);
    fixture.wait_state(NATIVE_WORKER, "idle");

    let mut selected_documents: Option<Vec<crate::context_pack::Source>> = None;
    for (entrypoint, worker, message) in [
        ("plain", PLAIN_WORKER, plain_message),
        ("issue --job", WRAPPER_WORKER, wrapper_message),
        ("native task", NATIVE_WORKER, native_message),
    ] {
        let agent = store.agent(worker).unwrap();
        assert_eq!(agent.provider, "fake");
        assert_eq!(agent.team_role.as_deref(), Some("dev"));
        let row = store.message(&message).unwrap().unwrap();
        assert_eq!(row.state, "completed", "{entrypoint}: {}", row.to_json());
        if entrypoint == "plain" {
            assert_eq!(row.source, "dispatch");
            assert!(row.task_id.is_none());
        } else {
            assert_eq!(row.source, "job_dispatch");
            assert!(row.task_id.is_some());
        }
        let pin = fixture.context_pin(worker, &message);
        let manifest: crate::context_pack::Manifest =
            serde_json::from_value(pin["manifest"].clone()).unwrap();
        assert_eq!(manifest.project, PROJECT);
        assert_eq!(manifest.role, "dev");
        if let Some(expected) = &selected_documents {
            assert_eq!(
                &manifest.documents, expected,
                "source/lesson selection parity: {entrypoint}"
            );
        } else {
            selected_documents = Some(manifest.documents.clone());
        }
        let lessons = manifest
            .documents
            .iter()
            .filter(|source| source.id == "native:lessons")
            .collect::<Vec<_>>();
        assert_eq!(
            lessons.len(),
            1,
            "one aggregate lesson source: {entrypoint}"
        );
        let lesson = lessons[0];
        assert_eq!(lesson.lesson_ids, vec![ELIGIBLE_ID.to_string()]);
        assert!(!lesson.required);
        let rendered = lesson.inline.as_deref().unwrap();
        assert_eq!(rendered.matches(FACT).count(), 1);
        assert_eq!(rendered.matches(APPLY).count(), 1);
        assert!(rendered.contains(&format!("- `{ELIGIBLE_ID}` (rule, unverified):")));
        assert!(!rendered.contains(STALE_FACT));
        assert!(rendered.contains(&format!(
            "- `{STALE_ID}`: evidence marked stale: {STALE_REASON}"
        )));
        assert!(manifest.withheld.iter().any(|gap| {
            gap.id == "native:lessons-withheld" && gap.state == "withheld" && !gap.required
        }));

        let supply = context_event(&fixture, worker, "context_supplied", &message);
        assert_eq!(supply["task"], manifest.task.task);
        assert_eq!(supply["pin"], message);
        let supplied_lesson = supply["sources"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|source| source["id"] == "native:lessons")
            .collect::<Vec<_>>();
        assert_eq!(supplied_lesson.len(), 1);
        assert_eq!(supplied_lesson[0]["lesson_ids"], json!([ELIGIBLE_ID]));
        assert_eq!(supplied_lesson[0]["disposition"], "appended");
        assert_eq!(
            supplied_lesson[0]["bytes"].as_u64(),
            Some(lesson.bytes as u64)
        );
        let finished = context_event(&fixture, worker, "turn_finished", &message);
        let provider_observation = finished["result"]["text"].as_str().unwrap();
        for once in [ELIGIBLE_ID, FACT, APPLY, STALE_ID, STALE_REASON] {
            assert_eq!(
                provider_observation.matches(once).count(),
                1,
                "{entrypoint}: lesson payload duplicated or omitted {once:?}: {finished}"
            );
        }
        assert!(!provider_observation.contains(STALE_FACT));
        eprintln!(
            "CAD-1846 synthetic lesson consumer {entrypoint}: {}",
            json!({
                "message": message,
                "project": manifest.project,
                "role": manifest.role,
                "provider": agent.provider,
                "lesson_ids": lesson.lesson_ids,
                "supply": supply,
                "synthetic_inputs_only": true,
            })
        );
    }
}
