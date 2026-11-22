//! Provider CLI detection, bounded probing, sign-in verification, and
//! master confinement checks.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::{Check, Ctx, Found};
use crate::error::Result;

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
    // CAD-322: the pinned, scoped package the master is tested against
    // (host may run a different one — setup installs exactly this).
    Provider {
        bin: "pi",
        install: "npm install -g @earendil-works/pi-coding-agent@0.87.1",
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
pub(super) fn version(path: &Path, timeout: Duration) -> Option<String> {
    let out = probe(path, &["--version"], timeout)?;
    if !out.status.success() {
        return None;
    }
    version_token(&String::from_utf8_lossy(&out.stdout))
}

pub(super) fn version_token(text: &str) -> Option<String> {
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
pub(super) fn signed_in(ctx: &Ctx, path: &Path, provider: &Provider) -> (Option<bool>, String) {
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

pub(super) fn provider_check(provider: &'static Provider) -> Check {
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
pub(super) fn master_check() -> Check {
    Check::new(
        "master",
        |ctx| {
            let dir = ctx.pm_dir.join("agents").join("master");
            let missing: Vec<&str> = crate::master::FILES
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
pub(super) fn confinement(ctx: &Ctx) -> Result<()> {
    let env = crate::adapter::ProviderEnv::default();
    if let Some(v) = (ctx.env)(crate::master::TEST_NO_LANDLOCK) {
        env.set(crate::master::TEST_NO_LANDLOCK, v);
    }
    crate::master::confinement_available(&env)
}

/// CAD-448: the master's own login (CAD-439) — a separate per-provider
/// config dir under the state dir (`master/claude`, `master/pi`),
/// never the operator's `~/.claude` or `~/.pi`. Detect only: the login
/// command is interactive, so an absent login is `missing` with the
/// command to run, never applied. The provider probed is the one the
/// master last ran as — its dir is what `master start` created — or,
/// before any start, the default. A host that cannot confine the master
/// runs it `--unconfined` on the operator's own login — there is no
/// separate login to ask for, and the detail states the risk that
/// choice carries.
pub(super) fn master_login_check() -> Check {
    Check::new(
        "master_login",
        |ctx| {
            if !ctx.has_verb("master") {
                return Found::Unknown("the master is not in this build".into());
            }
            if confinement(ctx).is_err() {
                return Found::Present(
                    "this host cannot confine the master — `master start --unconfined` \
                     runs it on your own login, with no filesystem sandbox: it can \
                     read and write your files"
                        .into(),
                );
            }
            let provider = master_provider(ctx);
            let dir = crate::master::provider_config_dir(provider, &ctx.state_dir);
            if crate::master::has_login_for(provider, &ctx.state_dir) {
                Found::Present(format!("own {provider} login in {}", dir.display()))
            } else {
                Found::Absent(format!(
                    "no login in {} — the master's {provider} cannot authenticate without \
                     its own (or `master start --provider {provider} --copy-login`)",
                    dir.display()
                ))
            }
        },
        |ctx| {
            if ctx.has_verb("master") {
                crate::master::login_command_for(master_provider(ctx), &ctx.state_dir)
            } else {
                String::new()
            }
        },
    )
}

/// Which provider the master runs as, for the login probe: the
/// per-provider config dir `master start` created, newest first —
/// `master/pi` only exists once a Pi master ran. Before any start the
/// answer is the default provider.
fn master_provider(ctx: &Ctx) -> &'static str {
    let mtime = |p: &str| {
        std::fs::metadata(crate::master::provider_config_dir(p, &ctx.state_dir))
            .and_then(|m| m.modified())
            .ok()
    };
    match (mtime("pi"), mtime("claude")) {
        (Some(pi), Some(cl)) if pi > cl => "pi",
        (Some(_), None) => "pi",
        _ => "claude",
    }
}

/// CAD-313 hook: the single-use operator login link for the board.
/// `None` until CAD-313's link-minting verb (`cadence ui login`) is on
/// main — then this calls it and the `login` check reports the link.
pub fn login_link(_ctx: &Ctx) -> Option<String> {
    None
}

pub(super) fn login_check() -> Check {
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
