//! CAD-535: `cadence project` — moved verbatim from src/main.rs.

use super::*;

/// `cadence project` verbs.
#[derive(Subcommand)]
pub(crate) enum ProjectCmd {
    /// Register a git repo as project `<key>` and seed `<pm>/<key>/PROJECT.md`
    /// (goal, staffing `agents:` map, the default stages, empty
    /// milestones) in one tracker commit. Idempotent for the same key
    /// and repo; a different repo for an existing key, the reserved key
    /// `agents`, an invalid key, a path that is not a git repo, the
    /// tracker or the daemon's state dir is refused with nothing written.
    /// The operator or the master, through the daemon.
    New {
        /// Project key (folder name): 1-32 lowercase letters, digits or
        /// hyphens.
        key: String,
        /// A path in the repo's checkout; its main checkout root and
        /// origin remote are recorded.
        #[arg(long)]
        repo: PathBuf,
        /// Issue id prefix [default: the key's first three letters,
        /// uppercased].
        #[arg(long)]
        prefix: Option<String>,
        /// The goal paragraph for PROJECT.md.
        #[arg(long)]
        goal: Option<String>,
        /// Staffing `<agent>=<sessions>` (repeatable or comma-separated)
        /// [default: pm=1,dev=1,qa=1].
        #[arg(long = "agent")]
        agents: Vec<String>,
        /// The issue this project is created for — the commit's
        /// `Issue:` trailer.
        #[arg(long)]
        issue: Option<String>,
    },
}

pub(super) fn run_project(state_dir: &Path, action: ProjectCmd) -> Result<i32> {
    match action {
        ProjectCmd::New {
            key,
            repo,
            prefix,
            goal,
            agents,
            issue,
        } => {
            let repo = cadence_agent::issue::project::expand_home(&repo.to_string_lossy());
            let repo = if repo.is_absolute() {
                repo
            } else {
                std::env::current_dir()?.join(repo)
            };
            let repo = repo.canonicalize().map_err(|_| {
                Error::rejected(format!(
                    "{} is not a git repo with a checkout — it does not exist",
                    repo.display()
                ))
            })?;
            print_json(&client::rpc(
                state_dir,
                "project_new",
                json!({"key": key, "repo": repo, "prefix": prefix, "goal": goal,
                       "agents": agents, "issue": issue}),
            )?);
            Ok(0)
        }
    }
}

pub(super) fn run(state_dir: PathBuf, action: ProjectCmd) -> Result<i32> {
    run_project(&state_dir, action)
}
