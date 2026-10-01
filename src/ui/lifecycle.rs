//! The board's process lifecycle (`ui run|start|stop|status`, pid and
//! ready files, detached spawn, the plain HTTP probe) — mirrors `daemon
//! start|stop|status`. CAD-1001: moved verbatim from `src/ui.rs` (the
//! CAD-982 split, PR-2); the parent re-exports what the sibling
//! submodules and the rest of the crate use, so the public surface of
//! `crate::ui` is unchanged.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::operator;
use super::tailscale::{ensure_mapping, is_ts_offline, remove_mapping};
use super::{
    load_opts, push_device_login_config, resolve_device_login, resolve_opts, save_opts, serve,
    UiFlags, UiOpts,
};
use crate::error::{Error, Result};

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
/// pidfile's pid, alive-checked. `daemon restart --ui` reads this to
/// decide whether to bounce the board.
pub fn detached_pid(state_dir: &Path) -> Option<i32> {
    read_pid(state_dir)
}

pub(crate) fn read_pid(state_dir: &Path) -> Option<i32> {
    std::fs::read_to_string(pid_file(state_dir))
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|pid| {
            // Alive check — a stale pidfile is cleaned, not trusted.
            unsafe { libc::kill(*pid, 0) == 0 }
        })
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
    let persisted = load_opts(state_dir);
    let (_eff, mut so) = resolve_opts(flags, &persisted)?;
    // CAD-841: `--device-login-*` is a thin client — resolve now so a
    // partial triple still fails before any bind, but push only once
    // `serve` owns the port: a failed start must not have already
    // replaced the live config on this state dir's boards (r1). A
    // detached `ui start` child resolves no flags and a scrubbed env,
    // so it never re-pushes.
    so.device_login_push = resolve_device_login(flags)?;
    serve(state_dir, &crate::issue::default_dir()?, &so)?;
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
    let recorded = load_opts(state_dir);
    let persisted = if reset {
        UiOpts::default()
    } else {
        recorded.clone()
    };
    let (eff, so) = resolve_opts(flags, &persisted)?;
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
    // Re-ensure a persisted mapping so `ui stop && ui start` keeps the
    // board shared — best effort when tailscaled itself is unreachable.
    if flags.tailscale.is_none() {
        if let Some(ts) = &eff.tailscale {
            match ensure_mapping(ts.https_port, &ts.target) {
                Ok(_) => {}
                Err(e) if is_ts_offline(&e) => {
                    eprintln!("warning: {e} — serving loopback only this run");
                }
                Err(e) => return Err(e),
            }
        }
    }
    save_opts(state_dir, &eff)?;
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
        .arg(state_dir)
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
    let mut child = crate::reaper::spawn(&mut command)?;
    std::fs::write(pid_file(state_dir), child.id().to_string())?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait()?.is_some() {
            // The bind failed (or the child died before it) — the log
            // line is the why, the port the what.
            let _ = std::fs::remove_file(pid_file(state_dir));
            let _ = std::fs::remove_file(ready_file(state_dir));
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
            return Err(Error::rejected(format!(
                "ui server exited during start — port {port} on {host}: {detail}"
            )));
        }
        if ready_pid(state_dir, &nonce) == Some(child.id() as i32) {
            // The child bound and reported itself — the health check
            // now confirms it serves, still not the other way around.
            if let Ok((200, _)) =
                http_get(&host, port, "/api/health", &format!("{host}:{port}"), &[])
            {
                let _ = std::fs::remove_file(ready_file(state_dir));
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
            let _ = std::fs::remove_file(ready_file(state_dir));
            return Err(Error::internal(
                "ui server did not prove its own start within 10s",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
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
        let mut opts = load_opts(state_dir);
        if let Some(ts) = opts.tailscale.take() {
            ts_result = match remove_mapping(ts.https_port, &ts.target) {
                Ok(true) => json!({"removed": ts.https_port}),
                Ok(false) => json!({"left_alone": ts.https_port, "why": "mapping changed hands"}),
                Err(e) => {
                    eprintln!("warning: {e}");
                    json!({"left_alone": ts.https_port, "why": e.to_string()})
                }
            };
            opts.tailscale = None;
            save_opts(state_dir, &opts)?;
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
