//! `cadence setup` — the idempotent first run (CAD-312).
//!
//! Setup is a list of [`Check`]s run in order. Each check has three
//! parts: `detect` (read only), an optional `apply`, and `fix` (the
//! command an operator can copy and paste). The runner is the only
//! place that decides what happens:
//!
//! - present → `ok` ("already present"); `apply` is never called, so an
//!   existing install is never re-initialised;
//! - absent with an `apply` → run it, detect again → `created` (or
//!   `started` when its state already existed — a stopped daemon or
//!   board), or `failed` when the check still does not pass;
//! - absent without an `apply`, or absent but the operator's to create
//!   (`Found::Manual`) → `missing` plus the fix;
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
//! sandbox profile the skill is never written into `$HOME`; an
//! installed skill that differs from this binary is reported, never
//! rewritten; a tailnet-shared board is never restarted (that would
//! re-run `tailscale serve`).
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
//! A probe that does not answer within 10 s is `unknown`. A
//! login held only in an API-key environment variable is not inspected
//! and reads as not signed in.
//!
//! The board's `/setup` page (CAD-327) runs the same list through
//! [`board_detect`]: detect only — [`detect_checks`] never calls an
//! `apply` — with the `ui` check answered by the serving board and each
//! provider probe bounded by [`BOARD_PROBE_TIMEOUT`]. Its master step
//! (CAD-448) also gets [`MasterOffer`]s — the providers `master start`
//! accepts ([`crate::master::PROVIDERS`]), each ready one with its exact
//! `master start --provider <bin>` command — and the `master_login`
//! check reports the master's own Claude login (CAD-439's separate
//! `CLAUDE_CONFIG_DIR`), never the operator's.
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
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
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
    /// Absent, but only the operator may create it — `missing`, never
    /// applied. `fix` is exactly this one (`None`: no command exists).
    Manual {
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
            Found::Broken { detail, .. } | Found::Manual { detail, .. } => detail,
        }
    }
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Created,
    /// Applied over state that already existed — a stopped daemon or
    /// board started again, not a new install.
    Started,
    Missing,
    Failed,
    Unknown,
}

impl Status {
    fn ready(self) -> bool {
        matches!(self, Status::Ok | Status::Created | Status::Started)
    }
    fn label(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Created => "created",
            Status::Started => "started",
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
type Apply = Box<dyn Fn(&Ctx) -> Result<Applied>>;

/// What an `apply` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// Made something that did not exist.
    Created(String),
    /// Started something whose state already existed.
    Started(String),
}
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

    pub fn apply(mut self, apply: impl Fn(&Ctx) -> Result<Applied> + 'static) -> Self {
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
        self.run_as(ctx, done, Mode::Apply)
    }

    fn run_as(&self, ctx: &Ctx, done: &[Outcome], mode: Mode) -> Outcome {
        // An empty fix is "no command exists in this build".
        let fix = || Some((self.fix)(ctx)).filter(|f| !f.is_empty());
        // Detect only: an absent check is reported with its fix, exactly
        // as a check without an `apply` is.
        let apply = self.apply.as_ref().filter(|_| mode == Mode::Apply);
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
            Found::Manual { detail, fix: own } => self.outcome(Status::Missing, detail, own),
            Found::Absent(d) => match apply {
                None => self.outcome(Status::Missing, d, fix()),
                Some(apply) => match apply(ctx) {
                    Err(e) => self.outcome(Status::Failed, format!("{d}; {e}"), fix()),
                    Ok(applied) => {
                        let (status, did) = match applied {
                            Applied::Created(d) => (Status::Created, d),
                            Applied::Started(d) => (Status::Started, d),
                        };
                        match (self.detect)(ctx) {
                            Found::Present(_) => self.outcome(status, did, None),
                            after => self.outcome(
                                Status::Failed,
                                format!("{did}, but the check still fails: {}", after.detail()),
                                fix(),
                            ),
                        }
                    }
                },
            },
        }
    }
}

/// Whether the runner may call a check's `apply`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Apply,
    DetectOnly,
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
    /// Bound on each provider probe.
    pub probe_timeout: Duration,
    /// This binary's top-level verbs — a fix names only one that exists.
    pub verbs: Vec<String>,
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

    /// The board port: the running board's own (`ui.json`); with none
    /// running, `--port`, else the persisted port, else `ui start`'s
    /// default.
    pub fn board_port(&self) -> u16 {
        let persisted = crate::ui::persisted_opts(&self.state_dir).port;
        if crate::ui::detached_pid(&self.state_dir).is_some() {
            return persisted.unwrap_or(DEFAULT_UI_PORT);
        }
        self.port.or(persisted).unwrap_or(DEFAULT_UI_PORT)
    }

    fn has_verb(&self, verb: &str) -> bool {
        self.verbs.iter().any(|v| v == verb)
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
    list.push(master_login_check());
    list.push(login_check());
    // A provider `master start` accepts must be a check here — the
    // wizard's offer (CAD-448) reads that outcome, never re-probes.
    debug_assert!(crate::master::PROVIDERS
        .iter()
        .all(|bin| list.iter().any(|c| c.name == *bin)));
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
        Ok(Applied::Created(format!(
            "created {} (0700)",
            ctx.state_dir.display()
        )))
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
        Ok(Applied::Created(format!(
            "initialised a git tracker at {}",
            ctx.pm_dir.display()
        )))
    })
}

fn skill_check() -> Check {
    Check::new(
        "skill",
        |ctx| {
            let status = crate::skill::status(&ctx.home);
            let path = status["path"].as_str().unwrap_or("").to_string();
            if status["installed"] != true {
                return match &ctx.sandbox {
                    Some(name) => Found::Unknown(format!(
                        "not installed; the sandbox profile ({name}) never writes the \
                         skill into $HOME"
                    )),
                    None => Found::Absent("not installed".into()),
                };
            }
            // Installed: setup never rewrites it. Another binary's copy —
            // older or newer — and a link the operator removed stay as
            // they are; `skill install` is the explicit refresh.
            let mut differs = Vec::new();
            if status["content_match"] != true {
                differs.push("SKILL.md content".to_string());
            }
            for (dir, state) in status["links"].as_object().into_iter().flatten() {
                if state != "ok" && state != "foreign" {
                    differs.push(format!("{dir} link {}", state.as_str().unwrap_or("?")));
                }
            }
            if differs.is_empty() {
                Found::Present(path)
            } else {
                Found::Unknown(format!(
                    "{path} differs from this binary ({}) — left as is",
                    differs.join(", ")
                ))
            }
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
        Ok(Applied::Created(format!(
            "installed {}",
            report["installed"].as_str().unwrap_or("the skill")
        )))
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
        let existed = crate::rollout::db_file(&ctx.state_dir).exists();
        let result = client::daemon_start_as(&ctx.state_dir, None)?;
        let state = result["state"].as_str().unwrap_or("started");
        let detail = format!(
            "{state} on {}",
            client::socket_path(&ctx.state_dir).display()
        );
        Ok(if existed || state != "started" {
            Applied::Started(detail)
        } else {
            Applied::Created(detail)
        })
    })
}

fn ui_check() -> Check {
    Check::new(
        "ui",
        |ctx| {
            let url = ctx.board_url();
            match crate::ui::detached_pid(&ctx.state_dir) {
                Some(pid) => match crate::ui::health(&ctx.state_dir) {
                    Some((200, _)) => {
                        let running = ctx.board_port();
                        let moved = match ctx.port {
                            Some(p) if p != running => format!(
                                "; --port {p} differs from the running board — `{}` then \
                                 `{}` moves it",
                                ctx.cadence("ui stop"),
                                ctx.cadence(&format!("ui start --port {p}"))
                            ),
                            _ => String::new(),
                        };
                        Found::Present(format!("board at {url} (pid {pid}){moved}"))
                    }
                    _ => Found::Broken {
                        detail: format!("board pid {pid} is alive but {url} does not answer"),
                        fix: Some(format!(
                            "{} && {}",
                            ctx.cadence("ui stop"),
                            ctx.cadence("ui start")
                        )),
                    },
                },
                // `tailscale serve` is the operator's to re-run: setup
                // never touches the tailnet.
                None if crate::ui::persisted_opts(&ctx.state_dir)
                    .tailscale
                    .is_some() =>
                {
                    Found::Manual {
                        detail: "not running, and ui.json shares it on the tailnet — setup \
                                 does not re-run `tailscale serve`"
                            .into(),
                        fix: Some(ctx.cadence("ui start")),
                    }
                }
                None if !bindable(ctx.board_port()) => Found::Broken {
                    detail: format!(
                        "port {} is taken by another process — not this state dir's board",
                        ctx.board_port()
                    ),
                    fix: Some(ctx.cadence(&format!(
                        "setup --port {}",
                        FREE_PORTS.clone().find(|p| bindable(*p)).unwrap_or(3110)
                    ))),
                },
                None => Found::Absent("not running".into()),
            }
        },
        |ctx| ctx.cadence("ui start"),
    )
    .needs(&["state_dir", "tracker", "daemon"])
    .apply(|ctx| {
        let existed = crate::ui::opts_present(&ctx.state_dir);
        let flags = crate::ui::UiFlags {
            port: ctx.port,
            ..Default::default()
        };
        crate::ui::start_quiet(&ctx.state_dir, &flags, false)?;
        let detail = format!("board at {}", ctx.board_url());
        Ok(if existed {
            Applied::Started(detail)
        } else {
            Applied::Created(detail)
        })
    })
}

/// Where a fix looks for a free board port — never production's 3010.
const FREE_PORTS: std::ops::RangeInclusive<u16> = 3110..=3199;

fn bindable(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
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

/// Output kept from one probe — enough for a version line.
const PROBE_OUTPUT_CAP: usize = 4096;

/// Run a probe bounded in time and output. `run_bounded_limited` kills
/// the whole process group when a reader misses the deadline, so a CLI
/// that leaves a background child holding its stdout cannot stall
/// setup past `timeout`.
fn probe(path: &Path, args: &[&str], timeout: Duration) -> Option<std::process::Output> {
    crate::proc::run_bounded_limited(Command::new(path).args(args), timeout, PROBE_OUTPUT_CAP)
        .ok()
        .map(|(out, _)| out)
}

/// The version token from `<bin> --version`: the first word of its
/// first line shaped like `[0-9][0-9A-Za-z.+-]*` with a dot, at most 40
/// characters. Nothing else is printed, so a CLI that answers with
/// something else — a token, a path — leaks nothing.
fn version(path: &Path, timeout: Duration) -> Option<String> {
    let out = probe(path, &["--version"], timeout)?;
    if !out.status.success() {
        return None;
    }
    version_token(&String::from_utf8_lossy(&out.stdout))
}

fn version_token(text: &str) -> Option<String> {
    text.lines()
        .next()?
        .split_whitespace()
        .find(|w| {
            w.len() <= 40
                && w.contains('.')
                && w.starts_with(|c: char| c.is_ascii_digit())
                && w.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-'))
        })
        .map(str::to_string)
}

/// `Some(true)` signed in, `Some(false)` signed out, `None` unknown —
/// plus a description of the signal read.
fn signed_in(ctx: &Ctx, path: &Path, provider: &Provider) -> (Option<bool>, String) {
    match provider.signin {
        SignIn::StatusExit(args) => {
            let signal = format!("signal: `{} {}` exit code", provider.bin, args.join(" "));
            match probe(path, args, ctx.probe_timeout) {
                Some(out) => (Some(out.status.success()), signal),
                None => (None, format!("{signal} (did not answer)")),
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
            let version = version(&path, ctx.probe_timeout)
                .map(|v| format!("{} {v}", provider.bin))
                .unwrap_or_else(|| "version unknown".into());
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
                return Found::Present(format!("agent files in {}", dir.display()));
            }
            let missing = format!("no {} in {}", missing.join(", "), dir.display());
            // Name the verb only when this binary has it.
            if ctx.has_verb("master") {
                Found::Manual {
                    detail: format!("{missing} — `master start` installs them (CAD-339)"),
                    fix: Some(ctx.cadence("master start")),
                }
            } else {
                Found::Manual {
                    detail: format!(
                        "{missing} — the master agent arrives with CAD-339, not in this build"
                    ),
                    fix: None,
                }
            }
        },
        // Without the verb there is nothing to paste — also while the
        // tracker it needs is still missing.
        |ctx| {
            if ctx.has_verb("master") {
                ctx.cadence("master start")
            } else {
                String::new()
            }
        },
    )
    .needs(&["tracker"])
}

/// [`crate::master::confinement_available`] over this context's env —
/// the daemon's `ProviderEnv` seam (`CADENCE_TEST_NO_LANDLOCK`, debug
/// builds) reaches the wizard this way, so tests can force the
/// unconfined branch on any host.
fn confinement(ctx: &Ctx) -> Result<()> {
    let env = crate::adapter::ProviderEnv::default();
    if let Some(v) = (ctx.env)(crate::master::TEST_NO_LANDLOCK) {
        env.set(crate::master::TEST_NO_LANDLOCK, v);
    }
    crate::master::confinement_available(&env)
}

/// CAD-448: the master's own Claude login (CAD-439) — a separate
/// `CLAUDE_CONFIG_DIR` under the state dir, never the operator's
/// `~/.claude`. Detect only: the login command is interactive, so an
/// absent login is `missing` with the command to run, never applied.
/// A host that cannot confine the master runs it `--unconfined` on the
/// operator's own login — there is no separate login to ask for, and
/// the detail states the risk that choice carries.
fn master_login_check() -> Check {
    Check::new(
        "master_login",
        |ctx| {
            if !ctx.has_verb("master") {
                return Found::Unknown("the master is not in this build".into());
            }
            if confinement(ctx).is_err() {
                return Found::Present(
                    "this host cannot confine the master — `master start --unconfined` \
                     runs it on your own Claude login, with no filesystem sandbox: it can \
                     read and write your files"
                        .into(),
                );
            }
            let dir = crate::master::claude_config_dir(&ctx.state_dir);
            if crate::master::has_login(&ctx.state_dir) {
                Found::Present(format!("own login in {}", dir.display()))
            } else {
                Found::Absent(format!(
                    "no login in {} — the master's Claude cannot authenticate without \
                     its own (or `master start --copy-login`)",
                    dir.display()
                ))
            }
        },
        |ctx| {
            if ctx.has_verb("master") {
                crate::master::login_command(&ctx.state_dir)
            } else {
                String::new()
            }
        },
    )
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
    if cfg!(debug_assertions) {
        validate(checks);
    }
    let mut done = Vec::with_capacity(checks.len());
    for check in checks {
        let outcome = check.run(ctx, &done);
        emit(&outcome);
        done.push(outcome);
    }
    done
}

/// Run `checks` detect only: no `apply` is ever called, so nothing is
/// created, started or written — an absent check is `missing` with its
/// fix. The `needs` rule still holds (a check whose prerequisite is not
/// ready reports that instead of probing).
pub fn detect_checks(ctx: &Ctx, checks: &[Check]) -> Vec<Outcome> {
    if cfg!(debug_assertions) {
        validate(checks);
    }
    let mut done: Vec<Outcome> = Vec::with_capacity(checks.len());
    for check in checks {
        let outcome = check.run_as(ctx, &done, Mode::DetectOnly);
        done.push(outcome);
    }
    done
}

// ---------- the board's detect-only entry point (CAD-327) ----------

/// Bound on each provider probe when the board runs the checks: a
/// browser waits on the answer, so it is half the CLI's.
pub const BOARD_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// This binary's top-level verbs, registered by `main` before a board
/// serves — the library cannot see the CLI, and a fix may name only a
/// verb that exists (`master start` arrives with CAD-339).
static VERBS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

pub fn register_verbs(verbs: Vec<String>) {
    let _ = VERBS.set(verbs);
}

/// Which wizard step a check belongs to — `provider` for each CLI in
/// [`PROVIDERS`], `master` for the master agent (files and its own
/// login), else `environment`.
pub fn check_group(name: &str) -> &'static str {
    if PROVIDERS.iter().any(|p| p.bin == name) {
        "provider"
    } else if matches!(name, "master" | "master_login") {
        "master"
    } else {
        "environment"
    }
}

/// One provider `master start` can run the master on (CAD-448): the
/// wizard's master step offers each of [`crate::master::PROVIDERS`]
/// with its sign-in state and — when installed and signed in — the
/// exact start command, `--unconfined` included where this host cannot
/// confine the master. Detect only: the command is the operator's to
/// paste, never run from the board.
#[derive(Serialize, Debug, Clone)]
pub struct MasterOffer {
    /// The provider check's name — `claude` today.
    pub bin: &'static str,
    /// Its check is ready: the CLI is installed and signed in.
    pub ready: bool,
    /// `cadence … master start --provider <bin>`; `None` while the
    /// provider is not ready, this build has no `master` verb, or the
    /// `master` check's own prerequisites are unmet — the wizard shows
    /// one command for starting the master, not two.
    pub start: Option<String>,
    /// [`crate::master::UNCONFINED_WARNING`] when the offered command
    /// runs the master without a filesystem sandbox — the operator's
    /// `--unconfined` decision is opt-in with the risk spelled out.
    pub warning: Option<&'static str>,
}

/// What `/api/setup` answers: the detect-only checks, and the master
/// step's provider offers — one per provider `master start` accepts,
/// read from the provider checks that already ran (never re-probed).
pub struct BoardDetect {
    pub checks: Vec<Outcome>,
    pub master_providers: Vec<MasterOffer>,
}

/// The master step's provider offers (CAD-448). `done` holds the
/// checks' outcomes; a ready provider earns its exact start command,
/// but only once the `master` check's own prerequisites (`tracker`)
/// are met — before that the step's one command is the check's fix.
fn master_offers(ctx: &Ctx, list: &[Check], done: &[Outcome]) -> Vec<MasterOffer> {
    let unconfined = confinement(ctx).is_err();
    let prereqs_met = list.iter().find(|c| c.name == "master").is_some_and(|m| {
        m.needs
            .iter()
            .all(|dep| done.iter().any(|o| o.check == *dep && o.status.ready()))
    });
    crate::master::PROVIDERS
        .iter()
        .map(|bin| {
            let ready = done
                .iter()
                .find(|o| o.check == *bin)
                .is_some_and(|o| o.status.ready());
            let start = (ready && prereqs_met && ctx.has_verb("master")).then(|| {
                if unconfined {
                    ctx.cadence(&format!("master start --unconfined --provider {bin}"))
                } else {
                    ctx.cadence(&format!("master start --provider {bin}"))
                }
            });
            let warning =
                (start.is_some() && unconfined).then_some(crate::master::UNCONFINED_WARNING);
            MasterOffer {
                bin,
                ready,
                start,
                warning,
            }
        })
        .collect()
}

/// The board itself: it is answering this request, so it is running.
/// Replaces the `ui` check, whose detect would probe the board over
/// HTTP and try to bind its port.
fn board_self_check(url: String) -> Check {
    Check::new(
        "ui",
        move |_| Found::Present(format!("board at {url} (serving this page)")),
        |ctx| ctx.cadence("ui status"),
    )
}

/// The checks the board runs: setup's list, the `ui` check answered by
/// the serving board.
pub fn board_checks(board_url: &str) -> Vec<Check> {
    checks()
        .into_iter()
        .map(|c| {
            if c.name == "ui" {
                board_self_check(board_url.to_string())
            } else {
                c
            }
        })
        .collect()
}

/// `GET /api/setup` — every setup check, detect only, for the board at
/// `port`, plus the master step's provider offers (CAD-448). Reads the
/// process environment as `cadence setup` does; never applies, starts
/// or writes anything; provider probes are bounded by
/// [`BOARD_PROBE_TIMEOUT`] and report only a version token and an exit
/// code or a file's presence (the rules in [`PROVIDERS`]).
pub fn board_detect(state_dir: &Path, pm_dir: &Path, port: u16) -> Result<BoardDetect> {
    let home = process_env("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| Error::rejected("HOME is not set to an absolute path"))?;
    let ctx = Ctx {
        state_dir: state_dir.to_path_buf(),
        pm_dir: pm_dir.to_path_buf(),
        home,
        port: Some(port),
        sandbox: crate::sandbox::profile(),
        env: process_env,
        probe_timeout: BOARD_PROBE_TIMEOUT,
        verbs: VERBS.get().cloned().unwrap_or_default(),
    };
    let list = board_checks(&ctx.board_url());
    let mut checks = detect_checks(&ctx, &list);
    let master_providers = master_offers(&ctx, &list, &checks);
    // CAD-448 review (N4): one command for starting the master. Once an
    // offer carries the provider-qualified command, the `master`
    // check's bare `master start` fix is the same action — the offer
    // absorbs it.
    if master_providers.iter().any(|o| o.start.is_some()) {
        if let Some(m) = checks.iter_mut().find(|o| o.check == "master") {
            m.fix = None;
        }
    }
    Ok(BoardDetect {
        checks,
        master_providers,
    })
}

/// Every `needs` names a check that runs earlier — a dependency that is
/// unknown or ordered later could never be ready, which is a bug in
/// the list, not a state of the host.
pub fn validate(checks: &[Check]) {
    for (i, check) in checks.iter().enumerate() {
        for dep in &check.needs {
            assert!(
                checks[..i].iter().any(|c| c.name == *dep),
                "setup check `{}` needs `{dep}`, which is not an earlier check",
                check.name
            );
        }
    }
}

/// `cadence setup [--json] [--port <n>]`. Exit 1 when any check
/// failed; `missing` and `unknown` are facts, not failures.
pub fn cli(state_dir: &Path, port: Option<u16>, json: bool, verbs: Vec<String>) -> Result<i32> {
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
        probe_timeout: PROBE_TIMEOUT,
        verbs,
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
            probe_timeout: Duration::from_secs(1),
            verbs: Vec::new(),
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
            Ok(Applied::Created("made it".into()))
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
        let checks = [tracker, master_check()];
        // Without the verb in this binary: no fix to paste.
        let out = run_checks(&ctx, &checks, |_| {});
        assert_eq!(out[1].status, Status::Missing);
        assert_eq!(out[1].fix, None);
        assert!(
            out[1].detail.contains("arrives with CAD-339"),
            "{:?}",
            out[1]
        );
        let with_verb = Ctx {
            verbs: vec!["master".into()],
            ..ctx
        };
        let out = run_checks(&with_verb, &checks, |_| {});
        assert_eq!(out[1].status, Status::Missing);
        assert!(out[1].fix.as_deref().unwrap().ends_with("master start"));
        assert!(!with_verb.pm_dir.join("agents").exists());
    }

    /// A missing tracker blocks master; the fix still names only a verb
    /// this binary has.
    #[test]
    fn master_behind_a_missing_tracker_names_no_absent_verb() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx(dir.path());
        let tracker = Check::new("tracker", |_| Found::Absent("t".into()), |_| "init".into());
        let checks = [tracker, master_check()];
        let out = detect_checks(&ctx, &checks);
        assert!(out[1].detail.contains("needs `tracker`"), "{:?}", out[1]);
        assert_eq!(out[1].fix, None, "{:?}", out[1]);
        let with_verb = Ctx {
            verbs: vec!["master".into()],
            ..ctx
        };
        let out = detect_checks(&with_verb, &checks);
        assert!(out[1].fix.as_deref().unwrap().ends_with("master start"));
    }

    /// CAD-448/CAD-439: the master's own login is its own check —
    /// `missing` with the separate-login command until the master's
    /// `CLAUDE_CONFIG_DIR` holds a `.credentials.json`; never applied.
    /// Where the host cannot confine the master it uses the operator's
    /// login, and nothing is asked for.
    #[test]
    fn master_login_is_detected_and_never_applied() {
        let dir = tempfile::tempdir().unwrap();
        let with_master = Ctx {
            verbs: vec!["master".into()],
            ..ctx(dir.path())
        };
        let out = detect_checks(&with_master, &[master_login_check()]);
        if crate::confine::available().is_ok() {
            assert_eq!(out[0].status, Status::Missing, "{:?}", out[0]);
            let fix = out[0].fix.as_deref().unwrap();
            assert!(fix.contains("claude auth login"), "{fix}");
            assert!(fix.contains("CLAUDE_CONFIG_DIR="), "{fix}");
            assert!(
                fix.contains(&with_master.state_dir.display().to_string()),
                "{fix}"
            );
            let creds =
                crate::master::claude_config_dir(&with_master.state_dir).join(".credentials.json");
            std::fs::create_dir_all(creds.parent().unwrap()).unwrap();
            std::fs::write(&creds, "{}").unwrap();
            let out = detect_checks(&with_master, &[master_login_check()]);
            assert_eq!(out[0].status, Status::Ok, "{:?}", out[0]);
            assert_eq!(out[0].fix, None, "{:?}", out[0]);
            assert!(out[0].detail.contains("own login"), "{:?}", out[0]);
        } else {
            assert_eq!(out[0].status, Status::Ok, "{:?}", out[0]);
            assert!(out[0].detail.contains("--unconfined"), "{:?}", out[0]);
        }
        // A build without `master` asks for nothing and names nothing.
        let out = detect_checks(&ctx(dir.path()), &[master_login_check()]);
        assert_eq!(out[0].status, Status::Unknown);
        assert_eq!(out[0].fix, None);
    }

    /// CAD-448: each provider `master start` accepts is offered with its
    /// own outcome's readiness and — ready and the verb present — the
    /// exact start command. A provider `master start` refuses is never
    /// offered.
    #[test]
    fn master_offers_follow_the_provider_checks() {
        let dir = tempfile::tempdir().unwrap();
        let with_master = Ctx {
            verbs: vec!["master".into()],
            ..ctx(dir.path())
        };
        let provider = |name: &str, ready: bool| Outcome {
            check: name.to_string(),
            status: if ready { Status::Ok } else { Status::Missing },
            detail: String::new(),
            fix: None,
        };
        let done = [
            provider("claude", true),
            provider("codex", true),
            provider("pi", false),
            provider("tracker", true),
        ];
        let offers = master_offers(&with_master, &checks(), &done);
        // Only master::PROVIDERS are offered — a signed-in codex is not
        // a master choice while the daemon refuses it.
        assert_eq!(
            offers.iter().map(|o| o.bin).collect::<Vec<_>>(),
            crate::master::PROVIDERS
        );
        let claude = &offers[0];
        assert!(claude.ready);
        let start = claude.start.as_deref().unwrap();
        assert!(start.contains("master start"), "{start}");
        assert!(start.contains("--provider claude"), "{start}");
        if crate::confine::available().is_err() {
            assert!(start.contains("--unconfined"), "{start}");
            assert_eq!(
                claude.warning,
                Some(crate::master::UNCONFINED_WARNING),
                "{claude:?}"
            );
        } else {
            assert_eq!(claude.warning, None, "{claude:?}");
        }
        // The served state dir is not the default: the command names it.
        assert!(start.contains("--state-dir"), "{start}");
        // Provider not ready → no command to start on it.
        let done = [provider("claude", false), provider("tracker", true)];
        assert_eq!(master_offers(&with_master, &checks(), &done)[0].start, None);
        // No `master` verb in this build → nothing to start with.
        let no_verb = ctx(dir.path());
        assert_eq!(master_offers(&no_verb, &checks(), &done)[0].start, None);
    }

    /// CAD-448 review (N4): while the `master` check's prerequisites
    /// are unmet — `tracker` missing on a fresh host — the offer shows
    /// the provider's state but no start command; the step's one
    /// command is the check's own fix, not two commands for one action.
    #[test]
    fn master_offers_wait_for_the_master_checks_prerequisites() {
        let dir = tempfile::tempdir().unwrap();
        let with_master = Ctx {
            verbs: vec!["master".into()],
            ..ctx(dir.path())
        };
        let provider = |name: &str, ready: bool| Outcome {
            check: name.to_string(),
            status: if ready { Status::Ok } else { Status::Missing },
            detail: String::new(),
            fix: None,
        };
        // `tracker` ran and reported missing → the offer carries no
        // command, whatever the provider's own readiness.
        let done = [provider("claude", true), provider("tracker", false)];
        let offers = master_offers(&with_master, &checks(), &done);
        assert!(offers[0].ready);
        assert_eq!(offers[0].start, None, "{offers:?}");
        assert_eq!(offers[0].warning, None, "{offers:?}");
        // Once `tracker` is ready the same offer earns its command.
        let done = [provider("claude", true), provider("tracker", true)];
        let offers = master_offers(&with_master, &checks(), &done);
        assert!(
            offers[0].start.as_deref().unwrap().contains("master start"),
            "{offers:?}"
        );
    }

    /// CAD-448 review (I1): `CADENCE_TEST_NO_LANDLOCK` (the daemon's
    /// debug-build seam) forces the unconfined branch — the offer's
    /// command names `--unconfined` and carries the warning, and
    /// `master_login` states the risk instead of asking for a login.
    /// Runs on any host, confined or not.
    #[test]
    fn unconfined_offers_carry_the_flag_and_the_warning() {
        fn no_landlock(name: &str) -> Option<String> {
            (name == crate::master::TEST_NO_LANDLOCK).then(|| "1".to_string())
        }
        let dir = tempfile::tempdir().unwrap();
        let unconfined = Ctx {
            verbs: vec!["master".into()],
            env: no_landlock,
            ..ctx(dir.path())
        };
        let provider = |name: &str, ready: bool| Outcome {
            check: name.to_string(),
            status: if ready { Status::Ok } else { Status::Missing },
            detail: String::new(),
            fix: None,
        };
        let done = [provider("claude", true), provider("tracker", true)];
        let offers = master_offers(&unconfined, &checks(), &done);
        let claude = &offers[0];
        let start = claude.start.as_deref().unwrap();
        assert!(start.contains("--unconfined"), "{start}");
        assert!(start.contains("--provider claude"), "{start}");
        assert_eq!(
            claude.warning,
            Some(crate::master::UNCONFINED_WARNING),
            "{claude:?}"
        );
        // The login check asks for nothing and says why the choice is
        // risky — not a convenience.
        let out = detect_checks(&unconfined, &[master_login_check()]);
        assert_eq!(out[0].status, Status::Ok, "{:?}", out[0]);
        assert!(out[0].detail.contains("--unconfined"), "{:?}", out[0]);
        assert!(
            out[0].detail.contains("no filesystem sandbox")
                && out[0].detail.contains("read and write your files"),
            "{:?}",
            out[0]
        );
    }

    #[test]
    fn a_started_apply_reports_started() {
        let dir = tempfile::tempdir().unwrap();
        let on = Rc::new(Cell::new(false));
        let seen = on.clone();
        let check = Check::new(
            "svc",
            move |_| {
                if seen.get() {
                    Found::Present("up".into())
                } else {
                    Found::Absent("down".into())
                }
            },
            |_| "start svc".into(),
        )
        .apply(move |_| {
            on.set(true);
            Ok(Applied::Started("started again".into()))
        });
        let out = run_checks(&ctx(dir.path()), &[check], |_| {});
        assert_eq!(out[0].status, Status::Started);
        assert!(out[0].status.ready());
    }

    #[test]
    fn the_shipped_check_list_orders_every_dependency() {
        validate(&checks());
        validate(&board_checks("http://127.0.0.1:3111"));
    }

    #[test]
    fn detect_only_never_applies_and_reports_the_fix() {
        let dir = tempfile::tempdir().unwrap();
        let applied = Rc::new(Cell::new(0));
        let checks = [flag_check("a", applied.clone(), true)];
        let out = detect_checks(&ctx(dir.path()), &checks);
        assert_eq!(out[0].status, Status::Missing, "{:?}", out[0]);
        assert_eq!(out[0].detail, "gone");
        assert_eq!(out[0].fix.as_deref(), Some("make a"));
        assert_eq!(applied.get(), 0, "detect-only ran an apply");
    }

    /// Every shipped check, detect only, over an empty host: nothing is
    /// created — no state dir, tracker, skill or board.
    #[test]
    fn the_board_list_detect_only_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx(dir.path());
        let url = "http://127.0.0.1:3111";
        let out = detect_checks(&ctx, &board_checks(url));
        let names: Vec<&str> = out.iter().map(|o| o.check.as_str()).collect();
        let shipped: Vec<String> = checks().into_iter().map(|c| c.name).collect();
        assert_eq!(names, shipped, "the board runs setup's own list");
        let by = |n: &str| out.iter().find(|o| o.check == n).unwrap();
        for name in ["state_dir", "tracker", "skill"] {
            assert_eq!(by(name).status, Status::Missing, "{:?}", by(name));
            assert!(by(name).fix.is_some(), "{:?}", by(name));
        }
        assert_eq!(by("ui").status, Status::Ok);
        assert!(by("ui").detail.contains(url), "{:?}", by("ui"));
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "detect-only created files"
        );
    }

    #[test]
    fn checks_are_grouped_for_the_wizard() {
        assert_eq!(check_group("claude"), "provider");
        assert_eq!(check_group("pi"), "provider");
        assert_eq!(check_group("master"), "master");
        assert_eq!(check_group("daemon"), "environment");
    }

    #[test]
    #[should_panic(expected = "not an earlier check")]
    fn a_dependency_on_a_later_check_is_a_bug() {
        let late = Check::new("a", |_| Found::Present("x".into()), |_| String::new()).needs(&["b"]);
        let b = Check::new("b", |_| Found::Present("x".into()), |_| String::new());
        validate(&[late, b]);
    }

    /// `run_checks` validates the list in debug builds.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "not an earlier check")]
    fn an_unknown_dependency_is_a_bug() {
        let dir = tempfile::tempdir().unwrap();
        let check =
            Check::new("a", |_| Found::Present("x".into()), |_| String::new()).needs(&["nope"]);
        run_checks(&ctx(dir.path()), &[check], |_| {});
    }

    #[test]
    fn an_installed_skill_that_differs_is_reported_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx(dir.path());
        let first = run_checks(&ctx, &[skill_check()], |_| {});
        assert_eq!(first[0].status, Status::Created, "{:?}", first[0]);
        let file = ctx.home.join(".agents/skills/cadence/SKILL.md");
        let link = ctx.home.join(".cursor/skills/cadence");
        // The operator edits the skill and removes one link.
        std::fs::write(&file, "operator's own skill\n").unwrap();
        std::fs::remove_file(&link).unwrap();
        let out = run_checks(&ctx, &[skill_check()], |_| {});
        assert_eq!(out[0].status, Status::Unknown, "{:?}", out[0]);
        assert!(
            out[0].detail.contains("differs from this binary"),
            "{:?}",
            out[0]
        );
        assert!(out[0]
            .fix
            .as_deref()
            .unwrap()
            .ends_with("cadence skill install"));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "operator's own skill\n"
        );
        assert!(std::fs::symlink_metadata(&link).is_err(), "link re-created");
    }

    #[test]
    fn a_probe_that_leaves_a_background_child_is_still_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake");
        // Answers at once, but its background child keeps stdout open.
        std::fs::write(&fake, "#!/bin/sh\necho 'fake 1.2.3'\n( sleep 40 ) &\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = std::time::Instant::now();
        let _ = version(&fake, Duration::from_secs(1));
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_secs(10), "probe took {elapsed:?}");
    }

    #[test]
    fn only_a_version_shaped_token_is_reported() {
        assert_eq!(
            version_token("2.1.280 (Claude Code)").as_deref(),
            Some("2.1.280")
        );
        assert_eq!(
            version_token("codex-cli 0.156.0").as_deref(),
            Some("0.156.0")
        );
        assert_eq!(
            version_token("2026.09.18-9a7762b").as_deref(),
            Some("2026.09.18-9a7762b")
        );
        assert_eq!(version_token("sk-ant-api03-abcdef"), None);
        assert_eq!(version_token("1234567890abcdef"), None);
        assert_eq!(version_token(""), None);
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
