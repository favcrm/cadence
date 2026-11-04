//! CAD-535: `cadence doctor` — moved verbatim from src/main.rs.
//! CAD-2111: [`run_remote`] is the same verb on a hosted org.

use super::*;
use cadence_agent::remote_cli::probe::{self, Check};
use cadence_agent::remote_cli::RemoteTarget;

pub(super) fn run(state_dir: PathBuf, host: bool, json: bool, reclaim_plan: bool) -> Result<i32> {
    if host {
        return cadence_agent::doctor::host::cli(&state_dir, json, reclaim_plan);
    }
    let report = cadence_agent::doctor::run(&state_dir)?;
    print_json(&report);
    let ok = report
        .pointer("/checks/storage/ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(if ok { 0 } else { 1 })
}

/// `cadence doctor` with a hosted org selected. It opens no local state, so it
/// works on a client-only installation; the probe never falls back to local.
pub(super) fn run_remote(
    target: &RemoteTarget,
    json: bool,
    offline: bool,
    org_flag: bool,
    client_only: bool,
) -> i32 {
    let kind = if client_only { "client-only" } else { "native" };
    let mut checks = vec![
        Check::ok(
            "install",
            format!(
                "{kind} installation, version {}",
                cadence_agent::overview::BUILD_ID
            ),
        ),
        Check::ok(
            "selection",
            format!(
                "org {} at {}, selected by {}",
                target.org,
                target.endpoint,
                selected_by(target, org_flag)
            ),
        ),
    ];
    checks.extend(probe::run(target, offline));
    if json {
        print_json(&probe::report_json(target, &checks));
    } else {
        print!("{}", probe::report_text(target, &checks));
    }
    probe::exit_code(&checks)
}

/// Why this org, by the precedence `org::settle` applies: a running assignment
/// binds a managed caller, then `--org`, then `CADENCE_ORG`, then the saved
/// default.
fn selected_by(target: &RemoteTarget, org_flag: bool) -> &'static str {
    if target.identity.is_some() {
        "this session's running assignment"
    } else if org_flag {
        "--org"
    } else if std::env::var_os("CADENCE_ORG").is_some_and(|value| !value.is_empty()) {
        "CADENCE_ORG"
    } else {
        "the saved default"
    }
}
