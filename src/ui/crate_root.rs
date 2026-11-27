//! `cadence ui` — the board: a small synchronous HTTP server
//! (tiny_http, no async runtime — the daemon is plain threads too)
//! serving the built SPA plus a JSON API on loopback.
//!
//! Private reads require a live session; unsigned local browsing is an
//! explicit persisted policy. Operator authority is a session (CAD-313,
//! ADR 0004): a `cadence ui login` link exchanged for an HttpOnly cookie
//! plus a tab-held `X-Cadence-Session` key, checked by the daemon.
//! [`operator`] defines the stronger caller proof used for writes.
//! The rest is containment: loopback bind, no CORS headers, a
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

use clap::Subcommand;

use crate::error::Result;

#[cfg(all(test, feature = "test-seam"))]
use serde_json::{json, Value};

mod agent_access;
mod app_assistant;
mod app_audiences;
mod app_chat;
mod app_content;
mod app_contexts;
mod app_explorer;
mod app_media;
mod app_records;
mod app_release;
mod app_runs;
mod app_screens;
mod app_social_drafts;
mod approvals;
mod apps;
mod artifacts;
mod assist_route;
#[cfg(test)]
mod board_api_golden;
mod business_templates;
#[cfg(all(test, feature = "test-seam"))]
mod cad2018_wiki_http_acceptance;
#[cfg(all(test, feature = "test-seam"))]
mod cad2317_routine_connection_guard_acceptance;
#[cfg(test)]
mod cad2321_actor_kind_http_acceptance;
#[cfg(all(test, feature = "test-seam"))]
mod cad2378_time_zone_guard_acceptance;
#[cfg(all(test, feature = "test-seam"))]
mod cad2402_routines_list_guard_acceptance;
#[cfg(all(test, feature = "test-seam"))]
mod cad2487_dedicated_entry_acceptance;
#[cfg(all(test, feature = "test-seam"))]
mod cad2683_works_read_guard_acceptance;
#[cfg(all(test, feature = "test-seam"))]
mod cad2859_project_create_http_acceptance;
mod cli_route;
mod cloud_agents;
#[cfg(all(test, feature = "cli-runtime-test"))]
pub(crate) use cli_route::prime_jwks_for_test as prime_cli_jwks_for_test;
mod connections;
mod credential_entry;
mod crm_send;
mod crm_smtp;
pub mod delivery_sync;
mod endpoints;
mod home;
mod human_threads;
mod lane;
mod locale;
mod login;
mod member_assist;
mod member_view;
mod my_work;
mod notify_service;
mod operator;
mod platform_account;
pub(crate) use crate::project_policy;
pub(crate) mod public_directory;
mod read_model;
mod routines;
mod serve;
mod setup;
mod social_publish;
mod stages;
mod stream;
#[cfg(all(test, feature = "test-seam"))]
mod team_assistants_http_refusal;
mod threads;
mod time_zone;
mod topics;
mod updates;
mod whatsapp;
mod wiki;
mod wiki_envelope;
mod work_conversations;
mod workflows;
mod write_path;

pub use cadence_agent::ui::{
    board_base, board_host, credential_entry_base, credential_entry_config,
    credential_entry_config_proven, credential_entry_origin, detached_pid, health, opts_present,
    persisted_opts, protected_profile_configured, public_scheme, serve_has_target, DeviceLoginOpts,
    ResolvedUiBind, TailscaleAction, TailscaleOpts, UiFlags, UiOpts, PUBLIC_COOKIE,
    START_BUDGET_ENV, START_BUDGET_RESTART, UI_ACTOR,
};
use cadence_agent::ui::{
    delegate_ui_gate, ensure_mapping, lifecycle_status, load_opts, load_opts_strict,
    push_device_login_config, ready_file, remove_mapping, resolve_device_login, resolve_ui_bind,
    resolve_ui_opts, start, startup_failed_file, stop, tailnet_url, tailscale_cli, READY_NONCE_ENV,
};
pub use cadence_core::PublicBoard;

pub use operator::{route_class, RouteClass, WriteRoute, WRITE_ROUTES};
pub use serve::serve;
pub use setup::{read_model_stats, setup_runs};

pub(crate) use serve::{
    agents_payload_typed, board_job_list, err_response, json_response, pct_decode,
};
pub(crate) use stream::{dir_mtime, event_resources, projects_payload, value_fp};
pub(crate) use write_path::{
    agent_roots, busy_response, coded_response, guard_fail, header_value, issue_payloads,
    parse_json, parse_multipart, proxied_actor, read_body, tailnet_proxy, write_err, write_guard,
    write_reply, HttpResp, JSON_CAP,
};

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
        /// CAD-1024: `delegate:<alias>` — start the board under a staging
        /// grant for `ui_start` on this state dir.
        #[arg(long = "as")]
        as_identity: Option<String>,
    },
    /// Stop the detached UI server.
    Stop {
        /// Also remove the tailscale serve mapping cadence created.
        #[arg(long)]
        tailscale_off: bool,
        /// CAD-1024: `delegate:<alias>` — stop the board under a staging
        /// grant for `ui_stop` on this state dir.
        #[arg(long = "as")]
        as_identity: Option<String>,
    },
    /// Report UI server health and the persisted options.
    Status,
    /// Print a single-use sign-in link for the board (CAD-313). Private
    /// reads and operator writes need a session; this link opens one. Run it
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

/// `cadence ui …`
pub fn run_cli(state_dir: &Path, action: &UiAction) -> Result<i32> {
    match action {
        UiAction::Run { flags } => run_foreground(state_dir, flags),
        UiAction::Start {
            flags,
            reset,
            as_identity,
        } => {
            delegate_ui_gate(state_dir, as_identity.as_deref(), "ui_start")?;
            start(state_dir, flags, *reset)
        }
        UiAction::Stop {
            tailscale_off,
            as_identity,
        } => {
            delegate_ui_gate(state_dir, as_identity.as_deref(), "ui_stop")?;
            stop(state_dir, *tailscale_off)
        }
        UiAction::Status => lifecycle_status(state_dir),
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

/// `ui run` — foreground. Flags merge over the persisted options but
/// never rewrite them: `ui start` owns persistence.
fn run_foreground(state_dir: &Path, flags: &UiFlags) -> Result<i32> {
    if flags.tailscale.is_some() {
        // Foreground sandbox runs have no durable sharing custody. Only
        // the recorded start paths may publish a host-wide mapping.
        cadence_agent::sandbox::refuse_global(
            "foreground `ui run --tailscale`; use `ui start --tailscale` for recorded sandbox sharing",
        )?;
    }
    let persisted = load_opts_strict(state_dir)?;
    let (eff, mut so) = resolve_opts(flags, &persisted)?;
    so.device_login_push = resolve_device_login(flags)?;
    let created = if flags.tailscale.is_some() {
        let ts = eff.tailscale.as_ref().expect("resolved above");
        ensure_mapping(&ts.dns_name, ts.https_port, &ts.target)?
    } else {
        false
    };
    if let Err(e) = serve(state_dir, &cadence_agent::issue::default_dir()?, &so) {
        if created {
            if let Some(ts) = &eff.tailscale {
                let _ = remove_mapping(&ts.dns_name, ts.https_port, &ts.target);
            }
        }
        return Err(e);
    }
    Ok(0)
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

#[cfg(test)]
pub(crate) type MemberStreamBatchHook = dyn Fn(&str, &[std::sync::Arc<str>]) + Send + Sync;

#[cfg(test)]
pub(crate) type AgentDetailReadHook = dyn Fn(&str) + Send + Sync;

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
    pub allow_anonymous_reads: bool,
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
    /// when `None`, clamped to its bounds. Never set from the command line —
    /// tests shorten it.
    pub delivery_sync_every: Option<Duration>,
    /// This board process's delivery sync. [`serve`] always replaces it
    /// (with `None` on a read-only board); never a caller's.
    pub delivery_sync: Option<std::sync::Arc<delivery_sync::DeliverySync>>,
    /// The in-process owner's stop (CAD-471): once set, [`serve`] stops
    /// accepting and returns, closing its port. Never set from the
    /// command line — a test's board on a thread of the runner stops
    /// with its test instead of serving for the rest of the run.
    pub stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Isolated acceptance fixture override; use the production config validator.
    #[cfg(test)]
    pub(crate) dedicated_deployment_for_test: Option<String>,
    /// Test-only barrier after a member batch is captured, before projection.
    #[cfg(test)]
    pub(crate) member_stream_batch_hook: Option<std::sync::Arc<MemberStreamBatchHook>>,
    /// Test-only barrier after an agent detail read, before final projection.
    #[cfg(test)]
    pub(crate) agent_detail_read_hook: Option<std::sync::Arc<AgentDetailReadHook>>,
    /// Test-only barrier between member agent-show and agent-events reads.
    #[cfg(test)]
    pub(crate) agent_detail_show_hook: Option<std::sync::Arc<AgentDetailReadHook>>,
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
    /// CAD-1085: the root-profile-pinned credential-entry origin,
    /// resolved once by [`serve`] from the daemon. `None` means this
    /// deployment has no separate trusted credential origin: the form is
    /// explicitly unsupported, never served on the ordinary board origin.
    /// Never set from the command line or `ui.json` — tests inject it.
    pub credential_entry_origin: Option<String>,
    /// CAD-1085: the root profile is installed (configured/protected), so
    /// the legacy directly-typed credential transports are refused even
    /// when the owner is down or the pinned origin cannot be used.
    pub protected_profile: bool,
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn resolve_opts(flags: &UiFlags, persisted: &UiOpts) -> Result<(UiOpts, ServeOpts)> {
    let (eff, _) = resolve_ui_opts(flags, persisted)?;
    let serve = serve_opts(&eff)?;
    Ok((eff, serve))
}

/// Build the runtime view of effective options from the core bind/allowlist
/// resolver in `cadence-agent`.
pub(crate) fn serve_opts(eff: &UiOpts) -> Result<ServeOpts> {
    let bind = resolve_ui_bind(eff)?;
    Ok(ServeOpts {
        agent_uid: None,
        host: bind.host,
        port: bind.port,
        dist: eff.dist.clone(),
        allow_hosts: bind.allow_hosts,
        allow_origins: bind.allow_origins,
        read_only: eff.read_only,
        allow_anonymous_reads: eff.allow_anonymous_reads,
        board_public_only: eff.board_public_only,
        tailnet: bind.tailnet,
        tailscaled_socket: None,
        tailnet_latch: Default::default(),
        gh: None,
        delivery_sync_every: None,
        delivery_sync: None,
        stop: None,
        #[cfg(test)]
        dedicated_deployment_for_test: None,
        #[cfg(test)]
        member_stream_batch_hook: None,
        #[cfg(test)]
        agent_detail_read_hook: None,
        #[cfg(test)]
        agent_detail_show_hook: None,
        startup: None,
        ready_nonce: None,
        public: eff.board.clone(),
        device_login: DeviceLogin::default(),
        device_login_push: None,
        test_seam: crate::test_seam::env_armed(),
        seam: None,
        update_helper: None,
        credential_entry_origin: None,
        protected_profile: false,
    })
}

/// Keep SIGCHLD from waking the client's socket reads in the real HTTP board
/// rigs (`cad2018_wiki_http_acceptance`, `cad2317_routine_connection_guard_acceptance`,
/// `team_assistants_http_refusal` and its children). The daemon and board
/// threads a rig starts spawn `git`. A child that exits while its parent
/// thread has signals blocked (as `Command::spawn` does across the fork) has
/// the SIGCHLD queued on the process instead of dropped as ignored, and the
/// kernel then wakes any thread that does not block it. When that is the thread
/// in a timed `recv`, the call fails with EINTR: a socket with a receive
/// timeout is never restarted, whatever the signal's disposition, and ureq 3.4.2
/// does not retry it. Blocking it on the test thread is inherited by every
/// thread the rig spawns; `wait`/`waitpid` do not need it delivered.
#[cfg(all(test, feature = "test-seam"))]
pub(crate) fn block_sigchld() {
    // SAFETY: a local signal set and a mask change on the calling thread.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGCHLD);
        assert_eq!(
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()),
            0
        );
    }
}

#[cfg(test)]
mod tests {
    use super::serve::{
        agents_payload_from, content_type, context_query, running_json, static_answer_for,
        static_file, StaticAnswer,
    };
    use super::write_path::{health_supports_model_defaults, proxied_actor};
    use super::{serve_opts, PublicBoard, TailscaleOpts, UiOpts};
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

    /// CAD-224: a duplicated or unknown context query key is refused before
    /// any project is resolved, and a revision that is not a full 40-hex SHA
    /// can never become an observed-revision comparison.
    #[test]
    fn context_query_refuses_duplicate_unknown_and_unbounded_values() {
        assert_eq!(
            context_query("role=pm&role=qa"),
            Err("duplicate context query key")
        );
        let revision = "a".repeat(40);
        assert_eq!(
            context_query(&format!(
                "expected_revision={revision}&expected_revision={revision}"
            )),
            Err("duplicate context query key")
        );
        assert_eq!(
            context_query(&format!("role=pm&expected_revision={revision}"))
                .unwrap()
                .1
                .as_deref(),
            Some(revision.as_str())
        );
        assert_eq!(
            context_query("project=cadence"),
            Err("unknown context query key")
        );
        assert_eq!(
            context_query("role"),
            Err("context query values must use key=value")
        );
        assert_eq!(
            context_query("expected_revision=main"),
            Err("bad expected revision")
        );
        assert_eq!(
            context_query(&format!("expected_revision={}", "a".repeat(39))),
            Err("bad expected revision")
        );
        assert_eq!(context_query("role=pm&"), context_query("role=pm"));
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
        let answer = |path: &str| static_answer_for(Some(dist.path()), path, None, None);
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
                StaticAnswer::File(name, bytes, None) => {
                    assert_eq!(name, "/index.html", "{route}");
                    assert_eq!(content_type(&name), "text/html; charset=utf-8", "{route}");
                    assert_eq!(bytes, b"<!doctype html>shell", "{route}");
                }
                _ => panic!("{route}: expected the SPA shell"),
            }
        }
        match answer("/assets/index.js") {
            StaticAnswer::File(name, bytes, None) => {
                assert_eq!(name, "/assets/index.js");
                assert_eq!(bytes, b"js");
            }
            _ => panic!("asset not served"),
        }
        match answer("/favicon.svg") {
            StaticAnswer::File(_, bytes, None) => assert_eq!(bytes, b"<svg/>"),
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
            static_answer_for(Some(empty.path()), "/projects", None, None),
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
            None,
        ) {
            StaticAnswer::File(name, bytes, _) => {
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
                    None,
                ),
                StaticAnswer::Missing
            ),
            "JSON Accept on a wiki page is not the shell"
        );
        assert!(
            matches!(
                static_answer_for(Some(dist.path()), "/api/wiki/file", None, None),
                StaticAnswer::Missing
            ),
            "file bytes stay on the API route"
        );
        assert!(matches!(
            static_answer_for(Some(dist.path()), "/projects/x/logo.png", None, None),
            StaticAnswer::Missing
        ));
    }

    /// Accept-Encoding picks the build's precompressed sibling when the
    /// client names its coding, falls back to the raw bytes otherwise,
    /// and honors refusals: an explicit `q=0` (or a malformed q) beats a
    /// `*` offer, and `identity;q=0` with no acceptable coded answer is a
    /// 406 rather than raw bytes the client ruled out.
    #[test]
    fn static_files_negotiate_precompressed_siblings() {
        let dist = tempfile::TempDir::new().unwrap();
        std::fs::write(dist.path().join("index.html"), "raw-html").unwrap();
        std::fs::write(dist.path().join("index.html.br"), "coded-html").unwrap();
        std::fs::write(dist.path().join("index.html.gz"), "gzip-html").unwrap();
        std::fs::create_dir(dist.path().join("assets")).unwrap();
        std::fs::write(dist.path().join("assets/index.js"), "raw-js").unwrap();
        std::fs::write(dist.path().join("assets/index.js.br"), "coded-js").unwrap();

        let file = |path: &str, ae: Option<&str>| -> (String, Vec<u8>, Option<&'static str>) {
            match static_answer_for(Some(dist.path()), path, None, ae) {
                StaticAnswer::File(name, bytes, encoding) => (name, bytes, encoding),
                other => panic!("{path} {ae:?}: expected a file, got {other:?}"),
            }
        };
        // An offered coding answers the sibling bytes under the plain name.
        assert_eq!(
            file("/assets/index.js", Some("gzip, deflate, br")),
            (
                "/assets/index.js".to_string(),
                b"coded-js".to_vec(),
                Some("br")
            )
        );
        assert_eq!(
            file("/assets/index.js", Some("gzip")),
            ("/assets/index.js".to_string(), b"raw-js".to_vec(), None)
        );
        assert_eq!(
            file("/assets/index.js", None),
            ("/assets/index.js".to_string(), b"raw-js".to_vec(), None)
        );
        // q=0 refuses: explicit beats `*`, malformed q fails closed.
        assert_eq!(
            file("/assets/index.js", Some("br;q=0, *;q=0.5")),
            ("/assets/index.js".to_string(), b"raw-js".to_vec(), None)
        );
        assert_eq!(
            file("/assets/index.js", Some("br ; q = 0 , gzip")),
            ("/assets/index.js".to_string(), b"raw-js".to_vec(), None)
        );
        assert_eq!(
            file("/assets/index.js", Some("br;q=oops, gzip")),
            ("/assets/index.js".to_string(), b"raw-js".to_vec(), None)
        );
        // Refused identity with an acceptable coded answer serves the code.
        assert_eq!(
            file("/", Some("identity;q=0, br")),
            (
                "/index.html".to_string(),
                b"coded-html".to_vec(),
                Some("br")
            )
        );
        assert!(matches!(
            static_answer_for(
                Some(dist.path()),
                "/assets/index.js",
                None,
                Some("identity;q=0"),
            ),
            StaticAnswer::NotAcceptable
        ));
        // RFC 9110 §12.5.3: `*;q=0` without a more-specific identity
        // entry excludes the raw file too — and a malformed `*` q
        // fails closed the same way.
        for ae in ["*;q=0", "*;q=oops"] {
            assert!(
                matches!(
                    static_answer_for(Some(dist.path()), "/assets/index.js", None, Some(ae),),
                    StaticAnswer::NotAcceptable
                ),
                "{ae} excludes identity"
            );
        }
        // An explicit identity entry overrides `*`, both ways.
        assert_eq!(
            file("/assets/index.js", Some("*;q=0, identity;q=0.5")),
            ("/assets/index.js".to_string(), b"raw-js".to_vec(), None)
        );
        assert_eq!(
            file("/assets/index.js", Some("*;q=1, identity;q=0")),
            (
                "/assets/index.js".to_string(),
                b"coded-js".to_vec(),
                Some("br")
            )
        );
        // And with every coding refused, the explicit identity q=0
        // leaves nothing acceptable.
        assert!(matches!(
            static_answer_for(
                Some(dist.path()),
                "/assets/index.js",
                None,
                Some("*;q=0.5, identity;q=0, br;q=0"),
            ),
            StaticAnswer::NotAcceptable
        ));
        // A trailing dot is grammatical: `1.` is full weight, `0.` is zero.
        assert_eq!(
            file("/assets/index.js", Some("br;q=1.")),
            (
                "/assets/index.js".to_string(),
                b"coded-js".to_vec(),
                Some("br")
            )
        );
        assert_eq!(
            file("/assets/index.js", Some("br;q=0.")),
            ("/assets/index.js".to_string(), b"raw-js".to_vec(), None)
        );
        // Out-of-range, non-finite and over-precise qvalues fail closed
        // to refusal, never to a positive weight.
        for ae in [
            "br;q=nan, gzip",
            "br;q=inf, gzip",
            "br;q=-1, gzip",
            "br;q=2, gzip",
            "br;q=0.9999, gzip",
        ] {
            assert_eq!(
                file("/assets/index.js", Some(ae)),
                ("/assets/index.js".to_string(), b"raw-js".to_vec(), None),
                "{ae} admits nothing"
            );
        }
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

    /// Boundary unit test for `cadence-ui::serve_opts`: verifies host, port,
    /// deduplicated allowlists, and public board / tailnet origins resolve
    /// purely in-memory without native syscalls, external processes, or waits.
    #[test]
    fn serve_opts_preserves_resolved_bind_allowlists_and_public_origin() {
        let eff = UiOpts {
            host: Some("127.0.0.1".to_string()),
            port: Some(3142),
            allow_hosts: vec!["BOARD.TAILNET.TS.NET:443".to_string()],
            allow_origins: vec!["https://board.tailnet.ts.net".to_string()],
            read_only: true,
            board_public_only: true,
            tailscale: Some(TailscaleOpts {
                dns_name: "board.tailnet.ts.net".to_string(),
                https_port: 443,
                target: "http://127.0.0.1:3142".to_string(),
            }),
            board: Some(PublicBoard {
                host: "acme.board.localhost:3142".to_string(),
                issuer: "http://api.internal".to_string(),
                company: "co_1".to_string(),
                authorize_url: "http://api.internal/v2/board/authorize".to_string(),
            }),
            ..Default::default()
        };
        let opts = serve_opts(&eff).unwrap();
        assert_eq!(opts.host, "127.0.0.1");
        assert_eq!(opts.port, 3142);
        assert!(opts.read_only);
        assert!(opts.board_public_only);
        assert_eq!(
            opts.tailnet,
            Some(("board.tailnet.ts.net".to_string(), 443))
        );
        assert_eq!(
            opts.allow_hosts,
            vec![
                "BOARD.TAILNET.TS.NET:443".to_string(),
                "board.tailnet.ts.net".to_string(),
                "acme.board.localhost:3142".to_string(),
            ]
        );
        assert_eq!(
            opts.allow_origins,
            vec![
                "https://board.tailnet.ts.net".to_string(),
                "https://board.tailnet.ts.net:443".to_string(),
                "http://acme.board.localhost:3142".to_string(),
            ]
        );
        assert_eq!(
            opts.public.as_ref().map(|b| b.company.as_str()),
            Some("co_1")
        );
    }
}
