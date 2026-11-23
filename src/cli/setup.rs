//! CAD-535: `cadence setup` — moved verbatim from src/main.rs.
//!
//! CAD-1922: the first run also asks where the work lives (Enter hosted,
//! `1` this machine, `2` a new hosted workspace), signs in at most once
//! through the existing device grant — never a second one, and never a
//! local daemon or board started just to authenticate — reads the
//! server-authored billing offer when it can, and hands over the four
//! facts plus exactly one next command. That one grant also requests the
//! optional owner-only `billing.read` (never `billing.cancel`); a declined
//! or member-narrowed grant still signs in and reports billing unavailable.
//! The checks themselves stay in `cadence_agent::setup`; `--json` keeps its
//! existing records and never prompts, signs in or calls the network — an
//! explicit `--hosted`/`--register` with `--json` is refused before any of
//! that. Without `--json` a pipe is asked the one scope question too (one
//! line on stdin, the question on stderr); `--local`/`--hosted`/`--register`
//! skip the read for automation.

use std::io::Write;

use cadence_agent::remote_cli::RemoteTarget;
use cadence_agent::setup::{Handover, PlanFact, SavedLogins, SignInFact, LIVE_INSTRUCTION};

use super::*;

/// Which first-run route the command line asked for. `Ask` is the
/// interactive default and the only value that may prompt.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ScopeChoice {
    Ask,
    Local,
    Hosted,
    Register,
}

impl ScopeChoice {
    pub(super) fn from_flags(local: bool, hosted: bool, register: bool) -> Self {
        if hosted {
            Self::Hosted
        } else if register {
            Self::Register
        } else if local {
            Self::Local
        } else {
            Self::Ask
        }
    }
}

/// What this run will actually do about sign-in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Decision {
    LocalOnly,
    SignIn,
    Register,
}

pub(super) fn run(
    state_dir: PathBuf,
    json: bool,
    port: Option<u16>,
    no_open: bool,
    choice: ScopeChoice,
) -> Result<i32> {
    run_with_hosted(state_dir, json, port, no_open, choice, hosted)
}

fn run_with_hosted(
    state_dir: PathBuf,
    json: bool,
    port: Option<u16>,
    no_open: bool,
    choice: ScopeChoice,
    hosted_flow: impl FnOnce(Decision, bool) -> Result<cadence_agent::remote_enrollment::LoginGrant>,
) -> Result<i32> {
    // `--json` is the machine contract: it never prompts, signs in or calls
    // the network. An explicit sign-in flag contradicts it, so the pair is
    // refused here — before a credential is read, a grant is opened or any
    // check is applied — instead of silently starting a browser approval a
    // JSON consumer never asked for. `--json` alone stays local.
    if json && matches!(choice, ScopeChoice::Hosted | ScopeChoice::Register) {
        return Err(Error::rejected(
            "`setup --json` never signs in; run setup without --json to approve a hosted \
             login, or drop --hosted/--register to keep the machine records",
        ));
    }
    let saved = cadence_agent::setup::saved_hosted_logins();
    let mut sign_in = saved_fact(saved.as_ref());
    let mut target: Option<RemoteTarget> = None;
    let mut sign_in_failed = false;
    let decision = decide(choice, json, &state_dir, &sign_in);
    match decision {
        Decision::LocalOnly => {}
        Decision::SignIn | Decision::Register => match hosted_flow(decision, no_open) {
            Ok(grant) => {
                sign_in = SignInFact::verified(&grant.slug, &grant.endpoint);
                target = Some(RemoteTarget {
                    identity: None,
                    org: grant.slug,
                    endpoint: grant.endpoint,
                    org_id: grant.organization_id,
                });
            }
            Err(error) => {
                sign_in = failed_sign_in_fact(decision, &error);
                sign_in_failed = true;
            }
        },
    }
    // The scope is what this run can confirm, not what the credential store
    // may hold. Only a verified sign-in selects hosted scope; an unconfirmed
    // sign-in keeps the local handover without claiming no session was issued.
    let scope = if sign_in.state == "verified" {
        cadence_agent::setup::SCOPE_HOSTED
    } else {
        cadence_agent::setup::SCOPE_LOCAL
    };
    // A hosted or unconfirmed sign-in reports the daemon and board without
    // starting either. An explicitly local run may start them.
    let start_services =
        decision == Decision::LocalOnly && scope == cadence_agent::setup::SCOPE_LOCAL;
    let (code, outcomes) =
        cadence_agent::setup::cli(&state_dir, port, json, cli_verbs(), start_services)
            .map_err(|error| preserve_sign_in_outcome(&sign_in, sign_in_failed, error))?;
    let tracker = cadence_agent::issue::default_dir()
        .map_err(|error| preserve_sign_in_outcome(&sign_in, sign_in_failed, error))?;
    let handover = Handover {
        scope,
        hosted_board_url: sign_in.endpoint.clone(),
        next_command: next_command(scope, &sign_in, &state_dir, &tracker),
        board_running: outcomes
            .iter()
            .find(|outcome| outcome.check == "ui")
            .is_some_and(|outcome| {
                matches!(
                    outcome.status,
                    cadence_agent::setup::Status::Ok
                        | cadence_agent::setup::Status::Created
                        | cadence_agent::setup::Status::Started
                )
            }),
        board_url: cadence_agent::setup::board_url(&state_dir, port),
        plan: plan_fact(target.as_ref(), json, saved.as_ref(), sign_in_failed),
        sign_in,
        instruction: LIVE_INSTRUCTION.to_string(),
    };
    if !json {
        print_handover(&handover);
    }
    emit_scope(&state_dir, json, &handover)
        .map_err(|error| preserve_sign_in_outcome(&handover.sign_in, sign_in_failed, error))?;
    Ok(if sign_in_failed { 1 } else { code })
}

/// The scope question is asked only on a real first run: this run's own
/// paths are still absent and no hosted login or saved org exists. A
/// layout marker, a directory a same-uid writer could plant or an
/// existing installation's state never makes setup ask, sign in or switch
/// anything. An unreadable registry or tracker answer counts as present —
/// setup then behaves exactly as it did before this question existed, and
/// never fails because of the probe.
///
/// The answer is read from a terminal *or* a pipe (one line: empty is
/// hosted, `1` this machine, `2` a new workspace); EOF with no line stays
/// local. `--json` never reads the answer, so machine records are
/// unchanged, and `--local`/`--hosted`/`--register` skip the read entirely
/// for automation that must not wait on stdin.
fn decide(choice: ScopeChoice, json: bool, state_dir: &Path, sign_in: &SignInFact) -> Decision {
    match choice {
        ScopeChoice::Local => Decision::LocalOnly,
        ScopeChoice::Hosted => Decision::SignIn,
        ScopeChoice::Register => Decision::Register,
        ScopeChoice::Ask => {
            // An unreadable registry counts as present, as documented above.
            let saved_org = match org::selected_org_readonly() {
                Ok(name) => name.is_some_and(|name| !name.is_empty()),
                Err(_) => true,
            };
            let tracker_present =
                cadence_agent::issue::default_dir().map_or(true, |dir| dir.exists());
            let first_run = sign_in.is_signed_out()
                && !saved_org
                && !state_dir.exists()
                && !tracker_present
                && cadence_agent::sandbox::profile().is_none()
                && std::env::var_os("CADENCE_ALIAS").is_none()
                && std::env::var_os("CADENCE_GRANT_TOKEN").is_none();
            if json || !first_run {
                return Decision::LocalOnly;
            }
            let prompt_on_stdout = std::io::IsTerminal::is_terminal(&std::io::stdin())
                && std::io::IsTerminal::is_terminal(&std::io::stdout());
            ask_scope(prompt_on_stdout)
        }
    }
}

/// One question, three routes, at most one device grant. Enter (or an
/// empty piped line) is hosted sign-in; `1` stays on this machine; `2`
/// creates a hosted workspace. The question goes to stdout for a person
/// at a terminal and to stderr otherwise, so a piped answer never mixes
/// with the run's own text output.
fn ask_scope(prompt_on_stdout: bool) -> Decision {
    for _ in 0..3 {
        if prompt_on_stdout {
            println!("\nCadence first run — where does your work live?");
            println!("  Enter  sign in to hosted AgenticOS (I already have a workspace)");
            println!("  1      stay on this machine — no account, no browser");
            println!("  2      create a hosted workspace in the browser");
            print!("> ");
            let _ = std::io::stdout().flush();
        } else {
            eprintln!(
                "Cadence first run — where does your work live? \
                 Enter hosted sign-in, 1 this machine, 2 a new hosted workspace:"
            );
        }
        let mut answer = String::new();
        match std::io::stdin().read_line(&mut answer) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        match answer.trim() {
            "" => return Decision::SignIn,
            "1" => return Decision::LocalOnly,
            "2" => return Decision::Register,
            _ => continue,
        }
    }
    // An unreadable or unrecognised answer is never a sign-in: staying
    // local writes nothing and opens nothing.
    if prompt_on_stdout {
        println!("staying on this machine — no login, browser or checkout was opened");
    } else {
        eprintln!("staying on this machine — no login, browser or checkout was opened");
    }
    Decision::LocalOnly
}

/// The one hosted sign-in or registration this run may open: the same
/// device grant `cadence login`/`cadence register` use, recorded exactly
/// as they record it, with the optional owner-only `billing.read` added to
/// that one consent so the handover can read the workspace's own offer.
/// `billing.cancel` is never requested, the owner may decline `billing.read`
/// (owner-only scopes start unchecked), and a member receives the narrowed
/// grant and still signs in. It moves no saved default (CAD-1125) and
/// starts no daemon or board.
fn hosted(
    decision: Decision,
    no_open: bool,
) -> Result<cadence_agent::remote_enrollment::LoginGrant> {
    if cadence_agent::sandbox::profile().is_some() {
        return Err(Error::rejected(
            "a dev or sandbox instance never signs in to a hosted workspace; run setup on the \
             installation that should hold the login",
        ));
    }
    if std::env::var_os("CADENCE_ALIAS").is_some()
        || std::env::var_os("CADENCE_GRANT_TOKEN").is_some()
    {
        return Err(Error::rejected(
            "a managed agent cannot sign in to a hosted workspace; the owner runs this from a \
             plain shell",
        ));
    }
    let dir = cadence_agent::remote_auth::auth_dir(None)?;
    let issuer = cadence_agent::remote_enrollment::HOSTED_LOGIN_ISSUER;
    let grant = match decision {
        Decision::Register => cadence_agent::remote_enrollment::register_portal_first_run(
            issuer,
            None,
            None,
            &dir,
            |url, code| super::action_dispatch::show_register_code(url, code, no_open),
        )?,
        _ => cadence_agent::remote_enrollment::login_portal_first_run(
            issuer,
            None,
            &dir,
            |url, code| super::action_dispatch::show_login_code(url, code, no_open),
        )?,
    };
    // The binding and the credential are one recovery unit, as in `login`:
    // a failed credential write restores the previous binding. A first run
    // never moves the saved default, so ambient local stays the default.
    persist_issued_grant(&grant, &dir)?;
    Ok(grant)
}

fn persist_issued_grant(
    grant: &cadence_agent::remote_enrollment::LoginGrant,
    dir: &Path,
) -> Result<()> {
    org::record_remote(
        &grant.slug,
        &grant.endpoint,
        &grant.organization_id,
        false,
        || grant.save_credential(dir),
    )
    .map(|_| ())
    .map_err(|error| {
        super::action_dispatch::issued_session_persistence_error(&grant.organization_id, dir, error)
    })
}

fn failed_sign_in_fact(decision: Decision, error: &Error) -> SignInFact {
    if error.kind() == "unknown" {
        return SignInFact::unknown(error.to_string());
    }
    let mut fact = SignInFact::failed(error);
    if decision == Decision::Register {
        // The browser may have created a workspace before the approval ended.
        fact.detail.push_str(
            " — a workspace the browser created, if any, is not bound to this \
             machine: no credential was saved",
        );
    }
    fact
}

fn preserve_sign_in_outcome(sign_in: &SignInFact, sign_in_failed: bool, error: Error) -> Error {
    if sign_in_failed && sign_in.state == "unknown" {
        return Error::unknown(format!(
            "{}; local setup also failed: {error}",
            sign_in.detail
        ));
    }
    if sign_in.state == "verified" {
        cadence_agent::output::eprint_format_line(format_args!(
            "{}; sign-in remains saved, but local setup failed",
            sign_in.detail
        ));
    }
    error
}

fn saved_fact(saved: Option<&SavedLogins>) -> SignInFact {
    match saved {
        Some(saved) => SignInFact::saved(saved),
        None => SignInFact::unknown("the hosted credential store could not be inspected"),
    }
}

/// The offer is read only when this run verified a hosted sign-in and the
/// output is for a person: `--json` is a machine path that keeps its
/// existing records and never calls the billing API.
fn plan_fact(
    target: Option<&RemoteTarget>,
    json: bool,
    saved: Option<&SavedLogins>,
    approval_failed: bool,
) -> PlanFact {
    // An unconfirmed sign-in may have created or approved a billed workspace.
    // "none" is only for a run that attempted nothing.
    if approval_failed {
        return PlanFact::unknown(
            "not read: this run's hosted sign-in was not confirmed, so it cannot say whether \
             a workspace exists or is billed; the offer stays in the workspace dashboard",
        );
    }
    match (target, json) {
        (Some(target), false) => {
            cadence_agent::setup::plan_fact(&cadence_agent::setup::billing_offer(target))
        }
        (Some(_), true) => PlanFact::unknown(
            "not read: `setup --json` keeps its existing records and never calls the billing API",
        ),
        (None, _) => match saved {
            // The inventory could not be inspected. "none" would be a claim
            // this run cannot make: a workspace may well be billed.
            None => PlanFact::unknown(
                "not read: the hosted credential store could not be inspected, so this run \
                 cannot say whether this machine is billed; the offer stays in the workspace \
                 dashboard",
            ),
            Some(saved) if !saved.is_empty() => PlanFact::unknown(
                "not read: this run verified no hosted sign-in, and a saved login is not \
                 re-read here; the offer stays in the workspace dashboard",
            ),
            Some(_) => PlanFact::none(),
        },
    }
}

/// Exactly one next command, in the scope this run selected. No outcome
/// proposes another approval: the live instruction already says not to
/// open a second login, and after an interruption — a denial, an expiry, a
/// transport failure, a grant that did not match — an approved credential
/// may already exist server-side. The safe next step is always a truthful
/// read of this machine, and only a hosted scope names a workspace.
fn next_command(scope: &str, sign_in: &SignInFact, state_dir: &Path, pm_dir: &Path) -> String {
    // An inherited binding — a pin — refuses `--org` (`org::pin_conflict`),
    // so a pinned shell's next command must read this machine locally and
    // name the exact state dir instead of the workspace.
    let pinned = [
        "CADENCE_STATE_DIR",
        "CADENCE_PM_DIR",
        "CADENCE_HOME",
        "CADENCE_PROFILE",
    ]
    .iter()
    .any(|name| std::env::var_os(name).is_some());
    if !pinned {
        if let (cadence_agent::setup::SCOPE_HOSTED, Some(slug)) = (scope, &sign_in.slug) {
            // A hosted read goes to the workspace through the saved login.
            return format!("cadence status --org {slug}");
        }
    }
    // A local handover pins this run's state dir and tracker, even when
    // they are the defaults: a saved remote org, `CADENCE_ORG`, or a saved
    // local connection with a custom pair would otherwise send a bare
    // `cadence status` to another destination or read the wrong tracker.
    format!(
        "CADENCE_PM_DIR={} cadence status --state-dir {}",
        shell_quote(&pm_dir.display().to_string()),
        shell_quote(&state_dir.display().to_string())
    )
}

/// Single-quote a value for a command the CLI only ever *prints*. `'`
/// becomes `'\''`, so a path with a quote cannot break out of the quotes.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn print_handover(handover: &Handover) {
    println!("\nfirst run");
    println!("         {:<12} {}", "scope", handover.scope);
    println!("         {:<12} {}", "sign-in", handover.sign_in.detail);
    println!("         {:<12} {}", "plan", handover.plan.detail);
    if let Some(url) = &handover.hosted_board_url {
        println!("         {:<12} {url}", "hosted board");
    }
    println!(
        "         {:<12} {}{}",
        "board",
        handover.board_url,
        if handover.board_running {
            ""
        } else {
            " (not running)"
        }
    );
    println!("         {:<12} {}", "next", handover.next_command);
    let mut lines = handover.instruction.lines();
    if let Some(first) = lines.next() {
        println!("         {:<12} {first}", "live");
    }
    for line in lines {
        println!("         {:<12} {line}", "");
    }
}

/// Folder, project, state dir, tracker and org are different things.
/// Reporting them does not create a repo, register a project, or change
/// the saved org. A `cwd/skills/<bundle>` snapshot is named and left as it is.
fn emit_scope(state_dir: &Path, json: bool, handover: &Handover) -> Result<()> {
    let cwd = std::env::current_dir()
        .unwrap_or_else(|error| PathBuf::from(format!("unreadable ({error})")));
    let pm_dir = cadence_agent::issue::default_dir()?;
    let named = std::env::var("CADENCE_PROJECT")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let project = cadence_agent::issue::project::observe(&pm_dir, &cwd, named.as_deref());
    let unmanaged = cadence_agent::skill::unmanaged_snapshots(&cwd);
    let org = match org::selected_org_readonly() {
        Ok(Some(name)) => json!({"selected": name, "changed": false}),
        Ok(None) => json!({"selected": Value::Null, "changed": false, "detail": "no saved org"}),
        Err(error) => json!({
            "selected": Value::Null,
            "changed": false,
            "detail": format!("saved org unreadable: {error}"),
        }),
    };
    let mut scope = json!({
        "check": "scope",
        "status": "ok",
        "detail": "folder, project, state dir, tracker and org are separate",
        "fix": Value::Null,
        "cwd": cwd,
        "project": project,
        "state_dir": state_dir,
        "tracker": pm_dir,
        "org": org,
        "unmanaged_snapshots": unmanaged,
        "changed_org": false,
    });
    // CAD-1922: the four handover facts ride this same record — one added
    // object, no new line and no changed field, so a piped consumer that
    // reads the checks and this row keeps working unchanged.
    scope["handover"] = serde_json::to_value(handover).unwrap_or(Value::Null);
    let scope = if let Ok(home) = super::home_dir() {
        let status = cadence_agent::skill::status(&home);
        let mut scope = scope;
        scope["file_match"] = status["file_match"].clone();
        scope["provider_discovery"] = status["provider_discovery"].clone();
        scope["guidance"] = status["guidance"].clone();
        scope["guidance_attention"] = status["attention"].clone();
        scope["live_session"] = status["live_session"].clone();
        scope
    } else {
        scope
    };
    if json {
        // The checks above print one compact object per line. `print_json`
        // pretty-prints on a terminal or `CADENCE_JSON=pretty`, which would
        // split this added row across lines.
        println!("{}", serde_json::to_string(&scope).unwrap_or_default());
        return Ok(());
    }
    println!(
        "\nscope    cwd          {}",
        scope["cwd"].as_str().unwrap_or("?")
    );
    let project_line = if scope["project"]["resolved"] == true {
        format!(
            "{} ({})",
            scope["project"]["key"].as_str().unwrap_or("?"),
            scope["project"]["source"].as_str().unwrap_or("?")
        )
    } else {
        format!(
            "unresolved — {} Register with `{}`.",
            scope["project"]["detail"].as_str().unwrap_or("no project"),
            scope["project"]["register"]
                .as_str()
                .unwrap_or("cadence issue project add")
        )
    };
    println!("         project      {project_line}");
    println!("         state_dir    {}", state_dir.display());
    println!(
        "         tracker      {}",
        scope["tracker"].as_str().unwrap_or("?")
    );
    let org_line = scope["org"]["selected"]
        .as_str()
        .map(str::to_string)
        .or_else(|| scope["org"]["detail"].as_str().map(str::to_string))
        .unwrap_or_else(|| "no saved org".into());
    println!("         org          {org_line}");
    if let Some(guidance) = scope["guidance"].as_str() {
        println!(
            "         guidance     {guidance} (file match {}, provider discovery {})",
            scope["file_match"].as_str().unwrap_or("not-checked"),
            scope["provider_discovery"]
                .as_str()
                .unwrap_or("not-checked")
        );
    }
    if let Some(note) = scope["guidance_attention"].as_str() {
        println!("         guidance     {note}");
    }
    if let Some(rows) = scope["unmanaged_snapshots"].as_array() {
        for row in rows {
            println!(
                "         unmanaged    {} is not the active install and was left unchanged",
                row["path"].as_str().unwrap_or("?")
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cad1922_failed_approval_never_reports_plan_none() {
        let empty = SavedLogins::default();
        // Nothing attempted and nothing saved: nothing is billed.
        assert_eq!(plan_fact(None, false, Some(&empty), false).state, "none");
        // An approval that did not complete may have created a billed
        // workspace in the browser: unknown, whatever the store holds.
        for saved in [Some(&empty), None] {
            let fact = plan_fact(None, false, saved, true);
            assert_eq!(fact.state, "unknown", "{}", fact.detail);
        }
        // A saved login whose workspace cannot be named is unknown too.
        let unnamed = SavedLogins {
            slugs: Vec::new(),
            unnamed: true,
        };
        assert_eq!(
            plan_fact(None, false, Some(&unnamed), false).state,
            "unknown"
        );
    }

    #[cfg(all(feature = "test-seam", target_os = "linux"))]
    mod issued_session_acceptance {
        use super::super::*;
        use std::fs::{self, File, OpenOptions};
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        use std::os::unix::net::UnixStream;
        use std::process::Command;
        use std::sync::Arc;
        use std::thread;
        use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

        const ISSUER: &str = "https://lc.cadencecloud.app";
        const WORKSPACE: &str = "ws_first_run_issued_2009";
        const SLUG: &str = "lc";
        const SESSION: &str = "hcs_00000000-0000-4000-8000-000000002009";
        const TOKEN: &str = "hct_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
        const SENTINEL: &[u8] = b"protected target must remain unchanged\n";

        struct RestoreEnv(Vec<(&'static str, Option<std::ffi::OsString>)>);

        impl RestoreEnv {
            fn isolated() -> Self {
                Self(
                    [
                        "HOME",
                        "XDG_CONFIG_HOME",
                        "XDG_STATE_HOME",
                        "CADENCE_ALIAS",
                        "CADENCE_ORG",
                        "CADENCE_STATE_DIR",
                        "CADENCE_PM_DIR",
                        "CADENCE_HOME",
                        "CADENCE_PROFILE",
                        "CADENCE_SOCKET",
                        "CADENCE_TEST_TLS_CA",
                        "CADENCE_TEST_TLS_CONNECT",
                        "CADENCE_GRANT_TOKEN",
                    ]
                    .into_iter()
                    .map(|name| {
                        let previous = std::env::var_os(name);
                        std::env::remove_var(name);
                        (name, previous)
                    })
                    .collect(),
                )
            }
        }

        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                for (name, previous) in self.0.drain(..) {
                    match previous {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }

        fn private_file(path: &Path, bytes: &[u8]) {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .unwrap();
            file.write_all(bytes).unwrap();
        }

        fn fixture_tls(root: &Path) -> Arc<rustls::ServerConfig> {
            let openssl = |args: &[&str]| {
                let mut command = Command::new("openssl");
                command.current_dir(root).args(args);
                let result = cadence_agent::reaper::output(&mut command).unwrap();
                assert!(
                    result.status.success(),
                    "PKI fixture: {}",
                    String::from_utf8_lossy(&result.stderr)
                );
            };
            openssl(&[
                "req",
                "-x509",
                "-newkey",
                "ec",
                "-pkeyopt",
                "ec_paramgen_curve:P-256",
                "-nodes",
                "-keyout",
                "ca.key",
                "-out",
                "ca.pem",
                "-days",
                "2",
                "-subj",
                "/CN=CAD-2009 first-run CA",
                "-addext",
                "basicConstraints=critical,CA:TRUE",
                "-addext",
                "keyUsage=critical,keyCertSign,cRLSign",
            ]);
            openssl(&[
                "req",
                "-new",
                "-newkey",
                "ec",
                "-pkeyopt",
                "ec_paramgen_curve:P-256",
                "-nodes",
                "-keyout",
                "leaf.key",
                "-out",
                "leaf.csr",
                "-subj",
                "/CN=lc.cadencecloud.app",
            ]);
            private_file(&root.join("leaf.ext"),
                b"subjectAltName=DNS:lc.cadencecloud.app\nbasicConstraints=CA:FALSE\nkeyUsage=digitalSignature\nextendedKeyUsage=serverAuth\n");
            openssl(&[
                "x509",
                "-req",
                "-in",
                "leaf.csr",
                "-CA",
                "ca.pem",
                "-CAkey",
                "ca.key",
                "-CAcreateserial",
                "-days",
                "2",
                "-extfile",
                "leaf.ext",
                "-out",
                "leaf.pem",
            ]);
            for name in ["ca.pem", "ca.key", "leaf.key"] {
                fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o600)).unwrap();
            }
            let certs = rustls_pemfile::certs(&mut std::io::BufReader::new(
                File::open(root.join("leaf.pem")).unwrap(),
            ))
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
            let key = rustls_pemfile::private_key(&mut std::io::BufReader::new(
                File::open(root.join("leaf.key")).unwrap(),
            ))
            .unwrap()
            .unwrap();
            Arc::new(
                rustls::ServerConfig::builder_with_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .unwrap(),
            )
        }

        struct Issuer {
            stop: UnixStream,
            worker: Option<thread::JoinHandle<Vec<String>>>,
        }

        impl Issuer {
            fn start(listener: TcpListener, tls: Arc<rustls::ServerConfig>) -> Self {
                listener.set_nonblocking(true).unwrap();
                let (stop, wake) = UnixStream::pair().unwrap();
                let worker = thread::spawn(move || {
                    let deadline = Instant::now() + Duration::from_secs(30);
                    let mut requests = Vec::new();
                    loop {
                        let mut descriptors = [
                            libc::pollfd {
                                fd: listener.as_raw_fd(),
                                events: libc::POLLIN,
                                revents: 0,
                            },
                            libc::pollfd {
                                fd: wake.as_raw_fd(),
                                events: libc::POLLIN,
                                revents: 0,
                            },
                        ];
                        let remaining = deadline
                            .saturating_duration_since(Instant::now())
                            .as_millis();
                        assert!(remaining > 0, "first-run issuer deadline");
                        // SAFETY: the array contains exactly two initialized pollfd values.
                        let ready = unsafe {
                            libc::poll(descriptors.as_mut_ptr(), 2, remaining.min(30_000) as i32)
                        };
                        assert!(ready > 0, "first-run issuer did not terminate");
                        if descriptors[0].revents & libc::POLLIN != 0 {
                            let (tcp, _) = listener.accept().unwrap();
                            tcp.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                            tcp.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
                            let connection =
                                rustls::ServerConnection::new(Arc::clone(&tls)).unwrap();
                            let mut stream = rustls::StreamOwned::new(connection, tcp);
                            let mut header = Vec::new();
                            while !header.ends_with(b"\r\n\r\n") {
                                assert!(header.len() < 16_384);
                                let mut byte = [0];
                                stream.read_exact(&mut byte).unwrap();
                                header.push(byte[0]);
                            }
                            let header = String::from_utf8(header).unwrap();
                            let mut lines = header.split("\r\n");
                            let path = lines
                                .next()
                                .unwrap()
                                .strip_prefix("POST ")
                                .unwrap()
                                .strip_suffix(" HTTP/1.1")
                                .unwrap()
                                .to_owned();
                            let length = lines
                                .filter_map(|line| line.split_once(':'))
                                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                                .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                                .unwrap();
                            assert!(length <= 65_536);
                            let mut bytes = vec![0; length];
                            stream.read_exact(&mut bytes).unwrap();
                            let body: Value = serde_json::from_slice(&bytes).unwrap();
                            requests.push(path.clone());
                            let reply = match (requests.len(), path.as_str()) {
                                (1, "/v1/hosted-cadence/device/code") => {
                                    assert_eq!(body["cli_session"], "renewable");
                                    assert!(body.get("organization_id").is_none());
                                    assert_eq!(body["requested_capabilities"], json!([
                                        "cli.read", "cli.write", "wiki.read", "wiki.write", "billing.read"
                                    ]));
                                    json!({
                                        "version":"hosted-cadence-device.v1",
                                        "device_code":format!("hcd_{}", "D".repeat(43)),
                                        "user_code":"K7PM-2QNF",
                                        "verification_uri":format!("{ISSUER}/device/hosted-cadence"),
                                        "verification_uri_complete":format!("{ISSUER}/device/hosted-cadence?code=K7PM-2QNF"),
                                        "expires_in":600,"interval":5
                                    })
                                }
                                (2, "/v1/hosted-cadence/device/token") => {
                                    assert_eq!(body["device_code"], format!("hcd_{}", "D".repeat(43)));
                                    assert!(body["code_verifier"].as_str().is_some_and(|value| value.len() == 43));
                                    let at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
                                    json!({
                                        "version":"hosted-cadence-auth.v1",
                                        "organization_id":WORKSPACE,"organization_slug":SLUG,
                                        "audience":ISSUER,
                                        "principal":{"kind":"user","subject_id":"user_first_run_issued","current_role":"owner"},
                                        "capabilities":["cli.read","cli.write"],
                                        "credential":{"credential_id":"hcp_first_run_issued","access_token":TOKEN,
                                            "token_type":"Bearer","issued_at":at,"expires_at":at+120,"renewal":"reexchange"},
                                        "cli_session":{"session_id":SESSION,"refresh_token":format!("hcr_{}", "R".repeat(43)),
                                            "generation":1,"device_label":"cadence-cli",
                                            "idle_expires_at":at+2_592_000,"absolute_expires_at":at+7_776_000}
                                    })
                                }
                                _ => panic!("second consent, token replay, or unexpected operation: {requests:?}"),
                            };
                            let bytes = serde_json::to_vec(&reply).unwrap();
                            write!(stream, "HTTP/1.1 200 Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
                            stream.write_all(&bytes).unwrap();
                            stream.flush().unwrap();
                        } else if descriptors[1].revents != 0 {
                            break;
                        } else {
                            panic!("unexpected first-run issuer readiness");
                        }
                    }
                    requests
                });
                Self {
                    stop,
                    worker: Some(worker),
                }
            }

            fn finish(mut self) -> Vec<String> {
                self.stop.write_all(b"x").unwrap();
                self.worker.take().unwrap().join().unwrap()
            }
        }

        impl Drop for Issuer {
            fn drop(&mut self) {
                if let Some(worker) = self.worker.take() {
                    let _ = self.stop.write_all(b"x");
                    let _ = worker.join();
                }
            }
        }

        #[test]
        fn issued_setup_session_save_refusal_retains_uncertainty_and_rollback() {
            let _lock = crate::cli::org::tests::RESOLVE_ENV
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let _env = RestoreEnv::isolated();
            let root = tempfile::tempdir().unwrap();
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
            let config = root.path().join("config/cadence");
            let authority = config.join("remote-auth");
            fs::create_dir_all(&authority).unwrap();
            fs::set_permissions(&authority, fs::Permissions::from_mode(0o700)).unwrap();
            std::env::set_var("HOME", root.path());
            std::env::set_var("XDG_CONFIG_HOME", root.path().join("config"));
            std::env::set_var("XDG_STATE_HOME", root.path().join("state"));
            private_file(
                &authority.join("trusted-issuer"),
                format!("{ISSUER}\n").as_bytes(),
            );
            let registry = config.join("orgs.json");
            let previous = json!({
                "selected":{"org":SLUG},
                "connections":[{"selection":{"org":SLUG},"destination":{
                    "mode":"remote","endpoint":ISSUER,"org_id":"ws_previous_2009"
                }}]
            });
            // Compact original bytes make rollback equality stronger than a parsed comparison.
            let previous_bytes = serde_json::to_vec(&previous).unwrap();
            private_file(&registry, &previous_bytes);
            let sentinel = root.path().join("protected-target");
            private_file(&sentinel, SENTINEL);
            let protected = authority.join(format!("cli-{SLUG}.json"));
            std::os::unix::fs::symlink(&sentinel, &protected).unwrap();

            let tls = fixture_tls(root.path());
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = Issuer::start(listener, tls);
            std::env::set_var("CADENCE_TEST_TLS_CA", root.path().join("ca.pem"));
            std::env::set_var("CADENCE_TEST_TLS_CONNECT", address.to_string());
            let approvals = std::cell::Cell::new(0);
            let grant = cadence_agent::remote_enrollment::login_portal_first_run(
                ISSUER,
                None,
                &authority,
                |url, code| {
                    approvals.set(approvals.get() + 1);
                    assert!(url.starts_with(ISSUER));
                    assert_eq!(code, "K7PM-2QNF");
                    Ok(())
                },
            )
            .expect("the synthetic session must actually be issued before testing its save");
            assert_eq!(grant.organization_id, WORKSPACE);
            assert_eq!(grant.slug, SLUG);
            assert_eq!(grant.endpoint, ISSUER);

            for decision in [Decision::SignIn, Decision::Register] {
                let error = persist_issued_grant(&grant, &authority)
                    .expect_err("protected credential save must refuse after issuance");
                assert_eq!(error.kind(), "unknown", "{error}");
                assert_eq!(
                    fs::read(&registry).unwrap(),
                    previous_bytes,
                    "the failed commit must restore the registry byte-for-byte"
                );
                assert!(fs::symlink_metadata(&protected)
                    .unwrap()
                    .file_type()
                    .is_symlink());
                assert_eq!(fs::read_link(&protected).unwrap(), sentinel);
                assert_eq!(fs::read(&sentinel).unwrap(), SENTINEL);
                let fact = failed_sign_in_fact(decision, &error);
                assert_eq!(fact.state, "unknown", "{fact:?}");
                let serialized = serde_json::to_string(&fact).unwrap();
                for rendered in [error.to_string(), fact.detail.clone(), serialized] {
                    let lower = rendered.to_lowercase();
                    assert!(
                        rendered.contains(WORKSPACE),
                        "missing actual workspace: {rendered}"
                    );
                    assert!(
                        rendered.contains(authority.to_str().unwrap()),
                        "missing original authority: {rendered}"
                    );
                    assert!(
                        lower.contains("issued"),
                        "missing issued-session fact: {rendered}"
                    );
                    assert!(
                        lower.contains("reconcil"),
                        "missing reconciliation instruction: {rendered}"
                    );
                    assert!(
                        !lower.contains("approval did not complete"),
                        "false approval failure: {rendered}"
                    );
                    assert!(
                        !lower.contains("no credential was saved")
                            && !lower.contains("no credential saved"),
                        "false absence claim: {rendered}"
                    );
                    assert!(!rendered.contains(TOKEN));
                    assert!(!rendered.contains(&format!("hcr_{}", "R".repeat(43))));
                }
            }

            // Valid control: persist this already-issued synthetic session locally after
            // removing only our fixture symlink, without another grant or consent.
            fs::remove_file(&protected).unwrap();
            persist_issued_grant(&grant, &authority).unwrap();
            assert_eq!(fs::read(&sentinel).unwrap(), SENTINEL);
            let metadata = fs::symlink_metadata(&protected).unwrap();
            assert!(metadata.is_file() && !metadata.file_type().is_symlink());
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            let saved: Value = serde_json::from_slice(&fs::read(&protected).unwrap()).unwrap();
            assert_eq!(saved["organization_id"], WORKSPACE);
            assert_eq!(saved["cli_session"]["session_id"], SESSION);
            let stored: Value = serde_json::from_slice(&fs::read(&registry).unwrap()).unwrap();
            assert_eq!(
                stored["selected"], previous["selected"],
                "setup must not switch defaults"
            );
            assert_eq!(stored["connections"][0]["destination"]["org_id"], WORKSPACE);
            let verified = SignInFact::verified(&grant.slug, &grant.endpoint);
            assert_eq!(verified.state, "verified");
            assert_eq!(
                approvals.get(),
                1,
                "save/reconciliation must not open another approval"
            );
            assert_eq!(
                server.finish(),
                [
                    "/v1/hosted-cadence/device/code",
                    "/v1/hosted-cadence/device/token",
                ],
                "no second grant, token replay, renewal, or revoke is allowed"
            );
        }

        // CAD-2009: independently authored post-issuance orchestration acceptance.
        #[test]
        fn issued_setup_session_unknown_survives_later_missing_home() {
            let _lock = crate::cli::org::tests::RESOLVE_ENV
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let _env = RestoreEnv::isolated();
            let root = tempfile::tempdir().unwrap();
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
            let config = root.path().join("config/cadence");
            let authority = config.join("remote-auth");
            fs::create_dir_all(&authority).unwrap();
            fs::set_permissions(&authority, fs::Permissions::from_mode(0o700)).unwrap();
            std::env::set_var("HOME", root.path());
            std::env::set_var("XDG_CONFIG_HOME", root.path().join("config"));
            std::env::set_var("XDG_STATE_HOME", root.path().join("legacy-state"));
            private_file(
                &authority.join("trusted-issuer"),
                format!("{ISSUER}\n").as_bytes(),
            );
            let registry = config.join("orgs.json");
            let previous_bytes = serde_json::to_vec(&json!({
                "selected": {"org": SLUG},
                "connections": [{
                    "selection": {"org": SLUG},
                    "destination": {
                        "mode": "remote",
                        "endpoint": ISSUER,
                        "org_id": "ws_previous_2009"
                    }
                }]
            }))
            .unwrap();
            private_file(&registry, &previous_bytes);
            let sentinel = root.path().join("protected-target");
            private_file(&sentinel, SENTINEL);
            let protected = authority.join(format!("cli-{SLUG}.json"));
            std::os::unix::fs::symlink(&sentinel, &protected).unwrap();

            let tls = fixture_tls(root.path());
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = Issuer::start(listener, tls);
            std::env::set_var("CADENCE_TEST_TLS_CA", root.path().join("ca.pem"));
            std::env::set_var("CADENCE_TEST_TLS_CONNECT", address.to_string());
            let approvals = std::cell::Cell::new(0);
            let grant = cadence_agent::remote_enrollment::login_portal_first_run(
                ISSUER,
                None,
                &authority,
                |url, code| {
                    approvals.set(approvals.get() + 1);
                    assert!(url.starts_with(ISSUER));
                    assert_eq!(code, "K7PM-2QNF");
                    Ok(())
                },
            )
            .expect("the actual synthetic issuer must issue the session first");
            assert_eq!(grant.organization_id, WORKSPACE);
            assert_eq!(grant.slug, SLUG);
            assert_eq!(grant.endpoint, ISSUER);

            let state = root.path().join("runtime-state");
            let tracker = root.path().join("tracker");
            std::env::set_var("CADENCE_STATE_DIR", &state);
            std::env::set_var("CADENCE_PM_DIR", &tracker);
            for (choice, expected_decision) in [
                (ScopeChoice::Hosted, Decision::SignIn),
                (ScopeChoice::Register, Decision::Register),
            ] {
                // Obtain the failure from the real save guard, not a constructed error.
                std::env::set_var("HOME", root.path());
                let issued_error = persist_issued_grant(&grant, &authority)
                    .expect_err("the real protected destination must refuse persistence");
                assert_eq!(issued_error.kind(), "unknown", "{issued_error}");
                assert_eq!(fs::read(&registry).unwrap(), previous_bytes);
                assert_eq!(fs::read_link(&protected).unwrap(), sentinel);
                assert_eq!(fs::read(&sentinel).unwrap(), SENTINEL);

                // The grant outcome is already unknown before this independent local
                // prerequisite fails. Keep absolute XDG and explicit fixture roots.
                std::env::remove_var("HOME");
                let hosted_calls = std::cell::Cell::new(0);
                let result = run_with_hosted(
                    state.clone(),
                    false,
                    None,
                    true,
                    choice,
                    |decision, no_open| {
                        hosted_calls.set(hosted_calls.get() + 1);
                        assert!(decision == expected_decision);
                        assert!(no_open);
                        Err(issued_error)
                    },
                );
                let error = result.expect_err(
                    "post-grant setup failure must retain the issued-session unknown outcome",
                );
                assert_eq!(
                    hosted_calls.get(),
                    1,
                    "orchestration retried hosted sign-in"
                );
                assert_eq!(error.kind(), "unknown", "{error}");
                assert_eq!(cadence_agent::error::exit_code_for_kind(error.kind()), 1);
                let rendered = error.to_string();
                assert!(rendered.contains(WORKSPACE), "{rendered}");
                assert!(rendered.contains(authority.to_str().unwrap()), "{rendered}");
                let lower = rendered.to_lowercase();
                assert!(lower.contains("issued"), "{rendered}");
                assert!(lower.contains("reconcil"), "{rendered}");
                assert!(!lower.contains("approval did not complete"), "{rendered}");
                assert!(!lower.contains("no credential was saved"), "{rendered}");
                assert!(!lower.contains("no credential saved"), "{rendered}");
                assert!(!rendered.contains(TOKEN));
                assert!(!rendered.contains(&format!("hcr_{}", "R".repeat(43))));
                assert_eq!(fs::read(&registry).unwrap(), previous_bytes);
                assert!(fs::symlink_metadata(&protected)
                    .unwrap()
                    .file_type()
                    .is_symlink());
                assert_eq!(fs::read_link(&protected).unwrap(), sentinel);
                assert_eq!(fs::read(&sentinel).unwrap(), SENTINEL);
                assert!(!state.exists(), "failed prerequisite must not create state");
                assert!(
                    !tracker.exists(),
                    "failed prerequisite must not create a tracker"
                );
                assert_eq!(approvals.get(), 1, "orchestration opened fresh consent");
            }
            assert_eq!(
                server.finish(),
                [
                    "/v1/hosted-cadence/device/code",
                    "/v1/hosted-cadence/device/token",
                ],
                "post-grant failure must not request consent, replay, renew or revoke"
            );
        }

        // CAD-2009: independently authored confirmed-sign-in aftermath acceptance.
        #[test]
        fn confirmed_setup_session_remains_saved_after_local_failure() {
            const CHILD: &str = "CAD2009_CONFIRMED_SETUP_AFTERMATH_CHILD";
            const DONE: &str = "CAD2009_CONFIRMED_SETUP_AFTERMATH_EXERCISED";
            const TEST: &str = "cli::setup::tests::issued_session_acceptance::confirmed_setup_session_remains_saved_after_local_failure";

            if std::env::var_os(CHILD).is_none() {
                let mut command = Command::new("timeout");
                command
                    .args(["--kill-after=2s", "30s"])
                    .arg(std::env::current_exe().unwrap())
                    .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
                    .env_clear()
                    .env(CHILD, "1")
                    .env("PATH", "/usr/bin:/bin")
                    .stdin(std::process::Stdio::null());
                let output = cadence_agent::reaper::output(&mut command).unwrap();
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(
                    output.status.success(),
                    "confirmed-session child failed: stdout={stdout}; stderr={stderr}"
                );
                assert!(stdout.contains(DONE), "exact child test did not execute");
                let diagnostic = stderr
                    .lines()
                    .find(|line| {
                        let lower = line.to_lowercase();
                        line.contains(SLUG) && line.contains(ISSUER) && lower.contains("saved")
                    })
                    .unwrap_or_else(|| {
                        panic!("missing separate confirmed-sign-in stderr diagnostic: {stderr}")
                    });
                let lower = diagnostic.to_lowercase();
                assert!(
                    lower.contains("verified") || lower.contains("confirmed"),
                    "diagnostic does not confirm sign-in: {diagnostic}"
                );
                assert!(
                    lower.contains("sign-in") || lower.contains("signed in"),
                    "diagnostic does not identify sign-in: {diagnostic}"
                );
                assert!(
                    lower.contains("remains") || lower.contains("still"),
                    "diagnostic does not say the saved sign-in survives: {diagnostic}"
                );
                assert!(!lower.contains("approval did not complete"), "{diagnostic}");
                assert!(!lower.contains("no credential was saved"), "{diagnostic}");
                assert!(!lower.contains("no credential saved"), "{diagnostic}");
                assert!(!lower.contains("outcome is unknown"), "{diagnostic}");
                assert!(!lower.contains("outcome unknown"), "{diagnostic}");
                assert!(!stdout.contains(TOKEN) && !stderr.contains(TOKEN));
                let refresh = format!("hcr_{}", "R".repeat(43));
                assert!(!stdout.contains(&refresh) && !stderr.contains(&refresh));
                return;
            }

            let _lock = crate::cli::org::tests::RESOLVE_ENV
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let _env = RestoreEnv::isolated();
            let root = tempfile::tempdir().unwrap();
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
            let config = root.path().join("config/cadence");
            let authority = config.join("remote-auth");
            fs::create_dir_all(&authority).unwrap();
            fs::set_permissions(&authority, fs::Permissions::from_mode(0o700)).unwrap();
            std::env::set_var("HOME", root.path());
            std::env::set_var("XDG_CONFIG_HOME", root.path().join("config"));
            std::env::set_var("XDG_STATE_HOME", root.path().join("legacy-state"));
            private_file(
                &authority.join("trusted-issuer"),
                format!("{ISSUER}\n").as_bytes(),
            );
            let registry = config.join("orgs.json");
            let previous = json!({
                "selected": {"org": SLUG},
                "connections": [{
                    "selection": {"org": SLUG},
                    "destination": {
                        "mode": "remote",
                        "endpoint": ISSUER,
                        "org_id": "ws_previous_2009"
                    }
                }]
            });
            private_file(&registry, &serde_json::to_vec(&previous).unwrap());
            let tls = fixture_tls(root.path());
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = Issuer::start(listener, tls);
            std::env::set_var("CADENCE_TEST_TLS_CA", root.path().join("ca.pem"));
            std::env::set_var("CADENCE_TEST_TLS_CONNECT", address.to_string());
            let approvals = std::cell::Cell::new(0);
            let grant = cadence_agent::remote_enrollment::login_portal_first_run(
                ISSUER,
                None,
                &authority,
                |url, code| {
                    approvals.set(approvals.get() + 1);
                    assert!(url.starts_with(ISSUER));
                    assert_eq!(code, "K7PM-2QNF");
                    Ok(())
                },
            )
            .expect("the real synthetic issuer must issue the grant");
            assert_eq!(grant.organization_id, WORKSPACE);
            assert_eq!(grant.slug, SLUG);
            assert_eq!(grant.endpoint, ISSUER);
            persist_issued_grant(&grant, &authority)
                .expect("real credential and registry guards must confirm persistence");
            let credential = authority.join(format!("cli-{SLUG}.json"));
            let metadata = fs::symlink_metadata(&credential).unwrap();
            assert!(metadata.is_file() && !metadata.file_type().is_symlink());
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            let credential_before = fs::read(&credential).unwrap();
            let saved: Value = serde_json::from_slice(&credential_before).unwrap();
            assert_eq!(saved["organization_id"], WORKSPACE);
            assert_eq!(saved["cli_session"]["session_id"], SESSION);
            let registry_before = fs::read(&registry).unwrap();
            let stored: Value = serde_json::from_slice(&registry_before).unwrap();
            assert_eq!(stored["selected"], previous["selected"]);
            assert_eq!(stored["connections"][0]["destination"]["org_id"], WORKSPACE);

            let state = root.path().join("runtime-state");
            let tracker = root.path().join("tracker");
            std::env::set_var("CADENCE_STATE_DIR", &state);
            std::env::set_var("CADENCE_PM_DIR", &tracker);
            std::env::remove_var("HOME");
            let original = match cadence_agent::setup::cli(&state, None, false, cli_verbs(), false)
            {
                Err(error) => error,
                Ok(_) => panic!("the same-environment local setup prerequisite must fail"),
            };
            let hosted_calls = std::cell::Cell::new(0);
            let result = run_with_hosted(
                state.clone(),
                false,
                None,
                true,
                ScopeChoice::Hosted,
                |decision, no_open| {
                    hosted_calls.set(hosted_calls.get() + 1);
                    assert!(decision == Decision::SignIn);
                    assert!(no_open);
                    Ok(grant)
                },
            );
            let error = result.expect_err("missing HOME must fail local setup after sign-in");
            assert_eq!(hosted_calls.get(), 1);
            assert_eq!(error.kind(), original.kind(), "local error kind changed");
            assert_eq!(error.code(), original.code(), "local error code changed");
            assert_eq!(
                cadence_agent::error::exit_code_for_kind(error.kind()),
                cadence_agent::error::exit_code_for_kind(original.kind()),
                "local error exit mapping changed"
            );
            assert_eq!(cadence_agent::error::exit_code_for_kind(error.kind()), 3);
            assert_eq!(
                error.to_string(),
                original.to_string(),
                "confirmed-sign-in context belongs in a separate diagnostic, not a rewritten error"
            );
            assert_eq!(fs::read(&credential).unwrap(), credential_before);
            assert_eq!(fs::read(&registry).unwrap(), registry_before);
            assert!(!state.exists());
            assert!(!tracker.exists());
            assert_eq!(approvals.get(), 1, "local failure requested fresh consent");
            assert_eq!(
                server.finish(),
                [
                    "/v1/hosted-cadence/device/code",
                    "/v1/hosted-cadence/device/token",
                ],
                "local failure must not replay, renew, revoke or request another grant"
            );
            println!("{DONE}");
        }
    }
}
