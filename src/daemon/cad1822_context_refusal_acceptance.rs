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
        "Reference policy clause.\n".repeat(1700)
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
            &gated.project,
            gated.source.as_bytes(),
            &gated.front,
            &gated.items,
            None,
            "fake",
            "fake",
            "Execute the originally selected task using its required project policy.",
        )
        .expect("the real required Git policy must remain deliverable")
        .expect("the fixture project is explicitly opted in")
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
