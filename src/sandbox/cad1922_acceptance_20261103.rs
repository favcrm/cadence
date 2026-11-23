//! Independent CAD-1922 acceptance (PR #1478, head 6440e63): the refusals the
//! consolidated-home change makes the daemon and the `dev` verbs enforce.
//!
//! Authored from the ticket by a reviewer who did not write the PR, because
//! AGENTS.md requires a daemon-enforced rule's acceptance check to come from
//! someone other than the implementer. The implementer must not edit or weaken
//! it. Each test drives the real guard and shows the bad case refused, next to
//! a positive control so a refusal cannot be a bad fixture.
//!
//! Child module of `sandbox`: `refuse_production` is private. Process
//! environment is rewritten only under `home::CAD1922_ENV_LOCK`.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use super::*;
use crate::adapter::ProviderEnv;

const NAMES: [&str; 9] = [
    "HOME",
    "CADENCE_HOME",
    "CADENCE_STATE_DIR",
    "CADENCE_PM_DIR",
    "CADENCE_PROFILE",
    "CADENCE_SANDBOX_ROOT",
    "XDG_STATE_HOME",
    "XDG_CONFIG_HOME",
    "CADENCE_TEST_REAL_HOME",
];

/// Process env isolated under the shared CAD-1922 lock, restored on drop.
struct Env {
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Env {
    fn with_home(home: &Path) -> Self {
        let lock = crate::home::CAD1922_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let env = Self {
            saved: NAMES.iter().map(|&n| (n, std::env::var_os(n))).collect(),
            _lock: lock,
        };
        for name in NAMES {
            std::env::remove_var(name);
        }
        std::env::set_var("HOME", home);
        crate::home::note_state_dir_flag(None);
        env
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        crate::home::note_state_dir_flag(None);
        for (name, value) in &self.saved {
            match value {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
    }
}

fn scratch(tag: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(tag)
        .tempdir_in("/tmp")
        .unwrap()
}

/// Every file under `dir` with its bytes, for a before/after comparison.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push((path.clone(), std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    out
}

/// A daemon-captured provider env that names a sandbox profile and a base
/// only here — the process env names neither.
fn provider(profile: &str, base: &Path, home: &Path) -> ProviderEnv {
    let env = ProviderEnv::default();
    env.set("CADENCE_PROFILE", profile);
    env.set("CADENCE_SANDBOX_ROOT", base.to_str().unwrap());
    env.set("HOME", home.to_str().unwrap());
    env
}

/// The result is a refusal; if it ever were a path it must not be the host's.
fn assert_refused(
    got: crate::error::Result<PathBuf>,
    home: &Path,
    why: &str,
    expect: &str,
) -> String {
    match got {
        Ok(path) => {
            assert_ne!(
                path,
                home.join(".cadence/workspace"),
                "{why}: fell back to host"
            );
            panic!("{why}: expected a refusal, got {}", path.display());
        }
        Err(e) => {
            let text = e.to_string();
            assert!(text.contains(expect), "{why}: wrong refusal: {text}");
            text
        }
    }
}

/// 1. The function the daemon calls refuses a sandbox profile that does not
///    resolve to a valid sandbox root, and never binds the host workspace.
#[test]
fn cad1922_accept_daemon_workspace_refuses_every_bad_sandbox_profile() {
    let fx = scratch("c22a1");
    let home = fx.path().join("home");
    let base = fx.path().join("box");
    let tracker = fx.path().join("tracker");
    for d in [&home, &base, &tracker] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::create_dir_all(home.join(".cadence")).unwrap();
    let _env = Env::with_home(&home);
    let env = provider("sandbox:acc", &base, &home);
    let root = base.join("acc");
    std::fs::create_dir_all(root.join("state")).unwrap();

    // Missing marker.
    assert_refused(
        crate::home::workspace_for_provider_env(&tracker, &env),
        &home,
        "missing marker",
        "refusing to fall back",
    );
    // A marker naming another sandbox.
    std::fs::write(root.join(MARKER), r#"{"name":"other"}"#).unwrap();
    assert_refused(
        crate::home::workspace_for_provider_env(&tracker, &env),
        &home,
        "marker for another sandbox",
        "belongs to sandbox",
    );
    // Positive control: the same fixture with the right marker binds the
    // sandbox's own workspace, so the refusals above are the guard.
    std::fs::write(root.join(MARKER), r#"{"name":"acc"}"#).unwrap();
    assert_eq!(
        crate::home::workspace_for_provider_env(&tracker, &env).unwrap(),
        root.join("workspace")
    );
    // No profile at all is the host's own workspace (the contrast).
    let plain = ProviderEnv::default();
    plain.set("HOME", home.to_str().unwrap());
    assert_eq!(
        crate::home::workspace_for_provider_env(&tracker, &plain).unwrap(),
        home.join(".cadence/workspace")
    );
}

/// 1b. A root that is, or sits inside, a live installation is refused even
///     with a marker that names it, and its data is untouched.
#[test]
fn cad1922_accept_daemon_workspace_refuses_a_root_overlapping_a_live_install() {
    let fx = scratch("c22a2");
    let home = fx.path().join("home");
    let tracker = home.join("pm");
    std::fs::create_dir_all(&tracker).unwrap();
    std::fs::write(tracker.join("pm.yaml"), b"live tracker\n").unwrap();
    std::fs::create_dir_all(home.join(".cadence/state")).unwrap();
    let _env = Env::with_home(&home);

    // Base = $HOME on the provider env only: the root is the production
    // tracker `~/pm`, and the planted marker names it.
    let env = provider("sandbox:pm", &home, &home);
    std::fs::write(tracker.join(MARKER), r#"{"name":"pm"}"#).unwrap();
    let before = snapshot(&tracker);
    let text = assert_refused(
        crate::home::workspace_for_provider_env(&tracker, &env),
        &home,
        "root is the production tracker",
        "overlaps",
    );
    assert!(text.contains("production tracker"), "{text}");

    // Base = the production home: the root is inside `~/.cadence`.
    let env = provider("sandbox:inside", &home.join(".cadence"), &home);
    let inside = home.join(".cadence/inside");
    std::fs::create_dir_all(&inside).unwrap();
    std::fs::write(inside.join(MARKER), r#"{"name":"inside"}"#).unwrap();
    let text = assert_refused(
        crate::home::workspace_for_provider_env(&tracker, &env),
        &home,
        "root inside the production home",
        "overlaps",
    );
    assert!(text.contains("production home"), "{text}");

    assert_eq!(snapshot(&tracker), before, "live tracker data changed");
}

/// 1c. A sandbox base set only on the provider env is authoritative: it
///     neither falls back to the process env's base nor to the host.
#[test]
fn cad1922_accept_daemon_workspace_base_comes_from_the_provider_env_only() {
    let fx = scratch("c22a3");
    let home = fx.path().join("home");
    let good = fx.path().join("good");
    let empty = fx.path().join("empty");
    let tracker = fx.path().join("tracker");
    for d in [&home, &good, &empty, &tracker] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::create_dir_all(good.join("acc/state")).unwrap();
    std::fs::write(good.join("acc").join(MARKER), r#"{"name":"acc"}"#).unwrap();
    let _env = Env::with_home(&home);

    // The process env points at the base that HAS the marker; the provider
    // env points at one that has none. The provider env must win: refused.
    std::env::set_var("CADENCE_SANDBOX_ROOT", &good);
    let env = provider("sandbox:acc", &empty, &home);
    assert_refused(
        crate::home::workspace_for_provider_env(&tracker, &env),
        &home,
        "provider base without a marker",
        "refusing to fall back",
    );
    // And the reverse: the process env points nowhere useful, the provider
    // env's base is the one honoured.
    std::env::set_var("CADENCE_SANDBOX_ROOT", &empty);
    let env = provider("sandbox:acc", &good, &home);
    assert_eq!(
        crate::home::workspace_for_provider_env(&tracker, &env).unwrap(),
        good.join("acc/workspace")
    );
    // A relative provider base is refused rather than followed.
    let env = provider("sandbox:acc", Path::new("relative/base"), &home);
    assert_refused(
        crate::home::workspace_for_provider_env(&tracker, &env),
        &home,
        "relative provider base",
        "absolute",
    );
}

/// 2 + 4. A planted marker at a live installation, with the exports set, is
///     refused by the deleting guard; data stays; the STOP warning leads
///     every command; and every printed command parses through the real clap
///     definitions.
#[test]
fn cad1922_accept_deleting_guard_refuses_planted_live_install_and_prints_safe_text() {
    let fx = scratch("c22a2b");
    let home = fx.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _env = Env::with_home(&home);

    // A custom live installation, not at any host default: its state dir is
    // exactly the sandbox's own `<root>/state`, which is the shape an export
    // can claim.
    let root = fx.path().join("live");
    std::fs::create_dir_all(root.join("state")).unwrap();
    std::fs::create_dir_all(root.join("pm")).unwrap();
    std::fs::write(root.join("state/cadence.sqlite3"), b"daemon db").unwrap();
    std::fs::write(root.join("pm/pm.yaml"), b"tracker").unwrap();
    std::fs::write(root.join(MARKER), r#"{"name":"live"}"#).unwrap();
    let sb = Sandbox {
        name: "live".into(),
        base: fx.path().to_path_buf(),
        root: root.clone(),
    };
    // Control: without the exports this root is an ordinary sandbox.
    refuse_production(&sb).unwrap();

    std::env::set_var("CADENCE_STATE_DIR", sb.state_dir());
    std::env::set_var("CADENCE_PM_DIR", sb.pm_dir());
    std::env::set_var("XDG_CONFIG_HOME", sb.config_dir());
    std::env::set_var("CADENCE_PROFILE", sb.profile());
    let before = snapshot(&root);
    let text = refuse_production(&sb).unwrap_err().to_string();
    assert_eq!(snapshot(&root), before, "refusal must not touch data");
    assert!(text.contains("refusing; nothing was changed"), "{text}");

    // The warning leads: STOP precedes every command and every `cadence dev`.
    let stop = text.find("STOP").expect("a STOP warning");
    assert!(text.contains("live installation"), "{text}");
    let first_cmd = ["env -u", "cadence dev", "`"]
        .iter()
        .filter_map(|needle| text.find(needle))
        .min()
        .expect("the refusal offers commands");
    assert!(stop < first_cmd, "STOP must precede any command: {text}");
    let nothing = text.find("run nothing below").expect("a do-not-run line");
    assert!(nothing < first_cmd, "{text}");

    // 4. Every command the text prints parses through the real definitions:
    // `Cli`'s `dev` subcommand is `DevAction` (src/cli/commands.rs).
    #[derive(Parser)]
    #[command(name = "cadence")]
    struct Probe {
        #[command(subcommand)]
        cmd: ProbeCmd,
    }
    #[derive(Subcommand)]
    enum ProbeCmd {
        Dev {
            #[command(subcommand)]
            action: DevAction,
        },
    }
    let spans: Vec<&str> = text
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|span| span.starts_with("env "))
        .collect();
    assert_eq!(spans.len(), 3, "down, reload and reset are offered: {text}");
    let mut seen = [false; 3];
    for span in spans {
        let mut tokens = span.split_whitespace();
        assert_eq!(tokens.next(), Some("env"), "{span}");
        let mut rest: Vec<&str> = tokens.collect();
        while rest.first() == Some(&"-u") {
            rest.drain(..2);
        }
        assert_eq!(&rest[..2], ["cadence", "dev"], "{span}");
        let parsed = Probe::try_parse_from(&rest)
            .unwrap_or_else(|e| panic!("printed command does not parse: {span}\n{e}"));
        match parsed.cmd {
            ProbeCmd::Dev { action } => match action {
                DevAction::Down { name } => {
                    assert_eq!(name.as_deref(), Some("live"));
                    seen[0] = true;
                }
                DevAction::Reload { name, build } => {
                    assert_eq!((name.as_deref(), build), (Some("live"), None));
                    seen[1] = true;
                }
                DevAction::Reset { name } => {
                    assert_eq!(name, "live");
                    seen[2] = true;
                }
                _ => panic!("unexpected verb in {span}"),
            },
        }
    }
    assert_eq!(seen, [true; 3], "{text}");
    // Control that the parser is the real one: the old positional spelling of
    // `reload` is a usage error.
    assert!(Probe::try_parse_from(["cadence", "dev", "reload", "live"]).is_err());

    // A marker planted at a host-derived production path is refused too, and
    // its data stays.
    let tracker = home.join("pm");
    std::fs::create_dir_all(&tracker).unwrap();
    std::fs::write(tracker.join("pm.yaml"), b"prod").unwrap();
    std::fs::write(tracker.join(MARKER), r#"{"name":"pm"}"#).unwrap();
    let planted = Sandbox {
        name: "pm".into(),
        base: home.clone(),
        root: tracker.clone(),
    };
    let before = snapshot(&tracker);
    let text = refuse_production(&planted).unwrap_err().to_string();
    assert!(text.contains("production tracker"), "{text}");
    assert_eq!(snapshot(&tracker), before);
}

/// 3. Layout evidence: stray files beside a legacy install do not disable it;
///    a real consolidated install plus a stray `~/pm` is refused and names
///    the evidence files.
#[test]
fn cad1922_accept_layout_ignores_strays_and_refuses_two_real_installs() {
    use crate::home::{layout, Source};
    let fx = scratch("c22a3b");
    let home = fx.path().join("home");
    let cadence = home.join(".cadence");
    let legacy_state = home.join(".local/state/cadence");
    std::fs::create_dir_all(&legacy_state).unwrap();
    std::fs::write(legacy_state.join("cadence.sqlite3"), b"live legacy").unwrap();
    std::fs::create_dir_all(cadence.join("state")).unwrap();
    std::fs::write(cadence.join("state/overview-gh.json"), b"{}").unwrap();
    std::fs::create_dir_all(cadence.join("wt/lane")).unwrap();
    std::fs::write(cadence.join("wt/lane/work"), b"uncommitted").unwrap();
    let _env = Env::with_home(&home);

    // Strays beside a legacy install: still the legacy install, not an error.
    let chosen = layout().expect("strays must not disable a legacy install");
    assert_eq!(chosen.source, Source::Legacy);
    assert_eq!(crate::home::state_dir().unwrap(), legacy_state);

    // A real consolidated install (tracker + daemon db) and a stray ~/pm,
    // with no legacy state: two installations by evidence.
    std::fs::remove_dir_all(home.join(".local")).unwrap();
    std::fs::create_dir_all(cadence.join("tracker")).unwrap();
    std::fs::write(cadence.join("tracker/pm.yaml"), b"x").unwrap();
    std::fs::write(cadence.join("state/cadence.sqlite3"), b"db").unwrap();
    assert_eq!(layout().unwrap().source, Source::Fresh, "control");
    std::fs::create_dir_all(home.join("pm")).unwrap();
    let before_refusal = snapshot(&home);
    let text = layout().unwrap_err().to_string();
    assert!(text.contains("two Cadence installations"), "{text}");
    assert!(text.contains("nothing was moved or deleted"), "{text}");
    for named in [
        cadence.join("tracker/pm.yaml"),
        cadence.join("state/cadence.sqlite3"),
        home.join("pm"),
    ] {
        assert!(
            text.contains(&named.display().to_string()),
            "{named:?}: {text}"
        );
    }
    assert_eq!(snapshot(&home), before_refusal, "refusal changed data");
}
