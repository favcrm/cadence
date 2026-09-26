//! Shared harness for the split board binaries (CAD-537): the `cadence`
//! CLI runners, HTTP helpers, board/daemon starters and fixtures that
//! more than one board area uses. Not every binary uses every helper.
#![allow(dead_code)]

use cadence_agent::client;
use cadence_agent::daemon;
use cadence_agent::ui;
use serde_json::json;
use serde_json::Value;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tempfile::TempDir;

#[path = "../support/operator.rs"]
pub mod op;

pub fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_cadence")
}

pub fn cli(pm: &Path, state: &Path, args: &[&str]) -> (bool, Value) {
    cli_env::<&str>(pm, state, args, &[])
}

pub fn cli_env<S: AsRef<str>>(
    pm: &Path,
    state: &Path,
    args: &[&str],
    env: &[(&str, S)],
) -> (bool, Value) {
    cli_run(pm, state, None, args, env)
}

pub fn cli_run<S: AsRef<str>>(
    pm: &Path,
    state: &Path,
    cwd: Option<&Path>,
    args: &[&str],
    env: &[(&str, S)],
) -> (bool, Value) {
    let mut cmd = Command::new(bin());
    cmd.arg("--state-dir")
        .arg(state)
        .args(args)
        .env("CADENCE_PM_DIR", pm)
        // The tracker's pre-commit hook runs `cadence` from PATH —
        // put the just-built binary first so its lint sees the ref
        // kinds this build writes.
        .env(
            "PATH",
            format!(
                "{}:{}",
                Path::new(bin()).parent().unwrap().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        // Ambient aliases would leak into Actor: trailers and comment
        // authors — remove it so every env resolves to `operator`.
        .env_remove("CADENCE_ALIAS");
    if let Ok(home) = std::env::var("HOME") {
        cmd.env("HOME", home);
    }
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    for (k, v) in env {
        cmd.env(k, v.as_ref());
    }
    let out = cmd.output().unwrap();
    // Errors print their JSON to stderr; successes to stdout.
    let text = if out.stdout.is_empty() {
        String::from_utf8_lossy(&out.stderr).to_string()
    } else {
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let json: Value = serde_json::from_str(text.trim()).unwrap_or_else(|_| {
        panic!(
            "not json: {text} (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (out.status.success(), json)
}

/// `cadence …` for verbs that print plain text instead of JSON
/// (`issue trailer`). Returns (ok, stdout-or-stderr).
pub fn cli_raw(pm: &Path, state: &Path, args: &[&str]) -> (bool, String) {
    cli_raw_env(pm, state, args, &[])
}

pub fn cli_raw_env(
    pm: &Path,
    state: &Path,
    args: &[&str],
    env: &[(&str, String)],
) -> (bool, String) {
    let mut cmd = Command::new(bin());
    cmd.arg("--state-dir")
        .arg(state)
        .args(args)
        .env("CADENCE_PM_DIR", pm)
        .env_remove("CADENCE_ALIAS");
    if let Ok(home) = std::env::var("HOME") {
        cmd.env("HOME", home);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let text = if out.stdout.is_empty() {
        String::from_utf8_lossy(&out.stderr).to_string()
    } else {
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    (out.status.success(), text.trim().to_string())
}

pub fn commits(pm: &Path) -> usize {
    let out = Command::new("git")
        .arg("-C")
        .arg(pm)
        .args(["rev-list", "--count", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Minimal blocking HTTP/1.0 client — enough for assertions without a
/// client dependency.
pub fn http_full(port: u16, method: &str, path: &str, host: &str) -> (u16, String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(s, "{method} {path} HTTP/1.0\r\nHost: {host}\r\n\r\n").unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    // Bodies may be binary (png artifacts) — lossy-decode for asserts.
    let buf = String::from_utf8_lossy(&raw).to_string();
    let status = buf
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let mut parts = buf.splitn(2, "\r\n\r\n");
    let headers = parts.next().unwrap_or("").to_string();
    let body = parts.next().unwrap_or("").to_string();
    (status, headers, body)
}

pub fn http(port: u16, method: &str, path: &str, host: &str) -> (u16, String) {
    let (status, _, body) = http_full(port, method, path, host);
    (status, body)
}

/// A write request: extra headers plus a raw body. `(status, headers,
/// body)` — headers joined so tests can assert on what is (not) sent.
pub fn http_write(
    port: u16,
    method: &str,
    path: &str,
    host: &str,
    headers: &[&str],
    body: &[u8],
) -> (u16, String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut req = format!("{method} {path} HTTP/1.0\r\nHost: {host}\r\n");
    for h in headers {
        req.push_str(h);
        req.push_str("\r\n");
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    s.write_all(req.as_bytes()).unwrap();
    s.write_all(body).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let buf = String::from_utf8_lossy(&raw).to_string();
    let status = buf
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let mut parts = buf.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or("").to_string();
    (status, head, parts.next().unwrap_or("").to_string())
}

/// The headers a legitimate board write always carries.
pub const WRITE_HEADERS: &[&str] = &[
    "Content-Type: application/json",
    "X-Cadence-Board: 1",
    "Sec-Fetch-Site: same-origin",
];

pub fn write_json(
    port: u16,
    method: &str,
    path: &str,
    host: &str,
    body: &str,
) -> (u16, String, String) {
    http_write(port, method, path, host, WRITE_HEADERS, body.as_bytes())
}

/// Stops an in-process board when dropped (CAD-471): the board closes
/// its port instead of serving for the rest of the run — a leaked one
/// outlives its temp dirs, and any client that finds its port pins one
/// server thread per idle connection until the runner exits.
pub struct BoardStop(pub std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Drop for BoardStop {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Sign in to the board on `port` as the operator (CAD-313) — the real
/// `cadence ui login` link exchanged at `POST /api/session`. The daemon
/// on `state` must be up: it is the session authority.
pub fn sign_in(state: &Path, port: u16) -> op::Session {
    op::sign_in(bin(), state, port)
}

/// [`http_write`] as the signed-in operator `op`: to `port`'s own board
/// Host with the session cookie, plus that Host's `Origin` unless
/// `headers` set one (a cookie-bearing write must say where it comes
/// from). `host` is replaced: sessions live on the board's name only.
pub fn op_http_write(
    op: &op::Session,
    port: u16,
    method: &str,
    path: &str,
    host: &str,
    headers: &[&str],
    body: &[u8],
) -> (u16, String, String) {
    // A session lives on the board's own Host only (CAD-313): the
    // signed-in write goes to `port`'s own name, whatever `host` the
    // caller used; an `Origin` the caller set on purpose is kept.
    let _ = host;
    let own = op::board_host(port);
    let cookie = format!("Cookie: {}", op.cookie);
    let key = op.key_header();
    let origin = format!("Origin: http://{own}");
    let mut all: Vec<&str> = headers.to_vec();
    if !headers
        .iter()
        .any(|h| h.to_ascii_lowercase().starts_with("origin:"))
    {
        all.push(&origin);
    }
    all.push(&cookie);
    all.push(&key);
    // CAD-482: the session's caller assertion rides too — the write
    // proves operator in a pane exactly as ambient proof does in CI.
    let seam = op.seam.trim_end().to_string();
    if !seam.is_empty() {
        all.push(&seam);
    }
    http_write(port, method, path, &own, &all, body)
}

/// [`write_json`] as the signed-in operator `op`.
pub fn op_write_json(
    op: &op::Session,
    port: u16,
    method: &str,
    path: &str,
    host: &str,
    body: &str,
) -> (u16, String, String) {
    op_http_write(op, port, method, path, host, WRITE_HEADERS, body.as_bytes())
}

/// Spawn `ui::serve` on a free port and wait for health. The caller owns
/// the TempDirs keeping the pm/state dirs alive, and the returned
/// [`BoardStop`] keeping the board up. `free_port` is a bind-release
/// race — a parallel test may grab the port first, so a failed start
/// retries on a fresh port.
pub fn start_ui(pm_dir: PathBuf, state_dir: PathBuf) -> (u16, BoardStop) {
    start_ui_opts(pm_dir, state_dir, |_| {})
}

/// `start_ui` with `ServeOpts` overrides (`f` runs after the free port
/// is chosen — the port itself is always the probe-verified one).
pub fn start_ui_opts(
    pm_dir: PathBuf,
    state_dir: PathBuf,
    f: impl Fn(&mut ui::ServeOpts) + Send + Sync + 'static,
) -> (u16, BoardStop) {
    let f = std::sync::Arc::new(f);
    let overall = Instant::now() + Duration::from_secs(20);
    loop {
        let port = free_port();
        let (sd, pd, f) = (state_dir.clone(), pm_dir.clone(), f.clone());
        // A start that loses its port race is stopped too.
        let board = BoardStop(Default::default());
        let stop = board.0.clone();
        thread::spawn(move || {
            let mut opts = ui::ServeOpts {
                host: "127.0.0.1".to_string(),
                port,
                // CAD-482: under the feature every in-process fixture
                // board attaches — the token is read lazily per request,
                // so a board may start before its daemon mints.
                test_seam: cfg!(feature = "test-seam"),
                ..Default::default()
            };
            f(&mut opts);
            opts.port = port;
            opts.stop = Some(stop);
            let _ = ui::serve(&sd, &pd, &opts);
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        // Host carries the port, so a stolen port's foreign server
        // rejects this probe (its allowlist names ITS port) — 200
        // only ever comes from OUR server.
        loop {
            if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) {
                s.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let probe = format!("GET /api/health HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n");
                let _ = s.write_all(probe.as_bytes());
                let mut buf = String::new();
                if s.read_to_string(&mut buf).is_ok() && buf.contains("200") {
                    return (port, board);
                }
            }
            if Instant::now() >= deadline {
                break; // port was likely stolen — retry on another
            }
            assert!(Instant::now() < overall, "ui server did not start");
            thread::sleep(Duration::from_millis(50));
        }
    }
}

pub fn seed(pm: &Path, state: &Path) {
    assert!(cli(pm, state, &["issue", "init"]).0);
    assert!(
        cli(
            pm,
            state,
            &["issue", "project", "add", "cadence", "--prefix", "CAD"]
        )
        .0
    );
    assert!(
        cli(
            pm,
            state,
            &["issue", "new", "root task", "--project", "cadence"]
        )
        .0
    );
    assert!(
        cli(
            pm,
            state,
            &[
                "issue",
                "new",
                "child task",
                "--project",
                "cadence",
                "--parent",
                "CAD-1"
            ]
        )
        .0
    );
    assert!(
        cli(
            pm,
            state,
            &["issue", "new", "sibling", "--project", "cadence"]
        )
        .0
    );
    assert!(
        cli(
            pm,
            state,
            &["issue", "set", "CAD-2", "status=doing", "owner=me"]
        )
        .0
    );
}

// ---------- CAD-39: issue sync — fetch, rebase, lint, push ----------

/// A bare remote plus two clones with `issue init` run in each — the
/// multi-host shape. `user.email`/`user.name` are set per clone so
/// tests can also commit by hand. The post-commit hook pushes in the
/// background, so remote state is only ever asserted through
/// `wait_remote`.
pub struct TrackerPair {
    pub _root: TempDir,
    pub remote: PathBuf,
    pub a: PathBuf,
    pub b: PathBuf,
    pub state: TempDir,
}

pub fn git(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

pub fn head(dir: &Path) -> String {
    git(dir, &["rev-parse", "HEAD"]).1
}

/// Poll `ls-remote` until the remote's branch tip equals `want` —
/// pushing `from` on each round. The winner's post-commit hook usually
/// lands it first; the explicit push covers the race. Diverged pushes
/// simply fail every round until the timeout.
pub fn wait_remote(remote: &Path, from: &Path, want: &str, branch: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let (ok, out) = Command::new("git")
            .arg("ls-remote")
            .arg(remote)
            .arg(format!("refs/heads/{branch}"))
            .output()
            .map(|o| {
                (
                    o.status.success(),
                    String::from_utf8_lossy(&o.stdout).to_string(),
                )
            })
            .unwrap_or((false, String::new()));
        if ok && out.split_whitespace().next() == Some(want) {
            return;
        }
        let _ = git(from, &["push", "-q", "origin", branch]);
        assert!(
            Instant::now() < deadline,
            "remote never reached {want} from {}",
            from.display()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

pub fn branch(dir: &Path) -> String {
    git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).1
}

impl TrackerPair {
    pub fn new() -> TrackerPair {
        let root = TempDir::new().unwrap();
        let remote = root.path().join("remote.git");
        let a = root.path().join("a");
        let b = root.path().join("b");
        let state = TempDir::new().unwrap();
        // Pin the branch name — host `init.defaultBranch` differs
        // (CI has none and would mint `master`, breaking every
        // `origin/main`/`"main"` reference below).
        Command::new("git")
            .args(["init", "-q", "--bare", "-b", "main"])
            .arg(&remote)
            .output()
            .unwrap();
        // A inits and publishes first — B clones the bootstrapped
        // remote so both share the same init commit.
        Command::new("git")
            .args(["clone", "-q"])
            .arg(&remote)
            .arg(&a)
            .output()
            .unwrap();
        git(&a, &["config", "user.email", "a@x"]);
        git(&a, &["config", "user.name", "a"]);
        let (ok, out) = cli(&a, state.path(), &["issue", "init"]);
        assert!(ok, "init on {}: {out}", a.display());
        wait_remote(&remote, &a, &head(&a), &branch(&a));
        Command::new("git")
            .args(["clone", "-q"])
            .arg(&remote)
            .arg(&b)
            .output()
            .unwrap();
        git(&b, &["config", "user.email", "b@x"]);
        git(&b, &["config", "user.name", "b"]);
        // Init on B is idempotent — the skeleton exists; only the
        // hooks land in B's .git/hooks.
        let (ok, out) = cli(&b, state.path(), &["issue", "init"]);
        assert!(ok, "init on {}: {out}", b.display());
        TrackerPair {
            _root: root,
            remote,
            a,
            b,
            state,
        }
    }

    pub fn cli(&self, clone: &Path, args: &[&str]) -> (bool, Value) {
        cli(clone, self.state.path(), args)
    }

    /// Push `clone`'s HEAD to the remote and wait until it lands.
    pub fn publish(&self, clone: &Path) {
        wait_remote(&self.remote, clone, &head(clone), &branch(clone));
    }
}

// ---------- I3: job-derived status, exact binding, SSE, agent detail ----------

/// In-process daemon for the runtime strip — the same `daemon::serve`
/// common/mod.rs wraps in TestDaemon, pared down to what the board
/// routes need.
pub struct UiDaemon {
    pub state: PathBuf,
    pub _tmp: Option<TempDir>,
    pub stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub handle: Option<thread::JoinHandle<()>>,
}

impl UiDaemon {
    pub fn start() -> Self {
        let tmp = TempDir::new().unwrap();
        Self::serve(tmp.path().to_path_buf(), Some(tmp), None)
    }

    /// Serve on a caller-owned state dir — for fixtures whose `state`
    /// the cli-under-test already points at.
    pub fn start_on(state: PathBuf) -> Self {
        Self::serve(state, None, None)
    }

    /// `start_on` with the daemon's `CADENCE_PM_DIR` bound — a dispatch
    /// reads the tracker daemon-side (`dispatch_send`), so the daemon
    /// must see the same pm dir the cli calls do.
    pub fn start_on_pm(state: PathBuf, pm: &Path) -> Self {
        Self::serve(state, None, Some(pm))
    }

    pub fn serve(state: PathBuf, tmp: Option<TempDir>, pm: Option<&Path>) -> Self {
        let owned = state.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let provider_env = cadence_agent::adapter::ProviderEnv::default();
        if let Some(pm) = pm {
            provider_env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
        }
        let opts = daemon::ServeOptions {
            stop: Some(stop.clone()),
            provider_env,
            test_seam: cfg!(feature = "test-seam"),
            ..Default::default()
        };
        let handle = thread::spawn(move || {
            let _ = daemon::serve_with(&owned, opts);
        });
        let d = Self {
            state,
            _tmp: tmp,
            stop,
            handle: Some(handle),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if d.rpc_opt("health", json!({})).is_ok() {
                return d;
            }
            assert!(Instant::now() < deadline, "daemon did not become healthy");
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Serve with the operator-auth clock `clock` (CAD-313): a test
    /// advances it past a login link's TTL instead of sleeping.
    pub fn start_with_clock(
        state: PathBuf,
        clock: std::sync::Arc<dyn Fn() -> i64 + Send + Sync>,
    ) -> Self {
        let owned = state.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let opts = daemon::ServeOptions {
            operator_clock: Some(clock),
            stop: Some(stop.clone()),
            test_seam: cfg!(feature = "test-seam"),
            ..Default::default()
        };
        let handle = thread::spawn(move || {
            let _ = daemon::serve_with(&owned, opts);
        });
        let d = Self {
            state,
            _tmp: None,
            stop,
            handle: Some(handle),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while d.rpc_opt("health", json!({})).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become healthy");
            thread::sleep(Duration::from_millis(50));
        }
        d
    }

    pub fn rpc_opt(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        client::rpc(&self.state, method, params)
    }

    /// A fixture call — the operator's act — that must succeed. When the
    /// suite runs in an agent pane, this process's ancestry carries
    /// `CADENCE_ALIAS` and operator-only methods (`agent_register`,
    /// `task_dispatch`, …) refuse it, correctly; the refused call is
    /// made again the way an operator shell outside every pane looks to
    /// the daemon ([`Self::operator_rpc`], CAD-471). Gate assertions use
    /// [`Self::rpc_opt`], which never retries.
    pub fn rpc(&self, method: &str, params: Value) -> Value {
        match self.rpc_opt(method, params.clone()) {
            Err(e) if e.to_string().contains("not provably the operator") => {
                self.operator_rpc(method, params)
            }
            r => r,
        }
        .unwrap_or_else(|e| panic!("{method}: {e}"))
    }

    /// `rpc` from a caller that is provably the operator however the
    /// suite is run — `TestDaemon::operator_rpc` in tests/common/mod.rs
    /// (CAD-291). On a seam-armed fixture (CAD-482) the identity is
    /// asserted in-band; otherwise `setsid -f` hands the call to a
    /// fresh session leader that waits until it has left this
    /// process's ancestry, `env_clear` leaves no `CADENCE_ALIAS`, and
    /// stdio is not a pane tty — the residual `peer::operator_proof`
    /// accepts. The gate itself is untouched.
    pub fn operator_rpc(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        if cadence_agent::test_seam::armed(&self.state) {
            return cadence_agent::test_seam::scoped(
                cadence_agent::test_seam::Asserted::Operator,
                || client::rpc(&self.state, method, params),
            );
        }
        let script = self.state.join("operator-rpc.py");
        if !script.exists() {
            std::fs::write(&script, op::rpc_script()).unwrap();
        }
        let out = self.state.join(format!(
            "operator-rpc-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let frame = json!({"method": method, "params": params}).to_string();
        let status = Command::new("setsid")
            .arg("-f")
            .arg("python3")
            .arg(&script)
            .arg(client::socket_path(&self.state))
            .arg(&frame)
            .arg(&out)
            .arg(std::process::id().to_string())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "setsid -f failed: {status}");
        let deadline = Instant::now() + Duration::from_secs(20);
        while !out.exists() {
            assert!(
                Instant::now() < deadline,
                "operator rpc {method} never answered"
            );
            thread::sleep(Duration::from_millis(20));
        }
        let frame: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        let _ = std::fs::remove_file(&out);
        cadence_agent::proto::unwrap(frame)
    }

    pub fn state(&self) -> PathBuf {
        self.state.clone()
    }
}

impl Drop for UiDaemon {
    /// Not the `shutdown` RPC: when the suite runs in an agent pane,
    /// this process's ancestry carries `CADENCE_ALIAS`, and the caller
    /// rule refuses it `shutdown` (CAD-384) — the join then waited
    /// forever (CAD-471). The in-process stop flag needs no connection.
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            join_within(handle, DAEMON_STOP_BOUND, "the in-process daemon");
        }
    }
}

/// How long a stopped in-process daemon may take to return: its accept
/// poll plus `Shared::shutdown` joining the actors.
pub const DAEMON_STOP_BOUND: Duration = Duration::from_secs(60);

/// Join `handle`, failing the test — never hanging it — when the thread
/// is still running after `bound` (CAD-471).
pub fn join_within(handle: thread::JoinHandle<()>, bound: Duration, what: &str) {
    let deadline = Instant::now() + bound;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            // A second panic while unwinding would abort the runner.
            if !thread::panicking() {
                panic!("{what} did not stop within {bound:?} of its stop flag");
            }
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let _ = handle.join();
}

/// Plant `pid` as `alias`'s live pty pane — the row shape the daemon's
/// pane map resolves callers by (a `pty` endpoint with a pid and a
/// generation). Registered as an actorless `inbox` pair first and kept
/// `enabled=0`, so no actor ever opens it and overwrites the plant —
/// the same recipe common/mod.rs's `plant_pane` uses.
pub fn plant_pane(d: &UiDaemon, alias: &str, pid: u32) {
    let _ = d.operator_rpc(
        "agent_register",
        json!({"alias": alias, "provider": "inbox", "endpoint_kind": "inbox",
               "cwd": d.state().to_str().unwrap()}),
    );
    let conn = rusqlite::Connection::open(d.state().join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE agents SET endpoint_kind='pty', pid=?1, pid_start=?3, enabled=0, \
            generation='planted', session_id='planted' WHERE alias=?2",
        rusqlite::params![pid as i64, alias, proc_start(pid)],
    )
    .unwrap();
}

/// `/proc/<pid>/stat` field 22 — what the daemon records as a pid's
/// `pid_start` (CAD-385), so a planted row names exactly that process.
pub fn proc_start(pid: u32) -> Option<i64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

/// Runs a bash script as a "pane" whose stdio is a real pty, relaying
/// the test's stdin lines to it; prints the pane's pid first.
pub const PTY_PANE_PY: &str = r#"
import os, subprocess, sys, threading
m, s = os.openpty()
p = subprocess.Popen(["bash", "-c", sys.argv[1]], stdin=s, stdout=s, stderr=s)
os.close(s)
print(p.pid, flush=True)
def drain():
    try:
        while os.read(m, 4096):
            pass
    except OSError:
        pass
threading.Thread(target=drain, daemon=True).start()
for line in sys.stdin:
    os.write(m, line.encode())
p.wait()
"#;

// ---------- CAD-41: issue history from git plumbing ----------

/// A tracker whose CAD-1 has a varied, deterministic write history:
/// created → two CLI sets → link → comment → attach → one HTTP PATCH
/// (the ` (operator (ui))` actor) last, so `diff`'s default lands on a
/// field change. Shas per step are captured for blame/diff asserts.
pub struct HistFx {
    pub pm: TempDir,
    pub state: TempDir,
    pub port: u16,
    pub _board: BoardStop,
    pub created_sha: String,
    pub set2_sha: String,
    pub patch_sha: String,
    pub link_sha: String,
}

// ---------- CAD-42: Issue:/Actor: trailers, truthful `by`, code commits ----------

/// `git interpret-trailers --parse` on one commit's message — the
/// acceptance criterion checks trailers through git's own parser.
pub fn trailers_of(dir: &Path, sha: &str) -> String {
    let (_, msg) = git(dir, &["show", "-s", "--format=%B", sha]);
    let mut child = Command::new("git")
        .args(["interpret-trailers", "--parse"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(msg.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Find the sha of the issue-folder commit whose subject contains
/// `needle` — same lookup shape as `history_fixture`'s `sha_of`.
pub fn sha_of(pm: &Path, rel: &str, needle: &str) -> String {
    let (_, log) = git(pm, &["log", "--format=%H %s", "--", rel]);
    log.lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no commit containing '{needle}': {log}"))
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

// ---------- CAD-43: issue start ----------

/// Temp project repo (`main`, one commit) + tracker with a `demo`/`D`
/// project pointing at it. Returns the tmp guard plus the paths.
pub fn start_fx() -> (TempDir, PathBuf, PathBuf, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let pm = tmp.path().join("pm");
    let state = tmp.path().join("state");
    let repo = tmp.path().join("repo");
    for d in [&pm, &state, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    assert!(git(&repo, &["init", "-b", "main"]).0);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "x").unwrap();
    git(&repo, &["add", "-A"]);
    assert!(git(&repo, &["commit", "-qm", "init"]).0);
    assert!(cli(&pm, &state, &["issue", "init"]).0);
    let repo_s = repo.to_str().unwrap().to_string();
    assert!(
        cli(
            &pm,
            &state,
            &["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s]
        )
        .0
    );
    (tmp, pm, state, repo)
}

/// CAD-275: age every file under `dir` past `issue finish`'s
/// 30-minute active window — the state of a lane nobody has touched
/// since. Without it a lane written seconds ago is "in use".
pub fn idle(dir: &Path) {
    let st = Command::new("find")
        .arg(dir)
        .args(["-exec", "touch", "-h", "-d", "2 hours ago", "{}", "+"])
        .status()
        .unwrap();
    assert!(st.success(), "backdate {}", dir.display());
}

// ---------- CAD-69: board over Tailscale ----------

pub const TS_DNS: &str = "node.tail1234.ts.net";

pub fn calls(fake: &Path) -> String {
    std::fs::read_to_string(fake.join("calls.log")).unwrap_or_default()
}

/// Stop a detached `cadence ui` for `state` — best effort, ignores
/// output. Tests that spawn the detached server hold this so a
/// panicking assert doesn't leak a setsid'd server.
pub struct DetachedUi(pub PathBuf);

impl Drop for DetachedUi {
    fn drop(&mut self) {
        let _ = Command::new(bin())
            .arg("--state-dir")
            .arg(&self.0)
            .args(["ui", "stop"])
            .env("CADENCE_PM_DIR", &self.0)
            .output();
    }
}

// --- request-level: identity, origins, read-only ---

/// The headers a proxied tailnet write carries — the tailnet Host,
/// the https Origin, and Tailscale's identity headers.
pub fn ts_write_headers(origin: &str, login: &str) -> Vec<String> {
    vec![
        "Content-Type: application/json".to_string(),
        "X-Cadence-Board: 1".to_string(),
        "Sec-Fetch-Site: same-origin".to_string(),
        format!("Origin: {origin}"),
        format!("Tailscale-User-Login: {login}"),
        "Tailscale-User-Name: Some User".to_string(),
    ]
}

/// Tailscale sharing armed, with the tailnet proof reading tailscaled's
/// LocalAPI at `socket` (a [`fake_localapi`], or a path with nothing).
pub fn tailnet_opts(socket: &Path) -> impl Fn(&mut ui::ServeOpts) + Send + Sync + 'static {
    let socket = socket.to_path_buf();
    move |o| {
        o.tailnet = Some((TS_DNS.to_string(), 9450));
        o.allow_hosts = vec![TS_DNS.to_string(), format!("{TS_DNS}:9450")];
        o.allow_origins = vec![format!("https://{TS_DNS}:9450")];
        o.tailscaled_socket = Some(socket.clone());
    }
}

/// A fake tailscaled LocalAPI (CAD-336): a unix socket, owned by this
/// test's uid, answering `/localapi/v0/status` with `status.json`,
/// `/localapi/v0/prefs` with `prefs.json` and `/localapi/v0/serve-config`
/// with `serve.json` from its directory, read per request — a missing
/// file answers `500`. It starts with no operator user, so a board's
/// startup read ([`ui::serve`]'s operator latch) finds none. Under `/tmp`: a
/// long TMPDIR would overflow `sun_path`.
pub fn fake_localapi() -> (TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("ts")
        .tempdir_in("/tmp")
        .unwrap();
    let sock = dir.path().join("ts.sock");
    localapi_operator(dir.path(), "");
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let root = dir.path().to_path_buf();
    thread::spawn(move || {
        for mut conn in listener.incoming().flatten() {
            // Read through the end of the request head.
            let mut raw = Vec::new();
            let mut buf = [0u8; 512];
            while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
                match conn.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => raw.extend_from_slice(&buf[..n]),
                }
            }
            let req = String::from_utf8_lossy(&raw).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or_default();
            let file = if path.starts_with("/localapi/v0/status") {
                Some("status.json")
            } else if path == "/localapi/v0/prefs" {
                Some("prefs.json")
            } else if path == "/localapi/v0/serve-config" {
                Some("serve.json")
            } else {
                None
            };
            let resp = match file.and_then(|f| std::fs::read(root.join(f)).ok()) {
                Some(body) => [
                    b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n".as_slice(),
                    &body,
                ]
                .concat(),
                None => b"HTTP/1.0 500 Internal Server Error\r\n\r\n".to_vec(),
            };
            let _ = conn.write_all(&resp);
        }
    });
    (dir, sock)
}

/// Write the fake LocalAPI's answers: `TUN` (None omits the field), no
/// operator user, and the serve config.
pub fn localapi_says(dir: &Path, tun: Option<bool>, serve: Value) {
    let status = match tun {
        Some(t) => json!({"TUN": t, "BackendState": "Running"}),
        None => json!({"BackendState": "Running"}),
    };
    std::fs::write(dir.join("status.json"), status.to_string()).unwrap();
    localapi_operator(dir, "");
    std::fs::write(dir.join("serve.json"), serve.to_string()).unwrap();
}

/// The fake LocalAPI's `prefs.OperatorUser` (`""` is none).
pub fn localapi_operator(dir: &Path, user: &str) {
    let prefs = json!({"OperatorUser": user, "WantRunning": true});
    std::fs::write(dir.join("prefs.json"), prefs.to_string()).unwrap();
}

/// The serve config `ui tailscale start` makes: https:9450 proxied to
/// the board, plus an unrelated TCP forwarder (ssh).
pub fn serve_https_only(board_port: u16) -> Value {
    json!({
        "TCP": {"9450": {"HTTPS": true}, "2222": {"TCPForward": "127.0.0.1:22"}},
        "Web": {format!("{TS_DNS}:9450"): {"Handlers": {"/": {"Proxy": format!("http://127.0.0.1:{board_port}")}}}}
    })
}

/// `ui run` as a subprocess with a sanitized env. Kills the child on drop
/// so a failed assert leaves no listener behind.
pub struct UiProc(pub std::process::Child);
impl Drop for UiProc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn spawn_ui(pm: &Path, state: &Path) -> (u16, UiProc) {
    spawn_ui_env(pm, state, &[])
}

/// `spawn_ui` with `env` set on the server after the sanitizing.
pub fn spawn_ui_env(pm: &Path, state: &Path, env: &[(&str, &str)]) -> (u16, UiProc) {
    spawn_ui_seam(pm, state, env, true)
}

#[allow(clippy::zombie_processes)] // UiProc's Drop kills + waits.
pub fn spawn_ui_seam(pm: &Path, state: &Path, env: &[(&str, &str)], arm: bool) -> (u16, UiProc) {
    let port = free_port();
    let mut cmd = Command::new(bin());
    cmd.arg("--state-dir")
        .arg(state)
        .args(["ui", "run", "--port", &port.to_string()])
        .env("CADENCE_PM_DIR", pm)
        // The tracker's pre-commit hook runs `cadence` from PATH — same
        // PATH fix as cli_run so the hook lints with THIS build.
        .env(
            "PATH",
            format!(
                "{}:{}",
                Path::new(bin()).parent().unwrap().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env_remove("CADENCE_ALIAS")
        .env_remove("CADENCE_STATE_DIR");
    if let Ok(home) = std::env::var("HOME") {
        cmd.env("HOME", home);
    }
    // CAD-482: under the feature every spawned fixture board attaches to
    // the seam — it honors assertion headers — and runs as the operator's
    // process on unasserted daemon calls, as `start_operator_ui` does.
    // A caller's `env` overrides either default (e.g. an agent-shaped
    // board carries its own CADENCE_TEST_AS). `arm: false` spawns the
    // unarmed shape: no env can leak an attach.
    if arm && cfg!(feature = "test-seam") {
        cmd.env(cadence_agent::test_seam::ARM_ENV, "1")
            .env(cadence_agent::test_seam::AS_ENV, "operator");
    } else {
        cmd.env_remove(cadence_agent::test_seam::ARM_ENV)
            .env_remove(cadence_agent::test_seam::AS_ENV);
    }
    cmd.envs(env.iter().copied());
    let child = cmd.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        // Connect-refused while the listener binds is expected — the
        // http helper unwraps the connect, so probe it raw here.
        if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) {
            s.set_read_timeout(Some(Duration::from_secs(2))).ok();
            let probe = format!("GET /api/health HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n");
            let _ = s.write_all(probe.as_bytes());
            let mut buf = String::new();
            if s.read_to_string(&mut buf).is_ok() && buf.contains("200") {
                return (port, UiProc(child));
            }
        }
        assert!(Instant::now() < deadline, "ui subprocess did not start");
        thread::sleep(Duration::from_millis(50));
    }
}

/// The board as an operator runs it, however the suite is run: a
/// detached `cadence ui start`. Operator-only relays (`model_defaults_set`,
/// CAD-337) refuse a board whose ancestry carries an agent, and when the
/// suite itself runs in an agent pane this test process is one — so
/// `start_ui`'s in-process board is (CAD-380). `ui start` hands the
/// server to a fresh session leader (`setsid`) that is reparented off
/// this process's ancestry once `start` exits, `env_clear` leaves no
/// `CADENCE_ALIAS`, and stdio is a log file, not a pane tty — the shape
/// `peer::operator_proof` accepts, as `TestDaemon::operator_rpc` does in
/// tests/common/mod.rs (CAD-291). The gate itself is untouched.
/// `free_port` is a bind-release race, so a failed start retries.
pub fn start_operator_ui(pm: &Path, state: &Path) -> (u16, DetachedUi) {
    let guard = DetachedUi(state.to_path_buf());
    let overall = Instant::now() + Duration::from_secs(30);
    loop {
        let port = free_port();
        let mut cmd = Command::new(bin());
        cmd.arg("--state-dir")
            .arg(state)
            .args(["ui", "start", "--port", &port.to_string()])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", pm)
            .env("CADENCE_PM_DIR", pm)
            .stdin(std::process::Stdio::null());
        // CAD-482: on a test-seam build the detached board arms the
        // seam (its daemon minted the credential) and asserts the
        // operator identity on every daemon call it relays — the
        // detached process is the production shape of `ui start`; the
        // envs only say WHO it runs as, identically in a pane and CI.
        if cfg!(feature = "test-seam") {
            cmd.env(cadence_agent::test_seam::ARM_ENV, "1")
                .env(cadence_agent::test_seam::AS_ENV, "operator");
        }
        let out = cmd.output().unwrap();
        if out.status.success() {
            return (port, guard);
        }
        // A start that timed out leaves its pid file — clear it, or the
        // retry answers `already_running` on the old port.
        drop(DetachedUi(state.to_path_buf()));
        assert!(
            Instant::now() < overall,
            "operator ui did not start: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// `cadence …` returning (ok, stdout, stderr) — load warnings go to
/// stderr while JSON stays on stdout, and cli_run merges them away.
pub fn cli_out_err(pm: &Path, state: &Path, args: &[&str]) -> (bool, String, String) {
    let mut cmd = Command::new(bin());
    cmd.arg("--state-dir")
        .arg(state)
        .args(args)
        .env("CADENCE_PM_DIR", pm)
        .env(
            "PATH",
            format!(
                "{}:{}",
                Path::new(bin()).parent().unwrap().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env_remove("CADENCE_ALIAS");
    if let Ok(home) = std::env::var("HOME") {
        cmd.env("HOME", home);
    }
    let out = cmd.output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}
// ==== CAD-83: `cadence overview` + /api/meta + /api/overview ====

/// A fake `gh` binary dir: `pr` calls answer $FAKE_GH_PRS, the
/// `ci.yml` runs listing answers $FAKE_GH_RUNS (default: no runs), the
/// repo read answers default branch `main`; FAKE_GH_FAIL=1 makes every
/// call exit 1. The call log records argv lines.
pub const FAKE_GH: &str = r#"#!/bin/sh
echo "$*" >> "$FAKE_GH_LOG"
if [ "$FAKE_GH_FAIL" = "1" ]; then echo "gh: simulated outage" >&2; exit 1; fi
no_runs='{"total_count": 0, "workflow_runs": []}'
case "$1 $2" in
  pr\ *) printf '%s' "$FAKE_GH_PRS" ;;
  "api repos/"*/actions/workflows/*) printf '%s' "${FAKE_GH_RUNS:-$no_runs}" ;;
  "api repos/"*) printf '%s' '{"default_branch": "main"}' ;;
  *) exit 1 ;;
esac
"#;

/// Fake-gh dir + call log; drop keeps the tempdir alive for the test.
pub struct FakeGh {
    pub _tmp: TempDir,
    pub bin: PathBuf,
    pub log: PathBuf,
}

pub fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}
