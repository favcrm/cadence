//! `cadence ui` — the board: a small synchronous HTTP server
//! (tiny_http, no async runtime — the daemon is plain threads too)
//! serving the built SPA plus a JSON API on loopback.
//!
//! Reads need no auth; operator authority is a session (CAD-313, ADR
//! 0004): a `cadence ui login` link exchanged for an HttpOnly cookie plus
//! a page-held `X-Cadence-Session` key, both of which the daemon checks
//! on every operator write — [`operator`] says exactly who
//! is trusted as the operator, and no relay, header or missing pane tie
//! ever is. The rest is containment: loopback bind, no CORS headers, a
//! Host allowlist against DNS rebinding, id grammar checked before any
//! path is touched, and no file reads outside the PM dir or `--dist`.
//! Writes are I2: POST/PATCH/DELETE routes must pass four cross-site
//! guards (known write route, exact JSON/octet-stream content type,
//! `X-Cadence-Board: 1`, same-origin Origin/Sec-Fetch-Site) before any
//! work is done, then the caller rule, then the writer the route
//! supports. Memory curation is deliberately refused here: an HTTP
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

mod app_contexts;
mod app_release;
mod app_runs;
mod apps;
mod connections;
pub mod delivery_sync;
mod home;
mod lane;
mod login;
mod operator;
mod platform_account;
mod read_model;
mod stages;
mod threads;
mod updates;
mod wiki;
mod workflows;

pub use operator::{route_class, RouteClass, WriteRoute, WRITE_ROUTES};

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
    /// Hosted board: accept protected traffic only on the configured
    /// AgenticOS public Host. `CADENCE_BOARD_PUBLIC_ONLY=1` also enables
    /// this during `cadence setup`; the choice persists in ui.json.
    #[arg(long)]
    pub board_public_only: bool,
    /// Publish the board on the tailnet through `tailscale serve`
    /// (https port default: 9450). Ensures the serve mapping, adds the
    /// tailnet name to the Host and Origin allowlists, and attributes
    /// writes to the Tailscale user. Never funnel.
    #[arg(long, num_args = 0..=1, default_missing_value = "9450",
           value_name = "HTTPS_PORT")]
    pub tailscale: Option<u16>,
    /// CAD-526: serve this company's board on its AgenticOS public name
    /// — `acme.cadencecloud.app`, `acme.board.localhost:port` locally —
    /// where sign-in is the platform's identity-assertion contract and
    /// the local login link does not apply. The assertion's `aud` must
    /// equal exactly this host (hostname plus port when present).
    /// Env `AGENTICOS_BOARD_HOST`.
    #[arg(long)]
    pub board_host: Option<String>,
    /// CAD-526: the AgenticOS API origin that signs this board's
    /// assertions — the JWS `iss`, and where the platform JWKS is
    /// fetched from (`{iss}/.well-known/agenticos-board-jwks.json`).
    /// Env `AGENTICOS_BOARD_ISSUER`.
    #[arg(long)]
    pub board_issuer: Option<String>,
    /// CAD-526: the platform workspace/company id this instance serves —
    /// assertions naming another company are refused, not routed.
    /// Env `AGENTICOS_BOARD_COMPANY`.
    #[arg(long)]
    pub board_company: Option<String>,
    /// CAD-526: where an absent or expired board session redirects —
    /// `{app}/v2/board/authorize` on the app origin.
    /// Env `AGENTICOS_BOARD_AUTHORIZE_URL`; defaults to
    /// `{issuer}/v2/board/authorize`.
    #[arg(long)]
    pub board_authorize_url: Option<String>,
    /// CAD-777: allow remote operator sign-in through the AgenticOS
    /// device grant — the issuer origin that mints the grant.
    /// Requires `--device-login-org` (and env `CADENCE_DEVICE_LOGIN_ORG`
    /// as fallback); the pair persists in ui.json. Default: off — the
    /// device routes answer 404 unless both resolve.
    #[arg(long)]
    pub device_login_issuer: Option<String>,
    /// CAD-777: the exact workspace the device sign-in is for.
    /// Requires `--device-login-issuer` (env
    /// `CADENCE_DEVICE_LOGIN_ISSUER` as fallback).
    #[arg(long)]
    pub device_login_org: Option<String>,
    /// CAD-777: an issuer subject allowed to sign in — repeatable,
    /// once per operator (`cadence auth status` prints yours under
    /// `principal.subject_id`). Required with the issuer/org pair;
    /// env `CADENCE_DEVICE_LOGIN_SUBJECTS` (comma-separated) is the
    /// fallback.
    #[arg(long)]
    pub device_login_subject: Vec<String>,
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
    /// Print a single-use sign-in link for the board (CAD-313). Board
    /// writes need the operator's session; this link opens one. Run it
    /// from your own shell — agents are refused. The link is valid for
    /// 2 minutes and one browser; the secret it proves never leaves the
    /// state dir.
    Login {
        /// A link for the tailnet URL (`ui tailscale start`) instead of
        /// this board's own `http://cadence-<port>.localhost:<port>`.
        #[arg(long)]
        tailnet: bool,
        /// Replace the operator secret and revoke every session and
        /// unused link first.
        #[arg(long)]
        rotate: bool,
        /// The board's port [default: the persisted `ui start` port, else 3010].
        #[arg(long)]
        port: Option<u16>,
        /// Print `{link, origin, expires_in}` as JSON.
        #[arg(long)]
        json: bool,
    },
    /// List the board's operator sessions, or revoke them (CAD-313).
    Sessions {
        /// Revoke the session with this display id.
        #[arg(long)]
        revoke: Option<String>,
        /// Revoke every session and every unused link.
        #[arg(long)]
        revoke_all: bool,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
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
    match action {
        UiAction::Run { flags } => run(state_dir, flags),
        UiAction::Start { flags, reset } => start(state_dir, flags, *reset),
        UiAction::Stop { tailscale_off } => stop(state_dir, *tailscale_off),
        UiAction::Status => status(state_dir),
        UiAction::Login {
            tailnet,
            rotate,
            port,
            json,
        } => login::login(state_dir, *tailnet, *rotate, *port, *json),
        UiAction::Sessions {
            revoke,
            revoke_all,
            json,
        } => login::sessions(state_dir, revoke.as_deref(), *revoke_all, *json),
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

/// CAD-526: this board's AgenticOS public identity as `ui start`
/// recorded it — all four fields together or none.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(deny_unknown_fields)]
pub struct PublicBoard {
    /// The board's public host — `aud` must equal exactly this
    /// (hostname plus port when present, e.g. `acme.board.localhost:3123`
    /// locally or `acme.cadencecloud.app` in production).
    pub host: String,
    /// The platform issuer — the JWS `iss`, and the origin the JWKS is
    /// fetched from (`{iss}/.well-known/agenticos-board-jwks.json`).
    pub issuer: String,
    /// The company/workspace id this instance serves — `company` must
    /// equal it; one Cadence serves one company (contract §9).
    pub company: String,
    /// Where an absent or expired browser session redirects:
    /// `{app}/v2/board/authorize` on the app origin.
    pub authorize_url: String,
}

/// Remote operator sign-in through the AgenticOS device grant
/// (CAD-777): the issuer origin, the exact workspace and the subject
/// allowlist. All or none — a partial triple never resolves.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeviceLoginOpts {
    pub issuer: String,
    pub org: String,
    /// The verified issuer subjects allowed a board session.
    #[serde(default)]
    pub subjects: Vec<String>,
}

/// A device grant the board is waiting on: the server-side code plus
/// its expiry. The browser holds only the pending id. Opaque outside
/// this module tree: fields stay private, so only `ui` and its
/// children construct or read rows.
#[doc(hidden)]
#[derive(Clone)]
pub struct DevicePending {
    device_code: String,
    expires_at: i64,
}

/// The resolved device-login configuration (CAD-777): the validated
/// issuer + workspace pair, the operator's subject allowlist, and the
/// live pending map. `None` in `ServeOpts` is off.
#[derive(Clone)]
pub struct DeviceLogin {
    pub config: crate::device_login::DeviceConfig,
    /// The allowlist `serve` writes into the daemon's pin — the only
    /// subjects a verified grant may mint for.
    pub subjects: Vec<String>,
    pub pending: std::sync::Arc<std::sync::Mutex<HashMap<String, DevicePending>>>,
    /// The issuer transport — live ureq unless a test injects a fake.
    pub transport: std::sync::Arc<dyn crate::device_login::IssuerTransport>,
}

/// Live device grants awaiting approval are bounded: past this many,
/// `/api/session/device/code` refuses with 429 until one settles.
const DEVICE_PENDING_CAP: usize = 16;

impl DeviceLogin {
    /// A live configuration: validated issuer pair + subject allowlist,
    /// empty pending map, live issuer transport. Tests point `config`
    /// at a loopback stub; production uses an HTTPS issuer origin.
    pub fn with_issuer(config: crate::device_login::DeviceConfig, subjects: Vec<String>) -> Self {
        Self {
            config,
            subjects,
            pending: Default::default(),
            transport: std::sync::Arc::new(crate::device_login::UreqTransport::new()),
        }
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
    pub board_public_only: bool,
    pub tailscale: Option<TailscaleOpts>,
    /// The AgenticOS board-identity configuration (CAD-526).
    pub board: Option<PublicBoard>,
    /// Remote operator sign-in through the AgenticOS device grant
    /// (CAD-777): issuer + workspace, or neither. Default off.
    pub device_login: Option<DeviceLoginOpts>,
}

/// Everything the running server needs, resolved.
#[derive(Clone, Default)]
pub struct ServeOpts {
    /// This board process's boot-pinned daemon agent UID. `serve` obtains
    /// it once from the private daemon socket; tests can inject a pin.
    pub agent_uid: Option<u32>,
    pub host: String,
    pub port: u16,
    pub dist: Option<PathBuf>,
    /// Operator `--allow-host` plus the tailnet-derived names.
    pub allow_hosts: Vec<String>,
    /// Operator `--allow-origin` plus the tailnet https origin.
    pub allow_origins: Vec<String>,
    pub read_only: bool,
    pub board_public_only: bool,
    /// Tailscale sharing armed: `(dns_name, https_port)` — the trust
    /// rule for `Tailscale-User-*` headers and the `/api/meta` URL.
    pub tailnet: Option<(String, u16)>,
    /// tailscaled's LocalAPI socket the tailnet proof reads
    /// ([`crate::tailnet_proof`]); `None` is tailscaled's default path.
    /// Never set from the command line — tests inject a fixture.
    pub tailscaled_socket: Option<PathBuf>,
    /// This board process's operator-user latch. [`serve`] always
    /// replaces it with a fresh startup read; the default is latched.
    pub tailnet_latch: crate::tailnet_proof::OperatorLatch,
    /// The `gh` the board's Merge runs (CAD-431) — the operator's own,
    /// `gh` on PATH when `None`. Never set from the command line —
    /// tests inject a fake.
    pub gh: Option<PathBuf>,
    /// The board's delivery-sync period (CAD-446), [`delivery_sync::EVERY`]
    /// when `None`, clamped to its bounds. Never set from the command
    /// line — tests shorten it.
    pub delivery_sync_every: Option<Duration>,
    /// This board process's delivery sync. [`serve`] always replaces it
    /// (with `None` on a read-only board); never a caller's.
    pub delivery_sync: Option<std::sync::Arc<delivery_sync::DeliverySync>>,
    /// The in-process owner's stop (CAD-471): once set, [`serve`] stops
    /// accepting and returns, closing its port. Never set from the
    /// command line — a test's board on a thread of the runner stops
    /// with its test instead of serving for the rest of the run.
    pub stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// An in-process owner's startup notification: success follows binding
    /// and initialization; a bind failure carries its I/O kind. Never set
    /// from the command line. Other startup failures disconnect the channel.
    pub startup: Option<std::sync::mpsc::Sender<std::result::Result<(), std::io::ErrorKind>>>,
    /// CAD-526: this board's public AgenticOS name, when configured.
    /// Requests that carry its Host are the platform sign-in surface —
    /// `__platform/*` routes and `__Host-aos-board-session` reads —
    /// never the local login flow.
    pub public: Option<PublicBoard>,
    /// CAD-777: remote operator sign-in through the AgenticOS device
    /// grant, when the issuer + workspace pair resolved. `None` is
    /// off — the device routes answer 404. Never set from the command
    /// line — tests inject a fake issuer transport beside it.
    pub device_login: Option<DeviceLogin>,
    /// CAD-482: arm the test-only caller seam — the board honors
    /// `X-Cadence-Test-As`/`X-Cadence-Test-Token` request headers and
    /// its daemon calls carry the asserted identity. Honored only in
    /// `test-seam` builds and only against a seam-armed fixture
    /// daemon; otherwise [`serve`] refuses to start.
    pub test_seam: bool,
    /// The credential [`serve`] resolved for `test_seam` — callers
    /// never set this.
    #[doc(hidden)]
    pub seam: Option<crate::test_seam::Seam>,
    /// The program the Update button spawns (CAD-561 r2): `cadence
    /// update --as <ui actor> --progress <state>/update-progress.jsonl`.
    /// `None` is this board's own binary. Never set from the command
    /// line — tests inject a fake helper.
    pub update_helper: Option<PathBuf>,
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
    let env_public_only = match std::env::var("CADENCE_BOARD_PUBLIC_ONLY") {
        Ok(v) if v == "1" => true,
        Ok(v) if v == "0" => false,
        Ok(_) => return Err(Error::rejected("CADENCE_BOARD_PUBLIC_ONLY must be 0 or 1")),
        Err(std::env::VarError::NotPresent) => false,
        Err(_) => {
            return Err(Error::rejected(
                "CADENCE_BOARD_PUBLIC_ONLY is not valid UTF-8",
            ))
        }
    };
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
        board_public_only: flags.board_public_only
            || persisted.board_public_only
            || env_public_only,
        tailscale: persisted.tailscale.clone(),
        // CAD-526: each field resolves flag → env → persisted; the block
        // is all-or-nothing — any field present with another missing is
        // an operator error, never a partial trust root.
        board: resolve_board(flags, persisted)?,
        // CAD-777: same shape — flag → env → persisted, issuer + org +
        // subjects or none. Validation (fail closed at boot) happens in
        // `serve_opts` so `ui status` can show the raw triple.
        device_login: resolve_device_login(flags, persisted)?,
    };
    if eff.board_public_only && eff.board.is_none() {
        return Err(Error::rejected(
            "board public-only mode requires a public board identity",
        ));
    }
    if let Some(https_port) = flags.tailscale {
        crate::sandbox::refuse_global("`ui start --tailscale`")?;
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

/// CAD-777: merge the device-login triple — flags win, then
/// `CADENCE_DEVICE_LOGIN_*` env, then the persisted block. Issuer +
/// org + at least one subject, or none of it; a partial combination
/// is an operator error naming the missing piece, never a silent
/// half trust root. Values are validated when `serve_opts` builds
/// the runtime config, so a bad triple fails the board at boot.
fn resolve_device_login(flags: &UiFlags, persisted: &UiOpts) -> Result<Option<DeviceLoginOpts>> {
    let field = |flag: Option<&String>, env: &str, saved: Option<&String>| {
        flag.cloned()
            .or_else(|| std::env::var(env).ok())
            .or_else(|| saved.cloned())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let saved = persisted.device_login.as_ref();
    let issuer = field(
        flags.device_login_issuer.as_ref(),
        "CADENCE_DEVICE_LOGIN_ISSUER",
        saved.map(|d| &d.issuer),
    );
    let org = field(
        flags.device_login_org.as_ref(),
        "CADENCE_DEVICE_LOGIN_ORG",
        saved.map(|d| &d.org),
    );
    // Subjects resolve as a list: any flag beats the env list, which
    // beats the saved one. Env is comma-separated, trimmed, empties
    // dropped.
    let flag_subjects: Vec<String> = flags
        .device_login_subject
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let subjects: Vec<String> = if !flag_subjects.is_empty() {
        flag_subjects
    } else if let Ok(list) = std::env::var("CADENCE_DEVICE_LOGIN_SUBJECTS") {
        list.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    } else {
        saved.map(|d| d.subjects.clone()).unwrap_or_default()
    };
    match (issuer, org, subjects.is_empty()) {
        (None, None, true) => Ok(None),
        (Some(issuer), Some(org), false) => Ok(Some(DeviceLoginOpts {
            issuer,
            org,
            subjects,
        })),
        (issuer, org, no_subjects) => {
            let mut missing = Vec::new();
            if issuer.is_none() {
                missing.push("--device-login-issuer (CADENCE_DEVICE_LOGIN_ISSUER)");
            }
            if org.is_none() {
                missing.push("--device-login-org (CADENCE_DEVICE_LOGIN_ORG)");
            }
            if no_subjects {
                missing.push("--device-login-subject (CADENCE_DEVICE_LOGIN_SUBJECTS)");
            }
            Err(Error::rejected(format!(
                "device login needs issuer, org and at least one subject together — missing {}",
                missing.join(" and ")
            )))
        }
    }
}

/// CAD-526: merge the board-identity configuration — flags win, then
/// `AGENTICOS_BOARD_*` env (how the hosted container is told), then the
/// persisted block. All of host/issuer/company must resolve together.
/// When a block resolves, its trust root (`host`, `issuer`, `company`)
/// is written to the daemon-owned `operator/board-identity.json` so the
/// board RPC has the same root the session check enforces.
fn resolve_board(flags: &UiFlags, persisted: &UiOpts) -> Result<Option<PublicBoard>> {
    let field = |flag: Option<&String>, env: &str, saved: Option<&String>| {
        flag.cloned()
            .or_else(|| std::env::var(env).ok())
            .or_else(|| saved.cloned())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let saved = persisted.board.as_ref();
    let host = field(
        flags.board_host.as_ref(),
        "AGENTICOS_BOARD_HOST",
        saved.map(|b| &b.host),
    );
    let issuer = field(
        flags.board_issuer.as_ref(),
        "AGENTICOS_BOARD_ISSUER",
        saved.map(|b| &b.issuer),
    );
    let company = field(
        flags.board_company.as_ref(),
        "AGENTICOS_BOARD_COMPANY",
        saved.map(|b| &b.company),
    );
    let authorize_url = field(
        flags.board_authorize_url.as_ref(),
        "AGENTICOS_BOARD_AUTHORIZE_URL",
        saved.map(|b| &b.authorize_url),
    );
    if host.is_none() && issuer.is_none() && company.is_none() && authorize_url.is_none() {
        return Ok(None);
    }
    let missing = |name: &str, env: &str| -> Error {
        Error::rejected(format!(
            "board sign-in needs {name} — pass `--board-{name}` or set {env} \
             (host, issuer and company must all resolve together)"
        ))
    };
    let host = host.ok_or_else(|| missing("host", "AGENTICOS_BOARD_HOST"))?;
    let issuer = issuer.ok_or_else(|| missing("issuer", "AGENTICOS_BOARD_ISSUER"))?;
    let company = company.ok_or_else(|| missing("company", "AGENTICOS_BOARD_COMPANY"))?;
    if !crate::board_identity::valid_aud(&host) {
        return Err(Error::rejected(format!(
            "invalid board host '{host}' — expected `slug.board-domain` or \
             `slug.board.localhost:port` (lowercase host, optional port)"
        )));
    }
    if !(issuer.starts_with("https://") || issuer.starts_with("http://")) {
        return Err(Error::rejected(format!(
            "invalid board issuer '{issuer}' — the platform origin, e.g. \
             https://api.agenticos.com or http://localhost:8810"
        )));
    }
    let authorize_url = authorize_url
        .unwrap_or_else(|| format!("{}/v2/board/authorize", issuer.trim_end_matches('/')));
    if !(authorize_url.starts_with("https://") || authorize_url.starts_with("http://")) {
        return Err(Error::rejected(
            "invalid board authorize URL — an absolute https:// (or local http://) address",
        ));
    }
    Ok(Some(PublicBoard {
        host,
        issuer,
        company,
        authorize_url,
    }))
}

/// Build the runtime view of effective options: tailnet- and
/// public-board-derived Host/Origin entries unioned in (deduped,
/// case-insensitive) and the loopback rule enforced.
fn serve_opts(eff: &UiOpts) -> Result<ServeOpts> {
    let host = eff.host.clone().unwrap_or_else(|| "127.0.0.1".to_string());
    let port = eff.port.unwrap_or(3010);
    // A sandbox board never takes production's port (CAD-310).
    crate::sandbox::refuse_production_port(port)?;
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
    // CAD-526: the public board name (and its write origin) enter the
    // allowlists at resolve time, like the tailnet ones — never a
    // caller-supplied header.
    if let Some(public) = &eff.board {
        if !allow_hosts
            .iter()
            .any(|x| x.eq_ignore_ascii_case(&public.host))
        {
            allow_hosts.push(public.host.clone());
        }
        let origin = format!(
            "{}://{}",
            operator::public_scheme(&public.host),
            public.host
        );
        if !allow_origins
            .iter()
            .any(|x| x.eq_ignore_ascii_case(&origin))
        {
            allow_origins.push(origin);
        }
    }
    Ok(ServeOpts {
        agent_uid: None,
        host,
        port,
        dist: eff.dist.clone(),
        allow_hosts,
        allow_origins,
        read_only: eff.read_only,
        board_public_only: eff.board_public_only,
        tailnet,
        tailscaled_socket: None,
        tailnet_latch: Default::default(),
        gh: None,
        delivery_sync_every: None,
        delivery_sync: None,
        stop: None,
        startup: None,
        public: eff.board.clone(),
        // CAD-777: the triple validates here so a bad issuer/org or a
        // malformed allowlist fails the board at boot, never at first
        // sign-in. The pending map starts empty; tests replace the
        // whole `DeviceLogin`.
        device_login: eff
            .device_login
            .as_ref()
            .map(|pair| -> Result<DeviceLogin> {
                crate::device_login::validate_subjects(&pair.subjects)?;
                Ok(DeviceLogin {
                    config: crate::device_login::DeviceConfig::new(&pair.issuer, &pair.org)?,
                    subjects: pair.subjects.clone(),
                    pending: Default::default(),
                    transport: std::sync::Arc::new(crate::device_login::UreqTransport::new()),
                })
            })
            .transpose()?,
        // CAD-482: `ui run`/`ui start`'s fixture child arms from its
        // environment; in-process fixtures set the field directly.
        test_seam: crate::test_seam::env_armed(),
        seam: None,
        // CAD-561 r2: a real board spawns its own binary as the update
        // helper; only tests inject a fake.
        update_helper: None,
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
        || host == operator::board_host(port)
        || host == format!("127.0.0.1:{port}")
        || host == format!("localhost:{port}")
        || host == format!("[::1]:{port}")
        || extra.iter().any(|h| h.eq_ignore_ascii_case(&host))
}

fn json_response(value: Value) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::to_vec_pretty(&value).unwrap_or_default();
    use sha2::{Digest, Sha256};
    let etag = format!("\"{:x}\"", Sha256::digest(&body));
    let mut resp = Response::from_data(body).with_status_code(StatusCode(200));
    resp.add_header(Header::from_bytes("ETag", etag).unwrap());
    resp.add_header(Header::from_bytes("Cache-Control", "private, no-cache").unwrap());
    resp.add_header(Header::from_bytes("Vary", "Cookie, X-Cadence-Session").unwrap());
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
/// agent-written Markdown. Social source previews use only these reviewed
/// image CDNs; the client checks the same host families before rendering.
const CSP: &str = "default-src 'self'; img-src 'self' data: https://cdninstagram.com https://*.cdninstagram.com https://fbcdn.net https://*.fbcdn.net; font-src 'self' data:; style-src 'self' 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'";

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
            "role" if role.is_none() => role = Some(context::canonical_role(&value).to_string()),
            "expected_revision" if expected_revision.is_none() => expected_revision = Some(value),
            "role" | "expected_revision" => return Err("duplicate context query key"),
            _ => return Err("unknown context query key"),
        }
    }
    if let Some(value) = role.as_deref() {
        if !context::valid_role(value) {
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

/// What a non-API GET answers with.
#[derive(Debug)]
enum StaticAnswer {
    /// A file of the build — or the SPA shell (`index.html`) for a client route.
    File(String, Vec<u8>),
    /// A path that names a file the build does not have.
    Missing,
    /// No build to serve at all.
    NoBuild,
}

/// Extensions the build serves as files. A missing one is a 404, never
/// the SPA shell — with `nosniff`, the browser still must not be handed
/// HTML under a script, stylesheet, or image name. Wiki pages (`.md`)
/// and dotted ids (`CAD-1.2`) are not in this set.
fn is_static_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "html"
            | "js"
            | "mjs"
            | "css"
            | "svg"
            | "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "ico"
            | "woff"
            | "woff2"
            | "ttf"
            | "map"
            | "json"
            | "webmanifest"
    )
}

/// Wiki addresses (`/wiki` and `/wiki/<path>`) name pages and blobs in
/// the store, including `note.md` and `photo.png`. They are never build
/// files. File bytes stay on `/api/wiki/file` only.
fn is_wiki_path(path: &str) -> bool {
    path == "/wiki" || path.starts_with("/wiki/")
}

/// Whether a path is a client-side route (ui/src/lib/router.ts) that the
/// SPA shell answers so deep links and refreshes work. `/api` and
/// `/assets/` never are. A missing build asset (a static extension,
/// outside the wiki) is a 404. Anything else — including a wiki page and
/// a dotted issue id — is a route. One rule for every screen, not a
/// branch per path.
fn is_client_route(path: &str) -> bool {
    if path == "/api" || path.starts_with("/api/") || path.starts_with("/assets/") {
        return false;
    }
    if is_wiki_path(path) {
        return true;
    }
    !matches!(
        path.rsplit('/').next().unwrap_or_default().rsplit_once('.'),
        Some((_, ext)) if is_static_ext(ext)
    )
}

/// `Accept: application/json` asks for the JSON 404, not the shell.
/// A browser navigation sends `text/html` (and `*/*`), which does not
/// count — `*/*` would hide every refresh.
fn prefers_json(accept: Option<&str>) -> bool {
    let Some(accept) = accept else {
        return false;
    };
    accept.split(',').any(|part| {
        part.split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("application/json")
    })
}

/// A build file when one matches; else the SPA shell for a client route.
/// `accept` is the request's Accept header. A wiki GET that prefers JSON
/// stays a 404 so the file API remains `/api/wiki/file`.
fn static_answer_for(dist: Option<&Path>, path: &str, accept: Option<&str>) -> StaticAnswer {
    let target = if path == "/" { "/index.html" } else { path };
    if !path.starts_with("/api/") {
        if let Some((name, bytes)) = static_file(dist, target) {
            return StaticAnswer::File(name, bytes);
        }
    }
    if !is_client_route(path) || (is_wiki_path(path) && prefers_json(accept)) {
        return StaticAnswer::Missing;
    }
    match static_file(dist, "/index.html") {
        Some((name, bytes)) => StaticAnswer::File(name, bytes),
        None => StaticAnswer::NoBuild,
    }
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

/// `job_list` as the board reads it: every job, each row carrying its
/// tasks (`task_list`, CAD-325) so bindings need no per-job `job_show`.
fn board_job_list(state_dir: &Path) -> Option<Value> {
    client::rpc(
        state_dir,
        "job_list",
        json!({"all": true, "tasks_detail": true}),
    )
    .ok()
}

/// task id → issue/job context for every task that belongs to an
/// issue-bound job.
fn task_issue_map(state_dir: &Path) -> HashMap<String, TaskBinding> {
    match board_job_list(state_dir) {
        Some(list) => task_issue_map_from(state_dir, &list),
        None => HashMap::new(),
    }
}

/// [`task_issue_map`] over a `job_list` already in hand. A row without
/// `task_list` comes from a daemon older than CAD-325 and falls back to
/// that job's `job_show` — the old per-job fan-out, which cost one RPC
/// per issue-bound job ever created on every board read.
fn task_issue_map_from(state_dir: &Path, list: &Value) -> HashMap<String, TaskBinding> {
    let mut map = HashMap::new();
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
        let show = if job["task_list"].is_array() {
            json!({"job": {"title": job["title"], "state": job["state"],
                           "tasks": job["task_list"]}})
        } else {
            let Ok(show) = client::rpc(state_dir, "job_show", json!({"job": job_id})) else {
                continue;
            };
            show
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

/// One running message reduced for the board: id, task and a bounded
/// redacted description for ad-hoc current work. Never the turn token:
/// it is `message_report`'s credential and the board serves any local
/// HTTP caller (CAD-375; the daemon withholds it from the board's own
/// connection too).
fn running_json(m: &Value) -> Value {
    json!({
        "id": m["id"],
        "task": m["task_id"],
        "created": m["created"],
        "summary": current_message_summary(m),
    })
}

fn agent_activity_seconds(value: &Value) -> Option<f64> {
    let seconds = if let Some(seconds) = value.as_f64() {
        seconds
    } else {
        let source = value.as_str()?;
        // Reuse the issue parser for calendar validation, then account for
        // fractional seconds and an optional RFC 3339 timezone offset.
        let base = source.get(..19)?;
        let mut suffix = source.get(19..)?;
        let epoch = crate::issue::time::parse_iso(&format!("{base}Z"))? as f64;
        let fraction = if let Some(rest) = suffix.strip_prefix('.') {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 {
                return None;
            }
            let (part, zone) = rest.split_at(digits);
            suffix = zone;
            format!("0.{part}").parse::<f64>().ok()?
        } else {
            0.0
        };
        let offset = if suffix == "Z" {
            0
        } else {
            let bytes = suffix.as_bytes();
            if bytes.len() != 6 || !matches!(bytes[0], b'+' | b'-') || bytes[3] != b':' {
                return None;
            }
            let hours = suffix.get(1..3)?.parse::<i64>().ok()?;
            let minutes = suffix.get(4..6)?.parse::<i64>().ok()?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            let direction = if bytes[0] == b'+' { 1 } else { -1 };
            direction * (hours * 3600 + minutes * 60)
        };
        epoch + fraction - offset as f64
    };
    (seconds.is_finite() && seconds > 0.0 && seconds <= 8.64e12).then_some(seconds)
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
/// the board still renders. Built from an `agent_list` asked with
/// `board: true` and a [`board_job_list`] — the read model fetches both
/// once and shares the job list with the cards' job outcomes.
fn agents_payload_from(state_dir: &Path, list: Option<Value>, jobs: Option<&Value>) -> Value {
    let Some(list) = list else {
        return json!({"daemon": "unreachable", "agents": [], "totals": null, "by_issue": {}});
    };
    let task_map = jobs
        .map(|jobs| task_issue_map_from(state_dir, jobs))
        .unwrap_or_default();
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
            // CAD-480: the mailbox's unread backlog and its oldest
            // unread age are the row's queue evidence — the daemon
            // computes both in `agent.inbox`.
            let unread = agent["inbox"]["queued"].as_i64().unwrap_or(0);
            let oldest_unread_age_secs = agent["inbox"]["oldest_age_secs"].clone();
            totals["queued"] = json!(totals["queued"].as_i64().unwrap_or(0) + unread);
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
                "running": 0, "queued": unread, "unread": unread,
                "oldest_unread_age_secs": oldest_unread_age_secs,
                "unknown": 0, "parked": 0,
                "fenced": false, "on": [], "tasks": [], "message": Value::Null,
                "dead": agent["dead"], "inbox": true,
            }));
            continue;
        }
        // A CAD-325 daemon folds the show slice into the row (`board`);
        // an older one answers it per agent.
        let show = match &agent["board"] {
            board if board.is_object() => Ok(board.clone()),
            _ => client::rpc(state_dir, "agent_show", json!({"alias": alias})),
        };
        let (mut running, mut parked) = (0i64, 0i64);
        let mut running_msgs: Vec<Value> = Vec::new();
        let mut last_activity = Value::Null;
        let mut latest_activity = 0.0;
        let (queued, unknown, cursor) = match &show {
            Ok(show) => {
                for m in show["messages"].as_array().cloned().unwrap_or_default() {
                    for ts in ["completed", "started", "created"] {
                        let at = &m[ts];
                        if let Some(seconds) = agent_activity_seconds(at) {
                            if seconds > latest_activity {
                                latest_activity = seconds;
                                last_activity = at.clone();
                            }
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
                if let Some(n) = show["parked"].as_i64() {
                    parked = n;
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
/// subjects. A pane's write commits as its alias
/// ([`operator::board_caller`]).
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
        format!("http://{}", operator::board_host(port)),
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
    // `multipart/form-data` is the one non-exact rule (CAD-580 wiki
    // upload): the boundary parameter must ride the type, so the check
    // is a bounded prefix — never a bare "simple" form type.
    let ct_ok = if want_ct == "multipart/form-data" {
        let v = ct.trim();
        v.starts_with("multipart/form-data; boundary=") && v.len() <= 200
    } else {
        ct.trim() == want_ct
    };
    if !ct_ok {
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
            let mut body = json!({"issue": detail, "card": card, "warnings": warnings});
            // CAD-447: an answer's delivery to the asker.
            if let Some(route) = out.get("route") {
                body["route"] = route.clone();
            }
            let body = serde_json::to_vec_pretty(&body).unwrap_or_default();
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
    let read = read_model::get(state_dir, &pm.dir).board(pm, None);
    let by_id: HashMap<String, &board::View> = read
        .views
        .iter()
        .map(|v| (v.issue.front.id.clone(), v))
        .collect();
    let view = by_id
        .get(id)
        .ok_or_else(|| Error::rejected(format!("unknown issue '{id}'")))?;
    let ctx = read.ctx(&by_id);
    Ok((
        read.card(&ctx, view),
        with_agents(
            crate::issue::work::detail_json(&pm.dir, &ctx, view),
            &read.by_issue,
            id,
        ),
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

/// The actor a request would write as, header-wise, and the tailnet
/// proof behind it — `/api/meta`'s `actor` and `tailnet_proof`. A
/// request [`tailnet_proxy`] proves came through `tailscale serve`
/// writes as its `Tailscale-User-Login` ([`proxied_actor`]) — or not at
/// all when it carries none; any other request as the plain operator,
/// and the header is never considered. `tailnet_proof` is `null` for a
/// request that is not tailnet-shaped, `{"proven": true, "login"}`, or
/// `{"proven": false, "check", "why"}` naming the check that refused.
fn request_identity(request: &Request, opts: &ServeOpts) -> (String, Value) {
    match tailnet_proxy(request, opts) {
        None => (UI_ACTOR.to_string(), Value::Null),
        Some(Ok(())) => {
            match proxied_actor(header_value(request, "Tailscale-User-Login").as_deref()) {
                Ok(actor) => (actor, json!({"proven": true, "login": true})),
                Err(_) => (
                    NO_TAILNET_LOGIN.to_string(),
                    json!({"proven": true, "login": false}),
                ),
            }
        }
        Some(Err(r)) => (
            UI_ACTOR.to_string(),
            json!({"proven": false, "check": r.check.as_str(), "why": r.why}),
        ),
    }
}

/// `/api/meta`'s actor for a proven proxy request without a login.
const NO_TAILNET_LOGIN: &str = "none (tailnet request without a login — writes refused)";

/// The actor of a request proven to come through the serve proxy:
/// `<login> (tailscale)` from its `Tailscale-User-Login`. A proven
/// request with no usable login — a Funnel client from the internet, a
/// tagged node — names nobody, so it is refused rather than written as
/// `operator (ui)`.
fn proxied_actor(login: Option<&str>) -> std::result::Result<String, String> {
    login
        .and_then(sanitize_actor)
        .map(|l| format!("{l} (tailscale)"))
        .ok_or_else(|| {
            "the tailscale proxy sent no usable Tailscale-User-Login — a Funnel or \
             tagged-node client names nobody to write as"
                .to_string()
        })
}

/// Is this request from the `tailscale serve` proxy (CAD-336)? `None`
/// — not tailnet-shaped at all (tailscale mode off, or the Host is not
/// the tailnet name). `Some(Ok)` — tailnet-shaped and proven by
/// [`crate::tailnet_proof::prove`]. `Some(Err)` — tailnet-shaped but
/// unproven: Host and loopback are caller-controlled, so the identity
/// headers are not trusted.
/// Does the request name the tailnet host — did it come (or claim to
/// come) through `tailscale serve`? Proof aside.
fn tailnet_host(request: &Request, opts: &ServeOpts) -> bool {
    let Some((dns, _)) = opts.tailnet.as_ref() else {
        return false;
    };
    let host = header_value(request, "Host").unwrap_or_default();
    let name = host.split(':').next().unwrap_or_default();
    name.eq_ignore_ascii_case(dns)
}

fn tailnet_proxy(
    request: &Request,
    opts: &ServeOpts,
) -> Option<std::result::Result<(), crate::tailnet_proof::Refusal>> {
    if !tailnet_host(request, opts) {
        return None;
    }
    Some(match request.remote_addr() {
        Some(peer) => crate::tailnet_proof::prove(
            opts.tailscaled_socket.as_deref(),
            &opts.tailnet_latch,
            opts.port,
            *peer,
        ),
        None => Err(crate::tailnet_proof::Refusal {
            check: crate::tailnet_proof::Check::ClientSocket,
            why: "the request has no peer address".to_string(),
        }),
    })
}

/// The live agents a write can be attributed to, read over the
/// daemon's `agent_list` RPC:
///
/// - registered panes, pane pid → alias — the same rows the daemon's
///   slot identity resolves against (`pty` endpoints with a pid and a
///   generation);
/// - managed endpoints (CAD-335), provider pid → alias — a claude or
///   codex `managed`/`managed-ws` endpoint with a pid: the provider
///   process the daemon launched, the same pid its build-slot
///   enrollment roots at. The daemon clears the pid when the endpoint
///   closes, stops or errors.
///
/// Each pid comes with the process start time the daemon recorded
/// with it (`pid_start`, CAD-385) and is classified against `/proc`
/// now ([`crate::peer::AgentPids`]): a reused pid attributes nothing,
/// and a row without one — an older daemon's list — refuses a write
/// tied to it.
///
/// No store file means no agent was ever registered here: provably no
/// agents. A store the daemon cannot answer for is an error.
fn agent_roots(state_dir: &Path) -> std::result::Result<crate::peer::AgentRoots, String> {
    if !state_dir.join("cadence.sqlite3").exists() {
        return Ok(crate::peer::AgentRoots::default());
    }
    let list = client::rpc(state_dir, "agent_list", json!({}))
        .map_err(|e| format!("the daemon cannot list registered agents ({e})"))?;
    let agents = list["agents"]
        .as_array()
        .ok_or_else(|| "the daemon's agent list is malformed".to_string())?;
    let mut panes = Vec::new();
    let mut managed = Vec::new();
    for a in agents {
        let (Some(pid), Some(alias)) = (
            a["pid"].as_u64().and_then(|p| u32::try_from(p).ok()),
            a["alias"].as_str(),
        ) else {
            continue;
        };
        let row = (alias.to_string(), pid, a["pid_start"].as_u64());
        let kind = a["endpoint_kind"].as_str().unwrap_or_default();
        if kind == "pty" && !a["generation"].is_null() {
            panes.push(row);
        } else if pid > 1
            && registry::enrolls_build_slots(a["provider"].as_str().unwrap_or_default(), kind)
        {
            managed.push(row);
        }
    }
    Ok(crate::peer::AgentRoots {
        panes: crate::peer::AgentPids::classify(panes),
        managed: crate::peer::AgentPids::classify(managed),
    })
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

/// `POST /api/settings/model-defaults` — operator-only, admitted by
/// `operator::admit` (session plus process proof) before this runs.
fn model_defaults_post(request: &mut Request, state_dir: &Path) -> HttpResp {
    if let Err(resp) = require_model_defaults(state_dir) {
        return resp;
    }
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
        json!({"document": document}),
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
    // CAD-313: signing in and out — the nonce, or the session itself,
    // is the credential (`operator`).
    if path == "/api/session" || path == "/api/session/logout" {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = if path == "/api/session" {
            operator::open(&mut request, state_dir, opts)
        } else {
            operator::logout(&request, state_dir, opts)
        };
        send(request, resp);
        return;
    }
    // CAD-777: device-grant sign-in — same login class as `/api/session`.
    // Unconfigured boards answer 404 inside the handlers, like an
    // unknown shape.
    if path == "/api/session/device/code" || path == "/api/session/device/poll" {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = if path == "/api/session/device/code" {
            operator::device_code(&mut request, state_dir, opts)
        } else {
            operator::device_poll(&mut request, state_dir, opts)
        };
        send(request, resp);
        return;
    }
    // Setup is detect only in the board: applying a fix is the
    // operator's command to run (CAD-327).
    if path == "/api/setup" {
        send(
            request,
            err_response(405, "setup is read-only here — GET only"),
        );
        return;
    }
    // CAD-313: every other write is admitted HERE by its class in
    // `operator::WRITE_ROUTES` (unlisted: operator-only) before any
    // handler runs; the handlers below check no caller themselves.
    let caller = match operator::admit(&request, method.as_str(), path, state_dir, opts) {
        Ok(caller) => caller,
        Err(resp) => {
            send(request, resp);
            return;
        }
    };
    // CAD-561: the operator's Update button — the same pipeline the CLI
    // runs, in this process; the card polls `GET /api/update`.
    if path == "/api/update" || path == "/api/update/check" {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = if path == "/api/update" {
            updates::start(state_dir, opts)
        } else {
            updates::check_now(state_dir)
        };
        send(request, resp);
        return;
    }
    if path == "/api/settings/model-defaults" {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = model_defaults_post(&mut request, state_dir);
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
        let Some(caller) = caller else {
            send(request, err_response(500, "unadmitted write"));
            return;
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
    // The operator's plan decision (CAD-328 → CAD-360 RPCs) and answer
    // to a question report (CAD-341) — guarded, operator-only, inside
    // `home`.
    if let Some(id) = home::idea_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = home::decide_idea(&mut request, state_dir, id);
        send(request, resp);
        return;
    }
    if let Some((epic, verb)) = home::plan_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = home::decide_plan(&mut request, state_dir, epic, verb);
        send(request, resp);
        return;
    }
    // CAD-431: the operator's merge decision — guarded, operator-only,
    // inside `home`.
    if let Some((id, verb)) = home::delivery_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = home::decide_delivery(&mut request, state_dir, opts, id, verb);
        send(request, resp);
        return;
    }
    // An epic stage move (CAD-432) — relayed to the daemon's
    // `epic_stage`, operator-only on the board (see `stages`).
    if let Some(epic) = stages::stage_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = stages::move_stage(&mut request, state_dir, epic);
        send(request, resp);
        return;
    }
    // A workflow run (CAD-496) — relayed to the daemon's
    // `plan_propose`, operator-only on the board (see `workflows`).
    if let Some((key, name)) = workflows::propose_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = workflows::propose(&mut request, state_dir, key, name);
        send(request, resp);
        return;
    }
    if let Some(route) = app_contexts::route(path) {
        let writable = matches!(route, app_contexts::Route::List(_)) || !route.is_read();
        if *method != Method::Post || !writable {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let response = app_contexts::handle(&mut request, state_dir, route, true);
        send(request, response);
        return;
    }
    if let Some(route) = connections::route(path) {
        let writable = matches!(route, connections::Route::List) || !route.is_read();
        if *method != Method::Post || !writable {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let response = connections::handle(&mut request, state_dir, route, true);
        send(request, response);
        return;
    }
    if let Some(route) = app_release::route(path) {
        if *method != Method::Post || !route.is_write() {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let response = app_release::handle(&mut request, state_dir, route, true);
        send(request, response);
        return;
    }
    if let Some(route) = app_runs::route(path) {
        let writable = matches!(route, app_runs::Route::List) || !route.is_read();
        if *method != Method::Post || !writable {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let response = app_runs::handle(&mut request, state_dir, route, true);
        send(request, response);
        return;
    }
    let catalog_recovery = path
        .strip_prefix("/api/app-installations/migrations/")
        .and_then(|tail| tail.strip_suffix("/recover"))
        .filter(|id| !id.is_empty() && !id.contains('/'));
    let install_upgrade_recovery = path
        .strip_prefix("/api/app-installations/")
        .and_then(|tail| tail.strip_suffix("/upgrade/recover"))
        .filter(|id| !id.is_empty() && !id.contains('/'));
    let install_upgrade_check = path
        .strip_prefix("/api/app-installations/")
        .and_then(|tail| tail.strip_suffix("/upgrade/check"))
        .filter(|id| !id.is_empty() && !id.contains('/'));
    let install_upgrade = path
        .strip_prefix("/api/app-installations/")
        .and_then(|tail| tail.strip_suffix("/upgrade"))
        .filter(|id| !id.is_empty() && !id.contains('/'));
    let install_recovery = path
        .strip_prefix("/api/app-installations/")
        .and_then(|tail| tail.strip_suffix("/recover"))
        .filter(|id| !id.is_empty() && !id.contains('/'));
    if path == "/api/app-installations/migrate"
        || catalog_recovery.is_some()
        || install_recovery.is_some()
        || install_upgrade.is_some()
        || install_upgrade_check.is_some()
        || install_upgrade_recovery.is_some()
    {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let (operation, id) = if let Some(id) = catalog_recovery {
            ("app_workspace_migration_recover", Some(id))
        } else if let Some(id) = install_upgrade_recovery {
            ("app_workspace_upgrade_recover", Some(id))
        } else if let Some(id) = install_upgrade_check {
            ("app_workspace_upgrade_check", Some(id))
        } else if let Some(id) = install_upgrade {
            ("app_workspace_upgrade", Some(id))
        } else if let Some(id) = install_recovery {
            ("app_workspace_recover", Some(id))
        } else {
            ("app_workspace_migrate", None)
        };
        let response = apps::workspace(&mut request, state_dir, operation, id);
        send(request, response);
        return;
    }
    if path == "/api/app-installations" {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let response = apps::workspace(&mut request, state_dir, "app_workspace_install", None);
        send(request, response);
        return;
    }
    // An app approval (CAD-557) — relayed to the daemon's
    // `app_approve`, operator-only on the board (see `apps`).
    if let Some((key, name)) = apps::approve_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = apps::approve(&mut request, state_dir, key, name);
        send(request, resp);
        return;
    }
    // An app revocation (CAD-577) — relayed to the daemon's
    // `app_revoke`, operator-only on the board (see `apps`).
    if let Some((key, name)) = apps::revoke_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = apps::revoke(&mut request, state_dir, key, name);
        send(request, resp);
        return;
    }
    // The app's default team (CAD-577) — relayed to the daemon's
    // `app_set_team`, operator-only on the board (see `apps`).
    if let Some((key, name)) = apps::team_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = apps::set_team(&mut request, state_dir, key, name);
        send(request, resp);
        return;
    }
    // "Add worker" (CAD-577) — relayed to the daemon's `app_add_worker`,
    // operator-only on the board (see `apps`).
    if let Some((key, name)) = apps::worker_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = apps::add_worker(&mut request, state_dir, key, name);
        send(request, resp);
        return;
    }
    if let Some(id) = home::answer_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let Some(caller) = &caller else {
            send(request, err_response(500, "unadmitted write"));
            return;
        };
        let resp = home::answer(&mut request, state_dir, pm_dir, caller.actor(), id);
        send(request, resp);
        return;
    }
    // CAD-606: operator Kick off. `admit` already required an operator
    // session; the handler relays `issue_kickoff`, which checks the
    // connection again.
    if let Some(id) = home::kickoff_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = home::post_kickoff(&mut request, state_dir, pm_dir, id);
        send(request, resp);
        return;
    }
    // The composer's slash commands and Stop (CAD-551) — operator-only,
    // relayed to the daemon's `master_command`; `home::master_command`
    // refuses every verb outside its own allowlist before the relay.
    if path == "/api/master/command" {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = home::master_command(&mut request, state_dir);
        send(request, resp);
        return;
    }
    // The operator's chat message to an agent (CAD-319) — guarded and
    // caller-attributed inside `threads::post_message`.
    if let Some((alias, sub)) = threads::route(path) {
        if *method != Method::Post || sub != Some("messages") {
            send(request, err_response(404, "no such thread write route"));
            return;
        }
        let resp = threads::post_message(&mut request, state_dir, alias);
        send(request, resp);
        return;
    }
    // The Needs-you rail's snooze/dismiss (CAD-574) — operator-only,
    // relayed to the daemon's `needs_dismiss`.
    if let Some((id, verb)) = home::permission_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = home::decide_permission(&mut request, state_dir, id, verb);
        send(request, resp);
        return;
    }
    if let Some(verb) = home::needs_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = home::decide_need(&mut request, state_dir, verb);
        send(request, resp);
        return;
    }
    // The rail's agent resume/unfence (CAD-574) — operator-only on the
    // board; the daemon's own rules for each verb still apply.
    if let Some((alias, verb)) = home::agent_action_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = home::agent_action(&mut request, state_dir, alias, verb);
        send(request, resp);
        return;
    }
    // CAD-608: the issue page's lane. Admitted above (operator-only);
    // the handler then relays on an operator assertion so an in-process
    // board test reaches the daemon's operator gate. Production builds
    // leave that assertion as a no-op — the board process is the proof.
    if let Some(tail) = path.strip_prefix("/api/issues/") {
        if let Some((id, verb)) = lane::write_target(tail) {
            if *method != Method::Post {
                send(request, err_response(405, "method not allowed"));
                return;
            }
            let resp = lane::post(&mut request, state_dir, id, verb);
            send(request, resp);
            return;
        }
    }
    // The wiki routes (CAD-580): the caller `admit` derived rides
    // `wiki_as` to the daemon — writes and uploads relay, paths and
    // ACLs are the daemon's, the board only ever shrinks a caller.
    if let Some(tail) = path.strip_prefix("/api/wiki/") {
        let Some(caller) = caller else {
            send(request, err_response(500, "unadmitted write"));
            return;
        };
        let resp = wiki::write(
            &mut request,
            method,
            tail,
            &caller,
            query,
            state_dir,
            pm_dir,
        );
        send(request, resp);
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
    let Some(caller) = caller else {
        send(request, err_response(500, "unadmitted write"));
        return;
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
    // Only after the route has run its authorization and validation. A
    // validator can never turn an operator-only refusal into a 304.
    let not_modified = request.method() == &Method::Get
        && resp.status_code() == StatusCode(200)
        && resp
            .headers()
            .iter()
            .find(|h| h.field.equiv("ETag"))
            .is_some_and(|etag| {
                header_value(&request, "If-None-Match").is_some_and(|values| {
                    values.split(',').any(|v| v.trim() == etag.value.as_str())
                })
            });
    if not_modified {
        let mut bare = Response::empty(StatusCode(304));
        for h in resp.headers() {
            bare.add_header(h.clone());
        }
        let _ = request.respond(bare);
    } else if head_only {
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

/// The exact collection representation, shared with SSE aggregate change
/// detection so config/count changes invalidate and title-only edits do not.
/// Counts come from the tracker's already-loaded issues — a title edit must
/// not re-parse every folder to learn that the counts did not move.
fn projects_payload(pm: &Pm, counts: &HashMap<String, usize>) -> Value {
    let projects = project::list(&pm.dir).unwrap_or_default();
    let payload: Vec<Value> = projects.iter().map(|p| json!({
        "key": p.key, "prefix": p.prefix, "components": p.components,
        "tags": p.tags, "default_owner": p.default_owner,
        "repos": p.repos.iter().map(|r| json!({"path": r.path, "remote": r.remote})).collect::<Vec<_>>(),
        "issues": counts.get(&p.key).copied().unwrap_or(0),
    })).collect();
    json!({"projects": payload})
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
        "issues" => &[
            "issues",
            "projects",
            "issue",
            "overview",
            "workflows",
            "apps",
            "app",
            "app_runs",
            "outbox",
            "app_outputs",
        ],
        "jobs" => &[
            "issues",
            "agents",
            "issue",
            "overview",
            "app_runs",
            "outbox",
            "app_outputs",
        ],
        // A released publish lands an outbox item — the same event the
        // effect row's state change produces. Agent rows change what an
        // app's workflow checks resolve to, so apps refetch too.
        "agents" => &["agents", "issue", "overview", "outbox", "apps", "app"],
        "monitoring" => &["overview"],
        _ => &[
            "issues",
            "projects",
            "agents",
            "issue",
            "overview",
            "workflows",
            "apps",
            "app",
            "app_runs",
            "outbox",
            "app_outputs",
        ],
    }
}

/// `GET /api/stream` — server-sent events written straight onto the
/// socket. tiny_http's chunked path buffers small writes inside
/// `chunked_transfer::Encoder` (it flushes only on `flush()` or a full
/// chunk), so a reader-based `Response` would never emit a small SSE
/// frame. `into_writer` hands over the socket: the head is written by
/// hand, each frame flushes immediately, and dropping the writer on
/// exit closes the stream — which is also how a dead client surfaces.
fn stream_hello(entities: bool) -> String {
    let mut data = json!({"build": crate::overview::BUILD_ID});
    if entities {
        data["entities"] = json!(true);
    }
    format!("event: hello\ndata: {data}\n\n")
}

fn stream_events(request: Request, state_dir: &Path, pm_dir: &Path) {
    let entities = request
        .url()
        .split_once('?')
        .is_some_and(|(_, q)| q.split('&').any(|part| part == "entities=1"));
    let mut w = request.into_writer();
    // Join the board's shared watcher before the head goes out: the
    // first subscriber's baseline is taken inside `subscribe`, so the
    // client's first action after it sees the stream live cannot be
    // absorbed into it (CAD-325: one watcher per board, not per client).
    let rx = read_model::get(state_dir, pm_dir).subscribe();
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                Cache-Control: no-store\r\nConnection: close\r\n\r\n";
    if w.write_all(head.as_bytes())
        .and_then(|_| w.flush())
        .is_err()
    {
        return;
    }
    let frame = |w: &mut dyn Write, bytes: &[u8]| -> bool {
        w.write_all(bytes).and_then(|_| w.flush()).is_ok()
    };
    // First frame immediately — `hello` names the serving build (a tab
    // running an older bundle compares and prompts a reload, CAD-573)
    // and the `: ping` right behind it proves the stream is live and
    // gives proxies something to flush before the first event exists.
    let hello = stream_hello(entities);
    if !frame(&mut w, hello.as_bytes()) || !frame(&mut w, b": ping\n\n") {
        return;
    }
    // `: ping` every 15 s of wire silence — the watcher's per-second
    // heartbeat would otherwise starve the keepalive, and an idle dead
    // client would never surface without a write.
    let mut ping_at = Instant::now() + Duration::from_secs(15);
    loop {
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(f) if &*f == read_model::HEARTBEAT => {}
            Ok(f) => {
                if !entities && f.starts_with("event: aggregates\n") {
                    continue;
                }
                let optimized = entities.then(|| read_model::entity_frame(&f));
                let bytes = optimized.as_deref().unwrap_or(&f);
                if !frame(&mut w, bytes.as_bytes()) {
                    return;
                }
                ping_at = Instant::now() + Duration::from_secs(15);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if Instant::now() >= ping_at {
            let ping: &[u8] = if entities {
                b"event: heartbeat\ndata: {}\n\n"
            } else {
                b": ping\n\n"
            };
            if !frame(&mut w, ping) {
                return;
            }
            ping_at = Instant::now() + Duration::from_secs(15);
        }
    }
}

fn handle(mut request: Request, state_dir: &Path, pm_dir: &Path, opts: &ServeOpts) {
    // CAD-482: a seam-armed board honors the assertion headers; the
    // scope makes the asserted identity visible to attribution and to
    // the daemon calls this request relays. Absent headers, or absent
    // the seam, the real peer checks run unchanged. A malformed or
    // forged assertion refuses the request outright.
    let _seam_scope = match crate::test_seam::scope_headers(
        opts.seam.as_ref(),
        header_value(&request, crate::test_seam::AS_HEADER).as_deref(),
        header_value(&request, crate::test_seam::TOKEN_HEADER).as_deref(),
    ) {
        Ok(scope) => scope,
        Err(why) => {
            let _ = request.respond(err_response(403, &why));
            return;
        }
    };
    let method = request.method().clone();
    let head_only = method == Method::Head;
    let is_write = matches!(
        method,
        Method::Post | Method::Patch | Method::Delete | Method::Put
    );
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

    // CAD-526: a request that names this board's public host is on the
    // platform sign-in surface. `/__platform/*` is the contract's
    // reserved prefix; every other request needs the
    // `__Host-aos-board-session` the session endpoint mints — never the
    // local login flow (`operator::open` refuses `Origin::Public` too).
    let public_host = opts
        .public
        .as_ref()
        .is_some_and(|p| host.trim().eq_ignore_ascii_case(&p.host));
    // CAD-747: a hosted agent shares this network namespace and can dial
    // the board's loopback port (or the image's blind TCP relay) itself.
    // Host is therefore a surface selector, never proof of the Worker.
    // In hosted-only mode the public board session gate must cover every
    // protected route, even when the caller chooses a local Host.
    if opts.board_public_only && !public_host {
        if matches!(method, Method::Get | Method::Head)
            && raw_path == "/api/health"
            && raw_query.is_empty()
        {
            send(
                request,
                json_response(json!({
                    "ok": true,
                    "build": crate::overview::BUILD_COMMIT,
                })),
            );
        } else {
            send(
                request,
                err_response(421, "hosted board requires its public Host"),
            );
        }
        return;
    }
    if public_host {
        if path.starts_with("/__platform/") {
            let resp = match (method.as_str(), path.as_str()) {
                ("GET" | "HEAD", "/__platform/login") => operator::platform_login(opts, raw_query),
                ("GET" | "HEAD", _) => operator::platform_read(&path),
                ("POST", "/__platform/session") => {
                    operator::platform_session(&mut request, state_dir, opts)
                }
                _ => operator::platform_unknown(),
            };
            send(request, resp);
            return;
        }
        if is_write {
            let send_write = |req: Request, resp: HttpResp| {
                read_model::get(state_dir, pm_dir).invalidate();
                send(req, resp)
            };
            write_route(
                request,
                &method,
                &path,
                &query,
                state_dir,
                pm_dir,
                opts,
                &send_write,
            );
            return;
        }
        // A read on the public host needs a live board session —
        // `/api/health` and `/api/version` stay open so a probe can see
        // the board is up, and a signed-out tab can still learn the
        // serving build, without holding a credential.
        if path != "/api/health" && path != "/api/version" {
            match operator::public_session(&request, state_dir, opts) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    let resp = operator::session_bounce(&request, &path, opts);
                    send(request, resp);
                    return;
                }
                Err(resp) => {
                    send(request, resp);
                    return;
                }
            }
        }
        // Signed-in reads fall through to the shared dispatch below.
    }

    if is_write {
        // The writer's next read must see its write (CAD-325): drop the
        // read model's daemon-side caches before the answer goes out.
        let send_write = |req: Request, resp: HttpResp| {
            read_model::get(state_dir, pm_dir).invalidate();
            send(req, resp)
        };
        write_route(
            request,
            &method,
            &path,
            &query,
            state_dir,
            pm_dir,
            opts,
            &send_write,
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
            let (actor, tailnet_proof) = request_identity(&request, opts);
            // CAD-432: may this client make the operator's board
            // decisions — the same proof those writes run. It walks
            // /proc, so it is computed only when asked (`?operator=1`,
            // once per page load), never on the 30 s poll.
            let operator = matches!(query("operator").as_deref(), Some("1" | "true"))
                .then(|| home::operator_viewer(&request, state_dir, opts));
            let session = operator::meta(&request, state_dir, opts);
            // Display the same verified identity that attributes public
            // board writes, never a name supplied by request fields.
            // Public-origin sessions carry a verified user; every other
            // session is the operator's, matching `held_of`'s
            // attribution exactly.
            let actor = if session["session"]["origin"].as_str() == Some("public") {
                serde_json::from_value::<crate::operator_auth::BoardUser>(
                    session["session"]["user"].clone(),
                )
                .map(|user| user.actor())
                .unwrap_or(actor)
            } else {
                actor
            };
            send(
                request,
                json_response(json!({
                    "read_only": opts.read_only,
                    "signed_in": session["signed_in"],
                    "hosted": session["hosted"],
                    "session": session["session"],
                    "login_hint": session["login_hint"],
                    "device_login": session["device_login"],
                    "tab_signed_out": session["tab_signed_out"],
                    "actor": actor,
                    "tailnet_proof": tailnet_proof,
                    "operator": operator,
                    "platform_account_configured": opts.public.as_ref().is_some_and(|board| board.issuer == "http://api.internal"),
                    "tailnet_url": opts
                        .tailnet
                        .as_ref()
                        .map(|(dns, port)| tailnet_url(dns, *port)),
                    "version": env!("CARGO_PKG_VERSION"),
                    "build_commit": crate::overview::BUILD_COMMIT,
                    "build_time": crate::overview::BUILD_TIME,
                    "daemon": daemon,
                    // CAD-446: the `gh` this board's sync runs, fixed at
                    // start; `null` when the board runs no sync.
                    "delivery_sync": opts.delivery_sync.as_ref().map(|s| s.meta()),
                })),
            );
        }
        // CAD-561: the Update card's view and the draining banner.
        "/api/update" => send(request, updates::get(state_dir)),
        "/api/update/banner" => send(request, updates::banner_get(state_dir)),
        "/api/settings/model-defaults" => {
            send(request, model_defaults_get(state_dir, opts.read_only));
        }
        "/api/platform-account" => send(request, platform_account::get(opts)),
        // CAD-615: the operator's master permission rules and pending
        // requests. The board relays over its own daemon connection, so
        // the HTTP peer is admitted here — the same operator proof as
        // the decision writes.
        "/api/master/permissions" => {
            if let Err(resp) = operator::admit_operator_read(&request, state_dir, opts) {
                send(request, resp);
            } else {
                let resp = match client::rpc(state_dir, "master_permission_list", json!({})) {
                    Ok(out) => json_response(out),
                    Err(e) => home::rpc_err(&e, "master_permission_list"),
                };
                send(request, resp);
            }
        }
        // The serving binary's build id and nothing else — cheap, and
        // unauthenticated like `/api/health`, so a tab whose stream is
        // stuck reconnecting can compare it against its own bundle's.
        "/api/version" => {
            send(
                request,
                json_response(json!({"build": crate::overview::BUILD_ID})),
            );
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
                    // CAD-561: which build is answering, so
                    // `cadence update`'s health check can require the
                    // board to come back on the new release.
                    "build": crate::overview::BUILD_COMMIT,
                })),
            );
        }
        "/api/setup" => {
            // Refused before anything runs: no probe for a viewer.
            let resp = match setup_refusal(&request, opts) {
                Some(refused) => refused,
                None => {
                    let fresh = matches!(query("fresh").as_deref(), Some("1" | "true"));
                    setup_get(state_dir, pm_dir, opts.port, fresh)
                }
            };
            send(request, resp);
        }
        "/api/overview" => {
            let mut overview = read_model::get(state_dir, pm_dir).overview();
            // CAD-446: a page view asks for a delivery sync (bounded,
            // never during a back-off) and shows the sync's problem.
            if let Some(sync) = &opts.delivery_sync {
                sync.nudge();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                if let Some(needs) = overview["needs_me"].as_array_mut() {
                    needs.extend(sync.needs_rows(now));
                }
                // CAD-574: the sync rows are appended after the build —
                // the dismissal filter runs again over the union so a
                // sync row is suppressible like any other.
                let dismissed = crate::needs_dismiss::dismissed(state_dir);
                if !dismissed.is_empty() {
                    if let Some(needs) = overview["needs_me"].as_array_mut() {
                        let kept = crate::needs_dismiss::filter_rows(
                            std::mem::take(needs),
                            &dismissed,
                            now,
                        );
                        *needs = kept;
                    }
                }
            }
            send(request, json_response(overview))
        }
        "/api/projects" => match Pm::at(pm_dir) {
            Ok(pm) => {
                send(
                    request,
                    json_response(read_model::get(state_dir, pm_dir).projects(&pm)),
                );
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
                // The same slices as `issue ls`: tag (all of — the
                // grammar's exception), every other key any-of over
                // repeated/comma values, open=1.
                let slice = board::Filter {
                    tags: query_all("tag"),
                    epics: query_all("epic"),
                    owners: query_all("owner"),
                    statuses: query_all("status"),
                    components: query_all("component"),
                    priorities: query_all("priority"),
                    types: query_all("type"),
                    milestones: query_all("milestone"),
                    plans: query_all("plan"),
                    open: query("open").is_some_and(|v| v == "1" || v == "true"),
                };
                if let Err(e) = slice.validate() {
                    send(request, err_response(400, &e.to_string()));
                    return;
                }
                let read = read_model::get(state_dir, pm_dir).board(&pm, filter.as_deref());
                // CAD-405: each card carries its `work` block.
                let by_id = read.by_id();
                let ctx = read.ctx(&by_id);
                send(
                    request,
                    json_response(json!({
                        "issues": read
                            .views
                            .iter()
                            .filter(|v| slice.matches(v))
                            .map(|v| read.card(&ctx, v))
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
                let read = read_model::get(state_dir, pm_dir).board(&pm, None);
                send(
                    request,
                    json_response(json!({
                        "epics": crate::issue::work::epics_json(
                            &pm.dir,
                            &read.views,
                            filter.as_deref(),
                            crate::issue::time::now_epoch(),
                            &read.approvals,
                        ),
                    })),
                );
            }
            Err(e) => send(request, err_response(503, &e.to_string())),
        },
        "/api/milestones" => send(
            request,
            stages::milestones(state_dir, pm_dir, query("project").as_deref()),
        ),
        "/api/agents" => send(
            request,
            json_response(read_model::get(state_dir, pm_dir).agents()),
        ),
        // CAD-546: the `local` platform's outbox — an operator-only
        // read, the same proof the board's operator write routes take.
        "/api/outbox" => {
            let resp = home::outbox(&request, state_dir, opts, query("effect_id"));
            send(request, resp);
        }
        "/api/memories" => match Pm::at(pm_dir) {
            Ok(pm) => {
                // The `memory ls` grammar: keys repeat and comma-join
                // (any-of), different keys AND.
                let project = query_all("project");
                let status = query_all("status");
                let kind = query_all("type");
                let component = query_all("component");
                let paths = query_all("path");
                if let Err(e) = crate::filter::check_set("status", &status, crate::memory::STATUSES)
                    .and_then(|_| crate::filter::check_set("type", &kind, crate::memory::TYPES))
                {
                    send(request, err_response(400, &e.to_string()));
                    return;
                }
                let (mems, errors) = crate::memory::load_all_report(&pm.dir);
                let projects = project::list(&pm.dir).unwrap_or_default();
                let payload: Vec<Value> =
                    mems.iter()
                        .filter(|m| {
                            let scope = &m.front.scope;
                            crate::filter::any_of(&project, Some(m.project.as_str()))
                                && crate::filter::any_of(&status, Some(m.front.status.as_str()))
                                && crate::filter::any_of(&kind, Some(m.front.kind.as_str()))
                                && (component.is_empty()
                                    || scope.project
                                    || scope.components.iter().any(|c| component.contains(c)))
                                && (paths.is_empty()
                                    || scope.project
                                    || scope.paths.iter().any(|g| {
                                        paths.iter().any(|p| crate::memory::glob_match(g, p))
                                    }))
                        })
                        .map(|m| {
                            let fresh = crate::memory::Freshness::among(&projects, &m.project);
                            crate::memory::card_json(m, &fresh)
                        })
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
            // The wiki reads (CAD-580): the request's caller — session,
            // named member, attributed agent — rides `wiki_as`; a caller
            // the board cannot attribute is refused before the daemon
            // sees it. Blob pages stream `.blobs/<sha>` with ranges.
            if let Some(tail) = path.strip_prefix("/api/wiki/") {
                let resp = wiki::read(&request, tail, &query, state_dir, pm_dir, opts);
                send(request, resp);
                return;
            }
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
                // `/api/projects/<key>/workflows[/<name>/preview]` — the
                // project's workflow templates beside PROJECT.md (CAD-496).
                if let Some(read) = workflows::read_route(&path) {
                    match Pm::at(pm_dir) {
                        Ok(pm) => send(request, workflows::read(&pm, state_dir, &query, read)),
                        Err(e) => send(request, err_response(503, &e.to_string())),
                    }
                    return;
                }
            }
            if let Some(route) = app_contexts::route(&path) {
                if !route.is_read() {
                    send(request, err_response(405, "method not allowed"));
                    return;
                }
                if let Err(response) = operator::admit_operator_read(&request, state_dir, opts) {
                    send(request, response);
                    return;
                }
                let response = app_contexts::handle(&mut request, state_dir, route, false);
                send(request, response);
                return;
            }
            if let Some(route) = connections::route(&path) {
                if !route.is_read() {
                    send(request, err_response(405, "method not allowed"));
                    return;
                }
                if let Err(response) = operator::admit_operator_read(&request, state_dir, opts) {
                    send(request, response);
                    return;
                }
                let response = connections::handle(&mut request, state_dir, route, false);
                send(request, response);
                return;
            }
            if let Some(route) = app_release::route(&path) {
                if !route.is_read() {
                    send(request, err_response(405, "method not allowed"));
                    return;
                }
                if let Err(response) = operator::admit_operator_read(&request, state_dir, opts) {
                    send(request, response);
                    return;
                }
                let response = app_release::handle(&mut request, state_dir, route, false);
                send(request, response);
                return;
            }
            if let Some(route) = app_runs::route(&path) {
                if !route.is_read() {
                    send(request, err_response(405, "method not allowed"));
                    return;
                }
                if let Err(response) = operator::admit_operator_read(&request, state_dir, opts) {
                    send(request, response);
                    return;
                }
                let response = app_runs::handle(&mut request, state_dir, route, false);
                send(request, response);
                return;
            }
            if path == "/api/app-installations"
                || path
                    .strip_prefix("/api/app-installations/")
                    .is_some_and(|id| !id.is_empty() && !id.contains('/'))
            {
                if let Err(response) = operator::admit_operator_read(&request, state_dir, opts) {
                    send(request, response);
                    return;
                }
                let id = path.strip_prefix("/api/app-installations/");
                let method = if id.is_some() {
                    "app_workspace_show"
                } else {
                    "app_workspace_list"
                };
                let response = apps::workspace(&mut request, state_dir, method, id);
                send(request, response);
                return;
            }
            // `/api/apps[/<project>/<name>[/runs|/outputs]]` — installed
            // apps: the list, or one app's guide, workflows, rubrics,
            // bindings and doctor findings, its runs and its outputs
            // (CAD-557, CAD-563).
            if let Some(read) = apps::read_route(&path) {
                match Pm::at(pm_dir) {
                    Ok(pm) => {
                        let resp = apps::read(&request, &pm, state_dir, opts, &query, read);
                        send(request, resp);
                    }
                    Err(e) => send(request, err_response(503, &e.to_string())),
                }
                return;
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
                    Ok((proj, m)) => {
                        let fresh = crate::memory::Freshness::for_project(Some(&proj));
                        send(
                            request,
                            json_response(crate::memory::detail_json(&m, &fresh)),
                        )
                    }
                    Err(e) => send(request, err_response(404, &e.to_string())),
                }
                return;
            }
            // `/api/master/models` — the master's model-picker read
            // (CAD-575): operator-gated like the `master_models` RPC
            // it relays.
            if path == "/api/master/models" {
                let resp = home::master_models(&request, state_dir, opts);
                send(request, resp);
                return;
            }
            // `/api/master/summary?since=` — "since you left" (CAD-328).
            if path == "/api/master/summary" {
                send(request, home::master_summary(state_dir, &query));
                return;
            }
            // `/api/master/state` — the header chips + turn state (CAD-551).
            if path == "/api/master/state" {
                send(request, home::master_state(state_dir));
                return;
            }
            // `/api/threads/<alias>[/stream]` — an agent's chat (CAD-319).
            if let Some((alias, sub)) = threads::route(&path) {
                match sub {
                    None => send(request, threads::read(state_dir, alias, &query)),
                    Some("stream") if head_only => {
                        send(request, err_response(405, "stream is GET only"))
                    }
                    Some("stream") => threads::stream(request, state_dir, alias, &query),
                    Some(_) => send(request, err_response(404, "no such thread route")),
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
                    !matches!(s, "file" | "activity" | "history" | "kickoff" | "lane")
                        && !s.starts_with("artifacts/")
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
                        let read = read_model::get(state_dir, pm_dir).board(&pm, None);
                        let by_id: std::collections::HashMap<String, &board::View> = read
                            .views
                            .iter()
                            .map(|v| (v.issue.front.id.clone(), v))
                            .collect();
                        let Some(view) = by_id.get(&id) else {
                            send(request, err_response(404, "unknown issue"));
                            return;
                        };
                        match sub {
                            Some("kickoff") => {
                                let resp = home::kickoff_options(&request, state_dir, opts, &id);
                                send(request, resp);
                            }
                            None => send(
                                request,
                                json_response(with_agents(
                                    crate::issue::work::detail_json(
                                        &pm.dir,
                                        &read.ctx(&by_id),
                                        view,
                                    ),
                                    &read.by_issue,
                                    &id,
                                )),
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
                            Some("lane") => send(request, lane::show(state_dir, &id)),
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
            // Static: a file of the build, the SPA shell for a client
            // route, or 404 for a file that is not there.
            match static_answer_for(
                opts.dist.as_deref(),
                &path,
                header_value(&request, "Accept").as_deref(),
            ) {
                StaticAnswer::File(name, bytes) => {
                    let mut resp = Response::from_data(bytes);
                    resp.add_header(
                        Header::from_bytes("Content-Type", content_type(&name)).unwrap(),
                    );
                    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
                    send(request, resp);
                }
                StaticAnswer::Missing => send(request, err_response(404, "no such file")),
                StaticAnswer::NoBuild => send(
                    request,
                    err_response(
                        503,
                        "no SPA build — pass --dist or rebuild with --features ui",
                    ),
                ),
            }
        }
    }
}

// ---------- /api/setup — the wizard's checks (CAD-327) ----------

/// How long one run of the setup checks answers `GET /api/setup`.
const SETUP_FRESH_FOR: Duration = Duration::from_secs(60);
/// A `?fresh=1` re-check younger than this is answered from the last
/// run — a held-down button cannot keep provider CLIs spawning.
const SETUP_MIN_RECHECK: Duration = Duration::from_secs(5);

/// The last run and when it finished. The lock is held while the
/// checks run, so concurrent requests share one run instead of each
/// spawning the provider probes.
static SETUP_CACHE: std::sync::Mutex<Option<(Instant, u64, Value)>> = std::sync::Mutex::new(None);

/// Runs of the setup checks this process made — tests pin that a
/// refused or reused request spawns no probe.
static SETUP_RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[doc(hidden)]
pub fn setup_runs() -> u64 {
    SETUP_RUNS.load(std::sync::atomic::Ordering::SeqCst)
}

/// `/api/setup` is the operator's, on the host: it shows HOME's layout,
/// the installed CLIs and their sign-in state, and the daemon's pid and
/// socket, and a visit spawns the provider probes. A read-only board, a
/// request through the tailnet (proven or not) and a peer that is not
/// loopback are refused — before anything runs.
fn setup_refusal(request: &Request, opts: &ServeOpts) -> Option<HttpResp> {
    const WHY: &str = "setup runs on the host — open the board on 127.0.0.1 there";
    if opts.read_only {
        return Some(guard_fail("read_only", WHY));
    }
    if tailnet_host(request, opts) {
        return Some(guard_fail("tailnet", WHY));
    }
    if !request.remote_addr().is_some_and(|a| a.ip().is_loopback()) {
        return Some(guard_fail("loopback", WHY));
    }
    None
}

/// `GET /api/setup` — setup's checks, detect only
/// ([`crate::setup::board_detect`]): nothing is applied, started or
/// written, provider probes are bounded and never echoed. Each entry is
/// setup's `{check, status, detail, fix}` plus the wizard `group`;
/// `master.providers` carries the master step's provider offers
/// (CAD-448) — its exact start command, never run from the board.
fn setup_get(state_dir: &Path, pm_dir: &Path, port: u16, fresh: bool) -> HttpResp {
    let mut cache = SETUP_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let reuse = cache.as_ref().is_some_and(|(at, _, _)| {
        let age = at.elapsed();
        age < SETUP_MIN_RECHECK || (!fresh && age < SETUP_FRESH_FOR)
    });
    if !reuse {
        SETUP_RUNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let detected = match crate::setup::board_detect(state_dir, pm_dir, port) {
            Ok(d) => d,
            Err(e) => return err_response(500, &e.to_string()),
        };
        let checks: Vec<Value> = detected
            .checks
            .iter()
            .map(|o| {
                let mut v = serde_json::to_value(o).unwrap_or_default();
                v["group"] = json!(crate::setup::check_group(&o.check));
                v
            })
            .collect();
        let checked_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        *cache = Some((
            Instant::now(),
            checked_at,
            json!({
                "checks": checks,
                "master": {"providers": detected.master_providers},
            }),
        ));
    }
    let (at, checked_at, run) = cache.as_ref().expect("filled above");
    let age = at.elapsed();
    json_response(json!({
        "checks": run["checks"],
        "master": run["master"],
        "checked_at": checked_at,
        "detect_only": true,
        // Whether this request ran the checks, how old the run is, and
        // how long until a re-check runs them again.
        "ran_now": !reuse,
        "age_ms": age.as_millis() as u64,
        "recheck_in_ms": SETUP_MIN_RECHECK.saturating_sub(age).as_millis() as u64,
    }))
}

/// The board read model's cost meters (`parses`, `overview_builds`,
/// `request_builds`) for
/// one `(state dir, PM dir)` — what the CAD-325 bench asserts the caches
/// by. Test support; the board serves no route for it.
#[doc(hidden)]
pub fn read_model_stats(state_dir: &Path, pm_dir: &Path) -> Value {
    read_model::get(state_dir, pm_dir).stats()
}

/// One mutex for every write route — the server is thread-per-request
/// since `/api/stream`, and issue file writes must not interleave.
static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn board_boot_agent_uid(state_dir: &Path, injected: Option<u32>) -> Result<Option<u32>> {
    if injected.is_some() {
        return Ok(injected);
    }
    let configured = crate::agent_uid::config::configured_uid(state_dir)?;
    let historical = crate::agent_uid::config::mode_marker_uid(state_dir)?;
    if configured.is_some() || historical.is_some() {
        crate::agent_uid::config::require_private_state_dir(state_dir)?;
    }
    reconcile_board_agent_uid(
        configured,
        historical,
        operator::active_agent_uid(state_dir),
    )
}

fn reconcile_board_agent_uid(
    configured: Option<u32>,
    historical: Option<u32>,
    health: std::result::Result<Option<u32>, String>,
) -> Result<Option<u32>> {
    match health {
        Ok(uid) if configured.is_some() && uid != configured => Err(Error::rejected(
            "Private daemon agent UID differs from configured UID",
        )),
        Ok(uid) if historical.is_some() && uid != historical => Err(Error::rejected(
            "Private daemon agent UID differs from persistent mode marker",
        )),
        Ok(uid) => Ok(uid),
        Err(error) if configured.is_some() || historical.is_some() => Err(Error::rejected(
            format!("Agent UID mode has no private daemon boot pin: {error}"),
        )),
        Err(_) => Ok(None), // Standalone local board, with the split disabled.
    }
}

pub fn serve(state_dir: &Path, pm_dir: &Path, opts: &ServeOpts) -> Result<()> {
    if opts.board_public_only && opts.public.is_none() {
        return Err(Error::rejected(
            "board public-only mode requires a public board identity",
        ));
    }
    // CAD-526: a publicly-named board hands the daemon its trust root —
    // `operator/board-identity.json`, under the same uid-private rules
    // as the operator secret — before the first request can mint a
    // session against it.
    if let Some(public) = &opts.public {
        crate::board_identity::write_config(
            state_dir,
            &crate::board_identity::Config {
                host: public.host.clone(),
                issuer: public.issuer.clone(),
                company: public.company.clone(),
            },
        )?;
    }
    // The tailnet proof's operator latch starts with this process: read
    // tailscaled's operator user now, never trust a caller-made latch.
    let mut opts = opts.clone();
    opts.agent_uid = board_boot_agent_uid(state_dir, opts.agent_uid)?;
    opts.tailnet_latch = if opts.tailnet.is_some() {
        crate::tailnet_proof::OperatorLatch::at_startup(opts.tailscaled_socket.as_deref())
    } else {
        Default::default()
    };
    let server = Server::http(format!("{}:{}", opts.host, opts.port)).map_err(|e| {
        if let Some(startup) = opts.startup.take() {
            let kind = e
                .downcast_ref::<std::io::Error>()
                .map_or(std::io::ErrorKind::Other, std::io::Error::kind);
            let _ = startup.send(Err(kind));
        }
        Error::internal(format!("ui bind {}:{}: {e}", opts.host, opts.port))
    })?;
    // CAD-777: same handoff for the device trust pin — the daemon
    // verifies the presented grant against this file at mint time, so
    // a socket caller can never choose the issuer, the workspace, or
    // mint for a subject off the operator's allowlist. When device
    // login is not configured, any stale pin is removed so an old
    // file cannot mint after the operator turned the flow off.
    //
    // Written ONLY here: after the bind succeeded (a process that
    // loses the port touches nothing) and while no OTHER live UI owns
    // this state dir — a second `ui run` must not rewrite or delete
    // the pin out from under the running board (review r3).
    // The lock File stays bound for the rest of `serve()` — the
    // board's claim on the pin dies only with this process.
    let _device_pin_lock = pin_device_login(state_dir, &opts)?;
    // CAD-446: merge decisions appear without a terminal — this process
    // (the operator's, when it proves so) reads the loop's PRs with the
    // operator's `gh`. Started only once the port is ours; a read-only
    // board writes nothing, observations included.
    // `gh` is fixed to an absolute path once, here: neither the sync
    // nor Merge looks it up on PATH again.
    let gh = delivery_sync::resolve_gh(
        opts.gh.as_deref().unwrap_or(Path::new(crate::delivery::GH)),
        std::env::var_os("PATH").as_deref(),
    );
    if let Ok(abs) = &gh {
        opts.gh = Some(abs.clone());
    }
    // CAD-482: a seam-armed board attaches to the credential its
    // fixture daemon minted — refused loudly on other builds/dirs so a
    // fixture never silently falls back to ambient identity. A state
    // dir that still carries the minted token re-attaches: `daemon
    // restart --ui` respawns this process without the arming env.
    opts.seam = crate::test_seam::attach_if_requested(
        state_dir,
        opts.test_seam || crate::test_seam::armed(state_dir),
    )?;
    opts.delivery_sync = (!opts.read_only).then(|| {
        delivery_sync::start(
            state_dir,
            pm_dir,
            opts.delivery_sync_every,
            gh,
            opts.seam.is_some(),
        )
    });
    if let Some(startup) = opts.startup.take() {
        let _ = startup.send(Ok(()));
    }
    let opts = &opts;
    eprintln!("cadence ui listening on http://{}:{}", opts.host, opts.port);
    loop {
        let request = match &opts.stop {
            None => match server.recv() {
                Ok(request) => request,
                Err(_) => break,
            },
            Some(stop) => {
                if stop.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                match server.recv_timeout(Duration::from_millis(100)) {
                    Ok(Some(request)) => request,
                    Ok(None) => continue,
                    Err(_) => break,
                }
            }
        };
        // Thread per request: `/api/stream` holds its connection open
        // for the session's lifetime and must not starve the board.
        let (state_dir, pm_dir, opts) =
            (state_dir.to_path_buf(), pm_dir.to_path_buf(), opts.clone());
        std::thread::spawn(move || {
            // CAD-777: the device sign-in exchange is exempt — its
            // handlers make issuer HTTP calls (up to 20 s each), so a
            // slow issuer or a `/code` spammer would stall every
            // unrelated board write. Safe: their only shared state is
            // the pending map under its own mutex, and the daemon
            // serializes the mint itself.
            let is_write = matches!(
                request.method(),
                Method::Post | Method::Patch | Method::Delete
            ) && !matches!(
                request.url().split('?').next().unwrap_or(""),
                "/api/session/device/code" | "/api/session/device/poll"
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

/// The device trust pin's advisory lock — a serving board holds
/// `flock` on it for `serve()`'s whole life (released by the kernel
/// on exit), so a second `ui run` on ANY port cannot rewrite or
/// clear the pin under a live board (review r4). Lives in the `0700`
/// operator dir next to the pin.
const DEVICE_PIN_LOCK: &str = "device-login.lock";

/// Try to take [`DEVICE_PIN_LOCK`]. `wait` retries up to 5 s so a
/// restart's old-board/new-child handoff (`ui tailscale start`,
/// `ui start` after `ui stop`) does not race the exiting holder;
/// `!wait` is a single non-blocking attempt — an unconfigured board
/// never stalls on a lock it does not need (review r6). `Ok(Some)` —
/// this board owns the pin; `Ok(None)` — another live board does.
fn device_pin_lock(state_dir: &Path, wait: bool) -> Result<Option<std::fs::File>> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    let dir = crate::operator_auth::checked_dir(state_dir)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(dir.join(DEVICE_PIN_LOCK))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Some(file));
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::WouldBlock {
            return Err(Error::internal(format!("device pin lock: {error}")));
        }
        if !wait || Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Write or clear the daemon's device trust pin for this board
/// (`opts.device_login` ⇔ the pin file), guarded by the lock. The
/// returned File must stay bound for the rest of `serve()`. While
/// ANOTHER board holds the lock: a board WITH device login refuses
/// to start — nothing is written; a board WITHOUT it serves anyway
/// but never clears the pin (two unconfigured boards on one state
/// dir keeps working as before).
fn pin_device_login(state_dir: &Path, opts: &ServeOpts) -> Result<Option<std::fs::File>> {
    // Only a board that would WRITE the pin waits out the handoff —
    // a board with no device login takes one non-blocking look: on
    // success it clears a stale pin, and either way it serves at once.
    let lock = device_pin_lock(state_dir, opts.device_login.is_some())?;
    if let Some(login) = opts
        .device_login
        .as_ref()
        .map(|login| crate::device_login::DevicePin {
            issuer: login.config.issuer().to_string(),
            org: login.config.org().to_string(),
            subjects: login.subjects.clone(),
        })
    {
        if lock.is_none() {
            return Err(Error::rejected(
                "device login is pinned by another live board on this state dir — \
                 stop it first",
            ));
        }
        crate::device_login::write_pin(state_dir, &login)?;
    } else if lock.is_some() {
        crate::device_login::clear_pin(state_dir)?;
    }
    Ok(lock)
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
fn run(state_dir: &Path, flags: &UiFlags) -> Result<i32> {
    let persisted = load_opts(state_dir);
    let (_eff, so) = resolve_opts(flags, &persisted)?;
    serve(state_dir, &crate::issue::default_dir()?, &so)?;
    Ok(0)
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
    let running = read_pid(state_dir);
    if running.is_some() {
        if eff.board_public_only != recorded.board_public_only {
            return Err(Error::rejected(
                "board public-only mode cannot change while the UI is running — stop the UI, then start it with the new mode",
            ));
        }
        // CAD-777: the device trust pin is written once at board
        // start; a live board serves its in-memory triple. Refuse any
        // change (enable, disable, re-point, re-subject) while running
        // so the pin file, the saved options and the live routes cannot
        // drift apart — stop the UI, then start it with the new values.
        if eff.device_login != recorded.device_login {
            return Err(Error::rejected(
                "device login configuration cannot change while the UI is running — stop the UI, then start it with the new configuration",
            ));
        }
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
    if let Some(pid) = running {
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
    // CAD-482: a board is never a caller either — requests assert via
    // headers, so a `CADENCE_TEST_AS` in this process's env must not
    // leak into the child (a `daemon restart --ui` under it would
    // blanket-assert every board→daemon RPC).
    command.env_remove(crate::test_seam::AS_ENV);
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
    let mut child = crate::reaper::spawn(&mut command)?;
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
                        // CAD-313: board writes need the operator's session.
                        "sign_in": "cadence ui login",
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
            // CAD-526: the public sign-in surface, when configured.
            "board_url": opts.board.as_ref().map(|b| format!(
                "{}://{}", operator::public_scheme(&b.host), b.host)),
            "board_issuer": opts.board.as_ref().map(|b| b.issuer.clone()),
            "board_company": opts.board.as_ref().map(|b| b.company.clone()),
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
    // The tailnet is host-wide: a sandbox board never goes on it.
    crate::sandbox::refuse_global("`tailscale serve`")?;
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
    // The identity probe (CAD-336): a local request shaped like the
    // proxy's — loopback peer, tailnet Host, a login header — is NOT
    // the proxy and must resolve to the plain operator. The real login
    // shows only through the tailnet URL (`<url>/api/meta`).
    if read_pid(state_dir).is_some() {
        let ui_port = opts.port.unwrap_or(3010);
        let host_hdr = format!("{}:{}", ts.dns_name, ts.https_port);
        let forged = "forged-probe@cadence.invalid";
        let login_hdr = format!("Tailscale-User-Login: {forged}");
        match http_get(
            "127.0.0.1",
            ui_port,
            "/api/meta",
            &host_hdr,
            &[login_hdr.as_str()],
        ) {
            Ok((200, body)) => {
                let meta = serde_json::from_str::<Value>(&body).unwrap_or_default();
                let actor = meta["actor"].as_str().unwrap_or_default();
                let check = meta["tailnet_proof"]["check"].as_str().unwrap_or("?");
                if actor.contains(forged) {
                    println!(
                        "identity: FORGEABLE — a local process posing as the proxy \
                         resolved to {actor}"
                    );
                } else {
                    println!(
                        "identity: local forged login ignored ({actor}; refused by \
                         check {check}); tailnet logins resolve only via {url}/api/meta"
                    );
                    println!(
                        "advice:   `cadence doctor --host` runs the whole tailnet \
                         proof up front and prints every remedy in order"
                    );
                }
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
    use super::{
        agents_payload_from, content_type, context_query, health_supports_model_defaults,
        proxied_actor, resolve_device_login, running_json, static_answer_for, static_file,
        DeviceLoginOpts, StaticAnswer, UiFlags, UiOpts,
    };
    use serde_json::{json, Value};
    use tiny_http::{Header, Response};

    #[test]
    fn board_csp_allows_only_reviewed_social_preview_image_hosts() {
        let mut response = Response::from_string("<html></html>");
        response.add_header(Header::from_bytes("Content-Type", "text/html").unwrap());
        assert!(super::add_security_headers(&mut response));
        let policy = response
            .headers()
            .iter()
            .find(|header| header.field.equiv("Content-Security-Policy"))
            .unwrap()
            .value
            .as_str();
        let image_sources = policy
            .split(';')
            .map(str::trim)
            .find_map(|directive| directive.strip_prefix("img-src "))
            .unwrap();
        assert_eq!(
            image_sources.split_whitespace().collect::<Vec<_>>(),
            [
                "'self'",
                "data:",
                "https://cdninstagram.com",
                "https://*.cdninstagram.com",
                "https://fbcdn.net",
                "https://*.fbcdn.net",
            ]
        );
        assert!(policy.contains("default-src 'self'"));
        assert!(policy.contains("base-uri 'none'"));
        assert!(policy.contains("frame-ancestors 'none'"));
        assert!(!policy
            .split_whitespace()
            .any(|source| source == "https:" || source == "*"));
    }

    #[test]
    fn hosted_public_only_is_explicit_and_requires_public_identity() {
        let mut flags = super::UiFlags::default();
        let empty = super::UiOpts::default();
        assert!(
            !super::resolve_opts(&flags, &empty)
                .unwrap()
                .0
                .board_public_only
        );
        flags.board_public_only = true;
        assert!(super::resolve_opts(&flags, &empty).is_err());

        let board = super::PublicBoard {
            host: "acme.board.localhost:3111".to_string(),
            issuer: "http://api.internal".to_string(),
            company: "co_1".to_string(),
            authorize_url: "http://api.internal/v2/board/authorize".to_string(),
        };
        let persisted = super::UiOpts {
            board: Some(board),
            board_public_only: true,
            ..Default::default()
        };
        let encoded = serde_json::to_vec(&persisted).unwrap();
        let restored: super::UiOpts = serde_json::from_slice(&encoded).unwrap();
        assert!(
            super::resolve_opts(&super::UiFlags::default(), &restored)
                .unwrap()
                .1
                .board_public_only
        );
    }

    #[test]
    fn standalone_board_without_uid_record_does_not_need_daemon_health() {
        let state = tempfile::TempDir::new().unwrap();
        assert_eq!(
            super::board_boot_agent_uid(state.path(), None).unwrap(),
            None
        );
        assert_eq!(
            super::board_boot_agent_uid(state.path(), Some(2200)).unwrap(),
            Some(2200)
        );
    }

    #[test]
    fn historical_uid_mode_cannot_start_standalone_after_record_loss() {
        use std::os::unix::fs::PermissionsExt;

        let state = tempfile::TempDir::new().unwrap();
        std::fs::set_permissions(state.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let marker = state.path().join(crate::agent_uid::config::MODE_MARKER);
        std::fs::write(&marker, br#"{"uid":2200}"#).unwrap();
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600)).unwrap();
        let error = super::board_boot_agent_uid(state.path(), None).unwrap_err();
        assert!(
            error.to_string().contains("no private daemon boot pin"),
            "{error}"
        );
    }

    #[test]
    fn first_uid_provisioning_requires_matching_private_daemon_boot() {
        for health in [Ok(None), Ok(Some(3300))] {
            assert!(
                super::reconcile_board_agent_uid(Some(2200), None, health).is_err(),
                "configured UID must match private daemon boot before board startup"
            );
        }
        assert_eq!(
            super::reconcile_board_agent_uid(Some(2200), None, Ok(Some(2200))).unwrap(),
            Some(2200)
        );
    }

    #[test]
    fn agent_activity_uses_latest_valid_instant_across_timestamp_shapes() {
        let dir = tempfile::TempDir::new().unwrap();
        let list = json!({"agents": [{
            "alias": "worker", "provider": "fake", "endpoint_kind": "fake",
            "state": "idle", "params": Value::Null,
            "board": {"messages": [
                {"created": "not-a-time"},
                {"created": 1790562108.64},
                {"created": "2026-09-29T03:00:00+02:00"},
                {"created": "2026-02-31T04:00:00Z"}
            ]}
        }]});
        let out = agents_payload_from(dir.path(), Some(list), None);
        assert_eq!(
            out["agents"][0]["last_activity"],
            "2026-09-29T03:00:00+02:00"
        );
        assert_eq!(super::agent_activity_seconds(&json!("not-a-time")), None);
        assert_eq!(
            super::agent_activity_seconds(&json!("2026-02-31T04:00:00Z")),
            None
        );
        assert_eq!(
            super::agent_activity_seconds(&json!("2026-09-28T02:21:48.640Z")),
            Some(1790562108.64)
        );
        assert_eq!(
            super::agent_activity_seconds(&json!("2026-09-28T04:21:48.640+02:00")),
            Some(1790562108.64)
        );
    }

    /// CAD-480: a mailbox row on the Agents screen carries its unread
    /// backlog and oldest-unread age from `agent.inbox`, and the unread
    /// count folds into the queued total.
    #[test]
    fn stream_hello_capability_is_opt_in() {
        assert_eq!(
            super::stream_hello(false),
            format!(
                "event: hello\ndata: {}\n\n",
                json!({"build": crate::overview::BUILD_ID})
            ),
            "legacy hello remains byte-compatible"
        );
        assert_eq!(
            super::stream_hello(true),
            format!(
                "event: hello\ndata: {}\n\n",
                json!({"build": crate::overview::BUILD_ID, "entities": true})
            ),
            "only opted-in clients receive the entity capability"
        );
    }

    #[test]
    fn agents_payload_inbox_row_reports_unread_backlog() {
        let dir = tempfile::TempDir::new().unwrap();
        let list = json!({"agents": [{
            "alias": "obs", "provider": "inbox", "endpoint_kind": "inbox",
            "role": Value::Null, "team_role": Value::Null,
            "model_selection": Value::Null, "model_lookup_role": Value::Null,
            "model": Value::Null, "model_reported": Value::Null,
            "model_configured": Value::Null, "model_source": Value::Null,
            "effort": Value::Null, "effort_reported": Value::Null,
            "effort_source": Value::Null, "effort_applicable": Value::Null,
            "quota": Value::Null, "usage_limit": Value::Null,
            "thread_id": "", "session_id": "", "endpoint": "inbox://obs",
            "params": Value::Null, "dead": false,
            "inbox": {"queued": 3, "oldest_age_secs": 42.0},
        }]});
        let out = agents_payload_from(dir.path(), Some(list), None);
        let row = &out["agents"][0];
        assert_eq!(row["inbox"], true, "{row}");
        assert_eq!(row["queued"], 3, "{row}");
        assert_eq!(row["unread"], 3, "{row}");
        assert_eq!(row["oldest_unread_age_secs"], 42.0, "{row}");
        assert_eq!(out["totals"]["queued"], 3, "{out}");
    }

    #[test]
    fn context_query_stores_devops_and_accepts_ops() {
        for raw in ["role=devops", "role=ops"] {
            let (role, _) = context_query(raw).unwrap();
            assert_eq!(role.as_deref(), Some("devops"), "{raw}");
        }
        assert_eq!(context_query("role=pm").unwrap().0.as_deref(), Some("pm"));
        assert_eq!(context_query("role=operations"), Err("bad context role"));
        assert_eq!(context_query("role=OPS"), Err("bad context role"));
    }

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

    /// Client routes get the SPA shell so deep links and refreshes work;
    /// build files come back as themselves; a missing file is a 404, never
    /// the shell under a script's name.
    #[test]
    fn client_routes_get_the_spa_shell_without_shadowing_files() {
        let dist = tempfile::TempDir::new().unwrap();
        std::fs::write(dist.path().join("index.html"), "<!doctype html>shell").unwrap();
        std::fs::write(dist.path().join("favicon.svg"), "<svg/>").unwrap();
        std::fs::create_dir(dist.path().join("assets")).unwrap();
        std::fs::write(dist.path().join("assets/index.js"), "js").unwrap();
        let answer = |path: &str| static_answer_for(Some(dist.path()), path, None);
        for route in [
            "/",
            "/projects",
            "/projects/cadence",
            "/projects/cadence/context",
            "/agents",
            "/agents/cc-1",
            "/setup",
            "/settings",
            "/settings/memory",
            "/login",
            "/unknown-page",
            "/agents/cc.worker-1",
        ] {
            match answer(route) {
                StaticAnswer::File(name, bytes) => {
                    assert_eq!(name, "/index.html", "{route}");
                    assert_eq!(content_type(&name), "text/html; charset=utf-8", "{route}");
                    assert_eq!(bytes, b"<!doctype html>shell", "{route}");
                }
                _ => panic!("{route}: expected the SPA shell"),
            }
        }
        match answer("/assets/index.js") {
            StaticAnswer::File(name, bytes) => {
                assert_eq!(name, "/assets/index.js");
                assert_eq!(bytes, b"js");
            }
            _ => panic!("asset not served"),
        }
        match answer("/favicon.svg") {
            StaticAnswer::File(_, bytes) => assert_eq!(bytes, b"<svg/>"),
            _ => panic!("favicon not served"),
        }
        for missing in [
            "/assets/gone.js",
            "/assets/chunk",
            "/gone.css",
            "/projects/x/logo.png",
            "/agents/a/b.js",
        ] {
            assert!(
                matches!(answer(missing), StaticAnswer::Missing),
                "{missing}"
            );
        }
        // /api never falls through to the shell, even with a file of that name.
        std::fs::create_dir(dist.path().join("api")).unwrap();
        std::fs::write(dist.path().join("api/issues"), "x").unwrap();
        assert!(matches!(answer("/api/issues"), StaticAnswer::Missing));
        assert!(matches!(answer("/api"), StaticAnswer::Missing));
        // No build: routes say so instead of serving nothing.
        let empty = tempfile::TempDir::new().unwrap();
        assert!(matches!(
            static_answer_for(Some(empty.path()), "/projects", None),
            StaticAnswer::NoBuild
        ));
    }

    /// CAD-609: a wiki page and a dotted issue path are client routes.
    /// The shell is HTML 200. A JSON Accept on `/wiki` stays the JSON
    /// 404, and `/api/wiki/file` is never the shell — that route serves
    /// the bytes.
    #[test]
    fn wiki_pages_and_dotted_issue_paths_serve_the_spa_shell() {
        let dist = tempfile::TempDir::new().unwrap();
        std::fs::write(dist.path().join("index.html"), "<!doctype html>shell").unwrap();
        let shell = |path: &str, accept: Option<&str>| match static_answer_for(
            Some(dist.path()),
            path,
            accept,
        ) {
            StaticAnswer::File(name, bytes) => {
                let resp = Response::from_data(bytes.clone());
                assert_eq!(resp.status_code().0, 200, "{path}");
                assert_eq!(name, "/index.html", "{path}");
                assert_eq!(content_type(&name), "text/html; charset=utf-8", "{path}");
                assert_eq!(bytes, b"<!doctype html>shell", "{path}");
            }
            other => panic!("{path}: expected the SPA shell, got {other:?}"),
        };
        for path in [
            "/wiki",
            "/wiki/global",
            "/wiki/global/x.md",
            "/wiki/global/hello.md",
            "/wiki/edit/global/hello.md",
            "/wiki/history/global/hello.md",
            "/projects/cadence/issues/CAD-1",
            "/projects/cadence/issues/CAD-1.2",
            "/agents/cc.worker-1",
        ] {
            shell(path, None);
            shell(path, Some("text/html,application/xhtml+xml"));
        }
        assert!(
            matches!(
                static_answer_for(
                    Some(dist.path()),
                    "/wiki/global/x.md",
                    Some("application/json"),
                ),
                StaticAnswer::Missing
            ),
            "JSON Accept on a wiki page is not the shell"
        );
        assert!(
            matches!(
                static_answer_for(Some(dist.path()), "/api/wiki/file", None),
                StaticAnswer::Missing
            ),
            "file bytes stay on the API route"
        );
        assert!(matches!(
            static_answer_for(Some(dist.path()), "/projects/x/logo.png", None),
            StaticAnswer::Missing
        ));
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

    /// CAD-336: a proven serve-proxy request writes as its login, and
    /// one without a usable login (Funnel, a tagged node) is refused —
    /// never `operator (ui)`. Unit-level: a board test cannot be the
    /// proxy (its socket is its own uid, never tailscaled's), so the
    /// proven branch is reached only through this function.
    #[test]
    fn a_proven_proxy_request_without_a_login_names_nobody() {
        assert_eq!(
            proxied_actor(Some("fable@example.com")),
            Ok("fable@example.com (tailscale)".to_string())
        );
        assert!(proxied_actor(None).is_err());
        assert!(proxied_actor(Some("")).is_err());
        assert!(proxied_actor(Some("   ")).is_err());
        assert!(proxied_actor(Some("bad\u{1}login")).is_err());
    }

    /// CAD-777: the device-login triple resolves flag → env →
    /// persisted — issuer + org + at least one subject, or none. A
    /// partial combination is an operator error naming the missing
    /// piece. Env is restored after each case so parallel runners
    /// sharing the process see no leak.
    #[test]
    fn device_login_triple_resolves_all_or_nothing() {
        fn persisted(triple: Option<(&str, &str, &[&str])>) -> UiOpts {
            UiOpts {
                device_login: triple.map(|(issuer, org, subjects)| DeviceLoginOpts {
                    issuer: issuer.to_string(),
                    org: org.to_string(),
                    subjects: subjects.iter().map(|s| s.to_string()).collect(),
                }),
                ..Default::default()
            }
        }
        fn flags(issuer: Option<&str>, org: Option<&str>, subjects: &[&str]) -> UiFlags {
            UiFlags {
                device_login_issuer: issuer.map(str::to_string),
                device_login_org: org.map(str::to_string),
                device_login_subject: subjects.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            }
        }
        struct EnvGuard;
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                std::env::remove_var("CADENCE_DEVICE_LOGIN_ISSUER");
                std::env::remove_var("CADENCE_DEVICE_LOGIN_ORG");
                std::env::remove_var("CADENCE_DEVICE_LOGIN_SUBJECTS");
            }
        }
        let _guard = EnvGuard;
        // Nothing anywhere: off.
        assert!(
            resolve_device_login(&flags(None, None, &[]), &persisted(None))
                .unwrap()
                .is_none()
        );
        // Full flags win and name all three.
        let triple = resolve_device_login(
            &flags(Some("https://issuer.example"), Some("ws_co"), &["op_1"]),
            &persisted(None),
        )
        .unwrap()
        .unwrap();
        assert_eq!(triple.issuer, "https://issuer.example");
        assert_eq!(triple.subjects, vec!["op_1".to_string()]);
        // Every partial combination refuses, and the error names the
        // missing piece.
        let err = resolve_device_login(
            &flags(Some("https://issuer.example"), Some("ws_co"), &[]),
            &persisted(None),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("--device-login-subject"), "{err}");
        let err = resolve_device_login(
            &flags(Some("https://issuer.example"), None, &["op_1"]),
            &persisted(None),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("--device-login-org"), "{err}");
        let err = resolve_device_login(&flags(None, None, &["op_1"]), &persisted(None))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--device-login-issuer"), "{err}");
        assert!(err.contains("--device-login-org"), "{err}");
        // A flag pair alone still falls through to persisted subjects.
        assert!(resolve_device_login(
            &flags(None, Some("ws_co"), &[]),
            &persisted(Some(("https://issuer.example", "ws_co", &["op_2"])))
        )
        .unwrap()
        .is_some());
        // Env fills the gaps; the subject list is comma-separated,
        // trimmed, empties dropped. Whitespace-only counts as absent.
        std::env::set_var("CADENCE_DEVICE_LOGIN_ISSUER", "https://env.example");
        std::env::set_var("CADENCE_DEVICE_LOGIN_ORG", "ws_env");
        std::env::set_var("CADENCE_DEVICE_LOGIN_SUBJECTS", " op_3 , ,op_4 ,");
        let triple = resolve_device_login(&flags(None, None, &[]), &persisted(None))
            .unwrap()
            .unwrap();
        assert_eq!(triple.org, "ws_env");
        assert_eq!(
            triple.subjects,
            vec!["op_3".to_string(), "op_4".to_string()]
        );
        // Flag subjects beat the env list.
        let triple = resolve_device_login(&flags(None, None, &["op_9"]), &persisted(None))
            .unwrap()
            .unwrap();
        assert_eq!(triple.subjects, vec!["op_9".to_string()]);
        // An env that yields no subject still misses the piece.
        std::env::set_var("CADENCE_DEVICE_LOGIN_SUBJECTS", " , ,");
        assert!(resolve_device_login(&flags(None, None, &[]), &persisted(None)).is_err());
        std::env::remove_var("CADENCE_DEVICE_LOGIN_ISSUER");
        std::env::remove_var("CADENCE_DEVICE_LOGIN_ORG");
        std::env::remove_var("CADENCE_DEVICE_LOGIN_SUBJECTS");
        // Persisted values survive when nothing overrides them.
        assert!(resolve_device_login(
            &flags(None, None, &[]),
            &persisted(Some(("https://saved.example", "ws_saved", &["op_7"])))
        )
        .unwrap()
        .is_some());
        // Persisted issuer/org without subjects is a partial block —
        // refused, never silently on.
        assert!(resolve_device_login(
            &flags(None, None, &[]),
            &persisted(Some(("https://saved.example", "ws_saved", &[])))
        )
        .is_err());
    }
}
