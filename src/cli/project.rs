//! CAD-535: `cadence project` — moved verbatim from src/main.rs.

use super::*;

/// `cadence project` verbs.
#[derive(Subcommand)]
pub(crate) enum ProjectCmd {
    /// Create project `<key>`, optionally register a git repo and seed
    /// `<pm>/<key>/PROJECT.md` (goal, staffing, default stages and empty
    /// milestones). Idempotent for the same key and mode; conflicts, an
    /// invalid key, unsafe repo paths and non-Git paths are refused without
    /// replacing metadata. The operator or the master, through the daemon.
    New {
        /// Project key (folder name): 1-32 lowercase letters, digits or
        /// hyphens.
        key: String,
        /// Existing Git checkout to register; its root and origin are recorded.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Issue id prefix [default: the key's first three letters,
        /// uppercased].
        #[arg(long)]
        prefix: Option<String>,
        /// The goal paragraph for PROJECT.md (requires --repo).
        #[arg(long)]
        goal: Option<String>,
        /// Staffing `<agent>=<sessions>` (repeatable or comma-separated;
        /// requires --repo) [default: pm=1,dev=1,qa=1].
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
            if repo
                .as_ref()
                .is_some_and(|repo| repo.as_os_str().is_empty())
            {
                return Err(Error::rejected("--repo path must not be empty"));
            }
            let repo = repo
                .map(|repo| {
                    let repo = cadence_agent::issue::project::expand_home(&repo.to_string_lossy());
                    let repo = if repo.is_absolute() {
                        repo
                    } else {
                        std::env::current_dir()?.join(repo)
                    };
                    repo.canonicalize().map_err(|_| {
                        Error::rejected(format!(
                            "{} is not a git repo with a checkout — it does not exist",
                            repo.display()
                        ))
                    })
                })
                .transpose()?;
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
