//! `cadence ui` — the read-only board: a small synchronous HTTP server
//! (tiny_http, no async runtime — the daemon is plain threads too)
//! serving the built SPA plus a five-route JSON API on loopback.
//!
//! No auth in I1 — containment is the defence: loopback bind, no CORS
//! headers, a Host allowlist against DNS rebinding, GET/HEAD only, id
//! grammar checked before any path is touched, and no file reads
//! outside the PM dir or `--dist`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use clap::Subcommand;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use crate::client;
use crate::error::{Error, Result};
use crate::issue::{board, model, project, Pm};

#[derive(Subcommand)]
pub enum UiAction {
    /// Serve the board + JSON API in the foreground.
    Run {
        /// Bind address [default: 127.0.0.1].
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Port [default: 3010].
        #[arg(long, default_value_t = 3010)]
        port: u16,
        /// Serve the SPA from this directory (required when the binary
        /// was built without `--features ui`).
        #[arg(long)]
        dist: Option<PathBuf>,
        /// Extra allowed Host header values (repeatable).
        #[arg(long = "allow-host")]
        allow_hosts: Vec<String>,
    },
    /// Detached `ui run`: pid + log under the state dir, mirrors
    /// `daemon start`.
    Start {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 3010)]
        port: u16,
        #[arg(long)]
        dist: Option<PathBuf>,
        #[arg(long = "allow-host")]
        allow_hosts: Vec<String>,
    },
    /// Stop the detached UI server.
    Stop,
    /// Report UI server health.
    Status,
}

/// `cadence ui …`
pub fn run_cli(state_dir: &Path, action: &UiAction) -> Result<i32> {
    match action {
        UiAction::Run {
            host,
            port,
            dist,
            allow_hosts,
        } => {
            serve(
                state_dir,
                &crate::issue::default_dir()?,
                host,
                *port,
                dist.clone(),
                allow_hosts,
            )?;
            Ok(0)
        }
        UiAction::Start {
            host,
            port,
            dist,
            allow_hosts,
        } => start(state_dir, host, *port, dist.clone(), allow_hosts),
        UiAction::Stop => stop(state_dir),
        UiAction::Status => status(state_dir),
    }
}

// ---------- server ----------

/// The Host allowlist: the gateway vhost (with or without its port —
/// browsers send `:18000`, a hand-set header may not) plus the bind
/// address forms.
fn host_allowed(host: &str, port: u16, extra: &[String]) -> bool {
    let host = host.trim().to_ascii_lowercase();
    host == "cadence.localhost"
        || host == "cadence.localhost:18000"
        || host == format!("127.0.0.1:{port}")
        || host == format!("localhost:{port}")
        || host == format!("[::1]:{port}")
        || extra.iter().any(|h| h.eq_ignore_ascii_case(&host))
}

fn json_response(value: Value) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::to_vec_pretty(&value).unwrap_or_default();
    let mut resp = Response::from_data(body).with_status_code(StatusCode(200));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp
}

fn err_response(code: u16, message: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut resp = Response::from_string(format!("{{\"error\": \"{message}\"}}\n"))
        .with_status_code(StatusCode(code));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp
}

/// Percent-decode a URL path/query component (UTF-8, `+` untouched in
/// path context). Returns None on malformed input.
fn pct_decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = bytes.get(i + 1..i + 3)?;
                let v = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
                out.push(v);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "woff2" => "font/woff2",
        "json" => "application/json",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

/// Static payload: `dist` dir first (dev / `--features ui` absent),
/// then the embedded build when compiled in.
fn static_file(dist: Option<&Path>, path: &str) -> Option<(String, Vec<u8>)> {
    if let Some(dist) = dist {
        let rel = path.trim_start_matches('/');
        if rel.is_empty()
            || rel
                .split('/')
                .any(|s| s.is_empty() || s == "." || s == "..")
        {
            return None;
        }
        let base = dist.canonicalize().ok()?;
        let file = base.join(rel).canonicalize().ok()?;
        if !file.starts_with(&base) || !file.is_file() {
            return None;
        }
        return std::fs::read(&file).ok().map(|b| (path.to_string(), b));
    }
    #[cfg(feature = "ui")]
    {
        let bytes: Option<&[u8]> = match path {
            "/" | "/index.html" => Some(embedded::INDEX.as_bytes()),
            "/assets/index.js" => Some(embedded::JS.as_bytes()),
            "/assets/index.css" => Some(embedded::CSS.as_bytes()),
            _ => None,
        };
        return bytes.map(|b| (path.to_string(), b.to_vec()));
    }
    #[allow(unreachable_code)]
    None
}

#[cfg(feature = "ui")]
mod embedded {
    pub const INDEX: &str = include_str!("../ui/dist/index.html");
    pub const JS: &str = include_str!("../ui/dist/assets/index.js");
    pub const CSS: &str = include_str!("../ui/dist/assets/index.css");
}

/// What the board needs from the daemon — agent rows plus the per-agent
/// queue/fence counts. `daemon: "unreachable"` instead of a 500 when
/// the socket is down: the board still renders.
fn agents_payload(state_dir: &Path, known_ids: &std::collections::HashSet<String>) -> Value {
    let list = match client::rpc(state_dir, "agent_list", json!({})) {
        Ok(list) => list,
        Err(_) => {
            return json!({"daemon": "unreachable", "agents": [], "totals": null});
        }
    };
    let agents = list["agents"].as_array().cloned().unwrap_or_default();
    let mut out = Vec::new();
    let mut totals = json!({"running": 0, "queued": 0, "fenced": 0, "parked": 0});
    for agent in &agents {
        let alias = agent["alias"].as_str().unwrap_or_default();
        let show = client::rpc(state_dir, "agent_show", json!({"alias": alias}));
        let (mut running, mut parked) = (0i64, 0i64);
        let mut on = Vec::new();
        let (queued, unknown) = match &show {
            Ok(show) => {
                for m in show["messages"].as_array().cloned().unwrap_or_default() {
                    match m["state"].as_str() {
                        Some("running") => {
                            running += 1;
                            // A running turn's body may name an issue —
                            // that is the card the agent is on.
                            for id in board::mentioned_ids(m["body"].as_str().unwrap_or_default()) {
                                if known_ids.contains(&id) {
                                    on.push(id);
                                }
                            }
                        }
                        _ => {
                            if m["result"]["via"].as_str() == Some("pty_render_miss") {
                                parked += 1;
                            }
                        }
                    }
                }
                (
                    show["queued"].as_i64().unwrap_or(0),
                    show["unknown"].as_i64().unwrap_or(0),
                )
            }
            Err(_) => (0, 0),
        };
        let fenced = unknown > 0 || agent["state"].as_str() == Some("attention");
        totals["running"] = json!(totals["running"].as_i64().unwrap_or(0) + running);
        totals["queued"] = json!(totals["queued"].as_i64().unwrap_or(0) + queued);
        totals["parked"] = json!(totals["parked"].as_i64().unwrap_or(0) + parked);
        if fenced {
            totals["fenced"] = json!(totals["fenced"].as_i64().unwrap_or(0) + 1);
        }
        out.push(json!({
            "alias": alias,
            "provider": agent["provider"],
            "endpoint_kind": agent["endpoint_kind"],
            "state": agent["state"],
            "group": agent["params"]["upstream"].as_str().unwrap_or(alias),
            "group_root": agent["params"]["upstream"].is_null(),
            "running": running, "queued": queued, "unknown": unknown,
            "parked": parked, "fenced": fenced,
            "on": on,
        }));
    }
    json!({"daemon": "reachable", "agents": out, "totals": totals})
}

fn handle(
    request: Request,
    state_dir: &Path,
    pm_dir: &Path,
    port: u16,
    dist: Option<&Path>,
    hosts: &[String],
) {
    let head_only = request.method() == &Method::Head;
    if request.method() != &Method::Get && request.method() != &Method::Head {
        let _ = request.respond(err_response(
            405,
            "method not allowed — the board is read-only",
        ));
        return;
    }
    let host = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Host"))
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default();
    if !host_allowed(&host, port, hosts) {
        let _ = request.respond(err_response(421, "misdirected request — Host not allowed"));
        return;
    }
    let raw_url = request.url().to_string();
    let (raw_path, raw_query) = raw_url.split_once('?').unwrap_or((&raw_url, ""));
    let Some(path) = pct_decode(raw_path) else {
        let _ = request.respond(err_response(400, "malformed path"));
        return;
    };
    if path.contains("..") || path.contains('\0') {
        let _ = request.respond(err_response(400, "bad path"));
        return;
    }
    let query = |key: &str| -> Option<String> {
        raw_query.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            if k == key {
                pct_decode(v)
            } else {
                None
            }
        })
    };

    let send = |req: Request, resp: Response<std::io::Cursor<Vec<u8>>>| {
        if head_only {
            // tiny_http does not strip bodies on HEAD — answer with the
            // status line only.
            let _ = req.respond(Response::empty(resp.status_code()));
        } else {
            let _ = req.respond(resp);
        }
    };

    match path.as_str() {
        "/api/health" => {
            let pm = Pm::at(pm_dir).ok();
            let (projects, issues) = match &pm {
                Some(pm) => {
                    let p = project::list(&pm.dir).map(|l| l.len()).unwrap_or(0);
                    let i = board::load_all(&pm.dir, None).map(|l| l.len()).unwrap_or(0);
                    (p, i)
                }
                None => (0, 0),
            };
            let daemon = if client::rpc(state_dir, "health", json!({})).is_ok() {
                "reachable"
            } else {
                "unreachable"
            };
            send(
                request,
                json_response(json!({
                    "ok": true,
                    "pm_dir": pm.as_ref().map(|p| p.dir.clone()),
                    "pm_present": pm.is_some(),
                    "projects": projects, "issues": issues,
                    "daemon": daemon,
                    "embedded": cfg!(feature = "ui"),
                })),
            );
        }
        "/api/projects" => match Pm::at(pm_dir) {
            Ok(pm) => {
                let projects = project::list(&pm.dir).unwrap_or_default();
                let issues = board::load_all(&pm.dir, None).unwrap_or_default();
                let payload: Vec<Value> = projects
                    .iter()
                    .map(|p| {
                        json!({
                            "key": p.key, "prefix": p.prefix,
                            "components": p.components,
                            "default_owner": p.default_owner,
                            "repos": p.repos.iter().map(|r| json!({
                                "path": r.path, "remote": r.remote})).collect::<Vec<_>>(),
                            "issues": issues.iter().filter(|i| i.project == p.key).count(),
                        })
                    })
                    .collect();
                send(request, json_response(json!({"projects": payload})));
            }
            Err(e) => send(request, err_response(503, &e.to_string())),
        },
        "/api/issues" => match Pm::at(pm_dir) {
            Ok(pm) => {
                let filter = query("project");
                if let Some(p) = &filter {
                    if !model::valid_key(p) {
                        send(request, err_response(400, "bad project key"));
                        return;
                    }
                }
                let issues = board::load_all(&pm.dir, filter.as_deref()).unwrap_or_default();
                let views = board::views(&pm.config.notes_dir(), issues);
                send(
                    request,
                    json_response(json!({
                        "issues": views.iter().map(board::card_json).collect::<Vec<_>>(),
                    })),
                );
            }
            Err(e) => send(request, err_response(503, &e.to_string())),
        },
        "/api/agents" => {
            let known: std::collections::HashSet<String> = Pm::at(pm_dir)
                .and_then(|pm| board::load_all(&pm.dir, None))
                .map(|issues| issues.iter().map(|i| i.front.id.clone()).collect())
                .unwrap_or_default();
            send(request, json_response(agents_payload(state_dir, &known)));
        }
        _ => {
            // `/api/issues/<ID>[/file|/activity]` — id grammar checked
            // before the id is ever used as a path component.
            if let Some(tail) = path.strip_prefix("/api/issues/") {
                let mut segs = tail.splitn(2, '/');
                let id_raw = segs.next().unwrap_or_default();
                let sub = segs.next();
                if sub.is_some_and(|s| !matches!(s, "file" | "activity")) {
                    send(request, err_response(404, "no such route"));
                    return;
                }
                let Ok(id) = model::check_id(id_raw) else {
                    send(request, err_response(400, "bad issue id"));
                    return;
                };
                match Pm::at(pm_dir) {
                    Ok(pm) => {
                        let issues = board::load_all(&pm.dir, None).unwrap_or_default();
                        let views = board::views(&pm.config.notes_dir(), issues);
                        let by_id: std::collections::HashMap<String, &board::View> = views
                            .iter()
                            .map(|v| (v.issue.front.id.clone(), v))
                            .collect();
                        let Some(view) = by_id.get(&id) else {
                            send(request, err_response(404, "unknown issue"));
                            return;
                        };
                        match sub {
                            None => send(
                                request,
                                json_response(board::detail_json(&pm.dir, view, &by_id)),
                            ),
                            Some("file") => {
                                let file = view.issue.dir.join("issue.md");
                                match std::fs::read(&file) {
                                    Ok(bytes) => {
                                        let mut resp = Response::from_data(bytes);
                                        resp.add_header(
                                            Header::from_bytes(
                                                "Content-Type",
                                                "text/markdown; charset=utf-8",
                                            )
                                            .unwrap(),
                                        );
                                        send(request, resp);
                                    }
                                    Err(_) => send(request, err_response(404, "no issue.md")),
                                }
                            }
                            Some("activity") => send(
                                request,
                                json_response(json!({
                                    "id": id,
                                    "activity": board::activity_json(&pm.dir, view),
                                })),
                            ),
                            Some(_) => unreachable!(),
                        }
                    }
                    Err(e) => send(request, err_response(503, &e.to_string())),
                }
                return;
            }
            if path.starts_with("/api/") {
                send(request, err_response(404, "no such route"));
                return;
            }
            // Static: `/` → index.html; otherwise a file under dist or
            // the embedded build. No SPA routes exist in I1.
            let target = if path == "/" { "/index.html" } else { &path };
            match static_file(dist, target) {
                Some((name, bytes)) => {
                    let mut resp = Response::from_data(bytes);
                    resp.add_header(
                        Header::from_bytes("Content-Type", content_type(&name)).unwrap(),
                    );
                    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
                    send(request, resp);
                }
                None => {
                    if let Some((name, bytes)) = static_file(dist, "/index.html") {
                        let mut resp = Response::from_data(bytes);
                        resp.add_header(
                            Header::from_bytes("Content-Type", content_type(&name)).unwrap(),
                        );
                        send(request, resp);
                    } else {
                        send(
                            request,
                            err_response(
                                503,
                                "no SPA build — pass --dist or rebuild with --features ui",
                            ),
                        );
                    }
                }
            }
        }
    }
}

pub fn serve(
    state_dir: &Path,
    pm_dir: &Path,
    host: &str,
    port: u16,
    dist: Option<PathBuf>,
    allow_hosts: &[String],
) -> Result<()> {
    let server = Server::http(format!("{host}:{port}"))
        .map_err(|e| Error::internal(format!("ui bind {host}:{port}: {e}")))?;
    eprintln!("cadence ui listening on http://{host}:{port}");
    for request in server.incoming_requests() {
        handle(
            request,
            state_dir,
            pm_dir,
            port,
            dist.as_deref(),
            allow_hosts,
        );
    }
    Ok(())
}

// ---------- lifecycle (mirrors `daemon start|stop|status`) ----------

fn pid_file(state_dir: &Path) -> PathBuf {
    state_dir.join("ui.pid")
}

fn read_pid(state_dir: &Path) -> Option<i32> {
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
/// dependency. Returns `(status, body)`.
fn http_get(host: &str, port: u16, path: &str) -> Result<(u16, String)> {
    let mut stream = TcpStream::connect((host, port))
        .map_err(|e| Error::internal(format!("ui not reachable at {host}:{port}: {e}")))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(stream, "GET {path} HTTP/1.0\r\nHost: {host}:{port}\r\n\r\n")?;
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

fn start(
    state_dir: &Path,
    host: &str,
    port: u16,
    dist: Option<PathBuf>,
    allow_hosts: &[String],
) -> Result<i32> {
    std::fs::create_dir_all(state_dir)?;
    if let Some(pid) = read_pid(state_dir) {
        let (code, _) = http_get(host, port, "/api/health").unwrap_or((0, String::new()));
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "state": "already_running", "pid": pid, "health_http": code,
            }))
            .unwrap_or_default()
        );
        return Ok(0);
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state_dir.join("ui.log"))?;
    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    command
        .arg("--state-dir")
        .arg(state_dir)
        .args(["ui", "run", "--host", host, "--port"])
        .arg(port.to_string());
    if let Some(dist) = &dist {
        command.arg("--dist").arg(dist);
    }
    for h in allow_hosts {
        command.arg("--allow-host").arg(h);
    }
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
    let mut child = command.spawn()?;
    std::fs::write(pid_file(state_dir), child.id().to_string())?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok((200, _)) = http_get(host, port, "/api/health") {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "state": "started", "pid": child.id(),
                    "url": format!("http://{host}:{port}"),
                    "gateway": "http://cadence.localhost:18000",
                    "log": state_dir.join("ui.log"),
                }))
                .unwrap_or_default()
            );
            return Ok(0);
        }
        if child.try_wait()?.is_some() {
            let _ = std::fs::remove_file(pid_file(state_dir));
            return Err(Error::rejected(format!(
                "ui server exited during start — see {}",
                state_dir.join("ui.log").display()
            )));
        }
        if Instant::now() >= deadline {
            return Err(Error::internal("ui server did not answer within 10s"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn stop(state_dir: &Path) -> Result<i32> {
    match read_pid(state_dir) {
        Some(pid) => {
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
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({"state": "stopped", "pid": pid}))
                    .unwrap_or_default()
            );
        }
        None => {
            let _ = std::fs::remove_file(pid_file(state_dir));
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({"state": "stopped", "note": "no live pid"}))
                    .unwrap_or_default()
            );
        }
    }
    Ok(0)
}

fn status(state_dir: &Path) -> Result<i32> {
    let pid = read_pid(state_dir);
    let health = pid.and_then(|_| {
        // The pidfile records the pid, not the port — try the default.
        http_get("127.0.0.1", 3010, "/api/health").ok()
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "state": if pid.is_some() { "running" } else { "stopped" },
            "pid": pid,
            "health": health.map(|(code, body)| json!({
                "http": code,
                "body": serde_json::from_str::<Value>(&body).unwrap_or(Value::Null),
            })),
        }))
        .unwrap_or_default()
    );
    Ok(0)
}
