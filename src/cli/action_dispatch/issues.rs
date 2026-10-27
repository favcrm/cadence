//! Issue, app, wiki and settings dispatch — split from `action_dispatch.rs` (CAD-1654).

use super::*;

pub(super) fn dispatch(state_dir: PathBuf, command: Commands) -> Result<i32> {
    match command {
        Commands::Org { action } => org::run(action),
        Commands::Issue { action } => issue::run(state_dir, action),
        Commands::Connection { action } => connection::run(state_dir, action),
        Commands::Platform { action } => platform::run(state_dir, action),
        Commands::Idea { action } => idea::run(&state_dir, action),
        Commands::Plan { action } => plan::run(state_dir, action),
        Commands::Workflow { action } => workflow::run(state_dir, action),
        Commands::App { action } => app::run(state_dir, action),
        Commands::Project { action } => project::run(state_dir, action),
        Commands::Milestone { action } => milestone::run(state_dir, action),
        Commands::Master { action } => master::run(state_dir, action),
        Commands::Delivery { action } => delivery::run(state_dir, action),
        Commands::Report {
            kind,
            project,
            issue,
            text,
            file,
            priority,
            action,
        } => report::run(
            state_dir, kind, project, issue, text, file, priority, action,
        ),
        Commands::Intake { action } => intake::run(state_dir, action),
        Commands::Memory { action } => memory::run(state_dir, action),
        Commands::Wiki { action } => wiki::run(state_dir, action),
        Commands::Secret { action } => secret::run(state_dir, action),
        Commands::Template { action } => business_templates::run_template(state_dir, action),
        Commands::Document { action } => business_templates::run_document(state_dir, action),
        Commands::Ui { action } => ui::run(state_dir, action),
        Commands::Settings { action } => settings::run(&state_dir, action),
        Commands::MyWork => Err(Error::usage(
            "my-work requires a hosted org; run cadence login, then cadence --org <org> my-work",
        )),
        Commands::Review {
            action,
            pr,
            repo,
            full,
            no_full,
            no_suite_lock,
            stress,
            keep,
            json,
        } => match action {
            Some(ReviewAction::Flakes { test }) => {
                cadence_agent::review::print_flakes(state_dir, test).map(|()| 0)
            }
            None => review::run(
                state_dir,
                pr.expect("clap requires PR without a subcommand"),
                repo,
                full,
                no_full,
                no_suite_lock,
                stress,
                keep,
                json,
            ),
        },
        Commands::Session { action } => session::run(state_dir, action),
        Commands::Audit {
            action,
            since,
            class,
            project,
            json,
            limit,
            repo,
            notes_dir,
            merge_report,
        } => audit::run(
            state_dir,
            action,
            since,
            class,
            project,
            json,
            limit,
            repo,
            notes_dir,
            merge_report,
        ),
        Commands::Overview {
            json,
            watch,
            project,
            group,
        } => overview::run(state_dir, json, watch, project, group),
        _ => unreachable!("issues dispatch received a non-issue command"),
    }
}
