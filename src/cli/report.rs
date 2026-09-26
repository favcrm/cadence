//! CAD-535: `cadence report` — moved verbatim from src/main.rs.

use super::*;

#[derive(Subcommand)]
pub(crate) enum ReportAction {
    /// The report ledger: open intake issues plus `cadence.report/2`
    /// task reports under each ticket's `reports/` — newest first.
    /// Bare `ls` lists open rows (open intake, unanswered questions);
    /// with filter flags it queries the record — open rows plus the
    /// done/blocked/answer/verdict records, which have no open state.
    /// Value flags repeat and comma-join and match ANY of their
    /// values; different flags AND.
    #[command(after_long_help = cadence_agent::filter::GRAMMAR)]
    Ls {
        /// question|feedback|idea|bug (intake) or done|question|
        /// blocked|answer|verdict (task reports); repeatable.
        #[arg(long, value_delimiter = ',')]
        kind: Vec<String>,
        /// Intake issue id, or the ticket a task report is filed on;
        /// repeatable.
        #[arg(long, value_delimiter = ',')]
        ticket: Vec<String>,
        /// Intake actor or task-report agent; repeatable.
        #[arg(long, value_delimiter = ',')]
        agent: Vec<String>,
        /// Project key; repeatable.
        #[arg(long, value_delimiter = ',')]
        project: Vec<String>,
        /// intake|task — which ledger a row comes from.
        #[arg(long, value_delimiter = ',')]
        source: Vec<String>,
        /// Only strictly-open rows (open intake, unanswered
        /// questions).
        #[arg(long, conflicts_with = "all")]
        open: bool,
        /// Everything, including resolved rows.
        #[arg(long)]
        all: bool,
        /// Sort by id at kind ticket agent project status source
        /// created (an `at` alias); `-KEY` descending [default: -at].
        #[arg(long, allow_hyphen_values = true)]
        sort: Option<String>,
        /// Keep only the first N rows.
        #[arg(long)]
        limit: Option<usize>,
        /// Keep only these keys in each row (comma-joined).
        #[arg(long, value_delimiter = ',')]
        fields: Vec<String>,
        /// Output is JSON already — accepted for grammar parity.
        #[arg(long)]
        json: bool,
    },
    /// Print one intake issue — status, tags, body with context.
    Show { id: String },
    /// File a task report (CAD-341, `cadence.report/2`): a Markdown
    /// file with frontmatter (kind, task, agent, sha, constraints,
    /// context_feedback; a question adds options, impact and
    /// `state: input-required`) and the six reflection headings
    /// (Expected, Evidence, Cause, Correction, Lesson, Next). Stored
    /// under the ticket's `reports/` through the tracker writer;
    /// malformed or credential-bearing reports are refused before
    /// anything is written. `cadence issue show` lists them.
    File {
        /// The ticket the report is about (must match `task:` if set).
        #[arg(long)]
        task: String,
        /// done|question|blocked|answer|verdict (must match `kind:` if
        /// set). A verdict (`verdict: pass|revise`, `sha:`) is filed by
        /// the daemon, only by the ticket's assigned reviewer.
        #[arg(long, value_enum)]
        kind: cadence_agent::issue::task_report::Kind,
        /// The report Markdown; else stdin.
        #[arg(long)]
        file: Option<PathBuf>,
    },
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    state_dir: PathBuf,
    kind: Option<cadence_agent::issue::report::Kind>,
    project: Option<String>,
    issue: Option<String>,
    text: Option<String>,
    file: Option<PathBuf>,
    priority: Option<String>,
    action: Option<ReportAction>,
) -> Result<i32> {
    use cadence_agent::issue::report;
    // CAD-431: a verdict is filed by the daemon, which checks the
    // caller is the assigned reviewer — this process needs no
    // tracker of its own for it.
    if let Some(ReportAction::File {
        task,
        kind: cadence_agent::issue::task_report::Kind::Verdict,
        file,
    }) = &action
    {
        let cap = cadence_agent::issue::task_report::BODY_MAX as u64;
        let text = read_body_capped(None, file.clone(), cap)?;
        print_json(&client::rpc(
            &state_dir,
            "report_verdict",
            json!({"issue": task, "text": text}),
        )?);
        return Ok(0);
    }
    let pm = cadence_agent::issue::Pm::open_default()?;
    match action {
        Some(ReportAction::Ls {
            kind,
            ticket,
            agent,
            project,
            source,
            open,
            all,
            sort,
            limit,
            fields,
            json: _,
        }) => {
            print_json(&report::ls(
                &pm,
                &report::LsFilter {
                    kinds: kind,
                    tickets: ticket,
                    agents: agent,
                    projects: project,
                    sources: source,
                    open,
                    all,
                    sort,
                    limit,
                    fields,
                },
            )?);
        }
        Some(ReportAction::Show { id }) => {
            print_json(&report::show(&pm, &id)?);
        }
        Some(ReportAction::File { task, kind, file }) => {
            use cadence_agent::issue::task_report;
            let text = read_body_capped(None, file, task_report::BODY_MAX as u64)?;
            let mut out = task_report::file(&pm, &text, Some(&task), Some(kind), "")?;
            // CAD-447: the daemon tells the question's author. The
            // answer stands either way; `route` says what happened.
            if kind == task_report::Kind::Answer {
                let report = out["report"].as_str().unwrap_or_default().to_string();
                let route = client::route_answer(&state_dir, &task, &report);
                if let Some(why) = client::answer_not_told(&route) {
                    eprintln!("cadence: the asker was not told: {why}");
                }
                out["route"] = route;
            }
            print_json(&out);
            // CAD-339: the daemon's report router routes it now
            // rather than at its next scan. Best effort.
            let _ = client::rpc_timeout(
                &state_dir,
                "reports_changed",
                json!({}),
                std::time::Duration::from_secs(2),
            );
        }
        None => {
            let body = read_body_capped(text, file, report::BODY_MAX as u64)?;
            let cwd = std::env::current_dir()?;
            print_json(&report::file(
                &pm,
                kind.unwrap_or(report::Kind::Feedback),
                project.as_deref(),
                issue.as_deref(),
                priority.as_deref(),
                &body,
                "",
                &state_dir,
                &cwd,
            )?);
        }
    }
    Ok(0)
}
