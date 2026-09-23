//! `cadence ui` — the board: a small synchronous HTTP server
//! (tiny_http, no async runtime — the daemon is plain threads too)
//! serving the built SPA plus a JSON API on loopback.
//!
//! No auth, by decision — containment is the defence: loopback bind, no
//! CORS headers, a Host allowlist against DNS rebinding, id grammar
//! checked before any path is touched, and no file reads outside the PM
//! dir or `--dist`. Writes are I2: POST/PATCH/DELETE routes must pass
//! four cross-site guards (known write route, exact JSON/octet-stream
//! content type, `X-Cadence-Board: 1`, same-origin Origin/Sec-Fetch-Site)
//! before any work is done, then go through the authenticated writer the
//! route supports. Memory curation is deliberately refused here: an HTTP
//! server peer is not the native agent endpoint proof required by the
//! daemon, so the browser cannot become a curator by reaching this route.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use crate::adapter::registry;
use crate::client;
use crate::doctor::host::redact_argv;
use crate::error::{Error, Result};
use crate::issue::{board, context, history, model, project, write as issue_write, Pm};
use crate::proc::{self, BoundedError};

/// The options `ui run` and `ui start` share. Every field is optional:
/// a given flag overrides the persisted `ui.json`, an absent one
/// inherits it, and `--reset` on `start` forgets the file first.
#[derive(Args, Clone, Default)]
pub struct UiFlags {
    /// Bind address [default: 127.0.0.1]. Tailscale sharing requires
    /// loopback — the proxy connects from the same host.
    #[arg(long)]
    pub host: Option<String>,
    /// Port [default: 3010].
    #[arg(long)]
    pub port: Option<u16>,
    /// Serve the SPA from this directory (required when the binary
    /// was built without `--features ui`).
    #[arg(long)]
    pub dist: Option<PathBuf>,
    /// Extra allowed Host header values (repeatable).
    #[arg(long = "allow-host")]
    pub allow_hosts: Vec<String>,
    /// Extra allowed write Origin values (repeatable) — e.g.
    /// `https://<name>.ts.net:9450` for a tailnet-shared board.
    #[arg(long = "allow-origin")]
    pub allow_origins: Vec<String>,
    /// Refuse every write route with 403; the SPA hides edit controls.
    #[arg(long, overrides_with = "no_read_only")]
    pub read_only: bool,
    /// Clear a persisted --read-only.
    #[arg(long, overrides_with = "read_only")]
    pub no_read_only: bool,
    /// Publish the board on the tailnet through `tailscale serve`
    /// (https port default: 9450). Ensures the serve mapping, adds the
    /// tailnet name to the Host and Origin allowlists, and attributes
    /// writes to the Tailscale user. Never funnel.
    #[arg(long, num_args = 0..=1, default_missing_value = "9450",
           value_name = "HTTPS_PORT")]
    pub tailscale: Option<u16>,
}

#[derive(Subcommand)]
pub enum UiAction {
    /// Serve the board + JSON API in the foreground.
    Run {
        #[command(flatten)]
        flags: UiFlags,
    },
    /// Detached `ui run`: pid + log under the state dir, mirrors
    /// `daemon start`. Effective options persist to `ui.json`; a later
    /// plain `ui start` reuses them.
    Start {
        #[command(flatten)]
        flags: UiFlags,
        /// Forget the persisted options before applying flags.
        #[arg(long)]
        reset: bool,
    },
    /// Stop the detached UI server.
    Stop {
        /// Also remove the tailscale serve mapping cadence created.
        #[arg(long)]
        tailscale_off: bool,
    },
    /// Report UI server health and the persisted options.
    Status,
    /// Share the board over the tailnet (`tailscale serve`, never
    /// funnel). The primary UX for phone/laptop access.
    Tailscale {
        #[command(subcommand)]
        action: TailscaleAction,
    },
}

#[derive(Subcommand)]
pub enum TailscaleAction {
    /// Publish the board on the tailnet: ensure the serve mapping,
    /// persist the options, (re)start the detached board so the Host
    /// and Origin allowlists take effect, print the tailnet URL.
    Start {
        /// Tailscale https port [default: 9450].
        #[arg(long, default_value_t = 9450)]
        port: u16,
        /// Make every write route answer 403 — browse-only sharing.
        #[arg(long)]
        read_only: bool,
    },
    /// Remove the serve mapping cadence created, drop the tailnet
    /// Host/Origin entries, and restart the board local-only.
    Stop,
    /// Report sharing state, the tailnet URL, and a terminal QR code.
    Status,
}

/// `cadence ui …`
pub fn run_cli(state_dir: &Path, action: &UiAction) -> Result<i32> {
    if let UiAction::Tailscale { .. } = action {
        crate::sandbox::refuse_global("`ui tailscale`")?;
    }
    match action {
        UiAction::Run { flags } => run(state_dir, flags),
        UiAction::Start { flags, reset } => start(state_dir, flags, *reset),
        UiAction::Stop { tailscale_off } => stop(state_dir, *tailscale_off),
        UiAction::Status => status(state_dir),
        UiAction::Tailscale { action } => tailscale_cli(state_dir, action),
    }
}

// ---------- persisted options (`<state>/ui.json`) ----------

/// Tailscale sharing as `ui start` recorded it. The derived Host and
/// Origin entries are computed from the dns name + port at resolve
/// time, never stored, so `tailscale stop` can drop them exactly.
#[derive(Serialize, Deserialize, Clone)]
pub struct TailscaleOpts {
    /// `Self.DNSName` from `tailscale status --json` (dot stripped).
    pub dns_name: String,
    /// The tailnet https port the serve mapping answers on.
    pub https_port: u16,
    /// The proxy target cadence registered — `http://127.0.0.1:<port>`.
    /// Removal only ever happens while the live mapping still equals
    /// this, so a foreign mapping on the port is never touched.
    pub target: String,
}

impl TailscaleOpts {
    /// Host header values a proxied request carries — `name` and
    /// `name:port` (browsers send the port; hand-set headers may not).
    fn hosts(&self) -> [String; 2] {
        [
            self.dns_name.clone(),
            format!("{}:{}", self.dns_name, self.https_port),
        ]
    }

    /// The browser's write Origin through the proxy.
    fn origins(&self) -> Vec<String> {
        let mut v = vec![format!("https://{}:{}", self.dns_name, self.https_port)];
        if self.https_port == 443 {
            v.push(format!("https://{}", self.dns_name));
        }
        v
    }

    pub fn url(&self) -> String {
        tailnet_url(&self.dns_name, self.https_port)
    }
}

/// `https://<name>` for 443, else `https://<name>:<port>`.
fn tailnet_url(dns_name: &str, https_port: u16) -> String {
    if https_port == 443 {
        format!("https://{dns_name}")
    } else {
        format!("https://{dns_name}:{https_port}")
    }
}

/// The effective options `ui start` persists — a later plain start
/// reuses them, `ui status` prints them, `--reset` forgets them.
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct UiOpts {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub dist: Option<PathBuf>,
    pub allow_hosts: Vec<String>,
    pub allow_origins: Vec<String>,
    pub read_only: bool,
    pub tailscale: Option<TailscaleOpts>,
}

/// Everything the running server needs, resolved.
#[derive(Clone, Default)]
pub struct ServeOpts {
    pub host: String,
    pub port: u16,
    pub dist: Option<PathBuf>,
    /// Operator `--allow-host` plus the tailnet-derived names.
    pub allow_hosts: Vec<String>,
    /// Operator `--allow-origin` plus the tailnet https origin.
    pub allow_origins: Vec<String>,
    pub read_only: bool,
    /// Tailscale sharing armed: `(dns_name, https_port)` — the trust
    /// rule for `Tailscale-User-*` headers and the `/api/meta` URL.
    pub tailnet: Option<(String, u16)>,
}

fn opts_file(state_dir: &Path) -> PathBuf {
    state_dir.join("ui.json")
}

/// Does this state dir have persisted ui options — `daemon restart
/// --ui` falls back to the running process's argv when it does not.
pub fn opts_present(state_dir: &Path) -> bool {
    opts_file(state_dir).is_file()
}

/// The persisted `ui start` options — `session start`'s board check
/// reads the tailscale block and port from them. Missing file is the
/// defaults.
pub fn persisted_opts(state_dir: &Path) -> UiOpts {
    load_opts(state_dir)
}

/// `/api/health` on the running board — `None` when nothing answers.
pub fn health(state_dir: &Path) -> Option<(u16, String)> {
    let port = load_opts(state_dir).port.unwrap_or(3010);
    http_get(
        "127.0.0.1",
        port,
        "/api/health",
        &format!("127.0.0.1:{port}"),
        &[],
    )
    .ok()
}

/// Does the live `tailscale serve` config map `target`? `None` when
/// tailscale cannot answer (not installed, daemon down).
pub fn serve_has_target(target: &str) -> Option<bool> {
    let out = ts(&["serve", "status", "--json"]).ok()?;
    if !out.status.success() {
        return Some(false);
    }
    Some(String::from_utf8_lossy(&out.stdout).contains(target))
}

fn load_opts(state_dir: &Path) -> UiOpts {
    let Ok(bytes) = std::fs::read(opts_file(state_dir)) else {
        return UiOpts::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        eprintln!("warning: ignoring unreadable ui.json: {e}");
        UiOpts::default()
    })
}

fn save_opts(state_dir: &Path, opts: &UiOpts) -> Result<()> {
    let path = opts_file(state_dir);
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(opts)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    matches!(h.as_str(), "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

/// Merge flags over the persisted options: a given flag wins, an
/// absent one inherits. `--tailscale` resolves the tailnet identity
/// and ensures the serve mapping; a persisted tailscale block is kept
/// (the detached server never re-ensures — only operator verbs do).
fn resolve_opts(flags: &UiFlags, persisted: &UiOpts) -> Result<(UiOpts, ServeOpts)> {
    let mut eff = UiOpts {
        host: flags.host.clone().or_else(|| persisted.host.clone()),
        port: flags.port.or(persisted.port),
        dist: flags.dist.clone().or_else(|| persisted.dist.clone()),
        allow_hosts: if flags.allow_hosts.is_empty() {
            persisted.allow_hosts.clone()
        } else {
            flags.allow_hosts.clone()
        },
        allow_origins: if flags.allow_origins.is_empty() {
            persisted.allow_origins.clone()
        } else {
            flags.allow_origins.clone()
        },
        read_only: if flags.no_read_only {
            false
        } else {
            flags.read_only || persisted.read_only
        },
        tailscale: persisted.tailscale.clone(),
    };
    if let Some(https_port) = flags.tailscale {
        let host = eff.host.clone().unwrap_or_else(|| "127.0.0.1".to_string());
        if !is_loopback_host(&host) {
            return Err(Error::rejected(format!(
                "--tailscale shares the board through a proxy on this host, \
                 but --host '{host}' is not loopback — bind 127.0.0.1"
            )));
        }
        let ui_port = eff.port.unwrap_or(3010);
        let target = format!("http://127.0.0.1:{ui_port}");
        let me = ts_self()?;
        ensure_mapping(https_port, &target)?;
        eff.tailscale = Some(TailscaleOpts {
            dns_name: me.dns_name,
            https_port,
            target,
        });
    }
    let serve = serve_opts(&eff)?;
    Ok((eff, serve))
}

/// Build the runtime view of effective options: tailnet-derived
/// Host/Origin entries unioned in (deduped, case-insensitive) and the
/// loopback rule enforced.
fn serve_opts(eff: &UiOpts) -> Result<ServeOpts> {
    let host = eff.host.clone().unwrap_or_else(|| "127.0.0.1".to_string());
    let port = eff.port.unwrap_or(3010);
    if eff.tailscale.is_some() && !is_loopback_host(&host) {
        return Err(Error::rejected(format!(
            "--tailscale shares the board through a proxy on this host, \
             but --host '{host}' is not loopback — bind 127.0.0.1"
        )));
    }
    let mut allow_hosts = eff.allow_hosts.clone();
    let mut allow_origins = eff.allow_origins.clone();
    let mut tailnet = None;
    if let Some(ts) = &eff.tailscale {
        for h in ts.hosts() {
            if !allow_hosts.iter().any(|x| x.eq_ignore_ascii_case(&h)) {
                allow_hosts.push(h);
            }
        }
        for o in ts.origins() {
            if !allow_origins.iter().any(|x| x.eq_ignore_ascii_case(&o)) {
                allow_origins.push(o);
            }
        }
        tailnet = Some((ts.dns_name.clone(), ts.https_port));
    }
    Ok(ServeOpts {
        host,
        port,
        dist: eff.dist.clone(),
        allow_hosts,
        allow_origins,
        read_only: eff.read_only,
        tailnet,
    })
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
    let body = serde_json::to_vec_pretty(&json!({"error": message})).unwrap_or_default();
    let mut resp = Response::from_data(body).with_status_code(StatusCode(code));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp
}

/// Defence in depth on an unauthenticated loopback origin that renders
/// agent-written Markdown.
const CSP: &str = "default-src 'self'; img-src 'self' data:; font-src 'self' data:; style-src 'self' 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'";

/// Every response gets nosniff + no-referrer; HTML additionally gets
/// the CSP. Returns whether the response is HTML.
fn add_security_headers(resp: &mut Response<std::io::Cursor<Vec<u8>>>) -> bool {
    let is_html = resp
        .headers()
        .iter()
        .any(|h| h.field.equiv("Content-Type") && h.value.as_str().starts_with("text/html"));
    resp.add_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
    resp.add_header(Header::from_bytes("Referrer-Policy", "no-referrer").unwrap());
    if is_html {
        resp.add_header(Header::from_bytes("Content-Security-Policy", CSP).unwrap());
    }
    is_html
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

fn context_query(
    raw_query: &str,
) -> std::result::Result<(Option<String>, Option<String>), &'static str> {
    let mut role = None;
    let mut expected_revision = None;
    for raw_pair in raw_query.split('&').filter(|pair| !pair.is_empty()) {
        let (raw_key, raw_value) = raw_pair
            .split_once('=')
            .ok_or("context query values must use key=value")?;
        let key = pct_decode(raw_key).ok_or("malformed context query")?;
        let value = pct_decode(raw_value).ok_or("malformed context query")?;
        match key.as_str() {
            "role" if role.is_none() => role = Some(value),
            "expected_revision" if expected_revision.is_none() => expected_revision = Some(value),
            "role" | "expected_revision" => return Err("duplicate context query key"),
            _ => return Err("unknown context query key"),
        }
    }
    if let Some(value) = role.as_deref() {
        if !matches!(value, "pm" | "dev" | "qa" | "ops") {
            return Err("bad context role");
        }
    }
    if let Some(value) = expected_revision.as_deref() {
        if !context::valid_revision(value) {
            return Err("bad expected revision");
        }
    }
    Ok((role, expected_revision))
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
            // Brand files Vite copies from `ui/public/` to the dist root —
            // without these arms the SPA fallback would answer with HTML.
            "/favicon.svg" => Some(embedded::FAVICON),
            "/icon.svg" => Some(embedded::ICON),
            "/apple-touch-icon.png" => Some(embedded::APPLE_TOUCH_ICON),
            _ => path
                .strip_prefix("/assets/")
                .and_then(|name| embedded::ASSETS.get(name).copied()),
        };
        return bytes.map(|b| (path.to_string(), b.to_vec()));
    }
    #[allow(unreachable_code)]
    None
}

#[cfg(feature = "ui")]
mod embedded {
    use std::collections::HashMap;
    use std::sync::LazyLock;

    pub const INDEX: &str = include_str!("../ui/dist/index.html");
    pub const JS: &str = include_str!("../ui/dist/assets/index.js");
    pub const CSS: &str = include_str!("../ui/dist/assets/index.css");
    pub const FAVICON: &[u8] = include_bytes!("../ui/dist/favicon.svg");
    pub const ICON: &[u8] = include_bytes!("../ui/dist/icon.svg");
    pub const APPLE_TOUCH_ICON: &[u8] = include_bytes!("../ui/dist/apple-touch-icon.png");

    /// The latin woff2 files the CSS references (woff fallbacks are not
    /// embedded — every supported browser takes woff2 first).
    pub static ASSETS: LazyLock<HashMap<&'static str, &'static [u8]>> = LazyLock::new(|| {
        HashMap::from([
            (
                "ibm-plex-sans-latin-400-normal.woff2",
                include_bytes!("../ui/dist/assets/ibm-plex-sans-latin-400-normal.woff2") as &[u8],
            ),
            (
                "ibm-plex-sans-latin-500-normal.woff2",
                include_bytes!("../ui/dist/assets/ibm-plex-sans-latin-500-normal.woff2") as &[u8],
            ),
            (
                "ibm-plex-sans-latin-600-normal.woff2",
                include_bytes!("../ui/dist/assets/ibm-plex-sans-latin-600-normal.woff2") as &[u8],
            ),
            (
                "ibm-plex-mono-latin-400-normal.woff2",
                include_bytes!("../ui/dist/assets/ibm-plex-mono-latin-400-normal.woff2") as &[u8],
            ),
            (
                "ibm-plex-mono-latin-500-normal.woff2",
                include_bytes!("../ui/dist/assets/ibm-plex-mono-latin-500-normal.woff2") as &[u8],
            ),
            (
                "ibm-plex-mono-latin-600-normal.woff2",
                include_bytes!("../ui/dist/assets/ibm-plex-mono-latin-600-normal.woff2") as &[u8],
            ),
        ])
    });
}

/// One task assignment enriched with its job context for the board. The
/// join stays exact: agent tasks are joined through the job's task rows and
/// `jobs.issue_id`, never by scanning message text for issue-shaped tokens.
#[derive(Clone)]
struct TaskBinding {
    issue: String,
    task_state: String,
    task_title: Option<String>,
    job: String,
    job_title: Option<String>,
    job_state: String,
}

/// task id → issue/job context for every task that belongs to an
/// issue-bound job.
fn task_issue_map(state_dir: &Path) -> HashMap<String, TaskBinding> {
    let mut map = HashMap::new();
    let Ok(list) = client::rpc(state_dir, "job_list", json!({"all": true})) else {
        return map;
    };
    for job in list["jobs"].as_array().cloned().unwrap_or_default() {
        let Some(issue) = job["issue"].as_str().map(str::to_string) else {
            continue;
        };
        if issue.is_empty() {
            continue;
        }
        let Some(job_id) = job["id"].as_str() else {
            continue;
        };
        let Ok(show) = client::rpc(state_dir, "job_show", json!({"job": job_id})) else {
            continue;
        };
        let job_title = show["job"]["title"].as_str().map(str::to_string);
        let job_state = show["job"]["state"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        for task in show["job"]["tasks"].as_array().cloned().unwrap_or_default() {
            if let (Some(tid), Some(state)) = (task["id"].as_str(), task["state"].as_str()) {
                map.insert(
                    tid.to_string(),
                    TaskBinding {
                        issue: issue.clone(),
                        task_state: state.to_string(),
                        task_title: task["title"].as_str().map(str::to_string),
                        job: job_id.to_string(),
                        job_title: job_title.clone(),
                        job_state: job_state.clone(),
                    },
                );
            }
        }
    }
    map
}

/// Keep an ad-hoc current-message description useful without putting the
/// queued prompt or provider credentials in browser JSON. The shared argv
/// scrubber handles credential-shaped flags, headers, URIs and token forms;
/// the character cap keeps one large prompt from becoming an agent row.
fn current_message_summary(m: &Value) -> Option<String> {
    let body = m["body"].as_str()?.trim();
    if body.is_empty() {
        return None;
    }
    let head: String = body.chars().take(512).collect();
    let tokens: Vec<String> = head.split_whitespace().map(str::to_string).collect();
    let safe = redact_argv(&tokens);
    if safe.is_empty() {
        return None;
    }
    let mut summary: String = safe.chars().take(180).collect();
    if safe.chars().count() > 180 {
        summary.push('…');
    }
    Some(summary)
}

/// One running message reduced for the board: id, task, turn token and a
/// bounded redacted description for ad-hoc current work.
fn running_json(m: &Value) -> Value {
    json!({
        "id": m["id"],
        "task": m["task_id"],
        "turn_id": m["turn_id"],
        "created": m["created"],
        "summary": current_message_summary(m),
    })
}

/// The tail of an agent's event log for the drawer — the last `n`
/// events via the same `events` RPC the CLI long-polls.
fn agent_events_tail(state_dir: &Path, alias: &str, cursor: i64, n: i64) -> Vec<Value> {
    let after = (cursor - n).max(0);
    client::rpc(state_dir, "events", json!({"alias": alias, "after": after}))
        .map(|r| {
            r["events"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|e| e["seq"].as_i64().unwrap_or(0) > cursor - n)
                .collect()
        })
        .unwrap_or_default()
}

/// What the board needs from the daemon — agent rows enriched with the
/// exact task/issue binding, plus the per-agent queue/fence counts and
/// the per-issue agent map for cards and drawers.
/// `daemon: "unreachable"` instead of a 500 when the socket is down:
/// the board still renders.
fn agents_payload(state_dir: &Path) -> Value {
    let list = match client::rpc(state_dir, "agent_list", json!({})) {
        Ok(list) => list,
        Err(_) => {
            return json!({"daemon": "unreachable", "agents": [], "totals": null, "by_issue": {}});
        }
    };
    let task_map = task_issue_map(state_dir);
    let agents = list["agents"].as_array().cloned().unwrap_or_default();
    let mut out = Vec::new();
    let mut inboxes = 0i64;
    let mut by_issue: HashMap<String, Vec<Value>> = HashMap::new();
    let mut totals = json!({"running": 0, "queued": 0, "fenced": 0, "parked": 0, "inboxes": 0});
    for agent in &agents {
        let alias = agent["alias"].as_str().unwrap_or_default();
        let provider = agent["provider"].as_str().unwrap_or_default();
        let kind = agent["endpoint_kind"].as_str().unwrap_or_default();
        let resume = registry::resume_command(
            provider,
            kind,
            agent["thread_id"].as_str().unwrap_or_default(),
            agent["session_id"].as_str().unwrap_or_default(),
            agent["endpoint"].as_str().unwrap_or_default(),
        );
        // Mailboxes are not workers — they still appear on the Agents
        // screen (as kind "inbox") but never count as busy/fenced.
        let actor = registry::has_actor(provider, kind);
        if !actor {
            inboxes += 1;
            out.push(json!({
                "alias": alias, "provider": agent["provider"],
                "endpoint_kind": agent["endpoint_kind"],
                "role": agent["role"],
                "team_role": agent["team_role"],
                "model_selection": agent["model_selection"],
                "model_lookup_role": agent["model_lookup_role"],
                "model": agent["model"],
                "model_reported": agent["model_reported"],
                "model_configured": agent["model_configured"],
                "model_source": agent["model_source"],
                "effort": agent["effort"],
                "effort_reported": agent["effort_reported"],
                "effort_source": agent["effort_source"],
                "effort_applicable": agent["effort_applicable"],
                "quota": agent["quota"],
                "usage_limit": agent["usage_limit"],
                "state": "inbox", "group": agent["params"]["upstream"].as_str().unwrap_or(alias),
                "group_root": agent["params"]["upstream"].is_null(),
                "running": 0, "queued": 0, "unknown": 0, "parked": 0,
                "fenced": false, "on": [], "tasks": [], "message": Value::Null,
                "dead": agent["dead"], "inbox": true,
            }));
            continue;
        }
        let show = client::rpc(state_dir, "agent_show", json!({"alias": alias}));
        let (mut running, mut parked) = (0i64, 0i64);
        let mut running_msgs: Vec<Value> = Vec::new();
        let mut last_activity = Value::Null;
        let (queued, unknown, cursor) = match &show {
            Ok(show) => {
                for m in show["messages"].as_array().cloned().unwrap_or_default() {
                    for ts in ["completed", "started", "created"] {
                        let at = &m[ts];
                        if at
                            .as_str()
                            .map(|a| last_activity.as_str().map(|cur| a > cur).unwrap_or(true))
                            == Some(true)
                        {
                            last_activity = at.clone();
                        }
                    }
                    match m["state"].as_str() {
                        Some("running") => {
                            running += 1;
                            running_msgs.push(running_json(&m));
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
                    show["event_cursor"].as_i64().unwrap_or(0),
                )
            }
            Err(_) => (0, 0, 0),
        };
        // Exact binding: the agent's assigned tasks joined through
        // `jobs.issue_id`. A running kickoff's task is the live one.
        let mut on: Vec<String> = Vec::new();
        let mut bound: Vec<Value> = Vec::new();
        for tid in agent["tasks"].as_array().cloned().unwrap_or_default() {
            let Some(tid) = tid.as_str() else { continue };
            let Some(binding) = task_map.get(tid) else {
                continue;
            };
            if !on.contains(&binding.issue) {
                on.push(binding.issue.clone());
            }
            let message = running_msgs
                .iter()
                .find(|m| m["task"].as_str() == Some(tid))
                .cloned()
                .unwrap_or(Value::Null);
            bound.push(json!({
                "task": tid,
                "task_state": binding.task_state,
                "title": binding.task_title,
                "issue": binding.issue,
                "job": binding.job,
                "job_title": binding.job_title,
                "job_state": binding.job_state,
                "message": message,
            }));
            by_issue
                .entry(binding.issue.clone())
                .or_default()
                .push(json!({
                    "alias": alias,
                    "task": tid,
                    "task_state": binding.task_state,
                    "state": agent["state"],
                    "message": message["id"].clone(),
                    "resume": resume,
                }));
        }
        let fenced = unknown > 0 || agent["state"].as_str() == Some("attention");
        totals["running"] = json!(totals["running"].as_i64().unwrap_or(0) + running);
        totals["queued"] = json!(totals["queued"].as_i64().unwrap_or(0) + queued);
        totals["parked"] = json!(totals["parked"].as_i64().unwrap_or(0) + parked);
        if fenced {
            totals["fenced"] = json!(totals["fenced"].as_i64().unwrap_or(0) + 1);
        }
        // The daemon's own fence text already names the recovery path —
        // the board renders it as copyable code, verbatim.
        let recovery = if fenced {
            agent["error"].as_str().map(str::to_string)
        } else {
            None
        };
        out.push(json!({
            "alias": alias,
            "provider": agent["provider"],
            "endpoint_kind": agent["endpoint_kind"],
            "role": agent["role"],
            "team_role": agent["team_role"],
            "model_selection": agent["model_selection"],
            "model_lookup_role": agent["model_lookup_role"],
            // Preserve provider evidence so the UI can distinguish a
            // confirmed effective model from a requested/configured one.
            "model": agent["model"],
            "model_reported": agent["model_reported"],
            "model_configured": agent["model_configured"],
            "model_source": agent["model_source"],
            "effort": agent["effort"],
            "effort_reported": agent["effort_reported"],
            "effort_source": agent["effort_source"],
            "effort_applicable": agent["effort_applicable"],
            // Quota integrations can add this account/pool-scoped view;
            // absent data stays absent and is rendered unavailable below.
            "quota": agent["quota"],
            "usage_limit": agent["usage_limit"],
            "state": agent["state"],
            "group": agent["params"]["upstream"].as_str().unwrap_or(alias),
            "group_root": agent["params"]["upstream"].is_null(),
            "running": running, "queued": queued, "unknown": unknown,
            "parked": parked, "fenced": fenced,
            "on": on,
            "tasks": bound,
            "message": running_msgs.first().cloned().unwrap_or(Value::Null),
            "running_messages": running_msgs,
            "recovery": recovery,
            "resume": resume,
            "resume_hint": "after stop",
            "dead": agent["dead"],
            "last_activity": last_activity,
            "silent_secs": agent["silent_secs"],
            "stalled": agent["stalled"],
            // CAD-96: `stopped (auto, idle 72m)` — null unless auto-stopped.
            "state_label": agent["state_label"],
            "event_cursor": cursor,
        }));
    }
    totals["inboxes"] = json!(inboxes);
    json!({"daemon": "reachable", "agents": out, "totals": totals, "by_issue": by_issue})
}

/// `/api/agents/<alias>` — the drawer detail: the daemon's own
/// `agent_show` plus the last 20 events and the recovery/resume
/// commands the row chips hinted at.
fn agent_detail(state_dir: &Path, alias: &str) -> std::result::Result<Value, String> {
    let show =
        client::rpc(state_dir, "agent_show", json!({"alias": alias})).map_err(|e| e.to_string())?;
    let agent = &show["agent"];
    let cursor = show["event_cursor"].as_i64().unwrap_or(0);
    let events = agent_events_tail(state_dir, alias, cursor, 20);
    let provider = agent["provider"].as_str().unwrap_or_default();
    let kind = agent["endpoint_kind"].as_str().unwrap_or_default();
    let running: Vec<Value> = show["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|m| m["state"].as_str() == Some("running"))
        .map(running_json)
        .collect();
    let fenced =
        show["unknown"].as_i64().unwrap_or(0) > 0 || agent["state"].as_str() == Some("attention");
    // `tasks` lives on the agent_list row, not the show payload.
    let tasks = client::rpc(state_dir, "agent_list", json!({}))
        .ok()
        .and_then(|l| {
            l["agents"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .find(|a| a["alias"].as_str() == Some(alias))
        })
        .map(|a| a["tasks"].clone())
        .unwrap_or(json!([]));
    // Bound issues through the same tasks × jobs.issue_id join.
    let task_map = task_issue_map(state_dir);
    let mut issues: Vec<&str> = Vec::new();
    for t in tasks.as_array().cloned().unwrap_or_default() {
        if let Some(binding) = t.as_str().and_then(|tid| task_map.get(tid)) {
            if !issues.contains(&binding.issue.as_str()) {
                issues.push(&binding.issue);
            }
        }
    }
    Ok(json!({
        "agent": agent,
        "queued": show["queued"],
        "unknown": show["unknown"],
        "running": running,
        "events": events,
        "fenced": fenced,
        "recovery": if fenced { agent["error"].clone() } else { Value::Null },
        "resume": registry::resume_command(
            provider, kind,
            agent["thread_id"].as_str().unwrap_or_default(),
            agent["session_id"].as_str().unwrap_or_default(),
            agent["endpoint"].as_str().unwrap_or_default(),
        ),
        "tasks": tasks,
        "on": issues,
    }))
}

// ---------- write path (I2) ----------

/// JSON write bodies are small — fields, links, a comment, a body
/// replace. Artifact bytes go through the octet-stream route, capped at
/// `artifact_max_bytes` while reading.
const JSON_CAP: u64 = 256 * 1024;

/// The actor an operator write commits as — visible in `git log`
/// subjects. A pane's write commits as its alias (`write_caller`).
const UI_ACTOR: &str = "operator (ui)";

type HttpResp = Response<std::io::Cursor<Vec<u8>>>;

fn header_value(request: &Request, name: &'static str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str().to_string())
}

fn guard_fail(check: &str, msg: &str) -> HttpResp {
    let body =
        serde_json::to_vec_pretty(&json!({"error": msg, "check": check})).unwrap_or_default();
    let mut resp = Response::from_data(body).with_status_code(StatusCode(403));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp
}

/// Allowed write origins: the allowlisted hosts over http plus the
/// explicit `--allow-origin` entries (the tailnet https origin lands
/// there). A same-origin browser page sends `Origin: <scheme>://<host>`
/// — anything else, or a cross-site `Sec-Fetch-Site`, is not our board.
fn origin_allowed(origin: &str, port: u16, hosts: &[String], origins: &[String]) -> bool {
    let origin = origin.trim().to_ascii_lowercase();
    let mut allowed = vec![
        "http://cadence.localhost".to_string(),
        "http://cadence.localhost:18000".to_string(),
        format!("http://127.0.0.1:{port}"),
        format!("http://localhost:{port}"),
        format!("http://[::1]:{port}"),
    ];
    allowed.extend(
        hosts
            .iter()
            .map(|h| format!("http://{}", h.trim().to_ascii_lowercase())),
    );
    allowed.extend(origins.iter().map(|o| o.trim().to_ascii_lowercase()));
    allowed.contains(&origin)
}

/// The three header guards every write request must pass, checked
/// before any work: exact content type (never a "simple" form type), the
/// custom `X-Cadence-Board: 1` marker, and same-origin Origin /
/// Sec-Fetch-Site when the browser sends them. A cross-site page cannot
/// satisfy any of the three without a preflight this server never
/// answers (OPTIONS is 405; no `Access-Control-*` header is ever sent).
fn write_guard(
    request: &Request,
    want_ct: &str,
    opts: &ServeOpts,
) -> std::result::Result<(), HttpResp> {
    let ct = header_value(request, "Content-Type").unwrap_or_default();
    if ct.trim() != want_ct {
        return Err(guard_fail(
            "content_type",
            &format!("content-type must be exactly '{want_ct}'"),
        ));
    }
    if header_value(request, "X-Cadence-Board").as_deref() != Some("1") {
        return Err(guard_fail("x_cadence_board", "missing X-Cadence-Board: 1"));
    }
    if let Some(origin) = header_value(request, "Origin") {
        if !origin_allowed(&origin, opts.port, &opts.allow_hosts, &opts.allow_origins) {
            return Err(guard_fail(
                "origin",
                &format!("origin '{origin}' is not a board origin"),
            ));
        }
    }
    if let Some(sfs) = header_value(request, "Sec-Fetch-Site") {
        if !sfs.eq_ignore_ascii_case("same-origin") {
            return Err(guard_fail(
                "sec_fetch_site",
                &format!("sec-fetch-site '{sfs}' must be 'same-origin'"),
            ));
        }
    }
    Ok(())
}

/// Read a request body, stopping at `cap + 1` — an oversized upload is
/// refused without ever buffering it whole.
fn read_body(request: &mut Request, cap: u64) -> std::result::Result<Vec<u8>, HttpResp> {
    let mut buf = Vec::new();
    let mut limited = request.as_reader().take(cap + 1);
    if let Err(e) = limited.read_to_end(&mut buf) {
        return Err(err_response(400, &format!("body read failed: {e}")));
    }
    if buf.len() as u64 > cap {
        return Err(err_response(
            413,
            &format!("body is over the {cap}-byte cap"),
        ));
    }
    Ok(buf)
}

fn parse_json<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> std::result::Result<T, HttpResp> {
    serde_json::from_slice(bytes).map_err(|e| err_response(400, &format!("bad request json: {e}")))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewIssueReq {
    project: String,
    title: String,
    priority: Option<String>,
    owner: Option<String>,
    component: Option<String>,
    tags: Option<Vec<String>>,
    parent: Option<String>,
    blocked_by: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchReq {
    status: Option<String>,
    priority: Option<String>,
    /// `""` clears owner.
    owner: Option<String>,
    /// `""` clears component.
    component: Option<String>,
    title: Option<String>,
    body: Option<String>,
    /// Replaces the tag list; `[]` clears it.
    tags: Option<Vec<String>>,
    if_rev: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkReq {
    #[serde(rename = "type")]
    kind: String,
    target: String,
    if_rev: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RefReq {
    kind: String,
    url: Option<String>,
    path: Option<String>,
    label: Option<String>,
    if_rev: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommentReq {
    body: String,
    if_rev: Option<String>,
}

/// A write op's outcome → HTTP response. Conflicts are 409 with the
/// reason; success re-reads the issue and returns the fresh card and
/// detail payloads so the UI needs no second fetch.
fn write_reply(pm: &Pm, state_dir: &Path, id: &str, out: Value, created: bool) -> HttpResp {
    if out.get("conflict").is_some() {
        let mut body = out.clone();
        let msg = match out["conflict"].as_str() {
            Some("if_rev") => "if_rev does not match issue.md — re-read and retry".to_string(),
            Some("status_derived") => out["reason"]
                .as_str()
                .unwrap_or("status is derived")
                .to_string(),
            Some("exists") => format!(
                "artifact '{}' already exists",
                out["artifact"].as_str().unwrap_or_default()
            ),
            _ => "conflict".to_string(),
        };
        body["error"] = json!(msg);
        // The fresh card lets the caller resync on the spot.
        if let Ok((card, _)) = issue_payloads(pm, state_dir, id) {
            body["card"] = card;
        }
        let bytes = serde_json::to_vec_pretty(&body).unwrap_or_default();
        let mut resp = Response::from_data(bytes).with_status_code(StatusCode(409));
        resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
        return resp;
    }
    match issue_payloads(pm, state_dir, id) {
        Ok((card, detail)) => {
            let warnings = out.get("warnings").cloned().unwrap_or(json!([]));
            let body = serde_json::to_vec_pretty(&json!({
                "issue": detail, "card": card, "warnings": warnings,
            }))
            .unwrap_or_default();
            let mut resp = Response::from_data(body).with_status_code(StatusCode(if created {
                201
            } else {
                200
            }));
            resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
            resp
        }
        Err(e) => err_response(500, &format!("write committed but reload failed: {e}")),
    }
}

/// Fresh card + detail payloads for one id after a write.
fn issue_payloads(pm: &Pm, state_dir: &Path, id: &str) -> Result<(Value, Value)> {
    let issues = board::load_all(&pm.dir, None)?;
    let jobs = board::fetch_job_outcomes(state_dir);
    let views = board::views_with_jobs(&pm.config.notes_dir(), issues, &jobs);
    let by_id: HashMap<String, &board::View> = views
        .iter()
        .map(|v| (v.issue.front.id.clone(), v))
        .collect();
    let view = by_id
        .get(id)
        .ok_or_else(|| Error::rejected(format!("unknown issue '{id}'")))?;
    let by_issue = agents_payload(state_dir)["by_issue"].clone();
    Ok((
        with_agents(board::card_json(view), &by_issue, id),
        with_agents(board::detail_json(&pm.dir, view, &by_id), &by_issue, id),
    ))
}

/// Map a writer error to an HTTP status: unknown ids are 404, rejections
/// are 400, internals are 500.
fn write_err(e: &Error) -> HttpResp {
    match e {
        Error::Rejected(m) if m.starts_with("Unknown issue") => err_response(404, m),
        Error::Rejected(m) => err_response(400, m),
        other => err_response(500, &other.to_string()),
    }
}

/// The actor a write commits as. `Tailscale-User-Login` is trusted
/// only when all three hold — tailscale mode armed, the TCP peer is
/// loopback (the proxy connects locally), and the request's Host is
/// the tailnet name. Anything else — including a direct loopback
/// request forging the header under a different Host — writes as the
/// plain operator and the header is never considered.
fn request_actor(request: &Request, opts: &ServeOpts) -> String {
    if !tailnet_request(request, opts) {
        return UI_ACTOR.to_string();
    }
    header_value(request, "Tailscale-User-Login")
        .and_then(|l| sanitize_actor(&l))
        .map(|l| format!("{l} (tailscale)"))
        .unwrap_or_else(|| UI_ACTOR.to_string())
}

/// Tailscale mode armed, the TCP peer is loopback (the proxy connects
/// locally), and the request's Host is the tailnet name — the shape
/// under which `Tailscale-User-Login` is considered at all.
fn tailnet_request(request: &Request, opts: &ServeOpts) -> bool {
    let Some((dns, _)) = &opts.tailnet else {
        return false;
    };
    let peer_ok = request
        .remote_addr()
        .map(|a| a.ip().is_loopback())
        .unwrap_or(false);
    let host = header_value(request, "Host").unwrap_or_default();
    let name = host.split(':').next().unwrap_or_default();
    peer_ok && name.eq_ignore_ascii_case(dns)
}

/// Who a board write commits as (CAD-254, CAD-263). The cross-site
/// guards stop browsers, not local processes: an agent with a shell can
/// send the same headers. So a write derives its caller from the peer
/// module the daemon shares — the TCP peer's process, attributed to a
/// registered pane by a process signal: its `/proc` ancestry or the
/// pane's pty on its stdio. A caller-chosen `CADENCE_ALIAS` alone never
/// attributes ([`crate::peer::tcp_peer_pane`]).
enum WriteCaller {
    /// The operator — `actor` is `operator (ui)` or a trusted tailnet
    /// login; comment authors and monitor acks record `operator`.
    Operator(String),
    /// A process tied to a registered pane IS that agent: its alias is
    /// the actor and author, never `operator`.
    Agent(String),
}

impl WriteCaller {
    fn actor(&self) -> &str {
        match self {
            WriteCaller::Operator(actor) => actor,
            WriteCaller::Agent(alias) => alias,
        }
    }

    fn author(&self) -> &str {
        match self {
            WriteCaller::Operator(_) => "operator",
            WriteCaller::Agent(alias) => alias,
        }
    }
}

/// Derive the write caller, or refuse (`403`, `check:
/// "caller_identity"`) when the peer cannot be attributed — fail
/// closed, never `operator` by default. A tailnet-shaped request keeps
/// its header-based identity unless its peer provably descends from a
/// pane: the real proxy is another user's process this user cannot
/// inspect, so an unattributable peer there is the expected case.
fn write_caller(
    request: &Request,
    state_dir: &Path,
    opts: &ServeOpts,
) -> std::result::Result<WriteCaller, HttpResp> {
    let pane = registered_panes(state_dir).and_then(|panes| {
        if panes.is_empty() {
            return Ok(None);
        }
        let peer = request
            .remote_addr()
            .ok_or_else(|| "the request has no peer address".to_string())?;
        crate::peer::tcp_peer_pane(opts.port, *peer, &panes)
    });
    match pane {
        Ok(Some(alias)) => Ok(WriteCaller::Agent(alias)),
        Ok(None) => Ok(WriteCaller::Operator(request_actor(request, opts))),
        Err(_) if tailnet_request(request, opts) => {
            Ok(WriteCaller::Operator(request_actor(request, opts)))
        }
        Err(why) => Err(guard_fail(
            "caller_identity",
            &format!(
                "board write refused: caller identity underivable — {why}. \
                 Writes attribute the peer process to the registered pane \
                 it is tied to, or to the operator when it is tied to none."
            ),
        )),
    }
}

/// Live registered panes, pane pid → alias — the same rows the
/// daemon's slot identity resolves against (`pty` endpoints with a pid
/// and a generation), read over the daemon's `agent_list` RPC. No
/// store file means no agent was ever registered here: provably no
/// panes. A store the daemon cannot answer for is an error.
fn registered_panes(state_dir: &Path) -> std::result::Result<HashMap<u32, String>, String> {
    if !state_dir.join("cadence.sqlite3").exists() {
        return Ok(HashMap::new());
    }
    let list = client::rpc(state_dir, "agent_list", json!({}))
        .map_err(|e| format!("the daemon cannot list registered panes ({e})"))?;
    let agents = list["agents"]
        .as_array()
        .ok_or_else(|| "the daemon's agent list is malformed".to_string())?;
    Ok(agents
        .iter()
        .filter(|a| a["endpoint_kind"] == "pty" && !a["generation"].is_null())
        .filter_map(|a| {
            let pid = u32::try_from(a["pid"].as_u64()?).ok()?;
            Some((pid, a["alias"].as_str()?.to_string()))
        })
        .collect())
}

/// The login lands in a commit `Actor:` trailer — take the first
/// whitespace-free token of printable ASCII, bounded, else no trust.
fn sanitize_actor(raw: &str) -> Option<String> {
    let tok = raw.split_whitespace().next().unwrap_or_default();
    if tok.is_empty()
        || tok.len() > 120
        || !tok.chars().all(|c| c.is_ascii() && !c.is_ascii_control())
    {
        return None;
    }
    Some(tok.to_string())
}

fn coded_response(status: u16, code: &str, message: &str, revision: Option<i64>) -> HttpResp {
    let body = serde_json::to_vec_pretty(&json!({
        "error": message,
        "code": code,
        "revision": revision,
    }))
    .unwrap_or_default();
    let mut resp = Response::from_data(body).with_status_code(StatusCode(status));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp
}

fn health_supports_model_defaults(health: &Value) -> bool {
    health["capabilities"].as_array().is_some_and(|caps| {
        caps.iter()
            .any(|cap| cap.as_str() == Some(registry::MODEL_DEFAULTS_CAPABILITY))
    })
}

fn daemon_unavailable(err: &Error) -> bool {
    err.to_string().starts_with("Daemon is not reachable")
}

/// Health capability is the compatibility gate. An older daemon stays
/// reachable and answers `health` without `model_defaults`.
fn require_model_defaults(state_dir: &Path) -> std::result::Result<(), HttpResp> {
    match client::rpc(state_dir, "health", json!({})) {
        Err(err) => Err(coded_response(
            503,
            "daemon_unavailable",
            &err.to_string(),
            None,
        )),
        Ok(health) => {
            if health_supports_model_defaults(&health) {
                Ok(())
            } else {
                Err(coded_response(
                    501,
                    "unsupported_daemon",
                    "this daemon does not support model defaults",
                    None,
                ))
            }
        }
    }
}

fn settings_rpc_error(err: Error) -> HttpResp {
    if daemon_unavailable(&err) {
        return coded_response(503, "daemon_unavailable", &err.to_string(), None);
    }
    if err.kind() == "conflict" {
        return coded_response(409, "revision_conflict", &err.to_string(), err.revision());
    }
    if err.kind() == "internal" {
        return err_response(500, &err.to_string());
    }
    let code = err.code().unwrap_or("invalid_request");
    coded_response(400, code, &err.to_string(), err.revision())
}

fn model_defaults_get(state_dir: &Path, read_only: bool) -> HttpResp {
    if let Err(resp) = require_model_defaults(state_dir) {
        return resp;
    }
    match client::rpc(state_dir, "model_defaults_get", json!({})) {
        Ok(mut snapshot) => {
            if let Some(obj) = snapshot.as_object_mut() {
                obj.insert("read_only".to_string(), json!(read_only));
            }
            json_response(snapshot)
        }
        Err(err) => settings_rpc_error(err),
    }
}

fn read_settings_body(request: &mut Request) -> std::result::Result<Vec<u8>, HttpResp> {
    let cap = crate::model_defaults::MAX_HTTP_BODY_BYTES as u64;
    let mut buf = Vec::new();
    let mut limited = request.as_reader().take(cap + 1);
    if let Err(e) = limited.read_to_end(&mut buf) {
        return Err(coded_response(
            400,
            "invalid_request",
            &format!("body read failed: {e}"),
            None,
        ));
    }
    if buf.len() as u64 > cap {
        return Err(coded_response(
            400,
            "invalid_request",
            &format!("settings body exceeds {cap} bytes"),
            None,
        ));
    }
    Ok(buf)
}

fn model_defaults_post(request: &mut Request, state_dir: &Path, opts: &ServeOpts) -> HttpResp {
    if opts.read_only {
        return guard_fail("read_only", "board is read-only — writes are disabled");
    }
    if let Err(resp) = write_guard(request, "application/json", opts) {
        return resp;
    }
    if let Err(resp) = require_model_defaults(state_dir) {
        return resp;
    }
    let caller = match write_caller(request, state_dir, opts) {
        Ok(caller) => caller,
        Err(resp) => return resp,
    };
    let bytes = match read_settings_body(request) {
        Ok(bytes) => bytes,
        Err(resp) => return resp,
    };
    let document = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(_) => {
            return coded_response(
                400,
                "invalid_request",
                "settings body must be UTF-8 JSON",
                None,
            )
        }
    };
    match client::rpc(
        state_dir,
        "model_defaults_set",
        json!({"document": document, "attribution": caller.actor()}),
    ) {
        Ok(mut snapshot) => {
            if let Some(obj) = snapshot.as_object_mut() {
                obj.insert("read_only".to_string(), json!(false));
            }
            json_response(snapshot)
        }
        Err(err) => settings_rpc_error(err),
    }
}

/// Dispatch POST/PATCH/DELETE on the write routes. Every route passes
/// `write_guard` before reading a body or touching the PM dir, and every
/// op goes through `issue::write` — one write path for CLI and API.
#[allow(clippy::too_many_arguments)]
fn write_route(
    mut request: Request,
    method: &Method,
    path: &str,
    query: &dyn Fn(&str) -> Option<String>,
    state_dir: &Path,
    pm_dir: &Path,
    opts: &ServeOpts,
    send: &dyn Fn(Request, HttpResp),
) {
    if path == "/api/settings/model-defaults" {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = model_defaults_post(&mut request, state_dir, opts);
        send(request, resp);
        return;
    }
    // Monitor acknowledgement: this is a daemon-owned durable write, kept
    // beside (and behind the same browser write guards as) tracker writes.
    // The UI never mutates the monitor SQLite store directly, which keeps a
    // board built from a newer binary compatible with an older live daemon.
    if let Some(tail) = path.strip_prefix("/api/monitors/") {
        let mut segs = tail.split('/');
        let monitor = segs.next().unwrap_or_default();
        let alerts = segs.next().unwrap_or_default();
        let seq = segs.next().unwrap_or_default();
        let ack = segs.next().unwrap_or_default();
        let shape_ok = *method == Method::Post
            && !monitor.is_empty()
            && alerts == "alerts"
            && seq.parse::<i64>().is_ok_and(|n| n > 0)
            && ack == "ack"
            && segs.next().is_none();
        if !shape_ok {
            send(request, err_response(404, "no such monitor write route"));
            return;
        }
        if opts.read_only {
            send(
                request,
                guard_fail("read_only", "board is read-only — writes are disabled"),
            );
            return;
        }
        if let Err(resp) = write_guard(&request, "application/json", opts) {
            send(request, resp);
            return;
        }
        let caller = match write_caller(&request, state_dir, opts) {
            Ok(caller) => caller,
            Err(resp) => {
                send(request, resp);
                return;
            }
        };
        // `MonitorAlert` uses the protocol identifier grammar for its audit
        // actor.  The browser actor includes a display suffix, so record
        // the derived author — `operator`, or the pane's alias — rather
        // than passing an invalid or user-controlled value to the daemon.
        let seq = seq.parse::<i64>().unwrap_or_default();
        match client::rpc(
            state_dir,
            "monitor_alert_ack",
            json!({"monitor": monitor, "alert": seq, "by": caller.author()}),
        ) {
            Ok(value) => send(
                request,
                json_response(json!({"ok": true, "alert": value["alert"]})),
            ),
            Err(error) => send(request, err_response(400, &error.to_string())),
        }
        return;
    }

    // Memory curation: POST /api/memories/<project>/<slug>/accept|reject.
    // The CSRF/write guards still run first, but HTTP cannot prove the
    // native socket/PTY identity required by the memory daemon. Refuse
    // explicitly instead of proxying the UI server's peer as a PM.
    if let Some(tail) = path.strip_prefix("/api/memories/") {
        let mut segs = tail.splitn(3, '/');
        let (key, slug, verb) = (
            segs.next().unwrap_or_default(),
            segs.next().unwrap_or_default(),
            segs.next(),
        );
        let shape_ok = matches!(verb, Some("accept" | "reject"))
            && *method == Method::Post
            && !key.is_empty()
            && crate::memory::valid_slug(slug);
        if !shape_ok {
            send(request, err_response(404, "no such write route"));
            return;
        }
        if opts.read_only {
            send(
                request,
                guard_fail("read_only", "board is read-only — writes are disabled"),
            );
            return;
        }
        if let Err(resp) = write_guard(&request, "application/json", opts) {
            send(request, resp);
            return;
        }
        let _ = (key, slug, verb);
        send(
            request,
            write_err(&Error::rejected(
                "memory curation through HTTP is unsupported — use an authenticated native agent endpoint",
            )),
        );
        return;
    }
    let Some(rest) = path.strip_prefix("/api/issues") else {
        send(request, err_response(404, "no such write route"));
        return;
    };
    let (id, sub) = if rest.is_empty() {
        (None, None)
    } else if let Some(tail) = rest.strip_prefix('/') {
        let mut segs = tail.splitn(2, '/');
        (Some(segs.next().unwrap_or_default()), segs.next())
    } else {
        send(request, err_response(404, "no such write route"));
        return;
    };
    // Route shape → expected method. A known shape with the wrong
    // method is 405; an unknown shape is 404.
    let known_sub = matches!(sub, Some("links" | "refs" | "comments" | "artifacts"));
    let shape_ok = matches!(
        (id.is_some(), sub, method),
        (false, None, &Method::Post)
            | (true, None, &Method::Patch)
            | (true, Some("links"), &Method::Post | &Method::Delete)
            | (true, Some("refs" | "comments" | "artifacts"), &Method::Post)
    );
    if !shape_ok {
        let code = if id.is_none() && sub.is_none() || known_sub || (id.is_some() && sub.is_none())
        {
            405
        } else {
            404
        };
        send(request, err_response(code, "no such write route"));
        return;
    }
    if opts.read_only {
        send(
            request,
            guard_fail("read_only", "board is read-only — writes are disabled"),
        );
        return;
    }
    let want_ct = if sub == Some("artifacts") {
        "application/octet-stream"
    } else {
        "application/json"
    };
    if let Err(resp) = write_guard(&request, want_ct, opts) {
        send(request, resp);
        return;
    }
    let caller = match write_caller(&request, state_dir, opts) {
        Ok(caller) => caller,
        Err(resp) => {
            send(request, resp);
            return;
        }
    };
    let actor = caller.actor().to_string();
    let pm = match Pm::at(pm_dir) {
        Ok(pm) => pm,
        Err(e) => {
            send(request, err_response(503, &e.to_string()));
            return;
        }
    };

    if id.is_none() {
        // POST /api/issues — create.
        let bytes = match read_body(&mut request, JSON_CAP) {
            Ok(b) => b,
            Err(resp) => {
                send(request, resp);
                return;
            }
        };
        let req: NewIssueReq = match parse_json(&bytes) {
            Ok(r) => r,
            Err(resp) => {
                send(request, resp);
                return;
            }
        };
        let blocked_by = req.blocked_by.unwrap_or_default();
        match issue_write::new_issue(
            &pm,
            &pm.dir,
            Some(&req.project),
            &req.title,
            req.priority.as_deref(),
            req.parent.as_deref(),
            &blocked_by,
            req.owner.as_deref(),
            req.component.as_deref(),
            &req.tags.unwrap_or_default(),
            None,
            &actor,
        ) {
            Ok(out) => {
                let new_id = out["id"].as_str().unwrap_or_default().to_string();
                send(request, write_reply(&pm, state_dir, &new_id, out, true));
            }
            Err(e) => send(request, write_err(&e)),
        }
        return;
    }

    let id_raw = id.unwrap_or_default();
    let Ok(id) = model::check_id(id_raw) else {
        send(request, err_response(400, "bad issue id"));
        return;
    };
    if sub == Some("artifacts") {
        // POST /api/issues/:id/artifacts?name=<basename> — raw bytes.
        let name = query("name").unwrap_or_default();
        if !model::valid_artifact_name(&name) {
            send(
                request,
                err_response(
                    400,
                    "bad artifact name — [A-Za-z0-9._-]{1,120}, no leading dot",
                ),
            );
            return;
        }
        let cap = pm.config.artifact_max_bytes;
        let bytes = match read_body(&mut request, cap) {
            Ok(b) => b,
            Err(resp) => {
                send(request, resp);
                return;
            }
        };
        match issue_write::attach_bytes(&pm, &id, &name, &bytes, false, &actor) {
            Ok(out) => send(request, write_reply(&pm, state_dir, &id, out, false)),
            Err(e) => send(request, write_err(&e)),
        }
        return;
    }

    let bytes = match read_body(&mut request, JSON_CAP) {
        Ok(b) => b,
        Err(resp) => {
            send(request, resp);
            return;
        }
    };
    let out = match (sub, method) {
        (None, &Method::Patch) => match parse_json::<PatchReq>(&bytes) {
            Ok(req) => issue_write::patch_issue(
                &pm,
                &id,
                &issue_write::IssuePatch {
                    status: req.status,
                    priority: req.priority,
                    owner: req.owner,
                    component: req.component,
                    title: req.title,
                    body: req.body,
                    tags: req.tags,
                },
                req.if_rev.as_deref(),
                &actor,
                Some(state_dir),
            ),
            Err(resp) => {
                send(request, resp);
                return;
            }
        },
        (Some("links"), m) => match parse_json::<LinkReq>(&bytes) {
            Ok(req) => issue_write::link(
                &pm,
                &id,
                &req.kind,
                &req.target,
                m == &Method::Delete,
                req.if_rev.as_deref(),
                &actor,
                Some(state_dir),
            ),
            Err(resp) => {
                send(request, resp);
                return;
            }
        },
        (Some("refs"), _) => match parse_json::<RefReq>(&bytes) {
            Ok(req) => {
                let target = match (req.url, req.path) {
                    (Some(u), None) | (None, Some(u)) => u,
                    _ => {
                        send(
                            request,
                            err_response(400, "send exactly one of url or path"),
                        );
                        return;
                    }
                };
                issue_write::add_ref(
                    &pm,
                    &id,
                    &req.kind,
                    &target,
                    req.label.as_deref(),
                    None,
                    None,
                    req.if_rev.as_deref(),
                    &actor,
                )
            }
            Err(resp) => {
                send(request, resp);
                return;
            }
        },
        (Some("comments"), _) => match parse_json::<CommentReq>(&bytes) {
            Ok(req) => issue_write::add_comment(
                &pm,
                &id,
                &req.body,
                Some(caller.author()),
                Some("ui"),
                req.if_rev.as_deref(),
                &actor,
            ),
            Err(resp) => {
                send(request, resp);
                return;
            }
        },
        _ => unreachable!("shape_ok gated"),
    };
    match out {
        Ok(out) => send(request, write_reply(&pm, state_dir, &id, out, false)),
        Err(e) => send(request, write_err(&e)),
    }
}

/// `GET /api/issues/:id/artifacts/:name` — the constrained read. The
/// name must satisfy the write grammar, resolve to a real regular file
/// inside that issue's `artifacts/` (symlinks refused), and is served
/// inline only for a small allowlist; everything else — and always html,
/// svg, xml, js, pdf — downloads as an octet-stream attachment so a
/// rendered report can never drive the write API.
fn artifact_response(view: &board::View, name: &str) -> HttpResp {
    if !model::valid_artifact_name(name) {
        return err_response(400, "bad artifact name");
    }
    let dir = view.issue.dir.join("artifacts");
    let path = dir.join(name);
    if !board::is_real_dir(&dir) || !board::is_real_file(&path) {
        return err_response(404, "no such artifact");
    }
    let Ok(bytes) = std::fs::read(&path) else {
        return err_response(500, "artifact read failed");
    };
    let ext = name
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let (mime, inline) = match ext.as_str() {
        "txt" | "md" | "log" | "json" | "jsonl" | "yaml" | "yml" | "toml" | "rs" | "ts" | "tsx"
        | "css" | "diff" | "patch" => ("text/plain; charset=utf-8", true),
        "png" => ("image/png", true),
        "jpg" | "jpeg" => ("image/jpeg", true),
        "gif" => ("image/gif", true),
        "webp" => ("image/webp", true),
        _ => ("application/octet-stream", false),
    };
    let mut resp = Response::from_data(bytes);
    resp.add_header(Header::from_bytes("Content-Type", mime).unwrap());
    resp.add_header(
        Header::from_bytes("Content-Security-Policy", "sandbox; default-src 'none'").unwrap(),
    );
    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    if !inline {
        resp.add_header(
            Header::from_bytes(
                "Content-Disposition",
                format!("attachment; filename=\"{name}\""),
            )
            .unwrap(),
        );
    }
    resp
}

fn send(request: Request, mut resp: HttpResp, head_only: bool) {
    add_security_headers(&mut resp);
    if head_only {
        // tiny_http does not strip bodies on HEAD — answer with the
        // same headers as GET, minus the body.
        let mut bare = Response::empty(resp.status_code());
        for h in resp.headers() {
            bare.add_header(h.clone());
        }
        let _ = request.respond(bare);
    } else {
        let _ = request.respond(resp);
    }
}

/// Merge the `by_issue` runtime strip into a card/detail payload —
/// cards get `agents: [{alias, task, task_state, state, message,
/// resume}]` only when a job actually binds agents to the issue.
fn with_agents(mut payload: Value, by_issue: &Value, id: &str) -> Value {
    if let Some(agents) = by_issue.get(id) {
        payload["agents"] = agents.clone();
    }
    payload
}

// ---------- /api/stream — server-sent events ----------

/// Newest mtime among regular files under `dir` — the tracker-change
/// fingerprint. Small tree; a full walk every poll is still cheap.
fn dir_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    let mut newest = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                let m = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                if newest.is_none_or(|n| m > n) {
                    newest = Some(m);
                }
            }
        }
    }
    newest
}

/// A cheap content fingerprint for a JSON value — the serialized bytes
/// through a stable hasher (no new dependency for one hash).
fn value_fp(value: &Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_vec(value).unwrap_or_default().hash(&mut h);
    h.finish()
}

/// The per-agent fingerprint input: the agent_list row plus the queue
/// counters and event cursor that only `agent_show` exposes — any
/// message, fence, or liveness change moves it.
fn agents_fp(state_dir: &Path, list: &Value) -> u64 {
    let mut parts = vec![list.clone()];
    for agent in list["agents"].as_array().cloned().unwrap_or_default() {
        if let Some(alias) = agent["alias"].as_str() {
            if let Ok(show) = client::rpc(state_dir, "agent_show", json!({"alias": alias})) {
                parts.push(json!({
                    "queued": show["queued"], "unknown": show["unknown"],
                    "cursor": show["event_cursor"],
                }));
            }
        }
    }
    value_fp(&json!(parts))
}

/// Poll the board inputs once a second and push
/// `issues|agents|jobs|monitoring` event names into `tx` on change. The
/// baseline is taken before the loop so a fresh client only sees deltas.
/// `__tick` is the per-second liveness probe: the reader drops it, and
/// a failed send means the client hung up — stop polling the daemon.
fn watch_changes(
    state_dir: PathBuf,
    pm_dir: PathBuf,
    mut tracker: Option<std::time::SystemTime>,
    mut jobs: u64,
    mut agents: u64,
    mut monitoring: u64,
    tx: std::sync::mpsc::Sender<&'static str>,
) {
    loop {
        std::thread::sleep(Duration::from_secs(1));
        if tx.send("__tick").is_err() {
            return;
        }
        let t = dir_mtime(&pm_dir);
        if t != tracker {
            tracker = t;
            if tx.send("issues").is_err() {
                return;
            }
        }
        if let Ok(list) = client::rpc(&state_dir, "job_list", json!({"all": true})) {
            let fp = value_fp(&list);
            if fp != jobs {
                jobs = fp;
                if tx.send("jobs").is_err() {
                    return;
                }
            }
        }
        if let Ok(list) = client::rpc(&state_dir, "agent_list", json!({})) {
            let fp = agents_fp(&state_dir, &list);
            if fp != agents {
                agents = fp;
                if tx.send("agents").is_err() {
                    return;
                }
            }
        }
        if let Ok(list) = client::rpc(&state_dir, "monitor_list", json!({})) {
            let fp = value_fp(&list);
            if fp != monitoring {
                monitoring = fp;
                if tx.send("monitoring").is_err() {
                    return;
                }
            }
        }
    }
}

/// The board resources one stream event invalidates, sent as the frame's
/// data (`{"resources":[...]}`) so the client refetches only those. The
/// event name stays the change source, which older clients key on.
/// - tracker files → the cards, the per-project list, the open issue
///   and the overview's tracker rows;
/// - jobs → card status (job outcomes) and agent task bindings;
/// - agents → agent rows and their `by_issue` strip (cards read their
///   agent chips from it), the open issue's agents, overview needs;
/// - monitors → the overview's monitoring block only.
fn event_resources(name: &str) -> &'static [&'static str] {
    match name {
        "issues" => &["issues", "projects", "issue", "overview"],
        "jobs" => &["issues", "agents", "issue", "overview"],
        "agents" => &["agents", "issue", "overview"],
        "monitoring" => &["overview"],
        _ => &["issues", "projects", "agents", "issue", "overview"],
    }
}

/// `GET /api/stream` — server-sent events written straight onto the
/// socket. tiny_http's chunked path buffers small writes inside
/// `chunked_transfer::Encoder` (it flushes only on `flush()` or a full
/// chunk), so a reader-based `Response` would never emit a small SSE
/// frame. `into_writer` hands over the socket: the head is written by
/// hand, each frame flushes immediately, and dropping the writer on
/// exit closes the stream — which is also how a dead client surfaces.
fn stream_events(request: Request, state_dir: &Path, pm_dir: &Path) {
    let mut w = request.into_writer();
    // Baselines at connect time, before the client can observe the
    // stream is live — if they were taken inside the spawned thread
    // they would race the client's first action and silently absorb it.
    let tracker0 = dir_mtime(pm_dir);
    let jobs0 = client::rpc(state_dir, "job_list", json!({"all": true}))
        .map(|l| value_fp(&l))
        .unwrap_or(0);
    let agents0 = client::rpc(state_dir, "agent_list", json!({}))
        .map(|l| agents_fp(state_dir, &l))
        .unwrap_or(0);
    let monitoring0 = client::rpc(state_dir, "monitor_list", json!({}))
        .map(|l| value_fp(&l))
        .unwrap_or(0);
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                Cache-Control: no-store\r\nConnection: close\r\n\r\n";
    if w.write_all(head.as_bytes())
        .and_then(|_| w.flush())
        .is_err()
    {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel::<&'static str>();
    std::thread::spawn({
        let (state_dir, pm_dir) = (state_dir.to_path_buf(), pm_dir.to_path_buf());
        move || watch_changes(state_dir, pm_dir, tracker0, jobs0, agents0, monitoring0, tx)
    });
    let frame = |w: &mut dyn Write, bytes: &[u8]| -> bool {
        w.write_all(bytes).and_then(|_| w.flush()).is_ok()
    };
    // First frame immediately — proves the stream is live and gives
    // proxies something to flush before the first event exists.
    if !frame(&mut w, b": ping\n\n") {
        return;
    }
    // `: ping` every 15 s of wire silence — the per-second `__tick`
    // would otherwise starve the keepalive, and an idle dead client
    // would never surface without a write.
    let mut ping_at = Instant::now() + Duration::from_secs(15);
    loop {
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok("__tick") => {}
            Ok(name) => {
                let data = json!({"resources": event_resources(name)});
                if !frame(
                    &mut w,
                    format!("event: {name}\ndata: {data}\n\n").as_bytes(),
                ) {
                    return;
                }
                ping_at = Instant::now() + Duration::from_secs(15);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if Instant::now() >= ping_at {
            if !frame(&mut w, b": ping\n\n") {
                return;
            }
            ping_at = Instant::now() + Duration::from_secs(15);
        }
    }
}

fn handle(request: Request, state_dir: &Path, pm_dir: &Path, opts: &ServeOpts) {
    let method = request.method().clone();
    let head_only = method == Method::Head;
    let is_write = matches!(method, Method::Post | Method::Patch | Method::Delete);
    if !matches!(method, Method::Get | Method::Head) && !is_write {
        let _ = request.respond(err_response(405, "method not allowed"));
        return;
    }
    let host = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Host"))
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default();
    if !host_allowed(&host, opts.port, &opts.allow_hosts) {
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

    // Every value of a repeatable key — `tag=a&tag=b` and `tag=a,b` agree.
    let query_all = |key: &str| -> Vec<String> {
        raw_query
            .split('&')
            .filter_map(|kv| {
                let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                (k == key).then(|| pct_decode(v)).flatten()
            })
            .flat_map(|v| {
                v.split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect()
    };

    let send = |req: Request, resp: HttpResp| send(req, resp, head_only);
    let actor = request_actor(&request, opts);

    if is_write {
        write_route(
            request, &method, &path, &query, state_dir, pm_dir, opts, &send,
        );
        return;
    }

    match path.as_str() {
        // What the SPA needs to render itself correctly for this
        // client: read-only mode, the actor this request would write
        // as, and the tailnet URL when sharing is armed. Build identity
        // rides too — the serving binary's, plus the daemon's when
        // reachable (`daemon_info` carries the running build, which is
        // the one deploy drift measures).
        "/api/meta" => {
            let daemon = client::rpc(state_dir, "daemon_info", json!({})).ok();
            send(
                request,
                json_response(json!({
                    "read_only": opts.read_only,
                    "actor": actor,
                    "tailnet_url": opts
                        .tailnet
                        .as_ref()
                        .map(|(dns, port)| tailnet_url(dns, *port)),
                    "version": env!("CARGO_PKG_VERSION"),
                    "build_commit": crate::overview::BUILD_COMMIT,
                    "build_time": crate::overview::BUILD_TIME,
                    "daemon": daemon,
                })),
            );
        }
        "/api/settings/model-defaults" => {
            send(request, model_defaults_get(state_dir, opts.read_only));
        }
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
        "/api/overview" => send(
            request,
            json_response(crate::overview::overview_board(state_dir, pm_dir)),
        ),
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
                            "tags": p.tags,
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
                // The same slices as `issue ls`: tag (all of), status
                // (any of), epic, owner, component, priority, open=1.
                let slice = board::Filter {
                    tags: query_all("tag"),
                    epic: query("epic").filter(|s| !s.is_empty()),
                    owner: query("owner").filter(|s| !s.is_empty()),
                    statuses: query_all("status"),
                    component: query("component").filter(|s| !s.is_empty()),
                    priority: query("priority").filter(|s| !s.is_empty()),
                    open: query("open").is_some_and(|v| v == "1" || v == "true"),
                };
                if let Err(e) = slice.validate() {
                    send(request, err_response(400, &e.to_string()));
                    return;
                }
                let issues = board::load_all(&pm.dir, filter.as_deref()).unwrap_or_default();
                let jobs = board::fetch_job_outcomes(state_dir);
                let views = board::views_with_jobs(&pm.config.notes_dir(), issues, &jobs);
                let by_issue = agents_payload(state_dir)["by_issue"].clone();
                send(
                    request,
                    json_response(json!({
                        "issues": views
                            .iter()
                            .filter(|v| slice.matches(v))
                            .map(|v| with_agents(board::card_json(v), &by_issue, &v.issue.front.id))
                            .collect::<Vec<_>>(),
                    })),
                );
            }
            Err(e) => send(request, err_response(503, &e.to_string())),
        },
        // `GET /api/epics?project=` — issues with children and their
        // children's progress; the payload `issue epic ls --json` prints.
        "/api/epics" => match Pm::at(pm_dir) {
            Ok(pm) => {
                let filter = query("project");
                if filter.as_ref().is_some_and(|p| !model::valid_key(p)) {
                    send(request, err_response(400, "bad project key"));
                    return;
                }
                // Every project loads so cross-project children count.
                let issues = board::load_all(&pm.dir, None).unwrap_or_default();
                let jobs = board::fetch_job_outcomes(state_dir);
                let views = board::views_with_jobs(&pm.config.notes_dir(), issues, &jobs);
                send(
                    request,
                    json_response(json!({
                        "epics": board::epics_json(&views, filter.as_deref()),
                    })),
                );
            }
            Err(e) => send(request, err_response(503, &e.to_string())),
        },
        "/api/agents" => send(request, json_response(agents_payload(state_dir))),
        "/api/memories" => match Pm::at(pm_dir) {
            Ok(pm) => {
                let project = query("project");
                let status = query("status");
                let kind = query("type");
                let (mems, errors) = crate::memory::load_all_report(&pm.dir);
                let payload: Vec<Value> = mems
                    .iter()
                    .filter(|m| {
                        project.as_deref().map(|p| m.project == p).unwrap_or(true)
                            && status
                                .as_deref()
                                .map(|s| m.front.status == s)
                                .unwrap_or(true)
                            && kind.as_deref().map(|k| m.front.kind == k).unwrap_or(true)
                    })
                    .map(crate::memory::card_json)
                    .collect();
                send(
                    request,
                    json_response(json!({
                        "memories": payload,
                        "memory_errors": errors,
                    })),
                );
            }
            Err(e) => send(request, err_response(503, &e.to_string())),
        },
        "/api/stream" => {
            if head_only {
                send(request, err_response(405, "stream is GET only"));
            } else {
                stream_events(request, state_dir, pm_dir);
            }
        }
        _ => {
            // A selected project's bounded, tracked-document context. The
            // project key is resolved through the PM registry before any repo
            // path is touched; no request value becomes a filesystem path.
            if let Some(tail) = path.strip_prefix("/api/projects/") {
                let mut segs = tail.split('/');
                let key = segs.next().unwrap_or_default();
                let sub = segs.next();
                if sub == Some("context") && segs.next().is_none() {
                    if !model::valid_key(key) {
                        send(request, err_response(400, "bad project key"));
                        return;
                    }
                    let (role, expected_revision) = match context_query(raw_query) {
                        Ok(query) => query,
                        Err(error) => {
                            send(request, err_response(400, error));
                            return;
                        }
                    };
                    match Pm::at(pm_dir) {
                        Ok(pm) => match project::list(&pm.dir) {
                            Ok(projects) => match projects.iter().find(|p| p.key == key) {
                                Some(selected) => send(
                                    request,
                                    json_response(context::bundle(
                                        &pm,
                                        selected,
                                        role.as_deref(),
                                        expected_revision.as_deref(),
                                    )),
                                ),
                                None => send(request, err_response(404, "unknown project")),
                            },
                            Err(error) => send(request, err_response(503, &error.to_string())),
                        },
                        Err(error) => send(request, err_response(503, &error.to_string())),
                    }
                    return;
                }
            }
            // `/api/memories/<project>/<slug>` — memory detail.
            if let Some(tail) = path.strip_prefix("/api/memories/") {
                let mut segs = tail.splitn(2, '/');
                let (key, slug) = (
                    segs.next().unwrap_or_default(),
                    segs.next().unwrap_or_default(),
                );
                if !crate::memory::valid_slug(slug) || key.is_empty() || slug.contains('/') {
                    send(request, err_response(400, "bad memory path"));
                    return;
                }
                match Pm::at(pm_dir).and_then(|pm| crate::memory::find(&pm, Some(key), slug)) {
                    Ok((_, m)) => send(request, json_response(crate::memory::detail_json(&m))),
                    Err(e) => send(request, err_response(404, &e.to_string())),
                }
                return;
            }
            // `/api/agents/<alias>` — the drawer detail endpoint.
            if let Some(alias) = path.strip_prefix("/api/agents/") {
                if alias.is_empty()
                    || alias.len() > 80
                    || !alias
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
                {
                    send(request, err_response(400, "bad agent alias"));
                    return;
                }
                match agent_detail(state_dir, alias) {
                    Ok(detail) => send(request, json_response(detail)),
                    Err(e) => send(request, err_response(404, &e)),
                }
                return;
            }
            // `/api/issues/<ID>[/file|/activity|/history|/artifacts/<name>]`
            // — id grammar checked before the id is ever a path component.
            if let Some(tail) = path.strip_prefix("/api/issues/") {
                let mut segs = tail.splitn(2, '/');
                let id_raw = segs.next().unwrap_or_default();
                let sub = segs.next();
                if sub.is_some_and(|s| {
                    !matches!(s, "file" | "activity" | "history") && !s.starts_with("artifacts/")
                }) {
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
                        let jobs = board::fetch_job_outcomes(state_dir);
                        let views = board::views_with_jobs(&pm.config.notes_dir(), issues, &jobs);
                        let by_id: std::collections::HashMap<String, &board::View> = views
                            .iter()
                            .map(|v| (v.issue.front.id.clone(), v))
                            .collect();
                        let Some(view) = by_id.get(&id) else {
                            send(request, err_response(404, "unknown issue"));
                            return;
                        };
                        match sub {
                            None => {
                                let by_issue = agents_payload(state_dir)["by_issue"].clone();
                                send(
                                    request,
                                    json_response(with_agents(
                                        board::detail_json(&pm.dir, view, &by_id),
                                        &by_issue,
                                        &id,
                                    )),
                                )
                            }
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
                            // `GET /api/issues/<ID>/history?limit=N` —
                            // the same entries `issue log` prints.
                            Some("history") => {
                                let limit = match query("limit") {
                                    Some(raw) => match raw.parse::<usize>() {
                                        Ok(n) if n > 0 => n,
                                        _ => {
                                            send(request, err_response(400, "bad limit"));
                                            return;
                                        }
                                    },
                                    None => 50,
                                };
                                match history::log(&pm.dir, &view.issue, limit) {
                                    Ok(h) => send(
                                        request,
                                        json_response(json!({"id": id, "history": h})),
                                    ),
                                    Err(e) => send(request, err_response(503, &e.to_string())),
                                }
                            }
                            Some(s) if s.starts_with("artifacts/") => {
                                let name = s.strip_prefix("artifacts/").unwrap_or_default();
                                send(request, artifact_response(view, name));
                            }
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
            match static_file(opts.dist.as_deref(), target) {
                Some((name, bytes)) => {
                    let mut resp = Response::from_data(bytes);
                    resp.add_header(
                        Header::from_bytes("Content-Type", content_type(&name)).unwrap(),
                    );
                    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
                    send(request, resp);
                }
                None => {
                    if let Some((name, bytes)) = static_file(opts.dist.as_deref(), "/index.html") {
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

/// One mutex for every write route — the server is thread-per-request
/// since `/api/stream`, and issue file writes must not interleave.
static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn serve(state_dir: &Path, pm_dir: &Path, opts: &ServeOpts) -> Result<()> {
    let server = Server::http(format!("{}:{}", opts.host, opts.port))
        .map_err(|e| Error::internal(format!("ui bind {}:{}: {e}", opts.host, opts.port)))?;
    eprintln!("cadence ui listening on http://{}:{}", opts.host, opts.port);
    for request in server.incoming_requests() {
        // Thread per request: `/api/stream` holds its connection open
        // for the session's lifetime and must not starve the board.
        let (state_dir, pm_dir, opts) =
            (state_dir.to_path_buf(), pm_dir.to_path_buf(), opts.clone());
        std::thread::spawn(move || {
            let is_write = matches!(
                request.method(),
                Method::Post | Method::Patch | Method::Delete
            );
            if is_write {
                let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
                handle(request, &state_dir, &pm_dir, &opts);
            } else {
                handle(request, &state_dir, &pm_dir, &opts);
            }
        });
    }
    Ok(())
}

// ---------- lifecycle (mirrors `daemon start|stop|status`) ----------

fn pid_file(state_dir: &Path) -> PathBuf {
    state_dir.join("ui.pid")
}

/// Is a detached `cadence ui` server alive for this state dir — the
/// pidfile's pid, alive-checked. `daemon restart --ui` reads this to
/// decide whether to bounce the board.
pub fn detached_pid(state_dir: &Path) -> Option<i32> {
    read_pid(state_dir)
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
/// dependency. `headers` are extra request lines (`Tailscale-User-Login`
/// for the identity probe). Returns `(status, body)`.
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
fn run(state_dir: &Path, flags: &UiFlags) -> Result<i32> {
    let persisted = load_opts(state_dir);
    sandbox_guard(flags, &persisted)?;
    let (_eff, so) = resolve_opts(flags, &persisted)?;
    serve(state_dir, &crate::issue::default_dir()?, &so)?;
    Ok(0)
}

/// Under the sandbox profile: no tailscale sharing (it publishes a
/// host-wide serve mapping) and never the production port 3010, which
/// is also what an unset port defaults to.
fn sandbox_guard(flags: &UiFlags, persisted: &UiOpts) -> Result<()> {
    if !crate::sandbox::active() {
        return Ok(());
    }
    if flags.tailscale.is_some() || persisted.tailscale.is_some() {
        crate::sandbox::refuse_global("tailscale sharing (`--tailscale`)")?;
    }
    crate::sandbox::refuse_port(flags.port.or(persisted.port).unwrap_or(3010))
}

/// `ui start` — merge flags over `ui.json`, persist the effective
/// options, spawn a detached `ui run` that reads them back. A
/// persisted tailscale block re-ensures its mapping (idempotent; a
/// foreign mapping on the port is still a hard refusal, an
/// unreachable tailscaled a warning — the board still serves
/// loopback).
fn start(state_dir: &Path, flags: &UiFlags, reset: bool) -> Result<i32> {
    start_inner(state_dir, flags, reset, false)
}

/// `ui start` with no stdout — for composed callers (session's
/// `--fix`) whose own output must stay a single document.
pub(crate) fn start_quiet(state_dir: &Path, flags: &UiFlags, reset: bool) -> Result<i32> {
    start_inner(state_dir, flags, reset, true)
}

fn start_inner(state_dir: &Path, flags: &UiFlags, reset: bool, quiet: bool) -> Result<i32> {
    std::fs::create_dir_all(state_dir)?;
    if reset {
        let _ = std::fs::remove_file(opts_file(state_dir));
    }
    let persisted = load_opts(state_dir);
    sandbox_guard(flags, &persisted)?;
    let (eff, so) = resolve_opts(flags, &persisted)?;
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
    if let Some(pid) = read_pid(state_dir) {
        let (code, _) = http_get(&host, port, "/api/health", &format!("{host}:{port}"), &[])
            .unwrap_or((0, String::new()));
        if !quiet {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
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
    command
        .arg("--state-dir")
        .arg(state_dir)
        .args(["ui", "run", "--host", &host, "--port"])
        .arg(port.to_string());
    if let Some(dist) = &eff.dist {
        command.arg("--dist").arg(dist);
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
        if let Ok((200, _)) = http_get(&host, port, "/api/health", &format!("{host}:{port}"), &[]) {
            if !quiet {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "state": "started", "pid": child.id(),
                        "url": format!("http://{host}:{port}"),
                        "tailnet_url": eff.tailscale.as_ref().map(|t| t.url()),
                        "read_only": eff.read_only,
                        "gateway": "http://cadence.localhost:18000",
                        "log": state_dir.join("ui.log"),
                    }))
                    .unwrap_or_default()
                );
            }
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

/// SIGTERM the detached server and wait for exit — no output, for
/// callers (stop, the tailscale verbs) that print their own result.
fn kill_detached(state_dir: &Path) -> Option<i32> {
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

fn stop(state_dir: &Path, tailscale_off: bool) -> Result<i32> {
    if tailscale_off {
        crate::sandbox::refuse_global("`ui stop --tailscale-off`")?;
    }
    let pid = kill_detached(state_dir);
    if pid.is_none() {
        let _ = std::fs::remove_file(pid_file(state_dir));
    }
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
        serde_json::to_string_pretty(&json!({
            "state": "stopped",
            "pid": pid,
            "note": if pid.is_none() { Some("no live pid") } else { None },
            "tailscale_off": ts_result,
        }))
        .unwrap_or_default()
    );
    Ok(0)
}

fn status(state_dir: &Path) -> Result<i32> {
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
        serde_json::to_string_pretty(&json!({
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
        }))
        .unwrap_or_default()
    );
    Ok(0)
}

// ---------- tailscale (serve only — never funnel) ----------

/// One bounded `tailscale` invocation — the only way cadence talks to
/// it, and `funnel` is never among the args.
fn ts(args: &[&str]) -> Result<std::process::Output> {
    let mut cmd = Command::new("tailscale");
    cmd.args(args);
    proc::run_bounded(&mut cmd, Duration::from_secs(15)).map_err(|e| match e {
        BoundedError::Spawn(_) => Error::rejected("tailscale is not installed or not on PATH"),
        other => Error::internal(format!("tailscale {}: {other}", args.join(" "))),
    })
}

/// The error a dead/logged-out tailscaled produces — `ui start` warns
/// and serves loopback rather than refuse outright.
fn is_ts_offline(e: &Error) -> bool {
    matches!(e, Error::Rejected(m) if m.contains("tailscale"))
}

struct TsSelf {
    dns_name: String,
}

/// `tailscale status --json` → the node's DNS name, with the three
/// refusal states the operator can act on named plainly.
fn ts_self() -> Result<TsSelf> {
    let out = ts(&["status", "--json"])?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let lower = stderr.to_lowercase();
        if lower.contains("logged out") {
            return Err(Error::rejected(
                "tailscale is logged out — run `tailscale up` first",
            ));
        }
        return Err(Error::rejected(format!(
            "tailscale status failed: {}",
            stderr.trim()
        )));
    }
    let v: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| Error::internal(format!("tailscale status --json: {e}")))?;
    let state = v["BackendState"].as_str().unwrap_or_default();
    if state != "Running" {
        return Err(Error::rejected(format!(
            "tailscale is not up (BackendState {state:?}) — run `tailscale up` first"
        )));
    }
    let dns = v["Self"]["DNSName"]
        .as_str()
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_string();
    if dns.is_empty() {
        return Err(Error::rejected(
            "tailscale reports no DNS name — is this node logged in?",
        ));
    }
    let cert_domains = v["CertDomains"].as_array().cloned().unwrap_or_default();
    if cert_domains.is_empty() {
        return Err(Error::rejected(
            "HTTPS certificates are not enabled for this tailnet — enable \
             them in the admin console (DNS → HTTPS Certificates) first",
        ));
    }
    Ok(TsSelf { dns_name: dns })
}

/// `tailscale serve status --json` → https port → proxy target.
/// `Web` keys are `<dns>:<port>` (bare `<dns>` is port 443).
fn serve_map() -> Result<HashMap<u16, String>> {
    let out = ts(&["serve", "status", "--json"])?;
    if !out.status.success() {
        return Err(Error::rejected(format!(
            "tailscale serve status failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let v: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| Error::internal(format!("tailscale serve status --json: {e}")))?;
    let mut map = HashMap::new();
    if let Some(web) = v["Web"].as_object() {
        for (key, entry) in web {
            let port: u16 = key
                .rsplit(':')
                .next()
                .and_then(|p| p.parse().ok())
                .unwrap_or(443);
            if let Some(target) = entry["Handlers"]["/"]["Proxy"].as_str() {
                map.insert(port, target.to_string());
            }
        }
    }
    Ok(map)
}

/// Ensure `https:<port>` proxies to `target`: identical mapping is
/// left alone (returns false), a different one on that port is a hard
/// refusal — cadence never overwrites somebody else's serve config.
fn ensure_mapping(port: u16, target: &str) -> Result<bool> {
    match serve_map()?.get(&port) {
        Some(existing) if existing == target => Ok(false),
        Some(other) => Err(Error::rejected(format!(
            "tailscale serve :{port} already targets {other} — refusing to \
             overwrite it; pick another port or free that mapping first"
        ))),
        None => {
            let out = ts(&["serve", "--bg", &format!("--https={port}"), target])?;
            if !out.status.success() {
                return Err(Error::rejected(format!(
                    "tailscale serve --https={port} failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            Ok(true)
        }
    }
}

/// Remove the mapping only while it still targets what cadence
/// recorded — a foreign or absent mapping returns false.
fn remove_mapping(port: u16, expected: &str) -> Result<bool> {
    match serve_map()?.get(&port) {
        Some(existing) if existing == expected => {
            let out = ts(&["serve", &format!("--https={port}"), "off"])?;
            if !out.status.success() {
                return Err(Error::rejected(format!(
                    "tailscale serve --https={port} off failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

// ---------- `ui tailscale …` ----------

fn tailscale_cli(state_dir: &Path, action: &TailscaleAction) -> Result<i32> {
    match action {
        TailscaleAction::Start { port, read_only } => ts_start(state_dir, *port, *read_only),
        TailscaleAction::Stop => ts_stop(state_dir),
        TailscaleAction::Status => ts_status(state_dir),
    }
}

/// `ui tailscale start` — the whole flow: resolve the tailnet
/// identity, ensure the mapping, persist, (re)start the board so the
/// new Host/Origin allowlists are live, print the URL.
fn ts_start(state_dir: &Path, https_port: u16, read_only: bool) -> Result<i32> {
    ts_start_inner(state_dir, https_port, read_only, false)
}

/// `ui tailscale start` with no stdout — for composed callers
/// (session's `--fix`).
pub(crate) fn ts_start_quiet(state_dir: &Path, https_port: u16, read_only: bool) -> Result<i32> {
    ts_start_inner(state_dir, https_port, read_only, true)
}

fn ts_start_inner(state_dir: &Path, https_port: u16, read_only: bool, quiet: bool) -> Result<i32> {
    crate::sandbox::refuse_global("`ui tailscale start`")?;
    let me = ts_self()?;
    let mut opts = load_opts(state_dir);
    let ui_port = opts.port.unwrap_or(3010);
    let target = format!("http://127.0.0.1:{ui_port}");
    let created = ensure_mapping(https_port, &target)?;
    opts.tailscale = Some(TailscaleOpts {
        dns_name: me.dns_name,
        https_port,
        target,
    });
    if read_only {
        opts.read_only = true;
    }
    save_opts(state_dir, &opts)?;
    // Validate before touching a running board — a persisted
    // non-loopback host must not kill it for a sharing mode that can
    // never come up.
    let _ = serve_opts(&opts)?;
    let was_running = read_pid(state_dir).is_some();
    if was_running {
        eprintln!("restarting the board so the tailnet allowlists take effect — brief outage");
        kill_detached(state_dir);
    }
    let code = start_inner(state_dir, &UiFlags::default(), false, true)?;
    let ts = opts.tailscale.as_ref().expect("set above");
    if !quiet {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "state": "sharing",
                "tailnet_url": ts.url(),
                "mapping": format!("https:{} → {}", ts.https_port, ts.target),
                "mapping_created": created,
                "board": if was_running { "restarted" } else { "started" },
                "read_only": opts.read_only,
            }))
            .unwrap_or_default()
        );
    }
    Ok(code)
}

/// `ui tailscale stop` — remove only cadence's mapping, drop the
/// tailnet options, restart the board local-only when it runs.
fn ts_stop(state_dir: &Path) -> Result<i32> {
    let mut opts = load_opts(state_dir);
    let Some(ts) = opts.tailscale.take() else {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"state": "not_sharing"})).unwrap_or_default()
        );
        return Ok(0);
    };
    let removed = match remove_mapping(ts.https_port, &ts.target) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("warning: {e}");
            false
        }
    };
    opts.tailscale = None;
    save_opts(state_dir, &opts)?;
    let was_running = read_pid(state_dir).is_some();
    if was_running {
        eprintln!("restarting the board local-only — brief outage");
        kill_detached(state_dir);
        start_inner(state_dir, &UiFlags::default(), false, true)?;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "state": "stopped_sharing",
            "mapping_removed": removed,
            "board": if was_running { "restarted" } else { "not_running" },
        }))
        .unwrap_or_default()
    );
    Ok(0)
}

/// `ui tailscale status` — sharing state, the URL, the live mapping,
/// the identity a test request resolves to, and the QR.
fn ts_status(state_dir: &Path) -> Result<i32> {
    let opts = load_opts(state_dir);
    let Some(ts) = &opts.tailscale else {
        println!("tailscale sharing: off");
        return Ok(0);
    };
    let url = ts.url();
    println!("tailscale sharing: on");
    println!("url:      {url}");
    match serve_map() {
        Ok(map) => match map.get(&ts.https_port) {
            Some(t) if t == &ts.target => {
                println!("mapping:  https:{} → {}  (live)", ts.https_port, t)
            }
            Some(t) => println!(
                "mapping:  https:{} → {}  (NOT ours — left alone)",
                ts.https_port, t
            ),
            None => println!("mapping:  https:{} absent", ts.https_port),
        },
        Err(e) => println!("mapping:  unknown — {e}"),
    }
    println!(
        "mode:     {}",
        if opts.read_only {
            "read-only"
        } else {
            "writable"
        }
    );
    // The identity probe: a request shaped like the proxy's — loopback
    // peer, tailnet Host, a login header — must resolve to
    // `<login> (tailscale)`.
    if read_pid(state_dir).is_some() {
        let ui_port = opts.port.unwrap_or(3010);
        let login = std::env::var("USER").unwrap_or_else(|_| "operator".to_string());
        let host_hdr = format!("{}:{}", ts.dns_name, ts.https_port);
        let login_hdr = format!("Tailscale-User-Login: {login}");
        match http_get(
            "127.0.0.1",
            ui_port,
            "/api/meta",
            &host_hdr,
            &[login_hdr.as_str()],
        ) {
            Ok((200, body)) => {
                let actor = serde_json::from_str::<Value>(&body)
                    .ok()
                    .and_then(|v| v["actor"].as_str().map(str::to_string))
                    .unwrap_or_default();
                println!("identity: {actor}  (test request)");
            }
            Ok((code, _)) => println!("identity: probe answered http {code}"),
            Err(e) => println!("identity: probe failed — {e}"),
        }
    } else {
        println!("identity: board not running — probe skipped");
    }
    match qr_term(&url) {
        Some(qr) => print!("{qr}"),
        None => println!("(qr encode failed — the URL above still works)"),
    }
    Ok(0)
}

/// Terminal QR: the `qrcode` crate (pure Rust, no other deps) plus a
/// two-rows-per-cell `▀` renderer using ANSI truecolor — dark modules
/// always black on white with a two-module quiet zone, scannable from
/// dark and light terminal themes alike.
fn qr_term(text: &str) -> Option<String> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let w = code.width();
    let modules = code.to_colors();
    const QUIET: usize = 2;
    let light = |x: usize, y: usize| -> bool {
        if x < QUIET || y < QUIET || x >= QUIET + w || y >= QUIET + w {
            return true;
        }
        matches!(modules[(y - QUIET) * w + (x - QUIET)], qrcode::Color::Light)
    };
    let mut out = String::new();
    let mut y = 0;
    while y < QUIET * 2 + w {
        let mut line = String::new();
        let (mut fg, mut bg) = (true, true);
        line.push_str("\x1b[38;2;255;255;255m\x1b[48;2;255;255;255m");
        for x in 0..QUIET * 2 + w {
            let (t, b) = (light(x, y), light(x, y + 1));
            if (t, b) != (fg, bg) {
                let (fv, bv) = (if t { 255 } else { 0 }, if b { 255 } else { 0 });
                line.push_str(&format!(
                    "\x1b[38;2;{fv};{fv};{fv}m\x1b[48;2;{bv};{bv};{bv}m"
                ));
                fg = t;
                bg = b;
            }
            line.push('▀');
        }
        line.push_str("\x1b[0m");
        out.push_str(&line);
        out.push('\n');
        y += 2;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{content_type, health_supports_model_defaults, running_json, static_file};
    use serde_json::json;

    /// Brand files sit at the dist root (Vite copies `ui/public/`); they
    /// must come back as themselves with an image type, never as the
    /// SPA's HTML fallback.
    #[test]
    fn dist_root_brand_files_are_served_with_image_types() {
        let dist = tempfile::TempDir::new().unwrap();
        std::fs::write(dist.path().join("favicon.svg"), "<svg/>").unwrap();
        std::fs::write(
            dist.path().join("apple-touch-icon.png"),
            [0x89, b'P', b'N', b'G'],
        )
        .unwrap();
        let (name, bytes) = static_file(Some(dist.path()), "/favicon.svg").unwrap();
        assert_eq!(bytes, b"<svg/>");
        assert_eq!(content_type(&name), "image/svg+xml");
        let (name, _) = static_file(Some(dist.path()), "/apple-touch-icon.png").unwrap();
        assert_eq!(content_type(&name), "image/png");
        assert!(static_file(Some(dist.path()), "/../favicon.svg").is_none());
    }

    #[test]
    fn current_message_summary_is_bounded_and_redacted() {
        let message = running_json(&json!({
            "id": "m1",
            "body": "investigate this issue password=super-secret and keep the useful context visible"
        }));
        let summary = message["summary"].as_str().unwrap();
        assert!(summary.contains("investigate this issue"));
        assert!(!summary.contains("super-secret"));
        assert!(summary.chars().count() <= 181);
    }

    #[test]
    fn model_defaults_capability_distinguishes_an_older_daemon() {
        assert!(health_supports_model_defaults(&json!({
            "capabilities": ["agent_registry", "model_defaults"]
        })));
        assert!(!health_supports_model_defaults(&json!({
            "capabilities": ["agent_registry"]
        })));
        assert!(!health_supports_model_defaults(&json!({})));
    }

    /// The embedded build answers the same three paths.
    #[cfg(feature = "ui")]
    #[test]
    fn embedded_brand_files_are_served() {
        for path in ["/favicon.svg", "/icon.svg", "/apple-touch-icon.png"] {
            let (name, bytes) = static_file(None, path).unwrap();
            assert_eq!(name, path);
            assert!(!bytes.is_empty());
        }
        let (_, svg) = static_file(None, "/favicon.svg").unwrap();
        assert!(svg.starts_with(b"<svg"));
    }
}
