//! Sandbox lifecycle orchestration and inspection (`up`, `reload`, `down`,
//! `reset`, `status`, `ls`).

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::client;
use crate::error::{Error, Result};

use super::{
    base_dir, choose_port, choose_port_skip, dev_owner, env_grant, env_lines, persisted_port,
    port_lock_dir, provider, read_marker, refuse_production, refuse_unmarked_nonempty,
    require_marker, resolved, run_child, run_child_state, share_stop_command, up_lock,
    validate_name, write_dev_marker, Sandbox, ALLOW_GLOBAL_ENV, DEV_MARKER, MARKER,
    SOCKET_PATH_MAX,
};

/// The sandbox's persisted `ui.json` still holds a tailnet share — an
/// ungranted `up` would refuse it at `ui start`, after the daemon is
/// already running and the marker revoked.
fn persisted_share(sb: &Sandbox) -> Result<bool> {
    // Share absence is a mutation prerequisite, not a lenient status
    // lookup: malformed roots or options must preserve the sandbox.
    crate::ui::has_persisted_share(&sb.state_dir())
}

pub(super) fn up(sb: &Sandbox, wanted_port: Option<u16>, exe: &Path) -> Result<Value> {
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
             {}, or `cadence dev reset {}` starts over",
            sb.name,
            share_stop_command(sb),
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
pub(super) fn running_exe() -> Result<PathBuf> {
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
pub(super) fn runnable(path: &Path) -> Result<PathBuf> {
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
pub(super) fn reload(state_dir: &Path, name: Option<&str>, build: Option<&Path>) -> Result<Value> {
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
             grant first: {}, or `cadence dev reset {}` starts over",
            sb.name,
            share_stop_command(&sb),
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

pub(super) fn status(sb: &Sandbox) -> Value {
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
pub(super) fn down(sb: &Sandbox) -> Result<Value> {
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
pub(super) fn reset(sb: &Sandbox) -> Result<Value> {
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
                 first: {}, or re-run reset with `{ALLOW_GLOBAL_ENV}=1`",
                sb.name,
                share_stop_command(sb)
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
pub(super) fn sandbox_roots(base: &Path) -> Vec<PathBuf> {
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

pub(super) fn ls() -> Result<Value> {
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
