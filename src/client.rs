//! CLI-side socket client: one request/response per connection.
//! Long waits (`ask`, `events --follow`) are driven client-side by
//! repeating requests, so a hung call never holds a daemon slot.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{Error, Result};
use crate::proto;

/// State directory: `$CADENCE_STATE_DIR`, else the resolved home's
/// `state/` — `$XDG_STATE_HOME/cadence` or `~/.local/state/cadence`
/// under the legacy layout ([`crate::home`]).
pub fn state_dir() -> Result<PathBuf> {
    crate::home::state_dir()
}

/// The state directory `state_dir` resolves with `CADENCE_STATE_DIR`
/// unset — the production default a sandbox must never reach.
pub fn default_state_dir() -> Result<PathBuf> {
    crate::home::state_default()
}

pub fn socket_path(state_dir: &Path) -> PathBuf {
    state_dir.join("cadence.sock")
}

fn rpc_socket_path(state_dir: &Path) -> Result<PathBuf> {
    // ADR 0007 T3: an agent-uid pane gets only the shared socket path,
    // never the operator's private state-dir path. Unset preserves the
    // original behavior for every operator CLI and existing fixture.
    match std::env::var_os("CADENCE_SOCKET") {
        Some(path) => {
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                return Err(Error::rejected("CADENCE_SOCKET must be an absolute path"));
            }
            Ok(path)
        }
        None => Ok(socket_path(state_dir)),
    }
}

/// The briefing file an actor agent reads —
/// `<state>/briefings/<root>/BRIEFING-<alias>.md` — the operator-owned
/// source copy. Split-mode pty panes get a separate lane copy. The
/// `<root>` segment is the upstream PM's
/// alias when `params` wires one, else the agent's own alias.
pub fn briefing_path(state_dir: &Path, params: &Value, alias: &str) -> PathBuf {
    let root = params["upstream"].as_str().unwrap_or(alias);
    state_dir
        .join("briefings")
        .join(root)
        .join(format!("BRIEFING-{alias}.md"))
}

/// Agent-UID panes cannot traverse the operator's private state dir.
/// Their briefing copy lives under the lane, using the same root name.
pub fn lane_briefing_path(cwd: &Path, params: &Value, alias: &str) -> PathBuf {
    let root = params["upstream"].as_str().unwrap_or(alias);
    cwd.join(".cadence")
        .join(root)
        .join(format!("BRIEFING-{alias}.md"))
}

/// `cadence daemon start`: spawn `<this binary> --state-dir <dir>
/// daemon run` detached (own session, output to `daemon.log`) and wait
/// for the socket to answer. Reports `already_running` when another
/// process holds the singleton lock, `started` only when the process
/// this call spawned became the daemon.
pub fn daemon_start(state_dir: &Path) -> Result<Value> {
    daemon_start_as(state_dir, None)
}

/// Read bound for one `health` probe while starting — a wedged
/// daemon must not hang `daemon start` for the full rpc timeout.
const START_HEALTH_TIMEOUT: Duration = Duration::from_secs(5);

/// How long `daemon start` waits for the child it spawned to answer
/// `health`. CAD-1251: a cold start on a loaded host took over the old
/// 10 s, and the failed restart rolled a healthy build back. Still
/// bounded below the update's 90 s health wait; a child that exits
/// (lost the singleton lock, crashed) ends the wait at once.
const DAEMON_START_BUDGET: Duration = Duration::from_secs(60);

/// `daemon_start`, forwarding a rollout identity to the child so
/// `daemon run` can prove the caller holds the lease when the binary
/// commit differs from the one last recorded.
///
/// The verdict comes from the singleton lock, never from "the socket
/// answered": only the process holding `cadence.lock` binds the socket,
/// and `health` reports that process's pid. So:
/// - a daemon already answers before we spawn → `already_running`,
///   nothing spawned;
/// - we spawned, and `health` reports our child's pid → `started`;
/// - we spawned, and `health` reports another pid → our child lost the
///   lock (a concurrent start won the race between our check and our
///   spawn). We wait for the child to exit on the lock, then report
///   `already_running`. If the other daemon exits first and our child
///   takes the lock after all, its pid answers and we report `started`.
///
/// Every return other than `started` — `already_running` or an error —
/// first makes sure the child this call spawned is gone (see
/// [`SpawnedDaemon`]): a child still alive then has not reached the
/// lock yet and would take it later, once the daemon we reported exits.
/// Only that child is ever signalled, by its `Child` handle.
///
/// The pre-spawn check is a `health` call, not a lock probe: a
/// try-lock-and-release would itself hold the lock for an instant and
/// can make a concurrent start's child fail its non-blocking lock with
/// no daemon left running.
pub fn daemon_start_as(state_dir: &Path, as_identity: Option<&str>) -> Result<Value> {
    use std::time::Instant;
    let forward = crate::rollout::authorize_daemon_spawn(state_dir, as_identity)?;
    if !precheck_forced_to_fail() {
        if let Ok(health) = rpc_timeout(
            state_dir,
            "health",
            serde_json::json!({}),
            START_HEALTH_TIMEOUT,
        ) {
            return Ok(already_running(state_dir, health));
        }
    }
    let mut child = SpawnedDaemon::new(spawn_daemon_run(
        state_dir,
        forward.as_deref().or(as_identity),
    )?);
    let child_pid = u64::from(child.id());
    let mut last_error = None;
    let began = Instant::now();
    let deadline = began + DAEMON_START_BUDGET;
    loop {
        // This round's `health` answer, when it came from a daemon that
        // is not our child.
        let foreign = match rpc_timeout(
            state_dir,
            "health",
            serde_json::json!({}),
            START_HEALTH_TIMEOUT,
        ) {
            Ok(health) if health["pid"].as_u64() == Some(child_pid) => {
                child.keep();
                // CAD-1251: log the cold start so the window can be sized.
                eprintln!(
                    "daemon: answered health {:.1}s after start (budget {}s)",
                    began.elapsed().as_secs_f64(),
                    DAEMON_START_BUDGET.as_secs()
                );
                return Ok(serde_json::json!({
                    "state": "started",
                    "pid": child.id(),
                    "socket": socket_path(state_dir),
                    "health": health,
                }));
            }
            Ok(health) => Some(health),
            Err(e) => {
                last_error = Some(e);
                None
            }
        };
        // Our child exiting means it never became the daemon; the
        // lock holder that answered is the one running. Until one
        // answers, the holder may still be binding the socket.
        let child_exited = child.try_wait()?.is_some();
        let timed_out = Instant::now() >= deadline;
        if child_exited || timed_out {
            if let Some(health) = foreign {
                return Ok(already_running(state_dir, health));
            }
        }
        if timed_out {
            // A probe that timed out reads as `io: Resource temporarily
            // unavailable (os error 11)` — the socket's read timeout,
            // not a lock — so name what happened.
            return Err(Error::internal(format!(
                "daemon did not answer health within {}s (last probe: {})",
                DAEMON_START_BUDGET.as_secs(),
                last_error.map_or_else(|| "none".to_string(), |e| e.to_string())
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Debug/test builds only: `CADENCE_TEST_START_PRECHECK_FAILS=1` makes
/// `daemon start` treat its pre-spawn `health` check as failed — as when
/// it times out under load while a daemon is live — so it spawns a child
/// anyway. Release builds never read it.
fn precheck_forced_to_fail() -> bool {
    cfg!(debug_assertions)
        && std::env::var_os("CADENCE_TEST_START_PRECHECK_FAILS").is_some_and(|v| v == "1")
}

/// How long a spawned child that did not become the daemon gets to
/// exit on the singleton lock by itself before it is sent SIGTERM.
const CHILD_EXIT_GRACE: Duration = Duration::from_secs(3);
/// How long after SIGTERM before SIGKILL.
const CHILD_TERM_WAIT: Duration = Duration::from_secs(2);

/// The `daemon run` child one `daemon start` spawned. Unless [`keep`]
/// was called (the `started` path), dropping it ends the child and
/// reaps it: wait for it to exit on the lock, else SIGTERM, else
/// SIGKILL — all bounded. It signals only this child, through its
/// handle; the child is unreaped until then, so its pid cannot be
/// reused.
///
/// [`keep`]: SpawnedDaemon::keep
struct SpawnedDaemon {
    child: std::process::Child,
    keep: bool,
}

impl SpawnedDaemon {
    fn new(child: std::process::Child) -> Self {
        Self { child, keep: false }
    }

    fn id(&self) -> u32 {
        self.child.id()
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    /// The child became the daemon: leave it running.
    fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for SpawnedDaemon {
    fn drop(&mut self) {
        if !self.keep {
            end_child(&mut self.child, CHILD_EXIT_GRACE, CHILD_TERM_WAIT);
        }
    }
}

/// Make sure `child` has exited and is reaped: wait up to `grace` for
/// it to exit by itself, then SIGTERM it and wait up to `term_wait`,
/// then SIGKILL it and wait.
fn end_child(child: &mut std::process::Child, grace: Duration, term_wait: Duration) {
    if exited_within(child, grace) {
        return;
    }
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: plain kill(2) on our own unreaped child's pid.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    if exited_within(child, term_wait) {
        return;
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Poll `child` until it has exited (reaping it) or `bound` passes.
fn exited_within(child: &mut std::process::Child, bound: Duration) -> bool {
    let deadline = std::time::Instant::now() + bound;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {}
            // Not waitable: there is nothing left of it to end.
            Err(_) => return true,
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn already_running(state_dir: &Path, health: Value) -> Value {
    serde_json::json!({
        "state": "already_running",
        "socket": socket_path(state_dir),
        "health": health,
    })
}

/// Spawn `daemon run` detached: its own session, output to
/// `daemon.log`, and the rollout identity only as an argument.
fn spawn_daemon_run(state_dir: &Path, identity: Option<&str>) -> Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let exe = std::env::current_exe()?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state_dir.join("daemon.log"))?;
    let mut command = std::process::Command::new(exe);
    command.env_remove("CADENCE_ROLLOUT_AS");
    // CAD-482: a daemon is never a caller — a restart under
    // `CADENCE_TEST_AS` must not let the child assert on its own
    // outbound RPCs. The child's `daemon run` re-arms from the state
    // dir's minted token instead.
    command.env_remove(crate::test_seam::AS_ENV);
    command
        .args(["--state-dir"])
        .arg(state_dir)
        .args(["daemon", "run"]);
    if let Some(identity) = identity {
        command.arg("--rollout-as").arg(identity);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(crate::reaper::spawn(&mut command)?)
}

/// Send one request, return the result value or the wire error.
pub fn rpc(state_dir: &Path, method: &str, params: Value) -> Result<Value> {
    rpc_timeout(state_dir, method, params, Duration::from_secs(700))
}

/// CAD-447: hand an answer report just filed on `issue` to the daemon,
/// which queues it to the author of the question it answers. The
/// answer stands whatever happens here: the daemon's reply, or its
/// refusal as `{"sent": false, "error"}`, is returned for the caller to
/// show.
pub fn route_answer(state_dir: &Path, issue: &str, report: &str) -> Value {
    let params = serde_json::json!({"issue": issue, "report": report});
    match rpc_relay_timeout(state_dir, "answer_route", params, Duration::from_secs(10)) {
        Ok(v) => v,
        Err(e) => serde_json::json!({"sent": false, "error": e.to_string()}),
    }
}

/// Why an [`route_answer`] reply did not tell the asker — `None` when it
/// was sent now or earlier (a duplicate).
pub fn answer_not_told(route: &Value) -> Option<String> {
    if route["sent"] == true || route["duplicate"] == true {
        return None;
    }
    let why = ["why", "undeliverable", "error"]
        .iter()
        .find_map(|k| route[*k].as_str())
        .unwrap_or("no reason given");
    Some(match route["to"].as_str() {
        Some(to) => format!("{to}: {why}"),
        None => why.to_string(),
    })
}

/// `rpc` with a caller-chosen read bound — best-effort callers
/// (status footer, doctor) must degrade in a second or two rather
/// than hang a screen on a wedged daemon.
pub fn rpc_timeout(
    state_dir: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value> {
    proto::unwrap(rpc_frame(state_dir, method, params, timeout)?)
}

/// CAD-1193: the shared deadline for the read-only dependency RPCs one
/// `/api/meta` response chains — session validation, the board/agent
/// identity facts behind the operator proof, daemon build info. Those
/// reads inform a courtesy answer only: a stalled or missing
/// dependency must cost the request seconds, not the default 700 s
/// call bound, and must produce an explicit unknown/unavailable
/// answer, never a granted authority or a manufactured sign-out.
///
/// This is purely a transport bound for existing RPCs: it supplies no
/// answer, no identity and no authority of its own, changes no rule
/// the daemon enforces, and is never applied to a write. One `Meta`
/// request shares one budget across every dependent read, so a slow
/// daemon cannot consume the bound serially call after call.
#[derive(Clone, Copy, Debug)]
pub struct MetaBudget {
    deadline: Instant,
}

impl MetaBudget {
    /// The bound `/api/meta` works under end to end — the ticket's
    /// five-second target.
    pub const LIMIT: Duration = Duration::from_secs(5);
    /// The most one dependent read may wait: the whole remaining
    /// budget, capped so a stalled call cannot consume it all while
    /// earlier facts are still unproven.
    pub const READ_CAP: Duration = Duration::from_secs(2);

    /// A budget whose deadline is `limit` from now.
    pub fn fresh(limit: Duration) -> Self {
        Self {
            deadline: Instant::now() + limit,
        }
    }

    /// A budget that allows no further RPC time — the request's bound
    /// is already spent before the first dependent read.
    pub fn exhausted() -> Self {
        Self {
            deadline: Instant::now(),
        }
    }

    /// This call's absolute expiry — the earlier of the shared request
    /// deadline and the per-read cap — so one dependency cannot spend
    /// the whole budget while earlier facts are still unproven.
    fn call_deadline(&self) -> Instant {
        self.deadline.min(Instant::now() + Self::READ_CAP)
    }

    /// One bounded required read for courtesy metadata. Returns the
    /// daemon's real answer, or `Err` on transport failure, refusal or
    /// an exhausted budget — callers turn that into unknown, never a
    /// fabricated `true`/`false`.
    ///
    /// The whole call — connect, the request write and the reply read
    /// — must finish by one absolute instant, [`call_deadline`]: a
    /// full socket backlog, a stalled accept, a peer that accepts no
    /// bytes or a trickled reply all draw on the same bound, which the
    /// per-phase timeout on the general [`rpc_timeout`] path cannot
    /// guarantee. An expired budget does not even connect. What the
    /// bound does NOT cover: time the daemon spends answering inside
    /// its own process past this instant is only abandoned, not
    /// cancelled, and the board's own `/proc` walks keep whatever
    /// latency the filesystem has — this transport bounds the socket
    /// dependency only.
    ///
    /// [`call_deadline`]: MetaBudget::call_deadline
    pub fn read(&self, state_dir: &Path, method: &str, params: Value) -> Result<Value> {
        let socket = rpc_socket_path(state_dir)?;
        proto::unwrap(rpc_deadline_frame_on(
            state_dir,
            &socket,
            method,
            params,
            self.call_deadline(),
        )?)
    }

    /// The same bounded read pinned to the private socket — the boot
    /// UID authority, which must never honor an inherited
    /// `CADENCE_SOCKET`.
    pub fn read_private(&self, state_dir: &Path, method: &str, params: Value) -> Result<Value> {
        proto::unwrap(rpc_deadline_frame_on(
            state_dir,
            &socket_path(state_dir),
            method,
            params,
            self.call_deadline(),
        )?)
    }
}

/// One request/response frame under a single absolute `deadline`
/// covering connect, the request write and the reply read together.
/// Only [`MetaBudget`] uses this: a courtesy metadata answer must
/// degrade inside seconds even when the dependency's listen backlog
/// is full (a blocking `UnixStream::connect` has no timeout), when the
/// peer accepts no bytes (the general path's write is unbounded), or
/// when the reply trickles (a per-read timeout resets on every byte).
/// The wire frames are identical to [`rpc_frame_on`]'s; the deadline
/// only decides when this side stops waiting — it supplies no answer,
/// no identity and no authority of its own, and there is no retry or
/// replay: the call happens once or not at all.
fn rpc_deadline_frame_on(
    state_dir: &Path,
    socket: &Path,
    method: &str,
    params: Value,
    deadline: Instant,
) -> Result<Value> {
    // An already-spent budget connects nothing: build the frame and
    // fail before the socket is even created.
    if Instant::now() >= deadline {
        return Err(Error::busy("metadata read deadline reached"));
    }
    let mut request = proto::request(method, params);
    // CAD-482: a test-seam caller asserts its identity on the frame —
    // scoped in-process ([`crate::test_seam::scoped`]) or via
    // `CADENCE_TEST_AS` in a spawned test binary. Absent the feature
    // this attaches nothing.
    if let Some(test_caller) = crate::test_seam::caller_frame(state_dir)? {
        request[crate::test_seam::FRAME_FIELD] = test_caller;
    }
    let mut body = request.to_string();
    body.push('\n');
    let stream = connect_bounded(socket, deadline).map_err(|e| match e {
        // A spent deadline is reported as spent, never as "not
        // reachable" — the distinction keeps a timeout from looking
        // like an absent daemon.
        e @ Error::Structured(_) => e,
        _ => Error::internal(format!(
            "Daemon is not reachable at {} — start it with `cadence daemon start`",
            socket.display()
        )),
    })?;
    let fd = stream.as_raw_fd();
    write_bounded(fd, body.as_bytes(), deadline)?;
    let line = read_line_bounded(fd, deadline)?;
    serde_json::from_str(&line).map_err(|_| Error::internal("Daemon returned a malformed response"))
}

/// Poll `fd` for `events` until it signals or `deadline` passes.
/// `Ok(false)` is the spent deadline — the caller reports it; a real
/// readiness (data, hangup, error) is `Ok(true)` and the next syscall
/// surfaces which. EINTR recomputes the remaining time and waits on.
fn poll_deadline(fd: RawFd, events: libc::c_short, deadline: Instant) -> Result<bool> {
    loop {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            return Ok(false);
        };
        let millis = left.as_millis().clamp(1, i32::MAX as u128) as i32;
        let mut fds = [libc::pollfd {
            fd,
            events,
            revents: 0,
        }];
        // SAFETY: `fds` points to one valid pollfd for the call; `fd`
        // is a live descriptor the caller keeps open across it.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 1, millis) };
        if ready > 0 {
            if fds[0].revents & libc::POLLNVAL != 0 {
                return Err(Error::internal("metadata socket became invalid"));
            }
            return Ok(true);
        }
        if ready == 0 {
            return Ok(false);
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e.into());
        }
    }
}

/// connect(2) on a fresh nonblocking unix socket, completed or
/// abandoned by `deadline`. Unlike `UnixStream::connect` — which can
/// block far past a read bound on a full listen backlog — this waits
/// on POLLOUT and reads the verdict from SO_ERROR.
fn connect_bounded(socket: &Path, deadline: Instant) -> Result<UnixStream> {
    // SAFETY: fixed, valid socket(2) arguments.
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    match connect_on(fd, socket, deadline) {
        Ok(()) => {
            // SAFETY: `fd` is a live connected descriptor we own.
            Ok(unsafe { UnixStream::from_raw_fd(fd) })
        }
        Err(e) => {
            // SAFETY: `fd` is a live descriptor we still own.
            unsafe { libc::close(fd) };
            Err(e)
        }
    }
}

/// The connect itself: start it, wait for writability inside
/// `deadline`, then read SO_ERROR for the kernel's verdict.
fn connect_on(fd: RawFd, socket: &Path, deadline: Instant) -> Result<()> {
    let path = socket.as_os_str().as_bytes();
    // SAFETY: an all-zero sockaddr_un is a valid starting value.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if path.len() >= addr.sun_path.len() {
        return Err(Error::internal(format!(
            "socket path is too long: {}",
            socket.display()
        )));
    }
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    // The slot is zeroed, so the byte after `path` is already NUL.
    for (slot, byte) in addr.sun_path.iter_mut().zip(path.iter().copied()) {
        *slot = byte as libc::c_char;
    }
    let len =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + path.len() + 1) as libc::socklen_t;
    // SAFETY: `addr` is a sockaddr_un valid for `len` bytes; `fd` is a
    // live nonblocking unix stream socket.
    let rc = unsafe {
        libc::connect(
            fd,
            (&addr as *const libc::sockaddr_un).cast::<libc::sockaddr>(),
            len,
        )
    };
    if rc == 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    if e.raw_os_error() != Some(libc::EINPROGRESS) && e.kind() != std::io::ErrorKind::Interrupted {
        return Err(e.into());
    }
    // EINPROGRESS (and EINTR, whose attempt continues asynchronously):
    // the verdict arrives as writability plus SO_ERROR.
    if !poll_deadline(fd, libc::POLLOUT, deadline)? {
        return Err(Error::busy(
            "metadata read deadline reached while connecting",
        ));
    }
    let mut so_err: libc::c_int = 0;
    let mut optlen = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: `fd` is a live socket; `so_err`/`optlen` are the proper
    // out-parameters for SO_ERROR.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            (&mut so_err as *mut libc::c_int).cast::<libc::c_void>(),
            &mut optlen,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if so_err == 0 {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(so_err).into())
    }
}

/// `write_all` bounded by the absolute `deadline`: a peer whose
/// buffers never drain fails the call at the deadline instead of
/// holding it, the way the general path's unbounded blocking write
/// can. Retries on EAGAIN and EINTR — with the deadline re-checked
/// each round — never grant the peer more time.
fn write_bounded(fd: RawFd, mut bytes: &[u8], deadline: Instant) -> Result<()> {
    while !bytes.is_empty() {
        if !poll_deadline(fd, libc::POLLOUT, deadline)? {
            return Err(Error::busy("metadata read deadline reached while writing"));
        }
        // SAFETY: `fd` is a live nonblocking socket; `bytes` is a
        // valid slice advanced only by the count write(2) reports.
        let n = unsafe { libc::write(fd, bytes.as_ptr().cast::<libc::c_void>(), bytes.len()) };
        if n > 0 {
            bytes = &bytes[n as usize..];
            continue;
        }
        let e = std::io::Error::last_os_error();
        match e.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted => continue,
            _ => return Err(e.into()),
        }
    }
    Ok(())
}

/// Read one reply line bounded by the absolute `deadline`. The
/// deadline never extends — received bytes cannot reset it, so a
/// trickled reply cannot out-wait the budget the way it out-waits the
/// general path's per-read inactivity timeout. Peer EOF before the
/// newline is a closed connection, not an answer.
fn read_line_bounded(fd: RawFd, deadline: Instant) -> Result<String> {
    let mut line = Vec::new();
    let mut buf = [0u8; 2048];
    loop {
        if !poll_deadline(fd, libc::POLLIN, deadline)? {
            return Err(Error::busy(
                "metadata read deadline reached awaiting the reply",
            ));
        }
        // SAFETY: `fd` is a live nonblocking socket; `buf` is valid
        // for buf.len() bytes.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len()) };
        if n > 0 {
            let chunk = &buf[..n as usize];
            match chunk.iter().position(|b| *b == b'\n') {
                Some(end) => {
                    line.extend_from_slice(&chunk[..end]);
                    break;
                }
                None => line.extend_from_slice(chunk),
            }
            continue;
        }
        if n == 0 {
            return Err(Error::internal("daemon closed the connection mid-reply"));
        }
        let e = std::io::Error::last_os_error();
        match e.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted => continue,
            _ => return Err(e.into()),
        }
    }
    String::from_utf8(line).map_err(|_| Error::internal("Daemon returned a malformed response"))
}

/// Query the daemon bound to this private state directory, ignoring
/// `CADENCE_SOCKET`. The board uses this for boot-pinned UID authority;
/// an inherited agent socket override must never choose that source.
pub fn rpc_private_timeout(
    state_dir: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value> {
    proto::unwrap(rpc_frame_on(
        state_dir,
        &socket_path(state_dir),
        method,
        params,
        timeout,
    )?)
}

/// `rpc` that tells the two failures apart: the outer `Err` is the
/// transport — no daemon at the socket, an I/O error, a malformed
/// frame — and the inner result is the daemon's own answer, a refusal
/// included (CAD-384: `daemon restart` must not read a caller-rule
/// refusal of `shutdown` as "not running").
pub fn rpc_answer(state_dir: &Path, method: &str, params: Value) -> Result<Result<Value>> {
    Ok(proto::unwrap(rpc_frame(
        state_dir,
        method,
        params,
        Duration::from_secs(700),
    )?))
}

/// CAD-508 retry budget for connects across the daemon restart window.
const RELAY_BUDGET: Duration = Duration::from_secs(60);
const RELAY_STEP: Duration = Duration::from_millis(50);
const RELAY_STEP_MAX: Duration = Duration::from_millis(500);

fn retryable_connect_error(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

/// Retry only the local socket connect for durable CLI operations. Once
/// connected, [`rpc_exchange`] sends exactly one request and never replays it.
pub fn rpc_relay(state_dir: &Path, method: &str, params: Value) -> Result<Value> {
    rpc_relay_timeout(state_dir, method, params, Duration::from_secs(700))
}

/// [`rpc_relay`] with a caller-selected response timeout.
pub(crate) fn rpc_relay_timeout(
    state_dir: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value> {
    proto::unwrap(rpc_frame_relay(
        state_dir,
        method,
        params,
        timeout,
        RELAY_BUDGET,
    )?)
}

fn rpc_frame_relay(
    state_dir: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
    budget: Duration,
) -> Result<Value> {
    let socket = rpc_socket_path(state_dir)?;
    let deadline = Instant::now() + budget;
    let mut step = RELAY_STEP;
    loop {
        match UnixStream::connect(&socket) {
            Ok(stream) => return rpc_exchange(stream, state_dir, method, params, timeout),
            Err(error) if retryable_connect_error(error.kind()) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(daemon_unreachable(&socket));
                }
                std::thread::sleep(step.min(left));
                step = (step * 2).min(RELAY_STEP_MAX);
            }
            Err(_) => return Err(daemon_unreachable(&socket)),
        }
    }
}

/// One request/response frame over the daemon socket.
fn rpc_frame(state_dir: &Path, method: &str, params: Value, timeout: Duration) -> Result<Value> {
    let socket = rpc_socket_path(state_dir)?;
    rpc_frame_on(state_dir, &socket, method, params, timeout)
}

fn rpc_frame_on(
    state_dir: &Path,
    socket: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value> {
    let stream = UnixStream::connect(socket).map_err(|_| daemon_unreachable(socket))?;
    rpc_exchange(stream, state_dir, method, params, timeout)
}

fn daemon_unreachable(socket: &Path) -> Error {
    Error::internal(format!(
        "Daemon is not reachable at {} — start it with `cadence daemon start`",
        socket.display()
    ))
}

fn rpc_exchange(
    mut stream: UnixStream,
    state_dir: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value> {
    stream.set_read_timeout(Some(timeout))?;
    let mut request = proto::request(method, params);
    // CAD-482: a test-seam caller asserts its identity on the frame —
    // scoped in-process ([`crate::test_seam::scoped`]) or via
    // `CADENCE_TEST_AS` in a spawned test binary. Absent the feature
    // this attaches nothing.
    if let Some(test_caller) = crate::test_seam::caller_frame(state_dir)? {
        request[crate::test_seam::FRAME_FIELD] = test_caller;
    }
    writeln!(stream, "{request}")?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(|_| Error::internal("Daemon returned a malformed response"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    fn serve_later(
        socket: PathBuf,
        delay: Duration,
        replies: Vec<Value>,
    ) -> std::thread::JoinHandle<usize> {
        use std::os::unix::net::UnixListener;
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let listener = UnixListener::bind(socket).unwrap();
            let mut served = 0;
            for reply in replies {
                let Ok((stream, _)) = listener.accept() else {
                    break;
                };
                served += 1;
                let mut request = String::new();
                let _ = BufReader::new(&stream).read_line(&mut request);
                let _ = writeln!(&stream, "{reply}");
            }
            served
        })
    }

    #[test]
    fn relay_retries_only_absent_or_refused_connects() {
        assert!(retryable_connect_error(std::io::ErrorKind::NotFound));
        assert!(retryable_connect_error(
            std::io::ErrorKind::ConnectionRefused
        ));
        assert!(!retryable_connect_error(
            std::io::ErrorKind::PermissionDenied
        ));
        assert!(!retryable_connect_error(std::io::ErrorKind::InvalidInput));
    }

    #[test]
    fn relay_waits_for_late_socket_and_sends_once() {
        let dir = tempfile::tempdir().unwrap();
        let server = serve_later(
            dir.path().join("cadence.sock"),
            Duration::from_millis(180),
            vec![proto::ok(serde_json::json!("ok"))],
        );
        let started = Instant::now();
        let out = rpc_frame_relay(
            dir.path(),
            "agent_send",
            Value::Null,
            Duration::from_secs(2),
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(out["ok"], true);
        assert!(started.elapsed() >= Duration::from_millis(180));
        assert_eq!(server.join().unwrap(), 1);
    }

    #[test]
    fn relay_retries_refused_stale_socket_then_accepts_once() {
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("cadence.sock");
        drop(UnixListener::bind(&socket).unwrap());
        let stale = socket.clone();
        let server = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            std::fs::remove_file(&stale).unwrap();
            let listener = UnixListener::bind(&stale).unwrap();
            let (stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            let _ = BufReader::new(&stream).read_line(&mut request);
            writeln!(&stream, "{}", proto::ok(Value::Null)).unwrap();
        });
        rpc_frame_relay(
            dir.path(),
            "message_read",
            Value::Null,
            Duration::from_secs(2),
            Duration::from_secs(2),
        )
        .unwrap();
        server.join().unwrap();
    }

    #[test]
    fn relay_does_not_replay_after_server_accepts_and_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let refusal =
            serde_json::json!({"ok": false, "error": {"kind": "rejected", "message": "no"}});
        let server = serve_later(
            dir.path().join("cadence.sock"),
            Duration::ZERO,
            vec![refusal.clone()],
        );
        let started = Instant::now();
        assert_eq!(
            rpc_frame_relay(
                dir.path(),
                "message_report",
                Value::Null,
                Duration::from_secs(2),
                Duration::from_secs(2),
            )
            .unwrap(),
            refusal
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(server.join().unwrap(), 1);
    }

    #[test]
    fn relay_does_not_replay_when_an_accepted_connection_loses_its_reply() {
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(dir.path().join("cadence.sock")).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            let _ = BufReader::new(&stream).read_line(&mut request);
            drop(stream);
            1
        });
        let error = rpc_frame_relay(
            dir.path(),
            "message_report",
            Value::Null,
            Duration::from_millis(100),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(!error.to_string().contains("Daemon is not reachable"));
        assert_eq!(server.join().unwrap(), 1);
    }

    #[test]
    fn concurrent_relays_connect_once_each_after_the_socket_appears() {
        let dir = tempfile::tempdir().unwrap();
        let server = serve_later(
            dir.path().join("cadence.sock"),
            Duration::from_millis(120),
            vec![proto::ok(Value::Null); 4],
        );
        let calls: Vec<_> = (0..4)
            .map(|_| {
                let state = dir.path().to_path_buf();
                std::thread::spawn(move || {
                    rpc_frame_relay(
                        &state,
                        "agent_send",
                        Value::Null,
                        Duration::from_secs(2),
                        Duration::from_secs(2),
                    )
                })
            })
            .collect();
        for call in calls {
            assert_eq!(call.join().unwrap().unwrap()["ok"], true);
        }
        assert_eq!(server.join().unwrap(), 4);
    }

    #[test]
    fn relay_budget_exhaustion_uses_existing_unreachable_error() {
        let dir = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let error = rpc_frame_relay(
            dir.path(),
            "agent_inbox",
            Value::Null,
            Duration::from_secs(2),
            Duration::from_millis(120),
        )
        .unwrap_err();
        assert!(error.to_string().contains("Daemon is not reachable"));
        assert!(started.elapsed() >= Duration::from_millis(120));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn end_child_waits_for_a_child_that_exits_by_itself() {
        let mut child = Command::new("sh").args(["-c", "exit 7"]).spawn().unwrap();
        end_child(&mut child, Duration::from_secs(10), Duration::from_secs(10));
        let status = child.try_wait().unwrap().expect("reaped");
        assert_eq!(status.code(), Some(7), "{status:?}");
    }

    #[test]
    fn end_child_terms_a_child_still_running_after_the_grace() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let begin = Instant::now();
        end_child(
            &mut child,
            Duration::from_millis(200),
            Duration::from_secs(10),
        );
        let status = child.try_wait().unwrap().expect("reaped");
        assert_eq!(status.signal(), Some(libc::SIGTERM), "{status:?}");
        assert!(begin.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn end_child_kills_a_child_that_ignores_sigterm() {
        let mut child = Command::new("sh")
            .args(["-c", "trap '' TERM; echo ready; exec sleep 30"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready.trim(), "ready");
        end_child(
            &mut child,
            Duration::from_millis(100),
            Duration::from_millis(300),
        );
        let status = child.try_wait().unwrap().expect("reaped");
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{status:?}");
    }

    #[test]
    fn a_kept_spawned_daemon_is_left_running() {
        let mut spawned = SpawnedDaemon::new(Command::new("sleep").arg("30").spawn().unwrap());
        spawned.keep();
        let pid = libc::pid_t::try_from(spawned.id()).unwrap();
        let begin = Instant::now();
        drop(spawned);
        assert!(begin.elapsed() < CHILD_EXIT_GRACE);
        // Still ours and unreaped: signal 0 finds it, and it is no zombie.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let running = stat
            .rsplit(')')
            .next()
            .is_some_and(|rest| !rest.trim_start().starts_with('Z'));
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, std::ptr::null_mut(), 0);
        }
        assert!(
            alive && running,
            "kept child {pid} should still run: {stat}"
        );
    }
}
