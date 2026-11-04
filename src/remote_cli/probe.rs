//! CAD-2111: the checks behind `cadence doctor` on a hosted org.
//!
//! Answers one question for whoever holds this login: does this connection
//! work, and if not, what should they do? It sends only `authorize` and one
//! read. The authorize answer is classified by [`decode_refusal`], the same
//! decoder every other hosted verb uses, so doctor cannot disagree with the
//! refusal a real command would print. A credential, an envelope or a raw
//! response body never reaches a [`Check`].

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{
    auth_dir, authorize_answer, decode_refusal, envelope_of, local_status_at, post,
    refusal_correlation, renewed_credential, request_timeout, Credential, Refusal, RemoteTarget,
    Surface, AUTH_TIMEOUT, CALL_TIMEOUT,
};

/// How long the whole live pass may take. Doctor reports a slow service; it
/// does not wait one out.
const BUDGET: Duration = Duration::from_secs(60);
/// The authorize probe's envelope is used at once or dropped.
const PROBE_TTL: u64 = 60;
/// A read every workspace can answer; `cli_requested_scopes` gives it `cli.read`.
const READ_VERB: &str = "project_ls";
const REPORT_SCHEMA: u64 = 1;

/// Optional access a login asks for by flag. Absence is a choice, not a fault.
const OPTIONAL_ACCESS: &[(&str, &[&str], &str)] = &[
    ("wiki", &["wiki.read", "wiki.write"], ""),
    ("web publishing", &["web.read", "web.prepare"], " --web"),
    (
        "settings",
        &["settings.read", "settings.write"],
        " --settings",
    ),
    (
        "cloud agents",
        &crate::remote_enrollment::CLOUD_AGENT_LOGIN_SCOPES,
        " --cloud-agent",
    ),
    ("merge approval", &["audit.approve"], " --audit-approve"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Ok,
    Skipped,
    Unknown,
    Warn,
    Fail,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Skipped => "skipped",
            Status::Unknown => "unknown",
            Status::Warn => "warn",
            Status::Fail => "fail",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Check {
    pub id: &'static str,
    pub status: Status,
    pub summary: String,
    pub remedy: Option<String>,
}

impl Check {
    pub fn ok(id: &'static str, summary: impl Into<String>) -> Self {
        Self {
            id,
            status: Status::Ok,
            summary: summary.into(),
            remedy: None,
        }
    }

    fn skipped(id: &'static str, summary: impl Into<String>) -> Self {
        Self {
            status: Status::Skipped,
            ..Self::ok(id, summary)
        }
    }

    fn with(id: &'static str, status: Status, summary: String, remedy: String) -> Self {
        Self {
            id,
            status,
            summary,
            remedy: Some(remedy),
        }
    }
}

fn login(target: &RemoteTarget) -> String {
    format!("run `cadence login {}`", target.org)
}

/// The hosted checks for `target`, in order. A check that needs an earlier
/// one to pass reports `skipped` rather than a second copy of its failure.
pub fn run(target: &RemoteTarget, offline: bool) -> Vec<Check> {
    match auth_dir(None) {
        Ok(dir) => checks_at(&dir, target, offline),
        Err(_) => vec![Check::with(
            "credential",
            Status::Fail,
            "the credential directory cannot be located".into(),
            "set HOME or XDG_CONFIG_HOME".into(),
        )],
    }
}

pub(super) fn checks_at(dir: &Path, target: &RemoteTarget, offline: bool) -> Vec<Check> {
    let status = local_status_at(dir, target);
    let credential = credential_check(&status, target);
    if credential.status != Status::Ok {
        return vec![
            credential,
            Check::skipped("auth", "needs a usable saved login"),
            Check::skipped("read", "needs a usable saved login"),
            Check::skipped("wiki", "needs a usable saved login"),
            Check::skipped("scopes", "needs a usable saved login"),
        ];
    }
    let scopes: Vec<&str> = status["scopes"]
        .as_array()
        .map(|values| values.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut checks = vec![credential];
    if offline {
        for id in ["auth", "read", "wiki"] {
            checks.push(Check::skipped(id, "--offline: not checked"));
        }
    } else {
        checks.extend(live_checks(dir, target, &scopes));
    }
    checks.push(scopes_check(&scopes, target));
    checks
}

fn credential_check(status: &Value, target: &RemoteTarget) -> Check {
    match status["state"].as_str().unwrap_or_default() {
        "renewable" => Check::ok(
            "credential",
            "a renewable login is saved; this does not prove the workspace accepts it",
        ),
        "access_valid_until_expiry" => Check::ok(
            "credential",
            "a login is saved until its access expires; it cannot renew itself",
        ),
        "no_credential" => Check::with(
            "credential",
            Status::Fail,
            format!("this machine has no login for org '{}'", target.org),
            login(target),
        ),
        "reauthentication_required" => Check::with(
            "credential",
            Status::Fail,
            "the saved login has expired and cannot renew".into(),
            login(target),
        ),
        "invalid_local_credential" => Check::with(
            "credential",
            Status::Fail,
            "the saved login cannot be read".into(),
            login(target),
        ),
        _ => Check::with(
            "credential",
            Status::Fail,
            "the credential store cannot be inspected".into(),
            "check the permissions of the Cadence credential directory".into(),
        ),
    }
}

fn live_checks(dir: &Path, target: &RemoteTarget, scopes: &[&str]) -> Vec<Check> {
    let Some(deadline) = Instant::now().checked_add(BUDGET) else {
        return Vec::new();
    };
    let cred = match renewed_credential(dir, target, deadline) {
        Ok(cred) => cred,
        // Renewal's own text already names the next step, and it separates a
        // session the issuer rejected from one that is only not renewed yet.
        Err(error) => {
            return vec![
                Check::with(
                    "auth",
                    Status::Fail,
                    "the saved login could not be renewed".into(),
                    error.to_string(),
                ),
                Check::skipped("read", "needs a working login"),
                Check::skipped("wiki", "needs a working login"),
            ];
        }
    };
    let envelope = match authorize("auth", target, &cred, Surface::Cli, "cli.read", deadline) {
        Ok(envelope) => envelope,
        Err(check) => {
            return vec![
                check,
                Check::skipped("read", "needs a working login"),
                Check::skipped("wiki", "needs a working login"),
            ];
        }
    };
    vec![
        Check::ok(
            "auth",
            "the workspace accepted this login and its membership",
        ),
        read_check(target, &cred, &envelope, deadline),
        wiki_check(target, &cred, scopes, deadline),
    ]
}

/// One authorize on `surface`. On refusal, the finished check instead of an
/// error a caller could flatten into a string.
fn authorize(
    id: &'static str,
    target: &RemoteTarget,
    cred: &Credential,
    surface: Surface,
    scope: &str,
    deadline: Instant,
) -> std::result::Result<String, Check> {
    let timeout = request_timeout(deadline, AUTH_TIMEOUT).map_err(|_| out_of_time(id))?;
    let (status, _, body) = authorize_answer(target, cred, surface, &[scope], PROBE_TTL, timeout)
        .map_err(|_| unreachable_check(id, target))?;
    match status {
        200 => envelope_of(surface, body).map_err(|_| unrecognised(id, "sign-in")),
        _ => Err(refused(id, target, status, body.as_ref())),
    }
}

/// A success this client cannot read, which usually means it is older than
/// the workspace. It proves neither health nor a fault.
fn unrecognised(id: &'static str, what: &str) -> Check {
    Check::with(
        id,
        Status::Unknown,
        format!("the workspace answered {what} in a shape this client does not recognise"),
        "update this client with `cadence update`, then report `cadence --version` if it repeats"
            .into(),
    )
}

fn read_check(
    target: &RemoteTarget,
    cred: &Credential,
    envelope: &str,
    deadline: Instant,
) -> Check {
    let Ok(timeout) = request_timeout(deadline, CALL_TIMEOUT) else {
        return out_of_time("read");
    };
    let (relay, version) = Surface::Cli.relay();
    let body = json!({"version": version, "envelope": envelope, "verb": READ_VERB});
    let Ok((status, _, answer)) = post(
        &target.endpoint,
        &format!("{relay}/call"),
        &cred.access_token,
        &body,
        timeout,
    ) else {
        return unreachable_check("read", target);
    };
    match status {
        200 if answer.as_ref().is_some_and(|a| a["projects"].is_array()) => {
            Check::ok("read", "the board answered a read")
        }
        200 => unrecognised("read", "a read"),
        _ => refused("read", target, status, answer.as_ref()),
    }
}

fn wiki_check(
    target: &RemoteTarget,
    cred: &Credential,
    scopes: &[&str],
    deadline: Instant,
) -> Check {
    if !scopes.contains(&"wiki.read") {
        return Check::skipped("wiki", "this login does not record wiki access");
    }
    match authorize("wiki", target, cred, Surface::Wiki, "wiki.read", deadline) {
        Ok(_) => Check::ok("wiki", "the workspace accepted this login for the wiki"),
        Err(check) => check,
    }
}

fn scopes_check(scopes: &[&str], target: &RemoteTarget) -> Check {
    if scopes.is_empty() {
        return Check::skipped(
            "scopes",
            "this login records no scopes, so the workspace decides each command",
        );
    }
    if !scopes.contains(&"cli.write") {
        return Check::with(
            "scopes",
            Status::Warn,
            "this login cannot change the board (it lacks cli.write)".into(),
            format!("{} if your workspace role allows writes", login(target)),
        );
    }
    let optional: Vec<String> = OPTIONAL_ACCESS
        .iter()
        .filter(|(_, needs, _)| !needs.iter().all(|scope| scopes.contains(scope)))
        .map(|(name, _, flag)| format!("{name}: `cadence login {}{flag}`", target.org))
        .collect();
    let summary = "this login includes board read and write";
    if optional.is_empty() {
        return Check::ok("scopes", summary);
    }
    Check::ok(
        "scopes",
        format!("{summary}; not included: {}", optional.join(", ")),
    )
}

fn out_of_time(id: &'static str) -> Check {
    Check::with(
        id,
        Status::Fail,
        "the workspace did not answer in time; this says nothing about your login".into(),
        "retry later; the saved login is kept".into(),
    )
}

fn unreachable_check(id: &'static str, target: &RemoteTarget) -> Check {
    Check::with(
        id,
        Status::Fail,
        format!(
            "could not reach {}; this says nothing about your login",
            target.endpoint
        ),
        "check this machine's network, then retry; the saved login is kept".into(),
    )
}

/// A non-success answer as a check, by the shared refusal decoder. Login is
/// the remedy only where the server proved the session is gone: the same
/// rule the hosted refusal text follows (CAD-1434, `docs/REMOTE-AUTH.md`).
fn refused(id: &'static str, target: &RemoteTarget, status: u16, body: Option<&Value>) -> Check {
    let (refusal, reason) = decode_refusal(status, body);
    let detail = reason.map_or(format!("HTTP {status}"), |r| format!("HTTP {status}, {r}"));
    let correlation = refusal_correlation(body).unwrap_or_default();
    let (status, summary, remedy) = match refusal {
        Refusal::Authentication => (
            Status::Fail,
            format!("the hosted session has ended ({detail})"),
            login(target),
        ),
        Refusal::Authorization => (
            Status::Fail,
            format!("the workspace says this is not permitted ({detail})"),
            "a new login does not change this; check your role with the workspace owner".into(),
        ),
        Refusal::Uncertain => (
            Status::Fail,
            format!(
                "the workspace refused this login without saying why ({detail}); that does not prove it expired"
            ),
            format!(
                "run doctor once more; if it repeats, {} once",
                login(target)
            ),
        ),
        Refusal::Availability => (
            Status::Fail,
            format!("the hosted service is unavailable ({detail}); your login is not the problem"),
            "retry later; the saved login is kept".into(),
        ),
        Refusal::MutationUncertain | Refusal::Unknown => (
            Status::Unknown,
            format!("the workspace answered with a refusal this client does not recognise ({detail})"),
            "report this line and `cadence --version` to the workspace owner".into(),
        ),
    };
    Check::with(id, status, format!("{summary}{correlation}"), remedy)
}

/// 0 ok, 1 warn or unknown, 2 fail: the codes `doctor --host` uses. A check
/// that could not be determined never reads as healthy.
pub fn exit_code(checks: &[Check]) -> i32 {
    match checks.iter().map(|c| c.status).max() {
        Some(Status::Fail) => 2,
        Some(Status::Warn | Status::Unknown) => 1,
        _ => 0,
    }
}

fn overall(checks: &[Check]) -> &'static str {
    match checks.iter().map(|c| c.status).max() {
        Some(Status::Fail) => "fail",
        Some(Status::Warn | Status::Unknown) => "warn",
        _ => "ok",
    }
}

pub fn report_json(target: &RemoteTarget, checks: &[Check]) -> Value {
    json!({
        "schema_version": REPORT_SCHEMA,
        "org": target.org,
        "endpoint": target.endpoint,
        "status": overall(checks),
        "checks": checks.iter().map(|c| json!({
            "id": c.id,
            "status": c.status.as_str(),
            "summary": c.summary,
            "remedy": c.remedy,
        })).collect::<Vec<_>>(),
    })
}

pub fn report_text(target: &RemoteTarget, checks: &[Check]) -> String {
    let mut out = format!("org {}: {}\n", target.org, overall(checks));
    for check in checks {
        out.push_str(&format!(
            "  {:<7} {:<10} {}\n",
            check.status.as_str(),
            check.id,
            check.summary
        ));
        if let Some(remedy) = &check.remedy {
            out.push_str(&format!("{:<20} → {remedy}\n", ""));
        }
    }
    out
}
