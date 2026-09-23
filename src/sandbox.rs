//! `cadence sandbox` — isolated dev instances on a host that also runs
//! the production daemon (CAD-310).
//!
//! A sandbox is one marked directory, `<base>/<name>`, holding its own
//! state dir (socket, database, logs), its own PM dir and a recorded UI
//! port from 3110 up. `up` starts a daemon and a UI from the invoking
//! binary with `CADENCE_STATE_DIR`, `CADENCE_PM_DIR` and
//! `CADENCE_PROFILE=sandbox:<name>` set, so every child those start —
//! agent panes included — inherits the sandbox.
//!
//! The base is `$CADENCE_SANDBOX_ROOT`, else
//! `$XDG_DATA_HOME/cadence/sandboxes`, else
//! `~/.local/share/cadence/sandboxes`. Not `$TMPDIR`: a sandbox is kept
//! across a working session and reused by `up`, a tmp reaper or reboot
//! would drop it mid-use, and macOS's long `$TMPDIR` pushes the socket
//! path toward the 104/108-byte `sun_path` limit.
//!
//! Safety is fail-closed at three layers:
//! - `up`/`down`/`reset` refuse any sandbox whose root, state dir,
//!   socket or PM dir lands on, inside or above a production default
//!   (`~/.local/state/cadence`, `$XDG_STATE_HOME/cadence`, `~/pm`), or
//!   whose port is 3010;
//! - `down`/`reset`/`env` act only on a real directory holding a
//!   regular marker file that names that directory and that sandbox;
//! - under `CADENCE_PROFILE=sandbox:*` the binary itself refuses the
//!   production dirs and port 3010, skips the `$HOME` skill sync,
//!   refuses tailscale sharing and `skill install`, and runs the
//!   provider WAL watcher observe-only.

use std::collections::HashSet;
use std::ffi::OsString;
use std::net::TcpListener;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use clap::Subcommand;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::client;
use crate::error::{Error, Result};

/// `sandbox:<name>` marks a sandboxed process tree.
pub const PROFILE_ENV: &str = "CADENCE_PROFILE";
/// Overrides where sandbox roots live.
pub const ROOT_ENV: &str = "CADENCE_SANDBOX_ROOT";
/// The file that makes a directory a sandbox root.
pub const MARKER: &str = "cadence-sandbox.json";
const MARKER_KIND: &str = "cadence-sandbox";
/// The production board's port — never a sandbox's.
pub const PRODUCTION_UI_PORT: u16 = 3010;
/// Where sandbox UI ports are chosen from.
pub const PORT_FIRST: u16 = 3110;
pub const PORT_LAST: u16 = 3199;
/// Keeps `<base>/<name>/state/cadence.sock` short and the name one
/// plain path segment.
const NAME_MAX: usize = 32;
/// Ports `up` tries before giving up when each one it picks is lost
/// to a concurrent binder.
const PORT_ATTEMPTS: usize = 5;

/// Identity the invoking shell may carry that must not leak into a
/// sandbox's daemon or UI (a cadence pane's alias, a rollout holder, a
/// build slot).
const SCRUBBED_ENV: &[&str] = &[
    "CADENCE_ALIAS",
    "CADENCE_DAEMON_ID",
    "CADENCE_ROLLOUT_AS",
    "CADENCE_BUILD_SLOT_TOKEN",
    "CADENCE_BUILD_SLOT_PID",
    "CADENCE_BUILD_SLOT_LANE",
];

#[derive(Subcommand)]
pub enum SandboxAction {
    /// Create (or reuse) the sandbox `<name>` and start its daemon and
    /// UI from this binary. Prints the dirs, the socket and the board
    /// URL; `eval "$(cadence sandbox env <name>)"` then points a shell
    /// at it.
    Up {
        name: String,
        /// Pin the UI port instead of picking a free one from 3110.
        /// 3010 is refused.
        #[arg(long)]
        port: Option<u16>,
        /// Serve the SPA from this directory (as `ui start --dist`).
        #[arg(long)]
        dist: Option<PathBuf>,
    },
    /// Print shell exports for the sandbox: `CADENCE_PROFILE`,
    /// `CADENCE_STATE_DIR`, `CADENCE_PM_DIR`.
    Env { name: String },
    /// Stop the sandbox's daemon and UI. Its data stays.
    Down { name: String },
    /// Stop the sandbox, then delete its root. Only a directory holding
    /// a matching marker file is ever deleted.
    Reset { name: String },
    /// List sandboxes with their port and daemon/UI state.
    Ls,
}

/// `cadence sandbox …`
pub fn run_cli(action: &SandboxAction) -> Result<i32> {
    match action {
        SandboxAction::Up { name, port, dist } => up(name, *port, dist.as_deref()),
        SandboxAction::Env { name } => env(name),
        SandboxAction::Down { name } => down(name),
        SandboxAction::Reset { name } => reset(name),
        SandboxAction::Ls => ls(),
    }
}

// ---------- the profile ----------

/// The sandbox name when `value` is a sandbox profile. Anything
/// starting `sandbox` counts — a malformed `sandbox` or `sandbox:` is
/// still gated, never treated as production.
fn parse_profile(value: Option<&str>) -> Option<String> {
    let rest = value?.trim().strip_prefix("sandbox")?;
    Some(rest.trim_start_matches(':').to_string())
}

/// The active sandbox name under `CADENCE_PROFILE=sandbox:<name>`.
pub fn profile() -> Option<String> {
    parse_profile(std::env::var(PROFILE_ENV).ok().as_deref())
}

/// Is this process running under a sandbox profile?
pub fn active() -> bool {
    profile().is_some()
}

/// Refuse `what` — a side effect outside the sandbox — under the
/// sandbox profile.
pub fn refuse_global(what: &str) -> Result<()> {
    match profile() {
        Some(name) => Err(Error::rejected(format!(
            "{what} is refused under the sandbox profile \
             ({PROFILE_ENV}=sandbox:{name}): it changes state outside the sandbox"
        ))),
        None => Ok(()),
    }
}

fn port_refusal(port: u16) -> Option<String> {
    (port == PRODUCTION_UI_PORT).then(|| {
        format!("port {PRODUCTION_UI_PORT} is the production board — a sandbox must not use it")
    })
}

/// Under the sandbox profile, refuse a UI on port 3010.
pub fn refuse_port(port: u16) -> Result<()> {
    match port_refusal(port) {
        Some(why) if active() => Err(Error::rejected(why)),
        _ => Ok(()),
    }
}

/// Under the sandbox profile, refuse a process whose state dir or PM
/// dir is a production default. `main` runs this before any command.
pub fn guard_runtime(state_dir: &Path) -> Result<()> {
    if !active() {
        return Ok(());
    }
    let prod = Production::from_env()?;
    let pm_dir = crate::issue::default_dir()?;
    check_isolated(&prod, state_dir, &pm_dir, None)
}

// ---------- production defaults and path checks ----------

/// The production defaults a sandbox must stay clear of.
pub struct Production {
    home: PathBuf,
    state_dirs: Vec<PathBuf>,
    pm_dir: PathBuf,
}

impl Production {
    /// From `$HOME` (and `$XDG_STATE_HOME` when set) — the same
    /// resolution `client::state_dir` and `issue::default_dir` apply
    /// when no override is given.
    pub fn from_env() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .ok_or_else(|| Error::rejected("HOME is not set to an absolute path"))?;
        let xdg = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute());
        Ok(Self::at(&home, xdg.as_deref()))
    }

    fn at(home: &Path, xdg_state: Option<&Path>) -> Self {
        let mut state_dirs = vec![home.join(".local/state/cadence")];
        if let Some(xdg) = xdg_state {
            state_dirs.push(xdg.join("cadence"));
        }
        Self {
            home: home.to_path_buf(),
            state_dirs,
            pm_dir: home.join("pm"),
        }
    }
}

/// Absolute, symlink-resolved form of `path` whether or not it exists:
/// the longest existing ancestor is canonicalized, the rest appended.
fn resolve(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut rest: Vec<OsString> = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            let mut out = real;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// One path equals, contains or lies inside the other.
fn overlaps(a: &Path, b: &Path) -> bool {
    let (a, b) = (resolve(a), resolve(b));
    a.starts_with(&b) || b.starts_with(&a)
}

/// Refuse a state dir, socket or PM dir on, inside or above a
/// production default, and port 3010.
fn check_isolated(
    prod: &Production,
    state_dir: &Path,
    pm_dir: &Path,
    port: Option<u16>,
) -> Result<()> {
    let socket = client::socket_path(state_dir);
    for live in &prod.state_dirs {
        for (what, path) in [("state dir", state_dir), ("PM dir", pm_dir)] {
            if overlaps(path, live) {
                return Err(Error::rejected(format!(
                    "sandbox {what} {} overlaps the production state dir {} — refused",
                    path.display(),
                    live.display()
                )));
            }
        }
        if resolve(&socket) == resolve(&client::socket_path(live)) {
            return Err(Error::rejected(format!(
                "sandbox socket {} is the production socket — refused",
                socket.display()
            )));
        }
    }
    for (what, path) in [("state dir", state_dir), ("PM dir", pm_dir)] {
        if overlaps(path, &prod.pm_dir) {
            return Err(Error::rejected(format!(
                "sandbox {what} {} overlaps the production PM dir {} — refused",
                path.display(),
                prod.pm_dir.display()
            )));
        }
    }
    if let Some(why) = port.and_then(port_refusal) {
        return Err(Error::rejected(why));
    }
    Ok(())
}

/// A sandbox root is never `/`, `$HOME` or above it, and never on,
/// inside or above a production dir.
fn check_root(prod: &Production, root: &Path) -> Result<()> {
    let real = resolve(root);
    if real.parent().is_none() || resolve(&prod.home).starts_with(&real) {
        return Err(Error::rejected(format!(
            "refusing sandbox root {}: it is / or $HOME or above it",
            root.display()
        )));
    }
    for live in prod.state_dirs.iter().chain([&prod.pm_dir]) {
        if overlaps(root, live) {
            return Err(Error::rejected(format!(
                "refusing sandbox root {}: it overlaps the production dir {}",
                root.display(),
                live.display()
            )));
        }
    }
    Ok(())
}

// ---------- layout and marker ----------

fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= NAME_MAX
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-');
    if ok {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "sandbox name {name:?} must be 1-{NAME_MAX} of [a-z0-9-], not starting with '-'"
        )))
    }
}

/// Where sandbox roots live — see the module doc.
pub fn base_dir() -> Result<PathBuf> {
    let nonempty = |v: OsString| (!v.is_empty()).then(|| PathBuf::from(v));
    let base = if let Some(root) = std::env::var_os(ROOT_ENV).and_then(nonempty) {
        root
    } else if let Some(data) = std::env::var_os("XDG_DATA_HOME")
        .and_then(nonempty)
        .filter(|p| p.is_absolute())
    {
        data.join("cadence/sandboxes")
    } else {
        Production::from_env()?
            .home
            .join(".local/share/cadence/sandboxes")
    };
    if !base.is_absolute() || base.components().any(|c| c == Component::ParentDir) {
        return Err(Error::rejected(format!(
            "sandbox base {} must be an absolute path without '..'",
            base.display()
        )));
    }
    Ok(base)
}

struct Layout {
    name: String,
    base: PathBuf,
    root: PathBuf,
    state_dir: PathBuf,
    pm_dir: PathBuf,
}

impl Layout {
    fn resolve(name: &str) -> Result<Self> {
        validate_name(name)?;
        Ok(Self::at(&base_dir()?, name))
    }

    fn at(base: &Path, name: &str) -> Self {
        let root = base.join(name);
        Self {
            name: name.to_string(),
            base: base.to_path_buf(),
            state_dir: root.join("state"),
            pm_dir: root.join("pm"),
            root,
        }
    }

    fn marker(&self) -> PathBuf {
        self.root.join(MARKER)
    }

    fn socket(&self) -> PathBuf {
        client::socket_path(&self.state_dir)
    }

    fn profile(&self) -> String {
        format!("sandbox:{}", self.name)
    }
}

/// `<root>/cadence-sandbox.json` — names the sandbox and its root, and
/// records the UI port.
#[derive(Serialize, Deserialize, Clone)]
struct Marker {
    kind: String,
    version: u32,
    name: String,
    root: PathBuf,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    created_at: u64,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Read and verify the marker of an existing root. Refuses a missing
/// root, a symlinked root or marker, a missing or unreadable marker,
/// and a marker naming another sandbox or another directory.
fn read_marker(layout: &Layout, prod: &Production) -> Result<Marker> {
    let root = &layout.root;
    let meta = std::fs::symlink_metadata(root).map_err(|_| {
        Error::rejected(format!(
            "no sandbox named {} at {}",
            layout.name,
            root.display()
        ))
    })?;
    if meta.file_type().is_symlink() {
        return Err(Error::rejected(format!(
            "sandbox root {} is a symlink — refused",
            root.display()
        )));
    }
    if !meta.is_dir() {
        return Err(Error::rejected(format!(
            "sandbox root {} is not a directory — refused",
            root.display()
        )));
    }
    check_root(prod, root)?;
    let marker = layout.marker();
    let meta = std::fs::symlink_metadata(&marker).map_err(|_| {
        Error::rejected(format!(
            "{} holds no {MARKER} marker — not a cadence sandbox, refused",
            root.display()
        ))
    })?;
    if meta.file_type().is_symlink() {
        return Err(Error::rejected(format!(
            "sandbox marker {} is a symlink — refused",
            marker.display()
        )));
    }
    if !meta.is_file() {
        return Err(Error::rejected(format!(
            "sandbox marker {} is not a regular file — refused",
            marker.display()
        )));
    }
    let parsed: Marker = std::fs::read(&marker)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(|| {
            Error::rejected(format!(
                "sandbox marker {} is unreadable — refused",
                marker.display()
            ))
        })?;
    if parsed.kind != MARKER_KIND
        || parsed.name != layout.name
        || resolve(&parsed.root) != resolve(root)
    {
        return Err(Error::rejected(format!(
            "sandbox marker {} names sandbox {:?} at {}, not {:?} at {} — refused",
            marker.display(),
            parsed.name,
            parsed.root.display(),
            layout.name,
            root.display()
        )));
    }
    Ok(parsed)
}

fn write_marker(layout: &Layout, marker: &Marker) -> Result<()> {
    let tmp = layout.root.join(format!(".{MARKER}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(marker)?)?;
    std::fs::rename(&tmp, layout.marker())?;
    Ok(())
}

// ---------- ports ----------

fn port_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Ports other sandboxes under `base` have recorded — kept for them
/// while they are down.
fn sibling_ports(base: &Path, except: &str) -> HashSet<u16> {
    let Ok(entries) = std::fs::read_dir(base) else {
        return HashSet::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_name() != except)
        .filter_map(|e| std::fs::read(e.path().join(MARKER)).ok())
        .filter_map(|b| serde_json::from_slice::<Marker>(&b).ok())
        .filter_map(|m| m.port)
        .collect()
}

/// The first port from 3110 no sibling has recorded, not in `skip`,
/// that binds right now. A concurrent binder can still win it between
/// this probe and the UI's bind; `up` detects that and picks again.
fn pick_port(layout: &Layout, skip: &HashSet<u16>) -> Result<u16> {
    let taken = sibling_ports(&layout.base, &layout.name);
    (PORT_FIRST..=PORT_LAST)
        .filter(|p| *p != PRODUCTION_UI_PORT && !skip.contains(p) && !taken.contains(p))
        .find(|p| port_free(*p))
        .ok_or_else(|| {
            Error::rejected(format!(
                "no free sandbox UI port in {PORT_FIRST}-{PORT_LAST}"
            ))
        })
}

// ---------- children ----------

struct Child {
    ok: bool,
    value: Value,
    stderr: String,
}

/// `<this binary> --state-dir <state> <args>` with the sandbox env —
/// the daemon and UI it starts inherit it.
fn cadence(layout: &Layout, args: &[&str]) -> Result<Child> {
    let mut cmd = Command::new(std::env::current_exe()?);
    for key in SCRUBBED_ENV {
        cmd.env_remove(key);
    }
    cmd.env("CADENCE_STATE_DIR", &layout.state_dir)
        .env("CADENCE_PM_DIR", &layout.pm_dir)
        .env(PROFILE_ENV, layout.profile())
        .arg("--state-dir")
        .arg(&layout.state_dir)
        .args(args)
        .stdin(Stdio::null());
    let out = cmd.output()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(Child {
        ok: out.status.success(),
        value: serde_json::from_str(&stdout).unwrap_or_else(|_| json!(stdout.trim())),
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    })
}

fn child_error(what: &str, child: &Child) -> Error {
    Error::rejected(format!(
        "sandbox {what} failed: {}",
        if child.stderr.is_empty() {
            child.value.to_string()
        } else {
            child.stderr.clone()
        }
    ))
}

/// Does the UI answering on `port` serve this sandbox's PM dir? Proves
/// our server won the bind, not a concurrent one on the same port.
fn ui_owned(layout: &Layout, port: u16) -> bool {
    crate::ui::http_get(
        "127.0.0.1",
        port,
        "/api/health",
        &format!("127.0.0.1:{port}"),
        &[],
    )
    .ok()
    .and_then(|(_, body)| serde_json::from_str::<Value>(&body).ok())
    .is_some_and(|h| h["pm_dir"].as_str() == Some(&layout.pm_dir.to_string_lossy()))
}

fn daemon_answers(layout: &Layout) -> bool {
    client::rpc_timeout(
        &layout.state_dir,
        "health",
        json!({}),
        Duration::from_secs(2),
    )
    .is_ok()
}

/// No process holds `cadence.lock` — the daemon has exited.
fn lock_free(state_dir: &Path) -> bool {
    use std::os::unix::io::AsRawFd;
    let Ok(file) = std::fs::OpenOptions::new()
        .write(true)
        .open(state_dir.join("cadence.lock"))
    else {
        return true;
    };
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

// ---------- verbs ----------

fn up(name: &str, port_flag: Option<u16>, dist: Option<&Path>) -> Result<i32> {
    let layout = Layout::resolve(name)?;
    let prod = Production::from_env()?;
    // Every refusal happens before anything is written. An existing
    // root must be a real, marked directory (`read_marker` checks the
    // root too); a new one must pass the root check alone.
    let existing = match std::fs::symlink_metadata(&layout.root) {
        Ok(_) => Some(read_marker(&layout, &prod)?),
        Err(_) => {
            check_root(&prod, &layout.root)?;
            None
        }
    };
    check_isolated(&prod, &layout.state_dir, &layout.pm_dir, port_flag)?;
    let reused = existing.is_some();
    let ui_running = reused && crate::ui::detached_pid(&layout.state_dir).is_some();
    let recorded = existing.as_ref().and_then(|m| m.port);
    if let Some(port) = port_flag {
        if ui_running && recorded != Some(port) {
            return Err(Error::rejected(format!(
                "sandbox {name} UI already runs on port {} — `cadence sandbox down {name}` first",
                recorded.map(|p| p.to_string()).unwrap_or_default()
            )));
        }
        if !ui_running && !port_free(port) {
            return Err(Error::rejected(format!(
                "port {port} is in use — pick another or omit --port"
            )));
        }
    }

    let mut marker = match existing {
        Some(marker) => marker,
        None => {
            std::fs::create_dir_all(&layout.base)?;
            std::fs::create_dir(&layout.root)
                .map_err(|e| Error::rejected(format!("create {}: {e}", layout.root.display())))?;
            let marker = Marker {
                kind: MARKER_KIND.to_string(),
                version: 1,
                name: layout.name.clone(),
                root: layout.root.clone(),
                port: None,
                created_at: now_secs(),
            };
            write_marker(&layout, &marker)?;
            marker
        }
    };

    std::fs::create_dir_all(&layout.state_dir)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&layout.state_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    init_pm(&layout)?;
    // The dirs exist now: re-resolve through any symlink planted since.
    read_marker(&layout, &prod)?;
    check_isolated(&prod, &layout.state_dir, &layout.pm_dir, port_flag)?;

    let daemon = cadence(&layout, &["daemon", "start"])?;
    if !daemon.ok {
        return Err(child_error("daemon start", &daemon));
    }

    let mut skip = HashSet::new();
    let mut port = match (port_flag, recorded) {
        (Some(p), _) => p,
        (None, Some(p)) if ui_running || port_free(p) => p,
        _ => pick_port(&layout, &skip)?,
    };
    let mut ui = Value::Null;
    for attempt in 1..=PORT_ATTEMPTS {
        marker.port = Some(port);
        write_marker(&layout, &marker)?;
        let port_arg = port.to_string();
        let mut args = vec!["ui", "start", "--host", "127.0.0.1", "--port", &port_arg];
        let dist_arg = dist.map(|d| d.to_string_lossy().into_owned());
        if let Some(d) = &dist_arg {
            args.extend(["--dist", d]);
        }
        let child = cadence(&layout, &args)?;
        if child.ok && ui_owned(&layout, port) {
            ui = child.value;
            break;
        }
        // Lost the port to a concurrent binder (or the UI failed):
        // clear our pidfile, then pick again unless the port is pinned.
        let _ = cadence(&layout, &["ui", "stop"]);
        if port_flag.is_some() || attempt == PORT_ATTEMPTS {
            return Err(child_error(&format!("ui start on port {port}"), &child));
        }
        skip.insert(port);
        port = pick_port(&layout, &skip)?;
    }

    print_json(&json!({
        "name": layout.name,
        "profile": layout.profile(),
        "reused": reused,
        "root": layout.root,
        "state_dir": layout.state_dir,
        "pm_dir": layout.pm_dir,
        "socket": layout.socket(),
        "port": port,
        "ui_url": format!("http://127.0.0.1:{port}"),
        // `daemon start` prints the whole health document; the state
        // and pid are what a caller of `up` acts on.
        "daemon": {
            "state": daemon.value["state"],
            "pid": daemon.value["health"]["pid"],
        },
        "ui": ui,
        "env": format!("eval \"$(cadence sandbox env {})\"", layout.name),
    }));
    Ok(0)
}

/// A PM dir of the sandbox's own, its notes dir inside the root so
/// board reads never look at the host's handover notes.
fn init_pm(layout: &Layout) -> Result<()> {
    let pm_yaml = layout.pm_dir.join("pm.yaml");
    if !pm_yaml.exists() {
        std::fs::create_dir_all(&layout.pm_dir)?;
        let config = crate::issue::PmConfig {
            notes_dir: layout.root.join("notes").to_string_lossy().into_owned(),
            ..crate::issue::PmConfig::default()
        };
        let text =
            serde_yaml::to_string(&config).map_err(|e| Error::internal(format!("pm.yaml: {e}")))?;
        std::fs::write(&pm_yaml, text)?;
    }
    crate::issue::Pm::init(&layout.pm_dir)?;
    Ok(())
}

/// Stop the UI, then the daemon, and wait for the daemon's lock.
fn stop(layout: &Layout) -> Result<Value> {
    let ui = cadence(layout, &["ui", "stop"])?;
    if !ui.ok {
        return Err(child_error("ui stop", &ui));
    }
    let daemon = if daemon_answers(layout) {
        let child = cadence(layout, &["daemon", "stop"])?;
        if !child.ok {
            return Err(child_error("daemon stop", &child));
        }
        child.value
    } else {
        json!({"state": "not_running"})
    };
    if !lock_free(&layout.state_dir) {
        return Err(Error::rejected(format!(
            "sandbox {} daemon still holds {} — see {}",
            layout.name,
            layout.state_dir.join("cadence.lock").display(),
            layout.state_dir.join("daemon.log").display()
        )));
    }
    Ok(json!({"ui": ui.value, "daemon": daemon}))
}

/// Resolve and verify a sandbox that must already exist.
fn existing(name: &str) -> Result<(Layout, Production, Marker)> {
    let layout = Layout::resolve(name)?;
    let prod = Production::from_env()?;
    let marker = read_marker(&layout, &prod)?;
    check_isolated(&prod, &layout.state_dir, &layout.pm_dir, marker.port)?;
    Ok((layout, prod, marker))
}

fn down(name: &str) -> Result<i32> {
    let (layout, _, _) = existing(name)?;
    let stopped = stop(&layout)?;
    print_json(&json!({"name": layout.name, "state": "down", "stopped": stopped}));
    Ok(0)
}

fn reset(name: &str) -> Result<i32> {
    let (layout, prod, _) = existing(name)?;
    let stopped = stop(&layout)?;
    // Re-verify right before deleting: the root must still be the same
    // real, marked directory.
    read_marker(&layout, &prod)?;
    std::fs::remove_dir_all(&layout.root)?;
    print_json(&json!({
        "name": layout.name, "state": "deleted", "root": layout.root, "stopped": stopped,
    }));
    Ok(0)
}

/// POSIX single-quoting.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn env(name: &str) -> Result<i32> {
    let (layout, _, marker) = existing(name)?;
    println!(
        "# cadence sandbox {0}: eval \"$(cadence sandbox env {0})\"",
        layout.name
    );
    println!("export {PROFILE_ENV}={}", shell_quote(&layout.profile()));
    for (key, path) in [
        ("CADENCE_STATE_DIR", &layout.state_dir),
        ("CADENCE_PM_DIR", &layout.pm_dir),
    ] {
        println!("export {key}={}", shell_quote(&path.to_string_lossy()));
    }
    println!("unset CADENCE_ALIAS CADENCE_DAEMON_ID");
    if let Some(port) = marker.port {
        println!("# board: http://127.0.0.1:{port}");
    }
    Ok(0)
}

fn ls() -> Result<i32> {
    let base = base_dir()?;
    let prod = Production::from_env()?;
    let mut names: Vec<String> = std::fs::read_dir(&base)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| !n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    let mut rows = Vec::new();
    let mut ignored = Vec::new();
    for name in names {
        let layout = Layout::at(&base, &name);
        let verified = validate_name(&name).and_then(|_| read_marker(&layout, &prod));
        match verified {
            Ok(marker) => rows.push(json!({
                "name": name,
                "root": layout.root,
                "port": marker.port,
                "ui_url": marker.port.map(|p| format!("http://127.0.0.1:{p}")),
                "daemon": if daemon_answers(&layout) { "running" } else { "stopped" },
                "ui": if crate::ui::detached_pid(&layout.state_dir).is_some() {
                    "running"
                } else {
                    "stopped"
                },
            })),
            Err(e) => ignored.push(json!({"path": layout.root, "why": e.to_string()})),
        }
    }
    print_json(&json!({"base": base, "sandboxes": rows, "ignored": ignored}));
    Ok(0)
}

fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prod(home: &Path) -> Production {
        Production::at(home, None)
    }

    #[test]
    fn profile_parse_fails_closed() {
        assert_eq!(parse_profile(None), None);
        assert_eq!(parse_profile(Some("")), None);
        assert_eq!(parse_profile(Some("production")), None);
        assert_eq!(parse_profile(Some("sandbox:dev")).as_deref(), Some("dev"));
        // Malformed sandbox profiles are still sandboxes.
        assert_eq!(parse_profile(Some("sandbox")).as_deref(), Some(""));
        assert_eq!(parse_profile(Some(" sandbox: ")).as_deref(), Some(""));
    }

    #[test]
    fn port_3010_is_refused() {
        assert!(port_refusal(3010).unwrap().contains("3010"));
        assert!(port_refusal(3110).is_none());
        let home = tempfile::tempdir().unwrap();
        let p = prod(home.path());
        let own = home.path().join("sb/x");
        let err = check_isolated(&p, &own.join("state"), &own.join("pm"), Some(3010));
        assert!(err.unwrap_err().to_string().contains("3010"));
        assert!(check_isolated(&p, &own.join("state"), &own.join("pm"), Some(3111)).is_ok());
    }

    #[test]
    fn production_dirs_are_refused_on_inside_and_above() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let p = prod(h);
        let pm = h.join("sb/x/pm");
        for state in [
            h.join(".local/state/cadence"),
            h.join(".local/state/cadence/sub"),
            h.join(".local/state"),
            h.to_path_buf(),
        ] {
            let err = check_isolated(&p, &state, &pm, None).unwrap_err();
            assert!(err.to_string().contains("production"), "{err}");
        }
        let state = h.join("sb/x/state");
        for pm in [h.join("pm"), h.join("pm/inner")] {
            let err = check_isolated(&p, &state, &pm, None).unwrap_err();
            assert!(err.to_string().contains("production PM dir"), "{err}");
        }
        // $XDG_STATE_HOME/cadence is production too.
        let xdg = h.join("xdg");
        let p = Production::at(h, Some(&xdg));
        assert!(check_isolated(&p, &xdg.join("cadence"), &pm, None).is_err());
    }

    #[test]
    fn symlinks_resolve_before_comparing() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let live = h.join(".local/state/cadence");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::create_dir_all(h.join("sb")).unwrap();
        std::os::unix::fs::symlink(&live, h.join("sb/evil")).unwrap();
        let err = check_isolated(&prod(h), &h.join("sb/evil"), &h.join("sb/pm"), None);
        assert!(err.unwrap_err().to_string().contains("production"));
    }

    #[test]
    fn root_is_never_home_slash_or_production() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let p = prod(h);
        for root in [
            PathBuf::from("/"),
            h.to_path_buf(),
            h.parent().unwrap().to_path_buf(),
            h.join("pm"),
            h.join(".local/state/cadence"),
            h.join(".local/state"),
        ] {
            assert!(check_root(&p, &root).is_err(), "{}", root.display());
        }
        assert!(check_root(&p, &h.join(".local/share/cadence/sandboxes/x")).is_ok());
    }

    #[test]
    fn names_are_one_plain_segment() {
        for good in ["a", "dev-1", "x9"] {
            assert!(validate_name(good).is_ok(), "{good}");
        }
        for bad in ["", "-a", "A", "a/b", "..", ".", "a b", &"a".repeat(33)] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn shell_quote_survives_quotes() {
        assert_eq!(shell_quote("/a b"), "'/a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
