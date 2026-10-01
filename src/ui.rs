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

use crate::error::{Error, Result};
use crate::proc::{self, BoundedError};

mod app_audiences;
mod app_content;
mod app_contexts;
mod app_records;
mod app_release;
mod app_runs;
mod apps;
mod connections;
mod crm_send;
mod crm_smtp;
pub mod delivery_sync;
mod home;
mod lane;
mod login;
mod operator;
mod platform_account;
mod read_model;
mod serve;
mod setup;
mod social_publish;
mod stages;
mod stream;
mod threads;
mod updates;
mod wiki;
mod workflows;
mod write_path;

pub use operator::{route_class, RouteClass, WriteRoute, WRITE_ROUTES};
pub use serve::serve;
pub use setup::{read_model_stats, setup_runs};

// Re-export the moved sections' shared items so the sibling submodules'
// `use super::{…}` and the tests' `super::` keep resolving — the public
// surface of `crate::ui` is unchanged by the split.
pub(crate) use serve::{
    agents_payload_from, board_job_list, err_response, json_response, pct_decode,
};
pub(crate) use stream::{dir_mtime, event_resources, projects_payload, value_fp};
pub(crate) use write_path::{
    agent_roots, coded_response, guard_fail, header_value, issue_payloads, parse_json,
    proxied_actor, read_body, tailnet_proxy, with_agents, write_err, write_guard, write_reply,
    HttpResp, JSON_CAP, UI_ACTOR,
};

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
    /// CAD-777/CAD-841: allow remote operator sign-in through the
    /// AgenticOS device grant — the issuer origin that mints the
    /// grant. A thin client of `cadence ui device-login set`: this
    /// process pushes the resolved triple to the daemon (operator
    /// proof + the operator secret) before serving, and the daemon's
    /// own store then owns it — nothing is persisted in ui.json and a
    /// board restart never changes it. Requires `--device-login-org`
    /// (env `CADENCE_DEVICE_LOGIN_ORG` is the fallback). Default: off.
    #[arg(long)]
    pub device_login_issuer: Option<String>,
    /// CAD-777/CAD-841: the exact workspace the device sign-in is for.
    /// Requires `--device-login-issuer` (env
    /// `CADENCE_DEVICE_LOGIN_ISSUER` as fallback).
    #[arg(long)]
    pub device_login_org: Option<String>,
    /// CAD-777/CAD-841: an issuer subject allowed to sign in —
    /// repeatable, once per operator (`cadence auth status` prints
    /// yours under `principal.subject_id`). Required with the
    /// issuer/org pair; env `CADENCE_DEVICE_LOGIN_SUBJECTS`
    /// (comma-separated) is the fallback.
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
    /// Manage remote sign-in through the AgenticOS device grant
    /// (CAD-777/CAD-841): set or clear the daemon-owned issuer +
    /// workspace + subject allowlist. Applies live to every board on
    /// this state dir — no restart. Operator-only: needs positive
    /// operator proof and the operator secret, like `ui login`.
    DeviceLogin {
        #[command(subcommand)]
        action: DeviceLoginAction,
    },
    /// Share the board over the tailnet (`tailscale serve`, never
    /// funnel). The primary UX for phone/laptop access.
    Tailscale {
        #[command(subcommand)]
        action: TailscaleAction,
    },
}

#[derive(Subcommand)]
pub enum DeviceLoginAction {
    /// Point the device sign-in at an AgenticOS issuer + workspace and
    /// name the subjects who may sign in. Replaces the whole triple —
    /// a later `set` rotates it, `clear` removes it.
    Set {
        /// The issuer origin that mints the grant (https://…).
        #[arg(long)]
        issuer: String,
        /// The exact workspace/org id the sign-in is for.
        #[arg(long)]
        org: String,
        /// An issuer subject allowed to sign in — repeatable, once
        /// per operator (`cadence auth status` prints yours under
        /// `principal.subject_id`).
        #[arg(long, required = true)]
        subject: Vec<String>,
    },
    /// Turn remote sign-in off: the mint path and the board routes
    /// fail closed until `set` runs again. Live sessions already
    /// minted keep running — `ui sessions --revoke-all` ends those.
    Clear,
    /// Show the configured issuer, workspace and subject allowlist.
    Show {
        /// Print `{configured, issuer, org, subjects}` as JSON.
        #[arg(long)]
        json: bool,
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
        UiAction::DeviceLogin { action } => login::device_login(state_dir, action),
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
pub(crate) fn tailnet_url(dns_name: &str, https_port: u16) -> String {
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
/// allowlist as the operator supplied them — flags win, then the
/// `CADENCE_DEVICE_LOGIN_*` env. All or none — a partial triple never
/// resolves. CAD-841: this is only the push payload for
/// `operator_device_login_set`; the daemon's own store is the
/// authority, ui.json never holds it.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct DeviceLoginOpts {
    pub issuer: String,
    pub org: String,
    /// The verified issuer subjects allowed a board session.
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

/// The board's device-login machinery (CAD-777/CAD-841): the live
/// pending map plus the issuer transport. The issuer + workspace +
/// allowlist are NOT here — they are the daemon's config, read
/// per-request through `device_login_config`, so a `set`/`clear`
/// takes effect on a running board without a restart.
#[derive(Clone)]
pub struct DeviceLogin {
    pub pending: std::sync::Arc<std::sync::Mutex<HashMap<String, DevicePending>>>,
    /// The issuer transport — live ureq unless a test injects a fake.
    pub transport: std::sync::Arc<dyn crate::device_login::IssuerTransport>,
}

impl Default for DeviceLogin {
    fn default() -> Self {
        Self {
            pending: Default::default(),
            transport: std::sync::Arc::new(crate::device_login::UreqTransport::new()),
        }
    }
}

/// Live device grants awaiting approval are bounded: past this many,
/// `/api/session/device/code` refuses with 429 until one settles.
const DEVICE_PENDING_CAP: usize = 16;

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
    /// The readiness nonce `ui start` handed this child through its
    /// environment — see [`READY_NONCE_ENV`]. Written to `ui.ready` only
    /// after the bind succeeds; never served over HTTP. [`serve`] reads
    /// it once and removes it from this process's own environment.
    pub ready_nonce: Option<String>,
    /// CAD-526: this board's public AgenticOS name, when configured.
    /// Requests that carry its Host are the platform sign-in surface —
    /// `__platform/*` routes and `__Host-aos-board-session` reads —
    /// never the local login flow.
    pub public: Option<PublicBoard>,
    /// CAD-777/CAD-841: the board's device-login machinery — pending
    /// map + issuer transport. Whether the flow is ON is the daemon's
    /// answer (`device_login_config` RPC), read per request; the
    /// board never owns configuration. Never set from the command
    /// line — tests inject a fake issuer transport.
    pub device_login: DeviceLogin,
    /// CAD-841: a resolved `--device-login-*` triple `serve` pushes to
    /// the daemon — only AFTER the port is bound, so a failed start
    /// never replaces live sign-in settings on other boards of this
    /// state dir (review r1). `ui run` fills it from its flags;
    /// `ui start`'s parent pushes itself post-spawn (the detached
    /// child cannot prove operator), and tests leave it `None`.
    pub device_login_push: Option<DeviceLoginOpts>,
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

pub(crate) fn opts_file(state_dir: &Path) -> PathBuf {
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

pub(crate) fn load_opts(state_dir: &Path) -> UiOpts {
    let Ok(bytes) = std::fs::read(opts_file(state_dir)) else {
        return UiOpts::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        eprintln!("warning: ignoring unreadable ui.json: {e}");
        UiOpts::default()
    })
}

pub(crate) fn save_opts(state_dir: &Path, opts: &UiOpts) -> Result<()> {
    let path = opts_file(state_dir);
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(opts)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub(crate) fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    matches!(h.as_str(), "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

/// Merge flags over the persisted options: a given flag wins, an
/// absent one inherits. `--tailscale` resolves the tailnet identity
/// and ensures the serve mapping; a persisted tailscale block is kept
/// (the detached server never re-ensures — only operator verbs do).
pub(crate) fn resolve_opts(flags: &UiFlags, persisted: &UiOpts) -> Result<(UiOpts, ServeOpts)> {
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

/// CAD-841: the `--device-login-*` flags (and their
/// `CADENCE_DEVICE_LOGIN_*` fallbacks) as a thin client of
/// `operator_device_login_set`. Flags win, then env — never a
/// persisted block: the daemon's own store is the persistence now.
/// Issuer + org + at least one subject, or none of it; a partial
/// combination is an operator error naming the missing piece, never
/// a silent half trust root. The daemon validates the resolved
/// triple authoritatively in the RPC.
pub(crate) fn resolve_device_login(flags: &UiFlags) -> Result<Option<DeviceLoginOpts>> {
    let field = |flag: Option<&String>, env: &str| {
        flag.cloned()
            .or_else(|| std::env::var(env).ok())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let issuer = field(
        flags.device_login_issuer.as_ref(),
        "CADENCE_DEVICE_LOGIN_ISSUER",
    );
    let org = field(flags.device_login_org.as_ref(), "CADENCE_DEVICE_LOGIN_ORG");
    // Subjects resolve as a list: any flag beats the env list. Env is
    // comma-separated, trimmed, empties dropped.
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
        Vec::new()
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

/// Push an already-resolved `--device-login-*` triple to the daemon —
/// `operator_device_login_set` carrying the operator secret, so it
/// only succeeds for a provably-operator caller against a live daemon
/// (`ui run`/`ui start`'s thin-client half of `cadence ui device-login
/// set`, CAD-841). Callers choose WHEN: `ui run` resolves eagerly but
/// pushes inside `serve` once the port is bound; `ui start` resolves
/// before spawn and pushes from this process after the child proves
/// it serves — a detached child sees no device flags and a scrubbed
/// env, so it never re-pushes. Absent flags resolve to `None` and
/// daemon state stays untouched — it no longer takes a board start to
/// change it.
pub(crate) fn push_device_login_config(state_dir: &Path, triple: &DeviceLoginOpts) -> Result<()> {
    let secret = crate::operator_auth::read_secret(state_dir)?;
    crate::client::rpc(
        state_dir,
        "operator_device_login_set",
        json!({
            "secret": secret,
            "issuer": triple.issuer,
            "org": triple.org,
            "subjects": triple.subjects,
        }),
    )?;
    Ok(())
}

/// CAD-526: merge the board-identity configuration — flags win, then
/// `AGENTICOS_BOARD_*` env (how the hosted container is told), then the
/// persisted block. All of host/issuer/company must resolve together.
/// When a block resolves, its trust root (`host`, `issuer`, `company`)
/// is written to the daemon-owned `operator/board-identity.json` so the
/// board RPC has the same root the session check enforces.
pub(crate) fn resolve_board(flags: &UiFlags, persisted: &UiOpts) -> Result<Option<PublicBoard>> {
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
pub(crate) fn serve_opts(eff: &UiOpts) -> Result<ServeOpts> {
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
        // `ui run` reads `CADENCE_UI_READY_NONCE` itself; nothing here
        // copies a caller's env into the field.
        ready_nonce: None,
        public: eff.board.clone(),
        // CAD-841: the board's half of device login is only the
        // pending map + issuer transport — configuration lives in the
        // daemon store and is read per request, so a `device-login
        // set`/`clear` applies to a running board at once. The
        // flag-push payload is `run`'s business, not resolved options'.
        device_login: DeviceLogin::default(),
        device_login_push: None,
        // CAD-482: `ui run`/`ui start`'s fixture child arms from its
        // environment; in-process fixtures set the field directly.
        test_seam: crate::test_seam::env_armed(),
        seam: None,
        // CAD-561 r2: a real board spawns its own binary as the update
        // helper; only tests inject a fake.
        update_helper: None,
    })
}

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

// ---------- tailscale (serve only — never funnel) ----------

/// One bounded `tailscale` invocation — the only way cadence talks to
/// it, and `funnel` is never among the args.
pub(crate) fn ts(args: &[&str]) -> Result<std::process::Output> {
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
            crate::output::json_text(&json!({
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
            crate::output::json_text(&json!({"state": "not_sharing"})).unwrap_or_default()
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
        crate::output::json_text(&json!({
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
    use super::serve::{
        agents_payload_from, content_type, context_query, running_json, static_answer_for,
        static_file, StaticAnswer,
    };
    use super::write_path::{health_supports_model_defaults, proxied_actor};
    use super::{resolve_device_login, UiFlags, UiOpts};
    use serde_json::{json, Value};
    use tiny_http::{Header, Response};

    #[test]
    fn board_csp_allows_only_reviewed_social_preview_image_hosts() {
        let mut response = Response::from_string("<html></html>");
        response.add_header(Header::from_bytes("Content-Type", "text/html").unwrap());
        assert!(super::serve::add_security_headers(&mut response));
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
            super::setup::board_boot_agent_uid(state.path(), None).unwrap(),
            None
        );
        assert_eq!(
            super::setup::board_boot_agent_uid(state.path(), Some(2200)).unwrap(),
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
        let error = super::setup::board_boot_agent_uid(state.path(), None).unwrap_err();
        assert!(
            error.to_string().contains("no private daemon boot pin"),
            "{error}"
        );
    }

    #[test]
    fn first_uid_provisioning_requires_matching_private_daemon_boot() {
        for health in [Ok(None), Ok(Some(3300))] {
            assert!(
                super::setup::reconcile_board_agent_uid(Some(2200), None, health).is_err(),
                "configured UID must match private daemon boot before board startup"
            );
        }
        assert_eq!(
            super::setup::reconcile_board_agent_uid(Some(2200), None, Ok(Some(2200))).unwrap(),
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
        assert_eq!(
            super::serve::agent_activity_seconds(&json!("not-a-time")),
            None
        );
        assert_eq!(
            super::serve::agent_activity_seconds(&json!("2026-02-31T04:00:00Z")),
            None
        );
        assert_eq!(
            super::serve::agent_activity_seconds(&json!("2026-09-28T02:21:48.640Z")),
            Some(1790562108.64)
        );
        assert_eq!(
            super::serve::agent_activity_seconds(&json!("2026-09-28T04:21:48.640+02:00")),
            Some(1790562108.64)
        );
    }

    /// CAD-480: a mailbox row on the Agents screen carries its unread
    /// backlog and oldest-unread age from `agent.inbox`, and the unread
    /// count folds into the queued total.
    #[test]
    fn stream_hello_capability_is_opt_in() {
        assert_eq!(
            super::stream::stream_hello(false),
            format!(
                "event: hello\ndata: {}\n\n",
                json!({"build": crate::overview::BUILD_ID})
            ),
            "legacy hello remains byte-compatible"
        );
        assert_eq!(
            super::stream::stream_hello(true),
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

    /// CAD-841: the device-login triple resolves flag → env — issuer +
    /// org + at least one subject, or none. `ui.json` is no longer a
    /// source (the daemon store is the persistence), and a stale
    /// `device_login` key in an old ui.json is ignored rather than
    /// resurrected. A partial combination is an operator error naming
    /// the missing piece. Env is restored after each case so parallel
    /// runners sharing the process see no leak.
    #[test]
    fn device_login_triple_resolves_all_or_nothing() {
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
        assert!(resolve_device_login(&flags(None, None, &[]))
            .unwrap()
            .is_none());
        // Full flags name all three.
        let triple = resolve_device_login(&flags(
            Some("https://issuer.example"),
            Some("ws_co"),
            &["op_1"],
        ))
        .unwrap()
        .unwrap();
        assert_eq!(triple.issuer, "https://issuer.example");
        assert_eq!(triple.subjects, vec!["op_1".to_string()]);
        // Every partial combination refuses, and the error names the
        // missing piece.
        let err = resolve_device_login(&flags(Some("https://issuer.example"), Some("ws_co"), &[]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--device-login-subject"), "{err}");
        let err = resolve_device_login(&flags(Some("https://issuer.example"), None, &["op_1"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--device-login-org"), "{err}");
        let err = resolve_device_login(&flags(None, None, &["op_1"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--device-login-issuer"), "{err}");
        assert!(err.contains("--device-login-org"), "{err}");
        // Env fills the gaps; the subject list is comma-separated,
        // trimmed, empties dropped. Whitespace-only counts as absent.
        std::env::set_var("CADENCE_DEVICE_LOGIN_ISSUER", "https://env.example");
        std::env::set_var("CADENCE_DEVICE_LOGIN_ORG", "ws_env");
        std::env::set_var("CADENCE_DEVICE_LOGIN_SUBJECTS", " op_3 , ,op_4 ,");
        let triple = resolve_device_login(&flags(None, None, &[]))
            .unwrap()
            .unwrap();
        assert_eq!(triple.org, "ws_env");
        assert_eq!(
            triple.subjects,
            vec!["op_3".to_string(), "op_4".to_string()]
        );
        // Flag subjects beat the env list.
        let triple = resolve_device_login(&flags(None, None, &["op_9"]))
            .unwrap()
            .unwrap();
        assert_eq!(triple.subjects, vec!["op_9".to_string()]);
        // An env that yields no subject still misses the piece.
        std::env::set_var("CADENCE_DEVICE_LOGIN_SUBJECTS", " , ,");
        assert!(resolve_device_login(&flags(None, None, &[])).is_err());
        std::env::remove_var("CADENCE_DEVICE_LOGIN_ISSUER");
        std::env::remove_var("CADENCE_DEVICE_LOGIN_ORG");
        std::env::remove_var("CADENCE_DEVICE_LOGIN_SUBJECTS");
        // A pre-CAD-841 ui.json carrying the old `device_login` options
        // block parses and is ignored — the board must not resurrect a
        // persisted triple the daemon no longer reads.
        let stale: UiOpts = serde_json::from_value(serde_json::json!({
            "port": 3110,
            "device_login": {
                "issuer": "https://saved.example",
                "org": "ws_saved",
                "subjects": ["op_7"]
            }
        }))
        .unwrap();
        assert_eq!(stale.port, Some(3110));
        assert!(
            resolve_device_login(&flags(None, None, &[]))
                .unwrap()
                .is_none(),
            "a stale ui.json triple must not feed resolution"
        );
    }

    /// CAD-140: the board's create takes an optional description body
    /// for filed reports and ideas — and still refuses unknown fields,
    /// so a forged `by`/`actor` never reaches the tracker write.
    #[test]
    fn new_issue_body_is_optional_and_unlisted_fields_refused() {
        let bare: super::write_path::NewIssueReq = serde_json::from_value(serde_json::json!({
            "project": "demo", "title": "t",
        }))
        .unwrap();
        assert!(bare.body.is_none());
        let filed: super::write_path::NewIssueReq = serde_json::from_value(serde_json::json!({
            "project": "demo", "title": "t", "tags": ["intake", "idea"],
            "body": "t\n\nwhy this matters",
        }))
        .unwrap();
        assert_eq!(filed.body.as_deref(), Some("t\n\nwhy this matters"));
        assert!(
            serde_json::from_value::<super::write_path::NewIssueReq>(serde_json::json!({
                "project": "demo", "title": "t", "by": "operator",
            }))
            .is_err()
        );
    }
}
