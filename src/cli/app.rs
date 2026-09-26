//! CAD-535: `cadence app` — moved verbatim from src/main.rs.

use super::*;

/// `cadence app` verbs (CAD-547). `install`/`update`/`set`/`remove`
/// write the tracker directly — one commit each, `Actor:` recorded —
/// `ls`/`show` read it; `approve` is the operator's daemon gate, like
/// `workflow approve`.
#[derive(Subcommand)]
pub(crate) enum AppAction {
    /// Install an app folder into the project: `app.md` (frontmatter
    /// `app`, `title`, `version`, `needs.connections`) plus
    /// `workflows/*.md` — every one checked like `workflow check` —
    /// and optional flat `rubrics/`, `templates/` dirs. `<source>` is a
    /// local path or a git URL (cloned, pinned to the commit SHA in the
    /// install record). Nothing in the bundle executes, and nothing in
    /// it runs until `cadence app approve`.
    Install {
        /// App source — a folder path or a git URL.
        source: String,
        /// Project key the app installs into.
        #[arg(long)]
        project: String,
    },
    /// Every installed app: project, name, version, workflows, slots
    /// with bindings, digest and approval state.
    Ls {
        /// Project key; all projects when absent.
        #[arg(long)]
        project: Option<String>,
    },
    /// An installed app's manifest, guide, workflow summaries, slot
    /// bindings, install record, digest and approval state.
    Show {
        /// App name — the `apps/<name>/` folder.
        name: String,
        /// Project key.
        #[arg(long)]
        project: String,
    },
    /// Bind a declared `needs.connections` slot to a connection name —
    /// `<slot>=<connection>`, repeatable; `<slot>=` unbinds it
    /// explicitly. Slots default to `local`. A binding change is
    /// structural — it re-gates the app until `app approve`.
    Set {
        /// App name.
        name: String,
        /// `<slot>=<connection>` pairs — repeatable.
        bindings: Vec<String>,
        /// Project key.
        #[arg(long)]
        project: String,
    },
    /// Replace an installed app from its recorded source (or the given
    /// one), after the same checks `install` runs — prints the diff.
    /// Any structural change re-gates the app until `app approve`.
    Update {
        /// App name.
        name: String,
        /// A folder path or git URL; absent re-reads the recorded
        /// source.
        source: Option<String>,
        /// Project key.
        #[arg(long)]
        project: String,
    },
    /// Remove an installed app — the folder and its install record, one
    /// commit. Refuses while a plan proposed from the app is open.
    Remove {
        /// App name.
        name: String,
        /// Project key.
        #[arg(long)]
        project: String,
    },
    /// Approve the app's current structure — `plan propose
    /// --workflow <app>/<wf>` refuses it until this matches the
    /// installed folder. Operator only, through the daemon.
    Approve {
        /// App name.
        name: String,
        /// Project key.
        #[arg(long)]
        project: String,
    },
    /// Record the app's default team (CAD-577): one agent alias per
    /// workflow input role, `<input>=<agent>` repeatable; `<input>=`
    /// clears a role. The team lives with the install record and is
    /// not part of the gate digest, so setting it never re-requires
    /// approval. Operator only, through the daemon.
    SetTeam {
        /// App name.
        name: String,
        /// `<input>=<agent>` pairs — repeatable.
        #[arg(long = "role", required = true)]
        roles: Vec<String>,
        /// Project key.
        #[arg(long)]
        project: String,
    },
    /// Join a new Devin worker for one of the app's team roles (CAD-577)
    /// — the board's "Add worker": a unique role-prefixed alias, under
    /// the operator (a group root), recorded in the app's default team.
    /// Operator only, through the daemon.
    AddWorker {
        /// App name.
        name: String,
        /// The team role the worker fills.
        #[arg(long)]
        role: String,
        /// Project key.
        #[arg(long)]
        project: String,
    },
}

/// `cadence app …` (CAD-547). `install`/`update`/`set`/`remove` write
/// the tracker directly — one commit each, `Actor:` recorded, all
/// unapproving-by-construction (a tracker write can only change the
/// digest, never the approval record) — `ls`/`show` read; `approve`
/// is the operator's daemon call, like `workflow approve`.
pub(super) fn run_app(state_dir: &Path, action: AppAction) -> Result<i32> {
    use cadence_agent::issue::app;
    let result = match &action {
        AppAction::Install { source, project } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::install(&pm, project, source, state_dir, "")?
        }
        AppAction::Ls { project } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::ls(&pm, project.as_deref(), state_dir)?
        }
        AppAction::Show { name, project } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::show(&pm, project, name, state_dir)?
        }
        AppAction::Set {
            name,
            bindings,
            project,
        } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::set(&pm, project, name, bindings, state_dir, "")?
        }
        AppAction::Update {
            name,
            source,
            project,
        } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::update(&pm, project, name, source.as_deref(), state_dir, "")?
        }
        AppAction::Remove { name, project } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::remove(&pm, project, name, state_dir, "")?
        }
        AppAction::Approve { name, project } => client::rpc(
            state_dir,
            "app_approve",
            json!({"project": project, "name": name}),
        )?,
        AppAction::SetTeam {
            name,
            roles,
            project,
        } => client::rpc(
            state_dir,
            "app_set_team",
            json!({"project": project, "name": name, "team": roles}),
        )?,
        AppAction::AddWorker {
            name,
            role,
            project,
        } => client::rpc(
            state_dir,
            "app_add_worker",
            json!({"project": project, "name": name, "role": role}),
        )?,
    };
    print_json(&result);
    Ok(0)
}

pub(super) fn run(state_dir: PathBuf, action: AppAction) -> Result<i32> {
    run_app(&state_dir, action)
}
