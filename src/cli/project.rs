//! CAD-535: `cadence project` — moved verbatim from src/main.rs.

use super::*;
use cadence_agent::issue::cli::{IssueAction, PolicyVisibility, ProjectAction};

/// `cadence project` verbs.
#[derive(Subcommand)]
pub(crate) enum ProjectCmd {
    /// List registered projects. On a selected hosted org this reads the
    /// org's own registry, including projects that exist only as their
    /// tickets' key; `--sort`, `--limit` and `--fields` refuse there.
    #[command(after_long_help = cadence_agent::filter::GRAMMAR)]
    #[command(visible_alias = "list")]
    Ls {
        /// Sort by key prefix default_owner; `-KEY` descending.
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
    /// Create a local project or register a project in a hosted workspace.
    ///
    /// Locally, optionally register a git repo and seed `<pm>/<key>/PROJECT.md`
    /// (goal, staffing, default stages and empty milestones). Idempotent for
    /// the same key and mode; conflicts, an invalid key, unsafe repo paths and
    /// non-Git paths are refused without replacing metadata. The operator or
    /// the master, through the daemon.
    ///
    /// On a selected hosted org this registers an empty project in the
    /// org's own registry: `--name` is required, the repo/PROJECT.md flags
    /// refuse, and only the live human workspace owner is admitted.
    New {
        /// Project key (folder name): 1-32 lowercase letters, digits or
        /// hyphens.
        key: String,
        /// Hosted orgs only: the project's human-readable name, kept
        /// distinct from the key (required remotely; 1-200 bytes). A local
        /// project is named by its key and its PROJECT.md heading.
        #[arg(long)]
        name: Option<String>,
        /// Existing Git checkout to register; its root and origin are recorded.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Issue id prefix. Local default: the key's first three
        /// letters/digits, uppercased (`reminders` -> REM); local prefixes
        /// are 1-8 uppercase letters/digits starting with a letter. Hosted
        /// default: derived from the key — letters/digits, up to 16,
        /// starting with a letter, else `ISSUE`.
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
        /// Access policy recorded in the project's first commit. Private is
        /// refused until the isolation gate passes; workspace-visible is the
        /// default and the only visibility this build records.
        ///
        /// No clap default: a hosted create records no policy at all, so
        /// naming the flag there must refuse rather than look like an
        /// omission — including `--visibility workspace-visible`.
        #[arg(long, value_enum)]
        visibility: Option<PolicyVisibility>,
        /// Grant a caller access to a private project
        /// (`users/<handle>` or `operator`); repeatable. Because the policy is
        /// written once at creation, grants cannot be added later by
        /// re-running this verb — use `issue project policy`.
        #[arg(long = "grant")]
        grants: Vec<String>,
    },
}

pub(super) fn run_project(state_dir: &Path, action: ProjectCmd) -> Result<i32> {
    match action {
        ProjectCmd::Ls {
            sort,
            limit,
            fields,
            json,
        } => cadence_agent::issue::cli::run(
            &IssueAction::Project {
                action: ProjectAction::Ls {
                    sort,
                    limit,
                    fields,
                    json,
                },
            },
            state_dir,
        ),
        ProjectCmd::New {
            key,
            name,
            repo,
            prefix,
            goal,
            agents,
            issue,
            visibility,
            grants,
        } => {
            // CAD-1999: a hosted org stores a project name beside its key.
            // A local project has no name field — its key names the folder
            // and the PROJECT.md heading — so the flag refuses instead of
            // being silently dropped.
            if name.is_some() {
                return Err(Error::rejected(
                    "`--name` is a hosted-org project field; a local project is \
                     named by its key and its PROJECT.md heading — nothing written",
                ));
            }
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
                       "agents": agents, "issue": issue,
                       "visibility": visibility
                           .unwrap_or(PolicyVisibility::WorkspaceVisible).wire(),
                       "grants": grants}),
            )?);
            Ok(0)
        }
    }
}

pub(super) fn run(state_dir: PathBuf, action: ProjectCmd) -> Result<i32> {
    run_project(&state_dir, action)
}
