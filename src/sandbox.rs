//! `cadence dev` (alias `sandbox`) — a disposable Cadence beside
//! production (CAD-310, CAD-1187).
//!
//! One marked root per name under the sandbox base: `.cadence-sandbox`
//! (the marker `reset` requires), `state/` (0700), `pm/` (its own
//! tracker) and `sandbox.env`. `up` starts a daemon and a board from the
//! invoking binary with `CADENCE_PROFILE=sandbox:<name>` exported, and
//! that profile is the one switch the daemon and the UI read to gate
//! global side effects: no skill sync into `$HOME`, no tailnet unless
//! the sandbox was started with `CADENCE_SANDBOX_ALLOW_GLOBAL=1`, an
//! observe-only provider WAL watcher, no Cursor `cli-config.json` merge.
//! Every verb first refuses a root that overlaps production's state
//! dir (and so its socket), tracker, home or config dir.
//!
//! CAD-1922: the sandbox is its own home. Its children carry
//! `CADENCE_PROFILE`, `CADENCE_STATE_DIR`, `CADENCE_PM_DIR` and
//! `XDG_CONFIG_HOME=<root>/config`; the resolver maps the profile to the
//! sandbox root, so a caller's consolidated home, system install or
//! hosted credentials cannot leak vault, repos, workspace or credentials
//! in. `CADENCE_HOME` is deliberately not set for children: the overlap
//! guard reads it as evidence of a real installation.
//!
//! CAD-1187: whether a store may change build without the rollout lease
//! is a property of the STORE, not of the caller's env. `up` writes a
//! dev marker (`<state>/.cadence-dev`) into the store, naming its own
//! resolved path; [`dev_owner`] (what `rollout::sandbox_exempt` asks)
//! is true only for a store that holds that marker AND sits directly
//! under the sandbox base. `CADENCE_PROFILE=sandbox:x` alone, a marker
//! copied to another directory, or a root marker without the dev marker
//! unlock nothing. `reload` restarts a dev store from a local binary
//! and checks the same gate first.

use std::net::TcpListener;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clap::Subcommand;
use serde_json::{json, Value};

use crate::client;
use crate::error::{Error, Result};

mod provider;

/// The file that makes a directory a sandbox root. `reset` deletes
/// nothing that lacks it.
const MARKER: &str = ".cadence-sandbox";
/// CAD-1187: the dev marker inside the store (`<root>/state`). It names
/// the store's own resolved path, so a copy elsewhere proves nothing.
pub const DEV_MARKER: &str = ".cadence-dev";
/// The name `cadence dev up` uses when none is given.
const DEFAULT_NAME: &str = "dev";
const PROFILE_PREFIX: &str = "sandbox:";
/// The production board's port — never a sandbox's.
const PRODUCTION_UI_PORT: u16 = 3010;
const PORTS: std::ops::RangeInclusive<u16> = 3110..=3199;
/// The operator's explicit opt-in for an overridable global write.
pub const ALLOW_GLOBAL_ENV: &str = "CADENCE_SANDBOX_ALLOW_GLOBAL";

#[derive(Subcommand)]
pub enum DevAction {
    /// Create (or reuse) the dev Cadence and start its daemon and board:
    /// its own state dir, tracker and a free port in 3110-3199, plus a
    /// durable dev marker in the new store. Prints the paths, URL and
    /// env file as JSON.
    Up {
        /// Name: [a-z0-9][a-z0-9-]{0,31} [default: dev].
        name: Option<String>,
        /// The same, as a flag.
        #[arg(
            long = "name",
            id = "name_flag",
            value_name = "NAME",
            conflicts_with = "name"
        )]
        name_flag: Option<String>,
        /// Board port [default: the sandbox's last port, else the first
        /// free one in 3110-3199]. 3010 is refused.
        #[arg(long)]
        port: Option<u16>,
        /// Start the daemon and board from this binary instead of the
        /// invoking one.
        #[arg(long, value_name = "PATH")]
        build: Option<PathBuf>,
    },
    /// Restart the dev daemon and board from a local binary: no rollout
    /// lease, no attestation, no backup. Refused unless the store holds
    /// the dev marker and sits under the sandbox base; nothing is
    /// stopped or started when refused.
    Reload {
        /// The dev Cadence to reload [default: the store this shell
        /// points at (`CADENCE_STATE_DIR` / `--state-dir`)].
        #[arg(long)]
        name: Option<String>,
        /// The binary to start [default: the newest of the repo's
        /// `target/release/cadence` and `target/debug/cadence`].
        #[arg(long, value_name = "PATH")]
        build: Option<PathBuf>,
    },
    /// Print the export lines, for `eval "$(cadence dev env)"`.
    Env { name: Option<String> },
    /// Stop the board and daemon; the files stay.
    Down { name: Option<String> },
    /// Show whether the dev Cadence is running, and its port.
    Status { name: Option<String> },
    /// Stop the sandbox, then delete its root — only a directory under
    /// the sandbox base that holds this sandbox's marker.
    Reset { name: String },
    /// List the sandboxes under the base with running status.
    #[command(visible_alias = "list")]
    Ls,
    /// Point this sandbox at real AgenticOS and wire one app.
    Agenticos {
        #[command(subcommand)]
        action: AgenticosAction,
    },
}

#[derive(Subcommand)]
pub enum AgenticosAction {
    /// Write the sandbox provider profile and 0600 credentials.
    ///
    /// `--token-dir` must contain `send.token` (`publish.send`) and either
    /// `provider.token` (`provider.read` + `provider.draft`) or
    /// `read.token`; `draft.token` is optional and defaults to the read
    /// credential.
    Setup {
        /// The dev Cadence [default: `CADENCE_PROFILE`, else `dev`].
        #[arg(long)]
        name: Option<String>,
        /// AgenticOS API origin (no path).
        #[arg(long, default_value = "https://api-v2.agenticos.hk")]
        origin: String,
        /// Exact company workspace account, `ws_<uuid>`.
        #[arg(long)]
        workspace: String,
        /// Reviewed external-provider manifest pin.
        #[arg(long, default_value = "agenticos-external-provider-tools@4")]
        pin: String,
        /// Directory holding provider.token (or read.token) and send.token.
        #[arg(long)]
        token_dir: PathBuf,
    },
    /// Install one app and bind its source/image/writer/publication slots.
    Bind {
        /// The dev Cadence [default: `CADENCE_PROFILE`, else `dev`].
        #[arg(long)]
        name: Option<String>,
        /// App bundle root (`app.md` at the top level).
        #[arg(long, default_value = "app")]
        bundle: PathBuf,
        /// Context label to create (or reuse).
        #[arg(long, default_value = "Default")]
        context: String,
        /// Connected Instagram destination; the first live row when omitted.
        #[arg(long)]
        destination_id: Option<String>,
        /// IANA timezone recorded on the publication binding.
        #[arg(long, default_value = "UTC")]
        timezone: String,
        /// Do not set the publication destination or sender.
        #[arg(long)]
        no_publish: bool,
    },
    /// Approve every awaiting run and waiting effect for one installation.
    ///
    /// Sandbox-only: it refuses a state directory that is not a dev store.
    /// Each approval uses the row's exact snapshot or effect digest.
    Approve {
        /// The dev Cadence [default: `CADENCE_PROFILE`, else `dev`].
        #[arg(long)]
        name: Option<String>,
        /// Installation ID to clear.
        #[arg(long)]
        install_id: String,
        /// Optional exact context.
        #[arg(long)]
        context_id: Option<String>,
        /// List the candidates without approving.
        #[arg(long)]
        dry_run: bool,
    },
}

/// `cadence dev …`. `state_dir` is the store this shell points at; it
/// names the default target of `reload`, `env`, `down` and `status`.
pub fn run_cli(state_dir: &Path, action: &DevAction) -> Result<i32> {
    let out = match action {
        DevAction::Up {
            name,
            name_flag,
            port,
            build,
        } => {
            let name = name
                .as_deref()
                .or(name_flag.as_deref())
                .unwrap_or(DEFAULT_NAME);
            let exe = match build {
                Some(path) => runnable(path)?,
                None => running_exe()?,
            };
            up(&Sandbox::open(name)?, *port, &exe)?
        }
        DevAction::Reload { name, build } => reload(state_dir, name.as_deref(), build.as_deref())?,
        DevAction::Env { name } => {
            let sb = target(state_dir, name.as_deref())?;
            refuse_production_readonly(&sb)?;
            let marker = require_marker(&sb)?;
            let allow = marker["allow_global"].as_bool() == Some(true);
            print!("{}", env_lines(&sb, persisted_port(&sb.state_dir()), allow));
            return Ok(0);
        }
        DevAction::Down { name } => {
            let sb = target(state_dir, name.as_deref())?;
            refuse_production(&sb)?;
            require_marker(&sb)?;
            down(&sb)?
        }
        DevAction::Status { name } => {
            let sb = target(state_dir, name.as_deref())?;
            refuse_production_readonly(&sb)?;
            require_marker(&sb)?;
            status(&sb)
        }
        DevAction::Reset { name } => reset(&Sandbox::open(name)?)?,
        DevAction::Ls => ls()?,
        DevAction::Agenticos { action } => run_agenticos(state_dir, action)?,
    };
    println!("{}", crate::output::json_text(&out).unwrap_or_default());
    Ok(0)
}

// ---------- real AgenticOS provider profile ----------

fn run_agenticos(_state_dir: &Path, action: &AgenticosAction) -> Result<Value> {
    match action {
        AgenticosAction::Setup {
            name,
            origin,
            workspace,
            pin,
            token_dir,
        } => {
            let sb = agenticos_sandbox(name.as_deref())?;
            refuse_production(&sb)?;
            let existing = read_marker(&sb)?;
            refuse_unmarked_nonempty(&sb, existing.as_ref())?;
            std::fs::create_dir_all(&sb.root)?;
            // `up` refuses a non-empty root without a marker. Setup may run
            // before `up`, so leave the minimal marker `up` would accept.
            if existing.is_none() {
                std::fs::write(
                    sb.marker(),
                    json!({
                        "name": sb.name,
                        "created_at": crate::issue::time::iso(crate::issue::time::now_epoch()),
                    })
                    .to_string(),
                )?;
            }
            provider::setup(&sb.root, origin, workspace, pin, token_dir)
        }
        AgenticosAction::Bind {
            name,
            bundle,
            context,
            destination_id,
            timezone,
            no_publish,
        } => bind_agenticos(
            name.as_deref(),
            bundle,
            context,
            destination_id.as_deref(),
            timezone,
            !*no_publish,
        ),
        AgenticosAction::Approve {
            name,
            install_id,
            context_id,
            dry_run,
        } => approve_agenticos(name.as_deref(), install_id, context_id.as_deref(), *dry_run),
    }
}

/// The sandbox a dev-agenticos verb acts on: the named one, else the
/// profile this shell points at, else `dev`.
fn agenticos_sandbox(name: Option<&str>) -> Result<Sandbox> {
    let name = name
        .map(str::to_owned)
        .or_else(profile)
        .unwrap_or_else(|| DEFAULT_NAME.to_owned());
    if let Some(profile) = profile() {
        if name != profile {
            return Err(Error::rejected(format!(
                "this shell points at sandbox '{profile}', not '{name}' — pass --name {name} \
                 from a shell without CADENCE_PROFILE"
            )));
        }
    }
    let sb = Sandbox::open(&name)?;
    refuse_production(&sb)?;
    Ok(sb)
}

fn bind_agenticos(
    name: Option<&str>,
    bundle: &Path,
    context: &str,
    destination_id: Option<&str>,
    timezone: &str,
    publish: bool,
) -> Result<Value> {
    let sb = agenticos_sandbox(name)?;
    require_marker(&sb)?;
    let profile = provider::load(&sb.root)?
        .ok_or_else(|| Error::rejected("run `cadence dev agenticos setup` first"))?;
    let bundle = std::fs::canonicalize(bundle)
        .map_err(|_| Error::rejected("app bundle path does not exist"))?;
    if !bundle.join("app.md").is_file() {
        return Err(Error::rejected("app bundle has no app.md at its root"));
    }
    let manifest = crate::issue::app::parse_manifest(
        &std::fs::read_to_string(bundle.join("app.md"))
            .map_err(|_| Error::rejected("app.md is unreadable"))?,
    )?;

    let installed = match client::rpc_state(
        &sb.state_dir(),
        "app_workspace_install",
        json!({"source": bundle}),
    ) {
        Ok(installed) => installed,
        Err(error)
            if error
                .to_string()
                .contains("already has workspace installation") =>
        {
            return Err(Error::rejected(format!(
                "{error}; reset the sandbox (`cadence dev reset {}`) or upgrade the \
                 existing installation before rebinding",
                sb.name
            )))
        }
        Err(error) => return Err(error),
    };
    let install_id = installed["install_id"]
        .as_str()
        .ok_or_else(|| Error::internal("install returned no install_id"))?
        .to_owned();
    let digest = installed["digest"]
        .as_str()
        .ok_or_else(|| Error::internal("install returned no digest"))?
        .to_owned();
    // Sandbox auto-approval: the exact installed digest only.
    let approved = client::rpc_state(
        &sb.state_dir(),
        "app_local_install_approve",
        json!({"install_id": install_id, "digest": digest}),
    )?;

    let created_context = client::rpc_state(
        &sb.state_dir(),
        "app_context_create",
        json!({
            "install_id": install_id,
            "label": context,
            "input_defaults": {},
            "request_id": "dev-context",
        }),
    )?;
    let context_id = created_context["context"]["id"]
        .as_str()
        .ok_or_else(|| Error::internal("context create returned no id"))?
        .to_owned();

    // The host keeps one connection per provider/account, so a combined
    // `provider.token` (read + draft) is the default. A read-only fallback
    // still binds source; image/writer then fail at the provider with the
    // missing scope rather than refusing the second enrollment here.
    let token = profile
        .credentials
        .provider
        .as_deref()
        .unwrap_or(&profile.credentials.read);
    let provider = enroll(&sb, &profile, &["provider.read", "provider.draft"], token)?;

    let mut bindings = serde_json::Map::new();
    for slot in ["source", "image", "writer"] {
        if !manifest.capabilities.contains_key(slot) {
            continue;
        }
        let binding = client::rpc_state(
            &sb.state_dir(),
            "app_binding_create",
            json!({
                "install_id": install_id,
                "context_id": context_id,
                "slot": slot,
                "connection_id": provider,
                "request_id": format!("dev-{slot}"),
            }),
        )?;
        bindings.insert(slot.to_owned(), binding);
    }

    let mut publication = Value::Null;
    let mut warnings = Vec::new();
    if publish {
        let destinations = client::rpc_state(&sb.state_dir(), "social_destinations", json!({}))?;
        let selected = destination_id.map(str::to_owned).or_else(|| {
            destinations["destinations"]
                .as_array()
                .and_then(|rows| rows.first())
                .and_then(|row| row["destination_id"].as_str())
                .map(str::to_owned)
        });
        match selected {
            Some(destination_id) => {
                let existing = client::rpc_state(
                    &sb.state_dir(),
                    "app_binding_list",
                    json!({"install_id": install_id, "context_id": context_id}),
                )?;
                let revision = existing["bindings"]
                    .as_array()
                    .and_then(|rows| {
                        // CAD-1939: that destination's own binding.
                        rows.iter().find(|row| {
                            row["slot"].as_str() == Some("publication")
                                && row["state"] == "configured"
                                && row["config"]["publish"]["destination_id"].as_str()
                                    == Some(destination_id.as_str())
                        })
                    })
                    .and_then(|row| row["revision"].as_u64());
                let mut params = json!({
                    "install_id": install_id,
                    "context_id": context_id,
                    "destination_id": destination_id,
                    "request_id": "dev-publication",
                    "timezone": timezone,
                });
                if let Some(revision) = revision {
                    params["expected_revision"] = json!(revision);
                }
                let binding =
                    client::rpc_state(&sb.state_dir(), "app_binding_use_destination", params)?;
                bindings.insert("publication".to_owned(), binding.clone());
                publication = binding;
            }
            None => warnings
                .push("no connected Instagram destination; publication binding not set".to_owned()),
        }
    }

    Ok(json!({
        "schema": 1,
        "sandbox": sb.name,
        "install_id": install_id,
        "digest": digest,
        "approval": approved,
        "context_id": context_id,
        "connections": {"provider": provider},
        "bindings": bindings,
        "publication": publication,
        "warnings": warnings,
    }))
}

fn approve_agenticos(
    name: Option<&str>,
    install_id: &str,
    context_id: Option<&str>,
    dry_run: bool,
) -> Result<Value> {
    let sb = agenticos_sandbox(name)?;
    require_marker(&sb)?;
    if dev_owner(&sb.state_dir())?.is_none() {
        return Err(Error::rejected(
            "dev agenticos approve only runs in a dev store",
        ));
    }
    crate::proto::identifier(install_id, "installation ID")?;
    let mut run_filter = json!({"install_id": install_id});
    let mut effect_filter = json!({"install_id": install_id});
    if let Some(context) = context_id {
        crate::proto::identifier(context, "context ID")?;
        run_filter["context_id"] = json!(context);
        effect_filter["context_id"] = json!(context);
    }
    effect_filter["state"] = json!("waiting");
    if dry_run {
        effect_filter["read_only"] = json!(true);
    }
    let runs = client::rpc_state(&sb.state_dir(), "app_run_list", run_filter)?;
    let pending_runs = pending_approvals(&runs, &json!({"effects": []}));
    let run_candidates = pending_runs
        .runs
        .iter()
        .map(|(id, digest)| json!({"id": id, "digest": digest}))
        .collect::<Vec<_>>();
    let mut approved_runs = Vec::new();
    if !dry_run {
        for (id, digest) in &pending_runs.runs {
            approved_runs.push(client::rpc_state(
                &sb.state_dir(),
                "app_run_approve",
                json!({"run_id": id, "digest": digest}),
            )?);
        }
    }
    let mut effect_candidates = Vec::new();
    let mut approved_effects = Vec::new();
    let mut skipped_effects = Vec::new();
    let mut warnings = Vec::new();
    // `app_effect_list` returns the daemon's 100-row window. Dry-run pages
    // it read-only with `offset`; live mode approves each batch and
    // re-queries, so later waiting rows become the next window.
    let mut offset = 0;
    for batch in 0..1_000 {
        if dry_run {
            effect_filter["offset"] = json!(offset);
        }
        let effects = client::rpc_state(&sb.state_dir(), "app_effect_list", effect_filter.clone())?;
        let visible = effects["effects"].as_array().map_or(0, Vec::len);
        let (pending, mut skipped) = pending_effect_rows(&effects);
        skipped_effects.append(&mut skipped);
        if !dry_run && pending.is_empty() {
            break;
        }
        effect_candidates.extend(
            pending
                .iter()
                .map(|(id, digest)| json!({"id": id, "digest": digest})),
        );
        if dry_run {
            if visible < 100 {
                break;
            }
            offset += 100;
            continue;
        }
        for (id, digest) in &pending {
            approved_effects.push(client::rpc_state(
                &sb.state_dir(),
                "app_effect_decide",
                json!({"effect_id": id, "digest": digest, "decision": "accept", "execute": false}),
            )?);
        }
        if batch == 999 {
            warnings.push("stopped after 1000 effect batches; rerun to continue".to_owned());
        }
    }
    if dry_run && offset >= 100_000 {
        warnings.push("stopped after 1000 effect pages; rerun to continue".to_owned());
    }
    Ok(json!({
        "schema": 1,
        "sandbox": sb.name,
        "install_id": install_id,
        "context_id": context_id,
        "dry_run": dry_run,
        "runs": {
            "selected": pending_runs.runs.len(),
            "candidates": run_candidates,
            "approved": approved_runs,
            "skipped": pending_runs.skipped_runs,
        },
        "effects": {
            "selected": effect_candidates.len(),
            "candidates": effect_candidates,
            "approved": approved_effects,
            "skipped": skipped_effects,
        },
        "warnings": warnings,
    }))
}

struct PendingApprovals {
    runs: Vec<(String, String)>,
    effects: Vec<(String, String)>,
    skipped_runs: Vec<Value>,
    skipped_effects: Vec<Value>,
}

fn pending_approvals(runs: &Value, effects: &Value) -> PendingApprovals {
    let mut pending = PendingApprovals {
        runs: Vec::new(),
        effects: Vec::new(),
        skipped_runs: Vec::new(),
        skipped_effects: Vec::new(),
    };
    for run in runs["runs"].as_array().map_or(&[][..], Vec::as_slice) {
        if run["state"].as_str() != Some("awaiting_approval") {
            continue;
        }
        match (run["id"].as_str(), run["snapshot_digest"].as_str()) {
            (Some(id), Some(digest)) => pending.runs.push((id.to_owned(), digest.to_owned())),
            _ => pending.skipped_runs.push(json!({
                "id": run["id"],
                "reason": "awaiting run has no id or snapshot digest",
            })),
        }
    }
    let (effects, skipped) = pending_effect_rows(effects);
    pending.effects = effects;
    pending.skipped_effects = skipped;
    pending
}

fn pending_effect_rows(effects: &Value) -> (Vec<(String, String)>, Vec<Value>) {
    let mut pending = Vec::new();
    let mut skipped = Vec::new();
    for effect in effects["effects"].as_array().map_or(&[][..], Vec::as_slice) {
        if effect["state"].as_str() != Some("waiting") {
            continue;
        }
        let id = effect["effect_id"]
            .as_str()
            .or_else(|| effect["id"].as_str());
        match (id, effect["digest"].as_str()) {
            (Some(id), Some(digest)) => pending.push((id.to_owned(), digest.to_owned())),
            _ => skipped.push(json!({
                "id": effect["effect_id"],
                "reason": "waiting effect has no id or digest",
            })),
        }
    }
    (pending, skipped)
}

fn enroll(
    sb: &Sandbox,
    profile: &provider::Profile,
    scopes: &[&str],
    path: &Path,
) -> Result<String> {
    let token = std::fs::read_to_string(path)
        .map_err(|_| Error::rejected("provider credential is unreadable"))?;
    let enrolled = client::rpc_state(
        &sb.state_dir(),
        "connection_create",
        json!({
            "provider": "agenticos_external",
            "account": profile.workspace,
            "shape": "token",
            "token": token.trim(),
            "scopes": scopes,
            "accept_same_uid_risk": true,
        }),
    )?;
    enrolled["connection"]["id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::internal("connection create returned no id"))
}

// ---------- profile gating ----------

/// The sandbox name when `CADENCE_PROFILE=sandbox:<name>` — `None` for
/// production.
pub fn profile() -> Option<String> {
    parse_profile(std::env::var("CADENCE_PROFILE").ok().as_deref())
}

fn parse_profile(raw: Option<&str>) -> Option<String> {
    raw?.strip_prefix(PROFILE_PREFIX).map(str::to_string)
}

/// The root of the sandbox this process is pinned to, when that sandbox
/// exists and still holds its marker. `home::layout` uses it so a sandbox
/// is its own home without exporting `CADENCE_HOME`, which the overlap
/// guard must be free to read as production evidence.
///
/// Answers only *which root*, never *may this run*: it runs no overlap
/// guard, no marker policy beyond existence and no daemon check.
pub fn active_home() -> Result<Option<PathBuf>> {
    let Some(name) = profile() else {
        return Ok(None);
    };
    let sb = Sandbox::open(&name)?;
    // The marker must name this sandbox: a copy of another sandbox's
    // marker is not a home for this profile.
    let Some(marker) = read_marker(&sb)? else {
        return Ok(None);
    };
    if marker["name"].as_str() != Some(name.as_str()) {
        return Ok(None);
    }
    // The home this profile selects must not be a live installation. The
    // marker is hand-writable, so a planted `{name}` beside a real
    // `CADENCE_HOME` would otherwise point credential and vault reads at
    // that install while the store ownership check only saw some other
    // state dir. Fail closed instead of falling back to the environment:
    // the environment is what the marker was trying to overrule.
    refuse_home_overlap(&sb)?;
    Ok(Some(sb.root))
}

/// Refuse a sandbox home that is, contains or sits inside a live dir. An
/// export or `CADENCE_HOME` that names this very root is the sandbox
/// claiming itself and is exempt; anything else that overlaps is a real
/// installation and refuses. Stricter than the store guard on purpose:
/// the resolver consults this before every home-derived path.
fn refuse_home_overlap(sb: &Sandbox) -> Result<()> {
    let root = resolved(&sb.root);
    for (label, dir) in production_dirs(sb, true)? {
        let live = resolved(&dir);
        // A sandbox's own children export its state and tracker, which
        // sit strictly inside its root; those are the sandbox itself. An
        // equal path is never exempt — `CADENCE_HOME=<root>` beside a
        // profile is a contradiction this resolver refuses rather than
        // guesses.
        let exported = label.starts_with("the exported ") || label.starts_with("the CADENCE_HOME ");
        if exported && live != root && live.starts_with(&root) {
            continue;
        }
        if root.starts_with(&live) || live.starts_with(&root) {
            return Err(Error::rejected(format!(
                "sandbox '{}' at {} overlaps {label} {} — refusing to use it as the home; \
                 point CADENCE_SANDBOX_ROOT at a scratch dir",
                sb.name,
                sb.root.display(),
                dir.display()
            )));
        }
    }
    Ok(())
}

/// Refuse `what` — a write to state shared by the whole host — under a
/// sandbox profile.
pub fn refuse_global(what: &str) -> Result<()> {
    global_gate(profile().as_deref(), None, what)
}

/// `refuse_global`, with both the exact environment opt-in and the
/// matching sandbox's persisted grant. A shell override cannot grant
/// authority that `sandbox up` did not record.
pub fn refuse_global_unless_allowed(what: &str) -> Result<()> {
    let profile = profile();
    let mut allowed = std::env::var(ALLOW_GLOBAL_ENV).is_ok_and(|v| v == "1");
    if allowed {
        if let Some(name) = profile.as_deref() {
            let sb = Sandbox::open(name)?;
            refuse_production_readonly(&sb)?;
            let marker = require_marker(&sb)?;
            allowed = marker["allow_global"].as_bool() == Some(true);
        }
    }
    global_gate(profile.as_deref(), Some(allowed), what)
}

/// `allowed: None` — not overridable.
fn global_gate(profile: Option<&str>, allowed: Option<bool>, what: &str) -> Result<()> {
    let Some(name) = profile else {
        return Ok(());
    };
    match allowed {
        Some(true) => Ok(()),
        // The opt-in must reach the sandbox's daemon, which does the
        // write — so it is a restart, not a variable in this shell.
        Some(false) => Err(Error::rejected(format!(
            "{what} is global to this host — refused under \
             CADENCE_PROFILE={PROFILE_PREFIX}{name}; to allow it, restart the \
             dev instance with the opt-in: `cadence dev down {name}` then \
             `{ALLOW_GLOBAL_ENV}=1 cadence dev up {name}`"
        ))),
        None => Err(Error::rejected(format!(
            "{what} is global to this host — refused under \
             CADENCE_PROFILE={PROFILE_PREFIX}{name}; a sandbox never touches it, run it \
             against production from a shell without the sandbox env"
        ))),
    }
}

// ---------- layout ----------

/// Where sandbox roots live: `$CADENCE_SANDBOX_ROOT`, else
/// `$XDG_STATE_HOME/cadence-sandbox`, else
/// `~/.local/state/cadence-sandbox`. Always a plain absolute path: a
/// relative one follows the cwd, and a `..` would land somewhere other
/// than the path the production guard compares.
pub fn base_dir() -> Result<PathBuf> {
    let var = |name| std::env::var_os(name).filter(|d| !d.is_empty());
    let (source, base) = if let Some(dir) = var("CADENCE_SANDBOX_ROOT") {
        ("CADENCE_SANDBOX_ROOT", PathBuf::from(dir))
    } else if let Some(dir) = var("XDG_STATE_HOME") {
        ("XDG_STATE_HOME", PathBuf::from(dir).join("cadence-sandbox"))
    } else {
        let home = var("HOME")
            .ok_or_else(|| Error::rejected("HOME is not set — export CADENCE_SANDBOX_ROOT"))?;
        (
            "HOME",
            PathBuf::from(home).join(".local/state/cadence-sandbox"),
        )
    };
    let plain = base.is_absolute()
        && !base
            .components()
            .any(|c| matches!(c, Component::CurDir | Component::ParentDir));
    if !plain {
        return Err(Error::rejected(format!(
            "sandbox base {} (from {source}) must be an absolute path with no \
             `.` or `..` — export CADENCE_SANDBOX_ROOT as one",
            base.display()
        )));
    }
    Ok(base)
}

struct Sandbox {
    name: String,
    base: PathBuf,
    root: PathBuf,
}

impl Sandbox {
    fn open(name: &str) -> Result<Self> {
        validate_name(name)?;
        let base = base_dir()?;
        Ok(Self {
            name: name.to_string(),
            root: base.join(name),
            base,
        })
    }
    fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }
    fn pm_dir(&self) -> PathBuf {
        self.root.join("pm")
    }
    /// The sandbox's own `XDG_CONFIG_HOME` — hosted config and
    /// credentials stay inside the sandbox, never the caller's.
    fn config_dir(&self) -> PathBuf {
        self.root.join("config")
    }
    fn env_file(&self) -> PathBuf {
        self.root.join("sandbox.env")
    }
    fn marker(&self) -> PathBuf {
        self.root.join(MARKER)
    }
    fn dev_marker(&self) -> PathBuf {
        self.state_dir().join(DEV_MARKER)
    }
    /// The sandbox a state dir is the `state` of, by layout alone
    /// (`<base>/<name>/state`) — nothing is trusted until the callers'
    /// checks pass.
    fn from_state_dir(state_dir: &Path) -> Option<Self> {
        if state_dir.file_name().and_then(|n| n.to_str()) != Some("state") {
            return None;
        }
        let root = state_dir.parent()?;
        let name = root.file_name()?.to_str()?.to_string();
        Some(Self {
            name,
            base: root.parent()?.to_path_buf(),
            root: root.to_path_buf(),
        })
    }
    fn profile(&self) -> String {
        format!("{PROFILE_PREFIX}{}", self.name)
    }
}

/// `[a-z0-9][a-z0-9-]{0,31}` — safe as one path segment and in a
/// profile value.
fn validate_name(name: &str) -> Result<()> {
    let ok = (1..=32).contains(&name.len())
        && name
            .bytes()
            .enumerate()
            .all(|(i, b)| b.is_ascii_lowercase() || b.is_ascii_digit() || (i > 0 && b == b'-'));
    if ok {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "sandbox name '{name}' must match [a-z0-9][a-z0-9-]{{0,31}} — \
             e.g. `cadence dev up smoke`"
        )))
    }
}

/// The sandbox a verb acts on: `name` when given, else the one this
/// shell's state dir belongs to, else `dev`.
fn target(state_dir: &Path, name: Option<&str>) -> Result<Sandbox> {
    match name {
        Some(name) => Sandbox::open(name),
        None => match Sandbox::from_state_dir(state_dir) {
            Some(sb) if sb.marker().exists() => {
                validate_name(&sb.name)?;
                Ok(sb)
            }
            _ => Sandbox::open(DEFAULT_NAME),
        },
    }
}

// ---------- production guard ----------

/// Where `path` lands: made absolute, each existing prefix
/// canonicalized (symlinks followed) and a `..` popping the component
/// before it — lexically once the path stops existing, which is where
/// creating it would land too. No spelling of a dir compares
/// differently from the dir itself.
pub(crate) fn resolved(path: &Path) -> PathBuf {
    let path = if path.is_relative() {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    };
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            Component::Normal(part) => {
                out.push(part);
                if let Ok(real) = std::fs::canonicalize(&out) {
                    out = real;
                }
            }
            root => out.push(root.as_os_str()),
        }
    }
    out
}

/// The live dirs a sandbox must never overlap, labelled for the refusal.
///
/// The list is evidence about the *host*, not about the candidate: the
/// marker is a hand-writable file and any caller can export any path, so
/// neither may exempt a production dir. Production's own roots are
/// therefore resolved from the host's defaults alone — `$HOME` and
/// `$XDG_*`, never `$CADENCE_HOME`, which a sandbox pins to itself — and
/// none is dropped because it happens to sit at or under the candidate
/// root: a marker planted in the production tracker stays a production
/// tracker.
///
/// Nothing a caller exports is exempt either. `CADENCE_PROFILE`,
/// `CADENCE_HOME`, `CADENCE_STATE_DIR`, `CADENCE_PM_DIR` and
/// `XDG_CONFIG_HOME` are same-uid environment, not identity: a genuine
/// installation may be configured at `<root>/state`, `<root>/pm` or
/// `<root>/config`, so a forged profile or an export equal to one of
/// this sandbox's own paths would let a planted marker in `<root>`
/// authorise deleting that installation. Every export is a production
/// dir. `exported: false` keeps the defaults only.
fn production_dirs(sb: &Sandbox, exported: bool) -> Result<Vec<(&'static str, PathBuf)>> {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    // CAD-1922: the consolidated home holds `state/`, `tracker/`,
    // `config/`, `repos/`, `vault/` and `workspace/`, so a sandbox inside
    // it would share a live install's roots. It is spelled from `$HOME`
    // like the legacy roots below, not through `home_default()`: that
    // resolver is test-guarded (CAD-1210) and would refuse to name the
    // real `~/.cadence` in a test process that only needs the path.
    let mut dirs: Vec<(&'static str, PathBuf)> = Vec::new();
    // The pre-`CADENCE_HOME` roots, from `$HOME`/`$XDG_*` alone: an
    // existing installation keeps its data here whatever branch the
    // caller's `$CADENCE_HOME` selects, so an export cannot hide it.
    // `home.rs` resolves the same spellings, but its public defaults
    // follow `$CADENCE_HOME`/`$XDG_CONFIG_HOME`; this guard must not.
    if let Some(home) = var("HOME") {
        dirs.push(("the production home", home.join(".cadence")));
        dirs.push(("the production tracker", home.join("pm")));
        dirs.push(("the production config dir", home.join(".config/cadence")));
        // A daemon started without XDG_STATE_HOME lives here even when
        // this shell sets it.
        dirs.push(("the production state dir", crate::home::local_state_dir()?));
    }
    // An explicit `CADENCE_HOME` names a real installation too — a system
    // install, a second profile. Sandbox children no longer carry it (the
    // resolver maps their profile to the sandbox root), so naming it here
    // cannot make a sandbox refuse its own store, and `owner_of` refuses a
    // marker planted in that installation.
    if let Some(root) = var("CADENCE_HOME").filter(|path| path.is_absolute()) {
        dirs.push(("the CADENCE_HOME installation", root.clone()));
        dirs.push(("the CADENCE_HOME state dir", root.join("state")));
        dirs.push(("the CADENCE_HOME tracker", root.join("tracker")));
        dirs.push(("the CADENCE_HOME config dir", root.join("config")));
    }
    if let Some(state_home) = var("XDG_STATE_HOME") {
        dirs.push(("the production state dir", state_home.join("cadence")));
    }
    if let Some(config_home) = var("XDG_CONFIG_HOME") {
        // The config root's own `cadence/` dir is the config *this
        // process* would use. A sandbox's children carry
        // `XDG_CONFIG_HOME=<root>/config`, and `owner_of` (exported:
        // false) must not read that as production. Nothing is exempted
        // here: a deleting verb passes exported: true and sees the
        // export itself, which stays a production dir even at this path.
        if resolved(&config_home) != resolved(&sb.config_dir()) {
            dirs.push(("the production config dir", config_home.join("cadence")));
        }
    }
    if exported {
        // CAD-1187: every export is a claim about a live cadence, with
        // no exemption. `CADENCE_PROFILE` is caller-controlled env, not
        // identity, so neither it nor a path equal to one of this
        // sandbox's own dirs may mark a dir disposable — a custom
        // installation can be configured at `<root>/state` or `<root>/pm`
        // too, and a planted marker would then delete it.
        // `CADENCE_HOME` is inventoried above, from its own value, so it
        // is not repeated here as an export.
        for (name, label) in [
            ("CADENCE_STATE_DIR", "the exported CADENCE_STATE_DIR"),
            ("CADENCE_PM_DIR", "the exported CADENCE_PM_DIR"),
            ("XDG_CONFIG_HOME", "the exported XDG_CONFIG_HOME"),
        ] {
            if let Some(dir) = var(name) {
                dirs.push((label, dir));
            }
        }
    }
    Ok(dirs)
}

/// Refuse a sandbox whose root, state dir, tracker or config dir
/// overlaps a live dir in either direction — equal (the socket lives in
/// the state dir), nested inside it, or containing it (a `reset` would
/// delete it).
fn refuse_production(sb: &Sandbox) -> Result<()> {
    refuse_overlap(sb, true, false)
}

/// The same overlap guard for a verb that does not delete: `status`,
/// `env` and the global gate in a shell exported by `cadence dev env`
/// name the sandbox's own dirs on both sides, and refusing that made the
/// sandbox unusable in its own environment (the CAD-832 acceptance
/// covers the gate).
///
/// Only an *exported* dir that is exactly one of the sandbox's own is
/// exempt; production's host-derived defaults keep their equality check,
/// so a sandbox rooted at the production tracker, home or config is still
/// refused. Every deleting verb — `reset`, `down`, `reload` — keeps the
/// strict inventory with no exemption at all: there, a marker planted at
/// a production path makes that path the sandbox's own, and only the
/// strict form refuses to delete it.
fn refuse_production_readonly(sb: &Sandbox) -> Result<()> {
    refuse_overlap(sb, true, true)
}

fn refuse_overlap(sb: &Sandbox, exported: bool, own_exports_are_ours: bool) -> Result<()> {
    let ours = [
        sb.root.clone(),
        sb.state_dir(),
        sb.pm_dir(),
        sb.config_dir(),
    ]
    .map(|p| resolved(&p));
    for (label, dir) in production_dirs(sb, exported)? {
        let live = resolved(&dir);
        // An exported dir that is one of the sandbox's own paths — or
        // inside its own root, as every `CADENCE_HOME=<root>` path is —
        // is the sandbox, not a live cadence. A host-derived default at
        // the same path is still production and still refuses.
        if own_exports_are_ours
            && (label.starts_with("the exported ") || label.starts_with("the CADENCE_HOME "))
            && (live.starts_with(&ours[0]) || ours.iter().any(|own| own == &live))
        {
            continue;
        }
        if ours
            .iter()
            .any(|p| p.starts_with(&live) || live.starts_with(p))
        {
            return Err(Error::rejected(format!(
                "sandbox '{}' at {} overlaps {label} {} — refusing; point \
                 CADENCE_SANDBOX_ROOT at a scratch dir, or `unset CADENCE_STATE_DIR \
                 CADENCE_PM_DIR` if this shell is exported at a live cadence",
                sb.name,
                sb.root.display(),
                dir.display()
            )));
        }
    }
    Ok(())
}

// ---------- marker ----------

/// The marker's JSON when the root holds one — refused when it names
/// another sandbox or is not a regular file.
fn read_marker(sb: &Sandbox) -> Result<Option<Value>> {
    let path = sb.marker();
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(Error::rejected(format!(
                "cannot read {} ({e}) — {} is not a sandbox root; pick another name",
                path.display(),
                sb.root.display()
            )))
        }
        Ok(meta) if !meta.file_type().is_file() => {
            return Err(Error::rejected(format!(
                "{} is not a regular file — {} is not a sandbox root; \
                 pick another name",
                path.display(),
                sb.root.display()
            )))
        }
        Ok(_) => {}
    }
    let marker: Value = serde_json::from_str(&std::fs::read_to_string(&path)?).map_err(|e| {
        Error::rejected(format!(
            "{} is not valid JSON ({e}) — pick another name",
            path.display()
        ))
    })?;
    if marker["name"].as_str() != Some(sb.name.as_str()) {
        return Err(Error::rejected(format!(
            "{} belongs to sandbox {}, not '{}' — pick another name \
             (`cadence dev ls` lists them)",
            sb.root.display(),
            marker["name"],
            sb.name
        )));
    }
    Ok(Some(marker))
}

/// A root without a marker must be empty; otherwise `up` or `setup` would
/// treat unrelated contents as a sandbox (and `reset` could delete them).
fn refuse_unmarked_nonempty(sb: &Sandbox, existing: Option<&Value>) -> Result<()> {
    if existing.is_some() {
        return Ok(());
    }
    match std::fs::read_dir(&sb.root) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Err(Error::rejected(format!(
                    "{} exists and holds no sandbox marker — pick another name",
                    sb.root.display()
                )));
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::rejected(format!(
            "cannot read {} ({e}) — refusing; pick another name",
            sb.root.display()
        ))),
    }
}

/// `<root>/.cadence-sandbox` when `state_dir` is `<root>/state` beside a
/// marker — the one file `owner_of` must READ to judge the root, so a
/// confined process (the master, CAD-524) needs it in its filesystem
/// policy or every `cadence` verb refuses before dispatch. Presence
/// only: contents stay `read_marker`'s to reject, and granting the file
/// never widens to the root. `None` for any other layout, production's
/// included, so a caller can push it unconditionally.
pub fn marker_for(state_dir: &Path) -> Option<PathBuf> {
    if state_dir.file_name().and_then(|n| n.to_str()) != Some("state") {
        return None;
    }
    let marker = state_dir.parent()?.join(MARKER);
    std::fs::symlink_metadata(&marker).is_ok().then_some(marker)
}

/// The sandbox `state_dir` belongs to: `<root>/state` beside a marker
/// naming `<root>`. `None` for any other dir, production's included. A
/// marker that is present but unusable refuses rather than let the dir
/// run ungated — and so does a layout that only looks like a sandbox:
/// a symlinked `state`, one resolving anywhere but `<root>/state`, or
/// a root overlapping production's defaults. The marker is a
/// hand-writable file; it must never exempt a production database from
/// the rollout gates.
pub fn owner_of(state_dir: &Path) -> Result<Option<String>> {
    if state_dir.file_name().and_then(|n| n.to_str()) != Some("state") {
        return Ok(None);
    }
    let Some(root) = state_dir.parent() else {
        return Ok(None);
    };
    if std::fs::symlink_metadata(root.join(MARKER)).is_err() {
        return Ok(None);
    }
    let name = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let sb = Sandbox {
        base: root.parent().unwrap_or(root).to_path_buf(),
        root: root.to_path_buf(),
        name,
    };
    let owned = validate_name(&sb.name)
        .and_then(|()| require_marker(&sb))
        .and_then(|_| {
            let linked =
                std::fs::symlink_metadata(state_dir).is_ok_and(|m| m.file_type().is_symlink());
            if linked || resolved(state_dir) != resolved(root).join("state") {
                return Err(Error::rejected(format!(
                    "{} is not the root's own `state` directory",
                    state_dir.display()
                )));
            }
            refuse_overlap(&sb, false, false)
        });
    owned.map(|()| Some(sb.name.clone())).map_err(|e| {
        Error::rejected(format!(
            "{} sits in a sandbox root whose marker is unusable ({e}) — \
             refusing to run it ungated",
            state_dir.display()
        ))
    })
}

/// CAD-1187: the dev store `state_dir` is — the only kind of store that
/// may change build without the rollout lease. All of: it is
/// `<root>/state` beside a usable sandbox marker ([`owner_of`]); `<root>`
/// is a direct child of the sandbox base once symlinks resolve; and the
/// store holds a dev marker that names its own resolved path. `None`
/// otherwise — for production, for a plain `--state-dir`, for a marker
/// copied elsewhere. The caller's env (`CADENCE_PROFILE`) is not read.
pub fn dev_owner(state_dir: &Path) -> Result<Option<String>> {
    dev_owner_under(state_dir, base_dir().ok().as_deref())
}

fn dev_owner_under(state_dir: &Path, base: Option<&Path>) -> Result<Option<String>> {
    let Some(name) = owner_of(state_dir)? else {
        return Ok(None);
    };
    let Some(sb) = Sandbox::from_state_dir(state_dir) else {
        return Ok(None);
    };
    if !base.is_some_and(|b| under_base(&sb, b)) || !dev_marker_ok(&sb, state_dir) {
        return Ok(None);
    }
    Ok(Some(name))
}

/// `<root>` is a direct child of the sandbox base, after symlinks.
fn under_base(sb: &Sandbox, base: &Path) -> bool {
    resolved(&sb.root).parent() == Some(resolved(base).as_path())
}

/// A regular, non-symlink `<state>/.cadence-dev` naming this sandbox
/// and this store's resolved path.
fn dev_marker_ok(sb: &Sandbox, state_dir: &Path) -> bool {
    let path = state_dir.join(DEV_MARKER);
    let regular = std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file());
    if !regular {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(marker) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    marker["v"].as_u64() == Some(1)
        && marker["name"].as_str() == Some(sb.name.as_str())
        && marker["state_dir"].as_str() == resolved(state_dir).to_str()
}

fn write_dev_marker(sb: &Sandbox) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let marker = json!({
        "v": 1,
        "name": sb.name,
        "state_dir": resolved(&sb.state_dir()),
    });
    let path = sb.dev_marker();
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(Error::rejected(format!(
            "{} is a symlink — refusing to write the dev marker through it",
            path.display()
        )));
    }
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&marker).unwrap_or_default() + "\n",
    )?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Run as the sandbox `state_dir` belongs to, whatever the caller's
/// env says: its profile, tracker and state dir go into this process's
/// env, so the daemon, the board and everything they spawn stay gated
/// — a bare `cadence --state-dir <root>/state daemon start` is still
/// the sandbox. Any other state dir is left alone.
pub fn adopt(state_dir: &Path) -> Result<Option<String>> {
    let Some(name) = owner_of(state_dir)? else {
        return Ok(None);
    };
    let root = state_dir.parent().unwrap_or(state_dir);
    std::env::set_var("CADENCE_PROFILE", format!("{PROFILE_PREFIX}{name}"));
    // CAD-1922: the adopted shell is the sandbox's home too — the
    // resolver maps the profile to this root — and XDG_CONFIG_HOME
    // keeps hosted config and credentials inside it. CADENCE_HOME is
    // deliberately left unset: the overlap guard must be free to read
    // it as evidence of a real installation.
    std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
    std::env::set_var("CADENCE_PM_DIR", root.join("pm"));
    std::env::set_var("CADENCE_STATE_DIR", state_dir);
    // Pin the base the *adopted* root came from, not whatever the ambient
    // environment says: the resolver rebuilds the root from the profile
    // name, and another exported base would select a different sandbox
    // with the same name.
    if let Some(base) = root.parent() {
        std::env::set_var("CADENCE_SANDBOX_ROOT", base);
    }
    Ok(Some(name))
}

fn require_marker(sb: &Sandbox) -> Result<Value> {
    read_marker(sb)?.ok_or_else(|| {
        Error::rejected(format!(
            "no dev instance '{}' at {} — `cadence dev up {}` creates it",
            sb.name,
            sb.root.display(),
            sb.name
        ))
    })
}

// ---------- port ----------

/// A cooperating test suite fences board ports with an exclusive
/// `flock` on `<dir>/<port>.lock` — `tests/setup.rs` leases 3110-3199
/// there. Without it the free pick probes bindability only, and a port
/// a test just leased — probe-bound, then released — can be stolen in
/// the gap before its `ui run` binds; the thief then answers the
/// test's requests. When the env names a dir the pick skips fenced
/// ports, and the fence `choose_port` returns is held until `up`'s
/// `ui start` has bound — neither side can take the other's port in
/// its pick-to-bind window. Test-only: unset outside the suite.
///
/// The fence outlives that window only if a suite member keeps it: a
/// sandbox goes on claiming its port in `state/ui.json` after `up`
/// returns, and the port is free and unfenced the moment `down` kills
/// the board. A holder records whose claim it protects by writing
/// `sandbox:<name>` into the lock file — [`port_claim`] — so a later
/// `up` can tell "fenced for me" from "fenced by another test".
pub const TEST_PORT_LOCK_DIR: &str = "CADENCE_TEST_PORT_LOCK_DIR";

/// The lease dir [`TEST_PORT_LOCK_DIR`] names — created when absent
/// so the fence file can be opened in it.
fn port_lock_dir() -> Option<PathBuf> {
    let dir = std::env::var_os(TEST_PORT_LOCK_DIR).map(PathBuf::from)?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// `Ok(Some(f))` holds the port's lease until `f` drops; `Ok(None)`
/// when no lease dir is configured; `Err` when the lock file cannot be
/// opened or a cooperating process already holds the port's lease —
/// either way the port is not ours to take.
fn fenced_lease(lock_dir: &Option<PathBuf>, port: u16) -> std::io::Result<Option<std::fs::File>> {
    let Some(dir) = lock_dir else {
        return Ok(None);
    };
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(format!("{port}.lock")))?;
    use std::os::fd::AsRawFd;
    // SAFETY: plain syscall on a descriptor this function owns.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(Some(lock))
}

/// What the holder of a port's fence writes into its lock file while
/// it protects `name`'s claim — see [`TEST_PORT_LOCK_DIR`]. `up`
/// writes it when it fences; a suite member that keeps the fence
/// across `down` writes it too, and a later `up` for the same sandbox
/// reads it to know the hold is for its own claim.
pub fn port_claim(name: &str) -> String {
    format!("sandbox:{name}")
}

/// `true` when the port's fence is already held for this sandbox —
/// [`fenced_lease`] lost the `flock`, and the lock file says the
/// holder protects this sandbox's claim. `false` for no lock dir, an
/// unreadable file, or a foreign claim — either way the port is not
/// ours to take.
fn claim_held(lock_dir: &Option<PathBuf>, port: u16, sb: &Sandbox) -> bool {
    let Some(dir) = lock_dir else {
        return false;
    };
    std::fs::read_to_string(dir.join(format!("{port}.lock")))
        .is_ok_and(|text| text.trim() == port_claim(&sb.name))
}

fn bindable(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// The port `ui.json` under `state_dir` records.
fn persisted_port(state_dir: &Path) -> Option<u16> {
    let text = std::fs::read_to_string(state_dir.join("ui.json")).ok()?;
    let opts: Value = serde_json::from_str(&text).ok()?;
    opts["port"].as_u64().and_then(|p| u16::try_from(p).ok())
}

/// `--port` when given, else the sandbox's own last port, else the
/// first bindable port in 3110-3199 that no other sandbox records and
/// no lease fences. 3010 is production's in every case. The returned
/// lease (when a lock dir is configured) is held until the caller
/// drops it — `up` keeps it until the board has bound.
fn choose_port(
    sb: &Sandbox,
    wanted: Option<u16>,
    lock_dir: &Option<PathBuf>,
) -> Result<(u16, Option<std::fs::File>)> {
    choose_port_skip(sb, wanted, lock_dir, &[])
}

/// `choose_port` plus ports the caller already proved unusable — `up`'s
/// bind-collision retry must not re-pick a candidate whose own child
/// just died `AddrInUse`, and must not be pointed back at the failed
/// port's own `ui.json` record (a fresh allocation's persisted port is
/// the failed candidate's, not an established claim).
fn choose_port_skip(
    sb: &Sandbox,
    wanted: Option<u16>,
    lock_dir: &Option<PathBuf>,
    skip: &[u16],
) -> Result<(u16, Option<std::fs::File>)> {
    let state = sb.state_dir();
    let running = crate::ui::detached_pid(&state)
        .is_some()
        .then(|| persisted_port(&state))
        .flatten();
    // Where the port came from decides the hint when it cannot be used:
    // only a `--port` the caller typed can be "omitted".
    let (port, flag, mut fence) = match (wanted, running) {
        (Some(p), Some(r)) if p != r => {
            return Err(Error::rejected(format!(
                "sandbox '{}' board is running on port {r} — \
                 `cadence dev down {}` first to move it",
                sb.name, sb.name
            )))
        }
        (Some(p), _) => (p, true, None),
        (None, Some(r)) => (r, false, None),
        (None, None) => {
            // A `ui.json` port this `up` already proved a failed
            // bind is a skipped candidate, not an established claim.
            let persisted = persisted_port(&state).filter(|p| !skip.contains(p));
            match persisted {
                Some(p) => (p, false, None),
                None => {
                    let taken = sandbox_roots(&sb.base)
                        .iter()
                        .filter_map(|root| persisted_port(&root.join("state")))
                        .collect::<Vec<_>>();
                    let mut free = None;
                    for port in PORTS {
                        if taken.contains(&port) || skip.contains(&port) {
                            continue;
                        }
                        let Ok(lease) = fenced_lease(lock_dir, port) else {
                            continue;
                        };
                        if bindable(port) {
                            free = Some((port, lease));
                            break;
                        }
                    }
                    match free {
                        Some((p, lease)) => (p, false, lease),
                        None => {
                            return Err(Error::rejected(
                                "no free port in 3110-3199 — pass `--port <n>`",
                            ))
                        }
                    }
                }
            }
        }
    };
    let hint = if flag {
        "pass another `--port` or omit it"
    } else {
        "it is this sandbox's last port (state/ui.json); free it or pass \
         `--port <n>` to move the board"
    };
    if port == PRODUCTION_UI_PORT {
        return Err(Error::rejected(format!(
            "port {PRODUCTION_UI_PORT} is the production board — {hint}"
        )));
    }
    if running.is_none() {
        if fence.is_none() {
            match fenced_lease(lock_dir, port) {
                Ok(lease) => fence = lease,
                // A suite member may hold the fence for this sandbox's
                // own claim — its hold covers the bind, so no lease.
                Err(_) if claim_held(lock_dir, port, sb) => {}
                Err(_) => {
                    // The holder may have gone between the `flock`
                    // and the claim read — take the lease if it did.
                    fence = fenced_lease(lock_dir, port).map_err(|_| {
                        Error::rejected(format!("port {port} is in use on 127.0.0.1 — {hint}"))
                    })?;
                }
            }
        }
        if !bindable(port) {
            return Err(Error::rejected(format!(
                "port {port} is in use on 127.0.0.1 — {hint}"
            )));
        }
        if let Some(lease) = &fence {
            use std::io::Write;
            let mut lease = lease;
            let _ = lease.set_len(0);
            let _ = lease.write_all(port_claim(&sb.name).as_bytes());
        }
    }
    Ok((port, fence))
}

/// Under a sandbox profile, refuse the production board's port — the
/// default a board without a persisted port would otherwise take.
pub fn refuse_production_port(port: u16) -> Result<()> {
    production_port_gate(profile().as_deref(), port)
}

fn production_port_gate(profile: Option<&str>, port: u16) -> Result<()> {
    match profile {
        Some(name) if port == PRODUCTION_UI_PORT => Err(Error::rejected(format!(
            "port {PRODUCTION_UI_PORT} is the production board — refused under \
             CADENCE_PROFILE={PROFILE_PREFIX}{name}; pass `--port <n>`, or \
             `cadence dev up {name}` picks a free one"
        ))),
        _ => Ok(()),
    }
}

// ---------- verbs ----------

/// Single-quoted for `sh`.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn env_lines(sb: &Sandbox, port: Option<u16>, allow_global: bool) -> String {
    // A revoked grant must not linger in an eval'd shell: unset it.
    let unset = if allow_global {
        "CADENCE_ALIAS CADENCE_ROLLOUT_AS"
    } else {
        "CADENCE_ALIAS CADENCE_ROLLOUT_AS CADENCE_SANDBOX_ALLOW_GLOBAL"
    };
    let mut text = format!(
        "# cadence dev {name} — `eval \"$(cadence dev env {name})\"`\n\
         unset {unset}\n\
         export XDG_CONFIG_HOME={config}\n\
         export CADENCE_STATE_DIR={state}\n\
         export CADENCE_PM_DIR={pm}\n\
         export CADENCE_PROFILE={profile}\n\
         export CADENCE_SANDBOX_ROOT={base}\n",
        name = sb.name,
        unset = unset,
        config = sh_quote(&sb.config_dir().to_string_lossy()),
        state = sh_quote(&sb.state_dir().to_string_lossy()),
        pm = sh_quote(&sb.pm_dir().to_string_lossy()),
        profile = sh_quote(&sb.profile()),
        base = sh_quote(&sb.base.to_string_lossy()),
    );
    if let Ok(Some(profile)) = provider::load(&sb.root) {
        text.push_str(&profile.env_exports(&sb.root));
    }
    if allow_global {
        text.push_str(&format!("export {ALLOW_GLOBAL_ENV}=1\n"));
    }
    if let Some(port) = port {
        text.push_str(&format!(
            "# board: http://127.0.0.1:{port} (persisted in state/ui.json)\n"
        ));
    }
    text
}

/// `<exe> --state-dir <state> <args>` inside the sandbox env. A
/// sandbox never runs as the caller's pane identity or rollout holder.
/// The caller's own grant: `CADENCE_SANDBOX_ALLOW_GLOBAL=1` in this env.
fn env_grant() -> bool {
    std::env::var(ALLOW_GLOBAL_ENV).is_ok_and(|v| v == "1")
}

/// `grant` is passed to the child, never read off our own env here, so a
/// verb that must use the recorded grant (`reload`) cannot be fooled by —
/// or have to rewrite — the caller's env.
fn child(sb: &Sandbox, exe: &Path, args: &[&str], grant: bool) -> Command {
    let mut cmd = Command::new(exe);
    cmd.arg("--state-dir")
        .arg(sb.state_dir())
        .args(args)
        // CAD-1922: the sandbox is its own home through the profile the
        // resolver maps to this root, so the caller's CADENCE_HOME (a
        // consolidated install, a system install) never leaks its vault,
        // repos or workspace in. It is removed rather than shadowed: the
        // overlap guard reads it as production evidence.
        .env_remove("CADENCE_HOME")
        .env("XDG_CONFIG_HOME", sb.config_dir())
        .env("CADENCE_STATE_DIR", sb.state_dir())
        .env("CADENCE_PM_DIR", sb.pm_dir())
        .env("CADENCE_PROFILE", sb.profile())
        // The dev gate compares the store with the sandbox base: pin the
        // base the children (and the daemon) see to the one `up` used.
        .env("CADENCE_SANDBOX_ROOT", &sb.base)
        .env_remove("CADENCE_ALIAS")
        .env_remove("CADENCE_ROLLOUT_AS")
        .stdin(Stdio::null());
    // Children get exactly the grant `up` records: `1` or nothing.
    if grant {
        cmd.env(ALLOW_GLOBAL_ENV, "1");
    } else {
        cmd.env_remove(ALLOW_GLOBAL_ENV);
    }
    cmd
}

/// Run an exit-only child verb; successful output is deliberately ignored.
fn run_child(sb: &Sandbox, exe: &Path, args: &[&str], grant: bool) -> Result<()> {
    run_child_output(sb, exe, args, grant, &[]).map(|_| ())
}

/// Startup commands must report a usable state, not merely exit successfully.
fn run_child_state(
    sb: &Sandbox,
    exe: &Path,
    args: &[&str],
    grant: bool,
    env: &[(String, String)],
) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct ChildState {
        state: String,
    }

    let output = run_child_output(sb, exe, args, grant, env)?;
    // Struct decoding also accepts positional arrays; this protocol needs an object.
    let start = output
        .iter()
        .position(|byte| !matches!(*byte, b' ' | b'\n' | b'\r' | b'\t'))
        .unwrap_or(output.len());
    let output = &output[start..];
    if output.first() != Some(&b'{') {
        return Err(Error::rejected(format!(
            "`cadence {}` in dev '{}' returned invalid startup state: expected a JSON object",
            args.join(" "),
            sb.name
        )));
    }
    let response: ChildState = serde_json::from_slice(output).map_err(|error| {
        Error::rejected(format!(
            "`cadence {}` in dev '{}' returned invalid startup state: {error}",
            args.join(" "),
            sb.name
        ))
    })?;
    if response.state.trim().is_empty() {
        return Err(Error::rejected(format!(
            "`cadence {}` in dev '{}' returned an empty startup state",
            args.join(" "),
            sb.name
        )));
    }
    Ok(response.state)
}

/// Run a child with its own environment and retain stdout for state consumers.
/// `up` passes only the board retry's remaining budget, without changing this
/// process's environment or another child's.
fn run_child_output(
    sb: &Sandbox,
    exe: &Path,
    args: &[&str],
    grant: bool,
    env: &[(String, String)],
) -> Result<Vec<u8>> {
    let mut cmd = child(sb, exe, args, grant);
    match provider::load(&sb.root) {
        Ok(Some(profile)) => profile.apply_env(&mut cmd, &sb.root),
        Ok(None) => {}
        // A broken profile must not make `down` or `reload` unable to reach
        // the sandbox. Startup (`up`) validates it before any child starts.
        Err(error) => eprintln!("dev provider profile ignored: {error}"),
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    let out = crate::reaper::output(&mut cmd)?;
    if !out.status.success() {
        // The child's own error frame is typed: preserve its `kind`,
        // never reclassify from the message text. Only the one kind `up`
        // retries — a verified own-child `AddrInUse` — becomes the
        // distinct variant; every other kind is the ordinary wrap.
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let kind = serde_json::from_str::<Value>(&detail)
            .ok()
            .and_then(|v| v["kind"].as_str().map(str::to_string));
        let what = format!(
            "`cadence {}` in dev '{}' failed: {} — see {}/*.log; \
             `cadence dev down {}` stops what did start, \
             `cadence dev reset {}` starts the dev instance over",
            args.join(" "),
            sb.name,
            detail,
            sb.state_dir().display(),
            sb.name,
            sb.name
        );
        return Err(match kind.as_deref() {
            Some("ui_bind_in_use") => Error::ui_bind_in_use(what),
            _ => Error::rejected(what),
        });
    }
    Ok(out.stdout)
}

/// `sun_path` holds 108 bytes including the NUL — a longer socket path
/// fails only inside the detached daemon, so `up` checks it first.
const SOCKET_PATH_MAX: usize = 107;

/// One `up` at a time per sandbox: the grant is recorded before the
/// children start, and a concurrent caller must wait to see that
/// marker — an unlocked window let two `up`s with opposite grants both
/// pass the change check, leaving the winner's live processes under a
/// marker the loser overwrote.
fn up_lock(sb: &Sandbox) -> Result<std::fs::File> {
    std::fs::create_dir_all(&sb.base).map_err(|e| {
        Error::rejected(format!(
            "cannot create sandbox base {}: {e}",
            sb.base.display()
        ))
    })?;
    let path = sb.base.join(format!(".up-{}.lock", sb.name));
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| Error::rejected(format!("cannot open {}: {e}", path.display())))?;
    use std::os::fd::AsRawFd;
    // SAFETY: plain syscall on a descriptor this function owns; the
    // returned guard's drop releases it.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(Error::rejected(format!(
            "cannot lock {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }
    Ok(lock)
}

/// The sandbox's persisted `ui.json` still holds a tailnet share — an
/// ungranted `up` would refuse it at `ui start`, after the daemon is
/// already running and the marker revoked.
fn persisted_share(sb: &Sandbox) -> Result<bool> {
    // Share absence is a mutation prerequisite, not a lenient status
    // lookup: malformed roots or options must preserve the sandbox.
    crate::ui::has_persisted_share(&sb.state_dir())
}

fn up(sb: &Sandbox, wanted_port: Option<u16>, exe: &Path) -> Result<Value> {
    refuse_production(sb)?;
    // Refuse a malformed profile before any child starts; `down`/`reload`
    // stay reachable even if the profile later breaks.
    provider::load(&sb.root)?;
    let socket = client::socket_path(&sb.state_dir());
    let len = socket.as_os_str().len();
    if len > SOCKET_PATH_MAX {
        return Err(Error::rejected(format!(
            "socket path {} is {len} bytes, over the {SOCKET_PATH_MAX}-byte Unix socket \
             limit — point CADENCE_SANDBOX_ROOT at a shorter dir",
            socket.display()
        )));
    }
    let _up_lock = up_lock(sb)?;
    let existing = read_marker(sb)?;
    refuse_unmarked_nonempty(sb, existing.as_ref())?;
    // CAD-1283: eligibility for a bind-collision retry is captured
    // BEFORE the first `ui start` writes `ui.json` — after it,
    // `persisted_port` would always find the port `choose_port` picked
    // and a fresh automatic pick would be indistinguishable from a
    // persisted one. Only a fresh default allocation may retry.
    let fresh_auto = wanted_port.is_none()
        && persisted_port(&sb.state_dir()).is_none()
        && crate::ui::detached_pid(&sb.state_dir()).is_none();
    let lock_dir = port_lock_dir();
    // The chosen port's lease is held from the pick until `ui start`
    // has bound the board: a cooperating suite can neither take the
    // port we picked nor have its own leased port stolen in the gap.
    let (port, lease) = choose_port(sb, wanted_port, &lock_dir)?;
    // CAD-1922: the sandbox root is its own home — create it and the
    // state/tracker dirs privately (0700) rather than at the umask.
    crate::home::create_private_tree(&sb.root, &sb.state_dir())?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(sb.state_dir(), std::fs::Permissions::from_mode(0o700))?;
    }
    crate::home::create_private_tree(&sb.root, &sb.pm_dir())?;
    let created_at = existing
        .as_ref()
        .and_then(|m| m["created_at"].as_str().map(str::to_string))
        .unwrap_or_else(|| crate::issue::time::iso(crate::issue::time::now_epoch()));
    // The opt-in is granted at `up` and recorded in the marker —
    // `sandbox env` re-exports exactly what was granted. The grant is
    // process environment: changing it under a live daemon or board
    // would lie about what they run with, so it needs a down first.
    let allow_global = std::env::var(ALLOW_GLOBAL_ENV).is_ok_and(|v| v == "1");
    let recorded = existing
        .as_ref()
        .and_then(|m| m["allow_global"].as_bool())
        .unwrap_or(false);
    if recorded != allow_global
        && (daemon_running(&sb.state_dir()) || crate::ui::detached_pid(&sb.state_dir()).is_some())
    {
        return Err(Error::rejected(format!(
            "sandbox '{}' is running with {ALLOW_GLOBAL_ENV} {}; its processes \
             keep that environment — `cadence dev down {}` first, then `up` \
             with the grant you want",
            sb.name,
            if recorded { "granted" } else { "not granted" },
            sb.name
        )));
    }
    // Refuse before the marker is written and the daemon started: a
    // persisted tailnet share under a revoked (or never granted) opt-in
    // makes `ui start` fail, leaving a half-up sandbox.
    if !allow_global && persisted_share(sb)? {
        return Err(Error::rejected(format!(
            "sandbox '{}' still has a persisted tailnet share — an \
             ungranted `up` would refuse it at `ui start` with the \
             daemon already running. Stop sharing under the grant first: \
             `{ALLOW_GLOBAL_ENV}=1 cadence --state-dir {} ui tailscale \
             stop`, or `cadence dev reset {}` starts over",
            sb.name,
            sb.state_dir().display(),
            sb.name
        )));
    }
    let marker = json!({
        "name": sb.name,
        "created_at": created_at,
        "binary": exe,
        "profile": sb.profile(),
        "allow_global": allow_global,
    });
    std::fs::write(
        sb.marker(),
        serde_json::to_string_pretty(&marker).unwrap_or_default() + "\n",
    )?;
    std::fs::write(sb.env_file(), env_lines(sb, Some(port), allow_global))?;
    // CAD-1187: the store itself says it is a dev store, before any
    // daemon starts from it.
    write_dev_marker(sb)?;
    run_child(sb, exe, &["issue", "init"], allow_global)?;
    // The daemon starts once, before the board retry window opens: a
    // bind collision is the board's port, never the daemon's socket,
    // so a retried `ui start` reuses this daemon and nothing is
    // replayed.
    let daemon = run_child_state(sb, exe, &["daemon", "start"], allow_global, &[])?;
    // CAD-1283: only a fresh automatic allocation gets the retry
    // window and the remaining-budget env (a clamp, never a raise) —
    // an explicit `--port`, a persisted `ui.json` port or an
    // already-running board runs the single unchanged `ui start`.
    const BOARD_START_BUDGET: Duration = Duration::from_secs(60);
    let deadline = fresh_auto.then(|| Instant::now() + BOARD_START_BUDGET);
    // `tried` holds the ports this `up` already saw lose their bind,
    // so a retried pick can neither re-pick a failed candidate nor be
    // pointed back at the failed port's own `ui.json` record.
    let mut tried = Vec::new();
    let mut port = port;
    let mut lease = lease;
    let ui;
    // The `ui start` may retry a fresh automatic allocation when its
    // own child proved the bind lost to another listener
    // (`Error::UiBindInUse`). Each candidate's lease stays held until
    // that start's child provably bound or died; it is dropped only
    // after the verified own exit, before the next pick frees the
    // claim for a successor. Nothing else is replayed — no daemon
    // start, no marker, no tailnet, no device-login work.
    loop {
        // The retry budget env exists only on a fresh automatic
        // allocation's `ui start`; other candidates inherit nothing.
        let env = match (fresh_auto, deadline) {
            (true, Some(dl)) => {
                let left = dl.saturating_duration_since(Instant::now()).as_secs();
                if left == 0 {
                    return Err(Error::rejected(format!(
                        "sandbox '{}' board start ran out of its {}s retry window \
                         before the next candidate could start",
                        sb.name,
                        BOARD_START_BUDGET.as_secs()
                    )));
                }
                vec![(crate::ui::START_REMAINING_ENV.to_string(), left.to_string())]
            }
            _ => Vec::new(),
        };
        let port_arg = port.to_string();
        let result = run_child_state(
            sb,
            exe,
            &["ui", "start", "--port", &port_arg],
            allow_global,
            &env,
        );
        match result {
            // The verified bind collision — own dead child, exact
            // pid/nonce/kind frame — retries a fresh automatic
            // allocation only. An explicit `--port`, a persisted
            // `ui.json` port or an already-running board fails closed.
            Err(e @ Error::UiBindInUse(_)) if fresh_auto => {
                tried.push(port);
                // The failed candidate's own lease frees its claim for
                // the next pick — only now that its own child provably
                // exited and `start_inner` cleaned up.
                drop(lease);
                match choose_port_skip(sb, None, &lock_dir, &tried) {
                    Ok((next, next_lease)) => {
                        port = next;
                        lease = next_lease;
                    }
                    Err(_) => return Err(e),
                }
            }
            _ => {
                ui = result?;
                break;
            }
        }
    }
    // The board may have landed on a retried port; the env file must
    // name the port that actually bound, not the first candidate.
    if fresh_auto {
        std::fs::write(sb.env_file(), env_lines(sb, Some(port), allow_global))?;
    }
    Ok(json!({
        "name": sb.name,
        "root": sb.root,
        "state_dir": sb.state_dir(),
        "pm_dir": sb.pm_dir(),
        "port": port,
        "url": format!("http://127.0.0.1:{port}"),
        "env_file": sb.env_file(),
        "profile": sb.profile(),
        "daemon": daemon,
        "ui": ui,
    }))
}

/// The binary `dev up` starts the sandbox from when no `--build` is
/// given: the running one. Held to the same name rule as `--build`
/// ([`runnable`]), checked before anything is started, so the default
/// path cannot start a board `dev down` could never find (CAD-1207).
fn running_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    if !exe
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(crate::ui::is_cadence_exe_name)
    {
        return Err(Error::rejected(format!(
            "this binary {} is not named `cadence` or `cadence-<suffix>` — the board it \
             would start is found again by that name, so `dev down` could not stop it. \
             Nothing was started; run it under a conforming name or pass --build",
            exe.display()
        )));
    }
    Ok(exe)
}

/// A binary `--build` may name: an absolute, symlink-resolved regular
/// file with an execute bit.
fn runnable(path: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let real = std::fs::canonicalize(path)
        .map_err(|e| Error::rejected(format!("--build {} cannot be read ({e})", path.display())))?;
    let meta = std::fs::metadata(&real)?;
    // CAD-1206: the board's identity check (`ui stop`, `dev down`) only
    // recognises cadence-named binaries; any other name would leave the
    // board it starts running with nothing able to stop it.
    if !real
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(crate::ui::is_cadence_exe_name)
    {
        return Err(Error::rejected(format!(
            "--build {} must be named `cadence` or `cadence-<suffix>` — the board it \
             starts is found again by that name, and `dev down` could not stop it \
             otherwise",
            real.display()
        )));
    }
    if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
        return Err(Error::rejected(format!(
            "--build {} is not an executable file",
            path.display()
        )));
    }
    Ok(real)
}

/// The newest `target/{release,debug}/cadence` of the repo the cwd sits
/// in (the nearest ancestor holding a `Cargo.toml` and a `target/`).
fn newest_local_build() -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    let repo = cwd
        .ancestors()
        .find(|d| d.join("Cargo.toml").is_file() && d.join("target").is_dir())
        .ok_or_else(|| {
            Error::rejected(
                "no cargo `target/` dir above the cwd — build first, or pass --build <path>",
            )
        })?;
    ["release", "debug"]
        .iter()
        .map(|profile| repo.join("target").join(profile).join("cadence"))
        .filter_map(|bin| {
            let modified = std::fs::metadata(&bin).ok()?.modified().ok()?;
            Some((modified, bin))
        })
        .max()
        .map(|(_, bin)| bin)
        .ok_or_else(|| {
            Error::rejected(format!(
                "no target/release/cadence or target/debug/cadence under {} — build \
                 first, or pass --build <path>",
                repo.display()
            ))
        })
}

/// `dev reload`: restart the dev store's board and daemon from a local
/// binary. Every refusal happens before anything is stopped: the
/// target must not overlap production and must be a dev store
/// ([`dev_owner`]: dev marker in the store, root under the sandbox
/// base). It then calls only stop and start verbs — never the rollout
/// lease, a backup or an attestation check — and the child daemon's own
/// start gate admits the new build for the same reason `dev_owner`
/// holds.
fn reload(state_dir: &Path, name: Option<&str>, build: Option<&Path>) -> Result<Value> {
    // No name: the store this shell points at, never a fallback to
    // `dev` — a plain `--state-dir` must be refused, not redirected.
    let sb = match name {
        Some(name) => {
            let sb = Sandbox::open(name)?;
            // `--name` picks the store; a `--state-dir` (or exported
            // CADENCE_STATE_DIR) pointing anywhere else is a second,
            // conflicting target — refused, never silently ignored. The
            // untouched default state dir is "no choice made" and loses.
            let chosen = resolved(state_dir);
            let default = client::default_state_dir().map(|d| resolved(&d)).ok();
            if chosen != resolved(&sb.state_dir()) && Some(&chosen) != default.as_ref() {
                return Err(Error::rejected(format!(
                    "`dev reload --name {name}` targets {} but --state-dir / \
                     CADENCE_STATE_DIR points at {} — pass only one of them. \
                     Nothing was stopped or started",
                    sb.state_dir().display(),
                    state_dir.display()
                )));
            }
            sb
        }
        None => Sandbox::from_state_dir(state_dir).ok_or_else(|| {
            Error::rejected(format!(
                "{} is not a dev store — `dev reload` only restarts a store that \
                 `cadence dev up` created. Nothing was stopped or started; production \
                 builds ship with `cadence update`",
                state_dir.display()
            ))
        })?,
    };
    validate_name(&sb.name)?;
    refuse_production(&sb)?;
    let store = sb.state_dir();
    if resolved(&store) != resolved(state_dir) && name.is_none() {
        return Err(Error::rejected(
            "dev reload target does not match the store",
        ));
    }
    if dev_owner(&store)?.as_deref() != Some(sb.name.as_str()) {
        return Err(Error::rejected(format!(
            "{} is not a dev store — `dev reload` only restarts a store that `cadence dev up` \
             created: it must hold {DEV_MARKER} and sit directly under the sandbox base {}. \
             Nothing was stopped or started; production builds ship with `cadence update`",
            store.display(),
            sb.base.display()
        )));
    }
    let exe = match build {
        Some(path) => runnable(path)?,
        None => runnable(&newest_local_build()?)?,
    };
    // CAD-832: the global-write grant is process env fixed at `up`. A
    // reload restarts the processes with exactly the recorded grant, passed
    // to each child, never the caller's env.
    let grant = require_marker(&sb)?["allow_global"].as_bool() == Some(true);
    // Same check `up` makes, before anything is stopped: a persisted
    // tailnet share under an ungranted marker would make `ui start` refuse
    // after the daemon was already restarted.
    if !grant && persisted_share(&sb)? {
        return Err(Error::rejected(format!(
            "sandbox '{}' still has a persisted tailnet share but its marker records no \
             {ALLOW_GLOBAL_ENV} grant — `dev reload` would restart the daemon and then \
             refuse the board. Nothing was stopped or started. Stop sharing under the \
             grant first: `{ALLOW_GLOBAL_ENV}=1 cadence --state-dir {} ui tailscale stop`, \
             or `cadence dev reset {}` starts over",
            sb.name,
            sb.state_dir().display(),
            sb.name
        )));
    }
    let port = persisted_port(&store);
    let had_ui = crate::ui::detached_pid(&store).is_some();
    let had_daemon = daemon_running(&store);
    // The stop verbs come from the running side, the start verbs from
    // the new binary.
    let current = std::env::current_exe()?;
    if had_ui {
        run_child(&sb, &current, &["ui", "stop"], grant)?;
    }
    if had_daemon {
        run_child(&sb, &current, &["daemon", "stop"], grant)?;
    }
    let daemon = run_child_state(&sb, &exe, &["daemon", "start"], grant, &[])?;
    let ui = match port {
        Some(port) if had_ui => {
            let port_arg = port.to_string();
            run_child_state(&sb, &exe, &["ui", "start", "--port", &port_arg], grant, &[])?
        }
        _ => "not_running".to_string(),
    };
    Ok(json!({
        "name": sb.name,
        "build": exe,
        "daemon": daemon,
        "ui": ui,
        "port": port,
        "lease": "none claimed",
    }))
}

fn status(sb: &Sandbox) -> Value {
    let state = sb.state_dir();
    json!({
        "name": sb.name,
        "root": sb.root,
        "port": persisted_port(&state),
        "dev": matches!(dev_owner(&state), Ok(Some(_))),
        "daemon": if daemon_running(&state) { "running" } else { "stopped" },
        "ui": if crate::ui::detached_pid(&state).is_some() { "running" } else { "stopped" },
    })
}

fn daemon_running(state_dir: &Path) -> bool {
    client::rpc_timeout(state_dir, "health", json!({}), Duration::from_secs(5)).is_ok()
}

/// `agent_stop` every enabled agent that has an actor — stopped, not
/// removed, so a later `up` can resume it. Returns how many stopped.
fn stop_agents(state_dir: &Path) -> usize {
    let Ok(list) = client::rpc_state(state_dir, "agent_list", json!({})) else {
        return 0;
    };
    list["agents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| {
            a["enabled"].as_bool() != Some(false)
                && crate::adapter::registry::has_actor(
                    a["provider"].as_str().unwrap_or_default(),
                    a["endpoint_kind"].as_str().unwrap_or_default(),
                )
        })
        .filter_map(|a| a["alias"].as_str())
        .filter(|alias| client::rpc_state(state_dir, "agent_stop", json!({"alias": alias})).is_ok())
        .count()
}

/// Stop the sandbox: its agents, the board and the daemon through their
/// own stop verbs, then whatever its private tmux server still holds —
/// a daemon stop keeps pty panes for a hot restart a stopped sandbox
/// never gets, and `reset` deletes the state dir under them.
fn down(sb: &Sandbox) -> Result<Value> {
    let exe = std::env::current_exe()?;
    let state = sb.state_dir();
    let running = daemon_running(&state);
    let agents_stopped = if running { stop_agents(&state) } else { 0 };
    let ui = if crate::ui::detached_pid(&state).is_some() {
        run_child(sb, &exe, &["ui", "stop"], env_grant())?;
        "stopped"
    } else {
        "not_running"
    };
    let daemon = if running {
        run_child(sb, &exe, &["daemon", "stop"], env_grant())?;
        "stopped"
    } else {
        "not_running"
    };
    let panes_killed =
        crate::adapter::pty::kill_server(&state, &crate::adapter::ProviderEnv::default());
    Ok(json!({
        "name": sb.name,
        "root": sb.root,
        "ui": ui,
        "daemon": daemon,
        "agents_stopped": agents_stopped,
        "panes_killed": panes_killed,
    }))
}

/// Delete the root — only a direct child of the sandbox base (after
/// symlinks) holding this sandbox's marker, stopped first. The root is
/// checked again after the stop: nothing may swap it in between.
fn reset(sb: &Sandbox) -> Result<Value> {
    refuse_production(sb)?;
    // Serialize the entire inspection/stop/delete sequence with `up`:
    // startup must not continue in a root reset has already removed.
    let _up_lock = up_lock(sb)?;
    if std::fs::symlink_metadata(&sb.root).is_err() {
        return Err(Error::rejected(format!(
            "no dev instance '{}' at {} — `cadence dev ls` lists them",
            sb.name,
            sb.root.display()
        )));
    }
    removable(sb)?;
    // A persisted share outlives `down` — remove it while its record
    // still exists, or refuse so reset cannot orphan a live mapping
    // onto a port another service may take.
    if persisted_share(sb)? {
        if !std::env::var(ALLOW_GLOBAL_ENV).is_ok_and(|v| v == "1") {
            return Err(Error::rejected(format!(
                "sandbox '{}' still has a persisted tailnet share — reset \
                 would orphan the live mapping. Stop it under the opt-in \
                 first: `{ALLOW_GLOBAL_ENV}=1 cadence --state-dir {} ui \
                 tailscale stop`, or re-run reset with `{ALLOW_GLOBAL_ENV}=1`",
                sb.name,
                sb.state_dir().display()
            )));
        }
        let exe = std::env::current_exe()?;
        run_child(sb, &exe, &["ui", "tailscale", "stop"], env_grant())?;
    }
    let stopped = down(sb)?;
    removable(sb)?;
    std::fs::remove_dir_all(&sb.root)?;
    Ok(json!({
        "name": sb.name,
        "root": sb.root,
        "state": "removed",
        "ui": stopped["ui"],
        "daemon": stopped["daemon"],
        "agents_stopped": stopped["agents_stopped"],
        "panes_killed": stopped["panes_killed"],
    }))
}

/// A root `reset` may delete: a real directory directly under the
/// sandbox base once symlinks resolve, holding this sandbox's marker.
fn removable(sb: &Sandbox) -> Result<()> {
    let inside_base = std::fs::canonicalize(&sb.root)
        .ok()
        .zip(std::fs::canonicalize(&sb.base).ok())
        .is_some_and(|(root, base)| root.parent() == Some(base.as_path()));
    if !inside_base
        || std::fs::symlink_metadata(&sb.root)?
            .file_type()
            .is_symlink()
    {
        return Err(Error::rejected(format!(
            "{} resolves outside the sandbox base {} — refusing to delete it; \
             remove it by hand if it is yours",
            sb.root.display(),
            sb.base.display()
        )));
    }
    require_marker(sb).map(|_| ())
}

/// Every direct child of `base` that holds a marker file.
fn sandbox_roots(base: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(base) else {
        return Vec::new();
    };
    let mut roots: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join(MARKER).is_file())
        .collect();
    roots.sort();
    roots
}

fn ls() -> Result<Value> {
    let base = base_dir()?;
    let sandboxes: Vec<Value> = sandbox_roots(&base)
        .iter()
        .map(|root| {
            let state = root.join("state");
            let marker: Value = std::fs::read_to_string(root.join(MARKER))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(Value::Null);
            json!({
                "name": marker["name"],
                "root": root,
                "created_at": marker["created_at"],
                "port": persisted_port(&state),
                "daemon": if daemon_running(&state) { "running" } else { "stopped" },
                "ui": if crate::ui::detached_pid(&state).is_some() { "running" } else { "stopped" },
            })
        })
        .collect();
    Ok(json!({"base": base, "sandboxes": sandboxes}))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Cad1922SandboxEnv(Vec<(&'static str, Option<std::ffi::OsString>)>);

    impl Cad1922SandboxEnv {
        fn isolated(home: &Path) -> Self {
            let names = [
                "HOME",
                "CADENCE_HOME",
                "CADENCE_STATE_DIR",
                "CADENCE_PM_DIR",
                "CADENCE_PROFILE",
                "CADENCE_SANDBOX_ROOT",
                "XDG_STATE_HOME",
                "XDG_CONFIG_HOME",
            ];
            let saved = Self(
                names
                    .iter()
                    .map(|&name| (name, std::env::var_os(name)))
                    .collect(),
            );
            for name in names {
                std::env::remove_var(name);
            }
            std::env::set_var("HOME", home);
            saved
        }
    }

    impl Drop for Cad1922SandboxEnv {
        fn drop(&mut self) {
            for (name, value) in &self.0 {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[test]
    fn cad1922_profile_home_refuses_cadence_home_installation() {
        let fixture = tempfile::tempdir().unwrap();
        let home = fixture.path().join("home");
        let root = fixture.path().join("live");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(MARKER), r#"{"name":"live"}"#).unwrap();
        let _env = Cad1922SandboxEnv::isolated(&home);
        std::env::set_var("CADENCE_HOME", &root);
        std::env::set_var("CADENCE_SANDBOX_ROOT", fixture.path());
        std::env::set_var("CADENCE_PROFILE", "sandbox:live");

        assert!(!root.join("config").exists());
        let error = active_home().unwrap_err().to_string();
        assert!(error.contains("overlap"), "{error}");
        assert!(error.contains("CADENCE_HOME"), "{error}");
        assert!(error.contains(&root.display().to_string()), "{error}");
        assert!(!root.join("config").exists());
    }

    #[test]
    fn cad1922_profile_home_accepts_disjoint_sandbox_own_exports() {
        let fixture = tempfile::tempdir().unwrap();
        let home = fixture.path().join("home");
        let base = fixture.path().join("sandboxes");
        let root = base.join("safe");
        let production = fixture.path().join("production");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(root.join("state")).unwrap();
        std::fs::create_dir_all(root.join("pm")).unwrap();
        std::fs::create_dir_all(&production).unwrap();
        std::fs::write(root.join(MARKER), r#"{"name":"safe"}"#).unwrap();
        let _env = Cad1922SandboxEnv::isolated(&home);
        std::env::set_var("CADENCE_HOME", &production);
        std::env::set_var("CADENCE_SANDBOX_ROOT", &base);
        std::env::set_var("CADENCE_PROFILE", "sandbox:safe");
        std::env::set_var("CADENCE_STATE_DIR", root.join("state"));
        std::env::set_var("CADENCE_PM_DIR", root.join("pm"));
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));

        assert_eq!(active_home().unwrap(), Some(root.clone()));
        assert!(!root.join("config").exists());
        assert!(!production.join("config").exists());
    }

    #[test]
    fn cad1922_adopt_pins_selected_sandbox_base() {
        let fixture = tempfile::tempdir().unwrap();
        let home = fixture.path().join("home");
        let first_base = fixture.path().join("first");
        let second_base = fixture.path().join("second");
        let first_root = first_base.join("smoke");
        let second_root = second_base.join("smoke");
        std::fs::create_dir_all(&home).unwrap();
        for root in [&first_root, &second_root] {
            std::fs::create_dir_all(root.join("state")).unwrap();
            std::fs::create_dir_all(root.join("pm")).unwrap();
            std::fs::write(root.join(MARKER), r#"{"name":"smoke"}"#).unwrap();
        }
        let _env = Cad1922SandboxEnv::isolated(&home);
        std::env::set_var("CADENCE_SANDBOX_ROOT", &second_base);
        std::env::set_var("CADENCE_PROFILE", "sandbox:smoke");

        // Both bases contain a usable sandbox with this name, so an
        // unchanged ambient base would select the wrong installation.
        assert_eq!(active_home().unwrap(), Some(second_root));
        assert_eq!(
            adopt(&first_root.join("state")).unwrap().as_deref(),
            Some("smoke")
        );
        assert_eq!(
            std::env::var_os("CADENCE_SANDBOX_ROOT"),
            Some(first_base.clone().into_os_string())
        );
        assert_eq!(base_dir().unwrap(), first_base);
        assert_eq!(Sandbox::open("smoke").unwrap().root, first_root);
        assert_eq!(active_home().unwrap(), Some(first_root));
    }

    #[test]
    fn cad1922_deleting_guard_refuses_planted_production_marker() {
        let fixture = tempfile::tempdir().unwrap();
        let home = fixture.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let _env = Cad1922SandboxEnv::isolated(&home);
        std::env::set_var("CADENCE_SANDBOX_ROOT", fixture.path().join("sandboxes"));

        let safe = Sandbox::open("safe").unwrap();
        std::fs::create_dir_all(safe.state_dir()).unwrap();
        std::fs::write(safe.marker(), r#"{"name":"safe"}"#).unwrap();
        require_marker(&safe).unwrap();
        refuse_production(&safe).unwrap();
        assert_eq!(
            owner_of(&safe.state_dir()).unwrap().as_deref(),
            Some("safe")
        );

        let planted = Sandbox {
            name: "pm".into(),
            base: home.clone(),
            root: home.join("pm"),
        };
        std::fs::create_dir_all(planted.state_dir()).unwrap();
        std::fs::write(planted.marker(), r#"{"name":"pm"}"#).unwrap();
        let canary = planted.root.join("ticket-canary");
        std::fs::write(&canary, b"existing tracker data\n").unwrap();
        require_marker(&planted).unwrap();

        let error = refuse_production(&planted).unwrap_err().to_string();
        assert!(error.contains("overlap"), "{error}");
        assert!(error.contains("production tracker"), "{error}");
        assert!(
            error.contains(&planted.root.display().to_string()),
            "{error}"
        );
        let error = owner_of(&planted.state_dir()).unwrap_err().to_string();
        assert!(error.contains("ungated"), "{error}");
        assert!(error.contains("overlap"), "{error}");
        assert_eq!(std::fs::read(&canary).unwrap(), b"existing tracker data\n");

        // The sandbox's own exported shell remains usable for store ownership
        // and read-only verbs, but exports cannot relax the deleting guard.
        std::env::set_var("CADENCE_PROFILE", safe.profile());
        std::env::set_var("CADENCE_STATE_DIR", safe.state_dir());
        std::env::set_var("CADENCE_PM_DIR", safe.pm_dir());
        std::env::set_var("XDG_CONFIG_HOME", safe.config_dir());
        assert_eq!(
            owner_of(&safe.state_dir()).unwrap().as_deref(),
            Some("safe")
        );
        refuse_production_readonly(&safe).unwrap();
        let error = refuse_production(&safe).unwrap_err().to_string();
        assert!(error.contains("overlap"), "{error}");
        assert!(error.contains("exported"), "{error}");

        // Even a matching forged profile cannot turn the host tracker into
        // disposable data, including through the read-only exemption.
        std::env::set_var("CADENCE_PROFILE", planted.profile());
        let error = refuse_production(&planted).unwrap_err().to_string();
        assert!(error.contains("production tracker"), "{error}");
        let error = refuse_production_readonly(&planted)
            .unwrap_err()
            .to_string();
        assert!(error.contains("production tracker"), "{error}");
    }

    #[test]
    fn state_socket_refuses_a_symlinked_socket() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("production.sock"),
            state.join("cadence.sock"),
        )
        .unwrap();
        let error = crate::client::rpc_state(&state, "health", json!({})).unwrap_err();
        assert!(
            error.to_string().contains("must not be a symlink"),
            "{error}"
        );
    }

    #[test]
    fn state_socket_refuses_a_hard_linked_socket() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let socket = state.join("cadence.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        if std::fs::hard_link(&socket, state.join("other.sock")).is_err() {
            return;
        }
        let error = crate::client::rpc_state(&state, "health", json!({})).unwrap_err();
        assert!(error.to_string().contains("hard-linked"), "{error}");
    }

    #[test]
    fn profile_parses_only_the_sandbox_prefix() {
        assert_eq!(
            parse_profile(Some("sandbox:smoke")).as_deref(),
            Some("smoke")
        );
        assert_eq!(parse_profile(Some("production")), None);
        assert_eq!(parse_profile(None), None);
    }

    #[test]
    fn global_gate_refuses_under_a_profile_and_honours_the_opt_in() {
        assert!(global_gate(None, None, "x").is_ok());
        let hard = global_gate(Some("s"), None, "`ui tailscale`").unwrap_err();
        assert!(hard.to_string().contains("sandbox:s"), "{hard}");
        assert!(global_gate(Some("s"), Some(false), "merge").is_err());
        assert!(global_gate(Some("s"), Some(true), "merge").is_ok());
    }

    #[test]
    fn names_are_one_safe_segment() {
        for ok in ["a", "smoke", "cad-310", "0", &"a".repeat(32)] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-a", "A", "a/b", "..", "a b", "a_b", &"a".repeat(33)] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn resolved_follows_symlinks_through_a_missing_tail() {
        let dir = tempfile::TempDir::new().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("link")).unwrap();
        assert_eq!(
            resolved(&dir.path().join("link/missing/x")),
            std::fs::canonicalize(&real).unwrap().join("missing/x")
        );
    }

    #[test]
    fn owner_of_needs_state_beside_a_marker_naming_its_root() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("sbx");
        std::fs::create_dir_all(root.join("state")).unwrap();
        // No marker: an ordinary state dir.
        assert_eq!(owner_of(&root.join("state")).unwrap(), None);
        std::fs::write(root.join(MARKER), r#"{"name":"sbx"}"#).unwrap();
        assert_eq!(
            owner_of(&root.join("state")).unwrap().as_deref(),
            Some("sbx")
        );
        // Only `<root>/state` belongs to the sandbox.
        assert_eq!(owner_of(&root.join("pm")).unwrap(), None);
        // A marker naming another sandbox, or unreadable, refuses.
        std::fs::write(root.join(MARKER), r#"{"name":"other"}"#).unwrap();
        let err = owner_of(&root.join("state")).unwrap_err();
        assert!(err.to_string().contains("ungated"), "{err}");
        std::fs::write(root.join(MARKER), "not json").unwrap();
        assert!(owner_of(&root.join("state")).is_err());
    }

    #[test]
    fn resolved_pops_dotdot_where_the_filesystem_would() {
        let dir = tempfile::TempDir::new().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(real.join("sub")).unwrap();
        std::os::unix::fs::symlink(real.join("sub"), dir.path().join("link")).unwrap();
        let canon = std::fs::canonicalize(&real).unwrap();
        // A `..` after a missing component pops it, as a create would.
        assert_eq!(resolved(&real.join("missing/../x")), canon.join("x"));
        // A `..` after a symlink leaves its target, not the link.
        assert_eq!(resolved(&dir.path().join("link/../y")), canon.join("y"));
    }

    #[test]
    fn production_port_is_refused_only_under_a_profile() {
        assert!(production_port_gate(None, 3010).is_ok());
        assert!(production_port_gate(Some("x"), 3111).is_ok());
        let err = production_port_gate(Some("x"), 3010).unwrap_err();
        assert!(err.to_string().contains("sandbox:x"), "{err}");
    }

    /// Only a `--port` the caller typed can be "omitted"; a taken port
    /// from `state/ui.json` says where it came from.
    #[test]
    fn a_taken_port_hint_names_where_the_port_came_from() {
        let dir = tempfile::TempDir::new().unwrap();
        let sb = Sandbox {
            name: "pt".into(),
            base: dir.path().to_path_buf(),
            root: dir.path().join("pt"),
        };
        std::fs::create_dir_all(sb.state_dir()).unwrap();
        let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        std::fs::write(
            sb.state_dir().join("ui.json"),
            format!(r#"{{"port":{port}}}"#),
        )
        .unwrap();
        let persisted = choose_port(&sb, None, &None).unwrap_err().to_string();
        assert!(persisted.contains("state/ui.json"), "{persisted}");
        assert!(!persisted.contains("omit it"), "{persisted}");
        let typed = choose_port(&sb, Some(port), &None).unwrap_err().to_string();
        assert!(typed.contains("omit it"), "{typed}");
    }

    /// A forged marker beside a `state` symlink onto another dir is
    /// refused, never adopted — it would exempt that dir's database from
    /// the rollout gates.
    #[test]
    fn owner_of_refuses_a_symlinked_state_beside_a_forged_marker() {
        let dir = tempfile::TempDir::new().unwrap();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let root = dir.path().join("x");
        std::fs::create_dir_all(&root).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("state")).unwrap();
        std::fs::write(root.join(MARKER), r#"{"name":"x"}"#).unwrap();
        let err = owner_of(&root.join("state")).unwrap_err();
        assert!(err.to_string().contains("not the root's own"), "{err}");
        assert!(!crate::rollout::sandbox_exempt(&root.join("state")));
    }

    /// CAD-1187: the dev gate is the store's. A root marker alone, a
    /// dev marker copied to another store, and a store outside the base
    /// are not dev stores; only the marker naming this very store under
    /// the base is.
    #[test]
    fn dev_owner_needs_the_marker_in_the_store_and_the_base() {
        let dir = tempfile::TempDir::new().unwrap();
        let base = dir.path().join("base");
        let root = base.join("sbx");
        let state = root.join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(root.join(MARKER), r#"{"name":"sbx"}"#).unwrap();
        // Root marker only (the pre-CAD-1187 exemption): not a dev store.
        assert_eq!(dev_owner_under(&state, Some(&base)).unwrap(), None);
        let sb = Sandbox::from_state_dir(&state).unwrap();
        write_dev_marker(&sb).unwrap();
        assert_eq!(
            dev_owner_under(&state, Some(&base)).unwrap().as_deref(),
            Some("sbx")
        );
        // Same store, but the base is somewhere else.
        assert_eq!(dev_owner_under(&state, Some(dir.path())).unwrap(), None);
        assert_eq!(dev_owner_under(&state, None).unwrap(), None);
        // A copied marker names another store's path.
        let other = dir.path().join("elsewhere/sbx");
        std::fs::create_dir_all(other.join("state")).unwrap();
        std::fs::write(other.join(MARKER), r#"{"name":"sbx"}"#).unwrap();
        std::fs::copy(state.join(DEV_MARKER), other.join("state").join(DEV_MARKER)).unwrap();
        assert_eq!(
            dev_owner_under(&other.join("state"), Some(&dir.path().join("elsewhere"))).unwrap(),
            None
        );
    }

    #[test]
    fn env_lines_quote_paths() {
        let sb = Sandbox {
            name: "q".into(),
            base: PathBuf::from("/t/it's"),
            root: PathBuf::from("/t/it's/q"),
        };
        let text = env_lines(&sb, Some(3111), false);
        assert!(
            text.contains(r"export CADENCE_STATE_DIR='/t/it'\''s/q/state'"),
            "{text}"
        );
        assert!(
            text.contains("export CADENCE_PROFILE='sandbox:q'"),
            "{text}"
        );
        assert!(
            text.contains(r"export CADENCE_SANDBOX_ROOT='/t/it'\''s'"),
            "{text}"
        );
        // Like the sandbox's own children, never a pane or rollout
        // identity — and no grant the marker did not record.
        assert!(
            text.contains("unset CADENCE_ALIAS CADENCE_ROLLOUT_AS CADENCE_SANDBOX_ALLOW_GLOBAL\n"),
            "{text}"
        );
        assert!(text.contains("127.0.0.1:3111"), "{text}");
    }

    /// The suite's port fence: a second opener on the same file is a
    /// different open-file-description, so its `flock` contends like
    /// another process's — no separate process needed.
    #[test]
    fn a_fenced_port_is_refused_until_the_lease_drops() {
        let dir = tempfile::TempDir::new().unwrap();
        let lock_dir = Some(dir.path().to_path_buf());
        // No dir configured: no protocol, nothing is held.
        assert!(fenced_lease(&None, 3111).unwrap().is_none());
        let held = fenced_lease(&lock_dir, 3111).unwrap();
        assert!(held.is_some());
        assert!(fenced_lease(&lock_dir, 3111).is_err());
        assert!(fenced_lease(&lock_dir, 3112).unwrap().is_some());
        drop(held);
        assert!(fenced_lease(&lock_dir, 3111).unwrap().is_some());
    }

    /// A fence the suite already holds for the sandbox's own claim is
    /// not a refusal — `choose_port` takes the port with no lease of
    /// its own while the holder keeps the bind window closed. A held
    /// fence with a foreign (or no) claim still refuses.
    #[test]
    fn a_fence_held_for_the_sandbox_is_not_a_refusal() {
        use std::io::Write;
        let dir = tempfile::TempDir::new().unwrap();
        let lock_dir = Some(dir.path().join("locks"));
        std::fs::create_dir_all(lock_dir.as_ref().unwrap()).unwrap();
        let free = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = free.local_addr().unwrap().port();
        drop(free);
        let sb = Sandbox {
            name: "iso".into(),
            base: dir.path().join("base"),
            root: dir.path().join("base").join("iso"),
        };
        std::fs::create_dir_all(sb.state_dir()).unwrap();
        std::fs::write(
            sb.state_dir().join("ui.json"),
            format!(r#"{{"port":{port}}}"#),
        )
        .unwrap();

        // The suite holds the fence without a claim: refused.
        let foreign = fenced_lease(&lock_dir, port).unwrap().unwrap();
        assert!(choose_port(&sb, None, &lock_dir).is_err());

        // The same hold named for this sandbox: proceeds, and the
        // caller's fence — not ours — covers the bind.
        let mut claim = &foreign;
        claim.write_all(port_claim("iso").as_bytes()).unwrap();
        let (picked, fence) = choose_port(&sb, None, &lock_dir).unwrap();
        assert_eq!(picked, port);
        assert!(fence.is_none());

        // Another sandbox's claim is foreign to this one.
        let other = Sandbox {
            name: "other".into(),
            base: dir.path().join("base"),
            root: dir.path().join("base").join("other"),
        };
        std::fs::create_dir_all(other.state_dir()).unwrap();
        std::fs::write(
            other.state_dir().join("ui.json"),
            format!(r#"{{"port":{port}}}"#),
        )
        .unwrap();
        assert!(choose_port(&other, None, &lock_dir).is_err());
    }

    /// CAD-1206: a board whose exe is not named like cadence is invisible
    /// to `ui stop`/`dev down` (the CAD-1081 identity check), so a build
    /// the board could not be found under is refused before any child is
    /// started, never left running as an orphan.
    #[test]
    fn runnable_refuses_a_build_the_board_could_not_be_found_under() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::Builder::new()
            .prefix("c1206-")
            .tempdir_in("/tmp")
            .unwrap();
        for (name, ok) in [
            ("cadence", true),
            ("cadence-new", true),
            ("cadence-old", true),
            ("a", false),
            ("cadencex", false),
            ("sleep", false),
        ] {
            let bin = dir.path().join(name);
            std::fs::write(&bin, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(runnable(&bin).is_ok(), ok, "{name}");
        }
    }
}

#[cfg(test)]
mod agenticos_approve_tests {
    use super::*;

    #[test]
    fn pending_approvals_selects_only_exact_waiting_states() {
        let runs = json!({"runs": [
            {"id": "run-a", "state": "awaiting_approval", "snapshot_digest": "sha256:a"},
            {"id": "run-b", "state": "approved", "snapshot_digest": "sha256:b"},
            {"id": "run-c", "state": "awaiting_approval"},
        ]});
        let effects = json!({"effects": [
            {"effect_id": "eff-a", "state": "waiting", "digest": "sha256:c"},
            {"effect_id": "eff-b", "state": "waiting", "digest": "sha256:d"},
            {"effect_id": "eff-c", "state": "approved", "digest": "sha256:e"},
            {"id": "eff-d", "state": "waiting"},
        ]});
        let pending = pending_approvals(&runs, &effects);
        assert_eq!(
            pending.runs,
            vec![("run-a".to_owned(), "sha256:a".to_owned())]
        );
        assert_eq!(
            pending.effects,
            vec![
                ("eff-a".to_owned(), "sha256:c".to_owned()),
                ("eff-b".to_owned(), "sha256:d".to_owned()),
            ]
        );
        assert_eq!(pending.skipped_runs.len(), 1);
        assert_eq!(pending.skipped_effects.len(), 1);
    }
}
