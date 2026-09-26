//! CAD-535: `cadence dispatch` — moved verbatim from src/main.rs.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    state_dir: PathBuf,
    issue: String,
    to: String,
    note: Option<PathBuf>,
    name: Option<String>,
    base: Option<String>,
    repo: Option<PathBuf>,
    reply_to: Option<String>,
    summary: Option<String>,
    job: bool,
    spec: Option<PathBuf>,
    no_lessons: bool,
    force: bool,
    take_over: Option<String>,
) -> Result<i32> {
    // CAD-339: the master dispatches only through the daemon,
    // which composes the kickoff itself — never this client path.
    if std::env::var("CADENCE_ALIAS").as_deref() == Ok(cadence_agent::master::ALIAS) {
        let out = client::rpc(
            &state_dir,
            "master_dispatch",
            json!({"issue": issue, "to": to}),
        )?;
        print_json(&out);
        return Ok(0);
    }
    let pm = cadence_agent::issue::Pm::open_default()?;
    let args = cadence_agent::issue::dispatch::DispatchArgs {
        to: to.clone(),
        note: note.clone(),
        name: name.clone(),
        base: base.clone(),
        repo: repo.clone(),
        reply_to: reply_to.clone(),
        summary: summary.clone(),
        job_spec: job.then(|| spec.clone().unwrap_or_default()),
        no_lessons,
        force,
        take_over,
    };
    let out =
        cadence_agent::issue::dispatch::run(&pm, issue.as_str(), &args, "", &state_dir, None)?;
    // CAD-383: a backlog/ready issue someone else owns warns only.
    if let Some(warning) = out["claim"]["warning"].as_str() {
        eprintln!("warning: {warning}");
    }
    // CAD-159: an issue with no acceptance items still
    // dispatches, but the operator sees why it should not.
    if let Some(warning) = out["acceptance"]["warning"].as_str() {
        eprintln!("warning: {warning}");
    }
    // CAD-378: overlapping lanes, owned or full code areas —
    // advisory only.
    for line in cadence_agent::issue::areas::warning_lines(&out["leases"]) {
        eprintln!("warning: {line}");
    }
    print_json(&out);
    Ok(0)
}
