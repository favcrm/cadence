//! `cadence sandbox` — a disposable Cadence beside production (CAD-310).
//!
//! One marked root per name under the sandbox base: `.cadence-sandbox`
//! (the marker `reset` requires), `state/` (0700), `pm/` (its own
//! tracker) and `sandbox.env`. `up` starts a daemon and a board from the
//! invoking binary with `CADENCE_PROFILE=sandbox:<name>` exported, and
//! that profile is the one switch the daemon and the UI read to gate
//! global side effects: no skill sync into `$HOME`, no tailnet, an
//! observe-only provider WAL watcher, no Cursor `cli-config.json` merge.
//! Every verb first refuses a root that overlaps production's state
//! dir (and so its socket) or tracker.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use clap::Subcommand;
use serde_json::{json, Value};

use crate::client;
use crate::error::{Error, Result};

/// The file that makes a directory a sandbox root. `reset` deletes
/// nothing that lacks it.
const MARKER: &str = ".cadence-sandbox";
const PROFILE_PREFIX: &str = "sandbox:";
/// The production board's port — never a sandbox's.
const PRODUCTION_UI_PORT: u16 = 3010;
const PORTS: std::ops::RangeInclusive<u16> = 3110..=3199;
/// The operator's explicit opt-in for an overridable global write.
pub const ALLOW_GLOBAL_ENV: &str = "CADENCE_SANDBOX_ALLOW_GLOBAL";

#[derive(Subcommand)]
pub enum SandboxAction {
    /// Create (or reuse) the sandbox and start its daemon and board
    /// from this binary: its own state dir, tracker and a free port in
    /// 3110-3199. Prints the paths, URL and env file as JSON.
    Up {
        /// Sandbox name: [a-z0-9][a-z0-9-]{0,31}.
        name: String,
        /// Board port [default: the sandbox's last port, else the first
        /// free one in 3110-3199]. 3010 is refused.
        #[arg(long)]
        port: Option<u16>,
    },
    /// Print the sandbox's export lines, for
    /// `eval "$(cadence sandbox env <name>)"`.
    Env { name: String },
    /// Stop the sandbox's board and daemon; its files stay.
    Down { name: String },
    /// Stop the sandbox, then delete its root — only a directory under
    /// the sandbox base that holds this sandbox's marker.
    Reset { name: String },
    /// List the sandboxes under the base with running status.
    Ls,
}

/// `cadence sandbox …`
pub fn run_cli(action: &SandboxAction) -> Result<i32> {
    let out = match action {
        SandboxAction::Up { name, port } => up(&Sandbox::open(name)?, *port)?,
        SandboxAction::Env { name } => {
            let sb = Sandbox::open(name)?;
            refuse_production(&sb)?;
            require_marker(&sb)?;
            print!("{}", env_lines(&sb, persisted_port(&sb.state_dir())));
            return Ok(0);
        }
        SandboxAction::Down { name } => {
            let sb = Sandbox::open(name)?;
            refuse_production(&sb)?;
            require_marker(&sb)?;
            down(&sb)?
        }
        SandboxAction::Reset { name } => reset(&Sandbox::open(name)?)?,
        SandboxAction::Ls => ls()?,
    };
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    Ok(0)
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

/// Refuse `what` — a write to state shared by the whole host — under a
/// sandbox profile.
pub fn refuse_global(what: &str) -> Result<()> {
    global_gate(profile().as_deref(), None, what)
}

/// `refuse_global`, with `CADENCE_SANDBOX_ALLOW_GLOBAL=1` as the
/// operator's explicit opt-in.
pub fn refuse_global_unless_allowed(what: &str) -> Result<()> {
    let allowed = std::env::var(ALLOW_GLOBAL_ENV).is_ok_and(|v| v == "1");
    global_gate(profile().as_deref(), Some(allowed), what)
}

/// `allowed: None` — not overridable.
fn global_gate(profile: Option<&str>, allowed: Option<bool>, what: &str) -> Result<()> {
    let Some(name) = profile else {
        return Ok(());
    };
    match allowed {
        Some(true) => Ok(()),
        Some(false) => Err(Error::rejected(format!(
            "{what} is global to this host — refused under \
             CADENCE_PROFILE={PROFILE_PREFIX}{name}; set {ALLOW_GLOBAL_ENV}=1 to allow it"
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
/// `~/.local/state/cadence-sandbox`.
pub fn base_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CADENCE_SANDBOX_ROOT").filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir);
        if !dir.is_absolute() {
            return Err(Error::rejected(format!(
                "CADENCE_SANDBOX_ROOT '{}' is relative — export an absolute path",
                dir.display()
            )));
        }
        return Ok(dir);
    }
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir).join("cadence-sandbox"));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| {
            Error::rejected("HOME is not set to an absolute path — export CADENCE_SANDBOX_ROOT")
        })?;
    Ok(home.join(".local/state/cadence-sandbox"))
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
    fn env_file(&self) -> PathBuf {
        self.root.join("sandbox.env")
    }
    fn marker(&self) -> PathBuf {
        self.root.join(MARKER)
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
             e.g. `cadence sandbox up smoke`"
        )))
    }
}

// ---------- production guard ----------

/// `path` with symlinks resolved as far as it exists; the missing tail
/// is appended as written, so a not-yet-created dir under a symlinked
/// parent compares by where it would land.
fn resolved(path: &Path) -> PathBuf {
    let mut tail = Vec::new();
    let mut cur = path.to_path_buf();
    loop {
        if let Ok(real) = std::fs::canonicalize(&cur) {
            return tail.iter().rev().fold(real, |acc, c| acc.join(c));
        }
        match (cur.file_name().map(|n| n.to_os_string()), cur.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                cur = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// The live dirs a sandbox must never overlap, labelled for the
/// refusal: production's defaults (`CADENCE_STATE_DIR` and
/// `CADENCE_PM_DIR` unset), plus whatever the caller exports now — a
/// shell pointed at a live cadence — unless that shell is this very
/// sandbox's (`eval "$(cadence sandbox env <name>)"`).
fn production_dirs(sb: &Sandbox) -> Result<Vec<(&'static str, PathBuf)>> {
    let mut dirs = vec![
        ("the production state dir", client::default_state_dir()?),
        ("the production tracker", crate::issue::home_default_dir()?),
    ];
    if profile().as_deref() != Some(sb.name.as_str()) {
        if let Some(d) = std::env::var_os("CADENCE_STATE_DIR") {
            dirs.push(("the exported CADENCE_STATE_DIR", PathBuf::from(d)));
        }
        if let Some(d) = std::env::var_os("CADENCE_PM_DIR") {
            dirs.push(("the exported CADENCE_PM_DIR", PathBuf::from(d)));
        }
    }
    Ok(dirs)
}

/// Refuse a sandbox whose root, state dir or tracker overlaps a live
/// dir in either direction — equal (the socket lives in the state dir),
/// nested inside it, or containing it (a `reset` would delete it).
fn refuse_production(sb: &Sandbox) -> Result<()> {
    let ours = [sb.root.clone(), sb.state_dir(), sb.pm_dir()].map(|p| resolved(&p));
    for (label, dir) in production_dirs(sb)? {
        let live = resolved(&dir);
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
        Err(e) => return Err(e.into()),
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
             (`cadence sandbox ls` lists them)",
            sb.root.display(),
            marker["name"],
            sb.name
        )));
    }
    Ok(Some(marker))
}

fn require_marker(sb: &Sandbox) -> Result<Value> {
    read_marker(sb)?.ok_or_else(|| {
        Error::rejected(format!(
            "no sandbox '{}' at {} — `cadence sandbox up {}` creates it",
            sb.name,
            sb.root.display(),
            sb.name
        ))
    })
}

// ---------- port ----------

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
/// first bindable port in 3110-3199 that no other sandbox records.
/// 3010 is production's in every case.
fn choose_port(sb: &Sandbox, wanted: Option<u16>) -> Result<u16> {
    let state = sb.state_dir();
    let running = crate::ui::detached_pid(&state)
        .is_some()
        .then(|| persisted_port(&state))
        .flatten();
    let port = match (wanted, running) {
        (Some(p), Some(r)) if p != r => {
            return Err(Error::rejected(format!(
                "sandbox '{}' board is running on port {r} — \
                 `cadence sandbox down {}` first to move it",
                sb.name, sb.name
            )))
        }
        (Some(p), _) => p,
        (None, Some(r)) => r,
        (None, None) => match persisted_port(&state) {
            Some(p) => p,
            None => {
                let taken = sandbox_roots(&sb.base)
                    .iter()
                    .filter_map(|root| persisted_port(&root.join("state")))
                    .collect::<Vec<_>>();
                PORTS
                    .clone()
                    .find(|p| !taken.contains(p) && bindable(*p))
                    .ok_or_else(|| {
                        Error::rejected("no free port in 3110-3199 — pass `--port <n>`")
                    })?
            }
        },
    };
    if port == PRODUCTION_UI_PORT {
        return Err(Error::rejected(
            "port 3010 is the production board — pass another `--port` or omit it",
        ));
    }
    if running.is_none() && !bindable(port) {
        return Err(Error::rejected(format!(
            "port {port} is in use on 127.0.0.1 — pass another `--port` or omit it"
        )));
    }
    Ok(port)
}

// ---------- verbs ----------

/// Single-quoted for `sh`.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn env_lines(sb: &Sandbox, port: Option<u16>) -> String {
    let mut text = format!(
        "# cadence sandbox {name} — `eval \"$(cadence sandbox env {name})\"`\n\
         export CADENCE_STATE_DIR={state}\n\
         export CADENCE_PM_DIR={pm}\n\
         export CADENCE_PROFILE={profile}\n",
        name = sb.name,
        state = sh_quote(&sb.state_dir().to_string_lossy()),
        pm = sh_quote(&sb.pm_dir().to_string_lossy()),
        profile = sh_quote(&sb.profile()),
    );
    if let Some(port) = port {
        text.push_str(&format!(
            "# board: http://127.0.0.1:{port} (persisted in state/ui.json)\n"
        ));
    }
    text
}

/// `<exe> --state-dir <state> <args>` inside the sandbox env. A
/// sandbox never runs as the caller's pane identity or rollout holder.
fn child(sb: &Sandbox, exe: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(exe);
    cmd.arg("--state-dir")
        .arg(sb.state_dir())
        .args(args)
        .env("CADENCE_STATE_DIR", sb.state_dir())
        .env("CADENCE_PM_DIR", sb.pm_dir())
        .env("CADENCE_PROFILE", sb.profile())
        .env_remove("CADENCE_ALIAS")
        .env_remove("CADENCE_ROLLOUT_AS")
        .stdin(Stdio::null());
    cmd
}

/// Run one child verb to completion; its JSON stdout is the result.
fn run_child(sb: &Sandbox, exe: &Path, args: &[&str]) -> Result<Value> {
    let out = child(sb, exe, args).output()?;
    if !out.status.success() {
        return Err(Error::rejected(format!(
            "`cadence {}` in sandbox '{}' failed: {} — see {}/*.log, then \
             `cadence sandbox down {}`",
            args.join(" "),
            sb.name,
            String::from_utf8_lossy(&out.stderr).trim(),
            sb.state_dir().display(),
            sb.name
        )));
    }
    Ok(serde_json::from_slice(&out.stdout).unwrap_or(Value::Null))
}

/// `sun_path` holds 108 bytes including the NUL — a longer socket path
/// fails only inside the detached daemon, so `up` checks it first.
const SOCKET_PATH_MAX: usize = 107;

fn up(sb: &Sandbox, wanted_port: Option<u16>) -> Result<Value> {
    refuse_production(sb)?;
    let socket = client::socket_path(&sb.state_dir());
    let len = socket.as_os_str().len();
    if len > SOCKET_PATH_MAX {
        return Err(Error::rejected(format!(
            "socket path {} is {len} bytes, over the {SOCKET_PATH_MAX}-byte Unix socket \
             limit — point CADENCE_SANDBOX_ROOT at a shorter dir",
            socket.display()
        )));
    }
    let existing = read_marker(sb)?;
    if existing.is_none()
        && std::fs::read_dir(&sb.root).is_ok_and(|mut entries| entries.next().is_some())
    {
        return Err(Error::rejected(format!(
            "{} exists and holds no sandbox marker — pick another name",
            sb.root.display()
        )));
    }
    let port = choose_port(sb, wanted_port)?;
    let exe = std::env::current_exe()?;
    std::fs::create_dir_all(sb.state_dir())?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(sb.state_dir(), std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::create_dir_all(sb.pm_dir())?;
    let created_at = existing
        .as_ref()
        .and_then(|m| m["created_at"].as_str().map(str::to_string))
        .unwrap_or_else(|| crate::issue::time::iso(crate::issue::time::now_epoch()));
    let marker = json!({
        "name": sb.name,
        "created_at": created_at,
        "binary": exe,
        "profile": sb.profile(),
    });
    std::fs::write(
        sb.marker(),
        serde_json::to_string_pretty(&marker).unwrap_or_default() + "\n",
    )?;
    std::fs::write(sb.env_file(), env_lines(sb, Some(port)))?;
    run_child(sb, &exe, &["issue", "init"])?;
    let daemon = run_child(sb, &exe, &["daemon", "start"])?;
    let port_arg = port.to_string();
    let ui = run_child(sb, &exe, &["ui", "start", "--port", &port_arg])?;
    Ok(json!({
        "name": sb.name,
        "root": sb.root,
        "state_dir": sb.state_dir(),
        "pm_dir": sb.pm_dir(),
        "port": port,
        "url": format!("http://127.0.0.1:{port}"),
        "env_file": sb.env_file(),
        "profile": sb.profile(),
        "daemon": daemon["state"],
        "ui": ui["state"],
    }))
}

fn daemon_running(state_dir: &Path) -> bool {
    client::rpc_timeout(state_dir, "health", json!({}), Duration::from_secs(5)).is_ok()
}

/// Stop the board, then the daemon, through their own stop verbs.
fn down(sb: &Sandbox) -> Result<Value> {
    let exe = std::env::current_exe()?;
    let state = sb.state_dir();
    let ui = if crate::ui::detached_pid(&state).is_some() {
        run_child(sb, &exe, &["ui", "stop"])?;
        "stopped"
    } else {
        "not_running"
    };
    let daemon = if daemon_running(&state) {
        run_child(sb, &exe, &["daemon", "stop"])?;
        "stopped"
    } else {
        "not_running"
    };
    Ok(json!({"name": sb.name, "root": sb.root, "ui": ui, "daemon": daemon}))
}

/// Delete the root — only a direct child of the sandbox base (after
/// symlinks) holding this sandbox's marker, stopped first.
fn reset(sb: &Sandbox) -> Result<Value> {
    refuse_production(sb)?;
    if std::fs::symlink_metadata(&sb.root).is_err() {
        return Err(Error::rejected(format!(
            "no sandbox '{}' at {} — `cadence sandbox ls` lists them",
            sb.name,
            sb.root.display()
        )));
    }
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
    require_marker(sb)?;
    let stopped = down(sb)?;
    std::fs::remove_dir_all(&sb.root)?;
    Ok(json!({
        "name": sb.name,
        "root": sb.root,
        "state": "removed",
        "ui": stopped["ui"],
        "daemon": stopped["daemon"],
    }))
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
        let soft = global_gate(Some("s"), Some(false), "merge").unwrap_err();
        assert!(soft.to_string().contains(ALLOW_GLOBAL_ENV), "{soft}");
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
    fn env_lines_quote_paths() {
        let sb = Sandbox {
            name: "q".into(),
            base: PathBuf::from("/t/it's"),
            root: PathBuf::from("/t/it's/q"),
        };
        let text = env_lines(&sb, Some(3111));
        assert!(
            text.contains(r"export CADENCE_STATE_DIR='/t/it'\''s/q/state'"),
            "{text}"
        );
        assert!(
            text.contains("export CADENCE_PROFILE='sandbox:q'"),
            "{text}"
        );
        assert!(text.contains("127.0.0.1:3111"), "{text}");
    }
}
