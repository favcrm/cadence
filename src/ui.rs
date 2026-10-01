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
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, Result};

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
mod lifecycle;
mod login;
mod operator;
mod platform_account;
mod read_model;
mod serve;
mod setup;
mod social_publish;
mod stages;
mod stream;
mod tailscale;
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
pub use lifecycle::detached_pid;
pub(crate) use lifecycle::{http_get, ready_file, start_quiet, READY_NONCE_ENV};
pub(crate) use serve::{
    agents_payload_from, board_job_list, err_response, json_response, pct_decode,
};
pub(crate) use stream::{dir_mtime, event_resources, projects_payload, value_fp};
pub(crate) use tailscale::{ensure_mapping, ts, ts_self, ts_start_quiet};
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
        UiAction::Run { flags } => lifecycle::run(state_dir, flags),
        UiAction::Start { flags, reset } => lifecycle::start(state_dir, flags, *reset),
        UiAction::Stop { tailscale_off } => lifecycle::stop(state_dir, *tailscale_off),
        UiAction::Status => lifecycle::status(state_dir),
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
        UiAction::Tailscale { action } => tailscale::tailscale_cli(state_dir, action),
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

// ---------- lifecycle — `src/ui/lifecycle.rs` ----------
// ---------- tailscale — `src/ui/tailscale.rs` (serve only, never funnel) ----------

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
