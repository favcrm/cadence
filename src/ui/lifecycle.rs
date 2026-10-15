//! The board's process lifecycle (`ui run|start|stop|status`, pid and
//! ready files, detached spawn, and the plain HTTP probe).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::operator;
use super::tailscale::{ensure_mapping, is_ts_offline, prepare_mapping, remove_mapping};
use super::{
    clear_persisted_share, load_opts, load_opts_strict, push_device_login_config,
    refuse_share_port_change, resolve_device_login, resolve_opts, save_opts, serve,
    validate_share_target, UiFlags, UiOpts,
};
use crate::error::{Error, Result};

// ---------- lifecycle (mirrors `daemon start|stop|status`) ----------

pub(crate) fn pid_file(state_dir: &Path) -> PathBuf {
    state_dir.join("ui.pid")
}

/// The file a spawned `ui run` writes once its bind has succeeded —
/// `{pid, nonce}` for the `ui start` that is waiting on it. The nonce
/// reaches the child only through `CADENCE_UI_READY_NONCE` in its
/// environment and this file, never over HTTP: any HTTP 200 a foreign
/// listener answers can no longer stand in for this board's readiness
/// (CAD-817).
pub(crate) fn ready_file(state_dir: &Path) -> PathBuf {
    state_dir.join("ui.ready")
}

/// `ui start`'s env channel for the readiness nonce. The child writes
/// it to `ui.ready` after binding; `serve` removes it from the child's
/// own environment before serving so nothing downstream inherits it.
pub(crate) const READY_NONCE_ENV: &str = "CADENCE_UI_READY_NONCE";

/// The frame a spawned `ui run` writes when its bind fails with a real
/// `AddrInUse` — `{pid, nonce, kind}` for the `ui start` that is
/// waiting on it (CAD-1283). The path is scoped to the writing child's
/// own pid, `ui.startup-failed-{pid}`, so concurrent genuine starts
/// never share a file and no start can overwrite another's proof. A
/// starter trusts it only when pid and nonce are this start's own
/// child and env nonce, so a stale, malformed or foreign frame can
/// never classify an unrelated exit as a bind collision. It carries no
/// role the nonce did not already have — the child's provenance for
/// its own start result.
pub(crate) fn startup_failed_file(state_dir: &Path, pid: u32) -> PathBuf {
    state_dir.join(format!("ui.startup-failed-{pid}"))
}

/// `true` only when `ui.startup-failed-{pid}` is a regular, non-symlink
/// file holding a well-formed frame that exactly names this start's own
/// child `pid`, this start's `nonce`, and the `addr_in_use` kind (CAD-1283).
/// Any miss — absent file, symlink, non-regular file, unreadable,
/// malformed, wrong pid, wrong nonce, unknown kind — is `false`: the
/// failure stays untyped and the caller must treat it as a generic exit,
/// never a bind collision it may retry. The file is opened
/// `O_NOFOLLOW` and read through that fd so a symlink or last-component
/// swap cannot redirect the proof to another file's bytes. Read only
/// after the start's own `child.try_wait()` reported exit — the child's
/// exit plus the exact pid/nonce match is what makes the frame proof
/// rather than a stale leftover from another start.
pub(crate) fn startup_bind_in_use(state_dir: &Path, pid: u32, nonce: &str) -> bool {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    // O_NOFOLLOW refuses a symlink; O_NONBLOCK refuses to hang on a
    // FIFO or device before the regular-file check can reject it.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(startup_failed_file(state_dir, pid));
    let Ok(file) = file else {
        return false;
    };
    let Ok(meta) = file.metadata() else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    // The frame is a small fixed JSON; anything larger is not ours.
    let mut text = String::new();
    if file.take(4097).read_to_string(&mut text).is_err() || text.len() > 4096 {
        return false;
    }
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    v["kind"].as_str() == Some("addr_in_use")
        && v["nonce"].as_str() == Some(nonce)
        && v["pid"].as_u64() == Some(u64::from(pid))
}

/// Read `ui.ready`; `Some(pid)` only when it names `nonce` — a stale
/// or foreign marker is a miss, not a match.
pub(crate) fn ready_pid(state_dir: &Path, nonce: &str) -> Option<i32> {
    let text = std::fs::read_to_string(ready_file(state_dir)).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    if v["nonce"].as_str()? != nonce {
        return None;
    }
    v["pid"].as_i64().and_then(|p| i32::try_from(p).ok())
}

/// Is a detached `cadence ui` server alive for this state dir — the
/// pidfile's pid, identity-checked by [`read_pid`]. `daemon restart
/// --ui` reads this to decide whether to bounce the board. Not a pure
/// read: like `read_pid`, it deletes a `ui.pid` that names no board.
pub fn detached_pid(state_dir: &Path) -> Option<i32> {
    read_pid(state_dir)
}

/// The pidfile's pid, but only when it is this state dir's board
/// (CAD-1081). A live pid is not enough: a `ui.pid` restored into a
/// new container can name pid 1, another process, or a thread id, and
/// trusting it skipped the board start and pointed `ui stop` at a
/// stranger. Anything else is stale.
///
/// Side effect: a pid proven not to be the board (`NotBoard`) deletes
/// `ui.pid` — only if the file still holds what was read, so a
/// concurrent `ui start` keeps its own. Every caller, `detached_pid`
/// included, inherits that; an `Unknown` pid leaves the file alone.
///
/// Linux only: with no `/proc` (macOS) no pid proves itself, so the
/// board reads as stopped there — the macOS board is unsupported until
/// CAD-315.
pub(crate) fn read_pid(state_dir: &Path) -> Option<i32> {
    read_pid_in(Path::new("/proc"), state_dir)
}

fn read_pid_in(proc_root: &Path, state_dir: &Path) -> Option<i32> {
    let path = pid_file(state_dir);
    let text = std::fs::read_to_string(&path).ok()?;
    let pid: i32 = text.trim().parse().ok()?;
    match board_identity(proc_root, pid, state_dir) {
        BoardPid::Board => Some(pid),
        // Unproven either way: not ours to signal, not ours to delete.
        BoardPid::Unknown => None,
        BoardPid::NotBoard => {
            if std::fs::read_to_string(&path).is_ok_and(|now| now == text) {
                let _ = std::fs::remove_file(&path);
            }
            None
        }
    }
}

/// What `/proc` proves about a pidfile's pid. A failed read proves
/// nothing: only a missing entry (ENOENT/ESRCH) or a field that
/// contradicts the board is `NotBoard`; every other error is `Unknown`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BoardPid {
    Board,
    NotBoard,
    Unknown,
}

fn proc_read(dir: &Path, name: &str) -> std::result::Result<Vec<u8>, BoardPid> {
    std::fs::read(dir.join(name)).map_err(|e| gone_or_unknown(&e))
}

fn gone_or_unknown(e: &std::io::Error) -> BoardPid {
    if e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ESRCH) {
        BoardPid::NotBoard
    } else {
        BoardPid::Unknown
    }
}

/// Is `pid` (under `proc_root`, `/proc` outside tests) a live
/// thread-group leader running `cadence … ui run` for `state_dir`?
/// Stricter than state-bridge.sh `kill_pidfile`, which only looks for
/// `cadence` in the cmdline: `Tgid` must be the pid itself (a thread id
/// answers `kill(tid, 0)` too), not a zombie, and the argv must be the
/// one `start_inner` spawns, for this state dir.
pub(crate) fn board_identity(proc_root: &Path, pid: i32, state_dir: &Path) -> BoardPid {
    if pid <= 0 {
        return BoardPid::NotBoard;
    }
    let dir = dir_of(proc_root, pid);
    let status = match proc_read(&dir, "status") {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(v) => return v,
    };
    let field = |name: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .map(str::trim)
    };
    let real_uid = field("Uid:").and_then(|u| u.split_whitespace().next());
    let (Some(tgid), Some(state), Some(uid)) = (field("Tgid:"), field("State:"), real_uid) else {
        return BoardPid::Unknown;
    };
    // The board runs as whoever started it — the owner of this state
    // dir. Never the exe path: an update's recovery stops the board the
    // previous release binary still runs.
    let me = unsafe { libc::getuid() }.to_string();
    if tgid != pid.to_string() || state.starts_with('Z') || uid != me {
        return BoardPid::NotBoard;
    }
    let raw = match proc_read(&dir, "cmdline") {
        Ok(b) => b,
        Err(v) => return v,
    };
    let argv: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    let Some(dir) = board_argv_state_dir(&argv) else {
        return BoardPid::NotBoard;
    };
    // A relative `--state-dir` is the board's, read from its own cwd —
    // never the reader's.
    let dir = if dir.is_relative() {
        match std::fs::read_link(dir_of(proc_root, pid).join("cwd")) {
            Ok(cwd) => cwd.join(dir),
            Err(e) => return gone_or_unknown(&e),
        }
    } else {
        dir
    };
    if same_dir(&dir, state_dir) {
        BoardPid::Board
    } else {
        BoardPid::NotBoard
    }
}

fn dir_of(proc_root: &Path, pid: i32) -> PathBuf {
    proc_root.join(pid.to_string())
}

/// The file names a cadence binary may run under: `cadence` or
/// `cadence-<suffix>` (a side-by-side build such as `cadence-new`).
/// One list for the board identity check and for `dev up|reload
/// --build`, so a build is refused up front rather than started as a
/// board `ui stop` could never find again (CAD-1206).
pub(crate) fn is_cadence_exe_name(name: &str) -> bool {
    name == "cadence" || name.strip_prefix("cadence-").is_some_and(|s| !s.is_empty())
}

/// The `--state-dir` of a board argv — `<…/cadence> … --state-dir
/// <dir> … ui run …`, as `start_inner` has always spawned it — or
/// `None` for anything else, an argv without `--state-dir` included.
pub(crate) fn board_argv_state_dir(argv: &[String]) -> Option<PathBuf> {
    let (exe, args) = argv.split_first()?;
    if !Path::new(exe)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_cadence_exe_name)
        || !args.windows(2).any(|w| w[0] == "ui" && w[1] == "run")
    {
        return None;
    }
    args.iter()
        .position(|a| a == "--state-dir")
        .and_then(|i| args.get(i + 1).cloned())
        .or_else(|| {
            args.iter()
                .find_map(|a| a.strip_prefix("--state-dir=").map(str::to_string))
        })
        .map(PathBuf::from)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

/// Tiny blocking GET — enough for health checks without an HTTP client
/// dependency. `headers` are extra request lines (`Tailscale-User-Login`
/// for the identity probe). Returns `(status, body)`.
/// `pub(crate)` for `doctor --host`'s tailnet probe (CAD-509).
pub(crate) fn http_get(
    host: &str,
    port: u16,
    path: &str,
    req_host: &str,
    headers: &[&str],
) -> Result<(u16, String)> {
    let mut stream = TcpStream::connect((host, port))
        .map_err(|e| Error::internal(format!("ui not reachable at {host}:{port}: {e}")))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut req = format!("GET {path} HTTP/1.0\r\nHost: {req_host}\r\n");
    for h in headers {
        req.push_str(h);
        req.push_str("\r\n");
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes())?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let body = text
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or_default()
        .to_string();
    Ok((status, body))
}

/// `ui run` — foreground. Flags merge over the persisted options but
/// never rewrite them: `ui start` owns persistence.
pub(crate) fn run(state_dir: &Path, flags: &UiFlags) -> Result<i32> {
    if flags.tailscale.is_some() {
        // Foreground sandbox runs have no durable sharing custody. Only
        // the recorded start paths may publish a host-wide mapping.
        crate::sandbox::refuse_global(
            "foreground `ui run --tailscale`; use `ui start --tailscale` for recorded sandbox sharing",
        )?;
    }
    let persisted = load_opts_strict(state_dir)?;
    let (eff, mut so) = resolve_opts(flags, &persisted)?;
    // CAD-841: `--device-login-*` is a thin client — resolve now so a
    // partial triple still fails before any bind, but push only once
    // `serve` owns the port: a failed start must not have already
    // replaced the live config on this state dir's boards (r1). A
    // detached `ui start` child resolves no flags and a scrubbed env,
    // so it never re-pushes.
    so.device_login_push = resolve_device_login(flags)?;
    // `ui run --tailscale` is the foreground opt-in path. Resolution
    // above validates all configuration first; preserve its historical
    // explicit-sharing behavior without moving map effects back into
    // the shared resolver.
    let created = if flags.tailscale.is_some() {
        let ts = eff.tailscale.as_ref().expect("resolved above");
        ensure_mapping(&ts.dns_name, ts.https_port, &ts.target)?
    } else {
        false
    };
    if let Err(e) = serve(state_dir, &crate::issue::default_dir()?, &so) {
        if created {
            if let Some(ts) = &eff.tailscale {
                let _ = remove_mapping(&ts.dns_name, ts.https_port, &ts.target);
            }
        }
        return Err(e);
    }
    Ok(0)
}

/// `ui start` — merge flags over `ui.json`, persist the effective
/// options, spawn a detached `ui run` that reads them back. A
/// persisted tailscale block re-ensures its mapping (idempotent; a
/// foreign mapping on the port is still a hard refusal, an
/// unreachable tailscaled a warning — the board still serves
/// loopback).
pub(crate) fn start(state_dir: &Path, flags: &UiFlags, reset: bool) -> Result<i32> {
    start_inner(state_dir, flags, reset, false)
}

/// CAD-1283: `dev up`'s board-start retry budget transfer. `up` sets it
/// on the `ui start` child process env only (never exported globally);
/// `start_inner` clamps its start budget to min(existing, remaining)
/// and clears the var. A start cannot grant itself more than the outer
/// retry still has, and no other caller is affected.
pub(crate) const START_REMAINING_ENV: &str = "CADENCE_DEV_UP_START_REMAINING_SECS";

/// CAD-1251: the board's start proof (bind, readiness report, health)
/// defaults to 10 s for an interactive `ui start`.
pub(crate) const START_BUDGET_DEFAULT: Duration = Duration::from_secs(10);
/// The budget a daemon restart gives the board it restarts — the same as
/// the update's health wait, so a loaded host is not rolled back for a
/// slow cold start.
pub const START_BUDGET_RESTART: Duration = Duration::from_secs(90);
/// Overrides the start-proof budget, in whole seconds (1..=600). The
/// updater sets it on the `ui start` commands it runs; garbage falls back
/// to the default rather than failing the start.
pub const START_BUDGET_ENV: &str = "CADENCE_UI_START_BUDGET_SECS";

fn start_budget() -> Duration {
    std::env::var(START_BUDGET_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| (1..=600).contains(s))
        .map_or(START_BUDGET_DEFAULT, Duration::from_secs)
}

/// `ui start` with no stdout — for composed callers (session's
/// `--fix`) whose own output must stay a single document.
pub(crate) fn start_quiet(state_dir: &Path, flags: &UiFlags, reset: bool) -> Result<i32> {
    start_inner(state_dir, flags, reset, true)
}

pub(crate) fn start_inner(
    state_dir: &Path,
    flags: &UiFlags,
    reset: bool,
    quiet: bool,
) -> Result<i32> {
    std::fs::create_dir_all(state_dir)?;
    // Resolve a reset against defaults without deleting ui.json first.
    // A refused security-mode transition must leave the running board's
    // saved options intact, including its public identity.
    let recorded = load_opts_strict(state_dir)?;
    let persisted = if reset {
        UiOpts::default()
    } else {
        recorded.clone()
    };
    let (eff, so) = resolve_opts(flags, &persisted)?;
    if let Some(existing) = recorded.tailscale.as_ref() {
        let mut recorded_bind = UiOpts {
            host: eff.host.clone(),
            port: eff.port,
            ..UiOpts::default()
        };
        recorded_bind.tailscale = Some(existing.clone());
        validate_share_target(&recorded_bind)?;
        if reset && flags.tailscale.is_none() {
            return Err(Error::rejected(
                "`ui start --reset` cannot discard a persisted tailnet share — stop sharing first",
            ));
        }
        if let Some(requested_port) = flags.tailscale {
            refuse_share_port_change(Some(existing), requested_port)?;
        }
    }
    // CAD-841 r2: resolve the device-login triple up front — a partial
    // or invalid triple fails before any side effect (a saved ui.json,
    // a spawned child). The push it feeds stays late: post-spawn on
    // the fresh path, live on the running path.
    let device_login_push = resolve_device_login(flags)?;
    let running = read_pid(state_dir);
    if running.is_some() {
        if eff.board_public_only != recorded.board_public_only {
            return Err(Error::rejected(
                "board public-only mode cannot change while the UI is running — stop the UI, then start it with the new mode",
            ));
        }
        if eff.allow_anonymous_reads != recorded.allow_anonymous_reads {
            return Err(Error::rejected(
                "anonymous read policy cannot change while the UI is running — stop the UI, then start it with the new policy",
            ));
        }
        // CAD-841: device login is deliberately absent from the
        // running-change refuses — the daemon owns the config now, and
        // a `--device-login-*` flag push on an already-running board is
        // a live `operator_device_login_set`, not a restart.
        if eff.board_public_only {
            if eff.board != recorded.board || eff.host != recorded.host || eff.port != recorded.port
            {
                return Err(Error::rejected(
                    "running public-only board identity or bind cannot change — stop the UI, then start it with the new configuration",
                ));
            }
            // A saved true flag is not proof the *running* process loaded
            // it (an older build could have saved options before noticing
            // an existing UI). The exact local query is 421 only under
            // the active hosted gate; ordinary health answers 200.
            let active = match http_get(
                &so.host,
                so.port,
                "/api/health?public-only-probe=1",
                &format!("{}:{}", so.host, so.port),
                &[],
            ) {
                Ok((421, body)) => serde_json::from_str::<Value>(&body)
                    .ok()
                    .is_some_and(|v| v["error"] == "hosted board requires its public Host"),
                _ => false,
            };
            if !active {
                return Err(Error::rejected(
                    "running UI has not proved the board public-only gate — stop the UI and start it again",
                ));
            }
        }
    }
    // Validate the exact LocalAPI snapshot and prepare its conditional
    // update before changing local custody. A genuinely offline GET can
    // still allow local startup, without claiming the share is verified.
    let prepared_mapping = if let Some(ts) = &eff.tailscale {
        match prepare_mapping(&ts.dns_name, ts.https_port, &ts.target) {
            Ok(plan) => Some(plan),
            Err(e) if is_ts_offline(&e) => {
                eprintln!("warning: {e} — tailnet state is unverified; continuing local startup with recorded sharing settings");
                None
            }
            Err(e) => return Err(e),
        }
    } else {
        None
    };
    // Persist matching sharing custody before the conditional update.
    // Once a POST is attempted, failures are never treated as offline.
    save_opts(state_dir, &eff)?;
    if let Some(plan) = prepared_mapping {
        plan.apply()?;
    }
    let (host, port) = (so.host.clone(), so.port);
    // A previous start's marker must not satisfy this one's wait —
    // clear it before any `running`/`spawn` path can read it.
    let _ = std::fs::remove_file(ready_file(state_dir));
    if let Some(pid) = running {
        // CAD-841: `--device-login-*` is a thin client of
        // `operator_device_login_set` — pushed from THIS process (it
        // carries the operator secret; a detached child could never
        // prove itself). The board is already up: routes read the
        // daemon's store per request, so this is a live reconfigure.
        if let Some(triple) = &device_login_push {
            push_device_login_config(state_dir, triple)?;
        }
        let (code, _) = http_get(&host, port, "/api/health", &format!("{host}:{port}"), &[])
            .unwrap_or((0, String::new()));
        if !quiet {
            println!(
                "{}",
                crate::output::json_text(&json!({
                    "state": "already_running", "pid": pid, "health_http": code,
                    "tailnet_url": eff.tailscale.as_ref().map(|t| t.url()),
                }))
                .unwrap_or_default()
            );
        }
        return Ok(0);
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state_dir.join("ui.log"))?;
    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    // The detached child re-resolves from `ui.json` — its argv only
    // pins the bind, everything else is the persisted file's business.
    // CAD-482: a board is never a caller either — requests assert via
    // headers, so a `CADENCE_TEST_AS` in this process's env must not
    // leak into the child (a `daemon restart --ui` under it would
    // blanket-assert every board→daemon RPC).
    command.env_remove(crate::test_seam::AS_ENV);
    // CAD-841: the child must never push a device-login config — the
    // parent already did (or nobody did). Its argv carries none of the
    // flags; strip the env fallbacks so an inherited
    // `CADENCE_DEVICE_LOGIN_*` cannot make it try (a detached child
    // fails operator proof and would die at startup).
    command
        .env_remove("CADENCE_DEVICE_LOGIN_ISSUER")
        .env_remove("CADENCE_DEVICE_LOGIN_ORG")
        .env_remove("CADENCE_DEVICE_LOGIN_SUBJECTS");
    command
        .arg("--state-dir")
        // CAD-1081: absolute, so the board's argv names its state dir
        // from any reader's cwd.
        .arg(std::path::absolute(state_dir)?)
        .args(["ui", "run", "--host", &host, "--port"])
        .arg(port.to_string());
    if let Some(dist) = &eff.dist {
        command.arg("--dist").arg(dist);
    }
    // Readiness the port cannot fake: a fresh nonce reaches the child
    // through its environment, and the child lands it in `ui.ready`
    // only after its own `Server::http` bind succeeds (CAD-817). An
    // HTTP 200 — whoever answers it — no longer proves our board up.
    let nonce = crate::operator_auth::random_credential()?;
    command.env(READY_NONCE_ENV, &nonce);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log.try_clone()?))
        .stderr(std::process::Stdio::from(log));
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    // CAD-1283: `dev up`'s fresh automatic allocation can carry a
    // remaining retry budget in its env; this start clamps its own
    // budget DOWN to it (never up) and drops the var before the child
    // is spawned. Absent or unparsable: the unchanged `start_budget()`.
    let outer_budget = std::env::var(START_REMAINING_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok());
    std::env::remove_var(START_REMAINING_ENV);
    let mut child = crate::reaper::spawn(&mut command)?;
    std::fs::write(pid_file(state_dir), child.id().to_string())?;
    let budget = match outer_budget {
        Some(remaining) => start_budget().min(Duration::from_secs(remaining)),
        None => start_budget(),
    };
    let began = Instant::now();
    let deadline = began + budget;
    loop {
        if child.try_wait()?.is_some() {
            // CAD-1283: the child is dead; only now read its
            // startup-failure frame. An exact pid+nonce+kind match is
            // typed proof the port was taken between `up`'s probe and
            // this bind; anything else (absent, stale, malformed,
            // foreign) stays the untyped exit below. The frame file is
            // pid-scoped and retained — no start ever unlinks a proof
            // file, so a concurrent or later start's frame can never
            // be removed out from under it.
            let bind_in_use = startup_bind_in_use(state_dir, child.id(), &nonce);
            // The legacy pid/ready unlinks are unchanged in order and
            // path; their results are now captured so a cleanup error
            // (anything but success or already-absent NotFound) keeps
            // the exit untyped — unknown cleanup state is not retried.
            let cleanup_ok = [
                std::fs::remove_file(pid_file(state_dir)),
                std::fs::remove_file(ready_file(state_dir)),
            ]
            .into_iter()
            .all(|r| match r {
                Ok(()) => true,
                Err(e) => e.kind() == std::io::ErrorKind::NotFound,
            });
            let detail = std::fs::read_to_string(state_dir.join("ui.log"))
                .unwrap_or_default()
                .lines()
                .rev()
                .find(|l| l.contains("bind") || l.contains("error"))
                .unwrap_or_default()
                .trim()
                .to_string();
            let detail = if detail.is_empty() {
                format!("see {}", state_dir.join("ui.log").display())
            } else {
                detail
            };
            let what = format!("ui server exited during start — port {port} on {host}: {detail}");
            return Err(if bind_in_use && cleanup_ok {
                Error::ui_bind_in_use(what)
            } else {
                Error::rejected(what)
            });
        }
        if ready_pid(state_dir, &nonce) == Some(child.id() as i32) {
            // The child bound and reported itself — the health check
            // now confirms it serves, still not the other way around.
            if let Ok((200, _)) =
                http_get(&host, port, "/api/health", &format!("{host}:{port}"), &[])
            {
                let _ = std::fs::remove_file(ready_file(state_dir));
                // CAD-1251: the window is sized from data, so say how
                // much of it this start used.
                eprintln!(
                    "ui: board proved its start in {:.1}s (budget {}s)",
                    began.elapsed().as_secs_f64(),
                    budget.as_secs()
                );
                // CAD-841: only now that the child provably bound and
                // serves does a `--device-login-*` triple reach the
                // daemon — a failed spawn changes no live config
                // (review r1). The child can never push: it is
                // detached, so this process does it. And if the push
                // itself fails, the just-spawned board goes down with
                // its start — `ui start` never leaves a live board the
                // requested config was refused for (r2).
                if let Some(triple) = &device_login_push {
                    if let Err(e) = push_device_login_config(state_dir, triple) {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = std::fs::remove_file(pid_file(state_dir));
                        return Err(e);
                    }
                }
                if !quiet {
                    println!(
                        "{}",
                        crate::output::json_text(&json!({
                            "state": "started", "pid": child.id(),
                            "url": format!("http://{host}:{port}"),
                            "tailnet_url": eff.tailscale.as_ref().map(|t| t.url()),
                            "read_only": eff.read_only,
                            "anonymous_reads": eff.allow_anonymous_reads,
                            "gateway": "http://cadence.localhost:18000",
                            "log": state_dir.join("ui.log"),
                            // CAD-313: board writes need the operator's session.
                            "sign_in": "cadence ui login",
                        }))
                        .unwrap_or_default()
                    );
                }
                return Ok(0);
            }
        }
        if Instant::now() >= deadline {
            // CAD-1207: the board this call spawned never proved itself —
            // it must not outlive the failure that tells the caller to
            // give up. Only our own `Child` handle is signalled (never a
            // pid read from a file), and the pidfile goes only if it
            // still names that child.
            reap_unready_child(&mut child, state_dir);
            return Err(Error::internal(format!(
                "ui server did not prove its own start within {}s — the board this \
                 start spawned was stopped",
                budget.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Stop the board a timed-out `ui start` spawned: kill our own child,
/// wait for it to exit, and drop `ui.pid` / `ui.ready` only while they
/// still describe it (a concurrent start's files are not ours).
pub(crate) fn reap_unready_child(child: &mut std::process::Child, state_dir: &Path) {
    let pid = child.id();
    let _ = child.kill();
    let _ = child.wait();
    let path = pid_file(state_dir);
    if std::fs::read_to_string(&path).is_ok_and(|t| t.trim() == pid.to_string()) {
        let _ = std::fs::remove_file(&path);
    }
    let _ = std::fs::remove_file(ready_file(state_dir));
}

/// SIGTERM the detached server and wait for exit — no output, for
/// callers (stop, the tailscale verbs) that print their own result.
pub(crate) fn kill_detached(state_dir: &Path) -> Option<i32> {
    let pid = read_pid(state_dir)?;
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(5);
    while unsafe { libc::kill(pid, 0) == 0 } {
        if Instant::now() >= deadline {
            unsafe { libc::kill(pid, libc::SIGKILL) };
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = std::fs::remove_file(pid_file(state_dir));
    Some(pid)
}

pub(crate) fn stop(state_dir: &Path, tailscale_off: bool) -> Result<i32> {
    let recorded = if tailscale_off {
        let opts = load_opts_strict(state_dir)?;
        if let Some(share) = opts.tailscale.as_ref() {
            let mut bind = UiOpts {
                host: opts.host.clone(),
                port: opts.port,
                ..UiOpts::default()
            };
            bind.tailscale = Some(share.clone());
            validate_share_target(&bind)?;
            crate::sandbox::refuse_global_unless_allowed("`ui stop --tailscale-off`")?;
        }
        Some(opts)
    } else {
        None
    };
    let pid = kill_detached(state_dir);
    if pid.is_none() {
        let _ = std::fs::remove_file(pid_file(state_dir));
    }
    let _ = std::fs::remove_file(ready_file(state_dir));
    // --tailscale-off: only ever the mapping cadence recorded — a
    // foreign one on the same port is left alone and named in the
    // result.
    let mut ts_result = Value::Null;
    if tailscale_off {
        let mut opts = recorded.expect("loaded before stop effects");
        if let Some(ts) = opts.tailscale.take() {
            ts_result = match remove_mapping(&ts.dns_name, ts.https_port, &ts.target) {
                Ok(true) => json!({"removed": ts.https_port}),
                Ok(false) => json!({"left_alone": ts.https_port, "why": "mapping already absent"}),
                Err(e) => {
                    return Err(Error::rejected(format!(
                        "{e} — the tailnet record is kept; fix the failure and retry"
                    )));
                }
            };
            clear_persisted_share(state_dir)?;
        } else {
            ts_result = json!({"left_alone": Value::Null, "why": "no recorded mapping"});
        }
    }
    println!(
        "{}",
        crate::output::json_text(&json!({
            "state": "stopped",
            "pid": pid,
            "note": if pid.is_none() { Some("no live pid") } else { None },
            "tailscale_off": ts_result,
        }))
        .unwrap_or_default()
    );
    Ok(0)
}

pub(crate) fn status(state_dir: &Path) -> Result<i32> {
    let pid = read_pid(state_dir);
    let opts = load_opts(state_dir);
    let port = opts.port.unwrap_or(3010);
    let health = pid.and_then(|_| {
        http_get(
            "127.0.0.1",
            port,
            "/api/health",
            &format!("127.0.0.1:{port}"),
            &[],
        )
        .ok()
    });
    println!(
        "{}",
        crate::output::json_text(&json!({
            "state": if pid.is_some() { "running" } else { "stopped" },
            "pid": pid,
            "health": health.map(|(code, body)| json!({
                "http": code,
                "body": serde_json::from_str::<Value>(&body).unwrap_or(Value::Null),
            })),
            "options": {
                "port": port,
                "allow_hosts": opts.allow_hosts,
                "allow_origins": opts.allow_origins,
                "read_only": opts.read_only,
                "allow_anonymous_reads": opts.allow_anonymous_reads,
            },
            "tailnet_url": opts.tailscale.as_ref().map(|t| t.url()),
            // CAD-526: the public sign-in surface, when configured.
            "board_url": opts.board.as_ref().map(|b| format!(
                "{}://{}", operator::public_scheme(&b.host), b.host)),
            "board_issuer": opts.board.as_ref().map(|b| b.issuer.clone()),
            "board_company": opts.board.as_ref().map(|b| b.company.clone()),
            // CAD-841: device login is daemon-owned — report the
            // daemon's answer (issuer/org only; the allowlist is for
            // `ui device-login show`), `null` while the daemon is away.
            "device_login": crate::client::rpc(
                state_dir, "device_login_config", json!({})).ok(),
        }))
        .unwrap_or_default()
    );
    Ok(0)
}
