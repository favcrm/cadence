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
    #[command(visible_alias = "list")]
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
        let text = read_master_command_file(&state_dir, file.as_deref(), cap)?;
        print_json(&client::rpc_relay(
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
            cadence_agent::master::refuse_in_app_conversation("report file")?;
            let cap = task_report::BODY_MAX as u64;
            let text = read_master_command_file(&state_dir, file.as_deref(), cap)?;
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
            let body = if let Some(text) = text {
                text
            } else {
                read_master_command_file(&state_dir, file.as_deref(), report::BODY_MAX as u64)?
            };
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

#[cfg(test)]
mod cad1098_tests {
    use super::*;

    const CHILD: &str = "CAD1098_BELT_CHILD";

    fn refused(error: &Error) -> bool {
        error
            .to_string()
            .contains("not available in an app conversation")
    }

    /// Runs inside the child process the test below spawns, with the
    /// environment the daemon gives an app conversation's master session.
    /// A normal run of the suite does nothing here.
    #[test]
    fn cad1098_belt_child() {
        let Ok(mode) = std::env::var(CHILD) else {
            return;
        };
        let dir = PathBuf::from(std::env::var("CAD1098_STATE").unwrap());
        let file = ReportAction::File {
            task: "CAD-1".into(),
            kind: cadence_agent::issue::task_report::Kind::Done,
            file: None,
        };
        let new = cadence_agent::issue::cli::IssueAction::New {
            title: "t".into(),
            project: None,
            priority: None,
            parent: None,
            epic: None,
            tags: vec![],
            blocked_by: vec![],
            owner: None,
            component: None,
            id: None,
            file: None,
            status: None,
        };
        let report = run(dir.clone(), None, None, None, None, None, None, Some(file));
        let issue = super::super::issue::run(dir, new);
        let (report, issue) = (report.unwrap_err(), issue.unwrap_err());
        match mode.as_str() {
            "app" => assert!(refused(&report) && refused(&issue), "{report} / {issue}"),
            // Control: the same verbs, same master, no app marker — they get
            // past the belt (and stop on their own, different, checks).
            _ => assert!(!refused(&report) && !refused(&issue), "{report} / {issue}"),
        }
    }

    fn spawn(mode: &str, conversation: Option<&str>) -> std::process::Output {
        let root = tempfile::Builder::new().prefix("c98b").tempdir().unwrap();
        let pm = root.path().join("pm");
        cadence_agent::issue::Pm::init(&pm).unwrap();
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", "cli::report::cad1098_tests::cad1098_belt_child"])
            .args(["--nocapture", "--test-threads", "1"])
            .env_remove("CADENCE_CONVERSATION")
            .env("CADENCE_ALIAS", "master")
            .env("CADENCE_PM_DIR", &pm)
            .env("CAD1098_STATE", root.path())
            .env("HOME", root.path())
            .env(CHILD, mode);
        if let Some(c) = conversation {
            cmd.env("CADENCE_CONVERSATION", c);
        }
        cmd.output().unwrap()
    }

    /// CAD-1098 I6 (defence in depth ONLY): the master with
    /// `CADENCE_CONVERSATION=app` is refused by `issue new` and
    /// `report file` before they touch the tracker; without it (or with
    /// any other value) the belt does not fire. The daemon-side half —
    /// that the daemon never reads the variable — is
    /// `daemon::conversations_acceptance::the_daemon_never_reads_the_conversation_env`.
    ///
    /// Guard: `master::refuse_in_app_conversation` in `issue new` and
    /// `report file`.
    #[test]
    fn cad1098_cli_belt_refuses_issue_new_and_report_file_in_an_app_conversation() {
        for (mode, conversation) in [("app", Some("app")), ("home", None), ("home", Some("home"))] {
            let out = spawn(mode, conversation);
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(out.status.success(), "{mode}/{conversation:?}: {text}");
            assert!(text.contains("1 passed"), "child did not run: {text}");
        }
    }
}
