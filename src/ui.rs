//! `cadence ui` — client, persisted options, process lifecycle, and tailnet seam.
//!
//! The HTTP board server (`serve` and all REST/SSE handlers) lives in the
//! `cadence-ui` workspace crate (`crates/cadence-ui`). Keeping only config,
//! lifecycle, and tailnet coordination here keeps `tiny_http` request routing
//! out of the core `cadence-agent` compilation unit.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, Result};

mod lifecycle;
mod tailscale;
mod tailscale_serve;

pub use cadence_core::PublicBoard;

/// The actor an operator write commits as — visible in `git log`
/// subjects. A pane's write commits as its alias.
#[doc(hidden)]
pub const UI_ACTOR: &str = "operator (ui)";

/// The public session cookie (contract §7): `__Host-` pins `Secure`,
/// `Path=/` and no `Domain`, so a sibling host under `*.BOARD_DOMAIN`
/// can neither toss nor overwrite it.
#[doc(hidden)]
pub const PUBLIC_COOKIE: &str = "__Host-aos-board-session";

/// This board's own loopback name: `cadence-<port>.localhost:<port>`.
#[doc(hidden)]
pub fn board_host(port: u16) -> String {
    format!("cadence-{port}.localhost:{port}")
}

/// The scheme a public board host speaks (contract §1).
#[doc(hidden)]
pub fn public_scheme(host: &str) -> &'static str {
    let name = host.split(':').next().unwrap_or_default();
    if name == "localhost" || name.ends_with(".localhost") {
        "http"
    } else {
        "https"
    }
}

#[cfg(test)]
pub(crate) use lifecycle::{
    board_argv_state_dir, pid_file, reap_unready_child, startup_bind_in_use,
};
pub use lifecycle::{detached_pid, START_BUDGET_ENV, START_BUDGET_RESTART};
#[doc(hidden)]
pub use lifecycle::{
    http_get, is_cadence_exe_name, ready_file, start, start_quiet, startup_failed_file,
    status as lifecycle_status, stop, READY_NONCE_ENV, START_REMAINING_ENV,
};
#[doc(hidden)]
pub use tailscale::{ensure_mapping, remove_mapping, tailscale_cli, ts, ts_self, ts_start_quiet};

/// Test-only bridge preserving `crate::ui::serve` and `crate::ui::ServeOpts`
/// for integration/acceptance fixtures still hosted in `cadence-agent`.
#[cfg(test)]
pub use cadence_ui::ServeOpts;

#[cfg(test)]
pub fn serve(state_dir: &Path, pm_dir: &Path, opts: &ServeOpts) -> Result<()> {
    cadence_ui::serve(state_dir, pm_dir, opts).map_err(|e| Error::internal(e.to_string()))
}

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
    /// Refuse workspace writes with 403; sign-in remains available.
    #[arg(long, overrides_with = "no_read_only")]
    pub read_only: bool,
    /// Clear a persisted --read-only.
    #[arg(long, overrides_with = "read_only")]
    pub no_read_only: bool,
    /// Allow unsigned reads on local boards. Writes still require their
    /// normal session and caller proof; hosted boards never allow this.
    #[arg(long, overrides_with = "no_allow_anonymous_reads")]
    pub allow_anonymous_reads: bool,
    /// Clear a persisted --allow-anonymous-reads.
    #[arg(long, overrides_with = "allow_anonymous_reads")]
    pub no_allow_anonymous_reads: bool,
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
pub enum TailscaleAction {
    /// Publish the board on the tailnet: ensure the serve mapping,
    /// persist the options, (re)start the detached board so the Host
    /// and Origin allowlists take effect, print the tailnet URL.
    Start {
        /// Tailscale https port [default: 9450].
        #[arg(long, default_value_t = 9450)]
        port: u16,
        /// Refuse workspace writes while retaining sign-in and sign-out.
        #[arg(long)]
        read_only: bool,
    },
    /// Remove the serve mapping cadence created, drop the tailnet
    /// Host/Origin entries, and restart the board local-only.
    Stop,
    /// Report sharing state, the tailnet URL, and a terminal QR code.
    Status,
}

/// CAD-1024: the `delegate:<alias>` seam for `ui start`/`ui stop`. No `--as`
/// (or one that is not `delegate:`) is the unchanged operator path — today
/// these are local pidfile ops needing no identity. A `delegate:` `--as`
/// requires a live grant for `op` on this state dir plus the I4 binding
/// (`require_delegate_grant`), so a staging agent — not the operator — can
/// bounce the board on a dir it is delegated for.
#[doc(hidden)]
pub fn delegate_ui_gate(state_dir: &Path, as_identity: Option<&str>, op: &str) -> Result<()> {
    let Some(as_identity) = as_identity else {
        return Ok(());
    };
    let caller = crate::rollout::resolve_caller(Some(as_identity))?;
    if caller.source == "delegate" {
        crate::rollout::require_delegate_grant(state_dir, &caller, op)?;
    }
    Ok(())
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
#[doc(hidden)]
pub fn tailnet_url(dns_name: &str, https_port: u16) -> String {
    if https_port == 443 {
        format!("https://{dns_name}")
    } else {
        format!("https://{dns_name}:{https_port}")
    }
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
    pub allow_anonymous_reads: bool,
    pub board_public_only: bool,
    pub tailscale: Option<TailscaleOpts>,
    /// The AgenticOS board-identity configuration (CAD-526).
    pub board: Option<PublicBoard>,
}

/// Resolved network bind and host/origin allowlist produced by
/// [`resolve_ui_opts`] and [`resolve_ui_bind`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub struct ResolvedUiBind {
    pub host: String,
    pub port: u16,
    pub allow_hosts: Vec<String>,
    pub allow_origins: Vec<String>,
    pub tailnet: Option<(String, u16)>,
}

#[doc(hidden)]
pub fn opts_file(state_dir: &Path) -> PathBuf {
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

/// CAD-1085: the server-selected base URL of this deployment's trusted
/// credential entry page — the root-pinned `https://` origin, never the
/// ordinary board, a caller-chosen origin, a CLI flag or `ui.json`.
///
/// `None` means this deployment has no separate trusted credential origin
/// (no protected profile, or the daemon cannot prove one). Callers must
/// report the entry as unsupported; they must never fall back to the board
/// origin, where the form does not exist.
pub fn credential_entry_base(state_dir: &Path) -> Option<String> {
    credential_entry_origin(state_dir)
}

/// CAD-1085: the deployment-pinned credential-entry origin from the root
/// profile, read through the daemon's operator-proven metadata RPC. The
/// answer needs no agent or loaded guest: it is the profile's exact
/// `https://host[:port]` plus live owner/core history evidence.
pub fn credential_entry_origin(state_dir: &Path) -> Option<String> {
    credential_entry_config(state_dir).1
}

/// `(root profile installed, pinned entry origin?)`. The daemon answers
/// from the live root profile and owner/core history; no agent or loaded
/// guest is required. `configured`/`protected` mean the profile is
/// installed — not qualified or ready — so the legacy directly-typed
/// transports are refused even while the owner is down.
pub fn credential_entry_config(state_dir: &Path) -> (bool, Option<String>) {
    // An unprovable answer fails CLOSED: an unknown/older daemon or a
    // transport error is not evidence that this is an unprotected board,
    // so the legacy secret paths stay refused until the daemon answers.
    credential_entry_config_proven(state_dir).unwrap_or((true, None))
}

/// The same answer as a `Result`, for callers (the CLI) that must
/// distinguish a positive operator-proven daemon reply from an unavailable
/// one before auto-opening a human-browser URL.
pub fn credential_entry_config_proven(state_dir: &Path) -> Result<(bool, Option<String>)> {
    let value = crate::client::rpc(state_dir, "credential_entry_config", json!({}))?;
    let entry = &value["entry"];
    let installed =
        entry["configured"].as_bool() == Some(true) && entry["protected"].as_bool() == Some(true);
    if !installed {
        return Ok((false, None));
    }
    let origin = entry["origin"].as_str().and_then(|origin| {
        let rest = origin.strip_prefix("https://")?;
        (rest.len() <= 200
            && !rest.is_empty()
            && !origin.bytes().any(|b| b.is_ascii_whitespace())
            && !rest.contains(['/', '?', '#', '@']))
        .then(|| origin.to_string())
    });
    Ok((true, origin))
}

/// Is a protected root profile installed, regardless of whether its pinned
/// origin is usable right now? Legacy secret collection is refused on this
/// answer alone.
pub fn protected_profile_configured(state_dir: &Path) -> bool {
    credential_entry_config(state_dir).0
}

/// The ordinary board origin — the metadata and management surface. Never
/// The ordinary board origin — the metadata and management surface. Never
/// the trusted credential origin.
pub fn board_base(state_dir: &Path) -> String {
    let port = load_opts(state_dir).port.unwrap_or(3010);
    format!("http://{}", board_host(port))
}

#[doc(hidden)]
pub fn load_opts(state_dir: &Path) -> UiOpts {
    try_load_opts(state_dir, false).unwrap_or_default()
}

/// Read persisted options while preserving a strict view of the sharing record.
/// Mutations must not treat an unreadable share as if no mapping were owned.
fn try_load_opts(state_dir: &Path, strict: bool) -> Result<UiOpts> {
    let bytes = match std::fs::read(opts_file(state_dir)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(UiOpts::default()),
        Err(e) if strict => {
            return Err(Error::rejected(format!(
                "cannot read {} — cannot prove it records no tailnet share: {e}",
                opts_file(state_dir).display()
            )));
        }
        Err(e) => {
            eprintln!("warning: cannot read ui.json: {e}");
            return Ok(UiOpts::default());
        }
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) if strict => {
            return Err(Error::rejected(format!(
                "{} is not valid JSON — cannot prove it records no tailnet share; fix or delete it by hand: {e}",
                opts_file(state_dir).display()
            )));
        }
        Err(e) => {
            eprintln!("warning: ignoring unreadable ui.json: {e}");
            return Ok(UiOpts::default());
        }
    };
    if strict && !value.is_object() {
        return Err(Error::rejected(format!(
            "{} must contain a UI options object — cannot establish tailnet share ownership; the document is kept",
            opts_file(state_dir).display()
        )));
    }
    let tailscale = match value.get("tailscale") {
        None | Some(Value::Null) => None,
        Some(v) => match serde_json::from_value::<TailscaleOpts>(v.clone()) {
            Ok(t) => Some(t),
            Err(e) if strict => {
                return Err(Error::rejected(format!(
                    "{} tailscale block is unreadable — fix or delete it by hand: {e}; the record is kept so its live mapping cannot be orphaned",
                    opts_file(state_dir).display()
                )));
            }
            Err(e) => {
                eprintln!("warning: ignoring unreadable ui.json tailscale block: {e}");
                None
            }
        },
    };
    let mut opts: UiOpts = match serde_json::from_value(value) {
        Ok(opts) => opts,
        Err(e) if strict => {
            return Err(Error::rejected(format!(
                "{} has unreadable UI options — cannot establish tailnet share ownership; fix it by hand; the document is kept: {e}",
                opts_file(state_dir).display()
            )));
        }
        Err(e) => {
            eprintln!("warning: ignoring unreadable ui.json: {e}");
            UiOpts::default()
        }
    };
    if tailscale.is_some() {
        opts.tailscale = tailscale;
    }
    Ok(opts)
}

#[doc(hidden)]
pub fn load_opts_strict(state_dir: &Path) -> Result<UiOpts> {
    try_load_opts(state_dir, true)
}

/// A mutation may infer absence of sharing only from a valid options
/// document (or a missing file), never from lenient display defaults.
pub(crate) fn has_persisted_share(state_dir: &Path) -> Result<bool> {
    Ok(load_opts_strict(state_dir)?.tailscale.is_some())
}

#[doc(hidden)]
pub fn save_opts(state_dir: &Path, opts: &UiOpts) -> Result<()> {
    let path = opts_file(state_dir);
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(opts)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Remove only the share record, without normalizing unrelated ui.json fields.
fn clear_persisted_share(state_dir: &Path) -> Result<()> {
    let path = opts_file(state_dir);
    let bytes = std::fs::read(&path)?;
    let mut value: Value = serde_json::from_slice(&bytes).map_err(|e| {
        Error::rejected(format!(
            "{} is not valid JSON — the mapping is removed but the record stays; fix or delete it by hand: {e}",
            path.display()
        ))
    })?;
    if let Some(obj) = value.as_object_mut() {
        obj.remove("tailscale");
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&value)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Merge flags over the persisted options: a given flag wins, an
/// absent one inherits. `--tailscale` resolves the tailnet identity
/// but does not mutate the serve mapping; callers publish it only after
/// their own remaining validation. A persisted tailscale block is kept.
/// but does not mutate the serve mapping; callers publish it only after
/// their own remaining validation. A persisted tailscale block is kept.
#[doc(hidden)]
pub fn resolve_ui_opts(flags: &UiFlags, persisted: &UiOpts) -> Result<(UiOpts, ResolvedUiBind)> {
    let env_public_only = match std::env::var("CADENCE_BOARD_PUBLIC_ONLY") {
        Ok(v) if v == "1" => true,
        Ok(v) if v == "0" => false,
        Ok(_) => return Err(Error::rejected("CADENCE_BOARD_PUBLIC_ONLY must be 0 or 1")),
        Err(std::env::VarError::NotPresent) => false,
        Err(_) => {
            return Err(Error::rejected(
                "CADENCE_BOARD_PUBLIC_ONLY is not valid UTF-8",
            ));
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
        allow_anonymous_reads: !flags.no_allow_anonymous_reads
            && (flags.allow_anonymous_reads || persisted.allow_anonymous_reads),
        board_public_only: flags.board_public_only
            || persisted.board_public_only
            || env_public_only,
        tailscale: persisted.tailscale.clone(),
        board: resolve_board(flags, persisted)?,
    };
    if eff.board_public_only && eff.board.is_none() {
        return Err(Error::rejected(
            "board public-only mode requires a public board identity",
        ));
    }
    if eff.allow_anonymous_reads
        && (eff.board_public_only
            || eff.board.is_some()
            || eff.tailscale.is_some()
            || flags.tailscale.is_some())
    {
        return Err(Error::rejected(
            "anonymous reads are local-only — clear --allow-anonymous-reads before configuring a hosted board or tailnet sharing",
        ));
    }
    if eff.tailscale.is_some() {
        crate::sandbox::refuse_global_unless_allowed(
            "`ui start` with persisted tailscale sharing",
        )?;
    }
    validate_share_target(&eff)?;
    if let Some(https_port) = flags.tailscale {
        refuse_share_port_change(persisted.tailscale.as_ref(), https_port)?;
        crate::sandbox::refuse_global_unless_allowed("`ui start --tailscale`")?;
        let host = eff.host.clone().unwrap_or_else(|| "127.0.0.1".to_string());
        if host != "127.0.0.1" {
            return Err(Error::rejected(format!(
                "--tailscale shares the board through a proxy on this host,                  but --host '{host}' is not the required literal IPv4 loopback bind — bind 127.0.0.1"
            )));
        }
        let ui_port = eff.port.unwrap_or(3010);
        crate::sandbox::refuse_production_port(ui_port)?;
        let target = format!("http://127.0.0.1:{ui_port}");
        let me = ts_self()?;
        eff.tailscale = Some(TailscaleOpts {
            dns_name: me.dns_name,
            https_port,
            target,
        });
    }
    let bind = resolve_ui_bind(&eff)?;
    Ok((eff, bind))
}

/// Refuse every recorded sharing target that does not match the
/// effective loopback bind. This runs even when a new `--tailscale`
/// flag would replace the record: invalid custody is never repaired by
/// overwriting it.
fn refuse_share_port_change(existing: Option<&TailscaleOpts>, requested_port: u16) -> Result<()> {
    if let Some(existing) = existing.filter(|share| share.https_port != requested_port) {
        return Err(Error::rejected(format!(
            "the persisted tailnet share uses HTTPS port {} — stop sharing before changing it to {requested_port}",
            existing.https_port
        )));
    }
    Ok(())
}

fn validate_share_target(eff: &UiOpts) -> Result<()> {
    let Some(ts) = &eff.tailscale else {
        return Ok(());
    };
    let bind_host = eff.host.clone().unwrap_or_else(|| "127.0.0.1".to_string());
    let bind_port = eff.port.unwrap_or(3010);
    crate::sandbox::refuse_production_port(bind_port)?;
    let expected = format!("http://127.0.0.1:{bind_port}");
    if bind_host != "127.0.0.1" || ts.target != expected {
        return Err(Error::rejected(format!(
            "the persisted tailnet target {} does not match the effective bind {bind_host}:{bind_port} — refusing to re-publish a forged or stale record",
            ts.target
        )));
    }
    Ok(())
}

/// CAD-841: the `--device-login-*` flags (and their
/// `CADENCE_DEVICE_LOGIN_*` fallbacks) as a thin client of
/// `operator_device_login_set`. Flags win, then env — never a
/// persisted block: the daemon's own store is the persistence now.
/// Issuer + org + at least one subject, or none of it; a partial
/// combination is an operator error naming the missing piece, never
/// a silent half trust root. The daemon validates the resolved
/// triple authoritatively in the RPC.
#[doc(hidden)]
pub fn resolve_device_login(flags: &UiFlags) -> Result<Option<DeviceLoginOpts>> {
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
#[doc(hidden)]
pub fn push_device_login_config(state_dir: &Path, triple: &DeviceLoginOpts) -> Result<()> {
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
#[doc(hidden)]
pub fn resolve_board(flags: &UiFlags, persisted: &UiOpts) -> Result<Option<PublicBoard>> {
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
#[doc(hidden)]
pub fn resolve_ui_bind(eff: &UiOpts) -> Result<ResolvedUiBind> {
    let host = eff.host.clone().unwrap_or_else(|| "127.0.0.1".to_string());
    let port = eff.port.unwrap_or(3010);
    // A sandbox board never takes production's port (CAD-310).
    crate::sandbox::refuse_production_port(port)?;
    validate_share_target(eff)?;
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
    if let Some(public) = &eff.board {
        if !allow_hosts
            .iter()
            .any(|x| x.eq_ignore_ascii_case(&public.host))
        {
            allow_hosts.push(public.host.clone());
        }
        let origin = format!("{}://{}", public_scheme(&public.host), public.host);
        if !allow_origins
            .iter()
            .any(|x| x.eq_ignore_ascii_case(&origin))
        {
            allow_origins.push(origin);
        }
    }
    Ok(ResolvedUiBind {
        host,
        port,
        allow_hosts,
        allow_origins,
        tailnet,
    })
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
#[path = "ui/cad1283_pid_startup_refusal_tests.rs"]
mod cad1283_pid_startup_refusal_tests;

#[cfg(test)]
mod tests {
    use super::{resolve_device_login, UiFlags, UiOpts};

    #[test]
    fn hosted_public_only_is_explicit_and_requires_public_identity() {
        let mut flags = super::UiFlags::default();
        let empty = super::UiOpts::default();
        assert!(
            !super::resolve_ui_opts(&flags, &empty)
                .unwrap()
                .0
                .board_public_only
        );
        flags.board_public_only = true;
        assert!(super::resolve_ui_opts(&flags, &empty).is_err());

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
            super::resolve_ui_opts(&super::UiFlags::default(), &restored)
                .unwrap()
                .0
                .board_public_only
        );
    }

    #[test]
    fn anonymous_read_policy_persists_and_explicitly_clears() {
        let state = tempfile::TempDir::new().unwrap();
        let flags = UiFlags {
            allow_anonymous_reads: true,
            ..Default::default()
        };
        let (enabled, _serve) = super::resolve_ui_opts(&flags, &UiOpts::default()).unwrap();
        assert!(enabled.allow_anonymous_reads);
        super::save_opts(state.path(), &enabled).unwrap();
        let restored = super::load_opts_strict(state.path()).unwrap();
        assert!(
            super::resolve_ui_opts(&UiFlags::default(), &restored)
                .unwrap()
                .0
                .allow_anonymous_reads
        );
        let (disabled, _serve) = super::resolve_ui_opts(
            &UiFlags {
                no_allow_anonymous_reads: true,
                ..Default::default()
            },
            &restored,
        )
        .unwrap();
        assert!(!disabled.allow_anonymous_reads);
        super::save_opts(state.path(), &disabled).unwrap();
        assert!(
            !super::load_opts_strict(state.path())
                .unwrap()
                .allow_anonymous_reads
        );
        let mut hosted = restored;
        hosted.board = Some(super::PublicBoard {
            host: "acme.board.localhost:3111".to_string(),
            issuer: "http://api.internal".to_string(),
            company: "co_1".to_string(),
            authorize_url: "http://api.internal/v2/board/authorize".to_string(),
        });
        assert!(super::resolve_ui_opts(&UiFlags::default(), &hosted).is_err());
        assert!(super::resolve_ui_opts(
            &UiFlags {
                no_allow_anonymous_reads: true,
                ..Default::default()
            },
            &hosted,
        )
        .is_ok());
    }

    #[test]
    fn anonymous_reads_refuse_tailnet_sharing() {
        let persisted = super::UiOpts {
            port: Some(3120),
            allow_anonymous_reads: true,
            tailscale: Some(super::TailscaleOpts {
                dns_name: "board.tailnet.ts.net".to_string(),
                https_port: 9450,
                target: "http://127.0.0.1:3120".to_string(),
            }),
            ..Default::default()
        };
        let err = super::resolve_ui_opts(&super::UiFlags::default(), &persisted)
            .err()
            .expect("anonymous reads with tailnet sharing must be refused");
        assert!(
            err.to_string().contains("anonymous reads are local-only"),
            "unexpected refusal: {err}"
        );

        // A newly requested share, with no persisted record, is caught too.
        let flags = super::UiFlags {
            allow_anonymous_reads: true,
            tailscale: Some(9450),
            ..Default::default()
        };
        let err = super::resolve_ui_opts(&flags, &super::UiOpts::default())
            .err()
            .expect("anonymous reads with a new --tailscale must be refused");
        assert!(
            err.to_string().contains("anonymous reads are local-only"),
            "unexpected refusal: {err}"
        );

        // Persisted anonymous reads plus a new --tailscale.
        let persisted_anon = super::UiOpts {
            allow_anonymous_reads: true,
            ..Default::default()
        };
        let err = super::resolve_ui_opts(
            &super::UiFlags {
                tailscale: Some(9450),
                ..Default::default()
            },
            &persisted_anon,
        )
        .err()
        .expect("a new --tailscale with persisted anonymous reads must be refused");
        assert!(
            err.to_string().contains("anonymous reads are local-only"),
            "unexpected refusal: {err}"
        );
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
        let bare: cadence_core::board_api::NewIssueReq =
            serde_json::from_value(serde_json::json!({
                "project": "demo", "title": "t",
            }))
            .unwrap();
        assert!(bare.body.is_none());
        let filed: cadence_core::board_api::NewIssueReq =
            serde_json::from_value(serde_json::json!({
                "project": "demo", "title": "t", "tags": ["intake", "idea"],
                "body": "t\n\nwhy this matters",
            }))
            .unwrap();
        assert_eq!(filed.body.as_deref(), Some("t\n\nwhy this matters"));
        assert!(
            serde_json::from_value::<cadence_core::board_api::NewIssueReq>(serde_json::json!({
                "project": "demo", "title": "t", "by": "operator",
            }))
            .is_err()
        );
    }

    /// CAD-1206: the board argv is recognised for a build named
    /// `cadence-*` (what `dev reload --build` runs), not for strangers.
    #[test]
    fn board_argv_accepts_cadence_named_builds_only() {
        let argv = |exe: &str| -> Vec<String> {
            [exe, "--state-dir", "/tmp/s", "ui", "run", "--port", "1"]
                .map(String::from)
                .to_vec()
        };
        for exe in ["/x/cadence", "/x/cadence-new", "/x/cadence-old"] {
            assert!(super::board_argv_state_dir(&argv(exe)).is_some(), "{exe}");
        }
        for exe in ["/x/a", "/x/cadencex", "/x/sleep", "/x/xcadence"] {
            assert!(super::board_argv_state_dir(&argv(exe)).is_none(), "{exe}");
        }
    }

    /// CAD-1207: the timeout cleanup stops exactly the child it holds and
    /// deletes `ui.pid` only while that file still names the child.
    #[test]
    fn reap_unready_child_kills_own_child_and_keeps_a_foreign_pidfile() {
        let dir = tempfile::Builder::new()
            .prefix("c1207-")
            .tempdir_in("/tmp")
            .unwrap();
        let spawn = || {
            std::process::Command::new("sleep")
                .arg("60")
                .spawn()
                .unwrap()
        };
        let mut ours = spawn();
        std::fs::write(super::pid_file(dir.path()), ours.id().to_string()).unwrap();
        let pid = ours.id() as i32;
        super::reap_unready_child(&mut ours, dir.path());
        assert!(!super::pid_file(dir.path()).exists());
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "child still alive");

        let mut other = spawn();
        std::fs::write(super::pid_file(dir.path()), "999999").unwrap();
        super::reap_unready_child(&mut other, dir.path());
        assert_eq!(
            std::fs::read_to_string(super::pid_file(dir.path())).unwrap(),
            "999999"
        );
    }
}
