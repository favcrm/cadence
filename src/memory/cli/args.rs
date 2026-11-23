//! Clap CLI argument definitions for `cadence memory`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::PathBuf;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum MemoryAction {
    /// Propose a memory through the authenticated native daemon endpoint.
    Propose {
        /// Project key. A native agent may omit it: the daemon then files
        /// the proposal into the project of its running issue turn
        /// (CAD-2662). Anyone else must pass it.
        #[arg(long)]
        project: Option<String>,
        /// rule | gotcha | decision | recipe.
        #[arg(long = "type")]
        kind: String,
        /// Scope: project component the fact applies to; repeatable.
        #[arg(long = "scope-component")]
        components: Vec<String>,
        /// Scope: repo-relative glob (`src/adapter/**`); repeatable.
        #[arg(long = "scope-path")]
        paths: Vec<String>,
        /// Scope: provider name (claude, devin, …); repeatable.
        #[arg(long = "scope-provider")]
        providers: Vec<String>,
        /// Scope: tag matching an issue's tags; repeatable.
        #[arg(long = "scope-tag")]
        tags: Vec<String>,
        /// Scope: the whole project — matches every dispatch.
        #[arg(long = "scope-project")]
        project_wide: bool,
        /// Where the fact was learned — issue id, note path or commit.
        /// The daemon refuses a blank or omitted source.
        #[arg(long)]
        source: Option<String>,
        /// low | medium | high [default: medium].
        #[arg(long)]
        confidence: Option<String>,
        /// Read the fact from a file — a full memory file (frontmatter
        /// kept) or a bare body.
        #[arg(long)]
        from: Option<PathBuf>,
        /// Inline body: fact line(s) then `**Why:**`/`**How to
        /// apply:**` sections.
        #[arg(short = 'm')]
        text: Option<String>,
        /// Explicit slug — else slugified from the fact's first line.
        #[arg(long)]
        id: Option<String>,
        /// Whether validity is mechanically checkable or a judgement.
        #[arg(long, default_value = "judgement", value_parser = ["mechanical", "judgement"])]
        nature: String,
        /// Company adds policy approval requirements, not cross-project visibility.
        #[arg(long, default_value = "project", value_parser = ["project", "role", "company"])]
        audience: String,
        /// Audience role selector. Not a retrieval axis; repeatable.
        #[arg(long = "scope-role")]
        roles: Vec<String>,
        /// Immutable source reference as JSON; repeatable.
        #[arg(long = "evidence-ref")]
        evidence_refs: Vec<String>,
        /// Declarative check identifier/revisions as JSON, never a command.
        #[arg(long)]
        verification_condition: Option<String>,
    },
    /// Finalize the exact candidate under its versioned native policy.
    Accept {
        /// Memory slug.
        slug: String,
        /// Project key — needed only when the slug is ambiguous.
        #[arg(long)]
        project: Option<String>,
        /// Semantic digest of the candidate actually reviewed.
        #[arg(long)]
        digest: String,
        #[arg(long)]
        evidence: Option<String>,
    },
    /// Reject the exact candidate under its native versioned policy.
    Reject {
        /// Memory slug.
        slug: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        digest: String,
        #[arg(long)]
        evidence: String,
    },
    /// Submit one native independent PM/worker review. The daemon derives the reviewer
    /// from the Unix socket peer; the digest must be supplied explicitly.
    Review {
        slug: String,
        #[arg(long)]
        project: Option<String>,
        /// accept or verify.
        #[arg(long)]
        operation: String,
        /// pass or revise.
        #[arg(long)]
        verdict: String,
        #[arg(long)]
        evidence: String,
        #[arg(long)]
        digest: String,
    },
    /// Supersede is refused until crash-atomic pair recovery is available.
    Supersede {
        /// Slug being replaced.
        old: String,
        /// Slug replacing it.
        new: String,
        #[arg(long)]
        project: Option<String>,
    },
    /// Complete native revalidation; a policy decision is not a logic check.
    Verify {
        /// Memory slug.
        slug: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        digest: String,
        #[arg(long)]
        evidence: Option<String>,
    },
    /// Attest agent source provenance as the actual operator; not a check result.
    Classify {
        slug: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        digest: String,
        #[arg(long)]
        evidence: String,
        #[arg(long = "evidence-ref")]
        evidence_refs: Vec<String>,
    },
    /// Withhold an accepted v2 claim pending a fresh policy decision.
    Revalidate {
        slug: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        digest: String,
        #[arg(long)]
        evidence: String,
    },
    /// Invalidate a v2 claim without deleting its history.
    Invalidate {
        slug: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        digest: String,
        #[arg(long)]
        evidence: String,
    },
    /// Report use/wrong/reread from the assigned current native message turn.
    Feedback {
        slug: String,
        #[arg(long)]
        project: String,
        #[arg(long)]
        digest: String,
        #[arg(long)]
        message: String,
        #[arg(long)]
        token: String,
        #[arg(long, value_parser = ["used", "wrong", "reread"])]
        kind: String,
        #[arg(long)]
        evidence: Option<String>,
    },
    /// List memories — a compact table on a TTY, JSON when piped or
    /// `--json` is set.
    /// Value flags repeat and comma-join and match ANY of their values;
    /// different flags AND.
    #[command(after_long_help = crate::filter::GRAMMAR)]
    #[command(visible_alias = "list")]
    Ls {
        /// Project key; repeatable.
        #[arg(long, value_delimiter = ',')]
        project: Vec<String>,
        /// proposed | quarantined | accepted | pending-revalidation | invalidated | rejected | superseded; repeatable.
        #[arg(long, value_delimiter = ',')]
        status: Vec<String>,
        /// rule | gotcha | decision | recipe; repeatable.
        #[arg(long = "type", value_delimiter = ',')]
        kind: Vec<String>,
        /// Only memories applying to this component; repeatable.
        #[arg(long, value_delimiter = ',')]
        component: Vec<String>,
        /// Only memories whose path globs match this file; repeatable.
        #[arg(long, value_delimiter = ',')]
        path: Vec<String>,
        /// Accepted memories whose evidence is not current, read the
        /// way retrieval reads it: marked stale (withheld), no verify
        /// inside the window (unverified), or path globs matching files
        /// changed since the last verify in a project repo.
        #[arg(long)]
        stale: bool,
        /// Freshness window in days [default: the project's
        /// `memory.stale_days`, else 30]. --component/--path select a
        /// retrieval scope and don't apply to the stale axis.
        #[arg(long)]
        days: Option<u64>,
        /// Sort by project slug type status confidence created;
        /// `-KEY` descending. Under --stale the row set is different:
        /// project slug type reason label verified_at.
        #[arg(long, allow_hyphen_values = true)]
        sort: Option<String>,
        /// Keep only the first N rows.
        #[arg(long)]
        limit: Option<usize>,
        /// Keep only these keys in each --json row (comma-joined).
        #[arg(long, value_delimiter = ',')]
        fields: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show one memory's frontmatter and body.
    Show {
        /// Memory slug.
        slug: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Ranked accepted memories applying to a context: `--issue <ID>`
    /// (component, tags, recorded-commit paths and the planned paths it
    /// declares) and/or explicit axes. `--json` reports the declared
    /// paths as `context.planned_paths`, apart from the recorded and
    /// explicit `context.paths`; without `--issue` it is empty.
    Match {
        /// Your own running task's lessons, from the daemon (CAD-2662):
        /// the selection dispatch made for it, with any `--path`,
        /// `--component` and `--tag` added, recorded as a lookup. The
        /// project, issue, provider and role come from your turn. Run it at
        /// the start of a task, when the scope changes, and before you
        /// repeat an approach that already failed.
        #[arg(long, conflicts_with_all = ["issue", "project", "provider", "role"])]
        turn: bool,
        /// Issue id — supplies component, tags, recorded-commit paths and
        /// the issue's validated declared planned paths, the same inputs
        /// dispatch selects with.
        #[arg(long)]
        issue: Option<String>,
        /// Project key for explicit-axis matching; defaults to the cwd's
        /// registered project when `--issue` is absent.
        #[arg(long)]
        project: Option<String>,
        /// Provider for scope matching (the dispatch target's).
        #[arg(long)]
        provider: Option<String>,
        /// Assigned role for role-restricted matching; unknown withholds.
        #[arg(long)]
        role: Option<String>,
        /// Component axis; repeatable.
        #[arg(long)]
        component: Vec<String>,
        /// Repo-relative path axis; repeatable. Recorded commit paths and
        /// the issue's declared planned paths are separate inputs —
        /// reported as `context.paths` and `context.planned_paths`.
        #[arg(long)]
        path: Vec<String>,
        /// Tag axis; repeatable.
        #[arg(long)]
        tag: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Lint memory files — schema, body contract, components,
    /// dangling supersedes. Non-zero exit on any error.
    Lint {
        #[arg(long)]
        project: Option<String>,
    },
}
