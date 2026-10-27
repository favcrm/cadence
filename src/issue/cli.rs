//! `cadence issue` — the only writer for the PM board. Every verb ends
//! in a single git commit inside the PM dir; reads need no daemon.

use std::io::Read;
use std::path::PathBuf;

use clap::Subcommand;
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::{
    blocked, board, claim, doctor, finish, groom, history, hooks, lint, model, project, reclaim,
    reconcile, retro, sprint, start, sync, work, write, Pm,
};

mod epic;
mod milestone;
pub use epic::{run_epic, EpicAction};
pub use milestone::{run_milestone, MilestoneAction};
mod board_actions;
mod checkout;
mod lifecycle;
mod links;
mod maintenance;
mod new;
mod project_actions;
mod recover;
pub use recover::{run_recover, RecoverAction};
mod presentation;
pub(crate) use presentation::{
    atty_stdin, ls_summary, print_ls_table, print_show, print_stage_and_health, print_table,
    read_stdin_body,
};

#[derive(Subcommand)]
pub enum CheckoutAction {
    /// Read-only, structured inventory for one repository.
    Inventory {
        #[arg(long)]
        repo: PathBuf,
    },
    /// Explicitly adopt a pre-existing linked checkout; no cleanup is run.
    Adopt {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        path: PathBuf,
        #[arg(long, value_parser = ["development", "review", "validation", "agent", "staging"])]
        purpose: String,
        #[arg(long)]
        tool: String,
        #[arg(long)]
        owner: String,
        #[arg(long)]
        pinned_sha: String,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long = "release-artifact")]
        release_artifacts: Vec<String>,
        #[arg(long = "rollback-artifact")]
        rollback_artifacts: Vec<String>,
    },
    /// Explicitly release or resolve an interrupted release without deleting checkout data.
    Release {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        reason: String,
    },
    /// Explicitly resume a released checkout at its exact current HEAD.
    Resume {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        pinned_sha: String,
        #[arg(long)]
        reason: String,
    },
    /// Explicitly retain an active checkout for investigation.
    Retain {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        reason: String,
    },
}

#[derive(Subcommand)]
pub enum IssueAction {
    /// Create the PM dir skeleton (pm.yaml, README, .gitignore, git
    /// init + first commit) and install the pre-commit/post-commit
    /// hooks. Idempotent; a foreign hook is never overwritten.
    Init,
    /// Read-only tracker health report: root, git repo, remote, hook
    /// state, lint, push lag and the failure-log tail. Non-zero exit
    /// when any check fails.
    Doctor,
    /// Manage projects under the PM dir.
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
    /// Create an issue: allocates `<PREFIX>-<n>` under the write lock.
    New {
        /// Issue title.
        title: String,
        /// Project key — else CADENCE_PROJECT, else the cwd's repo.
        #[arg(long)]
        project: Option<String>,
        /// P0..P3 [default: P2].
        #[arg(long)]
        priority: Option<String>,
        /// Parent issue id (sub-issue; depth is two levels).
        #[arg(long)]
        parent: Option<String>,
        /// The epic this issue belongs to — an alias of `--parent`.
        #[arg(long, conflicts_with = "parent")]
        epic: Option<String>,
        /// A tag; repeatable. Checked against the project's `tags:`
        /// list when it declares one.
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// blocked_by target; repeatable.
        #[arg(long = "blocked-by")]
        blocked_by: Vec<String>,
        /// Owner [default: the project's default_owner].
        #[arg(long)]
        owner: Option<String>,
        /// Component label (must be declared by the project).
        #[arg(long)]
        component: Option<String>,
        /// Mint this exact id — for seeds/imports; must match the
        /// project prefix and not exist. The master may not set it.
        #[arg(long)]
        id: Option<String>,
        /// Issue body. The Pi master writes this under its tmp dir
        /// first, then passes the path — stdin (`-`) is refused.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Status to create in. Only `backlog` is accepted; omit it
        /// for the same result. The master cannot create any other.
        #[arg(long)]
        status: Option<String>,
    },
    /// List issues — a compact table on a TTY, JSON when piped (or `--json`).
    /// Value flags repeat and comma-join and match ANY of their values;
    /// different flags AND; `--tag` is the exception — all must be
    /// present. The grammar prints from `crate::filter::GRAMMAR`.
    #[command(after_long_help = crate::filter::GRAMMAR)]
    #[command(visible_alias = "list")]
    Ls {
        /// Project key; repeatable — issues in any of them.
        #[arg(long, value_delimiter = ',')]
        project: Vec<String>,
        /// Status (backlog ready doing review done dropped);
        /// repeatable — any of them.
        #[arg(long, value_delimiter = ',')]
        status: Vec<String>,
        /// Only issues carrying this tag; repeatable — all of them.
        #[arg(long = "tag", value_delimiter = ',')]
        tags: Vec<String>,
        /// Only children of these epics; repeatable — any of them.
        #[arg(long, value_delimiter = ',')]
        epic: Vec<String>,
        /// Owner; repeatable — any of them.
        #[arg(long, value_delimiter = ',')]
        owner: Vec<String>,
        /// Component; repeatable — any of them.
        #[arg(long, value_delimiter = ',')]
        component: Vec<String>,
        /// P0..P3; repeatable — any of them.
        #[arg(long, value_delimiter = ',')]
        priority: Vec<String>,
        /// Effective item type (epic task bug spike); repeatable.
        #[arg(long = "type", value_delimiter = ',')]
        types: Vec<String>,
        /// Milestone id (`milestone:` field or `m<n>` tag); repeatable.
        #[arg(long, value_delimiter = ',')]
        milestone: Vec<String>,
        /// Work stage (the project's `stages:` ids, or shape build
        /// verify release done, plus rejected); repeatable. Not under
        /// `--at` — stages are computed for the live board.
        #[arg(long, value_delimiter = ',')]
        stage: Vec<String>,
        /// Work health (on_track at_risk stalled); repeatable. Not
        /// under `--at`.
        #[arg(long, value_delimiter = ',')]
        health: Vec<String>,
        /// Plan state (proposed approved rejected); a bare `--plan`
        /// matches every issue carrying a plan. Repeatable.
        #[arg(long, num_args = 0..=1, default_missing_value = "any")]
        plan: Vec<String>,
        /// Only issues whose tracker folder changed at or after this
        /// time (30m, 24h, 7d, YYYY-MM-DD, epoch).
        #[arg(long)]
        since: Option<String>,
        /// Only issues changed at or before this time.
        #[arg(long)]
        until: Option<String>,
        /// Only open issues — not done, not dropped.
        #[arg(long)]
        open: bool,
        /// Only computed-ready leaves.
        #[arg(long)]
        ready: bool,
        /// Only issues with at least one open `blocked_by` target
        /// (CAD-757) — blocked work in active states is one query away.
        #[arg(long)]
        blocked: bool,
        /// Board state at a git revision — exports the tree at `<rev>`
        /// and lists it read-only; cards report `status_source: file`.
        #[arg(long)]
        at: Option<String>,
        /// Sort by id created updated priority status owner title
        /// project; `-KEY` for descending [default: id].
        #[arg(long, allow_hyphen_values = true)]
        sort: Option<String>,
        /// Keep only the first N rows.
        #[arg(long)]
        limit: Option<usize>,
        /// Keep only these keys in each JSON row (comma-joined).
        /// Refused when stdout is a table; a pipe or `--json` is enough.
        #[arg(long, value_delimiter = ',')]
        fields: Vec<String>,
        /// Per-project rollup instead of rows: status counts plus the
        /// P0/P1 issues in doing|review — the master's one-call
        /// cross-project status (CAD-552). Filters still apply;
        /// `--sort`, `--limit` and `--fields` do not. Same JSON
        /// decision as `--fields`.
        #[arg(long)]
        summary: bool,
        #[arg(long)]
        json: bool,
    },
    /// `git log` for the issue folder, parsed: `sha`, `at`, `by`
    /// (the ` (actor)` suffix, else the commit author), `kind`
    /// (`created|set|tag|link|unlink|ref|comment|attach|claim|release|other`), `summary`
    /// and a `fields` map for `set` entries. Read-only.
    Log {
        /// Issue id (CAD-16).
        id: String,
        /// Max entries [default: 50].
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Field-level diff of `issue.md` between two revisions — `{field,
    /// from, to}` entries plus body line counts and comment/artifact
    /// files added/removed. `issue diff CAD-16` compares the issue's
    /// newest change with its parent; `diff CAD-16 <rev> --to HEAD`
    /// shows everything that changed since `<rev>`.
    Diff {
        id: String,
        /// From-revision [default: the parent of the newest change].
        rev: Option<String>,
        /// To-revision [default: the newest commit on the issue].
        #[arg(long)]
        to: Option<String>,
    },
    /// For every frontmatter field currently set, the history entry
    /// that last changed it (`value`, `sha`, `at`, `by`).
    Blame { id: String },
    /// Read-only retrospective for one issue: review rounds and
    /// verdicts, blocking findings, flake mentions, timings and merge
    /// evidence — assembled from the tracker, tagged notes, project
    /// repos and the daemon store (read-only). Prints a preview;
    /// nothing is attached, promoted or published. `--json` emits the
    /// `cadence.retro/1` document.
    Retro {
        /// Issue id (CAD-16).
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Print the exact trailer line for this issue (`Issue: <ID>`) —
    /// agents and hooks append it to code commits without guessing
    /// the format. Refuses an id that does not exist.
    Trailer { id: String },
    /// Inspect and explicitly manage checkout ownership/release records.
    Checkout {
        #[command(subcommand)]
        action: CheckoutAction,
    },
    /// Start work on an issue: mint `.cadence/wt/<id>-<slug>` on
    /// `cadence/<id>-<slug>` in the project repo, record both refs,
    /// move backlog|ready to doing, print the commit trailer. `--job`
    /// also opens an M3 job scoped to the worktree (needs the daemon).
    Start {
        id: String,
        /// Repo path — else the cwd's repo when it is one of the
        /// project's repos, else the project's only repo.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Worktree slug — default: the slugified title (≤32 chars).
        #[arg(long)]
        name: Option<String>,
        /// Integration ref to cut the lane from. Default: the repo's
        /// configured `base`, else the remote HEAD (`origin/HEAD`, else
        /// the remote's symbolic HEAD). A checkout with no `origin`
        /// uses its current branch. A resolved remote ref is never
        /// replaced by the main checkout's current branch.
        #[arg(long)]
        base: Option<String>,
        /// Owner to record when the issue has none — default: the
        /// resolved actor.
        #[arg(long)]
        owner: Option<String>,
        /// Open an M3 job bound to the worktree through `job new` +
        /// `job task add` (daemon required; checked before anything
        /// is created).
        #[arg(long, requires_all = ["pm", "spec"])]
        job: bool,
        /// Owning PM agent for the job.
        #[arg(long, requires = "job")]
        pm: Option<String>,
        /// Job spec file — hashed at creation like `job new`.
        #[arg(long, requires = "job")]
        spec: Option<PathBuf>,
        /// Assignee for the worktree-scoped task.
        #[arg(long, requires = "job")]
        assignee: Option<String>,
        /// Bind the task even when the assignee's pty pane cwd lies
        /// outside every repo of the issue's project (CAD-202). The
        /// override is recorded as an issue comment.
        #[arg(long, requires = "assignee")]
        force: bool,
        /// Who is asking (CAD-383) — the claim check's requester and the
        /// claimant recorded when this start puts the issue into work
        /// [default: --pm, else CADENCE_ALIAS, else operator].
        #[arg(long)]
        by: Option<String>,
        /// Start an issue someone else holds in doing/review (CAD-383).
        /// The reason is required; the take-over replaces the claim and
        /// owner and is recorded as a comment in its own commit.
        #[arg(long, value_name = "REASON")]
        take_over: Option<String>,
    },
    /// Record a claim on an issue (CAD-383) — for a PM whose lanes run
    /// outside cadence (Claude subagents, Codex, humans), so `dispatch`
    /// and `issue start` see it. One commit: the claim (who, when,
    /// note), backlog|ready → doing, and a comment; owner (the lane) is
    /// left alone except on a take-over.
    /// Re-claiming your own claim refreshes its time. Someone else's
    /// doing/review issue refuses unless --take-over — or unless the
    /// daemon marked the claim stale, which needs no flag (CAD-755).
    Claim {
        id: String,
        /// Claimant [default: CADENCE_ALIAS, else operator].
        #[arg(long)]
        by: Option<String>,
        /// One line on where the work runs (lane, branch, session).
        #[arg(long)]
        note: Option<String>,
        /// Take the issue from its current holder; the reason is
        /// recorded.
        #[arg(long, value_name = "REASON")]
        take_over: Option<String>,
    },
    /// Ack a lane's change to a code area its PROJECT.md `areas:` gives
    /// to another owner (CAD-378): clears that Needs-you row. The daemon
    /// binds the acker from the connection — only the area's owner PM
    /// (from its own pane or endpoint) or the operator may.
    Ack {
        id: String,
        /// The area name, as the Needs-you row names it.
        #[arg(long)]
        area: String,
        /// One line on what was agreed.
        #[arg(long)]
        note: Option<String>,
    },
    /// Give up a claim (CAD-383): clears the claim, and owner when it is
    /// the releaser; status is left alone. Only a holder may release.
    Release {
        id: String,
        /// Releaser [default: CADENCE_ALIAS, else operator].
        #[arg(long)]
        by: Option<String>,
        /// One line on why (handed back, finished, abandoned).
        #[arg(long)]
        note: Option<String>,
    },
    /// Plan or explicitly run the scheduled safe cleanup policy. Without
    /// --apply this is a read-only inventory; --apply revalidates before every
    /// existing target-cache reclaim and merged checkout finish.
    Reclaim {
        #[arg(long)]
        apply: bool,
        /// Override the configured idle window for lane target caches.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        idle_secs: Option<u64>,
    },
    /// Finish an issue's worktree: refuse while the worktree is in use
    /// (a live message recorded against it, a pane tree or any process
    /// with cwd/open FD inside it, any file modified in the last 30 minutes),
    /// while the worktree is dirty, while the branch has not started
    /// (no commits beyond where it was cut) or while it is neither
    /// merged nor pushed — `--force` records ordinary overrides, never failed
    /// safety enumeration. Before deletion, require issue-bound declared-repo
    /// ownership and an exact registered worktree; present lifecycle metadata
    /// must match. `--force` cannot bypass these checks.
    /// Then `git worktree remove`, delete the branch
    /// (`--keep-branch` keeps it, `--remote` deletes the remote one
    /// too) and mark both refs `closed: true` in one commit. The
    /// issue's status is untouched. `--merged` instead sweeps every
    /// open worktree ref in scope whose branch is merged and whose
    /// guard passes, one row per worktree; it never forces.
    Finish {
        /// Issue id — required unless --merged.
        id: Option<String>,
        /// Finish exactly this worktree ref — required when the issue
        /// has several open. A directory already gone closes only the
        /// worktree ref unless its branch is also missing; no git state is
        /// touched and a surviving branch ref remains open.
        #[arg(long, conflicts_with = "merged")]
        worktree: Option<PathBuf>,
        /// Override ordinary in-use, dirty, not-started and unmerged refusals —
        /// recorded on the finish commit and in the output. Failed process,
        /// daemon-binding, dirty-tree and activity probes are never forceable. A branch
        /// whose tip no merge/push evidence covers is still kept, and
        /// a probe made stale mid-finish refuses with "retry" — force
        /// never retargets a stale probe or deletes uncovered work.
        #[arg(long, conflicts_with = "merged")]
        force: bool,
        /// Remove the worktree but keep the local branch — with
        /// --remote the remote branch is still deleted when its
        /// evidence gate passes.
        #[arg(long, conflicts_with = "merged")]
        keep_branch: bool,
        /// Also delete the remote branch — the remote tip is fetched
        /// fresh and must be covered by merge evidence (unmerged or
        /// origin-ahead branches keep both copies, noted in the row;
        /// --force deletes anyway and records it). The delete is
        /// leased on the fetched tip: a remote that moved since
        /// refuses the push rather than losing unseen commits.
        #[arg(long)]
        remote: bool,
        /// Sweep every merged+idle worktree in scope — one row per
        /// open worktree ref: finished | skipped(reason) |
        /// refused(reason). Exits 1 when anything refused.
        #[arg(long, conflicts_with = "id")]
        merged: bool,
        /// Limit the sweep to one project (all projects when omitted).
        /// Sweep-only: rejected with a single-ID finish.
        #[arg(long, requires = "merged", conflicts_with = "id")]
        project: Option<String>,
        /// Print the sweep plan without changing anything.
        /// Sweep-only: rejected with a single-ID finish.
        #[arg(long, requires = "merged", conflicts_with = "id")]
        dry_run: bool,
        /// Emit the sweep rows as JSON. Single-ID output is already
        /// JSON, so this is accepted everywhere as a no-op there.
        #[arg(long)]
        json: bool,
    },
    /// Show one issue — frontmatter, body, links both ways, comments,
    /// artifacts, activity.
    Show {
        /// Issue id (CAD-16).
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Replace the level-two Acceptance checklist from a file. The file
    /// must contain at least one explicit checked or unchecked item.
    Acceptance {
        /// Issue id (CAD-16).
        id: String,
        /// Checklist file; every nonblank line must be a checkbox item.
        #[arg(long, value_name = "FILE")]
        from: PathBuf,
    },
    /// Set writable fields: `status priority owner component title
    /// tags type milestone size` (`type`: epic|task|bug|spike; `size`:
    /// S|M|L; an epic's stage moves with `issue epic stage`). Several ids make a bulk edit: one commit, and nothing is
    /// written unless every id and pair is valid.
    Set {
        /// `<ID>… key=value…` — ids first; an empty value clears
        /// owner/component/tags, `tags=a,b` replaces the tag list.
        #[arg(required = true)]
        args: Vec<String>,
        /// Only write when every id's `rev` (the `fnv1a:…` hash `issue
        /// show`/`ls --json` reports) still equals this token —
        /// optimistic concurrency against a write that landed since
        /// the read. A mismatch answers a `conflict` payload and
        /// changes nothing.
        #[arg(long, value_name = "REV")]
        if_rev: Option<String>,
        /// Override the `status=done` evidence gate (CAD-756) — the
        /// reason is recorded on the commit, never silent.
        #[arg(long, value_name = "REASON")]
        force: Option<String>,
    },
    /// Add or remove tags: `issue tag <ID>… add|rm <tag>…`. Bulk like
    /// `set`: one commit, all-or-nothing.
    Tag {
        /// `<ID>… add|rm <tag>…`
        #[arg(required = true)]
        args: Vec<String>,
    },
    /// Epics — `type: epic`, or issues with children — with stage,
    /// weighted progress and health; `stage` moves one.
    Epic {
        #[command(subcommand)]
        action: EpicAction,
    },
    /// Recover an interrupted tracker write: `review`, then `decide`,
    /// then `execute`, then `status`. Operator only for decide and
    /// execute; every verb goes through the daemon.
    Recover {
        #[command(subcommand)]
        action: RecoverAction,
    },
    /// Add a link: `blocked_by|relates|parent|duplicate_of`.
    Link {
        id: String,
        kind: String,
        target: String,
    },
    /// Remove a link.
    Unlink {
        id: String,
        kind: String,
        target: String,
    },
    /// Add a ref: `pr|commit|note|preview|message|url`. A http(s)
    /// target is stored as `url`, anything else as `path`.
    Ref {
        id: String,
        kind: String,
        target: String,
        #[arg(long)]
        label: Option<String>,
    },
    /// Add a comment — one create-only file named by UTC + author
    /// (`CADENCE_ALIAS`, else `operator`).
    Comment {
        id: String,
        /// Inline comment text.
        #[arg(short = 'm', long, conflicts_with = "file")]
        text: Option<String>,
        /// Read the comment body from a file.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Override the recorded author.
        #[arg(long)]
        author: Option<String>,
        /// Optional kind tag (e.g. `note`).
        #[arg(long)]
        kind: Option<String>,
    },
    /// Attach a file into `artifacts/` (basename only, size cap,
    /// create-only).
    Attach { id: String, file: PathBuf },
    /// Several updates to one issue as ONE tracker commit: set, tag,
    /// link, ref, attach, acceptance and a comment. Every part is
    /// validated first — one bad part refuses the whole edit and writes
    /// nothing. `-` reads stdin for `--comment-file` and `--acceptance`
    /// (one of the two). Prints `{id, rev, changed}`.
    Edit {
        id: String,
        /// `key=value`, as `issue set`. Repeatable.
        #[arg(long = "set", value_name = "KEY=VALUE")]
        set: Vec<String>,
        /// `+tag` adds, `-tag` removes, a bare `tag` adds; comma-separated
        /// or repeated: `--tag +a,-b`.
        #[arg(
            long = "tag",
            value_name = "+TAG|-TAG",
            value_delimiter = ',',
            allow_hyphen_values = true
        )]
        tag: Vec<String>,
        /// `kind:ID`, as `issue link` (`blocked_by:CAD-2`). Repeatable.
        #[arg(long = "link", value_name = "KIND:ID")]
        link: Vec<String>,
        /// `kind:ID`, as `issue unlink`. Repeatable.
        #[arg(long = "unlink", value_name = "KIND:ID")]
        unlink: Vec<String>,
        /// `kind:target` (`pr:https://…`) or a bare http(s) URL. Repeatable.
        #[arg(long = "ref", value_name = "KIND:TARGET")]
        refs: Vec<String>,
        /// A ref to remove, named as `--ref` names it. Repeatable.
        /// Every one must already exist, or the whole edit refuses.
        #[arg(long = "unref", value_name = "KIND:TARGET")]
        unrefs: Vec<String>,
        /// Attach a file, as `issue attach`. Repeatable.
        #[arg(long = "attach", value_name = "FILE")]
        attach: Vec<PathBuf>,
        /// Replace the Acceptance checklist from FILE (`-` = stdin).
        #[arg(long, value_name = "FILE|-")]
        acceptance: Option<PathBuf>,
        /// Add a comment from FILE (`-` = stdin).
        #[arg(long = "comment-file", value_name = "FILE|-")]
        comment_file: Option<PathBuf>,
        /// Recorded author of the comment; needs `--comment-file`.
        #[arg(long)]
        author: Option<String>,
        /// Override the `status=done` evidence gate, as `issue set --force`.
        #[arg(long, value_name = "REASON")]
        force: Option<String>,
    },
    /// Check the whole PM dir: schema, id/folder mismatch, dangling and
    /// cyclic links, depth > 2, oversize artifacts, unknown status/kind.
    /// Non-zero exit on any error.
    /// With `--staged`, lint only the issues the git index stages (what
    /// the tracker's commit hook runs); a staged change that is not a
    /// plain issue edit falls back to the whole tracker.
    Lint {
        #[arg(long, conflicts_with = "staged")]
        project: Option<String>,
        #[arg(long)]
        staged: bool,
    },
    /// Reconcile tracker status with merge reality (CAD-754): classify
    /// every `doing`/`review` leaf issue against its recorded branch,
    /// worktree and `pr:` refs, and mark `done` the ones whose work
    /// provably merged — one commit per issue naming the evidence.
    /// `held`/`stalled` rows are reported, never moved: an open PR or
    /// a claim newer than the merge blocks the flip. A note-derived
    /// candidate also needs a fresh note — a status write newer than
    /// the newest tagged note keeps it parked, and a failed history
    /// walk parks the whole notes arm for that sweep (reported on
    /// `history`, never widened on a guess). Merged worktree refs are
    /// swept with the same guard `finish --merged` uses.
    /// `issue sync` runs this sweep after a successful push.
    /// `--issue <ID>` bounds the run to one ticket: it alone is
    /// classified and only its recorded lanes face the cleanup pass —
    /// every other in-flight ticket is neither examined nor probed.
    Reconcile {
        /// Limit the sweep to one project (all projects when omitted).
        #[arg(long)]
        project: Option<String>,
        /// Reconcile only this issue (CAD-1377): classify it and clean
        /// its recorded lanes — the post-merge path for one ticket.
        /// Unknown ids and `--project` mismatches refuse before any
        /// git/gh probe.
        #[arg(long, value_name = "ID")]
        issue: Option<String>,
        /// Print the classification without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Classify at most this many in-scope issues (0 = no limit).
        /// A bounded run cleans only the lanes of the issues it
        /// actually examined and whose verdict is safe — deferred and
        /// held candidates are reported under `deferred`/
        /// `cleanup_deferred`, never implied reconciled. The daemon
        /// tick uses a bound so the rest drain over later ticks.
        #[arg(long, default_value = "0")]
        limit: usize,
        /// Emit the sweep rows as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Bring the tracker level with `origin`: fetch, rebase the local
    /// commits on top, lint the result, push. A conflict or lint
    /// failure aborts and leaves the tree exactly as found; the report
    /// names the paths. Needs a clean tree and no rebase in progress.
    Sync {
        /// Rebase and lint but do not push.
        #[arg(long)]
        no_push: bool,
        /// Fetch and report ahead/behind plus the paths that would
        /// conflict — nothing changes.
        #[arg(long)]
        dry_run: bool,
        /// Resolve conflicting paths by taking one side whole:
        /// `ours` keeps the local commit's content, `theirs` takes the
        /// fetched remote's.
        #[arg(long, value_parser = ["ours", "theirs"])]
        resolve: Option<String>,
    },
    /// Backlog freshness (CAD-812): an advisory groom pass over open
    /// `backlog`/`ready` leaves — the `sweep` sibling for dormant work.
    /// Each dormant ticket is re-judged against the tree and the
    /// tracker: `paths:` that moved since `created`, closed siblings on
    /// the same paths, satisfied blockers never advanced. A `stale` or
    /// `superseded` verdict lands `needs-triage` + a comment +
    /// `last_groomed_at`; `valid` only stamps `last_groomed_at`. Never
    /// changes `status` — the operator decides drop or re-scope. The
    /// daemon checkup runs the same pass; this verb is the manual and
    /// dry-run path.
    Groom {
        /// Report what would be flagged — nothing is written.
        #[arg(long)]
        dry_run: bool,
        /// Limit the pass to one project (all projects when omitted).
        #[arg(long, value_name = "PROJECT")]
        project: Option<String>,
        /// Emit the verdicts as JSON (the verb already prints JSON — the
        /// flag is accepted for consistency with `issue ls --json`).
        #[arg(long)]
        json: bool,
        /// Seconds a `backlog`/`ready` leaf may sit before the pass
        /// judges it — inside the window the ticket is its own evidence
        /// [default: 1209600 (14d)].
        #[arg(long, value_name = "SECS")]
        grace: Option<i64>,
    },
    /// Blocked-work hygiene (CAD-757): park `doing`/`review` leaves
    /// whose blockers are still open and whose claim is older than
    /// `--grace` — tagged `blocked-park`, comment-bearing commit,
    /// reversible — and for a tagged item whose blockers all closed,
    /// notify its last claimer and drop the tag. The daemon checkup
    /// runs the same sweep; this verb is the manual and dry-run path.
    Sweep {
        /// Report what would change — nothing is written.
        #[arg(long)]
        dry_run: bool,
        /// Seconds a blocked active item may hold an unchanged claim
        /// before it parks [default: 86400].
        #[arg(long, value_name = "SECS")]
        grace: Option<i64>,
    },
    /// Sprint pick-batch verbs (CAD-758): `close` ends a batch tag
    /// with a velocity report and moves survivors in one commit;
    /// `open` refills the next batch from ready in priority order.
    /// Any tag scheme works — `sprint-*`, `batch-*`, project-local.
    Sprint {
        #[command(subcommand)]
        action: SprintAction,
    },
}

#[derive(Subcommand)]
pub enum SprintAction {
    /// Close a batch: report each tagged item (done / in-flight /
    /// untouched / blocked stragglers), strip the tag from finished
    /// work, and move every survivor — `--next <tag>` re-tags them
    /// into the next batch, `--drop` returns them to plain status.
    Close {
        /// The pick-batch tag being closed (e.g. sprint-2026w40).
        tag: String,
        /// Re-tag every survivor to this tag in the same commit.
        #[arg(long, value_name = "TAG", conflicts_with = "drop")]
        next: Option<String>,
        /// Drop the tag from survivors — they keep their status.
        #[arg(long)]
        drop: bool,
        /// Report without writing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Open a batch: tag the first `--cap` computed-ready leaves in
    /// priority order; blocked items are skipped and named.
    Open {
        /// The pick-batch tag to populate.
        tag: String,
        /// Batch size cap [default: 10].
        #[arg(long, default_value_t = sprint::DEFAULT_CAP)]
        cap: usize,
        /// Report without writing.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub enum ProjectAction {
    /// Register a project.
    Add {
        /// Project key (folder name), e.g. `cadence`.
        key: String,
        /// Issue id prefix, e.g. `CAD`.
        #[arg(long)]
        prefix: String,
        /// A repo path belonging to the project (repeatable); its
        /// origin remote is recorded for cwd resolution.
        #[arg(long = "repo")]
        repos: Vec<String>,
        /// A component label (repeatable).
        #[arg(long = "component")]
        components: Vec<String>,
        /// A declared tag (repeatable) — with any declared, issues may
        /// only carry tags from the list.
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Default owner applied by `issue new`.
        #[arg(long)]
        owner: Option<String>,
    },
    /// List registered projects.
    #[command(after_long_help = crate::filter::GRAMMAR)]
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
    /// Approve the project's PROJECT.md gate keys (`stages`,
    /// `operator_stages`) and its `delivery:` policy as they are now —
    /// operator only, through the daemon. Until approved (and after
    /// any later edit), the default stages and operator stages apply;
    /// a custom `delivery:` policy likewise applies only while it
    /// matches this approval.
    ApproveWork {
        /// Project key.
        key: String,
    },
    /// Update a registered repository remote after a GitHub transfer or
    /// rename. Run this after the GitHub transfer; old-slug PR URLs then
    /// stop matching. `--from` is the current recorded remote and `--to`
    /// is the new `host/owner/repo` (a git URL is fine). Local tracker
    /// write, same caller model as `project add`.
    SetRemote {
        /// Project key.
        key: String,
        /// Current recorded remote (`host/owner/repo` or a git URL).
        #[arg(long)]
        from: String,
        /// New remote (`host/owner/repo` or a git URL).
        #[arg(long)]
        to: String,
    },
    /// Record a project's access policy: visibility and the callers it
    /// admits. `--grant` is the full member list and replaces whatever
    /// was recorded; omitting it clears grants. It does not merge.
    /// Private is unavailable until the isolation gate passes;
    /// workspace-visible is the only visibility this build can write.
    Policy {
        /// Project key.
        key: String,
        /// Visibility to record.
        #[arg(long, value_enum, default_value = "workspace-visible")]
        visibility: PolicyVisibility,
        /// Grant a caller access (`users/<handle>` or `operator`);
        /// repeatable.
        #[arg(long = "grant")]
        grants: Vec<String>,
    },
}

/// `--visibility` for `project policy`: the recorded vocabulary, before it
/// is mapped to the project policy type.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum PolicyVisibility {
    WorkspaceVisible,
    Private,
}

impl PolicyVisibility {
    fn recorded(self) -> crate::issue::project::Visibility {
        match self {
            Self::WorkspaceVisible => crate::issue::project::Visibility::WorkspaceVisible,
            Self::Private => crate::issue::project::Visibility::Private,
        }
    }
}

pub(crate) fn print_json(value: &Value) {
    println!("{}", crate::output::json_text(value).unwrap_or_default());
}

fn open_pm() -> Result<Pm> {
    Pm::open_default()
}

/// `cadence issue …` — returns the process exit code.
pub fn run(action: &IssueAction, state_dir: &std::path::Path) -> Result<i32> {
    match action {
        IssueAction::Init | IssueAction::Doctor | IssueAction::Project { .. } => {
            project_actions::run(action, state_dir)
        }
        IssueAction::Checkout { .. } => checkout::run(action, state_dir),
        IssueAction::New { .. } => new::run(action, state_dir),
        IssueAction::Ls { .. }
        | IssueAction::Log { .. }
        | IssueAction::Diff { .. }
        | IssueAction::Blame { .. }
        | IssueAction::Trailer { .. }
        | IssueAction::Retro { .. }
        | IssueAction::Show { .. }
        | IssueAction::Acceptance { .. }
        | IssueAction::Set { .. }
        | IssueAction::Tag { .. } => board_actions::run(action, state_dir),
        IssueAction::Start { .. }
        | IssueAction::Claim { .. }
        | IssueAction::Ack { .. }
        | IssueAction::Release { .. }
        | IssueAction::Reclaim { .. }
        | IssueAction::Finish { .. } => lifecycle::run(action, state_dir),
        IssueAction::Link { .. }
        | IssueAction::Unlink { .. }
        | IssueAction::Ref { .. }
        | IssueAction::Comment { .. }
        | IssueAction::Edit { .. }
        | IssueAction::Attach { .. } => links::run(action, state_dir),
        IssueAction::Lint { .. }
        | IssueAction::Reconcile { .. }
        | IssueAction::Sync { .. }
        | IssueAction::Sweep { .. }
        | IssueAction::Groom { .. }
        | IssueAction::Sprint { .. } => maintenance::run(action, state_dir),
        IssueAction::Epic { action } => run_epic(action, state_dir),
        IssueAction::Recover { action } => run_recover(action, state_dir),
    }
}
