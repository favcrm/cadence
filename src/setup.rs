//! `cadence setup` — the idempotent first run (CAD-312).
//!
//! Setup is a list of [`Check`]s run in order. Each check has three
//! parts: `detect` (read only), an optional `apply`, and `fix` (the
//! command an operator can copy and paste). The runner is the only
//! place that decides what happens:
//!
//! - present → `ok` ("already present"); `apply` is never called, so an
//!   existing install is never re-initialised;
//! - absent with an `apply` → run it, detect again → `created`, or
//!   `failed` when the check still does not pass;
//! - absent without an `apply` → `missing` plus the fix;
//! - broken → `failed` plus the fix; unknown → `unknown` plus the fix.
//!
//! A check whose prerequisite (`needs`) is not `ok`/`created` is not
//! applied. A new check (platforms, vault) is one more entry in
//! [`checks`], not a new branch.
//!
//! Setup reuses the verbs that already exist: the state dir resolver,
//! `Pm::init` (`issue init`), `skill::sync`, `client::daemon_start_as`
//! (`daemon start`, which reports `already_running` rather than start a
//! second daemon on a state dir) and `ui start`. Under a CAD-310
//! sandbox profile the skill is never written into `$HOME`.
//!
//! Provider CLIs are detected with their version and a sign-in signal
//! that is cheap and never a secret — see [`PROVIDERS`]. Setup never
//! reads a credential file (only whether it exists) and never prints
//! the output of a status command, only its exit code.
//!
//! | CLI | version | sign-in signal |
//! |---|---|---|
//! | `claude` | `claude --version` | `claude auth status` exit code |
//! | `codex` | `codex --version` | `codex login status` exit code |
//! | `cursor-agent` | `cursor-agent --version` | `$XDG_CONFIG_HOME/cursor/auth.json` exists (`cursor-agent status` exits 0 signed out) |
//! | `devin` | `devin --version` | `$XDG_DATA_HOME/devin/credentials.toml` exists (`devin auth status` exits 0 signed out) |
//! | `pi` | `pi --version` | `$PI_CODING_AGENT_DIR/auth.json` (default `~/.pi/agent`) exists; `pi auth` can print or refresh credentials, so it is not run |
//!
//! A status command that does not answer within 15 s is `unknown`. A
//! login held only in an API-key environment variable is not inspected
//! and reads as not signed in.
//!
//! Hooks for work in other lanes: the single-use operator login link
//! is CAD-313 ([`login_link`] returns `None` until its `cadence ui
//! login` lands), and the master agent's files and bootstrap are
//! CAD-339 (`cadence master start`) — setup only reports them.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Serialize;
use serde_json::json;

use crate::client;
use crate::error::{Error, Result};

/// `sun_path` holds 108 bytes including the NUL.
const SOCKET_PATH_MAX: usize = 107;
/// The board port `ui start` takes when nothing else is persisted.
const DEFAULT_UI_PORT: u16 = 3010;
/// Bound on each provider probe (`--version`, a status subcommand).
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// Bound on the daemon `health` probe.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);

/// What `detect` saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    Present(String),
    Absent(String),
    Unknown(String),
    /// Present but unusable — never repaired by `apply`. `fix`
    /// overrides the check's own fix when the cause needs another one.
    Broken {
        detail: String,
        fix: Option<String>,
    },
}

impl Found {
    fn broken(detail: impl Into<String>) -> Self {
        Found::Broken {
            detail: detail.into(),
            fix: None,
        }
    }
    fn detail(&self) -> &str {
        match self {
            Found::Present(d) | Found::Absent(d) | Found::Unknown(d) => d,
            Found::Broken { detail, .. } => detail,
        }
    }
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Created,
    Missing,
    Failed,
    Unknown,
}

impl Status {
    fn ready(self) -> bool {
        matches!(self, Status::Ok | Status::Created)
    }
    fn label(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Created => "created",
            Status::Missing => "missing",
            Status::Failed => "failed",
            Status::Unknown => "unknown",
        }
    }
}

/// One line of `setup --json`: `{check, status, detail, fix}`. `fix`
/// is a copy-paste command, `null` when there is nothing to do.
#[derive(Serialize, Debug, Clone)]
pub struct Outcome {
    pub check: String,
    pub status: Status,
    pub detail: String,
    pub fix: Option<String>,
}

type Detect = Box<dyn Fn(&Ctx) -> Found>;
type Apply = Box<dyn Fn(&Ctx) -> Result<String>>;
type Fix = Box<dyn Fn(&Ctx) -> String>;

/// One setup step: detect, an optional apply, and the fix command.
pub struct Check {
    pub name: String,
    needs: Vec<&'static str>,
    detect: Detect,
    apply: Option<Apply>,
    fix: Fix,
}

impl Check {
    pub fn new(
        name: impl Into<String>,
        detect: impl Fn(&Ctx) -> Found + 'static,
        fix: impl Fn(&Ctx) -> String + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            needs: Vec::new(),
            detect: Box::new(detect),
            apply: None,
            fix: Box::new(fix),
        }
    }

    pub fn apply(mut self, apply: impl Fn(&Ctx) -> Result<String> + 'static) -> Self {
        self.apply = Some(Box::new(apply));
        self
    }

    /// Checks that must be `ok` or `created` before this one applies.
    pub fn needs(mut self, names: &[&'static str]) -> Self {
        self.needs.extend_from_slice(names);
        self
    }

    fn outcome(&self, status: Status, detail: String, fix: Option<String>) -> Outcome {
        Outcome {
            check: self.name.clone(),
            status,
            detail,
            fix,
        }
    }

    /// The runner — the one place the status rules live.
    pub fn run(&self, ctx: &Ctx, done: &[Outcome]) -> Outcome {
        let fix = || Some((self.fix)(ctx));
        if let Some(dep) = self
            .needs
            .iter()
            .find(|dep| !done.iter().any(|o| o.check == **dep && o.status.ready()))
        {
            return self.outcome(Status::Missing, format!("needs `{dep}` first"), fix());
        }
        match (self.detect)(ctx) {
            Found::Present(d) if self.apply.is_some() => {
                self.outcome(Status::Ok, format!("already present — {d}"), None)
            }
            Found::Present(d) => self.outcome(Status::Ok, d, None),
            Found::Unknown(d) => self.outcome(Status::Unknown, d, fix()),
            Found::Broken { detail, fix: own } => {
                self.outcome(Status::Failed, detail, own.or_else(fix))
            }
            Found::Absent(d) => match &self.apply {
                None => self.outcome(Status::Missing, d, fix()),
                Some(apply) => match apply(ctx) {
                    Err(e) => self.outcome(Status::Failed, format!("{d}; {e}"), fix()),
                    Ok(did) => match (self.detect)(ctx) {
                        Found::Present(_) => self.outcome(Status::Created, did, None),
                        after => self.outcome(
                            Status::Failed,
                            format!("{did}, but the check still fails: {}", after.detail()),
                            fix(),
                        ),
                    },
                },
            },
        }
    }
}

/// What every check reads.
pub struct Ctx {
    pub state_dir: PathBuf,
    pub pm_dir: PathBuf,
    pub home: PathBuf,
    /// `--port`, when given.
    pub port: Option<u16>,
    /// `CADENCE_PROFILE=sandbox:<name>`.
    pub sandbox: Option<String>,
    /// Environment lookups (tests pass a map; the CLI the process env).
    pub env: fn(&str) -> Option<String>,
}

fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

impl Ctx {
    /// `cadence`, plus `--state-dir` when this state dir is not the one
    /// a bare `cadence` resolves — so every fix pastes as is.
    fn cadence(&self, verb: &str) -> String {
        if client::state_dir().ok().as_deref() == Some(self.state_dir.as_path()) {
            format!("cadence {verb}")
        } else {
            format!(
                "cadence --state-dir {} {verb}",
                sh_quote(&self.state_dir.to_string_lossy())
            )
        }
    }

    /// The board port: `--port`, else the persisted `ui.json`, else
    /// `ui start`'s default.
    pub fn board_port(&self) -> u16 {
        self.port
            .or(crate::ui::persisted_opts(&self.state_dir).port)
            .unwrap_or(DEFAULT_UI_PORT)
    }

    pub fn board_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.board_port())
    }

    /// `$<var>`, else `$HOME/<fallback>`.
    fn xdg(&self, var: &str, fallback: &str) -> PathBuf {
        (self.env)(var)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| self.home.join(fallback))
    }
}

fn sh_quote(s: &str) -> String {
    if s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"/._-+:@".contains(&b))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

// ---------- the checks ----------

/// Every setup check, in the order they run.
pub fn checks() -> Vec<Check> {
    let mut list = vec![state_dir_check(), tracker_check(), skill_check()];
    list.push(daemon_check());
    list.push(ui_check());
    list.extend(PROVIDERS.iter().map(provider_check));
    list.push(master_check());
    list.push(login_check());
    list
}

fn state_dir_check() -> Check {
    Check::new(
        "state_dir",
        |ctx| {
            let socket = client::socket_path(&ctx.state_dir);
            let len = socket.as_os_str().len();
            if len > SOCKET_PATH_MAX {
                return Found::Broken {
                    detail: format!(
                        "socket path {} is {len} bytes, over the {SOCKET_PATH_MAX}-byte \
                         Unix socket limit",
                        socket.display()
                    ),
                    fix: Some("cadence --state-dir /tmp/cadence-state setup".into()),
                };
            }
            match std::fs::metadata(&ctx.state_dir) {
                Ok(m) if m.is_dir() => Found::Present(ctx.state_dir.display().to_string()),
                Ok(_) => Found::broken(format!(
                    "{} exists and is not a directory",
                    ctx.state_dir.display()
                )),
                Err(_) => Found::Absent(format!("no {}", ctx.state_dir.display())),
            }
        },
        |ctx| {
            format!(
                "mkdir -p -m 700 {}",
                sh_quote(&ctx.state_dir.to_string_lossy())
            )
        },
    )
    .apply(|ctx| {
        std::fs::create_dir_all(&ctx.state_dir)?;
        std::fs::set_permissions(&ctx.state_dir, std::fs::Permissions::from_mode(0o700))?;
        Ok(format!("created {} (0700)", ctx.state_dir.display()))
    })
}

fn tracker_check() -> Check {
    Check::new(
        "tracker",
        |ctx| {
            let dir = &ctx.pm_dir;
            if dir.join("pm.yaml").exists() {
                return match crate::issue::Pm::at(dir) {
                    Ok(_) => Found::Present(format!("tracker at {}", dir.display())),
                    Err(e) => Found::broken(e.to_string()),
                };
            }
            match std::fs::read_dir(dir).map(|mut entries| entries.next().is_some()) {
                Ok(true) => Found::Broken {
                    detail: format!(
                        "{} exists, is not empty and holds no pm.yaml — refusing to \
                         initialise a tracker over it",
                        dir.display()
                    ),
                    fix: Some("CADENCE_PM_DIR=$HOME/cadence-pm cadence setup".into()),
                },
                Ok(false) => Found::Absent(format!("{} is empty", dir.display())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    Found::Absent(format!("no tracker at {}", dir.display()))
                }
                Err(e) => Found::broken(format!("cannot read {}: {e}", dir.display())),
            }
        },
        |_| "cadence issue init".into(),
    )
    .apply(|ctx| {
        crate::issue::Pm::init(&ctx.pm_dir)?;
        Ok(format!(
            "initialised a git tracker at {}",
            ctx.pm_dir.display()
        ))
    })
}

fn skill_check() -> Check {
    Check::new(
        "skill",
        |ctx| {
            let status = crate::skill::status(&ctx.home);
            let links_ok = status["links"]
                .as_object()
                .is_some_and(|m| m.values().all(|v| v == "ok" || v == "foreign"));
            let current = status["installed"] == true && status["content_match"] == true;
            if current && links_ok {
                return Found::Present(status["path"].as_str().unwrap_or("").to_string());
            }
            let what = if status["installed"] != true {
                "not installed"
            } else if !current {
                "installed but older than this binary"
            } else {
                "an agent skill dir lacks its `cadence` link"
            };
            if let Some(name) = &ctx.sandbox {
                return Found::Unknown(format!(
                    "{what}; the sandbox profile ({name}) never writes the skill into $HOME"
                ));
            }
            Found::Absent(what.into())
        },
        |ctx| {
            if ctx.sandbox.is_some() {
                "env -u CADENCE_PROFILE -u CADENCE_STATE_DIR -u CADENCE_PM_DIR cadence skill install"
                    .into()
            } else {
                "cadence skill install".into()
            }
        },
    )
    .apply(|ctx| {
        let report = crate::skill::sync(&ctx.home, false)?;
        Ok(format!(
            "installed {}",
            report["installed"].as_str().unwrap_or("the skill")
        ))
    })
}

fn daemon_check() -> Check {
    Check::new(
        "daemon",
        |ctx| match client::rpc_timeout(&ctx.state_dir, "health", json!({}), HEALTH_TIMEOUT) {
            Ok(health) => Found::Present(format!(
                "running (pid {}) on {}",
                health["pid"],
                client::socket_path(&ctx.state_dir).display()
            )),
            Err(_) => Found::Absent("not running".into()),
        },
        |ctx| ctx.cadence("daemon start"),
    )
    .needs(&["state_dir"])
    // `daemon start` answers `already_running` when any daemon owns
    // this state dir — setup never starts a second one.
    .apply(|ctx| {
        let result = client::daemon_start_as(&ctx.state_dir, None)?;
        Ok(format!(
            "{} on {}",
            result["state"].as_str().unwrap_or("started"),
            client::socket_path(&ctx.state_dir).display()
        ))
    })
}

fn ui_check() -> Check {
    Check::new(
        "ui",
        |ctx| {
            let url = ctx.board_url();
            match crate::ui::detached_pid(&ctx.state_dir) {
                Some(pid) => match crate::ui::health(&ctx.state_dir) {
                    Some((200, _)) => Found::Present(format!("board at {url} (pid {pid})")),
                    _ => Found::Broken {
                        detail: format!("board pid {pid} is alive but {url} does not answer"),
                        fix: Some(format!(
                            "{} && {}",
                            ctx.cadence("ui stop"),
                            ctx.cadence("ui start")
                        )),
                    },
                },
                None if std::net::TcpListener::bind(("127.0.0.1", ctx.board_port())).is_err() => {
                    Found::Broken {
                        detail: format!(
                            "port {} is taken by another process — not this state dir's board",
                            ctx.board_port()
                        ),
                        fix: Some(ctx.cadence("setup --port 3110")),
                    }
                }
                None => Found::Absent("not running".into()),
            }
        },
        |ctx| ctx.cadence("ui start"),
    )
    .needs(&["state_dir", "tracker"])
    .apply(|ctx| {
        let flags = crate::ui::UiFlags {
            port: ctx.port,
            ..Default::default()
        };
        crate::ui::start_quiet(&ctx.state_dir, &flags, false)?;
        Ok(format!("board at {}", ctx.board_url()))
    })
}

/// The cheap, non-secret signal a provider's sign-in is read from.
#[derive(Debug, Clone, Copy)]
pub enum SignIn {
    /// Exit code of a status subcommand: 0 signed in, else signed out.
    /// Its output is discarded unread.
    StatusExit(&'static [&'static str]),
    /// Whether the CLI's own credentials file exists — never opened.
    /// `(env var for the dir, default dir under $HOME, file name)`.
    FilePresent(&'static str, &'static str, &'static str),
}

/// One provider CLI setup looks for.
#[derive(Debug)]
pub struct Provider {
    pub bin: &'static str,
    pub install: &'static str,
    pub login: &'static str,
    pub signin: SignIn,
}

/// Provider CLIs and the sign-in signal each one uses. `cursor-agent
/// status` and `devin auth status` exit 0 while signed out, so those
/// two use their credentials file's presence instead.
pub const PROVIDERS: &[Provider] = &[
    Provider {
        bin: "claude",
        install: "curl -fsSL https://claude.ai/install.sh | bash",
        login: "claude auth login",
        signin: SignIn::StatusExit(&["auth", "status"]),
    },
    Provider {
        bin: "codex",
        install: "npm install -g @openai/codex",
        login: "codex login",
        signin: SignIn::StatusExit(&["login", "status"]),
    },
    Provider {
        bin: "cursor-agent",
        install: "curl https://cursor.com/install -fsS | bash",
        login: "cursor-agent login",
        signin: SignIn::FilePresent("XDG_CONFIG_HOME", ".config", "cursor/auth.json"),
    },
    Provider {
        bin: "devin",
        install: "curl -fsSL https://cli.devin.ai/install.sh | bash",
        login: "devin auth login",
        signin: SignIn::FilePresent("XDG_DATA_HOME", ".local/share", "devin/credentials.toml"),
    },
    Provider {
        bin: "pi",
        install: "npm install -g @mariozechner/pi-coding-agent",
        login: "pi  # then type /login",
        signin: SignIn::FilePresent("PI_CODING_AGENT_DIR", ".pi/agent", "auth.json"),
    },
];

/// `bin` on `$PATH`, as an executable file.
fn which(ctx: &Ctx, bin: &str) -> Option<PathBuf> {
    let path = (ctx.env)("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(bin))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// First line of `<bin> --version`, at most 80 characters.
fn version(path: &Path) -> Option<String> {
    let out = crate::proc::run_bounded(Command::new(path).arg("--version"), PROBE_TIMEOUT).ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.chars().take(80).collect())
}

/// `Some(true)` signed in, `Some(false)` signed out, `None` unknown —
/// plus a description of the signal read.
fn signed_in(ctx: &Ctx, path: &Path, provider: &Provider) -> (Option<bool>, String) {
    match provider.signin {
        SignIn::StatusExit(args) => {
            let signal = format!("signal: `{} {}` exit code", provider.bin, args.join(" "));
            match crate::proc::run_bounded(Command::new(path).args(args), PROBE_TIMEOUT) {
                Ok(out) => (Some(out.status.success()), signal),
                Err(_) => (None, format!("{signal} (did not answer)")),
            }
        }
        SignIn::FilePresent(var, fallback, file) => {
            let file = ctx.xdg(var, fallback).join(file);
            let signal = format!("signal: whether {} exists", file.display());
            match std::fs::symlink_metadata(&file) {
                Ok(_) => (Some(true), signal),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Some(false), signal),
                Err(_) => (None, signal),
            }
        }
    }
}

fn provider_check(provider: &'static Provider) -> Check {
    Check::new(
        provider.bin,
        move |ctx| {
            let Some(path) = which(ctx, provider.bin) else {
                return Found::Absent("not on PATH".into());
            };
            let version = version(&path).unwrap_or_else(|| "version unknown".into());
            match signed_in(ctx, &path, provider) {
                (Some(true), signal) => Found::Present(format!("{version}; signed in ({signal})")),
                (Some(false), signal) => {
                    Found::Absent(format!("{version}; not signed in ({signal})"))
                }
                (None, signal) => Found::Unknown(format!("{version}; sign-in unknown ({signal})")),
            }
        },
        move |ctx| {
            if which(ctx, provider.bin).is_some() {
                provider.login.into()
            } else {
                provider.install.into()
            }
        },
    )
}

/// CAD-339 owns the master agent's files and bootstrap; setup only
/// reports whether they are there.
fn master_check() -> Check {
    Check::new(
        "master",
        |ctx| {
            let dir = ctx.pm_dir.join("agents").join("master");
            let missing: Vec<&str> = ["SOUL.md", "AGENT.md"]
                .into_iter()
                .filter(|f| !dir.join(f).is_file())
                .collect();
            if missing.is_empty() {
                Found::Present(format!("agent files in {}", dir.display()))
            } else {
                Found::Absent(format!(
                    "no {} in {} — the master agent is set up by `cadence master start` (CAD-339)",
                    missing.join(", "),
                    dir.display()
                ))
            }
        },
        |ctx| ctx.cadence("master start"),
    )
    .needs(&["tracker"])
}

/// CAD-313 hook: the single-use operator login link for the board.
/// `None` until CAD-313's link-minting verb (`cadence ui login`) is on
/// main — then this calls it and the `login` check reports the link.
pub fn login_link(_ctx: &Ctx) -> Option<String> {
    None
}

fn login_check() -> Check {
    Check::new(
        "login",
        |ctx| match login_link(ctx) {
            Some(link) => Found::Present(link),
            None => Found::Unknown(format!(
                "the single-use operator login link is CAD-313 and not in this build; \
                 the board is at {}",
                ctx.board_url()
            )),
        },
        |ctx| ctx.cadence("ui status"),
    )
    .needs(&["ui"])
}

// ---------- the CLI ----------

/// Run `checks` in order, handing each outcome to `emit` as it lands.
pub fn run_checks(ctx: &Ctx, checks: &[Check], mut emit: impl FnMut(&Outcome)) -> Vec<Outcome> {
    let mut done = Vec::with_capacity(checks.len());
    for check in checks {
        let outcome = check.run(ctx, &done);
        emit(&outcome);
        done.push(outcome);
    }
    done
}

/// `cadence setup [--json] [--port <n>]`. Exit 1 when any check
/// failed; `missing` and `unknown` are facts, not failures.
pub fn cli(state_dir: &Path, port: Option<u16>, json: bool) -> Result<i32> {
    let home = process_env("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| Error::rejected("HOME is not set to an absolute path"))?;
    let ctx = Ctx {
        state_dir: state_dir.to_path_buf(),
        pm_dir: crate::issue::default_dir()?,
        home,
        port,
        sandbox: crate::sandbox::profile(),
        env: process_env,
    };
    let outcomes = run_checks(&ctx, &checks(), |o| {
        if json {
            println!("{}", serde_json::to_string(o).unwrap_or_default());
        } else {
            println!("{:<8} {:<13} {}", o.status.label(), o.check, o.detail);
            if let Some(fix) = &o.fix {
                println!("{:<22} fix: {fix}", "");
            }
        }
    });
    if !json {
        println!("\nboard: {}", ctx.board_url());
    }
    let failed = outcomes.iter().any(|o| o.status == Status::Failed);
    Ok(i32::from(failed))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn ctx(dir: &Path) -> Ctx {
        Ctx {
            state_dir: dir.join("state"),
            pm_dir: dir.join("pm"),
            home: dir.join("home"),
            port: Some(3111),
            sandbox: None,
            env: no_env,
        }
    }

    /// A check over a flag: present once `apply` flipped it.
    fn flag_check(name: &'static str, applied: Rc<Cell<u32>>, works: bool) -> Check {
        let seen = applied.clone();
        Check::new(
            name,
            move |_| {
                if seen.get() > 0 && works {
                    Found::Present("here".into())
                } else {
                    Found::Absent("gone".into())
                }
            },
            move |_| format!("make {name}"),
        )
        .apply(move |_| {
            applied.set(applied.get() + 1);
            Ok("made it".into())
        })
    }

    #[test]
    fn absent_is_applied_once_and_present_is_never_reapplied() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx(dir.path());
        let applied = Rc::new(Cell::new(0));
        let checks = [flag_check("a", applied.clone(), true)];
        let first = run_checks(&ctx, &checks, |_| {});
        assert_eq!(first[0].status, Status::Created);
        assert_eq!(first[0].fix, None);
        let second = run_checks(&ctx, &checks, |_| {});
        assert_eq!(second[0].status, Status::Ok);
        assert!(
            second[0].detail.starts_with("already present"),
            "{second:?}"
        );
        assert_eq!(applied.get(), 1, "apply ran on a present check");
    }

    #[test]
    fn an_apply_that_does_not_take_fails_with_the_fix() {
        let dir = tempfile::tempdir().unwrap();
        let out = run_checks(
            &ctx(dir.path()),
            &[flag_check("a", Rc::new(Cell::new(0)), false)],
            |_| {},
        );
        assert_eq!(out[0].status, Status::Failed);
        assert_eq!(out[0].fix.as_deref(), Some("make a"));
    }

    #[test]
    fn an_unready_prerequisite_blocks_apply() {
        let dir = tempfile::tempdir().unwrap();
        let applied = Rc::new(Cell::new(0));
        let broken = Check::new("base", |_| Found::broken("bad"), |_| "fix base".into());
        let dependent = flag_check("top", applied.clone(), true).needs(&["base"]);
        let out = run_checks(&ctx(dir.path()), &[broken, dependent], |_| {});
        assert_eq!(out[0].status, Status::Failed);
        assert_eq!(out[1].status, Status::Missing);
        assert!(out[1].detail.contains("needs `base`"), "{:?}", out[1]);
        assert_eq!(applied.get(), 0);
    }

    #[test]
    fn detect_only_checks_report_missing_and_unknown_with_a_fix() {
        let dir = tempfile::tempdir().unwrap();
        let checks = [
            Check::new("m", |_| Found::Absent("no".into()), |_| "get m".into()),
            Check::new("u", |_| Found::Unknown("?".into()), |_| "ask u".into()),
        ];
        let out = run_checks(&ctx(dir.path()), &checks, |_| {});
        assert_eq!(out[0].status, Status::Missing);
        assert_eq!(out[0].fix.as_deref(), Some("get m"));
        assert_eq!(out[1].status, Status::Unknown);
        assert_eq!(out[1].fix.as_deref(), Some("ask u"));
    }

    #[test]
    fn outcome_serialises_as_the_four_documented_fields() {
        let o = Outcome {
            check: "x".into(),
            status: Status::Created,
            detail: "d".into(),
            fix: None,
        };
        let v = serde_json::to_value(&o).unwrap();
        assert_eq!(
            v,
            json!({"check": "x", "status": "created", "detail": "d", "fix": null})
        );
    }

    #[test]
    fn tracker_refuses_a_non_empty_dir_that_is_not_a_tracker() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx(dir.path());
        std::fs::create_dir_all(&ctx.pm_dir).unwrap();
        std::fs::write(ctx.pm_dir.join("notes.txt"), "mine").unwrap();
        let out = run_checks(&ctx, &[tracker_check()], |_| {});
        assert_eq!(out[0].status, Status::Failed);
        assert!(out[0].detail.contains("refusing"), "{:?}", out[0]);
        assert!(!ctx.pm_dir.join("pm.yaml").exists());
        assert!(!ctx.pm_dir.join(".git").exists());
    }

    #[test]
    fn a_state_dir_too_long_for_a_socket_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = ctx(dir.path());
        ctx.state_dir = dir.path().join("s".repeat(120));
        let out = run_checks(&ctx, &[state_dir_check()], |_| {});
        assert_eq!(out[0].status, Status::Failed);
        assert!(!ctx.state_dir.exists());
    }

    #[test]
    fn a_sandbox_never_writes_the_skill_into_home() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = ctx(dir.path());
        ctx.sandbox = Some("s".into());
        let out = run_checks(&ctx, &[skill_check()], |_| {});
        assert_eq!(out[0].status, Status::Unknown);
        assert!(!ctx.home.join(".agents").exists());
    }

    #[test]
    fn master_is_reported_never_created() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx(dir.path());
        let tracker = Check::new("tracker", |_| Found::Present("t".into()), |_| String::new());
        let out = run_checks(&ctx, &[tracker, master_check()], |_| {});
        assert_eq!(out[1].status, Status::Missing);
        assert!(out[1].fix.as_deref().unwrap().ends_with("master start"));
        assert!(!ctx.pm_dir.join("agents").exists());
    }

    #[test]
    fn a_file_signal_is_read_by_presence_only() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx(dir.path());
        let devin = PROVIDERS.iter().find(|p| p.bin == "devin").unwrap();
        let (state, _) = signed_in(&ctx, Path::new("/bin/false"), devin);
        assert_eq!(state, Some(false));
        let creds = ctx.home.join(".local/share/devin/credentials.toml");
        std::fs::create_dir_all(creds.parent().unwrap()).unwrap();
        // Unreadable: presence is the whole signal, the file is never opened.
        std::fs::write(&creds, "token = 'x'").unwrap();
        std::fs::set_permissions(&creds, std::fs::Permissions::from_mode(0o000)).unwrap();
        let (state, signal) = signed_in(&ctx, Path::new("/bin/false"), devin);
        assert_eq!(state, Some(true));
        assert!(!signal.contains("token"), "{signal}");
    }
}
