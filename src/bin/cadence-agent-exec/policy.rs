//! The policy half of `cadence-agent-exec` — ADR 0007 §5 L1,
//! CAD-512 T2.
//!
//! Every decision that gates the `caller → agent-uid` crossing lives
//! here as a pure function so the boundaries are unit-testable without
//! a setuid install. `main.rs` is the syscall layer: it gathers kernel
//! facts (the caller's group set, the fixed account's passwd entry, the
//! ids after the drop), asks this module, then acts.
//!
//! Fixed sources — never argv, never env (§5 L1): the target account
//! name ([`AGENT_USER`]), the caller gate group ([`LAUNCH_GROUP`]), the
//! one shared supplementary group ([`SHARED_GROUP`]) and the child's
//! PATH ([`CHILD_PATH`]).

#![forbid(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

/// Target account — resolved once via `getpwnam` in the privileged
/// layer. The uid is never taken from argv or env: "uid from a fixed
/// source" (acceptance item 1). T1 provisions the user.
pub const AGENT_USER: &str = "cadence-agent";
/// The caller must carry this group (as its real gid or in its
/// supplementary set). T1 provisions it and adds `ubuntu` only.
pub const LAUNCH_GROUP: &str = "cadence-launch";
/// The shared-edge group (§2): the dropped process keeps exactly the
/// agent's primary group plus this one — no inherited operator groups.
pub const SHARED_GROUP: &str = "cadence";
/// The child's PATH — the caller's PATH is never propagated (it can
/// carry trojaned directories). `/opt/cadence/bin` first so the shared
/// release tree wins over system paths (§3 move 4).
pub const CHILD_PATH: &str = "/opt/cadence/bin:/usr/local/bin:/usr/bin:/bin";

/// Bounds so a hostile or buggy caller cannot make the helper spend
/// unbounded effort or land an oversized exec. Every bound is
/// generous versus the daemon's real argv (a pane command plus a
/// handful of `--env` pairs).
pub const MAX_ARGS: usize = 1024;
pub const MAX_ARG_BYTES: usize = 128 * 1024;
pub const MAX_ENV_PAIRS: usize = 64;
pub const MAX_ENV_NAME_BYTES: usize = 128;
pub const MAX_ENV_VALUE_BYTES: usize = 16 * 1024;

/// Signals the daemon legitimately sends agent processes today:
/// SIGINT interrupts a turn (`StdioAdapter::signal_group`), SIGTERM
/// then SIGKILL reap a pane's session (`lane::reap_session`). Nothing
/// else is signallable through this helper — STOP/CONT would stall the
/// scheduler's accounting, QUIT dumps agent memory to disk, and HUP is
/// delivered by pty teardown, not the daemon.
pub const SIGNALS: &[(&str, i32)] = &[
    ("INT", libc::SIGINT),
    ("TERM", libc::SIGTERM),
    ("KILL", libc::SIGKILL),
];

/// Exact `--env` names allowed into the child.
const ENV_EXACT: &[&str] = &["TERM", "COLORTERM", "LANG", "TZ", "TMPDIR"];
/// `--env` name prefixes allowed into the child: `CADENCE_*` is the
/// daemon's context (alias, socket, tracker, profile, suite lock);
/// `CLAUDE_*`/`CODEX_*`/`ANTHROPIC_*` are provider config the adapters
/// already inject today (`claude_env_scrub`'s keep-list and pass-through
/// prefixes); `LC_*` is locale. PATH, HOME, USER, LOGNAME and SHELL are
/// absent on purpose — the helper sets them itself.
const ENV_PREFIX: &[&str] = &["CADENCE_", "LC_", "CLAUDE_", "CODEX_", "ANTHROPIC_"];
/// The carve-out inside the allowed prefixes: the `CADENCE_DEVIN_*`
/// cloud credentials that `adapter::CLOUD_SECRET_ENV` exists to keep
/// off pane environments. An allowlist entry must never name a secret.
const ENV_NEVER_PREFIX: &[&str] = &["CADENCE_DEVIN_"];

/// What the caller asked for, fully validated. Produced by [`parse`];
/// `main.rs` executes it after the drop.
#[derive(Debug)]
pub enum Request {
    /// `exec [--env K=V]… -- <argv…>` — spawn as the agent uid.
    Exec {
        env: Vec<(OsString, OsString)>,
        argv: Vec<OsString>,
    },
    /// `kill <pid> <signal>` — signal an agent-uid pid.
    Kill { pid: i32, signal: i32 },
    /// `inspect <pid>` — read-only `/proc` facts about an agent-uid
    /// pid (cwd is the read that fails cross-uid, §6).
    Inspect { pid: i32 },
}

/// The three verbs only — no flags, no `--help` that does work, no
/// fourth verb. Anything else is refused.
pub fn parse(args: &[OsString]) -> Result<Request, String> {
    if args.len() > MAX_ARGS {
        return Err(format!("too many arguments ({} > {MAX_ARGS})", args.len()));
    }
    if args.iter().any(|a| a.as_bytes().len() > MAX_ARG_BYTES) {
        return Err(format!("an argument exceeds {MAX_ARG_BYTES} bytes"));
    }
    match args.first().map(|a| a.as_bytes()) {
        Some(b"exec") => parse_exec(&args[1..]),
        Some(b"kill") => parse_kill(&args[1..]),
        Some(b"inspect") => parse_inspect(&args[1..]),
        Some(other) => Err(format!(
            "unknown verb {:?} — expected exec, kill or inspect",
            String::from_utf8_lossy(other)
        )),
        None => Err("usage: cadence-agent-exec exec [--env K=V]… -- <argv…> | \
                     kill <pid> <signal> | inspect <pid>"
            .into()),
    }
}

/// `exec [--env K=V]… -- <argv…>`: only `--env` pairs may precede the
/// mandatory `--`; everything after `--` is verbatim argv, so a
/// command's own flags are never parsed as ours.
fn parse_exec(rest: &[OsString]) -> Result<Request, String> {
    let mut env: Vec<(OsString, OsString)> = Vec::new();
    let mut i = 0;
    loop {
        let arg = rest
            .get(i)
            .ok_or("exec needs `--` before the command argv")?;
        if arg.as_bytes() == b"--" {
            let argv = &rest[i + 1..];
            if argv.is_empty() {
                return Err("exec needs a command after `--`".into());
            }
            return Ok(Request::Exec {
                env,
                argv: argv.to_vec(),
            });
        }
        if arg.as_bytes() == b"--env" {
            i += 1;
            let (name, value) = parse_env_pair(
                rest.get(i)
                    .ok_or("`--env` needs a following K=V argument")?,
            )?;
            if env.iter().any(|(k, _)| *k == name) {
                return Err(format!("duplicate --env name {:?}", name.to_string_lossy()));
            }
            env.push((name, value));
            if env.len() > MAX_ENV_PAIRS {
                return Err(format!("too many --env pairs (> {MAX_ENV_PAIRS})"));
            }
            i += 1;
            continue;
        }
        return Err(format!(
            "exec: unexpected argument {:?} — only `--env K=V` or `--` may \
             precede the command",
            arg.to_string_lossy()
        ));
    }
}

/// One `K=V` pair for `--env`: POSIX name syntax, allowlisted name,
/// bounded both halves. The value may itself contain `=` (only the
/// first splits).
fn parse_env_pair(pair: &OsString) -> Result<(OsString, OsString), String> {
    let bytes = pair.as_bytes();
    let eq = bytes
        .iter()
        .position(|&b| b == b'=')
        .ok_or("`--env` needs K=V form")?;
    let (name, value) = (&bytes[..eq], &bytes[eq + 1..]);
    let first = name.first().copied().unwrap_or(0);
    if !(first.is_ascii_alphabetic() || first == b'_')
        || !name.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_')
    {
        return Err(format!(
            "bad --env name {:?} — expected [A-Za-z_][A-Za-z0-9_]*",
            String::from_utf8_lossy(name)
        ));
    }
    if name.len() > MAX_ENV_NAME_BYTES {
        return Err(format!("--env name exceeds {MAX_ENV_NAME_BYTES} bytes"));
    }
    if value.len() > MAX_ENV_VALUE_BYTES {
        return Err(format!("--env value exceeds {MAX_ENV_VALUE_BYTES} bytes"));
    }
    // The name is pure ASCII at this point — from_utf8 cannot fail.
    let name_str = std::str::from_utf8(name).map_err(|_| "unreachable".to_string())?;
    if !env_allowed(name_str) {
        return Err(format!("--env {name_str}=… is not in the env allowlist"));
    }
    Ok((
        OsString::from_vec(name.to_vec()),
        OsString::from_vec(value.to_vec()),
    ))
}

/// The env allowlist: exact names or allowed prefixes with a
/// non-empty suffix (a bare `LC_`/`CADENCE_` is junk, not a name),
/// minus the secret-bearing carve-out. Deny-by-default — `LD_*`,
/// `PATH`, `IFS`, `BASH_ENV`, `GIT_*`, `SSH_*`, `*_KEY` never need
/// enumerating.
pub fn env_allowed(name: &str) -> bool {
    if ENV_NEVER_PREFIX.iter().any(|p| name.starts_with(p)) {
        return false;
    }
    ENV_EXACT.contains(&name)
        || ENV_PREFIX
            .iter()
            .any(|p| name.len() > p.len() && name.starts_with(p))
}

fn parse_kill(rest: &[OsString]) -> Result<Request, String> {
    if rest.len() != 2 {
        return Err("kill takes exactly <pid> <signal>".into());
    }
    Ok(Request::Kill {
        pid: parse_pid(&rest[0])?,
        signal: parse_signal(&rest[1])?,
    })
}

fn parse_inspect(rest: &[OsString]) -> Result<Request, String> {
    if rest.len() != 1 {
        return Err("inspect takes exactly <pid>".into());
    }
    Ok(Request::Inspect {
        pid: parse_pid(&rest[0])?,
    })
}

/// Digits only, `1..=i32::MAX` (`pid_t`). `0` and negative numbers are
/// process-*group* targets — `kill(0, sig)` would signal the helper's
/// own group, `kill(-n, sig)` any group — never allowed.
pub fn parse_pid(arg: &OsStr) -> Result<i32, String> {
    let bytes = arg.as_bytes();
    if bytes.is_empty() || bytes.len() > 10 || !bytes.iter().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "{:?} is not a pid — digits only",
            String::from_utf8_lossy(bytes)
        ));
    }
    let pid: i32 = std::str::from_utf8(bytes)
        .unwrap_or("")
        .parse()
        .map_err(|_| "pid out of range".to_string())?;
    if pid <= 0 {
        return Err("pid must be > 0 — 0 and negatives are process groups".into());
    }
    Ok(pid)
}

/// A signal name from the [`SIGNALS`] table, or a bare number that
/// resolves into the same set — so `kill 17 TERM` and `kill 17 15` are
/// the same admission.
pub fn parse_signal(arg: &OsStr) -> Result<i32, String> {
    let bytes = arg.as_bytes();
    if !bytes.is_empty() && bytes.iter().all(|b| b.is_ascii_digit()) {
        let n: i32 = std::str::from_utf8(bytes)
            .unwrap_or("")
            .parse()
            .map_err(|_| "signal out of range".to_string())?;
        if SIGNALS.iter().any(|&(_, s)| s == n) {
            return Ok(n);
        }
    } else if let Some(&(_, s)) = SIGNALS.iter().find(|&&(name, _)| name.as_bytes() == bytes) {
        return Ok(s);
    }
    Err(format!(
        "signal {:?} is not in the allowlist ({})",
        String::from_utf8_lossy(bytes),
        SIGNALS
            .iter()
            .map(|&(n, _)| n)
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// The caller gate, part 1 — the outermost boundary, run before the
/// target account is even resolved: the caller's kernel group set
/// (real gid + supplementary) must contain the `cadence-launch` gid.
/// A host without the group fails closed — nobody is a member.
pub fn caller_is_member(groups: &[u32], launch_gid: Option<u32>) -> Result<(), String> {
    let Some(gid) = launch_gid else {
        return Err(format!(
            "group {LAUNCH_GROUP} does not exist — host is not provisioned (T1)"
        ));
    };
    if !groups.contains(&gid) {
        return Err(format!("caller is not in group {LAUNCH_GROUP}"));
    }
    Ok(())
}

/// The ownership boundary for `kill`/`inspect`: both the real and the
/// effective uid of the target must be the agent's. Checking either
/// alone is a hole — a setuid-root binary on the target side, or a
/// kernel thread carrying a different eff-uid, would slip past one.
/// (For `kill(2)` itself the kernel's EPERM is still the last word;
/// this is the clean refusal ahead of it.)
pub fn target_is_agent(real: u32, effective: u32, agent_uid: u32) -> bool {
    real == agent_uid && effective == agent_uid
}

/// The caller gate, part 2: the agent uid may never invoke its own
/// launch helper — the crossing is `ubuntu → cadence-agent` only, and
/// an agent-uid caller gains nothing (it could signal or inspect its
/// peers directly anyway).
pub fn caller_is_not_agent(uid: u32, agent_uid: u32) -> Result<(), String> {
    if uid == agent_uid {
        return Err(format!(
            "the {AGENT_USER} uid may not invoke its own helper"
        ));
    }
    Ok(())
}

/// The child's environment: an empty start plus the fixed base vars —
/// PATH from [`CHILD_PATH`], HOME/USER/LOGNAME/SHELL from the agent's
/// passwd entry — then the validated `--env` pairs. Nothing from the
/// caller's `environ` is ever copied, so daemon credentials cannot
/// leak through inheritance.
pub fn child_env(
    home: &str,
    name: &str,
    shell: &str,
    pairs: &[(OsString, OsString)],
) -> Vec<OsString> {
    let mut out: Vec<OsString> = [
        format!("PATH={CHILD_PATH}"),
        format!("HOME={home}"),
        format!("USER={name}"),
        format!("LOGNAME={name}"),
        format!("SHELL={shell}"),
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    for (k, v) in pairs {
        let mut kv = k.as_encoded_bytes().to_vec();
        kv.push(b'=');
        kv.extend_from_slice(v.as_encoded_bytes());
        out.push(OsString::from_vec(kv));
    }
    out
}

/// `execvp` resolution against [`CHILD_PATH`] only: a program name
/// containing `/` is used as-is, otherwise every fixed dir yields one
/// candidate — the privileged layer probes them with `access(X_OK)`
/// after the drop, so the check runs with the agent's permissions.
/// No `ENOENT → /bin/sh` fallback: this helper never introduces a
/// shell.
pub fn program_candidates(program: &OsStr) -> Vec<OsString> {
    if program.as_bytes().contains(&b'/') {
        return vec![program.to_os_string()];
    }
    CHILD_PATH
        .split(':')
        .map(|dir| {
            let mut p = OsString::from(dir);
            p.push("/");
            p.push(program);
            p
        })
        .collect()
}

/// `Uid:` line from `/proc/<pid>/status` → (real, effective) uid. The
/// owner check for `inspect`/`kill`: both must equal the helper's own
/// post-drop uid, i.e. the agent's — the helper only ever acts on
/// agent-uid pids.
pub fn status_uids(status: &str) -> Option<(u32, u32)> {
    ids_line(status, "Uid:")
}

/// The matching `Gid:` line — reported by `inspect` so the daemon can
/// confirm the group identity of the process it launched.
pub fn status_gids(status: &str) -> Option<(u32, u32)> {
    ids_line(status, "Gid:")
}

fn ids_line(status: &str, tag: &str) -> Option<(u32, u32)> {
    let line = status.lines().find(|l| l.starts_with(tag))?;
    let mut f = line[tag.len()..].split_whitespace();
    let real: u32 = f.next()?.parse().ok()?;
    let effective: u32 = f.next()?.parse().ok()?;
    Some((real, effective))
}

/// ppid (field 4), session (field 6) and starttime (field 22) from a
/// `/proc/<pid>/stat` line. `comm` may contain spaces and parens, so
/// fields split after its *last* `)` (same trick as
/// `pty::claude::proc_start_ticks`).
pub fn proc_stat_ids(stat: &str) -> Option<(u64, u64, u64)> {
    let rest = stat.rsplit_once(')')?.1;
    let f: Vec<&str> = rest.split_whitespace().collect();
    if f.len() <= 19 {
        return None;
    }
    Some((
        f[1].parse().ok()?,  // field 4  ppid
        f[3].parse().ok()?,  // field 6  session
        f[19].parse().ok()?, // field 22 starttime
    ))
}

/// Percent-encode bytes for one-line output: anything outside
/// printable non-space ASCII becomes `%XX`, as does `%` itself — a
/// cwd can legally contain tabs and newlines that would corrupt the
/// `inspect` line format.
pub fn pct_encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        if b.is_ascii_graphic() && b != b'%' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(|s| os(s)).collect()
    }

    // ---- verbs: the three only ----

    #[test]
    fn the_three_verbs_parse() {
        assert!(matches!(
            parse(&args(&["exec", "--", "cmd"])).unwrap(),
            Request::Exec { .. }
        ));
        assert!(matches!(
            parse(&args(&["kill", "1", "TERM"])).unwrap(),
            Request::Kill {
                pid: 1,
                signal: libc::SIGTERM
            }
        ));
        assert!(matches!(
            parse(&args(&["inspect", "7"])).unwrap(),
            Request::Inspect { pid: 7 }
        ));
    }

    #[test]
    fn no_verb_or_a_fourth_verb_is_refused() {
        assert!(parse(&[]).is_err());
        for verb in ["sh", "shell", "run", "sudo", "eval", "", "EXEC", "Kill"] {
            assert!(parse(&args(&[verb, "--", "x"])).is_err(), "{verb}");
        }
        // A second verb after a valid one is just an unexpected arg.
        assert!(parse(&args(&["inspect", "1", "extra"])).is_err());
        assert!(parse(&args(&["kill", "1", "TERM", "extra"])).is_err());
    }

    // ---- exec argv injection ----

    #[test]
    fn exec_requires_double_dash_and_a_command() {
        assert!(parse(&args(&["exec"])).is_err());
        assert!(parse(&args(&["exec", "--"])).is_err());
        assert!(parse(&args(&["exec", "ls"])).is_err()); // missing --
        assert!(parse(&args(&["exec", "--env", "TERM=x"])).is_err()); // no --
        assert!(parse(&args(&["exec", "-l", "--", "ls"])).is_err()); // unknown flag
    }

    #[test]
    fn exec_env_pairs_stay_before_the_dash() {
        let Request::Exec { env, argv } = parse(&args(&[
            "exec",
            "--env",
            "TERM=xterm",
            "--",
            "sh",
            "-c",
            "x",
        ]))
        .unwrap() else {
            panic!()
        };
        assert_eq!(env.len(), 1);
        assert_eq!(argv, args(&["sh", "-c", "x"]));
        // After --, even option-shaped tokens pass through verbatim.
        let Request::Exec { argv, .. } = parse(&args(&["exec", "--", "--env", "-x"])).unwrap()
        else {
            panic!()
        };
        assert_eq!(argv, args(&["--env", "-x"]));
    }

    #[test]
    fn exec_refuses_malformed_env_and_duplicates() {
        for bad in ["", "=v", "K", "9K=v", "K-A=v", "K A=v"] {
            assert!(
                parse(&args(&["exec", "--env", bad, "--", "x"])).is_err(),
                "{bad:?}"
            );
        }
        assert!(parse(&args(&["exec", "--env"])).is_err()); // dangling
        assert!(parse(&args(&[
            "exec", "--env", "TERM=a", "--env", "TERM=b", "--", "x"
        ]))
        .is_err());
    }

    #[test]
    fn exec_bounds_env_count_and_sizes() {
        let mut a = args(&["exec"]);
        for i in 0..=MAX_ENV_PAIRS {
            a.push(os("--env"));
            a.push(os(&format!("CADENCE_K{i}=v")));
        }
        a.push(os("--"));
        a.push(os("x"));
        assert!(parse(&a).is_err());

        let long = "x".repeat(MAX_ENV_VALUE_BYTES + 1);
        let pair = format!("TERM={long}");
        assert!(parse(&args(&["exec", "--env", &pair, "--", "x"])).is_err());

        let long_name = format!("CADENCE_{}", "K".repeat(MAX_ENV_NAME_BYTES));
        let pair = format!("{long_name}=v");
        assert!(parse(&args(&["exec", "--env", &pair, "--", "x"])).is_err());

        let huge_arg = "y".repeat(MAX_ARG_BYTES + 1);
        assert!(parse(&args(&["exec", "--", &huge_arg])).is_err());

        // And the argv count cap itself — a valid exec shape, just too
        // many argv words (a leading non-verb would err for the wrong
        // reason, so this must parse cleanly absent the cap).
        let mut too_many = args(&["exec", "--", "x"]);
        for _ in 0..MAX_ARGS {
            too_many.push(os("x"));
        }
        assert!(parse(&too_many).is_err());
    }

    // ---- env smuggling ----

    #[test]
    fn env_allowlist_refuses_loader_shell_and_credential_names() {
        for name in [
            // dynamic-loader / runtime injection
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "LD_AUDIT",
            "DYLD_INSERT_LIBRARIES",
            "GCONV_PATH",
            "NODE_OPTIONS",
            "PYTHONPATH",
            "PERL5OPT",
            "RUBYLIB",
            // shell startup / field splitting
            "PATH",
            "IFS",
            "ENV",
            "BASH_ENV",
            "SHELLOPTS",
            "PS4",
            "PROMPT_COMMAND",
            "ENV",
            // helper-owned base vars — never caller-supplied
            "HOME",
            "USER",
            "LOGNAME",
            "SHELL",
            // program-level exec vectors and agent-side auth material
            "GIT_SSH_COMMAND",
            "GIT_EXEC_PATH",
            "GIT_EXTERNAL_DIFF",
            "GIT_CONFIG_COUNT",
            "SSH_AUTH_SOCK",
            "SSH_ASKPASS",
            "GH_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "NPM_CONFIG__AUTH",
            // cloud credentials the daemon itself strips (CLOUD_SECRET_ENV)
            "DEVIN_API_KEY",
            "DEVIN_ORG_ID",
            "CADENCE_DEVIN_API_KEY",
            "CADENCE_DEVIN_ORG_ID",
            "CADENCE_DEVIN_API_BASE",
            // prefix lookalikes and case tricks
            "cadence_alias",
            "Cadence_ALIAS",
            "CLAUDE",
            "LC_",
            "TERM_PROGRAM",
            "ANTHROPIC",
            "XDG_CONFIG_HOME",
        ] {
            assert!(!env_allowed(name), "{name}");
            assert!(
                parse(&args(&["exec", "--env", &format!("{name}=v"), "--", "x"])).is_err(),
                "{name}"
            );
        }
    }

    #[test]
    fn env_allowlist_accepts_daemon_context_and_provider_names() {
        for name in [
            "CADENCE_ALIAS",
            "CADENCE_SOCKET",
            "CADENCE_PM_DIR",
            "CADENCE_PROFILE",
            "CADENCE_SUITE_LOCK",
            "CADENCE_STATE_DIR",
            "TERM",
            "COLORTERM",
            "LANG",
            "TZ",
            "TMPDIR",
            "LC_ALL",
            "LC_CTYPE",
            "CLAUDE_CONFIG_DIR",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CODEX_HOME",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_BASE_URL",
        ] {
            assert!(env_allowed(name), "{name}");
        }
    }

    #[test]
    fn child_env_starts_empty_plus_fixed_and_never_inherits() {
        // A caller-side environ value must not appear in the child env.
        // (set_var is safe in edition 2021.)
        std::env::set_var("CADENCE_TEST_INHERITED", "leak");
        let env = child_env(
            "/home/cadence-agent",
            "cadence-agent",
            "/usr/sbin/nologin",
            &[(os("TERM"), os("xterm")), (os("CADENCE_ALIAS"), os("w1"))],
        );
        let text: Vec<String> = env
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(env.len(), 7);
        assert_eq!(text[0], format!("PATH={CHILD_PATH}"));
        assert_eq!(text[1], "HOME=/home/cadence-agent");
        assert_eq!(text[2], "USER=cadence-agent");
        assert_eq!(text[3], "LOGNAME=cadence-agent");
        assert_eq!(text[4], "SHELL=/usr/sbin/nologin");
        assert_eq!(text[5], "TERM=xterm");
        assert_eq!(text[6], "CADENCE_ALIAS=w1");
        assert!(!text.iter().any(|l| l.contains("leak")));
    }

    // ---- kill boundaries ----

    #[test]
    fn pid_must_be_a_plain_positive_digit_string() {
        for bad in [
            "",
            "0",
            "-1",
            "+1",
            " 1",
            "1 ",
            "1x",
            "x1",
            "1.0",
            "2147483648",
            "99999999999999999999",
            "00 1",
        ] {
            assert!(parse_pid(&os(bad)).is_err(), "{bad:?}");
        }
        assert_eq!(parse_pid(&os("1")).unwrap(), 1);
        assert_eq!(parse_pid(&os("4194304")).unwrap(), 4194304);
        assert_eq!(parse_pid(&os("2147483647")).unwrap(), i32::MAX);
    }

    #[test]
    fn signals_are_allowlisted_by_name_or_number() {
        assert_eq!(parse_signal(&os("TERM")).unwrap(), libc::SIGTERM);
        assert_eq!(parse_signal(&os("KILL")).unwrap(), libc::SIGKILL);
        assert_eq!(parse_signal(&os("INT")).unwrap(), libc::SIGINT);
        assert_eq!(parse_signal(&os("15")).unwrap(), libc::SIGTERM);
        assert_eq!(parse_signal(&os("9")).unwrap(), libc::SIGKILL);
        assert_eq!(parse_signal(&os("2")).unwrap(), libc::SIGINT);
        for bad in [
            "", "STOP", "19", "USR1", "10", "HUP", "1", "QUIT", "3", "SIGTERM", "term", "-9", "0",
            "CONT", "18",
        ] {
            assert!(parse_signal(&os(bad)).is_err(), "{bad:?}");
        }
        // The gate lands inside `parse` too.
        assert!(parse(&args(&["kill", "4", "STOP"])).is_err());
        assert!(parse(&args(&["kill", "0", "TERM"])).is_err());
        assert!(parse(&args(&["kill", "-4", "KILL"])).is_err());
    }

    // ---- the caller gate ----

    #[test]
    fn caller_must_carry_the_launch_group_and_not_be_the_agent() {
        // Member via supplementary group, and via primary gid.
        assert!(caller_is_member(&[1000, 2000, 3000], Some(2000)).is_ok());
        assert!(caller_is_member(&[2000], Some(2000)).is_ok());
        // Non-member refused — including root: uid 0 is not magic.
        assert!(caller_is_member(&[1000, 3000], Some(2000)).is_err());
        assert!(caller_is_member(&[0], Some(2000)).is_err());
        assert!(caller_is_member(&[], Some(2000)).is_err());
        // Group absent on the host: fail closed for every caller.
        assert!(caller_is_member(&[2000], None).is_err());
        // The agent uid may not invoke its own helper even as a member.
        assert!(caller_is_not_agent(500, 500).is_err());
        assert!(caller_is_not_agent(1000, 500).is_ok());
    }

    // ---- target ownership (kill/inspect) ----

    #[test]
    fn foreign_uid_ownership_is_refused() {
        assert!(target_is_agent(501, 501, 501));
        assert!(!target_is_agent(0, 0, 501)); // a foreign (root-owned) pid
        assert!(!target_is_agent(502, 502, 501)); // a foreign non-root pid
        assert!(!target_is_agent(501, 0, 501)); // setuid drift, either direction
        assert!(!target_is_agent(0, 501, 501));
    }

    // ---- fixed-source uid: env and argv cannot override ----

    #[test]
    fn the_target_uid_source_is_a_constant() {
        std::env::set_var("CADENCE_AGENT_UID", "0");
        std::env::set_var("CADENCE_AGENT_USER", "root");
        std::env::set_var("CADENCE_AGENT_EXEC_TARGET", "root");
        assert_eq!(AGENT_USER, "cadence-agent");
        assert_eq!(LAUNCH_GROUP, "cadence-launch");
        assert_eq!(SHARED_GROUP, "cadence");
        // argv offers no override knob either: an extra arg is refused.
        assert!(parse(&args(&["inspect", "1", "--uid", "0"])).is_err());
        assert!(parse(&args(&["exec", "--uid", "0", "--", "x"])).is_err());
    }

    // ---- program resolution ----

    #[test]
    fn program_resolution_uses_only_the_fixed_path() {
        assert_eq!(
            program_candidates(&os("claude")),
            args(&[
                "/opt/cadence/bin/claude",
                "/usr/local/bin/claude",
                "/usr/bin/claude",
                "/bin/claude"
            ])
        );
        // Anything containing a slash is used verbatim — including
        // ./relative forms; PATH is never consulted for those.
        assert_eq!(program_candidates(&os("/opt/x/run")), args(&["/opt/x/run"]));
        assert_eq!(program_candidates(&os("./x")), args(&["./x"]));
    }

    // ---- inspect's /proc parsers ----

    #[test]
    fn status_uids_reads_the_uid_line() {
        let status = "Name:\tsleep\nUid:\t501\t501\t501\t501\nGid:\t502\t502\t502\t502\n";
        assert_eq!(status_uids(status), Some((501, 501)));
        assert_eq!(status_gids(status), Some((502, 502)));
        assert_eq!(status_uids("Name:\tx\n"), None);
        assert_eq!(status_uids(""), None);
    }

    #[test]
    fn proc_stat_ids_survives_a_padded_comm() {
        // comm containing spaces and parens: split after the LAST ')'.
        let stat =
            "42 (weird ) name) S 1 42 42 0 -1 4194304 100 0 0 0 0 0 0 0 20 0 1 0 98765 0 0 0";
        // ppid=1(session 42)… starttime=98765
        assert_eq!(proc_stat_ids(stat), Some((1, 42, 98765)));
        assert_eq!(proc_stat_ids("garbage"), None);
        assert_eq!(proc_stat_ids("42 (x) S 1"), None); // too few fields
    }

    #[test]
    fn pct_encode_keeps_a_line_atomic() {
        assert_eq!(pct_encode(b"/plain/path"), "/plain/path");
        assert_eq!(
            pct_encode("/a b/c\td/e\nf/%g".as_bytes()),
            "/a%20b/c%09d/e%0Af/%25g"
        );
        assert_eq!(pct_encode(&[0xff, b'a']), "%FFa");
    }
}
