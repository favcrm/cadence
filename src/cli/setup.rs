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
//! The checks themselves stay in `cadence_agent::setup`; `--json` and every
//! pipe keep their existing records and never prompt, sign in or call the
//! network — an explicit `--hosted`/`--register` with `--json` is refused
//! before any of that.

use std::io::Write;

use cadence_agent::remote_cli::RemoteTarget;
use cadence_agent::setup::{Handover, PlanFact, SignInFact, LIVE_INSTRUCTION};

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
    let mut sign_in = saved_fact(saved.as_deref());
    let mut target: Option<RemoteTarget> = None;
    let mut sign_in_failed = false;
    let decision = decide(choice, json, &state_dir, &sign_in);
    match decision {
        Decision::LocalOnly => {}
        Decision::SignIn | Decision::Register => match hosted(decision, no_open) {
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
                let mut fact = SignInFact::failed(&error);
                if decision == Decision::Register {
                    // The browser may have created a workspace before the
                    // approval ended. This machine saved no credential, so
                    // nothing here is bound to one and the handover never
                    // claims a workspace it cannot prove.
                    fact.detail.push_str(
                        " — a workspace the browser created, if any, is not bound to this \
                         machine: no credential was saved",
                    );
                }
                sign_in = fact;
                sign_in_failed = true;
            }
        },
    }
    // The scope fact is the scope this run selected, not what the
    // credential store happens to hold: choosing this machine stays local
    // even when a hosted login is saved, and a failed approval bound
    // nothing, so it stays local too. Only this run's verified sign-in
    // makes the scope hosted. A saved or unreadable store is reported as
    // the sign-in fact, where it belongs.
    let scope = if sign_in.state == "verified" {
        cadence_agent::setup::SCOPE_HOSTED
    } else {
        cadence_agent::setup::SCOPE_LOCAL
    };
    // A hosted run — and a run whose requested approval did not complete —
    // reports the daemon and board without starting either. A local run
    // starts them even when a hosted login is saved elsewhere on this
    // machine.
    let start_services =
        decision == Decision::LocalOnly && scope == cadence_agent::setup::SCOPE_LOCAL;
    let (code, outcomes) =
        cadence_agent::setup::cli(&state_dir, port, json, cli_verbs(), start_services)?;
    let handover = Handover {
        scope,
        hosted_board_url: sign_in.endpoint.clone(),
        next_command: next_command(
            scope,
            &sign_in,
            &state_dir,
            &cadence_agent::issue::default_dir()?,
        ),
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
        plan: plan_fact(target.as_ref(), json, saved.as_deref()),
        sign_in,
        instruction: LIVE_INSTRUCTION.to_string(),
    };
    if !json {
        print_handover(&handover);
    }
    emit_scope(&state_dir, json, &handover)?;
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
            let saved_org = org::selected_org_readonly()
                .ok()
                .flatten()
                .is_some_and(|name| !name.is_empty());
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
    org::record_remote(
        &grant.slug,
        &grant.endpoint,
        &grant.organization_id,
        false,
        || grant.save_credential(&dir),
    )?;
    Ok(grant)
}

fn saved_fact(saved: Option<&[String]>) -> SignInFact {
    match saved {
        Some(slugs) => SignInFact::saved(slugs),
        None => SignInFact::unknown("the hosted credential store could not be inspected"),
    }
}

/// The offer is read only when this run verified a hosted sign-in and the
/// output is for a person: `--json` is a machine path that keeps its
/// existing records and never calls the billing API.
fn plan_fact(target: Option<&RemoteTarget>, json: bool, saved: Option<&[String]>) -> PlanFact {
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
            Some(slugs) if !slugs.is_empty() => PlanFact::unknown(
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
